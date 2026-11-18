//! The Workbench document view and draft review (#1748, SPEC-15 slice 7,
//! ADR-0064 decisions 5 and 6, AC-0669).
//!
//! [`document_view`] is the read: the sections of one catalogue document in
//! catalogue order, each with its kind and every stored draft addressed to
//! it, with the draft's claims, citations and current review. It reads the
//! section table and the draft store and nothing else, so it shows a draft
//! that is pending, rejected or returned as well as an accepted one. The
//! state of an accepted draft (eligible, stale, superseded, conflicted,
//! unresolvable) is not computed here: it comes from the curated read the
//! Workbench already holds, so the view and the export agree by
//! construction.
//!
//! [`review`] records a decision by compare-and-set on the revision the
//! reviewer read. It changes no content, tier or fact (R-INT-1, R-INT-3):
//! an accepted draft stays T3 / InferredWeak, and what a document prints is
//! decided by the next curated read.

use agents::draft_store::{DraftDecision, DraftStore, MAX_DRAFT_PAGE_ITEMS, StoredDraft};
use agents::section_change::{SectionChange, section_change};
use serde::Serialize;
use std::collections::BTreeMap;
use tauri::Manager;

/// Most stored drafts one document view reads.
pub(crate) const MAX_VIEW_DRAFTS: usize = 2_000;

/// One section of the document with the drafts addressed to it.
#[derive(Debug, Serialize)]
pub(crate) struct SectionView {
    /// The catalogue number, such as `1.3`.
    number: &'static str,
    /// The catalogue title.
    title: &'static str,
    /// The catalogue's mark for the kind: `P`, `A`, `P+A` or `H`.
    kind: &'static str,
    /// Whether a role can draft the section (A or P+A).
    drafted: bool,
    /// The node label of the subject the section is written per, if any.
    subject: Option<&'static str>,
    /// Stored drafts for the section: the system scope first, then by
    /// scope, newest first within a scope.
    drafts: Vec<StoredDraft>,
}

/// One catalogue document as the Workbench shows it.
#[derive(Debug, Serialize)]
pub(crate) struct SectionDocument {
    /// The catalogue version of the section table.
    catalogue_version: &'static str,
    /// `D1`..`D11`.
    document: String,
    /// The sections, in catalogue order.
    sections: Vec<SectionView>,
    /// Whether more stored drafts exist than the view read. The newest
    /// [`MAX_VIEW_DRAFTS`] are shown.
    drafts_truncated: bool,
    /// For each listed draft that replaces a stored draft, by its id: what
    /// it changes in that draft (SPEC-17, AC-0675). Computed from the two
    /// stored drafts alone.
    changes: BTreeMap<String, SectionChange>,
}

/// The document view of `document`, or the closed reason there is none.
pub(crate) fn document_view(store: &DraftStore, document: &str) -> Result<SectionDocument, String> {
    let mut sections: Vec<SectionView> = spec::catalogue_sections(document)
        .map(|section| SectionView {
            number: section.number,
            title: section.title,
            kind: section.kind.mark(),
            drafted: section.kind.drafted(),
            subject: section.subject,
            drafts: Vec::new(),
        })
        .collect();
    if sections.is_empty() {
        return Err("document_not_in_section_table".to_owned());
    }
    let mut before = None;
    let mut read = 0;
    let mut drafts_truncated = false;
    loop {
        let page = store
            .list(MAX_DRAFT_PAGE_ITEMS, before)
            .map_err(|error| error.to_string())?;
        read += page.items.len();
        for draft in page.items {
            let address = &draft.draft.address;
            if address.document != document || address.catalogue_version != spec::CATALOGUE_VERSION
            {
                continue;
            }
            if let Some(section) = sections
                .iter_mut()
                .find(|section| section.number == address.section)
            {
                section.drafts.push(draft);
            }
        }
        before = page.next_before;
        if before.is_none() {
            break;
        }
        if read >= MAX_VIEW_DRAFTS {
            drafts_truncated = true;
            break;
        }
    }
    for section in &mut sections {
        // Stable: pages are newest first, and that order is kept in a scope.
        section.drafts.sort_by(|left, right| {
            let key = |draft: &StoredDraft| {
                let scope = draft.draft.address.scope.clone();
                (scope != spec::SYSTEM_SCOPE, scope)
            };
            key(left).cmp(&key(right))
        });
    }
    let mut changes = BTreeMap::new();
    for draft in sections.iter().flat_map(|section| &section.drafts) {
        let Some(replaced) = draft.replaces.as_deref() else {
            continue;
        };
        // Read by id: the replaced draft may be older than the view reads.
        if let Some(replaced) = store.get(replaced).map_err(|error| error.to_string())? {
            changes.insert(
                draft.draft_id.clone(),
                section_change(&replaced.draft, &draft.draft),
            );
        }
    }
    Ok(SectionDocument {
        catalogue_version: spec::CATALOGUE_VERSION,
        document: document.to_owned(),
        sections,
        drafts_truncated,
        changes,
    })
}

