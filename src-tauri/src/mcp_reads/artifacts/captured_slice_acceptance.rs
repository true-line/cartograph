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
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::{Arc, Mutex},
};

const FROZEN_REPO: &str = "vendurehq/vendure";
const FROZEN_COMMIT: &str = "2cc309deb5394af176314552ac7320a7b26a0104";
const FROZEN_MANIFEST_SHA256: &str =
    "96d2595907c4b1ea05991c7f0f2460df2f7b75267c486d5ee3e96c3d6ae49b83";
const FROZEN_PROOF_SHA256: &str =
    "22d1cb69a593d6cf87abf1207cac9a50e82e6df61181874f9ab2fbd79e30a763";
// This fixed historical capture is intentionally adapter-version scoped. A
// re-ingest with changed extraction code must receive a newly reviewed pin.
const FROZEN_GRAPH_SNAPSHOT_ID: &str =
    "context-v1:e002581d7e7a4aa39cc5d54962415cfcea5bb3c492fbe7e6cac2874c727c5af6";
const FROZEN_SOURCE_ID: &str = "src_5a2efb974cf8eb1674d52655df1e85b4";
const FROZEN_REPO_KEY: &str = "local/src_5a2efb974cf8eb1674d52655df1e85b4";
const FROZEN_INGEST_ID: &str = "job:1";
const FROZEN_CONFIG_HASH: &str = "1e94359386957269770da884f7526d43f2d38f7572694fdac67ed6836c7cdaec";
const FROZEN_COVERAGE_INVENTORY_SHA256: &str =
    "a67283739c05cd7206dede1d871bc6e05a7129835a633bd3899b11d3dd4b428b";
// Independently sampled current curation baseline for the exact fresh-profile
// path pinned by the preparation. Sampled 2026-10-31T14:18:57.940647Z by raw
// file reads without SQLite opens; baseline JSON SHA-256 is 839123f004d76d2d5ed3653e04f494e0bc98a83f85040164ad1fb71fed55c942.
// This is not historical-at-capture proof.
const FROZEN_DECISION_STATE_FILES: [(&str, u64, &str); 3] = [
    (
        "state.db",
        4_096,
        "da63a52bdf62b3fc863995d79f497584be3ba97c1de4c55d1d301afbafab6cfc",
    ),
    (
        "state.db-wal",
        1_545_032,
        "2cea22c384ffe2926d7cfd89f102414bb4c9f6483386832ddbd6c5c2fc3d1022",
    ),
    (
        "state.db-shm",
        32_768,
        "7481c13dd55d11838d91b07ca452a9382267cc96718f9c0abd6a6d8c367d5538",
    ),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    original_data: PathBuf,
    scratch_data: PathBuf,
    preparation_sha256: String,
    policy_sha256: String,
    files: BTreeMap<String, FileDigest>,
}
#[derive(Deserialize, Clone)]
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

// Copy only caller-verified buffers, never reopen their mutable source paths.
// The random private directory isolates caller-path swaps, not hostile same-UID
// access to harness-owned files. These extra buffers are harness ownership,
// outside the claimed MCP logical-component evidence and any RSS guarantee.
fn private_decision_copy(
    files: &BTreeMap<String, Vec<u8>>,
) -> (tempfile::TempDir, agents::DecisionLog) {
    assert_eq!(files.len(), FROZEN_DECISION_STATE_FILES.len());
    let directory = tempfile::Builder::new()
        .prefix("cartograph-captured-decisions-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    for (name, _, _) in FROZEN_DECISION_STATE_FILES {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.path().join(name))
            .unwrap();
        file.write_all(files.get(name).expect("complete verified state triplet"))
            .unwrap();
    }
    let decisions = agents::DecisionLog::open(directory.path().join("state.db")).unwrap();
    (directory, decisions)
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

fn verify_frozen_decision_state(files: &BTreeMap<String, FileDigest>) -> Result<(), &'static str> {
    for (name, bytes, sha256) in FROZEN_DECISION_STATE_FILES {
        let actual = files
            .get(name)
            .ok_or("copied decision state differs from the reviewed current baseline")?;
        if actual.bytes != bytes || actual.sha256 != sha256 {
            return Err("copied decision state differs from the reviewed current baseline");
        }
    }
    Ok(())
}

fn verify_pinned_graph_snapshot(snapshot_id: &str) -> Result<(), &'static str> {
    if snapshot_id == FROZEN_GRAPH_SNAPSHOT_ID {
        Ok(())
    } else {
        Err("stored graph does not match the frozen Vendure capture")
    }
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

