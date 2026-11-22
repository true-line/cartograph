use super::*;
use core_graph::{GraphPatch, GraphStore, coverage::CoverageRecord};
use serde_json::{Value, json};
use std::sync::Mutex;
use tauri::Manager;

const TEST_ARCHIVE_CAPACITY: usize = 1024 * 1024;

struct Fixture {
    _directory: tempfile::TempDir,
    state: AppState,
    scope: Vec<ScopedRepo>,
}
impl Fixture {
    fn new(nodes: usize) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private = crate::paths::canonicalize(&private).unwrap();
        let state = crate::registered_source_tests::app_state(&private);
        let mut scope = Vec::new();
        for index in 0..3 {
            let root = directory.path().join(format!("repo{index}"));
            std::fs::create_dir(&root).unwrap();
            let source = state.sources.lock().unwrap().register_local(&root).unwrap();
            scope.push(ScopedRepo {
                repo_key: source.repo_key,
                registered_source_id: Some(source.source_id),
                config_hash: crate::coverage_config_hash(&[], crate::Registries::builtin()),
                layer_hints: None,
            });
        }
        let fixture = Self {
            _directory: directory,
            state,
            scope,
        };
        fixture.publish(0, "job:1", nodes);
        fixture.publish(1, "job:1", 0);
        fixture
    }
    fn publish(&self, index: usize, job: &str, nodes: usize) {
        self.publish_padded(index, job, nodes, 0);
    }
    fn publish_padded(&self, index: usize, job: &str, nodes: usize, padding: usize) {
        let scoped = &self.scope[index];
        let record = CoverageRecord {
            repo_key: scoped.repo_key.clone(),
            registered_source_id: scoped.registered_source_id.clone(),
            ingest_id: job.into(),
            config_hash: scoped.config_hash.clone(),
            inventory_json: json!({"inventory":"coverage@1", "repo":scoped.repo_key,
                "commit":"workdir", "layer_hints":[], "producers":[{"state":"missing_input"}],
                "unsupported":[], "limited":true, "padding":"x".repeat(padding)})
            .to_string(),
        };
        let mut graph = self.state.graph.lock().unwrap();
        let expected = graph.read_snapshot().unwrap();
        let patch = GraphPatch {
            upsert_nodes: (0..nodes)
                .map(|index| Node {
                    id: format!("mod:{index}"),
                    label: "Module".into(),
                    props: json!({"name":format!("module{index}")}),
                })
                .collect(),
            ..GraphPatch::default()
        };
        assert!(
            graph
                .apply_patch_with_source_bindings_and_coverage_if_snapshot_matches(
                    &expected,
                    &patch,
                    &scoped.repo_key,
                    &[],
                    Some(&record),
                    core_graph::source::SourceOmissions::NONE,
                )
                .unwrap()
        );
    }
    fn prepare(&self, request: &StartInvestigationRequest) -> Result<FrozenContext, HostError> {
        FrozenContext::prepare_qualified_with_scope(
            &self.state,
            request,
            &[],
            TEST_ARCHIVE_CAPACITY,
            || Ok(self.scope.clone()),
        )
    }
}
fn request() -> StartInvestigationRequest {
    serde_json::from_value(
        json!({"schema_version":3,"request_nonce":"qualified-context",
        "specialist_id":"domain-analyst@6","question":"What limits this scope?",
        "scope":{"type":"all"},"provider_mode":"local","limit_profile":"investigation-v1"}),
    )
    .unwrap()
}
fn query() -> InvestigationQuery {
    InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        labels: vec![],
        max_facts: 3,
        max_bytes: 32768,
        cursor: None,
    }
}
#[derive(Default)]
struct Sink {
    usage: InvestigationUsage,
    publications: usize,
}
impl AcquisitionSink for Sink {
    fn usage(&self) -> &InvestigationUsage {
        &self.usage
    }
    fn reserve_validation(&mut self, _: u64) -> Result<(), HostError> {
        panic!("query metadata cannot read source")
    }
    fn publish_input(
        &mut self,
        ledger: &InvestigationInputLedger,
        usage: InvestigationUsage,
    ) -> Result<(), HostError> {
        ledger.validate()?;
        self.usage = usage;
        self.publications += 1;
        Ok(())
    }
}

// AC-0239: app preparation records the exact scoped proof before any discovery,
// including registered/recorded repos with no facts, with truthful compared checks.
#[test]
fn qualified_preparation_keeps_zero_fact_repos_and_exact_scoped_proof() {
    let fixture = Fixture::new(3);
    let mut requested = request();
    requested.scope = InvestigationScope::Neighborhood {
        anchor: "mod:0".into(),
        hops: 1,
    };
    let app = crate::test_app::TestApp::new(
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap(),
    );
    app.manage(fixture.state);
    let state = app.state::<AppState>();
    let prepared = FrozenContext::prepare_with_app(
        app.handle(),
        &state,
        &requested,
        &[],
        Some(TEST_ARCHIVE_CAPACITY),
    )
    .unwrap();
    let proof = prepared.ledger.coverage.as_ref().unwrap();
    proof.validate().unwrap();
    assert_eq!(prepared.ledger.schema_version, 2);
    assert_eq!(proof.snapshot_id, prepared.ledger.scope_snapshot_id);
    assert_ne!(prepared.ledger.graph_snapshot_id, proof.snapshot_id);
    assert!(prepared.query_pages.is_empty());
    assert!(prepared.inventory_details.is_empty());
    assert_eq!(proof.qualification.repos.len(), 3);
    for (index, scoped) in fixture.scope.iter().enumerate() {
        let repo = proof
            .qualification
            .repos
            .iter()
            .find(|repo| repo.repo == scoped.repo_key)
            .unwrap();
        assert_eq!(
            repo.status,
            if index < 2 {
                CoverageStatus::Recorded
            } else {
                CoverageStatus::Unrecorded
            }
        );
        if index < 2 {
            assert_eq!(repo.compared, vec!["registration", "plugins"]);
        }
    }
    assert_eq!(prepared.coverage_archive.as_ref().unwrap().proof(), proof);
}

