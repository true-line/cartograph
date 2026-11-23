//! Registered identity through the real intake, reader and durable-state seams.

use crate::*;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use rusqlite::Connection;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).unwrap();
    crate::paths::canonicalize(path).unwrap()
}

pub(crate) fn app_state(app_data: &Path) -> AppState {
    let state_path = app_data.join("state.db");
    // Match startup ordering: the registry migration sees the actual findings
    // table, and completes before any new intake can publish registered rows.
    let findings = FindingStore::open(&state_path).unwrap();
    let sources = SourceRegistry::open(&state_path, app_data).unwrap();
    AppState {
        graph: Mutex::new(SqliteGraphStore::open(app_data.join("graph.db")).unwrap()),
        jobs: Mutex::new(JobStore::open(&state_path).unwrap()),
        investigations: crate::investigations::InvestigationRuntime::default(),
        job_execution_locks: crate::job_execution_host_tests::locks(&state_path),
        findings: Mutex::new(findings),
        settings: Mutex::new(settings::SettingsStore::open(&state_path).unwrap()),
        decisions: Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
        proposals: Mutex::new(
            agents::ProposalStore::open(app_data.join("proposals.sqlite")).unwrap(),
        ),
        drafts: Mutex::new(
            agents::draft_store::DraftStore::open(app_data.join("section-drafts.sqlite")).unwrap(),
        ),
        work_packets: work_packets::PacketRuntime::open(&state_path).unwrap(),
        extraction_caches: Mutex::new(ExtractionCaches::default()),
        embedding_cache: None,
        knowledge_overlay: None,
        curated_settled: Arc::default(),
        sources: Arc::new(Mutex::new(sources)),
        primary_sources: primary_source::PrimarySourceStore::open(app_data).unwrap(),
        metrics: Mutex::new(metrics::MetricsStore::open(&state_path).unwrap()),
    }
}

fn recover(state: &AppState, source: &RegisteredSource) -> (DeltaSummary, ReconcileStats) {
    let operation = source_operation(state, vec![(source.clone(), false)]).unwrap();
    let (job, execution) = start_job(state, &source.ingest_job_kind()).unwrap();
    let root = operation.root(&source.repo_key).unwrap();
    let (extraction, layers, delta) = {
        let mut caches = state.extraction_caches.lock().unwrap();
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_incremental(
            root,
            &source.repo_key,
            "workdir",
            &[],
            &BTreeMap::new(),
            None,
            None,
            &[],
            cache,
            &[],
            &mut |_| {},
        )
        .unwrap()
    };
    let reconciled = load_into_graph(
        &mut state.graph.lock().unwrap(),
        &extraction,
        &source.repo_key,
        root,
        "workdir",
    )
    .unwrap();
    relink_found_adrs(
        state,
        &operation,
        &execution,
        &mut crate::OperationFacts::default(),
        &mut Default::default(),
        &crate::Documented::from([(source.repo_key.clone(), Vec::new())]),
        &crate::DocumentCaptures::new(),
        ingest::api_docs::ApiDocLimits::default(),
    )
    .unwrap();
    record_ingest_metrics(
        state,
        job.id,
        &source.repo_key,
        "workdir",
        &layers,
        &BTreeSet::from([source.repo_key.clone()]),
    )
    .unwrap();
    state
        .jobs
        .lock()
        .unwrap()
        .finish_execution(&execution, &[])
        .unwrap();
    (delta, reconciled)
}

fn owned_facts(state: &AppState, repo: &str) -> (Vec<Node>, Vec<Edge>) {
    let (nodes, edges) = state.graph.lock().unwrap().read_snapshot().unwrap();
    (
        nodes
            .into_iter()
            .filter(|node| {
                fact_owned_by_repo(&node.props, repo) || id_explicitly_owned_by_repo(&node.id, repo)
            })
            .collect(),
        edges
            .into_iter()
            .filter(|edge| fact_owned_by_repo(&edge.props, repo))
            .collect(),
    )
}

fn serialized(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap()
}

#[test]
fn registered_sources_isolate_intake_findings_and_recovery() {
    // AC-0142: same-basename, initially byte-identical TS sources must not share
    // cache ownership, findings replacement or deletion reconciliation.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first/project");
    let second_root = directory(dir.path(), "second/project");
    for root in [&first_root, &second_root] {
        std::fs::write(root.join("app.ts"),
            "import { helper } from './helper';\nexport function run() { helper(); eval(getCode()); }\n").unwrap();
        std::fs::write(root.join("helper.ts"), "export function helper() {}\n").unwrap();
    }
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    let state = app_state(&app_data);
    for root in [&first_root, &second_root] {
        let report =
            crate::preflight_uncancelled(root.to_str().unwrap(), app.handle(), &state).unwrap();
        assert!(
            report
                .unsupported
                .iter()
                .any(|finding| finding.kind == "inline-eval")
        );
    }
    let first = state
        .sources
        .lock()
        .unwrap()
        .get_by_root(&first_root)
        .unwrap()
        .unwrap();
    let second = state
        .sources
        .lock()
        .unwrap()
        .get_by_root(&second_root)
        .unwrap()
        .unwrap();
    assert_eq!(first.display_name, second.display_name);
    assert_ne!(first.repo_key, second.repo_key);
    let second_findings = serialized(
        &state
            .findings
            .lock()
            .unwrap()
            .list_for(&second.repo_key)
            .unwrap(),
    );
    assert!(
        !state
            .findings
            .lock()
            .unwrap()
            .list_for(&first.repo_key)
            .unwrap()
            .is_empty()
    );
    assert_eq!(recover(&state, &first).0.recomputed_files, 2);
    assert_eq!(recover(&state, &second).0.recomputed_files, 2);
    assert_eq!(state.extraction_caches.lock().unwrap().repos.len(), 2);
    let first_helper = format!("sym:{}@helper.ts#helper", first.repo_key);
    let second_helper = format!("sym:{}@helper.ts#helper", second.repo_key);
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .get_node(&first_helper)
            .unwrap()
            .is_some()
    );
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .get_node(&second_helper)
            .unwrap()
            .is_some()
    );
    let second_facts = owned_facts(&state, &second.repo_key);
    assert!(!second_facts.0.is_empty());
    assert!(!second_facts.1.is_empty());
    let unchanged = recover(&state, &first);
    assert_eq!(unchanged.0.recomputed_files, 0);
    assert_eq!(unchanged.0.reused_files, 2);
    assert_eq!(unchanged.1.inserted_or_updated, 0);
    std::fs::write(
        first_root.join("helper.ts"),
        "export function replacement() {}\n",
    )
    .unwrap();
    let changed = recover(&state, &first);
    assert_eq!(changed.0.recomputed_files, 1);
    assert_eq!(changed.0.reused_files, 1);
    assert!(changed.1.deleted > 0);
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .get_node(&first_helper)
            .unwrap()
            .is_none()
    );
    assert_eq!(owned_facts(&state, &second.repo_key), second_facts);
    std::fs::remove_file(first_root.join("app.ts")).unwrap();
    assert_eq!(recover(&state, &first).0.deleted_files, 1);
    crate::preflight_uncancelled(first_root.to_str().unwrap(), app.handle(), &state).unwrap();
    assert!(
        state
            .findings
            .lock()
            .unwrap()
            .list_for(&first.repo_key)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        serialized(
            &state
                .findings
                .lock()
                .unwrap()
                .list_for(&second.repo_key)
                .unwrap()
        ),
        second_findings
    );
    assert_eq!(owned_facts(&state, &second.repo_key), second_facts);
    let latest = state.metrics.lock().unwrap().history(1).unwrap();
    assert_eq!(latest[0].repo, first.repo_key);
    let snapshot = state.graph.lock().unwrap().read_snapshot().unwrap();
    drop(state);
    let restarted = app_state(&app_data);
    assert!(restarted.extraction_caches.lock().unwrap().repos.is_empty());
    assert_eq!(
        register_local_source(&restarted, &first_root)
            .unwrap()
            .source_id,
        first.source_id
    );
    assert_eq!(
        register_local_source(&restarted, &second_root)
            .unwrap()
            .source_id,
        second.source_id
    );
    assert_eq!(
        restarted.graph.lock().unwrap().read_snapshot().unwrap(),
        snapshot
    );
    assert_eq!(
        serialized(
            &restarted
                .findings
                .lock()
                .unwrap()
                .list_for(&second.repo_key)
                .unwrap()
        ),
        second_findings
    );
    assert_eq!(recover(&restarted, &second).0.recomputed_files, 2);
    assert_eq!(owned_facts(&restarted, &second.repo_key), second_facts);
}

fn reference(repo: &str, path: &str, byte_end: u64) -> EvidenceRef {
    EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start: 0,
        byte_end,
        commit_sha: "historical-unverified".into(),
    }
}

