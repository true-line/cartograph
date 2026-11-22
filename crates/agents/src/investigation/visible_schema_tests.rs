//! AC-0766..AC-0769 (#1335): analyst @28 shows the exact per-call selected
//! response schema as prompt text. Production builders, validators, reserves,
//! firewall preparation and sealing only; no provider dispatch or source I/O.
use super::*;

const POLICY: &InvestigationPolicy = &INVESTIGATION_V2_POLICY;

fn shown(input: &InvestigationInput) -> Option<Value> {
    let prompt: Value = serde_json::from_str(&input.payload.prompt).unwrap();
    prompt["turn_progress"].get("response_schema").cloned()
}

fn chosen(role: SpecialistId, mode: CompletionCallMode, read: bool) -> InvestigationInput {
    let (mut f, raw, target, prototype) = source_fixture(role, "bounded source");
    let prior = f.ledger.clone();
    let mut evidence = Vec::new();
    if read {
        acquire_source(&mut f, &target, &prototype, &mut evidence, "bounded source");
    }
    let input = build_source(&f, &raw, &evidence, role, mode);
    source_choices(&f, input, &[], &prior)
}

fn with_prompt(input: &InvestigationInput, prompt: &Value) -> InvestigationInput {
    let mut changed = input.clone();
    changed.payload.prompt = serde_json::to_string(prompt).unwrap();
    changed
}

// AC-0766: Choice and FinishOnly @28 prompts carry the transport bytes
// exactly, before and after an admitted source citation; @27 never gains the
// field, and both roles derive their schema from the same selection.
#[test]
fn analyst28_prompt_shows_exact_selected_schema_in_both_modes_and_after_growth() {
    for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
        for read in [false, true] {
            let old = chosen(SpecialistId::DomainAnalystV27, mode, read);
            let new = chosen(SpecialistId::DomainAnalystV28, mode, read);
            assert!(shown(&old).is_none(), "@27 wire is unchanged");
            let transport = new.payload.response_schema.clone().unwrap();
            let visible = shown(&new).unwrap();
            assert_eq!(serde_json::to_string(&visible).unwrap(), transport);
            assert!(new.payload.prompt.contains(&transport));
            let selected: Value =
                serde_json::from_str(old.payload.response_schema.as_deref().unwrap()).unwrap();
            assert_eq!(selected, visible, "same authority as the @27 selection");
            if mode == CompletionCallMode::FinishOnly {
                let finish: Value = serde_json::from_str(response_schema::FINISH).unwrap();
                assert_eq!(visible, finish);
                assert!(!transport.contains("query_context"));
            } else {
                assert!(transport.contains("read_evidence") || transport.contains("enum"));
            }
            new.validate_acquisition_projection().unwrap();
        }
    }
}

// AC-0767: validation rejects a missing, altered or injected copy and a
// non-canonical transport; a valid @28 input seals in both modes.
#[test]
fn analyst28_visible_schema_is_validated_and_sealed() {
    for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
        let input = chosen(SpecialistId::DomainAnalystV28, mode, false);
        let prepared = prepare(&input);
        let disclosure = input
            .seal_disclosure_with_policy(&prepared, POLICY)
            .unwrap()
            .unwrap();
        assert_eq!(disclosure.admission.unwrap().mode, mode);
        let prompt: Value = serde_json::from_str(&input.payload.prompt).unwrap();

        let mut missing = prompt.clone();
        missing["turn_progress"]
            .as_object_mut()
            .unwrap()
            .remove("response_schema");
        let mut altered = prompt.clone();
        altered["turn_progress"]["response_schema"] = json!({"type": "object"});
        let mut transport = input.clone();
        // Semantically the finish schema, but not the exact shown bytes.
        transport.payload.response_schema = Some(response_schema::FINISH.into());
        for tampered in [
            with_prompt(&input, &missing),
            with_prompt(&input, &altered),
            transport,
        ] {
            assert!(tampered.validate_acquisition_projection().is_err());
            assert!(tampered.action("current:1".into()).is_err());
            assert!(
                tampered
                    .measure_v9_reserve_with_policy(
                        &profile(),
                        &CompletionLimits::default(),
                        "next:2",
                        POLICY,
                    )
                    .is_err()
            );
        }

        let old = chosen(SpecialistId::DomainAnalystV27, mode, false);
        let mut injected: Value = serde_json::from_str(&old.payload.prompt).unwrap();
        injected["turn_progress"]["response_schema"] =
            serde_json::from_str(old.payload.response_schema.as_deref().unwrap()).unwrap();
        let injected = with_prompt(&old, &injected);
        assert!(injected.validate_acquisition_projection().is_err());
        assert!(injected.action("current:1".into()).is_err());
    }
}

