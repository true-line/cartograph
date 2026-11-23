//! Version selection through production correction and sidecar entry points.
use super::*;
use crate::investigation::diagnostic_sidecar::{Detail, Retention};

fn input(role: SpecialistId) -> InvestigationInput {
    let source = "{A: [], B: ['T']}";
    let (mut fixture, raw, target, prototype) = source_fixture(role, source);
    let mut evidence = Vec::new();
    acquire_source(&mut fixture, &target, &prototype, &mut evidence, source);
    build_source(
        &fixture,
        &raw,
        &evidence,
        role,
        CompletionCallMode::FinishOnly,
    )
}

fn finish(statement: &str) -> staged_finish::StagedFinish {
    staged_finish::StagedFinish {
        schema_version: 1,
        outcome: staged_finish::CompletionOutcome::Supported,
        narrative: None,
        action: InvestigationAction::Finish {
            findings: vec![ProposedInvestigationFinding {
                claim_kind: InvestigationClaimKind::ImplementedBehavior,
                title: "Transition table".into(),
                statement: statement.into(),
                citation_ids: vec!["evidence-1".into()],
                limitations: vec!["Only selected evidence was assessed.".into()],
            }],
            knowledge_completeness: KnowledgeCompleteness::Partial,
            limitations: vec!["Only selected evidence was assessed.".into()],
        },
    }
}

// AC-0287 / #1729: role-selected verdict and diagnostic visitors agree; @25
// and @26 retain identical serialized sidecars for the same candidate.
#[test]
fn analyst27_input_versions_bounded_verdicts_and_sidecars_together() {
    for statement in [
        "A is terminal.",
        "B is terminal.",
        "B is the sole state that lists T.",
        "A is terminal, and executes payments.",
    ] {
        let proposed = finish(statement);
        let mut historical = None;
        for role in [
            SpecialistId::DomainAnalystV25,
            SpecialistId::DomainAnalystV26,
            SpecialistId::DomainAnalystV27,
        ] {
            let input = input(role);
            let before = proposed.fingerprint().unwrap();
            let first = input.claim_correction(&proposed).unwrap();
            let mut corrections = Vec::new();
            input
                .visit_claim_corrections(&proposed, |c| corrections.push(c))
                .unwrap();
            assert_eq!(first.as_ref(), corrections.first());
            let sidecar = input
                .refusal_diagnostic_sidecar(&proposed, content_hash(b"refusal"))
                .unwrap();
            let bytes = sidecar.bytes().unwrap();
            let Retention::Recorded {
                entries,
                omitted_entries,
            } = sidecar.retention
            else {
                panic!("small fixture fits");
            };
            assert_eq!(omitted_entries, 0);
            assert_eq!(corrections.len(), entries.len(), "{role:?}: {statement}");
            if role == SpecialistId::DomainAnalystV27 {
                let rejected =
                    statement.starts_with("B is terminal") || statement.contains("payments");
                assert_eq!(corrections.len(), usize::from(rejected), "{statement}");
                for entry in entries {
                    assert_eq!(entry.citation_index, Some(0));
                    assert!(matches!(entry.detail, Detail::Enumeration { .. }));
                }
            } else if let Some(old) = &historical {
                assert_eq!(&bytes, old, "historical sidecar: {statement}");
            } else {
                historical = Some(bytes);
            }
            assert_eq!(proposed.fingerprint().unwrap(), before);
        }
    }
    let mut uncited = finish("B is terminal.");
    if let InvestigationAction::Finish { findings, .. } = &mut uncited.action {
        findings[0].citation_ids = vec!["fact-1".into()];
    }
    assert!(
        input(SpecialistId::DomainAnalystV27)
            .claim_correction(&uncited)
            .unwrap()
            .is_none()
    );
}

