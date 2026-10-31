// AC-0506: the knowledge phase runs after ingest and records its outcome in
// the overlay recipe without touching the base (#1441).
use super::*;
use core_graph::overlay::OverlayRead;
use core_graph::overlay::{BaseRef, MAX_NAME_BYTES, OverlayItem};
use core_graph::{GraphStore, Node};
use llm::{Embedding, EmbeddingIdentity, Locality, ProviderCaps, ProviderError};

struct Fixture {
    fail: bool,
}
impl LlmProvider for Fixture {
    fn id(&self) -> &str {
        "fixture"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: true,
            chat: false,
            tool_use: false,
        }
    }
    fn embedding_cache_identity(&self) -> Result<Option<EmbeddingIdentity>, ProviderError> {
        Ok(Some(EmbeddingIdentity {
            endpoint: "http://127.0.0.1:11434/".into(),
            model: "fixture".into(),
            digest: format!("sha256:{:064x}", 1),
            recipe: "fixture@1".into(),
        }))
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        if self.fail {
            return Err(ProviderError::InvalidEmbeddings("fixture".into()));
        }
        Ok(texts.iter().map(|t| vec![1.0, t.len() as f32]).collect())
    }
}

struct Host {
    _dir: tempfile::TempDir,
    graph: Mutex<SqliteGraphStore>,
    cache: Arc<Mutex<EmbeddingCache>>,
    overlay: Arc<Mutex<OverlayStore>>,
}

fn host() -> Host {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    for (id, label) in [("ep:pay", "Endpoint"), ("sym:charge", "Symbol")] {
        graph
            .put_node(&Node {
                id: id.into(),
                label: label.into(),
                props: serde_json::json!({ "name": id }),
            })
            .unwrap();
    }
    let instance = graph.instance_id().unwrap();
    let cache = EmbeddingCache::open(dir.path().join("cache.db")).unwrap();
    let mut overlay = OverlayStore::open_in_memory().unwrap();
    overlay.bind_instance(&instance).unwrap();
    Host {
        _dir: dir,
        graph: Mutex::new(graph),
        cache: Arc::new(Mutex::new(cache)),
        overlay: Arc::new(Mutex::new(overlay)),
    }
}

fn recipe_of(host: &Host) -> Option<OverlayRecipe> {
    let revision = host.graph.lock().unwrap().read_revision().unwrap();
    match host.overlay.lock().unwrap().read(&revision).unwrap() {
        OverlayRead::Current { basis, facts, .. } => {
            assert!(facts.is_empty());
            Some(basis.recipe)
        }
        _ => None,
    }
}

#[test]
fn no_provider_publishes_the_overlay_as_a_named_omission() {
    let host = host();
    let before = host.graph.lock().unwrap().read_snapshot().unwrap();
    let report = run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert_eq!(
        report,
        KnowledgeReport {
            embedded: 0,
            omitted: 0,
            omission: Some(knowledge_ingest::NO_PROVIDER),
            published: true,
            derived: 0,
            oversized: 0,
            derived_omission: None,
            producer_omissions: vec![],
        }
    );
    assert_eq!(recipe_of(&host), Some(recipe(None, false)));
    assert_eq!(host.graph.lock().unwrap().read_snapshot().unwrap(), before);
}

#[test]
fn an_embedded_phase_records_its_namespace_in_the_overlay_recipe() {
    let host = host();
    let provider = Fixture { fail: false };
    let report = run(&host.graph, &host.cache, &host.overlay, Some(&provider)).unwrap();
    assert_eq!(
        (report.embedded, report.omission, report.published),
        (2, None, true)
    );
    let recipe = recipe_of(&host).unwrap();
    assert!(recipe.provider_ran);
    let namespace = recipe.embedding_namespace.unwrap();
    assert!(!namespace.is_empty());
    // A rerun over the same base reuses the cached vectors and the namespace.
    run(&host.graph, &host.cache, &host.overlay, Some(&provider)).unwrap();
    assert_eq!(
        recipe_of(&host).unwrap().embedding_namespace,
        Some(namespace)
    );
}

