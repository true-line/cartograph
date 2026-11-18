//! AC-0665 (T-0665): the draft store and its review.

use super::*;
use crate::section_draft::{Claim, ClaimKind, SYSTEM_SCOPE, SectionAddress};
use std::collections::BTreeMap;

fn address(section: &str) -> SectionAddress {
    SectionAddress {
        catalogue_version: "1.0".into(),
        document: "D1".into(),
        section: section.into(),
        scope: SYSTEM_SCOPE.into(),
    }
}

fn draft(section: &str, statement: &str) -> SectionDraft {
    SectionDraft {
        address: address(section),
        claims: vec![Claim {
            statement: statement.into(),
            kind: ClaimKind::Implemented,
            citations: vec!["F-0000000000000001".into(), "F-0000000000000002".into()],
        }],
        cited_hashes: BTreeMap::from([
            ("F-0000000000000001".to_owned(), "hash-1".to_owned()),
            ("F-0000000000000002".to_owned(), "hash-2".to_owned()),
        ]),
    }
}

fn origin() -> DraftOrigin {
    DraftOrigin {
        role: "section-writer@1".into(),
        role_definition: "definition-1".into(),
        job_id: 7,
        graph_content_hash: "graph-1".into(),
    }
}

fn store() -> (tempfile::TempDir, DraftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = DraftStore::open(dir.path().join("review.sqlite")).unwrap();
    (dir, store)
}

