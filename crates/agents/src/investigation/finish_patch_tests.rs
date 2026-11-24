use super::*;
use core_prov::content_hash;
use serde_json::json;

fn finding(title: &str) -> ProposedInvestigationFinding {
    ProposedInvestigationFinding {
        claim_kind: InvestigationClaimKind::ImplementedBehavior,
        title: title.into(),
        statement: "The selected transition is present.".into(),
        citation_ids: vec!["fact-1".into()],
        limitations: vec!["Other behavior was not assessed.".into()],
    }
}
fn finish() -> StagedFinish {
    StagedFinish {
        schema_version: 1,
        outcome: CompletionOutcome::Supported,
        narrative: None,
        action: InvestigationAction::Finish {
            findings: vec![finding("First"), finding("Second"), finding("Third")],
            knowledge_completeness: KnowledgeCompleteness::Partial,
            limitations: vec!["Only selected evidence was assessed.".into()],
        },
    }
}
fn basis(finish: &StagedFinish, indices: Vec<usize>) -> PatchBasis {
    PatchBasis {
        finish_hash: finish.fingerprint().unwrap(),
        refusal_hash: content_hash(b"saved refusal"),
        finding_count: 3,
        flagged_indices: indices,
        flagged_limitations: vec![],
        capacity_gaps: vec![],
    }
}
fn merge(patches: serde_json::Value, indices: Vec<usize>) -> Result<MergedFinish, PatchRejection> {
    let first = finish();
    let saved = basis(&first, indices);
    VolatileFinish::capture(first, saved.clone())
        .unwrap()
        .merge(
            &json!({"type":"finish_patch", "patches":patches}).to_string(),
            &saved,
        )
}

// AC-0572: patch order cannot reorder findings, and only authenticated indices
// are eligible. An omitted flagged index is retained for full re-admission.
#[test]
fn finish_patch_merges_in_original_order_and_validates_indices() {
    let result = merge(
        json!([
            {"finding_index":2,"replace":finding("New third")},
            {"finding_index":0,"replace":finding("New first")}
        ]),
        vec![0, 2],
    )
    .unwrap();
    let InvestigationAction::Finish { findings, .. } = &result.finish.action else {
        panic!()
    };
    assert_eq!(
        findings
            .iter()
            .map(|f| f.title.as_str())
            .collect::<Vec<_>>(),
        ["New first", "Second", "New third"]
    );
    assert_eq!(
        result.counts,
        PatchCounts {
            finding_count: 3,
            flagged_count: 2,
            patch_entries: 2,
            patched_count: 2,
            withdrawn_count: 0,
            kept_count: 1
        }
    );
    let unchanged = merge(json!([]), vec![0]).unwrap();
    assert_eq!(
        unchanged.finish.fingerprint().unwrap(),
        finish().fingerprint().unwrap()
    );
    let withdrawn = merge(json!([{"finding_index":1,"withdraw":true}]), vec![1]).unwrap();
    let InvestigationAction::Finish { findings, .. } = withdrawn.finish.action else {
        panic!()
    };
    assert_eq!(
        findings
            .iter()
            .map(|f| f.title.as_str())
            .collect::<Vec<_>>(),
        ["First", "Third"]
    );
    assert_eq!(withdrawn.counts.withdrawn_count, 1);
    for patches in [
        json!([{"finding_index":0,"withdraw":true},{"finding_index":0,"withdraw":true}]),
        json!([{"finding_index":1,"withdraw":true}]),
        json!([{"finding_index":3,"withdraw":true}]),
        json!([{"finding_index":0,"withdraw":false}]),
    ] {
        assert_eq!(
            merge(patches, vec![0]).err(),
            Some(PatchRejection::InvalidIndex)
        );
    }
    for patches in [
        json!([{"finding_index":0,"withdraw":true,"replace":finding("Ambiguous")}]),
        json!([{"finding_index":0,"withdraw":true,"extra":true}]),
        json!([{"finding_index":-1,"withdraw":true}]),
        json!([{"finding_index":0,"replace":{"title":"Incomplete"}}]),
    ] {
        assert_eq!(
            merge(patches, vec![0]).err(),
            Some(PatchRejection::InvalidShape)
        );
    }
}