#[test]
fn a_failing_provider_is_a_named_omission_and_still_publishes() {
    let host = host();
    let report = run(
        &host.graph,
        &host.cache,
        &host.overlay,
        Some(&Fixture { fail: true }),
    )
    .unwrap();
    assert_eq!(
        (report.omission, report.published),
        (Some(PROVIDER_FAILED), true)
    );
    assert_eq!(recipe_of(&host), Some(recipe(None, false)));
}

#[test]
fn a_base_past_the_snapshot_bounds_is_a_named_omission() {
    let host = host();
    let limits = SnapshotReadLimits {
        max_rows: 1,
        ..SnapshotReadLimits::default()
    };
    let report = run_within(&host.graph, &host.cache, &host.overlay, None, limits).unwrap();
    assert_eq!(
        (report.omission, report.published),
        (Some(BASE_BOUNDS), false)
    );
    assert_eq!(recipe_of(&host), None);
}

#[test]
fn a_base_that_moved_during_the_phase_is_not_published_over() {
    let host = host();
    let snapshot = host
        .graph
        .lock()
        .unwrap()
        .read_snapshot_with_revision_bounded(SnapshotReadLimits::default())
        .unwrap();
    host.graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "sym:late".into(),
            label: "Symbol".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
    assert!(matches!(
        publish(
            &host.graph,
            &host.overlay,
            &snapshot,
            recipe(None, false),
            vec![]
        )
        .unwrap(),
        Published::Moved
    ));
    assert_eq!(recipe_of(&host), None);
}

/// A NestJS resolver as the real TS adapter extracts it.
fn put_resolver(graph: &Mutex<SqliteGraphStore>) {
    let facts = adapters_lang_ts::extract_source(
        b"import {Resolver, Query, Mutation} from '@nestjs/graphql'; @Resolver() export class ShopOrderResolver { @Query() order() {} @Mutation() addItemToOrder() {} }",
        "packages/core/src/api/resolvers/shop/shop-order.resolver.ts",
        &adapters_lang_ts::SourceId {
            repo: "vendurehq/vendure",
            commit: "2cc309deb5394af176314552ac7320a7b26a0104",
        },
    )
    .unwrap();
    let mut graph = graph.lock().unwrap();
    for node in &facts.nodes {
        graph.put_node(node).unwrap();
    }
    // Import edges to external modules have no local node to land on.
    let ids: std::collections::BTreeSet<_> = facts.nodes.iter().map(|n| &n.id).collect();
    for edge in facts
        .edges
        .iter()
        .filter(|e| ids.contains(&e.src) && ids.contains(&e.dst))
    {
        graph.put_edge(edge).unwrap();
    }
}

fn published_facts(host: &Host) -> Vec<OverlayFact> {
    let revision = host.graph.lock().unwrap().read_revision().unwrap();
    match host.overlay.lock().unwrap().read(&revision).unwrap() {
        OverlayRead::Current { facts, .. } => facts,
        _ => panic!("overlay is current"),
    }
}

// AC-0514: deterministic producer facts publish with the phase, without a
// provider, and the base is unchanged.
#[test]
fn derived_capabilities_publish_without_a_provider() {
    let host = host();
    put_resolver(&host.graph);
    let before = host.graph.lock().unwrap().read_snapshot().unwrap();
    let report = run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    // One resolver Capability and one traced BusinessFlow per operation.
    assert_eq!(
        (report.published, report.derived, report.oversized),
        (true, 3, 0)
    );
    assert_eq!(report.derived_omission, None);
    assert!(report.producer_omissions.is_empty());
    let facts = published_facts(&host);
    let nodes: Vec<_> = facts
        .iter()
        .map(|fact| match &fact.item {
            OverlayItem::Node(node) => node,
            OverlayItem::Edge(_) => panic!("producers emit nodes"),
        })
        .collect();
    let [capability] = nodes
        .iter()
        .filter(|n| n.label == "Capability")
        .collect::<Vec<_>>()[..]
    else {
        panic!("one Capability")
    };
    assert_eq!(
        capability.props["title"],
        "shop GraphQL operations in ShopOrderResolver"
    );
    let flows: Vec<_> = nodes.iter().filter(|n| n.label == "BusinessFlow").collect();
    assert_eq!(flows.len(), 2);
    assert!(facts.iter().all(|fact| !fact.cites.is_empty()));
    assert_eq!(host.graph.lock().unwrap().read_snapshot().unwrap(), before);
    // Same base, same facts: a rerun republishes the identical set.
    run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert_eq!(published_facts(&host), facts);
}

