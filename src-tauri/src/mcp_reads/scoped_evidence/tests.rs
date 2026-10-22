// AC-0306 / T-0306: retained evidence identity and bounded access.
//! AC-0288/#731: real local capture/receipt storage, never provider or checkout fallback.
use super::super::scoped_session::{ScopedReadContext, ScopedReadSession};
use super::*;
use context_hub::coverage::ScopedRepo;
use core_graph::{GraphStore, SnapshotReadLimits, coverage::CoverageReadLimits, source::FactKey};
use std::sync::atomic::{AtomicBool, Ordering};

struct Fixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    private: std::path::PathBuf,
    state: AppState,
    scopes: Vec<ScopedRepo>,
    session: ScopedReadSession,
    key: FactKey,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("source.ts"),
            "export function checkout(flag: boolean) { if (flag) return false; return true; }\n",
        )
        .unwrap();
        let state = crate::registered_source_tests::app_state(&private);
        let registered = crate::register_local_source(&state, &root).unwrap();
        let layers = vec!["client".to_owned()];
        let prepared = state
            .primary_sources
            .prepare(&registered, registered.root(), &layers)
            .unwrap();
        let (facts, receipts) = adapters_lang_ts::captured::extract_file(
            prepared
                .capture
                .as_ref()
                .unwrap()
                .file("source.ts")
                .unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &registered.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        let key = receipts
            .iter()
            .find(|receipt| matches!(receipt.fact_key(), FactKey::Node { .. }))
            .unwrap()
            .fact_key()
            .clone();
        state
            .primary_sources
            .persist(&prepared, &registered, &receipts)
            .unwrap();
        let bindings = crate::primary_source::matching_bindings(&facts, &receipts);
        crate::load_into_graph_with_bindings(
            &mut state.graph.lock().unwrap(),
            &facts,
            &registered.repo_key,
            registered.root(),
            "workdir",
            &bindings,
            None,
        )
        .unwrap();
        drop(prepared);
        let scopes = vec![ScopedRepo {
            repo_key: registered.repo_key,
            registered_source_id: Some(registered.source_id),
            config_hash: "fixture-config".into(),
            layer_hints: None,
        }];
        let session = Self::session(&state, &scopes);
        Self {
            _temp: temp,
            root,
            private,
            state,
            scopes,
            session,
            key,
        }
    }
    fn session(state: &AppState, scopes: &[ScopedRepo]) -> ScopedReadSession {
        let mut scope = || Ok(scopes.to_vec());
        ScopedReadSession::acquire(
            &mut ScopedReadContext {
                graph: &state.graph,
                decisions: &state.decisions,
                scope: &mut scope,
            },
            SnapshotReadLimits::default(),
            CoverageReadLimits::default(),
            spec::ExportMode::BestEffort,
        )
        .unwrap()
    }
    fn proof(&self) -> ScopedEvidenceProof {
        let mut scope = || Ok(self.scopes.clone());
        self.session
            .authorize_evidence(
                &mut ScopedReadContext {
                    graph: &self.state.graph,
                    decisions: &self.state.decisions,
                    scope: &mut scope,
                },
                self.session.basis_id(),
                &self.key,
            )
            .unwrap()
    }
    fn check(&self) -> Result<(), ReadError> {
        let mut scope = || Ok(self.scopes.clone());
        self.session.check_current(&mut ScopedReadContext {
            graph: &self.state.graph,
            decisions: &self.state.decisions,
            scope: &mut scope,
        })
    }
    fn describe(&self) -> ScopedEvidenceDescription {
        describe(&self.state, &self.proof(), 0, 64 * 1024, || self.check()).unwrap()
    }
}
fn limits() -> EvidenceLimits {
    EvidenceLimits {
        max_file_validation_bytes: source_capture::MAX_FILE_BYTES,
        max_span_bytes: 8192,
        max_response_bytes: 64 * 1024,
    }
}

#[test]
fn scoped_evidence_real_receipt_survives_checkout_removal_and_cannot_be_full_target() {
    let f = Fixture::new();
    let description = f.describe();
    assert!(
        description
            .target
            .basis_id
            .starts_with("mcp-scoped-session-v1:")
    );
    assert!(
        description
            .target
            .target_id
            .starts_with("mcp-scoped-evidence-target-v1:")
    );
    let wire = serde_json::to_value(&description.target).unwrap();
    assert_eq!(
        ScopedEvidenceTarget::decode(&wire).unwrap(),
        description.target
    );
    assert!(
        serde_json::from_value::<agents::investigation::InvestigationInspectionTarget>(wire)
            .is_err()
    );
    std::fs::remove_dir_all(&f.root).unwrap();
    let response = read(&f.state, &f.proof(), &description.target, limits(), || {
        f.check()
    })
    .unwrap();
    assert!(!response.text.is_empty());
    assert_eq!(response.scope, "primary_source_only");
    assert_eq!(response.input_closure, "input_closure_not_established");
    assert_eq!(response.target, description.target);
}