#[test]
fn registered_evidence_reads_never_fall_back() {
    // AC-0143: repository facts may contain tempting old roots; only the exact
    // live host registration authorizes a working-tree read, with no freshness claim.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first/project");
    let second_root = directory(dir.path(), "second/project");
    std::fs::write(first_root.join("source.ts"), "FIRST").unwrap();
    std::fs::write(second_root.join("source.ts"), "OTHER").unwrap();
    let state = app_state(&app_data);
    let first = register_local_source(&state, &first_root).unwrap();
    let second = register_local_source(&state, &second_root).unwrap();
    for repo in [&first.repo_key, &second.repo_key, "local/legacy"] {
        state.graph.lock().unwrap().put_node(&Node {
            id: format!("repo:{repo}"), label: "Repo".into(),
            props: json!({"root": second_root.to_str().unwrap(), "commit": "historical-unverified"}),
        }).unwrap();
    }
    assert_eq!(
        state.graph.lock().unwrap().read_snapshot().unwrap().0.len(),
        3
    );
    let reader = |reference: &EvidenceRef| {
        source_access::with_registered_read(&state.sources, &reference.repo, |root| {
            evidence::read_span_exact(
                root,
                &reference.path,
                &(reference.byte_start..reference.byte_end),
            )
            .map_err(|error| error.to_string())
        })
        .ok()
    };
    assert_eq!(
        reader(&reference(&first.repo_key, "source.ts", 5)).as_deref(),
        Some("FIRST")
    );
    assert_eq!(
        reader(&reference(&second.repo_key, "source.ts", 5)).as_deref(),
        Some("OTHER")
    );
    for repo in ["local/legacy", "local/unknown", "project"] {
        assert!(reader(&reference(repo, "source.ts", 5)).is_none());
        let called = std::cell::Cell::new(false);
        assert!(
            source_access::with_registered_read(&state.sources, repo, |_| {
                called.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(!called.get());
    }
    assert!(
        reader(&reference(
            &first.repo_key,
            second_root.join("source.ts").to_str().unwrap(),
            5
        ))
        .is_none()
    );
    assert!(
        reader(&reference(
            &first.repo_key,
            "../../second/project/source.ts",
            5
        ))
        .is_none()
    );
    assert!(
        reader(&reference(
            &first.repo_key,
            "source.ts",
            evidence::MAX_EVIDENCE_BYTES + 1
        ))
        .is_none()
    );
    std::fs::remove_dir_all(&first_root).unwrap();
    // The previously assembled closure re-reads availability; it cannot retain
    // an old root association or use the other available repository as fallback.
    assert!(reader(&reference(&first.repo_key, "source.ts", 5)).is_none());
    assert!(
        source_access::with_registered_read(&state.sources, &first.repo_key, |_| Ok(())).is_err()
    );
    assert_eq!(
        reader(&reference(&second.repo_key, "source.ts", 5)).as_deref(),
        Some("OTHER")
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&second_root, &first_root).unwrap();
        assert!(reader(&reference(&first.repo_key, "source.ts", 5)).is_none());
    }
}

#[test]
fn repo_facts_keep_operational_roots_out_of_identity() {
    // AC-0144: vary only the operational root passed to the real loader. Full
    // fact equality and aggregate hashes must agree, beyond a field-name check.
    let dir = tempfile::tempdir().unwrap();
    let first_root = directory(dir.path(), "first");
    let second_root = directory(dir.path(), "second");
    std::fs::write(
        first_root.join("source.ts"),
        "export function handle() {}\n",
    )
    .unwrap();
    let repo = "local/src_11111111111111111111111111111111";
    let extraction = extract_tree(
        &first_root,
        repo,
        "revision-one",
        &[],
        &BTreeMap::new(),
        None,
        None,
        &[],
    )
    .unwrap();
    let mut first = SqliteGraphStore::open_in_memory().unwrap();
    let mut second = SqliteGraphStore::open_in_memory().unwrap();
    load_into_graph(&mut first, &extraction, repo, &first_root, "revision-one").unwrap();
    load_into_graph(&mut second, &extraction, repo, &second_root, "revision-one").unwrap();
    assert_eq!(
        first.read_snapshot().unwrap(),
        second.read_snapshot().unwrap()
    );
    assert_eq!(
        deterministic_graph_hashes(&first).unwrap(),
        deterministic_graph_hashes(&second).unwrap()
    );
    let facts = first.read_snapshot().unwrap();
    let original_hash =
        metrics::compute(&facts.0, &facts.1, &BTreeMap::new(), &BTreeSet::new()).content_hash;
    let node = first.get_node(&format!("repo:{repo}")).unwrap().unwrap();
    let props = node.props.as_object().unwrap();
    assert_eq!(
        props.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["commit", "prov"])
    );
    let fact_bytes = serialized(&node.props);
    assert!(!fact_bytes.contains(first_root.to_str().unwrap()));
    assert!(!fact_bytes.contains(second_root.to_str().unwrap()));
    let unchanged =
        load_into_graph(&mut first, &extraction, repo, &second_root, "revision-one").unwrap();
    assert_eq!(unchanged.inserted_or_updated, 0);
    load_into_graph(&mut second, &extraction, repo, &second_root, "revision-two").unwrap();
    let revised = second.read_snapshot().unwrap();
    let changed_hash =
        metrics::compute(&revised.0, &revised.1, &BTreeMap::new(), &BTreeSet::new()).content_hash;
    assert_ne!(
        original_hash, changed_hash,
        "recovered revision remains part of Repo identity"
    );
}

fn failed_job(state: &AppState, kind: &str) -> Job {
    let (_, execution) = start_job(state, kind).unwrap();
    let mut jobs = state.jobs.lock().unwrap();
    jobs.progress_execution(&execution, "extract", 42.0)
        .unwrap();
    updated_job(
        jobs.fail_execution(&execution, "preserve historical failure")
            .unwrap(),
    )
}

#[test]
fn source_bound_retry_preserves_legacy_job_history() {
    // AC-0145: exercise the production preparation seam which resolves the
    // source and guard plan before it calls the mutating JobStore::retry.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "first/project");
    let state = app_state(&app_data);
    let source = register_local_source(&state, &root).unwrap();
    let bound = failed_job(&state, &source.ingest_job_kind());
    let legacy = failed_job(&state, &format!("ingest:{}", root.to_str().unwrap()));
    let unsupported = failed_job(&state, "add-repo:owner/project");
    let system = failed_job(&state, "add-system:system");
    let unknown = failed_job(
        &state,
        "ingest-source-v1:src_00000000000000000000000000000000",
    );
    drop(state);
    let restarted = app_state(&app_data);
    let other_root = directory(dir.path(), "second/project");
    let other = register_local_source(&restarted, &other_root).unwrap();
    assert_ne!(source.source_id, other.source_id);
    for old in [&legacy, &unsupported, &system, &unknown] {
        let error = prepare_job_retry(&restarted, old.id)
            .err()
            .expect("must refuse replay");
        if old.id == legacy.id {
            assert!(error.contains("re-run ingestion"));
        }
        assert_eq!(
            serialized(&restarted.jobs.lock().unwrap().get(old.id).unwrap()),
            serialized(old)
        );
    }
    let (running, execution, resolved, operation) =
        prepare_job_retry(&restarted, bound.id).unwrap();
    let resolved = resolved.unwrap();
    assert_eq!(resolved.source_id, source.source_id);
    assert_eq!(resolved.repo_key, source.repo_key);
    assert_eq!(operation.unwrap().root(&source.repo_key).unwrap(), root);
    assert_eq!(running.id, bound.id);
    assert_eq!(running.kind, bound.kind);
    assert_eq!(running.created_at, bound.created_at);
    assert_eq!(running.status, "running");
    assert!(running.error.is_none() && running.stage.is_none() && running.progress.is_none());
    drop(execution);
    let unavailable = failed_job(&restarted, &source.ingest_job_kind());
    std::fs::remove_dir_all(&root).unwrap();
    assert!(prepare_job_retry(&restarted, unavailable.id).is_err());
    assert_eq!(
        serialized(&restarted.jobs.lock().unwrap().get(unavailable.id).unwrap()),
        serialized(&unavailable)
    );
    assert_eq!(restarted.sources.lock().unwrap().list().unwrap().len(), 2);
}

struct ProposalFixtureProvider;

impl llm::LlmProvider for ProposalFixtureProvider {
    fn id(&self) -> &str {
        "fixture:historical-proposal"
    }
    fn locality(&self) -> llm::Locality {
        llm::Locality::Local
    }
    fn capabilities(&self) -> llm::ProviderCaps {
        llm::ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
        Err(llm::ProviderError::Unsupported("embeddings"))
    }
    fn complete(
        &self,
        _: &llm::ProviderCompletionRequest,
    ) -> Result<llm::Completion, llm::ProviderError> {
        Ok(llm::Completion {
            text: json!({
                "target_id": "sym:local/project@target.ts#target",
                "annotation": "Possible relationship awaiting context reconciliation.",
                "citations": ["source", "target"]
            })
            .to_string(),
        })
    }
}

pub(crate) fn historical_proposal() -> (agents::AgentTask, agents::AgentProposal) {
    let evidence = [
        ("source", "export function source() { target(); }"),
        ("target", "export function target() {}"),
    ]
    .into_iter()
    .map(|(id, text)| agents::AgentEvidence {
        id: id.into(),
        source: reference("local/project", &format!("{id}.ts"), text.len() as u64),
        text: text.into(),
    })
    .collect();
    let task = agents::AgentTask {
        action_id: "historical:run".into(),
        gap_id: "gap:local/project:call".into(),
        source_id: "sym:local/project@source.ts#source".into(),
        edge_label: "CALLS".into(),
        existing_confidence: ConfidenceTier::Gap,
        source_evidence_ids: vec!["source".into()],
        evidence,
        candidates: vec![agents::AgentCandidate {
            node_id: "sym:local/project@target.ts#target".into(),
            label: "Symbol".into(),
            summary: "Candidate target function".into(),
            evidence_ids: vec!["target".into()],
        }],
    };
    let proposal = agents::AgentBroker::bounded_default()
        .propose(
            &ProposalFixtureProvider,
            &llm::EgressFirewall::new(llm::EgressPolicy::local_only()),
            &task,
            None,
        )
        .unwrap();
    (task, proposal)
}

pub(crate) fn staged_bytes(path: &Path, id: &str) -> String {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT immutable_json FROM staged_agent_proposals WHERE proposal_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn source_identity_migration_preserves_historical_stages() {
    // AC-0146: use actual graph, jobs, findings, metrics, decision and proposal
    // stores. Only disposable schema-three graph facts and known generated
    // findings retire; historical JSON, identities and review revisions survive.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let state_path = app_data.join("state.db");
    let graph_path = app_data.join("graph.db");
    let proposals_path = app_data.join("proposals.sqlite");
    let mut graph = SqliteGraphStore::open(&graph_path).unwrap();
    let legacy_prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![],
        "migration.fixture",
        b"legacy-repo",
    )
    .unwrap();
    let legacy_nodes = vec![
        Node {
            id: "repo:local/project".into(),
            label: "Repo".into(),
            props: json!({"root": root.to_str().unwrap(), "prov": legacy_prov}),
        },
        Node {
            id: "file:local/project@source.ts".into(),
            label: "File".into(),
            props: json!({"prov": legacy_prov}),
        },
    ];
    for node in &legacy_nodes {
        graph.put_node(node).unwrap();
    }
    let legacy_edge = Edge {
        src: legacy_nodes[0].id.clone(),
        dst: legacy_nodes[1].id.clone(),
        label: "CONTAINS".into(),
        props: json!({"prov": legacy_prov}),
    };
    graph.put_edge(&legacy_edge).unwrap();
    drop(graph);
    Connection::open(&graph_path)
        .unwrap()
        .pragma_update(None, "user_version", 3)
        .unwrap();
    let mut findings = FindingStore::open(&state_path).unwrap();
    let finding = |detector: &'static str| NewFinding {
        kind: "unsupported",
        detector,
        path: "source.ts",
        line: 1,
        message: "historical finding",
    };
    findings
        .replace_for(
            "local/project",
            ingest::preflight::DETECTOR_ID,
            &[finding(ingest::preflight::DETECTOR_ID)],
        )
        .unwrap();
    findings
        .replace_for("local/project", "custom@1", &[finding("custom@1")])
        .unwrap();
    let custom = serialized(
        &findings
            .list()
            .unwrap()
            .into_iter()
            .filter(|row| row.detector == "custom@1")
            .collect::<Vec<_>>(),
    );
    let jobs = JobStore::open(&state_path).unwrap();
    let conn = Connection::open(&state_path).unwrap();
    conn.execute("INSERT INTO jobs(kind,status,stage,progress,error) VALUES ('ingest:/historical/project','failed','extract',37,'historical failure')", []).unwrap();
    let job = jobs.get(conn.last_insert_rowid()).unwrap();
    let jobs_before = serialized(&jobs.list().unwrap());
    let computed = metrics::compute(
        &legacy_nodes,
        std::slice::from_ref(&legacy_edge),
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    let mut metrics_store = metrics::MetricsStore::open(&state_path).unwrap();
    metrics_store
        .record(job.id, "local/project", "legacy", &computed, 1, 0)
        .unwrap();
    let metrics_before = serialized(&metrics_store.history(50).unwrap());
    let (task, proposal) = historical_proposal();
    let mut decisions = agents::DecisionLog::open(&state_path).unwrap();
    decisions
        .record(
            &proposal,
            agents::ProposalDecision::Rejected,
            Some("historical decision"),
        )
        .unwrap();
    let assertion = agents::CuratableAssertion {
        subject_id: "historical:assertion".into(),
        summary: "Historical inferred design".into(),
        provenance: Provenance::new(
            Tier::Semantic,
            ConfidenceTier::InferredStrong,
            vec![reference("local/project", "source.ts", 12)],
            "migration.fixture",
            b"historical assertion",
        )
        .unwrap(),
    };
    decisions
        .record_assertion(
            &assertion,
            agents::AssertionDecision::Annotated,
            Some("Keep historical annotation"),
        )
        .unwrap();
    let decisions_before = serialized(&decisions.list().unwrap());
    let assertions_before = serialized(&decisions.list_assertions().unwrap());
    let mut proposals = agents::ProposalStore::open(&proposals_path).unwrap();
    let staged = proposals
        .stage(&task, &proposal, job.id, "snapshot:historical")
        .unwrap();
    proposals
        .review(
            &staged.proposal_id,
            0,
            agents::ProposalDecision::Accepted,
            Some("first review"),
        )
        .unwrap();
    let reviewed = proposals
        .review(
            &staged.proposal_id,
            1,
            agents::ProposalDecision::Rejected,
            Some("second review"),
        )
        .unwrap();
    let immutable_before = staged_bytes(&proposals_path, &staged.proposal_id);
    assert_eq!(reviewed.review_revision, 2);
    drop((findings, jobs, metrics_store, decisions, proposals));
    let state = app_state(&app_data);
    assert_eq!(core_graph::GRAPH_SCHEMA_VERSION, 6);
    assert_eq!(state.graph.lock().unwrap().fact_counts().unwrap(), (0, 0));
    assert_eq!(
        serialized(&state.findings.lock().unwrap().list().unwrap()),
        custom
    );
    assert_eq!(
        serialized(&state.jobs.lock().unwrap().list().unwrap()),
        jobs_before
    );
    assert_eq!(
        serialized(&state.metrics.lock().unwrap().history(50).unwrap()),
        metrics_before
    );
    assert_eq!(
        serialized(&state.decisions.lock().unwrap().list().unwrap()),
        decisions_before
    );
    assert_eq!(
        serialized(&state.decisions.lock().unwrap().list_assertions().unwrap()),
        assertions_before
    );
    assert_eq!(
        state
            .proposals
            .lock()
            .unwrap()
            .get(&staged.proposal_id)
            .unwrap()
            .unwrap(),
        reviewed
    );
    assert_eq!(
        staged_bytes(&proposals_path, &staged.proposal_id),
        immutable_before
    );
    let registered = register_local_source(&state, &root).unwrap();
    assert_ne!(registered.repo_key, "local/project");
    state
        .findings
        .lock()
        .unwrap()
        .replace_for(
            &registered.repo_key,
            ingest::preflight::DETECTOR_ID,
            &[finding(ingest::preflight::DETECTOR_ID)],
        )
        .unwrap();
    drop(state);
    let reopened = app_state(&app_data);
    assert_eq!(reopened.findings.lock().unwrap().list().unwrap().len(), 2);
    assert_eq!(
        reopened
            .proposals
            .lock()
            .unwrap()
            .get(&staged.proposal_id)
            .unwrap()
            .unwrap(),
        reviewed
    );
    reopened.graph.lock().unwrap().clear().unwrap();
    assert_eq!(reopened.jobs.lock().unwrap().clear_finished().unwrap(), 1);
    assert_eq!(
        reopened
            .sources
            .lock()
            .unwrap()
            .get_by_repo(&registered.repo_key)
            .unwrap()
            .unwrap()
            .source_id,
        registered.source_id
    );
    assert_eq!(
        staged_bytes(&proposals_path, &staged.proposal_id),
        immutable_before
    );
    assert_eq!(
        serialized(&reopened.decisions.lock().unwrap().list().unwrap()),
        decisions_before
    );
}

#[test]
fn managed_readiness_batch_rolls_back_on_late_invalid_member() {
    // AC-0147: neither unavailable nor ready publication can expose a prefix
    // when a later member fails validation, including after reopening state.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let local_root = directory(dir.path(), "direct");
    let state = app_state(&app_data);
    let first = state
        .sources
        .lock()
        .unwrap()
        .reserve_managed("https://github.com/fixture/first")
        .unwrap();
    let second = state
        .sources
        .lock()
        .unwrap()
        .reserve_managed("https://github.com/fixture/second")
        .unwrap();
    let local = register_local_source(&state, &local_root).unwrap();
    // Availability is the transaction under test. These host fixture folders
    // are not a clone/owner-guard attestation and no source read is performed.
    std::fs::create_dir_all(first.root()).unwrap();
    std::fs::create_dir_all(second.root()).unwrap();
    let ids = vec![first.source_id.clone(), second.source_id.clone()];
    state
        .sources
        .lock()
        .unwrap()
        .set_ready_batch(&ids, true)
        .unwrap();
    assert!(
        state
            .sources
            .lock()
            .unwrap()
            .set_ready_batch(&[first.source_id.clone(), local.source_id.clone()], false)
            .is_err()
    );
    for id in &ids {
        assert!(
            state
                .sources
                .lock()
                .unwrap()
                .get_by_id(id)
                .unwrap()
                .unwrap()
                .is_ready()
        );
    }
    state
        .sources
        .lock()
        .unwrap()
        .set_ready_batch(&ids, false)
        .unwrap();
    assert!(
        state
            .sources
            .lock()
            .unwrap()
            .set_ready_batch(
                &[
                    first.source_id.clone(),
                    "src_00000000000000000000000000000000".into()
                ],
                true
            )
            .is_err()
    );
    drop(state);
    let restarted = app_state(&app_data);
    for id in &ids {
        assert!(
            !restarted
                .sources
                .lock()
                .unwrap()
                .get_by_id(id)
                .unwrap()
                .unwrap()
                .is_ready()
        );
    }
    restarted
        .sources
        .lock()
        .unwrap()
        .set_ready_batch(&ids, true)
        .unwrap();
    for id in &ids {
        assert!(
            restarted
                .sources
                .lock()
                .unwrap()
                .get_by_id(id)
                .unwrap()
                .unwrap()
                .is_ready()
        );
    }
}

fn isolated_managed_operation_case(name: &str) -> bool {
    isolated_managed_exact_test(&format!("registered_source_tests::{name}"))
}

/// Run lock-release assertions after exec, apart from parallel fork activity.
pub(crate) fn isolated_managed_exact_test(exact: &str) -> bool {
    use std::io::Read;
    use std::process::{Child, Command, Stdio};
    const MARKER: &str = "CARTOGRAPH_REGISTERED_OPERATION_CASE";
    if std::env::var(MARKER).is_ok_and(|value| value == exact) {
        return false;
    }
    // Parallel fixtures spawn Git processes. CLOEXEC closes inherited lock
    // handles only at exec, so open this fixture's locks after its own exec.
    struct Cleanup(Child);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Cleanup(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", exact, "--nocapture", "--test-threads=1"])
            .env(MARKER, exact)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let status = child.0.wait().unwrap();
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(
        status.success(),
        "isolated {exact} failed:\n{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert!(
        String::from_utf8_lossy(&stdout).contains("test result: ok. 1 passed;"),
        "isolated {exact} must execute exactly one passing test:\n{}",
        String::from_utf8_lossy(&stdout)
    );
    true
}

#[test]
#[should_panic(expected = "must execute exactly one passing test")]
fn isolated_managed_exact_test_rejects_missing_case() {
    isolated_managed_exact_test("registered_source_tests::__missing_isolated_case");
}

fn ready_managed_source(state: &AppState, name: &str) -> RegisteredSource {
    let source = state
        .sources
        .lock()
        .unwrap()
        .reserve_managed(&format!("https://github.com/fixture/{name}"))
        .unwrap();
    let managed = source.managed().unwrap().unwrap();
    let guard = managed.try_write().unwrap();
    // The real slot initializer owns this destination; an empty Git checkout
    // suffices to exercise the actual host acquisition/validation boundary.
    std::fs::create_dir(guard.root()).unwrap();
    let output = std::process::Command::new("git")
        .args(["-c", "init.templateDir=", "init", "--quiet"])
        .arg(guard.root())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git fixture init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(guard.root().join("keep.ts"), "retained checkout canary").unwrap();
    drop(guard);
    state
        .sources
        .lock()
        .unwrap()
        .set_ready(&source.source_id, true)
        .unwrap();
    let source = state
        .sources
        .lock()
        .unwrap()
        .get_by_id(&source.source_id)
        .unwrap()
        .unwrap();
    assert!(source.is_ready());
    source
}

fn ready_after_reopen(app_data: &Path, source: &RegisteredSource) -> bool {
    SourceRegistry::open(app_data.join("state.db"), app_data)
        .unwrap()
        .get_by_id(&source.source_id)
        .unwrap()
        .unwrap()
        .is_ready()
}

#[test]
fn managed_plugin_gate_retains_source_guards_through_verdict() {
    if isolated_managed_operation_case("managed_plugin_gate_retains_source_guards_through_verdict")
    {
        return;
    }
    // AC-0147: exercise the real discovery/gate pipeline. Synchronous host
    // events place replacement attempts between the WASM and corpus reads,
    // before verdict storage, and after its durable publication. No timer or
    // test-only pipeline hook determines the ordering.
    use tauri::Listener;
    const PLUGIN: &str = "t0.plugin-fixture";
    const WASM: &[u8] =
        include_bytes!("../../crates/adapters-plugin-host/tests/fixtures/compiled/ok-adapter.wasm");
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let state = app_state(&app_data);
    let source = ready_managed_source(&state, "plugin-gate");
    let adapters = source.root().join(".cartograph/adapters");
    std::fs::create_dir_all(&adapters).unwrap();
    std::fs::write(adapters.join(format!("{PLUGIN}.wasm")), WASM).unwrap();
    let corpus_path = adapters.join(format!("{PLUGIN}.golden.json"));
    std::fs::write(
        &corpus_path,
        json!({
            "extensions": ["foo"],
            "cases": [{
                "path": "src/lib.rs",
                "source": "hello world",
                "nodes": [{"id":"golden:src/lib.rs","label":"TestNode","props":{"len":11}}],
                "edges": [{"src":"golden:src/lib.rs","dst":"golden:src/lib.rs","label":"SELF","props":{}}]
            }]
        })
        .to_string(),
    )
    .unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(state);
    let state = app.state::<AppState>();
    let hash = core_prov::content_hash(WASM);

    for corpus_present in [true, false] {
        if !corpus_present {
            // The preceding gate released its guard. Change the fixture under
            // the real exclusive source lock before testing a failed verdict.
            let guard = source.managed().unwrap().unwrap().try_write().unwrap();
            std::fs::remove_file(&corpus_path).unwrap();
            drop(guard);
        }
        let (job, execution) = start_job(&state, &format!("plugin-gate:{PLUGIN}")).unwrap();
        let job_id = job.id;
        let other_registry =
            Mutex::new(SourceRegistry::open(app_data.join("state.db"), &app_data).unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observations = Arc::clone(&observed);
        let source_for_attempt = source.clone();
        let event_id = app.listen("job://changed", move |event| {
            let job: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            if job["id"].as_i64() != Some(job_id) {
                return;
            }
            let boundary = if job["status"] == "done" {
                "done"
            } else {
                match job["stage"].as_str() {
                    Some("gate") => "gate",
                    Some("record") => "record",
                    _ => return,
                }
            };
            // A second registry connection represents another participating
            // host operation. Busy must occur before it invalidates readiness.
            let blocked = matches!(
                SourceOperation::acquire(
                    &other_registry,
                    [(source_for_attempt.clone(), true)],
                ),
                Err(error) if error.contains("busy")
            );
            let ready = other_registry
                .lock()
                .unwrap()
                .get_by_id(&source_for_attempt.source_id)
                .unwrap()
                .unwrap()
                .is_ready();
            observations
                .lock()
                .unwrap()
                .push((boundary.to_string(), blocked, ready));
        });

        let report = plugin_gate_blocking(PLUGIN, &execution, app.handle()).unwrap();
        app.unlisten(event_id);
        assert_eq!(report["passed"], json!(corpus_present));
        assert_eq!(
            *observed.lock().unwrap(),
            vec![
                ("gate".to_string(), true, true),
                ("record".to_string(), true, true),
                ("done".to_string(), true, true),
            ]
        );
        let stored = state
            .settings
            .lock()
            .unwrap()
            .plugin_gate(
                PLUGIN,
                &adapters_plugin_host::gate::gate_key(
                    &hash,
                    std::fs::read(&corpus_path).ok().as_deref(),
                ),
            )
            .unwrap()
            .unwrap();
        assert_eq!(stored.0, corpus_present);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored.1).unwrap(),
            report
        );
        assert!(ready_after_reopen(&app_data, &source));
        // Both successful and failed verdicts release the owned guard on
        // return; this process spawns no children while the gate holds it.
        let released = source.managed().unwrap().unwrap().try_write().unwrap();
        drop(released);
    }
}

#[test]
fn managed_operation_invalidates_ready_source_before_checkout_validation() {
    if isolated_managed_operation_case(
        "managed_operation_invalidates_ready_source_before_checkout_validation",
    ) {
        return;
    }
    // AC-0147: re-add of a previously ready but corrupt checkout fails only
    // after durable unavailability; an existing directory cannot revive it.
    let directory_root = tempfile::tempdir().unwrap();
    let app_data = directory(directory_root.path(), "private");
    let state = app_state(&app_data);
    let source = ready_managed_source(&state, "corrupt");
    std::fs::remove_dir_all(source.root().join(".git")).unwrap();
    assert!(
        ready_after_reopen(&app_data, &source),
        "the old ready flag is observable before re-add"
    );
    assert!(SourceOperation::acquire(&state.sources, [(source.clone(), true)]).is_err());
    assert!(!ready_after_reopen(&app_data, &source));
    assert_eq!(
        std::fs::read_to_string(source.root().join("keep.ts")).unwrap(),
        "retained checkout canary"
    );
    assert!(
        source_access::with_registered_read(&state.sources, &source.repo_key, |_| Ok(())).is_err()
    );
}

#[test]
fn managed_operation_busy_lock_preserves_all_ready_flags() {
    if isolated_managed_operation_case("managed_operation_busy_lock_preserves_all_ready_flags") {
        return;
    }
    // AC-0147: a later busy lock means no full reservation was obtained. The
    // earlier reserved source remains ready, and its temporary handle releases.
    let directory_root = tempfile::tempdir().unwrap();
    let app_data = directory(directory_root.path(), "private");
    let state = app_state(&app_data);
    let mut sources = [
        ready_managed_source(&state, "first"),
        ready_managed_source(&state, "second"),
    ];
    sources.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    let busy = sources[1]
        .managed()
        .unwrap()
        .unwrap()
        .try_reserve_write()
        .unwrap();
    let result = SourceOperation::acquire(
        &state.sources,
        sources.iter().cloned().map(|source| (source, true)),
    );
    assert!(matches!(result, Err(error) if error.contains("busy")));
    for source in &sources {
        assert!(ready_after_reopen(&app_data, source));
    }
    let released = sources[0]
        .managed()
        .unwrap()
        .unwrap()
        .try_reserve_write()
        .unwrap();
    drop(released);
    drop(busy);
}

#[test]
fn managed_operation_late_validation_failure_keeps_all_writes_unavailable() {
    if isolated_managed_operation_case(
        "managed_operation_late_validation_failure_keeps_all_writes_unavailable",
    ) {
        return;
    }
    // AC-0147: once every lock is reserved, all planned writes become unavailable
    // atomically before either write initialization or read validation can fail.
    let directory_root = tempfile::tempdir().unwrap();
    let app_data = directory(directory_root.path(), "private");
    let state = app_state(&app_data);
    let mut sources = [
        ready_managed_source(&state, "first"),
        ready_managed_source(&state, "second"),
    ];
    sources.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    std::fs::remove_dir_all(sources[1].root().join(".git")).unwrap();
    assert!(
        SourceOperation::acquire(
            &state.sources,
            sources.iter().cloned().map(|source| (source, true))
        )
        .is_err()
    );
    for source in &sources {
        assert!(!ready_after_reopen(&app_data, source));
        assert_eq!(
            std::fs::read_to_string(source.root().join("keep.ts")).unwrap(),
            "retained checkout canary"
        );
    }
    let ids: Vec<_> = sources
        .iter()
        .map(|source| source.source_id.clone())
        .collect();
    state
        .sources
        .lock()
        .unwrap()
        .set_ready_batch(&ids, true)
        .unwrap();
    // The corrupt later source may also be an ADR read member. Its failed read
    // validation must not leave the already-reserved write source marked ready.
    assert!(
        SourceOperation::acquire(
            &state.sources,
            [(sources[0].clone(), true), (sources[1].clone(), false)]
        )
        .is_err()
    );
    assert!(!ready_after_reopen(&app_data, &sources[0]));
    assert!(
        ready_after_reopen(&app_data, &sources[1]),
        "read-only members do not mutate readiness"
    );
}

#[cfg(unix)]
#[test]
fn managed_origin_rebinding_is_rejected_before_clone() {
    // AC-0141, AC-0147: a retained file origin cannot be redirected to a new
    // canonical path between admission and the guarded clone attempt.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let origin = directory(dir.path(), "origin");
    let other = directory(dir.path(), "unrelated");
    std::fs::write(other.join("keep.ts"), "unrelated source content").unwrap();
    let mut url = "file://".to_string();
    for byte in origin.to_str().unwrap().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            url.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(&mut url, "%{byte:02X}").unwrap();
        }
    }
    let state = app_state(&app_data);
    let source = state.sources.lock().unwrap().reserve_managed(&url).unwrap();
    let mut operation = SourceOperation::acquire(&state.sources, [(source.clone(), true)]).unwrap();
    std::fs::rename(&origin, dir.path().join("moved-origin")).unwrap();
    std::os::unix::fs::symlink(&other, &origin).unwrap();
    let error = operation
        .clone_source(&source, None)
        .expect_err("changed origin must fail");
    assert!(error.contains("registered clone origin changed"));
    assert!(!source.root().exists());
    assert_eq!(
        std::fs::read_to_string(other.join("keep.ts")).unwrap(),
        "unrelated source content"
    );
    drop(operation);
    let retained = state
        .sources
        .lock()
        .unwrap()
        .get_by_id(&source.source_id)
        .unwrap()
        .unwrap();
    assert_eq!(retained.clone_url(), source.clone_url());
    assert!(!retained.is_ready());
}

