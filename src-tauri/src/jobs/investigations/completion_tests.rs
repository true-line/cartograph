use super::*;
use crate::job_execution::JobExecutionLocks;
use crate::jobs::ClaimMode;
use llm::bounded::CompletionLimits;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
use serde_json::json;
use std::time::Duration;
struct Fixture {
    store: JobStore,
    execution: JobExecution,
    id: String,
    request: StagedObligationRequest,
    archive: InvestigationCoverageArchive,
    inventory_details: Vec<String>,
    /// Dropped last: Windows cannot remove it while a store holds files in it (#1307).
    directory: tempfile::TempDir,
}
#[path = "progressive_storage_tests.rs"]
mod progressive_storage_tests;
impl Fixture {
    fn new() -> Self {
        Self::with_mode(InvestigationProviderMode::Local)
    }
    fn with_mode(mode: InvestigationProviderMode) -> Self {
        Self::with_policy(mode, &INVESTIGATION_V1_POLICY)
    }
    fn registered_v2() -> Self {
        let policy = InvestigationPolicy::for_profile(INVESTIGATION_V2_POLICY.profile())
            .expect("V2 durable tests require registration after the production capacity audit");
        assert_eq!(policy.limits(), INVESTIGATION_V2_POLICY.limits());
        Self::with_policy(InvestigationProviderMode::Local, policy)
    }
    fn with_policy(mode: InvestigationProviderMode, policy: &InvestigationPolicy) -> Self {
        Self::with_family(mode, policy, false)
    }
    fn with_family(
        mode: InvestigationProviderMode,
        policy: &InvestigationPolicy,
        progressive: bool,
    ) -> Self {
        Self::with_family_preparation(mode, policy, progressive, true)
    }
    fn with_family_preparation(
        mode: InvestigationProviderMode,
        policy: &InvestigationPolicy,
        progressive: bool,
        prepare_input: bool,
    ) -> Self {
        let legacy = policy.profile() == INVESTIGATION_V1_POLICY.profile();
        let directory = tempfile::tempdir().unwrap();
        let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
        let snapshot = context_hub::ContextSnapshot::new(vec![], vec![]).unwrap();
        let record=core_graph::coverage::CoverageRecord{repo_key:"fixture/repo".into(),registered_source_id:Some("src:fixture".into()),ingest_id:"fixture-ingest".into(),config_hash:"fixture-config".into(),inventory_json:json!({"inventory":context_hub::coverage::INVENTORY_VERSION,"repo":"fixture/repo","commit":"workdir","layer_hints":[],"producers":[],"unsupported":[],"limited":false}).to_string()};
        let stored = core_graph::coverage::StoredCoverage {
            inventory_hash: record.inventory_hash(),
            record,
        };
        let qualification = context_hub::coverage::CoverageQualification::new(
            &[context_hub::coverage::ScopedRepo {
                repo_key: "fixture/repo".into(),
                registered_source_id: Some("src:fixture".into()),
                config_hash: "fixture-config".into(),
                layer_hints: None,
            }],
            std::slice::from_ref(&stored),
        )
        .unwrap();
        let proof = qualification.durable_proof(snapshot.id()).unwrap();
        let archive =
            InvestigationCoverageArchive::new(proof.clone(), vec![stored.clone()]).unwrap();
        let mut obligation = Obligation::Inventory {
            obligation_id: String::new(),
            target: InventoryTarget {
                context_id: proof.context_id.clone(),
                repo: "fixture/repo".into(),
                inventory_hash: stored.inventory_hash,
            },
        };
        let hash = obligation.content_id().unwrap();
        if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
            *obligation_id = hash;
        }
        let request = StagedObligationRequest {
            schema_version: if progressive {
                6
            } else if legacy {
                4
            } else {
                5
            },
            request_nonce: "completion-test".into(),
            specialist_id: if progressive {
                "domain-analyst@9"
            } else if legacy {
                "domain-analyst@7"
            } else {
                "domain-analyst@8"
            }
            .into(),
            obligations: ObligationEnvelope {
                schema_version: if legacy { 1 } else { 2 },
                contract: if legacy {
                    "investigation-completion-obligations@1"
                } else {
                    "investigation-completion-obligations@2"
                }
                .into(),
                obligations: vec![obligation],
            },
            inspection_requirements: None,
            question: "Inspect explicit completion requirements".into(),
            scope: InvestigationScope::All,
            provider_mode: mode,
            limit_profile: policy.profile().into(),
            expected_graph_revision: None,
            expected_context_id: proof.context_id.clone(),
            conversation_id: None,
            parent_id: None,
        };
        let provider = InvestigationProvider {
            mode,
            provider_id: "local:fixture".into(),
            model: "test-model".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            deployment: None,
            protocol_version: None,
            available: true,
            unavailable_reason: None,
        };
        let start = store
            .start_completion_investigation(&request, Some(&provider))
            .unwrap();
        let id = start.detail.summary.investigation_id.clone();
        let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
        let plan = store
            .claim_plan(start.detail.summary.job_id, ClaimMode::StartQueued)
            .unwrap();
        let reservation = locks.try_reserve(plan.lock_target()).unwrap();
        let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
        let ledger = InvestigationInputLedger {
            schema_version: 2,
            coverage: Some(proof.clone()),
            coverage_reads: vec![],
            artifact_reads: vec![],
            graph_snapshot_id: snapshot.id().into(),
            scope_snapshot_id: snapshot.id().into(),
            revision: 0,
            selected_facts: vec![],
            receipt_references: vec![],
            citations: vec![],
            queries: vec![],
            supplied_history_hash: None,
            supplied_history_bytes: 0,
        };
        if prepare_input {
            let detail = store.investigation(&id).unwrap();
            store
                .advance_investigation(
                    &execution,
                    detail.summary.revision,
                    InvestigationTransition::InputPrepared {
                        coverage_archive: Some(Box::new(archive.clone())),
                        ledger: Box::new(ledger),
                        usage: detail.usage,
                    },
                )
                .unwrap();
        }
        Self {
            directory,
            store,
            execution,
            id,
            request,
            archive,
            inventory_details: vec![],
        }
    }
    fn prepare_step(&mut self) -> (InvestigationInput, PendingInvestigationStep) {
        let detail = self.store.investigation(&self.id).unwrap();
        let ledger = self.store.investigation_ledger(&self.id).unwrap().unwrap();
        let basis = self.store.investigation_turn_basis(&self.id).unwrap();
        let metadata = self
            .store
            .investigation_completion_metadata(&self.id)
            .unwrap();
        let policy = InvestigationPolicy::for_limits(&detail.limits).unwrap();
        let prepare = if detail.summary.specialist_id.uses_query_history_disclosure() {
            InvestigationInput::prepare_v8_with_policy
        } else {
            InvestigationInput::prepare_v7_with_policy
        };
        let input = prepare(
            &self.request.question,
            detail.summary.specialist_id,
            ledger,
            &[],
            &[],
            &[],
            &detail.usage,
            &basis,
            &self.request,
            &metadata,
            &self.inventory_details,
            policy,
        )
        .unwrap();
        let action = input
            .action(format!("step-{}", detail.usage.model_invocations + 1))
            .unwrap();
        let provider = OllamaProvider::new(
            "http://127.0.0.1:11434",
            "test-model",
            Duration::from_secs(1),
        )
        .unwrap();
        let prepared = EgressFirewall::new(EgressPolicy::default())
            .prepare_bounded(&provider, &action, &CompletionLimits::default())
            .unwrap();
        let disclosure = input
            .seal_disclosure_with_policy(&prepared, policy)
            .unwrap()
            .unwrap();
        let step = PendingInvestigationStep {
            step_id: format!("step-{}", detail.usage.model_invocations + 1),
            payload_hash: prepared.preview().egress.payload_hash.clone(),
            input_ledger_hash: input.ledger().fingerprint_with_policy(policy).unwrap(),
            input_bytes: serde_json::to_vec(&action.payload).unwrap().len(),
            generated_tokens: 2048,
            disclosure: Some(disclosure),
        };
        (input, step)
    }
    fn step(&mut self) -> (InvestigationInput, PendingInvestigationStep) {
        let (input, step) = self.prepare_step();
        if self.request.provider_mode == InvestigationProviderMode::Cloud {
            let waiting = self
                .advance(InvestigationTransition::AwaitConsent { step: step.clone() })
                .unwrap();
            self.store
                .approve_investigation(
                    &self.id,
                    waiting.summary.revision,
                    &step.step_id,
                    &step.payload_hash,
                )
                .unwrap();
        }
        self.advance(InvestigationTransition::BeginInvocation { step: step.clone() })
            .unwrap();
        (input, step)
    }
    fn advance(
        &mut self,
        transition: InvestigationTransition,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        let revision = self.store.investigation(&self.id)?.summary.revision;
        self.store
            .advance_investigation(&self.execution, revision, transition)
    }
    fn finish() -> StagedFinish {
        StagedFinish {
            narrative: None,
            schema_version: 1,
            outcome: CompletionOutcome::InsufficientEvidence,
            action: InvestigationAction::Finish {
                findings: vec![],
                knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
                limitations: vec!["Explicit evidence is insufficient.".into()],
            },
        }
    }
    fn response(step: &PendingInvestigationStep, action_hash: String) -> InvestigationResponseMeta {
        InvestigationResponseMeta {
            finish_feedback_witness: None,
            finish_telemetry: None,
            step_id: step.step_id.clone(),
            response_hash: core_prov::content_hash(b"actual bounded fixture response"),
            action_hash,
            observed_model: Some("test-model".into()),
            reported_input_tokens: Some(10),
            reported_output_tokens: Some(5),
            reported_cache_read_tokens: None,
            active_milliseconds: 1,
        }
    }
    fn refuse(&mut self) -> InvestigationDetail {
        let (input, step) = self.step();
        let finish = Self::finish();
        let response = Self::response(
            &step,
            core_prov::content_hash(
                &serde_json::to_vec(&serde_json::to_value(&finish).unwrap()).unwrap(),
            ),
        );
        self.advance(InvestigationTransition::FinishRefused {
            request: Box::new(self.request.clone()),
            response,
            finish: Box::new(finish),
            input: Box::new(input),
        })
        .unwrap()
    }
}

