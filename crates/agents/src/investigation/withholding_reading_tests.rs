//! AC-1336 (T-1336, #598): a withheld parent's history names each saved
//! finding's original position, and only the host's reading is admitted.
use super::*;
use crate::investigation::staged_finish::CompletionOutcome;
use serde_json::Value;

fn parent() -> InvestigationResult {
    InvestigationResult::admit(
        "inv-parent",
        &ledger(),
        &action(),
        std::iter::empty(),
        None,
        "2026-09-11T00:00:00Z".into(),
    )
    .unwrap()
}

fn withheld(mut result: InvestigationResult) -> InvestigationResult {
    let hash = |digit: &str| digit.repeat(64);
    result.completion = Some(InvestigationCompletionResult {
        withholding: Some(FindingWithholdingWitness {
            schema_version: 1,
            disposition: WithholdingDisposition::FindingsWithheldAfterCorrection,
            detector_version: 1,
            original_finish_hash: hash("1"),
            original_response_hash: hash("2"),
            request_hash: hash("3"),
            obligations_hash: hash("4"),
            input_ledger_hash: hash("5"),
            disclosure_hash: format!("investigation-disclosure-v6:{}", hash("6")),
            prior_correction_witness_hash: hash("7"),
            projected_result_hash: hash("8"),
            original_finding_count: 1 + u32::try_from(result.findings.len()).unwrap(),
            withheld_indexes: vec![0],
            survivor_indexes: (1..=u32::try_from(result.findings.len()).unwrap()).collect(),
        }),
        schema_version: 1,
        outcome: CompletionOutcome::FindingsWithheldAfterCorrection,
        request_hash: hash("3"),
        obligations_hash: hash("4"),
        finishing_attempts_hash: hash("9"),
        assessment: Vec::new(),
    });
    result
}

#[test]
fn a_withheld_parent_names_each_saved_finding_by_its_original_position() {
    assert_eq!(WithholdingReading::of(&parent()), None);
    let result = withheld(parent());
    let reading = WithholdingReading::of(&result).unwrap();
    assert_eq!(reading.withheld_original_positions, vec![0]);
    // finding-1 is the original finish's second finding, not its first.
    assert_eq!(reading.saved_findings[0].finding_id, "finding-1");
    assert_eq!(reading.saved_findings[0].original_position, 1);
    assert_eq!(reading.saved_findings.len(), result.findings.len());
    assert!(reading.note.contains("not in saved_result.findings"));
}

fn prepare(reading: Option<Value>, result: &InvestigationResult) -> Result<(), InvestigationError> {
    let mut envelope = json!({
        "parent_investigation_id": "inv-parent",
        "original_graph_snapshot_id": result.graph_snapshot_id,
        "original_scope": {"type":"all"},
        "saved_execution_status": "completed",
        "cancellation_requested": false,
        "invocation_outcome_pending": false,
        "authority": "Prior findings remain T3/InferredWeak.",
        "saved_result": result
    });
    if let Some(reading) = reading {
        envelope["withholding_reading"] = reading;
    }
    let history = vec![envelope.to_string()];
    let bytes = serde_json::to_vec(&history).unwrap();
    let mut basis = ledger();
    basis.supplied_history_hash = Some(content_hash(&bytes));
    basis.supplied_history_bytes = bytes.len();
    let page = json!({"facts":[]}).to_string();
    basis.queries[0].response_hash = content_hash(page.as_bytes());
    basis.queries[0].response_bytes = page.len();
    InvestigationInput::prepare(
        "Explain evidence",
        SpecialistId::DomainAnalyst,
        basis,
        &[page],
        &[],
        &history,
        &InvestigationUsage::default(),
    )
    .map(|_| ())
}

#[test]
fn a_supplied_reading_must_be_the_saved_results_own() {
    let plain = parent();
    let result = withheld(parent());
    let reading = serde_json::to_value(WithholdingReading::of(&result).unwrap()).unwrap();
    // A history saved before the reading omits it.
    assert_eq!(prepare(None, &plain), Ok(()));
    // A reading beside a parent that withheld nothing is refused.
    assert_eq!(
        prepare(Some(reading.clone()), &plain),
        Err(InvestigationError::InvalidInput)
    );
    // The host's own reading is admitted beside its withheld parent.
    assert_eq!(prepare(Some(reading.clone()), &result), Ok(()));
    let mut moved = reading;
    moved["saved_findings"][0]["original_position"] = json!(0);
    moved["withheld_original_positions"] = json!([1]);
    let shifted = withheld(parent());
    assert_eq!(
        prepare(Some(moved), &shifted),
        Err(InvestigationError::InvalidInput)
    );
}
