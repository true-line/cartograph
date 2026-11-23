//! AC-0980 (T-0980): the gated `draft-rules@1` workflow.
use super::*;
use std::collections::VecDeque;

fn address() -> SectionAddress {
    SectionAddress {
        catalogue_version: "1".into(),
        document: "D1".into(),
        section: "1.5".into(),
        scope: "system".into(),
    }
}

fn id(n: usize) -> String {
    format!("F-{n:016x}")
}

fn fact(n: usize) -> ComposeFact {
    ComposeFact {
        citation_id: id(n),
        label: "BusinessRule".into(),
        name: Some(format!("rule {n}")),
        tier: "Deterministic".into(),
        confidence: "Verified".into(),
    }
}

/// A §1.5 section with `rows` rule rows under one capability.
fn section(rows: usize) -> String {
    let mut text = String::from(
        "## 1.5 Business rules (P+A)\n\n### Billing\n\n#### Manage billing (dashboard)\n\n\
         Changes:\n\n| Where | Rule | Source | Citation |\n|---|---|---|---|\n",
    );
    for n in 1..=rows {
        text.push_str(&format!(
            "| billing | When the plan is plan{n}, the page flashes \"Plan {n} chosen\" | \
             billing.py | {} |\n",
            id(n)
        ));
    }
    text
}

fn claim(statement: &str, ids: &[usize]) -> serde_json::Value {
    serde_json::json!({
        "statement": statement,
        "claim_kind": "implemented_behavior",
        "citation_ids": ids.iter().map(|n| id(*n)).collect::<Vec<_>>(),
    })
}

fn reply(claims: Vec<serde_json::Value>) -> String {
    serde_json::json!({ "claims": claims }).to_string()
}

fn good(n: usize) -> serde_json::Value {
    claim(
        &format!("When the chosen plan is plan{n}, the billing page confirms \"Plan {n} chosen\"."),
        &[n],
    )
}

fn supported(claims: usize) -> String {
    serde_json::json!({
        "verdicts": (1..=claims)
            .map(|n| serde_json::json!({"claim": n, "verdict": "supported"}))
            .collect::<Vec<_>>()
    })
    .to_string()
}

struct Fake {
    replies: VecDeque<String>,
    prompts: Vec<String>,
}

impl Fake {
    fn new(replies: Vec<String>) -> Self {
        Self {
            replies: replies.into(),
            prompts: Vec::new(),
        }
    }
}

impl StageModel for Fake {
    fn complete(&mut self, action: &CompletionAction) -> Result<String, String> {
        self.prompts.push(action.payload.prompt.clone());
        self.replies
            .pop_front()
            .ok_or_else(|| "no reply".to_owned())
    }
}

fn never(_: &Claim) -> bool {
    false
}

fn run_with(workflow: &Workflow, rows: usize, model: &mut Fake) -> GatedOutcome {
    let address = address();
    let text = section(rows);
    let facts: Vec<ComposeFact> = (1..=rows).map(fact).collect();
    run(
        workflow,
        &GatedInput {
            address: &address,
            title: "Business rules",
            section: &text,
            facts: &facts,
        },
        model,
        &never,
    )
    .expect("the run completes")
}

fn no_audit() -> Workflow {
    Workflow {
        audit_round_trips: None,
        ..Workflow::shipped().expect("the shipped workflow parses")
    }
}

#[test]
fn the_shipped_workflow_parses_with_every_check_and_the_audit() {
    let workflow = Workflow::shipped().expect("parses");
    assert_eq!(workflow.section, "1.5");
    assert_eq!(
        (workflow.max_rows, workflow.max_claims, workflow.max_retries),
        (10, 8, 2)
    );
    assert!(workflow.checks.contains(&CheckType::LiteralsSupported));
    assert_eq!(workflow.checks.len(), 8);
    assert_eq!(workflow.audit_round_trips, Some(1));
    assert!(
        Workflow::from_json(&WORKFLOW_FILE.replace("\"max_rows\": 10", "\"max_rows\": 0")).is_err()
    );
    assert!(
        Workflow::from_json(&WORKFLOW_FILE.replace("\"literals_supported\"", "\"guess\"")).is_err()
    );
}

#[test]
fn partition_cuts_rule_rows_into_bounded_chunks_that_read_on_their_own() {
    let citable: BTreeSet<String> = (1..=23).map(id).collect();
    let chunks = partition(&section(23), &citable, 10);
    assert_eq!(
        chunks.iter().map(|c| c.rows.len()).collect::<Vec<_>>(),
        [10, 10, 3]
    );
    for chunk in &chunks {
        assert!(chunk.text.starts_with("## 1.5 Business rules"));
        assert!(chunk.text.contains("#### Manage billing (dashboard)"));
        assert!(
            chunk
                .text
                .contains("| Where | Rule | Source | Citation |\n|---|---|---|---|\n")
        );
    }
    // A row citing nothing citable is not a rule row.
    let few: BTreeSet<String> = [id(1)].into();
    assert_eq!(partition(&section(3), &few, 10)[0].rows.len(), 1);
}

