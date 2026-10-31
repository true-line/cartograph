use super::*;
use core_graph::{
    StoreRevision,
    overlay::{OverlayRead, OverlayRecipe, OverlayStore},
};
use serde_json::json;

fn seal(node: &mut Node) {
    let mut payload = node.props["permission_site"].as_object().unwrap().clone();
    payload.remove("prov");
    let content = format!(
        "GraphQL permission syntax {}",
        serde_json::Value::Object(payload)
    );
    node.props["permission_site"]["prov"]["content_hash"] =
        json!(core_prov::content_hash(content.as_bytes()));
}
fn endpoint(id: &str, args: &[&str]) -> Node {
    // Explicitly synthetic AC-0517 graph payload; adapter is not in this base.
    let evidence = |a, b| EvidenceRef {
        repo: "sample".into(),
        path: "order.ts".into(),
        byte_start: a,
        byte_end: b,
        commit_sha: "fixed".into(),
    };
    let site = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![
            evidence(1, 2),
            evidence(3, 4),
            evidence(5, 6),
            evidence(7, 8),
            evidence(20, 30),
        ],
        "t0.adapter-ts",
        b"placeholder",
    )
    .unwrap();
    let mut n = Node {
        id: id.into(),
        label: "Endpoint".into(),
        props: json!({
            "protocol":"graphql", "framework":"nestjs-graphql", "permissions":args,
            "prov":Provenance::new(Tier::Deterministic, ConfidenceTier::InferredStrong, vec![evidence(40,48)], "t0.adapter-ts", b"query").unwrap(),
            "permission_site":{"basis":"import-bound-syntax", "arguments":args,
                "decorator":{"exported":"Allow", "local":"Gate", "module":"./custom"},
                "permission_import":{"exported":"Permission", "local":"P", "module":"other-types"}, "prov":site}
        }),
    };
    seal(&mut n);
    n
}
fn base(nodes: Vec<Node>) -> RevisionedSnapshot {
    RevisionedSnapshot {
        revision: StoreRevision {
            graph_instance: "fixture".into(),
            generation: 1,
        },
        graph: (nodes, vec![]),
    }
}
fn actor_node(fact: &OverlayFact) -> &Node {
    let OverlayItem::Node(n) = &fact.item else {
        panic!("Actor node")
    };
    n
}

