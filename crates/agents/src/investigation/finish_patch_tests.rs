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
