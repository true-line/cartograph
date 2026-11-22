//! #1443 deterministic knowledge producer tests; no live ingestion.
// Capability and DomainEntity candidates consume existing base extractor facts.
use super::*;
use core_graph::StoreRevision;
const PATH: &str = "packages/core/src/api/resolvers/shop/shop-order.resolver.ts";
const REPO: &str = "vendurehq/vendure";
const SOURCE: &[u8] = br#"
import {Resolver, Query, Mutation} from '@nestjs/graphql';
@Resolver()
export class ShopOrderResolver {
  @Query() order() {}
  @Mutation() addItemToOrder() {}
}
"#;
fn fixture(path: &str, repo: &str) -> RevisionedSnapshot {
    fixture_source(SOURCE, path, repo)
}
fn fixture_source(source: &[u8], path: &str, repo: &str) -> RevisionedSnapshot {
    let facts = adapters_lang_ts::extract_source(
        source,
        path,
        &adapters_lang_ts::SourceId {
            repo,
            commit: "2cc309deb5394af176314552ac7320a7b26a0104",
        },
    )
    .unwrap();
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (facts.nodes, facts.edges),
    }
}
fn capability(fact: &OverlayFact) -> &Node {
    let OverlayItem::Node(node) = &fact.item else {
        panic!("Capability is a node")
    };
    node
}

fn jpa_entity_fixture() -> RevisionedSnapshot {
    let source = br#"package com.demo;
import jakarta.persistence.Entity;
import jakarta.persistence.Id;
import jakarta.persistence.Table;
@Entity
@Table(name = "orders")
public class Order {
    @Id private Integer id;
    private String status;
}
"#;
    let extraction = adapters_lang_java::extract_source(
        source,
        "src/main/java/com/demo/Order.java",
        &adapters_lang_java::SourceId {
            repo: "local/demo",
            commit: "fixture-commit",
        },
    )
    .unwrap();
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (extraction.nodes, extraction.edges),
    }
}

fn sqlalchemy_entity_fixture() -> RevisionedSnapshot {
    // Exact `DataEntity` shape and provenance construction from
    // adapters-lang-python's SQLAlchemy resolver and users-model fixture.
    let id = "data:local/python-service@sqlalchemy:users";
    let repo = "local/python-service";
    let commit = "abc123";
    let evidence = [
        ("app/models.py", 10, 20),
        ("app/models.py", 30, 40),
        ("app/models.py", 50, 60),
        ("app/models.py", 70, 80),
        ("app/mixins.py", 90, 100),
    ]
    .into_iter()
    .map(|(path, byte_start, byte_end)| EvidenceRef {
        repo: repo.into(),
        path: path.into(),
        byte_start,
        byte_end,
        commit_sha: commit.into(),
    })
    .collect();
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        evidence,
        "t0.adapter-python",
        format!("DataEntity {id}").as_bytes(),
    )
    .unwrap();
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture-python".into(),
            generation: 1,
        },
        graph: (
            vec![Node {
                id: id.into(),
                label: "DataEntity".into(),
                props: serde_json::json!({
                    "name": "User",
                    "storage": "sqlalchemy",
                    "store": "users",
                    "table": "users",
                    "columns": [
                        { "name": "id", "type": "Integer", "primary_key": true },
                        { "name": "email", "type": "String", "nullable": false },
                        { "name": "active", "type": "Boolean" },
                        { "name": "created_at", "type": "DateTime", "nullable": false }
                    ],
                    "framework": "sqlalchemy",
                    "language": "python",
                    "prov": provenance
                }),
            }],
            Vec::new(),
        ),
    }
}

/// A GORM model as the Go adapter recovers it (#2019).
fn gorm_entity_fixture() -> RevisionedSnapshot {
    let mut snapshot = sqlalchemy_entity_fixture();
    let node = &mut snapshot.graph.0[0];
    let evidence = vec![EvidenceRef {
        repo: "local/go".into(),
        path: "users/models.go".into(),
        byte_start: 10,
        byte_end: 200,
        commit_sha: "a".repeat(40),
    }];
    node.id = "data:local/go@gorm:users#UserModel".into();
    node.props["name"] = "UserModel".into();
    node.props["storage"] = "gorm".into();
    node.props["framework"] = "gorm".into();
    node.props["language"] = "go".into();
    node.props["table_source"] = "default".into();
    node.props["table"] = "user_models".into();
    node.props["store"] = "user_models".into();
    node.props["prov"] = serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            evidence,
            "t0.adapter-go",
            format!("DataEntity {}", node.id).as_bytes(),
        )
        .unwrap(),
    )
    .unwrap();
    snapshot
}

// AC-0511: ORM-derived candidates and mappings stay cited in the overlay; malformed and oversized pairs fail closed.
#[test]
fn orm_domain_entities_map_real_jpa_facts_in_the_overlay_only() {
    use core_graph::overlay::{OverlayRead, OverlayRecipe, OverlayStore};

    let base = jpa_entity_fixture();
    let before = base.clone();
    let data = base
        .graph
        .0
        .iter()
        .find(|node| node.label == "DataEntity")
        .expect("real JPA adapter emits the persistence fact");
    assert_eq!(data.id, "data:local/demo@jpa:Order");

    let report = derive_orm_domain_entities(&base);
    assert_eq!(report.oversized_models, 0);
    let facts = report.facts.clone();
    assert_eq!(facts.len(), 2); // candidate plus the spec-required MAPS_TO
    let candidate = facts
        .iter()
        .find_map(|fact| match &fact.item {
            OverlayItem::Node(node) if node.label == "DomainEntity" => Some(node),
            _ => None,
        })
        .unwrap();
    let mapping = facts
        .iter()
        .find_map(|fact| match &fact.item {
            OverlayItem::Edge(edge) if edge.label == "MAPS_TO" => Some(edge),
            _ => None,
        })
        .unwrap();
    assert_eq!(candidate.props["name"], "Order"); // display only
    assert_eq!(candidate.props["backing_data_entity_id"], data.id);
    assert_eq!(mapping.src, candidate.id);
    assert_eq!(mapping.dst, data.id);
    for fact in &facts {
        assert_eq!(fact.cites, BTreeSet::from([BaseRef::Node(data.id.clone())]));
        let props = match &fact.item {
            OverlayItem::Node(node) => &node.props,
            OverlayItem::Edge(edge) => &edge.props,
        };
        let provenance: Provenance = serde_json::from_value(props["prov"].clone()).unwrap();
        assert_eq!(provenance.tier, Tier::Semantic);
        assert_eq!(provenance.confidence_tier, ConfidenceTier::InferredWeak);
        assert_eq!(
            provenance.method,
            ProducingMethod::Rule {
                rule: DOMAIN_ENTITY_ORM_RULE.into(),
                version: DOMAIN_ENTITY_ORM_RULE_VERSION.into(),
            }
        );
        assert_eq!(
            provenance.evidence,
            serde_json::from_value::<Provenance>(data.props["prov"].clone())
                .unwrap()
                .evidence
        );
        assert!(provenance.validate().is_ok());
    }

    let mut reversed = base.clone();
    reversed.graph.0.reverse();
    reversed.graph.1.reverse();
    assert_eq!(derive_orm_domain_entities(&reversed), report);
    assert_eq!(base, before);

    let mut overlay = OverlayStore::open_in_memory().unwrap();
    overlay.bind_instance("fixture").unwrap();
    let identity = overlay
        .publish(
            &base,
            OverlayRecipe {
                recipe: DOMAIN_ENTITY_ORM_RULE.into(),
                embedding_namespace: None,
                provider_ran: false,
            },
            facts.clone(),
        )
        .unwrap();
    match overlay.read(&base.revision).unwrap() {
        OverlayRead::Current {
            identity: read_identity,
            facts: read_facts,
            ..
        } => {
            assert_eq!(read_identity, identity);
            let key = |fact: &OverlayFact| match &fact.item {
                OverlayItem::Node(node) => format!("n\0{}", node.id),
                OverlayItem::Edge(edge) => format!("e\0{}\0{}\0{}", edge.src, edge.dst, edge.label),
            };
            let expected: BTreeMap<_, _> = facts.iter().map(|fact| (key(fact), fact)).collect();
            let actual: BTreeMap<_, _> = read_facts.iter().map(|fact| (key(fact), fact)).collect();
            assert_eq!(actual, expected);
        }
        other => panic!("published overlay must read current: {other:?}"),
    }
    assert_eq!(base, before); // candidate and mapping never enter the base graph
}

