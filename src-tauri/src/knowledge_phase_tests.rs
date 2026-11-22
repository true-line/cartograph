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
            sections_read: 0,
            sections_unread: 0,
            derived: 0,
            oversized: 0,
            derived_omission: None,
            producer_omissions: vec![],
            published_producer_nodes: ProducerNodeCounts::default(),
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

// AC-0583 / #1771: a base larger than the interactive read default is not
// skipped; the phase reads it and publishes.
#[test]
fn a_base_past_the_interactive_default_is_read_and_published() {
    let host = host();
    {
        let mut graph = host.graph.lock().unwrap();
        for index in 0..=SnapshotReadLimits::DEFAULT_ROWS {
            graph
                .put_node(&Node {
                    id: format!("sym:bulk:{index}"),
                    label: "Symbol".into(),
                    props: serde_json::json!({}),
                })
                .unwrap();
        }
    }
    // The same base under the interactive default is the named omission.
    let skipped = run_within(
        &host.graph,
        &host.cache,
        &host.overlay,
        None,
        SnapshotReadLimits::default(),
        &no_sections,
    )
    .unwrap();
    assert_eq!(skipped.omission, Some(BASE_BOUNDS));

    let report = run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert_ne!(report.omission, Some(BASE_BOUNDS));
    assert!(report.published);
    assert!(recipe_of(&host).is_some());
}