// AC-0287 / #1729: the new role must retain pre-call reserve certification.
#[test]
fn analyst27_certifies_frozen_source_reserve() {
    let role = SpecialistId::DomainAnalystV27;
    let (fixture, raw, target, _) = source_fixture(role, "bounded source");
    let input = source_choices(
        &fixture,
        build_source(&fixture, &raw, &[], role, CompletionCallMode::Choice),
        &[],
        &fixture.ledger,
    );
    let prepared = prepare(&input);
    let proof = input
        .certify_unread_source_v9_with_policy(
            &prepared,
            &target,
            &profile(),
            &CompletionLimits::default(),
            "next:2",
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
    proof
        .verify_with_policy(
            &input,
            &prepared,
            &target,
            &profile(),
            &CompletionLimits::default(),
            "next:2",
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
}

#[test]
fn analyst27_patch_projection_seals_finish_only_allowance_before_acquisition_boundary() {
    use crate::investigation::finish_patch::PatchBasis;
    use staged_finish::{
        CombinedCorrection, CombinedCorrections, ProseCorrection, RefusalFeedback,
    };
    for opted in [false, true] {
        let policy = if opted {
            INVESTIGATION_V2_POLICY
                .with_local_finish_generation()
                .unwrap()
        } else {
            INVESTIGATION_V2_POLICY.clone()
        };
        let role = SpecialistId::DomainAnalystV27;
        let (mut f, raw, _, _) = source_fixture(role, "bounded source");
        f.metadata.refusal_count = 1;
        f.metadata.last_refusal_id = Some("c".repeat(64));
        f.metadata.feedback = Some(RefusalFeedback {
            citation_drop: None,
            schema_version: 6,
            unmet: vec![],
            citation_correction: None,
            prose_correction: Some(ProseCorrection::Combined(Box::new(CombinedCorrections {
                entries: vec![CombinedCorrection::RewriteSchemaPlaceholders],
                omitted: 0,
            }))),
        });
        let input = InvestigationInput::prepare_v9_with_policy(
            &f.request.question,
            role,
            f.ledger.clone(),
            &[&raw],
            &[],
            &[],
            &usage(CompletionCallMode::Choice),
            &f.basis,
            &f.request,
            &f.metadata,
            &[],
            CompletionCallMode::Choice,
            0,
            None,
            &policy,
        )
        .unwrap();
        let input = source_choices(&f, input, &[], &f.ledger);
        assert_eq!(input.invocation_output_tokens(&policy).unwrap(), 2048);
        // Shape projection does not authenticate this test basis. The host
        // separately checks immutable refusal evidence and the live holder.
        let patch = PatchBasis {
            finish_hash: "f".repeat(64),
            refusal_hash: "c".repeat(64),
            finding_count: 3,
            flagged_indices: vec![1],
            flagged_limitations: vec![],
            capacity_gaps: vec![],
        };
        let input = input.with_finish_patch(&patch).unwrap();
        let expected = if opted { 4096 } else { 2048 };
        assert_eq!(input.invocation_output_tokens(&policy).unwrap(), expected);
        let mut values = CompletionLimits::default().values();
        values.max_output_tokens = expected;
        let prepared = llm::EgressFirewall::new(llm::EgressPolicy::default())
            .prepare_bounded(
                &DescriptorOnly,
                &input.action("current:1".into()).unwrap(),
                &CompletionLimits::for_investigation(values).unwrap(),
            )
            .unwrap();
        let disclosure = input
            .seal_disclosure_with_policy(&prepared, &policy)
            .unwrap()
            .unwrap();
        let state = disclosure.admission.unwrap();
        assert_eq!(state.mode, CompletionCallMode::FinishOnly);
        assert_eq!(
            state.acquisition.as_ref().unwrap().finish_patch.as_ref(),
            Some(&patch)
        );
        assert_eq!(
            state.acquisition.as_ref().unwrap().finish_policy_complete,
            opted
        );
        let schema = input.payload.response_schema.as_ref().unwrap();
        assert!(schema.contains("finish_patch"));
        assert!(!schema.contains("query_context"));
        let mut tampered = input.clone();
        tampered.finish_patch.as_mut().unwrap().refusal_hash = "d".repeat(64);
        assert!(tampered.validate_acquisition_projection().is_err());
    }
}

#[test]
fn analyst27_merge_does_not_exempt_unflagged_findings_from_guards() {
    use crate::investigation::finish_patch::{PatchBasis, VolatileFinish};
    let input = input(SpecialistId::DomainAnalystV27);
    let mut first = finish("B is terminal.");
    let InvestigationAction::Finish { findings, .. } = &mut first.action else {
        panic!()
    };
    let InvestigationAction::Finish {
        findings: mut other,
        ..
    } = finish("A is terminal.").action
    else {
        panic!()
    };
    other[0].title = "Other transition".into();
    findings.extend(other);
    // Deliberately mark only index 1 to exercise re-admission of index 0.
    // This isolated projection test grants no saved-refusal authority.
    let basis = PatchBasis {
        finish_hash: first.fingerprint().unwrap(),
        refusal_hash: content_hash(b"refusal"),
        finding_count: 2,
        flagged_indices: vec![1],
        flagged_limitations: vec![],
        capacity_gaps: vec![],
    };
    let merged = VolatileFinish::capture(first, basis.clone())
        .unwrap()
        .merge(
            r#"{"type":"finish_patch","patches":[{"finding_index":1,"withdraw":true}]}"#,
            &basis,
        )
        .unwrap();
    assert!(merged.flagged_merged_indices.is_empty());
    assert!(input.claim_correction(&merged.finish).unwrap().is_some());
    let plan = input
        .finding_withholding_plan_with_policy(&merged.finish, None, &INVESTIGATION_V2_POLICY)
        .unwrap();
    // No survivors: a retained unflagged failure cannot become an empty success.
    assert!(plan.is_none());
}