/// A JPA entity as the real Java adapter extracts it.
fn put_entity(graph: &Mutex<SqliteGraphStore>) {
    let facts = adapters_lang_java::extract_source(
        b"package com.demo; import jakarta.persistence.Entity; import jakarta.persistence.Id; import jakarta.persistence.Table; @Entity @Table(name = \"orders\") public class Order { @Id private Integer id; private String status; }",
        "src/main/java/com/demo/Order.java",
        &adapters_lang_java::SourceId {
            repo: "local/demo",
            commit: "fixture-commit",
        },
    )
    .unwrap();
    let mut graph = graph.lock().unwrap();
    for node in &facts.nodes {
        graph.put_node(node).unwrap();
    }
    let ids: std::collections::BTreeSet<_> = facts.nodes.iter().map(|n| &n.id).collect();
    for edge in facts
        .edges
        .iter()
        .filter(|e| ids.contains(&e.src) && ids.contains(&e.dst))
    {
        graph.put_edge(edge).unwrap();
    }
}

// AC-0520: ORM DomainEntity facts publish with the phase beside the other
// producers, without a provider, and the base is unchanged.
#[test]
fn orm_domain_entities_publish_with_the_phase() {
    let host = host();
    put_resolver(&host.graph);
    put_entity(&host.graph);
    let before = host.graph.lock().unwrap().read_snapshot().unwrap();
    let report = run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    // The resolver's Capability and two BusinessFlows, then the entity's
    // DomainEntity candidate and its MAPS_TO mapping.
    assert_eq!(
        (report.published, report.derived, report.oversized),
        (true, 5, 0)
    );
    assert_eq!(report.derived_omission, None);
    assert!(report.producer_omissions.is_empty());
    let facts = published_facts(&host);
    let [entity] = facts
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Node(node) if node.label == "DomainEntity" => Some(node),
            _ => None,
        })
        .collect::<Vec<_>>()[..]
    else {
        panic!("one DomainEntity")
    };
    let [mapping] = facts
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Edge(edge) if edge.label == "MAPS_TO" => Some(edge),
            _ => None,
        })
        .collect::<Vec<_>>()[..]
    else {
        panic!("one MAPS_TO mapping")
    };
    assert_eq!(mapping.src, entity.id);
    assert!(
        before
            .0
            .iter()
            .any(|n| n.id == mapping.dst && n.label == "DataEntity")
    );
    assert!(facts.iter().all(|fact| !fact.cites.is_empty()));
    assert_eq!(host.graph.lock().unwrap().read_snapshot().unwrap(), before);
    run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert_eq!(published_facts(&host), facts);
}

/// A permission-gated NestJS resolver as the real TS adapter extracts it.
fn put_permission_resolver(graph: &Mutex<SqliteGraphStore>) {
    let facts = adapters_lang_ts::extract_source(
        b"import { Query, Resolver } from '@nestjs/graphql';\nimport { Permission } from '@vendure/common/lib/generated-shop-types';\nimport { Allow } from '../../decorators/allow.decorator';\n@Resolver() export class ShopOrderResolver { @Query()\n@Allow(Permission.Owner)\nactiveOrder() {} }",
        "packages/core/src/api/resolvers/shop/shop-order.resolver.ts",
        &adapters_lang_ts::SourceId {
            repo: "vendurehq/vendure",
            commit: "2cc309deb5394af176314552ac7320a7b26a0104",
        },
    )
    .unwrap();
    let mut graph = graph.lock().unwrap();
    for node in &facts.nodes {
        graph.put_node(node).unwrap();
    }
    let ids: std::collections::BTreeSet<_> = facts.nodes.iter().map(|n| &n.id).collect();
    for edge in facts
        .edges
        .iter()
        .filter(|e| ids.contains(&e.src) && ids.contains(&e.dst))
    {
        graph.put_edge(edge).unwrap();
    }
}