#[test]
fn registered_git_worktrees_keep_distinct_identity_across_dirty_state() {
    // AC-0140: real Git worktrees share history and a common Git directory but
    // remain distinct logical sources, both before and after one becomes dirty.
    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "init.templateDir="])
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "Cartograph fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Cartograph fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    // Include directory entries, working files, the linked .git pointer, and
    // every common/worktree Git metadata file. Status runs use optional-locks=0
    // and all Git commands precede each snapshot comparison.
    fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut snapshot = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if kind.is_dir() {
                    snapshot.insert(relative, None);
                    pending.push(path);
                } else {
                    assert!(kind.is_file(), "fixture contains an unexpected file type");
                    snapshot.insert(relative, Some(std::fs::read(path).unwrap()));
                }
            }
        }
        snapshot
    }

    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let checkout = directory(dir.path(), "main/project");
    let linked_parent = directory(dir.path(), "linked");
    let worktree = linked_parent.join("project");
    std::fs::write(checkout.join("app.ts"), "export const value = 1;\n").unwrap();
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["add", "app.ts"]);
    git(&checkout, &["commit", "-q", "-m", "offline fixture"]);
    git(
        &checkout,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let worktree = crate::paths::canonicalize(worktree).unwrap();
    assert_eq!(checkout.file_name(), worktree.file_name());
    assert!(checkout.join(".git").is_dir());
    assert!(worktree.join(".git").is_file());
    let common = |root: &Path| {
        let value = git(root, &["rev-parse", "--git-common-dir"]);
        crate::paths::canonicalize(root.join(value.trim())).unwrap()
    };
    assert_eq!(common(&checkout), common(&worktree));
    assert_eq!(
        git(&checkout, &["rev-parse", "HEAD"]),
        git(&worktree, &["rev-parse", "HEAD"])
    );
    assert!(git(&checkout, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&worktree, &["status", "--porcelain=v1"]).is_empty());
    let clean_checkout = tree_bytes(&checkout);
    let clean_worktree = tree_bytes(&worktree);
    let state_path = app_data.join("state.db");
    let mut registry = SourceRegistry::open(&state_path, &app_data).unwrap();
    let first = registry.register_local(&checkout).unwrap();
    let second = registry.register_local(&worktree).unwrap();
    assert_ne!(first.source_id, second.source_id);
    assert_ne!(first.repo_key, second.repo_key);
    assert_eq!(tree_bytes(&checkout), clean_checkout);
    assert_eq!(tree_bytes(&worktree), clean_worktree);

    std::fs::write(worktree.join("app.ts"), "export const value = 2;\n").unwrap();
    assert!(git(&checkout, &["status", "--porcelain=v1"]).is_empty());
    assert!(git(&worktree, &["status", "--porcelain=v1"]).contains("M app.ts"));
    assert_eq!(
        git(&checkout, &["rev-parse", "HEAD"]),
        git(&worktree, &["rev-parse", "HEAD"])
    );
    let before_checkout = tree_bytes(&checkout);
    let before_worktree = tree_bytes(&worktree);
    assert_eq!(
        registry.register_local(&worktree).unwrap().source_id,
        second.source_id
    );
    drop(registry);
    let mut reopened = SourceRegistry::open(&state_path, &app_data).unwrap();
    assert_eq!(
        reopened
            .register_local(&checkout.join("."))
            .unwrap()
            .source_id,
        first.source_id
    );
    assert_eq!(
        reopened
            .register_local(&worktree.join("."))
            .unwrap()
            .source_id,
        second.source_id
    );
    #[cfg(unix)]
    {
        let alias = dir.path().join("worktree-alias");
        std::os::unix::fs::symlink(&worktree, &alias).unwrap();
        assert_eq!(
            reopened.register_local(&alias).unwrap().source_id,
            second.source_id
        );
    }
    assert_eq!(reopened.list().unwrap().len(), 2);
    assert_eq!(
        reopened
            .get_by_repo(&first.repo_key)
            .unwrap()
            .unwrap()
            .root(),
        checkout
    );
    assert_eq!(
        reopened
            .get_by_repo(&second.repo_key)
            .unwrap()
            .unwrap()
            .root(),
        worktree
    );
    assert_eq!(tree_bytes(&checkout), before_checkout);
    assert_eq!(tree_bytes(&worktree), before_worktree);
}

#[test]
fn extraction_facts_are_independent_of_checkout_location() {
    // AC-0144 (#342): the same tree extracted from two checkout locations under
    // one registration yields identical facts and hashes across every
    // compiled-in adapter, including Terraform module resolution. The loader
    // test above varies only the root handed to `load_into_graph`; this varies
    // the root the adapters actually walk.
    fn write_tree(root: &Path) {
        let files = [
            (
                "src/app.ts",
                "import { helper } from './util';\nexport function handle() { return helper(); }\n",
            ),
            ("src/util.ts", "export function helper() { return 1; }\n"),
            (
                "svc/main.py",
                "from flask import Flask\napp = Flask(__name__)\n@app.get('/x')\ndef x():\n    return 1\n",
            ),
            (
                "go/main.go",
                "package main\nimport \"net/http\"\nfunc h(w http.ResponseWriter, r *http.Request) {}\nfunc main() { http.HandleFunc(\"/g\", h) }\n",
            ),
            (
                "java/A.java",
                "package a;\nimport org.springframework.web.bind.annotation.*;\n@RestController\nclass A { @GetMapping(\"/j\") String j() { return \"\"; } }\n",
            ),
            (
                "infra/main.tf",
                "resource \"aws_sqs_queue\" \"q\" { name = \"orders\" }\nmodule \"m\" { source = \"./mod\" }\n",
            ),
            (
                "infra/mod/main.tf",
                "resource \"aws_sns_topic\" \"t\" { name = \"t\" }\n",
            ),
            (
                "kt/Svc.kt",
                "package kt\nfun helper(): Int = 1\nfun handle(): Int = helper()\n",
            ),
        ];
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let shallow = directory(dir.path(), "checkout");
    let deep = directory(&directory(dir.path(), "a/much/deeper/path"), "checkout");
    write_tree(&shallow);
    write_tree(&deep);
    let repo = "local/src_11111111111111111111111111111111";
    let load = |root: &Path| {
        let extraction = extract_tree(
            root,
            repo,
            "revision-one",
            &[],
            &BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        load_into_graph(&mut graph, &extraction, repo, root, "revision-one").unwrap();
        graph
    };
    let (first, second) = (load(&shallow), load(&deep));
    let first_facts = first.read_snapshot().unwrap();
    let second_facts = second.read_snapshot().unwrap();
    // Equality alone would pass if both runs skipped an adapter, so first pin
    // that every compiled-in adapter and Terraform module expansion ran.
    let extractors = first_facts
        .0
        .iter()
        .map(|node| &node.props)
        .chain(first_facts.1.iter().map(|edge| &edge.props))
        .filter_map(|props| props["prov"]["extractor_id"].as_str())
        .collect::<BTreeSet<_>>();
    for extractor in [
        "t0.adapter-ts",
        "t0.adapter-python",
        "t0.adapter-go",
        "t0.adapter-java",
        "t0.adapter-kotlin",
        "t0.iac-terraform",
    ] {
        assert!(
            extractors.contains(extractor),
            "{extractor} produced no facts"
        );
    }
    // Root module `infra/` scopes the ids (ADR-0077).
    let module = format!("res:{repo}@infra/module.m");
    let child = format!("res:{repo}@infra/module.m.aws_sns_topic.t");
    assert!(first_facts.0.iter().any(|node| node.id == child));
    assert!(
        first_facts
            .1
            .iter()
            .any(|edge| { edge.label == "REFERENCES" && edge.src == module && edge.dst == child })
    );
    // Inspect decoded string values rather than serialized JSON, whose
    // escaping (e.g. Windows backslashes) could hide a leaked root.
    fn strings<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(text) => out.push(text),
            serde_json::Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
            serde_json::Value::Object(map) => map.values().for_each(|item| strings(item, out)),
            _ => {}
        }
    }
    for (facts, root) in [(&first_facts, &shallow), (&second_facts, &deep)] {
        let value = serde_json::to_value(facts).unwrap();
        let mut values = Vec::new();
        strings(&value, &mut values);
        for forbidden in [root.to_str().unwrap(), dir.path().to_str().unwrap()] {
            assert!(
                values.iter().all(|text| !text.contains(forbidden)),
                "a fact string contains the checkout location {forbidden}"
            );
        }
    }
    assert_eq!(first_facts, second_facts);
    assert_eq!(
        deterministic_graph_hashes(&first).unwrap(),
        deterministic_graph_hashes(&second).unwrap()
    );
    assert_eq!(
        metrics::compute(
            &first_facts.0,
            &first_facts.1,
            &BTreeMap::new(),
            &BTreeSet::new()
        )
        .content_hash,
        metrics::compute(
            &second_facts.0,
            &second_facts.1,
            &BTreeMap::new(),
            &BTreeSet::new()
        )
        .content_hash
    );
}

#[cfg(unix)]
#[test]
fn managed_clone_root_uses_the_same_canonical_form_as_a_local_root() {
    // AC-0223 (#341): a cloned repo's root — the key its plugin settings and
    // evidence reads use — derives from the canonicalized app data directory,
    // so it has the same prefix-free form a local registration gets even when
    // the app data directory is reached through a symlink.
    let dir = tempfile::tempdir().unwrap();
    let real = directory(dir.path(), "real-app-data");
    let link = dir.path().join("linked-app-data");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let state = app_state(&link);
    let managed = state
        .sources
        .lock()
        .unwrap()
        .reserve_managed("https://github.com/fixture/canonical")
        .unwrap();
    assert!(managed.root().starts_with(&real), "{:?}", managed.root());
    std::fs::create_dir_all(managed.root()).unwrap();
    assert_eq!(
        crate::paths::canonicalize(managed.root()).unwrap(),
        managed.root()
    );
}

