// AC-1428 (ADR-0085): the host hands each authorized step to the runtime as a
// start or as a tool result carrying only what changed.
use super::*;
use crate::investigation::response_schema::CHOICE;
use serde_json::json;

const CONTEXT: &str = "context-v2:0000000000000000000000000000000000000000000000000000000000000000";

fn span(id: &str, text: &str) -> PayloadSpan {
    PayloadSpan {
        id: id.into(),
        repo: "repo".into(),
        path: "src/a.rs".into(),
        byte_start: 0,
        byte_end: text.len() as u64,
        commit_sha: "c".repeat(40),
        text: text.into(),
    }
}

fn payload(prompt: Value, spans: Vec<PayloadSpan>) -> CompletionPayload {
    CompletionPayload {
        response_schema: Some(CHOICE.into()),
        system: "You are a bounded Cartograph investigation specialist.".into(),
        prompt: prompt.to_string(),
        spans,
    }
}

fn first_prompt() -> Value {
    json!({"schema_version": 1, "context_id": CONTEXT, "question": "How does checkout work?",
        "turn_progress": {"turn": 1}, "limits": {"max_turns": 12}})
}

#[test]
fn the_first_step_starts_the_runtime_with_the_full_authorized_payload() {
    let mut bridge = BridgeConversation::new();
    let first = payload(first_prompt(), vec![span("s1", "fn a() {}")]);
    let BridgeTurn::Start {
        system,
        prompt,
        tools,
        reason,
    } = bridge.next_turn(&first, 1 << 20).unwrap()
    else {
        panic!("first step must start the runtime");
    };
    assert_eq!(reason, StartReason::First);
    assert_eq!(system, first.system);
    assert!(prompt.starts_with(&first.prompt));
    assert!(prompt.contains("\n\nEvidence spans (JSON):\n") && prompt.contains("fn a() {}"));
    assert_eq!(tools, tool_definitions_from_schema(CHOICE).unwrap());
    assert_eq!(bridge.conversation_bytes(), system.len() + prompt.len());
}

#[test]
fn a_later_step_sends_only_changed_fields_and_new_spans() {
    let mut bridge = BridgeConversation::new();
    bridge
        .next_turn(
            &payload(first_prompt(), vec![span("s1", "fn a() {}")]),
            1 << 20,
        )
        .unwrap();
    let mut next = first_prompt();
    next["turn_progress"] = json!({"turn": 2});
    next["consumed_budgets"] = json!({"queries": 1});
    let before = bridge.conversation_bytes();
    let BridgeTurn::ToolResult { content } = bridge
        .next_turn(
            &payload(next, vec![span("s1", "fn a() {}"), span("s2", "fn b() {}")]),
            1 << 20,
        )
        .unwrap()
    else {
        panic!("a later step is a tool result");
    };
    let (delta, added) = content
        .split_once("\n\nNew evidence spans (JSON):\n")
        .unwrap();
    let delta: Value = serde_json::from_str(delta).unwrap();
    assert_eq!(
        delta,
        json!({"changed": {"turn_progress": {"turn": 2}, "consumed_budgets": {"queries": 1}}})
    );
    assert!(
        !content.contains("How does checkout work?"),
        "unchanged fields are not resent"
    );
    let added: Value = serde_json::from_str(added).unwrap();
    assert_eq!(added.as_array().unwrap().len(), 1);
    assert_eq!(added[0]["id"], "s2");
    assert_eq!(bridge.conversation_bytes(), before + content.len());
}

#[test]
fn removed_fields_and_dropped_spans_are_named() {
    let mut bridge = BridgeConversation::new();
    bridge
        .next_turn(
            &payload(first_prompt(), vec![span("s1", "fn a() {}")]),
            1 << 20,
        )
        .unwrap();
    let mut next = first_prompt();
    next.as_object_mut().unwrap().remove("limits");
    let BridgeTurn::ToolResult { content } =
        bridge.next_turn(&payload(next, vec![]), 1 << 20).unwrap()
    else {
        panic!("tool result");
    };
    let delta: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        delta,
        json!({"changed": {}, "removed": ["limits"], "dropped_spans": ["s1"]})
    );
}

#[test]
fn changed_tools_system_or_prompt_shape_restart_from_the_full_payload() {
    let mut bridge = BridgeConversation::new();
    bridge
        .next_turn(&payload(first_prompt(), vec![]), 1 << 20)
        .unwrap();

    let mut narrowed: Value = serde_json::from_str(CHOICE).unwrap();
    narrowed["anyOf"].as_array_mut().unwrap().remove(0);
    let mut fewer = payload(first_prompt(), vec![]);
    fewer.response_schema = Some(narrowed.to_string());
    assert!(matches!(
        bridge.next_turn(&fewer, 1 << 20).unwrap(),
        BridgeTurn::Start {
            reason: StartReason::ToolsChanged,
            ..
        }
    ));
    assert_eq!(
        bridge.offered().len(),
        tool_definitions_from_schema(CHOICE).unwrap().len() - 1
    );

    let mut other_system = payload(first_prompt(), vec![]);
    other_system.system = "Another role.".into();
    assert!(matches!(
        bridge.next_turn(&other_system, 1 << 20).unwrap(),
        BridgeTurn::Start {
            reason: StartReason::SystemChanged,
            ..
        }
    ));

    let mut text_prompt = payload(first_prompt(), vec![]);
    text_prompt.system = "Another role.".into();
    text_prompt.prompt = "plain text".into();
    assert!(matches!(
        bridge.next_turn(&text_prompt, 1 << 20).unwrap(),
        BridgeTurn::Start {
            reason: StartReason::PromptNotObject,
            ..
        }
    ));
}

#[test]
fn a_conversation_past_its_budget_restarts_and_resets_its_size() {
    let mut bridge = BridgeConversation::new();
    let first = payload(first_prompt(), vec![]);
    let BridgeTurn::Start { system, prompt, .. } = bridge.next_turn(&first, 1 << 20).unwrap()
    else {
        panic!()
    };
    let start_bytes = system.len() + prompt.len();
    bridge.record_model_output(500);
    let mut next = first_prompt();
    next["turn_progress"] = json!({"turn": 2});
    let budget = start_bytes + 500 + 10;
    let BridgeTurn::Start { reason, prompt, .. } = bridge
        .next_turn(&payload(next.clone(), vec![]), budget)
        .unwrap()
    else {
        panic!("over budget restarts");
    };
    assert_eq!(reason, StartReason::Budget);
    assert!(prompt.starts_with(&next.to_string()));
    assert_eq!(bridge.conversation_bytes(), system.len() + prompt.len());
}

#[test]
fn a_tool_call_becomes_action_bytes_with_the_current_context_id() {
    let mut bridge = BridgeConversation::new();
    bridge
        .next_turn(&payload(first_prompt(), vec![]), 1 << 20)
        .unwrap();
    let text = bridge
        .action_text(
            "query_context",
            &json!({"query": {"scope": {"type": "all"}, "kind": null, "labels": [],
            "max_facts": 12, "max_bytes": 16384, "cursor": null}}),
        )
        .unwrap();
    let action: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(action["type"], "query_context");
    assert_eq!(action["context_id"], CONTEXT);
    assert_eq!(
        bridge.action_text("bash", &json!({})).unwrap_err().code,
        "unknown_tool"
    );
}
