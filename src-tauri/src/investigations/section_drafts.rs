//! The host side of a drafting run (#1748, SPEC-15 slice 6, ADR-0064
//! decisions 3 and 4, AC-0668).
//!
//! A drafting run is an ordinary `section-writer@1` investigation task. This
//! module adds the two things the task loop does not know about:
//!
//! - [`validate_start`] refuses a start whose question does not name one
//!   drafted section of the catalogue table, before anything durable.
//! - [`stage`] turns a completed run into a staged draft, or into a closed
//!   reason why there is none. It reads the saved result and ledger, refuses
//!   when the current graph is no longer the snapshot the run froze, takes
//!   the citation set of the address from that graph, applies the pure
//!   admission rule and stores the draft. It calls no model and is
//!   idempotent: storing identical content returns the stored record.
//!
//! The worker calls [`stage`] once when a drafting run ends. The
//! `section_draft_outcome` command calls it again on request, so a draft
//! that could not be stored (a locked store, a crash between the run's
//! completion and the store) is staged by the next read.

use crate::AppState;
use agents::draft_store::{DraftDecision, DraftOrigin, DraftStoreError, MAX_DRAFT_PAGE_ITEMS};
use agents::investigation::{InvestigationStatus, KnowledgeCompleteness, SpecialistId};
use agents::section_draft::{SectionAddress, admit};
use agents::section_run::{SECTION_WRITER_ROLE, finish_of, question, requested_address};
use agents::source_basis::TaskFactKey;
use core_graph::FactKey;
use core_graph::{Edge, Node};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tauri::Manager;

/// Most stored drafts read to find the returned draft a new one succeeds.
const MAX_PREDECESSOR_SCAN: usize = 2_000;

/// What a drafting run produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum DraftingOutcome {
    /// The run's finish was admitted and the draft is stored for review.
    Staged {
        /// The stored draft.
        draft_id: String,
    },
    /// The run staged no draft.
    NoDraft {
        /// A closed reason code; never text from the run.
        reason: &'static str,
    },
}

impl DraftingOutcome {
    fn none(reason: &'static str) -> Self {
        Self::NoDraft { reason }
    }
}

/// The address a drafting run's `question` names, checked against the
/// section table: a drafted section of this catalogue version, under its
/// catalogue title, for the system or (where the section is written per
/// subject) for one subject.
pub(crate) fn drafting_address(text: &str) -> Result<SectionAddress, &'static str> {
    let (address, title) = requested_address(text).ok_or("question_not_an_address")?;
    let section = spec::drafted_section(
        &address.catalogue_version,
        &address.document,
        &address.section,
    )
    .map_err(spec::SectionScopeError::code)?;
    if title != section.title {
        return Err("question_not_an_address");
    }
    if address.scope != spec::SYSTEM_SCOPE && section.subject.is_none() {
        return Err(spec::SectionScopeError::NotPerSubject.code());
    }
    Ok(address)
}

/// The question that starts a drafting run for the address, or the closed
/// reason the address cannot be drafted.
pub(crate) fn drafting_question(
    catalogue_version: &str,
    document: &str,
    section: &str,
    scope: &str,
) -> Result<String, &'static str> {
    let entry = spec::drafted_section(catalogue_version, document, section)
        .map_err(spec::SectionScopeError::code)?;
    let text = question(
        &SectionAddress {
            catalogue_version: catalogue_version.to_owned(),
            document: document.to_owned(),
            section: section.to_owned(),
            scope: scope.to_owned(),
        },
        entry.title,
    );
    drafting_address(&text).map(|_| text)
}

/// Refuse a `section-writer@1` start whose question is not a drafting
/// request. Every other role is untouched.
pub(crate) fn validate_start(specialist_id: &str, text: &str) -> Result<(), String> {
    if specialist_id != SECTION_WRITER_ROLE {
        return Ok(());
    }
    drafting_address(text).map(|_| ()).map_err(|reason| {
        format!("A drafting run must name one drafted catalogue section ({reason}).")
    })
}

pub(crate) fn fact_key(fact: &TaskFactKey) -> FactKey {
    match fact {
        TaskFactKey::Node { id } => FactKey::Node { id: id.clone() },
        TaskFactKey::Edge {
            source,
            label,
            destination,
        } => FactKey::Edge {
            source: source.clone(),
            label: label.clone(),
            destination: destination.clone(),
        },
    }
}

