//! #718 / AC-0289 admission foundation only; no server acceptance is claimed.
use super::*;

fn session(tools: &[ReadTool]) -> Admission {
    Admission::new(tools.iter().copied(), NonZeroUsize::new(64).unwrap())
}
fn request(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}
fn init(id: Value) -> Value {
    request(
        id,
        "initialize",
        json!({"protocolVersion":"future", "capabilities":{}, "clientInfo":{"name":"test", "version":"1"}}),
    )
}
fn initialized() -> Value {
    json!({"jsonrpc":"2.0", "method":"notifications/initialized"})
}
fn ready(tools: &[ReadTool]) -> Admission {
    let mut admission = session(tools);
    assert!(matches!(
        admission.receive(init(json!(0))),
        Action::Respond(_)
    ));
    assert_eq!(admission.receive(initialized()), Action::Ignore);
    admission
}
fn code(action: Action) -> i64 {
    let Action::Respond(response) = action else {
        panic!("expected response")
    };
    response["error"]["code"].as_i64().unwrap()
}

#[test]
fn lifecycle_requires_both_steps_and_negotiates_supported_version() {
    let mut admission = session(&[ReadTool::Discover]);
    assert_eq!(admission.receive(initialized()), Action::Ignore);
    assert_eq!(
        code(admission.receive(request(json!(1), "tools/list", json!({})))),
        -32600
    );
    let Action::Respond(response) = admission.receive(init(json!(2))) else {
        panic!()
    };
    assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(response["result"]["capabilities"], json!({"tools":{}}));
    assert_eq!(
        code(admission.receive(request(
            json!(3),
            "tools/call",
            json!({"name":"cartograph_discover"})
        ))),
        -32600
    );
    assert_eq!(admission.receive(initialized()), Action::Ignore);
    assert_eq!(
        admission.receive(request(json!(4), "tools/list", json!({}))),
        Action::ListTools {
            id: json!(4),
            tools: vec![ReadTool::Discover]
        }
    );
    assert_eq!(code(admission.receive(init(json!(5)))), -32600);
}

#[test]
fn configured_tools_only_and_closed_dispatch() {
    let mut admission = ready(&[ReadTool::Evidence]);
    assert_eq!(
        admission.receive(request(
            json!(1),
            "tools/call",
            json!({"name":"cartograph_evidence", "arguments":{"reference":"untrusted"}})
        )),
        Action::Call {
            id: json!(1),
            tool: ReadTool::Evidence,
            arguments: json!({"reference":"untrusted"})
                .as_object()
                .unwrap()
                .clone()
        }
    );
    for (id, name) in [
        "cartograph_discover",
        "start_investigation",
        "clear_graph",
        "arbitrary",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            code(admission.receive(request(json!(id + 2), "tools/call", json!({"name":name})))),
            -32602
        );
    }
    assert_eq!(
        code(admission.receive(request(json!(8), "clear_graph", json!({})))),
        -32601
    );
    let mut empty = session(&[]);
    let Action::Respond(response) = empty.receive(init(json!(0))) else {
        panic!()
    };
    assert_eq!(response["result"]["capabilities"], json!({}));
    empty.receive(initialized());
    assert_eq!(
        code(empty.receive(request(json!(1), "tools/list", json!({})))),
        -32601
    );
}

#[test]
fn notifications_never_dispatch_or_reply() {
    let mut admission = ready(&[ReadTool::Evidence]);
    for method in [
        "tools/call",
        "tools/list",
        "initialize",
        "ping",
        "clear_graph",
    ] {
        assert_eq!(
            admission.receive(
                json!({"jsonrpc":"2.0", "method":method, "params":{"name":"cartograph_evidence"}})
            ),
            Action::Ignore
        );
    }
    assert_eq!(
        admission.receive(json!({"method":"tools/call", "params":null, "result":{}})),
        Action::Ignore
    );
    assert_eq!(admission.receive(initialized()), Action::Ignore);
}

#[test]
fn invalid_envelopes_ids_and_params_are_rejected() {
    let mut admission = ready(&[ReadTool::Evidence]);
    for id in [
        Value::Null,
        json!(true),
        json!(1.5),
        json!(1.0),
        json!([]),
        json!({}),
    ] {
        assert_eq!(
            code(admission.receive(request(id, "ping", json!({})))),
            -32600
        );
    }
    for (index, field) in ["result", "error"].into_iter().enumerate() {
        let mut hybrid = request(json!(index + 1), "ping", json!({}));
        hybrid[field] = json!({});
        assert_eq!(code(admission.receive(hybrid)), -32600);
    }
    assert_eq!(code(admission.receive(json!([]))), -32600);
    assert_eq!(
        code(admission.receive(request(json!(3), "ping", json!([])))),
        -32602
    );
    assert_eq!(
        code(admission.receive(request(
            json!(4),
            "tools/call",
            json!({"name":"cartograph_evidence", "arguments":null})
        ))),
        -32602
    );
    let mut invalid = request(json!(5), "ping", json!({}));
    invalid["jsonrpc"] = json!("1.0");
    assert_eq!(code(admission.receive(invalid)), -32600);
}

#[test]
fn initialize_requires_client_metadata_and_capabilities() {
    for key in ["protocolVersion", "capabilities", "clientInfo"] {
        let mut admission = session(&[]);
        let mut message = init(json!(1));
        message["params"].as_object_mut().unwrap().remove(key);
        assert_eq!(code(admission.receive(message)), -32602);
    }
    for key in ["name", "version"] {
        let mut admission = session(&[]);
        let mut message = init(json!(1));
        message["params"]["clientInfo"][key] = json!(null);
        assert_eq!(code(admission.receive(message)), -32602);
    }
}