#[test]
fn terraform_file_count_includes_explicitly_referenced_module_files() {
    // AC-0225/T-0225 (#468, ADR-0034): a local module's explicit `source`
    // overrides `.gitignore` for expansion (unchanged), and the reported
    // Terraform layer file count now comes from the same incremental-cache
    // file contexts extraction itself parsed, so it counts those module
    // files instead of undercounting them the way a separate,
    // module-unaware filesystem walk did.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "vendor/\n").unwrap();
    std::fs::write(
        dir.path().join("main.tf"),
        "resource \"aws_sqs_queue\" \"q\" { name = \"orders\" }\nmodule \"m\" { source = \"./vendor/mod\" }\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("vendor/mod")).unwrap();
    std::fs::write(
        dir.path().join("vendor/mod/main.tf"),
        "resource \"aws_sns_topic\" \"t\" { name = \"t\" }\n",
    )
    .unwrap();
    let repo = "local/tf_22222222222222222222222222222222";
    let mut cache = RepoExtractionCache::default();
    let (extraction, layers, _delta) = extract_tree_incremental(
        dir.path(),
        repo,
        "workdir",
        &[],
        &BTreeMap::new(),
        None,
        None,
        &[],
        &mut cache,
        &[],
        &mut |_| {},
    )
    .unwrap();
    // Sanity: the module really did expand (matches the existing
    // cross-checkout test's assertion style).
    let child = format!("res:{repo}@module.m.aws_sns_topic.t");
    assert!(extraction.nodes.iter().any(|node| node.id == child));
    // Two files parsed: the root main.tf and the gitignored module's file,
    // both explicitly referenced by the module block.
    assert_eq!(layers.tf.files, 2);
}

// AC-0282: a captured ingest whose file exceeds its declaration receipt bound
// publishes a coverage inventory naming the uncitable declarations.
#[test]
fn captured_ingest_names_declaration_receipt_omissions_in_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let declarations = adapters_lang_ts::captured::MAX_FILE_DECLARATION_RECEIPTS + 76;
    let source: String = (0..declarations)
        .map(|index| format!("export const value{index} = {index};\n"))
        .collect();
    std::fs::write(root.join("many.ts"), source).unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();
    let (_, coverage) = state
        .graph
        .lock()
        .unwrap()
        .read_snapshot_with_coverage_bounded(
            core_graph::SnapshotReadLimits::default(),
            core_graph::coverage::CoverageReadLimits {
                max_records: 1,
                max_bytes: 2 * 1024 * 1024,
            },
        )
        .unwrap();
    let inventory: ingest::coverage::RepoCoverage =
        serde_json::from_str(&coverage[0].record.inventory_json).unwrap();
    let omitted = inventory
        .unsupported
        .iter()
        .find(|entry| entry.capability == ingest::coverage::RECEIPT_OMISSION_CAPABILITY)
        .expect("receipt omissions are named");
    assert_eq!(
        (omitted.occurrences, omitted.first_site.as_str()),
        (76, "many.ts")
    );
    assert!(inventory.limited);
}

// AC-0148 (#1229): a symlink in the tree, such as CLAUDE.md -> AGENTS.md,
// no longer fails captured recovery; it is skipped unread and named in
// coverage, and the tree's sources are still captured.
#[cfg(unix)]
#[test]
fn captured_ingest_skips_symlinks_and_names_them_in_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("AGENTS.md"), "notes").unwrap();
    std::fs::write(root.join("app.ts"), "export function app() { return 1; }\n").unwrap();
    std::os::unix::fs::symlink(root.join("AGENTS.md"), root.join("CLAUDE.md")).unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();
    let ((nodes, _), coverage) = state
        .graph
        .lock()
        .unwrap()
        .read_snapshot_with_coverage_bounded(
            core_graph::SnapshotReadLimits::default(),
            core_graph::coverage::CoverageReadLimits {
                max_records: 1,
                max_bytes: 2 * 1024 * 1024,
            },
        )
        .unwrap();
    assert!(
        nodes
            .iter()
            .any(|node| node.label == "File" && node.id.ends_with("app.ts"))
    );
    let inventory: ingest::coverage::RepoCoverage =
        serde_json::from_str(&coverage[0].record.inventory_json).unwrap();
    let skipped = inventory
        .unsupported
        .iter()
        .find(|entry| entry.capability == ingest::coverage::UNFOLLOWED_LINK_CAPABILITY)
        .expect("unfollowed symlinks are named");
    assert_eq!(
        (skipped.occurrences, skipped.first_site.as_str()),
        (1, "CLAUDE.md")
    );
    assert!(inventory.limited);
}

// AC-0700 (#1656): after a relaunch over the same on-disk stores, the
// inventory a real ingest published is restored as current from the coverage
// table alone, equal to the stored body. Clear removes it permanently: a
// later relaunch restores nothing although the registration and the job
// history remain.
#[test]
fn recorded_coverage_survives_relaunch_and_clear_removes_it_for_good() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("app.ts"), "export function app() { return 1; }\n").unwrap();
    let launch = || {
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
        app.manage(app_state(&app_data));
        app.manage(crate::PreflightRuns::default());
        app
    };
    let restored = |app: &tauri::App<tauri::test::MockRuntime>| {
        let state = tauri::Manager::state::<AppState>(app);
        crate::context::restored_coverage(&state.graph, || {
            crate::context::coverage_scope(app.handle(), &state)
        })
        .unwrap()
    };
    let stored = {
        let app = launch();
        let state = tauri::Manager::state::<AppState>(&*app);
        let registered = register_local_source(&state, &root).unwrap();
        let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
        let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
        run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();
        let stored = state.graph.lock().unwrap().coverage_records().unwrap();
        assert_eq!(stored.len(), 1);
        stored[0].clone()
    };

    let app = launch();
    let reopened = restored(&app);
    assert_eq!(reopened.len(), 1);
    assert_eq!(
        reopened[0].status,
        context_hub::coverage::CoverageStatus::Recorded
    );
    assert_eq!(reopened[0].repo, stored.record.repo_key);
    assert_eq!(reopened[0].inventory_hash, stored.inventory_hash);
    assert_eq!(
        reopened[0].inventory,
        Some(
            serde_json::from_str::<ingest::coverage::RepoCoverage>(&stored.record.inventory_json)
                .unwrap()
        )
    );
    {
        let state = tauri::Manager::state::<AppState>(&*app);
        crate::clear_graph_store(&mut state.graph.lock().unwrap()).unwrap();
        assert!(restored(&app).is_empty());
    }
    drop(app);

    let app = launch();
    let state = tauri::Manager::state::<AppState>(&*app);
    assert_eq!(state.sources.lock().unwrap().list().unwrap().len(), 1);
    assert!(!state.jobs.lock().unwrap().list().unwrap().is_empty());
    assert!(restored(&app).is_empty());
}

// AC-0592 (#1786): the export of a real ingest carries the coverage
// artifacts. A tree with one file left unread (a symlink) and one language no
// producer covers lists both with their recorded reasons, carries the stored
// inventory hash, is the same in both modes and is byte-identical when
// exported again.
#[cfg(unix)]
#[test]
fn export_carries_the_stored_coverage_inventory_in_both_modes() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("AGENTS.md"), "notes").unwrap();
    std::fs::write(root.join("app.ts"), "export function app() { return 1; }\n").unwrap();
    std::fs::write(root.join("report.rb"), "puts 'report'\n").unwrap();
    std::os::unix::fs::symlink(root.join("AGENTS.md"), root.join("CLAUDE.md")).unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();

    let stored = state.graph.lock().unwrap().coverage_records().unwrap();
    assert_eq!(stored.len(), 1);
    let stored = &stored[0];
    let export = |mode| {
        let graph = state.graph.lock().unwrap();
        let decisions = state.decisions.lock().unwrap();
        crate::build_spec_bundle(
            &graph,
            &decisions,
            &crate::curated::CuratedReview::default(),
            mode,
        )
        .unwrap()
    };
    let artifact = |bundle: &spec::SpecBundle, id: &str| {
        bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == id)
            .cloned()
            .unwrap()
    };
    let verified = export(spec::ExportMode::VerifiedOnly);
    let markdown = artifact(&verified, "coverage");
    let index = artifact(&verified, "coverage-index");
    assert_eq!(markdown.file_name, "coverage.md");
    assert_eq!(index.file_name, "coverage.json");

    // Both limits are listed with the reason the inventory recorded.
    assert!(
        markdown.content.contains(
            "| Ruby | 1 | `report.rb:1` | no installed producer covers Ruby: a tool \
             limitation, not a system gap |"
        ),
        "{}",
        markdown.content
    );
    assert!(
        markdown.content.contains(
            "| symlink-not-followed | 1 | `CLAUDE.md` | 1 symlink(s) in the selected tree were \
             not followed or read; sources they point to are not covered unless also present in \
             the tree |"
        ),
        "{}",
        markdown.content
    );

    // The hash is the stored inventory's, and the index carries that body.
    assert_eq!(stored.inventory_hash, stored.record.inventory_hash());
    assert!(
        markdown
            .content
            .contains(&format!("- Inventory hash: `{}`", stored.inventory_hash))
    );
    let read: serde_json::Value = serde_json::from_str(&index.content).unwrap();
    let listed = &read["repositories"][0];
    assert_eq!(read["inventories"], 1);
    assert_eq!(listed["repo"], stored.record.repo_key.as_str());
    assert_eq!(listed["inventory_hash"], stored.inventory_hash.as_str());
    assert_eq!(
        listed["inventory"],
        serde_json::from_str::<serde_json::Value>(&stored.record.inventory_json).unwrap()
    );
    assert!(index.content.contains(&stored.record.inventory_json));

    // A second export is byte-identical, and mode filters nothing.
    assert_eq!(export(spec::ExportMode::VerifiedOnly), verified);
    let best = export(spec::ExportMode::BestEffort);
    assert_eq!(artifact(&best, "coverage"), markdown);
    assert_eq!(artifact(&best, "coverage-index"), index);
    // The single-artifact read used for copy and save returns the same bytes.
    let graph = state.graph.lock().unwrap();
    let decisions = state.decisions.lock().unwrap();
    for (id, expected) in [("coverage", &markdown), ("coverage-index", &index)] {
        assert_eq!(
            &crate::build_spec_artifact(
                &graph,
                &decisions,
                Some(&crate::curated::CuratedReview::default()),
                spec::ExportMode::BestEffort,
                id
            )
            .unwrap(),
            expected
        );
    }
}

// AC-0657 (#1814): an MCP read of `coverage` and `coverage-index` is
// supplied with the inventories of the session's own bounded read. On a real
// ingest with one language no producer covers it returns that language's
// entry, not the "not supplied" sentence, and is byte-equal to the artifact
// the desktop export produces from the same store state. An inventory the
// session's qualification holds as stale is stated to be stale for its
// repository; it is neither read as current nor left out.
#[cfg(unix)]
#[test]
fn mcp_coverage_read_equals_the_desktop_export_and_states_a_stale_inventory() {
    use crate::mcp_reads::{ReadSession, artifacts::ArtifactCompilation};
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(root.join("app.ts"), "export function app() { return 1; }\n").unwrap();
    std::fs::write(root.join("report.rb"), "puts 'report'\n").unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();

    let stored = state.graph.lock().unwrap().coverage_records().unwrap();
    assert_eq!(stored.len(), 1);
    let stored = &stored[0];
    let desktop = |id: &str| {
        let graph = state.graph.lock().unwrap();
        let decisions = state.decisions.lock().unwrap();
        crate::build_spec_artifact(&graph, &decisions, None, spec::ExportMode::VerifiedOnly, id)
            .unwrap()
    };
    let read = |session: &ReadSession, id: &str| {
        let compiled = session
            .compile_artifact(
                id,
                flowtracer::TraceLimits {
                    max_flows: 64,
                    max_hops: 512,
                    max_output_bytes: 1024 * 1024,
                },
                spec::SelectedCompileLimits {
                    max_bytes: 1024 * 1024,
                    adr: spec::AdrLimits {
                        max_rows: 1024,
                        max_output_bytes: 1024 * 1024,
                    },
                },
            )
            .unwrap();
        match compiled {
            ArtifactCompilation::Produced(compiled) => compiled.artifact().clone(),
            ArtifactCompilation::Limited { .. } => panic!("the fixture fits"),
        }
    };
    let acquire = |scope: &dyn Fn() -> Vec<context_hub::coverage::ScopedRepo>| {
        ReadSession::acquire(
            &state.graph,
            &state.decisions,
            || Ok(scope()),
            core_graph::SnapshotReadLimits::default(),
            core_graph::coverage::CoverageReadLimits::default(),
            spec::ExportMode::VerifiedOnly,
        )
        .unwrap()
    };

    // The scope the desktop qualifies with: the registered source under its
    // current plugin basis, so the stored inventory is current.
    let current = || crate::context::coverage_scope(app.handle(), &state).unwrap();
    let session = acquire(&current);
    let markdown = read(&session, "coverage");
    assert!(
        markdown.content.contains(
            "| Ruby | 1 | `report.rb:1` | no installed producer covers Ruby: a tool \
             limitation, not a system gap |"
        ),
        "{}",
        markdown.content
    );
    assert!(!markdown.content.contains("No stored coverage inventory"));
    assert_eq!(markdown, desktop("coverage"));
    let index = read(&session, "coverage-index");
    assert!(index.content.contains(&stored.record.inventory_json));
    assert_eq!(index, desktop("coverage-index"));
    // The read's basis already binds the inventories.
    assert!(
        session
            .basis()
            .coverage_id
            .starts_with("coverage-context-v1:")
    );

    // The same store, read by a session whose plugin basis has moved: the
    // qualification holds the inventory as stale.
    let moved = || {
        let mut scope = current();
        scope[0].config_hash = "another plugin basis".into();
        scope
    };
    let stale = acquire(&moved);
    assert_ne!(stale.basis().coverage_id, session.basis().coverage_id);
    assert_ne!(stale.basis_id().unwrap(), session.basis_id().unwrap());
    let markdown = read(&stale, "coverage");
    assert!(
        markdown.content.contains(&format!(
            "| {} | unknown | `{}` | unknown: the stored inventory is stale (compared: \
             registration, plugins) |",
            stored.record.repo_key, stored.inventory_hash
        )),
        "{}",
        markdown.content
    );
    assert!(markdown.content.contains("- The stored inventory is stale"));
    assert!(!markdown.content.contains("No stored coverage inventory"));
    assert!(!markdown.content.contains("report.rb"));
    let index: serde_json::Value =
        serde_json::from_str(&read(&stale, "coverage-index").content).unwrap();
    assert_eq!(index["stale"], 1);
    let listed = &index["repositories"][0];
    assert_eq!(listed["repo"], stored.record.repo_key.as_str());
    assert_eq!(listed["inventory_hash"], stored.inventory_hash.as_str());
    assert_eq!(
        listed["stale"]["compared"],
        json!(["registration", "plugins"])
    );
    assert!(listed["inventory"].is_null());
    // A repository that is no longer registered is stale the same way.
    let unregistered = acquire(&Vec::new);
    assert!(
        read(&unregistered, "coverage")
            .content
            .contains("- The stored inventory is stale")
    );
}

// AC-0595 (#1785, ADR-0058): the export of a real ingest carries the fact and
// evidence index. Every assertion of every exported artifact has a row, a
// verified-only export holds no InferredWeak row, a second export is
// byte-identical and the single-artifact read returns the same bytes.
#[test]
fn export_carries_a_fact_index_resolving_every_assertion() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(
        root.join("app.ts"),
        "import express from \"express\";\n\
         const app = express();\n\
         function listOrders(req, res) { res.json([]); }\n\
         app.get(\"/orders\", listOrders);\n",
    )
    .unwrap();
    std::fs::write(
        root.join("package.json"),
        r#"{"name":"shop","dependencies":{"express":"^4.19.0"},"scripts":{"build":"tsc"}}"#,
    )
    .unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();

    let graph = state.graph.lock().unwrap();
    let decisions = state.decisions.lock().unwrap();
    for mode in [spec::ExportMode::VerifiedOnly, spec::ExportMode::BestEffort] {
        let bundle = crate::build_spec_bundle(
            &graph,
            &decisions,
            &crate::curated::CuratedReview::default(),
            mode,
        )
        .unwrap();
        let artifact = |id: &str| {
            bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == id)
                .unwrap()
        };
        let (markdown, json) = (artifact("fact-index"), artifact("fact-index-json"));
        assert_eq!(markdown.file_name, "fact-index.md");
        assert_eq!(json.file_name, "fact-index.json");
        assert!(markdown.assertions.is_empty() && json.assertions.is_empty());

        let index: spec::FactIndex = serde_json::from_str(&json.content).unwrap();
        assert_eq!(index.mode, mode);
        assert!(bundle.assertion_count > 0, "the ingest recovered facts");
        assert!(!index.rows.is_empty());
        // Every cited fact is a row with its artifact, and nothing else is.
        let mut cited = std::collections::BTreeSet::new();
        for row in &index.rows {
            assert_eq!(row.citation_id, spec::citation_id(&row.fact));
            let subject = match &row.fact {
                core_graph::FactKey::Node { id } => id.clone(),
                core_graph::FactKey::Edge {
                    source,
                    label,
                    destination,
                } => format!("{source} {label} {destination}"),
            };
            for id in &row.cited_by {
                assert!(
                    artifact(id)
                        .assertions
                        .iter()
                        .any(|assertion| assertion.subject_id == subject),
                    "{id} states {subject}"
                );
                cited.insert((id.to_string(), subject.clone()));
            }
            assert!(
                markdown
                    .content
                    .contains(&format!("| {} |", row.citation_id))
            );
            if mode == spec::ExportMode::VerifiedOnly {
                assert!(row.records.iter().all(|record| !record.inferred_weak));
            }
        }
        let stated: std::collections::BTreeSet<_> = bundle
            .artifacts
            .iter()
            .flat_map(|artifact| {
                artifact
                    .assertions
                    .iter()
                    .map(|assertion| (artifact.id.clone(), assertion.subject_id.clone()))
            })
            .collect();
        assert_eq!(cited, stated);
        spec::check_citations(
            &index,
            index.rows.iter().map(|row| row.citation_id.as_str()),
        )
        .unwrap();
        assert!(spec::check_citations(&index, ["F-0000000000000000"]).is_err());

        // A second export is byte-identical, as is the single-artifact read.
        assert_eq!(
            crate::build_spec_bundle(
                &graph,
                &decisions,
                &crate::curated::CuratedReview::default(),
                mode
            )
            .unwrap(),
            bundle
        );
        for expected in [markdown, json] {
            assert_eq!(
                &crate::build_spec_artifact(
                    &graph,
                    &decisions,
                    Some(&crate::curated::CuratedReview::default()),
                    mode,
                    &expected.id
                )
                .unwrap(),
                expected
            );
        }
    }
}