// AC-0239: identical graph bytes with a replacement inventory do not substitute
// the running task's preparation-time basis; a new task receives a new identity.
#[test]
fn qualified_query_and_archive_stay_frozen_after_inventory_replacement() {
    let fixture = Fixture::new(3);
    let mut prepared = fixture.prepare(&request()).unwrap();
    let original = prepared.ledger.coverage.clone().unwrap();
    fixture.publish(0, "job:2", 3);
    let next = fixture.prepare(&request()).unwrap();
    assert_eq!(
        next.ledger.graph_snapshot_id,
        prepared.ledger.graph_snapshot_id
    );
    assert_ne!(
        next.ledger.coverage.as_ref().unwrap().context_id,
        original.context_id
    );
    let mut sink = Sink::default();
    prepared.query(&fixture.state, query(), &mut sink).unwrap();
    let raw: Value = serde_json::from_str(&prepared.query_pages[0]).unwrap();
    assert_eq!(raw["context"]["context_id"], original.context_id);
    assert_eq!(
        raw["context"]["coverage"],
        serde_json::to_value(&original.qualification).unwrap()
    );
    assert!(raw["context"]["response"]["facts"].is_array());
    assert_eq!(prepared.ledger.coverage, Some(original.clone()));
    assert_eq!(
        prepared.coverage_archive.as_ref().unwrap().proof(),
        &original
    );
    let mut requested = request();
    requested.expected_context_id = Some(original.context_id);
    assert!(matches!(
        fixture.prepare(&requested),
        Err(HostError::InputChanged)
    ));
    // Inventory-only changes preserve the old task, but fact changes retain the
    // existing source-acquisition graph guard and cannot publish a new page.
    use core_graph::GraphStore;
    fixture
        .state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "mod:new".into(),
            label: "Module".into(),
            props: json!({}),
        })
        .unwrap();
    let before = prepared.ledger.clone();
    assert_eq!(
        prepared.query(&fixture.state, query(), &mut sink),
        Err(HostError::InputChanged)
    );
    assert_eq!(prepared.ledger, before);
}

// AC-0239: qualification and host evidence inventory both consume page bytes.
// Retiring old owners must preserve the exact prefix, options and cursor.
// Shrinking drops only the fact suffix and preserves exact continuation/basis.
#[test]
fn qualified_host_page_budget_preserves_qualification_and_continuation() {
    let fixture = Fixture::new(3);
    let mut prepared = fixture.prepare(&request()).unwrap();
    let qualified = prepared.qualified.as_ref().unwrap();
    let page = qualified
        .query(QualifiedRequest {
            request: QueryRequest {
                max_facts: 3,
                max_bytes: 32768,
                ..Default::default()
            },
            context_id: None,
        })
        .unwrap();
    let mut requested = query();
    requested.max_bytes = serde_json::to_vec(&page).unwrap().len() + 32;
    let initial = qualified
        .query(QualifiedRequest {
            request: QueryRequest {
                max_facts: 3,
                max_bytes: requested.max_bytes,
                ..Default::default()
            },
            context_id: None,
        })
        .unwrap();
    assert_eq!(initial.response.facts.len(), 3);
    let proof = prepared.ledger.coverage.clone().unwrap();
    let mut sink = Sink::default();
    prepared
        .query(&fixture.state, requested.clone(), &mut sink)
        .unwrap();
    let raw = &prepared.query_pages[0];
    assert!(raw.len() <= requested.max_bytes);
    let page: Value = serde_json::from_str(raw).unwrap();
    let returned = page["context"]["response"]["facts"]
        .as_array()
        .unwrap()
        .len();
    assert!(returned > 0 && returned < 3);
    assert_eq!(
        page["context"]["response"]["facts"],
        serde_json::to_value(&initial.response.facts[..returned]).unwrap()
    );
    let options = page["evidence_options"].as_array().unwrap();
    assert_eq!(options.len(), returned);
    for (option, fact) in options.iter().zip(&initial.response.facts) {
        let key = crate::task_evidence::task_fact(&graph_reference(&fact.reference));
        assert_eq!(option["fact"], serde_json::to_value(key).unwrap());
    }
    assert_eq!(
        page["context"]["coverage"],
        serde_json::to_value(&proof.qualification).unwrap()
    );
    let next = page["context"]["response"]["next_cursor"].clone();
    assert_eq!(next["offset"], returned);
    requested.cursor = Some(serde_json::from_value(next).unwrap());
    prepared
        .query(&fixture.state, requested, &mut sink)
        .unwrap();
    assert_eq!(prepared.ledger.coverage, Some(proof));
    assert_eq!(sink.publications, 2);
    let mut tiny = query();
    tiny.max_bytes = 64;
    let before = prepared.ledger.clone();
    assert_eq!(
        prepared.query(&fixture.state, tiny, &mut sink),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(before, prepared.ledger);
    assert_eq!(sink.publications, 2);
}

// AC-0239: scope basis must hold across the single graph+coverage copy. Drift
// fails closed and its inventory cannot enter the archive as Recorded.
#[test]
fn qualified_preparation_brackets_registration_and_plugin_basis() {
    let fixture = Fixture::new(0);
    for registration in [false, true] {
        let mut reads = 0;
        let prepared = FrozenContext::prepare_qualified_with_scope(
            &fixture.state,
            &request(),
            &[],
            TEST_ARCHIVE_CAPACITY,
            || {
                reads += 1;
                let mut scope = fixture.scope.clone();
                if reads == 1 {
                    if registration {
                        scope[0].registered_source_id = Some("src:other".into());
                    } else {
                        scope[0].config_hash = "plugins:other".into();
                    }
                }
                Ok(scope)
            },
        )
        .unwrap();
        assert_eq!(reads, 2);
        let proof = prepared.ledger.coverage.as_ref().unwrap();
        let repo = proof
            .qualification
            .repos
            .iter()
            .find(|repo| repo.repo == fixture.scope[0].repo_key)
            .unwrap();
        assert_eq!(repo.status, CoverageStatus::Stale);
        let read = InvestigationCoverageRead {
            context_id: proof.context_id.clone(),
            repo: repo.repo.clone(),
            inventory_hash: repo.inventory_hash.clone().unwrap(),
            max_bytes: 32768,
        };
        assert!(
            prepared
                .coverage_archive
                .as_ref()
                .unwrap()
                .read(&read)
                .is_err()
        );
    }
}

// AC-0239: bounded preparation is mandatory. A deep stored graph is refused,
// while legacy requests retain graph-only envelopes and no synthetic coverage.
#[test]
fn qualified_preparation_is_bounded_and_legacy_wire_is_unchanged() {
    use core_graph::GraphStore;
    let fixture = Fixture::new(1);
    let mut legacy = request();
    legacy.specialist_id = SpecialistId::DomainAnalystV4;
    legacy.schema_version = 1;
    let mut prepared = FrozenContext::prepare(&fixture.state, &legacy, &[]).unwrap();
    assert!(prepared.ledger.coverage.is_none());
    assert!(prepared.coverage_archive.is_none());
    let mut sink = Sink::default();
    prepared.query(&fixture.state, query(), &mut sink).unwrap();
    let raw: Value = serde_json::from_str(&prepared.query_pages[0]).unwrap();
    assert!(raw["context"]["facts"].is_array());
    assert!(raw["context"].get("coverage").is_none());
    assert_eq!(prepared.ledger.schema_version, 1);
    assert!(matches!(
        FrozenContext::prepare(&fixture.state, &request(), &[]),
        Err(HostError::InvalidInput)
    ));
    let mut nested = json!(true);
    for _ in 0..40 {
        nested = json!({"nested":nested});
    }
    fixture
        .state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "deep".into(),
            label: "Module".into(),
            props: nested,
        })
        .unwrap();
    assert!(matches!(
        fixture.prepare(&request()),
        Err(HostError::LimitExceeded)
    ));
}

