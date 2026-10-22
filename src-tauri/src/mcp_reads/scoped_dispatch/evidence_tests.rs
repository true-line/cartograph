//! Dispatcher-to-production retained capture/receipt acceptance, no Tauri/provider.
use super::*;
use context_hub::coverage::ScopedRepo;
use core_graph::source::FactKey;
const SOURCE: &str =
    "export function retained(flag: boolean) { if (flag) return false; return true; }\n";
struct RetainedFixture {
    _temp: tempfile::TempDir,
    state: AppState,
    root: std::path::PathBuf,
    scopes: Vec<ScopedRepo>,
    fact: Value,
}
impl RetainedFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");
        let root = temp.path().join("checkout");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("source.ts"), SOURCE).unwrap();
        let state = crate::registered_source_tests::app_state(&private);
        let source = crate::register_local_source(&state, &root).unwrap();
        let prepared = state
            .primary_sources
            .prepare(&source, source.root(), &["client".into()])
            .unwrap();
        let (facts, receipts) = adapters_lang_ts::captured::extract_file(
            prepared
                .capture
                .as_ref()
                .unwrap()
                .file("source.ts")
                .unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        let fact = receipts
            .iter()
            .find_map(|r| match r.fact_key() {
                FactKey::Node { id } => Some(json!({"kind":"node","id":id})),
                _ => None,
            })
            .unwrap();
        state
            .primary_sources
            .persist(&prepared, &source, &receipts)
            .unwrap();
        let bindings = crate::primary_source::matching_bindings(&facts, &receipts);
        crate::load_into_graph_with_bindings(
            &mut state.graph.lock().unwrap(),
            &facts,
            &source.repo_key,
            source.root(),
            "workdir",
            &bindings,
            None,
        )
        .unwrap();
        drop(prepared);
        let scopes = vec![ScopedRepo {
            repo_key: source.repo_key,
            registered_source_id: Some(source.source_id),
            config_hash: "fixture-config".into(),
            layer_hints: None,
        }];
        Self {
            _temp: temp,
            state,
            root,
            scopes,
            fact,
        }
    }
}
fn start<'a>(
    state: &'a AppState,
    ctx: &mut ScopedReadContext<'_>,
    session_id: &str,
) -> (ScopedDispatcher<'a>, String) {
    let limits = super::super::super::worker::tests::limits();
    let session = ScopedReadSession::acquire(
        ctx,
        limits.graph,
        limits.coverage,
        spec::ExportMode::BestEffort,
    )
    .unwrap();
    let basis = session.basis_id().to_owned();
    let mut dispatch = ScopedDispatcher::new(
        session,
        session_id.into(),
        limits.dispatch,
        state,
        limits.evidence,
    )
    .unwrap();
    assert!(matches!(dispatch.receive(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"retained-fixture","version":"1"}}}),ctx),DispatchAction::Respond(_)));
    assert_eq!(
        dispatch.receive(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            ctx
        ),
        DispatchAction::Ignore
    );
    (dispatch, basis)
}
fn read_args(session_id: &str, basis: &str, target: &Value, cap: usize) -> Value {
    json!({"session_id":session_id,"session_basis_id":basis,"mode":"read","kind":"retained_primary_source","target":target,"max_bytes":cap})
}

