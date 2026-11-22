// ADR-0072 slice 3 (AC-1072): explicit host reconciliation of staged records.
use super::*;
use crate::registered_source_tests::app_state;
use crate::task_evidence::tests::{
    GAP, ORIGINAL, RecordingProvider, capture, directory, install_task, rule_pair,
};
use agents::reconciliation::{ItemOutcome, ReconciliationOutcome};
use core_graph::GraphStore;
use serde_json::json;
use std::sync::Mutex;

/// Stage one captured (v2) proposal the way an escalation job does.
fn staged_proposal(state: &AppState) -> StagedProposal {
    let prepared = prepare(state, GAP, "task:reconcile").unwrap();
    let selected = &prepared.task().candidates[0];
    let provider = RecordingProvider {
        response: json!({
            "target_id": selected.node_id,
            "annotation": "Possible lexical connection",
            "citations": [prepared.task().source_evidence_ids[0], selected.evidence_ids[0]],
        })
        .to_string(),
        seen: Mutex::new(Vec::new()),
    };
    let broker = agents::AgentBroker::bounded_default();
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let proposal = broker
        .propose_prepared(&provider, &firewall, &prepared, None)
        .unwrap();
    state
        .proposals
        .lock()
        .unwrap()
        .stage_prepared(
            &prepared,
            &proposal,
            1,
            &prepared.source_basis().graph_snapshot_id,
        )
        .unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    app_data: std::path::PathBuf,
    root: std::path::PathBuf,
    state: AppState,
    source: RegisteredSource,
    staged: StagedProposal,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&app_data);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, candidate) = rule_pair(&captured);
    install_task(&state, &owner, &[candidate]);
    let staged = staged_proposal(&state);
    Fixture {
        _dir: dir,
        app_data,
        root,
        state,
        source,
        staged,
    }
}

fn result_rows(state: &AppState, id: &str) -> usize {
    state
        .proposals
        .lock()
        .unwrap()
        .reconciliations(id, 64)
        .unwrap()
        .len()
}

#[test]
fn an_untouched_recovery_reconciles_and_survives_restart_and_review() {
    let Fixture {
        _dir,
        app_data,
        state,
        staged,
        ..
    } = fixture();
    let id = &staged.proposal_id;
    assert_eq!(
        history(&state, id, 64).unwrap().state,
        ReconciliationState::AwaitingReconciliation
    );
    let recorded = reconcile(&state, id, 41).unwrap();
    let result = &recorded.result;
    assert_eq!(result.outcome, ReconciliationOutcome::Reconciled);
    assert_eq!(result.recovery.job_id, 41);
    assert!(!result.recovery.captures.is_empty());
    assert!(
        result
            .evidence
            .iter()
            .all(|item| item.outcome == ItemOutcome::RetainedMatch && item.reconciled.is_some())
    );
    assert_eq!(
        history(&state, id, 64).unwrap().state,
        ReconciliationState::Reconciled
    );
    // Metadata only: none of the supplied source text is in the result.
    let json = String::from_utf8(result.canonical_bytes().unwrap()).unwrap();
    for line in ORIGINAL.lines().filter(|line| line.len() > 12) {
        assert!(!json.contains(line.trim()), "{line}");
    }
    // Review moves neither the record nor its state.
    let reviewed = state
        .proposals
        .lock()
        .unwrap()
        .review(id, 0, agents::ProposalDecision::Accepted, None)
        .unwrap();
    assert_eq!(reviewed.context_status, staged.context_status);
    assert_eq!(reviewed.evidence_binding, staged.evidence_binding);
    assert_eq!(reviewed.basis, staged.basis);
    assert_eq!(reviewed.source_basis, staged.source_basis);
    drop(state);
    let reopened = app_state(&app_data);
    let after = history(&reopened, id, 64).unwrap();
    assert_eq!(after.state, ReconciliationState::Reconciled);
    assert_eq!(after.results, vec![recorded]);
}

#[test]
fn a_later_graph_change_makes_any_result_stale() {
    let fixture = fixture();
    let (state, id) = (&fixture.state, &fixture.staged.proposal_id);
    reconcile(state, id, 1).unwrap();
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "doc:unrelated".into(),
            label: "Document".into(),
            props: json!({}),
        })
        .unwrap();
    assert_eq!(
        history(state, id, 64).unwrap().state,
        ReconciliationState::ReconciliationStale
    );
    // Reconciling again reads the new recovery and is current again.
    reconcile(state, id, 2).unwrap();
    let again = history(state, id, 64).unwrap();
    assert_eq!(again.state, ReconciliationState::Reconciled);
    assert_eq!(again.results.len(), 2);
    assert_eq!(again.results[0].result.recovery.job_id, 2);
}

#[test]
fn an_edited_cited_source_is_changed_or_missing_never_reconciled() {
    let fixture = fixture();
    let (state, id) = (&fixture.state, &fixture.staged.proposal_id);
    std::fs::write(
        fixture.root.join("source.ts"),
        // Same length, so every fact keeps its identity and offsets.
        ORIGINAL.replacen("return false;", "return !true;", 1),
    )
    .unwrap();
    capture(state, &fixture.source, true);
    let result = reconcile(state, id, 3).unwrap().result;
    assert_eq!(result.outcome, ReconciliationOutcome::Changed);
    assert!(
        result
            .evidence
            .iter()
            .any(|item| matches!(item.outcome, ItemOutcome::Changed { .. }))
    );
}

#[test]
fn a_failed_read_records_nothing() {
    let fixture = fixture();
    let (state, id) = (&fixture.state, &fixture.staged.proposal_id);
    assert!(reconcile(state, "stage-v2:unknown", 1).is_err());
    assert_eq!(result_rows(state, id), 0);
}

#[test]
fn a_reversed_slot_edge_is_not_the_same_relation() {
    let fixture = fixture();
    let (state, staged) = (&fixture.state, &fixture.staged);
    {
        let mut graph = state.graph.lock().unwrap();
        graph
            .delete_edge(&staged.basis.source_id, GAP, "CALLS")
            .unwrap();
        graph
            .put_edge(&Edge {
                src: GAP.into(),
                dst: staged.basis.source_id.clone(),
                label: "CALLS".into(),
                props: json!({}),
            })
            .unwrap();
    }
    let result = reconcile(state, &staged.proposal_id, 1).unwrap().result;
    assert_eq!(
        result.slot.relation,
        agents::reconciliation::SlotPresence::Missing
    );
    assert_eq!(result.outcome, ReconciliationOutcome::Missing);
}

#[test]
fn v2_origins_bind_to_their_items_by_evidence_id_not_order() {
    let fixture = fixture();
    let mut reordered = fixture.staged.clone();
    reordered.source_basis.as_mut().unwrap().evidence.reverse();
    assert_eq!(item_facts(&reordered), item_facts(&fixture.staged));
    let first = &fixture.staged.basis.evidence[0].id;
    let origin = origin_of(reordered.source_basis.as_ref().unwrap(), first).unwrap();
    assert_eq!(&origin.evidence_id, first);
}