fn labels(facts: &[OverlayFact]) -> Vec<&str> {
    let mut labels: Vec<_> = facts
        .iter()
        .map(|fact| match &fact.item {
            OverlayItem::Node(node) => node.label.as_str(),
            OverlayItem::Edge(edge) => edge.label.as_str(),
        })
        .collect();
    labels.sort_unstable();
    labels
}

// AC-0532: a permission Actor publishes with the phase beside the other
// producers, citing its base Endpoint, and the base is unchanged.
#[test]
fn permission_actors_publish_with_the_phase() {
    let host = host();
    put_permission_resolver(&host.graph);
    let before = host.graph.lock().unwrap().read_snapshot().unwrap();
    let report = run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert_eq!(
        (report.published, report.derived, report.oversized),
        (true, 3, 0)
    );
    assert_eq!(report.derived_omission, None);
    let facts = published_facts(&host);
    assert_eq!(labels(&facts), ["Actor", "BusinessFlow", "Capability"]);
    let [(actor, cites)] = facts
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Node(node) if node.label == "Actor" => Some((node, &fact.cites)),
            _ => None,
        })
        .collect::<Vec<_>>()[..]
    else {
        panic!("one Actor")
    };
    assert_eq!(actor.props["any_of"], serde_json::json!(["Owner"]));
    let endpoint = actor.props["endpoint_id"].as_str().unwrap();
    assert!(
        before
            .0
            .iter()
            .any(|n| n.id == endpoint && n.label == "Endpoint")
    );
    assert_eq!(
        *cites,
        std::collections::BTreeSet::from([BaseRef::Node(endpoint.into())])
    );
    assert_eq!(host.graph.lock().unwrap().read_snapshot().unwrap(), before);
    run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert_eq!(published_facts(&host), facts);
}

// AC-0532: an Actor the overlay refuses refuses the whole derived set; no
// Capability, BusinessFlow, DomainEntity or Actor subset is published.
#[test]
fn a_refused_actor_publishes_none_of_the_derived_facts() {
    let host = host();
    put_permission_resolver(&host.graph);
    put_entity(&host.graph);
    let snapshot = host
        .graph
        .lock()
        .unwrap()
        .read_snapshot_with_revision_bounded(SnapshotReadLimits::default())
        .unwrap();
    let derived = derive(&snapshot);
    assert_eq!(
        labels(&derived.facts),
        [
            "Actor",
            "BusinessFlow",
            "Capability",
            "DomainEntity",
            "MAPS_TO"
        ]
    );
    let actor = derived
        .facts
        .iter()
        .find_map(|fact| match &fact.item {
            OverlayItem::Node(node) if node.label == "Actor" => Some(node.id.clone()),
            _ => None,
        })
        .unwrap();
    // A base node under the Actor id makes the overlay refuse to shadow it.
    host.graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: actor,
            label: "Symbol".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
    let report = run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert_eq!(
        (report.published, report.derived, report.derived_omission),
        (true, 0, Some(DERIVED_REJECTED))
    );
    assert!(published_facts(&host).is_empty());
    assert_eq!(
        recipe_of(&host).unwrap().recipe,
        format!("{};{DERIVED_REJECTED}", recipe_name())
    );
}

// AC-0514: a producer that fails as a whole is a named omission; the other
// producers' facts are still derived.
#[test]
fn a_failed_producer_is_named_and_the_others_still_derive() {
    let host = host();
    put_resolver(&host.graph);
    let snapshot = host
        .graph
        .lock()
        .unwrap()
        .read_snapshot_with_revision_bounded(SnapshotReadLimits::default())
        .unwrap();
    let derived = derive_within(
        &snapshot,
        flowtracer::TraceLimits {
            max_flows: 1,
            ..FLOW_LIMITS
        },
    );
    assert_eq!(derived.omissions, vec!["business-flow-trace-flows"]);
    let [fact] = derived.facts.as_slice() else {
        panic!("only the Capability")
    };
    assert!(matches!(&fact.item, OverlayItem::Node(n) if n.label == "Capability"));
}

