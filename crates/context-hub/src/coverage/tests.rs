use super::*;
use core_graph::coverage::CoverageRecord;
use core_graph::{Edge, Node};
use serde_json::json;

fn inventory(repo: &str, states: &[&str], unsupported: usize, limited: bool) -> String {
    json!({
        "inventory": "coverage@1",
        "repo": repo,
        "commit": "a".repeat(40),
        "layer_hints": ["server", "infra"],
        "producers": states
            .iter()
            .map(|state| json!({"producer": "t0.test", "state": state}))
            .collect::<Vec<_>>(),
        "unsupported": (0..unsupported).map(|n| json!({"capability": n})).collect::<Vec<_>>(),
        "limited": limited,
    })
    .to_string()
}

fn stored(repo: &str, ingest_id: &str, inventory_json: String) -> StoredCoverage {
    let record = CoverageRecord {
        repo_key: repo.into(),
        registered_source_id: Some(format!("src:{repo}")),
        ingest_id: ingest_id.into(),
        config_hash: "config:v1".into(),
        inventory_json,
    };
    StoredCoverage {
        inventory_hash: record.inventory_hash(),
        record,
    }
}

fn scoped(repo: &str, source: Option<&str>, config: &str) -> ScopedRepo {
    ScopedRepo {
        repo_key: repo.into(),
        registered_source_id: source.map(str::to_string),
        config_hash: config.into(),
        layer_hints: None,
    }
}

fn snapshot() -> ContextSnapshot {
    let nodes = (0..3)
        .map(|n| Node {
            id: format!("mod:{n}"),
            label: "Module".into(),
            props: json!({}),
        })
        .collect();
    ContextSnapshot::new(nodes, Vec::<Edge>::new()).unwrap()
}

// AC-0239: every scoped or recorded repo is qualified; only a current
// registration and configuration make an inventory recorded, and nothing is
// ever exhaustive. (T-0239)
#[test]
fn qualification_marks_recorded_stale_and_unrecorded_repos_and_never_exhaustive() {
    let recorded = [
        stored(
            "acme/api",
            "job-1",
            inventory(
                "acme/api",
                &["ran", "not_selected", "missing_input"],
                1,
                true,
            ),
        ),
        stored(
            "acme/web",
            "job-1",
            inventory("acme/web", &["ran", "not_applicable"], 0, false),
        ),
        stored(
            "acme/config",
            "job-1",
            inventory("acme/config", &["ran"], 0, false),
        ),
        stored(
            "acme/gone",
            "job-1",
            inventory("acme/gone", &["ran"], 0, false),
        ),
    ];
    let scope = [
        scoped("acme/api", Some("src:acme/api"), "config:v1"),
        scoped("acme/web", Some("src:acme/web"), "config:v1"),
        scoped("acme/config", Some("src:acme/config"), "config:v2"),
        scoped("acme/new", Some("src:acme/new"), "config:v1"),
    ];
    let coverage = CoverageQualification::new(&scope, &recorded).unwrap();
    assert_eq!(coverage.contract, COVERAGE_CONTRACT);
    assert!(!coverage.exhaustive);
    let status: Vec<_> = coverage
        .repos
        .iter()
        .map(|repo| (repo.repo.as_str(), repo.status, repo.limited))
        .collect();
    assert_eq!(
        status,
        [
            ("acme/api", CoverageStatus::Recorded, Some(true)),
            ("acme/config", CoverageStatus::Stale, None),
            ("acme/gone", CoverageStatus::Stale, None),
            ("acme/new", CoverageStatus::Unrecorded, None),
            ("acme/web", CoverageStatus::Recorded, Some(false)),
        ]
    );
    assert_eq!(
        coverage.repos[0].counts,
        Some(CoverageCounts {
            not_selected: 1,
            missing_input: 1,
            not_applicable: 0,
            unsupported: 1,
        })
    );
    assert_eq!(
        coverage.repos[0].commit.as_deref(),
        Some("a".repeat(40).as_str())
    );
    // Without a requested selection, recorded means that exact prior ingest,
    // and the block says what was and was not compared.
    assert_eq!(
        coverage.repos[0].layer_hints,
        Some(vec!["infra".to_string(), "server".to_string()])
    );
    assert_eq!(
        coverage.repos[0].compared,
        [COMPARED_REGISTRATION, COMPARED_PLUGINS]
    );
    assert_eq!(coverage.repos[3].compared, Vec::<String>::new());
    // A stale inventory keeps its hash but derives nothing from its body.
    assert_eq!(
        coverage.repos[1].inventory_hash,
        Some(recorded[2].inventory_hash.clone())
    );
    assert_eq!(
        (coverage.repos[1].commit.as_ref(), coverage.repos[1].counts),
        (None, None)
    );

    let api = &recorded[0];
    assert_eq!(
        coverage.inventory("acme/api", &api.inventory_hash).unwrap(),
        api.record.inventory_json
    );
    assert!(matches!(
        coverage.inventory("acme/api", &recorded[1].inventory_hash),
        Err(ContextError::InventoryChanged(_))
    ));
    assert!(matches!(
        coverage.inventory("acme/config", &recorded[2].inventory_hash),
        Err(ContextError::InventoryUnavailable(_))
    ));
    assert!(matches!(
        coverage.inventory("acme/other", &api.inventory_hash),
        Err(ContextError::InventoryUnavailable(_))
    ));

    // A different registration is stale too, and so is a matching absent one.
    let moved = [scoped("acme/api", Some("src:elsewhere"), "config:v1")];
    let coverage = CoverageQualification::new(&moved, &recorded[..1]).unwrap();
    assert_eq!(coverage.repos[0].status, CoverageStatus::Stale);
    let mut unregistered = recorded[0].clone();
    unregistered.record.registered_source_id = None;
    let coverage = CoverageQualification::new(
        &[scoped("acme/api", None, "config:v1")],
        std::slice::from_ref(&unregistered),
    )
    .unwrap();
    assert_eq!(coverage.repos[0].status, CoverageStatus::Stale);

    // A requested layer selection is compared as a set; a different one is
    // stale even though the registration and plugins match.
    for (requested, status) in [
        (vec!["infra", "server", "infra"], CoverageStatus::Recorded),
        (vec!["server"], CoverageStatus::Stale),
        (vec![], CoverageStatus::Stale),
    ] {
        let mut now = scoped("acme/api", Some("src:acme/api"), "config:v1");
        now.layer_hints = Some(requested.into_iter().map(str::to_string).collect());
        let coverage = CoverageQualification::new(&[now], &recorded[..1]).unwrap();
        assert_eq!(coverage.repos[0].status, status);
        assert_eq!(
            coverage.repos[0].compared,
            [
                COMPARED_REGISTRATION,
                COMPARED_PLUGINS,
                COMPARED_LAYER_HINTS
            ]
        );
    }
}

