//! Found-ADR receipts are producer-keyed: an ADR node is established on
//! the closure over the captured decision records the reader read, and a
//! `DECIDES` edge, which a recovered target decides, is not (ADR-0072
//! slice 9, AC-1540).

use super::*;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const RECORD: &str = "docs/adr/0001-orders.md";

fn prov(start: u64, end: u64, fact: &str) -> serde_json::Value {
    let evidence = EvidenceRef {
        repo: REPO.into(),
        path: RECORD.into(),
        byte_start: start,
        byte_end: end,
        commit_sha: "workdir".into(),
    };
    serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![evidence],
            "t0.adr-markdown",
            fact.as_bytes(),
        )
        .unwrap(),
    )
    .unwrap()
}

/// Capture a decision record and a TS file, and stage receipts for the
/// record's ADR node and a DECIDES edge, as the found-ADR reader emits them.
fn walk(root: &Path, title: &str) -> Vec<MintedReceipt> {
    std::fs::create_dir_all(root.join("docs/adr")).unwrap();
    let target = format!("file:{REPO}@app.ts");
    let text = format!("# {title}\n\nGoverns: `{target}`\n");
    std::fs::write(root.join(RECORD), &text).unwrap();
    std::fs::write(root.join("app.ts"), "export const a = 1;\n").unwrap();
    let paths: Vec<String> = [RECORD, "app.ts"].map(String::from).to_vec();
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &paths,
        CaptureLimits::default(),
    )
    .unwrap();
    let adr_id = format!("adr:{REPO}@{RECORD}");
    let start = text.find(&target).unwrap() as u64;
    let end = start + target.len() as u64;
    let nodes = vec![Node {
        id: adr_id.clone(),
        label: "ADR".into(),
        props: serde_json::json!({
            "title": title,
            "path": RECORD,
            "prov": prov(0, text.len() as u64, &adr_id),
        }),
    }];
    let edges = vec![Edge {
        src: adr_id.clone(),
        dst: target,
        label: "DECIDES".into(),
        props: serde_json::json!({ "prov": prov(start, end, &adr_id) }),
    }];
    let id = SourceId {
        repo: REPO,
        commit: "workdir",
    };
    stage_adr_receipts(&capture, &[RECORD.to_string()], &id, (&nodes, &edges)).into_receipts()
}

// AC-1540: a found ADR gets a contract-v4 Adr receipt whatever lane its
// path selects, established on the capture's enumeration under
// `adr-walk-v1` and each captured record read; its DECIDES edge is
// receipted `cross_file_input_unbound`. A change to the record changes the
// closure.
#[test]
fn adr_receipts_are_established_on_the_record_closure() {
    let directory = tempfile::tempdir().unwrap();
    let before = walk(directory.path(), "1. Orders");
    assert_eq!(before.len(), 2);
    for receipt in &before {
        assert!(
            receipt.id().starts_with("adr-primary-v2:"),
            "{}",
            receipt.id()
        );
        let full = receipt.to_receipt().unwrap();
        assert_eq!(full.grammar(), Grammar::Adr);
        assert_eq!(Receipt::from_json(&full.to_json().unwrap()).unwrap(), full);
    }
    let node = before
        .iter()
        .find(|receipt| matches!(receipt.fact_key(), core_graph::FactKey::Node { .. }))
        .unwrap();
    let record = node.input_closure().expect("an established closure");
    assert_eq!(record.parameters.walk_policy, ADR_WALK_POLICY);
    assert_eq!(record.producer_contract, "t0.adr-markdown/declared-v2");
    let mut inputs: Vec<String> = record
        .inputs
        .iter()
        .map(|input| match input {
            ClosureInput::CapturedFile { file } => format!("file {}", file.path),
            ClosureInput::Enumeration { walk_policy, .. } => format!("enumeration {walk_policy}"),
            other => panic!("unexpected input {other:?}"),
        })
        .collect();
    inputs.sort();
    assert_eq!(
        inputs,
        [
            format!("enumeration {ADR_WALK_POLICY}"),
            format!("file {RECORD}")
        ]
    );
    let edge = before
        .iter()
        .find(|receipt| matches!(receipt.fact_key(), core_graph::FactKey::Edge { .. }))
        .unwrap();
    assert_eq!(
        edge.closure(),
        Some(&ReceiptClosure::InputClosureNotEstablished {
            reasons: vec![ClosureGapReason::CrossFileInputUnbound]
        })
    );
    let after = walk(directory.path(), "1. Payments");
    let changed = after
        .iter()
        .find_map(|receipt| receipt.input_closure())
        .unwrap();
    assert_ne!(changed.closure_id, record.closure_id);
}

// AC-1540: the found-ADR grammar is chosen by the producer, never by a
// path, and records a closure state.
#[test]
fn the_adr_grammar_is_producer_keyed() {
    assert!(Grammar::Adr.producer_keyed());
    assert!(Grammar::Adr.has_v4());
    assert_ne!(Grammar::for_path(RECORD), Grammar::Adr);
}