// AC-1604 (#1748): a Gap row states nothing and a row repeating an earlier
// row's rule (the same Rule cell, or only ids already printed) is left out
// before drafting, and counted.
#[test]
fn partition_leaves_out_gap_rows_and_repeated_rules() {
    let mut text = section(2);
    text.push_str(&format!(
        "| billing | Not stated in domain terms (Gap): the condition or outcome is outside the \
         rendering rules | x | {} |\n",
        id(3)
    ));
    text.push_str(
        "\n#### Change plan (API)\n\n| Where | Rule | Source | Citation |\n|---|---|---|---|\n",
    );
    // The same rule printed again under another capability, with its own id.
    text.push_str(&format!(
        "| ↳ billing | When the plan is plan1, the page flashes \"Plan 1 chosen\" | billing.py | {} |\n",
        id(4)
    ));
    // Another wording, but only an id an earlier row printed.
    text.push_str(&format!(
        "| ↳ billing | The plan is checked | billing.py | {} |\n",
        id(2)
    ));
    text.push_str(&format!(
        "| api | When no plan is given, it fails | api.py | {} |\n",
        id(5)
    ));
    let citable: BTreeSet<String> = (1..=5).map(id).collect();
    let (chunks, skipped) = partition_counted(&text, &citable, 10);
    let kept: Vec<String> = chunks.iter().flat_map(|c| c.ids()).collect();
    assert_eq!(kept, [id(1), id(2), id(5)]);
    assert_eq!(
        skipped,
        Skipped {
            gap_rows: 1,
            repeated_rows: 2
        }
    );
    assert_eq!(partition(&text, &citable, 10), chunks);
}

#[test]
fn a_passing_draft_advances_without_a_retry() {
    let mut model = Fake::new(vec![reply((1..=3).map(good).collect())]);
    let outcome = run_with(&no_audit(), 3, &mut model);
    assert_eq!(outcome.stats.first_try_passed, 1);
    assert_eq!(outcome.stats.retries, 0);
    assert_eq!(outcome.stats.rows_covered, 3);
    assert_eq!(outcome.finish.expect("kept").claims.len(), 3);
    assert_eq!(outcome.left_out, 0);
}

#[test]
fn a_dropped_row_fails_rows_covered_and_the_retry_carries_its_prescription() {
    let mut model = Fake::new(vec![
        reply(vec![good(1), good(2)]),
        reply((1..=3).map(good).collect()),
    ]);
    let outcome = run_with(&no_audit(), 3, &mut model);
    assert_eq!(outcome.stats.retries, 1);
    assert_eq!(outcome.stats.rows_covered, 3);
    let retry = &model.prompts[1];
    assert!(retry.contains("<checks_failed>"));
    assert!(retry.contains("1 rule row(s) are not cited"));
    assert!(retry.contains(&id(3)));
    assert!(retry.contains("<previous_reply>"));
}

#[test]
fn literals_supported_catches_invented_statuses_and_placeholders() {
    let text = section(2);
    let citable: BTreeSet<String> = [id(1), id(2)].into();
    let chunk = &partition(&text, &citable, 10)[0];
    let claims = vec![
        Claim {
            statement: "When the plan is plan1, the page shows a notice with status code 400."
                .into(),
            kind: ClaimKind::Implemented,
            citations: vec![id(1)],
        },
        Claim {
            statement:
                "When the plan is plan2, the page greets {partner_user.partner.name} by name."
                    .into(),
            kind: ClaimKind::Implemented,
            citations: vec![id(2)],
        },
        Claim {
            statement: "When the plan is plan1, the page confirms \"Plan 1 chosen\".".into(),
            kind: ClaimKind::Implemented,
            citations: vec![id(1)],
        },
    ];
    let result = gate(
        &Workflow::shipped().unwrap().checks,
        &claims,
        Some(0),
        chunk,
        &never,
    );
    assert!(result.failures.contains(&CheckType::LiteralsSupported));
    assert_eq!(result.claim_ok, [false, false, true]);
    assert!(result.prescriptions.iter().any(|p| p.contains("400")));
    assert!(
        result
            .prescriptions
            .iter()
            .any(|p| p.contains("placeholder"))
    );
}

