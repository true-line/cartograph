//! Source basis schema 3 and staging v3 carry the input closure state of the
//! receipt each captured range was read from (ADR-0072 slice 5, AC-1086).

use super::*;
use crate::prepared::tests::{fixture, result};
use crate::{AgentError, PreparedAgentTask, TaskClosureGap, TaskInputClosure};
use serde_json::json;

const CLOSURE: &str = "closure-v1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// `task`'s basis read from a contract-v4 receipt at `schema`, its captured
/// origin carrying `closure`.
fn read_from_v4(
    task: &PreparedAgentTask,
    schema: u32,
    closure: TaskInputClosure,
) -> TaskSourceBasisV2 {
    read_from(task, schema, closure, "ts-primary-v4:")
}

/// `task`'s basis read from a receipt whose id starts with `prefix`.
fn read_from(
    task: &PreparedAgentTask,
    schema: u32,
    closure: TaskInputClosure,
    prefix: &str,
) -> TaskSourceBasisV2 {
    let mut source = task.source_basis().clone();
    source.schema_version = schema;
    let receipt = format!("{prefix}{}", "4".repeat(64));
    for fact in &mut source.selected_facts {
        if let Some(binding) = &mut fact.binding {
            binding.receipt_id = receipt.clone();
        }
    }
    let TaskEvidenceKind::CapturedPrimarySource {
        receipt_id,
        input_closure,
        ..
    } = &mut source.evidence[0].origin
    else {
        panic!("captured origin");
    };
    *receipt_id = receipt;
    *input_closure = closure;
    source
}

fn established() -> TaskInputClosure {
    TaskInputClosure::Established {
        closure_id: CLOSURE.into(),
    }
}

fn prepared(
    task: &PreparedAgentTask,
    source: TaskSourceBasisV2,
) -> Result<PreparedAgentTask, AgentError> {
    PreparedAgentTask::new(task.task().clone(), source)
}

fn origin_closure(source: &TaskSourceBasisV2) -> serde_json::Value {
    serde_json::to_value(source).unwrap()["evidence"][0]["origin"]["input_closure"].clone()
}

// AC-1086: schema 2 keeps its exact wire form and fingerprint domain; schema
// 3 records a v4 receipt's state tagged by `state`, as the receipt does.
#[test]
fn schema_three_copies_the_receipts_closure_state_and_schema_two_stays_legacy() {
    let task = fixture();
    assert_eq!(
        origin_closure(task.source_basis()),
        json!("input_closure_not_established")
    );
    let v2_bytes = serde_json::to_vec(task.source_basis()).unwrap();
    assert_eq!(
        task.source_basis().fingerprint().unwrap(),
        crate::source_basis::domain_hash(b"cartograph:task-source-basis:v2\0", &v2_bytes)
    );

    let v3 = prepared(&task, read_from_v4(&task, 3, established())).unwrap();
    assert_eq!(
        origin_closure(v3.source_basis()),
        json!({"state": "established", "closure_id": CLOSURE})
    );
    let v3_bytes = serde_json::to_vec(v3.source_basis()).unwrap();
    assert_eq!(
        v3.source_basis().fingerprint().unwrap(),
        crate::source_basis::domain_hash(b"cartograph:task-source-basis:v3\0", &v3_bytes)
    );
    assert_ne!(v3.basis_hash().unwrap(), task.basis_hash().unwrap());

    let gaps = TaskInputClosure::NotEstablished {
        reasons: vec![
            TaskClosureGap::CrossFileInputUnbound,
            TaskClosureGap::CacheReuseUnproven,
        ]
        .into(),
    };
    let gapped = prepared(&task, read_from_v4(&task, 3, gaps.clone())).unwrap();
    assert_eq!(
        origin_closure(gapped.source_basis()),
        json!({
            "state": "input_closure_not_established",
            "reasons": ["cross_file_input_unbound", "cache_reuse_unproven"]
        })
    );
    // The wire form decodes back to the same state.
    let decoded: TaskSourceBasisV2 =
        serde_json::from_slice(&serde_json::to_vec(gapped.source_basis()).unwrap()).unwrap();
    assert_eq!(&decoded, gapped.source_basis());
}

