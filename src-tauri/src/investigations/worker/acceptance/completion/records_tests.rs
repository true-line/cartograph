use super::*;
use serde_json::Value;

fn fixture(
    name: &str,
) -> (
    InvestigationDetail,
    Option<InvestigationResult>,
    Vec<InvestigationEvent>,
    RunBinding,
) {
    let raw = match name {
        "supported" => {
            include_str!("../../../../../../ui/src/fixtures/completion-v7-supported.json")
        }
        "insufficient" => {
            include_str!("../../../../../../ui/src/fixtures/completion-v7-insufficient.json")
        }
        _ => include_str!("../../../../../../ui/src/fixtures/completion-v7-terminal.json"),
    };
    let value: Value = serde_json::from_str(raw).unwrap();
    let detail: InvestigationDetail = serde_json::from_value(value["detail"].clone()).unwrap();
    let result: Option<InvestigationResult> =
        serde_json::from_value(value["result"].clone()).unwrap();
    let completion = detail.completion.as_ref().unwrap();
    let binding = RunBinding {
        run_id: "synthetic-v7-one".into(),
        protocol_hash: "a".repeat(64),
        source_commit: "b".repeat(40),
        source_tree: "c".repeat(40),
        context_id: detail.coverage_context.as_ref().unwrap().context_id.clone(),
        request_hash: completion.request_hash.clone(),
        obligations_hash: completion.obligations_hash.clone(),
    };
    // Classification unit seam: use real exported production detail/result DTOs,
    // with a synthetic complete event stream containing only existing event kinds.
    let mut events: Vec<_> = (1..=detail.summary.last_event_sequence)
        .map(|sequence| InvestigationEvent {
            investigation_id: detail.summary.investigation_id.clone(),
            sequence,
            revision: 1,
            kind: InvestigationEventKind::Created,
            step_id: None,
            tool: None,
            summary: "unit event".into(),
            created_at: "2026-10-20T00:00:00Z".into(),
        })
        .collect();
    for event in events.iter_mut().take(completion.refusal_count as usize) {
        event.kind = InvestigationEventKind::FinishRefused;
    }
    events.last_mut().unwrap().kind = if result.is_some() {
        InvestigationEventKind::ResultPersisted
    } else {
        InvestigationEventKind::Failed
    };
    (detail, result, events, binding)
}
fn fixture_role(detail: &InvestigationDetail) -> Role {
    if detail.summary.specialist_id == SpecialistId::EvidenceAuditorV7 {
        Role::Auditor
    } else {
        Role::Analyst
    }
}
fn checks(status: CheckStatus) -> Vec<MechanicalCheck> {
    vec![MechanicalCheck {
        name: "durable_binding".into(),
        status,
    }]
}
fn receipt() -> RoleReceipt {
    let (detail, result, events, binding) = fixture("supported");
    record_role(
        fixture_role(&detail),
        binding,
        &detail,
        result.as_ref(),
        &events,
        checks(CheckStatus::Passed),
    )
    .unwrap()
}

// AC-0267: predeclared supported categories remain distinct, and mechanical
// success alone never enables the semantic gate or changes the sealed receipt.
#[test]
fn completion_acceptance_classifies_supported_and_separates_semantic_gate() {
    for refusals in [0, 1] {
        let (mut detail, result, mut events, binding) = fixture("supported");
        detail.completion.as_mut().unwrap().refusal_count = refusals;
        if refusals == 0 {
            for event in &mut events {
                if event.kind == InvestigationEventKind::FinishRefused {
                    event.kind = InvestigationEventKind::Created;
                }
            }
        }
        let record = record_role(
            fixture_role(&detail),
            binding,
            &detail,
            result.as_ref(),
            &events,
            checks(CheckStatus::Passed),
        )
        .unwrap();
        assert_eq!(
            record.classification,
            if refusals == 0 {
                ControlClassification::SupportedFirstShot
            } else {
                ControlClassification::SupportedAfterOneRefusal
            }
        );
        assert_eq!(record.refusals_before_finish, Some(refusals));
        let before = json_bytes(&record).unwrap();
        assert!(
            !evaluate(&record, SemanticGate::Pending)
                .unwrap()
                .control_pass
        );
        assert!(
            !evaluate(&record, SemanticGate::Failed)
                .unwrap()
                .control_pass
        );
        assert!(
            evaluate(&record, SemanticGate::Passed)
                .unwrap()
                .control_pass
        );
        let unreviewable = evaluate(&record, SemanticGate::Unreviewable).unwrap();
        assert_eq!(
            unreviewable.classification,
            ControlClassification::Unreviewable
        );
        assert!(!unreviewable.control_pass);
        assert_eq!(json_bytes(&record).unwrap(), before);
    }
}