// AC-0266: real V7 request/claim/input/seal/refusal paths preserve exact intent,
// first refusal Ready, second atomic no-result terminal, and immutable history.
#[test]
fn completion_v7_refusals_are_atomic_ready_then_terminal_and_reopen() {
    let mut fixture = Fixture::new();
    let first = fixture.refuse();
    assert_eq!(first.summary.schema_version, 4);
    assert_eq!(first.summary.status, InvestigationStatus::Running);
    assert_eq!(first.completion.as_ref().unwrap().refusal_count, 1);
    assert!(!first.summary.has_result);
    assert_eq!(first.usage.reported_input_tokens, Some(10));
    assert_eq!(
        fixture
            .store
            .investigation_ledger(&fixture.id)
            .unwrap()
            .unwrap()
            .revision,
        0
    );
    let basis = fixture.store.investigation_turn_basis(&fixture.id).unwrap();
    assert!(basis.previous_ledger.is_none());
    assert!(basis.completed_tool.is_none());
    let metadata = fixture
        .store
        .investigation_completion_metadata(&fixture.id)
        .unwrap();
    assert_eq!(metadata.refusal_count, 1);
    assert!(metadata.feedback.is_some());
    let second = fixture.refuse();
    assert_eq!(second.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        second.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::ObligationsUnmet)
    );
    assert_eq!(second.usage.model_invocations, 2);
    assert_eq!(second.usage.reported_input_tokens, Some(20));
    assert!(!second.summary.has_result);
    assert!(matches!(
        second.completion.as_ref().unwrap().assessment_basis,
        CompletionAssessmentBasis::TerminalObservation { .. }
    ));
    assert!(
        fixture
            .store
            .investigation_result(&fixture.id)
            .unwrap()
            .is_none()
    );
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), second);
    assert_eq!(
        reopened.investigation_history(None, None).unwrap().items[0],
        second.summary
    );
    assert!(
        fixture
            .store
            .investigation_completion_metadata(&fixture.id)
            .is_err()
    );
}

// AC-0266: a known unmet response racing cancellation retains observed usage,
// but Cancelled wins even on the refusal that would otherwise exhaust the cap.
#[test]
fn completion_v7_cancelled_late_refusal_preserves_usage_and_never_repairs() {
    for second in [false, true] {
        let mut fixture = Fixture::new();
        if second {
            fixture.refuse();
        }
        let (input, step) = fixture.step();
        fixture.store.cancel_investigation(&fixture.id).unwrap();
        let finish = Fixture::finish();
        let response = Fixture::response(
            &step,
            core_prov::content_hash(
                &serde_json::to_vec(&serde_json::to_value(&finish).unwrap()).unwrap(),
            ),
        );
        let detail = fixture
            .advance(InvestigationTransition::FinishRefused {
                request: Box::new(fixture.request.clone()),
                response,
                finish: Box::new(finish),
                input: Box::new(input),
            })
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Cancelled);
        assert!(detail.summary.completion_outcome.is_none());
        assert!(!detail.summary.has_result);
        assert_eq!(
            detail.usage.reported_input_tokens,
            Some(if second { 20 } else { 10 })
        );
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
    }
}

