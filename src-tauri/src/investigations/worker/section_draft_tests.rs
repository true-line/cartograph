//! AC-0668 (T-0668): a `section-writer@1` run inside the real worker. A valid
//! finish stages one draft for the requested address; an insufficient or
//! refused finish, an unresolved scope and every other role stage none.

use super::*;
use crate::investigations::section_drafts::{DraftingOutcome, drafting_question, stage};
use agents::draft_store::StoredDraft;
use agents::section_draft::ClaimKind;

fn drafting(mode: Mode, section: &str, scope: &str) -> (Arc<Scripted>, CompletionFixture) {
    drafting_with(mode, section, scope, true)
}

fn drafting_with(
    mode: Mode,
    section: &str,
    scope: &str,
    source: bool,
) -> (Arc<Scripted>, CompletionFixture) {
    let provider = Arc::new(Scripted::new(mode));
    let question = drafting_question(spec::CATALOGUE_VERSION, "D1", section, scope).unwrap();
    let fixture = CompletionFixture::with_question(
        &*provider,
        SpecialistId::SectionWriterV1,
        source,
        true,
        &question,
    );
    fixture.run(provider.clone());
    (provider, fixture)
}

fn stored(state: &AppState) -> Vec<StoredDraft> {
    state.drafts.lock().unwrap().list(50, None).unwrap().items
}

fn status(state: &AppState, id: &str) -> InvestigationStatus {
    let jobs = state.jobs.lock().unwrap();
    jobs.investigation(id).unwrap().summary.status
}

#[test]
fn a_valid_finish_stages_one_draft_for_the_requested_address() {
    let (_provider, fixture) = drafting(Mode::CompletionSupported, "1.5", spec::SYSTEM_SCOPE);
    let state = fixture.base.app.state::<AppState>();
    assert_eq!(status(&state, &fixture.id), InvestigationStatus::Completed);
    let (detail, result) = {
        let jobs = state.jobs.lock().unwrap();
        (
            jobs.investigation(&fixture.id).unwrap(),
            jobs.investigation_result(&fixture.id).unwrap().unwrap(),
        )
    };
    // The run is an ordinary generation-four task of its own role.
    assert_eq!(detail.summary.schema_version, 4);
    assert_eq!(detail.summary.specialist_id, SpecialistId::SectionWriterV1);
    assert_eq!(detail.specialist.version, 1);
    assert_eq!(result.findings.len(), 1);

    // The worker staged the draft when the run ended.
    let drafts = stored(&state);
    assert_eq!(drafts.len(), 1);
    let draft = &drafts[0];
    assert_eq!(
        draft.draft.address.catalogue_version,
        spec::CATALOGUE_VERSION
    );
    assert_eq!(draft.draft.address.document, "D1");
    assert_eq!(draft.draft.address.section, "1.5");
    assert_eq!(draft.draft.address.scope, spec::SYSTEM_SCOPE);
    assert_eq!(draft.origin.role, "section-writer@1");
    assert_eq!(
        draft.origin.role_definition,
        detail.specialist.prompt_fingerprint
    );
    assert_eq!(draft.origin.job_id, detail.summary.job_id);
    assert_eq!(draft.replaces, None);
    assert_eq!(draft.review_decision, None);

    // One claim per finding, citing the fact by its ADR-0058 id with the
    // hash the graph holds for it.
    let finding = &result.findings[0];
    assert_eq!(draft.draft.claims.len(), 1);
    let claim = &draft.draft.claims[0];
    assert_eq!(claim.statement, finding.statement);
    assert_eq!(claim.kind, ClaimKind::Interpretation);
    let cited: std::collections::BTreeSet<String> = detail
        .citations
        .iter()
        .filter(|citation| finding.citation_ids.contains(&citation.citation_id))
        .map(|citation| match &citation.fact {
            agents::TaskFactKey::Node { id } => {
                spec::citation_id(&core_graph::FactKey::Node { id: id.clone() })
            }
            agents::TaskFactKey::Edge {
                source,
                label,
                destination,
            } => spec::citation_id(&core_graph::FactKey::Edge {
                source: source.clone(),
                label: label.clone(),
                destination: destination.clone(),
            }),
        })
        .collect();
    assert!(!cited.is_empty());
    assert_eq!(
        claim
            .citations
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        cited
    );
    let (nodes, edges) = {
        let graph = state.graph.lock().unwrap();
        let (_, (snapshot, _)) = crate::curated::read_graph(&graph).unwrap();
        snapshot
    };
    assert_eq!(
        draft.draft.cited_hashes,
        spec::cited_fact_hashes(&nodes, &edges, &cited)
    );
    assert_eq!(
        draft.origin.graph_content_hash,
        spec::graph_content_hash(&nodes, &edges)
    );

    // Reading the outcome again stores nothing new.
    assert_eq!(
        stage(&state, &fixture.id),
        Ok(DraftingOutcome::Staged {
            draft_id: draft.draft_id.clone()
        })
    );
    assert_eq!(stored(&state).len(), 1);
}