#[test]
fn scoped_dispatcher_real_retained_descriptor_and_read_survive_checkout_removal() {
    // AC-0288/#731: exercise public protocol wiring and full response bytes,
    // not just the lower-level evidence resolver or a fabricated target.
    let f = RetainedFixture::new();
    let mut scope = || Ok(f.scopes.clone());
    let mut ctx = ScopedReadContext {
        graph: &f.state.graph,
        decisions: &f.state.decisions,
        scope: &mut scope,
    };
    let (mut dispatch, basis) = start(&f.state, &mut ctx, "retained-session");
    let description = super::call(
        &mut dispatch,
        &mut ctx,
        2,
        "cartograph_evidence",
        json!({"session_id":"retained-session","session_basis_id":basis,"mode":"describe","kind":"retained_primary_source","fact":f.fact,"ordinal":0,"max_bytes":65536}),
    );
    assert!(serde_json::to_vec(&description).unwrap().len() <= 65536);
    assert_eq!(description["result"]["isError"], false);
    let target = description["result"]["structuredContent"]["evidence"]["target"].clone();
    assert_eq!(target["basis_id"], basis);
    assert!(
        target["target_id"]
            .as_str()
            .unwrap()
            .starts_with("mcp-scoped-evidence-target-v1:")
    );
    assert!(
        serde_json::from_value::<agents::investigation::InvestigationInspectionTarget>(
            target.clone()
        )
        .is_err()
    );
    let begin = target["captured"]["byte_start"].as_u64().unwrap() as usize;
    let end = target["captured"]["byte_end"].as_u64().unwrap() as usize;
    std::fs::remove_dir_all(&f.root).unwrap();
    let response = super::call(
        &mut dispatch,
        &mut ctx,
        3,
        "cartograph_evidence",
        read_args("retained-session", &basis, &target, 65536),
    );
    assert_eq!(response["result"]["isError"], false);
    let evidence = &response["result"]["structuredContent"]["evidence"];
    assert_eq!(evidence["text"], &SOURCE[begin..end]);
    assert_eq!(evidence["target"], target);
    assert_eq!(evidence["scope"], "primary_source_only");
    assert_eq!(evidence["input_closure"], "input_closure_not_established");
    let actual_wire = serde_json::to_vec(&response).unwrap().len();
    // Retained read admission deliberately reserves worst-case JSON escaping
    // before touching source bytes. Exercise that documented preflight boundary,
    // without claiming every actual-wire-sized request must be admitted.
    let escaped_text = serde_json::to_vec(&evidence["text"]).unwrap().len() - 2;
    let admitted_cap = actual_wire - escaped_text + 6 * (end - begin);
    let exact_response = super::call(
        &mut dispatch,
        &mut ctx,
        4,
        "cartograph_evidence",
        read_args("retained-session", &basis, &target, admitted_cap),
    );
    assert_eq!(exact_response["result"]["isError"], false);
    assert!(serde_json::to_vec(&exact_response).unwrap().len() <= admitted_cap);
    let before = f.state.primary_sources.evidence_access_attempts();
    let limited = super::call(
        &mut dispatch,
        &mut ctx,
        5,
        "cartograph_evidence",
        read_args("retained-session", &basis, &target, admitted_cap - 1),
    );
    assert!(serde_json::to_vec(&limited).unwrap().len() < admitted_cap);
    assert_eq!(
        limited["result"]["structuredContent"]["error"],
        "response_limit"
    );
    assert_eq!(
        f.state.primary_sources.evidence_access_attempts(),
        before,
        "whole-envelope shortage precedes retained lease acquisition"
    );
    let mut wrong = target.clone();
    wrong["target_id"] = json!(format!("mcp-scoped-evidence-target-v1:{}", "0".repeat(64)));
    let invalid = super::call(
        &mut dispatch,
        &mut ctx,
        6,
        "cartograph_evidence",
        read_args("retained-session", &basis, &wrong, 65536),
    );
    assert_eq!(invalid["error"]["code"], -32602);
    assert_eq!(f.state.primary_sources.evidence_access_attempts(), before);
    let limited = super::call(
        &mut dispatch,
        &mut ctx,
        7,
        "cartograph_evidence",
        read_args("retained-session", &basis, &target, 256),
    );
    assert!(serde_json::to_vec(&limited).unwrap().len() <= 256);
    assert_eq!(
        limited["result"]["structuredContent"]["error"],
        "response_limit"
    );
    assert_eq!(f.state.primary_sources.evidence_access_attempts(), before);
}

#[test]
fn scoped_dispatcher_stale_target_cannot_be_reused_under_a_fresh_session() {
    let f = RetainedFixture::new();
    let mut scope = || Ok(f.scopes.clone());
    let mut ctx = ScopedReadContext {
        graph: &f.state.graph,
        decisions: &f.state.decisions,
        scope: &mut scope,
    };
    let (mut dispatch, basis) = start(&f.state, &mut ctx, "old-session");
    let described = super::call(
        &mut dispatch,
        &mut ctx,
        2,
        "cartograph_evidence",
        json!({"session_id":"old-session","session_basis_id":basis,"mode":"describe","kind":"retained_primary_source","fact":f.fact,"ordinal":0,"max_bytes":65536}),
    );
    let target = described["result"]["structuredContent"]["evidence"]["target"].clone();
    assert!(target.is_object());
    f.state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "unrelated-mutation".into(),
            label: "Symbol".into(),
            props: json!({}),
        })
        .unwrap();
    let before = f.state.primary_sources.evidence_access_attempts();
    let stale = super::call(
        &mut dispatch,
        &mut ctx,
        3,
        "cartograph_evidence",
        read_args("old-session", &basis, &target, 65536),
    );
    assert_eq!(stale["result"]["structuredContent"]["error"], "stale_basis");
    let (mut fresh, new_basis) = start(&f.state, &mut ctx, "fresh-session");
    assert_ne!(basis, new_basis);
    let stale = super::call(
        &mut fresh,
        &mut ctx,
        2,
        "cartograph_evidence",
        read_args("fresh-session", &new_basis, &target, 65536),
    );
    assert_eq!(stale["result"]["structuredContent"]["error"], "stale_basis");
    assert_eq!(f.state.primary_sources.evidence_access_attempts(), before);
    assert_eq!(
        fresh.receive(json!({"jsonrpc":"2.0","id":2,"method":"ping"}), &mut ctx),
        DispatchAction::Close
    );
}