// AC-0513: copied decision state cannot be self-attested by the receipt.
#[test]
fn captured_decision_state_identity_is_pinned() {
    let expected: BTreeMap<String, FileDigest> = FROZEN_DECISION_STATE_FILES
        .into_iter()
        .map(|(name, bytes, sha256)| {
            (
                name.to_owned(),
                FileDigest {
                    bytes,
                    sha256: sha256.to_owned(),
                },
            )
        })
        .collect();
    assert!(verify_frozen_decision_state(&expected).is_ok());

    for (name, _, _) in FROZEN_DECISION_STATE_FILES {
        let mut changed = expected.clone();
        changed.get_mut(name).unwrap().sha256 = "0".repeat(64);
        assert!(verify_frozen_decision_state(&changed).is_err());
        let mut changed = expected.clone();
        changed.get_mut(name).unwrap().bytes += 1;
        assert!(verify_frozen_decision_state(&changed).is_err());
    }
    let mut missing = expected;
    missing.remove("state.db-wal");
    assert!(verify_frozen_decision_state(&missing).is_err());
}

// AC-0513: SQLite consumes the verified WAL bytes despite a later caller-path swap.
#[test]
fn captured_decision_copy_survives_caller_path_replacement() {
    let source = tempfile::tempdir().unwrap();
    let replacement = tempfile::tempdir().unwrap();
    let fixture = |directory: &std::path::Path, seed: &str| {
        let mut log = agents::DecisionLog::open(directory.join("state.db")).unwrap();
        let assertion = agents::CuratableAssertion {
            subject_id: format!("symbol:{seed}"),
            summary: "Synthetic inferred assertion".into(),
            provenance: core_prov::Provenance::new(
                core_prov::Tier::Semantic,
                core_prov::ConfidenceTier::InferredWeak,
                vec![core_prov::EvidenceRef {
                    repo: "fixture/repo".into(),
                    path: "source.ts".into(),
                    byte_start: 0,
                    byte_end: 1,
                    commit_sha: "fixture".into(),
                }],
                "fixture",
                seed.as_bytes(),
            )
            .unwrap(),
        };
        log.record_assertion(&assertion, agents::AssertionDecision::Rejected, None)
            .unwrap();
        let files: BTreeMap<String, Vec<u8>> = FROZEN_DECISION_STATE_FILES
            .into_iter()
            .map(|(name, _, _)| (name.to_owned(), fs::read(directory.join(name)).unwrap()))
            .collect();
        assert!(files["state.db-wal"].len() > 32);
        (log, files, assertion.provenance.content_hash)
    };
    let (source_log, verified, original_hash) = fixture(source.path(), "original");
    let (replacement_log, other, replacement_hash) = fixture(replacement.path(), "replacement");
    // These hashes stand for the initial verification; retain those exact bytes.
    let frozen_hashes: BTreeMap<_, _> =
        verified.iter().map(|(n, b)| (n.clone(), hash(b))).collect();
    drop(source_log);
    drop(replacement_log);
    for (name, bytes) in &other {
        fs::write(source.path().join(name), bytes).unwrap();
    }
    assert_ne!(
        hash(&fs::read(source.path().join("state.db-wal")).unwrap()),
        frozen_hashes["state.db-wal"]
    );
    let (private, decisions) = private_decision_copy(&verified);
    assert!(!private.path().starts_with(source.path()));
    assert_eq!(
        fs::metadata(private.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for (name, _, _) in FROZEN_DECISION_STATE_FILES {
        assert_eq!(
            fs::metadata(private.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let observed = decisions.read_curation_observation(&[]).unwrap();
    assert_eq!(
        observed.rejected_hashes,
        [original_hash].into_iter().collect()
    );
    let swapped = agents::DecisionLog::open(source.path().join("state.db")).unwrap();
    assert_eq!(
        swapped
            .read_curation_observation(&[])
            .unwrap()
            .rejected_hashes,
        [replacement_hash].into_iter().collect()
    );
    drop(decisions);
    drop(private);
}

// AC-0513: the stored graph identity is pinned to the historical capture.
#[test]
fn captured_graph_snapshot_id_is_pinned() {
    assert!(verify_pinned_graph_snapshot(FROZEN_GRAPH_SNAPSHOT_ID).is_ok());
    assert!(verify_pinned_graph_snapshot("context-v1:other-graph").is_err());
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
    let mut verified_state = BTreeMap::new();
    for name in required {
        let expected = receipt
            .files
            .get(name)
            .expect("complete copied graph/state triplets");
        let bytes = read(&scratch.join(name), cap);
        assert_eq!(u64::try_from(bytes.len()).unwrap(), expected.bytes);
        assert_eq!(hash(&bytes), expected.sha256);
        if name.starts_with("state.db") {
            verified_state.insert(name.to_owned(), bytes);
        }
    }
    // Receipt copy hashes are not the expected decision state; bind them to the
    // separately reviewed current baseline before opening either SQLite store.
    verify_frozen_decision_state(&receipt.files).unwrap();
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
    let graph_snapshot_id =
        context_hub::ContextSnapshot::new(before.0.graph.0.clone(), before.0.graph.1.clone())
            .unwrap()
            .id()
            .to_owned();
    verify_pinned_graph_snapshot(&graph_snapshot_id).unwrap();
    assert_eq!(graph_snapshot_id, preparation["graph"]["graph_snapshot_id"]);
    assert_eq!(preparation["graph"]["source_id"], FROZEN_SOURCE_ID);
    assert_eq!(preparation["graph"]["repo_key"], FROZEN_REPO_KEY);
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
    assert_eq!(stored.repo_key, FROZEN_REPO_KEY);
    assert_eq!(
        stored.registered_source_id.as_deref(),
        Some(FROZEN_SOURCE_ID)
    );
    assert_eq!(stored.ingest_id, FROZEN_INGEST_ID);
    assert_eq!(stored.config_hash, FROZEN_CONFIG_HASH);
    assert_eq!(
        hash(stored.inventory_json.as_bytes()),
        FROZEN_COVERAGE_INVENTORY_SHA256
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
    // Keep the private directory alive until after the DecisionLog/session.
    let (_decision_directory, decisions) = private_decision_copy(&verified_state);
    drop(verified_state);
    let decisions = Mutex::new(decisions);
    let session = ReadSession::acquire_with_overlay(
        &graph,
        Some(overlay.as_ref()),
        &decisions,
        &crate::curated::NoReviews,
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
    let mut rendered_actors = 0;
    for (fact, is_actor) in session
        .capabilities
        .iter()
        .map(|fact| (fact, false))
        .chain(session.actors.iter().map(|fact| (fact, true)))
    {
        let core_graph::overlay::OverlayItem::Node(node) = &fact.item else {
            panic!("producer node expected")
        };
        assert_eq!(node.label, if is_actor { "Actor" } else { "Capability" });
        if is_actor {
            assert_eq!(fact.cites.len(), 1);
            let core_graph::overlay::BaseRef::Node(endpoint_id) = fact.cites.first().unwrap()
            else {
                panic!("Actor must cite its base Endpoint")
            };
            assert_eq!(
                node.props["endpoint_id"].as_str(),
                Some(endpoint_id.as_str())
            );
            assert_eq!(
                before
                    .0
                    .graph
                    .0
                    .iter()
                    .find(|node| &node.id == endpoint_id)
                    .unwrap()
                    .label,
                "Endpoint"
            );
        }
        let candidate = artifact
            .artifact
            .assertions
            .iter()
            .find(|a| a.id == format!("node:{}", node.id));
        let visible = candidate.is_some();
        if let Some(row) = candidate {
            if is_actor {
                rendered_actors += 1;
                assert_eq!(
                    node.props["assumption"],
                    serde_json::json!({
                        "name": "allow-any-listed-permission",
                        "statement": spec::ACTOR_ALLOW_ASSUMPTION,
                        "rule": spec::ACTOR_ALLOW_RULE,
                        "version": spec::ACTOR_ALLOW_RULE_VERSION
                    })
                );
                let caller_set = serde_json::to_string(&(
                    &node.id,
                    &node.props["any_of"],
                    &node.props["assumption"],
                    &node.props["permission_site"]["prov"]["evidence"],
                ))
                .unwrap();
                assert!(
                    artifact
                        .artifact
                        .content
                        .contains(&format!("    {caller_set}\n"))
                );
                assert!(
                    artifact
                        .artifact
                        .content
                        .contains("## Actor caller set — Semantic / InferredWeak convention")
                );
                assert!(artifact.artifact.content.contains(
                    "Assumed caller set; not effective runtime authorization or an identified user, role or principal."
                ));
            } else {
                rendered += 1;
            }
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
    eprintln!(
        "actors_selected={}, actors_rendered={rendered_actors}, actors_filtered={}",
        session.actors.len(),
        session.actors.len() - rendered_actors
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
