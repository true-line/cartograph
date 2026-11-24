//! Entry-point reader receipts are producer-keyed and established on the
//! reader's closure over the captured files it read, when the reader
//! reports its facts bound to them (ADR-0072 slice 9, AC-1534).

use super::*;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const WORKER: &str = "worker.py";
const PROCFILE: &str = "Procfile";

/// Capture a Python worker, the Procfile that starts it and a TS file,
/// read the entry points from the capture, and stage their receipts.
fn walk(root: &Path, process: &str) -> (Vec<MintedReceipt>, Vec<String>) {
    std::fs::write(
        root.join(WORKER),
        "if __name__ == \"__main__\":\n    run()\n",
    )
    .unwrap();
    std::fs::write(root.join(PROCFILE), format!("{process}: python {WORKER}\n")).unwrap();
    std::fs::write(root.join("app.ts"), "export const a = 1;\n").unwrap();
    let paths: Vec<String> = [WORKER, PROCFILE, "app.ts"].map(String::from).to_vec();
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &paths,
        CaptureLimits::default(),
    )
    .unwrap();
    let read = |path: &str| {
        capture
            .file(path)
            .map(|file| file.bytes().to_vec())
            .map_err(|_| std::io::Error::other("not captured"))
    };
    let none = std::collections::BTreeSet::new();
    let (facts, reads) = ingest::entry_points::extract_from_capture_reading(
        root,
        &paths,
        &read,
        &ingest::schedules::SourceId {
            repo: REPO,
            commit: "workdir",
        },
        &none,
        &none,
    )
    .unwrap();
    assert!(reads.bound, "{reads:?}");
    assert!(!facts.nodes.is_empty());
    let id = SourceId {
        repo: REPO,
        commit: "workdir",
    };
    let unbound = stage_entry_point_receipts(
        &capture,
        &reads.read,
        &id,
        (&facts.nodes, &facts.edges),
        false,
    )
    .into_receipts();
    assert!(!unbound.is_empty());
    for receipt in &unbound {
        assert_eq!(
            receipt.closure(),
            Some(&ReceiptClosure::InputClosureNotEstablished {
                reasons: vec![ClosureGapReason::CrossFileInputUnbound]
            })
        );
    }
    let receipts = stage_entry_point_receipts(
        &capture,
        &reads.read,
        &id,
        (&facts.nodes, &facts.edges),
        true,
    )
    .into_receipts();
    (receipts, reads.read)
}

// AC-1534: the entry-point reader's facts over a capture, reported bound,
// get contract-v4 EntryPoint receipts whatever lane the cited file's path
// selects, established on one closure: the capture's enumeration under
// `entry-point-walk-v1` and each captured file the reader read. A change
// to the Procfile changes the closure.
#[test]
fn entry_point_receipts_are_established_on_the_reader_closure() {
    let directory = tempfile::tempdir().unwrap();
    let (before, read) = walk(directory.path(), "worker");
    for path in [WORKER, PROCFILE] {
        assert!(read.contains(&path.to_string()), "{read:?}");
    }
    let worker: Vec<&MintedReceipt> = before
        .iter()
        .filter(|receipt| receipt.file().path == WORKER)
        .collect();
    assert!(!worker.is_empty());
    assert_eq!(Grammar::for_path(WORKER), Grammar::Python);
    let record = worker[0].input_closure().expect("an established closure");
    for receipt in &before {
        assert!(
            receipt.id().starts_with("entry-primary-v2:"),
            "{}",
            receipt.id()
        );
        assert_eq!(receipt.input_closure(), Some(record));
        let full = receipt.to_receipt().unwrap();
        assert_eq!(full.grammar(), Grammar::EntryPoint);
        assert_eq!(Receipt::from_json(&full.to_json().unwrap()).unwrap(), full);
    }
    assert_eq!(record.parameters.walk_policy, ENTRY_POINT_WALK_POLICY);
    assert_eq!(record.producer_contract, "t0.entry-point/declared-v2");
    // The closure pins every grammar the reader parses with.
    for pin in [
        "tree-sitter-python@0.25.0",
        "tree-sitter-typescript@0.23.2",
        "tree-sitter-java@0.23.5",
        "tree-sitter-kotlin-ng@1.1.0",
        "tree-sitter-rust@0.24.2",
        "tree-sitter-yaml@0.7.2",
        "tree-sitter@0.27.2",
    ] {
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
        format!("enumeration {ENTRY_POINT_WALK_POLICY}"),
        format!("file {PROCFILE}"),
        format!("file {WORKER}"),
    ];
    assert_eq!(inputs, expected);
    let (after, _) = walk(directory.path(), "jobs");
    let changed = after
        .iter()
        .find(|receipt| receipt.file().path == WORKER)
        .expect("the worker is still receipted");
    assert_ne!(
        changed.input_closure().unwrap().closure_id,
        record.closure_id
    );
}

// AC-1534: the entry-point grammar is chosen by the producer, never by a
// path, and records a closure state.
#[test]
fn the_entry_point_grammar_is_producer_keyed() {
    assert!(Grammar::EntryPoint.producer_keyed());
    assert!(Grammar::EntryPoint.has_v4());
    for path in [WORKER, PROCFILE, "Dockerfile", "main.go", "package.json"] {
        assert_ne!(Grammar::for_path(path), Grammar::EntryPoint);
    }
}