// AC-0514: a refused derived set still publishes the phase, named.
#[test]
fn a_refused_derived_set_publishes_the_phase_without_it() {
    let host = host();
    put_resolver(&host.graph);
    let snapshot = host
        .graph
        .lock()
        .unwrap()
        .read_snapshot_with_revision_bounded(SnapshotReadLimits::default());
    let facts = derive(&snapshot.unwrap()).facts;
    let OverlayItem::Node(capability) = &facts[0].item else {
        panic!("Capability is a node")
    };
    // A base node under the derived id makes the overlay refuse to shadow it.
    host.graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: capability.id.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({}),
        })
        .unwrap();
    let report = run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert_eq!(
        (report.published, report.derived, report.derived_omission),
        (true, 0, Some(DERIVED_REJECTED))
    );
    let recipe = recipe_of(&host).unwrap();
    assert_eq!(
        recipe.recipe,
        format!("{};{DERIVED_REJECTED}", recipe_name())
    );
    assert!(!recipe.provider_ran);
}

// AC-0514: each producer rule version is part of the overlay identity.
#[test]
fn the_recipe_names_every_producer_rule_within_the_name_bound() {
    let name = recipe_name();
    assert!(name.starts_with(RECIPE));
    assert!(name.contains(&format!(
        "{}@{}",
        spec::CAPABILITY_GRAPHQL_RULE,
        spec::CAPABILITY_GRAPHQL_RULE_VERSION
    )));
    assert!(name.contains(&format!(
        "{}@{}",
        spec::BUSINESS_FLOW_RULE,
        spec::BUSINESS_FLOW_RULE_VERSION
    )));
    assert!(name.contains(&format!(
        "{}@{}",
        spec::DOMAIN_ENTITY_ORM_RULE,
        spec::DOMAIN_ENTITY_ORM_RULE_VERSION
    )));
    assert!(name.contains(&format!(
        "{}@{}",
        spec::ACTOR_ALLOW_RULE,
        spec::ACTOR_ALLOW_RULE_VERSION
    )));
    assert!(format!("{name};{DERIVED_REJECTED}").len() <= MAX_NAME_BYTES);
}

#[test]
fn ingests_during_a_running_phase_coalesce_into_one_follow_up_pass() {
    let worker = Coalescer::new();
    assert!(worker.admit());
    // Any number of ingests while it runs start no worker and owe one pass.
    for _ in 0..5 {
        assert!(!worker.admit());
    }
    assert!(worker.next());
    assert!(!worker.next());
    // Idle again: the next ingest starts a worker.
    assert!(worker.admit());
    worker.abandon();
    assert!(worker.admit());
}

#[test]
fn the_worker_runs_owed_passes_and_none_while_t2_is_disabled() {
    let worker = Coalescer::new();
    assert!(worker.admit());
    let mut passes = 0;
    drain(
        &worker,
        || true,
        || {
            passes += 1;
            if passes == 1 {
                assert!(!worker.admit());
            }
        },
    );
    assert_eq!(passes, 2);

    assert!(worker.admit());
    assert!(!worker.admit());
    let mut disabled = 0;
    drain(&worker, || false, || disabled += 1);
    assert_eq!(disabled, 0);
    assert!(worker.admit());
}

#[test]
fn the_phase_follows_the_t2_setting() {
    let dir = tempfile::tempdir().unwrap();
    let settings =
        Mutex::new(crate::settings::SettingsStore::open(dir.path().join("s.db")).unwrap());
    assert!(semantic_enabled(&settings));
    settings.lock().unwrap().set_enabled("T2", false).unwrap();
    assert!(!semantic_enabled(&settings));
    settings.lock().unwrap().set_enabled("T3", true).unwrap();
    assert!(!semantic_enabled(&settings));
}