// AC-0267: honest insufficient evidence and capped obligations_unmet remain
// nonpassing control outcomes, including null refusal-before-finish for no result.
#[test]
fn completion_acceptance_classifies_nonpassing_terminal_outcomes() {
    for (name, expected) in [
        ("insufficient", ControlClassification::Insufficient),
        ("terminal", ControlClassification::ObligationsUnmet),
    ] {
        let (detail, result, events, binding) = fixture(name);
        let record = record_role(
            fixture_role(&detail),
            binding,
            &detail,
            result.as_ref(),
            &events,
            checks(CheckStatus::Passed),
        )
        .unwrap();
        assert_eq!(record.classification, expected);
        assert_eq!(
            record.refusals_before_finish,
            result.as_ref().map(|_| record.durable_refusal_count)
        );
        assert!(
            !evaluate(&record, SemanticGate::Passed)
                .unwrap()
                .control_pass
        );
    }
    for (status, expected) in [
        (InvestigationStatus::Failed, ControlClassification::Failed),
        (
            InvestigationStatus::Cancelled,
            ControlClassification::Cancelled,
        ),
        (
            InvestigationStatus::OutcomeUnknown,
            ControlClassification::Unknown,
        ),
        (
            InvestigationStatus::Interrupted,
            ControlClassification::Unknown,
        ),
    ] {
        let (mut detail, _, mut events, binding) = fixture("terminal");
        detail.summary.status = status;
        events.last_mut().unwrap().kind = match status {
            InvestigationStatus::Cancelled => InvestigationEventKind::Cancelled,
            InvestigationStatus::OutcomeUnknown => InvestigationEventKind::OutcomeUnknown,
            InvestigationStatus::Interrupted => InvestigationEventKind::Interrupted,
            _ => InvestigationEventKind::Failed,
        };
        detail.summary.completion_outcome = None;
        let record = record_role(
            fixture_role(&detail),
            binding,
            &detail,
            None,
            &events,
            checks(CheckStatus::Unknown),
        )
        .unwrap();
        assert_eq!(record.classification, expected);
        assert_eq!(
            evaluate(&record, SemanticGate::Passed).unwrap().mechanical,
            CheckStatus::Unknown
        );
        assert!(
            !evaluate(&record, SemanticGate::Passed)
                .unwrap()
                .control_pass
        );
    }
}

// AC-0267: exhaustion needs the exact admitted production Failed message and
// detail error together; high usage, similar prose, or only one field proves none.
#[test]
fn completion_acceptance_exhaustion_requires_exact_durable_reason() {
    for exact_detail in [false, true] {
        for exact_event in [false, true] {
            let (mut detail, _, mut events, binding) = fixture("terminal");
            detail.summary.completion_outcome = None;
            detail.error = Some(
                if exact_detail {
                    EXHAUSTION_REASON
                } else {
                    "A different failure."
                }
                .into(),
            );
            events.last_mut().unwrap().summary = if exact_event {
                EXHAUSTION_REASON
            } else {
                "A different failure."
            }
            .into();
            let record = record_role(
                fixture_role(&detail),
                binding,
                &detail,
                None,
                &events,
                checks(CheckStatus::Unknown),
            )
            .unwrap();
            assert_eq!(
                record.classification,
                if exact_detail && exact_event {
                    ControlClassification::Exhausted
                } else {
                    ControlClassification::Failed
                }
            );
        }
    }
}

