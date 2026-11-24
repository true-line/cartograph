//! Toolchain reader receipts are producer-keyed and established on the
//! reader's closure over the captured config files it read, when the reader
//! reports its facts bound to them (ADR-0072 slice 9, AC-1536).

use super::*;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const MANIFEST: &str = "package.json";
const DOCKERFILE: &str = "Dockerfile";

/// Capture a package manifest, a Dockerfile and a TS file, read the
/// toolchain from the capture, and stage its receipts.
fn walk(root: &Path, name: &str) -> (Vec<MintedReceipt>, Vec<String>) {
    std::fs::write(
        root.join(MANIFEST),
        format!("{{\"name\": \"{name}\", \"dependencies\": {{\"react\": \"18\"}}}}\n"),
    )
    .unwrap();
    std::fs::write(root.join(DOCKERFILE), "FROM node:20\n").unwrap();
    std::fs::write(root.join("app.ts"), "export const a = 1;\n").unwrap();
    let paths: Vec<String> = [MANIFEST, DOCKERFILE, "app.ts"].map(String::from).to_vec();
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
    let selected: Vec<String> = paths
        .iter()
        .filter(|path| ingest::toolchain::is_toolchain_file(path))
        .cloned()
        .collect();
    let (facts, reads) = ingest::toolchain::extract_from_capture_reading(
        root,
        &selected,
        &read,
        &ingest::toolchain::SourceId {
            repo: REPO,
            commit: "workdir",
        },
        &std::collections::BTreeSet::new(),
        &mut |_| {},
    )
    .unwrap();
    assert!(reads.bound, "{reads:?}");
    assert!(!facts.nodes.is_empty());
    let id = SourceId {
        repo: REPO,
        commit: "workdir",
    };
    let unbound = stage_toolchain_receipts(
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
    let receipts = stage_toolchain_receipts(
        &capture,
        &reads.read,
        &id,
        (&facts.nodes, &facts.edges),
        true,
    )
    .into_receipts();
    (receipts, reads.read)
}

// AC-1536: the toolchain reader's facts over a capture, reported bound, get
// contract-v4 Toolchain receipts whatever lane the cited file's path
// selects, established on one closure: the capture's enumeration under
// `toolchain-walk-v1` and each captured config file the reader read. A
// change to the manifest changes the closure.
#[test]
fn toolchain_receipts_are_established_on_the_reader_closure() {
    let directory = tempfile::tempdir().unwrap();
    let (before, read) = walk(directory.path(), "web");
    for path in [MANIFEST, DOCKERFILE] {
        assert!(read.contains(&path.to_string()), "{read:?}");
    }
    let docker: Vec<&MintedReceipt> = before
        .iter()
        .filter(|receipt| receipt.file().path == DOCKERFILE)
        .collect();
    assert!(!docker.is_empty());
    assert_eq!(Grammar::for_path(DOCKERFILE), Grammar::Pipeline);
    let record = docker[0].input_closure().expect("an established closure");
    for receipt in &before {
        assert!(
            receipt.id().starts_with("tool-primary-v2:"),
            "{}",
            receipt.id()
        );
        assert_eq!(receipt.input_closure(), Some(record));
        let full = receipt.to_receipt().unwrap();
        assert_eq!(full.grammar(), Grammar::Toolchain);
        assert_eq!(Receipt::from_json(&full.to_json().unwrap()).unwrap(), full);
    }
    assert_eq!(record.parameters.walk_policy, TOOLCHAIN_WALK_POLICY);
    assert_eq!(record.producer_contract, "t0.toolchain/declared-v2");
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
        format!("enumeration {TOOLCHAIN_WALK_POLICY}"),
        format!("file {DOCKERFILE}"),
        format!("file {MANIFEST}"),
    ];
    assert_eq!(inputs, expected);
    let (after, _) = walk(directory.path(), "api");
    let changed = after
        .iter()
        .find(|receipt| receipt.file().path == DOCKERFILE)
        .expect("the Dockerfile is still receipted");
    assert_ne!(
        changed.input_closure().unwrap().closure_id,
        record.closure_id
    );
}

// AC-1536: the toolchain grammar is chosen by the producer, never by a
// path, and records a closure state.
#[test]
fn the_toolchain_grammar_is_producer_keyed() {
    assert!(Grammar::Toolchain.producer_keyed());
    assert!(Grammar::Toolchain.has_v4());
    for path in [MANIFEST, DOCKERFILE, "main.tf", "go.mod", "yarn.lock"] {
        assert_ne!(Grammar::for_path(path), Grammar::Toolchain);
    }
}

// AC-1536: a tool proven by a file's name alone (a pip requirements file,
// a Maven POM, a lockfile) cites the whole captured file, so its `Tool`,
// `DEFINED_IN` edge and `File` all get established receipts.
#[test]
fn presence_only_tools_are_receipted_on_the_whole_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let paths: Vec<String> = ["requirements.txt", "pom.xml", "yarn.lock"]
        .map(String::from)
        .to_vec();
    for path in &paths {
        std::fs::write(root.join(path), "content\n").unwrap();
    }
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
    let (facts, reads) = ingest::toolchain::extract_from_capture_reading(
        root,
        &paths,
        &read,
        &ingest::toolchain::SourceId {
            repo: REPO,
            commit: "workdir",
        },
        &std::collections::BTreeSet::new(),
        &mut |_| {},
    )
    .unwrap();
    let mut read_paths = reads.read.clone();
    read_paths.sort();
    let mut expected = paths.clone();
    expected.sort();
    assert_eq!(read_paths, expected);
    let id = SourceId {
        repo: REPO,
        commit: "workdir",
    };
    let receipts = stage_toolchain_receipts(
        &capture,
        &reads.read,
        &id,
        (&facts.nodes, &facts.edges),
        reads.bound,
    )
    .into_receipts();
    for node in &facts.nodes {
        assert!(
            receipts.iter().any(|receipt| receipt.matches_node(node)),
            "{} has no receipt",
            node.id
        );
    }
    for edge in &facts.edges {
        assert!(
            receipts.iter().any(|receipt| receipt.matches_edge(edge)),
            "{} -> {} has no receipt",
            edge.src,
            edge.dst
        );
    }
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt.input_closure().is_some())
    );
}