// AC-0572: the held candidate has no recovery constructor without its exact
// first finish and binding; consume it on failure as well as success.
#[test]
fn finish_patch_binding_and_empty_result_fail_closed() {
    for change in [0, 1, 2] {
        let first = finish();
        let saved = basis(&first, vec![0]);
        let held = VolatileFinish::capture(first, saved.clone()).unwrap();
        let mut altered = saved;
        match change {
            0 => altered.finish_hash = content_hash(b"other finish"),
            1 => altered.refusal_hash = content_hash(b"other refusal"),
            _ => altered.flagged_indices = vec![1],
        }
        assert_eq!(
            held.merge(r#"{"type":"finish_patch","patches":[]}"#, &altered)
                .err(),
            Some(PatchRejection::BindingMismatch)
        );
    }
    assert_eq!(
        merge(
            json!([
                {"finding_index":0,"withdraw":true}, {"finding_index":1,"withdraw":true},
                {"finding_index":2,"withdraw":true}
            ]),
            vec![0, 1, 2]
        )
        .err(),
        Some(PatchRejection::EmptyResult)
    );
    let mut replacement = finding("Oversized");
    replacement.statement = "x".repeat(MAX_ACTION_BYTES + 1);
    assert!(merge(json!([{"finding_index":0,"replace":replacement}]), vec![0]).is_err());
    assert!(workspace_bytes().unwrap() >= MAX_ACTION_BYTES * 4);
}

// AC-0572: a diagnostic entry count is not a distinct flagged-index count.
#[test]
fn finish_patch_indices_come_only_from_complete_finding_feedback() {
    let first = finish();
    let location = ReplayLocation {
        start: ProsePosition {
            field: ProseField::FindingStatement { finding: 1 },
            sentence: 1,
        },
        end: ProsePosition {
            field: ProseField::FindingStatement { finding: 1 },
            sentence: 1,
        },
    };
    let mut record = RefusalRecord {
        schema_version: 1,
        refusal_id: String::new(),
        task_id: "task".into(),
        request_hash: content_hash(b"request"),
        obligations_hash: content_hash(b"obligations"),
        step_id: "step".into(),
        input_ledger_hash: content_hash(b"ledger"),
        disclosure_hash: content_hash(b"disclosure"),
        finish_hash: first.fingerprint().unwrap(),
        outcome: CompletionOutcome::Supported,
        response_hash: content_hash(b"response"),
        previous_refusal: None,
        refusal_count: 1,
        assessment: vec![],
        feedback: RefusalFeedback {
            citation_drop: None,
            schema_version: 6,
            citation_correction: None,
            unmet: vec![],
            prose_correction: Some(ProseCorrection::Combined(Box::new(CombinedCorrections {
                entries: vec![
                    CombinedCorrection::EnumerationAt(location.clone()),
                    CombinedCorrection::ConfigurationAt(location),
                ],
                omitted: 0,
            }))),
        },
    };
    record.refusal_id = record.content_id().unwrap();
    let saved = PatchBasis::from_refusal(&record, 3).unwrap();
    assert_eq!(saved.flagged_indices, vec![1]);
    if let Some(ProseCorrection::Combined(corrections)) = &mut record.feedback.prose_correction {
        corrections.omitted = 1;
    }
    record.refusal_id = record.content_id().unwrap();
    assert_eq!(
        PatchBasis::from_refusal(&record, 3).err(),
        Some(PatchRejection::UnsupportedRefusal)
    );
}

// AC-0572: schema eligibility does not authorize tools or arbitrary indices.
#[test]
fn finish_patch_schema_uses_saved_indices_and_existing_finding_bounds() {
    let saved = basis(&finish(), vec![0, 2]);
    let ledger = crate::investigation::tests::ledger();
    let schema: serde_json::Value =
        serde_json::from_str(&saved.response_schema(&ledger).unwrap()).unwrap();
    assert_eq!(schema["properties"]["type"]["const"], "finish_patch");
    assert_eq!(schema["properties"]["patches"]["maxItems"], 2);
    for variant in schema["properties"]["patches"]["items"]["oneOf"]
        .as_array()
        .unwrap()
    {
        assert_eq!(
            variant["properties"]["finding_index"]["enum"],
            json!([0, 2])
        );
        assert_eq!(variant["additionalProperties"], false);
    }
    let ordinary: serde_json::Value =
        serde_json::from_str(crate::investigation::response_schema::FINISH).unwrap();
    assert_eq!(
        schema["$defs"]["finding"]["properties"]["statement"],
        ordinary["$defs"]["finding"]["properties"]["statement"]
    );
}

// AC-1420: a finish-patch step on a native tool runtime offers exactly one
// finish_patch tool, and a call decodes to the bare patch action.
#[test]
fn finish_patch_step_becomes_one_native_tool() {
    use crate::investigation::native_tools::{action_from_tool_call, tool_definitions_from_schema};
    let saved = basis(&finish(), vec![0, 2]);
    let ledger = crate::investigation::tests::ledger();
    let tools = tool_definitions_from_schema(&saved.response_schema(&ledger).unwrap()).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "finish_patch");
    assert!(tools[0].input_schema["properties"].get("type").is_none());
    assert!(tools[0].input_schema["$defs"]["finding"].is_object());
    let arguments = json!({"patches": [{"finding_index": 2, "withdraw": true}]});
    let action = action_from_tool_call(&tools, "finish_patch", &arguments, "ctx").unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&action).unwrap(),
        json!({"type": "finish_patch", "patches": [{"finding_index": 2, "withdraw": true}]})
    );
}