// AC-0267: inconsistent counts, generation/result bindings and incomplete
// journals cannot be relabeled as a passing observation by caller assertions.
#[test]
fn completion_acceptance_rejects_inconsistent_durable_evidence() {
    for field in ["count", "events", "result", "binding", "role", "running"] {
        let (mut detail, mut result, mut events, mut binding) = fixture("supported");
        match field {
            "count" => detail.completion.as_mut().unwrap().refusal_count = 0,
            "events" => {
                events.pop();
            }
            "result" => result.as_mut().unwrap().schema_version = 4,
            "binding" => binding.request_hash = "f".repeat(64),
            "role" => detail.summary.specialist_id = SpecialistId::DomainAnalystV6,
            _ => detail.summary.status = InvestigationStatus::Running,
        }
        assert!(
            record_role(
                fixture_role(&detail),
                binding,
                &detail,
                result.as_ref(),
                &events,
                checks(CheckStatus::Passed)
            )
            .is_err(),
            "{field}"
        );
    }
    let mut record = receipt();
    record.mechanical_checks[0].status = CheckStatus::Unknown;
    assert_eq!(
        evaluate(&record, SemanticGate::Passed).unwrap().mechanical,
        CheckStatus::Unknown
    );
    assert!(
        !evaluate(&record, SemanticGate::Passed)
            .unwrap()
            .control_pass
    );
    record.mechanical_checks[0].status = CheckStatus::Failed;
    assert_eq!(
        evaluate(&record, SemanticGate::Passed).unwrap().mechanical,
        CheckStatus::Failed
    );
}

// AC-0267: receipt creation is exclusive, canonical and verified by exact byte
// hash/length; later semantic review leaves those original evidence bytes intact.
#[test]
fn completion_acceptance_receipt_exclusive_write_and_tamper_detection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("analyst.json");
    let record = receipt();
    let reference = write_new_receipt(&path, &record).unwrap();
    assert_eq!(read_verified_receipt(&path, &reference).unwrap(), record);
    let bytes = std::fs::read(&path).unwrap();
    assert!(write_new_receipt(&path, &record).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mut mutated = bytes.clone();
    mutated[5] ^= 1;
    std::fs::write(&path, &mutated).unwrap();
    assert!(read_verified_receipt(&path, &reference).is_err());
    std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
    assert!(read_verified_receipt(&path, &reference).is_err());
    assert!(
        write_new_receipt(
            &directory.path().join("missing").join("receipt.json"),
            &record
        )
        .is_err()
    );
}

// AC-0267: even a recomputed hash cannot admit extra fields, noncanonical bytes,
// oversized/truncated input, generation changes, or a second sample/retry policy.
#[test]
fn completion_acceptance_receipt_rejects_rehashed_shape_and_capacity_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("receipt.json");
    let record = receipt();
    for mutation in [
        "extra",
        "pretty",
        "generation",
        "retry",
        "samples",
        "oversize",
        "truncated",
    ] {
        let mut value = serde_json::to_value(&record).unwrap();
        match mutation {
            "extra" => value["unexpected"] = Value::Bool(true),
            "generation" => value["schema_version"] = 2.into(),
            "retry" => value["automatic_retry"] = true.into(),
            "samples" => value["samples_per_role"] = 2.into(),
            _ => {}
        }
        let bytes = match mutation {
            "pretty" => serde_json::to_vec_pretty(&value).unwrap(),
            "oversize" => vec![b' '; MAX_RECEIPT_BYTES + 1],
            "truncated" => b"{".to_vec(),
            _ => serde_json::to_vec(&value).unwrap(),
        };
        std::fs::write(&path, &bytes).unwrap();
        let reference = ReceiptRef {
            body_hash: core_prov::content_hash(&bytes),
            byte_len: bytes.len() as u64,
        };
        assert!(
            read_verified_receipt(&path, &reference).is_err(),
            "{mutation}"
        );
    }
    let mut invalid = record;
    invalid.mechanical_checks = vec![MechanicalCheck {
        name: "x".repeat(MAX_RECEIPT_BYTES),
        status: CheckStatus::Passed,
    }];
    let new_path = directory.path().join("invalid.json");
    assert!(write_new_receipt(&new_path, &invalid).is_err());
    assert!(!new_path.exists());
}

