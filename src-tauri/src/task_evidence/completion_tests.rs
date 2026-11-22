use super::assessment::{Availability, BasisComparison, assess};
use super::tests::{
    GAP, ORIGINAL, capture, directory, install_task, replace_current_receipts, rule_pair,
};
use super::*;
use crate::registered_source_tests::app_state;
use llm::{
    Completion, EgressFirewall, EgressPolicy, Embedding, LlmProvider, Locality, ProviderCaps,
    ProviderCompletionRequest, ProviderError,
};
use std::sync::Mutex;

struct DuringCall<F> {
    task: AgentTask,
    expect_prepared: bool,
    change: Mutex<Option<F>>,
}

impl<F: FnOnce() + Send> LlmProvider for DuringCall<F> {
    fn id(&self) -> &str {
        "test:captured-task-completion"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        Err(ProviderError::Unsupported("embeddings"))
    }
    fn complete(&self, request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        // This is the actual firewall-authorized, redacted provider request.
        assert_eq!(request.spans().len(), self.task.evidence.len());
        for (span, original) in request.spans().iter().zip(&self.task.evidence) {
            assert_eq!(span.text, original.text);
            assert_eq!(span.byte_start, original.source.byte_start);
            assert_eq!(span.byte_end, original.source.byte_end);
        }
        assert_eq!(
            request.prompt().contains("source_basis"),
            self.expect_prepared
        );
        self.change.lock().unwrap().take().expect("one model call")();
        Ok(Completion { text: serde_json::json!({
            "target_id": self.task.candidates[0].node_id,
            "annotation": "The cited observations suggest a relationship for review.",
            "citations": [self.task.source_evidence_ids[0], self.task.candidates[0].evidence_ids[0]],
        }).to_string() })
    }
}

fn export(state: &AppState) -> serde_json::Value {
    serde_json::to_value(
        crate::build_spec_bundle(
            &state.graph.lock().unwrap(),
            &state.decisions.lock().unwrap(),
            &crate::curated::CuratedReview::default(),
            spec::ExportMode::BestEffort,
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn completed_captured_task_keeps_old_available_receipts_after_equal_fact_reingest() {
    // AC-0175/0176/0177/0178: receipt-only publication changes the current
    // association comparison without making historical captured bytes unavailable.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let path = root.join("source.ts");
    std::fs::write(&path, ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let task = prepare(&state, GAP, "escalate:fixture").unwrap();
    let original_basis = task.source_basis().clone();
    let before = export(&state);
    let (_, execution) = crate::start_job(&state, "escalate:captured:local").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {
            std::fs::write(&path, ORIGINAL.replace("trailing-A", "trailing-B")).unwrap();
            let next = capture(&state, &source, false);
            replace_current_receipts(&state, &source, &next);
        })),
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose_prepared(
            &provider,
            &EgressFirewall::new(EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    let staged =
        crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal).unwrap();
    assert_eq!(staged.source_basis.as_ref(), Some(&original_basis));
    let changed = assess(&state, &staged.proposal_id).unwrap();
    assert_eq!(changed.graph_comparison, BasisComparison::Unchanged);
    assert_eq!(changed.association_comparison, BasisComparison::Changed);
    assert!(
        changed
            .evidence
            .iter()
            .all(|item| item.status == Availability::Available)
    );
    let next = prepare(&state, GAP, "escalate:fixture").unwrap();
    assert_ne!(next.basis_hash().unwrap(), task.basis_hash().unwrap());
    assert_eq!(next.task().evidence, task.task().evidence);
    assert_eq!(export(&state), before);
    assert_eq!(
        state
            .proposals
            .lock()
            .unwrap()
            .get(&staged.proposal_id)
            .unwrap()
            .unwrap(),
        staged
    );
}

#[test]
fn completed_captured_task_survives_forgetting_cancel_cleanup_and_graph_clear() {
    // AC-0177/0178: source guards must end before the provider, and history
    // staging may not consult current captures or require a live job row.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let task = prepare(&state, GAP, "escalate:forgotten").unwrap();
    let before_graph = state.graph.lock().unwrap().read_snapshot().unwrap();
    let (job, execution) = crate::start_job(&state, "escalate:forgotten:local").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {
            let preview = crate::primary_source::preview(&state, &source.source_id).unwrap();
            assert_eq!(preview.staged_references, 0);
            crate::primary_source::forget(&state, &source.source_id, &preview.fingerprint).unwrap();
            state.jobs.lock().unwrap().cancel(job.id).unwrap();
            state.jobs.lock().unwrap().clear_finished().unwrap();
        })),
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose_prepared(
            &provider,
            &EgressFirewall::new(EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    let staged =
        crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal).unwrap();
    assert!(state.jobs.lock().unwrap().get(job.id).is_err());
    assert_eq!(staged.source_basis.as_ref(), Some(task.source_basis()));
    assert!(
        !serde_json::to_string(&staged)
            .unwrap()
            .contains("const disabled")
    );
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        before_graph
    );
    let unavailable = assess(&state, &staged.proposal_id).unwrap();
    assert_eq!(unavailable.graph_comparison, BasisComparison::Unchanged);
    assert_eq!(
        unavailable.association_comparison,
        BasisComparison::Unchanged
    );
    assert!(
        unavailable
            .evidence
            .iter()
            .all(|item| item.status == Availability::Unavailable)
    );
    let preview = crate::primary_source::preview(&state, &source.source_id).unwrap();
    assert_eq!(preview.staged_references, task.task().evidence.len());
    let reviewed = state
        .proposals
        .lock()
        .unwrap()
        .review(
            &staged.proposal_id,
            0,
            agents::ProposalDecision::Accepted,
            None,
        )
        .unwrap();
    assert_eq!(reviewed.source_basis, staged.source_basis);
    assert_eq!(
        reviewed.proposal.provenance.confidence_tier,
        ConfidenceTier::InferredWeak
    );
    assert_eq!(
        reviewed.context_status,
        agents::ContextStatus::AwaitingReconciliation
    );
    {
        let mut graph = state.graph.lock().unwrap();
        use core_graph::GraphStore;
        for node in before_graph.0 {
            graph.delete_node(&node.id).unwrap();
        }
    }
    let changed = assess(&state, &staged.proposal_id).unwrap();
    assert_eq!(changed.graph_comparison, BasisComparison::Changed);
    assert_eq!(changed.association_comparison, BasisComparison::Changed);
    assert_eq!(
        state
            .proposals
            .lock()
            .unwrap()
            .get(&staged.proposal_id)
            .unwrap()
            .unwrap(),
        reviewed
    );
    assert!(state.decisions.lock().unwrap().list().unwrap().is_empty());
}

#[test]
fn captured_class_prepares_only_reached_instances_and_preserves_completed_results() {
    // AC-0176/0178: the same generic production loop holds only gap IDs. A
    // cancellation during the first call prevents the next source preparation.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let (job, execution) = crate::start_job(&state, "escalate-class:2:local").unwrap();
    let mut preparations = 0;
    let outcome = crate::run_job_staged_batch(
        &state,
        &execution,
        vec![
            (GAP.into(), Ok(GAP.to_string())),
            ("unread".into(), Ok("unread".to_string())),
        ],
        |_, _| Ok(()),
        |gap_id| {
            preparations += 1;
            let task = prepare(&state, gap_id, "escalate:class")?;
            let provider = DuringCall {
                task: task.task().clone(),
                expect_prepared: true,
                change: Mutex::new(Some(|| {
                    state.jobs.lock().unwrap().cancel(job.id).unwrap();
                })),
            };
            let proposal = agents::AgentBroker::bounded_default()
                .propose_prepared(
                    &provider,
                    &EgressFirewall::new(EgressPolicy::local_only()),
                    &task,
                    None,
                )
                .map_err(|error| error.to_string())?;
            crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal)
        },
    )
    .unwrap();
    assert_eq!(preparations, 1);
    assert!(outcome.cancelled);
    assert!(outcome.failures.is_empty());
    assert_eq!(outcome.proposals.len(), 1);
}