#[test]
fn the_other_checks_name_what_to_fix() {
    let text = section(1);
    let citable: BTreeSet<String> = [id(1)].into();
    let chunk = &partition(&text, &citable, 10)[0];
    let checks = Workflow::shipped().unwrap().checks;
    let one = |statement: &str| Claim {
        statement: statement.into(),
        kind: ClaimKind::Implemented,
        citations: vec![id(1)],
    };
    let fails = |claim: Claim| gate(&checks, &[claim], Some(0), chunk, &never).failures;
    assert!(
        fails(one(
            "When plan1 is chosen, set_plan() stores the plan for the user."
        ))
        .contains(&CheckType::NoCodeIdentifiers)
    );
    assert!(
        fails(one(
            "When plan1 is chosen, the page says \"Welcome aboard friend\"."
        ))
        .contains(&CheckType::QuotesVerbatim)
    );
    assert!(
        fails(one(
            "The plan is checked first and then the page confirms plan1."
        ))
        .contains(&CheckType::OrderingSupported)
    );
    let names = |claim: &Claim| claim.statement.contains("billing.py");
    let named = gate(
        &checks,
        &[one(
            "When plan1 is chosen, billing.py confirms the choice for the user.",
        )],
        Some(0),
        chunk,
        &names,
    );
    assert!(named.failures.contains(&CheckType::NamesSource));
    assert!(
        gate(&checks, &[], None, chunk, &never)
            .failures
            .contains(&CheckType::ReplyMatchesSchema)
    );
}

#[test]
fn after_the_last_retry_only_claims_passing_every_check_are_kept() {
    let bad = claim(
        "When the plan is plan2, the server answers with status 500 to the user.",
        &[2],
    );
    let mut model = Fake::new(vec![
        reply(vec![good(1), bad.clone()]),
        reply(vec![good(1), bad.clone()]),
        reply(vec![good(1), bad]),
    ]);
    let outcome = run_with(&no_audit(), 2, &mut model);
    assert_eq!(outcome.stats.retries, 2);
    assert_eq!(outcome.finish.expect("kept").claims.len(), 1);
    assert_eq!(outcome.left_out, 1);
    assert_eq!(outcome.stats.rows_covered, 1);
    assert_eq!(outcome.stats.failures.get("literals_supported"), Some(&1));
}

#[test]
fn the_auditor_sends_unsupported_claims_back_once_then_drops_them() {
    let added = |n: usize| {
        claim(
            &format!(
                "When the plan is plan{n}, the billing page confirms the plan and emails the user."
            ),
            &[n],
        )
    };
    let adds_detail = serde_json::json!({"verdicts": [
        {"claim": 1, "verdict": "supported"},
        {"claim": 2, "verdict": "adds_detail"}
    ]})
    .to_string();
    let mut model = Fake::new(vec![
        reply(vec![good(1), added(2)]),
        adds_detail.clone(),
        reply(vec![good(1), added(2)]),
        adds_detail,
    ]);
    let outcome = run_with(&Workflow::shipped().unwrap(), 2, &mut model);
    assert_eq!(outcome.stats.sent_back, 1);
    assert_eq!(outcome.stats.audit_dropped, 1);
    assert_eq!(outcome.finish.expect("kept").claims.len(), 1);
    assert!(model.prompts[2].contains("Claim 2 states something its cited rows do not"));
    assert!(model.prompts[2].contains("Keep the claims the checks did not name unchanged."));

    // A revision the auditor accepts is kept.
    let mut model = Fake::new(vec![
        reply(vec![good(1), added(2)]),
        serde_json::json!({"verdicts": [
            {"claim": 1, "verdict": "supported"},
            {"claim": 2, "verdict": "adds_detail"}
        ]})
        .to_string(),
        reply(vec![good(1), good(2)]),
        supported(2),
    ]);
    let outcome = run_with(&Workflow::shipped().unwrap(), 2, &mut model);
    assert_eq!(outcome.stats.audit_dropped, 0);
    assert_eq!(outcome.stats.rows_covered, 2);
}

#[test]
fn an_auditor_reply_outside_the_schema_is_asked_again_then_its_claims_are_left_out() {
    // A second reply in the schema judges the claims.
    let mut model = Fake::new(vec![reply(vec![good(1)]), "not json".into(), supported(1)]);
    let outcome = run_with(&Workflow::shipped().unwrap(), 1, &mut model);
    assert_eq!(outcome.stats.audit_failed, 1);
    assert_eq!(outcome.finish.expect("kept").claims.len(), 1);
    // Two replies outside it: no claim is staged unjudged.
    let mut model = Fake::new(vec![
        reply(vec![good(1), good(2)]),
        "not json".into(),
        serde_json::json!({"verdicts": [{"claim": 1, "verdict": "supported"}]}).to_string(),
    ]);
    let outcome = run_with(&Workflow::shipped().unwrap(), 2, &mut model);
    assert_eq!(outcome.stats.audit_failed, 2);
    assert_eq!(outcome.stats.audit_dropped, 2);
    assert_eq!(outcome.left_out, 2);
    assert!(outcome.finish.is_none());
}