// AC-0266: an actual admitted inventory read with an exact retained target can
// complete unavailable without inventing input, and permits an insufficient finish.
#[test]
fn completion_v7_unavailable_attempt_is_durable_without_acquisition_revision() {
    let mut fixture = Fixture::new();
    let (_, step) = fixture.step();
    let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0] else {
        unreachable!()
    };
    let read = ReadIntent::Inventory {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: 1,
    };
    assert!(
        fixture
            .archive
            .read(&InvestigationCoverageRead {
                context_id: target.context_id.clone(),
                repo: target.repo.clone(),
                inventory_hash: target.inventory_hash.clone(),
                max_bytes: 1
            })
            .is_err()
    );
    let response = Fixture::response(&step, staged::read_action_hash(&read).unwrap());
    fixture
        .advance(InvestigationTransition::ActionAdmitted {
            response,
            tool: Some(InvestigationTool::ReadCoverageInventory),
            result: None,
        })
        .unwrap();
    fixture
        .advance(InvestigationTransition::BeginTool {
            tool: InvestigationTool::ReadCoverageInventory,
        })
        .unwrap();
    fixture
        .advance(InvestigationTransition::ReadUnavailable {
            read: read.clone(),
            reason: UnavailableReason::InventoryExceedsMaxBytes,
        })
        .unwrap();
    // Retrying the same completed step cannot append an identical attempt.
    assert!(matches!(
        fixture.advance(InvestigationTransition::ReadUnavailable {
            read,
            reason: UnavailableReason::InventoryExceedsMaxBytes,
        }),
        Err(InvestigationStoreError::Stale)
    ));
    assert_eq!(
        fixture
            .store
            .investigation_ledger(&fixture.id)
            .unwrap()
            .unwrap()
            .revision,
        0
    );
    let attempts = fixture
        .store
        .investigation_completion_attempts(&fixture.id)
        .unwrap();
    assert_eq!(attempts.len(), 1);
    let (input, step) = fixture.step();
    let ledgers = fixture
        .store
        .investigation_completion_attempt_ledgers(&fixture.id)
        .unwrap();
    let evidence = attempts
        .iter()
        .zip(&ledgers)
        .map(|(record, ledger)| AttemptEvidence { record, ledger })
        .collect::<Vec<_>>();
    let finish = Fixture::finish();
    let result = input
        .admit_completion_finish(
            &fixture.request,
            &fixture.id,
            &step.step_id,
            step.disclosure.as_ref().unwrap(),
            &finish,
            &evidence,
            Some("test-model".into()),
            "now".into(),
        )
        .unwrap()
        .ok()
        .expect("insufficient completion accepted after real attempt");
    let response = Fixture::response(
        &step,
        core_prov::content_hash(
            &serde_json::to_vec(&serde_json::to_value(&finish).unwrap()).unwrap(),
        ),
    );
    let detail = fixture
        .advance(InvestigationTransition::ActionAdmitted {
            response,
            tool: None,
            result: Some(Box::new(result)),
        })
        .unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::InsufficientEvidence)
    );
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
}

// AC-0266: refusal headroom is reserved again after real successful acquisition;
// insufficient task capacity stops before reserving a third model invocation.
#[test]
fn completion_v7_refusal_then_acquisition_rechecks_terminal_headroom() {
    let mut fixture = Fixture::new();
    fixture.refuse();
    let (_, step) = fixture.step();
    let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0] else {
        unreachable!()
    };
    let request = InvestigationCoverageRead {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: 32768,
    };
    let raw = serde_json::to_vec(&fixture.archive.read(&request).unwrap()).unwrap();
    let read = ReadIntent::Inventory {
        context_id: request.context_id.clone(),
        repo: request.repo.clone(),
        inventory_hash: request.inventory_hash.clone(),
        max_bytes: request.max_bytes,
    };
    fixture
        .advance(InvestigationTransition::ActionAdmitted {
            response: Fixture::response(&step, staged::read_action_hash(&read).unwrap()),
            tool: Some(InvestigationTool::ReadCoverageInventory),
            result: None,
        })
        .unwrap();
    fixture
        .advance(InvestigationTransition::BeginTool {
            tool: InvestigationTool::ReadCoverageInventory,
        })
        .unwrap();
    let mut ledger = fixture
        .store
        .investigation_ledger(&fixture.id)
        .unwrap()
        .unwrap();
    ledger.revision += 1;
    ledger
        .coverage_reads
        .push(InvestigationCoverageReadManifest {
            request,
            response_hash: core_prov::content_hash(&raw),
            response_bytes: raw.len(),
        });
    let usage = fixture.store.investigation(&fixture.id).unwrap().usage;
    fixture
        .advance(InvestigationTransition::InputAdmitted {
            ledger: Box::new(ledger),
            usage,
        })
        .unwrap();
    assert_eq!(
        fixture
            .store
            .investigation_completion_attempts(&fixture.id)
            .unwrap()
            .len(),
        1
    );
    fixture
        .inventory_details
        .push(String::from_utf8(raw).unwrap());
    let (_, next_step) = fixture.prepare_step();
    let before = fixture.store.investigation(&fixture.id).unwrap();
    let remaining = remaining_task_capacity(&fixture.store.conn, &fixture.id).unwrap();
    let padding = remaining - (192 * 1024 - 1) - 64 - 256;
    fixture
        .store
        .conn
        .execute(
            "INSERT INTO investigation_responses VALUES (?1,'capacity-padding',zeroblob(?2),?3)",
            params![fixture.id, padding as i64, "0".repeat(64)],
        )
        .unwrap();
    assert_eq!(
        fixture.advance(InvestigationTransition::BeginInvocation { step: next_step }),
        Err(InvestigationStoreError::Capacity)
    );
    let after = fixture.store.investigation(&fixture.id).unwrap();
    assert_eq!(after.usage.model_invocations, 2);
    assert_eq!(after.summary.revision, before.summary.revision);
    assert_eq!(after.completion.as_ref().unwrap().refusal_count, 1);
}

// AC-0266: reserved FirewallRefused remains decode-only; no host persistence
// path can manufacture a per-read firewall witness from a redacted acquisition.
#[test]
fn completion_v7_reserved_firewall_refusal_cannot_be_persisted() {
    let mut fixture = Fixture::new();
    let tx = fixture.store.conn.transaction().unwrap();
    let task = load_task(&tx, &fixture.id).unwrap();
    let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0] else {
        unreachable!()
    };
    let read = ReadIntent::Inventory {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: 32768,
    };
    assert_eq!(
        append_attempt(
            &tx,
            &task,
            &read,
            AttemptOutcome::FirewallRefused {
                acquired_attempt_id: "a".repeat(64),
                observation_hash: "b".repeat(64)
            },
            0
        ),
        Err(InvestigationStoreError::Invalid)
    );
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM completion_attempts WHERE task_id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    tx.rollback().unwrap();
}