fn inventory_request(context: &FrozenContext, repo: &str) -> InvestigationCoverageRead {
    let proof = context.ledger.coverage.as_ref().unwrap();
    let repo = proof
        .qualification
        .repos
        .iter()
        .find(|entry| entry.repo == repo)
        .unwrap();
    InvestigationCoverageRead {
        context_id: proof.context_id.clone(),
        repo: repo.repo.clone(),
        inventory_hash: repo.inventory_hash.clone().unwrap(),
        max_bytes: MAX_QUERY_BYTES,
    }
}

// AC-0239: detail acquisition is exact retained coverage, not source acquisition;
// each successful response is durably manifested before entering the live input.
#[test]
fn inventory_detail_is_exact_bounded_and_does_not_create_source_citations() {
    let fixture = Fixture::new(0);
    let mut prepared = fixture.prepare(&request()).unwrap();
    let requested = inventory_request(&prepared, &fixture.scope[0].repo_key);
    let original = prepared
        .coverage_archive
        .as_ref()
        .unwrap()
        .read(&requested)
        .unwrap();
    fixture.publish_padded(0, "job:replacement", 0, 256);
    let mut sink = Sink::default();
    prepared
        .read_inventory(requested.clone(), &mut sink)
        .unwrap();
    let raw = &prepared.inventory_details[0];
    let detail: InvestigationCoverageDetail = serde_json::from_str(raw).unwrap();
    assert_eq!(detail, original);
    assert_eq!(
        prepared.ledger.coverage_reads[0].response_hash,
        content_hash(raw.as_bytes())
    );
    let response_bytes = raw.len();
    assert_eq!(
        prepared.ledger.coverage_reads[0].response_bytes,
        response_bytes
    );
    assert!(prepared.ledger.citations.is_empty());
    assert!(prepared.ledger.selected_facts.is_empty());
    assert_eq!(sink.usage.evidence_items, 0);
    assert_eq!(sink.usage.evidence_requests, 0);
    let before = prepared.ledger.clone();
    let mut changed = requested.clone();
    changed.context_id.push('0');
    assert_eq!(
        prepared.read_inventory(changed, &mut sink),
        Err(HostError::InvalidInput)
    );
    let mut changed = requested.clone();
    changed.inventory_hash = "0".repeat(64);
    assert_eq!(
        prepared.read_inventory(changed, &mut sink),
        Err(HostError::InvalidInput)
    );
    let mut changed = requested;
    changed.max_bytes = response_bytes - 1;
    assert_eq!(
        prepared.read_inventory(changed, &mut sink),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(prepared.ledger, before);
    assert_eq!(prepared.inventory_details.len(), 1);
    assert_eq!(sink.publications, 1);
}

// AC-0239: publication failure cannot silently add detail to a later prompt,
// and repeated reads consume cumulative raw bytes even when content is identical.
#[test]
fn inventory_detail_publication_failure_and_cumulative_budget_are_atomic() {
    struct FailingSink(InvestigationUsage);
    impl AcquisitionSink for FailingSink {
        fn usage(&self) -> &InvestigationUsage {
            &self.0
        }
        fn reserve_validation(&mut self, _: u64) -> Result<(), HostError> {
            panic!("no source")
        }
        fn publish_input(
            &mut self,
            _: &InvestigationInputLedger,
            _: InvestigationUsage,
        ) -> Result<(), HostError> {
            Err(HostError::Operational)
        }
    }
    let fixture = Fixture::new(0);
    fixture.publish_padded(0, "job:large-detail", 0, 26000);
    let mut prepared = fixture.prepare(&request()).unwrap();
    let requested = inventory_request(&prepared, &fixture.scope[0].repo_key);
    let before = prepared.ledger.clone();
    assert_eq!(
        prepared.read_inventory(
            requested.clone(),
            &mut FailingSink(InvestigationUsage::default())
        ),
        Err(HostError::Operational)
    );
    assert_eq!(prepared.ledger, before);
    assert!(prepared.inventory_details.is_empty());
    let mut sink = Sink::default();
    for _ in 0..3 {
        prepared
            .read_inventory(requested.clone(), &mut sink)
            .unwrap();
    }
    assert!(
        InvestigationQueryBudget::from_ledger(&prepared.ledger)
            .unwrap()
            .remaining_bytes
            < 26000
    );
    let before = prepared.ledger.clone();
    assert_eq!(
        prepared.read_inventory(requested, &mut sink),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(prepared.ledger, before);
    assert_eq!(prepared.inventory_details.len(), 3);
    assert_eq!(sink.publications, 3);
    assert!(prepared.ledger.citations.is_empty());
}

// AC-0239: SQL measures current raw inventory fields before allocating bodies.
// An invalid body hash would yield Operational only after reading owned strings;
// the smaller capacity instead wins first with the explicit limit classification.
#[test]
fn qualified_preparation_capacity_rejects_large_raw_inventories_before_copy() {
    let fixture = Fixture::new(0);
    fixture.publish_padded(0, "job:large-a", 0, 700_000);
    fixture.publish_padded(1, "job:large-b", 0, 700_000);
    let graph_path = fixture._directory.path().join("private/graph.db");
    let connection = rusqlite::Connection::open(graph_path).unwrap();
    connection
        .execute(
            "UPDATE coverage_inventories SET inventory_hash=?1",
            ["0".repeat(64)],
        )
        .unwrap();
    let mut scope_reads = 0;
    let prepared = FrozenContext::prepare_qualified_with_scope(
        &fixture.state,
        &request(),
        &[],
        TEST_ARCHIVE_CAPACITY,
        || {
            scope_reads += 1;
            Ok(fixture.scope.clone())
        },
    );
    assert!(matches!(prepared, Err(HostError::LimitExceeded)));
    assert_eq!(scope_reads, 1);
    let mut tiny_reads = 0;
    assert!(matches!(
        FrozenContext::prepare_qualified_with_scope(
            &fixture.state,
            &request(),
            &[],
            MAX_MANIFEST_BYTES + ARCHIVE_ENVELOPE_BYTES,
            || {
                tiny_reads += 1;
                Ok(fixture.scope.clone())
            }
        ),
        Err(HostError::LimitExceeded)
    ));
    assert_eq!(tiny_reads, 0);
}

// AC-0239: raw admission is not a promise about encoded escaping. The separate
// count-only archive bound refuses expansion before the larger format buffer.
#[test]
fn qualified_preparation_checks_exact_archive_encoding_after_raw_admission() {
    let fixture = Fixture::new(0);
    let scoped = &fixture.scope[0];
    let inventory = json!({"inventory":context_hub::coverage::INVENTORY_VERSION,"repo":scoped.repo_key,"commit":"workdir","layer_hints":[],
        "producers":[],"unsupported":[],"limited":false,"padding":"\\".repeat(350_000)})
    .to_string();
    let connection =
        rusqlite::Connection::open(fixture._directory.path().join("private/graph.db")).unwrap();
    connection
        .execute(
            "UPDATE coverage_inventories SET inventory_json=?1,inventory_hash=?2 WHERE repo_key=?3",
            rusqlite::params![
                inventory,
                content_hash(inventory.as_bytes()),
                scoped.repo_key
            ],
        )
        .unwrap();
    let raw_bytes:i64=connection.query_row("SELECT SUM(length(CAST(repo_key AS BLOB))+COALESCE(length(CAST(registered_source_id AS BLOB)),0)+length(CAST(ingest_id AS BLOB))+length(CAST(config_hash AS BLOB))+length(CAST(inventory_hash AS BLOB))+length(CAST(inventory_json AS BLOB))) FROM coverage_inventories",[],|row|row.get(0)).unwrap();
    assert!(
        usize::try_from(raw_bytes).unwrap()
            < coverage_preparation_limits(TEST_ARCHIVE_CAPACITY)
                .unwrap()
                .max_bytes
    );
    assert!(matches!(
        fixture.prepare(&request()),
        Err(HostError::LimitExceeded)
    ));
    // Ordinary ASCII inventories substantially larger than 100KiB still fit.
    fixture.publish_padded(0, "job:plain", 0, 600_000);
    let prepared = fixture.prepare(&request()).unwrap();
    let archive = prepared.coverage_archive.unwrap();
    let encoded = serde_json::to_vec(&archive).unwrap();
    assert!(encoded.len() > 512 * 1024 && encoded.len() <= TEST_ARCHIVE_CAPACITY);
    let inventories: Vec<core_graph::coverage::StoredCoverage> =
        serde_json::from_value(serde_json::to_value(&archive).unwrap()["inventories"].clone())
            .unwrap();
    assert!(admit_archive_size(archive.proof(), &inventories, encoded.len()).is_ok());
    assert_eq!(
        admit_archive_size(archive.proof(), &inventories, encoded.len() - 1),
        Err(HostError::LimitExceeded)
    );
}

// AC-0239: reproduce the previous two-build preparation beside the new shared
// path. Count actual constructor inputs, not guessed heap/allocator/RSS bytes.
#[test]
fn qualified_preparation_shares_one_scoped_projection_and_halves_construction_work() {
    let fixture = Fixture::new(128);
    {
        use core_graph::GraphStore;
        let mut graph = fixture.state.graph.lock().unwrap();
        for index in 0..127 {
            graph
                .put_edge(&Edge {
                    src: format!("mod:{index}"),
                    dst: format!("mod:{}", index + 1),
                    label: "CALLS".into(),
                    props: json!({"order": index}),
                })
                .unwrap();
        }
    }
    for scope in [
        InvestigationScope::All,
        InvestigationScope::Neighborhood {
            anchor: "mod:0".into(),
            hops: 1,
        },
    ] {
        let mut requested = request();
        requested.scope = scope.clone();
        let (graph, records) = fixture
            .state
            .graph
            .lock()
            .unwrap()
            .read_snapshot_with_coverage_bounded(
                SnapshotReadLimits::default(),
                coverage_preparation_limits(TEST_ARCHIVE_CAPACITY).unwrap(),
            )
            .unwrap();
        let qualification = CoverageQualification::new(&fixture.scope, &records).unwrap();
        take_projection_work();
        let mut previous = FrozenContext::from_graph(graph, &requested, &[]).unwrap();
        let complete = projection(previous.graph.0.clone(), previous.graph.1.clone()).unwrap();
        let scoped =
            FrozenContext::scoped_snapshot(&previous.graph, &requested.scope, None, complete)
                .unwrap();
        let previous_qualified = scoped.with_coverage(qualification).unwrap();
        let before = take_projection_work();
        assert!(!previous_qualified.shares_snapshot(previous.snapshot.held().unwrap()));
        assert_eq!(Arc::strong_count(previous.snapshot.held().unwrap()), 1);
        previous.ledger.schema_version = 2;
        previous.ledger.coverage = Some(previous_qualified.durable_proof().unwrap());
        previous.ledger.validate().unwrap();

        let prepared = fixture.prepare(&requested).unwrap();
        let after = take_projection_work();
        let qualified = prepared.qualified.as_ref().unwrap();
        assert!(qualified.shares_snapshot(prepared.snapshot.held().unwrap()));
        assert_eq!(Arc::strong_count(prepared.snapshot.held().unwrap()), 2);
        // `All` halves the full-read work; a neighborhood (AC-1301) builds only
        // its scope's projection, its complete identity streamed instead.
        let expected = if matches!(scope, InvestigationScope::All) {
            assert_eq!(before.constructions, 2 * after.constructions);
            assert_eq!(before.nodes, 2 * after.nodes);
            assert_eq!(before.edges, 2 * after.edges);
            ProjectionWork {
                constructions: 1,
                nodes: 128,
                edges: 127,
            }
        } else {
            ProjectionWork {
                constructions: 1,
                nodes: 2,
                edges: 1,
            }
        };
        assert_eq!(after, expected);
        assert_eq!(
            serde_json::to_vec(&previous.ledger).unwrap(),
            serde_json::to_vec(&prepared.ledger).unwrap()
        );
        let page = QualifiedRequest {
            request: QueryRequest {
                max_facts: 7,
                max_bytes: 4096,
                ..QueryRequest::default()
            },
            context_id: Some(qualified.context_id().to_owned()),
        };
        assert_eq!(
            serde_json::to_vec(&previous_qualified.query(page.clone()).unwrap()).unwrap(),
            serde_json::to_vec(&qualified.query(page).unwrap()).unwrap()
        );
        eprintln!(
            "{scope:?}: projection constructor work {before:?} -> {after:?}; distinct retained scoped projections 2 -> 1; immutable handles 2 (not allocator-byte measurement)"
        );
    }
}

fn expected_nodes(graph: &(Vec<Node>, Vec<Edge>)) -> Vec<Node> {
    graph
        .0
        .iter()
        .filter(|n| n.id != "mod:2")
        .cloned()
        .collect()
}

// AC-0328: scope preparation selects references, not public response pages.
#[test]
fn qualified_neighborhood_preparation_keeps_large_facts_without_response_pages() {
    use core_graph::GraphStore;
    for padding in [0, context_hub::MAX_RESPONSE_BYTES + 1] {
        let fixture = Fixture::new(3);
        {
            let mut graph = fixture.state.graph.lock().unwrap();
            graph
                .put_node(&Node {
                    id: "mod:0".into(),
                    label: "Module".into(),
                    props: json!({"text": "x".repeat(padding)}),
                })
                .unwrap();
            graph
                .put_edge(&Edge {
                    src: "mod:0".into(),
                    dst: "mod:1".into(),
                    label: "CALLS".into(),
                    props: json!({}),
                })
                .unwrap();
        }
        let graph = fixture
            .state
            .graph
            .lock()
            .unwrap()
            .read_snapshot_bounded(SnapshotReadLimits::default())
            .unwrap();
        let complete = projection(graph.0.clone(), graph.1.clone()).unwrap();
        if padding > 0 {
            assert!(matches!(
                complete.query(QueryRequest::default()),
                Err(context_hub::ContextError::ResponseBudgetExceeded { .. })
            ));
        }
        let expected = projection(
            graph
                .0
                .iter()
                .filter(|n| n.id != "mod:2")
                .cloned()
                .collect(),
            graph.1.clone(),
        )
        .unwrap();
        let mut requested = request();
        requested.scope = InvestigationScope::Neighborhood {
            anchor: "mod:0".into(),
            hops: 1,
        };
        let prepared = fixture.prepare(&requested).unwrap();
        assert_eq!(prepared.snapshot.id(), expected.id());
        // AC-1301: only the neighborhood is retained.
        assert_eq!(prepared.graph, (expected_nodes(&graph), graph.1.clone()));
        assert!(
            prepared
                .qualified
                .as_ref()
                .unwrap()
                .shares_snapshot(prepared.snapshot.held().unwrap())
        );
        requested.scope = InvestigationScope::Neighborhood {
            anchor: "missing".into(),
            hops: 1,
        };
        assert!(fixture.prepare(&requested).is_err());
    }
}

// AC-0327: preparation accounting covers actual qualified owners and fails closed.
#[cfg(unix)]
#[test]
fn qualified_preparation_workspace_covers_fixture_and_rejects_overflow() {
    use context_hub::logical::count;
    let fixture = Fixture::new(3);
    let requested = request();
    let frame = serde_json::to_vec(&requested).unwrap().len();
    let scope = crate::mcp_reads::scope::tests::limits();
    let (retained, peak) =
        preparation_workspace::workspace_bytes(scope, TEST_ARCHIVE_CAPACITY, frame).unwrap();
    let prepared = fixture.prepare(&requested).unwrap();
    let observed = count(&prepared.graph).unwrap()
        + count(&prepared.ledger).unwrap()
        + count(&prepared.coverage_archive).unwrap();
    assert!(retained >= observed);
    assert!(peak >= retained);
    assert!(prepared.coverage_archive.is_some());
    assert!(prepared.query_pages.is_empty() && prepared.evidence.is_empty());
    let larger_request =
        preparation_workspace::workspace_bytes(scope, TEST_ARCHIVE_CAPACITY, frame + 1024).unwrap();
    assert!(larger_request.1 > peak);
    let larger_archive =
        preparation_workspace::workspace_bytes(scope, TEST_ARCHIVE_CAPACITY * 2, frame).unwrap();
    assert!(larger_archive.1 > peak);
    assert!(
        preparation_workspace::workspace_bytes(scope, TEST_ARCHIVE_CAPACITY, usize::MAX,).is_none()
    );
}
// AC-0327: exercise the draft host bound against actual qualified preparation.
#[test]
fn query_workspace_covers_real_prepared_ledger_and_raw_response() {
    use super::super::query_workspace::{self, QueryInputs};
    use core_graph::GraphStore;
    use core_prov::{ConfidenceTier, EvidenceRef, ProducingMethod, Provenance, Tier};
    for legacy in [false, true] {
        let fixture = Fixture::new(20);
        let evidence_path = "quoted\"/λ/source.ts".repeat(8);
        let provenance = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            (0..4)
                .map(|index| EvidenceRef {
                    repo: fixture.scope[0].repo_key.clone(),
                    path: evidence_path.clone(),
                    byte_start: index,
                    byte_end: index + 1,
                    commit_sha: "workdir".into(),
                })
                .collect(),
            "fixture",
            b"fixture",
        )
        .unwrap()
        .with_method(ProducingMethod::Rule {
            rule: "query-fixture".into(),
            version: "1".into(),
        });
        {
            let mut graph = fixture.state.graph.lock().unwrap();
            let mut node = graph.get_node("mod:0").unwrap().unwrap();
            node.props["prov"] = serde_json::to_value(provenance).unwrap();
            graph.put_node(&node).unwrap();
        }
        let frozen = if legacy {
            let mut start = request();
            start.specialist_id = SpecialistId::DomainAnalystV4;
            start.schema_version = 1;
            FrozenContext::prepare(&fixture.state, &start, &[]).unwrap()
        } else {
            fixture.prepare(&request()).unwrap()
        };
        let request = query();
        let input = QueryInputs {
            graph: SnapshotReadLimits::default(),
            coverage: CoverageReadLimits::default(),
            scoped_repos: fixture.scope.len(),
            scope_identity_bytes: context_hub::logical::count(&fixture.scope).unwrap(),
            ledger_wire: context_hub::logical::count(&frozen.ledger).unwrap(),
            request_wire: context_hub::logical::count(&request).unwrap() + 128,
            app_path_bytes: crate::sources::MAX_PATH_BYTES,
        };
        let bound = query_workspace::workspace(&frozen.policy, input).unwrap();
        let prepared = frozen.prepare_query(&fixture.state, request).unwrap();
        let actual = context_hub::logical::count(&prepared.base_ledger).unwrap()
            + context_hub::logical::count(&prepared.ledger).unwrap()
            + prepared.raw.len();
        assert!(bound.retained >= actual);
        assert!(bound.peak >= bound.retained);
        let response: Value = serde_json::from_str(&prepared.raw).unwrap();
        let ranges = response["evidence_options"][0]["ranges"]
            .as_array()
            .unwrap();
        assert_eq!(ranges.len(), 4);
        assert_eq!(ranges[0]["source"]["path"], evidence_path);
        // The core's one-fact page fits; its added evidence inventory pushes the
        // host response past the exact byte boundary, with no smaller page left.
        let mut single = query();
        single.max_facts = 1;
        let one = frozen
            .prepare_query(&fixture.state, single.clone())
            .unwrap();
        single.max_bytes = one.raw.len() - 1;
        let core_request = QueryRequest {
            max_facts: 1,
            max_bytes: single.max_bytes,
            ..Default::default()
        };
        match &frozen.qualified {
            Some(qualified) => {
                qualified
                    .query(QualifiedRequest {
                        request: core_request,
                        context_id: Some(qualified.context_id().into()),
                    })
                    .unwrap();
            }
            None => match &frozen.snapshot {
                ScopeFacts::Held(snapshot) => {
                    snapshot.query(core_request).unwrap();
                }
                ScopeFacts::Paged { id } => assert!(
                    paged::query_at(
                        &fixture.state,
                        frozen.scoped_revision.as_ref().unwrap(),
                        frozen.graph_limits,
                        id,
                        core_request,
                    )
                    .is_ok()
                ),
            },
        }
        assert!(matches!(
            frozen.prepare_query(&fixture.state, single),
            Err(HostError::LimitExceeded)
        ));
        let before = frozen.ledger.clone();
        let mut tiny = query();
        tiny.max_bytes = 1;
        assert!(matches!(
            frozen.prepare_query(&fixture.state, tiny),
            Err(HostError::LimitExceeded)
        ));
        assert_eq!(frozen.ledger, before);
    }
}

/// A fixture graph with two files, one symbol in each and a call between
/// them, and an overlay current for it whose one area holds the first file.
fn area_fixture() -> (Fixture, String) {
    use core_graph::overlay::{BaseRef, OverlayFact, OverlayItem, OverlayRecipe, OverlayStore};
    let mut fixture = Fixture::new(3);
    let area = "subsystem:cluster:alias".to_owned();
    let base = {
        let mut graph = fixture.state.graph.lock().unwrap();
        for (id, label) in [
            ("file:alias", "File"),
            ("sym:alias", "Symbol"),
            ("file:mailbox", "File"),
            ("sym:mailbox", "Symbol"),
        ] {
            graph
                .put_node(&Node {
                    id: id.into(),
                    label: label.into(),
                    props: json!({}),
                })
                .unwrap();
        }
        for (src, label, dst) in [
            ("sym:alias", "DEFINED_IN", "file:alias"),
            ("sym:mailbox", "DEFINED_IN", "file:mailbox"),
            ("sym:alias", "CALLS", "sym:mailbox"),
        ] {
            graph
                .put_edge(&Edge {
                    src: src.into(),
                    dst: dst.into(),
                    label: label.into(),
                    props: json!({}),
                })
                .unwrap();
        }
        graph
            .read_snapshot_with_revision_bounded(SnapshotReadLimits::investigation())
            .unwrap()
    };
    let mut overlay = OverlayStore::open_in_memory().unwrap();
    overlay
        .bind_instance(&base.revision.graph_instance)
        .unwrap();
    overlay
        .publish(
            &base,
            OverlayRecipe {
                recipe: "host-fixture@1".into(),
                embedding_namespace: None,
                provider_ran: false,
            },
            vec![OverlayFact {
                item: OverlayItem::Node(Node {
                    id: area.clone(),
                    label: "Subsystem".into(),
                    props: json!({"name":"Alias","kind":"cluster","files":1,
                        "prov": core_prov::Provenance::new(
                            core_prov::Tier::Semantic,
                            core_prov::ConfidenceTier::InferredWeak,
                            vec![core_prov::EvidenceRef {
                                repo: "local/fixture".into(),
                                path: "app/alias.py".into(),
                                byte_start: 0,
                                byte_end: 1,
                                commit_sha: "fixed".into(),
                            }],
                            "t2.knowledge.subsystems",
                            b"alias",
                        )
                        .unwrap()}),
                }),
                cites: [BaseRef::Node("file:alias".into())].into(),
            }],
        )
        .unwrap();
    fixture.state.knowledge_overlay = Some(Arc::new(Mutex::new(overlay)));
    (fixture, area)
}

// AC-0930 (#2044): a task scoped to an area freezes the area's files, what they define
// and the edges between them, and nothing else of the graph.
#[test]
fn an_area_scope_freezes_the_areas_files_and_what_they_define() {
    let (fixture, area) = area_fixture();
    let mut requested = request();
    requested.scope = InvestigationScope::Area { id: area };
    let prepared = fixture.prepare(&requested).unwrap();
    let references = prepared
        .snapshot
        .held()
        .unwrap()
        .references_in_scope(QueryScope::All)
        .unwrap();
    let node = |id: &str| FactReference::Node { id: id.into() };
    assert_eq!(
        references,
        [
            node("file:alias"),
            node("sym:alias"),
            FactReference::Edge {
                source: "sym:alias".into(),
                label: "DEFINED_IN".into(),
                destination: "file:alias".into(),
            },
        ]
        .into_iter()
        .collect()
    );
    assert_ne!(
        prepared.ledger.graph_snapshot_id,
        prepared.ledger.scope_snapshot_id
    );
}

// AC-0930 (#2044): an area that is unknown, has no overlay, or was derived from another
// graph revision refuses the task; it never widens to the whole graph.
#[test]
fn an_unknown_or_stale_area_refuses_the_task() {
    let (mut fixture, area) = area_fixture();
    let mut requested = request();
    requested.scope = InvestigationScope::Area {
        id: "subsystem:cluster:missing".into(),
    };
    assert!(matches!(
        fixture.prepare(&requested),
        Err(HostError::InvalidInput)
    ));
    requested.scope = InvestigationScope::Area { id: area };
    fixture
        .state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "file:later".into(),
            label: "File".into(),
            props: json!({}),
        })
        .unwrap();
    assert!(matches!(
        fixture.prepare(&requested),
        Err(HostError::InputChanged)
    ));
    // An overlay store nothing was published to holds no such area.
    fixture
        .state
        .knowledge_overlay
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .invalidate()
        .unwrap();
    assert!(matches!(
        fixture.prepare(&requested),
        Err(HostError::InvalidInput)
    ));
    fixture.state.knowledge_overlay = None;
    assert!(matches!(
        fixture.prepare(&requested),
        Err(HostError::InvalidInput)
    ));
}