// AC-0511: SQLAlchemy-shaped persistence facts are accepted only with their matching Python extractor provenance.
#[test]
fn orm_domain_entities_accept_sqlalchemy_shape_and_reject_wrong_extractor() {
    let base = sqlalchemy_entity_fixture();
    let report = derive_orm_domain_entities(&base);
    assert_eq!(report.oversized_models, 0);
    assert_eq!(report.facts.len(), 2);
    let candidate = report
        .facts
        .iter()
        .find_map(|fact| match &fact.item {
            OverlayItem::Node(node) if node.label == "DomainEntity" => Some(node),
            _ => None,
        })
        .unwrap();
    assert_eq!(candidate.props["name"], "User");
    assert_eq!(candidate.props["storage"], "sqlalchemy");
    assert_eq!(candidate.props["table"], "users");

    let mut wrong_extractor = base;
    wrong_extractor.graph.0[0].props["prov"]["extractor_id"] = "t0.adapter-java".into();
    assert!(
        derive_orm_domain_entities(&wrong_extractor)
            .facts
            .is_empty()
    );
}

// AC-0511: oversized candidate/mapping pairs are omitted together and counted.
#[test]
fn orm_domain_entities_omit_oversized_candidate_and_mapping_together() {
    let mut base = jpa_entity_fixture();
    let data = base
        .graph
        .0
        .iter_mut()
        .find(|node| node.label == "DataEntity")
        .unwrap();
    data.props["name"] = serde_json::json!("x".repeat(MAX_FACT_BYTES));
    let report = derive_orm_domain_entities(&base);
    assert!(report.facts.is_empty());
    assert_eq!(report.oversized_models, 1);
}

// AC-0511: malformed, weak, incomplete and ambiguous ORM evidence is rejected.
#[test]
fn orm_domain_entities_reject_unproven_or_ambiguous_models() {
    let original = jpa_entity_fixture();
    assert_eq!(derive_orm_domain_entities(&original).facts.len(), 2);
    let data_index = original
        .graph
        .0
        .iter()
        .position(|node| node.label == "DataEntity")
        .unwrap();
    for case in [
        "missing-provenance",
        "malformed-provenance",
        "weak-provenance",
        "inverted-evidence-span",
        "missing-table",
        "missing-table-source",
        "invalid-table-source",
        "unresolved-table",
        "empty-fields",
        "unsupported-storage",
        "ambiguous-id",
    ] {
        let mut base = original.clone();
        if case == "ambiguous-id" {
            let duplicate = base.graph.0[data_index].clone();
            base.graph.0.push(duplicate);
        } else {
            let node = &mut base.graph.0[data_index];
            match case {
                "missing-provenance" => {
                    node.props.as_object_mut().unwrap().remove("prov");
                }
                "malformed-provenance" => {
                    node.props["prov"] = serde_json::json!({"tier":"not-a-tier"})
                }
                "weak-provenance" => node.props["prov"]["confidence_tier"] = "InferredWeak".into(),
                "inverted-evidence-span" => {
                    let end = node.props["prov"]["evidence"][0]["byte_end"]
                        .as_u64()
                        .unwrap();
                    node.props["prov"]["evidence"][0]["byte_start"] = serde_json::json!(end + 1);
                }
                "missing-table" => {
                    node.props.as_object_mut().unwrap().remove("table");
                }
                "missing-table-source" => {
                    node.props.as_object_mut().unwrap().remove("table_source");
                }
                "invalid-table-source" => node.props["table_source"] = "guessed".into(),
                "unresolved-table" => node.props["table_source"] = "unresolved".into(),
                "empty-fields" => node.props["fields"] = serde_json::json!([]),
                "unsupported-storage" => node.props["storage"] = "unknown".into(),
                _ => unreachable!(),
            }
        }
        if case == "weak-provenance" || case == "inverted-evidence-span" {
            let provenance: Provenance =
                serde_json::from_value(base.graph.0[data_index].props["prov"].clone()).unwrap();
            assert!(
                provenance.validate().is_ok(),
                "{case} stays structurally valid"
            );
        }
        assert!(derive_orm_domain_entities(&base).facts.is_empty(), "{case}");
    }
    let mut no_structural_fact = original;
    no_structural_fact
        .graph
        .0
        .retain(|node| node.label != "DataEntity");
    assert!(
        derive_orm_domain_entities(&no_structural_fact)
            .facts
            .is_empty()
    );
}

// AC-0511: ORM source evidence cannot be relabelled from Artifact or Observation.
#[test]
fn orm_domain_entities_require_source_basis_on_model_support() {
    let artifact = serde_json::json!({"kind": "artifact", "refs": [{
        "digest": format!("sha256:{}", "a".repeat(64)),
        "kind": "orm-schema",
        "at": {"offset": {"start": 0, "end": 4}}
    }]});
    for (storage, original) in [
        ("jpa", jpa_entity_fixture()),
        ("sqlalchemy", sqlalchemy_entity_fixture()),
        ("gorm", gorm_entity_fixture()),
    ] {
        let expected = derive_orm_domain_entities(&original);
        assert_eq!(expected.facts.len(), 2); // Implicit source basis remains admitted.
        for (basis, admitted) in [
            (serde_json::json!({"kind": "source"}), true),
            (artifact.clone(), false),
            (serde_json::json!({"kind": "observation"}), false),
        ] {
            let mut base = original.clone();
            let model = base
                .graph
                .0
                .iter_mut()
                .find(|n| n.label == "DataEntity")
                .unwrap();
            model.props["prov"]["basis"] = basis.clone();
            let prov: Provenance = serde_json::from_value(model.props["prov"].clone()).unwrap();
            assert!(
                prov.validate().is_ok(),
                "{storage} {basis} is otherwise valid"
            );
            let before = base.clone();
            let report = derive_orm_domain_entities(&base);
            if admitted {
                assert_eq!(
                    report, expected,
                    "{storage} explicit source preserves the pair"
                );
            } else {
                assert!(report.facts.is_empty(), "{storage} {basis}");
                assert_eq!(report.oversized_models, 0);
            }
            assert_eq!(base, before);
        }
    }
}