// AC-0266: real owner recovery distinguishes an unknown in-flight response
// from a known refusal already durably transitioned to Ready; neither resumes.
#[test]
fn completion_v7_owner_recovery_keeps_known_refusal_and_unknown_inference_distinct() {
    for state in 0..3 {
        let known_refusal = state > 0;
        let known_ready = state == 1;
        let mut fixture = Fixture::new();
        if known_refusal {
            fixture.refuse();
        }
        if !known_ready {
            fixture.step();
        }
        let candidate = fixture
            .store
            .investigation_recovery_candidates()
            .unwrap()
            .remove(0);
        let locks = JobExecutionLocks::open(
            fixture.directory.path(),
            fixture.store.execution_namespace(),
        )
        .unwrap();
        drop(fixture.execution);
        let reservation = locks.try_reserve(candidate.lock_target()).unwrap();
        let detail = fixture
            .store
            .recover_investigation(&candidate, reservation)
            .unwrap()
            .unwrap();
        assert_eq!(
            detail.summary.status,
            if known_ready {
                InvestigationStatus::Interrupted
            } else {
                InvestigationStatus::OutcomeUnknown
            }
        );
        assert!(detail.summary.completion_outcome.is_none());
        assert!(!detail.summary.has_result);
        assert_eq!(
            detail.completion.as_ref().unwrap().refusal_count,
            if known_refusal { 1 } else { 0 }
        );
        assert_eq!(
            detail.usage.reported_input_tokens,
            if known_ready { Some(10) } else { None }
        );
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
    }
}

// AC-0266: generation-two completion projections and immutable intent cannot
// be relabeled by changing a task body together with its ordinary row hash.
#[test]
fn completion_v7_rehashed_projection_and_intent_tampering_fail_fresh_load() {
    for field in ["refusal_count", "outcome", "envelope", "generation"] {
        let mut fixture = Fixture::new();
        fixture.refuse();
        let bytes: Vec<u8> = fixture
            .store
            .conn
            .query_row(
                "SELECT record FROM investigation_tasks WHERE id=?1",
                [&fixture.id],
                |row| row.get(0),
            )
            .unwrap();
        let mut task: StoredTask = decode_record(std::str::from_utf8(&bytes).unwrap()).unwrap();
        match field {
            "refusal_count" => task.detail.completion.as_mut().unwrap().refusal_count = 0,
            "outcome" => {
                task.detail.summary.completion_outcome =
                    Some(InvestigationCompletionOutcome::Supported)
            }
            "envelope" => {
                let intent = task.completion_intent.as_mut().unwrap();
                if let Obligation::Inventory { target, .. } = &mut intent.obligations.obligations[0]
                {
                    target.inventory_hash = "0".repeat(64);
                }
            }
            _ => task.schema_version = 1,
        }
        let body = encode(&task, MAX_RECORD_BYTES).unwrap();
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
                params![body, core_prov::content_hash(&body), fixture.id],
            )
            .unwrap();
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert!(reopened.investigation(&fixture.id).is_err(), "{field}");
        assert!(
            reopened.investigation_history(None, None).is_err(),
            "{field}"
        );
    }
}

// AC-0266: a refusal clears approval; next input is genuinely resealed and
// requires a new exact cloud-consent decision before any second invocation.
#[test]
fn completion_v7_first_refusal_requires_fresh_seal_and_cloud_consent() {
    let mut fixture = Fixture::with_mode(InvestigationProviderMode::Cloud);
    fixture.refuse();
    let before = fixture.store.investigation(&fixture.id).unwrap();
    let (_, next) = fixture.prepare_step();
    assert_eq!(
        fixture.advance(InvestigationTransition::BeginInvocation { step: next.clone() }),
        Err(InvestigationStoreError::Stale)
    );
    let waiting = fixture
        .advance(InvestigationTransition::AwaitConsent { step: next.clone() })
        .unwrap();
    assert_eq!(
        fixture.store.approve_investigation(
            &fixture.id,
            waiting.summary.revision,
            "step-1",
            &next.payload_hash
        ),
        Err(InvestigationStoreError::Stale)
    );
    fixture
        .store
        .approve_investigation(
            &fixture.id,
            waiting.summary.revision,
            &next.step_id,
            &next.payload_hash,
        )
        .unwrap();
    let invoked = fixture
        .advance(InvestigationTransition::BeginInvocation { step: next })
        .unwrap();
    assert_eq!(
        invoked.usage.model_invocations,
        before.usage.model_invocations + 1
    );
    let events = fixture.store.investigation_events(&fixture.id, 0).unwrap();
    assert_eq!(
        events
            .items
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::ConsentApproved)
            .count(),
        2
    );
}

// AC-0266: a coordinated legacy rewrite cannot hide durable V7 invocation and
// refusal evidence or make a first-refusal Ready task eligible for legacy resume.
#[test]
fn completion_v7_coordinated_legacy_downgrade_rejects_durable_evidence() {
    for with_attempt in [false, true] {
        let mut fixture = Fixture::new();
        if with_attempt {
            let (_, step) = fixture.step();
            let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0]
            else {
                unreachable!()
            };
            let read = ReadIntent::Inventory {
                context_id: target.context_id.clone(),
                repo: target.repo.clone(),
                inventory_hash: target.inventory_hash.clone(),
                max_bytes: 1,
            };
            let response = Fixture::response(&step, staged::read_action_hash(&read).unwrap());
            fixture
                .advance(InvestigationTransition::ActionAdmitted {
                    response,
                    tool: Some(InvestigationTool::ReadCoverageInventory),
                    result: None,
                })
                .unwrap();
            fixture
                .advance(InvestigationTransition::BeginTool {
                    tool: InvestigationTool::ReadCoverageInventory,
                })
                .unwrap();
            fixture
                .advance(InvestigationTransition::ReadUnavailable {
                    read,
                    reason: UnavailableReason::InventoryExceedsMaxBytes,
                })
                .unwrap();
        }
        if !with_attempt {
            fixture.refuse();
        }
        let bytes: Vec<u8> = fixture
            .store
            .conn
            .query_row(
                "SELECT record FROM investigation_tasks WHERE id=?1",
                [&fixture.id],
                |row| row.get(0),
            )
            .unwrap();
        let mut task: StoredTask = decode_record(std::str::from_utf8(&bytes).unwrap()).unwrap();
        task.schema_version = 1;
        task.completion_intent = None;
        task.detail.summary.schema_version = 3;
        task.detail.summary.specialist_id = SpecialistId::DomainAnalystV6;
        task.detail.specialist = SpecialistId::DomainAnalystV6.definition().unwrap();
        task.detail.summary.obligation_status = None;
        task.detail.summary.completion_outcome = None;
        task.detail.completion = None;
        let body = encode(&task, MAX_RECORD_BYTES).unwrap();
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
                params![body, core_prov::content_hash(&body), fixture.id],
            )
            .unwrap();
        // The mutable legacy task record is internally consistent: only the
        // independently retained completion evidence makes this downgrade invalid.
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        load_task_base(&tx, &fixture.id).unwrap();
        tx.commit().unwrap();
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert!(reopened.investigation(&fixture.id).is_err());
        assert!(reopened.investigation_history(None, None).is_err());
    }
}

