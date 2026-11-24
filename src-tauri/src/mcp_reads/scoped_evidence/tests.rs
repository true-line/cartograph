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
        Self::with_source(
            "export function checkout(flag: boolean) { if (flag) return false; return true; }\n",
        )
    }
    fn with_source(source: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("source.ts"), source).unwrap();
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
        describe(
            &self.state,
            &self.proof(),
            BindingRole::Primary,
            0,
            64 * 1024,
            || self.check(),
        )
        .unwrap()
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

// AC-0581 / #1754: an MCP client receives redacted source text and is told so.
#[test]
fn scoped_evidence_text_is_redacted_before_it_leaves_the_host() {
    let f = Fixture::with_source(
        "export function connect() { const password = \"hunter2-hunter2\"; return password.length; }\n",
    );
    let description = f.describe();
    let response = read(&f.state, &f.proof(), &description.target, limits(), || {
        f.check()
    })
    .unwrap();
    assert!(response.text.contains("function connect()"));
    assert!(!response.text.contains("hunter2"));
    assert!(response.text.contains("[REDACTED]"));
    assert_eq!(
        response.redaction,
        super::super::evidence::EvidenceRedaction {
            policy: core_redact::TEXT_POLICY_VERSION,
            replacements: 1,
        }
    );
    let wire = serde_json::to_value(&response).unwrap();
    assert_eq!(wire["redaction"]["replacements"], 1);
    assert_eq!(
        wire["redaction"]["policy"],
        core_redact::TEXT_POLICY_VERSION
    );
    assert!(!wire.to_string().contains("hunter2"));

    // Text with nothing to redact is returned as captured and says so.
    let f = Fixture::new();
    let description = f.describe();
    let response = read(&f.state, &f.proof(), &description.target, limits(), || {
        f.check()
    })
    .unwrap();
    assert_eq!(response.redaction.replacements, 0);
    assert!(!response.text.is_empty() && !response.text.contains("[REDACTED]"));
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
        describe(
            &f.state,
            &f.proof(),
            BindingRole::Primary,
            0,
            64 * 1024,
            || f.check()
        ),
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
        describe(
            &f.state,
            &f.proof(),
            BindingRole::Primary,
            0,
            64 * 1024,
            || f.check()
        ),
        Err(ReadError::EvidenceInvalid)
    ));
}

