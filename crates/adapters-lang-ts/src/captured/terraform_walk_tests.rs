//! Terraform receipts are established on the module walk's closure when the
//! walk read only captured `.tf` files (ADR-0072 slice 9, AC-1526).

use super::*;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const MAIN: &str = r#"resource "aws_sqs_queue" "jobs" {}
"#;

fn capture(root: &Path, network: &str) -> Capture {
    std::fs::write(root.join("main.tf"), MAIN).unwrap();
    std::fs::write(root.join("network.tf"), network).unwrap();
    std::fs::write(root.join("app.ts"), "export const a = 1;\n").unwrap();
    capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &["app.ts".into(), "main.tf".into(), "network.tf".into()],
        CaptureLimits::default(),
    )
    .unwrap()
}

fn resource() -> Node {
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: REPO.into(),
            path: "main.tf".into(),
            byte_start: 0,
            byte_end: MAIN.len() as u64 - 1,
            commit_sha: "workdir".into(),
        }],
        "t0.iac",
        b"fact",
    )
    .unwrap();
    Node {
        id: "tf:main.tf#aws_sqs_queue.jobs".into(),
        label: "Resource".into(),
        props: serde_json::json!({ "prov": provenance }),
    }
}

fn stage(capture: &Capture, walk: TerraformWalk) -> Vec<MintedReceipt> {
    let files = vec!["main.tf".to_string(), "network.tf".to_string()];
    stage_terraform_receipts(
        capture,
        &files,
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
        (&[resource()], &[]),
        walk,
    )
    .into_receipts()
}

// AC-1526: a bound walk's Terraform receipts are contract-v4 receipts
// established on one closure: the capture's enumeration under
// `tf-module-walk-v1` and every captured `.tf` file, no other source. A
// change to another `.tf` file changes the closure but not the fact.
#[test]
fn a_bound_terraform_walk_establishes_its_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let before = capture(directory.path(), "# network\n");
    let receipts = stage(&before, TerraformWalk::Bound);
    assert_eq!(receipts.len(), 1);
    let minted = &receipts[0];
    assert!(minted.id().starts_with("tf-primary-v2:"), "{}", minted.id());
    let record = minted.input_closure().expect("an established closure");
    assert_eq!(
        minted.closure(),
        Some(&ReceiptClosure::Established {
            closure_id: record.closure_id.clone()
        })
    );
    assert_eq!(record.parameters.walk_policy, TF_MODULE_WALK_POLICY);
    assert_eq!(record.producer_contract, "t0.adapter-iac/direct-lexical-v2");
    let inputs: Vec<String> = record
        .inputs
        .iter()
        .map(|input| match input {
            ClosureInput::CapturedFile { file } => format!("file {}", file.path),
            ClosureInput::Enumeration { walk_policy, .. } => format!("enumeration {walk_policy}"),
            other => panic!("unexpected input {other:?}"),
        })
        .collect();
    let mut inputs = inputs;
    inputs.sort();
    assert_eq!(
        inputs,
        [
            "enumeration tf-module-walk-v1",
            "file main.tf",
            "file network.tf"
        ]
    );
    let receipt = minted.to_receipt().unwrap();
    assert_eq!(
        Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
        receipt
    );
    assert!(receipt.matches_node(&resource()));
    let after = capture(directory.path(), "# network, renamed\n");
    let changed = stage(&after, TerraformWalk::Bound);
    assert_eq!(changed[0].fact_digest(), minted.fact_digest());
    assert_ne!(
        changed[0].input_closure().unwrap().closure_id,
        record.closure_id
    );
}

// AC-1526: a walk that read what the capture does not bind keeps its
// receipts unestablished under the host's reason, with no closure record.
#[test]
fn an_unbound_terraform_walk_keeps_its_reason() {
    let directory = tempfile::tempdir().unwrap();
    let capture = capture(directory.path(), "# network\n");
    for reason in [
        ClosureGapReason::CrossFileInputUnbound,
        ClosureGapReason::RemoteReference,
    ] {
        let receipts = stage(&capture, TerraformWalk::NotEstablished(reason));
        assert_eq!(receipts.len(), 1);
        assert!(receipts[0].id().starts_with("tf-primary-v2:"));
        assert_eq!(
            receipts[0].closure(),
            Some(&ReceiptClosure::InputClosureNotEstablished {
                reasons: vec![reason]
            })
        );
        assert!(receipts[0].input_closure().is_none());
        let receipt = receipts[0].to_receipt().unwrap();
        assert_eq!(
            Receipt::from_json(&receipt.to_json().unwrap()).unwrap(),
            receipt
        );
    }
}