#[test]
fn selected_basis_assessment_distinguishes_storage_failure_and_legacy_unverified() {
    // AC-0177: current graph storage failure is neither graph equality nor
    // missing retained source; v1 assessment never rereads a working tree.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let task = prepare(&state, GAP, "escalate:assessment").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {})),
    };
    let broker = agents::AgentBroker::bounded_default();
    let firewall = EgressFirewall::new(EgressPolicy::local_only());
    let proposal = broker
        .propose_prepared(&provider, &firewall, &task, None)
        .unwrap();
    let staged = state
        .proposals
        .lock()
        .unwrap()
        .stage_prepared(&task, &proposal, 1, &task.source_basis().graph_snapshot_id)
        .unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: false,
        change: Mutex::new(Some(|| {})),
    };
    let legacy_result = broker
        .propose(&provider, &firewall, task.task(), None)
        .unwrap();
    let legacy = state
        .proposals
        .lock()
        .unwrap()
        .stage(
            task.task(),
            &legacy_result,
            2,
            &task.source_basis().graph_snapshot_id,
        )
        .unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    // Poisoning represents an operational failure, not an absent association.
    // It does not mutate graph rows or retained receipts.
    std::thread::scope(|scope| {
        assert!(
            scope
                .spawn(|| {
                    let _graph = state.graph.lock().unwrap();
                    panic!("controlled graph-lock failure");
                })
                .join()
                .is_err()
        );
    });
    let observed = assess(&state, &staged.proposal_id).unwrap();
    assert_eq!(
        observed.graph_comparison,
        BasisComparison::OperationalFailure
    );
    assert_eq!(
        observed.association_comparison,
        BasisComparison::OperationalFailure
    );
    assert!(observed.current_graph_snapshot_id.is_none());
    assert!(
        observed
            .evidence
            .iter()
            .all(|item| item.status == Availability::Available)
    );
    let old = assess(&state, &legacy.proposal_id).unwrap();
    assert_eq!(old.graph_comparison, BasisComparison::LegacyUnverified);
    assert_eq!(
        old.association_comparison,
        BasisComparison::LegacyUnverified
    );
    assert_eq!(old.unverified_evidence, legacy.basis.evidence.len());
    assert_eq!(old.captured_validation_bytes, 0);
    assert!(
        old.evidence
            .iter()
            .all(|item| item.status == Availability::WorkingTreeUnverified)
    );
    assert_eq!(
        state
            .proposals
            .lock()
            .unwrap()
            .get(&legacy.proposal_id)
            .unwrap()
            .unwrap(),
        legacy
    );
}

#[test]
fn prepared_task_pins_its_receipts_against_a_supersession_reclaim() {
    // #762 review: a newer publish may reclaim superseded receipts between
    // preparation and staging; the receipts the task cites must survive.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let path = root.join("source.ts");
    std::fs::write(&path, ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let task = prepare(&state, GAP, "escalate:superseded").unwrap();
    let cited: Vec<String> = task
        .source_basis()
        .evidence
        .iter()
        .filter_map(|item| match &item.origin {
            TaskEvidenceKind::CapturedPrimarySource { receipt_id, .. } => Some(receipt_id.clone()),
            _ => None,
        })
        .collect();
    assert!(!cited.is_empty());
    let status = |id: &str| {
        state
            .primary_sources
            .receipts
            .lock()
            .unwrap()
            .receipt_status(id)
            .unwrap()
    };
    let (_, execution) = crate::start_job(&state, "escalate:superseded:local").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {
            std::fs::write(&path, ORIGINAL.replace("trailing-A", "trailing-B")).unwrap();
            let next = capture(&state, &source, false);
            replace_current_receipts(&state, &source, &next);
            // Every superseded receipt but the cited ones is reclaimed.
            assert!(
                crate::primary_source::reclaim_superseded(&state, &source, "next").unwrap() > 0
            );
            for id in &cited {
                assert_eq!(status(id), crate::primary_source::ReceiptStatus::Present);
            }
        })),
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose_prepared(
            &provider,
            &EgressFirewall::new(EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    let staged =
        crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal).unwrap();
    drop(task);
    // Once staged, the proposal's own pin keeps the receipts.
    assert_eq!(
        crate::primary_source::reclaim_superseded(&state, &source, "next").unwrap(),
        0
    );
    for id in &cited {
        assert_eq!(status(id), crate::primary_source::ReceiptStatus::Present);
    }
    let available = assess(&state, &staged.proposal_id).unwrap();
    assert!(
        available
            .evidence
            .iter()
            .all(|item| item.status == Availability::Available)
    );
}