fn count(store: &DraftStore) -> i64 {
    store
        .conn
        .query_row("SELECT COUNT(*) FROM staged_section_drafts", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn identity_covers_address_claims_hashes_role_task_and_graph_hash() {
    let (_dir, mut store) = store();
    let base = store
        .store(&draft("1.3", "A user can check out."), &origin(), None)
        .unwrap();
    assert_eq!(base.schema_version, DRAFT_SCHEMA_VERSION);
    assert_eq!(base.draft_id.len(), 64);
    assert_eq!(base.review_revision, 0);
    assert_eq!(base.review_decision, None);

    let mut other_hash = draft("1.3", "A user can check out.");
    other_hash
        .cited_hashes
        .insert("F-0000000000000002".into(), "hash-2b".into());
    let mut other_kind = draft("1.3", "A user can check out.");
    other_kind.claims[0].kind = ClaimKind::Interpretation;
    let variants: Vec<(SectionDraft, DraftOrigin)> = vec![
        (draft("1.2", "A user can check out."), origin()),
        (draft("1.3", "A user can refund."), origin()),
        (other_kind, origin()),
        (other_hash, origin()),
        (
            draft("1.3", "A user can check out."),
            DraftOrigin {
                role: "section-writer@2".into(),
                ..origin()
            },
        ),
        (
            draft("1.3", "A user can check out."),
            DraftOrigin {
                role_definition: "definition-2".into(),
                ..origin()
            },
        ),
        (
            draft("1.3", "A user can check out."),
            DraftOrigin {
                job_id: 8,
                ..origin()
            },
        ),
        (
            draft("1.3", "A user can check out."),
            DraftOrigin {
                graph_content_hash: "graph-2".into(),
                ..origin()
            },
        ),
    ];
    let mut ids = vec![base.draft_id.clone()];
    for (draft, origin) in &variants {
        ids.push(store.store(draft, origin, None).unwrap().draft_id);
    }
    let distinct: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(distinct.len(), variants.len() + 1);
    assert_eq!(count(&store), variants.len() as i64 + 1);
}

#[test]
fn storing_the_same_draft_again_is_idempotent_before_and_after_review() {
    let (_dir, mut store) = store();
    let content = draft("1.3", "A user can check out.");
    let first = store.store(&content, &origin(), None).unwrap();
    assert_eq!(store.store(&content, &origin(), None).unwrap(), first);
    assert_eq!(count(&store), 1);

    let reviewed = store
        .review(&first.draft_id, 0, DraftDecision::Accepted, Some("fine"))
        .unwrap();
    assert_eq!(store.store(&content, &origin(), None).unwrap(), reviewed);
    assert_eq!(count(&store), 1);
}

#[test]
fn review_is_a_compare_and_set_that_leaves_the_content_unchanged() {
    let (_dir, mut store) = store();
    let stored = store
        .store(&draft("1.3", "A user can check out."), &origin(), None)
        .unwrap();
    let id = stored.draft_id.clone();

    let mut revision = 0;
    for decision in [
        DraftDecision::Accepted,
        DraftDecision::Rejected,
        DraftDecision::Returned,
        DraftDecision::Accepted,
    ] {
        let reviewed = store.review(&id, revision, decision, None).unwrap();
        revision += 1;
        assert_eq!(reviewed.review_revision, revision);
        assert_eq!(reviewed.review_decision, Some(decision));
        assert!(reviewed.reviewed_at.is_some());
        assert_eq!(reviewed.draft, stored.draft);
        assert_eq!(reviewed.origin, stored.origin);
        assert_eq!(reviewed.draft_id, id);
        assert_eq!(reviewed.created_at, stored.created_at);
    }

    // A reviewer who read an older revision changes nothing.
    for stale in [0, revision - 1, revision + 1] {
        assert!(matches!(
            store.review(&id, stale, DraftDecision::Rejected, None),
            Err(DraftStoreError::StaleReviewRevision { expected, actual })
                if expected == stale && actual == revision
        ));
    }
    let current = store.get(&id).unwrap().unwrap();
    assert_eq!(current.review_revision, revision);
    assert_eq!(current.review_decision, Some(DraftDecision::Accepted));

    assert!(matches!(
        store.review(&"0".repeat(64), 0, DraftDecision::Accepted, None),
        Err(DraftStoreError::UnknownDraft)
    ));
    assert!(matches!(
        store.review(
            &id,
            revision,
            DraftDecision::Accepted,
            Some(&"n".repeat(MAX_DRAFT_NOTE_BYTES + 1))
        ),
        Err(DraftStoreError::NoteTooLarge)
    ));
    assert_eq!(store.get(&id).unwrap().unwrap(), current);
}

#[test]
fn a_returned_draft_admits_exactly_one_successor_that_cannot_be_returned() {
    let (_dir, mut store) = store();
    let first = store
        .store(&draft("1.3", "A user can check out."), &origin(), None)
        .unwrap();
    let next_origin = DraftOrigin {
        job_id: 8,
        ..origin()
    };
    let successor = draft("1.3", "A signed-in user can check out.");

    // Not while the first is pending or accepted.
    assert!(matches!(
        store.store(&successor, &next_origin, Some(&first.draft_id)),
        Err(DraftStoreError::PredecessorNotReturned)
    ));
    store
        .review(&first.draft_id, 0, DraftDecision::Accepted, None)
        .unwrap();
    assert!(matches!(
        store.store(&successor, &next_origin, Some(&first.draft_id)),
        Err(DraftStoreError::PredecessorNotReturned)
    ));
    store
        .review(
            &first.draft_id,
            1,
            DraftDecision::Returned,
            Some("too broad"),
        )
        .unwrap();

    // Not for another address, and not for a draft that does not exist.
    assert!(matches!(
        store.store(
            &draft("1.2", "An admin exists."),
            &next_origin,
            Some(&first.draft_id)
        ),
        Err(DraftStoreError::PredecessorAddress)
    ));
    assert!(matches!(
        store.store(&successor, &next_origin, Some(&"0".repeat(64))),
        Err(DraftStoreError::UnknownDraft)
    ));

    let second = store
        .store(&successor, &next_origin, Some(&first.draft_id))
        .unwrap();
    assert_eq!(second.replaces.as_deref(), Some(first.draft_id.as_str()));
    assert_ne!(second.draft_id, first.draft_id);
    // Storing the successor again is the same record, not a second one.
    assert_eq!(
        store
            .store(&successor, &next_origin, Some(&first.draft_id))
            .unwrap(),
        second
    );

    // Exactly one.
    assert!(matches!(
        store.store(
            &draft("1.3", "Another attempt."),
            &DraftOrigin {
                job_id: 9,
                ..origin()
            },
            Some(&first.draft_id)
        ),
        Err(DraftStoreError::SuccessorExists)
    ));
    assert_eq!(count(&store), 2);

    // The successor can be accepted or rejected, never returned.
    assert!(matches!(
        store.review(&second.draft_id, 0, DraftDecision::Returned, None),
        Err(DraftStoreError::SuccessorCannotBeReturned)
    ));
    assert_eq!(
        store
            .get(&second.draft_id)
            .unwrap()
            .unwrap()
            .review_revision,
        0
    );
    let accepted = store
        .review(&second.draft_id, 0, DraftDecision::Accepted, None)
        .unwrap();
    assert_eq!(accepted.review_decision, Some(DraftDecision::Accepted));
    assert!(matches!(
        store.review(&second.draft_id, 1, DraftDecision::Returned, None),
        Err(DraftStoreError::SuccessorCannotBeReturned)
    ));
}

#[test]
fn draft_review_and_citations_reopen_unchanged_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("review.sqlite");
    let (pending, reviewed) = {
        let mut store = DraftStore::open(&path).unwrap();
        let pending = store
            .store(&draft("1.2", "An admin exists."), &origin(), None)
            .unwrap();
        let stored = store
            .store(&draft("1.3", "A user can check out."), &origin(), None)
            .unwrap();
        let reviewed = store
            .review(&stored.draft_id, 0, DraftDecision::Accepted, Some("ok"))
            .unwrap();
        (pending, reviewed)
    };

    // The proposal store opens the same file and leaves the drafts alone.
    drop(crate::staging::ProposalStore::open(&path).unwrap());

    let store = DraftStore::open(&path).unwrap();
    assert_eq!(store.get(&pending.draft_id).unwrap().unwrap(), pending);
    let reopened = store.get(&reviewed.draft_id).unwrap().unwrap();
    assert_eq!(reopened, reviewed);
    assert_eq!(reopened.review_note.as_deref(), Some("ok"));
    assert_eq!(reopened.draft.cited_hashes, draft("1.3", "x").cited_hashes);
    assert_eq!(reopened.draft.claims[0].citations.len(), 2);
    assert!(store.get(&"0".repeat(64)).unwrap().is_none());
}

#[test]
fn a_draft_that_admission_would_refuse_or_that_is_too_large_is_not_stored() {
    let (_dir, mut store) = store();

    let mut uncited = draft("1.3", "A user can check out.");
    uncited.claims[0].citations.clear();
    assert!(matches!(
        store.store(&uncited, &origin(), None),
        Err(DraftStoreError::InvalidDraft(DraftRefusal::NoCitation {
            claim: 0
        }))
    ));

    // A hash for a fact no claim cites, and a missing hash.
    let mut extra = draft("1.3", "A user can check out.");
    extra
        .cited_hashes
        .insert("F-0000000000000003".into(), "hash-3".into());
    assert!(matches!(
        store.store(&extra, &origin(), None),
        Err(DraftStoreError::CitedHashes)
    ));
    let mut missing = draft("1.3", "A user can check out.");
    missing.cited_hashes.remove("F-0000000000000002");
    assert!(matches!(
        store.store(&missing, &origin(), None),
        Err(DraftStoreError::InvalidDraft(
            DraftRefusal::CitationNotAdmitted { claim: 0 }
        ))
    ));

    for (bad, field) in [
        (
            DraftOrigin {
                role: " ".into(),
                ..origin()
            },
            "role",
        ),
        (
            DraftOrigin {
                role_definition: String::new(),
                ..origin()
            },
            "role definition",
        ),
        (
            DraftOrigin {
                job_id: 0,
                ..origin()
            },
            "job",
        ),
        (
            DraftOrigin {
                graph_content_hash: "a\nb".into(),
                ..origin()
            },
            "graph content hash",
        ),
    ] {
        assert!(matches!(
            store.store(&draft("1.3", "A user can check out."), &bad, None),
            Err(DraftStoreError::InvalidOrigin(name)) if name == field
        ));
    }

    // Sixty-four maximal statements of quotes double when escaped.
    let mut large = draft("1.3", "A user can check out.");
    large.claims = (0..crate::section_draft::MAX_CLAIMS)
        .map(|_| Claim {
            statement: format!(
                "a{}",
                "\"".repeat(crate::section_draft::MAX_STATEMENT_BYTES - 1)
            ),
            kind: ClaimKind::Implemented,
            citations: vec!["F-0000000000000001".into(), "F-0000000000000002".into()],
        })
        .collect();
    assert!(matches!(
        store.store(&large, &origin(), None),
        Err(DraftStoreError::RecordTooLarge { bytes }) if bytes > MAX_DRAFT_RECORD_BYTES
    ));
    assert_eq!(count(&store), 0);
}

#[test]
fn stored_content_cannot_be_updated_by_any_caller() {
    let (_dir, mut store) = store();
    let stored = store
        .store(&draft("1.3", "A user can check out."), &origin(), None)
        .unwrap();
    for column in ["draft_id", "immutable_json", "replaces", "created_at"] {
        let error = store
            .conn
            .execute(
                &format!("UPDATE staged_section_drafts SET {column} = 'x'"),
                [],
            )
            .unwrap_err();
        assert!(error.to_string().contains("immutable"), "{column}");
    }
    assert_eq!(store.get(&stored.draft_id).unwrap().unwrap(), stored);
}

#[test]
fn drafts_are_listed_newest_first_in_bounded_pages() {
    let (_dir, mut store) = store();
    let ids: Vec<String> = (1..=5)
        .map(|job_id| {
            store
                .store(
                    &draft("1.3", "A user can check out."),
                    &DraftOrigin { job_id, ..origin() },
                    None,
                )
                .unwrap()
                .draft_id
        })
        .collect();
    let page = store.list(2, None).unwrap();
    assert_eq!(
        page.items.iter().map(|d| &d.draft_id).collect::<Vec<_>>(),
        [&ids[4], &ids[3]]
    );
    let page = store.list(2, page.next_before).unwrap();
    assert_eq!(
        page.items.iter().map(|d| &d.draft_id).collect::<Vec<_>>(),
        [&ids[2], &ids[1]]
    );
    let page = store.list(2, page.next_before).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].draft_id, ids[0]);
    assert_eq!(page.next_before, None);

    assert_eq!(
        store.list(MAX_DRAFT_PAGE_ITEMS, None).unwrap().items.len(),
        5
    );
    for limit in [0, MAX_DRAFT_PAGE_ITEMS + 1] {
        assert!(matches!(
            store.list(limit, None),
            Err(DraftStoreError::InvalidLimit)
        ));
    }
}

