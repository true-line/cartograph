use super::*;
use serde_json::json;

fn parse(tool: TaskTool, value: Value) -> Result<TaskArguments, InvalidTaskArguments> {
    let Value::Object(arguments) = value else {
        panic!("test requires an envelope")
    };
    TaskArguments::parse(tool, arguments)
}

// AC-0320/0327: closed argument vocabulary is not authorization or domain validation.
#[test]
fn task_arguments_cover_all_operations_and_preserve_untrusted_start() {
    let request = json!({"question":"fixture", "nested":[1, {"untrusted":true}]});
    let expected = request.as_object().unwrap().clone();
    assert_eq!(
        parse(TaskTool::Start, json!({"request":request})),
        Ok(TaskArguments::Start { request: expected })
    );
    let id = "investigation:abc_0-1".to_string();
    for (tool, expected) in [
        (
            TaskTool::Status,
            TaskArguments::Status {
                task_id: id.clone(),
            },
        ),
        (
            TaskTool::Result,
            TaskArguments::Result {
                task_id: id.clone(),
            },
        ),
        (
            TaskTool::Cancel,
            TaskArguments::Cancel {
                task_id: id.clone(),
            },
        ),
    ] {
        assert_eq!(parse(tool, json!({"task_id":id})), Ok(expected));
    }
    for after in [0, u64::MAX] {
        assert_eq!(
            parse(TaskTool::Events, json!({"task_id":id,"after":after})),
            Ok(TaskArguments::Events {
                task_id: id.clone(),
                after
            })
        );
    }
}

// AC-0320/0326: no authority or consent field exists on a task envelope.
#[test]
fn task_arguments_reject_missing_extra_and_wrongly_typed_fields() {
    for tool in [
        TaskTool::Start,
        TaskTool::Status,
        TaskTool::Events,
        TaskTool::Result,
        TaskTool::Cancel,
        TaskTool::ProposeAttested,
    ] {
        let base = match tool {
            TaskTool::Start => json!({"request":{}}),
            TaskTool::Events => json!({"task_id":"task:1","after":0}),
            TaskTool::ProposeAttested => {
                json!({"task_id":"task:1","finding_id":"f","gap_id":"gap:1","slot":"owner"})
            }
            _ => json!({"task_id":"task:1"}),
        };
        assert_eq!(parse(tool, json!({})), Err(InvalidTaskArguments));
        for field in [
            "client_id",
            "origin",
            "grant",
            "consent",
            "provider",
            "family",
        ] {
            let mut forged = base.clone();
            forged[field] = json!(true);
            assert_eq!(parse(tool, forged), Err(InvalidTaskArguments));
        }
    }
    for request in [Value::Null, json!(true), json!("secret"), json!([])] {
        assert_eq!(
            parse(TaskTool::Start, json!({"request":request})),
            Err(InvalidTaskArguments)
        );
    }
    for after in [
        json!(-1),
        json!(1.5),
        json!("1"),
        Value::Null,
        json!(true),
        json!([]),
    ] {
        assert_eq!(
            parse(TaskTool::Events, json!({"task_id":"task:1","after":after})),
            Err(InvalidTaskArguments)
        );
    }
    assert_eq!(
        parse(TaskTool::Status, json!({"task_id":42})),
        Err(InvalidTaskArguments)
    );
    assert_eq!(InvalidTaskArguments.to_string(), "invalid task arguments");
}

// AC-0327: IDs use byte bounds and the host's opaque-ID alphabet.
#[test]
fn task_argument_id_boundaries_are_exact() {
    assert!(parse(TaskTool::Status, json!({"task_id":"a".repeat(128)})).is_ok());
    for id in [
        String::new(),
        "a".repeat(129),
        "é".repeat(64),
        "task:one\n".into(),
        "task:one two".into(),
        "../task".into(),
    ] {
        assert_eq!(
            parse(TaskTool::Status, json!({"task_id":id})),
            Err(InvalidTaskArguments)
        );
    }
}

// AC-1079 (ADR-0090 decision 9): a proposal names a task, a finding and a
// Gap slot. No verb, contributor or confirmation is accepted.
#[test]
fn an_attested_proposal_names_a_finding_and_a_gap_slot_and_nothing_else() {
    let base = json!({"task_id":"task:1","finding_id":"finding-1","gap_id":"gap:login-owner","slot":"owner"});
    assert_eq!(
        parse(TaskTool::ProposeAttested, base.clone()),
        Ok(TaskArguments::ProposeAttested {
            task_id: "task:1".into(),
            finding_id: "finding-1".into(),
            gap_id: "gap:login-owner".into(),
            slot: "owner".into(),
        })
    );
    assert_eq!(
        TaskTool::ProposeAttested.name(),
        "cartograph_task_propose_attested"
    );
    for field in ["verb", "confirm", "contributor", "engineer", "statement"] {
        let mut forged = base.clone();
        forged[field] = json!("confirmed");
        assert_eq!(
            parse(TaskTool::ProposeAttested, forged),
            Err(InvalidTaskArguments)
        );
    }
    for field in ["finding_id", "gap_id", "slot"] {
        for value in [
            json!(""),
            json!(7),
            Value::Null,
            json!("line\nbreak"),
            json!("a".repeat(MAX_ATTESTED_REFERENCE_BYTES + 1)),
        ] {
            let mut bad = base.clone();
            bad[field] = value;
            assert_eq!(
                parse(TaskTool::ProposeAttested, bad),
                Err(InvalidTaskArguments)
            );
        }
        let mut longest = base.clone();
        longest[field] = json!("a".repeat(MAX_ATTESTED_REFERENCE_BYTES));
        assert!(parse(TaskTool::ProposeAttested, longest).is_ok());
    }
    let mut bad_task = base;
    bad_task["task_id"] = json!("../task");
    assert_eq!(
        parse(TaskTool::ProposeAttested, bad_task),
        Err(InvalidTaskArguments)
    );
}
