use super::*;
use agents::investigation::{
    InvestigationProviderMode, InvestigationScope,
    obligations::{InventoryTarget, ObligationEnvelope},
};

fn request() -> StagedObligationRequest {
    let context = format!("context-v2:{}", "b".repeat(64));
    let mut item = Obligation::Inventory {
        obligation_id: String::new(),
        target: InventoryTarget {
            context_id: context.clone(),
            repo: "example/app".into(),
            inventory_hash: "a".repeat(64),
        },
    };
    let id = item.content_id().unwrap();
    if let Obligation::Inventory { obligation_id, .. } = &mut item {
        *obligation_id = id;
    }
    StagedObligationRequest {
        schema_version: 7,
        request_nonce: "admission-records".into(),
        specialist_id: "domain-analyst@10".into(),
        obligations: ObligationEnvelope {
            schema_version: 2,
            contract: "investigation-completion-obligations@2".into(),
            obligations: vec![item],
        },
        inspection_requirements: None,
        question: "What evidence is established?".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v2".into(),
        expected_graph_revision: None,
        expected_context_id: context,
        conversation_id: None,
        parent_id: None,
    }
}
#[test]
fn frozen_denominator_survives_refusal_cancellation_and_finish() {
    // AC-0191: execution dispositions cannot narrow preparation obligations.
    let request = request();
    let ledger = FrozenLedger::freeze(&request).unwrap();
    ledger.validate_request(&request).unwrap();
    assert_eq!(ledger.count, 13);
    assert_eq!(ledger, FrozenLedger::freeze(&request).unwrap());
    let mut complete = StageReceipt::unexecuted(&ledger, Stage::Analyst);
    let LedgerEntry::Request { obligation } = &ledger.entries[0] else {
        panic!()
    };
    complete.observations[0] = Observation::Discharged {
        evidence: DischargeEvidence::WorkerSavedRecord {
            record_hash: "a".repeat(64),
            obligation_id: obligation.key().1.into(),
        },
    };
    complete.validate_against(&ledger).unwrap();
    for disposition in [
        Observation::Refused,
        Observation::Cancelled,
        Observation::Insufficient,
        Observation::Unexecuted,
    ] {
        let mut refused = complete.clone();
        refused.observations[0] = disposition;
        refused.validate_against(&ledger).unwrap();
        assert_eq!(refused.ledger.digest, complete.ledger.digest);
        assert_eq!(refused.ledger.count, complete.ledger.count);
        assert!(refused.discharged_count() < complete.discharged_count());
    }
    assert_eq!(complete.discharged_count(), 1); // Native/semantic work stays unexecuted.
}
#[test]
fn worker_coverage_cannot_discharge_procedure_or_drop_denominator() {
    // AC-0191: structural evidence boundary; refs alone are not acceptance.
    let ledger = FrozenLedger::freeze(&request()).unwrap();
    let mut receipt = StageReceipt::unexecuted(&ledger, Stage::Analyst);
    receipt.observations[1] = Observation::Discharged {
        evidence: DischargeEvidence::WorkerSavedRecord {
            record_hash: "a".repeat(64),
            obligation_id: "H4".into(),
        },
    };
    assert!(receipt.validate_against(&ledger).is_err());
    receipt.observations[1] = Observation::Discharged {
        evidence: DischargeEvidence::OperatorProcedure {
            artifact_hash: "a".repeat(64),
            procedure: Procedure::H4NativeProcessRestart,
        },
    };
    assert!(receipt.validate_against(&ledger).is_err());
    receipt.observations[1] = Observation::Unexecuted;
    receipt.observations.pop();
    assert!(receipt.validate_against(&ledger).is_err());
    receipt.ledger.entries.pop();
    receipt.ledger.count -= 1;
    assert!(receipt.validate_against(&ledger).is_err());
}
#[test]
fn family_and_request_substitution_fail_closed() {
    // AC-0191: public production validation and exact saved identity are both required.
    let original = request();
    let ledger = FrozenLedger::freeze(&original).unwrap();
    for (schema, role, policy) in [
        (4, "domain-analyst@7", "investigation-v1"),
        (5, "domain-analyst@8", "investigation-v2"),
        (6, "domain-analyst@9", "investigation-v2"),
        (7, "domain-analyst@9", "investigation-v2"),
        (7, "unknown@10", "investigation-v2"),
    ] {
        let mut mixed = original.clone();
        mixed.schema_version = schema;
        mixed.specialist_id = role.into();
        mixed.limit_profile = policy.into();
        assert!(FrozenLedger::freeze(&mixed).is_err());
    }
    let mut changed = original.clone();
    changed.question.push('?');
    assert!(ledger.validate_request(&changed).is_err());
    let mut receipt = StageReceipt::unexecuted(&ledger, Stage::Preparation);
    receipt.ledger.family.prompt_schema = 8;
    assert!(receipt.validate_against(&ledger).is_err());
    let old = serde_json::json!({"schema_version":1,"role":"analyst","samples_per_role":1});
    assert!(serde_json::from_value::<StageReceipt>(old).is_err());
    let mut raw = serde_json::to_value(&receipt).unwrap();
    raw["ledger"]["family"]["unregistered"] = true.into();
    assert!(serde_json::from_value::<StageReceipt>(raw).is_err());
}