// AC-0515: endpoint-scoped weak convention preserves exact synthetic syntax evidence.
#[test]
fn actors_preserve_exact_synthetic_site_and_endpoint_scope() {
    let mut shared = endpoint("ep:shared", &["UpdateOrder", "Owner", "Owner"]);
    // Source-shaped synthetic fixture, not an adapter extraction result.
    let text = "import { Allow, Permission } from 'shared';\nimport { Query, Resolver } from '@nestjs/graphql';\n@Resolver() class R { @Query()\n@Allow(Permission.UpdateOrder, Permission.Owner, Permission.Owner)\norder() {} }";
    let owner: EvidenceRef =
        serde_json::from_value(shared.props["prov"]["evidence"][0].clone()).unwrap();
    let span = |needle: &str| {
        let start = text.find(needle).unwrap();
        EvidenceRef {
            byte_start: start as u64,
            byte_end: (start + needle.len()) as u64,
            ..owner.clone()
        }
    };
    let needles = [
        "Allow",
        "Permission",
        "'shared'",
        "@Allow(Permission.UpdateOrder, Permission.Owner, Permission.Owner)",
    ];
    let mut evidence: Vec<_> = needles.iter().map(|s| span(s)).collect();
    evidence.sort_by_key(|e| (e.byte_start, e.byte_end));
    let slices: Vec<_> = evidence
        .iter()
        .map(|e| &text[e.byte_start as usize..e.byte_end as usize])
        .collect();
    assert_eq!(slices, needles); // two specifiers, ONE module literal, decorator
    shared.props["permission_site"]["decorator"]["local"] = json!("Allow");
    shared.props["permission_site"]["permission_import"]["local"] = json!("Permission");
    shared.props["permission_site"]["decorator"]["module"] = json!("shared");
    shared.props["permission_site"]["permission_import"]["module"] = json!("shared");
    shared.props["permission_site"]["prov"]["evidence"] = json!(evidence);
    shared.props["prov"]["evidence"] = json!([span("@Query()")]);
    seal(&mut shared);
    let source = base(vec![
        endpoint("ep:a", &["UpdateOrder", "Owner", "Owner"]),
        endpoint("ep:b", &["UpdateOrder", "Owner", "Owner"]),
        shared,
    ]);
    let before = source.clone();
    let report = derive_permission_actors(&source);
    assert_eq!(report.facts.len(), 3);
    assert_eq!(report.oversized_endpoints, 0);
    assert_ne!(
        actor_node(&report.facts[0]).id,
        actor_node(&report.facts[1]).id
    );
    for (fact, endpoint) in report.facts.iter().zip(&source.graph.0) {
        let n = actor_node(fact);
        assert_eq!(n.label, "Actor");
        assert_eq!(n.props["endpoint_id"], endpoint.id);
        assert_eq!(n.props["any_of"], json!(["UpdateOrder", "Owner", "Owner"]));
        assert_eq!(
            n.props["permission_site"],
            endpoint.props["permission_site"]
        );
        assert_eq!(
            n.props["assumption"],
            json!({"name":"allow-any-listed-permission", "statement":ACTOR_ALLOW_ASSUMPTION, "rule":ACTOR_ALLOW_RULE, "version":ACTOR_ALLOW_RULE_VERSION})
        );
        assert_eq!(
            fact.cites,
            BTreeSet::from([BaseRef::Node(endpoint.id.clone())])
        );
        let p: Provenance = serde_json::from_value(n.props["prov"].clone()).unwrap();
        assert_eq!(
            (p.tier, p.confidence_tier),
            (Tier::Semantic, ConfidenceTier::InferredWeak)
        );
        assert_eq!(
            p.evidence,
            serde_json::from_value::<Provenance>(endpoint.props["permission_site"]["prov"].clone())
                .unwrap()
                .evidence
        );
        assert_eq!(
            p.method,
            ProducingMethod::Rule {
                rule: ACTOR_ALLOW_RULE.into(),
                version: ACTOR_ALLOW_RULE_VERSION.into()
            }
        );
    }
    let mut reversed = source.clone();
    reversed.graph.0.reverse();
    assert_eq!(derive_permission_actors(&reversed), report);
    assert_eq!(source, before);
    let mut store = OverlayStore::open_in_memory().unwrap();
    store.bind_instance("fixture").unwrap();
    let id = store
        .publish(
            &source,
            OverlayRecipe {
                recipe: ACTOR_ALLOW_RULE.into(),
                embedding_namespace: None,
                provider_ran: false,
            },
            report.facts,
        )
        .unwrap();
    assert!(
        matches!(store.read(&source.revision).unwrap(),OverlayRead::Current {identity,..} if identity==id)
    );
    assert_eq!(source, before);
}

// AC-0515: assumption, exact site and order bind identity; per-fact admission stays separate.
#[test]
fn actors_bind_the_versioned_assumption_and_omit_oversized_candidates() {
    let n = endpoint("ep:a", &["Read", "Owner"]);
    let validated = site(&n).unwrap();
    let report = derive_permission_actors(&base(vec![n.clone()]));
    let original = &report.facts[0];
    let changed = actor(&n, &validated, "different convention");
    assert_ne!(actor_node(original).id, actor_node(&changed).id);
    assert_ne!(
        actor_node(original).props["prov"]["content_hash"],
        actor_node(&changed).props["prov"]["content_hash"]
    );
    let reordered = derive_permission_actors(&base(vec![endpoint("ep:a", &["Owner", "Read"])]));
    assert_ne!(actor_node(original).id, actor_node(&reordered.facts[0]).id);
    let mut moved = n.clone();
    moved.props["permission_site"]["prov"]["evidence"][4]["byte_end"] = json!(31);
    assert_ne!(
        actor_node(original).id,
        actor_node(&derive_permission_actors(&base(vec![moved])).facts[0]).id
    );
    let report = derive_permission_actors(&base(vec![endpoint(
        &"x".repeat(MAX_FACT_BYTES),
        &["Owner"],
    )]));
    assert!(report.facts.is_empty());
    assert_eq!(report.oversized_endpoints, 1);
}

