use super::*;
use core_graph::{GraphPatch, coverage::CoverageRecord};
use serde_json::{Value, json};
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
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
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
            FrozenContext::scoped_snapshot(&previous.graph, &requested.scope, complete).unwrap();
        let previous_qualified = scoped.with_coverage(qualification).unwrap();
        let before = take_projection_work();
        assert!(!previous_qualified.shares_snapshot(&previous.snapshot));
        assert_eq!(Arc::strong_count(&previous.snapshot), 1);
        previous.ledger.schema_version = 2;
        previous.ledger.coverage = Some(previous_qualified.durable_proof().unwrap());
        previous.ledger.validate().unwrap();

        let prepared = fixture.prepare(&requested).unwrap();
        let after = take_projection_work();
        let qualified = prepared.qualified.as_ref().unwrap();
        assert!(qualified.shares_snapshot(&prepared.snapshot));
        assert_eq!(Arc::strong_count(&prepared.snapshot), 2);
        assert_eq!(before.constructions, 2 * after.constructions);
        assert_eq!(before.nodes, 2 * after.nodes);
        assert_eq!(before.edges, 2 * after.edges);
        let expected = if matches!(scope, InvestigationScope::All) {
            ProjectionWork {
                constructions: 1,
                nodes: 128,
                edges: 127,
            }
        } else {
            ProjectionWork {
                constructions: 2,
                nodes: 130,
                edges: 128,
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
        assert_eq!(prepared.graph, graph);
        assert!(
            prepared
                .qualified
                .as_ref()
                .unwrap()
                .shares_snapshot(&prepared.snapshot)
        );
        requested.scope = InvestigationScope::Neighborhood {
            anchor: "missing".into(),
            hops: 1,
        };
        assert!(fixture.prepare(&requested).is_err());
    }
}