// AC-0599 (#1787): the export of a real ingest carries the endpoint
// permission projection. A Flask route behind `login_required` lists the
// recovered guard with its span, a Flask route with none is not in that
// table, and an Express route, for which no permission site is recovered, is
// under the coverage note only. The text never states that an endpoint has
// no check, every assertion has a fact-index row, and a second export and
// the single-artifact read return the same bytes.
#[test]
fn export_projects_ingested_endpoints_against_permission_sites() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let flask = "from flask import Flask, jsonify\n\
         from flask_login import login_required\n\
         \n\
         app = Flask(__name__)\n\
         \n\
         @app.route(\"/me\")\n\
         @login_required\n\
         def me():\n    return jsonify(ok=True)\n\
         \n\
         @app.route(\"/health\")\n\
         def health():\n    return jsonify(ok=True)\n";
    std::fs::write(root.join("app.py"), flask).unwrap();
    std::fs::write(
        root.join("server.ts"),
        "import express from \"express\";\n\
         const app = express();\n\
         function listOrders(req, res) { res.json([]); }\n\
         app.get(\"/orders\", listOrders);\n",
    )
    .unwrap();
    std::fs::write(
        root.join("package.json"),
        r#"{"name":"shop","dependencies":{"express":"^4.19.0"}}"#,
    )
    .unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();

    let graph = state.graph.lock().unwrap();
    let decisions = state.decisions.lock().unwrap();
    let guard = flask.find("@login_required").unwrap();
    for mode in [spec::ExportMode::VerifiedOnly, spec::ExportMode::BestEffort] {
        let bundle = crate::build_spec_bundle(
            &graph,
            &decisions,
            &crate::curated::CuratedReview::default(),
            mode,
        )
        .unwrap();
        let artifact = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "endpoint-permissions")
            .unwrap();
        assert_eq!(artifact.file_name, "endpoint-permissions.md");
        let content = &artifact.content;
        let section = |heading: &str| {
            let body = &content[content.find(&format!("\n## {heading}\n")).unwrap() + 1..];
            body[1..].find("\n## ").map_or(body, |end| &body[..=end])
        };
        let row = |heading: &str, endpoint: &str| {
            section(heading)
                .lines()
                .find(|line| line.starts_with(&format!("| {endpoint} |")))
        };
        const SITED: &str = "Endpoints with recovered permission sites";
        const NONE: &str = "Endpoints with none recovered";
        const PARTIAL: &str = "Endpoints whose handler path is partial";
        const OUTSIDE: &str = "Coverage note: endpoints outside permission-site recovery";

        let guarded = row(SITED, "GET /me").expect("the guarded route lists its site");
        assert!(guarded.contains("guard: flask\\_login.login\\_required"));
        assert!(guarded.contains(&format!(
            "app.py bytes {guard}..{}",
            guard + "@login_required".len()
        )));
        assert!(guarded.contains("| Deterministic | Confirmed |"));
        for other in [NONE, PARTIAL, OUTSIDE] {
            assert!(row(other, "GET /me").is_none(), "{other}");
        }
        assert!(row(SITED, "GET /health").is_none());
        assert!(row(OUTSIDE, "GET /health").is_none());
        let open = row(NONE, "GET /health").expect("the route with no guard: none recovered");
        assert!(open.contains("| read: 1 facts, no unresolved hop |"));
        assert!(row(PARTIAL, "GET /health").is_none());
        assert!(row(OUTSIDE, "GET /orders").is_some());
        for table in [SITED, NONE, PARTIAL] {
            assert!(row(table, "GET /orders").is_none(), "{table}");
        }
        let text = content.to_lowercase();
        assert!(!text.contains("unauthenticated") && !text.contains("unprotected"));

        let index: spec::FactIndex = serde_json::from_str(
            &bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "fact-index-json")
                .unwrap()
                .content,
        )
        .unwrap();
        assert!(!artifact.assertions.is_empty());
        for assertion in &artifact.assertions {
            assert!(
                index.rows.iter().any(|row| {
                    row.cited_by.iter().any(|by| by == "endpoint-permissions")
                        && match &row.fact {
                            core_graph::FactKey::Node { id } => *id == assertion.subject_id,
                            core_graph::FactKey::Edge {
                                source,
                                label,
                                destination,
                            } => format!("{source} {label} {destination}") == assertion.subject_id,
                        }
                }),
                "{} has a fact-index row",
                assertion.subject_id
            );
        }
        assert_eq!(
            crate::build_spec_bundle(
                &graph,
                &decisions,
                &crate::curated::CuratedReview::default(),
                mode
            )
            .unwrap(),
            bundle
        );
        assert_eq!(
            &crate::build_spec_artifact(
                &graph,
                &decisions,
                Some(&crate::curated::CuratedReview::default()),
                mode,
                "endpoint-permissions"
            )
            .unwrap(),
            artifact
        );
    }
}

// AC-0650 (#1818): the export of a real ingest carries the pipeline
// projection. A workflow's jobs are listed in dependency order against their
// declared order, with steps in declared order and spans; a `Containerfile`
// stage that mounts from an earlier stage is ordered after it; GitLab CI and
// a Dockerfile with a heredoc are listed as present and not read, from the
// stored coverage inventory. Every assertion has a fact-index row, and a
// second export and the single-artifact read return the same bytes.
#[test]
fn export_projects_ingested_pipelines_in_dependency_order() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let workflow = "name: Deploy\n\
         on:\n  push:\n    branches: [main]\n\
         jobs:\n\
         \x20 deploy:\n    needs: [build]\n    runs-on: ubuntu-latest\n    steps:\n      - run: ./deploy.sh\n\
         \x20 build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: make build\n";
    std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
    std::fs::write(root.join(".github/workflows/deploy.yml"), workflow).unwrap();
    std::fs::write(
        root.join("Containerfile"),
        "FROM golang:1.23 AS deps\nRUN go mod download\nFROM scratch\n\
         RUN --mount=type=bind,from=deps,target=/deps true\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("docker")).unwrap();
    std::fs::write(
        root.join("docker/legacy.dockerfile"),
        "FROM alpine\nRUN <<EOF\ntrue\nEOF\n",
    )
    .unwrap();
    std::fs::write(root.join(".gitlab-ci.yml"), "build:\n  script: make\n").unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();

    let graph = state.graph.lock().unwrap();
    let decisions = state.decisions.lock().unwrap();
    let build = workflow.find("build:\n").unwrap();
    let checkout = workflow.find("uses: actions/checkout@v4").unwrap();
    for mode in [spec::ExportMode::VerifiedOnly, spec::ExportMode::BestEffort] {
        let bundle = crate::build_spec_bundle(
            &graph,
            &decisions,
            &crate::curated::CuratedReview::default(),
            mode,
        )
        .unwrap();
        let artifact = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "pipelines")
            .unwrap();
        assert_eq!(artifact.file_name, "pipelines.md");
        let content = &artifact.content;
        let row = |start: &str| {
            content
                .lines()
                .find(|line| line.starts_with(start))
                .unwrap_or_else(|| panic!("no row starting {start:?} in:\n{content}"))
        };
        assert!(content.contains("Pipelines: 2. Triggers: 1. Jobs: 4, of which not ordered: 0."));

        // Declared deploy then build; build is needed, so it is first.
        let first = row("| 1 | build | job | 1 | — |");
        assert!(first.contains("| Deterministic | Confirmed |"));
        assert!(first.contains(&format!("deploy.yml bytes {build}..")));
        row("| 2 | deploy | job | 0 | build (via needs) |");
        let step = row("| build | 0 | uses | uses: actions/checkout@v4 |");
        assert!(step.contains(&format!("deploy.yml bytes {checkout}..")));
        row("| build | 1 | run | run: make build |");
        row("| deploy | 0 | run | run: ./deploy.sh |");
        row("| 0 | push | filters: branches: \\[main\\] |");
        // The Containerfile is read, and the mount is a stage relation.
        row("| 1 | stage-0 | stage | 0 | — |");
        row("| 2 | stage-1 | stage | 1 | stage-0 (via mount-from) |");

        let unread = &content[content.find("\n## Files present and not read\n").unwrap()..];
        let unread = &unread[..unread.find("\n## Coverage note").unwrap()];
        let listed = |what: &str| {
            unread
                .lines()
                .find(|line| line.contains(&format!("| {what} | 1 |")))
                .unwrap_or_else(|| panic!("{what} is listed in:\n{unread}"))
        };
        assert!(
            listed("GitLab CI configuration")
                .contains("| .gitlab-ci.yml:1 | GitLab CI configuration is present and not read")
        );
        assert!(
            listed("GitHub Actions workflow or Dockerfile that could not be read exactly")
                .contains(
                    "| docker/legacy.dockerfile:1 | Dockerfile is present and its steps were \
                     not recovered: Dockerfile uses a heredoc"
                )
        );
        assert!(unread.contains("Stored inventories read: 1."));
        assert!(!content.contains("legacy.dockerfile bytes"));

        let index: spec::FactIndex = serde_json::from_str(
            &bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "fact-index-json")
                .unwrap()
                .content,
        )
        .unwrap();
        assert!(!artifact.assertions.is_empty());
        for assertion in &artifact.assertions {
            assert_eq!(
                assertion.provenance.confidence_tier,
                core_prov::ConfidenceTier::Confirmed
            );
            assert!(
                index.rows.iter().any(|row| {
                    row.cited_by.iter().any(|by| by == "pipelines")
                        && match &row.fact {
                            core_graph::FactKey::Node { id } => *id == assertion.subject_id,
                            core_graph::FactKey::Edge {
                                source,
                                label,
                                destination,
                            } => format!("{source} {label} {destination}") == assertion.subject_id,
                        }
                }),
                "{} has a fact-index row",
                assertion.subject_id
            );
        }
        assert_eq!(
            crate::build_spec_bundle(
                &graph,
                &decisions,
                &crate::curated::CuratedReview::default(),
                mode
            )
            .unwrap(),
            bundle
        );
        assert_eq!(
            &crate::build_spec_artifact(
                &graph,
                &decisions,
                Some(&crate::curated::CuratedReview::default()),
                mode,
                "pipelines"
            )
            .unwrap(),
            artifact
        );
    }
}

// AC-0661 (#1791, ADR-0070, SPEC-14): the export of a real ingest carries
// the environments projection. Two Compose files are two environments shown
// by path, neither named from its path; the workload both declare is listed
// once per declaration with the properties that differ and both spans; an
// environment file that names an environment only by its file name is a Gap
// with no span and no value; a Kubernetes manifest of a kind that is not
// read is named in the stored coverage inventory, by kind, as present and
// not read (AC-0662). The reader's facts add no hop
// to any flow and nothing to the topology map, and the toolchain reader's
// Compose fact is still there. Every assertion has a fact-index row, a
// second export and the single-artifact read return the same bytes, a
// second ingest of the same tree gives the same graph content hash, and a
// name-only Gap leaves the graph with its file.
#[test]
fn export_projects_ingested_environments() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let base = "services:\n\
         \x20 web:\n    image: shop/web:1\n    ports:\n      - \"8080:80\"\n    environment:\n      \
         - SESSION_KEY=hunter2-sentinel\n    depends_on:\n      - db\n\
         \x20 db:\n    image: postgres:16\n";
    let prod = "services:\n\
         \x20 web:\n    image: shop/web:2\n    ports:\n      - \"443:8443\"\n    environment:\n      \
         SESSION_KEY: hunter2-sentinel\n";
    std::fs::write(root.join("docker-compose.yml"), base).unwrap();
    std::fs::write(root.join("docker-compose.prod.yml"), prod).unwrap();
    std::fs::write(root.join("staging.env"), "SESSION_KEY=hunter2-sentinel\n").unwrap();
    std::fs::create_dir_all(root.join("k8s")).unwrap();
    std::fs::write(
        root.join("k8s/service.yaml"),
        "apiVersion: v1\nkind: Service\nmetadata:\n  name: web\n",
    )
    .unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let ingest = || {
        let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
        let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
        run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();
    };
    ingest();
    let repo = registered.repo_key.as_str();
    // As the document writes it: literal text, so `_` is escaped.
    let shown = repo.replace('_', "\\_");
    let hashes = || {
        let graph = state.graph.lock().unwrap();
        let (nodes, edges) = graph.read_snapshot().unwrap();
        (
            spec::graph_content_hash(&nodes, &edges),
            deterministic_graph_hashes(&*graph).unwrap(),
        )
    };
    let first = hashes();

    {
        let graph = state.graph.lock().unwrap();
        let decisions = state.decisions.lock().unwrap();
        let (nodes, edges) = graph.read_snapshot().unwrap();
        let workload = |file: &str, name: &str| format!("workload:{repo}@{file}#{name}");
        let web = nodes
            .iter()
            .find(|node| node.id == workload("docker-compose.yml", "web"))
            .unwrap();
        assert_eq!(web.label, "Workload");
        assert_eq!(web.props["prov"]["extractor_id"], "t0.environments");
        assert_eq!(
            web.props["environment_keys"],
            serde_json::json!(["SESSION_KEY"])
        );
        // No value of an environment key is anywhere in the graph.
        assert!(!serialized(&(&nodes, &edges)).contains("hunter2-sentinel"));
        assert!(edges.iter().any(|edge| {
            edge.src == workload("docker-compose.yml", "web")
                && edge.label == "DEPENDS_ON"
                && edge.dst == workload("docker-compose.yml", "db")
        }));
        // The toolchain reader's Compose fact is still recorded.
        let tool = nodes
            .iter()
            .find(|node| node.id == format!("tool:{repo}@docker-compose.yml"))
            .unwrap();
        assert_eq!(tool.props["display"], "Docker Compose");
        assert_eq!(
            tool.props["settings"]["services"],
            serde_json::json!(["web", "db"])
        );
        // No flow and no topology line comes from a workload.
        assert!(flowtracer::trace(&nodes, &edges).is_empty());
        assert!(!spec::topology_mermaid(&nodes, &edges).contains("workload"));

        let staging = format!("gap:environment:{repo}@staging.env");
        let web_base = base.find("image: shop/web:1").unwrap();
        let web_prod = prod.find("image: shop/web:2").unwrap();
        for mode in [spec::ExportMode::VerifiedOnly, spec::ExportMode::BestEffort] {
            let bundle = crate::build_spec_bundle(
                &graph,
                &decisions,
                &crate::curated::CuratedReview::default(),
                mode,
            )
            .unwrap();
            let artifact = bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "environments")
                .unwrap();
            assert_eq!(artifact.file_name, "environments.md");
            let content = &artifact.content;
            let row = |start: &str| {
                content
                    .lines()
                    .find(|line| line.starts_with(start))
                    .unwrap_or_else(|| panic!("no row starting {start:?} in:\n{content}"))
            };
            assert!(content.contains(
                "Environments: 2. Workloads: 3. Environments named only by a file name: 1. \
                 Other items not established: 0."
            ));
            // Two environments, by path; neither is named.
            let listed = row(&format!(
                "| {shown} | docker-compose.prod.yml | none stated | compose | 1 | Deterministic \
                 | Confirmed |"
            ));
            assert!(listed.contains(&format!(
                "docker-compose.prod.yml bytes {}..{}",
                0,
                "services".len()
            )));
            row(&format!(
                "| {shown} | docker-compose.yml | none stated | compose | 2 | Deterministic | \
                 Confirmed |"
            ));
            assert!(!content.contains("| prod |"));
            assert!(!content.contains("| staging |"));
            // The workload both declare: each declaration, what differs, and
            // each span. Environment key names are the same and not listed.
            let declared = &content[content
                .find("\n## Declared in more than one environment\n")
                .unwrap()
                ..content.find("\n## Not established\n").unwrap()];
            let both: Vec<&str> = declared
                .lines()
                .filter(|line| line.starts_with(&format!("| {shown} | web |")))
                .collect();
            assert_eq!(both.len(), 2, "{declared}");
            assert!(both[0].contains(
                "| docker-compose.prod.yml | image: shop/web:2; ports: 443:8443; dependencies: \
                 not stated |"
            ));
            assert!(both[0].contains(&format!("docker-compose.prod.yml bytes {web_prod}..")));
            assert!(both[1].contains(
                "| docker-compose.yml | image: shop/web:1; ports: 8080:80; dependencies: db |"
            ));
            assert!(both[1].contains(&format!("docker-compose.yml bytes {web_base}..")));
            assert!(!declared.contains("| db |"));
            assert!(!content.to_lowercase().contains("overrid"));
            // The name-only environment file: a Gap, no span, no value.
            let gap = row(&format!(
                "| {shown} | staging.env | — | environment named only by a file name |"
            ));
            assert!(gap.ends_with("| Deterministic | Gap | none stored for an environment file |"));
            assert!(!content.contains("hunter2-sentinel"));
            // Nowhere is an environment labelled with its path.
            assert!(!content.contains("Environment: docker-compose"));
            assert!(content.contains(&format!("| Environment: env:{shown}@docker-compose.yml |")));

            let index: spec::FactIndex = serde_json::from_str(
                &bundle
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.id == "fact-index-json")
                    .unwrap()
                    .content,
            )
            .unwrap();
            // 2 environments, 3 workloads, 3 DECLARES, 1 DEPENDS_ON, 1 Gap.
            assert_eq!(artifact.assertions.len(), 2 + 3 + 3 + 1 + 1);
            for assertion in &artifact.assertions {
                assert!(
                    index.rows.iter().any(|row| {
                        row.cited_by.iter().any(|by| by == "environments")
                            && match &row.fact {
                                core_graph::FactKey::Node { id } => *id == assertion.subject_id,
                                core_graph::FactKey::Edge {
                                    source,
                                    label,
                                    destination,
                                } => {
                                    format!("{source} {label} {destination}")
                                        == assertion.subject_id
                                }
                            }
                    }),
                    "{} has a fact-index row",
                    assertion.subject_id
                );
            }
            assert!(
                artifact
                    .assertions
                    .iter()
                    .any(|assertion| assertion.subject_id == staging
                        && assertion.provenance.evidence.is_empty())
            );
            assert_eq!(
                crate::build_spec_bundle(
                    &graph,
                    &decisions,
                    &crate::curated::CuratedReview::default(),
                    mode
                )
                .unwrap(),
                bundle
            );
            assert_eq!(
                &crate::build_spec_artifact(
                    &graph,
                    &decisions,
                    Some(&crate::curated::CuratedReview::default()),
                    mode,
                    "environments"
                )
                .unwrap(),
                artifact
            );

            // The Kubernetes manifest is in the stored coverage inventory,
            // under its kind.
            let coverage = &bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "coverage")
                .unwrap()
                .content;
            assert!(coverage.contains("t0.environments"), "{coverage}");
            assert!(
                coverage.contains("kubernetes-manifest:Service"),
                "{coverage}"
            );
            assert!(
                coverage.contains(
                    "manifest is present and not read: no installed producer recovers \
                     environments from it"
                ),
                "{coverage}"
            );
        }
    }

    // The same tree again: the same graph.
    ingest();
    assert_eq!(hashes(), first);
    // And in a fresh graph, as the determinism gate ingests it.
    let fresh = || {
        let extraction = extract_tree(
            &root,
            repo,
            "workdir",
            &[],
            &BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        load_into_graph(&mut graph, &extraction, repo, &root, "workdir").unwrap();
        let (nodes, edges) = graph.read_snapshot().unwrap();
        (
            spec::graph_content_hash(&nodes, &edges),
            deterministic_graph_hashes(&graph).unwrap(),
        )
    };
    assert_eq!(fresh(), fresh());

    // The Gap has no evidence span to be owned by; it still leaves the graph
    // when its file does.
    std::fs::remove_file(root.join("staging.env")).unwrap();
    ingest();
    let (nodes, _) = state.graph.lock().unwrap().read_snapshot().unwrap();
    assert!(
        nodes
            .iter()
            .all(|node| !node.id.starts_with("gap:environment:"))
    );
    assert!(nodes.iter().any(|node| node.label == "Environment"));
}