// AC-0515: no public/anonymous/role inference, and malformed or unproved sites abstain.
#[test]
fn actors_refuse_public_unproved_mismatched_and_ambiguous_sites() {
    let original = endpoint("ep:a", &["Read", "Owner"]);
    let mut same_owner = original.clone();
    let mut second = same_owner.props["prov"]["evidence"][0].clone();
    second["byte_start"] = json!(50);
    second["byte_end"] = json!(55);
    let first = same_owner.props["prov"]["evidence"][0].clone();
    same_owner.props["prov"]["evidence"] = json!([first, second]);
    assert_eq!(
        derive_permission_actors(&base(vec![same_owner]))
            .facts
            .len(),
        1,
        "a valid multi-span Endpoint from one source remains eligible"
    );
    for case in [
        "absent",
        "public",
        "mixed-public",
        "unrelated-export",
        "permission-export",
        "basis",
        "empty",
        "empty-argument",
        "control-argument",
        "empty-alias",
        "control-alias",
        "mismatch",
        "unknown-field",
        "weak",
        "missing-prov",
        "foreign",
        "bad-span",
        "overlap",
        "missing-evidence",
        "forged-hash",
        "wrong-extractor",
        "ambiguous",
        "mixed-endpoint-repo",
        "mixed-endpoint-path",
        "mixed-endpoint-commit",
    ] {
        let mut n = original.clone();
        match case {
            "absent" => {
                n.props.as_object_mut().unwrap().remove("permission_site");
            }
            "public" | "mixed-public" => {
                let a = if case == "public" {
                    json!(["Public"])
                } else {
                    json!(["Read", "Public"])
                };
                n.props["permission_site"]["arguments"] = a.clone();
                n.props["permissions"] = a;
                seal(&mut n);
            }
            "unrelated-export" => {
                n.props["permission_site"]["decorator"]["exported"] = json!("Other");
                seal(&mut n);
            }
            "permission-export" => {
                n.props["permission_site"]["permission_import"]["exported"] = json!("Other");
                seal(&mut n);
            }
            "basis" => {
                n.props["permission_site"]["basis"] = json!("unproved");
                seal(&mut n);
            }
            "empty" => {
                n.props["permission_site"]["arguments"] = json!([]);
                n.props["permissions"] = json!([]);
                seal(&mut n);
            }
            "empty-argument" => {
                n.props["permission_site"]["arguments"] = json!([""]);
                n.props["permissions"] = json!([""]);
                seal(&mut n);
            }
            "control-argument" => {
                n.props["permission_site"]["arguments"] = json!(["Read\n"]);
                n.props["permissions"] = json!(["Read\n"]);
                seal(&mut n);
            }
            "empty-alias" => {
                n.props["permission_site"]["decorator"]["local"] = json!("");
                seal(&mut n);
            }
            "control-alias" => {
                n.props["permission_site"]["permission_import"]["local"] = json!("P\t");
                seal(&mut n);
            }
            "mismatch" => n.props["permissions"] = json!(["Owner", "Read"]),
            "unknown-field" => {
                n.props["permission_site"]["authenticated"] = json!(true);
                seal(&mut n);
            }
            "weak" => {
                n.props["permission_site"]["prov"]["confidence_tier"] = json!("InferredStrong")
            }
            "missing-prov" => {
                n.props["permission_site"]
                    .as_object_mut()
                    .unwrap()
                    .remove("prov");
            }
            "foreign" => {
                n.props["permission_site"]["prov"]["evidence"][0]["repo"] = json!("foreign")
            }
            "bad-span" => n.props["permission_site"]["prov"]["evidence"][0]["byte_end"] = json!(0),
            "overlap" => n.props["permission_site"]["prov"]["evidence"][0]["byte_end"] = json!(9),
            "missing-evidence" => n.props["permission_site"]["prov"]["evidence"] = json!([]),
            "forged-hash" => {
                n.props["permission_site"]["prov"]["content_hash"] =
                    json!(core_prov::content_hash(b"forged"))
            }
            "wrong-extractor" => {
                n.props["permission_site"]["prov"]["extractor_id"] = json!("other")
            }
            "ambiguous" => {}
            "mixed-endpoint-repo" | "mixed-endpoint-path" | "mixed-endpoint-commit" => {
                let mut second = n.props["prov"]["evidence"][0].clone();
                match case {
                    "mixed-endpoint-repo" => second["repo"] = json!("other-repo"),
                    "mixed-endpoint-path" => second["path"] = json!("other/order.ts"),
                    "mixed-endpoint-commit" => second["commit_sha"] = json!("other-commit"),
                    _ => unreachable!(),
                }
                let first = n.props["prov"]["evidence"][0].clone();
                n.props["prov"]["evidence"] = json!([first, second]);
            }
            _ => unreachable!(),
        }
        let nodes = if case == "ambiguous" {
            vec![n.clone(), n]
        } else {
            vec![n]
        };
        assert!(
            derive_permission_actors(&base(nodes)).facts.is_empty(),
            "{case}"
        );
    }
}

