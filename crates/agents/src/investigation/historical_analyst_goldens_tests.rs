//! AC-0287: canonical byte hashes generated independently at base commit
//! b4bb1248c78c3bbb4e9a6aba8027e462c09fe7cc, before analyst24 changes.
//! Includes absent historical witnesses, not invented @20/@21 witnesses.
use super::*;

#[test]
fn historical_analyst20_through23_byte_goldens() {
    let policy = &INVESTIGATION_V2_POLICY;
    let mut rows = Vec::new();
    for (role, expected_schema) in [
        (SpecialistId::DomainAnalystV20, 3),
        (SpecialistId::DomainAnalystV21, 3),
        (SpecialistId::DomainAnalystV22, 4),
        (SpecialistId::DomainAnalystV23, 4),
    ] {
        let (f, raw, _, _) = source_fixture(role, "bounded source");
        let input = source_choices(
            &f,
            build_source(&f, &raw, &[], role, CompletionCallMode::Choice),
            &[],
            &f.ledger,
        );
        let disclosure = input
            .seal_disclosure_with_policy(&prepare(&input), policy)
            .unwrap()
            .unwrap();
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: f.metadata.request_hash.clone(),
            obligations_hash: f.metadata.obligations_hash.clone(),
            obligations: f.request.obligations.clone(),
        };
        let bound = input
            .acquisition_choices
            .as_ref()
            .unwrap()
            .bind_completion_disclosure(
                "source-preview-task",
                "current:1",
                &contract,
                &f.metadata,
                input.ledger(),
                &disclosure,
                None,
                policy,
            )
            .unwrap();
        let replay = input
            .ledger()
            .coverage
            .as_ref()
            .unwrap()
            .qualification
            .coverage_id
            .clone();
        let finish = staged_finish::StagedFinish {
            schema_version: 1,
            outcome: staged_finish::CompletionOutcome::InsufficientEvidence,
            narrative: None,
            action: InvestigationAction::Finish {
                findings: vec![],
                knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
                limitations: vec![replay.clone(), replay, "not_available".into()],
            },
        };
        let decision = staged_finish::admit_finish_with_policy_using_consistency(
            &f.request,
            "source-preview-task",
            "current:1",
            &input,
            &disclosure,
            &finish,
            &[],
            None,
            "2026-11-06T00:00:00Z".into(),
            policy,
            Some(&bound),
        )
        .unwrap();
        let staged_finish::FinishDecision::Refused {
            feedback,
            assessment,
        } = decision
        else {
            panic!()
        };
        assert_eq!(feedback.schema_version, expected_schema);
        let mut record = staged_refusal::RefusalRecord {
            schema_version: 1,
            refusal_id: String::new(),
            task_id: "source-preview-task".into(),
            request_hash: contract.request_hash.clone(),
            obligations_hash: contract.obligations_hash.clone(),
            step_id: "current:1".into(),
            input_ledger_hash: input.ledger().fingerprint_with_policy(policy).unwrap(),
            disclosure_hash: bound.fingerprint().unwrap(),
            finish_hash: finish.fingerprint().unwrap(),
            outcome: finish.outcome,
            response_hash: content_hash(b"historical-golden-response"),
            previous_refusal: None,
            refusal_count: 1,
            assessment,
            feedback,
        };
        record.refusal_id = record.content_id().unwrap();
        let action = input.action("current:1".into()).unwrap();
        rows.push(serde_json::json!({
            "role":role, "prompt":content_hash(action.payload.system.as_bytes()),
            "schema":content_hash(action.payload.response_schema.as_ref().unwrap().as_bytes()),
            "feedback":content_hash(&record.feedback.bytes_with_policy(policy).unwrap()),
            "record":content_hash(&serde_json::to_vec(&record).unwrap()),
            "witness":if role.uses_finish_feedback_witness() { Some(record.feedback_witness_hash().unwrap()) } else { None },
        }));
    }
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("data/analyst20-23-byte-goldens.json")).unwrap();
    assert_eq!(serde_json::to_value(rows).unwrap(), expected);
}