// AC-0768: reserves charge the visible copy, dominate the actual call, and
// the exact wire boundary refuses @28 where the shorter @27 still fits.
#[test]
fn analyst28_reserves_and_exact_wire_boundary_account_the_visible_copy() {
    let limits = CompletionLimits::default();
    let measure = |input: &InvestigationInput, limits: &CompletionLimits| {
        llm::bounded::measure_bounded_action(
            &profile(),
            &input.action("current:1".into()).unwrap(),
            limits,
        )
    };
    let old = chosen(
        SpecialistId::DomainAnalystV27,
        CompletionCallMode::Choice,
        false,
    );
    let new = chosen(
        SpecialistId::DomainAnalystV28,
        CompletionCallMode::Choice,
        false,
    );
    let old_reserve = old
        .measure_v9_reserve_with_policy(&profile(), &limits, "next:2", POLICY)
        .unwrap();
    let new_reserve = new
        .measure_v9_reserve_with_policy(&profile(), &limits, "next:2", POLICY)
        .unwrap();
    let visible = new.payload.response_schema.as_ref().unwrap().len() as u64;
    assert!(
        new_reserve.choice.raw_string_bytes >= old_reserve.choice.raw_string_bytes + visible,
        "choice reserve charges the visible schema"
    );
    let finish = chosen(
        SpecialistId::DomainAnalystV28,
        CompletionCallMode::FinishOnly,
        false,
    );
    let actual = measure(&finish, &limits).unwrap();
    assert!(new_reserve.finish_only.original_payload_bytes >= actual.original_payload_bytes);
    assert!(new_reserve.finish_only.raw_string_bytes >= actual.raw_string_bytes);

    // The unread-source certificate also covers both continuation modes.
    let (f, _, target, _) = source_fixture(SpecialistId::DomainAnalystV28, "bounded source");
    drop(f);
    let prepared = prepare(&new);
    new.certify_unread_source_v9_with_policy(
        &prepared,
        &target,
        &profile(),
        &limits,
        "next:2",
        POLICY,
    )
    .unwrap();

    let exact = measure(&new, &limits).unwrap().original_payload_bytes;
    let mut values = limits.values();
    values.input_bytes = exact;
    let at = CompletionLimits::new(values).unwrap();
    assert!(measure(&new, &at).is_ok());
    values.input_bytes = exact - 1;
    let below = CompletionLimits::new(values).unwrap();
    assert_eq!(
        measure(&new, &below).unwrap_err().code,
        llm::bounded::FailureCode::InputTooLarge
    );
    assert!(
        measure(&old, &below).is_ok(),
        "@27 is unchanged and smaller"
    );
}

// AC-0769: the @27 finish-patch projection keeps its selection and shows it.
#[test]
fn analyst28_finish_patch_shows_its_patch_schema() {
    use crate::investigation::finish_patch::PatchBasis;
    use staged_finish::{
        CombinedCorrection, CombinedCorrections, ProseCorrection, RefusalFeedback,
    };
    let mut transports = Vec::new();
    for role in [
        SpecialistId::DomainAnalystV27,
        SpecialistId::DomainAnalystV28,
    ] {
        let (mut f, raw, _, _) = source_fixture(role, "bounded source");
        f.metadata.refusal_count = 1;
        f.metadata.last_refusal_id = Some("c".repeat(64));
        f.metadata.feedback = Some(RefusalFeedback {
            schema_version: 6,
            unmet: vec![],
            citation_correction: None,
            prose_correction: Some(ProseCorrection::Combined(Box::new(CombinedCorrections {
                entries: vec![CombinedCorrection::RewriteSchemaPlaceholders],
                omitted: 0,
            }))),
        });
        let input = build_source(&f, &raw, &[], role, CompletionCallMode::Choice);
        let input = source_choices(&f, input, &[], &f.ledger);
        let patch = PatchBasis {
            finish_hash: "f".repeat(64),
            refusal_hash: "c".repeat(64),
            finding_count: 3,
            flagged_indices: vec![1],
            flagged_limitations: vec![],
            capacity_gaps: vec![],
        };
        let input = input.with_finish_patch(&patch).unwrap();
        input.validate_acquisition_projection().unwrap();
        let transport = input.payload.response_schema.clone().unwrap();
        assert!(transport.contains("finish_patch"));
        if role == SpecialistId::DomainAnalystV28 {
            let visible = shown(&input).unwrap();
            assert_eq!(serde_json::to_string(&visible).unwrap(), transport);
            let prepared = prepare(&input);
            input
                .seal_disclosure_with_policy(&prepared, POLICY)
                .unwrap()
                .unwrap();
        } else {
            assert!(shown(&input).is_none());
        }
        transports.push(transport);
    }
    assert_eq!(
        transports[0], transports[1],
        "patch schema bytes are unchanged"
    );
}