/// The stored drafts for one address, read to find what a new draft
/// replaces.
struct AddressDrafts {
    /// Drafts some draft names as replaced, whatever its review.
    succeeded: BTreeSet<String>,
    /// Drafts a successor that is not rejected names as replaced.
    standing: BTreeSet<String>,
    /// Returned drafts, newest first.
    returned: Vec<String>,
    /// Accepted drafts with the hashes they recorded, newest first.
    accepted: Vec<(String, BTreeMap<String, String>)>,
}

fn address_drafts(
    store: &agents::draft_store::DraftStore,
    address: &SectionAddress,
) -> Result<AddressDrafts, DraftStoreError> {
    let mut drafts = AddressDrafts {
        succeeded: BTreeSet::new(),
        standing: BTreeSet::new(),
        returned: Vec::new(),
        accepted: Vec::new(),
    };
    let mut before = None;
    let mut read = 0;
    loop {
        let page = store.list(MAX_DRAFT_PAGE_ITEMS, before)?;
        read += page.items.len();
        for item in page.items {
            if item.draft.address != *address {
                continue;
            }
            if let Some(replaced) = item.replaces {
                if item.review_decision != Some(DraftDecision::Rejected) {
                    drafts.standing.insert(replaced.clone());
                }
                drafts.succeeded.insert(replaced);
            }
            match item.review_decision {
                Some(DraftDecision::Returned) => drafts.returned.push(item.draft_id),
                Some(DraftDecision::Accepted) => drafts
                    .accepted
                    .push((item.draft_id, item.draft.cited_hashes)),
                Some(DraftDecision::Rejected) | None => {}
            }
        }
        before = page.next_before;
        if before.is_none() || read >= MAX_PREDECESSOR_SCAN {
            break;
        }
    }
    Ok(drafts)
}

/// Whether a cited fact of an accepted draft changed or is gone: the draft
/// is `stale` or `unresolvable` in the curated projection (ADR-0045).
fn drifted(recorded: &BTreeMap<String, String>, nodes: &[Node], edges: &[Edge]) -> bool {
    let cited: BTreeSet<String> = recorded.keys().cloned().collect();
    spec::cited_fact_hashes(nodes, edges, &cited) != *recorded
}

/// The draft a new draft for `address` replaces. First the newest returned
/// draft that no draft has succeeded (ADR-0064 decision 6). Otherwise the
/// one accepted draft whose cited facts drifted and that has no standing
/// successor: the new draft is its re-draft (ADR-0045 decision 6). Two such
/// accepted drafts are in conflict and neither is chosen.
pub(crate) fn predecessor(
    store: &agents::draft_store::DraftStore,
    address: &SectionAddress,
    (nodes, edges): (&[Node], &[Edge]),
) -> Result<Option<String>, DraftStoreError> {
    let drafts = address_drafts(store, address)?;
    // Pages are newest first, so the first unsucceeded one is the newest.
    if let Some(returned) = drafts
        .returned
        .iter()
        .find(|id| !drafts.succeeded.contains(*id))
    {
        return Ok(Some(returned.clone()));
    }
    let mut stale = drafts
        .accepted
        .iter()
        .filter(|(id, recorded)| !drafts.standing.contains(id) && drifted(recorded, nodes, edges));
    match (stale.next(), stale.next()) {
        (Some((id, _)), None) => Ok(Some(id.clone())),
        _ => Ok(None),
    }
}

/// The question that starts a re-draft of the accepted draft `draft_id`, or
/// the closed reason it cannot be re-drafted. Only a draft the curated
/// projection lists as `stale` or `unresolvable` is re-drafted, and only
/// while no successor of it stands.
pub(crate) fn redraft_question(state: &AppState, draft_id: &str) -> Result<String, String> {
    let (nodes, edges) = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        let (_, (snapshot, _)) = crate::curated::read_graph(&graph)?;
        snapshot
    };
    let store = state.drafts.lock().map_err(|e| e.to_string())?;
    let Some(draft) = store.get(draft_id).map_err(|e| e.to_string())? else {
        return Err("unknown_draft".to_owned());
    };
    if draft.review_decision != Some(DraftDecision::Accepted) {
        return Err("draft_not_accepted".to_owned());
    }
    let address = &draft.draft.address;
    let drafts = address_drafts(&store, address).map_err(|e| e.to_string())?;
    if drafts.standing.contains(draft_id) {
        return Err("successor_pending".to_owned());
    }
    if !drifted(&draft.draft.cited_hashes, &nodes, &edges) {
        return Err("draft_not_stale".to_owned());
    }
    drafting_question(
        &address.catalogue_version,
        &address.document,
        &address.section,
        &address.scope,
    )
    .map_err(str::to_owned)
}

