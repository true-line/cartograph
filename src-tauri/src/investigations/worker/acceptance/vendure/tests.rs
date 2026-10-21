use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

struct Case {
    _directory: tempfile::TempDir,
    root: PathBuf,
    manifest: PathBuf,
    proof: PathBuf,
    config: PathBuf,
    profile: PathBuf,
}

impl Case {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let base = crate::paths::canonicalize(directory.path()).unwrap();
        let root = base.join("source");
        std::fs::create_dir(&root).unwrap();
        let files = [
            (
                "order.ts",
                "import { missing } from './absent';\nexport const policy = { minimum: 1 };\nexport function checkout(amount: number) { if (amount < policy.minimum) { return false; } return missing(amount); }\n",
            ),
            (
                "stock.ts",
                "export function inStock(quantity: number) { if (quantity <= 0) { throw new Error('out of stock'); } return true; }\n",
            ),
        ];
        let mut hashes = serde_json::Map::new();
        for (path, source) in files {
            std::fs::write(root.join(path), source).unwrap();
            hashes.insert(
                path.into(),
                json!(format!("{:x}", Sha256::digest(source.as_bytes()))),
            );
        }
        let manifest = base.join("input.json");
        let proof = base.join("proof.json");
        let bytes = serde_json::to_vec(&json!({"schema_version":1,"repo":"example/control",
            "commit_sha":"a".repeat(40),"scope":"production source only",
            "source_files":["order.ts","stock.ts"]}))
        .unwrap();
        std::fs::write(&manifest, &bytes).unwrap();
        std::fs::write(
            &proof,
            serde_json::to_vec(&json!({"repo":"example/control",
            "commit_sha":"a".repeat(40),"manifest_sha256":format!("{:x}",Sha256::digest(&bytes)),
            "files_sha256":hashes}))
            .unwrap(),
        )
        .unwrap();
        let profile = base.join("private");
        let config = base.join("native.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&json!({
                "productName":"Cartograph Acceptance 598 Fixture",
                "identifier":"com.qwtm.cartograph.acceptance598.fixture",
                "app":{"appDirectoriesOverride":{"data":profile.join("data"),
                    "config":profile.join("config"),"localData":profile.join("local-data"),
                    "cache":profile.join("cache"),"log":profile.join("logs")}}
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            _directory: directory,
            root,
            manifest,
            proof,
            config,
            profile,
        }
    }
    fn prepare(&self) -> Result<Preparation, String> {
        prepare(
            &self.root,
            &self.manifest,
            &self.proof,
            &self.config,
            "fixture-code-revision".into(),
            &self.root.parent().unwrap().join("report"),
            false,
        )
    }
}