fn limitation_refusal(first: &StagedFinish) -> RefusalRecord {
    let at = |field| ProsePosition { field, sentence: 1 };
    let mut record = RefusalRecord {
        schema_version: 1,
        refusal_id: String::new(),
        task_id: "task".into(),
        request_hash: content_hash(b"request"),
        obligations_hash: content_hash(b"obligations"),
        step_id: "step".into(),
        input_ledger_hash: content_hash(b"ledger"),
        disclosure_hash: content_hash(b"disclosure"),
        finish_hash: first.fingerprint().unwrap(),
        outcome: CompletionOutcome::Supported,
        response_hash: content_hash(b"response"),
        previous_refusal: None,
        refusal_count: 1,
        assessment: vec![],
        feedback: RefusalFeedback {
            citation_drop: None,
            schema_version: 6,
            citation_correction: None,
            unmet: vec![],
            prose_correction: Some(ProseCorrection::Combined(Box::new(CombinedCorrections {
                entries: vec![CombinedCorrection::ReplayAt(ReplayLocation {
                    start: at(ProseField::Limitation { limitation: 0 }),
                    end: at(ProseField::Limitation { limitation: 0 }),
                })],
                omitted: 0,
            }))),
        },
    };
    record.refusal_id = record.content_id().unwrap();
    record
}