// AC-0930 (#2044): a role prepared without coverage qualification resolves an area the
// same way.
#[test]
fn an_area_scope_is_resolved_for_a_role_without_coverage_qualification() {
    let (fixture, area) = area_fixture();
    let mut legacy = request();
    legacy.schema_version = 1;
    legacy.specialist_id = SpecialistId::DomainAnalystV1;
    legacy.scope = InvestigationScope::Area { id: area };
    let prepared = FrozenContext::prepare(&fixture.state, &legacy, &[]).unwrap();
    assert_eq!(
        prepared
            .snapshot
            .held()
            .unwrap()
            .references_in_scope(QueryScope::All)
            .unwrap()
            .len(),
        3
    );
    legacy.scope = InvestigationScope::Area {
        id: "subsystem:cluster:missing".into(),
    };
    assert!(matches!(
        FrozenContext::prepare(&fixture.state, &legacy, &[]),
        Err(HostError::InvalidInput)
    ));
}

// AC-0930 (#2044): an area scopes a task; a query inside a task cannot name one.
#[test]
fn a_query_cannot_name_an_area() {
    let mut query = query();
    query.scope = InvestigationScope::Area {
        id: "subsystem:cluster:alias".into(),
    };
    assert!(query.validate().is_err());
    assert!(
        InvestigationScope::Area {
            id: "subsystem:cluster:alias".into()
        }
        .validate()
        .is_ok()
    );
    assert_eq!(
        serde_json::to_value(InvestigationScope::Area { id: "a".into() }).unwrap(),
        json!({"type":"area","id":"a"})
    );
}