#[test]
fn id_history_is_bounded_unique_and_terminal() {
    let mut admission = Admission::new([], NonZeroUsize::new(2).unwrap());
    assert_eq!(
        admission.receive(request(json!(1), "ping", json!({}))),
        response(json!(1), json!({}))
    );
    assert_eq!(
        admission.receive(request(json!("1"), "ping", json!({}))),
        response(json!("1"), json!({}))
    );
    assert_eq!(
        admission.receive(request(json!(2), "ping", json!({}))),
        Action::Close(CloseReason::IdLimit)
    );
    assert_eq!(
        admission.receive(initialized()),
        Action::Close(CloseReason::IdLimit)
    );
    let mut admission = session(&[]);
    admission.receive(request(json!(-1), "unknown", json!({})));
    assert_eq!(
        admission.receive(request(json!(-1), "ping", json!({}))),
        Action::Close(CloseReason::ReusedId)
    );
    assert_eq!(
        admission.receive(init(json!(99))),
        Action::Close(CloseReason::ReusedId)
    );
}

#[test]
fn unsupported_task_and_cursor_are_not_silently_ignored() {
    let mut admission = ready(&[ReadTool::Evidence]);
    for (index, task) in [json!({}), json!({"ttl":10}), Value::Null]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            code(admission.receive(request(
                json!(index + 1),
                "tools/call",
                json!({"name":"cartograph_evidence", "task":task})
            ))),
            -32602
        );
    }
    assert_eq!(
        code(admission.receive(request(json!(4), "tools/list", json!({"cursor":"opaque"})))),
        -32602
    );
    assert_eq!(
        admission.receive(request(
            json!(5),
            "tools/call",
            json!({"name":"cartograph_evidence", "_meta":{"task":{},"method":"clear_graph"}})
        )),
        Action::Call {
            id: json!(5),
            tool: ReadTool::Evidence,
            arguments: Map::new()
        }
    );
}

#[test]
fn selected_fact_tool_requires_explicit_host_implementation() {
    let call = request(
        json!(1),
        "tools/call",
        json!({"name":"cartograph_read_fact","arguments":{}}),
    );
    assert_eq!(
        code(ready(&[ReadTool::Discover]).receive(call.clone())),
        -32602
    );
    assert!(matches!(
        ready(&[ReadTool::ReadFact]).receive(call),
        Action::Call {
            tool: ReadTool::ReadFact,
            ..
        }
    ));
}

// #773 (prerequisite to #766): configuring the new vocabulary never expands a read-only session.
#[test]
fn task_vocabulary_does_not_expand_read_only_admission() {
    use crate::tools::TaskTool;
    let mut admission = ready(&[ReadTool::Capabilities]);
    for (index, tool) in [
        TaskTool::Start,
        TaskTool::Status,
        TaskTool::Events,
        TaskTool::Result,
        TaskTool::Cancel,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            code(admission.receive(request(
                json!(index + 1),
                "tools/call",
                json!({"name":tool.name(),"arguments":{}})
            ))),
            -32602
        );
    }
    assert_eq!(
        admission.receive(request(json!(20), "tools/list", json!({}))),
        Action::ListTools {
            id: json!(20),
            tools: vec![ReadTool::Capabilities]
        }
    );
}

// #773: mixed tools share lifecycle and ID limits. This is protocol
// dispatch only; real handlers must still authenticate the activation grant.
#[test]
fn configured_tasks_share_read_lifecycle_and_request_identity_ledger() {
    use crate::tools::{TaskTool, Tool};
    let read = Tool::Read(ReadTool::Capabilities);
    let task = Tool::Task(TaskTool::Status);
    let mut admission = ProtocolAdmission::new([read, task], NonZeroUsize::new(16).unwrap());
    let before = admission.receive(request(
        json!(0),
        "tools/call",
        json!({"name":TaskTool::Status.name()}),
    ));
    assert!(matches!(before, Action::Respond(value) if value["error"]["code"] == -32600));
    assert!(matches!(
        admission.receive(init(json!(1))),
        Action::Respond(_)
    ));
    assert_eq!(admission.receive(initialized()), Action::Ignore);
    assert_eq!(
        admission.receive(request(json!(2), "tools/list", json!({}))),
        Action::ListTools {
            id: json!(2),
            tools: vec![read, task]
        }
    );
    assert_eq!(
        admission.receive(request(
            json!(3),
            "tools/call",
            json!({"name":TaskTool::Status.name(), "arguments":{"id":"owned"}})
        )),
        Action::Call {
            id: json!(3),
            tool: task,
            arguments: json!({"id":"owned"}).as_object().unwrap().clone()
        }
    );
    let unknown = admission.receive(request(
        json!(4),
        "tools/call",
        json!({"name":TaskTool::Start.name()}),
    ));
    assert!(matches!(unknown, Action::Respond(value) if value["error"]["code"] == -32602));
    let augmentation = admission.receive(request(
        json!(5),
        "tools/call",
        json!({"name":TaskTool::Status.name(),"task":{}}),
    ));
    assert!(matches!(augmentation, Action::Respond(value) if value["error"]["code"] == -32602));
    assert_eq!(
        admission.receive(request(
            json!(3),
            "tools/call",
            json!({"name":ReadTool::Capabilities.name()})
        )),
        Action::Close(CloseReason::ReusedId)
    );
}