#[test]
fn a_task_staged_between_reclaim_snapshots_keeps_its_receipts() {
    // #762 review: a task may stage and drop its in-flight pin while a
    // supersession reclaim is between its keep-set snapshots; one of the
    // two snapshots must still hold the receipts.
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let path = root.join("source.ts");
    std::fs::write(&path, ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let task = prepare(&state, GAP, "escalate:handoff").unwrap();
    let cited: Vec<String> = task
        .source_basis()
        .evidence
        .iter()
        .filter_map(|item| match &item.origin {
            TaskEvidenceKind::CapturedPrimarySource { receipt_id, .. } => Some(receipt_id.clone()),
            _ => None,
        })
        .collect();
    assert!(!cited.is_empty());
    let (_, execution) = crate::start_job(&state, "escalate:handoff:local").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {
            std::fs::write(&path, ORIGINAL.replace("trailing-A", "trailing-B")).unwrap();
            let next = capture(&state, &source, false);
            replace_current_receipts(&state, &source, &next);
        })),
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose_prepared(
            &provider,
            &EgressFirewall::new(EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    let mut staged = None;
    let reclaimed =
        crate::primary_source::reclaim_superseded_with(&state, &source, Some("next"), None, || {
            staged = Some(
                crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal)
                    .unwrap(),
            );
            drop(task);
        })
        .unwrap();
    assert!(reclaimed > 0);
    let receipts = state.primary_sources.receipts.lock().unwrap();
    for id in &cited {
        assert_eq!(
            receipts.receipt_status(id).unwrap(),
            crate::primary_source::ReceiptStatus::Present
        );
    }
    drop(receipts);
    let available = assess(&state, &staged.unwrap().proposal_id).unwrap();
    assert!(
        available
            .evidence
            .iter()
            .all(|item| item.status == Availability::Available)
    );
}

fn curated_export(state: &AppState, mode: spec::ExportMode) -> spec::SpecBundle {
    // The order `export_spec` uses: the review first, then the graph and
    // decision locks.
    let review = crate::curated::gather(state);
    crate::build_spec_bundle(
        &state.graph.lock().unwrap(),
        &state.decisions.lock().unwrap(),
        &review,
        mode,
    )
    .unwrap()
}

fn artifact<'a>(bundle: &'a spec::SpecBundle, id: &str) -> &'a spec::SpecArtifact {
    bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == id)
        .unwrap()
}

/// The `curated.md` table row of one record.
fn curated_row<'a>(bundle: &'a spec::SpecBundle, record_id: &str) -> Option<&'a str> {
    artifact(bundle, "curated")
        .content
        .lines()
        .find(|line| line.starts_with(&format!("| {record_id} |")))
}

/// A version 2 record staged from captured evidence that is still readable,
/// not yet reviewed. Once accepted it is eligible.
fn staged_captured_record() -> (tempfile::TempDir, AppState, Node, agents::StagedProposal) {
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    let task = prepare(&state, GAP, "escalate:fixture").unwrap();
    let (_, execution) = crate::start_job(&state, "escalate:curated:local").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {})),
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose_prepared(
            &provider,
            &EgressFirewall::new(EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    let staged =
        crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal).unwrap();
    (dir, state, owner, staged)
}

