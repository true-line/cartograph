//! Environment reader receipts are established on the reader's closure over
//! the captured environment files when the reader reports its facts bound
//! to them (ADR-0072 slice 9, AC-1530).

use super::*;
use source_capture::{CaptureLimits, SourceId as CaptureSourceId, capture_working_tree};

const SOURCE: &str = "src_11111111111111111111111111111111";
const REPO: &str = "local/src_11111111111111111111111111111111";
const COMPOSE: &str = "compose.yaml";
const KUSTOMIZATION: &str = "k8s/kustomization.yaml";
const DEPLOYMENT: &str = "k8s/deployment.yaml";
/// An environment-named JS file: the reader reads it, the TS lane keeps it.
const SETTINGS: &str = "config/environments/production.js";

/// Capture a Compose file, a kustomization, a manifest and a TS file, read
/// the environments from the capture, and stage their receipts.
fn walk(root: &Path, image: &str) -> Vec<MintedReceipt> {
    std::fs::create_dir_all(root.join("k8s")).unwrap();
    std::fs::write(
        root.join(COMPOSE),
        format!("services:\n  web:\n    image: {image}\n"),
    )
    .unwrap();
    std::fs::write(
        root.join(KUSTOMIZATION),
        "resources:\n  - deployment.yaml\n",
    )
    .unwrap();
    std::fs::write(
        root.join(DEPLOYMENT),
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n",
    )
    .unwrap();
    std::fs::write(root.join("app.ts"), "export const a = 1;\n").unwrap();
    std::fs::create_dir_all(root.join("config/environments")).unwrap();
    std::fs::write(root.join(SETTINGS), "module.exports = {};\n").unwrap();
    let paths: Vec<String> = [COMPOSE, DEPLOYMENT, KUSTOMIZATION, SETTINGS, "app.ts"]
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
        .filter(|path| ingest::environments::is_environment_file(path))
        .collect();
    let read = |path: &str| {
        capture
            .file(path)
            .map(|file| file.bytes().to_vec())
            .map_err(|_| std::io::Error::other("not captured"))
    };
    let (facts, reads) = ingest::environments::extract_from_capture_reading(
        root,
        &files,
        &read,
        &ingest::environments::SourceId {
            repo: REPO,
            commit: "workdir",
        },
    )
    .unwrap();
    assert!(reads.bound, "{reads:?}");
    assert!(reads.read.contains(&SETTINGS.to_string()), "{reads:?}");
    assert!(!facts.nodes.is_empty());
    let id = SourceId {
        repo: REPO,
        commit: "workdir",
    };
    let unbound =
        stage_environment_receipts(&capture, &files, &id, (&facts.nodes, &facts.edges), None)
            .into_receipts();
    assert!(!unbound.is_empty());
    for receipt in &unbound {
        assert!(receipt.input_closure().is_none());
        assert_eq!(
            receipt.closure(),
            Some(&ReceiptClosure::InputClosureNotEstablished {
                reasons: vec![ClosureGapReason::CrossFileInputUnbound]
            })
        );
    }
    stage_environment_receipts(
        &capture,
        &files,
        &id,
        (&facts.nodes, &facts.edges),
        Some(&reads.read),
    )
    .into_receipts()
}

// AC-1530: the environment reader's facts over a capture, reported bound to
// the captured files, get contract-v4 Environment receipts established on
// one closure: the capture's enumeration under `environment-walk-v1` and
// each captured environment file, and the environment-named JS file the
// reader read, though its own receipts stay in the TS lane. Unbound facts carry
// `cross_file_input_unbound`. A change to one file changes the closure but
// not another file's facts.
#[test]
fn environment_receipts_are_established_on_the_reader_closure() {
    let directory = tempfile::tempdir().unwrap();
    let before = walk(directory.path(), "nginx:1");
    let manifest: Vec<&MintedReceipt> = before
        .iter()
        .filter(|receipt| receipt.file().path == DEPLOYMENT)
        .collect();
    assert!(!manifest.is_empty());
    assert!(before.iter().any(|receipt| receipt.file().path == COMPOSE));
    let record = manifest[0].input_closure().expect("an established closure");
    for receipt in &before {
        assert!(
            receipt.id().starts_with("env-primary-v2:"),
            "{}",
            receipt.id()
        );
        assert_eq!(receipt.input_closure(), Some(record));
        let full = receipt.to_receipt().unwrap();
        assert_eq!(full.grammar(), Grammar::Environment);
        assert_eq!(Receipt::from_json(&full.to_json().unwrap()).unwrap(), full);
    }
    assert_eq!(record.parameters.walk_policy, ENVIRONMENT_WALK_POLICY);
    assert_eq!(record.producer_contract, "t0.environments/declared-v2");
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
            "enumeration environment-walk-v1".to_string(),
            format!("file {COMPOSE}"),
            format!("file {SETTINGS}"),
            format!("file {DEPLOYMENT}"),
            format!("file {KUSTOMIZATION}"),
        ]
    );
    let after = walk(directory.path(), "nginx:2");
    let changed = after
        .iter()
        .find(|receipt| receipt.fact_key() == manifest[0].fact_key())
        .expect("the manifest fact is still receipted");
    assert_eq!(changed.fact_digest(), manifest[0].fact_digest());
    assert_ne!(
        changed.input_closure().unwrap().closure_id,
        record.closure_id
    );
}

// AC-1530: the environment grammar takes only the files no language,
// config or pipeline grammar claims.
#[test]
fn the_environment_grammar_follows_the_reader_selection() {
    assert_eq!(Grammar::for_path(COMPOSE), Grammar::Environment);
    assert_eq!(Grammar::for_path(DEPLOYMENT), Grammar::Environment);
    assert_eq!(
        Grammar::for_path("src/main/resources/application.properties"),
        Grammar::Environment
    );
    assert_eq!(
        Grammar::for_path("config/environments/production.js"),
        Grammar::Tsx
    );
    assert_eq!(
        Grammar::for_path(".github/workflows/ci.yml"),
        Grammar::Pipeline
    );
    assert_eq!(Grammar::for_path("notes/readme.md"), Grammar::Tsx);
}