// AC-0515: real AC-0517 adapter encoding must admit Actors without synthetic resealing.
#[test]
fn actors_accept_real_adapter_permission_site_encoding() {
    // Requires the AC-0517 adapter in the branch before execution. Shared import
    // deduplication and a quote in the module exercise actual JSON encoding.
    let source_text = r#"import { Permission as $PΔ, Allow as $Gate } from 'policy"module';
import { Query, Resolver } from '@nestjs/graphql';
@Resolver() class R {
    @Query()
    @$Gate($PΔ.Read$, $PΔ.Öwner, $PΔ.Öwner)
    order() {}
}"#;
    let mut ex = adapters_lang_ts::extract_source(
        source_text.as_bytes(),
        "src/api/resolvers/shop/order.ts",
        &adapters_lang_ts::SourceId {
            repo: "real-fixture",
            commit: "fixed",
        },
    )
    .unwrap();
    ex.close_over_endpoints();
    let source = RevisionedSnapshot {
        graph: (ex.nodes, ex.edges),
        ..base(vec![])
    };
    let report = derive_permission_actors(&source);
    assert_eq!(
        report.facts.len(),
        1,
        "actual adapter site must pass producer validation"
    );
    let endpoint = source
        .graph
        .0
        .iter()
        .find(|n| n.label == "Endpoint")
        .unwrap();
    let n = actor_node(&report.facts[0]);
    let raw = &endpoint.props["permission_site"];
    assert_eq!(n.props["permission_site"], *raw);
    assert_eq!(n.props["any_of"], json!(["Read$", "Öwner", "Öwner"]));
    assert_eq!(n.props["assumption"]["statement"], ACTOR_ALLOW_ASSUMPTION);
    assert_eq!(
        report.facts[0].cites,
        BTreeSet::from([BaseRef::Node(endpoint.id.clone())])
    );
    let p: Provenance = serde_json::from_value(n.props["prov"].clone()).unwrap();
    let site_prov: Provenance = serde_json::from_value(raw["prov"].clone()).unwrap();
    assert_eq!(p.evidence, site_prov.evidence);
    assert_eq!(p.evidence.len(), 4); // one module literal shared by two imports
    let mut payload = raw.as_object().unwrap().clone();
    payload.remove("prov");
    let wire = serde_json::to_string(&payload).unwrap();
    assert!(wire.contains(r#"policy\"module"#));
    assert_eq!(
        site_prov.content_hash,
        core_prov::content_hash(format!("GraphQL permission syntax {wire}").as_bytes())
    );
}

// AC-0519: only source-backed Endpoint support can seed an Actor candidate.
#[test]
fn actors_require_source_basis_for_endpoint_support_and_preserve_site_provenance() {
    let endpoint = endpoint("ep:a", &["Read"]);
    let site_provenance = endpoint.props["permission_site"]["prov"].clone();
    let source = base(vec![endpoint.clone()]);
    let before = source.clone();
    let endpoint_provenance: Provenance =
        serde_json::from_value(endpoint.props["prov"].clone()).unwrap();
    assert_eq!(
        endpoint_provenance.basis(),
        core_prov::EvidenceBasis::Source
    );
    let report = derive_permission_actors(&source);
    assert_eq!(report.facts.len(), 1);
    assert_eq!(
        actor_node(&report.facts[0]).props["permission_site"]["prov"],
        site_provenance
    );
    assert_eq!(source, before);

    let artifact = core_prov::EvidenceBasis::Artifact {
        refs: vec![core_prov::ArtifactRef {
            digest: format!("sha256:{}", "a".repeat(64)),
            kind: "jvm-class".into(),
            at: core_prov::ArtifactLocator::Member {
                path: "com/example/OrderResolver.class".into(),
            },
        }],
    };
    for basis in [artifact, core_prov::EvidenceBasis::Observation] {
        let mut unsupported = endpoint.clone();
        let provenance: Provenance =
            serde_json::from_value(unsupported.props["prov"].clone()).unwrap();
        let provenance = provenance.with_basis(basis);
        assert!(provenance.validate().is_ok());
        unsupported.props["prov"] = serde_json::to_value(provenance).unwrap();
        assert!(
            derive_permission_actors(&base(vec![unsupported.clone()]))
                .facts
                .is_empty()
        );
        assert_eq!(
            unsupported.props["permission_site"]["prov"],
            site_provenance
        );
    }
}

// AC-0515: a consistent hash does not make malformed adapter syntax eligible.
#[test]
fn actors_require_complete_property_identifiers_in_resealed_sites() {
    for argument in [
        "",
        " ",
        "Owner Admin",
        " Owner",
        "Owner ",
        "1Owner",
        "Owner.Admin",
        "Owner[0]",
        "Owner()",
        "Owner; other",
        "Owner/*comment*/",
        "Owner+Admin",
        "Owner\\u",
        "Owner\\u{xyz}",
        "Owner\u{85}Admin",
        "Owner\u{feff}Admin",
        "Owner\u{2060}Admin",
        "Owner\u{200b}Admin",
        "Owner\u{2028}Admin",
        "Owner\u{2029}Admin",
    ] {
        // endpoint() matches permissions to arguments and seals the changed payload.
        let source = base(vec![endpoint("ep:invalid", &[argument])]);
        assert!(
            derive_permission_actors(&source).facts.is_empty(),
            "{argument:?}"
        );
    }
    for argument in [
        "$Owner",
        "Read$",
        "Öwner",
        "ΔRead",
        "O\u{301}wner",
        "O\u{200c}wner",
        "O\u{200d}wner",
        r"\u004fwner",
        r"\u{004f}wner",
        r"\u0050ublicity",
        r"\u{ffffffffffffffffffffffffffffffff}wner",
        r"\u{000000000000000000000000000000004f}wner",
    ] {
        let source = base(vec![endpoint("ep:valid", &[argument])]);
        let report = derive_permission_actors(&source);
        assert_eq!(report.facts.len(), 1, "{argument:?}");
        assert_eq!(
            actor_node(&report.facts[0]).props["any_of"],
            json!([argument])
        );
        // Oracle: the pinned real adapter must emit this exact source spelling.
        let text = format!(
            "import {{ Allow, Permission }} from 'policy';\nimport {{ Query, Resolver }} from '@nestjs/graphql';\n@Resolver() class R {{ @Query() @Allow(Permission.{argument}) order() {{}} }}"
        );
        let mut extraction = adapters_lang_ts::extract_source(
            text.as_bytes(),
            "order.ts",
            &adapters_lang_ts::SourceId {
                repo: "oracle",
                commit: "fixed",
            },
        )
        .unwrap();
        extraction.close_over_endpoints();
        let real = RevisionedSnapshot {
            graph: (extraction.nodes, extraction.edges),
            ..base(vec![])
        };
        let report = derive_permission_actors(&real);
        assert_eq!(report.facts.len(), 1, "adapter: {argument:?}");
        assert_eq!(
            actor_node(&report.facts[0]).props["any_of"],
            json!([argument])
        );
    }
}

// AC-0515: Public abstains even when its exact adapter spelling uses escapes.
#[test]
fn actors_refuse_escaped_public_without_normalizing_other_permissions() {
    for argument in [
        r"\u0050ublic",
        r"\u{50}ublic",
        r"P\u0075blic",
        r"\u0050\u0075\u0062\u006c\u0069\u0063",
        r"\u{0000000000000000000000000000000050}ublic",
    ] {
        assert!(
            derive_permission_actors(&base(vec![endpoint("ep:public", &[argument])]))
                .facts
                .is_empty(),
            "{argument:?}"
        );
        let text = format!(
            "import {{ Allow, Permission }} from 'policy';\nimport {{ Query, Resolver }} from '@nestjs/graphql';\n@Resolver() class R {{ @Query() @Allow(Permission.{argument}) order() {{}} }}"
        );
        let mut extraction = adapters_lang_ts::extract_source(
            text.as_bytes(),
            "order.ts",
            &adapters_lang_ts::SourceId {
                repo: "oracle",
                commit: "fixed",
            },
        )
        .unwrap();
        extraction.close_over_endpoints();
        let endpoint = extraction
            .nodes
            .iter()
            .find(|n| n.label == "Endpoint")
            .unwrap();
        assert_eq!(
            endpoint.props["permission_site"]["arguments"],
            json!([argument])
        );
        let real = RevisionedSnapshot {
            graph: (extraction.nodes, extraction.edges),
            ..base(vec![])
        };
        assert!(
            derive_permission_actors(&real).facts.is_empty(),
            "adapter: {argument:?}"
        );
    }
}