// AC-0662 (#1791, ADR-0070 decision 8, SPEC-14 increment 2): the issue's
// acceptance tree through a real ingest and export. A Compose file is one
// environment; a kustomize base and its two overlays are three, each listing
// the base workload with its span, the overlays through the base and each
// with what its own patch states; an environment is named only where its
// file states a namespace; a bare `staging.env` is a Gap and not an
// environment. No value reaches the graph or the document, every assertion
// has a fact-index row, a second ingest gives the same graph content hash,
// and the facts leave the graph with their files.
#[test]
fn export_projects_ingested_kustomize_environments() {
    const SENTINEL: &str = "hunter2-sentinel";
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    let compose = format!(
        "services:\n  web:\n    image: shop/web:1\n    environment:\n      - \
         SESSION_KEY={SENTINEL}\n    depends_on:\n      - db\n  db:\n    image: postgres:16\n"
    );
    let base = "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\nresources:\n  - \
                deployment.yaml\n  - service.yaml\n";
    let deployment = format!(
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  replicas: 1\n  \
         template:\n    spec:\n      containers:\n        - name: web\n          image: \
         shop/web:1\n          ports:\n            - containerPort: 8080\n          env:\n            \
         - name: SESSION_KEY\n              value: {SENTINEL}\n          envFrom:\n            - \
         secretRef:\n                name: web-secrets\n"
    );
    let staging = "namespace: shop-staging\nresources:\n  - ../../base\npatches:\n  - path: \
                   patch.yaml\nimages:\n  - name: shop/web\n    newTag: 2-rc\n";
    let staging_patch = format!(
        "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  replicas: 2\n  \
         template:\n    spec:\n      containers:\n        - name: web\n          env:\n            - \
         name: LOG_LEVEL\n              value: {SENTINEL}\n"
    );
    let prod = "resources:\n  - ../../base\npatchesStrategicMerge:\n  - patch.yaml\nnamePrefix: \
                live-\n";
    let prod_patch = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  \
                      replicas: 5\n";
    let files: [(&str, &str); 9] = [
        ("docker-compose.yml", &compose),
        ("k8s/base/kustomization.yaml", base),
        ("k8s/base/deployment.yaml", &deployment),
        (
            "k8s/base/service.yaml",
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: web\n",
        ),
        ("k8s/overlays/staging/kustomization.yaml", staging),
        ("k8s/overlays/staging/patch.yaml", &staging_patch),
        ("k8s/overlays/prod/kustomization.yaml", prod),
        ("k8s/overlays/prod/patch.yaml", prod_patch),
        ("staging.env", "SESSION_KEY=hunter2-sentinel\n"),
    ];
    for (path, body) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let ingest = || {
        let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
        let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
        run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();
    };
    ingest();
    let repo = registered.repo_key.as_str();
    let shown = repo.replace('_', "\\_");
    let hashes = || {
        let graph = state.graph.lock().unwrap();
        let (nodes, edges) = graph.read_snapshot().unwrap();
        (
            spec::graph_content_hash(&nodes, &edges),
            deterministic_graph_hashes(&*graph).unwrap(),
        )
    };
    let first = hashes();
    let web = format!("workload:{repo}@k8s/base/deployment.yaml#0:Deployment/web");
    let environment = |path: &str| format!("env:{repo}@{path}");

    {
        let graph = state.graph.lock().unwrap();
        let decisions = state.decisions.lock().unwrap();
        let (nodes, edges) = graph.read_snapshot().unwrap();
        // Four environments and no fifth: `staging.env` is a Gap.
        let mut environments: Vec<&str> = nodes
            .iter()
            .filter(|node| node.label == "Environment")
            .map(|node| node.id.as_str())
            .collect();
        environments.sort_unstable();
        assert_eq!(
            environments,
            [
                environment("docker-compose.yml"),
                environment("k8s/base/kustomization.yaml"),
                environment("k8s/overlays/prod/kustomization.yaml"),
                environment("k8s/overlays/staging/kustomization.yaml"),
            ]
        );
        let gap = nodes
            .iter()
            .find(|node| node.id == format!("gap:environment:{repo}@staging.env"))
            .unwrap();
        assert_eq!(gap.label, "Gap");
        assert_eq!(gap.props["kind"], "name-only-environment");
        // One Kubernetes workload: the patch files are read only as patches.
        let workloads: Vec<&str> = nodes
            .iter()
            .filter(|node| node.label == "Workload" && node.props["kind"] == "k8s-deployment")
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(workloads, [web.as_str()]);
        for path in [
            "k8s/base/kustomization.yaml",
            "k8s/overlays/prod/kustomization.yaml",
            "k8s/overlays/staging/kustomization.yaml",
        ] {
            assert!(
                edges.iter().any(|edge| edge.src == environment(path)
                    && edge.label == "DECLARES"
                    && edge.dst == web),
                "{path}"
            );
        }
        let patched: Vec<&str> = edges
            .iter()
            .filter(|edge| edge.label == "PATCHES")
            .map(|edge| edge.src.as_str())
            .collect();
        assert_eq!(patched.len(), 2);
        // No value is anywhere in the graph.
        assert!(!serialized(&(&nodes, &edges)).contains(SENTINEL));
        // No flow and no topology line comes from the new facts.
        assert!(flowtracer::trace(&nodes, &edges).is_empty());
        assert!(!spec::topology_mermaid(&nodes, &edges).contains("workload"));

        let span = |text: &str, part: &str| {
            let start = text.find(part).unwrap();
            format!("bytes {start}..{}", start + part.len())
        };
        for mode in [spec::ExportMode::VerifiedOnly, spec::ExportMode::BestEffort] {
            let bundle = crate::build_spec_bundle(
                &graph,
                &decisions,
                &crate::curated::CuratedReview::default(),
                mode,
            )
            .unwrap();
            let artifact = bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "environments")
                .unwrap();
            let content = &artifact.content;
            let row = |start: &str| {
                content
                    .lines()
                    .find(|line| line.starts_with(start))
                    .unwrap_or_else(|| panic!("no row starting {start:?} in:\n{content}"))
            };
            assert!(content.contains(
                "Environments: 4. Workloads: 3. Environments named only by a file name: 1. \
                 Other items not established: 1. Workloads no environment declares: 0."
            ));
            // Each environment by its path, with its span; only the one
            // that states a namespace has a name.
            let listed = row(&format!(
                "| {shown} | docker-compose.yml | none stated | compose | 2 | Deterministic | \
                 Confirmed |"
            ));
            assert!(listed.contains("docker-compose.yml bytes 0..8 @"));
            let listed = row(&format!(
                "| {shown} | k8s/base/kustomization.yaml | none stated | kustomize | 1 | \
                 Deterministic | Confirmed |"
            ));
            assert!(listed.contains(&span(base, "resources")));
            assert!(listed.ends_with(
                "| included by k8s/overlays/prod/kustomization.yaml, \
                 k8s/overlays/staging/kustomization.yaml |"
            ));
            let listed = row(&format!(
                "| {shown} | k8s/overlays/prod/kustomization.yaml | none stated | kustomize | 1 | \
                 Deterministic | Confirmed |"
            ));
            assert!(listed.contains(&span(prod, "resources")));
            assert!(listed.contains("sets `namePrefix`"));
            let listed = row(&format!(
                "| {shown} | k8s/overlays/staging/kustomization.yaml | shop-staging | kustomize | \
                 1 | Deterministic | Confirmed |"
            ));
            assert!(listed.contains(&span(staging, "resources")));
            assert!(!content.contains("| prod |"));
            assert!(!content.contains("| staging |"));

            // Each environment lists its workloads, each with its span.
            let compose_web = compose.find("image: shop/web:1").unwrap();
            assert!(
                row(&format!(
                    "| {shown} | docker-compose.yml | web | shop/web:1 |"
                ))
                .contains(&format!("docker-compose.yml bytes {compose_web}.."))
            );
            row(&format!(
                "| {shown} | docker-compose.yml | db | postgres:16 |"
            ));
            let document = format!("k8s/base/deployment.yaml bytes 0..{} @", deployment.len());
            for (path, through) in [
                ("k8s/base/kustomization.yaml", "its own entry"),
                (
                    "k8s/overlays/prod/kustomization.yaml",
                    "k8s/base/kustomization.yaml",
                ),
                (
                    "k8s/overlays/staging/kustomization.yaml",
                    "k8s/base/kustomization.yaml",
                ),
            ] {
                let listed = row(&format!(
                    "| {shown} | {path} | {through} | web | Deployment | \
                     k8s/base/deployment.yaml | — | 1 | web | shop/web:1 | 8080 | SESSION\\_KEY | \
                     Secret/web-secrets | Deterministic | Confirmed |"
                ));
                assert!(listed.contains(&document), "{listed}");
            }

            // `web` is declared twice, as a Compose service and as one
            // Deployment that three kustomizations reach: one row for each
            // fact, never one for each reaching environment, and only what
            // both kinds state is compared.
            let declared = &content[content
                .find("\n## Declared in more than one environment\n")
                .unwrap()
                ..content
                    .find("\nA Kubernetes workload that two or more kustomizations reach")
                    .unwrap()];
            let facts: Vec<&str> = declared
                .lines()
                .filter(|line| line.starts_with(&format!("| {shown} | web |")))
                .collect();
            assert_eq!(facts.len(), 2, "{declared}");
            assert!(facts[0].contains(
                "| Compose service | docker-compose.yml | docker-compose.yml | ports: not \
                 stated |"
            ));
            assert!(facts[1].contains(
                "| Deployment | k8s/base/deployment.yaml | k8s/base/kustomization.yaml, \
                 k8s/overlays/prod/kustomization.yaml, k8s/overlays/staging/kustomization.yaml \
                 | ports: 8080 |"
            ));
            assert!(!declared.contains("replicas"));

            // The workload three environments reach: one row for each, with
            // that environment's patch and `images` entries.
            let several = &content[content
                .find("\nA Kubernetes workload that two or more kustomizations reach")
                .unwrap()
                ..content
                    .find("\n## Workloads no environment declares\n")
                    .unwrap()];
            let rows: Vec<&str> = several
                .lines()
                .filter(|line| line.starts_with(&format!("| {shown} | web | Deployment |")))
                .collect();
            assert_eq!(rows.len(), 3, "{several}");
            assert!(rows[0].contains(
                "| k8s/base/kustomization.yaml | its own entry | no patch stated | none names \
                 its images |"
            ));
            assert!(rows[0].contains(&span(base, "deployment.yaml")));
            assert!(rows[1].contains(
                "| k8s/overlays/prod/kustomization.yaml | k8s/base/kustomization.yaml | patch \
                 k8s/overlays/prod/patch.yaml states replicas 5 | none names its images |"
            ));
            assert!(rows[1].contains(&span(prod, "../../base")));
            assert!(rows[1].contains("the deployed name differs from the recorded one"));
            assert!(rows[2].contains(
                "| k8s/overlays/staging/kustomization.yaml | k8s/base/kustomization.yaml | patch \
                 k8s/overlays/staging/patch.yaml states replicas 2; container web: environment \
                 key names LOG\\_LEVEL | name shop/web, newTag 2-rc |"
            ));
            assert!(rows[2].contains(&span(staging, "../../base")));
            assert!(!content.to_lowercase().contains("overrid"));

            // `staging.env`: a Gap, no span, no value, and not an environment.
            let gap = row(&format!(
                "| {shown} | staging.env | — | environment named only by a file name |"
            ));
            assert!(gap.ends_with("| Deterministic | Gap | none stored for an environment file |"));
            let listed = &content[content.find("\n## Environments\n").unwrap()
                ..content.find("\n## Workloads by environment\n").unwrap()];
            assert!(!listed.contains("staging.env"));
            // The transformer is named, not applied.
            row(&format!(
                "| {shown} | k8s/overlays/prod/kustomization.yaml | — | kustomization | \
                 \\`namePrefix\\` is present and not evaluated |"
            ));
            assert!(!content.contains("live-"));
            assert!(!content.contains(SENTINEL));

            let index: spec::FactIndex = serde_json::from_str(
                &bundle
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.id == "fact-index-json")
                    .unwrap()
                    .content,
            )
            .unwrap();
            // 4 environments, 3 workloads, 2 + 3 DECLARES, 1 DEPENDS_ON,
            // 2 PATCHES and 2 Gaps.
            assert_eq!(artifact.assertions.len(), 4 + 3 + 5 + 1 + 2 + 2);
            for assertion in &artifact.assertions {
                assert!(
                    index.rows.iter().any(|row| {
                        row.cited_by.iter().any(|by| by == "environments")
                            && match &row.fact {
                                core_graph::FactKey::Node { id } => *id == assertion.subject_id,
                                core_graph::FactKey::Edge {
                                    source,
                                    label,
                                    destination,
                                } => {
                                    format!("{source} {label} {destination}")
                                        == assertion.subject_id
                                }
                            }
                    }),
                    "{} has a fact-index row",
                    assertion.subject_id
                );
            }
            assert_eq!(
                crate::build_spec_bundle(
                    &graph,
                    &decisions,
                    &crate::curated::CuratedReview::default(),
                    mode
                )
                .unwrap(),
                bundle
            );
            // The Service is in the stored coverage inventory by its kind;
            // the Deployment and the kustomizations are read and are not.
            let coverage = &bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "coverage")
                .unwrap()
                .content;
            assert!(
                coverage.contains("kubernetes-manifest:Service"),
                "{coverage}"
            );
            assert!(!coverage.contains("kubernetes-manifest:Deployment"));
            assert!(!coverage.contains("kustomize-file"));
        }
    }

    // The same tree again: the same graph.
    ingest();
    assert_eq!(hashes(), first);
    // And in a fresh graph, as the determinism gate ingests it.
    let fresh = || {
        let extraction = extract_tree(
            &root,
            repo,
            "workdir",
            &[],
            &BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        load_into_graph(&mut graph, &extraction, repo, &root, "workdir").unwrap();
        let (nodes, edges) = graph.read_snapshot().unwrap();
        (
            spec::graph_content_hash(&nodes, &edges),
            deterministic_graph_hashes(&graph).unwrap(),
        )
    };
    assert_eq!(fresh(), fresh());

    // One overlay goes: its environment, its relations and its Gap go with
    // it, and the workload the others still reach stays.
    std::fs::remove_dir_all(root.join("k8s/overlays/prod")).unwrap();
    ingest();
    {
        let (nodes, edges) = state.graph.lock().unwrap().read_snapshot().unwrap();
        let prod = environment("k8s/overlays/prod/kustomization.yaml");
        assert!(nodes.iter().all(|node| node.id != prod));
        assert!(
            nodes
                .iter()
                .all(|node| !node.id.ends_with("@k8s/overlays/prod/kustomization.yaml"))
        );
        assert!(edges.iter().all(|edge| edge.src != prod));
        assert!(nodes.iter().any(|node| node.id == web));
        assert_eq!(
            edges
                .iter()
                .filter(|edge| edge.label == "DECLARES" && edge.dst == web)
                .count(),
            2
        );
    }
    // Everything goes: no kustomize or Kubernetes fact is left, and the
    // Compose environment is untouched.
    std::fs::remove_dir_all(root.join("k8s")).unwrap();
    std::fs::remove_file(root.join("staging.env")).unwrap();
    ingest();
    let (nodes, edges) = state.graph.lock().unwrap().read_snapshot().unwrap();
    assert!(nodes.iter().all(|node| !node.id.contains("@k8s/")));
    assert!(
        edges
            .iter()
            .all(|edge| !edge.src.contains("@k8s/") && !edge.dst.contains("@k8s/"))
    );
    assert!(edges.iter().all(|edge| edge.label != "PATCHES"));
    assert!(
        nodes
            .iter()
            .all(|node| !node.id.starts_with("gap:environment:"))
    );
    let left: Vec<&str> = nodes
        .iter()
        .filter(|node| node.label == "Environment")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(left, [environment("docker-compose.yml")]);
}