// AC-1431 (T-1431): for @29 a correction located in a top-level limitation
// becomes a patch of that limitation; older roles keep the whole-answer
// fallback and their exact marker and schema bytes.
#[test]
fn a_flagged_top_level_limitation_is_patched_not_rewritten_whole() {
    let first = finish();
    let record = limitation_refusal(&first);
    assert_eq!(
        PatchBasis::from_refusal(&record, 3).err(),
        Some(PatchRejection::UnsupportedRefusal)
    );
    let saved = PatchBasis::from_refusal_for(&record, 3, true).unwrap();
    assert!(saved.flagged_indices.is_empty());
    assert_eq!(saved.flagged_limitations, vec![0]);
    let legacy = basis(&first, vec![0, 2]);
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("flagged_limitations")
            .is_none()
    );
    let ledger = crate::investigation::tests::ledger();
    let schema: serde_json::Value =
        serde_json::from_str(&saved.response_schema(&ledger).unwrap()).unwrap();
    // Row form: every flagged limitation is a required key.
    assert_eq!(schema["required"], json!(["type", "limitations"]));
    assert_eq!(
        schema["properties"]["limitations"]["required"],
        json!(["0"])
    );
    assert!(schema["properties"].get("patches").is_none());
    let merged = |rows: serde_json::Value| {
        let mut body = json!({"type":"finish_patch"});
        body.as_object_mut()
            .unwrap()
            .extend(rows.as_object().unwrap().clone());
        VolatileFinish::capture(first.clone(), saved.clone())
            .unwrap()
            .merge(&body.to_string(), &saved)
    };
    let replaced = merged(json!({"limitations":{"0":{"replace":"Rewritten scope."}}})).unwrap();
    let InvestigationAction::Finish {
        findings,
        limitations,
        ..
    } = &replaced.finish.action
    else {
        panic!()
    };
    assert_eq!(limitations, &["Rewritten scope.".to_string()]);
    assert_eq!(findings.len(), 3);
    // Withdrawing the only limitation leaves an invalid finish.
    assert_eq!(
        merged(json!({"limitations":{"0":{"withdraw":true}}})).err(),
        Some(PatchRejection::InvalidShape)
    );
    // A flagged row may not be skipped or swapped for another.
    for rows in [
        json!({"limitations":{}}),
        json!({"limitations":{"1":{"replace":"Other."}}}),
    ] {
        assert_eq!(merged(rows).err(), Some(PatchRejection::InvalidIndex));
    }
    for rows in [
        json!({}),
        json!({"limitations":{"0":{"withdraw":false}}}),
        json!({"limitations":{"0":{"replace":"A.","withdraw":true}}}),
        json!({"limitations":{"0":{"replace":"A."}},"findings":{}}),
    ] {
        assert_eq!(merged(rows).err(), Some(PatchRejection::InvalidShape));
    }
}