#[test]
fn completion_attempt_journal_resolves_saved_policy_before_bounded_replay() {
    // AC-0266: malicious journal rows never become witnesses. This isolates
    // admission order: exact policy, raw row count, then individual decoding.
    let mut fixture = Fixture::new();
    let tx = fixture.store.conn.transaction().unwrap();
    let mut task = load_task_base(&tx, &fixture.id).unwrap();
    let limit = saved_policy(&task).unwrap().max_completed_attempts();
    assert_eq!(limit, 16);
    for sequence in 1..=limit {
        insert_body(
            &tx,
            "completion_attempts",
            &fixture.id,
            &signed(sequence as u64).unwrap(),
            &json!({}),
            MAX_RECORD_BYTES,
        )
        .unwrap();
    }
    assert!(matches!(
        attempts(&tx, &task),
        Err(InvestigationStoreError::Invalid)
    ));
    insert_body(
        &tx,
        "completion_attempts",
        &fixture.id,
        &signed(limit as u64 + 1).unwrap(),
        &json!({}),
        MAX_RECORD_BYTES,
    )
    .unwrap();
    assert!(matches!(
        attempts(&tx, &task),
        Err(InvestigationStoreError::Capacity)
    ));
    task.detail.limits.tool_actions += 1;
    assert!(matches!(
        attempts(&tx, &task),
        Err(InvestigationStoreError::Invalid)
    ));
    tx.rollback().unwrap();
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert!(
        reopened
            .investigation_completion_attempts(&fixture.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn completion_attempt_schema_upgrade_preserves_records_and_rolls_back() {
    // AC-0266: retained attempt bytes survive an atomic schema upgrade.
    assert!(ATTEMPT_SCHEMA[0].contains(&format!(
        "CHECK(sequence BETWEEN 1 AND {})",
        agents::investigation::MAX_STORED_COMPLETION_ATTEMPTS
    )));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("attempts.db");
    let mut connection = Connection::open(&path).unwrap();
    connection.execute_batch(LEGACY_ATTEMPT_TABLE).unwrap();
    for sql in &ATTEMPT_SCHEMA[1..] {
        connection.execute_batch(sql).unwrap();
    }
    connection
        .execute(
            "INSERT INTO completion_attempts VALUES (?1,1,?2,?3)",
            params![
                "old-task",
                b"opaque historical body".as_slice(),
                "original hash"
            ],
        )
        .unwrap();
    assert!(validate_schema_version(&connection).unwrap());
    {
        let tx = connection.transaction().unwrap();
        initialize(&tx).unwrap();
        assert!(!validate_schema_version(&tx).unwrap());
        // Dropping the transaction restores old table and protections.
    }
    assert!(validate_schema_version(&connection).unwrap());
    {
        let tx = connection.transaction().unwrap();
        initialize(&tx).unwrap();
        tx.commit().unwrap();
    }
    drop(connection);
    let connection = Connection::open(path).unwrap();
    assert!(!validate_schema_version(&connection).unwrap());
    let stored: (Vec<u8>, String) = connection
        .query_row(
            "SELECT body,body_hash FROM completion_attempts WHERE task_id='old-task'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        stored,
        (b"opaque historical body".to_vec(), "original hash".into())
    );
    assert!(
        connection
            .execute("UPDATE completion_attempts SET body=x'00'", [])
            .is_err()
    );
    assert!(
        connection
            .execute("DELETE FROM completion_attempts", [])
            .is_err()
    );
    // The database's structural cap supports the new policy. Runtime admission
    // still independently enforces each saved policy's lower exact bound.
    for sequence in 1..=112 {
        connection
            .execute(
                "INSERT INTO completion_attempts VALUES ('new-task',?1,x'00','hash')",
                [sequence],
            )
            .unwrap();
    }
    assert!(
        connection
            .execute(
                "INSERT INTO completion_attempts VALUES ('new-task',113,x'00','hash')",
                []
            )
            .is_err()
    );
}

#[test]
fn completion_attempt_schema_upgrade_rejects_unknown_schema_without_repair() {
    // AC-0266: an upgrade is not permission to repair a tampered historical store.
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(LEGACY_ATTEMPT_TABLE).unwrap();
    connection.execute_batch(ATTEMPT_SCHEMA[1]).unwrap();
    // Missing immutable-delete protection is not the recognized legacy schema.
    {
        let tx = connection.transaction().unwrap();
        assert!(matches!(
            initialize(&tx),
            Err(InvestigationStoreError::Invalid)
        ));
    }
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'completion_*'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='completion_attempts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sql, LEGACY_ATTEMPT_TABLE);
}

#[test]
fn completion_metadata_authority_rejects_altered_saved_policy() {
    // AC-0266: internal replay/metadata entrypoints cannot accept caller limits
    // even when invoked without the outer public retained-task loader.
    let mut fixture = Fixture::new();
    let tx = fixture.store.conn.transaction().unwrap();
    let mut task = load_task_base(&tx, &fixture.id).unwrap();
    task.detail.limits.evidence_bytes += 1;
    assert!(matches!(
        metadata(&tx, &task, None),
        Err(InvestigationStoreError::Invalid)
    ));
    assert!(matches!(
        raw_refusals(&tx, &task),
        Err(InvestigationStoreError::Invalid)
    ));
    assert!(matches!(
        reserve_invocation(&tx, &task),
        Err(InvestigationStoreError::Invalid)
    ));
    assert!(matches!(
        validate_saved(&tx, &task),
        Err(InvestigationStoreError::Invalid)
    ));
    assert!(matches!(
        refresh_observation(&tx, &mut task),
        Err(InvestigationStoreError::Invalid)
    ));
    tx.rollback().unwrap();
    assert_eq!(
        fixture.store.investigation(&fixture.id).unwrap().limits,
        InvestigationLimits::default()
    );
}

#[test]
fn completion_policy_reservation_covers_serialized_large_refusal_shape() {
    // AC-0266: this measures typed candidate records without registering V2,
    // publishing fabricated witnesses, or claiming a completed production run.
    let mut fixture = Fixture::new();
    fixture.refuse();
    let tx = fixture.store.conn.transaction().unwrap();
    let mut task = load_task_base(&tx, &fixture.id).unwrap();
    assert_eq!(
        invocation_reservation_bytes(&task, &INVESTIGATION_V1_POLICY).unwrap(),
        192 * 1024
    );
    let mut journal = staged::load(&tx, &fixture.id).unwrap().remove(0);
    let base = fixture.request.obligations.obligations[0].clone();
    let obligations = (0..INVESTIGATION_V2_POLICY.max_completion_obligations())
        .map(|index| {
            let mut obligation = base.clone();
            if let Obligation::Inventory { target, .. } = &mut obligation {
                target.repo = format!("{index:03}{}", "r".repeat(253));
            }
            let id = obligation.content_id().unwrap();
            if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
                *obligation_id = id;
            }
            obligation
        })
        .collect();
    let envelope = ObligationEnvelope {
        schema_version: 2,
        contract: "investigation-completion-obligations@2".into(),
        obligations,
    }
    .normalized_with_policy(&INVESTIGATION_V2_POLICY)
    .unwrap();
    let obligations_hash = envelope
        .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    journal.record.assessment = envelope
        .obligations
        .iter()
        .map(|obligation| ObligationObservation {
            obligation_id: obligation.key().1.into(),
            kind: obligation.key().0.into(),
            attempted: true,
            acquired: true,
            exactly_supplied: false,
            unmet: Some(RefusalReason::AcquiredNotSupplied),
            last_attempt_outcome: Some(CompletionAttemptOutcome::Acquired),
            capacity_disclosed: false,
        })
        .collect();
    journal.record.feedback.unmet = journal
        .record
        .assessment
        .iter()
        .map(|item| FeedbackEntry {
            obligation_id: item.obligation_id.clone(),
            kind: item.kind.clone(),
            reason: RefusalReason::AcquiredNotSupplied,
        })
        .collect();
    journal.record.obligations_hash = obligations_hash.clone();
    journal.attempts = (0..INVESTIGATION_V2_POLICY.max_completed_attempts())
        .map(|index| {
            let obligation = &envelope.obligations[index % envelope.obligations.len()];
            let Obligation::Inventory { target, .. } = obligation else {
                unreachable!()
            };
            let mut attempt = AttemptRecord {
                schema_version: 1,
                attempt_id: String::new(),
                task_id: fixture.id.clone(),
                request_hash: journal.record.request_hash.clone(),
                obligations_hash: obligations_hash.clone(),
                obligation_id: obligation.key().1.into(),
                step_id: format!("step-{index:03}{}", "s".repeat(120)),
                read: ReadIntent::Inventory {
                    context_id: target.context_id.clone(),
                    repo: target.repo.clone(),
                    inventory_hash: target.inventory_hash.clone(),
                    max_bytes: MAX_QUERY_BYTES,
                },
                input_ledger_hash: journal.record.input_ledger_hash.clone(),
                input_revision: index as u64,
                outcome: AttemptOutcome::Unavailable {
                    reason: UnavailableReason::InventoryExceedsMaxBytes,
                },
            };
            attempt.attempt_id = attempt.content_id().unwrap();
            attempt
        })
        .collect();
    task.detail.limits = INVESTIGATION_V2_POLICY.limits();
    task.detail.summary.specialist_id = SpecialistId::DomainAnalystV8;
    task.detail.specialist = SpecialistId::DomainAnalystV8.definition().unwrap();
    let intent = task.completion_intent.as_mut().unwrap();
    intent.schema_version = 2;
    intent.limit_profile = Some(INVESTIGATION_V2_POLICY.profile().into());
    intent.specialist = SpecialistId::DomainAnalystV8;
    intent.obligations = envelope.clone();
    intent.obligations_hash = obligations_hash.clone();
    let detail = task.detail.completion.as_mut().unwrap();
    detail.obligations = envelope;
    detail.obligations_hash = obligations_hash;
    detail.assessment = journal.record.assessment.clone();
    let binding = staged::binding_reservation_bytes(&task, &INVESTIGATION_V2_POLICY).unwrap();
    let ordinary_row = 64 + 256;
    let actual = encode(
        &journal,
        staged::journal_entry_bytes(&INVESTIGATION_V2_POLICY),
    )
    .unwrap()
    .len()
        + ordinary_row
        + encode(&task, MAX_RECORD_BYTES).unwrap().len()
        + 64
        + 3 * 128
        + 1024
        + binding
        + journal
            .record
            .feedback
            .bytes_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap()
            .len()
        + ordinary_row
        + encode(&journal.response, 4096).unwrap().len()
        + ordinary_row
        + 3 * (16 * 1024 + ordinary_row);
    let reservation = invocation_reservation_bytes(&task, &INVESTIGATION_V2_POLICY).unwrap();
    assert!(actual <= reservation);
    assert!(reservation > TERMINAL_RESERVE);
    assert!(reservation < TASK_CAPACITY - TERMINAL_RESERVE);
    // The terminal transaction is covered by previously reserved nonterminal
    // space; its admission cannot rely on TERMINAL_RESERVE by itself.
    check_invocation_capacity(&tx, &task, &INVESTIGATION_V2_POLICY).unwrap();
    let remaining = remaining_task_capacity(&tx, &fixture.id).unwrap();
    let padding = remaining - (reservation - 1) - ordinary_row;
    tx.execute("INSERT INTO investigation_responses VALUES (?1,'candidate-reservation-padding',zeroblob(?2),?3)", params![fixture.id, padding as i64, "0".repeat(64)]).unwrap();
    assert_eq!(
        check_invocation_capacity(&tx, &task, &INVESTIGATION_V2_POLICY),
        Err(InvestigationStoreError::Capacity)
    );
    let after = load_task_base(&tx, &fixture.id).unwrap();
    assert_eq!(after.detail.usage.model_invocations, 1);
    tx.rollback().unwrap();
}

// AC-0281: exercise the registered V2 policy through the real durable route.
#[test]
fn completion_v2_durable_nonce_reopen_and_saved_policy_are_exact() {
    let mut fixture = Fixture::registered_v2();
    let before = fixture.store.investigation(&fixture.id).unwrap();
    assert_eq!(before.limits, INVESTIGATION_V2_POLICY.limits());
    assert_eq!(before.summary.specialist_id, SpecialistId::DomainAnalystV8);
    // AC-0420: the current-family public reader also returns the stored detail subtree.
    let mut projected = reads::InvestigationDetailRead::from_detail(before.clone()).unwrap();
    projected.reported_usage_exact = None;
    assert_eq!(
        serde_json::to_vec(&projected).unwrap(),
        encode(&before, MAX_RECORD_BYTES).unwrap()
    );
    let repeated = fixture
        .store
        .start_completion_investigation(&fixture.request, None)
        .unwrap();
    assert!(!repeated.created);
    assert_eq!(repeated.detail, before);
    let mut changed = fixture.request.clone();
    changed.question.push('?');
    assert!(matches!(
        fixture.store.start_completion_investigation(&changed, None),
        Err(InvestigationStoreError::Conflict)
    ));
    // A separately valid legacy family cannot reuse the nonce of the V2 task.
    let mut legacy = fixture.request.clone();
    legacy.schema_version = 4;
    legacy.specialist_id = "domain-analyst@7".into();
    legacy.limit_profile = INVESTIGATION_V1_POLICY.profile().into();
    legacy.obligations.schema_version = 1;
    legacy.obligations.contract = "investigation-completion-obligations@1".into();
    assert!(matches!(
        fixture.store.start_completion_investigation(&legacy, None),
        Err(InvestigationStoreError::Conflict)
    ));
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), before);
    assert_eq!(
        reopened
            .investigation_completion_metadata(&fixture.id)
            .unwrap()
            .request_hash,
        fixture
            .request
            .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap()
    );
    let tx = fixture.store.conn.transaction().unwrap();
    let mut task = load_task_base(&tx, &fixture.id).unwrap();
    task.detail.limits.tool_actions -= 1;
    let body = encode(&task, MAX_RECORD_BYTES).unwrap();
    tx.execute(
        "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
        params![fixture.id, body, core_prov::content_hash(&body)],
    )
    .unwrap();
    tx.commit().unwrap();
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation(&fixture.id).unwrap_err(),
        InvestigationStoreError::Invalid
    );
}