#[test]
fn scoped_evidence_tampered_identity_limits_and_stale_generation_precede_text_read() {
    let f = Fixture::new();
    let description = f.describe();
    let called = AtomicBool::new(false);
    let mut target = description.target.clone();
    target.fact_digest = format!("node-v1:{}", "0".repeat(64));
    target.target_id = target.content_id().unwrap();
    assert!(matches!(
        read(&f.state, &f.proof(), &target, limits(), || {
            called.store(true, Ordering::SeqCst);
            f.check()
        }),
        Err(ReadError::StaleBasis)
    ));
    let mut small = limits();
    small.max_response_bytes = 1;
    called.store(false, Ordering::SeqCst);
    assert!(matches!(
        read(&f.state, &f.proof(), &description.target, small, || {
            called.store(true, Ordering::SeqCst);
            Ok(())
        }),
        Err(ReadError::Limit)
    ));
    assert!(!called.load(Ordering::SeqCst));
    let proof = f.proof();
    f.state
        .graph
        .lock()
        .unwrap()
        .put_node(&core_graph::Node {
            id: "unrelated".into(),
            label: "Symbol".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
    assert!(matches!(
        read(&f.state, &proof, &description.target, limits(), || f
            .check()),
        Err(ReadError::StaleBasis)
    ));
}

#[test]
fn scoped_evidence_missing_invalid_receipt_and_binding_are_distinct_without_absence_claim() {
    let mut f = Fixture::new();
    let description = f.describe();
    let connection =
        rusqlite::Connection::open(f.private.join("retained-source/receipts.sqlite")).unwrap();
    connection
        .execute(
            "DELETE FROM receipts WHERE id=?1",
            [&description.target.receipt_id],
        )
        .unwrap();
    assert!(matches!(
        describe(&f.state, &f.proof(), 0, 64 * 1024, || f.check()),
        Err(ReadError::EvidenceUnavailable)
    ));
    // No capture/source/path heuristic turns this missing receipt into omitted.
    let connection = rusqlite::Connection::open(f.private.join("graph.db")).unwrap();
    connection
        .execute(
            "UPDATE source_node_bindings SET emitted_fact_digest=?1 WHERE node_id=?2",
            rusqlite::params![
                format!("node-v1:{}", "0".repeat(64)),
                match &f.key {
                    FactKey::Node { id } => id,
                    _ => unreachable!(),
                }
            ],
        )
        .unwrap();
    f.session = Fixture::session(&f.state, &f.scopes);
    assert!(matches!(
        describe(&f.state, &f.proof(), 0, 64 * 1024, || f.check()),
        Err(ReadError::EvidenceInvalid)
    ));
}

#[test]
fn scoped_evidence_post_guard_qualification_check_rejects_before_receipt_access() {
    let f = Fixture::new();
    let proof = f.proof();
    let called = AtomicBool::new(false);
    assert!(matches!(
        describe(&f.state, &proof, 0, 64 * 1024, || {
            called.store(true, Ordering::SeqCst);
            let mut changed = f.scopes.clone();
            changed[0].config_hash.push_str("-changed");
            let mut scope = || Ok(changed.clone());
            f.session.check_current(&mut ScopedReadContext {
                graph: &f.state.graph,
                decisions: &f.state.decisions,
                scope: &mut scope,
            })
        }),
        Err(ReadError::StaleBasis)
    ));
    assert!(called.load(Ordering::SeqCst));
    assert!(matches!(
        describe(&f.state, &proof, 1024, 64 * 1024, || panic!(
            "invalid ordinal acquired evidence"
        )),
        Err(ReadError::InvalidRequest)
    ));
}

#[test]
fn scoped_evidence_target_ingress_bounds_before_clone_and_rejects_unknown_nested_fields() {
    let f = Fixture::new();
    let target = f.describe().target;
    let mut wire = serde_json::to_value(&target).unwrap();
    wire["source"]["extra"] = serde_json::json!(true);
    assert!(matches!(
        ScopedEvidenceTarget::decode(&wire),
        Err(ReadError::InvalidRequest)
    ));
    let mut wire = serde_json::to_value(&target).unwrap();
    wire["source"]["path"] = serde_json::json!("x".repeat(target::MAX_TARGET_BYTES));
    assert!(matches!(
        ScopedEvidenceTarget::decode(&wire),
        Err(ReadError::Limit)
    ));
    let mut wire = serde_json::to_value(&target).unwrap();
    wire["basis_id"] = serde_json::json!(format!("context-v1:{}", "a".repeat(64)));
    assert!(matches!(
        ScopedEvidenceTarget::decode(&wire),
        Err(ReadError::InvalidRequest)
    ));
    assert!(ScopedEvidenceTarget::decode(&serde_json::json!({"schema_version": 2})).is_err());
}