/// The complete-graph preparation of `requested` over the fixture's whole
/// graph, as the context the scope-aware path must equal (#1602).
fn complete_graph_context(
    fixture: &Fixture,
    requested: &StartInvestigationRequest,
) -> FrozenContext {
    let base = fixture
        .state
        .graph
        .lock()
        .unwrap()
        .read_snapshot_with_revision_bounded(SnapshotReadLimits::investigation())
        .unwrap();
    let area = super::super::area::files(&fixture.state, &base.revision, &requested.scope).unwrap();
    FrozenContext::from_graph_in(base.graph, requested, &[], area.as_ref()).unwrap()
}

// AC-1301 (#1602): a neighborhood or area task reads only its scope, and keeps exactly
// the complete-graph identity and scope identity the whole-graph read gave, for
// qualified and unqualified roles.
#[test]
fn scoped_preparation_keeps_both_identities_and_retains_only_the_scope() {
    let (fixture, area) = area_fixture();
    let scopes = [
        InvestigationScope::Area { id: area },
        InvestigationScope::Neighborhood {
            anchor: "sym:alias".into(),
            hops: 1,
        },
    ];
    for scope in scopes {
        let mut qualified = request();
        qualified.scope = scope.clone();
        let mut legacy = request();
        legacy.schema_version = 1;
        legacy.specialist_id = SpecialistId::DomainAnalystV1;
        legacy.scope = scope.clone();
        for (prepared, requested) in [
            (fixture.prepare(&qualified).unwrap(), &qualified),
            (
                FrozenContext::prepare(&fixture.state, &legacy, &[]).unwrap(),
                &legacy,
            ),
        ] {
            let complete = complete_graph_context(&fixture, requested);
            assert_eq!(
                prepared.ledger.graph_snapshot_id, complete.ledger.graph_snapshot_id,
                "{scope:?}"
            );
            assert_eq!(
                prepared.ledger.scope_snapshot_id, complete.ledger.scope_snapshot_id,
                "{scope:?}"
            );
            assert!(prepared.scoped_revision.is_some());
            // Only the scope is retained, never the rest of the graph.
            let retained = prepared.graph.0.len() + prepared.graph.1.len();
            assert_eq!(
                retained,
                prepared
                    .snapshot
                    .held()
                    .unwrap()
                    .references_in_scope(QueryScope::All)
                    .unwrap()
                    .len()
            );
            assert!(retained < complete.graph.0.len() + complete.graph.1.len());
            assert!(!prepared.graph.0.iter().any(|node| node.id == "mod:0"));
        }
    }
    // A qualified `All` task still freezes the complete graph.
    let all = fixture.prepare(&request()).unwrap();
    assert!(all.scoped_revision.is_none());
}

