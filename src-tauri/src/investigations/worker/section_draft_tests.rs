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

// AC-0674 (T-0674): a re-draft is requested only for an accepted draft
// whose cited facts drifted, and the draft a later run stages for the
// address names that accepted draft as the one it replaces.
#[test]
fn a_stale_accepted_draft_is_re_drafted_as_its_successor() {
    use crate::investigations::section_drafts::{predecessor, redraft_question};
    use agents::draft_store::{DraftDecision, DraftOrigin};

    let (_provider, fixture) = drafting(Mode::CompletionSupported, "1.5", spec::SYSTEM_SCOPE);
    let state = fixture.base.app.state::<AppState>();
    let staged = stored(&state).remove(0);
    let question =
        drafting_question(spec::CATALOGUE_VERSION, "D1", "1.5", spec::SYSTEM_SCOPE).unwrap();
    let snapshot = || {
        let graph = state.graph.lock().unwrap();
        let (_, (snapshot, _)) = crate::curated::read_graph(&graph).unwrap();
        snapshot
    };
    let replaced = |state: &AppState| {
        let (nodes, edges) = snapshot();
        let store = state.drafts.lock().unwrap();
        predecessor(&store, &staged.draft.address, (&nodes, &edges)).unwrap()
    };

    // Closed refusals: unknown, not accepted, accepted and current.
    assert_eq!(
        redraft_question(&state, &"0".repeat(64)),
        Err("unknown_draft".to_owned())
    );
    assert_eq!(
        redraft_question(&state, &staged.draft_id),
        Err("draft_not_accepted".to_owned())
    );
    let mut drafts = state.drafts.lock().unwrap();
    drafts
        .review(&staged.draft_id, 0, DraftDecision::Accepted, None)
        .unwrap();
    drop(drafts);
    assert_eq!(
        redraft_question(&state, &staged.draft_id),
        Err("draft_not_stale".to_owned())
    );
    // A current accepted draft is not replaced by a new run either.
    assert_eq!(replaced(&state), None);

    // A cited fact changes: its content hash moves.
    let cited = staged.draft.cited_hashes.keys().next().unwrap().clone();
    let (nodes, _) = snapshot();
    let mut node = nodes
        .into_iter()
        .find(|node| {
            spec::citation_id(&core_graph::FactKey::Node {
                id: node.id.clone(),
            }) == cited
        })
        .expect("the draft cites a node");
    let recorded = node.props["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    node.props["prov"]["content_hash"] =
        serde_json::json!(recorded.chars().rev().collect::<String>());
    state.graph.lock().unwrap().put_node(&node).unwrap();

    // The request is the ordinary drafting question for the same address,
    // and the draft the run stages will name the accepted draft.
    assert_eq!(redraft_question(&state, &staged.draft_id), Ok(question));
    assert_eq!(replaced(&state), Some(staged.draft_id.clone()));

    // A successor that stands blocks another request; the accepted draft
    // is unchanged.
    let successor = {
        let mut draft = staged.draft.clone();
        draft.claims[0].statement = "Rewritten against the changed fact.".into();
        let origin = DraftOrigin {
            job_id: staged.origin.job_id + 1,
            ..staged.origin.clone()
        };
        let mut drafts = state.drafts.lock().unwrap();
        drafts
            .store(&draft, &origin, Some(&staged.draft_id))
            .unwrap()
    };
    assert_eq!(
        successor.replaces.as_deref(),
        Some(staged.draft_id.as_str())
    );
    assert_eq!(
        redraft_question(&state, &staged.draft_id),
        Err("successor_pending".to_owned())
    );
    assert_eq!(replaced(&state), None);
    let kept = state
        .drafts
        .lock()
        .unwrap()
        .get(&staged.draft_id)
        .unwrap()
        .unwrap();
    assert_eq!(kept.review_decision, Some(DraftDecision::Accepted));

    // Rejecting the successor allows another re-draft.
    state
        .drafts
        .lock()
        .unwrap()
        .review(&successor.draft_id, 0, DraftDecision::Rejected, None)
        .unwrap();
    assert!(redraft_question(&state, &staged.draft_id).is_ok());
    assert_eq!(replaced(&state), Some(staged.draft_id.clone()));
}

// AC-0668: a draft is staged only against the graph snapshot its run froze.
// A cited fact that keeps its id but changes content after the run (a
// re-ingest, or a retry after the store failed) stages nothing.
#[test]
fn a_cited_fact_changed_after_the_run_stages_no_draft() {
    let (_provider, fixture) = drafting(Mode::CompletionSupported, "1.5", spec::SYSTEM_SCOPE);
    let state = fixture.base.app.state::<AppState>();
    assert_eq!(status(&state, &fixture.id), InvestigationStatus::Completed);
    // The store lost the draft the worker staged, so the next read stages
    // again; against the unchanged graph it does.
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let fresh = |state: &AppState, dir: &tempfile::TempDir| {
        *state.drafts.lock().unwrap() =
            agents::draft_store::DraftStore::open(dir.path().join("drafts.sqlite")).unwrap();
    };
    fresh(&state, &dirs[0]);
    assert!(matches!(
        stage(&state, &fixture.id),
        Ok(DraftingOutcome::Staged { .. })
    ));
    fresh(&state, &dirs[1]);

    // Change the content of a fact the finish cites, keeping its id.
    let cited = {
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        detail
            .citations
            .iter()
            .filter(|citation| {
                result.findings[0]
                    .citation_ids
                    .contains(&citation.citation_id)
            })
            .find_map(|citation| match &citation.fact {
                agents::TaskFactKey::Node { id } => Some(id.clone()),
                agents::TaskFactKey::Edge { .. } => None,
            })
            .unwrap()
    };
    {
        let mut graph = state.graph.lock().unwrap();
        let expected = graph.read_snapshot().unwrap();
        let mut node = expected
            .0
            .iter()
            .find(|node| node.id == cited)
            .unwrap()
            .clone();
        node.props["changed_after_run"] = serde_json::json!(true);
        let patch = core_graph::GraphPatch {
            upsert_nodes: vec![node],
            ..Default::default()
        };
        assert!(
            graph
                .apply_patch_if_snapshot_matches(&expected, &patch)
                .unwrap()
        );
    }
    assert_eq!(
        stage(&state, &fixture.id),
        Ok(DraftingOutcome::NoDraft {
            reason: "graph_changed"
        })
    );
    assert!(stored(&state).is_empty());
}
