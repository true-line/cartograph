//! Result admission keeps reference history distinct from completion authority.
use super::*;
use context_hub::{ContextSnapshot, coverage::CoverageQualification};
use serde_json::json;

fn fixture(
    progressive: bool,
) -> (
    InvestigationInputLedger,
    InvestigationDisclosure,
    InvestigationCompletionResult,
) {
    let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
    let proof = CoverageQualification::new(&[], &[])
        .unwrap()
        .durable_proof(snapshot.id())
        .unwrap();
    let ledger = InvestigationInputLedger {
        schema_version: 2,
        coverage: Some(proof),
        coverage_reads: vec![],
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
    let request_hash = "a".repeat(64);
    let disclosure = InvestigationDisclosure {
        schema_version: if progressive { 5 } else { 4 },
        query_history: progressive.then(|| {
            DisclosedQueryHistory::from_ledger(&request_hash, &ledger, &INVESTIGATION_V2_POLICY)
                .unwrap()
        }),
        coverage: Some(
            DisclosedInvestigationCoverage::from_proof(ledger.coverage.as_ref().unwrap()).unwrap(),
        ),
        coverage_details: vec![],
        completion_contract_hash: Some("b".repeat(64)),
        completion_metadata_hash: Some("c".repeat(64)),
        policy: if progressive {
            QUERY_HISTORY_DISCLOSURE_POLICY
        } else {
            COMPACT_DISCLOSURE_POLICY
        }
        .into(),
        inspection_requirements_hash: None,
        input_ledger_hash: ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
        graph_snapshot_id: ledger.graph_snapshot_id.clone(),
        scope_snapshot_id: ledger.scope_snapshot_id.clone(),
        payload_hash: "d".repeat(64),
        facts: vec![],
        queries: vec![],
        sources: vec![],
        history: None,
    };
    // This lower-level result test checks shape and finishing disclosure binding.
    // Production completion validation separately replays durable attempts.
    let completion = InvestigationCompletionResult {
        schema_version: 1,
        outcome: staged_finish::CompletionOutcome::InsufficientEvidence,
        request_hash,
        obligations_hash: "e".repeat(64),
        finishing_attempts_hash: "f".repeat(64),
        assessment: vec![staged_finish::ObligationObservation {
            obligation_id: "1".repeat(64),
            kind: "inventory".into(),
            attempted: true,
            acquired: false,
            exactly_supplied: false,
            unmet: None,
            last_attempt_outcome: Some(CompletionAttemptOutcome::Unavailable),
        }],
    };
    disclosure
        .validate_with_policy(&ledger, &INVESTIGATION_V2_POLICY)
        .unwrap();
    (ledger, disclosure, completion)
}

fn result(
    ledger: &InvestigationInputLedger,
    disclosure: &InvestigationDisclosure,
    completion: InvestigationCompletionResult,
) -> InvestigationResult {
    InvestigationResult::admit_with_completion_and_policy(
        "task:history",
        ledger,
        "step:finish",
        disclosure,
        None,
        completion,
        &InvestigationAction::Finish {
            findings: vec![],
            knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
            limitations: vec!["No acquired inventory supports a conclusion.".into()],
        },
        std::iter::empty::<&str>(),
        None,
        "2026-10-21T00:00:00Z".into(),
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap()
}

#[test]
fn query_history_result_version_and_domain_are_exact() {
    // AC-0286: @9 disclosure5 has a distinct result6; @8 remains result5.
    for progressive in [false, true] {
        let (ledger, disclosure, completion) = fixture(progressive);
        let admitted = result(&ledger, &disclosure, completion);
        let expected = if progressive { 6 } else { 5 };
        assert_eq!(admitted.schema_version, expected);
        assert!(
            admitted
                .result_id
                .starts_with(&format!("investigation-result-v{expected}:"))
        );
        admitted
            .validate_with_disclosure_and_policy(
                &ledger,
                "step:finish",
                &disclosure,
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
        let mut relabeled = admitted.clone();
        relabeled.schema_version = if progressive { 5 } else { 6 };
        relabeled.result_id = relabeled.content_id().unwrap();
        assert!(
            relabeled
                .validate_with_disclosure_and_policy(
                    &ledger,
                    "step:finish",
                    &disclosure,
                    &INVESTIGATION_V2_POLICY
                )
                .is_err()
        );
        let mut missing = admitted;
        missing.completion = None;
        missing.result_id = missing.content_id().unwrap();
        assert!(
            missing
                .validate_with_disclosure_and_policy(
                    &ledger,
                    "step:finish",
                    &disclosure,
                    &INVESTIGATION_V2_POLICY
                )
                .is_err()
        );
    }
}

#[test]
fn query_history_result_rejects_self_consistent_foreign_registry_and_missing_completion() {
    // AC-0286: even an empty, internally valid reference registry is anchored to
    // the finishing completion request; rehashing mutable envelopes cannot rebind it.
    let (ledger, disclosure, completion) = fixture(true);
    let mut admitted = result(&ledger, &disclosure, completion.clone());
    let mut foreign = disclosure;
    foreign.query_history = Some(
        DisclosedQueryHistory::from_ledger(&"2".repeat(64), &ledger, &INVESTIGATION_V2_POLICY)
            .unwrap(),
    );
    foreign
        .validate_with_policy(&ledger, &INVESTIGATION_V2_POLICY)
        .unwrap();
    admitted.disclosure_hash = Some(
        foreign
            .fingerprint_with_policy(&ledger, &INVESTIGATION_V2_POLICY)
            .unwrap(),
    );
    admitted.result_id = admitted.content_id().unwrap();
    assert!(
        admitted
            .validate_with_disclosure_and_policy(
                &ledger,
                "step:finish",
                &foreign,
                &INVESTIGATION_V2_POLICY
            )
            .is_err()
    );
    assert!(
        InvestigationResult::admit_with_completion_and_policy(
            "task:history",
            &ledger,
            "step:finish",
            &foreign,
            None,
            completion,
            &InvestigationAction::Finish {
                findings: vec![],
                knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
                limitations: vec!["Only bounded inventory was assessed.".into()]
            },
            std::iter::empty::<&str>(),
            None,
            "2026-10-21T00:00:00Z".into(),
            &INVESTIGATION_V2_POLICY,
        )
        .is_err()
    );
    assert!(
        foreign
            .eligible_citations_with_policy(&ledger, &INVESTIGATION_V2_POLICY)
            .unwrap()
            .is_empty()
    );
    assert!(
        InvestigationResult::admit_with_disclosure_and_policy(
            "task:history",
            &ledger,
            "step:finish",
            &foreign,
            &InvestigationAction::Finish {
                findings: vec![],
                knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
                limitations: vec!["Only bounded inventory was assessed.".into()]
            },
            std::iter::empty::<&str>(),
            None,
            "2026-10-21T00:00:00Z".into(),
            &INVESTIGATION_V2_POLICY,
        )
        .is_err()
    );
    assert!(json!(foreign).get("query_history").is_some());
}