/// The outcome of a task when it is a drafting run, and none for a task of
/// any other role. The MCP result read reports it to the session that
/// started the run.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn outcome_of_drafting_run(
    state: &AppState,
    id: &str,
) -> Result<Option<DraftingOutcome>, String> {
    let drafting = {
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let summary = jobs.investigation(id).map_err(|e| e.to_string())?.summary;
        summary.specialist_id == SpecialistId::SectionWriterV1
    };
    if drafting {
        stage(state, id).map(Some)
    } else {
        Ok(None)
    }
}

/// The draft already stored for the drafting job `job_id`, if any. A job
/// produces at most one draft, so a reread must return it rather than
/// recompute a predecessor, which changes with later reviews.
fn existing_draft(
    store: &agents::draft_store::DraftStore,
    job_id: i64,
) -> Result<Option<String>, DraftStoreError> {
    Ok(store.for_job(job_id)?.map(|item| item.draft_id))
}

/// Stage the draft of the completed drafting run `id`, or say why it staged
/// none. `Err` is an operational failure (a store or the graph could not be
/// read); the run's result is unchanged and a later call retries.
pub(crate) fn stage(state: &AppState, id: &str) -> Result<DraftingOutcome, String> {
    let (summary, result, ledger) = {
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let summary = jobs.investigation(id).map_err(|e| e.to_string())?.summary;
        if summary.specialist_id != SpecialistId::SectionWriterV1 {
            return Ok(DraftingOutcome::none("not_a_drafting_run"));
        }
        if summary.status != InvestigationStatus::Completed {
            return Ok(DraftingOutcome::none("run_not_completed"));
        }
        let result = jobs.investigation_result(id).map_err(|e| e.to_string())?;
        let ledger = jobs.investigation_ledger(id).map_err(|e| e.to_string())?;
        (summary, result, ledger)
    };
    // A run that already staged a draft returns it, whatever has been
    // reviewed since.
    let stored = {
        let store = state.drafts.lock().map_err(|e| e.to_string())?;
        existing_draft(&store, summary.job_id).map_err(|e| e.to_string())?
    };
    if let Some(draft_id) = stored {
        return Ok(DraftingOutcome::Staged { draft_id });
    }
    let Some(result) = result else {
        return Ok(DraftingOutcome::none("run_not_completed"));
    };
    if result.findings.is_empty()
        || result.knowledge_completeness == KnowledgeCompleteness::InsufficientEvidence
    {
        return Ok(DraftingOutcome::none("insufficient_evidence"));
    }
    let address = match drafting_address(&summary.question) {
        Ok(address) => address,
        Err(reason) => return Ok(DraftingOutcome::none(reason)),
    };
    // The draft is recorded as written against the current graph, so that
    // graph must be the one the run's evidence was frozen from. A completed
    // run without its ledger has no frozen snapshot to compare.
    let Some(ledger) = ledger else {
        return Ok(DraftingOutcome::none("graph_changed"));
    };
    let finish = match finish_of(&address, &result.findings, &ledger.citations, |fact| {
        spec::citation_id(&fact_key(fact))
    }) {
        Ok(finish) => finish,
        Err(refusal) => return Ok(DraftingOutcome::none(refusal.code())),
    };
    let cited: BTreeSet<String> = finish
        .claims
        .iter()
        .flat_map(|claim| claim.citations.iter().cloned())
        .collect();
    let (_, ((nodes, edges), _)) = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        crate::curated::read_graph(&graph)?
    };
    // A re-ingest since the run (or a retry after a failed store) may keep a
    // cited fact's id and change its content: the snapshot identity covers
    // every fact's content, so any change refuses rather than staging the
    // draft against a graph it was not written from.
    let current = context_hub::ContextSnapshot::new(nodes.clone(), edges.clone())
        .ok()
        .map(|snapshot| snapshot.id().to_owned());
    if current.as_deref() != Some(ledger.graph_snapshot_id.as_str()) {
        return Ok(DraftingOutcome::none("graph_changed"));
    }
    // The table held the section when the address was checked above.
    let section = spec::drafted_section(
        &address.catalogue_version,
        &address.document,
        &address.section,
    )
    .map_err(|error| error.code().to_owned())?;
    let citation_set =
        match spec::section_citation_set(section, &address.scope, &nodes, &edges, &cited) {
            Ok(set) => set,
            Err(refusal) => return Ok(DraftingOutcome::none(refusal.code())),
        };
    let graph_content_hash = spec::graph_content_hash(&nodes, &edges);
    let draft = match admit(&address, &citation_set, finish) {
        Ok(draft) => draft,
        Err(refusal) => return Ok(DraftingOutcome::none(refusal.code())),
    };
    // SPEC-16 rule 3 (AC-0683): a statement that spells out a file path or
    // a code symbol of a fact it cites is not staged. The identifiers stay
    // in the citation and the fact index.
    let source = spec::SourceIdentifiers::of(&nodes, &edges, &cited);
    let claims = draft
        .claims
        .iter()
        .map(|claim| (claim.statement.as_str(), claim.citations.as_slice()));
    if source.first_naming(claims).is_some() {
        return Ok(DraftingOutcome::none("statement_names_source"));
    }
    let origin = DraftOrigin {
        role: SECTION_WRITER_ROLE.to_owned(),
        role_definition: SpecialistId::SectionWriterV1
            .definition()
            .map_err(|e| e.to_string())?
            .prompt_fingerprint,
        job_id: summary.job_id,
        graph_content_hash,
    };
    let mut store = state.drafts.lock().map_err(|e| e.to_string())?;
    let replaces = predecessor(&store, &address, (&nodes, &edges)).map_err(|e| e.to_string())?;
    match store.store(&draft, &origin, replaces.as_deref()) {
        Ok(stored) => Ok(DraftingOutcome::Staged {
            draft_id: stored.draft_id,
        }),
        Err(DraftStoreError::Storage(error)) => Err(error.to_string()),
        Err(_) => Ok(DraftingOutcome::none("draft_not_storable")),
    }
}

