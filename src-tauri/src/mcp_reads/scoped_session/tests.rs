// AC-0305 / T-0305: scoped identity, projection and discovery.
use super::*;
use core_graph::{Edge, GraphPatch, GraphStore, Node};
use serde_json::json;
fn node(id: &str, label: &str) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        props: json!({"name":id}),
    }
}
fn with_fixture(test: impl FnOnce(&mut ScopedReadContext<'_>)) {
    let temp = tempfile::tempdir().unwrap();
    let graph = Mutex::new(SqliteGraphStore::open_in_memory().unwrap());
    let decisions = Mutex::new(DecisionLog::open(temp.path().join("decisions.db")).unwrap());
    let mut scope = || Ok(Vec::new());
    test(&mut ScopedReadContext {
        graph: &graph,
        decisions: &decisions,
        scope: &mut scope,
    });
}
fn session(ctx: &mut ScopedReadContext<'_>) -> ScopedReadSession {
    ScopedReadSession::acquire(
        ctx,
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        spec::ExportMode::VerifiedOnly,
    )
    .unwrap()
}
fn request(query: DiscoveryQuery, rows: usize) -> ScopedDiscoveryRequest {
    ScopedDiscoveryRequest {
        query,
        labels: Vec::new(),
        max_scan_rows: rows,
        max_candidates: 10,
        max_bytes: context_hub::MAX_RESPONSE_BYTES,
        cursor: None,
    }
}
#[test]
fn scoped_session_reads_above_complete_graph_ceiling_and_stales_on_mutation() {
    with_fixture(|ctx| {
        let patch = GraphPatch {
            upsert_nodes: (0..50_001)
                .map(|i| node(&format!("n{i:06}"), "Symbol"))
                .collect(),
            ..GraphPatch::default()
        };
        ctx.graph
            .lock()
            .unwrap()
            .apply_patch_if_snapshot_matches(&(vec![], vec![]), &patch)
            .unwrap();
        assert!(
            ctx.graph
                .lock()
                .unwrap()
                .read_snapshot_bounded(SnapshotReadLimits::default())
                .is_err()
        );
        let session = session(ctx);
        let page = session
            .discover(
                ctx,
                request(
                    DiscoveryQuery::Identifier {
                        id: "n000002".into(),
                    },
                    3,
                ),
            )
            .unwrap();
        assert_eq!(page.scanned_rows, 3);
        assert_eq!(page.candidates.len(), 1);
        assert_eq!(page.remaining, Remaining::More);
        assert!(page.session_basis_id.starts_with("mcp-scoped-session-v1:"));
        assert!(page.query_basis_id.starts_with("scoped-read-basis-v1:"));
        assert_eq!(page.candidates[0].outgoing_edges, None);
        let key = FactKey::Node {
            id: "n000002".into(),
        };
        assert!(
            session
                .detail(
                    ctx,
                    session.basis_id(),
                    &key,
                    context_hub::MAX_RESPONSE_BYTES
                )
                .unwrap()
                .fact
                .is_some()
        );
        assert_eq!(
            session
                .authorize_evidence(ctx, session.basis_id(), &key)
                .unwrap()
                .fact(),
            &key
        );
        ctx.graph
            .lock()
            .unwrap()
            .put_node(&node("changed", "Symbol"))
            .unwrap();
        assert_eq!(session.check_current(ctx), Err(ReadError::StaleBasis));
    });
}
#[test]
fn zero_match_window_and_output_stop_keep_exact_raw_key_continuation() {
    with_fixture(|ctx| {
        for id in ["a", "b", "c"] {
            ctx.graph
                .lock()
                .unwrap()
                .put_node(&node(id, "Symbol"))
                .unwrap();
        }
        let session = session(ctx);
        let first = session
            .discover(ctx, request(DiscoveryQuery::Name { name: "c".into() }, 1))
            .unwrap();
        assert!(first.candidates.is_empty());
        assert_eq!(first.remaining, Remaining::More);
        assert_eq!(first.next_cursor.unwrap().after_id, "a");
        let mut req = request(DiscoveryQuery::Text { text: "".into() }, 3);
        assert!(session.discover(ctx, req).is_err());
        req = request(DiscoveryQuery::Text { text: "a".into() }, 3);
        req.max_bytes = 1;
        assert!(matches!(session.discover(ctx, req), Err(ReadError::Limit)));
        let mut req = request(DiscoveryQuery::Name { name: "c".into() }, 1);
        req.cursor = Some(ScopedCursor {
            query_basis_id: first.query_basis_id,
            after_id: "a".into(),
        });
        let second = session.discover(ctx, req).unwrap();
        assert!(second.candidates.is_empty());
        assert_eq!(second.next_cursor.unwrap().after_id, "b");
        let mut arbitrary = request(DiscoveryQuery::Name { name: "c".into() }, 1);
        arbitrary.cursor = Some(ScopedCursor {
            query_basis_id: second.query_basis_id,
            after_id: "zzz".into(),
        });
        let tail = session.discover(ctx, arbitrary).unwrap();
        assert_eq!(tail.remaining, Remaining::Exhausted);
        assert!(!tail.complete_system);
        assert!(tail.remaining_rows.is_none());
        assert!(tail.candidates.is_empty());
        let detail = session
            .detail(
                ctx,
                session.basis_id(),
                &FactKey::Node {
                    id: "missing".into(),
                },
                context_hub::MAX_RESPONSE_BYTES,
            )
            .unwrap();
        assert!(detail.fact.is_none());
        assert!(!detail.absence_claim);
        assert!(!detail.complete_system);
    });
}
#[test]
fn hidden_business_rule_endpoint_outside_edge_probe_suppresses_relation() {
    with_fixture(|ctx| {
        let rule = Node {
            id: "rule".into(),
            label: "BusinessRule".into(),
            props: json!({"prov":{"tier":"Agentic","confidence_tier":"InferredWeak","evidence":[],"content_hash":"x","method":"x"}}),
        };
        // Use the production provenance constructor so policy filtering is real.
        let prov = core_prov::Provenance::new(
            core_prov::Tier::Agentic,
            core_prov::ConfidenceTier::InferredWeak,
            vec![],
            "fixture",
            b"rule",
        )
        .unwrap();
        let rule = Node {
            props: json!({"prov":prov}),
            ..rule
        };
        let mut graph = ctx.graph.lock().unwrap();
        graph.put_node(&node("a", "Symbol")).unwrap();
        graph.put_node(&rule).unwrap();
        graph
            .put_edge(&Edge {
                src: "a".into(),
                dst: "rule".into(),
                label: "CALLS".into(),
                props: json!({}),
            })
            .unwrap();
        drop(graph);
        let session = session(ctx);
        let key = FactKey::Edge {
            source: "a".into(),
            label: "CALLS".into(),
            destination: "rule".into(),
        };
        assert!(
            session
                .detail(
                    ctx,
                    session.basis_id(),
                    &key,
                    context_hub::MAX_RESPONSE_BYTES
                )
                .unwrap()
                .fact
                .is_none()
        );
        assert!(matches!(
            session.authorize_evidence(ctx, session.basis_id(), &key),
            Err(ReadError::InvalidRequest)
        ));
    });
}

#[test]
fn exact_response_admission_never_advances_past_a_blocked_candidate() {
    with_fixture(|ctx| {
        for id in ["item1", "item2", "item3"] {
            ctx.graph
                .lock()
                .unwrap()
                .put_node(&node(id, "Symbol"))
                .unwrap();
        }
        let session = session(ctx);
        let mut one = request(
            DiscoveryQuery::Text {
                text: "ITEM".into(),
            },
            3,
        );
        one.max_candidates = 1;
        let first = session.discover(ctx, one).unwrap();
        let exact = context_hub::logical::count(&first).unwrap();
        let mut bounded = request(
            DiscoveryQuery::Text {
                text: "item".into(),
            },
            3,
        );
        bounded.max_bytes = exact;
        let result = session.discover(ctx, bounded).unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.next_cursor.unwrap().after_id, "item1");
        assert_eq!(result.scanned_rows, 1);
        assert_eq!(result.loaded_rows, 3);
        let mut too_small = request(
            DiscoveryQuery::Text {
                text: "item".into(),
            },
            3,
        );
        too_small.max_bytes = exact - 1;
        assert!(matches!(
            session.discover(ctx, too_small),
            Err(ReadError::Limit)
        ));
        let mut wrong_query = request(
            DiscoveryQuery::Name {
                name: "item1".into(),
            },
            3,
        );
        wrong_query.cursor = first.next_cursor;
        assert!(matches!(
            session.discover(ctx, wrong_query),
            Err(ReadError::StaleBasis)
        ));
    });
}

