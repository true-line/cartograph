//! Shared recovered-context reads for the desktop transport (SPEC-01, H1).

use context_hub::coverage::{
    CoverageQualification, QualifiedRequest, QualifiedResponse, ScopedRepo,
};
use context_hub::{ContextSnapshot, QueryRequest, QueryResponse};
use core_graph::coverage::CoverageReadLimits;
use core_graph::{SnapshotReadLimits, SqliteGraphStore};
use std::sync::Mutex;
use tauri::Manager;

/// Copy one coherent graph revision, then release storage before preparing the
/// immutable query snapshot. This adapter never projects accepted proposals.
fn query_graph(
    graph: &Mutex<SqliteGraphStore>,
    request: QueryRequest,
) -> Result<QueryResponse, String> {
    request
        .validate_limits()
        .map_err(|error| error.to_string())?;
    let (nodes, edges) = {
        let graph = graph.lock().map_err(|error| error.to_string())?;
        graph.read_snapshot().map_err(|error| error.to_string())?
    };
    ContextSnapshot::new(nodes, edges)
        .and_then(|snapshot| snapshot.query(request))
        .map_err(|error| error.to_string())
}

/// Bounded, read-only context endpoint. The graph copy, canonicalization and
/// selection all run on the worker; no database work occurs on the caller.
#[tauri::command]
pub(crate) async fn query_context(
    request: QueryRequest,
    app: tauri::AppHandle,
) -> Result<QueryResponse, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        query_graph(&state.graph, request)
    })
    .await
}

/// The current registration and plugin basis of every registered repo (AC-0239).
/// Layer selections are not requested here, so a recorded inventory qualifies
/// exactly its own prior ingest.
pub(crate) fn coverage_scope<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &crate::AppState,
) -> Result<Vec<ScopedRepo>, String> {
    // The registry lock is released before plugin discovery reads settings.
    let sources = state
        .sources
        .lock()
        .map_err(|error| error.to_string())?
        .list()?;
    let registries = crate::event_registries_for(app)?;
    sources
        .iter()
        .map(|source| {
            let plugins = crate::active_plugins_for_root(app, state, source.root())?;
            Ok(ScopedRepo {
                repo_key: source.repo_key.clone(),
                registered_source_id: Some(source.source_id.clone()),
                config_hash: crate::coverage_config_hash(&plugins, &registries),
                layer_hints: None,
            })
        })
        .collect()
}

/// The scope held throughout a read bracketed by `before` and `after`. A repo
/// whose registration or plugin basis changed in between, or that was
/// registered only during the read, keeps no current registration and so can
/// never qualify as recorded; a repo removed during the read is unregistered.
fn stable_scope(before: &[ScopedRepo], after: Vec<ScopedRepo>) -> Vec<ScopedRepo> {
    after
        .into_iter()
        .map(|repo| {
            if before.contains(&repo) {
                repo
            } else {
                ScopedRepo {
                    registered_source_id: None,
                    ..repo
                }
            }
        })
        .collect()
}

/// Copy one graph revision with the inventories recorded alongside it, both
/// admitted under the store's read limits before either is materialized, then
/// serve the page under that qualification. The registrations and plugin basis
/// live outside the graph, so they are read before and after the graph copy
/// and only what held across it is current.
pub(crate) fn qualified_query_graph(
    graph: &Mutex<SqliteGraphStore>,
    mut scope: impl FnMut() -> Result<Vec<ScopedRepo>, String>,
    request: QualifiedRequest<QueryRequest>,
) -> Result<QualifiedResponse<QueryResponse>, String> {
    let before = scope()?;
    let ((nodes, edges), recorded) = {
        let graph = graph.lock().map_err(|error| error.to_string())?;
        graph
            .read_snapshot_with_coverage_bounded(
                SnapshotReadLimits::default(),
                CoverageReadLimits::default(),
            )
            .map_err(|error| error.to_string())?
    };
    let scope = stable_scope(&before, scope()?);
    let coverage =
        CoverageQualification::new(&scope, &recorded).map_err(|error| error.to_string())?;
    ContextSnapshot::new(nodes, edges)
        .and_then(|snapshot| snapshot.with_coverage(coverage))
        .and_then(|context| context.query(request))
        .map_err(|error| error.to_string())
}