/// Called by the worker when a drafting run ends. A failure is logged and
/// left for the next `section_draft_outcome` read; it never fails the run.
pub(crate) fn stage_after_run<R: tauri::Runtime>(app: &tauri::AppHandle<R>, id: &str) {
    let state = app.state::<AppState>();
    if let Err(error) = stage(&state, id) {
        eprintln!("section draft of investigation {id} was not staged: {error}");
    }
}

/// The question that starts a drafting run for one address.
#[tauri::command]
pub(crate) async fn section_draft_question(
    catalogue_version: String,
    document: String,
    section: String,
    scope: String,
) -> Result<String, String> {
    drafting_question(&catalogue_version, &document, &section, &scope).map_err(str::to_owned)
}

/// The question that starts a re-draft of one accepted draft (SPEC-17).
#[tauri::command]
pub(crate) async fn section_redraft_question(
    draft_id: String,
    app: tauri::AppHandle,
) -> Result<String, String> {
    crate::off_ui_thread(move || redraft_question(&app.state::<AppState>(), &draft_id)).await
}

/// What the drafting run `investigation_id` produced. Stages the draft of a
/// completed run that has none stored yet.
#[tauri::command]
pub(crate) async fn section_draft_outcome(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<DraftingOutcome, String> {
    crate::off_ui_thread(move || stage(&app.state::<AppState>(), &investigation_id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0668: a drafting run names one drafted section of the table, under
    // its catalogue title, and a subject only where the section has one.
    #[test]
    fn a_drafting_question_names_one_drafted_section() {
        let text = drafting_question("1.0", "D1", "1.3", spec::SYSTEM_SCOPE).unwrap();
        assert_eq!(
            text,
            "Draft section 1.3 \"Capabilities\" of document D1 (catalogue 1.0) for scope system."
        );
        let address = drafting_address(&text).unwrap();
        assert_eq!(
            (address.document.as_str(), address.section.as_str()),
            ("D1", "1.3")
        );
        assert!(drafting_question("1.0", "D1", "1.3", "F-0123456789abcdef").is_ok());

        for (version, document, section, scope, reason) in [
            ("1.0", "D1", "1.6", "system", "section_not_drafted"),
            ("1.0", "D1", "1.7", "system", "section_not_drafted"),
            ("1.0", "D2", "2.1", "system", "section_not_drafted"),
            ("0.9", "D1", "1.3", "system", "section_not_drafted"),
            (
                "1.0",
                "D1",
                "1.1",
                "F-0123456789abcdef",
                "section_not_per_subject",
            ),
            ("1.0", "D1", "1.3", "everything", "question_not_an_address"),
        ] {
            assert_eq!(
                drafting_question(version, document, section, scope),
                Err(reason),
                "{document} {section} {scope}"
            );
        }
        // The catalogue title is part of the one spelling.
        assert_eq!(
            drafting_address(
                "Draft section 1.3 \"Anything\" of document D1 (catalogue 1.0) for scope system."
            ),
            Err("question_not_an_address")
        );
    }

    fn draft(section: &str, statement: &str) -> agents::section_draft::SectionDraft {
        use agents::section_draft::{Claim, ClaimKind, SYSTEM_SCOPE};
        agents::section_draft::SectionDraft {
            address: SectionAddress {
                catalogue_version: "1.0".into(),
                document: "D1".into(),
                section: section.into(),
                scope: SYSTEM_SCOPE.into(),
            },
            claims: vec![Claim {
                statement: statement.into(),
                kind: ClaimKind::Implemented,
                citations: vec!["F-0000000000000001".into()],
            }],
            cited_hashes: std::collections::BTreeMap::from([(
                "F-0000000000000001".to_owned(),
                "hash-1".to_owned(),
            )]),
        }
    }

    fn origin(job_id: i64) -> DraftOrigin {
        DraftOrigin {
            role: SECTION_WRITER_ROLE.to_owned(),
            role_definition: "definition-1".into(),
            job_id,
            graph_content_hash: "graph-1".into(),
        }
    }

    // Rereading a run's outcome returns the draft the run produced: after a
    // reviewer returned it (it must not become its own predecessor), and for
    // a run that produced a successor (it must not store another draft).
    #[test]
    fn rereading_a_run_returns_the_draft_it_produced() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = agents::draft_store::DraftStore::open(dir.path().join("d.sqlite")).unwrap();
        assert_eq!(existing_draft(&store, 1).unwrap(), None);

        let first = store.store(&draft("1.3", "A"), &origin(1), None).unwrap();
        assert_eq!(
            existing_draft(&store, 1).unwrap(),
            Some(first.draft_id.clone())
        );
        store
            .review(&first.draft_id, 0, DraftDecision::Returned, None)
            .unwrap();
        // Returned: the run still maps to its own draft, and no second
        // draft exists for it.
        assert_eq!(
            existing_draft(&store, 1).unwrap(),
            Some(first.draft_id.clone())
        );
        assert_eq!(
            predecessor(&store, &draft("1.3", "A").address, (&[], &[])).unwrap(),
            Some(first.draft_id.clone())
        );

        let second = store
            .store(&draft("1.3", "B"), &origin(2), Some(&first.draft_id))
            .unwrap();
        // The successor's run maps to the successor, where recomputing
        // would now find no predecessor and store a duplicate.
        assert_eq!(existing_draft(&store, 2).unwrap(), Some(second.draft_id));
        assert_eq!(existing_draft(&store, 1).unwrap(), Some(first.draft_id));
        assert_eq!(existing_draft(&store, 3).unwrap(), None);
    }

    // AC-0668: only the drafting role's start is checked.
    #[test]
    fn only_a_drafting_start_is_checked() {
        let text = drafting_question("1.0", "D1", "1.3", spec::SYSTEM_SCOPE).unwrap();
        assert!(validate_start("section-writer@1", &text).is_ok());
        let refused = validate_start("section-writer@1", "What does checkout do?").unwrap_err();
        assert!(refused.contains("question_not_an_address"));
        assert!(!refused.contains("checkout"));
        for role in ["spec-writer@1", "gap-triager@2", "domain-analyst@7"] {
            assert!(validate_start(role, "What does checkout do?").is_ok());
        }
    }
}