#[test]
fn every_family_dimension_is_bound_and_empty_discharge_evidence_rejected() {
    // AC-0191: no individually compatible field permits a mixed-family receipt.
    let ledger = FrozenLedger::freeze(&request()).unwrap();
    let receipt = StageReceipt::unexecuted(&ledger, Stage::NativeOperator);
    for field in [
        "request_schema",
        "prompt_schema",
        "disclosure_schema",
        "result_schema",
        "task_schema",
        "summary_schema",
        "intent_schema",
    ] {
        let mut raw = serde_json::to_value(&receipt).unwrap();
        raw["ledger"]["family"][field] = 99.into();
        let mixed: StageReceipt = serde_json::from_value(raw).unwrap();
        assert!(mixed.validate_against(&ledger).is_err(), "{field}");
    }
    let mut malformed = receipt.clone();
    let LedgerEntry::Request { obligation } = &ledger.entries[0] else {
        panic!()
    };
    malformed.observations[0] = Observation::Discharged {
        evidence: DischargeEvidence::WorkerSavedRecord {
            record_hash: String::new(),
            obligation_id: obligation.key().1.into(),
        },
    };
    assert!(malformed.validate_against(&ledger).is_err());
    malformed.observations[0] = Observation::Discharged {
        evidence: DischargeEvidence::WorkerSavedRecord {
            record_hash: "a".repeat(64),
            obligation_id: "unrelated-obligation".into(),
        },
    };
    assert!(malformed.validate_against(&ledger).is_err());
}

// AC-0191: frozen denominator survives an actual immutable artifact round trip.
#[test]
fn frozen_ledger_reopens_with_all_cross_ingress_obligations() {
    use super::super::artifacts;
    let request = request();
    let ledger = FrozenLedger::freeze(&request).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preparation-ledger.json");
    let reference = artifacts::write_new(&path, &ledger).unwrap();
    let bytes = artifacts::read_verified(&path, &reference).unwrap();
    let reopened: FrozenLedger = serde_json::from_slice(&bytes).unwrap();
    reopened.validate_request(&request).unwrap();
    let receipt = StageReceipt::unexecuted(&reopened, Stage::Analyst);
    for procedure in [
        Procedure::H4SharedCoordinator,
        Procedure::H5CapabilitiesEnvironmentEgress,
        Procedure::H5CancellationDisconnectUnknown,
        Procedure::H5DelegationAncestrySharedLimits,
        Procedure::H5ExternalMcpAcp,
    ] {
        let position = reopened
            .entries
            .iter()
            .position(|entry| *entry == LedgerEntry::Procedure { procedure })
            .unwrap();
        assert_eq!(receipt.observations[position], Observation::Unexecuted);
    }
    assert_eq!(receipt.discharged_count(), 0);
    let mut narrowed = reopened;
    narrowed.entries.pop();
    narrowed.count -= 1;
    assert!(narrowed.validate_request(&request).is_err());
}

// AC-0191: stage names and receipt schemas cannot be invented or rebound.
#[test]
fn admission_receipt_rejects_unregistered_stage_and_schema() {
    let ledger = FrozenLedger::freeze(&request()).unwrap();
    let receipt = StageReceipt::unexecuted(&ledger, Stage::Preparation);
    let mut raw = serde_json::to_value(&receipt).unwrap();
    raw["stage"] = "pretend_acceptance".into();
    assert!(serde_json::from_value::<StageReceipt>(raw).is_err());
    let mut foreign = receipt;
    foreign.schema = "cartograph-v7-control@1".into();
    assert!(foreign.validate_against(&ledger).is_err());
}