// AC-0651 (#1821): the export of a real ingest carries the configuration
// key projection. A key an environment file declares and source reads is
// listed once with both; a key read and never declared and a key declared
// and never read are marked; the file with a read whose key is not a literal
// is listed; no value of the environment file reaches the document. Every
// assertion has a fact-index row, and a second export and the
// single-artifact read return the same bytes.
#[test]
fn export_projects_ingested_configuration_keys() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(
        root.join(".env.example"),
        "DATABASE_URL=postgres://localhost/hunter2\nUNUSED_FLAG=1\n",
    )
    .unwrap();
    let config = "export function load(name: string) {\n  \
                  return [process.env.DATABASE_URL, process.env[name]];\n}\n";
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/config.ts"), config).unwrap();
    let settings = "import os\n\nHOST = os.getenv(\"SMTP_HOST\")\n";
    std::fs::write(root.join("settings.py"), settings).unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();

    let graph = state.graph.lock().unwrap();
    let decisions = state.decisions.lock().unwrap();
    let read = config.find("process.env.DATABASE_URL").unwrap();
    let host = settings.find("os.getenv").unwrap();
    for mode in [spec::ExportMode::VerifiedOnly, spec::ExportMode::BestEffort] {
        let bundle = crate::build_spec_bundle(
            &graph,
            &decisions,
            &crate::curated::CuratedReview::default(),
            mode,
        )
        .unwrap();
        let artifact = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "configuration")
            .unwrap();
        assert_eq!(artifact.file_name, "configuration.md");
        let content = &artifact.content;
        let row = |part: &str| {
            content
                .lines()
                .find(|line| line.starts_with("| ") && line.contains(part))
                .unwrap_or_else(|| panic!("no row containing {part:?} in:\n{content}"))
        };
        assert!(content.contains(
            "Keys: 3. Declared and read: 1. Read, never declared: 1. Declared, never read: 1. \
             Environment files: 1. Files with reads that cannot be named: 1."
        ));
        row("| DATABASE\\_URL | declared and read | 1 | 1 | — |");
        row("| SMTP\\_HOST | read, never declared | 0 | 1 | — |");
        row("| UNUSED\\_FLAG | declared, never read | 1 | 0 | — |");
        row(
            "| DATABASE\\_URL | .env.example | Deterministic | Confirmed | none stored for an \
             environment file | — |",
        );
        let named = row(
            "| DATABASE\\_URL | src/config.ts | typescript | load | Deterministic | \
                         Confirmed |",
        );
        assert!(named.contains(&format!("src/config.ts bytes {read}..")));
        let module = row(
            "| SMTP\\_HOST | settings.py | python | file level | Deterministic | \
                          Confirmed |",
        );
        assert!(module.contains(&format!("settings.py bytes {host}..")));
        let unnamed = &content[content
            .find("\n## Files with reads that cannot be named\n")
            .unwrap()..];
        let unnamed = &unnamed[..unnamed.find("\n## Languages read\n").unwrap()];
        assert!(unnamed.contains("| src/config.ts | typescript | load | Deterministic |"));
        assert!(!unnamed.contains("settings.py"));
        // Names only: no value of the environment file is in the document.
        assert!(!content.contains("hunter2") && !content.contains("postgres"));

        let index: spec::FactIndex = serde_json::from_str(
            &bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "fact-index-json")
                .unwrap()
                .content,
        )
        .unwrap();
        // The environment file, three reads and their three relations.
        assert_eq!(artifact.assertions.len(), 7);
        for assertion in &artifact.assertions {
            assert_eq!(
                assertion.provenance.confidence_tier,
                core_prov::ConfidenceTier::Confirmed
            );
            assert!(
                index.rows.iter().any(|row| {
                    row.cited_by.iter().any(|by| by == "configuration")
                        && match &row.fact {
                            core_graph::FactKey::Node { id } => *id == assertion.subject_id,
                            core_graph::FactKey::Edge {
                                source,
                                label,
                                destination,
                            } => format!("{source} {label} {destination}") == assertion.subject_id,
                        }
                }),
                "{} has a fact-index row",
                assertion.subject_id
            );
        }
        assert_eq!(
            crate::build_spec_bundle(
                &graph,
                &decisions,
                &crate::curated::CuratedReview::default(),
                mode
            )
            .unwrap(),
            bundle
        );
        assert_eq!(
            &crate::build_spec_artifact(
                &graph,
                &decisions,
                Some(&crate::curated::CuratedReview::default()),
                mode,
                "configuration"
            )
            .unwrap(),
            artifact
        );
    }
}

/// The rows of `external.md` under `## heading`, as cells.
fn external_rows(content: &str, heading: &str) -> Vec<Vec<String>> {
    let start = content
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("no section {heading} in:\n{content}"));
    content[start..]
        .lines()
        .skip(1)
        .skip_while(|line| !line.starts_with("|---"))
        .skip(1)
        .take_while(|line| line.starts_with('|'))
        .map(|line| {
            line.trim_matches('|')
                .split(" | ")
                .map(|cell| cell.trim().to_string())
                .collect()
        })
        .collect()
}

const EXTERNAL_CLIENT: &str = "export function Checkout() {\n  \
    const submit = () => fetch('https://orders.example.com/v1/orders', { method: 'POST' });\n  \
    const charge = () => fetch('https://api.stripe.com/v1/charges', { method: 'POST' });\n  \
    return <button onClick={submit}>Order</button>;\n}\n";
const EXTERNAL_WORKER: &str = "import requests\n\n\ndef rates():\n    \
    return requests.get(\"https://api.tax.example/v2/rates\")\n";

// AC-0660 (#1788): through the real adapters, with no system manifest, a
// literal third-party `fetch` and a literal third-party `requests` call are
// listed in `external.md` as outside, each with its site, method, path and
// span, as Gap facts. The `fetch` call is one site although the observed
// client pass records it as well. Every assertion has a fact-index row, and
// a second export and the single-artifact read return the same bytes.
#[test]
fn export_lists_ingested_third_party_calls_as_outside() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "web");
    std::fs::write(root.join("client.tsx"), EXTERNAL_CLIENT).unwrap();
    std::fs::write(root.join("worker.py"), EXTERNAL_WORKER).unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();

    let graph = state.graph.lock().unwrap();
    let decisions = state.decisions.lock().unwrap();
    let charge = EXTERNAL_CLIENT
        .find("fetch('https://api.stripe.com")
        .unwrap();
    let rates = EXTERNAL_WORKER.find("requests.get").unwrap();
    for mode in [spec::ExportMode::VerifiedOnly, spec::ExportMode::BestEffort] {
        let bundle = crate::build_spec_bundle(
            &graph,
            &decisions,
            &crate::curated::CuratedReview::default(),
            mode,
        )
        .unwrap();
        let artifact = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "external")
            .unwrap();
        assert_eq!(artifact.file_name, "external.md");
        let content = &artifact.content;
        // Without a declared binding the sibling service's host is outside
        // too, and the document says so.
        let origins: Vec<String> = external_rows(content, "External services")
            .into_iter()
            .map(|row| format!("{} {} {}", row[0], row[1], row[2]))
            .collect();
        assert_eq!(
            origins,
            [
                "https://api.stripe.com outside 1",
                "https://api.tax.example outside 1",
                "https://orders.example.com outside 1",
            ],
            "{content}"
        );
        assert!(content.contains(
            "**Without declared bindings, a call to another registered repository by absolute \
             URL is listed as outside.**"
        ));
        let sites = external_rows(content, "Sites of each external service");
        let site = |origin: &str| {
            sites
                .iter()
                .find(|row| row[0] == origin)
                .unwrap_or_else(|| panic!("no site of {origin} in:\n{content}"))
        };
        let stripe = site("https://api.stripe.com");
        assert_eq!(stripe[2..5], ["POST", "/v1/charges", "fetch"]);
        assert_eq!(stripe[5..7], ["Deterministic", "Gap"]);
        assert!(
            stripe[7].contains(&format!("client.tsx bytes {charge}..")),
            "{stripe:?}"
        );
        let tax = site("https://api.tax.example");
        assert_eq!(tax[1..4], ["rates", "GET", "/v2/rates"]);
        assert_eq!(tax[5..7], ["Deterministic", "Gap"]);
        assert!(
            tax[7].contains(&format!("worker.py bytes {rates}..")),
            "{tax:?}"
        );
        assert!(
            external_rows(content, "Unclassified")[0][1] == "None in this export",
            "{content}"
        );

        let index: spec::FactIndex = serde_json::from_str(
            &bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "fact-index-json")
                .unwrap()
                .content,
        )
        .unwrap();
        assert!(!artifact.assertions.is_empty());
        for assertion in &artifact.assertions {
            assert!(
                index.rows.iter().any(|row| {
                    row.cited_by.iter().any(|by| by == "external")
                        && match &row.fact {
                            core_graph::FactKey::Node { id } => *id == assertion.subject_id,
                            core_graph::FactKey::Edge {
                                source,
                                label,
                                destination,
                            } => format!("{source} {label} {destination}") == assertion.subject_id,
                        }
                }),
                "{} has a fact-index row",
                assertion.subject_id
            );
        }
        assert_eq!(
            crate::build_spec_bundle(
                &graph,
                &decisions,
                &crate::curated::CuratedReview::default(),
                mode
            )
            .unwrap(),
            bundle
        );
        assert_eq!(
            &crate::build_spec_artifact(
                &graph,
                &decisions,
                Some(&crate::curated::CuratedReview::default()),
                mode,
                "external"
            )
            .unwrap(),
            artifact
        );
    }
}

// AC-0660 (#1788, #1843): through the production `add_system` path, with a
// declared service binding, the call the binding pass joins to the other
// repository's endpoint is not listed in `external.md`, as a call or as an
// observed client, and is not counted as outside. The third-party calls the
// binding pass evaluated are listed as outside, each with its site and
// span: their Gaps record that no declared binding names their origin.
#[test]
fn export_does_not_list_a_call_bound_into_another_repository() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let system = directory(dir.path(), "system");
    let api = directory(&system, "api");
    let web = directory(&system, "web");
    std::fs::write(
        api.join("server.ts"),
        "import express from 'express';\nconst app = express();\n\
         app.post('/v1/orders', (req, res) => {});\n",
    )
    .unwrap();
    std::fs::write(web.join("client.tsx"), EXTERNAL_CLIENT).unwrap();
    std::fs::write(web.join("worker.py"), EXTERNAL_WORKER).unwrap();
    let manifest = system.join(ingest::manifest::MANIFEST_NAME);
    std::fs::write(
        &manifest,
        "[[repos]]\nurl = \"api\"\n[[repos]]\nurl = \"web\"\n[[services]]\nname = \"orders\"\n\
         repo = \"api\"\nbase_urls = [\"https://orders.example.com\"]\n",
    )
    .unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    crate::add_system_blocking(
        manifest.to_string_lossy().into_owned(),
        app.handle().clone(),
    )
    .unwrap();

    let state = tauri::Manager::state::<AppState>(&*app);
    let graph = state.graph.lock().unwrap();
    let decisions = state.decisions.lock().unwrap();
    // The bound call is a relation into the other repository.
    let (_, edges) = graph.read_snapshot().unwrap();
    assert!(
        edges.iter().any(|edge| edge.label == "FETCHES"
            && edge.dst.ends_with("@POST:/v1/orders")
            && edge.props["binding"][0]["service"] == "orders"),
        "the binding pass joined the call"
    );
    let bundle = crate::build_spec_bundle(
        &graph,
        &decisions,
        &crate::curated::CuratedReview::default(),
        spec::ExportMode::VerifiedOnly,
    )
    .unwrap();
    let content = &bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == "external")
        .unwrap()
        .content;
    for heading in [
        "External services",
        "Sites of each external service",
        "External data flows",
        "Unclassified",
    ] {
        for row in external_rows(content, heading) {
            assert!(
                row.iter().all(|cell| !cell.contains("orders.example.com")),
                "{heading}: {row:?}"
            );
        }
    }
    assert!(
        content.contains("Outside targets: 2. Unclassified targets: 0."),
        "{content}"
    );
    let origins: Vec<String> = external_rows(content, "External services")
        .into_iter()
        .map(|row| format!("{} {} {}", row[0], row[1], row[2]))
        .collect();
    assert_eq!(
        origins,
        [
            "https://api.stripe.com outside 1",
            "https://api.tax.example outside 1",
        ],
        "{content}"
    );
    let sites = external_rows(content, "Sites of each external service");
    let site = |origin: &str| {
        sites
            .iter()
            .find(|row| row[0] == origin)
            .unwrap_or_else(|| panic!("no site of {origin} in:\n{content}"))
    };
    let charge = EXTERNAL_CLIENT
        .find("fetch('https://api.stripe.com")
        .unwrap();
    let rates = EXTERNAL_WORKER.find("requests.get").unwrap();
    let stripe = site("https://api.stripe.com");
    assert_eq!(stripe[2..4], ["POST", "/v1/charges"]);
    assert_eq!(stripe[5..7], ["Deterministic", "Gap"]);
    assert!(
        stripe[7].contains(&format!("client.tsx bytes {charge}..")),
        "{stripe:?}"
    );
    let tax = site("https://api.tax.example");
    assert_eq!(tax[1..4], ["rates", "GET", "/v2/rates"]);
    assert!(
        tax[7].contains(&format!("worker.py bytes {rates}..")),
        "{tax:?}"
    );
    assert!(
        external_rows(content, "Unclassified")[0][1] == "None in this export",
        "{content}"
    );
    // The stored facts carry the outcome; the sentence is not what decided.
    let (nodes, _) = graph.read_snapshot().unwrap();
    let states: Vec<&str> = nodes
        .iter()
        .filter(|node| node.id.starts_with("gap:fetch:"))
        .filter_map(|node| node.props[events::BINDING_STATE].as_str())
        .collect();
    assert_eq!(states, [events::BINDING_STATE_NONE; 2], "{nodes:?}");
}