#[test]
fn accepted_captured_record_is_a_curated_item_in_best_effort_and_counted_in_verified_only() {
    // AC-0653 (SPEC-12 slice 2, ADR-0061 decisions 5 to 7): a version 2
    // record whose captured evidence is still readable and whose basis is
    // unchanged is eligible once accepted. A best-effort export shows it as
    // an item marked InferredWeak and cites its nodes; a verified-only export
    // shows no item and counts it. The base graph hash is the same in both
    // and the same as before the review.
    let (_dir, state, owner, staged) = staged_captured_record();
    let id = staged.proposal_id.as_str();
    let modes = [spec::ExportMode::BestEffort, spec::ExportMode::VerifiedOnly];

    // Staged and not reviewed: no accepted record.
    let pending = modes.map(|mode| curated_export(&state, mode));
    for bundle in &pending {
        assert_eq!(bundle.curated_basis.as_ref().unwrap().counts.accepted, 0);
        assert!(curated_row(bundle, id).is_none());
    }
    state
        .proposals
        .lock()
        .unwrap()
        .review(id, 0, agents::ProposalDecision::Accepted, None)
        .unwrap();
    let graph_before = state.graph.lock().unwrap().read_snapshot().unwrap();

    let accepted = modes.map(|mode| curated_export(&state, mode));
    let [best, verified] = &accepted;
    for (before, after) in pending.iter().zip(&accepted) {
        let basis = after.curated_basis.as_ref().unwrap();
        assert_eq!(basis.counts.accepted, 1);
        assert_eq!(basis.counts.eligible, 1, "{:?}", curated_row(after, id));
        // Acceptance wrote nothing to the graph and moved no base hash.
        assert_eq!(before.graph_basis, after.graph_basis);
        assert_ne!(
            before.curated_basis.as_ref().unwrap().revision,
            basis.revision
        );
        // Only `curated.md` and the index of what it cites can differ.
        for (old, new) in before.artifacts.iter().zip(&after.artifacts) {
            if !crate::curated::artifact_reads_projection(&old.id) {
                assert_eq!(old, new, "{} in {:?}", old.id, after.mode);
            }
        }
    }
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap(),
        graph_before
    );
    // One revision on both modes: the mode filters the document, not the
    // projection.
    assert_eq!(best.curated_basis, verified.curated_basis);

    let row = curated_row(best, id).expect("best-effort shows the item");
    assert!(
        row.contains("| Agentic | InferredWeak | accepted |"),
        "{row}"
    );
    assert!(row.contains("| eligible |"), "{row}");
    assert!(row.contains("InferredWeak curated item"), "{row}");
    assert!(
        row.contains(&format!(
            "| {GAP} | {} CALLS {} |",
            owner.id, staged.proposal.target_id
        )) || row.contains(GAP),
        "{row}"
    );
    let cited: Vec<&str> = artifact(best, "curated")
        .assertions
        .iter()
        .map(|assertion| assertion.subject_id.as_str())
        .collect();
    assert!(cited.contains(&GAP), "{cited:?}");
    assert!(cited.contains(&staged.proposal.source_id.as_str()));
    assert!(cited.contains(&staged.proposal.target_id.as_str()));

    assert!(curated_row(verified, id).is_none());
    assert!(!artifact(verified, "curated").content.contains(id));
    assert!(
        artifact(verified, "curated")
            .content
            .contains("Curated items left out: 1.")
    );
    assert!(artifact(verified, "curated").assertions.is_empty());
    // With no item to cite, a verified-only export's fact index is unmoved.
    for id in ["fact-index", "fact-index-json"] {
        assert_eq!(artifact(&pending[1], id), artifact(verified, id));
    }

    // The artifact read alone is the bundle's, in both modes.
    for bundle in &accepted {
        let review = crate::curated::gather(&state);
        for id in ["curated", "fact-index-json"] {
            assert_eq!(
                &crate::build_spec_artifact(
                    &state.graph.lock().unwrap(),
                    &state.decisions.lock().unwrap(),
                    Some(&review),
                    bundle.mode,
                    id,
                )
                .unwrap(),
                artifact(bundle, id),
                "{id} in {:?}",
                bundle.mode
            );
        }
    }

    // An assessment made before the graph moved is not used for the graph an
    // export reads afterwards: the record is listed, not shown as an item.
    let review = crate::curated::gather(&state);
    {
        use core_graph::GraphStore;
        state
            .graph
            .lock()
            .unwrap()
            .put_node(&Node {
                id: "sym:later".into(),
                label: "Symbol".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
    }
    let moved = crate::build_spec_bundle(
        &state.graph.lock().unwrap(),
        &state.decisions.lock().unwrap(),
        &review,
        spec::ExportMode::BestEffort,
    )
    .unwrap();
    let row = curated_row(&moved, id).unwrap();
    assert!(row.contains("| stale |"), "{row}");
    assert!(row.contains("(`assessment_failed`)"), "{row}");
    assert_eq!(moved.curated_basis.as_ref().unwrap().counts.eligible, 0);
    // Read again, the assessment sees the changed graph and says so.
    let reread = curated_export(&state, spec::ExportMode::BestEffort);
    let row = curated_row(&reread, id).unwrap();
    assert!(row.contains("| stale |"), "{row}");
    assert!(row.contains("(`graph_changed`)"), "{row}");
    assert!(artifact(&reread, "curated").assertions.is_empty());

    // Withdrawal is rejection (ADR-0061 decision 9): the record leaves the
    // document and the revision changes.
    state
        .proposals
        .lock()
        .unwrap()
        .review(id, 1, agents::ProposalDecision::Rejected, None)
        .unwrap();
    let rejected = curated_export(&state, spec::ExportMode::BestEffort);
    assert!(curated_row(&rejected, id).is_none());
    let basis = rejected.curated_basis.as_ref().unwrap();
    assert_eq!(basis.counts.accepted, 0);
    assert_ne!(basis.revision, reread.curated_basis.unwrap().revision);
}

/// An MCP read session over the stores of `state`, acquired the way the
/// worker acquires it: the reviews are the ones the export gathers.
fn mcp_session(
    state: &AppState,
    mode: spec::ExportMode,
    scope: impl FnMut() -> Result<Vec<context_hub::coverage::ScopedRepo>, crate::mcp_reads::ReadError>,
) -> Result<crate::mcp_reads::ReadSession, crate::mcp_reads::ReadError> {
    crate::mcp_reads::ReadSession::acquire_with_overlay(
        &state.graph,
        None,
        &state.decisions,
        state,
        scope,
        core_graph::SnapshotReadLimits::default(),
        core_graph::coverage::CoverageReadLimits::default(),
        mode,
    )
}

fn mcp_artifact(session: &crate::mcp_reads::ReadSession, id: &str) -> spec::SpecArtifact {
    let trace = flowtracer::TraceLimits {
        max_flows: 4096,
        max_hops: 65536,
        max_output_bytes: 64 * 1024 * 1024,
    };
    let compile = spec::SelectedCompileLimits {
        max_bytes: 64 * 1024 * 1024,
        adr: spec::AdrLimits {
            max_rows: 65536,
            max_output_bytes: 64 * 1024 * 1024,
        },
    };
    match session.compile_artifact(id, trace, compile).unwrap() {
        crate::mcp_reads::artifacts::ArtifactCompilation::Produced(artifact) => {
            artifact.artifact().clone()
        }
        limited => panic!("{id}: {limited:?}"),
    }
}

#[test]
fn mcp_curated_read_serves_the_export_bytes_and_revision_for_an_eligible_record() {
    // AC-0654 (SPEC-12 slice 4, ADR-0061 decisions 3, 7 and 11): an MCP read
    // session computes the projection through the functions the export
    // uses. For the same reviews, graph and export mode its `curated`
    // artifact is the export's, byte for byte and by content identity, and
    // its basis carries the export's curated revision. A verified-only
    // session gets no item, only the count (R-INT-5).
    use crate::mcp_reads::ReadError;
    use crate::mcp_reads::curated_read::CuratedBasis;
    let (_dir, state, _, staged) = staged_captured_record();
    let id = staged.proposal_id.as_str();
    let modes = [spec::ExportMode::BestEffort, spec::ExportMode::VerifiedOnly];
    let pending = mcp_session(&state, spec::ExportMode::BestEffort, || Ok(vec![])).unwrap();
    state
        .proposals
        .lock()
        .unwrap()
        .review(id, 0, agents::ProposalDecision::Accepted, None)
        .unwrap();

    for mode in modes {
        let bundle = curated_export(&state, mode);
        let exported = artifact(&bundle, "curated");
        let basis = bundle.curated_basis.as_ref().unwrap();
        assert_eq!(basis.counts.eligible, 1);
        let session = mcp_session(&state, mode, || Ok(vec![])).unwrap();
        // The whole artifact: id, schema, content identity, bytes and the
        // assertions it cites.
        assert_eq!(&mcp_artifact(&session, "curated"), exported, "{mode:?}");
        assert!(session.basis().h3_curated_available);
        assert_eq!(
            session.basis().curated,
            CuratedBasis::Computed {
                revision: basis.revision.clone()
            }
        );
        // The review entered the basis identity.
        assert_ne!(session.basis().curated, pending.basis().curated);
        if mode == spec::ExportMode::BestEffort {
            assert_ne!(session.basis_id().unwrap(), pending.basis_id().unwrap());
        }

        // The structured form: the DR-4 fields of the same projection.
        let json = mcp_artifact(&session, "curated-json");
        let data: serde_json::Value = serde_json::from_str(&json.content).unwrap();
        assert_eq!(data["status"], "computed");
        assert_eq!(data["view"], "curated");
        assert_eq!(data["revision"], basis.revision.as_str());
        assert_eq!(
            data["graph_basis"],
            bundle.graph_basis.as_ref().unwrap().content_hash.as_str()
        );
        assert_eq!(data["counts"]["accepted"], 1);
        assert_eq!(data["counts"]["eligible"], 1);
        assert_eq!(data["listed"], serde_json::json!([]));
        if mode == spec::ExportMode::BestEffort {
            assert_eq!(data["export_mode"], "best-effort");
            assert_eq!(data["items_omitted"], 0);
            let item = &data["items"][0];
            assert_eq!(item["record_id"], id);
            assert_eq!(item["gap_id"], GAP);
            assert_eq!(item["source_id"], staged.proposal.source_id.as_str());
            assert_eq!(item["target_id"], staged.proposal.target_id.as_str());
            assert_eq!(item["edge_label"], staged.proposal.edge_label.as_str());
            // As produced: acceptance upgrades nothing (R-INT-1, R-INT-2).
            assert_eq!(item["tier"], "Agentic");
            assert_eq!(item["confidence"], "InferredWeak");
            assert_eq!(item["review_status"], "accepted");
            assert_eq!(item["state"], "eligible");
            assert_eq!(item["reason"], "all_conditions_hold");
            assert!(item["source_support"]["captured"].as_u64().unwrap() > 0);
            assert_eq!(item["source_support"]["unverified"], 0);
        } else {
            assert_eq!(data["export_mode"], "verified-only");
            assert_eq!(data["items"], serde_json::json!([]));
            assert_eq!(data["items_omitted"], 1);
            assert!(!json.content.contains(id));
            assert!(!mcp_artifact(&session, "curated").content.contains(id));
        }
    }

    // A review made while the session is acquired, after its reviews were
    // gathered, would give another curated revision: the read is refused.
    // The scope is read before the graph and again after the reviews.
    let mut calls = 0;
    let stale = mcp_session(&state, spec::ExportMode::BestEffort, || {
        calls += 1;
        if calls == 2 {
            state
                .proposals
                .lock()
                .unwrap()
                .review(id, 1, agents::ProposalDecision::Rejected, None)
                .unwrap();
        }
        Ok(vec![])
    });
    assert!(matches!(stale, Err(ReadError::StaleBasis)));
    // Read again, the withdrawn record is gone and the revision is new.
    let withdrawn = mcp_session(&state, spec::ExportMode::BestEffort, || Ok(vec![])).unwrap();
    let bundle = curated_export(&state, spec::ExportMode::BestEffort);
    assert_eq!(
        &mcp_artifact(&withdrawn, "curated"),
        artifact(&bundle, "curated")
    );
    assert_eq!(
        withdrawn.basis().curated,
        CuratedBasis::Computed {
            revision: bundle.curated_basis.unwrap().revision
        }
    );
    assert!(
        !mcp_artifact(&withdrawn, "curated-json")
            .content
            .contains(id)
    );
}

#[test]
fn workbench_curated_read_reports_the_export_revision_and_counts_in_both_modes() {
    // AC-0655 (SPEC-12 slice 3, "Surfaces"): the Workbench read is returned
    // beside the bundle, from the projection that compile computed. Its
    // revision and counts are that bundle's `curated_basis`, and the basis of
    // a separate export of the same reviews, graph and mode. An eligible
    // record is an item in best-effort, with every DR-4 field and the
    // sentence `curated.md` prints for its reason; a verified-only read
    // returns no item and the number left out (R-INT-5).
    let (_dir, state, owner, staged) = staged_captured_record();
    let id = staged.proposal_id.as_str();
    let modes = [spec::ExportMode::BestEffort, spec::ExportMode::VerifiedOnly];
    let read = |mode| {
        // The order `export_spec_with_views` uses.
        let review = crate::curated::gather(&state);
        let (bundle, _, read) = crate::build_spec_with_views_and_curated(
            &state.graph.lock().unwrap(),
            &state.decisions.lock().unwrap(),
            &review,
            mode,
        )
        .unwrap();
        let read = serde_json::to_value(read).unwrap();
        // One projection: the read beside the bundle is the bundle's basis.
        let basis = bundle.curated_basis.as_ref().unwrap();
        assert_eq!(read["revision"], basis.revision.as_str(), "{mode:?}");
        assert_eq!(read["counts"], serde_json::to_value(basis.counts).unwrap());
        read
    };
    let same_basis = |read: &serde_json::Value, mode| {
        let bundle = curated_export(&state, mode);
        let basis = bundle.curated_basis.as_ref().unwrap();
        assert_eq!(read["status"], "computed", "{mode:?}");
        assert_eq!(read["view"], "curated");
        assert_eq!(read["revision"], basis.revision.as_str(), "{mode:?}");
        assert_eq!(read["counts"], serde_json::to_value(basis.counts).unwrap());
        assert_eq!(
            read["graph_basis"],
            bundle.graph_basis.as_ref().unwrap().content_hash.as_str()
        );
        assert_eq!(read["export_mode"], serde_json::to_value(mode).unwrap());
        bundle
    };

    // Staged and not reviewed: computed, and nothing accepted.
    for mode in modes {
        let pending = read(mode);
        same_basis(&pending, mode);
        assert_eq!(pending["counts"]["accepted"], 0);
        assert_eq!(pending["items"], serde_json::json!([]));
        assert_eq!(pending["listed"], serde_json::json!([]));
        assert_eq!(pending["items_omitted"], 0);
    }

    state
        .proposals
        .lock()
        .unwrap()
        .review(id, 0, agents::ProposalDecision::Accepted, None)
        .unwrap();
    let best = read(spec::ExportMode::BestEffort);
    let bundle = same_basis(&best, spec::ExportMode::BestEffort);
    assert_eq!(best["counts"]["eligible"], 1);
    assert_eq!(best["items_omitted"], 0);
    assert_eq!(best["listed"], serde_json::json!([]));
    let item = &best["items"][0];
    assert_eq!(item["record_id"], id);
    assert_eq!(item["gap_id"], GAP);
    assert_eq!(item["source_id"], owner.id.as_str());
    assert_eq!(item["target_id"], staged.proposal.target_id.as_str());
    assert_eq!(item["edge_label"], staged.proposal.edge_label.as_str());
    // R-INT-2: the tier and confidence it was produced with.
    assert_eq!(item["tier"], "Agentic");
    assert_eq!(item["confidence"], "InferredWeak");
    assert_eq!(item["review_status"], "accepted");
    assert_eq!(item["source_support"]["unverified"], 0);
    assert!(item["source_support"]["captured"].as_u64().unwrap() > 0);
    assert_eq!(item["state"], "eligible");
    assert_eq!(item["reason"], "all_conditions_hold");
    assert_eq!(item["reason_sentence"], "Every condition holds.");
    // The basis text is the Basis cell of the record's `curated.md` row.
    let row = curated_row(&bundle, id).unwrap();
    assert!(
        row.contains(&format!("| {} |", item["basis_summary"].as_str().unwrap())),
        "{row}"
    );

    let verified = read(spec::ExportMode::VerifiedOnly);
    same_basis(&verified, spec::ExportMode::VerifiedOnly);
    assert_eq!(verified["revision"], best["revision"]);
    assert_eq!(verified["items"], serde_json::json!([]));
    assert_eq!(verified["items_omitted"], 1);
    assert!(!verified.to_string().contains(id));

    // Withdrawn by rejection: a new revision, still the export's.
    state
        .proposals
        .lock()
        .unwrap()
        .review(id, 1, agents::ProposalDecision::Rejected, None)
        .unwrap();
    let withdrawn = read(spec::ExportMode::BestEffort);
    same_basis(&withdrawn, spec::ExportMode::BestEffort);
    assert_ne!(withdrawn["revision"], best["revision"]);
    assert_eq!(withdrawn["counts"]["accepted"], 0);
}

/// `records` accepted version 2 records, each assessable (all evidence
/// captured and readable), over the fixture graph with `filler` more
/// deterministic Symbol nodes and as many CALLS edges. The records are one
/// prepared task staged under `records` jobs, so they differ only in
/// identity.
fn accepted_captured_records(
    filler: usize,
    records: usize,
) -> (tempfile::TempDir, AppState, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let private = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("source.ts"), ORIGINAL).unwrap();
    let state = app_state(&private);
    let source = crate::register_local_source(&state, &root).unwrap();
    let captured = capture(&state, &source, true);
    let (owner, target) = rule_pair(&captured);
    install_task(&state, &owner, &[target]);
    if filler > 0 {
        let symbol = |index: usize| format!("sym:filler:{index:06}");
        let prov = |content: &str| {
            core_prov::Provenance::new(
                core_prov::Tier::Deterministic,
                core_prov::ConfidenceTier::Confirmed,
                vec![],
                "test.curated-cost-fixture",
                content.as_bytes(),
            )
            .unwrap()
        };
        let patch = core_graph::GraphPatch {
            upsert_nodes: (0..filler)
                .map(|index| Node {
                    id: symbol(index),
                    label: "Symbol".into(),
                    props: serde_json::json!({
                        "name": symbol(index), "prov": prov(&symbol(index)),
                    }),
                })
                .collect(),
            upsert_edges: (0..filler)
                .map(|index| Edge {
                    src: symbol(index),
                    dst: symbol((index + 1) % filler),
                    label: "CALLS".into(),
                    props: serde_json::json!({"prov": prov(&format!("{index} calls"))}),
                })
                .collect(),
            ..Default::default()
        };
        let mut graph = state.graph.lock().unwrap();
        let expected = graph.read_snapshot().unwrap();
        assert!(
            graph
                .apply_patch_if_snapshot_matches(&expected, &patch)
                .unwrap()
        );
    }
    let task = prepare(&state, GAP, "escalate:fixture").unwrap();
    let provider = DuringCall {
        task: task.task().clone(),
        expect_prepared: true,
        change: Mutex::new(Some(|| {})),
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose_prepared(
            &provider,
            &EgressFirewall::new(EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    let ids = (0..records)
        .map(|index| {
            let (_, execution) =
                crate::start_job(&state, &format!("escalate:curated-{index}:local")).unwrap();
            let staged =
                crate::stage_completed_prepared_job_proposal(&state, &execution, &task, &proposal)
                    .unwrap();
            state
                .proposals
                .lock()
                .unwrap()
                .review(
                    &staged.proposal_id,
                    0,
                    agents::ProposalDecision::Accepted,
                    None,
                )
                .unwrap();
            staged.proposal_id
        })
        .collect();
    (dir, state, ids)
}

/// A Workbench decision on a statement that no stored fact and no derived
/// statement carries.
fn record_unmatched_decision(state: &AppState, content: &str) {
    state
        .decisions
        .lock()
        .unwrap()
        .record_assertion(
            &agents::CuratableAssertion {
                subject_id: format!("edge:{content}"),
                summary: "A statement the graph no longer makes".into(),
                provenance: core_prov::Provenance::new(
                    core_prov::Tier::Semantic,
                    core_prov::ConfidenceTier::InferredStrong,
                    vec![core_prov::EvidenceRef {
                        repo: "local/fixture".into(),
                        commit_sha: "workdir".into(),
                        path: "gone.ts".into(),
                        byte_start: 0,
                        byte_end: 4,
                    }],
                    "t2.semantic",
                    content.as_bytes(),
                )
                .unwrap(),
            },
            agents::AssertionDecision::Rejected,
            None,
        )
        .unwrap();
}

#[test]
#[ignore = "measurement, not a gate: run with --ignored --nocapture"]
fn curated_export_cost_measurement() {
    // #1834: where one export spends its time, with 0, 16 and 256 assessable
    // accepted records over a graph of CURATED_COST_FILLER (default 20000)
    // extra nodes and as many edges. Prints; asserts only the counts.
    use std::time::Instant;
    let filler: usize = std::env::var("CURATED_COST_FILLER")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000);
    let ms = |from: Instant| from.elapsed().as_secs_f64() * 1000.0;
    for records in [0usize, 16, 256] {
        let (_dir, state, _ids) = accepted_captured_records(filler, records);
        let mode = spec::ExportMode::BestEffort;

        // What one assessment's graph observation costs on its own.
        let at = Instant::now();
        let observed = state
            .graph
            .lock()
            .unwrap()
            .read_source_selection_snapshot(&[])
            .unwrap();
        let copy_ms = ms(at);
        let (nodes, edges) = (observed.graph.0.len(), observed.graph.1.len());
        let at = Instant::now();
        context_hub::ContextSnapshot::new(observed.graph.0, observed.graph.1).unwrap();
        let identity_ms = ms(at);

        let at = Instant::now();
        let review = crate::curated::gather(&state);
        let gather_ms = ms(at);

        let export = |label: &str| {
            let graph = state.graph.lock().unwrap();
            let decisions = state.decisions.lock().unwrap();
            let at = Instant::now();
            let inputs = crate::spec_inputs(&graph, &decisions, Some(&review)).unwrap();
            let inputs_ms = ms(at);
            let at = Instant::now();
            let bundle = spec::try_compile_spec_supplied(
                &inputs.base.graph.0,
                &inputs.base.graph.1,
                &inputs.flows,
                mode,
                &inputs.rejected_hashes,
                inputs.supplied(None),
            )
            .unwrap();
            let compile_ms = ms(at);
            let crate::CuratedInput::Computed(projection) = &inputs.curated else {
                panic!("projection not computed");
            };
            assert_eq!(projection.counts.accepted, records);
            assert_eq!(projection.counts.eligible, records);
            eprintln!(
                "  {label}: read+trace+project {inputs_ms:.1} ms, compile {compile_ms:.1} ms \
                 ({} artifacts)",
                bundle.artifacts.len()
            );
            inputs_ms
        };
        eprintln!(
            "records={records} graph={nodes} nodes/{edges} edges: gather {gather_ms:.1} ms; \
             one graph observation = copy {copy_ms:.1} ms + identity {identity_ms:.1} ms"
        );
        let plain = export("no unmatched decision");
        record_unmatched_decision(&state, "sym:gone CALLS sym:nowhere");
        let first = export("unmatched decision, 1st export");
        let repeat = export("unmatched decision, 2nd export");
        eprintln!(
            "  settling the unmatched decision: 1st export +{:.1} ms, 2nd export +{:.1} ms",
            first - plain,
            repeat - plain
        );
    }
}

/// One node more in the graph: another store revision and content hash.
fn move_graph(state: &AppState, id: &str) {
    use core_graph::GraphStore;
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: id.into(),
            label: "Symbol".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
}

#[test]
fn unmatched_decision_costs_one_compile_for_a_revision_and_decision_set() {
    // AC-0659 (#1834): a Workbench decision that no stored fact carries is
    // settled by one compile for one store revision and one set of such
    // decisions, in both modes and for the single-artifact read, not by one
    // for every export. A new revision or another set compiles again, so no
    // answer is carried to inputs it was not made for. Every export is, in
    // every byte, revision and count, the one made with nothing kept (the
    // path before #1834). An MCP read still makes no such compile and does
    // not use the kept answer.
    use crate::mcp_reads::curated_read::CuratedBasis;
    let (_dir, state, _) = accepted_captured_records(0, 2);
    let modes = [spec::ExportMode::BestEffort, spec::ExportMode::VerifiedOnly];
    let compiles = crate::curated::derived_compiles;
    let export = |mode| {
        serde_json::to_value(crate::export_spec_with_views_blocking(mode, &state).unwrap()).unwrap()
    };
    // Nothing kept: the review settles alone, under the locks, as before.
    let alone = |mode| {
        let review = crate::curated::gather(&state).settling_alone();
        let mut inputs = crate::spec_inputs(
            &state.graph.lock().unwrap(),
            &state.decisions.lock().unwrap(),
            Some(&review),
        )
        .unwrap();
        // The repository names the desktop export reads beside the graph
        // (#2049), so `architecture.md` names them the same way.
        inputs.repo_names = crate::repo_names_of(&state, &inputs.base.graph.0).unwrap();
        let (bundle, views, curated) =
            crate::compile_spec_with_views_and_curated(inputs, mode, |a| a).unwrap();
        serde_json::to_value(crate::SpecWithViewsPreview {
            bundle: crate::preview_spec_bundle(bundle),
            views,
            curated,
        })
        .unwrap()
    };
    // Each mode exported twice costs `expected` (0 or 1) compiles in all,
    // and is the export made alone, which costs that many every time.
    let check = |expected: usize, what: &str| {
        let before = compiles();
        let exported = modes.map(export);
        assert_eq!(modes.map(export), exported, "{what}");
        assert_eq!(compiles() - before, expected, "{what}");
        for (mode, exported) in modes.iter().zip(&exported) {
            let before = compiles();
            assert_eq!(&alone(*mode), exported, "{what} in {mode:?}");
            assert_eq!(compiles() - before, expected, "{what}");
            assert_eq!(exported["curated"]["status"], "computed", "{what}");
            // One projection: the read beside the bundle is its basis.
            let basis = &exported["bundle"]["curated_basis"];
            assert_eq!(exported["curated"]["revision"], basis["revision"]);
            assert_eq!(exported["curated"]["counts"], basis["counts"]);
            assert_eq!(basis["counts"]["eligible"], 2, "{what}");
            // The single-artifact read is the bundle's document and costs
            // no compile of its own.
            let before = compiles();
            let document = crate::read_spec_artifact_blocking(*mode, "curated", &state).unwrap();
            assert_eq!(compiles(), before, "{what}");
            let listed = exported["bundle"]["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|artifact| artifact["id"] == "curated")
                .unwrap();
            assert_eq!(listed["content"], document.content.as_str(), "{what}");
            assert_eq!(listed["content_id"], document.content_id.as_str());
        }
        exported
    };

    // Every decision on a stored fact, here none: no second compile at all.
    let plain = check(0, "no unmatched decision");

    record_unmatched_decision(&state, "sym:gone CALLS sym:nowhere");
    let unmatched = check(1, "one unmatched decision");
    assert_ne!(
        unmatched[0]["curated"]["revision"],
        plain[0]["curated"]["revision"]
    );
    // An MCP read of the same state: not computed, kept answer or not.
    for mode in modes {
        let before = compiles();
        let session = mcp_session(&state, mode, || Ok(vec![])).unwrap();
        assert_eq!(
            session.basis().curated,
            CuratedBasis::NotComputed(spec::CuratedFailure::DerivedDecisionsNeedCompile)
        );
        assert_eq!(compiles(), before);
    }

    // Another set of unmatched decisions: one compile again, then none.
    record_unmatched_decision(&state, "sym:gone CALLS sym:elsewhere");
    let two = check(1, "two unmatched decisions");
    assert_ne!(
        two[0]["curated"]["revision"],
        unmatched[0]["curated"]["revision"]
    );

    // Another graph revision, the same decisions: one compile again.
    move_graph(&state, "sym:later");
    let before = compiles();
    let moved = modes.map(export);
    assert_eq!(modes.map(export), moved);
    assert_eq!(compiles() - before, 1);
    for (mode, moved) in modes.iter().zip(&moved) {
        assert_eq!(&alone(*mode), moved, "{mode:?}");
        assert_ne!(
            moved["bundle"]["graph_basis"],
            two[0]["bundle"]["graph_basis"]
        );
    }
}

#[test]
fn assessments_of_one_export_share_one_graph_copy_at_one_store_revision() {
    // AC-0659 (#1834): the assessments of one read copy the graph once
    // between them, not once each, and each is the assessment made alone:
    // same comparisons, same evidence, same observed revision. When the
    // store moves between two of them the rows are read again, so the later
    // one reports the new revision and the changed graph, and a review
    // gathered before a move is still stale for the export that reads after
    // it.
    use super::assessment::{SharedObservation, assess_observed, graph_copies};
    let (_dir, state, ids) = accepted_captured_records(0, 3);
    let revision = || state.graph.lock().unwrap().read_revision().unwrap();
    let assessed = |id: &str, shared: &mut SharedObservation| {
        let (assessment, observed) = assess_observed(&state, id, shared).unwrap();
        (serde_json::to_value(assessment).unwrap(), observed)
    };

    let before = graph_copies();
    let alone: Vec<_> = ids
        .iter()
        .map(|id| assessed(id, &mut SharedObservation::default()))
        .collect();
    assert_eq!(graph_copies() - before, ids.len());
    let before = graph_copies();
    let mut shared = SharedObservation::default();
    let together: Vec<_> = ids.iter().map(|id| assessed(id, &mut shared)).collect();
    assert_eq!(graph_copies() - before, 1);
    assert_eq!(together, alone);
    for (assessment, observed) in &together {
        assert_eq!(observed.as_ref(), Some(&revision()));
        assert_eq!(assessment["graph_comparison"], "unchanged");
        assert_eq!(assessment["association_comparison"], "unchanged");
    }

    // One export: one copy for its three assessments, all eligible.
    let before = graph_copies();
    let bundle = curated_export(&state, spec::ExportMode::BestEffort);
    assert_eq!(graph_copies() - before, 1);
    let basis = bundle.curated_basis.as_ref().unwrap();
    assert_eq!((basis.counts.accepted, basis.counts.eligible), (3, 3));

    // A review gathered before the graph moves describes another revision.
    let review = crate::curated::gather(&state);
    let old = revision();
    move_graph(&state, "sym:later");
    assert_ne!(revision(), old);
    let stale = crate::build_spec_bundle(
        &state.graph.lock().unwrap(),
        &state.decisions.lock().unwrap(),
        &review,
        spec::ExportMode::BestEffort,
    )
    .unwrap();
    assert_eq!(stale.curated_basis.as_ref().unwrap().counts.eligible, 0);
    for id in &ids {
        let row = curated_row(&stale, id).unwrap();
        assert!(row.contains("(`assessment_failed`)"), "{row}");
    }

    // The observation kept from before the move is not used after it.
    let before = graph_copies();
    let later = assessed(&ids[0], &mut shared);
    assert_eq!(graph_copies() - before, 1);
    assert_eq!(later, assessed(&ids[0], &mut SharedObservation::default()));
    assert_eq!(later.1, Some(revision()));
    assert_eq!(later.0["graph_comparison"], "changed");
    // And the rows read then serve the next assessment.
    let before = graph_copies();
    assert_eq!(assessed(&ids[1], &mut shared).1, Some(revision()));
    assert_eq!(graph_copies(), before);
}

#[test]
fn export_commands_do_their_work_off_the_calling_thread() {
    // AC-0659 (#1834): each export command hops to a blocking worker before
    // it gathers, reads or compiles, so the invoking (main) thread runs none
    // of it. Shown by the one compile an unmatched decision costs: the
    // command makes it, this thread's count does not move, and what the
    // worker settled then serves this thread. Each command returns what its
    // blocking form returns.
    use tauri::Manager;
    let (_dir, state, _) = accepted_captured_records(0, 1);
    record_unmatched_decision(&state, "sym:gone CALLS sym:nowhere");
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(state);
    let state = app.state::<AppState>();
    fn json<T: serde::Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }
    let compiles = crate::curated::derived_compiles;
    let before = compiles();
    let mode = spec::ExportMode::BestEffort;

    let bundle =
        tauri::async_runtime::block_on(crate::export_spec(mode, app.handle().clone())).unwrap();
    // Settled, and not here.
    assert_eq!(compiles(), before);
    assert_eq!(
        json(&bundle),
        json(&crate::export_spec_blocking(mode, &state).unwrap())
    );
    assert_eq!(compiles(), before, "served from what the worker settled");
    // With nothing kept, this thread would have compiled.
    let review = crate::curated::gather(&state).settling_alone();
    crate::build_spec_bundle(
        &state.graph.lock().unwrap(),
        &state.decisions.lock().unwrap(),
        &review,
        mode,
    )
    .unwrap();
    assert_eq!(compiles(), before + 1);

    let views =
        tauri::async_runtime::block_on(crate::export_spec_views(mode, app.handle().clone()))
            .unwrap();
    assert_eq!(
        json(&views),
        json(&crate::export_spec_views_blocking(mode, &state).unwrap())
    );
    let with_views =
        tauri::async_runtime::block_on(crate::export_spec_with_views(mode, app.handle().clone()))
            .unwrap();
    assert_eq!(
        json(&with_views),
        json(&crate::export_spec_with_views_blocking(mode, &state).unwrap())
    );
    assert_eq!(json(&with_views)["bundle"], json(&bundle));
    assert_eq!(json(&with_views)["views"], json(&views));
    for id in ["curated", "topology"] {
        let artifact = tauri::async_runtime::block_on(crate::read_spec_artifact(
            mode,
            id.into(),
            app.handle().clone(),
        ))
        .unwrap();
        assert_eq!(
            json(&artifact),
            json(&crate::read_spec_artifact_blocking(mode, id, &state).unwrap()),
            "{id}"
        );
    }
    // AC-0720 (#1662): the batched read on its worker returns what the
    // single reads return, in the requested order, from one snapshot.
    let ids = vec!["topology".to_string(), "curated".to_string()];
    let together = tauri::async_runtime::block_on(crate::read_spec_artifacts(
        mode,
        ids.clone(),
        app.handle().clone(),
    ))
    .unwrap();
    let reads = crate::spec_reads();
    let blocking = crate::read_spec_artifacts_blocking(mode, &ids, &state).unwrap();
    assert_eq!(crate::spec_reads(), reads + 1);
    assert_eq!(json(&together), json(&blocking));
    for (artifact, id) in blocking.iter().zip(&ids) {
        assert_eq!(
            json(artifact),
            json(&crate::read_spec_artifact_blocking(mode, id, &state).unwrap()),
            "{id}"
        );
    }
    let reads = crate::spec_reads();
    assert!(
        crate::read_spec_artifacts_blocking(mode, &[], &state)
            .unwrap()
            .is_empty()
    );
    assert_eq!(crate::spec_reads(), reads, "nothing capped, nothing read");
    assert_eq!(compiles(), before + 1);
}