/// AC-0667: a stored draft becomes projection input with nothing derived.
#[test]
fn stored_draft_converts_to_projection_input_as_stored() {
    use context_hub::curated::{DraftReview, ReviewedDraft};
    let (_dir, mut store) = store();
    let stored = store
        .store(&draft("1.3", "A user can check out."), &origin(), None)
        .unwrap();
    let pending: ReviewedDraft = crate::curated::reviewed_draft(&stored);
    assert_eq!(pending.draft_id, stored.draft_id);
    assert_eq!(pending.address.document, "D1");
    assert_eq!(pending.address.section, "1.3");
    assert_eq!(pending.address.scope, SYSTEM_SCOPE);
    assert_eq!(pending.address.catalogue_version, "1.0");
    assert_eq!(pending.role, "section-writer@1");
    assert_eq!(pending.cited_hashes, stored.draft.cited_hashes);
    assert_eq!(pending.replaces, None);
    assert_eq!(pending.decision, None);
    let [claim] = pending.claims.as_slice() else {
        panic!("one claim");
    };
    assert_eq!(claim.statement, "A user can check out.");
    assert_eq!(claim.kind, "implemented");
    assert_eq!(claim.citations, stored.draft.claims[0].citations);

    for (decision, review) in [
        (DraftDecision::Accepted, DraftReview::Accepted),
        (DraftDecision::Rejected, DraftReview::Rejected),
        (DraftDecision::Returned, DraftReview::Returned),
    ] {
        let (_dir, mut store) = self::store();
        let stored = store
            .store(&draft("1.3", "A user can check out."), &origin(), None)
            .unwrap();
        let reviewed = store
            .review(&stored.draft_id, 0, decision, Some("a note"))
            .unwrap();
        let converted = crate::curated::reviewed_draft(&reviewed);
        assert_eq!(converted.decision, Some(review));
        // The note, the times and the review counter are not copied.
        assert_eq!(
            ReviewedDraft {
                decision: None,
                ..converted
            },
            pending
        );
    }
}