// AC-0314: when receipt store capacity omits receipts during recovery,
// publication records per-fact StoreCapacity omissions while keeping the
// retained fact's source binding intact. (T-0314)
#[test]
fn captured_ingest_records_per_fact_store_capacity_omissions_and_retains_bound_fact() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "project");
    std::fs::write(
        root.join("rules.ts"),
        "export function alpha(ok: boolean) { if (!ok) return false; return true; }\n\
         export function beta() { return 2; }\n",
    )
    .unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(app_state(&app_data));
    app.manage(crate::PreflightRuns::default());
    let state = tauri::Manager::state::<AppState>(&*app);
    state.primary_sources.receipts.lock().unwrap().capacity.0 = 1;
    let registered = register_local_source(&state, &root).unwrap();
    let operation = source_operation(&state, vec![(registered.clone(), false)]).unwrap();
    let (_, execution) = start_job(&state, &registered.ingest_job_kind()).unwrap();
    run_ingest(&registered, &operation, &execution, app.handle(), &state).unwrap();

    let graph = state.graph.lock().unwrap();
    let bindings = graph
        .source_bindings_for_repo(&registered.repo_key)
        .unwrap();
    assert_eq!(bindings.len(), 1);
    let bound_fact = bindings[0].fact.clone();
    assert_eq!(
        graph.current_source_binding(&bound_fact).unwrap(),
        Some(bindings[0].clone())
    );
    let bound_selection = graph
        .read_source_selection_snapshot(std::slice::from_ref(&bound_fact))
        .unwrap();
    assert_eq!(
        bound_selection.selections[0].omission,
        core_graph::source::OmissionLookup::NotOmitted
    );

    let (nodes, edges) = graph.read_snapshot().unwrap();
    let facts: Vec<_> = nodes
        .iter()
        .map(core_graph::source::FactKey::from_node)
        .chain(edges.iter().map(core_graph::source::FactKey::from_edge))
        .collect();
    let omitted = core_graph::source::OmissionLookup::Omitted {
        repo_key: registered.repo_key.clone(),
        kind: core_graph::source::ReceiptOmissionKind::StoreCapacity,
    };
    let mut omitted_facts = Vec::new();
    for chunk in facts.chunks(core_graph::source::MAX_SOURCE_SELECTION_KEYS) {
        let selection = graph.read_source_selection_snapshot(chunk).unwrap();
        omitted_facts.extend(
            selection
                .selections
                .into_iter()
                .filter(|selection| selection.omission == omitted)
                .map(|selection| selection.fact),
        );
    }
    assert!(
        !omitted_facts.is_empty(),
        "expected at least one StoreCapacity omission record"
    );
    assert!(!omitted_facts.contains(&bound_fact));
}

// AC-0593 (#1765): every completed ingest records the graph content hash, it
// is shown with its source registration, and the export manifest carries the
// whole-graph hash with the source revisions. Equality holds per
// registration; two `local/` registrations of byte-identical trees differ.
#[test]
fn graph_hash_is_recorded_per_registration_and_carried_by_the_export() {
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let first_root = directory(dir.path(), "first/project");
    let second_root = directory(dir.path(), "second/project");
    for root in [&first_root, &second_root] {
        std::fs::write(
            root.join("app.ts"),
            "import { helper } from './helper';\nexport function run() { helper(); }\n",
        )
        .unwrap();
        std::fs::write(root.join("helper.ts"), "export function helper() {}\n").unwrap();
    }
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    let state = app_state(&app_data);
    let mut sources = Vec::new();
    for root in [&first_root, &second_root] {
        crate::preflight_uncancelled(root.to_str().unwrap(), app.handle(), &state).unwrap();
        let source = state.sources.lock().unwrap().get_by_root(root).unwrap();
        sources.push(source.unwrap());
    }
    let shown = |state: &AppState, repo: &str| {
        crate::system_contents_for(state)
            .unwrap()
            .into_iter()
            .find(|entry| entry.repo == repo)
            .map(|entry| (entry.commit, entry.graph_hash))
    };

    recover(&state, &sources[0]);
    let first_hash = state.metrics.lock().unwrap().history(1).unwrap()[0]
        .content_hash
        .clone();
    assert_eq!(
        shown(&state, &sources[0].repo_key),
        Some(("workdir".into(), Some(first_hash.clone())))
    );

    // A second registration's ingest records its own hash and leaves the
    // first registration's untouched.
    recover(&state, &sources[1]);
    let second_hash = state.metrics.lock().unwrap().history(1).unwrap()[0]
        .content_hash
        .clone();
    assert_eq!(
        shown(&state, &sources[1].repo_key),
        Some(("workdir".into(), Some(second_hash.clone())))
    );
    assert_eq!(
        shown(&state, &sources[0].repo_key).unwrap().1,
        Some(first_hash.clone())
    );
    // Scope: both are random `local/` keys over identical bytes, and no
    // equality is claimed across registrations (or installations).
    assert!(
        sources
            .iter()
            .all(|source| source.repo_key.starts_with("local/"))
    );
    assert_ne!(first_hash, second_hash);

    // Re-ingesting the same tree under the same registration records the
    // identical hash.
    recover(&state, &sources[0]);
    let history = state.metrics.lock().unwrap().history(3).unwrap();
    assert_eq!(history[0].repo, sources[0].repo_key);
    assert_eq!(history[0].content_hash, first_hash);

    // The export manifest states the graph it was built from.
    let export = |state: &AppState| {
        let graph = state.graph.lock().unwrap();
        let decisions = state.decisions.lock().unwrap();
        crate::build_spec_bundle(
            &graph,
            &decisions,
            &crate::curated::CuratedReview::default(),
            spec::ExportMode::BestEffort,
        )
        .unwrap()
        .graph_basis
        .unwrap()
    };
    let basis = export(&state);
    let (nodes, edges) = state.graph.lock().unwrap().read_snapshot().unwrap();
    assert_eq!(basis.content_hash, spec::graph_content_hash(&nodes, &edges));
    let mut expected: Vec<_> = sources
        .iter()
        .map(|source| spec::SourceRevision {
            repo: source.repo_key.clone(),
            commit: "workdir".into(),
        })
        .collect();
    expected.sort();
    assert_eq!(basis.sources, expected);
    let preview = {
        let graph = state.graph.lock().unwrap();
        let decisions = state.decisions.lock().unwrap();
        crate::preview_spec_bundle(
            crate::build_spec_bundle(
                &graph,
                &decisions,
                &crate::curated::CuratedReview::default(),
                spec::ExportMode::VerifiedOnly,
            )
            .unwrap(),
        )
    };
    assert_eq!(
        serde_json::to_value(&preview).unwrap()["graph_basis"],
        serde_json::to_value(&basis).unwrap(),
        "the export payload carries the same basis in every mode"
    );

    // The bundle manifest states that basis, and read alone it is the file
    // the export holds (AC-0682).
    {
        let graph = state.graph.lock().unwrap();
        let decisions = state.decisions.lock().unwrap();
        let review = crate::curated::CuratedReview::default();
        let mode = spec::ExportMode::BestEffort;
        let bundle = crate::build_spec_bundle(&graph, &decisions, &review, mode).unwrap();
        let manifest = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "bundle-manifest")
            .unwrap();
        assert!(
            manifest
                .content
                .contains(&format!("- Graph content hash: {}\n", basis.content_hash))
        );
        assert!(
            manifest
                .content
                .contains(&format!("- Source revisions: {}\n", basis.sources.len()))
        );
        for source in &basis.sources {
            assert!(
                manifest
                    .content
                    .contains(&format!("| `{}` | `{}` |\n", source.repo, source.commit))
            );
        }
        let alone =
            crate::build_spec_artifact(&graph, &decisions, Some(&review), mode, "bundle-manifest")
                .unwrap();
        assert_eq!(&alone, manifest);
    }

    // Curation is not a graph fact: a recorded decision changes no hash.
    state
        .decisions
        .lock()
        .unwrap()
        .record_assertion(
            &agents::CuratableAssertion {
                subject_id: "edge:sym:a CALLS sym:b".into(),
                summary: "CALLS: a → b".into(),
                provenance: Provenance::new(
                    Tier::Semantic,
                    ConfidenceTier::InferredStrong,
                    vec![reference(&sources[0].repo_key, "app.ts", 10)],
                    "t2.semantic",
                    b"sym:a CALLS sym:b",
                )
                .unwrap(),
            },
            agents::AssertionDecision::Rejected,
            None,
        )
        .unwrap();
    assert_eq!(export(&state), basis);
    assert_eq!(
        shown(&state, &sources[0].repo_key).unwrap().1,
        Some(first_hash)
    );
}

// AC-0593 (#1765): determinism on a real repository, not only on fixtures.
// Ingests one checkout twice into fresh graphs at the production (Auto)
// ingest parallelism, and once serially, and compares the graph content
// hashes. The checkout is supplied by the caller so the test itself needs no
// network: CARTOGRAPH_DETERMINISM_ROOT is the tree, and the optional
// CARTOGRAPH_DETERMINISM_COMMIT is the pinned commit it is checked out at.
#[test]
#[ignore = "requires CARTOGRAPH_DETERMINISM_ROOT: a checkout of the pinned repository"]
fn pinned_repository_reingest_yields_identical_graph_hash() {
    let root = std::env::var("CARTOGRAPH_DETERMINISM_ROOT").expect("checkout path");
    let root = crate::paths::canonicalize(PathBuf::from(root)).unwrap();
    let commit =
        std::env::var("CARTOGRAPH_DETERMINISM_COMMIT").unwrap_or_else(|_| "workdir".into());
    // A fixed registration key: the comparison is within one registration.
    let repo = "determinism/pinned";
    let ingest = |workers: usize| {
        source_walk::parallel::with_workers(workers, || {
            let extraction =
                extract_tree(&root, repo, &commit, &[], &BTreeMap::new(), None, None, &[]).unwrap();
            let mut graph = SqliteGraphStore::open_in_memory().unwrap();
            load_into_graph(&mut graph, &extraction, repo, &root, &commit).unwrap();
            crate::stitch_name_unique(&mut graph, &mut crate::OperationFacts::default()).unwrap();
            let (nodes, edges) = graph.read_snapshot().unwrap();
            let recorded = metrics::compute(
                &nodes,
                &edges,
                &BTreeMap::new(),
                &BTreeSet::from([repo.to_owned()]),
            )
            .content_hash;
            PinnedIngest {
                graph_hash: spec::graph_content_hash(&nodes, &edges),
                recorded_hash: recorded,
                identities: deterministic_graph_hashes(&graph).unwrap(),
                facts: keyed_facts(&nodes, &edges),
                nodes: nodes.len(),
                edges: edges.len(),
            }
        })
    };
    let production = source_walk::parallel::Parallelism::Auto.workers();
    let first = ingest(production);
    println!(
        "{} @ {commit}: {} nodes, {} edges, {production} workers, graph hash {}",
        root.display(),
        first.nodes,
        first.edges,
        first.graph_hash
    );
    assert!(first.nodes > 0, "the pinned checkout produced no facts");
    if let Some(difference) = ingest_difference(&first, &ingest(production)) {
        panic!("repeat ingest changed the graph: {difference}");
    }
    if let Some(difference) = ingest_difference(&first, &ingest(1)) {
        panic!("worker count changed the graph: {difference}");
    }
}

// What one ingest of the pinned checkout yields, for comparison.
struct PinnedIngest {
    graph_hash: String,
    recorded_hash: String,
    identities: Vec<String>,
    facts: Vec<(String, String)>,
    nodes: usize,
    edges: usize,
}

// Every fact as (identity key, full serialized fact), in key order.
fn keyed_facts(nodes: &[core_graph::Node], edges: &[core_graph::Edge]) -> Vec<(String, String)> {
    let mut facts = nodes
        .iter()
        .map(|node| {
            (
                format!("node {}", node.id),
                serde_json::to_string(node).unwrap(),
            )
        })
        .chain(edges.iter().map(|edge| {
            (
                format!("edge {} -[{}]-> {}", edge.src, edge.label, edge.dst),
                serde_json::to_string(edge).unwrap(),
            )
        }))
        .collect::<Vec<_>>();
    facts.sort();
    facts
}

// The first fact, in key order, on which two ingests disagree: present in
// only one of them, or present in both with different content.
fn first_differing_fact(first: &[(String, String)], second: &[(String, String)]) -> Option<String> {
    let (mut left, mut right) = (first.iter().peekable(), second.iter().peekable());
    loop {
        match (left.peek(), right.peek()) {
            (None, None) => return None,
            (Some((key, _)), None) => return Some(format!("{key} only in the first ingest")),
            (None, Some((key, _))) => return Some(format!("{key} only in the second ingest")),
            (Some(a), Some(b)) => match a.0.cmp(&b.0) {
                std::cmp::Ordering::Less => {
                    return Some(format!("{} only in the first ingest", a.0));
                }
                std::cmp::Ordering::Greater => {
                    return Some(format!("{} only in the second ingest", b.0));
                }
                std::cmp::Ordering::Equal if a.1 != b.1 => {
                    return Some(format!("{} differs: first {} second {}", a.0, a.1, b.1));
                }
                std::cmp::Ordering::Equal => {
                    left.next();
                    right.next();
                }
            },
        }
    }
}

// Why two ingests differ, naming the first differing fact when there is one.
fn ingest_difference(first: &PinnedIngest, second: &PinnedIngest) -> Option<String> {
    if let Some(fact) = first_differing_fact(&first.facts, &second.facts) {
        return Some(format!("first differing fact: {fact}"));
    }
    if first.graph_hash != second.graph_hash {
        return Some(format!(
            "graph hash {} became {} over the same facts",
            first.graph_hash, second.graph_hash
        ));
    }
    if first.recorded_hash != second.recorded_hash {
        return Some(format!(
            "recorded hash {} became {} over the same facts",
            first.recorded_hash, second.recorded_hash
        ));
    }
    (first.identities != second.identities)
        .then(|| "the T0 identity list changed over the same facts".to_owned())
}

// AC-0770 (#1812): a failed real-repository determinism run names the first
// differing fact rather than only reporting that the graph changed.
#[test]
fn determinism_failure_names_the_first_differing_fact() {
    let fact = |key: &str, body: &str| (key.to_owned(), body.to_owned());
    let base = vec![fact("edge a -[CALLS]-> b", "1"), fact("node a", "2")];
    assert_eq!(first_differing_fact(&base, &base.clone()), None);
    let changed = vec![fact("edge a -[CALLS]-> b", "1"), fact("node a", "3")];
    assert_eq!(
        first_differing_fact(&base, &changed).as_deref(),
        Some("node a differs: first 2 second 3")
    );
    let missing = vec![fact("node a", "2")];
    assert_eq!(
        first_differing_fact(&base, &missing).as_deref(),
        Some("edge a -[CALLS]-> b only in the first ingest")
    );
    assert_eq!(
        first_differing_fact(&missing, &base).as_deref(),
        Some("edge a -[CALLS]-> b only in the second ingest")
    );
    let extra = vec![
        fact("edge a -[CALLS]-> b", "1"),
        fact("node a", "2"),
        fact("node z", "9"),
    ];
    assert_eq!(
        first_differing_fact(&base, &extra).as_deref(),
        Some("node z only in the second ingest")
    );

    let ingest = |facts: Vec<(String, String)>, hash: &str| PinnedIngest {
        graph_hash: hash.to_owned(),
        recorded_hash: hash.to_owned(),
        identities: vec![],
        facts,
        nodes: 1,
        edges: 1,
    };
    assert_eq!(
        ingest_difference(&ingest(base.clone(), "h"), &ingest(base.clone(), "h")),
        None
    );
    assert_eq!(
        ingest_difference(&ingest(base.clone(), "h"), &ingest(changed, "g")).as_deref(),
        Some("first differing fact: node a differs: first 2 second 3")
    );
}

#[test]
fn relinked_documentation_references_resolve_against_the_whole_graph() {
    // AC-0678, AC-0864 (#2003, #2004): the relink pass reads the graph packed
    // and reads documents only for the published repository, while a
    // reference still resolves against every fact of the system.
    let dir = tempfile::tempdir().unwrap();
    let app_data = directory(dir.path(), "private");
    let root = directory(dir.path(), "shop");
    std::fs::write(root.join("app.ts"), "export function handle() {}\n").unwrap();
    std::fs::write(root.join("README.md"), "# Shop\n\nSee `app.ts`.\n").unwrap();
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    let state = app_state(&app_data);
    crate::preflight_uncancelled(root.to_str().unwrap(), app.handle(), &state).unwrap();
    let source = state
        .sources
        .lock()
        .unwrap()
        .get_by_root(&root)
        .unwrap()
        .unwrap();
    recover(&state, &source);
    let first = state.graph.lock().unwrap().read_snapshot().unwrap();
    let file = format!("file:{}@app.ts", source.repo_key);
    let mentions: Vec<_> = first
        .1
        .iter()
        .filter(|edge| edge.label == "MENTIONS")
        .collect();
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].dst, file);
    assert!(first.0.iter().any(|node| node.label == "Document"));
    // The pass is stable: relinking an unchanged tree changes nothing.
    recover(&state, &source);
    assert_eq!(state.graph.lock().unwrap().read_snapshot().unwrap(), first);
}