// AC-0239: an inventory that disagrees with its repo, its producer states,
// its own limited flag or its version is refused rather than qualified. (T-0239)
#[test]
fn qualification_refuses_inconsistent_inventories() {
    let scope = [scoped("acme/api", Some("src:acme/api"), "config:v1")];
    for body in [
        inventory("acme/web", &["ran"], 0, false),
        inventory("acme/api", &["ran", "not_selected"], 0, false),
        inventory("acme/api", &["ran", "not_applicable"], 0, true),
        inventory("acme/api", &["ran", "skipped"], 0, false),
        json!({"repo": "acme/api"}).to_string(),
        inventory("acme/api", &["ran"], 0, false).replace("coverage@1", "coverage@2"),
    ] {
        assert!(matches!(
            CoverageQualification::new(&scope, &[stored("acme/api", "job-1", body)]),
            Err(ContextError::InvalidCoverage(_))
        ));
    }
    let twice = [scope[0].clone(), scope[0].clone()];
    assert!(matches!(
        CoverageQualification::new(&twice, &[]),
        Err(ContextError::InvalidCoverage(_))
    ));
}

// AC-0239: the coverage identity binds the recorded basis behind an identical
// public block, and the context identity binds coverage to an unchanged graph
// snapshot identity. (T-0239)
#[test]
fn coverage_and_context_identities_bind_the_recorded_basis() {
    let scope = [scoped("acme/api", Some("src:acme/api"), "config:v1")];
    let body = inventory("acme/api", &["ran"], 0, false);
    let first =
        CoverageQualification::new(&scope, &[stored("acme/api", "job-1", body.clone())]).unwrap();
    let rerun = CoverageQualification::new(&scope, &[stored("acme/api", "job-2", body)]).unwrap();
    assert_eq!(first.repos, rerun.repos);
    assert_ne!(first.coverage_id, rerun.coverage_id);

    let graph_id = snapshot().id().to_string();
    let first = snapshot().with_coverage(first).unwrap();
    let rerun = snapshot().with_coverage(rerun).unwrap();
    assert_eq!(
        (first.snapshot_id(), rerun.snapshot_id()),
        (graph_id.as_str(), graph_id.as_str())
    );
    assert_ne!(first.context_id(), rerun.context_id());
}