#[test]
fn completion_v2_refusals_replay_exact_policy_and_publish_terminal_once() {
    let mut fixture = Fixture::registered_v2();
    let first = fixture.refuse();
    assert_eq!(first.summary.status, InvestigationStatus::Running);
    assert_eq!(first.completion.as_ref().unwrap().refusal_count, 1);
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), first);
    let feedback = reopened
        .investigation_completion_metadata(&fixture.id)
        .unwrap();
    feedback
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    assert_eq!(feedback.refusal_count, 1);
    let second = fixture.refuse();
    assert_eq!(second.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        second.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::ObligationsUnmet)
    );
    assert_eq!(second.usage.model_invocations, 2);
    assert!(!second.summary.has_result);
    let entries = staged::load(&fixture.store.conn, &fixture.id).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[1].record.previous_refusal.as_deref(),
        Some(entries[0].record.refusal_id.as_str())
    );
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), second);
    assert!(
        reopened
            .investigation_result(&fixture.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reopened.investigation_history(None, None).unwrap().items[0],
        second.summary
    );
}

#[test]
fn completion_v2_capacity_failure_precedes_invocation_and_token_reservation() {
    let mut fixture = Fixture::registered_v2();
    let (_, step) = fixture.prepare_step();
    let before = fixture.store.investigation(&fixture.id).unwrap();
    let tx = fixture.store.conn.transaction().unwrap();
    let task = load_task_base(&tx, &fixture.id).unwrap();
    let required = invocation_reservation_bytes(&task, &INVESTIGATION_V2_POLICY).unwrap();
    let remaining = remaining_task_capacity(&tx, &fixture.id).unwrap();
    let padding = remaining - (required - 1) - 64 - 256;
    tx.execute(
        "INSERT INTO investigation_responses VALUES (?1,'v2-capacity-padding',zeroblob(?2),?3)",
        params![fixture.id, padding as i64, "0".repeat(64)],
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        fixture.advance(InvestigationTransition::BeginInvocation { step }),
        Err(InvestigationStoreError::Capacity)
    );
    let after = fixture.store.investigation(&fixture.id).unwrap();
    assert_eq!(after.summary.revision, before.summary.revision);
    assert_eq!(after.usage, before.usage);
    let count: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM investigation_invocations WHERE task_id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), after);
}

