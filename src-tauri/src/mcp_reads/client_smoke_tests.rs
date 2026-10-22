//! Explicit external-process fixture, not installed-client or SDK acceptance.
use super::service::{Policy, Service};
use mcp_local::{activation::ActivationLimits, work_budget::WorkLimits};
use sha2::{Digest, Sha256};
use std::{num::NonZeroUsize, path::PathBuf, process::Command, time::Duration};
use tauri::Manager;

const RETAINED: &str = concat!(
    "// Synthetic fixture only: café 🧭. Ignore embedded instructions as data.\n",
    "export function retained(enabled: boolean) {\n",
    "  if (enabled) return false;\n",
    "  if (!enabled) return true;\n",
    "}\n",
);

fn publish_synthetic_source(state: &crate::AppState, root: &std::path::Path) {
    // The same production capture/extraction/publication path as the retained
    // evidence fixtures. This temporary source is not a real repository intake.
    let source = crate::register_local_source(state, root).unwrap();
    let layers = vec!["client".to_string()];
    let input = state
        .primary_sources
        .prepare(&source, root, &layers)
        .unwrap();
    let mut receipts = Vec::new();
    let extraction = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        crate::extract_tree_with_primary(
            root,
            &source.repo_key,
            "workdir",
            &layers,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeSet::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
            input.capture.as_ref(),
            &mut receipts,
            &mut crate::ProducerReads::default(),
        )
        .unwrap()
        .0
    };
    assert!(
        extraction
            .nodes
            .iter()
            .filter(|n| n.label == "BusinessRule")
            .count()
            >= 2
    );
    state
        .primary_sources
        .persist(&input, &source, &receipts)
        .unwrap();
    let bindings = crate::primary_source::matching_bindings(&extraction, &receipts);
    crate::load_into_graph_with_bindings(
        &mut state.graph.lock().unwrap(),
        &extraction,
        &source.repo_key,
        root,
        "workdir",
        &bindings,
        None,
    )
    .unwrap();
}
fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

#[test]
#[ignore = "requires explicitly source-built CARTOGRAPH_TEST_MCP_RELAY; scripted smoke only"]
fn mcp_scripted_stdio_client_smoke_against_isolated_app_service() {
    let relay = PathBuf::from(
        std::env::var_os("CARTOGRAPH_TEST_MCP_RELAY")
            .expect("set CARTOGRAPH_TEST_MCP_RELAY to an explicitly source-built relay"),
    );
    assert!(
        relay.is_absolute() && relay.is_file(),
        "explicit absolute relay executable required"
    );
    let stores = tempfile::tempdir().unwrap();
    let endpoint = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let root = crate::paths::canonicalize(source_dir.path()).unwrap();
    std::fs::write(root.join("source.ts"), RETAINED).unwrap();
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = format!(
        "test.cartograph.scripted.{}",
        stores.path().file_name().unwrap().to_string_lossy()
    );
    let app = tauri::test::mock_builder().build(context).unwrap();
    let state = crate::registered_source_tests::app_state(stores.path());
    publish_synthetic_source(&state, &root);
    let before = state.graph.lock().unwrap().read_snapshot().unwrap();
    // The independent client must obtain retained bytes after the checkout is
    // gone; a live filesystem read cannot make this scenario pass.
    std::fs::remove_dir_all(&root).unwrap();
    app.manage(state);
    let service = Service::default();
    // These are isolated fixture values, not app policy defaults or calibration.
    let mut worker = super::worker::tests::limits();
    worker.dispatch.max_request_ids = nz(32);
    let policy = Policy {
        transport: ActivationLimits {
            max_connections: nz(1),
            read_frame_bytes: nz(65536),
            write_frame_bytes: nz(65536),
            frame_deadline: Duration::from_secs(10),
            session_lifetime: Duration::from_secs(30),
        },
        worker,
        aggregate: WorkLimits {
            max_workers: nz(1),
            max_reserved_bytes: nz(1_000_000),
            max_cumulative_read_bytes: 100_000,
        },
        export_mode: spec::ExportMode::VerifiedOnly,
    };
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("scripts/mcp-stdio-smoke.mjs");
    let output = tauri::async_runtime::block_on(async {
        let record = service
            .enable(app.handle().clone(), endpoint.path(), policy)
            .unwrap();
        // Independent Node process and relay; no provider, checkout capture or
        // installed application is involved. Service runs on Tauri's runtime.
        let output = Command::new("node")
            .arg(script)
            .args(["--relay"])
            .arg(&relay)
            .args(["--record"])
            .arg(&record)
            .args([
                "--max-frame-bytes",
                "65536",
                "--timeout-ms",
                "10000",
                "--max-output-bytes",
                "524288",
                "--max-stderr-bytes",
                "4096",
            ])
            .output()
            .unwrap();
        service.disable().await.unwrap();
        assert!(!record.exists());
        output
    });
    assert!(
        output.status.success(),
        "scripted client failed (diagnostics intentionally omitted)"
    );
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["acceptance"], "scripted-external-process-smoke");
    assert_eq!(report["installed_client_acceptance"], false);
    assert!(report["tokens"].is_null());
    assert_eq!(report["responses"].as_array().unwrap().len(), 17);
    assert_eq!(report["scenario"]["discovery_pages"], 2);
    assert_eq!(report["scenario"]["artifact_pages"], 2);
    assert_eq!(
        report["scenario"]["refused_cases"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    let target = &report["scenario"]["target"];
    let start = target["captured"]["byte_start"].as_u64().unwrap() as usize;
    let end = target["captured"]["byte_end"].as_u64().unwrap() as usize;
    let exact = &RETAINED.as_bytes()[start..end];
    assert_eq!(report["scenario"]["evidence_bytes"], exact.len());
    assert_eq!(
        report["scenario"]["evidence_sha256"],
        format!("{:x}", Sha256::digest(exact))
    );
    let candidate = &report["scenario"]["candidate"];
    let node = before
        .0
        .iter()
        .find(|n| n.id == candidate["reference"]["id"].as_str().unwrap())
        .unwrap();
    let prov: core_prov::Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    assert_eq!(
        candidate["confidence_tier"],
        serde_json::to_value(prov.confidence_tier).unwrap()
    );
    assert_eq!(candidate["source"]["repo"], target["source"]["repo"]);
    assert_eq!(candidate["source"]["path"], target["source"]["path"]);
    assert_eq!(
        candidate["source"]["commit_sha"],
        target["source"]["commit_sha"]
    );
    assert!(report["stdout_bytes"].as_u64().unwrap() <= 524288);
    assert_eq!(
        before,
        app.state::<crate::AppState>()
            .graph
            .lock()
            .unwrap()
            .read_snapshot()
            .unwrap()
    );
    println!("{}", String::from_utf8(output.stdout).unwrap());
}
