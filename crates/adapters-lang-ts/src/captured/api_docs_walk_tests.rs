//! Declared API reference receipts are producer-keyed and established on
//! the loader's closure over the captured documents and config it read
//! (ADR-0072 slice 9, AC-1538).

use super::*;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const DOC: &str = "docs/api.md";
const CONFIG: &str = ".cartograph/api-docs.toml";

fn prov(start: u64, end: u64, fact: &str) -> serde_json::Value {
    let evidence = EvidenceRef {
        repo: REPO.into(),
        path: DOC.into(),
        byte_start: start,
        byte_end: end,
        commit_sha: "workdir".into(),
    };
    serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![evidence],
            "t0.api-contract",
            fact.as_bytes(),
        )
        .unwrap(),
    )
    .unwrap()
}

/// Capture a declared Markdown reference, its config and a TS file, and
/// stage receipts for the reference's File, its route and their edge, as
/// the contract extractor emits them.
fn walk(root: &Path, heading: &str) -> (Vec<MintedReceipt>, Vec<String>) {
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join(".cartograph")).unwrap();
    let text = format!("# API\n\n{heading}\n");
    std::fs::write(root.join(DOC), &text).unwrap();
    std::fs::write(root.join(CONFIG), "[[doc]]\npath = \"docs/api.md\"\n").unwrap();
    std::fs::write(root.join("app.ts"), "export const a = 1;\n").unwrap();
    let paths: Vec<String> = [DOC, CONFIG, "app.ts"].map(String::from).to_vec();
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &paths,
        CaptureLimits::default(),
    )
    .unwrap();
    let start = text.find(heading).unwrap() as u64;
    let end = start + heading.len() as u64;
    let file_id = format!("file:{REPO}@{DOC}");
    let route_id = format!("doc-route:{REPO}@{DOC}#GET /orders");
    let nodes = vec![
        Node {
            id: file_id.clone(),
            label: "File".into(),
            props: serde_json::json!({
                "path": DOC,
                "prov": prov(0, text.len() as u64, &file_id),
            }),
        },
        Node {
            id: route_id.clone(),
            label: "DocumentedRoute".into(),
            props: serde_json::json!({
                "method": "GET",
                "path": "/orders",
                "doc_path": DOC,
                "prov": prov(start, end, &route_id),
            }),
        },
    ];
    let edges = vec![Edge {
        src: route_id.clone(),
        dst: file_id,
        label: "DOCUMENTED_IN".into(),
        props: serde_json::json!({ "prov": prov(start, end, &route_id) }),
    }];
    let read: Vec<String> = [CONFIG, DOC].map(String::from).to_vec();
    let id = SourceId {
        repo: REPO,
        commit: "workdir",
    };
    let receipts = stage_api_doc_receipts(&capture, &read, &id, (&nodes, &edges)).into_receipts();
    (receipts, read)
}

// AC-1538: a declared API reference's route, File and DOCUMENTED_IN edge
// get contract-v4 ApiDocs receipts whatever lane the document's path
// selects, established on one closure: the capture's enumeration under
// `api-docs-walk-v1` and each captured document and config the loader
// read. A change to the document changes the closure.
#[test]
fn api_doc_receipts_are_established_on_the_loader_closure() {
    let directory = tempfile::tempdir().unwrap();
    let (before, _) = walk(directory.path(), "## GET /orders");
    assert_eq!(
        before.len(),
        3,
        "{:?}",
        before.iter().map(|r| r.id()).collect::<Vec<_>>()
    );
    let record = before[0].input_closure().expect("an established closure");
    for receipt in &before {
        assert!(
            receipt.id().starts_with("apidoc-primary-v2:"),
            "{}",
            receipt.id()
        );
        assert_eq!(receipt.input_closure(), Some(record));
        let full = receipt.to_receipt().unwrap();
        assert_eq!(full.grammar(), Grammar::ApiDocs);
        assert_eq!(Receipt::from_json(&full.to_json().unwrap()).unwrap(), full);
    }
    assert_eq!(record.parameters.walk_policy, API_DOCS_WALK_POLICY);
    assert_eq!(record.producer_contract, "t0.api-contract/declared-v2");
    for pin in ["toml@1.1.6", "serde_json@1.0.154"] {
        assert!(
            record.producer_packages.contains(&pin.to_string()),
            "{pin}: {:?}",
            record.producer_packages
        );
    }
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
    let expected = [
        format!("enumeration {API_DOCS_WALK_POLICY}"),
        format!("file {CONFIG}"),
        format!("file {DOC}"),
    ];
    assert_eq!(inputs, expected);
    let (after, _) = walk(directory.path(), "## GET  /orders");
    assert_ne!(
        after[0].input_closure().unwrap().closure_id,
        record.closure_id
    );
}

// AC-1538: the API reference grammar is chosen by the producer, never by a
// path, and records a closure state.
#[test]
fn the_api_docs_grammar_is_producer_keyed() {
    assert!(Grammar::ApiDocs.producer_keyed());
    assert!(Grammar::ApiDocs.has_v4());
    for path in [DOC, CONFIG, "openapi.json"] {
        assert_ne!(Grammar::for_path(path), Grammar::ApiDocs);
    }
}
