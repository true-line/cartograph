use super::projection_tests::{bind_pages, page};
use super::*;
use llm::bounded::CompletionLimits;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
use std::time::Duration;

fn initial() -> InvestigationInputLedger {
    let mut ledger = ledger();
    ledger.revision = 0;
    ledger.queries.clear();
    ledger.citations.clear();
    ledger.selected_facts.clear();
    ledger
}
fn basis(
    current: &InvestigationInputLedger,
    previous: Option<InvestigationInputLedger>,
    tool: Option<InvestigationTool>,
) -> InvestigationTurnBasis {
    InvestigationTurnBasis {
        input_ledger_hash: current.fingerprint().unwrap(),
        previous_ledger: previous,
        completed_tool: tool,
    }
}
fn input(
    current: &InvestigationInputLedger,
    previous: Option<InvestigationInputLedger>,
    tool: Option<InvestigationTool>,
    pages: &[String],
) -> InvestigationInput {
    let evidence = current
        .citations
        .iter()
        .filter(|citation| citation.source.is_some())
        .map(|citation| crate::AgentEvidence {
            id: citation.citation_id.clone(),
            source: citation.source.clone().unwrap(),
            text: "private text".into(),
        })
        .collect::<Vec<_>>();
    InvestigationInput::prepare_v4(
        "Inspect behavior",
        SpecialistId::DomainAnalystV4,
        current.clone(),
        pages,
        &evidence,
        &[],
        &InvestigationUsage::default(),
        &basis(current, previous, tool),
    )
    .unwrap()
}
fn prompt(input: &InvestigationInput) -> serde_json::Value {
    serde_json::from_str(&input.action("inv:step".into()).unwrap().payload.prompt).unwrap()
}

// AC-0260/T0260: verified query metadata drives progress, not an invented action.
#[test]
fn v4_turn_progress_preserves_continuation_and_distinguishes_repeated_results() {
    let empty = initial();
    let first = prompt(&input(&empty, None, None, &[]));
    assert_eq!(first["turn_progress"]["phase"], "initial");
    assert!(first["turn_progress"]["last_completed_action"].is_null());
    let mut current = ledger();
    let mut value: serde_json::Value =
        serde_json::from_str(&page(&current, "first representation")).unwrap();
    let cursor = json!({"snapshot_id":current.scope_snapshot_id,"selection_id":"selection-fixture","offset":1});
    value["context"]["next_cursor"] = cursor.clone();
    let raw = value.to_string();
    current.queries[0].has_more = true;
    bind_pages(&mut current, std::slice::from_ref(&raw));
    let first = prompt(&input(
        &current,
        Some(empty),
        Some(InvestigationTool::QueryContext),
        std::slice::from_ref(&raw),
    ));
    let action = &first["turn_progress"]["last_completed_action"];
    assert_eq!(action["new_fact_count"], 1);
    assert_eq!(action["next_cursor"], cursor);
    assert_eq!(action["no_progress"], false);
    let previous = current.clone();
    current.revision += 1;
    bind_pages(&mut current, &[raw.clone(), raw.clone()]);
    let repeated = prompt(&input(
        &current,
        Some(previous.clone()),
        Some(InvestigationTool::QueryContext),
        &[raw.clone(), raw.clone()],
    ));
    assert_eq!(
        repeated["turn_progress"]["last_completed_action"]["no_progress"],
        true
    );
    value["context"]["facts"][0]["properties"]["note"] = json!("changed representation");
    let changed = vec![raw, value.to_string()];
    bind_pages(&mut current, &changed);
    let changed = prompt(&input(
        &current,
        Some(previous),
        Some(InvestigationTool::QueryContext),
        &changed,
    ));
    let action = &changed["turn_progress"]["last_completed_action"];
    assert_eq!(action["new_fact_count"], 0);
    assert_eq!(action["repeated_query"], true);
    assert_eq!(action["unchanged_result"], false);
    assert_eq!(action["no_progress"], false);
}

#[test]
fn v4_turn_progress_admits_repeated_reads_and_rejects_mismatched_basis() {
    let mut current = super::disclosure_tests::basis_with_source();
    let pages = vec![page(&current, "source evidence")];
    bind_pages(&mut current, &pages);
    current.revision = 2;
    let mut previous = current.clone();
    previous.revision = 1;
    previous.citations.pop();
    let first = prompt(&input(
        &current,
        Some(previous),
        Some(InvestigationTool::ReadEvidence),
        &pages,
    ));
    assert_eq!(
        first["turn_progress"]["last_completed_action"]["previously_acquired"],
        false
    );
    let previous = current.clone();
    current.revision += 1;
    let mut citation = current.citations.last().unwrap().clone();
    citation.citation_id = "evidence-2".into();
    current.citations.push(citation);
    let repeated = prompt(&input(
        &current,
        Some(previous.clone()),
        Some(InvestigationTool::ReadEvidence),
        &pages,
    ));
    assert_eq!(
        repeated["turn_progress"]["last_completed_action"]["previously_acquired"],
        true
    );
    assert_eq!(
        repeated["turn_progress"]["last_completed_action"]["source_range_bytes"],
        12
    );
    let mut wrong = basis(
        &current,
        Some(previous),
        Some(InvestigationTool::QueryContext),
    );
    assert_eq!(
        wrong.validate(&current),
        Err(InvestigationError::InvalidInput)
    );
    wrong.input_ledger_hash = "a".repeat(64);
    assert_eq!(
        wrong.validate(&current),
        Err(InvestigationError::InvalidInput)
    );
}