// AC-1301 (#1602): preparation cost tracks the scope, not the estate. Under retained
// limits the whole graph exceeds, a neighborhood task still prepares (the identity
// pass streams one row at a time under the ADR-0052 ceilings).
#[test]
fn a_scope_prepares_under_limits_the_whole_graph_exceeds() {
    let (fixture, _) = area_fixture();
    for index in 0..60 {
        fixture
            .state
            .graph
            .lock()
            .unwrap()
            .put_node(&Node {
                id: format!("pad:{index:03}"),
                label: "Module".into(),
                props: json!({"name": index}),
            })
            .unwrap();
    }
    let retained = SnapshotReadLimits {
        max_rows: 40,
        ..SnapshotReadLimits::default()
    };
    let mut legacy = request();
    legacy.schema_version = 1;
    legacy.specialist_id = SpecialistId::DomainAnalystV1;
    // A legacy `All` task pages instead (AC-1058, below).
    legacy.scope = InvestigationScope::Neighborhood {
        anchor: "sym:alias".into(),
        hops: 1,
    };
    let prepared = FrozenContext::prepare_bounded(&fixture.state, &legacy, &[], retained).unwrap();
    assert_eq!(
        prepared.ledger.graph_snapshot_id,
        complete_graph_context(&fixture, &legacy)
            .ledger
            .graph_snapshot_id
    );
    assert_eq!(prepared.graph.0.len(), 3);
}

