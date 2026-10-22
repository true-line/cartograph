//! Proposed explicit synthetic measurement fixture; not a provider/manual intake test.
//! Apply only after review. Timing is opt-in and never a normal CI assertion.
use super::*;
use core_graph::{Edge, GraphPatch, GraphStore, Node, SqliteGraphStore};
use mcp_local::{
    connection::Connection,
    serve::{ServeError, serve_connection},
    work_budget::WorkLimits,
};
use mcp_wire::capability::{Capability, LOCAL_PROTOCOL};
use serde_json::{Value, json};
use std::{
    io::Write,
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

const NODES: usize = 60_001;
const WINDOW: usize = 4_096;
const FRAME: usize = 65_536;
const REPEATS: usize = 3;
const SESSION: &str = "synthetic-scoped-measure";
fn nz(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap()
}
fn frame(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    mcp_wire::write_frame(&mut out, value, nz(FRAME)).unwrap();
    out
}
fn sample(label: &str, elapsed: Duration, request: &Value, response: &Value) -> Value {
    let request = frame(request);
    let response = frame(response);
    json!({"operation":label,"wall_ns":elapsed.as_nanos().to_string(),
        "request_frame_bytes_including_lf":request.len(),
        "response_frame_bytes_including_lf":response.len(),
        "request_hash":core_prov::content_hash(&request),
        "response_hash":core_prov::content_hash(&response)})
}
async fn rpc(
    client: &mut Connection,
    request: Value,
    label: &str,
    samples: &mut Vec<Value>,
) -> Value {
    let start = Instant::now();
    client.write(&request).await.unwrap();
    let response = client.read().await.unwrap().expect("MCP response");
    let elapsed = start.elapsed();
    assert_eq!(response["id"], request["id"]);
    assert!(
        response.get("error").is_none(),
        "protocol error: {response}"
    );
    samples.push(sample(label, elapsed, &request, &response));
    response
}
fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}
fn wal_bytes(path: &std::path::Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => panic!("WAL metadata: {e}"),
    }
}
fn phase(name: &str) {
    println!(
        "CARTOGRAPH_MCP_MEASURE_PHASE {}",
        json!({"name":name,"pid":std::process::id()})
    );
    std::io::stdout().flush().unwrap();
}
#[cfg(unix)]
#[test]
#[ignore = "explicit synthetic MCP latency/RSS/WAL measurement; no provider or real target"]
fn scoped_mcp_synthetic_measurement() {
    // Short socket path, isolated app namespace. No owner database is opened.
    let dir = tempfile::Builder::new()
        .prefix("cg731-")
        .tempdir_in("/tmp")
        .unwrap();
    let app_data = dir.path().join("state");
    std::fs::create_dir(&app_data).unwrap();
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = format!(
        "test.cartograph.{}",
        dir.path().file_name().unwrap().to_string_lossy()
    );
    let app = tauri::test::mock_builder().build(context).unwrap();
    let state = crate::registered_source_tests::app_state(&app_data);
    let setup = Instant::now();
    let patch = GraphPatch {
        upsert_nodes: (0..NODES)
            .map(|i| Node {
                id: format!("n{i:06}"),
                label: "Symbol".into(),
                props: json!({"name":format!("n{i:06}")}),
            })
            .collect(),
        upsert_edges: (0..NODES - 1)
            .map(|i| Edge {
                src: format!("n{i:06}"),
                dst: format!("n{:06}", i + 1),
                label: "CALLS".into(),
                props: json!({}),
            })
            .collect(),
        ..GraphPatch::default()
    };
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .apply_patch_if_snapshot_matches(&(vec![], vec![]), &patch)
            .unwrap()
    );
    drop(patch);
    let setup_ns = setup.elapsed().as_nanos().to_string();
    app.manage(state);
    let graph_path = app_data.join("graph.db");
    let wal_path = app_data.join("graph.db-wal");
    // Open the independent writer before measuring any session, excluding its
    // schema/open cost from the controlled write latency below.
    let writer = SqliteGraphStore::open(&graph_path).unwrap();
    let mut policy = super::tests::limits();
    policy.acquisition_mode = AcquisitionMode::Scoped;
    policy.dispatch.max_request_ids = nz(64);
    policy.reserved_bytes = nz(required_reservation(policy).unwrap());
    let budget_for = |p: WorkerLimits| {
        WorkBudget::new(WorkLimits {
            max_workers: nz(1),
            max_reserved_bytes: p.reserved_bytes,
            max_cumulative_read_bytes: 1_048_576,
        })
    };
    phase("fixture_ready");
    let mut rounds = Vec::new();
    let mut deterministic = None;
    let mut writer = Some(writer);
    tauri::async_runtime::block_on(async {
        let mut complete = policy;
        complete.acquisition_mode = AcquisitionMode::Complete;
        complete.reserved_bytes = nz(required_reservation(complete).unwrap());
        let start = Instant::now();
        let refused = Worker::start(
            app.handle().clone(),
            "complete-baseline".into(),
            spec::ExportMode::VerifiedOnly,
            complete,
            budget_for(complete),
        )
        .await;
        assert!(matches!(refused, Err(ReadError::Limit)));
        let complete_ns = start.elapsed().as_nanos().to_string();
        for repeat in 0..REPEATS {
            phase("session_start");
            let path = dir.path().join(format!("s{repeat}.sock"));
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let secret = Arc::new(Capability::generate().unwrap());
            let server_secret = Arc::clone(&secret);
            let (revoke, revoked) = tokio::sync::watch::channel(false);
            let handle = app.handle().clone();
            let budget = budget_for(policy);
            let server_budget = budget.clone();
            let task = tauri::async_runtime::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let connection =
                    Connection::from_stream(stream, nz(FRAME), nz(FRAME), Duration::from_secs(60))
                        .unwrap();
                serve_connection(
                    connection,
                    &server_secret,
                    Duration::from_secs(300),
                    revoked,
                    || async move {
                        Worker::start(
                            handle,
                            SESSION.into(),
                            spec::ExportMode::VerifiedOnly,
                            policy,
                            server_budget,
                        )
                        .await
                        .map_err(|_| ServeError::Application)
                    },
                )
                .await
            });
            let start = Instant::now();
            let mut client =
                Connection::connect(&path, nz(FRAME), nz(FRAME), Duration::from_secs(60))
                    .await
                    .unwrap();
            let connect_ns = start.elapsed().as_nanos().to_string();
            let auth =
                json!({"protocol":LOCAL_PROTOCOL,"capability":secret.expose_for_local_transport()});
            let auth_request_bytes = frame(&auth).len();
            let auth_start = Instant::now();
            client.write(&auth).await.unwrap();
            let ack = client.read().await.unwrap().unwrap();
            assert!(mcp_wire::capability::is_acknowledgement(&ack));
            let auth_ns = auth_start.elapsed().as_nanos().to_string();
            // Never serialize the random capability or its frame hash to output.
            drop(auth);
            let mut samples = Vec::new();
            let ready_start = Instant::now();
            let init=rpc(&mut client,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cg731-measure","version":"1"}}}),"initialize_including_worker_acquisition",&mut samples).await;
            assert!(init.get("result").is_some());
            let ready_ns = ready_start.elapsed().as_nanos().to_string();
            let notification = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
            client.write(&notification).await.unwrap();
            let capabilities = rpc(
                &mut client,
                call(2, "cartograph_capabilities", json!({})),
                "capabilities",
                &mut samples,
            )
            .await;
            let cap = &capabilities["result"]["structuredContent"];
            assert_eq!(cap["mode"], "scoped");
            assert_eq!(cap["complete_system"], false);
            let basis = cap["session_basis_id"].as_str().unwrap().to_owned();
            assert!(basis.starts_with("mcp-scoped-session-v1:"));
            let mut cursor = Value::Null;
            let mut first_cursor = None;
            let mut query_basis = None;
            let (mut scanned, mut loaded, mut candidates) = (0u64, 0u64, Vec::new());
            let scan_start = Instant::now();
            let mut id = 3u64;
            loop {
                let request = call(
                    id,
                    "cartograph_discover",
                    json!({"session_id":SESSION,"session_basis_id":basis,"request":{"query":{"by":"identifier","id":"n060000"},"labels":[],"max_scan_rows":WINDOW,"max_candidates":3,"max_bytes":FRAME,"cursor":cursor}}),
                );
                let response = rpc(&mut client, request, "discovery_page", &mut samples).await;
                let page = &response["result"]["structuredContent"]["discovery"];
                assert_eq!(page["session_basis_id"], basis);
                assert_eq!(page["complete_system"], false);
                let current = page["query_basis_id"].as_str().unwrap().to_owned();
                assert!(current.starts_with("scoped-read-basis-v1:"));
                if let Some(prior) = &query_basis {
                    assert_eq!(prior, &current);
                } else {
                    query_basis = Some(current);
                }
                let n = page["scanned_rows"].as_u64().unwrap();
                let l = page["loaded_rows"].as_u64().unwrap();
                assert!(n > 0 && n <= WINDOW as u64 && l <= WINDOW as u64);
                scanned += n;
                loaded += l;
                candidates.extend(
                    page["candidates"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|c| c["reference"].clone()),
                );
                cursor = page["next_cursor"].clone();
                if first_cursor.is_none() {
                    first_cursor = Some(cursor.clone());
                }
                id += 1;
                assert!(id < 40, "bounded number of pages");
                if cursor.is_null() {
                    break;
                }
                assert!(cursor.is_object());
            }
            let scan_ns = scan_start.elapsed().as_nanos().to_string();
            assert_eq!(scanned, NODES as u64);
            assert_eq!(loaded, NODES as u64);
            assert_eq!(candidates, vec![json!({"kind":"node","id":"n060000"})]);
            let mut detail_fingerprints = Vec::new();
            for fact in [
                json!({"kind":"node","id":"n060000"}),
                json!({"kind":"edge","source":"n059999","label":"CALLS","destination":"n060000"}),
            ] {
                let response=rpc(&mut client,call(id,"cartograph_read_fact",json!({"session_id":SESSION,"session_basis_id":basis,"fact":fact,"max_bytes":FRAME})),"selected_detail",&mut samples).await;
                let detail = &response["result"]["structuredContent"]["detail"];
                assert_eq!(detail["fact"]["reference"], fact);
                assert_eq!(detail["complete_system"], false);
                assert_eq!(detail["absence_claim"], false);
                detail_fingerprints.push(core_prov::content_hash(
                    &serde_json::to_vec(detail).unwrap(),
                ));
                id += 1;
            }
            let stable = json!({"basis":basis,"query_basis":query_basis,"candidates":candidates,"detail_hashes":detail_fingerprints});
            if let Some(prior) = &deterministic {
                assert_eq!(prior, &stable);
            } else {
                deterministic = Some(stable.clone());
            }
            // No source registrations/receipts are fabricated for a graph-only
            // fixture. Confirm unavailable; this is NOT positive evidence proof.
            let unavailable=rpc(&mut client,call(id,"cartograph_evidence",json!({"session_id":SESSION,"session_basis_id":basis,"mode":"describe","kind":"retained_primary_source","fact":{"kind":"node","id":"n060000"},"ordinal":0,"max_bytes":FRAME})),"unbound_evidence",&mut samples).await;
            id += 1;
            assert_eq!(
                unavailable["result"]["structuredContent"]["error"],
                "evidence_unavailable"
            );
            let catalog = rpc(
                &mut client,
                call(
                    id,
                    "cartograph_artifacts",
                    json!({"session_id":SESSION,"session_basis_id":basis}),
                ),
                "unavailable_artifact_catalog",
                &mut samples,
            )
            .await;
            id += 1;
            let artifacts = catalog["result"]["structuredContent"]["catalog"]["artifacts"]
                .as_array()
                .unwrap();
            assert!(!artifacts.is_empty());
            assert!(
                artifacts
                    .iter()
                    .all(|a| a["status"] == "unavailable" || a["status"] == "partition_supported")
            );
            let mut writer_observation = Value::Null;
            if repeat + 1 == REPEATS {
                phase("writer_overlap");
                let wal_before = wal_bytes(&wal_path);
                let mut store = writer.take().unwrap();
                let gate = Arc::new(Barrier::new(2));
                let writer_gate = Arc::clone(&gate);
                let writing = std::thread::spawn(move || {
                    writer_gate.wait();
                    let start = Instant::now();
                    store
                        .put_node(&Node {
                            id: "writer-mutation".into(),
                            label: "Symbol".into(),
                            props: json!({"name":"writer"}),
                        })
                        .unwrap();
                    start.elapsed()
                });
                gate.wait();
                let args = json!({"session_id":SESSION,"session_basis_id":basis,"request":{"query":{"by":"identifier","id":"n060000"},"labels":[],"max_scan_rows":WINDOW,"max_candidates":3,"max_bytes":FRAME,"cursor":first_cursor.unwrap()}});
                let concurrent = rpc(
                    &mut client,
                    call(id, "cartograph_discover", args.clone()),
                    "discovery_racing_writer",
                    &mut samples,
                )
                .await;
                id += 1;
                let writer_ns = writing.join().unwrap().as_nanos().to_string();
                let race = &concurrent["result"]["structuredContent"];
                assert!(race["discovery"].is_object() || race["error"] == "stale_basis");
                let stale = rpc(
                    &mut client,
                    call(id, "cartograph_discover", args),
                    "discovery_after_writer_commit",
                    &mut samples,
                )
                .await;
                assert_eq!(stale["result"]["structuredContent"]["error"], "stale_basis");
                writer_observation = json!({"write_wall_ns":writer_ns,"wal_file_bytes_before":wal_before,"wal_file_bytes_after":wal_bytes(&wal_path),"racing_read_outcome":if race["discovery"].is_object(){"completed_before_or_during_writer"}else{"stale_basis"},"postcommit_outcome":"stale_basis","note":"separate SQLite connection; barrier aligns starts, not proof of lock overlap; WAL file length is not logical retained bytes"});
            }
            let row = json!({"repeat":repeat,"connect_wall_ns":connect_ns,"auth_ack_wall_ns":auth_ns,"auth_request_frame_bytes_including_lf":auth_request_bytes,"auth_response_frame_bytes_including_lf":frame(&ack).len(),"initialized_notification_frame_bytes_including_lf":frame(&notification).len(),"initialize_wall_ns":ready_ns,"discovery_total_wall_ns":scan_ns,"scanned_node_rows":scanned,"loaded_node_rows":loaded,"stable_result":stable,"capabilities":cap,"samples":samples,"writer":writer_observation});
            budget.revoke();
            revoke.send(true).unwrap();
            assert_eq!(task.await.unwrap(), Err(ServeError::Revoked));
            assert!(client.read().await.unwrap().is_none());
            for _ in 0..100 {
                if budget.is_idle().unwrap() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert!(budget.is_idle().unwrap());
            rounds.push(row);
        }
        phase("sessions_done");
        let result = json!({"schema_version":1,"fixture":{"nodes":NODES,"edges":NODES-1,"combined_rows":NODES*2-1,"seed":"ascending n000000..n060000 chain CALLS","graph_setup_ns":setup_ns},"transport":"real temporary UnixListener/Connection::connect, peer UID check, capability auth, serve_connection, Worker, ScopedDispatcher/ScopedReadSession","limits":{"frame_bytes_before_lf":FRAME,"scan_rows":WINDOW,"max_candidates":3,"request_ids":64,"session_seconds":300,"frame_deadline_seconds":60,"workers":1,"worker_reservation_model":SCOPED_WORKER_MODEL,"logical_reserved_bytes":policy.reserved_bytes.get(),"cumulative_validation_allowance_bytes":1_048_576,"validation_charge_per_dispatch":policy.evidence.max_file_validation_bytes.get()},"complete_worker_refusal":{"outcome":"ReadError::Limit","wall_ns":complete_ns,"path":"direct Worker::start, no implicit scoped fallback"},"rounds":rounds,"scope":{"providers":false,"real_target":false,"retained_positive_evidence":false,"partitions":false,"artifact_content":false,"listener_activation_ui":false,"multi_connection_contention":false,"logical_reservation_is_rss":false,"row_raw_or_canonical_bytes_exposed_by_mcp":false},"notes":["Discovery scans nodes only; edges are selected exactly with endpoint closure.","Memory measurement belongs to runner: process baseline/peak include test harness and allocator; not a worker ceiling.","Framed bytes are production mcp_wire reserialization including LF; no raw socket syscall instrumentation.","Three repetitions share one process and graph; not cold-cache measurements."]});
        println!("CARTOGRAPH_MCP_MEASURE_RESULT {result}");
    });
}