// AC-1430/AC-1431 (T-1430): a @29 refusal whose only unmet entries are
// capacity exclusions is patchable; the patch must disclose each gap, keyed
// by its 12-char name, and the host prefixes the disclosure with that name.
#[test]
fn a_capacity_gap_is_disclosed_inside_the_patch() {
    let first = finish();
    let gap = "e".repeat(64);
    let mut record = limitation_refusal(&first);
    record.feedback.prose_correction = None;
    record.feedback.unmet = vec![FeedbackEntry {
        obligation_id: gap.clone(),
        kind: "source".into(),
        reason: RefusalReason::ExcludedByCapacity,
    }];
    record.refusal_id = record.content_id().unwrap();
    assert_eq!(
        PatchBasis::from_refusal(&record, 3).err(),
        Some(PatchRejection::UnsupportedRefusal)
    );
    let saved = PatchBasis::from_refusal_for(&record, 3, true).unwrap();
    assert_eq!(saved.capacity_gaps, vec![gap.clone()]);
    let name = capacity_name(&gap).to_string();
    let ledger = crate::investigation::tests::ledger();
    let schema: serde_json::Value =
        serde_json::from_str(&saved.response_schema(&ledger).unwrap()).unwrap();
    assert_eq!(schema["required"], json!(["type", "disclosures"]));
    assert_eq!(
        schema["properties"]["disclosures"]["required"],
        json!([name.clone()])
    );
    let merged = |body: serde_json::Value| {
        VolatileFinish::capture(first.clone(), saved.clone())
            .unwrap()
            .merge(&body.to_string(), &saved)
    };
    let done =
        merged(json!({"type":"finish_patch","disclosures":{name.clone():"Not read: capacity."}}))
            .unwrap();
    let InvestigationAction::Finish { limitations, .. } = &done.finish.action else {
        panic!()
    };
    assert_eq!(
        limitations,
        &[
            "Only selected evidence was assessed.".to_string(),
            format!("{name}: Not read: capacity.")
        ]
    );
    for body in [
        json!({"type":"finish_patch","disclosures":{}}),
        json!({"type":"finish_patch","disclosures":{name.clone():"  "}}),
        json!({"type":"finish_patch"}),
    ] {
        assert!(merged(body).is_err());
    }
    // A full finish must withdraw an unflagged limitation to make room.
    let mut full = first.clone();
    let InvestigationAction::Finish { limitations, .. } = &mut full.action else {
        panic!()
    };
    *limitations = (0..MAX_LIMITATIONS)
        .map(|index| format!("Scope note {index}."))
        .collect();
    let mut full_record = record.clone();
    full_record.finish_hash = full.fingerprint().unwrap();
    full_record.refusal_id = full_record.content_id().unwrap();
    let full_saved = PatchBasis::from_refusal_for(&full_record, 3, true).unwrap();
    let crowded = |body: serde_json::Value| {
        VolatileFinish::capture(full.clone(), full_saved.clone())
            .unwrap()
            .merge(&body.to_string(), &full_saved)
    };
    assert_eq!(
        crowded(json!({"type":"finish_patch","disclosures":{name.clone():"Not read."}})).err(),
        Some(PatchRejection::InvalidShape)
    );
    let roomy = crowded(json!({"type":"finish_patch",
        "limitations":{"7":{"withdraw":true}},"disclosures":{name.clone():"Not read."}}))
    .unwrap();
    let InvestigationAction::Finish { limitations, .. } = &roomy.finish.action else {
        panic!()
    };
    assert_eq!(limitations.len(), MAX_LIMITATIONS);
    assert_eq!(limitations[7], format!("{name}: Not read."));
    assert_eq!(
        crowded(json!({"type":"finish_patch",
            "limitations":{"7":{"replace":"Other."}},"disclosures":{name.clone():"Not read."}}))
        .err(),
        Some(PatchRejection::InvalidIndex)
    );
    // A withdraw of a slot the finish does not have removes nothing (D1, v0.76.0).
    let phantom = merged(json!({"type":"finish_patch",
        "limitations":{"5":{"withdraw":true}},"disclosures":{name.clone():"Not read."}}))
    .unwrap();
    let InvestigationAction::Finish { limitations, .. } = &phantom.finish.action else {
        panic!()
    };
    assert_eq!(
        limitations,
        &[
            "Only selected evidence was assessed.".to_string(),
            format!("{name}: Not read.")
        ]
    );
    // Gaps sharing a 12-char name cannot be told apart: whole answer.
    let mut twins = record.clone();
    let mut other = gap.clone();
    other.replace_range(63.., "f");
    twins.feedback.unmet.push(FeedbackEntry {
        obligation_id: other,
        kind: "source".into(),
        reason: RefusalReason::ExcludedByCapacity,
    });
    twins.refusal_id = twins.content_id().unwrap();
    assert_eq!(
        PatchBasis::from_refusal_for(&twins, 3, true).err(),
        Some(PatchRejection::UnsupportedRefusal)
    );
    // Any other unmet reason still needs a whole answer.
    record.feedback.unmet[0].reason = RefusalReason::NotAttempted;
    record.refusal_id = record.content_id().unwrap();
    assert_eq!(
        PatchBasis::from_refusal_for(&record, 3, true).err(),
        Some(PatchRejection::UnsupportedRefusal)
    );
}

/// AC-1434 (#1728 run-29): an @29 disclosure that claims a finding was
/// withdrawn is refused when the patch withdrew nothing; earlier roles merge
/// it unchanged.
// AC-1444: @32's patch asks for no capacity disclosure, since the host writes
// it. A refusal that flags nothing else has nothing to patch.
#[test]
fn a_host_disclosed_capacity_gap_is_not_asked_of_the_patch() {
    let first = finish();
    let gap = "e".repeat(64);
    let mut record = limitation_refusal(&first);
    record.feedback.unmet = vec![FeedbackEntry {
        obligation_id: gap.clone(),
        kind: "source".into(),
        reason: RefusalReason::ExcludedByCapacity,
    }];
    record.refusal_id = record.content_id().unwrap();
    let old =
        PatchBasis::from_refusal_for_role(&record, 3, SpecialistId::DomainAnalystV31).unwrap();
    assert_eq!(old.capacity_gaps, vec![gap]);
    let new =
        PatchBasis::from_refusal_for_role(&record, 3, SpecialistId::DomainAnalystV32).unwrap();
    assert!(new.capacity_gaps.is_empty());
    assert_eq!(new.flagged_limitations, old.flagged_limitations);
    assert_eq!(new.flagged_indices, old.flagged_indices);
    let ledger = crate::investigation::tests::ledger();
    assert!(
        !new.response_schema(&ledger)
            .unwrap()
            .contains("disclosures")
    );
    record.feedback.prose_correction = None;
    record.refusal_id = record.content_id().unwrap();
    assert!(PatchBasis::from_refusal_for_role(&record, 3, SpecialistId::DomainAnalystV31).is_ok());
    assert_eq!(
        PatchBasis::from_refusal_for_role(&record, 3, SpecialistId::DomainAnalystV32).err(),
        Some(PatchRejection::UnsupportedRefusal)
    );
}