// AC-1058 (#1602): a whole-system task prepares under limits the whole graph exceeds,
// holds no facts, and keeps the complete graph's identity.
#[test]
fn a_whole_system_task_prepares_without_holding_the_graph() {
    let (fixture, _) = area_fixture();
    for index in 0..60 {
        fixture
            .state
            .graph
            .lock()
            .unwrap()
            .put_node(&Node {
                id: format!("pad:{index:03}"),
                label: "Module".into(),
                props: json!({"name": index}),
            })
            .unwrap();
    }
    let retained = SnapshotReadLimits {
        max_rows: 40,
        ..SnapshotReadLimits::default()
    };
    let mut legacy = request();
    legacy.schema_version = 1;
    legacy.specialist_id = SpecialistId::DomainAnalystV1;
    let prepared = FrozenContext::prepare_bounded(&fixture.state, &legacy, &[], retained).unwrap();
    let complete = complete_graph_context(&fixture, &legacy);
    assert_eq!(
        prepared.ledger.graph_snapshot_id,
        complete.ledger.graph_snapshot_id
    );
    assert_eq!(
        prepared.ledger.scope_snapshot_id,
        complete.ledger.scope_snapshot_id
    );
    assert!(prepared.scoped_revision.is_some());
    assert!(prepared.snapshot.held().is_none());
    assert!(prepared.graph.0.is_empty() && prepared.graph.1.is_empty());
}