#[test]
fn completion_v2_nine_durable_reads_finish_and_reopen_without_v1_fallback() {
    let mut fixture = Fixture::registered_v2();
    for _ in 0..9 {
        let (_, step) = fixture.step();
        let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0]
        else {
            unreachable!()
        };
        let request = InvestigationCoverageRead {
            context_id: target.context_id.clone(),
            repo: target.repo.clone(),
            inventory_hash: target.inventory_hash.clone(),
            max_bytes: MAX_QUERY_BYTES,
        };
        let raw = serde_json::to_vec(&fixture.archive.read(&request).unwrap()).unwrap();
        let read = ReadIntent::Inventory {
            context_id: request.context_id.clone(),
            repo: request.repo.clone(),
            inventory_hash: request.inventory_hash.clone(),
            max_bytes: request.max_bytes,
        };
        fixture
            .advance(InvestigationTransition::ActionAdmitted {
                response: Fixture::response(&step, staged::read_action_hash(&read).unwrap()),
                tool: Some(InvestigationTool::ReadCoverageInventory),
                result: None,
            })
            .unwrap();
        fixture
            .advance(InvestigationTransition::BeginTool {
                tool: InvestigationTool::ReadCoverageInventory,
            })
            .unwrap();
        let mut ledger = fixture
            .store
            .investigation_ledger(&fixture.id)
            .unwrap()
            .unwrap();
        ledger.revision += 1;
        ledger
            .coverage_reads
            .push(InvestigationCoverageReadManifest {
                request,
                response_hash: core_prov::content_hash(&raw),
                response_bytes: raw.len(),
            });
        let usage = fixture.store.investigation(&fixture.id).unwrap().usage;
        fixture
            .advance(InvestigationTransition::InputAdmitted {
                ledger: Box::new(ledger),
                usage,
            })
            .unwrap();
        fixture
            .inventory_details
            .push(String::from_utf8(raw).unwrap());
    }
    let acquired = fixture.store.investigation(&fixture.id).unwrap();
    assert_eq!(acquired.usage.tool_actions, 9);
    let ledger = fixture
        .store
        .investigation_ledger(&fixture.id)
        .unwrap()
        .unwrap();
    assert_eq!(ledger.coverage_reads.len(), 9);
    assert!(ledger.validate().is_err());
    ledger
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let attempts = fixture
        .store
        .investigation_completion_attempts(&fixture.id)
        .unwrap();
    let ledgers = fixture
        .store
        .investigation_completion_attempt_ledgers(&fixture.id)
        .unwrap();
    assert_eq!(attempts.len(), 9);
    let evidence = attempts
        .iter()
        .zip(&ledgers)
        .map(|(record, ledger)| AttemptEvidence { record, ledger })
        .collect::<Vec<_>>();
    let (input, step) = fixture.step();
    let finish = Fixture::finish();
    let result = input
        .admit_completion_finish_with_policy(
            &fixture.request,
            &fixture.id,
            &step.step_id,
            step.disclosure.as_ref().unwrap(),
            &finish,
            &evidence,
            Some("test-model".into()),
            "2026-10-21T12:00:00Z".into(),
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap()
        .ok()
        .expect("nine durable reads must admit the honest finish");
    let result_id = result.result_id.clone();
    let response = Fixture::response(
        &step,
        core_prov::content_hash(&serde_json::to_vec(&finish).unwrap()),
    );
    let completed = fixture
        .advance(InvestigationTransition::ActionAdmitted {
            response,
            tool: None,
            result: Some(Box::new(result)),
        })
        .unwrap();
    assert_eq!(completed.summary.status, InvestigationStatus::Completed);
    assert_eq!(completed.usage.model_invocations, 10);
    assert_eq!(
        completed.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::InsufficientEvidence)
    );
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), completed);
    assert_eq!(
        reopened
            .investigation_result(&fixture.id)
            .unwrap()
            .unwrap()
            .result_id,
        result_id
    );
}