// AC-0239: a qualified page counts its qualification inside the caller's byte
// budget, continues only within the context that issued it and fails rather
// than dropping a qualification that cannot fit. (T-0239)
#[test]
fn qualified_query_is_bounded_and_continues_only_in_its_context() {
    let scope = [scoped("acme/api", Some("src:acme/api"), "config:v1")];
    let recorded = [stored(
        "acme/api",
        "job-1",
        inventory("acme/api", &["ran"], 0, false),
    )];
    let context = snapshot()
        .with_coverage(CoverageQualification::new(&scope, &recorded).unwrap())
        .unwrap();
    let request = QueryRequest {
        max_facts: 1,
        max_bytes: 4096,
        ..QueryRequest::default()
    };
    let first = context
        .query(QualifiedRequest {
            request: request.clone(),
            context_id: None,
        })
        .unwrap();
    assert!(serde_json::to_vec(&first).unwrap().len() <= 4096);
    assert_eq!(first.context_id, context.context_id());
    assert_eq!(&first.coverage, context.coverage());
    assert_eq!(first.response.snapshot_id, context.snapshot_id());
    let cursor = first.response.next_cursor.clone().unwrap();
    let continued = QueryRequest {
        cursor: Some(cursor),
        ..request.clone()
    };

    let rerun = [stored(
        "acme/api",
        "job-2",
        inventory("acme/api", &["ran"], 0, false),
    )];
    let other = snapshot()
        .with_coverage(CoverageQualification::new(&scope, &rerun).unwrap())
        .unwrap();
    for (context_id, target) in [
        (None, &context),
        (Some(context.context_id().to_string()), &other),
    ] {
        assert!(matches!(
            target.query(QualifiedRequest {
                request: continued.clone(),
                context_id,
            }),
            Err(ContextError::CursorContextMismatch)
        ));
    }
    let second = context
        .query(QualifiedRequest {
            request: continued,
            context_id: Some(context.context_id().to_string()),
        })
        .unwrap();
    assert_ne!(second.response.facts, first.response.facts);

    let tight = QueryRequest {
        max_bytes: 64,
        ..request
    };
    assert!(matches!(
        context.query(QualifiedRequest {
            request: tight,
            context_id: None,
        }),
        Err(ContextError::ResponseBudgetExceeded { max_bytes: 64, .. })
    ));
}

// AC-0239: qualifying an already-owned projection shares exactly that immutable
// snapshot and preserves legacy query bytes, qualified pages, budgets and proof.
#[test]
fn shared_coverage_projection_preserves_owned_wire_identity_and_cursor_contract() {
    let scope = [scoped("acme/api", Some("src:acme/api"), "config:v1")];
    let recorded = [stored(
        "acme/api",
        "job-1",
        inventory("acme/api", &["ran"], 0, false),
    )];
    let coverage = CoverageQualification::new(&scope, &recorded).unwrap();
    let graph = || {
        ContextSnapshot::new((0..3).map(|index| Node {
        id: format!("mod:{index}"), label: "Module".into(), props: json!({"name": format!("module-{index}"), "nested": {"escaped": "a\nb"}}),
    }).collect(), (0..2).map(|index| Edge { src: format!("mod:{index}"), dst: format!("mod:{}", index + 1), label: "CALLS".into(), props: json!({}) }).collect()).unwrap()
    };
    let original = Arc::new(graph());
    let shared =
        CoverageContext::from_shared_snapshot(Arc::clone(&original), coverage.clone()).unwrap();
    let owned = graph().with_coverage(coverage).unwrap();
    assert!(std::ptr::eq(original.as_ref(), shared.snapshot()));
    assert_eq!(Arc::strong_count(&original), 2);
    assert_eq!(shared.snapshot_id(), owned.snapshot_id());
    assert_eq!(shared.context_id(), owned.context_id());
    assert_eq!(
        serde_json::to_vec(&shared.durable_proof().unwrap()).unwrap(),
        serde_json::to_vec(&owned.durable_proof().unwrap()).unwrap()
    );
    let request = QueryRequest {
        max_facts: 1,
        max_bytes: 4096,
        ..QueryRequest::default()
    };
    assert_eq!(
        serde_json::to_vec(&original.query(request.clone()).unwrap()).unwrap(),
        serde_json::to_vec(&shared.snapshot().query(request.clone()).unwrap()).unwrap()
    );
    let mut cursor = None;
    let mut facts = 0;
    loop {
        let qualified = QualifiedRequest {
            request: QueryRequest {
                cursor,
                ..request.clone()
            },
            context_id: Some(shared.context_id().to_owned()),
        };
        let actual = shared.query(qualified.clone()).unwrap();
        let previous = owned.query(qualified).unwrap();
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&previous).unwrap()
        );
        facts += actual.response.facts.len();
        let Some(next) = actual.response.next_cursor else {
            break;
        };
        cursor = Some(next);
    }
    assert_eq!(facts, 5);
    let neighborhood = QualifiedRequest {
        request: QueryRequest {
            scope: super::super::QueryScope::Neighborhood {
                anchor: "mod:0".into(),
                hops: 1,
            },
            ..QueryRequest::default()
        },
        context_id: None,
    };
    let scoped = shared.query(neighborhood.clone()).unwrap();
    assert_eq!(scoped.response.facts.len(), 3);
    assert_eq!(
        serde_json::to_vec(&scoped).unwrap(),
        serde_json::to_vec(&owned.query(neighborhood).unwrap()).unwrap()
    );
    for (max_bytes, context_id) in [(64, None), (4096, Some("wrong-context".into()))] {
        let request = QualifiedRequest {
            request: QueryRequest {
                max_bytes,
                ..request.clone()
            },
            context_id,
        };
        assert_eq!(
            shared.query(request.clone()).unwrap_err().to_string(),
            owned.query(request).unwrap_err().to_string()
        );
    }
    drop(shared);
    assert_eq!(Arc::strong_count(&original), 1);
    assert_eq!(
        original.query(QueryRequest::default()).unwrap().facts.len(),
        5
    );
}
