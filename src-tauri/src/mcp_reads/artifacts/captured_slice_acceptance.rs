//! Opt-in frozen 17-file evidence check, not live registration or MCP service acceptance.
use super::*;
use crate::mcp_reads::{KnowledgeBasis, ReadSession};
use core_graph::{SqliteGraphStore, overlay::OverlayStore};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const FROZEN_REPO: &str = "vendurehq/vendure";
const FROZEN_COMMIT: &str = "2cc309deb5394af176314552ac7320a7b26a0104";
const FROZEN_MANIFEST_SHA256: &str =
    "96d2595907c4b1ea05991c7f0f2460df2f7b75267c486d5ee3e96c3d6ae49b83";
const FROZEN_PROOF_SHA256: &str =
    "22d1cb69a593d6cf87abf1207cac9a50e82e6df61181874f9ab2fbd79e30a763";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    original_data: PathBuf,
    scratch_data: PathBuf,
    preparation_sha256: String,
    policy_sha256: String,
    files: BTreeMap<String, FileDigest>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDigest {
    bytes: u64,
    sha256: String,
}
fn path(name: &str) -> PathBuf {
    std::env::var_os(name).map(PathBuf::from).expect(name)
}
fn read(path: &std::path::Path, cap: usize) -> Vec<u8> {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file() && metadata.nlink() == 1);
    assert!(metadata.len() <= u64::try_from(cap).unwrap());
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take(u64::try_from(cap).unwrap().checked_add(1).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= cap);
    assert_eq!(u64::try_from(bytes.len()).unwrap(), metadata.len());
    bytes
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn frozen_files() -> Value {
    serde_json::from_str(include_str!(
        "../../investigations/worker/acceptance/vendure/frozen-files.json"
    ))
    .expect("embedded frozen Vendure file map")
}

fn pinned_proof_files() -> Value {
    let frozen = frozen_files();
    let files = frozen.as_object().expect("frozen file map");
    let hashes: serde_json::Map<String, Value> = files
        .iter()
        .map(|(path, file)| (path.clone(), file["sha256"].clone()))
        .collect();
    Value::Object(hashes)
}

fn verify_pinned_capture_input(
    input: &Value,
    proof: &Value,
    proof_sha256: &str,
) -> Result<(), &'static str> {
    if proof_sha256 != FROZEN_PROOF_SHA256
        || input["proof_sha256"] != FROZEN_PROOF_SHA256
        || input["repo"] != FROZEN_REPO
        || input["commit_sha"] != FROZEN_COMMIT
        || input["manifest_sha256"] != FROZEN_MANIFEST_SHA256
        || proof["repo"] != FROZEN_REPO
        || proof["commit_sha"] != FROZEN_COMMIT
        || proof["manifest_sha256"] != FROZEN_MANIFEST_SHA256
        || input["files"] != frozen_files()
        || proof["files_sha256"] != pinned_proof_files()
    {
        return Err("captured input does not match the frozen Vendure proof");
    }
    Ok(())
}

#[test]
fn pinned_capture_identity_rejects_caller_repaired_proof_pair() {
    let files = frozen_files();
    let proof_files = pinned_proof_files();
    let input = serde_json::json!({
        "repo": FROZEN_REPO,
        "commit_sha": FROZEN_COMMIT,
        "manifest_sha256": FROZEN_MANIFEST_SHA256,
        "proof_sha256": FROZEN_PROOF_SHA256,
        "files": files,
    });
    let proof = serde_json::json!({
        "repo": FROZEN_REPO,
        "commit_sha": FROZEN_COMMIT,
        "manifest_sha256": FROZEN_MANIFEST_SHA256,
        "files_sha256": proof_files,
    });
    assert!(verify_pinned_capture_input(&input, &proof, FROZEN_PROOF_SHA256).is_ok());
    assert!(verify_pinned_capture_input(&input, &proof, "0").is_err());

    // A caller who changes both sides to self-consistent alternate identities
    // or contents still cannot replace the compiled-in capture.
    for field in ["repo", "commit_sha", "manifest_sha256"] {
        let mut changed_input = input.clone();
        let mut changed_proof = proof.clone();
        changed_input[field] = Value::String("untrusted replacement".into());
        changed_proof[field] = Value::String("untrusted replacement".into());
        assert!(
            verify_pinned_capture_input(&changed_input, &changed_proof, FROZEN_PROOF_SHA256)
                .is_err()
        );
    }
    let path = frozen_files()
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let mut changed_input = input;
    let mut changed_proof = proof;
    changed_input["files"][path.as_str()]["sha256"] = Value::String("0".repeat(64));
    changed_proof["files_sha256"][path.as_str()] = Value::String("0".repeat(64));
    assert!(
        verify_pinned_capture_input(&changed_input, &changed_proof, FROZEN_PROOF_SHA256).is_err()
    );
}

