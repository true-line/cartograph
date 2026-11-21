//! AC-0932 (T-0932): a question about one area is answered in one bounded
//! call from the area's briefing.
use super::*;
use crate::area_briefing::tests::state;
use crate::section_compose::tests::Scripted;
use serde_json::json;
use std::sync::Mutex;

fn scripted(reply: serde_json::Value) -> Scripted {
    Scripted {
        reply: reply.to_string(),
        prompts: Mutex::new(Vec::new()),
    }
}

#[test]
fn a_question_is_answered_from_the_areas_briefing() {
    let (_dir, state) = state();
    assert_eq!(
        areas(&state)
            .unwrap()
            .iter()
            .map(|choice| (choice.name.as_str(), choice.files))
            .collect::<Vec<_>>(),
        [("Alias", 1), ("Mailbox", 1)]
    );
    // The digest names the briefing's text: the same read gives the same
    // one, and two areas' briefings differ.
    let (first, again) = (areas(&state).unwrap(), areas(&state).unwrap());
    assert_eq!(first, again);
    assert_ne!(first[0].briefing_digest, first[1].briefing_digest);
    let turns = [ExpertTurn {
        question: "What is this area?".into(),
        answer: "It manages aliases.".into(),
    }];
    let prepared = prepare(
        &state,
        "subsystem:cluster:alias",
        "Who may delete an alias?",
        &turns,
    )
    .unwrap();
    // This fixture's briefing prints no citation id, so a statement cannot
    // stand; what is not known and a named neighbour can.
    let provider = scripted(json!({
        "answer": [{"statement": "Any signed-in user may delete an alias.",
            "basis": "inferred", "citation_ids": ["F-00000000000000a1"]}],
        "not_known": ["The briefing holds no rule about deleting an alias."],
        "ask_instead": ["Mailbox", "Billing"],
    }));
    let asked = ask(&state, &provider, &prepared, None, &|| {
        CallDirective::Continue
    })
    .unwrap();
    // The scripted provider reports no token usage; the request is sized.
    assert_eq!(asked.usage.model, "scripted-model");
    assert_eq!(asked.usage.input_tokens, None);
    assert!(asked.usage.request_bytes > 0);
    let answer = asked.answer.unwrap();
    assert!(answer.statements.is_empty());
    assert_eq!(answer.left_out, 1);
    assert_eq!(
        answer.not_known,
        ["The briefing holds no rule about deleting an alias."]
    );
    assert_eq!(answer.ask_instead, ["Mailbox"]);
    // The expert read the briefing, the earlier turn and the question.
    let prompt = provider.prompts.lock().unwrap().remove(0);
    assert!(prompt.contains("<briefing>\n# Area briefing: Alias\n"));
    assert!(prompt.contains("- Calls into: Mailbox."));
    assert!(prompt.contains("Q: What is this area?\nA: It manages aliases.\n"));
    assert!(prompt.ends_with("<question>\nWho may delete an alias?\n</question>\n"));

    // A reply off the schema is a closed reason, not an error.
    let prose = Scripted {
        reply: "Here is what I found.".into(),
        prompts: Mutex::new(Vec::new()),
    };
    assert_eq!(
        ask(&state, &prose, &prepared, None, &|| CallDirective::Continue)
            .unwrap()
            .answer,
        Err("reply_not_the_schema")
    );
}

#[test]
fn a_blank_question_or_an_unknown_area_is_refused_before_any_call() {
    let (_dir, mut state) = state();
    for (area, question, reason) in [
        ("subsystem:cluster:alias", "  ", "question_not_admissible"),
        ("subsystem:cluster:missing", "What is it?", "area_not_found"),
    ] {
        assert_eq!(
            prepare(&state, area, question, &[]).err().as_deref(),
            Some(reason)
        );
    }
    // With no overlay there is no area to ask.
    state.knowledge_overlay = None;
    assert_eq!(areas(&state).unwrap(), []);
    assert_eq!(
        prepare(&state, "subsystem:cluster:alias", "What is it?", &[])
            .err()
            .as_deref(),
        Some("area_not_found")
    );
}

// A question names its area as the asker knows it; a run may name that area
// more narrowly, so the words asked find it.
#[test]
fn a_question_finds_its_area_by_the_words_of_its_name() {
    let choice = |name: &str, files| AreaChoice {
        area: format!("subsystem:{name}"),
        name: name.into(),
        files,
        sections: Vec::new(),
        briefing_bytes: 0,
        briefing_digest: String::new(),
    };
    let choices = [
        choice("Product variant", 40),
        choice("Product option", 12),
        choice("Tax", 9),
        choice("Custom domain", 5),
        choice("Alias", 30),
        choice("Status", 3),
    ];
    let name = |wanted| named(&choices, wanted).map(|choice| choice.name.as_str());
    assert_eq!(name("Tax"), Some("Tax"));
    assert_eq!(name("taxes"), Some("Tax"));
    assert_eq!(name("custom-domain"), Some("Custom domain"));
    assert_eq!(
        name("Product"),
        Some("Product variant"),
        "the larger of the two"
    );
    assert_eq!(name("Products"), Some("Product variant"));
    assert_eq!(name("Product options"), Some("Product option"));
    assert_eq!(name("Aliases"), Some("Alias"));
    assert_eq!(name("alias"), Some("Alias"));
    assert_eq!(name("Statuses"), Some("Status"));
    assert_eq!(name("Order"), None);
    assert_eq!(name("  "), None);
}