#[test]
fn an_insufficient_evidence_finish_stages_no_draft() {
    // The inventory read is attempted and unavailable, so the role finishes
    // with no findings.
    let (_provider, fixture) = drafting_with(
        Mode::CompletionInventoryUnavailable,
        "1.5",
        spec::SYSTEM_SCOPE,
        false,
    );
    let state = fixture.base.app.state::<AppState>();
    assert_eq!(status(&state, &fixture.id), InvestigationStatus::Completed);
    assert!(stored(&state).is_empty());
    assert_eq!(
        stage(&state, &fixture.id),
        Ok(DraftingOutcome::NoDraft {
            reason: "insufficient_evidence"
        })
    );
    assert!(stored(&state).is_empty());
}

#[test]
fn a_proposed_design_finish_is_refused_in_the_run_and_stages_no_draft() {
    let (_provider, fixture) = drafting(Mode::SpecWriterSupported, "1.5", spec::SYSTEM_SCOPE);
    let state = fixture.base.app.state::<AppState>();
    // The existing finish refusal ends the run without a result.
    assert_ne!(status(&state, &fixture.id), InvestigationStatus::Completed);
    assert!(
        state
            .jobs
            .lock()
            .unwrap()
            .investigation_result(&fixture.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        stage(&state, &fixture.id),
        Ok(DraftingOutcome::NoDraft {
            reason: "run_not_completed"
        })
    );
    assert!(stored(&state).is_empty());
}

#[test]
fn a_claim_outside_the_declared_scope_stages_no_draft() {
    // A subject the graph does not hold: the scope has no citation set.
    let (_provider, fixture) = drafting(Mode::CompletionSupported, "1.5", "F-0000000000000000");
    let state = fixture.base.app.state::<AppState>();
    assert_eq!(status(&state, &fixture.id), InvestigationStatus::Completed);
    assert_eq!(
        stage(&state, &fixture.id),
        Ok(DraftingOutcome::NoDraft {
            reason: "subject_not_found"
        })
    );
    assert!(stored(&state).is_empty());
}

#[test]
fn another_role_stages_no_draft() {
    let provider = Arc::new(Scripted::new(Mode::SpecWriterSupported));
    let question =
        drafting_question(spec::CATALOGUE_VERSION, "D1", "1.5", spec::SYSTEM_SCOPE).unwrap();
    let fixture = CompletionFixture::with_question(
        &*provider,
        SpecialistId::SpecWriterV1,
        true,
        true,
        &question,
    );
    fixture.run(provider.clone());
    let state = fixture.base.app.state::<AppState>();
    assert_eq!(status(&state, &fixture.id), InvestigationStatus::Completed);
    assert_eq!(
        stage(&state, &fixture.id),
        Ok(DraftingOutcome::NoDraft {
            reason: "not_a_drafting_run"
        })
    );
    assert!(stored(&state).is_empty());
}