// AC-1058 (#1602): every page a whole-system task reads from the store, at its pinned
// revision, is the page the complete in-memory snapshot gives, cursor for cursor, for
// each scope, kind and label filter; refusals match too.
#[test]
fn whole_system_pages_are_the_complete_snapshot_pages() {
    let (fixture, _) = area_fixture();
    let mut legacy = request();
    legacy.schema_version = 1;
    legacy.specialist_id = SpecialistId::DomainAnalystV1;
    let prepared = FrozenContext::prepare(&fixture.state, &legacy, &[]).unwrap();
    let complete = complete_graph_context(&fixture, &legacy);
    let held = complete.snapshot.held().unwrap();
    let revision = prepared.scoped_revision.clone().unwrap();
    let id = prepared.snapshot.id().to_owned();
    assert_eq!(id, held.id());
    let selections = [
        (QueryScope::All, None, vec![]),
        (QueryScope::All, Some(FactKind::Node), vec![]),
        (QueryScope::All, Some(FactKind::Edge), vec![]),
        (
            QueryScope::All,
            None,
            vec!["Symbol".to_owned(), "CALLS".to_owned()],
        ),
        (
            QueryScope::All,
            Some(FactKind::Edge),
            vec!["DEFINED_IN".to_owned()],
        ),
        (QueryScope::All, None, vec!["Absent".to_owned()]),
        (
            QueryScope::Node {
                id: "sym:alias".into(),
            },
            None,
            vec![],
        ),
        (
            QueryScope::Node {
                id: "sym:alias".into(),
            },
            Some(FactKind::Edge),
            vec![],
        ),
        (
            QueryScope::Neighborhood {
                anchor: "sym:alias".into(),
                hops: 1,
            },
            None,
            vec![],
        ),
        (
            QueryScope::Neighborhood {
                anchor: "file:alias".into(),
                hops: 2,
            },
            Some(FactKind::Node),
            vec![],
        ),
    ];
    for (scope, kind, labels) in selections {
        for max_facts in [1, 2, 5] {
            let mut cursor = None;
            let mut pages = 0;
            loop {
                let request = QueryRequest {
                    scope: scope.clone(),
                    kind,
                    labels: labels.clone(),
                    max_facts,
                    max_bytes: 32 * 1024,
                    cursor: cursor.clone(),
                };
                let expected = held.query(request.clone()).unwrap();
                let (page, rows) = match paged::query_at(
                    &fixture.state,
                    &revision,
                    SnapshotReadLimits::investigation(),
                    &id,
                    request,
                ) {
                    Ok(read) => read,
                    Err(_) => panic!("{scope:?} {kind:?} {labels:?}"),
                };
                assert_eq!(page, expected, "{scope:?} {kind:?} {labels:?} {max_facts}");
                // The stored copies are exactly the returned facts.
                assert_eq!(rows.0.len() + rows.1.len(), page.facts.len());
                pages += 1;
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
                assert!(pages < 64);
            }
        }
    }
    for scope in [
        QueryScope::Node {
            id: "sym:none".into(),
        },
        QueryScope::Neighborhood {
            anchor: "sym:none".into(),
            hops: 1,
        },
    ] {
        let request = QueryRequest {
            scope: scope.clone(),
            max_facts: 4,
            max_bytes: 32 * 1024,
            ..Default::default()
        };
        let expected = held.query(request.clone()).unwrap_err();
        match paged::query_at(
            &fixture.state,
            &revision,
            SnapshotReadLimits::investigation(),
            &id,
            request,
        ) {
            Err(paged::PagedError::Context(error)) => {
                assert_eq!(error.to_string(), expected.to_string())
            }
            _ => panic!("{scope:?}"),
        }
    }
    // A store that moved past the pinned revision is a changed input.
    fixture
        .state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "late".into(),
            label: "Module".into(),
            props: json!({}),
        })
        .unwrap();
    assert!(matches!(
        paged::query_at(
            &fixture.state,
            &revision,
            SnapshotReadLimits::investigation(),
            &id,
            QueryRequest {
                max_facts: 4,
                max_bytes: 32 * 1024,
                ..Default::default()
            },
        ),
        Err(paged::PagedError::Host(HostError::InputChanged))
    ));
}

// AC-1311 (#1602): completion choices for a neighborhood or area read only the
// scope at the coverage revision, and give exactly the choices a whole-graph
// read gives; under retained limits the whole graph exceeds they still list.
#[test]
fn scoped_completion_choices_match_the_whole_graph_read() {
    use super::super::completion_choices::{SelectionError, choices, choices_at};
    let (fixture, area) = area_fixture();
    let read_scope = || Ok::<_, SelectionError>(fixture.scope.clone());
    for scope in [
        InvestigationScope::Area { id: area },
        InvestigationScope::Neighborhood {
            anchor: "sym:alias".into(),
            hops: 1,
        },
    ] {
        let scoped = choices_at(
            &fixture.state,
            &scope,
            SnapshotReadLimits::investigation(),
            read_scope,
        )
        .unwrap();
        let (base, recorded) = fixture
            .state
            .graph
            .lock()
            .unwrap()
            .read_snapshot_with_coverage_and_revision_bounded(
                SnapshotReadLimits::investigation(),
                core_graph::coverage::CoverageReadLimits::default(),
            )
            .unwrap();
        let files = super::super::area::files(&fixture.state, &base.revision, &scope).unwrap();
        let coverage =
            context_hub::coverage::CoverageQualification::new(&fixture.scope, &recorded).unwrap();
        let whole = choices(base.graph, coverage, &scope, files.as_ref()).unwrap();
        assert_eq!(
            serde_json::to_value(&scoped).unwrap(),
            serde_json::to_value(&whole).unwrap(),
            "{scope:?}"
        );
        assert_eq!(scoped.scoped_facts(), whole.scoped_facts(), "{scope:?}");
        assert!(scoped.scoped_facts().is_some_and(|facts| !facts.is_empty()));
    }
    for index in 0..60 {
        fixture
            .state
            .graph
            .lock()
            .unwrap()
            .put_node(&Node {
                id: format!("pad:{index:03}"),
                label: "Module".into(),
                props: json!({"name": index}),
            })
            .unwrap();
    }
    let retained = SnapshotReadLimits {
        max_rows: 40,
        ..SnapshotReadLimits::default()
    };
    let neighborhood = InvestigationScope::Neighborhood {
        anchor: "sym:alias".into(),
        hops: 1,
    };
    assert!(choices_at(&fixture.state, &neighborhood, retained, read_scope).is_ok());
    assert!(
        choices_at(
            &fixture.state,
            &InvestigationScope::All,
            retained,
            read_scope
        )
        .is_err()
    );
}

// AC-1302 (#1602): a scope-aware context rereads only the selected facts, at the
// revision it pinned; once the store has moved on, selection is a changed input.
#[test]
fn scoped_selection_rereads_selected_facts_at_the_pinned_revision() {
    let (fixture, _) = area_fixture();
    let mut requested = request();
    requested.scope = InvestigationScope::Neighborhood {
        anchor: "sym:alias".into(),
        hops: 1,
    };
    let prepared = fixture.prepare(&requested).unwrap();
    let keys = [FactKey::Node {
        id: "sym:alias".into(),
    }];
    let selected = prepared.select(&fixture.state, &keys).unwrap();
    assert_eq!(selected.len(), 1);
    fixture
        .state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "mod:later".into(),
            label: "Module".into(),
            props: json!({}),
        })
        .unwrap();
    assert!(matches!(
        prepared.select(&fixture.state, &keys),
        Err(HostError::InputChanged)
    ));
}
