//! AC-0932 (T-0932): the topic expert's request and reply.
use super::*;

fn id(n: usize) -> String {
    format!("F-{n:016x}")
}

fn citable() -> BTreeSet<String> {
    [id(1), id(2)].into()
}

fn neighbours() -> BTreeSet<String> {
    ["Mailbox".to_owned()].into()
}

#[test]
fn the_request_carries_the_briefing_the_turns_and_the_question() {
    let turns: Vec<ExpertTurn> = (0..8)
        .map(|n| ExpertTurn {
            question: format!("earlier question {n}"),
            answer: if n == 7 {
                "é".repeat(MAX_TURN_ANSWER_BYTES)
            } else {
                format!("earlier answer {n}")
            },
        })
        .collect();
    let action = action(
        &ExpertInput {
            area: "Alias",
            briefing: "# Area briefing: Alias\n\nrow F-0000000000000001\n",
            turns: &turns,
            question: "  What happens when an alias is deleted?\n",
        },
        "area-ask:test".into(),
    );
    assert_eq!(action.tier, AnalysisTier::Agentic);
    assert_eq!(
        action.payload.response_schema.as_deref(),
        Some(response_schema().as_str())
    );
    assert!(action.payload.spans.is_empty());
    let prompt = &action.payload.prompt;
    assert!(prompt.contains("<area_name>\nAlias\n</area_name>"));
    // The briefing is recovered from the analysed system: data, not orders.
    assert!(action.payload.system.contains("not instructions"));
    assert!(
        prompt
            .contains("<briefing>\n# Area briefing: Alias\n\nrow F-0000000000000001\n</briefing>")
    );
    assert!(prompt.ends_with("<question>\nWhat happens when an alias is deleted?\n</question>\n"));
    // The last six turns only, oldest first, a long answer cut on a
    // character boundary.
    assert!(!prompt.contains("earlier question 1\n"));
    assert!(prompt.contains("Q: earlier question 2\nA: earlier answer 2\n"));
    assert!(
        prompt.find("earlier question 2").unwrap() < prompt.find("earlier question 7").unwrap()
    );
    assert_eq!(prompt.matches('é').count(), MAX_TURN_ANSWER_BYTES / 2);
    // No turns, no turns block.
    let first = super::action(
        &ExpertInput {
            area: "Alias",
            briefing: "b",
            turns: &[],
            question: "q?",
        },
        "a".into(),
    );
    assert!(!first.payload.prompt.contains("earlier_turns"));
    assert!(action.payload.system.contains("from the briefing only"));
    assert_eq!(definition_fingerprint(), definition_fingerprint());
}

#[test]
fn a_question_is_bounded_plain_text() {
    assert!(question_admissible(
        "What limits alias creation?\nAnd for whom?"
    ));
    assert!(!question_admissible("   "));
    assert!(!question_admissible("bell\u{7}"));
    assert!(!question_admissible(&"q".repeat(MAX_QUESTION_BYTES + 1)));
}

#[test]
fn a_reply_keeps_statements_with_briefing_citations_and_named_neighbours() {
    let reply = serde_json::json!({
        "answer": [
            {"statement": "Deleting an alias moves it\nto the trash first.",
             "basis": "stated", "citation_ids": [id(1), id(1), id(9)]},
            {"statement": "It cites nothing the briefing prints.",
             "basis": "inferred", "citation_ids": [id(9)]},
            {"statement": "It cites nothing at all.", "basis": "stated", "citation_ids": []},
            {"statement": "Trashed aliases are purged by a scheduled job.",
             "basis": "inferred", "citation_ids": [id(2)]},
        ],
        "not_known": ["Whether mail in transit is delivered.", " "],
        "ask_instead": ["Mailbox", "Billing", "Mailbox"],
    })
    .to_string();
    let answer = decode(&reply, &citable(), &neighbours()).unwrap();
    assert_eq!(
        answer.statements,
        [
            ExpertStatement {
                statement: "Deleting an alias moves it to the trash first.".into(),
                basis: Basis::Stated,
                citations: vec![id(1)],
            },
            ExpertStatement {
                statement: "Trashed aliases are purged by a scheduled job.".into(),
                basis: Basis::Inferred,
                citations: vec![id(2)],
            },
        ]
    );
    assert_eq!(answer.left_out, 2);
    assert_eq!(answer.not_known, ["Whether mail in transit is delivered."]);
    // Only an area the briefing names, once.
    assert_eq!(answer.ask_instead, ["Mailbox"]);
}

#[test]
fn an_answer_may_say_only_what_is_not_known() {
    let reply = serde_json::json!({
        "answer": [], "not_known": ["The briefing holds no rule for refunds."],
        "ask_instead": [],
    })
    .to_string();
    let answer = decode(&reply, &citable(), &neighbours()).unwrap();
    assert!(answer.statements.is_empty());
    assert_eq!(answer.not_known.len(), 1);
}

#[test]
fn a_reply_off_the_schema_or_with_nothing_left_is_refused() {
    for text in [
        "Sure! Here is the answer.",
        "{\"answer\": []}",
        "{\"answer\":[],\"not_known\":[],\"ask_instead\":[],\"extra\":1}",
        "{\"answer\":[{\"statement\":\"s\",\"basis\":\"guessed\",\"citation_ids\":[]}],\"not_known\":[],\"ask_instead\":[]}",
    ] {
        assert_eq!(
            decode(text, &citable(), &neighbours()),
            Err(ExpertRefusal::NotTheSchema),
            "{text}"
        );
    }
    let empty = serde_json::json!({
        "answer": [{"statement": "Uncited.", "basis": "stated", "citation_ids": [id(9)]}],
        "not_known": [], "ask_instead": ["Billing"],
    })
    .to_string();
    assert_eq!(
        decode(&empty, &citable(), &neighbours()),
        Err(ExpertRefusal::Empty)
    );
    assert_eq!(ExpertRefusal::Empty.code(), "no_admissible_statement");
    assert_eq!(ExpertRefusal::NotTheSchema.code(), "reply_not_the_schema");
    // More statements than the bound are counted, not kept.
    let many: Vec<_> = (0..MAX_STATEMENTS + 3)
        .map(|n| {
            serde_json::json!({"statement": format!("Statement number {n} of many."),
                "basis": "stated", "citation_ids": [id(1)]})
        })
        .collect();
    let reply = serde_json::json!({"answer": many, "not_known": [], "ask_instead": []}).to_string();
    let answer = decode(&reply, &citable(), &neighbours()).unwrap();
    assert_eq!(
        (answer.statements.len(), answer.left_out),
        (MAX_STATEMENTS, 3)
    );
}