#[test]
fn a_base_past_the_snapshot_bounds_is_a_named_omission() {
    let host = host();
    let limits = SnapshotReadLimits {
        max_rows: 1,
        ..SnapshotReadLimits::default()
    };
    let report = run_within(
        &host.graph,
        &host.cache,
        &host.overlay,
        None,
        limits,
        &no_sections,
    )
    .unwrap();
    assert_eq!(
        (report.omission, report.published),
        (Some(BASE_BOUNDS), false)
    );
    assert_eq!(recipe_of(&host), None);
    assert_eq!(
        report.published_producer_nodes,
        ProducerNodeCounts::default()
    );
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

// AC-0506: the flows the producers trace from the base are embedded with
// the base's own units, and a rerun finds every one of them cached.
#[test]
fn traced_flows_are_embedded_with_the_base_units() {
    let host = host();
    put_resolver(&host.graph);
    let base_units = {
        let snapshot = host.graph.lock().unwrap().read_snapshot().unwrap();
        knowledge_ingest::units(&snapshot.0).0.len()
    };
    let provider = Fixture { fail: false };
    let report = run(&host.graph, &host.cache, &host.overlay, Some(&provider)).unwrap();
    assert_eq!(report.published_producer_nodes.business_flows, 2);
    assert_eq!(
        (report.embedded, report.omitted, report.omission),
        (base_units + 2, 0, None)
    );
    let again = run(&host.graph, &host.cache, &host.overlay, Some(&provider)).unwrap();
    assert_eq!(again.embedded, base_units + 2);
    // Without a provider the flows still publish and nothing is embedded.
    let none = run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert_eq!(
        (none.embedded, none.published_producer_nodes.business_flows),
        (0, 2)
    );
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
    assert_eq!(
        report.published_producer_nodes,
        ProducerNodeCounts {
            capabilities: 1,
            business_flows: 2,
            domain_entities: 0,
            actors: 0,
        }
    );
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
    assert_eq!(capability.props["title"], "Manage orders (shop API)");
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
    assert_eq!(
        report.published_producer_nodes,
        ProducerNodeCounts {
            capabilities: 1,
            business_flows: 1,
            domain_entities: 0,
            actors: 1,
        }
    );
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
    assert_eq!(
        producer_node_counts(&derived.facts),
        ProducerNodeCounts {
            capabilities: 1,
            business_flows: 1,
            domain_entities: 1,
            actors: 1,
        }
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
    // The empty phase overlay published, but none of the attempted producers did.
    assert_eq!(
        report.published_producer_nodes,
        ProducerNodeCounts::default()
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

// AC-0514, AC-0978: each producer rule version is part of the overlay
// identity, and the name has one length however many producers land.
#[test]
fn the_recipe_names_every_producer_rule_within_the_name_bound() {
    let name = recipe_name();
    assert!(
        name.starts_with(&format!("{RECIPE};producers@blake3:")),
        "{name}"
    );
    assert_eq!(PRODUCER_RULES.len(), 7);
    // Every rule's version is in the identity: changing any one changes it.
    for at in 0..PRODUCER_RULES.len() {
        let mut moved = PRODUCER_RULES;
        moved[at].1 = "changed";
        assert_ne!(recipe_over(&moved), name, "{}", PRODUCER_RULES[at].0);
    }
    // So is the order.
    let mut reordered = PRODUCER_RULES;
    reordered.swap(0, 1);
    assert_ne!(recipe_over(&reordered), name);
    // A hundred producers name the same length as seven.
    let many: Vec<(String, String)> = (0..100)
        .map(|n| (format!("knowledge.producer-{n}"), "1".to_owned()))
        .collect();
    let many: Vec<(&str, &str)> = many.iter().map(|(r, v)| (r.as_str(), v.as_str())).collect();
    assert_eq!(recipe_over(&many).len(), name.len());
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
fn the_worker_runs_its_pass_and_every_owed_pass() {
    let worker = Coalescer::new();
    assert!(worker.admit());
    let mut passes = 0;
    drain(&worker, || {
        passes += 1;
        if passes == 1 {
            assert!(!worker.admit());
        }
    });
    assert_eq!(passes, 2);
    assert!(worker.admit());
}

// AC-0588 / #1782: with T2 off nothing is embedded and no provider is even
// asked for, the report names that reason, and the deterministic producers
// still publish.
#[test]
fn with_t2_off_the_producers_publish_and_the_report_names_the_tier() {
    let host = host();
    put_resolver(&host.graph);
    let report = run_for_setting(&host.graph, &host.cache, &host.overlay, false, || {
        panic!("no provider is probed while T2 is off")
    })
    .unwrap();
    assert_eq!(report.omission, Some(SEMANTIC_DISABLED));
    assert_eq!(report.embedded, 0);
    assert!(report.published);
    assert_eq!(report.derived, 3);
    assert_eq!(published_facts(&host).len(), 3);

    // With T2 on and no provider installed the reason is the provider's.
    let enabled = run_for_setting(&host.graph, &host.cache, &host.overlay, true, || None).unwrap();
    assert_eq!(enabled.omission, Some(knowledge_ingest::NO_PROVIDER));
}

// AC-0588 / #1782: the outcome the UI receives carries every named reason,
// and a failed phase is a message rather than silence.
#[test]
fn the_status_sent_to_the_ui_carries_the_named_reasons() {
    let host = host();
    let report = run_within(
        &host.graph,
        &host.cache,
        &host.overlay,
        None,
        SnapshotReadLimits {
            max_rows: 1,
            ..SnapshotReadLimits::default()
        },
        &no_sections,
    );
    let status = serde_json::to_value(KnowledgeStatus::from(report)).unwrap();
    assert_eq!(status["report"]["omission"], BASE_BOUNDS);
    assert_eq!(status["report"]["published"], false);
    assert!(status["report"]["producerOmissions"].is_array());
    assert!(status["report"]["derivedOmission"].is_null());
    assert!(status["error"].is_null());

    let failed =
        serde_json::to_value(KnowledgeStatus::from(Err("store closed".to_owned()))).unwrap();
    assert!(failed["report"].is_null());
    assert_eq!(failed["error"], "store closed");
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

// AC-0532: a successful full-set census is obtained without another overlay read;
// all four node categories are counted, while MAPS_TO is a separate edge fact.
#[test]
fn published_producer_node_census_counts_all_four_and_skips_edges() {
    let host = host();
    put_permission_resolver(&host.graph);
    put_entity(&host.graph);
    let report = run(&host.graph, &host.cache, &host.overlay, None).unwrap();
    assert!(report.published);
    assert_eq!(report.derived_omission, None);
    assert_eq!(report.derived, 5);
    assert_eq!(
        report.published_producer_nodes,
        ProducerNodeCounts {
            capabilities: 1,
            business_flows: 1,
            domain_entities: 1,
            actors: 1,
        }
    );
    // Even test-build report serialization retains the existing production schema.
    let wire = serde_json::to_value(&report).unwrap();
    assert!(wire.get("publishedProducerNodes").is_none());
    assert!(wire.get("published_producer_nodes").is_none());
}

// AC-1202/T-1202: the phase reads its base streamed, in the materialized
// read's order and revision, with BusinessRule props left out; every other
// row is unchanged and the producers derive the identical facts from it.
// AC-1209/T-1209: each fact's `prov` is held as its text, and restored it is
// the stored `prov`.
#[test]
fn the_phase_base_leaves_out_only_props_no_producer_reads() {
    let host = host();
    put_resolver(&host.graph);
    put_entity(&host.graph);
    {
        let mut graph = host.graph.lock().unwrap();
        let symbol = graph.nodes_with_label("Symbol").unwrap()[0].id.clone();
        graph
            .put_node(&Node {
                id: "rule:refund-window".into(),
                label: "BusinessRule".into(),
                props: serde_json::json!({
                    "name": "Refunds close after 30 days",
                    "text": "x".repeat(4096),
                }),
            })
            .unwrap();
        for (dst, label) in [(symbol.as_str(), "GOVERNS"), ("gap:refund", "DEPENDS_ON")] {
            if label == "DEPENDS_ON" {
                graph
                    .put_node(&Node {
                        id: dst.into(),
                        label: "Gap".into(),
                        props: serde_json::json!({ "reason": "unresolved" }),
                    })
                    .unwrap();
            }
            graph
                .put_edge(&core_graph::Edge {
                    src: "rule:refund-window".into(),
                    dst: dst.into(),
                    label: label.into(),
                    props: serde_json::json!({ "rule_evidence_gap": true }),
                })
                .unwrap();
        }
    }
    let graph = host.graph.lock().unwrap();
    let full = graph
        .read_snapshot_with_revision_bounded(PHASE_LIMITS)
        .unwrap();
    let base = read_base(&graph, PHASE_LIMITS).unwrap();
    drop(graph);
    assert_eq!(base.revision, full.revision);
    let held =
        |props: &serde_json::Value| core_graph::stored::compact_text(&props["prov"]).is_some();
    let restored = |props: &serde_json::Value| {
        let mut props = props.clone();
        core_graph::stored::restore_prov(&mut props);
        props
    };
    assert_eq!(base.graph.1.len(), full.graph.1.len());
    for (read, stored) in base.graph.1.iter().zip(&full.graph.1) {
        assert_eq!(
            (&read.src, &read.label, &read.dst),
            (&stored.src, &stored.label, &stored.dst)
        );
        assert_eq!(held(&read.props), stored.props["prov"].is_object());
        assert_eq!(restored(&read.props), stored.props);
    }
    assert_eq!(base.graph.0.len(), full.graph.0.len());
    for (read, stored) in base.graph.0.iter().zip(&full.graph.0) {
        assert_eq!((&read.id, &read.label), (&stored.id, &stored.label));
        if stored.label == "BusinessRule" {
            assert_eq!(read.props, serde_json::json!({}));
        } else {
            assert_eq!(held(&read.props), stored.props["prov"].is_object());
            assert_eq!(restored(&read.props), stored.props);
        }
    }
    assert!(base.graph.0.iter().any(|node| held(&node.props)));
    let facts =
        |snapshot: &RevisionedSnapshot| serde_json::to_string(&derive(snapshot).facts).unwrap();
    let derived = facts(&full);
    assert!(derived.contains("Capability") && derived.contains("DomainEntity"));
    assert_eq!(facts(&base), derived);
}

struct Recording(Mutex<Vec<String>>);
impl LlmProvider for Recording {
    fn id(&self) -> &str {
        "recording"
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
        Fixture { fail: false }.embedding_cache_identity()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.0.lock().unwrap().extend_from_slice(texts);
        Ok(texts.iter().map(|t| vec![1.0, t.len() as f32]).collect())
    }
}

// AC-1505: a documentation section is embedded with its text, read from the
// registered source's span only while it still hashes to the section's
// provenance (ADR-0065 keeps the text out of the graph); a section whose
// file moved since ingest is embedded by its heading alone and counted.
#[test]
fn sections_are_embedded_with_text_read_from_their_source() {
    let host = host();
    let repo = tempfile::tempdir().unwrap();
    // A run of whitespace past the text bound never crowds out the prose
    // after it: the bound applies to the collapsed text.
    let readme = format!(
        "# Shop\n\nIntro.\n\n## Refunds\n\n{}Refunds settle nightly.\n",
        " ".repeat(5000)
    );
    std::fs::write(repo.path().join("README.md"), &readme).unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut registry =
        crate::sources::SourceRegistry::open(data.path().join("sources.db"), data.path()).unwrap();
    let source = registry.register_local(repo.path()).unwrap();
    let registry = Mutex::new(registry);
    let facts = spec::extract_documents(repo.path(), &source.repo_key, "workdir").unwrap();
    assert_eq!(facts.counts.sections, 2);
    {
        let mut graph = host.graph.lock().unwrap();
        for node in &facts.nodes {
            graph.put_node(node).unwrap();
        }
    }
    let provider = Recording(Mutex::new(Vec::new()));
    let reader = |sections: &[&Node]| source_sections(&registry, sections);
    let report = run_with_sections(
        &host.graph,
        &host.cache,
        &host.overlay,
        Some(&provider),
        &reader,
    )
    .unwrap();
    assert_eq!((report.sections_read, report.sections_unread), (2, 0));
    let sent = provider.0.lock().unwrap().clone();
    let refunds = sent
        .iter()
        .find(|text| text.contains("heading: Refunds"))
        .expect("the Refunds section is a unit");
    assert!(
        refunds.contains("text: ## Refunds Refunds settle nightly."),
        "{refunds}"
    );

    // The file changed after ingest, keeping its length: both sections'
    // spans (the title section holds the Refunds one) still read, but no
    // longer hash to the graph.
    std::fs::write(
        repo.path().join("README.md"),
        readme.replace("nightly", "Weekly!"),
    )
    .unwrap();
    let provider = Recording(Mutex::new(Vec::new()));
    let report = run_with_sections(
        &host.graph,
        &host.cache,
        &host.overlay,
        Some(&provider),
        &reader,
    )
    .unwrap();
    assert_eq!((report.sections_read, report.sections_unread), (0, 2));
    let sent = provider.0.lock().unwrap().clone();
    assert!(sent.iter().all(|text| !text.contains("Weekly")), "{sent:?}");
    // Without a provider nothing is read and nothing is counted.
    let report = run_with_sections(&host.graph, &host.cache, &host.overlay, None, &reader).unwrap();
    assert_eq!((report.sections_read, report.sections_unread), (0, 0));
}