#[test]
fn claims_past_the_stage_bound_are_left_out_and_fail_the_reply() {
    let workflow = Workflow {
        max_claims: 2,
        max_retries: 0,
        ..no_audit()
    };
    let mut model = Fake::new(vec![reply(vec![good(1), good(2), good(3)])]);
    let outcome = run_with(&workflow, 3, &mut model);
    assert_eq!(outcome.finish.expect("kept").claims.len(), 2);
    assert_eq!(outcome.left_out, 1);
    assert_eq!(outcome.stats.first_try_passed, 0);
    // The retry is told the bound.
    let workflow = Workflow {
        max_claims: 2,
        max_retries: 1,
        ..no_audit()
    };
    let mut model = Fake::new(vec![
        reply(vec![good(1), good(2), good(3)]),
        reply(vec![good(1), good(2)]),
    ]);
    run_with(&workflow, 3, &mut model);
    assert!(
        model.prompts[1].contains("Reply with at most 2 claims; the 1 after them were left out."),
        "{}",
        model.prompts[1]
    );
}

#[test]
fn an_auditor_reply_that_misses_or_repeats_a_claim_is_a_failed_audit() {
    assert_eq!(
        decode_audit(&supported(2), 2),
        Some(vec![Verdict::Supported; 2])
    );
    for reply in [
        serde_json::json!({"verdicts": []}),
        serde_json::json!({"verdicts": [{"claim": 1, "verdict": "supported"}]}),
        serde_json::json!({"verdicts": [
            {"claim": 1, "verdict": "supported"},
            {"claim": 1, "verdict": "supported"},
            {"claim": 2, "verdict": "supported"}
        ]}),
        serde_json::json!({"verdicts": [
            {"claim": 0, "verdict": "supported"},
            {"claim": 2, "verdict": "supported"}
        ]}),
        serde_json::json!({"verdicts": [
            {"claim": 1, "verdict": "supported"},
            {"claim": 3, "verdict": "supported"}
        ]}),
    ] {
        assert_eq!(decode_audit(&reply.to_string(), 2), None, "{reply}");
    }
    let mut model = Fake::new(vec![
        reply(vec![good(1), good(2)]),
        serde_json::json!({"verdicts": []}).to_string(),
        serde_json::json!({"verdicts": []}).to_string(),
    ]);
    let outcome = run_with(&Workflow::shipped().unwrap(), 2, &mut model);
    assert_eq!(outcome.stats.audit_failed, 2);
    assert!(outcome.finish.is_none());
}

#[test]
fn assemble_keeps_one_claim_per_exact_statement_citing_every_row() {
    let workflow = Workflow {
        max_rows: 1,
        ..no_audit()
    };
    let same = |statement: &str, n: usize| claim(statement, &[n]);
    let text = "When a plan is chosen, the billing page confirms the choice.";
    let mut model = Fake::new(vec![
        reply(vec![same(text, 1)]),
        reply(vec![same(&text.replace(' ', "  "), 2)]),
        reply(vec![same(&text.replace("the choice", "the CHOICE"), 3)]),
    ]);
    let outcome = run_with(&workflow, 3, &mut model);
    assert_eq!(outcome.stats.chunks, 3);
    // Spacing alone is the same claim, which then cites both rows; a
    // statement that differs in case is another claim.
    let claims = outcome.finish.expect("kept").claims;
    assert_eq!(claims.len(), 2);
    assert_eq!(claims[0].citations, vec![id(1), id(2)]);
    assert_eq!(outcome.left_out, 0);
    assert_eq!(outcome.stats.rows_covered, 3);

    // Rows past one claim's citation bound keep the repeat as its own claim.
    let mut replies = Vec::new();
    for chunk in 0..3 {
        let ids: Vec<usize> = (chunk * 10 + 1..=chunk * 10 + 10).collect();
        replies.push(reply(vec![claim(text, &ids)]));
    }
    let workflow = Workflow {
        max_rows: 10,
        ..no_audit()
    };
    let mut model = Fake::new(replies);
    let outcome = run_with(&workflow, 30, &mut model);
    let claims = outcome.finish.expect("kept").claims;
    assert_eq!(claims.len(), 3);
    assert!(claims.iter().all(|claim| claim.citations.len() == 10));
    assert_eq!(outcome.stats.rows_covered, 30);
}

#[test]
fn a_model_error_stops_the_run() {
    let address = address();
    let text = section(1);
    let facts = vec![fact(1)];
    let mut model = Fake::new(Vec::new());
    let result = run(
        &no_audit(),
        &GatedInput {
            address: &address,
            title: "Business rules",
            section: &text,
            facts: &facts,
        },
        &mut model,
        &never,
    );
    assert_eq!(result, Err("no reply".to_owned()));
}