/// Bounded context read qualified by recorded coverage (AC-0239). Unlike
/// [`query_context`], every page states which repos' coverage was recorded,
/// stale or unrecorded, and never claims exhaustive coverage.
#[tauri::command]
pub(crate) async fn query_qualified_context(
    request: QualifiedRequest<QueryRequest>,
    app: tauri::AppHandle,
) -> Result<QualifiedResponse<QueryResponse>, String> {
    crate::off_ui_thread(move || {
        request
            .request
            .validate_limits()
            .map_err(|error| error.to_string())?;
        let state = app.state::<crate::AppState>();
        qualified_query_graph(&state.graph, || coverage_scope(&app, &state), request)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graph::{Edge, GraphStore, Node};
    use std::sync::Arc;

    fn request() -> QueryRequest {
        serde_json::from_value(serde_json::json!({
            "scope": {"type": "all"},
            "kind": null,
            "labels": [],
            "max_facts": 1,
            "max_bytes": 16384,
            "cursor": null
        }))
        .unwrap()
    }

    #[test]
    fn context_query_reads_on_worker_without_mutating_graph() {
        // AC-0105: exercise the storage adapter on the same worker boundary as
        // the command; paging and sanitization must leave source facts intact.
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        graph
            .put_node(&Node {
                id: "order".into(),
                label: "Symbol".into(),
                props: serde_json::json!({"name": "order", "prov": {"invalid": true}}),
            })
            .unwrap();
        graph
            .put_edge(&Edge {
                src: "order".into(),
                dst: "order".into(),
                label: "CALLS".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        let before = (graph.all_nodes().unwrap(), graph.all_edges().unwrap());
        let graph = Arc::new(Mutex::new(graph));
        let worker_graph = Arc::clone(&graph);
        let caller = std::thread::current().id();
        let (worker, page) = tauri::async_runtime::block_on(crate::off_ui_thread(move || {
            Ok((
                std::thread::current().id(),
                query_graph(&worker_graph, request())?,
            ))
        }))
        .unwrap();
        assert_ne!(caller, worker);
        assert_eq!(
            serde_json::to_value(&page).unwrap()["view"],
            "recovered_graph"
        );
        assert_eq!(page.total_selected, 2);
        assert_eq!(page.facts.len(), 1);
        let mut next = request();
        next.cursor = page.next_cursor;
        let second = query_graph(&graph, next).unwrap();
        assert_eq!(second.snapshot_id, page.snapshot_id);
        assert_eq!(second.facts.len(), 1);
        assert!(second.next_cursor.is_none());
        let graph = graph.lock().unwrap();
        assert_eq!(
            before,
            (graph.all_nodes().unwrap(), graph.all_edges().unwrap())
        );
    }

    #[test]
    fn context_query_propagates_errors_without_mutating_graph() {
        // AC-0105: invalid requests fail through the worker/transport boundary.
        let graph = Mutex::new(SqliteGraphStore::open_in_memory().unwrap());
        let mut invalid = request();
        invalid.max_facts = 0;
        let error = tauri::async_runtime::block_on(crate::off_ui_thread(move || {
            let response = query_graph(&graph, invalid);
            assert_eq!(graph.lock().unwrap().node_count().unwrap(), 0);
            response
        }));
        assert!(error.is_err());
    }

    #[test]
    fn context_query_rejects_invalid_request_before_graph_access() {
        // AC-0105: even unavailable storage must not be touched to reject an
        // invalid budget. Otherwise malformed requests can rebuild a huge graph.
        let graph = Arc::new(Mutex::new(SqliteGraphStore::open_in_memory().unwrap()));
        let worker_graph = Arc::clone(&graph);
        assert!(
            std::thread::spawn(move || {
                let _guard = worker_graph.lock().unwrap();
                panic!("poison fixture storage");
            })
            .join()
            .is_err()
        );
        let mut invalid = request();
        invalid.max_facts = 0;
        let error = query_graph(&graph, invalid).unwrap_err();
        assert!(error.contains("max_facts"), "{error}");
    }

    #[test]
    fn qualified_context_query_reads_coverage_with_its_graph_revision() {
        // AC-0239: the page and its qualification come from one graph
        // revision; a scoped repo with nothing recorded is unrecorded, and a
        // continuation stays within the context that issued it. (T-0239)
        use context_hub::coverage::CoverageStatus;
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        let inventory = serde_json::json!({
            "inventory": "coverage@1", "repo": "acme/api", "commit": "a".repeat(40),
            "layer_hints": [], "producers": [{"id": "t0.adapter-ts", "state": "ran"}],
            "missing_inputs": [], "unsupported": [], "limited": false,
        });
        let record = core_graph::coverage::CoverageRecord {
            repo_key: "acme/api".into(),
            registered_source_id: Some("src:api".into()),
            ingest_id: "job:1".into(),
            config_hash: "config".into(),
            inventory_json: inventory.to_string(),
        };
        let expected = graph.read_snapshot().unwrap();
        let patch = core_graph::GraphPatch {
            upsert_nodes: ["a", "b"]
                .map(|id| Node {
                    id: id.into(),
                    label: "Module".into(),
                    props: serde_json::json!({}),
                })
                .to_vec(),
            ..core_graph::GraphPatch::default()
        };
        assert!(
            graph
                .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                    &expected,
                    &patch,
                    "acme/api",
                    &[],
                    Some(&record),
                )
                .unwrap()
        );
        let graph = Mutex::new(graph);
        let scope = ["acme/api", "acme/web"].map(|repo| ScopedRepo {
            repo_key: repo.into(),
            registered_source_id: Some(format!("src:{}", &repo[5..])),
            config_hash: "config".into(),
            layer_hints: None,
        });
        let first = qualified_query_graph(
            &graph,
            || Ok(scope.to_vec()),
            QualifiedRequest {
                request: request(),
                context_id: None,
            },
        )
        .unwrap();
        assert!(!first.coverage.exhaustive);
        assert_eq!(
            first
                .coverage
                .repos
                .iter()
                .map(|repo| (repo.repo.as_str(), repo.status))
                .collect::<Vec<_>>(),
            [
                ("acme/api", CoverageStatus::Recorded),
                ("acme/web", CoverageStatus::Unrecorded)
            ]
        );
        assert_eq!(first.response.facts.len(), 1);
        let mut next = request();
        next.cursor = first.response.next_cursor.clone();
        assert!(
            qualified_query_graph(
                &graph,
                || Ok(scope.to_vec()),
                QualifiedRequest {
                    request: next.clone(),
                    context_id: None,
                },
            )
            .is_err()
        );
        let second = qualified_query_graph(
            &graph,
            || Ok(scope.to_vec()),
            QualifiedRequest {
                request: next,
                context_id: Some(first.context_id.clone()),
            },
        )
        .unwrap();
        assert_eq!(second.context_id, first.context_id);
        assert!(second.response.next_cursor.is_none());

        // A registration or plugin basis that changes while the graph is
        // copied is not current, even though the copied basis still matches
        // the stored inventory; the drifted read has its own context.
        for drift in [
            |repo: &mut ScopedRepo| repo.config_hash = "config:next".into(),
            |repo: &mut ScopedRepo| repo.registered_source_id = Some("src:other".into()),
        ] {
            let mut reads = 0;
            let drifted = qualified_query_graph(
                &graph,
                || {
                    reads += 1;
                    let mut now = scope.to_vec();
                    if reads == 1 {
                        drift(&mut now[0]);
                    }
                    Ok(now)
                },
                QualifiedRequest {
                    request: request(),
                    context_id: None,
                },
            )
            .unwrap();
            assert_eq!(reads, 2);
            assert_eq!(drifted.coverage.repos[0].status, CoverageStatus::Stale);
            assert_ne!(drifted.context_id, first.context_id);
        }
    }
}