// AC-0513: actual captured facts reach the real no-provider phase, session and official stories.
#[test]
#[ignore = "requires separately approved scratch-copy receipt and complete caller policy"]
fn captured_seventeen_file_capability_stories_without_provider() {
    // This additional harness-input ceiling is caller supplied, never a fixture/default policy.
    let input_cap: std::num::NonZeroUsize = std::env::var("CARTOGRAPH_CAPTURE_INPUT_MAX_BYTES")
        .unwrap()
        .parse()
        .unwrap();
    let cap = input_cap.get();
    let receipt: Receipt =
        serde_json::from_slice(&read(&path("CARTOGRAPH_CAPTURE_RECEIPT"), cap)).unwrap();
    let preparation = read(&path("CARTOGRAPH_CAPTURE_PREPARATION"), cap);
    let policy_bytes = read(&path("CARTOGRAPH_CAPTURE_POLICY"), cap);
    let proof_bytes = read(&path("CARTOGRAPH_CAPTURE_INPUT_PROOF"), cap);
    assert_eq!(hash(&preparation), receipt.preparation_sha256);
    assert_eq!(hash(&policy_bytes), receipt.policy_sha256);
    let preparation: Value = serde_json::from_slice(&preparation).unwrap();
    let proof: Value = serde_json::from_slice(&proof_bytes).unwrap();
    let input = &preparation["input"];
    verify_pinned_capture_input(input, &proof, &hash(&proof_bytes)).unwrap();
    assert_eq!(input["files"].as_object().unwrap().len(), 17);
    let policy = crate::mcp_reads::commands::captured_slice_policy(&policy_bytes).unwrap();
    assert!(matches!(
        policy.worker.acquisition_mode,
        crate::mcp_reads::worker::AcquisitionMode::Complete
    ));
    assert_eq!(
        policy.export_mode,
        spec::ExportMode::BestEffort,
        "weak story visibility requires caller-approved best-effort"
    );
    let original = receipt.original_data.canonicalize().unwrap();
    assert_eq!(
        original,
        PathBuf::from(
            preparation["profile"]["app"]["appDirectoriesOverride"]["data"]
                .as_str()
                .unwrap()
        )
        .canonicalize()
        .unwrap()
    );
    assert!(
        fs::symlink_metadata(&receipt.scratch_data)
            .unwrap()
            .is_dir()
    );
    let scratch = receipt.scratch_data.canonicalize().unwrap();
    assert!(!scratch.starts_with(&original));
    let graph_path = path("CARTOGRAPH_CAPTURE_SCRATCH_GRAPH");
    assert_eq!(graph_path, scratch.join("graph.db"));
    let required = [
        "graph.db",
        "graph.db-wal",
        "graph.db-shm",
        "state.db",
        "state.db-wal",
        "state.db-shm",
    ];
    assert_eq!(receipt.files.len(), required.len());
    for name in required {
        let expected = receipt
            .files
            .get(name)
            .expect("complete copied graph/state triplets");
        let bytes = read(&scratch.join(name), cap);
        assert_eq!(u64::try_from(bytes.len()).unwrap(), expected.bytes);
        assert_eq!(hash(&bytes), expected.sha256);
    }
    // Only verified scratch paths reach any SQLite constructor below.
    let graph = Mutex::new(SqliteGraphStore::open(&graph_path).unwrap());
    let before = graph
        .lock()
        .unwrap()
        .read_snapshot_with_coverage_and_revision_bounded(
            policy.worker.graph,
            policy.worker.coverage,
        )
        .unwrap();
    assert_eq!(
        context_hub::ContextSnapshot::new(before.0.graph.0.clone(), before.0.graph.1.clone())
            .unwrap()
            .id(),
        preparation["graph"]["graph_snapshot_id"].as_str().unwrap()
    );
    let record = &preparation["graph"]["coverage"]["record"];
    assert_eq!(record["repo_key"], preparation["graph"]["repo_key"]);
    assert_eq!(
        record["registered_source_id"],
        preparation["graph"]["source_id"]
    );
    let stored = &before
        .1
        .iter()
        .find(|r| r.record.repo_key == record["repo_key"].as_str().unwrap())
        .unwrap()
        .record;
    assert_eq!(
        stored.registered_source_id.as_deref(),
        record["registered_source_id"].as_str()
    );
    assert_eq!(stored.config_hash, record["config_hash"].as_str().unwrap());
    assert_eq!(
        stored.inventory_json,
        record["inventory_json"].as_str().unwrap()
    );
    let inventory: Value = serde_json::from_str(&stored.inventory_json).unwrap();
    let scope = context_hub::coverage::ScopedRepo {
        repo_key: stored.repo_key.clone(),
        registered_source_id: stored.registered_source_id.clone(),
        config_hash: stored.config_hash.clone(),
        layer_hints: Some(serde_json::from_value(inventory["layer_hints"].clone()).unwrap()),
    };
    let cache_path = scratch.join("capture-acceptance-cache.db");
    let overlay_path = scratch.join("capture-acceptance-overlay.db");
    assert!(!cache_path.exists() && !overlay_path.exists());
    let cache = Arc::new(Mutex::new(
        semantic::embedding_cache::EmbeddingCache::open(cache_path).unwrap(),
    ));
    let mut overlay = OverlayStore::open(overlay_path).unwrap();
    overlay
        .bind_instance(&before.0.revision.graph_instance)
        .unwrap();
    let overlay = Arc::new(Mutex::new(overlay));
    let report = crate::knowledge_phase::run(&graph, &cache, &overlay, None).unwrap();
    assert!(
        report.published && report.derived > 0 && report.derived_omission.is_none(),
        "{report:?}"
    );
    assert_eq!(report.embedded, 0);
    assert_eq!(
        report.omission,
        Some(semantic::knowledge_ingest::NO_PROVIDER)
    );
    let decisions = Mutex::new(agents::DecisionLog::open(scratch.join("state.db")).unwrap());
    let session = ReadSession::acquire_with_overlay(
        &graph,
        Some(overlay.as_ref()),
        &decisions,
        || Ok(vec![scope.clone()]),
        policy.worker.graph,
        policy.worker.coverage,
        policy.export_mode,
    )
    .unwrap();
    assert!(matches!(
        session.basis.knowledge,
        KnowledgeBasis::Current(_)
    ));
    assert!(!session.capabilities.is_empty());
    let artifact = match session
        .compile_artifact(
            "user-stories",
            policy.worker.dispatch.trace,
            policy.worker.dispatch.compile,
        )
        .unwrap()
    {
        ArtifactCompilation::Produced(artifact) => artifact,
        ArtifactCompilation::Limited { limit, .. } => {
            panic!("caller policy refused official stories: {limit:?}")
        }
    };
    assert!(artifact.artifact.verify_identity());
    let mut rendered = 0;
    for fact in &session.capabilities {
        let core_graph::overlay::OverlayItem::Node(node) = &fact.item else {
            panic!("Capability node expected")
        };
        let candidate = artifact
            .artifact
            .assertions
            .iter()
            .find(|a| a.id == format!("node:{}", node.id));
        let visible = candidate.is_some();
        if let Some(row) = candidate {
            rendered += 1;
            assert_eq!(
                row.provenance,
                serde_json::from_value::<core_prov::Provenance>(node.props["prov"].clone())
                    .unwrap()
            );
        }
        for cite in &fact.cites {
            let mut encoded = b"capability-story-support-v1\0".to_vec();
            encoded.extend(serde_json::to_vec(&(&node.id, cite)).unwrap());
            let id = format!("support:{}", core_prov::content_hash(&encoded));
            let row = artifact.artifact.assertions.iter().find(|a| a.id == id);
            if !visible {
                assert!(
                    row.is_none(),
                    "omitted candidate cannot leave partial support"
                );
                continue;
            }
            let row = row.unwrap();
            let props = match cite {
                core_graph::overlay::BaseRef::Node(id) => {
                    &before.0.graph.0.iter().find(|n| &n.id == id).unwrap().props
                }
                core_graph::overlay::BaseRef::Edge(src, dst, label) => {
                    &before
                        .0
                        .graph
                        .1
                        .iter()
                        .find(|e| (&e.src, &e.dst, &e.label) == (src, dst, label))
                        .unwrap()
                        .props
                }
            };
            assert_eq!(
                row.provenance,
                serde_json::from_value::<core_prov::Provenance>(props["prov"].clone()).unwrap()
            );
        }
    }
    assert!(
        rendered > 0,
        "no visible derived Capability under captured decisions, caller policy and compiler eligibility; captured-slice acceptance not established"
    );
    eprintln!(
        "selected={}, rendered={rendered}, filtered={}",
        session.capabilities.len(),
        session.capabilities.len() - rendered
    );
    let page = artifact
        .page(
            artifact.basis_id(),
            artifact.content_id(),
            0,
            policy.worker.dispatch.max_response_bytes,
            policy.worker.dispatch.max_response_bytes,
        )
        .unwrap();
    assert!(!page.content.is_empty());
    assert_eq!(page.basis_id, session.basis_id().unwrap());
    assert_eq!(
        graph
            .lock()
            .unwrap()
            .read_snapshot_with_coverage_and_revision_bounded(
                policy.worker.graph,
                policy.worker.coverage
            )
            .unwrap(),
        before
    );
    eprintln!(
        "captured 17-file slice only; direct session, historical scope; MCP service/transport admission not executed; phase={report:?}; assertions={}; page_partial={}",
        artifact.artifact.assertions.len(),
        page.partial
    );
}