// AC-0191: deterministic preparation is distinct from actual-provider evidence.
#[test]
fn vendure_preparation_uses_real_ingest_captures_and_coverage() {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    assert_eq!(prepared.status, "captured_preparation_only");
    assert_eq!(prepared.provider_calls, 0);
    assert_eq!(prepared.ingest.files, 2);
    assert_eq!(prepared.graph.capture.files.len(), 2);
    assert!(matches!(
        prepared.graph.capture.kind,
        source_capture::CaptureKind::WorkingTree
    ));
    assert!(prepared.graph.repo_key.starts_with("local/src_"));
    assert!(!prepared.graph.candidates.is_empty());
    let value = serde_json::to_value(&prepared).unwrap();
    let candidates = value["graph"]["candidates"].as_array().unwrap();
    assert!(candidates.iter().any(|candidate| {
        !candidate["receipt"]["content"]["ranges"]
            .as_array()
            .unwrap()
            .is_empty()
    }));
    assert_eq!(
        value["graph"]["coverage"]["record"]["ingest_id"],
        format!("job:{}", prepared.ingest.job_id)
    );
    let inventory: Value = serde_json::from_str(
        value["graph"]["coverage"]["record"]["inventory_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(inventory["inventory"], "coverage@1");
    assert!(
        inventory["producers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|producer| producer["state"] == "ran" && producer["files"].as_u64().unwrap() > 0)
    );
    assert_eq!(
        inventory,
        serde_json::to_value(&prepared.ingest.coverage[0]).unwrap()
    );
    assert_eq!(
        value["graph"]["selection_status"],
        "caller_selection_required"
    );
    assert_eq!(
        value["graph"]["input_closure"],
        "input_closure_not_established"
    );
    assert_eq!(prepared.remaining_stages.len(), 5);
    let app = prepared.profile.app().unwrap();
    let state = app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap().list().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, "done");
    assert!(jobs[0].investigation_id.is_none());
    assert_eq!(
        app.path().app_data_dir().unwrap(),
        case.profile.join("data")
    );
    assert!(!case.profile.join("data/adapters").exists());
}

#[test]
fn vendure_preparation_rejects_input_before_creating_profile_or_jobs() {
    let case = Case::new();
    std::fs::write(case.root.join("expected.json"), b"withheld oracle").unwrap();
    assert!(case.prepare().is_err());
    assert!(!case.profile.exists());
    let case = Case::new();
    assert!(
        prepare(
            &case.root,
            &case.manifest,
            &case.proof,
            &case.config,
            "fixture".into(),
            &case.root.parent().unwrap().join("report"),
            true
        )
        .is_err()
    );
    assert!(!case.profile.exists());
}

#[test]
fn vendure_profile_requires_exact_fresh_disposable_directory_map() {
    let case = Case::new();
    std::fs::create_dir(&case.profile).unwrap();
    std::fs::write(case.profile.join("previous-run"), b"retained").unwrap();
    assert!(case.prepare().is_err());
    assert!(!case.profile.join("data").exists());
    let case = Case::new();
    let mut config: Value = serde_json::from_slice(&std::fs::read(&case.config).unwrap()).unwrap();
    config["app"]["appDirectoriesOverride"]["cache"] = json!(case.root.join("cache"));
    std::fs::write(&case.config, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(case.prepare().is_err());
    assert!(!case.profile.exists());
}

#[test]
fn vendure_preparation_retains_original_objects_after_checkout_removal() {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    std::fs::remove_dir_all(&case.root).unwrap();
    let app = prepared.profile.app().unwrap();
    let state = app.state::<AppState>();
    let source = state
        .sources
        .lock()
        .unwrap()
        .get_by_id(&prepared.graph.source_id)
        .unwrap()
        .unwrap();
    let reopened =
        report::collect(&state, &source, &prepared.input, prepared.profile.data()).unwrap();
    assert_eq!(reopened.capture_id, prepared.graph.capture_id);
    assert_eq!(reopened.graph_snapshot_id, prepared.graph.graph_snapshot_id);
    assert_eq!(
        serde_json::to_value(reopened.candidates).unwrap(),
        serde_json::to_value(prepared.graph.candidates).unwrap()
    );
    assert_eq!(state.jobs.lock().unwrap().list().unwrap().len(), 1);
}

#[test]
fn vendure_preparation_reports_are_exclusive_and_size_admitted() {
    let case = Case::new();
    let prepared = case.prepare().unwrap();
    let output = case._directory.path().join("preparation.json");
    report::write_new(&output, &prepared).unwrap();
    let first = std::fs::read(&output).unwrap();
    assert!(report::write_new(&output, &prepared).is_err());
    assert_eq!(first, std::fs::read(&output).unwrap());
    assert!(!String::from_utf8(first).unwrap().contains("out of stock"));
    let oversized = case._directory.path().join("oversized.json");
    assert!(report::write_new(&oversized, &"x".repeat(16 * 1024 * 1024)).is_err());
    assert!(!oversized.exists());
}

// AC-0191: evaluator and retained/native boundaries must be rejected before ingest.
#[test]
fn vendure_preparation_rejects_overlapping_and_aliased_roots() {
    let case = Case::new();
    let (profile, _) = profile::NativeProfile::read(&case.config).unwrap();
    for output in [
        &case.root,
        &case.profile,
        &case.root.join("reports"),
        case.root.parent().unwrap(),
    ] {
        assert!(
            validate_paths(
                &case.root,
                &case.manifest,
                &case.proof,
                &case.config,
                &profile,
                output
            )
            .is_err()
        );
    }
    let nested = case.profile.join("proof.json");
    std::fs::create_dir(&case.profile).unwrap();
    std::fs::copy(&case.proof, &nested).unwrap();
    assert!(
        validate_paths(
            &case.root,
            &case.manifest,
            &nested,
            &case.config,
            &profile,
            &case.root.parent().unwrap().join("report")
        )
        .is_err()
    );
    assert!(!case.profile.join("data").exists());
    #[cfg(unix)]
    {
        let alias = case.root.parent().unwrap().join("alias");
        std::os::unix::fs::symlink(&case.root, &alias).unwrap();
        assert!(
            validate_paths(
                &alias,
                &case.manifest,
                &case.proof,
                &case.config,
                &profile,
                &case.root.parent().unwrap().join("report")
            )
            .is_err()
        );
    }
}

// AC-0191: failures must not write a receipt through a swapped output path.
#[test]
fn vendure_receipts_refuse_swapped_output_and_changed_native_config() {
    let case = Case::new();
    let output = case.root.parent().unwrap().join("output");
    std::fs::create_dir(&output).unwrap();
    let guard = OutputDirectory::pin(&output).unwrap();
    let hash = profile::NativeProfile::read(&case.config).unwrap().1;
    verify_config(&case.config, &hash).unwrap();
    std::fs::write(&case.config, b"{}").unwrap();
    assert!(verify_config(&case.config, &hash).is_err());
    let original = output.with_extension("original");
    std::fs::rename(&output, &original).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&case.root, &output).unwrap();
    #[cfg(not(unix))]
    std::fs::create_dir(&output).unwrap();
    for name in ["preparation.json", "preparation-failed.json"] {
        assert!(guard.write(name, &json!({"must_not_escape":true})).is_err());
        assert!(!case.root.join(name).exists());
        assert!(!original.join(name).exists());
    }
}