// AC-0500: real adapter pairs retain all support and weak provenance.
#[test]
fn graphql_capability_groups_real_adapter_pairs_and_cites_every_support() {
    let base = fixture(PATH, REPO);
    let before = base.clone();
    let report = derive_graphql_resolver_capabilities(&base);
    assert_eq!(report.oversized_groups, 0);
    let facts = report.facts;
    assert_eq!(base, before);
    assert_eq!(facts.len(), 1);
    let node = capability(&facts[0]);
    assert_eq!(node.label, "Capability");
    assert_eq!(node.props["title"], "Manage orders (shop API)");
    assert_eq!(node.props.as_object().unwrap().len(), 2); // no auth/business assertions
    let expected = [
        BaseRef::Node(format!("ep:{REPO}@graphql:shop:query:order")),
        BaseRef::Node(format!("ep:{REPO}@graphql:shop:mutation:addItemToOrder")),
        BaseRef::Node(format!("sym:{REPO}@{PATH}#ShopOrderResolver.order")),
        BaseRef::Node(format!(
            "sym:{REPO}@{PATH}#ShopOrderResolver.addItemToOrder"
        )),
        BaseRef::Edge(
            format!("ep:{REPO}@graphql:shop:query:order"),
            format!("sym:{REPO}@{PATH}#ShopOrderResolver.order"),
            "HANDLES".into(),
        ),
        BaseRef::Edge(
            format!("ep:{REPO}@graphql:shop:mutation:addItemToOrder"),
            format!("sym:{REPO}@{PATH}#ShopOrderResolver.addItemToOrder"),
            "HANDLES".into(),
        ),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(facts[0].cites, expected);
    let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    assert_eq!(prov.tier, Tier::Semantic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::InferredWeak);
    assert_eq!(
        prov.method,
        ProducingMethod::Rule {
            rule: CAPABILITY_GRAPHQL_RULE.into(),
            version: "3".into()
        }
    );
    let mut expected_evidence = BTreeMap::new();
    for cite in &expected {
        let props = match cite {
            BaseRef::Node(id) => &base.graph.0.iter().find(|n| n.id == *id).unwrap().props,
            BaseRef::Edge(src, dst, label) => {
                &base
                    .graph
                    .1
                    .iter()
                    .find(|e| e.src == *src && e.dst == *dst && e.label == *label)
                    .unwrap()
                    .props
            }
        };
        for e in support(props).unwrap().evidence {
            expected_evidence.insert(
                (
                    e.repo.clone(),
                    e.path.clone(),
                    e.byte_start,
                    e.byte_end,
                    e.commit_sha.clone(),
                ),
                e,
            );
        }
    }
    assert_eq!(
        prov.evidence,
        expected_evidence.into_values().collect::<Vec<_>>()
    );
    assert!(prov.validate().is_ok());
}
// AC-0500: permutations preserve candidate identity and leave base facts unchanged.
#[test]
fn graphql_capability_identity_order_and_base_are_permutation_independent() {
    let mut base = fixture(PATH, REPO);
    let admin = fixture(&PATH.replace("/shop/", "/admin/"), REPO);
    let foreign_repo = fixture(PATH, "other/vendure");
    base.graph.0.extend(admin.graph.0);
    base.graph.1.extend(admin.graph.1);
    base.graph.0.extend(foreign_repo.graph.0);
    base.graph.1.extend(foreign_repo.graph.1);
    let expected = derive_graphql_resolver_capabilities(&base);
    assert_eq!(expected.facts.len(), 3); // same basename never merges API/repo ownership
    assert_eq!(
        expected
            .facts
            .iter()
            .map(|f| &capability(f).id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    let before = base.clone();
    for _ in 0..base.graph.0.len() {
        base.graph.0.rotate_left(1);
        base.graph.1.reverse();
        let input = base.clone();
        assert_eq!(derive_graphql_resolver_capabilities(&base), expected);
        assert_eq!(base, input);
    }
    assert_eq!(base.revision, before.revision);
}

// AC-0500: retain adapter-emitted API scope text while rejecting unsafe path segments.
#[test]
fn graphql_capability_accepts_adapter_emitted_hyphenated_api_directory() {
    let path = "packages/core/src/api/resolvers/admin-api/shop-order.resolver.ts";
    let base = fixture(path, REPO);
    let before = base.clone();
    let endpoint = base
        .graph
        .0
        .iter()
        .find(|node| node.label == "Endpoint")
        .unwrap();
    assert_eq!(endpoint.props["api"], "admin-api");
    assert_eq!(endpoint.props["scope_basis"], "resolver_directory");
    assert_eq!(endpoint.props["path"], "admin-api.order");

    let report = derive_graphql_resolver_capabilities(&base);
    assert_eq!(report.oversized_groups, 0);
    assert_eq!(report.facts.len(), 1);
    assert_eq!(
        capability(&report.facts[0]).props["title"],
        "Manage orders (admin API)"
    );
    assert_eq!(base, before);

    // API names remain path segments: punctuation is accepted, but unsafe
    // segment structure is still rejected by the existing path guard.
    assert!(scoped_path(path, "admin-api"));
    assert!(!scoped_path(
        "packages/core/src/api/resolvers/../shop-order.resolver.ts",
        ".."
    ));
    assert!(!scoped_path(
        "packages/core/src/api/resolvers/admin\\api/shop-order.resolver.ts",
        "admin\\api"
    ));
    assert!(!scoped_path(
        "packages/core/src/api/resolvers/admin-api//shop-order.resolver.ts",
        "admin-api"
    ));
}

// AC-0500: TS identity text is adapter-proven; GraphQL field grammar stays separate.
#[test]
fn graphql_capability_accepts_adapter_dollar_and_unicode_identity_names() {
    for (class, member) in [
        ("$OrdersResolver", "order"),
        ("注文Resolver", "order"),
        ("$OrdersResolver", "$order"),
        ("注文Resolver", "注文"),
    ] {
        let source = format!(
            "import {{Resolver, Query}} from '@nestjs/graphql'; @Resolver() export class {class} {{ @Query('order') {member}() {{}} }}"
        );
        let base = fixture_source(source.as_bytes(), PATH, REPO);
        let before = base.clone();
        let handler = base
            .graph
            .0
            .iter()
            .find(|n| n.label == "Symbol" && n.props["kind"] == "Method")
            .unwrap();
        assert_eq!(handler.props["class"], class);
        assert_eq!(handler.props["name"], format!("{class}.{member}"));
        assert_eq!(handler.id, format!("sym:{REPO}@{PATH}#{class}.{member}"));
        let report = derive_graphql_resolver_capabilities(&base);
        assert_eq!(report.facts.len(), 1, "{class}.{member}");
        assert!(
            report.facts[0]
                .cites
                .contains(&BaseRef::Node(handler.id.clone()))
        );
        assert_eq!(
            capability(&report.facts[0]).props["title"],
            format!(
                "View {} (shop API)",
                class
                    .trim_start_matches('$')
                    .trim_end_matches("Resolver")
                    .to_lowercase()
            )
        );
        assert_eq!(base, before);
    }
    for field in ["$order", "注文", "order-bad", "../order"] {
        let source = format!(
            "import {{Resolver, Query}} from '@nestjs/graphql'; @Resolver() class $OrdersResolver {{ @Query('{field}') order() {{}} }}"
        );
        assert!(
            derive_graphql_resolver_capabilities(&fixture_source(source.as_bytes(), PATH, REPO))
                .facts
                .is_empty(),
            "invalid GraphQL field {field}"
        );
    }
}

// AC-0500: incomplete, ambiguous and foreign support cannot form a candidate.
#[test]
fn graphql_capability_excludes_missing_ambiguous_gap_and_foreign_ownership() {
    for case in [
        "missing-edge",
        "missing-symbol",
        "ambiguous",
        "duplicate-symbol",
        "missing-class",
        "wrong-class",
        "foreign-owner",
        "unscoped",
        "foreign-api",
        "http",
        "gap-endpoint",
        "gap-edge",
        "gap-handler",
    ] {
        let mut base = fixture(PATH, REPO);
        let ep = format!("ep:{REPO}@graphql:shop:query:order");
        let handler = format!("sym:{REPO}@{PATH}#ShopOrderResolver.order");
        base.graph.0.retain(|n| n.label != "Endpoint" || n.id == ep);
        base.graph.1.retain(|e| e.label != "HANDLES" || e.src == ep);
        assert_eq!(
            derive_graphql_resolver_capabilities(&base).facts.len(),
            1,
            "{case} positive"
        );
        let endpoint_at = base.graph.0.iter().position(|n| n.id == ep).unwrap();
        let handler_at = base.graph.0.iter().position(|n| n.id == handler).unwrap();
        let edge_at = base
            .graph
            .1
            .iter()
            .position(|e| e.label == "HANDLES")
            .unwrap();
        match case {
            "missing-edge" => {
                base.graph.1.remove(edge_at);
            }
            "missing-symbol" => {
                base.graph.0.remove(handler_at);
            }
            "ambiguous" => {
                let mut extra = base.graph.1[edge_at].clone();
                extra.dst = format!("sym:{REPO}@{PATH}#ShopOrderResolver.addItemToOrder");
                base.graph.1.push(extra);
            }
            "duplicate-symbol" => {
                base.graph.0.push(base.graph.0[handler_at].clone());
            }
            "missing-class" => {
                base.graph.0[handler_at]
                    .props
                    .as_object_mut()
                    .unwrap()
                    .remove("class");
            }
            "wrong-class" => base.graph.0[handler_at].props["class"] = "AnotherResolver".into(),
            "foreign-owner" => {
                base.graph.0[handler_at].props["prov"]["evidence"][0]["repo"] =
                    "foreign/repo".into()
            }
            "unscoped" => base.graph.0[endpoint_at].props["scope_basis"] = "none".into(),
            "foreign-api" => {
                let node = &mut base.graph.0[endpoint_at];
                node.id = format!("ep:{REPO}@graphql:admin:query:order");
                node.props["api"] = "admin".into();
                node.props["path"] = "admin.order".into();
                base.graph.1[edge_at].src = node.id.clone();
            }
            "http" => base.graph.0[endpoint_at].props["protocol"] = "http".into(),
            "gap-endpoint" => {
                base.graph.0[endpoint_at].props["prov"]["confidence_tier"] = "Gap".into()
            }
            "gap-edge" => base.graph.1[edge_at].props["prov"]["confidence_tier"] = "Gap".into(),
            "gap-handler" => {
                base.graph.0[handler_at].props["prov"]["confidence_tier"] = "Gap".into()
            }
            _ => unreachable!(),
        }
        assert!(
            derive_graphql_resolver_capabilities(&base).facts.is_empty(),
            "{case}"
        );
    }
}

// AC-0519: only source-basis support forms a candidate; the derived fact
// defaults to Source, so artifact or observation support would be relabelled.
#[test]
fn graphql_capability_refuses_non_source_basis_support() {
    let artifact = serde_json::json!({"kind": "artifact", "refs": [{
        "digest": format!("sha256:{}", "a".repeat(64)),
        "kind": "js-bundle",
        "at": {"offset": {"start": 0, "end": 4}}
    }]});
    for target in ["endpoint", "edge", "handler"] {
        for (basis, admitted) in [
            (serde_json::json!({"kind": "source"}), true),
            (artifact.clone(), false),
            (serde_json::json!({"kind": "observation"}), false),
        ] {
            let mut base = fixture(PATH, REPO);
            let ep = format!("ep:{REPO}@graphql:shop:query:order");
            let handler = format!("sym:{REPO}@{PATH}#ShopOrderResolver.order");
            base.graph.0.retain(|n| n.label != "Endpoint" || n.id == ep);
            base.graph.1.retain(|e| e.label != "HANDLES" || e.src == ep);
            let props = match target {
                "endpoint" => &mut base.graph.0.iter_mut().find(|n| n.id == ep).unwrap().props,
                "handler" => {
                    &mut base
                        .graph
                        .0
                        .iter_mut()
                        .find(|n| n.id == handler)
                        .unwrap()
                        .props
                }
                _ => {
                    &mut base
                        .graph
                        .1
                        .iter_mut()
                        .find(|e| e.label == "HANDLES")
                        .unwrap()
                        .props
                }
            };
            props["prov"]["basis"] = basis.clone();
            let prov: Provenance = serde_json::from_value(props["prov"].clone()).unwrap();
            assert!(
                prov.validate().is_ok(),
                "{target} {basis} is otherwise valid"
            );
            assert_eq!(
                derive_graphql_resolver_capabilities(&base).facts.len(),
                usize::from(admitted),
                "{target} {basis}"
            );
        }
    }
}

// AC-0500: oversized groups are omitted while ordinary groups remain.
#[test]
fn graphql_capability_oversized_group_is_counted_without_losing_ordinary_group() {
    let mut base = fixture(PATH, REPO);
    let ordinary = derive_graphql_resolver_capabilities(&base);
    // Long but valid components reduce method count without weakening the
    // actual-adapter or serialized-citation lower bound below.
    let nested = std::iter::repeat_n("sizing_".repeat(32), 14)
        .collect::<Vec<_>>()
        .join("/");
    let path = format!("packages/core/src/api/resolvers/admin/{nested}/large.resolver.ts");
    assert!(path.len() < 4096);
    assert!(path.split('/').all(|component| component.len() < 255));
    // Each field cites its handler ID twice (node + edge), both containing
    // this path. Citations alone therefore exceed the existing fact cap.
    let fields = MAX_FACT_BYTES / (2 * path.len()) + 1;
    let mut source = String::from(
        "import {Resolver, Query} from '@nestjs/graphql'; @Resolver() export class LargeResolver {",
    );
    for i in 0..fields {
        source.push_str(&format!("@Query() field_{i}() {{}}\n"));
    }
    source.push('}');
    let oversized = fixture_source(source.as_bytes(), &path, REPO);
    base.graph.0.extend(oversized.graph.0);
    base.graph.1.extend(oversized.graph.1);
    let before = base.clone();
    let report = derive_graphql_resolver_capabilities(&base);
    assert_eq!(report.oversized_groups, 1);
    assert_eq!(report.facts, ordinary.facts);
    assert_eq!(base, before);
    assert!(
        report
            .facts
            .iter()
            .all(|fact| serde_json::to_string(fact).unwrap().len() <= MAX_FACT_BYTES)
    );
}

// AC-0500: serialized fact admission accounts for exact boundaries and escaping.
#[test]
fn capability_fact_size_guard_uses_exact_overlay_boundary_and_json_escaping() {
    // Representation guard only: start with a real candidate, then size its
    // title to the exact boundary. This is not a new semantic/provenance claim.
    let mut exact = derive_graphql_resolver_capabilities(&fixture(PATH, REPO))
        .facts
        .remove(0);
    let padding = MAX_FACT_BYTES - serde_json::to_string(&exact).unwrap().len();
    let OverlayItem::Node(node) = &mut exact.item else {
        unreachable!()
    };
    node.props["title"] = format!(
        "{}{}",
        node.props["title"].as_str().unwrap(),
        "x".repeat(padding)
    )
    .into();
    assert_eq!(serde_json::to_string(&exact).unwrap().len(), MAX_FACT_BYTES);
    let mut escaped = exact.clone();
    let OverlayItem::Node(node) = &mut escaped.item else {
        unreachable!()
    };
    let mut title = node.props["title"].as_str().unwrap().to_owned();
    assert_eq!(title.pop(), Some('x'));
    title.push('"'); // one source byte, two JSON bytes: exactly one over
    node.props["title"] = title.into();
    assert_eq!(
        serde_json::to_string(&escaped).unwrap().len(),
        MAX_FACT_BYTES + 1
    );
    let mut report = CapabilityReport::default();
    retain_sized_fact(&mut report, exact);
    assert_eq!(report.facts.len(), 1);
    assert_eq!(report.oversized_groups, 0);
    retain_sized_fact(&mut report, escaped);
    assert_eq!(report.facts.len(), 1);
    assert_eq!(report.oversized_groups, 1);
}

#[test]
fn graphql_capability_roundtrips_overlay_store_with_exact_base_citations() {
    // AC-0500: the real overlay admission and persisted representation accept
    // the producer's actual facts, including every supporting base reference.
    use core_graph::overlay::{OverlayRead, OverlayRecipe, OverlayStore};
    let base = fixture(PATH, REPO);
    let before = base.clone();
    let report = derive_graphql_resolver_capabilities(&base);
    assert_eq!(report.oversized_groups, 0);
    assert_eq!(report.facts.len(), 1);
    let expected = report.facts.clone();
    assert_eq!(expected[0].cites.len(), 6);
    let mut store = OverlayStore::open_in_memory().unwrap();
    store.bind_instance(&base.revision.graph_instance).unwrap();
    let recipe = OverlayRecipe {
        recipe: format!("{CAPABILITY_GRAPHQL_RULE}@{CAPABILITY_GRAPHQL_RULE_VERSION}"),
        embedding_namespace: None,
        provider_ran: false,
    };
    let published = store.publish(&base, recipe.clone(), report.facts).unwrap();
    let OverlayRead::Current {
        identity,
        basis,
        facts,
    } = store.read(&base.revision).unwrap()
    else {
        panic!("published capabilities must read as Current");
    };
    assert_eq!(identity, published);
    assert_eq!(basis.base, base.revision);
    assert_eq!(basis.recipe, recipe);
    assert_eq!(facts, expected);
    assert_eq!(facts[0].cites, expected[0].cites);
    assert_eq!(base, before);
}
// AC-0504: compiler-only citations, strict input consistency, no host currency claim.
fn citation_limits(max_bytes: usize) -> crate::SelectedCompileLimits {
    crate::SelectedCompileLimits {
        max_bytes,
        adr: crate::AdrLimits {
            max_rows: usize::MAX,
            max_output_bytes: usize::MAX,
        },
    }
}
fn citation_compile(
    base: &RevisionedSnapshot,
    facts: &[OverlayFact],
    mode: crate::ExportMode,
    rejected: &BTreeSet<String>,
    max: usize,
) -> Result<crate::SelectedArtifactOutcome, crate::CapabilityStoryInputError> {
    let candidates: Vec<_> = facts
        .iter()
        .map(|fact| crate::CapabilityStory {
            capability: capability(fact),
            cites: &fact.cites,
        })
        .collect();
    crate::compile_capability_user_stories(base, &candidates, mode, rejected, citation_limits(max))
}
fn citation_artifact(outcome: crate::SelectedArtifactOutcome) -> crate::SpecArtifact {
    let crate::SelectedArtifactOutcome::Produced(artifact) = outcome else {
        panic!("complete artifact")
    };
    assert!(artifact.verify_identity());
    artifact
}
#[test]
fn capability_stories_roundtrip_current_overlay_with_per_capability_support() {
    use core_graph::overlay::{OverlayRead, OverlayRecipe, OverlayStore};
    let base = fixture(PATH, REPO);
    let before = base.clone();
    let mut facts = derive_graphql_resolver_capabilities(&base).facts;
    let mut second = facts[0].clone();
    let OverlayItem::Node(node) = &mut second.item else {
        unreachable!()
    };
    node.id.push_str(":second");
    facts.push(second);
    let mut store = OverlayStore::open_in_memory().unwrap();
    store.bind_instance(&base.revision.graph_instance).unwrap();
    store
        .publish(
            &base,
            OverlayRecipe {
                recipe: format!("{CAPABILITY_GRAPHQL_RULE}@{CAPABILITY_GRAPHQL_RULE_VERSION}"),
                embedding_namespace: None,
                provider_ran: false,
            },
            facts,
        )
        .unwrap();
    let OverlayRead::Current { mut facts, .. } = store.read(&base.revision).unwrap() else {
        panic!("Current")
    };
    let compile = |facts: &[OverlayFact]| {
        citation_artifact(
            citation_compile(
                &base,
                facts,
                crate::ExportMode::BestEffort,
                &BTreeSet::new(),
                usize::MAX,
            )
            .unwrap(),
        )
    };
    let artifact = compile(&facts);
    assert_eq!(artifact.assertions.len(), 14); // two capabilities, six support occurrences each
    let mut occurrence_ids = BTreeSet::new();
    for fact in &facts {
        for cite in &fact.cites {
            let mut hash = blake3::Hasher::new();
            hash.update(b"capability-story-support-v1\0");
            hash.update(&serde_json::to_vec(&(&capability(fact).id, cite)).unwrap());
            let id = format!("support:{}", hash.finalize().to_hex());
            assert!(occurrence_ids.insert(id.clone()));
            let assertion = artifact.assertions.iter().find(|a| a.id == id).unwrap();
            let props = match cite {
                BaseRef::Node(id) => &base.graph.0.iter().find(|n| &n.id == id).unwrap().props,
                BaseRef::Edge(src, dst, label) => {
                    &base
                        .graph
                        .1
                        .iter()
                        .find(|e| (&e.src, &e.dst, &e.label) == (src, dst, label))
                        .unwrap()
                        .props
                }
            };
            assert_eq!(
                assertion.provenance,
                serde_json::from_value::<Provenance>(props["prov"].clone()).unwrap()
            );
            assert!(
                artifact
                    .content
                    .contains(&serde_json::to_string(&(id, cite)).unwrap())
            );
        }
    }
    facts.reverse();
    assert_eq!(compile(&facts), artifact); // bytes AND content identity, not presence only
    assert!(!artifact.content.contains("omitted because"));
    assert_eq!(base, before);
}
#[test]
fn capability_stories_preserve_base_bytes_and_filter_whole_derived_story() {
    use crate::ExportMode::{BestEffort, VerifiedOnly};
    let mut base = fixture(PATH, REPO);
    let facts = derive_graphql_resolver_capabilities(&base).facts;
    let mut existing = capability(&facts[0]).clone();
    existing.id = "base-capability".into();
    base.graph.0.push(existing);
    for mode in [BestEffort, VerifiedOnly] {
        let legacy = crate::compile_spec_selected(
            &base.graph.0,
            &base.graph.1,
            &[],
            mode,
            &BTreeSet::new(),
            &["user-stories"],
            citation_limits(usize::MAX),
        )
        .unwrap()
        .remove(0);
        assert_eq!(
            citation_compile(&base, &[], mode, &BTreeSet::new(), usize::MAX).unwrap(),
            legacy
        );
    }
    let legacy = citation_artifact(
        crate::compile_spec_selected(
            &base.graph.0,
            &base.graph.1,
            &[],
            BestEffort,
            &BTreeSet::new(),
            &["user-stories"],
            citation_limits(usize::MAX),
        )
        .unwrap()
        .remove(0),
    );
    let with_derived = citation_artifact(
        citation_compile(&base, &facts, BestEffort, &BTreeSet::new(), usize::MAX).unwrap(),
    );
    let (legacy_stories, legacy_table) = legacy
        .content
        .split_once("\n## Assertions and inline provenance")
        .unwrap();
    let (derived_stories, derived_table) = with_derived
        .content
        .split_once("\n## Assertions and inline provenance")
        .unwrap();
    let (derived_base_stories, derived_story) = derived_stories
        .split_once("## US-R-0002 — Derived Capability")
        .unwrap();
    // A readable title, then the story's exact JSON identity (#1443).
    assert!(derived_story.starts_with(": "));
    assert!(derived_story.contains("\n\n    [\""));
    let derived_table = derived_table
        .split_once("\n## Derived assertions and provenance")
        .unwrap()
        .0;
    assert_eq!(derived_base_stories, legacy_stories);
    assert_eq!(derived_table, legacy_table);
    let strict = citation_artifact(
        citation_compile(&base, &facts, VerifiedOnly, &BTreeSet::new(), usize::MAX).unwrap(),
    );
    assert!(strict.assertions.is_empty());
    let candidate_hash = capability(&facts[0]).props["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let rejected = BTreeSet::from([candidate_hash]);
    let rejected_artifact = citation_artifact(
        citation_compile(&base, &facts, BestEffort, &rejected, usize::MAX).unwrap(),
    );
    assert!(rejected_artifact.assertions.is_empty());
    let support_id = facts[0]
        .cites
        .iter()
        .find_map(|cite| match cite {
            BaseRef::Node(id) => Some(id),
            _ => None,
        })
        .unwrap();
    let support = base
        .graph
        .0
        .iter_mut()
        .find(|n| &n.id == support_id)
        .unwrap();
    support.props["prov"]["confidence_tier"] = serde_json::json!("InferredStrong");
    let hash = support.props["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let artifact = citation_artifact(
        citation_compile(
            &base,
            &facts,
            BestEffort,
            &BTreeSet::from([hash]),
            usize::MAX,
        )
        .unwrap(),
    );
    assert_eq!(artifact.assertions.len(), 1); // unaffected base story remains
    assert_eq!(artifact.assertions[0].subject_id, "base-capability");
    assert!(artifact.content.contains(
        "1 derived stories omitted because candidate facts or their supporting facts are filtered."
    ));
    assert!(!artifact.content.contains("support:"));
}
// AC-0504: an entirely filtered derived set reports omissions, not absent recovery.
#[test]
fn capability_stories_all_filtered_do_not_claim_no_recovered_capabilities() {
    let mut base = fixture(PATH, REPO);
    let facts = derive_graphql_resolver_capabilities(&base).facts;
    assert_eq!(facts.len(), 1);
    let support = base
        .graph
        .0
        .iter_mut()
        .find(|node| facts[0].cites.contains(&BaseRef::Node(node.id.clone())))
        .unwrap();
    // Curation rejects inferred support; Confirmed support cannot be rejected.
    support.props["prov"]["confidence_tier"] = serde_json::json!("InferredStrong");
    let rejected = BTreeSet::from([support.props["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned()]);
    let compile = |facts: &[OverlayFact]| {
        citation_artifact(
            citation_compile(
                &base,
                facts,
                crate::ExportMode::BestEffort,
                &rejected,
                usize::MAX,
            )
            .unwrap(),
        )
    };
    let filtered = compile(&facts);
    assert!(filtered.assertions.is_empty());
    assert!(filtered.content.contains(
        "1 derived stories omitted because candidate facts or their supporting facts are filtered."
    ));
    assert!(
        !filtered
            .content
            .contains("No Capability facts have been recovered yet.")
    );
    let absent = compile(&[]);
    assert!(
        absent
            .content
            .contains("No Capability facts have been recovered yet.")
    );
    assert!(!absent.content.contains("omitted because"));
}
#[test]
fn capability_stories_reject_invalid_or_foreign_support_without_partial_output() {
    use crate::CapabilityStoryInputError::*;
    let base = fixture(PATH, REPO);
    let facts = derive_graphql_resolver_capabilities(&base).facts;
    let check = |base: &RevisionedSnapshot, facts: &[OverlayFact]| {
        citation_compile(
            base,
            facts,
            crate::ExportMode::BestEffort,
            &BTreeSet::new(),
            usize::MAX,
        )
    };
    assert!(check(&base, &facts).is_ok());
    assert_eq!(
        check(&base, &[facts[0].clone(), facts[0].clone()]),
        Err(DuplicateCandidate)
    );
    let mut changed = facts.clone();
    changed[0].cites = BTreeSet::from([BaseRef::Node("other-revision".into())]);
    assert_eq!(check(&base, &changed), Err(SupportNotInBase));
    let support_id = facts[0]
        .cites
        .iter()
        .find_map(|cite| match cite {
            BaseRef::Node(id) => Some(id),
            BaseRef::Edge(..) => None,
        })
        .unwrap();
    for invalid_provenance in ["absent", "malformed", "invalid"] {
        let mut changed_base = base.clone();
        let support = changed_base
            .graph
            .0
            .iter_mut()
            .find(|node| &node.id == support_id)
            .unwrap();
        match invalid_provenance {
            "absent" => {
                support.props.as_object_mut().unwrap().remove("prov");
            }
            "malformed" => support.props["prov"] = serde_json::Value::Null,
            "invalid" => {
                support.props["prov"]["tier"] = serde_json::json!("Agentic");
                support.props["prov"]["confidence_tier"] = serde_json::json!("Confirmed");
            }
            _ => unreachable!(),
        }
        assert_eq!(check(&changed_base, &facts), Err(InvalidSupportProvenance));
    }
    changed[0].cites.clear();
    assert_eq!(check(&base, &changed), Err(EmptySupport));
    let mut ambiguous = base.clone();
    let support = base
        .graph
        .0
        .iter()
        .find(|n| facts[0].cites.contains(&BaseRef::Node(n.id.clone())))
        .unwrap();
    ambiguous.graph.0.push(support.clone());
    assert_eq!(check(&ambiguous, &facts), Err(AmbiguousSupport));
    for mutation in ["method", "prov", "label", "shadow"] {
        let mut changed = facts.clone();
        let OverlayItem::Node(node) = &mut changed[0].item else {
            unreachable!()
        };
        match mutation {
            "method" => {
                node.props["prov"]["method"] =
                    serde_json::json!({"kind":"model","model":"untrusted"})
            }
            "prov" => node.props["prov"] = serde_json::Value::Null,
            "label" => node.label = "Actor".into(),
            "shadow" => node.id = base.graph.0[0].id.clone(),
            _ => unreachable!(),
        }
        assert_eq!(check(&base, &changed), Err(Candidate), "{mutation}");
    }
}
#[test]
fn capability_stories_admit_full_escaped_associations_at_exact_output_boundary() {
    let mut base = fixture(PATH, REPO);
    let mut facts = derive_graphql_resolver_capabilities(&base).facts;
    let hostile = "```\n# injected <script>alert('x')</script> ` & |";
    let OverlayItem::Node(node) = &mut facts[0].item else {
        unreachable!()
    };
    node.props["title"] = hostile.into();
    node.props["prov"]["method"]["rule"] = hostile.into();
    let support = base
        .graph
        .0
        .iter_mut()
        .find(|n| n.label == "Symbol" && facts[0].cites.contains(&BaseRef::Node(n.id.clone())))
        .unwrap();
    support.props["name"] = hostile.into();
    support.props["prov"]["evidence"][0]["path"] = hostile.into();
    let compile = |max| {
        citation_compile(
            &base,
            &facts,
            crate::ExportMode::BestEffort,
            &BTreeSet::new(),
            max,
        )
        .unwrap()
    };
    let artifact = citation_artifact(compile(usize::MAX));
    assert!(artifact.content.contains("<script>")); // literal inside indented JSON only
    assert!(!artifact.content.contains("\n# injected"));
    assert!(!artifact.content.contains("\n```"));
    for line in artifact.content.lines().filter(|l| l.contains("<script>")) {
        assert!(line.starts_with("    "), "{line}");
    }
    let bytes = artifact.content.len() + serde_json::to_vec(&artifact.assertions).unwrap().len();
    assert_eq!(
        compile(bytes),
        crate::SelectedArtifactOutcome::Produced(artifact)
    );
    assert_eq!(
        compile(bytes - 1),
        crate::SelectedArtifactOutcome::Limited {
            artifact_id: "user-stories".into(),
            limit: crate::SelectedArtifactLimit::OutputBytes {
                remaining: bytes - 1
            },
        }
    );
}

// AC-0504: own provenance and support filtering count a valid candidate once;
// neither mode nor curation may turn hidden recovery into absent recovery.
#[test]
fn capability_stories_count_own_filtering_once_and_keep_validation() {
    let mut base = fixture(PATH, REPO);
    let facts = derive_graphql_resolver_capabilities(&base).facts;
    assert_eq!(facts.len(), 1);
    let own_hash = capability(&facts[0]).props["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let support = base
        .graph
        .0
        .iter_mut()
        .find(|n| facts[0].cites.contains(&BaseRef::Node(n.id.clone())))
        .unwrap();
    // Use curation-eligible support as in the existing support-filter regression.
    support.props["prov"]["confidence_tier"] = serde_json::json!("InferredStrong");
    let support_hash = support.props["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(own_hash, support_hash);
    let before = base.clone();
    let visible = citation_artifact(
        citation_compile(
            &base,
            &facts,
            crate::ExportMode::BestEffort,
            &BTreeSet::new(),
            usize::MAX,
        )
        .unwrap(),
    );
    assert!(!visible.content.contains("derived stories omitted"));
    assert!(
        visible
            .assertions
            .iter()
            .any(|a| a.subject_id == capability(&facts[0]).id)
    );
    for mode in [
        crate::ExportMode::VerifiedOnly,
        crate::ExportMode::BestEffort,
    ] {
        let mut rejections = vec![
            BTreeSet::from([own_hash.clone()]),
            BTreeSet::from([support_hash.clone()]),
            BTreeSet::from([own_hash.clone(), support_hash.clone()]),
        ];
        if mode == crate::ExportMode::VerifiedOnly {
            rejections.push(BTreeSet::new());
        }
        for rejected in rejections {
            let artifact = citation_artifact(
                citation_compile(&base, &facts, mode, &rejected, usize::MAX).unwrap(),
            );
            assert!(artifact.content.contains("1 derived stories omitted because candidate facts or their supporting facts are filtered."));
            assert!(
                !artifact
                    .content
                    .contains("No Capability facts have been recovered yet.")
            );
            assert!(artifact.assertions.is_empty());
            assert!(artifact.verify_identity());
            let bytes =
                artifact.content.len() + serde_json::to_vec(&artifact.assertions).unwrap().len();
            assert_eq!(
                citation_artifact(citation_compile(&base, &facts, mode, &rejected, bytes).unwrap()),
                artifact
            );
            assert!(matches!(
                citation_compile(&base, &facts, mode, &rejected, bytes - 1).unwrap(),
                crate::SelectedArtifactOutcome::Limited { .. }
            ));
        }
        // Even an own-filtered candidate must validate all supporting references.
        let mut invalid = facts.clone();
        invalid[0].cites = BTreeSet::from([BaseRef::Node("missing-support".into())]);
        assert_eq!(
            citation_compile(
                &base,
                &invalid,
                mode,
                &BTreeSet::from([own_hash.clone()]),
                usize::MAX
            ),
            Err(crate::CapabilityStoryInputError::SupportNotInBase)
        );
    }
    assert_eq!(base, before);
}

const CONTROLLER_PATH: &str = "packages/admin/src/orders.controller.ts";
const CONTROLLER_SOURCE: &[u8] = br#"
import {Controller, Get, Post} from '@nestjs/common';
@Controller('orders')
export class OrdersController {
  @Get() list() {}
  @Post(':id/cancel') cancel() {}
  helper() {}
}
@Controller('health')
export class HealthController {
  @Get() check() {}
}
export class Plain {
  @Get() notRouted() {}
}
"#;

// AC-0801: one Capability per controller class, citing every endpoint,
// handler and relation it groups; the base is unchanged.
#[test]
fn controller_capability_groups_real_adapter_routes_by_class() {
    let base = fixture_source(CONTROLLER_SOURCE, CONTROLLER_PATH, REPO);
    let before = base.clone();
    let report = derive_controller_capabilities(&base);
    assert_eq!(base, before);
    assert_eq!(report.oversized_groups, 0);
    let mut titles: Vec<_> = report
        .facts
        .iter()
        .map(|fact| capability(fact).props["title"].as_str().unwrap().to_owned())
        .collect();
    titles.sort();
    assert_eq!(titles, ["Manage orders", "View health"]);
    let orders = report
        .facts
        .iter()
        .find(|fact| capability(fact).props["title"] == "Manage orders")
        .unwrap();
    let node = capability(orders);
    assert_eq!(node.label, "Capability");
    assert!(node.id.starts_with("capability:http-controller:"));
    assert_eq!(node.props.as_object().unwrap().len(), 2); // no auth/business assertions
    let list = format!("ep:{REPO}@GET:/orders");
    let cancel = format!("ep:{REPO}@POST:/orders/:id/cancel");
    let list_handler = format!("sym:{REPO}@{CONTROLLER_PATH}#OrdersController.list");
    let cancel_handler = format!("sym:{REPO}@{CONTROLLER_PATH}#OrdersController.cancel");
    let expected: BTreeSet<_> = [
        BaseRef::Node(list.clone()),
        BaseRef::Node(cancel.clone()),
        BaseRef::Node(list_handler.clone()),
        BaseRef::Node(cancel_handler.clone()),
        BaseRef::Edge(list, list_handler, "HANDLES".into()),
        BaseRef::Edge(cancel, cancel_handler, "HANDLES".into()),
    ]
    .into_iter()
    .collect();
    assert_eq!(orders.cites, expected);
    let prov: Provenance = serde_json::from_value(node.props["prov"].clone()).unwrap();
    assert!(prov.validate().is_ok());
    assert_eq!(prov.tier, Tier::Semantic);
    assert_eq!(prov.confidence_tier, ConfidenceTier::InferredWeak);
    assert_eq!(prov.extractor_id, "t2.knowledge.capability.http-controller");
    assert_eq!(
        prov.method,
        ProducingMethod::Rule {
            rule: CAPABILITY_CONTROLLER_RULE.into(),
            version: CAPABILITY_CONTROLLER_RULE_VERSION.into()
        }
    );
    assert!(!prov.evidence.is_empty());
    assert!(prov.evidence.iter().all(|e| e.path == CONTROLLER_PATH));

    // Fact order does not change identity or content.
    let mut reversed = base.clone();
    reversed.graph.0.reverse();
    reversed.graph.1.reverse();
    assert_eq!(derive_controller_capabilities(&reversed), report);
    // GraphQL resolvers are another rule's subject, and the reverse.
    assert!(derive_graphql_resolver_capabilities(&base).facts.is_empty());
    assert!(
        derive_controller_capabilities(&fixture(PATH, REPO))
            .facts
            .is_empty()
    );
}

// AC-0801: support that is ambiguous, not a class method, from another file
// or not source-backed yields no candidate for that route.
#[test]
fn controller_capability_omits_ambiguous_foreign_and_unproven_routes() {
    let base = fixture_source(CONTROLLER_SOURCE, CONTROLLER_PATH, REPO);
    let health = format!("ep:{REPO}@GET:/health");
    let titles = |snapshot: &RevisionedSnapshot| -> Vec<String> {
        let mut titles: Vec<_> = derive_controller_capabilities(snapshot)
            .facts
            .iter()
            .map(|fact| capability(fact).props["title"].as_str().unwrap().to_owned())
            .collect();
        titles.sort();
        titles
    };
    let orders_only = ["Manage orders"];

    // A second handler for the route.
    let mut two_handlers = base.clone();
    let mut extra = two_handlers
        .graph
        .1
        .iter()
        .find(|e| e.src == health && e.label == "HANDLES")
        .unwrap()
        .clone();
    extra.dst = format!("sym:{REPO}@{CONTROLLER_PATH}#OrdersController.list");
    two_handlers.graph.1.push(extra);
    assert_eq!(titles(&two_handlers), orders_only);

    // The endpoint id declared twice.
    let mut duplicate = base.clone();
    let copy = duplicate
        .graph
        .0
        .iter()
        .find(|n| n.id == health)
        .unwrap()
        .clone();
    duplicate.graph.0.push(copy);
    assert_eq!(titles(&duplicate), orders_only);

    let edit = |change: &dyn Fn(&mut Node)| {
        let mut changed = base.clone();
        for node in &mut changed.graph.0 {
            change(node);
        }
        changed
    };
    let handler = format!("sym:{REPO}@{CONTROLLER_PATH}#HealthController.check");
    // The handler is not a class method.
    assert_eq!(
        titles(&edit(&|node| if node.id == handler {
            node.props["kind"] = "Function".into();
        })),
        orders_only
    );
    // The class has no name of its own: the adapter's `class@<offset>`
    // placeholder is not something to title a Capability with.
    let mut anonymous = edit(&|node| {
        if node.id == handler {
            node.id = format!("sym:{REPO}@{CONTROLLER_PATH}#class@42.check");
            node.props["class"] = "class@42".into();
            node.props["name"] = "class@42.check".into();
        }
    });
    for edge in &mut anonymous.graph.1 {
        if edge.dst == handler {
            edge.dst = format!("sym:{REPO}@{CONTROLLER_PATH}#class@42.check");
        }
    }
    assert_eq!(titles(&anonymous), orders_only);
    // Another framework's endpoint.
    assert_eq!(
        titles(&edit(&|node| if node.id == health {
            node.props["framework"] = "express".into();
        })),
        orders_only
    );
    // Endpoint evidence from another file.
    assert_eq!(
        titles(&edit(&|node| if node.id == health {
            node.props["prov"]["evidence"][0]["path"] = "other.ts".into();
        })),
        orders_only
    );
    // Support that is not source-backed, or not confident.
    assert_eq!(
        titles(&edit(&|node| if node.id == health {
            node.props["prov"]["basis"] = "structural".into();
        })),
        orders_only
    );
    assert_eq!(
        titles(&edit(&|node| if node.id == health {
            node.props["prov"]["extractor_id"] = "t0.adapter-go".into();
        })),
        orders_only
    );
    // Unchanged, both controllers are named.
    assert_eq!(titles(&base).len(), 2);
}

// #1443: a Vendure-shaped admin resolver, through the real TypeScript
// adapter, yields readable user stories in the whole-bundle export the
// desktop compiles, not only in the MCP single-artifact read.
const ADMIN_PATH: &str = "packages/core/src/api/resolvers/admin/product.resolver.ts";
const ADMIN_SOURCE: &[u8] = br#"
import { Mutation, Query, Resolver } from '@nestjs/graphql';
import { Permission } from '@vendure/common/lib/generated-types';
import { Allow } from '../../decorators/allow.decorator';
@Resolver()
export class ProductResolver {
  @Query()
  @Allow(Permission.ReadCatalog, Permission.ReadProduct)
  products() {}
  @Mutation()
  @Allow(Permission.CreateCatalog, Permission.CreateProduct)
  createProduct() {}
}
"#;
fn knowledge_bundle(
    base: &RevisionedSnapshot,
    capabilities: &[OverlayFact],
    actors: &[OverlayFact],
    mode: crate::ExportMode,
) -> crate::SpecBundle {
    let candidates: Vec<_> = capabilities
        .iter()
        .map(|fact| crate::CapabilityStory {
            capability: capability(fact),
            cites: &fact.cites,
        })
        .collect();
    let knowledge =
        crate::validate_knowledge_stories(base, &candidates, actors, mode, &BTreeSet::new())
            .unwrap();
    crate::try_compile_spec_supplied(
        &base.graph.0,
        &base.graph.1,
        &[],
        mode,
        &BTreeSet::new(),
        crate::Supplied {
            knowledge: Some(&knowledge),
            ..Default::default()
        },
    )
    .unwrap()
}
#[test]
fn vendure_like_bundle_renders_readable_derived_user_stories() {
    use crate::ExportMode::{BestEffort, VerifiedOnly};
    let base = fixture_source(ADMIN_SOURCE, ADMIN_PATH, REPO);
    let capabilities = derive_graphql_resolver_capabilities(&base).facts;
    let actors = derive_permission_actors(&base).facts;
    assert_eq!(capabilities.len(), 1);
    assert_eq!(actors.len(), 2);
    // No base Capability: before #1443 the desktop export said so.
    let bare = crate::compile_spec(
        &base.graph.0,
        &base.graph.1,
        &[],
        BestEffort,
        &BTreeSet::new(),
    );
    let stories_of = |bundle: &crate::SpecBundle| {
        bundle
            .artifacts
            .iter()
            .find(|a| a.id == "user-stories")
            .unwrap()
            .clone()
    };
    assert!(
        stories_of(&bare)
            .content
            .contains("No Capability facts have been recovered yet.")
    );

    let bundle = knowledge_bundle(&base, &capabilities, &actors, BestEffort);
    let stories = stories_of(&bundle);
    assert!(stories.verify_identity());
    let content = &stories.content;
    assert!(!content.contains("No Capability facts"), "{content}");
    assert!(
        content.contains("## US-R-0001 — Derived Capability: Manage products (admin API)"),
        "{content}"
    );
    assert!(
        content.contains(
            "- As an admin API caller holding CreateCatalog or CreateProduct, I can create product (MUTATION admin.createProduct)."
        ),
        "{content}"
    );
    assert!(
        content.contains(
            "- As an admin API caller holding ReadCatalog or ReadProduct, I can view products (QUERY admin.products)."
        ),
        "{content}"
    );
    // The bundle's artifact equals the selected one and the MCP read's, and
    // the selected fact index (which resolves every citation) is the bundle's.
    let candidates: Vec<_> = capabilities
        .iter()
        .map(|fact| crate::CapabilityStory {
            capability: capability(fact),
            cites: &fact.cites,
        })
        .collect();
    let mcp = citation_artifact(
        crate::compile_knowledge_user_stories(
            &base,
            &candidates,
            &actors,
            BestEffort,
            &BTreeSet::new(),
            citation_limits(usize::MAX),
        )
        .unwrap(),
    );
    assert_eq!(mcp, stories);
    let knowledge = crate::validate_knowledge_stories(
        &base,
        &candidates,
        &actors,
        BestEffort,
        &BTreeSet::new(),
    )
    .unwrap();
    let selected = crate::compile_spec_selected_supplied(
        &base.graph.0,
        &base.graph.1,
        &[],
        BestEffort,
        &BTreeSet::new(),
        &["user-stories", "fact-index"],
        citation_limits(usize::MAX),
        crate::Supplied {
            knowledge: Some(&knowledge),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(citation_artifact(selected[0].clone()), stories);
    let bundle_index = bundle
        .artifacts
        .iter()
        .find(|a| a.id == "fact-index")
        .unwrap();
    assert_eq!(&citation_artifact(selected[1].clone()), bundle_index);
    if std::env::var_os("PRINT_STORIES").is_some() {
        println!("{content}");
    }

    // A verified-only export withholds the weak derived stories and says so.
    let strict = stories_of(&knowledge_bundle(
        &base,
        &capabilities,
        &actors,
        VerifiedOnly,
    ));
    assert!(strict.assertions.is_empty());
    assert!(
        strict.content.contains("derived stories omitted"),
        "{}",
        strict.content
    );
}

// AC-0663, #1443: the product specification's Actors (1.2) and Capabilities (1.3)
// frame the knowledge layer's derived facts, not "No `Capability` fact".
#[test]
fn product_specification_frames_derived_capabilities_and_caller_roles() {
    use crate::ExportMode::{BestEffort, VerifiedOnly};
    let base = fixture_source(ADMIN_SOURCE, ADMIN_PATH, REPO);
    let capabilities = derive_graphql_resolver_capabilities(&base).facts;
    let actors = derive_permission_actors(&base).facts;
    let d1_of = |bundle: &crate::SpecBundle| {
        bundle
            .artifacts
            .iter()
            .find(|a| a.id == "d1-product-specification")
            .unwrap()
            .clone()
    };
    let section = |content: &str, heading: &str| {
        let start = content.find(heading).unwrap();
        let rest = &content[start + heading.len()..];
        rest[..rest.find("\n## ").unwrap_or(rest.len())].to_owned()
    };
    let d1 = d1_of(&knowledge_bundle(&base, &capabilities, &actors, BestEffort));
    assert!(d1.verify_identity());
    let content = &d1.content;
    let caps = section(content, "## 1.3 ");
    assert!(!caps.contains("No `Capability` fact"), "{content}");
    assert!(caps.contains("Derived Capabilities: 1."), "{caps}");
    assert!(
        caps.contains("| Manage products (admin API) | 2 |"),
        "{caps}"
    );
    let roles = section(content, "## 1.2 ");
    assert!(!roles.contains("No `Actor` fact"), "{content}");
    assert!(
        roles.contains("2 permission sets over 2 guarded operations"),
        "{roles}"
    );
    assert!(
        roles.contains("| CreateCatalog, CreateProduct | 1 | MUTATION admin.createProduct |"),
        "{roles}"
    );
    // Each role row prints the citation ids of its Actors, so a composer
    // can cite who may act (AC-1315, #2326).
    for fact in &actors {
        let OverlayItem::Node(node) = &fact.item else {
            unreachable!()
        };
        let id = crate::citation_id(&core_graph::FactKey::Node {
            id: node.id.clone(),
        });
        assert!(roles.contains(&id), "{id} in {roles}");
    }
    assert!(
        roles.contains("| Any of | Operations | Examples | Citation |"),
        "{roles}"
    );
    // A §1.2 composition therefore lists the derived Actors as citable.
    let derived: Vec<_> = actors
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Node(node) => Some((node, &fact.cites)),
            OverlayItem::Edge(_) => None,
        })
        .collect();
    let picked = crate::composition_facts(&roles, &base.graph.0, &base.graph.1, &derived);
    assert_eq!(picked.derived.len(), actors.len(), "{roles}");
    // Every derived fact printed is cited, so the fact index resolves it.
    for fact in capabilities.iter().chain(&actors) {
        let OverlayItem::Node(node) = &fact.item else {
            unreachable!()
        };
        let id = format!("node:{}", node.id);
        assert!(d1.assertions.iter().any(|a| a.id == id), "{id}");
    }
    // Verified-only withholds the weak derived facts: the frame is the base one.
    let strict = d1_of(&knowledge_bundle(
        &base,
        &capabilities,
        &actors,
        VerifiedOnly,
    ));
    assert!(section(&strict.content, "## 1.3 ").contains("No `Capability` fact"));
}
