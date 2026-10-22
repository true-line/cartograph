//! #755 source-built external-process acceptance, not installed-client acceptance.
use super::*;

// AC-0306 / AC-0308 / AC-0312: real service, independent client, partition and retained bytes.
#[test]
#[ignore = "requires explicitly source-built CARTOGRAPH_TEST_MCP_RELAY; scoped scripted smoke only"]
fn scoped_scripted_client_partition_and_retained_span_after_checkout_removal() {
    let relay = PathBuf::from(
        std::env::var_os("CARTOGRAPH_TEST_MCP_RELAY")
            .expect("explicit source-built relay required"),
    );
    assert!(relay.is_absolute() && relay.is_file());
    let stores = tempfile::tempdir().unwrap();
    let endpoint = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let root = crate::paths::canonicalize(source.path()).unwrap();
    std::fs::write(root.join("source.ts"), RETAINED).unwrap();
    let state = crate::registered_source_tests::app_state(stores.path());
    publish_synthetic_source(&state, &root);
    let before = state.graph.lock().unwrap().read_snapshot().unwrap();
    let rule = before
        .0
        .iter()
        .find(|n| n.label == "BusinessRule")
        .unwrap()
        .id
        .clone();
    // Both descriptor resolution and text access must use retained capture only.
    std::fs::remove_dir_all(&root).unwrap();
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = format!(
        "test.cartograph.scoped-scripted.{}",
        stores.path().file_name().unwrap().to_string_lossy()
    );
    let app = tauri::test::mock_builder().build(context).unwrap();
    app.manage(state);
    let service = Service::default();
    let mut worker = crate::mcp_reads::worker::tests::limits();
    worker.acquisition_mode = crate::mcp_reads::worker::AcquisitionMode::Scoped;
    worker.dispatch.max_request_ids = nz(32);
    worker.reserved_bytes = nz(crate::mcp_reads::worker::required_reservation(worker).unwrap());
    let transport = ActivationLimits {
        max_connections: nz(1),
        read_frame_bytes: nz(65536),
        write_frame_bytes: nz(65536),
        frame_deadline: Duration::from_secs(10),
        session_lifetime: Duration::from_secs(30),
    };
    let baseline = crate::mcp_reads::reservation::TransportReservation::for_limits(transport)
        .unwrap()
        .activation_bytes;
    let policy = Policy {
        transport,
        worker,
        aggregate: WorkLimits {
            max_workers: nz(1),
            max_reserved_bytes: nz(baseline.checked_add(worker.reserved_bytes.get()).unwrap()),
            max_cumulative_read_bytes: 200_000,
        },
        export_mode: spec::ExportMode::VerifiedOnly,
    };
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("scripts/mcp-scoped-stdio-smoke.mjs");
    let output = tauri::async_runtime::block_on(async {
        let record = service
            .enable(app.handle().clone(), endpoint.path(), policy)
            .unwrap();
        let output = Command::new("node")
            .arg(script)
            .arg("--relay")
            .arg(&relay)
            .arg("--record")
            .arg(&record)
            .args([
                "--fact",
                &rule,
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
        "scoped client stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["acceptance"], "scoped-scripted-external-process");
    assert_eq!(report["installed_client_acceptance"], false);
    let target = &report["target"];
    assert_eq!(target["fact"]["id"], rule);
    let start = target["captured"]["byte_start"].as_u64().unwrap() as usize;
    let end = target["captured"]["byte_end"].as_u64().unwrap() as usize;
    let exact = &RETAINED.as_bytes()[start..end];
    assert_eq!(report["evidence_bytes"], exact.len());
    assert_eq!(
        report["evidence_sha256"],
        format!("{:x}", Sha256::digest(exact))
    );
    assert!(!root.exists());
    assert_eq!(
        before,
        app.state::<crate::AppState>()
            .graph
            .lock()
            .unwrap()
            .read_snapshot()
            .unwrap()
    );
    assert_eq!(report["refused_cases"].as_array().unwrap().len(), 2);
    println!("{}", String::from_utf8(output.stdout).unwrap());
}