#[test]
fn rejection_change_stales_session_without_a_graph_mutation() {
    with_fixture(|ctx| {
        let session = session(ctx);
        let before = ctx.graph.lock().unwrap().read_revision().unwrap();
        let provenance = core_prov::Provenance::new(
            core_prov::Tier::Agentic,
            core_prov::ConfidenceTier::InferredWeak,
            vec![core_prov::EvidenceRef {
                repo: "fixture/repo".into(),
                path: "source.ts".into(),
                byte_start: 0,
                byte_end: 1,
                commit_sha: "fixed".into(),
            }],
            "fixture",
            b"decision",
        )
        .unwrap();
        ctx.decisions
            .lock()
            .unwrap()
            .record_assertion(
                &agents::CuratableAssertion {
                    subject_id: "proposal".into(),
                    summary: "proposed".into(),
                    provenance,
                },
                agents::AssertionDecision::Rejected,
                None,
            )
            .unwrap();
        assert_eq!(ctx.graph.lock().unwrap().read_revision().unwrap(), before);
        assert_eq!(session.check_current(ctx), Err(ReadError::StaleBasis));
    });
}

#[test]
fn discovery_is_a_lean_handle_and_detail_carries_the_selected_properties() {
    with_fixture(|ctx| {
        ctx.graph.lock().unwrap().put_node(&Node{id:"selected".into(),label:"Symbol".into(),props:json!({"name":"selected","implementation":"large source-bearing property remains in detail"})}).unwrap();
        let session = session(ctx);
        let page = session
            .discover(
                ctx,
                request(
                    DiscoveryQuery::Identifier {
                        id: "selected".into(),
                    },
                    1,
                ),
            )
            .unwrap();
        let output = serde_json::to_value(&page).unwrap();
        let candidate = &output["candidates"][0];
        assert!(candidate.get("fact").is_none());
        assert!(candidate.get("properties").is_none());
        assert!(
            !serde_json::to_string(&output)
                .unwrap()
                .contains("source-bearing")
        );
        let detail = session
            .detail(
                ctx,
                session.basis_id(),
                &FactKey::Node {
                    id: "selected".into(),
                },
                context_hub::MAX_RESPONSE_BYTES,
            )
            .unwrap();
        assert_eq!(
            detail.fact.unwrap().properties["implementation"],
            "large source-bearing property remains in detail"
        );
    });
}
