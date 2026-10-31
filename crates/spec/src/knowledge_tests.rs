//! #1443 Capability-only draft: real existing adapter facts, no live ingestion.
// AC-0500: Capability support, ordering, abstention and exact per-fact omissions.
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
    assert_eq!(
        node.props["title"],
        "shop GraphQL operations in ShopOrderResolver"
    );
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
            version: "1".into()
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
        "admin-api GraphQL operations in ShopOrderResolver"
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
