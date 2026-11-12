//! Durable host-owned review transport and per-instance staging (SPEC-03).

use agents::{BatchFailure, ProposalDecision, StagedProposal, StagedProposalPage};
use serde::Serialize;
use tauri::Manager;

/// Resolve reviewed material on the host; caller-supplied proposal bodies are
/// deliberately absent from this command's input contract.
#[tauri::command]
pub(crate) async fn record_agent_decision(
    proposal_id: String,
    expected_revision: u64,
    decision: ProposalDecision,
    note: Option<String>,
    app: tauri::AppHandle,
) -> Result<StagedProposal, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        state
            .proposals
            .lock()
            .map_err(|error| error.to_string())?
            .review(&proposal_id, expected_revision, decision, note.as_deref())
            .map_err(|error| error.to_string())
    })
    .await
}

/// Restore pending and reviewed proposals independently of job history.
#[tauri::command]
pub(crate) async fn list_staged_proposals(
    limit: usize,
    cursor: Option<String>,
    app: tauri::AppHandle,
) -> Result<StagedProposalPage, String> {
    if !(1..=50).contains(&limit) {
        return Err("proposal history page limit must be 1..=50".into());
    }
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        state
            .proposals
            .lock()
            .map_err(|error| error.to_string())?
            .list(limit, cursor.as_deref())
            .map_err(|error| error.to_string())
    })
    .await
}

/// A completed entry is always a persisted result, including partial batches.
#[derive(Debug, Serialize)]
pub(crate) struct StagedBatchOutcome {
    pub proposals: Vec<StagedProposal>,
    pub failures: Vec<BatchFailure>,
    pub cancelled: bool,
}