#[test]
fn scoped_evidence_post_guard_qualification_check_rejects_before_receipt_access() {
    let f = Fixture::new();
    let proof = f.proof();
    let called = AtomicBool::new(false);
    assert!(matches!(
        describe(&f.state, &proof, BindingRole::Primary, 0, 64 * 1024, || {
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
        describe(
            &f.state,
            &proof,
            BindingRole::Primary,
            1024,
            64 * 1024,
            || panic!("invalid ordinal acquired evidence")
        ),
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

// AC-0306 / T-0306: exact scoped retained-source identity and classification.
#[test]
fn scoped_evidence_exact_reclaimed_receipt_is_forgotten_not_unknown_or_forged() {
    let f = Fixture::new();
    let description = f.describe();
    let proof = f.proof();
    let source_id = f.scopes[0].registered_source_id.as_ref().unwrap();
    let preview = crate::primary_source::preview(&f.state, source_id).unwrap();
    crate::primary_source::forget(&f.state, source_id, &preview.fingerprint).unwrap();
    // Supported forgetting leaves graph fact/binding revision intact.
    f.check().unwrap();
    assert!(matches!(
        f.state
            .primary_sources
            .receipts
            .lock()
            .unwrap()
            .receipt_status(&description.target.receipt_id)
            .unwrap(),
        crate::primary_source::ReceiptStatus::Reclaimed { .. }
    ));
    let db = rusqlite::Connection::open(f.private.join("retained-source/receipts.sqlite")).unwrap();
    db.execute(
        "UPDATE receipt_tombstones SET capture_ids='[]',capture_ids_truncated=1",
        [],
    )
    .unwrap();
    assert_eq!(
        describe(&f.state, &proof, BindingRole::Primary, 0, 65536, || f
            .check())
        .unwrap_err(),
        ReadError::EvidenceForgotten
    );
    assert_eq!(
        read(&f.state, &proof, &description.target, limits(), || f
            .check())
        .unwrap_err(),
        ReadError::EvidenceForgotten
    );
    for field in ["receipt", "digest", "source"] {
        let mut forged = description.target.clone();
        match field {
            "receipt" => forged.receipt_id = format!("ts-primary-v3:{}", "0".repeat(64)),
            "digest" => forged.fact_digest = format!("node-v1:{}", "0".repeat(64)),
            _ => {
                forged.registered_source_id = format!("src_{}", "f".repeat(32));
                forged.captured.file.source_id = forged.registered_source_id.clone();
            }
        }
        forged.target_id = forged.content_id().unwrap();
        assert_eq!(
            read(&f.state, &proof, &forged, limits(), || f.check()).unwrap_err(),
            ReadError::StaleBasis
        );
    }
    db.execute(
        "UPDATE receipt_tombstones SET source_id=?1",
        [format!("src_{}", "f".repeat(32))],
    )
    .unwrap();
    assert_eq!(
        describe(&f.state, &proof, BindingRole::Primary, 0, 65536, || f
            .check())
        .unwrap_err(),
        ReadError::EvidenceInvalid
    );
}

// AC-0306 / T-0306: exact scoped retained-source identity and classification.
#[test]
fn scoped_forgotten_status_storage_failure_and_graph_drift_do_not_become_absence() {
    let f = Fixture::new();
    let description = f.describe();
    let proof = f.proof();
    let source_id = f.scopes[0].registered_source_id.as_ref().unwrap();
    let preview = crate::primary_source::preview(&f.state, source_id).unwrap();
    crate::primary_source::forget(&f.state, source_id, &preview.fingerprint).unwrap();
    let db = rusqlite::Connection::open(f.private.join("retained-source/receipts.sqlite")).unwrap();
    db.execute_batch("DROP TABLE receipt_reclaims").unwrap();
    assert_eq!(
        describe(&f.state, &proof, BindingRole::Primary, 0, 65536, || f
            .check())
        .unwrap_err(),
        ReadError::Storage
    );
    f.state
        .graph
        .lock()
        .unwrap()
        .put_node(&core_graph::Node {
            id: "changed-after-forget".into(),
            label: "Module".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
    assert_eq!(
        read(&f.state, &proof, &description.target, limits(), || f
            .check())
        .unwrap_err(),
        ReadError::StaleBasis
    );
}

// AC-0306 / T-0306: exact scoped retained-source identity and classification.
#[test]
fn scoped_target_preserves_canonical_v1_v2_v3_receipt_history() {
    let f = Fixture::new();
    let original = f.describe().target;
    assert!(original.receipt_id.starts_with("ts-primary-v3:"));
    // v4 carries an input closure state (AC-1080).
    for version in [1, 2, 3, 4] {
        let mut target = original.clone();
        target.receipt_id = format!("ts-primary-v{version}:{}", "a".repeat(64));
        target.target_id = target.content_id().unwrap();
        target.validate().unwrap();
        target.receipt_id.push('\n');
        target.target_id = target.content_id().unwrap();
        assert_eq!(target.validate().unwrap_err(), ReadError::InvalidRequest);
    }
    for prefix in ["ts-primary-v0:", "ts-primary-v5:", "other:"] {
        let mut target = original.clone();
        target.receipt_id = format!("{prefix}{}", "a".repeat(64));
        target.target_id = target.content_id().unwrap();
        assert_eq!(target.validate().unwrap_err(), ReadError::InvalidRequest);
    }
}

// AC-0306: real production bounded receipt publication, no synthetic receipt facts.
#[test]
fn scoped_real_store_capacity_omission_is_guarded_and_stale_safe() {
    use core_graph::source::{OmissionLookup, ReceiptOmissionKind};
    use tauri::Manager;
    let temp = tempfile::tempdir().unwrap();
    let private = temp.path().join("private");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&private).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("rules.ts"), "export function alpha(ok: boolean) { if (!ok) return false; return true; }\nexport function beta() { return 2; }\n").unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(crate::registered_source_tests::app_state(&private));
    app.manage(crate::PreflightRuns::default());
    let state = app.state::<AppState>();
    state.primary_sources.receipts.lock().unwrap().capacity.0 = 1;
    let source = crate::register_local_source(&state, &root).unwrap();
    let operation = crate::source_operation(&state, vec![(source.clone(), false)]).unwrap();
    let (_, execution) = crate::start_job(&state, &source.ingest_job_kind()).unwrap();
    crate::run_ingest(&source, &operation, &execution, app.handle(), &state).unwrap();
    let scopes = vec![ScopedRepo {
        repo_key: source.repo_key.clone(),
        registered_source_id: Some(source.source_id.clone()),
        config_hash: "fixture-config".into(),
        layer_hints: None,
    }];
    let session = Fixture::session(&state, &scopes);
    let graph = state.graph.lock().unwrap();
    let (nodes, _) = graph.read_snapshot().unwrap();
    let revision = graph.read_revision().unwrap();
    let key = nodes
        .iter()
        .map(FactKey::from_node)
        .find(|key| {
            matches!(
                graph
                    .read_selected_facts_at_revision(
                        std::slice::from_ref(key),
                        &revision,
                        SnapshotReadLimits::default()
                    )
                    .unwrap()
                    .selections[0]
                    .omission,
                OmissionLookup::Omitted {
                    kind: ReceiptOmissionKind::StoreCapacity,
                    ..
                }
            )
        })
        .unwrap();
    drop(graph);
    let mut scope = || Ok(scopes.clone());
    let proof = session
        .authorize_evidence(
            &mut ScopedReadContext {
                graph: &state.graph,
                decisions: &state.decisions,
                scope: &mut scope,
            },
            session.basis_id(),
            &key,
        )
        .unwrap();
    let check = || {
        let mut scope = || Ok(scopes.clone());
        session.check_current(&mut ScopedReadContext {
            graph: &state.graph,
            decisions: &state.decisions,
            scope: &mut scope,
        })
    };
    let describe =
        || super::describe(&state, &proof, BindingRole::Primary, 0, 65536, check).unwrap_err();
    assert_eq!(
        describe(),
        ReadError::EvidenceOmitted(ReceiptOmissionKind::StoreCapacity)
    );
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            private
                .join("retained-source/locks")
                .join(format!("{}.lock", source.source_id)),
        )
        .unwrap();
    lock.try_lock().unwrap();
    assert_eq!(describe(), ReadError::Storage);
    drop(lock);
    assert_eq!(
        super::describe(&state, &proof, BindingRole::Primary, 0, 65536, || Err(
            ReadError::StaleBasis
        ))
        .unwrap_err(),
        ReadError::StaleBasis
    );
    // Test-only metadata changes retain the real producer's exact fact digest.
    // They exercise all closed reasons and unknown/malformed lookup outcomes.
    let db = rusqlite::Connection::open(private.join("graph.db")).unwrap();
    let FactKey::Node { id } = &key else {
        unreachable!()
    };
    for kind in [
        ReceiptOmissionKind::Declarations,
        ReceiptOmissionKind::Rules,
    ] {
        db.execute(
            "UPDATE source_node_omissions SET kind=?1 WHERE node_id=?2",
            rusqlite::params![kind.as_str(), id],
        )
        .unwrap();
        assert_eq!(describe(), ReadError::EvidenceOmitted(kind));
        let wire = crate::mcp_reads::dispatch::tool_error(
            &serde_json::Value::Null,
            ReadError::EvidenceOmitted(kind),
        );
        assert_eq!(
            wire["result"]["structuredContent"]["error"],
            "evidence_omitted"
        );
        assert_eq!(
            wire["result"]["structuredContent"]["omission_kind"],
            kind.as_str()
        );
    }
    // A lookup mutation between pinned observations is stale even though this
    // unsupported raw SQL does not advance the normal producer revision.
    let db_path = private.join("graph.db");
    let changed_id = id.clone();
    state
        .graph
        .lock()
        .unwrap()
        .set_source_binding_after_lookup_hook(move || {
            rusqlite::Connection::open(db_path)
                .unwrap()
                .execute(
                    "UPDATE source_node_omissions SET kind='store_capacity' WHERE node_id=?1",
                    [&changed_id],
                )
                .unwrap();
            Ok(())
        });
    assert_eq!(describe(), ReadError::StaleBasis);
    db.execute(
        "UPDATE source_node_omissions SET emitted_fact_digest=?1 WHERE node_id=?2",
        rusqlite::params![format!("node-v1:{}", "0".repeat(64)), id],
    )
    .unwrap();
    assert_eq!(describe(), ReadError::EvidenceUnavailable);
    db.execute("DELETE FROM source_node_omissions WHERE node_id=?1", [id])
        .unwrap();
    assert_eq!(describe(), ReadError::EvidenceUnavailable);
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&core_graph::Node {
            id: "mutation".into(),
            label: "Symbol".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
    assert_eq!(describe(), ReadError::StaleBasis);
}

#[test]
fn observed_kind_reads_the_input_span_only_in_its_own_role() {
    // AC-1051 (ADR-0079, #385): scoped evidence of kind
    // `retained_observed_input` resolves the fact's observed binding, names
    // the source's input identity and reads the cited span of the captured
    // state; the primary kind finds no declaration for the same fact, and a
    // target relabelled as the source's own is refused.
    let dir = tempfile::tempdir().unwrap();
    let system = dir.path().join("system");
    std::fs::create_dir_all(system.join("infra")).unwrap();
    std::fs::write(
        system.join("infra/main.tf"),
        "resource \"aws_sqs_queue\" \"jobs\" {\n  name = \"jobs\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        system.join("state.json"),
        r#"{"format_version":"1.0","values":{"root_module":{"resources":[
            {"address":"aws_sqs_queue.jobs","mode":"managed","type":"aws_sqs_queue","name":"jobs",
             "values":{"url":"https://sqs.example/a"},"sensitive_values":{}}]}}}"#,
    )
    .unwrap();
    let manifest = system.join("cartograph.system.toml");
    std::fs::write(
        &manifest,
        "[[repos]]\nurl = \"infra\"\nlayers = [\"infra\"]\nstate_json = \"state.json\"\n",
    )
    .unwrap();
    let app = crate::tests::preflight_test_app(dir.path());
    crate::add_system_blocking(
        manifest.to_string_lossy().into_owned(),
        app.handle().clone(),
    )
    .unwrap();
    let state: &AppState = tauri::Manager::state::<AppState>(app.handle()).inner();
    let source = state.sources.lock().unwrap().list().unwrap().remove(0);
    let key = FactKey::Node {
        id: format!("res:{}@aws_sqs_queue.jobs", source.repo_key),
    };
    let scopes = vec![ScopedRepo {
        repo_key: source.repo_key.clone(),
        registered_source_id: Some(source.source_id.clone()),
        config_hash: "fixture-config".into(),
        layer_hints: None,
    }];
    let session = Fixture::session(state, &scopes);
    let mut scope = || Ok(scopes.clone());
    let proof = session
        .authorize_evidence(
            &mut ScopedReadContext {
                graph: &state.graph,
                decisions: &state.decisions,
                scope: &mut scope,
            },
            session.basis_id(),
            &key,
        )
        .unwrap();
    let check = || {
        let mut scope = || Ok(scopes.clone());
        session.check_current(&mut ScopedReadContext {
            graph: &state.graph,
            decisions: &state.decisions,
            scope: &mut scope,
        })
    };
    let described = describe(state, &proof, BindingRole::Observed, 0, 65536, check).unwrap();
    assert_eq!(described.scope, "observed_input_only");
    assert_eq!(described.total_ranges, 1);
    assert_eq!(
        described.target.registered_source_id,
        format!("input/{}", source.source_id)
    );
    assert!(described.target.receipt_id.starts_with("obs-input-v1:"));
    assert_eq!(described.target.binding_role(), BindingRole::Observed);
    assert_eq!(described.target.source.path, "state.json");
    assert!(matches!(
        describe(state, &proof, BindingRole::Primary, 0, 65536, check),
        Err(ReadError::EvidenceUnavailable)
    ));
    let span = read(state, &proof, &described.target, limits(), check).unwrap();
    assert_eq!(span.scope, "observed_input_only");
    assert_eq!(span.text, "\"aws_sqs_queue.jobs\"");
    let mut relabelled = described.target.clone();
    relabelled.registered_source_id = source.source_id.clone();
    relabelled.captured.file.source_id = source.source_id.clone();
    relabelled.target_id = relabelled.content_id().unwrap();
    assert!(read(state, &proof, &relabelled, limits(), check).is_err());
}