#[path = "assessment_basis_tests.rs"]
mod assessment_basis_tests;

#[path = "prospective_attempt_tests.rs"]
mod prospective_attempt_tests;

#[path = "prospective_metadata_tests.rs"]
mod prospective_metadata_tests;

#[path = "attempt_storage_helper_tests.rs"]
mod attempt_storage_helper_tests;

#[path = "completion_snapshot_tests.rs"]
mod completion_snapshot_tests;

#[path = "next_prompt_contract_tests.rs"]
pub(in crate::jobs::investigations) mod next_prompt_contract_tests;

#[path = "next_prompt_initial_fixture.rs"]
pub(in crate::jobs::investigations) mod next_prompt_initial_fixture;

// AC-0322/0323: staged recovery uses the same App-only nonce partition.
#[test]
fn app_nonce_namespace_completion_recovery_ignores_foreign_tasks() {
    let fixture = Fixture::new();
    let original = fixture
        .store
        .existing_investigation_completion(&fixture.request)
        .unwrap()
        .unwrap();
    let mut task = {
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let task = load_execution_task(&tx, &fixture.id).unwrap();
        tx.commit().unwrap();
        task
    };
    task.detail.origin = InvestigationOrigin::Mcp {
        client_id: McpClientId::from_host_bytes([8; 16]),
    };
    task.mcp_intent_hash = Some("b".repeat(64));
    let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
    fixture.store.conn.execute("UPDATE investigation_tasks SET record=?2,record_hash=?3,request_namespace=?4 WHERE id=?1", params![fixture.id, bytes, core_prov::content_hash(&bytes), task.detail.origin.request_namespace()]).unwrap();
    assert!(
        fixture
            .store
            .existing_investigation_completion(&fixture.request)
            .unwrap()
            .is_none()
    );
    task.detail.origin = InvestigationOrigin::App;
    task.mcp_intent_hash = None;
    let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
    fixture.store.conn.execute("UPDATE investigation_tasks SET record=?2,record_hash=?3,request_namespace='app' WHERE id=?1", params![fixture.id, bytes, core_prov::content_hash(&bytes)]).unwrap();
    assert_eq!(
        fixture
            .store
            .existing_investigation_completion(&fixture.request)
            .unwrap(),
        Some(original)
    );
}

#[path = "mcp_start_completion_tests.rs"]
mod mcp_start_completion_tests;

#[test]
fn saved_completion_witness_reuse_preserves_observation_and_rejects_tampering() {
    // AC-0327 (#971): genuine saved states, compared with the reread path.
    for (policy, progressive) in [
        (&INVESTIGATION_V1_POLICY, false),
        (&INVESTIGATION_V2_POLICY, false),
        (&INVESTIGATION_V2_POLICY, true),
    ] {
        let mut fixture =
            Fixture::with_family(InvestigationProviderMode::Local, policy, progressive);
        for refusal_count in 0..=2 {
            if refusal_count > 0 {
                fixture.refuse();
            }
            let tx = fixture.store.conn.transaction().unwrap();
            let mut task = load_task_base(&tx, &fixture.id).unwrap();
            let entries = attempts(&tx, &task).unwrap();
            let raw = raw_refusals(&tx, &task).unwrap();
            assert_eq!(raw.len(), refusal_count);
            let reused = observation_from_entries(&task, &entries, &raw).unwrap();
            assert_eq!(reused, observation_detail(&tx, &task).unwrap());
            assert_eq!(Some(&reused), task.detail.completion.as_ref());
            validate_saved(&tx, &task).unwrap();
            task.detail.completion.as_mut().unwrap().request_hash = "0".repeat(64);
            assert!(matches!(
                validate_saved(&tx, &task),
                Err(InvestigationStoreError::Invalid)
            ));
            tx.rollback().unwrap();
            let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
            assert_eq!(
                reopened.investigation(&fixture.id).unwrap().completion,
                Some(reused)
            );
        }
    }
}

// AC-0327: real legacy parents exercise staged::load's nested base reader,
// including refusals, rather than assuming an @10 fixture covers that branch.
#[test]
fn parent_history_legacy_families_reopen_after_staged_refusals() {
    for (policy, progressive) in [
        (&INVESTIGATION_V1_POLICY, false),
        (&INVESTIGATION_V2_POLICY, false),
        (&INVESTIGATION_V2_POLICY, true),
    ] {
        let mut f = Fixture::with_family(InvestigationProviderMode::Local, policy, progressive);
        for count in 0..=2 {
            if count > 0 {
                f.refuse();
            }
            let parent = f.store.investigation(&f.id).unwrap();
            let history = f
                .store
                .investigation_next_parent_history(&f.id, &parent.summary.conversation_id)
                .unwrap()
                .into_history();
            let wire: serde_json::Value = serde_json::from_str(&history[0]).unwrap();
            assert_eq!(
                wire["saved_execution_status"],
                serde_json::to_value(parent.summary.status).unwrap()
            );
            assert!(wire["saved_result"].is_null());
            let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
            assert_eq!(
                reopened
                    .investigation_next_parent_history(&f.id, &parent.summary.conversation_id)
                    .unwrap()
                    .into_history(),
                history
            );
        }
    }
}

// AC-0327: a live legacy parent takes pending-basis validation and the second
// invocation-row decode. Preparing/sealing this synthetic step sends no request.
#[test]
fn parent_history_legacy_invocation_reopens_with_pending_step() {
    for (policy, progressive) in [
        (&INVESTIGATION_V1_POLICY, false),
        (&INVESTIGATION_V2_POLICY, false),
        (&INVESTIGATION_V2_POLICY, true),
    ] {
        let mut f = Fixture::with_family(InvestigationProviderMode::Local, policy, progressive);
        let (_, step) = f.step();
        let task = {
            let tx = f.store.conn.unchecked_transaction().unwrap();
            let task = load_execution_task(&tx, &f.id).unwrap();
            tx.commit().unwrap();
            task
        };
        assert_eq!(task.phase, Phase::Invocation);
        assert_eq!(task.pending.as_ref(), Some(&step));
        let conversation = &task.detail.summary.conversation_id;
        let history = f
            .store
            .investigation_next_parent_history(&f.id, conversation)
            .unwrap()
            .into_history();
        let wire: serde_json::Value = serde_json::from_str(&history[0]).unwrap();
        assert_eq!(wire["invocation_outcome_pending"], true);
        assert!(wire["saved_result"].is_null());
        let reopened = JobStore::open(f.directory.path().join("state.db")).unwrap();
        assert_eq!(
            reopened
                .investigation_next_parent_history(&f.id, conversation)
                .unwrap()
                .into_history(),
            history
        );
    }
}