#[test]
fn v4_raw_query_budget_uses_the_shared_admission_limit_and_counts_repeats() {
    let mut current = initial();
    let budget = InvestigationQueryBudget::from_ledger(&current).unwrap();
    assert_eq!(budget.limit_bytes, MAX_TOTAL_QUERY_BYTES);
    assert_eq!(
        budget.admit(MAX_TOTAL_QUERY_BYTES).unwrap().remaining_bytes,
        0
    );
    assert_eq!(
        budget.admit(MAX_TOTAL_QUERY_BYTES + 1),
        Err(InvestigationError::LimitExceeded)
    );
    let template = ledger().queries.remove(0);
    current.queries = vec![
        InvestigationQueryManifest {
            response_bytes: MAX_QUERY_BYTES,
            ..template
        };
        3
    ];
    assert_eq!(
        InvestigationQueryBudget::from_ledger(&current)
            .unwrap()
            .remaining_bytes,
        0
    );
}

#[test]
fn v4_sealing_protects_turn_state_and_legacy_input_requires_explicit_version() {
    let current = initial();
    let input = input(&current, None, None, &[]);
    assert!(
        InvestigationInput::prepare(
            "Inspect",
            SpecialistId::DomainAnalystV4,
            current,
            &[],
            &[],
            &[],
            &InvestigationUsage::default()
        )
        .is_err()
    );
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let action = input.action("inv:step".into()).unwrap();
    let prepared = firewall
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    assert!(input.seal_disclosure(&prepared).unwrap().is_some());
    let mut changed = action.clone();
    let mut value: serde_json::Value = serde_json::from_str(&changed.payload.prompt).unwrap();
    value["turn_progress"]["raw_query_budget"]["remaining_bytes"] = json!(1);
    changed.payload.prompt = value.to_string();
    let prepared = firewall
        .prepare_bounded(&provider, &changed, &CompletionLimits::default())
        .unwrap();
    assert!(
        crate::investigation::sealing::seal(input.ledger(), &action.payload, &prepared).is_err()
    );
    assert_ne!(
        SpecialistId::DomainAnalystV4
            .definition()
            .unwrap()
            .prompt_fingerprint,
        SpecialistId::DomainAnalystV3
            .definition()
            .unwrap()
            .prompt_fingerprint
    );
}

#[test]
fn v4_no_progress_requires_one_prior_query_response_pair() {
    let mut current = ledger();
    let a = current.queries[0].query.clone();
    let mut b = a.clone();
    b.max_facts = 11;
    let y = page(&current, "result Y");
    let x = page(&current, "result X");
    let pages = vec![y, x.clone(), x];
    bind_pages(&mut current, &pages);
    current.queries[1].query = b;
    current.revision = 3;
    let mut previous = current.clone();
    previous.revision = 2;
    previous.queries.pop();
    let value = prompt(&input(
        &current,
        Some(previous),
        Some(InvestigationTool::QueryContext),
        &pages,
    ));
    let action = &value["turn_progress"]["last_completed_action"];
    assert_eq!(action["repeated_query"], true);
    assert_eq!(action["unchanged_result"], false);
    assert_eq!(action["no_progress"], false);
}

#[test]
fn v4_turn_progress_rejects_malformed_admitted_source_ranges() {
    let mut current = super::disclosure_tests::basis_with_source();
    current.revision = 2;
    let mut previous = current.clone();
    previous.revision = 1;
    previous.citations.pop();
    let basis = basis(
        &current,
        Some(previous),
        Some(InvestigationTool::ReadEvidence),
    );
    // A tampered admission must return a typed error, never wrap the range or
    // emit a fabricated byte count. Exercise the public progress boundary.
    for (start, end) in [(12, 0), (u64::MAX, 1), (12, 12)] {
        let source = current
            .citations
            .last_mut()
            .unwrap()
            .source
            .as_mut()
            .unwrap();
        source.byte_start = start;
        source.byte_end = end;
        assert_eq!(
            basis.progress(&current, &json!({})),
            Err(InvestigationError::InvalidInput)
        );
    }
}