// AC-1086: a closure state is accepted only where the receipt read records
// one: never in schema 2, never for a pre-v4 receipt, and never malformed.
#[test]
fn a_closure_state_the_read_receipt_cannot_carry_is_refused() {
    let task = fixture();
    // Schema 2 records only the legacy state, even for a v4 receipt.
    assert!(prepared(&task, read_from_v4(&task, 2, TaskInputClosure::LEGACY)).is_ok());
    assert!(prepared(&task, read_from_v4(&task, 2, established())).is_err());
    // Schema 3 requires a v4 receipt's own state.
    assert!(prepared(&task, read_from_v4(&task, 3, TaskInputClosure::LEGACY)).is_err());
    for bad in [
        TaskInputClosure::Established {
            closure_id: "closure-v1:short".into(),
        },
        TaskInputClosure::Established {
            closure_id: format!("closure-v2:{}", "0".repeat(64)).into(),
        },
        TaskInputClosure::NotEstablished {
            reasons: Box::new([]),
        },
        TaskInputClosure::NotEstablished {
            reasons: Box::new([
                TaskClosureGap::CacheReuseUnproven,
                TaskClosureGap::CrossFileInputUnbound,
            ]),
        },
        TaskInputClosure::NotEstablished {
            reasons: Box::new([TaskClosureGap::LimitExceeded, TaskClosureGap::LimitExceeded]),
        },
    ] {
        assert!(prepared(&task, read_from_v4(&task, 3, bad)).is_err());
    }
    // A pre-v4 receipt in schema 3 keeps the legacy state.
    let mut legacy = task.source_basis().clone();
    legacy.schema_version = 3;
    assert!(prepared(&task, legacy.clone()).is_ok());
    if let TaskEvidenceKind::CapturedPrimarySource { input_closure, .. } =
        &mut legacy.evidence[0].origin
    {
        *input_closure = established();
    }
    assert!(prepared(&task, legacy).is_err());
    // Unknown schemas and unknown tagged fields are refused.
    let mut unknown = task.source_basis().clone();
    unknown.schema_version = 4;
    assert!(prepared(&task, unknown).is_err());
    let mut wire = serde_json::to_value(read_from_v4(&task, 3, established())).unwrap();
    wire["evidence"][0]["origin"]["input_closure"]["extra"] = json!(1);
    assert!(serde_json::from_value::<TaskSourceBasisV2>(wire).is_err());
}

// AC-1086: a task prepared on a schema-3 basis stages as v3 content under its
// own identity domain, decodes from the store, and v2 staging is unchanged.
#[test]
fn a_schema_three_task_stages_as_v3_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stages.sqlite");
    let mut store = ProposalStore::open(&path).unwrap();
    let task = fixture();
    let stage = |store: &mut ProposalStore, task: &PreparedAgentTask| {
        store
            .stage_prepared(
                task,
                &result(task, "Possible target link", &["source", "target"]),
                1,
                &task.source_basis().graph_snapshot_id,
            )
            .unwrap()
    };
    let v2 = stage(&mut store, &task);
    assert_eq!(v2.schema_version, 2);
    assert!(v2.proposal_id.starts_with("proposal-stage-v2:"));

    let v3_task = prepared(&task, read_from_v4(&task, 3, established())).unwrap();
    let v3 = stage(&mut store, &v3_task);
    assert_eq!(v3.schema_version, 3);
    assert!(v3.proposal_id.starts_with("proposal-stage-v3:"));
    assert_eq!(v3.source_basis.as_ref(), Some(v3_task.source_basis()));
    // The record crosses the host boundary and back, and names a basis of its
    // own schema.
    let wire = serde_json::to_value(&v3).unwrap();
    assert_eq!(
        serde_json::from_value::<StagedProposal>(wire.clone()).unwrap(),
        v3
    );
    let mut mismatched = wire;
    mismatched["source_basis"]["schema_version"] = json!(2);
    assert!(serde_json::from_value::<StagedProposal>(mismatched).is_err());
    let raw: String = store
        .conn
        .query_row(
            "SELECT immutable_json FROM staged_agent_proposals WHERE proposal_id=?1",
            [&v3.proposal_id],
            |r| r.get(0),
        )
        .unwrap();
    let content: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(content["schema_version"], json!(3));
    assert_eq!(
        content["source_basis"]["evidence"][0]["origin"]["input_closure"],
        json!({"state": "established", "closure_id": CLOSURE})
    );
    drop(store);
    let store = ProposalStore::open(&path).unwrap();
    assert_eq!(store.get(&v3.proposal_id).unwrap(), Some(v3.clone()));
    assert_eq!(store.get(&v2.proposal_id).unwrap(), Some(v2));

    // A v3 envelope over a schema-2 basis, or the reverse, never decodes.
    let mut mislabeled = content.clone();
    mislabeled["schema_version"] = json!(2);
    assert!(ImmutableContent::decode(&mislabeled.to_string()).is_err());
    assert!(ImmutableContent::decode(&raw).is_ok());
}

// AC-1526: a Terraform contract-v4 receipt (`tf-primary-v2:`) carries its
// closure state into a schema-3 basis as a TypeScript v4 receipt does; a
// v3 lane receipt still carries only the legacy state.
#[test]
fn a_terraform_v4_receipt_carries_its_closure_state() {
    let task = fixture();
    // AC-1528: a pipeline v4 receipt (`pipe-primary-v2:`) does too, and
    // AC-1530: an environment v4 receipt (`env-primary-v2:`), and AC-1532:
    // a schedule v4 receipt (`sched-primary-v2:`).
    for prefix in [
        "tf-primary-v2:",
        "pipe-primary-v2:",
        "env-primary-v2:",
        "sched-primary-v2:",
    ] {
        let v3 = prepared(&task, read_from(&task, 3, established(), prefix)).unwrap();
        assert_eq!(
            origin_closure(v3.source_basis()),
            json!({"state": "established", "closure_id": CLOSURE})
        );
    }
    assert!(prepared(&task, read_from(&task, 3, established(), "tf-primary-v1:")).is_err());
}
