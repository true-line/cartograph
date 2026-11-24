//! Pipeline reader receipts are established on the reader's closure over
//! the captured workflow files and Dockerfiles (ADR-0072 slice 9, AC-1528).

use super::*;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const WORKFLOW: &str = ".github/workflows/ci.yml";
const BUILD: &str =
    "on: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: make\n";

/// Capture a workflow, a Dockerfile and a TS file, read the pipelines from
/// the capture, and stage their receipts.
fn walk(root: &Path, dockerfile: &str) -> Vec<MintedReceipt> {
    std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
    std::fs::write(root.join(WORKFLOW), BUILD).unwrap();
    std::fs::write(root.join("Dockerfile"), dockerfile).unwrap();
    std::fs::write(root.join("app.ts"), "export const a = 1;\n").unwrap();
    let paths: Vec<String> = [WORKFLOW, "Dockerfile", "app.ts"]
        .map(String::from)
        .to_vec();
    let capture = capture_working_tree(
        root,
        &CaptureSourceId::new(SOURCE).unwrap(),
        &paths,
        CaptureLimits::default(),
    )
    .unwrap();
    let files: Vec<String> = paths
        .into_iter()
        .filter(|path| ingest::pipelines::is_pipeline_file(path))
        .collect();
    let read = |path: &str| {
        capture
            .file(path)
            .map(|file| file.bytes().to_vec())
            .map_err(|_| std::io::Error::other("not captured"))
    };
    let facts = ingest::pipelines::extract_from_sources(
        &files,
        &read,
        &ingest::pipelines::SourceId {
            repo: REPO,
            commit: "workdir",
        },
    );
    assert!(!facts.nodes.is_empty());
    stage_pipeline_receipts(
        &capture,
        &files,
        &SourceId {
            repo: REPO,
            commit: "workdir",
        },
        (&facts.nodes, &facts.edges),
    )
    .into_receipts()
}

// AC-1528: the pipeline reader's facts over a capture get contract-v4
// Pipeline receipts established on one closure: the capture's enumeration
// under `pipeline-walk-v1` and each captured pipeline file, no other
// source. A change to one pipeline file changes the closure but not the
// other file's facts.
#[test]
fn pipeline_receipts_are_established_on_the_reader_closure() {
    let directory = tempfile::tempdir().unwrap();
    let before = walk(directory.path(), "FROM rust:1\nRUN cargo build\n");
    let workflow: Vec<&MintedReceipt> = before
        .iter()
        .filter(|receipt| receipt.file().path == WORKFLOW)
        .collect();
    assert!(!workflow.is_empty());
    let record = workflow[0].input_closure().expect("an established closure");
    for receipt in &before {
        assert!(
            receipt.id().starts_with("pipe-primary-v2:"),
            "{}",
            receipt.id()
        );
        assert_eq!(receipt.input_closure(), Some(record));
        let full = receipt.to_receipt().unwrap();
        assert_eq!(full.grammar(), Grammar::Pipeline);
        assert_eq!(Receipt::from_json(&full.to_json().unwrap()).unwrap(), full);
    }
    assert_eq!(record.parameters.walk_policy, PIPELINE_WALK_POLICY);
    assert_eq!(record.producer_contract, "t0.pipeline/declared-v2");
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
            "enumeration pipeline-walk-v1".to_string(),
            format!("file {WORKFLOW}"),
            "file Dockerfile".to_string(),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
    let after = walk(directory.path(), "FROM rust:1\nRUN cargo test\n");
    let changed = after
        .iter()
        .find(|receipt| receipt.fact_key() == workflow[0].fact_key())
        .expect("the workflow fact is still receipted");
    assert_eq!(changed.fact_digest(), workflow[0].fact_digest());
    assert_ne!(
        changed.input_closure().unwrap().closure_id,
        record.closure_id
    );
}

// AC-1528: a Pipeline receipt's grammar follows the reader's selection, so
// a YAML file outside it is no Pipeline file (it is the environment
// reader's, AC-1530).
#[test]
fn the_pipeline_grammar_follows_the_reader_selection() {
    assert_eq!(Grammar::for_path(WORKFLOW), Grammar::Pipeline);
    assert_eq!(Grammar::for_path("svc/Dockerfile"), Grammar::Pipeline);
    assert_eq!(Grammar::for_path("config/app.yml"), Grammar::Environment);
}