// AC-0267: a terminal preparation failure without a first invocation can retain
// honest no-context evidence; absent context never authorizes supported success.
#[test]
fn completion_acceptance_early_failure_without_context_cannot_pass_supported() {
    let (mut detail, _, mut events, binding) = fixture("terminal");
    detail.coverage_context = None;
    detail.usage.model_invocations = 0;
    detail.summary.completion_outcome = None;
    let completion = detail.completion.as_mut().unwrap();
    completion.refusal_count = 0;
    if let CompletionAssessmentBasis::TerminalObservation {
        last_refusal_id,
        last_refusal_basis,
        ..
    } = &mut completion.assessment_basis
    {
        *last_refusal_id = None;
        *last_refusal_basis = None;
    }
    for event in &mut events {
        if event.kind == InvestigationEventKind::FinishRefused {
            event.kind = InvestigationEventKind::Created;
        }
    }
    let record = record_role(
        Role::Analyst,
        binding,
        &detail,
        None,
        &events,
        checks(CheckStatus::Unknown),
    )
    .unwrap();
    assert_eq!(record.classification, ControlClassification::Failed);
    assert!(
        !evaluate(&record, SemanticGate::Passed)
            .unwrap()
            .control_pass
    );
    let (mut detail, result, events, binding) = fixture("supported");
    detail.coverage_context = None;
    assert!(
        record_role(
            fixture_role(&detail),
            binding,
            &detail,
            result.as_ref(),
            &events,
            checks(CheckStatus::Passed)
        )
        .is_err()
    );
}

// AC-0430/0267: two valid limit messages cannot authenticate each other.
#[test]
fn completion_acceptance_exhaustion_requires_matching_closed_limit_messages() {
    use crate::jobs::investigations::InvestigationStopReason as Stop;
    use llm::bounded::{BoundedCallError, FailureCode, InvocationOutcome};
    let host = Stop::HostLimit.test_message();
    let query_host = Stop::HostLimitAt {
        phase: crate::jobs::investigations::PreparationPhase::Query,
        limit: crate::jobs::investigations::HostLimitKind::CumulativeQueryAndInventoryBytes,
    }
    .test_message();
    let inventory_host = Stop::HostLimitAt {
        phase: crate::jobs::investigations::PreparationPhase::Inventory,
        limit: crate::jobs::investigations::HostLimitKind::CumulativeQueryAndInventoryBytes,
    }
    .test_message();
    let store = Stop::StoreCapacity.test_message();
    let unknown = Stop::BoundedProvider(BoundedCallError {
        code: FailureCode::ResponseTooLarge,
        outcome: InvocationOutcome::Unknown,
    })
    .test_message();
    let unrecognized_host =
        "Investigation limit reached: host_limit; phase=metadata_query; limit=some_other_budget.";
    let unrecognized_phase = "Investigation limit reached: host_limit; phase=untrusted; limit=cumulative_query_and_inventory_bytes.";
    for (detail_reason, event_reason, classification) in [
        (host, host, ControlClassification::Exhausted),
        (query_host, query_host, ControlClassification::Exhausted),
        (
            inventory_host,
            inventory_host,
            ControlClassification::Exhausted,
        ),
        (query_host, host, ControlClassification::Failed),
        (store, store, ControlClassification::Exhausted),
        (host, store, ControlClassification::Failed),
        (
            unrecognized_host,
            unrecognized_host,
            ControlClassification::Failed,
        ),
        (
            unrecognized_phase,
            unrecognized_phase,
            ControlClassification::Failed,
        ),
        (unknown, unknown, ControlClassification::Failed),
    ] {
        let (mut detail, _, mut events, binding) = fixture("terminal");
        detail.summary.completion_outcome = None;
        detail.error = Some(detail_reason.into());
        events.last_mut().unwrap().summary = event_reason.into();
        let receipt = record_role(
            fixture_role(&detail),
            binding,
            &detail,
            None,
            &events,
            checks(CheckStatus::Unknown),
        )
        .unwrap();
        assert_eq!(receipt.classification, classification);
        if detail_reason == unknown {
            let mut forged = receipt;
            forged.classification = ControlClassification::Exhausted;
            assert!(forged.validate().is_err());
        }
    }
    for code in [
        FailureCode::DeadlineExceeded,
        FailureCode::InputTooLarge,
        FailureCode::RequestTooLarge,
        FailureCode::ResponseTooLarge,
        FailureCode::OutputTooLarge,
    ] {
        for outcome in [
            InvocationOutcome::NotDispatched,
            InvocationOutcome::ResponseReceived,
            InvocationOutcome::Unknown,
        ] {
            assert_eq!(
                exhaustion_reason(
                    Stop::BoundedProvider(BoundedCallError { code, outcome }).test_message()
                ),
                outcome != InvocationOutcome::Unknown
            );
        }
    }
}