#[test]
fn a_disclosure_claiming_an_unmade_withdrawal_is_refused() {
    let first = finish();
    let gap = "e".repeat(64);
    let mut record = limitation_refusal(&first);
    record.feedback.prose_correction = None;
    record.feedback.unmet = vec![FeedbackEntry {
        obligation_id: gap.clone(),
        kind: "source".into(),
        reason: RefusalReason::ExcludedByCapacity,
    }];
    record.refusal_id = record.content_id().unwrap();
    let saved = PatchBasis::from_refusal_for(&record, 3, true).unwrap();
    let name = capacity_name(&gap).to_string();
    let merged = |text: &str, whole: bool| {
        VolatileFinish::capture(first.clone(), saved.clone())
            .unwrap()
            .merge_with(
                &json!({"type":"finish_patch","disclosures":{name.clone():text}}).to_string(),
                &saved,
                whole,
            )
    };
    let claim = "Not read for capacity; finding 3 is withdrawn.";
    assert_eq!(
        merged(claim, true).err(),
        Some(PatchRejection::UnsupportedEditClaim)
    );
    assert_eq!(
        merged("Not read for capacity. Its finding was withdrawn.", true).err(),
        Some(PatchRejection::UnsupportedEditClaim)
    );
    // The same disclosure without the claim, or with a negated one, merges.
    assert!(merged("Not read for capacity.", true).is_ok());
    assert!(merged("Not read for capacity; no finding was withdrawn.", true).is_ok());
    // Earlier roles keep their exact merge.
    assert!(merged(claim, false).is_ok());
}

// AC-1431 (T-1431): on a @29 patch step a full finish is read as a patch at
// the flagged indices, so unflagged rows stay as first admitted; without the
// opt-in, or with a different finding count, it stays InvalidShape.
#[test]
fn a_full_finish_on_a_patch_step_is_read_as_the_patch() {
    let first = finish();
    let mut saved = basis(&first, vec![1]);
    saved.flagged_limitations = vec![0];
    let mut whole = first.clone();
    let InvestigationAction::Finish {
        findings,
        limitations,
        ..
    } = &mut whole.action
    else {
        panic!()
    };
    findings[0].title = "Unflagged edit".into();
    findings[1].title = "Flagged edit".into();
    limitations[0] = "Rewritten scope.".into();
    let mut raw = serde_json::to_value(&whole.action).unwrap();
    raw["limitations"] = json!(serde_json::to_string(&raw["limitations"]).unwrap());
    let raw = raw.to_string();
    let held = || VolatileFinish::capture(first.clone(), saved.clone()).unwrap();
    assert_eq!(
        held().merge(&raw, &saved).err(),
        Some(PatchRejection::InvalidShape)
    );
    let merged = held().merge_with(&raw, &saved, true).unwrap();
    let InvestigationAction::Finish {
        findings,
        limitations,
        ..
    } = &merged.finish.action
    else {
        panic!()
    };
    assert_eq!(
        findings
            .iter()
            .map(|f| f.title.as_str())
            .collect::<Vec<_>>(),
        ["First", "Flagged edit", "Third"]
    );
    assert_eq!(limitations, &["Rewritten scope.".to_string()]);
    assert_eq!(merged.counts.patched_count, 1);
    let mut short = serde_json::to_value(&whole.action).unwrap();
    short["findings"].as_array_mut().unwrap().pop();
    assert_eq!(
        held().merge_with(&short.to_string(), &saved, true).err(),
        Some(PatchRejection::InvalidShape)
    );
}