/// Record `decision` on a draft at the revision the reviewer read.
pub(crate) fn review(
    store: &mut DraftStore,
    draft_id: &str,
    expected_revision: u64,
    decision: DraftDecision,
    note: Option<&str>,
) -> Result<StoredDraft, String> {
    let note = note.map(str::trim).filter(|note| !note.is_empty());
    store
        .review(draft_id, expected_revision, decision, note)
        .map_err(|error| error.to_string())
}

/// The sections of one catalogue document with their stored drafts.
#[tauri::command]
pub(crate) async fn section_document(
    document: String,
    app: tauri::AppHandle,
) -> Result<SectionDocument, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        let store = state.drafts.lock().map_err(|e| e.to_string())?;
        document_view(&store, &document)
    })
    .await
}

/// Accept, reject or return one section draft.
#[tauri::command]
pub(crate) async fn review_section_draft(
    draft_id: String,
    expected_revision: u64,
    decision: DraftDecision,
    note: Option<String>,
    app: tauri::AppHandle,
) -> Result<StoredDraft, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        let mut store = state.drafts.lock().map_err(|e| e.to_string())?;
        review(
            &mut store,
            &draft_id,
            expected_revision,
            decision,
            note.as_deref(),
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::draft_store::DraftOrigin;
    use agents::section_draft::{Claim, ClaimKind, SectionAddress, SectionDraft};
    use std::collections::BTreeMap;

    fn draft(section: &str, scope: &str, statement: &str) -> SectionDraft {
        SectionDraft {
            address: SectionAddress {
                catalogue_version: spec::CATALOGUE_VERSION.into(),
                document: "D1".into(),
                section: section.into(),
                scope: scope.into(),
            },
            claims: vec![Claim {
                statement: statement.into(),
                kind: ClaimKind::Implemented,
                citations: vec!["F-0000000000000001".into()],
            }],
            cited_hashes: BTreeMap::from([("F-0000000000000001".to_owned(), "hash".to_owned())]),
        }
    }

    fn origin() -> DraftOrigin {
        DraftOrigin {
            role: "section-writer@1".into(),
            role_definition: "definition".into(),
            job_id: 1,
            graph_content_hash: "graph".into(),
        }
    }

    fn store() -> (tempfile::TempDir, DraftStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DraftStore::open(dir.path().join("drafts.sqlite")).unwrap();
        (dir, store)
    }

    // AC-0669: every section is listed with its kind, and each stored draft
    // sits under its section whatever its review.
    #[test]
    fn the_view_lists_each_section_with_its_kind_and_its_stored_drafts() {
        let (_dir, mut store) = store();
        let pending = store
            .store(&draft("1.3", "system", "Pending."), &origin(), None)
            .unwrap();
        let subject = store
            .store(
                &draft("1.3", "F-00000000000000aa", "Per subject."),
                &origin(),
                None,
            )
            .unwrap();
        let rejected = store
            .store(&draft("1.1", "system", "Rejected."), &origin(), None)
            .unwrap();
        review(
            &mut store,
            &rejected.draft_id,
            0,
            DraftDecision::Rejected,
            Some("  Not supported.  "),
        )
        .unwrap();

        let view = document_view(&store, "D1").unwrap();
        assert_eq!(view.catalogue_version, spec::CATALOGUE_VERSION);
        assert!(!view.drafts_truncated);
        let kinds: Vec<_> = view
            .sections
            .iter()
            .map(|section| (section.number, section.kind, section.drafted))
            .collect();
        assert_eq!(
            kinds,
            [
                ("1.1", "A", true),
                ("1.2", "P+A", true),
                ("1.3", "P+A", true),
                ("1.4", "P+A", true),
                ("1.5", "P+A", true),
                ("1.6", "P", false),
                ("1.7", "H", false),
                ("1.8", "P", false),
            ]
        );
        let capabilities = &view.sections[2];
        assert_eq!(capabilities.title, "Capabilities");
        assert_eq!(capabilities.subject, Some("Capability"));
        // The system scope first, then the subject.
        let ids: Vec<_> = capabilities
            .drafts
            .iter()
            .map(|draft| draft.draft_id.as_str())
            .collect();
        assert_eq!(ids, [pending.draft_id.as_str(), subject.draft_id.as_str()]);
        assert_eq!(capabilities.drafts[0].review_decision, None);
        assert_eq!(capabilities.drafts[0].draft.claims[0].statement, "Pending.");
        let purpose = &view.sections[0];
        assert_eq!(purpose.drafts.len(), 1);
        assert_eq!(
            purpose.drafts[0].review_decision,
            Some(DraftDecision::Rejected)
        );
        assert_eq!(
            purpose.drafts[0].review_note.as_deref(),
            Some("Not supported.")
        );
        assert!(view.sections[5].drafts.is_empty());

        // A document the table does not hold has no view.
        assert_eq!(
            document_view(&store, "D2").unwrap_err(),
            "document_not_in_section_table"
        );
    }

    // AC-0675 (T-0675): the view carries, for a draft that replaces
    // another, what it changes in that draft, from the two stored drafts.
    #[test]
    fn the_view_carries_the_change_of_a_draft_that_replaces_another() {
        let (_dir, mut store) = store();
        let accepted = store
            .store(
                &draft("1.3", "system", "A user can check out."),
                &origin(),
                None,
            )
            .unwrap();
        review(
            &mut store,
            &accepted.draft_id,
            0,
            DraftDecision::Accepted,
            None,
        )
        .unwrap();
        // No draft replaces another yet.
        assert!(document_view(&store, "D1").unwrap().changes.is_empty());

        let mut rewritten = draft("1.3", "system", "A signed-in user can check out.");
        rewritten.claims.push(Claim {
            statement: "Stock is checked.".into(),
            kind: ClaimKind::Implemented,
            citations: vec!["F-0000000000000002".into()],
        });
        rewritten.cited_hashes = BTreeMap::from([
            ("F-0000000000000001".to_owned(), "hash-after".to_owned()),
            ("F-0000000000000002".to_owned(), "hash".to_owned()),
        ]);
        let successor = store
            .store(
                &rewritten,
                &DraftOrigin {
                    job_id: 8,
                    ..origin()
                },
                Some(&accepted.draft_id),
            )
            .unwrap();

        let view = document_view(&store, "D1").unwrap();
        assert_eq!(
            view.changes.keys().collect::<Vec<_>>(),
            [&successor.draft_id]
        );
        let change = &view.changes[&successor.draft_id];
        assert_eq!(change.claims_added, rewritten.claims);
        assert_eq!(change.claims_removed, accepted.draft.claims);
        assert!(change.claims_kept.is_empty());
        assert_eq!(change.facts_added, ["F-0000000000000002"]);
        assert!(change.facts_removed.is_empty());
        assert_eq!(change.facts_changed, ["F-0000000000000001"]);
        // Serialized under the successor's id for the Workbench.
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(
            json["changes"][&successor.draft_id]["facts_changed"],
            serde_json::json!(["F-0000000000000001"])
        );
    }

    // AC-0669: a review is a compare-and-set that changes no content.
    #[test]
    fn a_review_is_recorded_at_the_revision_read_and_changes_no_content() {
        let (_dir, mut store) = store();
        let stored = store
            .store(&draft("1.3", "system", "A claim."), &origin(), None)
            .unwrap();
        let accepted = review(
            &mut store,
            &stored.draft_id,
            0,
            DraftDecision::Accepted,
            None,
        )
        .unwrap();
        assert_eq!(accepted.review_decision, Some(DraftDecision::Accepted));
        assert_eq!(accepted.review_revision, 1);
        assert_eq!(accepted.review_note, None);
        assert_eq!(accepted.draft, stored.draft);
        assert_eq!(accepted.origin, stored.origin);
        // A second reviewer who read revision 0 is refused.
        let stale = review(
            &mut store,
            &stored.draft_id,
            0,
            DraftDecision::Rejected,
            None,
        )
        .unwrap_err();
        assert!(stale.contains("stale review revision"));
        let view = document_view(&store, "D1").unwrap();
        assert_eq!(
            view.sections[2].drafts[0].review_decision,
            Some(DraftDecision::Accepted)
        );
        // Returning at the current revision is recorded.
        let returned = review(
            &mut store,
            &stored.draft_id,
            1,
            DraftDecision::Returned,
            Some("Cite the handler."),
        )
        .unwrap();
        assert_eq!(returned.review_revision, 2);
        assert_eq!(returned.review_note.as_deref(), Some("Cite the handler."));
        assert!(review(&mut store, "unknown", 0, DraftDecision::Accepted, None).is_err());
    }
}