/// Run one host-assembled task at a time. The execution callback includes durable
/// staging: storage errors stay failures and cannot enter the successful results.
/// Cancellation is observed between instances; already completed results survive.
pub(crate) fn run_staged_batch<T>(
    tasks: Vec<(String, Result<T, String>)>,
    mut cancelled: impl FnMut() -> bool,
    mut progress: impl FnMut(usize, usize),
    mut execute_and_stage: impl FnMut(&T) -> Result<StagedProposal, String>,
) -> StagedBatchOutcome {
    let total = tasks.len();
    let mut outcome = StagedBatchOutcome {
        proposals: Vec::new(),
        failures: Vec::new(),
        cancelled: false,
    };
    for (index, (gap_id, task)) in tasks.into_iter().enumerate() {
        if cancelled() {
            outcome.cancelled = true;
            break;
        }
        progress(index, total);
        match task.and_then(|task| execute_and_stage(&task)) {
            Ok(proposal) => outcome.proposals.push(proposal),
            Err(error) => outcome.failures.push(BatchFailure { gap_id, error }),
        }
    }
    // A cancel during the last/only provider call still wins the job lifecycle,
    // while the completed staged result remains available in this outcome.
    outcome.cancelled |= cancelled();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::{AgentBroker, AgentCandidate, AgentEvidence, AgentTask, ProposalStore};
    use core_graph::GraphStore;
    use core_prov::{ConfidenceTier, EvidenceRef};
    use llm::{
        Completion, EgressFirewall, EgressPolicy, Embedding, LlmProvider, Locality, ProviderCaps,
        ProviderCompletionRequest, ProviderError,
    };
    use std::cell::Cell;

    struct Provider;

    impl LlmProvider for Provider {
        fn id(&self) -> &str {
            "test:staged-result"
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
        fn complete(&self, _: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
            Ok(Completion { text: serde_json::json!({
                "target_id": "target", "annotation": "A proposed relationship, awaiting review.",
                "citations": ["source", "target"]
            }).to_string() })
        }
    }

    fn task(id: &str) -> AgentTask {
        let evidence = ["source", "target"].map(|id| AgentEvidence {
            id: id.into(),
            source: EvidenceRef {
                repo: "example/project".into(),
                path: format!("{id}.ts"),
                byte_start: 0,
                byte_end: 12,
                commit_sha: "a".repeat(40),
            },
            text: "const x = 1;".into(),
        });
        AgentTask {
            action_id: format!("run:{id}"),
            gap_id: id.into(),
            source_id: "source".into(),
            edge_label: "CALLS".into(),
            existing_confidence: ConfidenceTier::Gap,
            source_evidence_ids: vec!["source".into()],
            evidence: evidence.to_vec(),
            candidates: vec![AgentCandidate {
                node_id: "target".into(),
                label: "Symbol".into(),
                summary: "Target function".into(),
                evidence_ids: vec!["target".into()],
            }],
        }
    }

    fn snapshot() -> String {
        context_hub::ContextSnapshot::new(vec![], vec![])
            .unwrap()
            .id()
            .to_string()
    }

    fn produce(task: &AgentTask) -> agents::AgentProposal {
        AgentBroker::bounded_default()
            .propose(
                &Provider,
                &EgressFirewall::new(EgressPolicy::local_only()),
                task,
                None,
            )
            .unwrap()
    }

    #[test]
    fn staged_batch_preserves_completed_results_when_cancelled_during_last_call() {
        // AC-0128: cancellation during the only/last provider cannot erase the
        // persisted result or disagree with the returned cancellation status.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proposals.sqlite");
        let mut store = ProposalStore::open(&path).unwrap();
        let cancelled = Cell::new(false);
        let task = task("gap:last");
        let outcome = run_staged_batch(
            vec![(task.gap_id.clone(), Ok(task))],
            || cancelled.get(),
            |_, _| {},
            |task| {
                let proposal = produce(task);
                cancelled.set(true);
                store
                    .stage(task, &proposal, 1, &snapshot())
                    .map_err(|error| error.to_string())
            },
        );
        assert!(outcome.cancelled);
        assert!(outcome.failures.is_empty());
        assert_eq!(outcome.proposals.len(), 1);
        drop(store);
        let restored = ProposalStore::open(&path).unwrap().list(10, None).unwrap();
        assert_eq!(
            restored.items[0].proposal_id,
            outcome.proposals[0].proposal_id
        );
    }

    #[test]
    fn staged_batch_stops_before_next_provider_and_keeps_storage_failures_out_of_successes() {
        // AC-0128: persistence participates in the per-instance result, and one
        // failed stage does not destroy earlier/later durable instances.
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProposalStore::open(dir.path().join("proposals.sqlite")).unwrap();
        let cancelled = Cell::new(false);
        let calls = Cell::new(0);
        let tasks = (0..4)
            .map(|index| {
                let task = task(&format!("gap:{index}"));
                (task.gap_id.clone(), Ok(task))
            })
            .collect();
        let outcome = run_staged_batch(
            tasks,
            || cancelled.get(),
            |_, _| {},
            |task| {
                calls.set(calls.get() + 1);
                let proposal = produce(task);
                if calls.get() == 2 {
                    return Err("injected storage failure".into());
                }
                let result = store
                    .stage(task, &proposal, 1, &snapshot())
                    .map_err(|error| error.to_string());
                if calls.get() == 3 {
                    cancelled.set(true);
                }
                result
            },
        );
        assert!(outcome.cancelled);
        assert_eq!(calls.get(), 3);
        assert_eq!(outcome.proposals.len(), 2);
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].gap_id, "gap:1");
        assert_eq!(store.list(10, None).unwrap().items.len(), 2);
        let blocked = run_staged_batch(
            vec![("gap:blocked".into(), Ok(task("gap:blocked")))],
            || true,
            |_, _| panic!("cancelled before progress"),
            |_| panic!("cancelled before provider"),
        );
        assert!(blocked.cancelled);
        assert!(blocked.proposals.is_empty());
    }

    #[test]
    fn curated_record_read_fails_whole_beyond_its_accepted_bound() {
        // AC-0653 (SPEC-12 "The projection"): the staging read is bounded and
        // never returns a part of the accepted records as if it were all.
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProposalStore::open(dir.path().join("proposals.sqlite")).unwrap();
        let bound = crate::curated::MAX_CURATED_ACCEPTED;
        for index in 0..=bound {
            let task = task(&format!("gap:{index}"));
            let staged = store.stage(&task, &produce(&task), 1, &snapshot()).unwrap();
            // The last one stays pending: pending records do not count.
            if index < bound {
                store
                    .review(&staged.proposal_id, 0, ProposalDecision::Accepted, None)
                    .unwrap();
            }
        }
        // Exactly the bound, read across several pages, is complete.
        let review = crate::curated::read_records(&store);
        let graph = core_graph::SqliteGraphStore::open_in_memory().unwrap();
        let decisions = agents::DecisionLog::open(":memory:").unwrap();
        let bundle =
            crate::build_spec_bundle(&graph, &decisions, &review, spec::ExportMode::BestEffort)
                .unwrap();
        let counts = bundle.curated_basis.clone().unwrap().counts;
        assert_eq!(counts.accepted, bound);
        assert_eq!(counts.unverified, bound);
        assert!(bundle.artifacts.iter().any(|artifact| {
            artifact.id == "curated" && artifact.content.contains("| unverified |")
        }));
        let pending = store.list(1, None).unwrap().items.remove(0);
        store
            .review(&pending.proposal_id, 0, ProposalDecision::Accepted, None)
            .unwrap();
        // One more, and nothing of the read is projected. The export still
        // succeeds in both modes: the recovered documents do not depend on
        // the review stores. `curated.md` gives the reason, no curated basis
        // is stamped, and every other artifact is the one a compile with no
        // projection produces.
        let review = crate::curated::read_records(&store);
        let (nodes, edges) = graph.read_snapshot().unwrap();
        for mode in [spec::ExportMode::BestEffort, spec::ExportMode::VerifiedOnly] {
            let bundle = crate::build_spec_bundle(&graph, &decisions, &review, mode).unwrap();
            assert_eq!(bundle.curated_basis, None);
            assert!(bundle.graph_basis.is_some());
            let bare = spec::compile_spec(&nodes, &edges, &[], mode, &Default::default());
            for (bare, failed) in bare.artifacts.iter().zip(&bundle.artifacts) {
                assert_eq!(bare == failed, bare.id != "curated", "{}", bare.id);
            }
            let curated = bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "curated")
                .unwrap();
            assert!(
                curated
                    .content
                    .contains("The curated projection was not computed for this export"),
                "{}",
                curated.content
            );
            assert!(
                curated
                    .content
                    .contains(&format!("More than {bound} proposals are accepted")),
                "{}",
                curated.content
            );
            assert!(curated.content.contains("(`too_many_accepted`)"));
            assert!(!curated.content.contains("gap:"));
            assert!(curated.assertions.is_empty());
            // The views build and the single-artifact read succeed too.
            let (with_views, _) =
                crate::build_spec_with_views(&graph, &decisions, &review, mode).unwrap();
            assert_eq!(with_views, bundle);
            assert_eq!(
                &crate::build_spec_artifact(&graph, &decisions, Some(&review), mode, "curated")
                    .unwrap(),
                curated
            );
        }
    }

    #[test]
    fn staged_result_survives_job_cleanup_without_activating_legacy_or_recovered_facts() {
        // AC-0128/AC-0131: a commit before job finalization survives restart and
        // job cleanup. Neither staging nor review alters a recovered fact or
        // imports legacy caller-body decisions into the staged history.
        let dir = tempfile::tempdir().unwrap();
        let stage_path = dir.path().join("proposals.sqlite");
        let state_path = dir.path().join("state.db");
        let mut store = ProposalStore::open(&stage_path).unwrap();
        let mut jobs = crate::jobs::JobStore::open(&state_path).unwrap();
        let mut legacy = agents::DecisionLog::open(&state_path).unwrap();
        let mut graph = core_graph::SqliteGraphStore::open_in_memory().unwrap();
        let task = task("gap:restart");
        let proposal = produce(&task);
        for (id, label, confidence) in [
            ("source", "Symbol", ConfidenceTier::Confirmed),
            ("target", "Symbol", ConfidenceTier::Confirmed),
            ("gap:restart", "Gap", ConfidenceTier::Gap),
        ] {
            let provenance = core_prov::Provenance::new(
                core_prov::Tier::Deterministic,
                confidence,
                vec![task.evidence[0].source.clone()],
                "t0.fixture",
                id.as_bytes(),
            )
            .unwrap();
            graph
                .put_node(&core_graph::Node {
                    id: id.into(),
                    label: label.into(),
                    props: serde_json::json!({"name":id,"prov":provenance}),
                })
                .unwrap();
        }
        let recovered_before = context_hub::ContextSnapshot::new(
            graph.all_nodes().unwrap(),
            graph.all_edges().unwrap(),
        )
        .unwrap()
        .id()
        .to_string();
        legacy
            .record(&proposal, ProposalDecision::Accepted, None)
            .unwrap();
        assert!(store.list(10, None).unwrap().items.is_empty());
        let before = serde_json::to_value(
            crate::build_spec_bundle(
                &graph,
                &legacy,
                &crate::curated::CuratedReview::default(),
                spec::ExportMode::BestEffort,
            )
            .unwrap(),
        )
        .unwrap();
        let job = jobs.enqueue("escalate:restart:local").unwrap();
        let locks = crate::job_execution_host_tests::locks(&state_path);
        let execution = crate::job_execution_host_tests::claim(
            &mut jobs,
            &locks,
            job.id,
            crate::jobs::ClaimMode::StartQueued,
        );
        let staged = store.stage(&task, &proposal, job.id, &snapshot()).unwrap();
        jobs.cancel(job.id).unwrap();
        let interrupted = jobs.enqueue("escalate:interrupted:local").unwrap();
        let interrupted_execution = crate::job_execution_host_tests::claim(
            &mut jobs,
            &locks,
            interrupted.id,
            crate::jobs::ClaimMode::StartQueued,
        );
        drop(execution);
        drop(interrupted_execution);
        drop(store);
        drop(jobs);
        let jobs = std::sync::Mutex::new(crate::jobs::JobStore::open(&state_path).unwrap());
        assert_eq!(
            crate::recover_jobs(&jobs, &locks)
                .unwrap()
                .into_iter()
                .map(|job| job.id)
                .collect::<Vec<_>>(),
            vec![interrupted.id]
        );
        jobs.lock().unwrap().clear_finished().unwrap();
        assert!(jobs.lock().unwrap().get(job.id).is_err());
        let mut restored = ProposalStore::open(&stage_path).unwrap();
        // A staged proposal nobody has reviewed changes no byte of the export.
        assert_eq!(
            serde_json::to_value(
                crate::build_spec_bundle(
                    &graph,
                    &legacy,
                    &crate::curated::read_records(&restored),
                    spec::ExportMode::BestEffort
                )
                .unwrap()
            )
            .unwrap(),
            before
        );
        let accepted = restored
            .review(&staged.proposal_id, 0, ProposalDecision::Accepted, None)
            .unwrap();
        assert_eq!(
            accepted.proposal.provenance.confidence_tier,
            ConfidenceTier::InferredWeak
        );
        // AC-0653 (SPEC-12 slice 2): acceptance now reaches the export, and
        // only as a listed state. This record has no captured evidence, so it
        // is not a curated item: `curated.md` and the curated basis change,
        // and every other artifact, the fact index and the graph basis are
        // byte-identical in both modes.
        let review = crate::curated::read_records(&restored);
        for mode in [spec::ExportMode::BestEffort, spec::ExportMode::VerifiedOnly] {
            let before = crate::build_spec_bundle(
                &graph,
                &legacy,
                &crate::curated::CuratedReview::default(),
                mode,
            )
            .unwrap();
            let after = crate::build_spec_bundle(&graph, &legacy, &review, mode).unwrap();
            assert_eq!(before.artifacts.len(), after.artifacts.len());
            for (before, after) in before.artifacts.iter().zip(&after.artifacts) {
                assert_eq!(
                    before == after,
                    before.id != "curated",
                    "{} in {mode:?}",
                    before.id
                );
            }
            assert_eq!(before.graph_basis, after.graph_basis);
            assert_eq!(before.assertion_count, after.assertion_count);
            let (before_basis, basis) = (
                before.curated_basis.unwrap(),
                after.curated_basis.clone().unwrap(),
            );
            assert_eq!(before_basis.counts.accepted, 0);
            assert_ne!(before_basis.revision, basis.revision);
            assert_eq!(basis.counts.accepted, 1);
            assert_eq!(basis.counts.unverified, 1);
            assert_eq!(basis.counts.eligible, 0);
            let curated = after
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "curated")
                .unwrap();
            assert!(curated.assertions.is_empty());
            assert!(curated.content.contains(&basis.revision));
            assert!(curated.content.contains("Curated items: 0."));
            let row = curated
                .content
                .lines()
                .find(|line| line.starts_with(&format!("| {} |", staged.proposal_id)))
                .unwrap();
            assert!(
                row.contains("| Agentic | InferredWeak | accepted |"),
                "{row}"
            );
            assert!(row.contains("| unverified |"), "{row}");
            // The artifact compiled alone is the bundle's.
            assert_eq!(
                &crate::build_spec_artifact(&graph, &legacy, Some(&review), mode, "curated")
                    .unwrap(),
                curated
            );
        }
        assert_eq!(legacy.list().unwrap().len(), 1);
        assert_eq!(
            context_hub::ContextSnapshot::new(
                graph.all_nodes().unwrap(),
                graph.all_edges().unwrap()
            )
            .unwrap()
            .id(),
            recovered_before
        );
    }
}
