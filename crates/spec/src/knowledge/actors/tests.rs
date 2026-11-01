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

// AC-0515: both import locals require complete adapter identifier syntax;
// Public is a legal alias and is excluded only as a permission alternative.
#[test]
fn actors_validate_both_import_locals_in_resealed_sites() {
    for binding in ["decorator", "permission_import"] {
        for local in [
            "Gate Admin",
            " ",
            "Gate.Admin",
            "1Gate",
            "Gate()",
            r"\u12",
            r"\u{}",
        ] {
            let mut node = endpoint("ep:invalid-local", &["Read"]);
            node.props["permission_site"][binding]["local"] = json!(local);
            seal(&mut node);
            assert!(
                derive_permission_actors(&base(vec![node])).facts.is_empty(),
                "{binding}: {local:?}"
            );
        }
        for local in [
            "$Gate",
            "Öwner",
            "G\u{301}ate",
            "G\u{200c}ate",
            "G\u{200d}ate",
            r"\u0047ate",
            r"\u{47}ate",
            "Public",
            r"\u0050ublic",
        ] {
            let mut node = endpoint("ep:valid-local", &["Read"]);
            node.props["permission_site"][binding]["local"] = json!(local);
            seal(&mut node);
            let report = derive_permission_actors(&base(vec![node]));
            assert_eq!(report.facts.len(), 1, "{binding}: {local:?}");
            assert_eq!(
                actor_node(&report.facts[0]).props["permission_site"][binding]["local"],
                json!(local)
            );
        }
    }
}

fn compile_actors(
    base: &RevisionedSnapshot,
    actors: &[OverlayFact],
    caps: &[crate::CapabilityStory<'_>],
    mode: crate::ExportMode,
    rejected: &BTreeSet<String>,
    bytes: usize,
) -> Result<crate::SelectedArtifactOutcome, crate::CapabilityStoryInputError> {
    crate::compile_knowledge_user_stories(
        base,
        caps,
        actors,
        mode,
        rejected,
        crate::SelectedCompileLimits {
            max_bytes: bytes,
            adr: crate::AdrLimits {
                max_rows: usize::MAX,
                max_output_bytes: usize::MAX,
            },
        },
    )
}
fn produced(outcome: crate::SelectedArtifactOutcome) -> crate::SpecArtifact {
    let crate::SelectedArtifactOutcome::Produced(artifact) = outcome else {
        panic!("complete artifact")
    };
    assert!(artifact.verify_identity());
    artifact
}

// AC-0518: canonical producer facts share one artifact with stable Capability bytes.
#[test]
fn actor_stories_render_canonical_assumptions_and_preserve_capability_bytes() {
    let source = base(vec![
        endpoint("ep:b", &["Update", "Owner", "Owner"]),
        endpoint("ep:a", &["Read"]),
    ]);
    let before = source.clone();
    let facts = derive_permission_actors(&source).facts;
    for fact in &facts {
        assert!(verify_actor(&source, fact).is_some());
    }
    let mut cap = actor_node(&facts[0]).clone();
    cap.id = "cap:fixture".into();
    cap.label = "Capability".into();
    let caps = [crate::CapabilityStory {
        capability: &cap,
        cites: &facts[0].cites,
    }];
    let rejected = BTreeSet::new();
    let legacy = crate::compile_capability_user_stories(
        &source,
        &caps,
        crate::ExportMode::BestEffort,
        &rejected,
        crate::SelectedCompileLimits {
            max_bytes: usize::MAX,
            adr: crate::AdrLimits {
                max_rows: usize::MAX,
                max_output_bytes: usize::MAX,
            },
        },
    )
    .unwrap();
    assert_eq!(
        compile_actors(
            &source,
            &[],
            &caps,
            crate::ExportMode::BestEffort,
            &rejected,
            usize::MAX
        )
        .unwrap(),
        legacy
    );
    let combined = produced(
        compile_actors(
            &source,
            &facts,
            &caps,
            crate::ExportMode::BestEffort,
            &rejected,
            usize::MAX,
        )
        .unwrap(),
    );
    assert_eq!(combined.id, "user-stories");
    assert!(combined.content.contains("Derived Capability"));
    assert_eq!(
        combined
            .content
            .matches("Actor caller set — Semantic / InferredWeak convention")
            .count(),
        2
    );
    assert!(
        combined.content.contains(
            "not effective runtime authorization or an identified user, role or principal"
        )
    );
    for fact in &facts {
        let node = actor_node(fact);
        for value in [
            &node.props["any_of"],
            &node.props["assumption"],
            &node.props["permission_site"]["prov"]["evidence"],
        ] {
            assert!(
                combined
                    .content
                    .contains(&serde_json::to_string(value).unwrap())
            );
        }
        assert!(combined.assertions.iter().any(|a| a.subject_id == node.id
            && a.provenance.confidence_tier == ConfidenceTier::InferredWeak));
        let cite = fact.cites.first().unwrap();
        let mut hash = blake3::Hasher::new();
        hash.update(b"capability-story-support-v1\0");
        hash.update(&serde_json::to_vec(&(&node.id, cite)).unwrap());
        let id = format!("support:{}", hash.finalize().to_hex());
        let assertion = combined.assertions.iter().find(|a| a.id == id).unwrap();
        let endpoint = verify_actor(&source, fact).unwrap();
        assert_eq!(assertion.subject_id, endpoint.id);
        assert_eq!(assertion.provenance, support(&endpoint.props).unwrap());
        assert!(
            combined
                .content
                .contains(&serde_json::to_string(cite).unwrap())
        );
    }
    let mut reordered = facts.clone();
    reordered.reverse();
    assert_eq!(
        produced(
            compile_actors(
                &source,
                &reordered,
                &caps,
                crate::ExportMode::BestEffort,
                &rejected,
                usize::MAX
            )
            .unwrap()
        ),
        combined
    );
    assert_eq!(source, before);
}

// AC-0518: malformed full facts fail atomically; valid hidden facts count omissions.
#[test]
fn actor_stories_refuse_invalid_and_count_hidden_candidates() {
    let source = base(vec![endpoint("ep:a", &["Read"])]);
    let facts = derive_permission_actors(&source).facts;
    let run =
        |base: &RevisionedSnapshot, actors: &[OverlayFact], mode, rejected: &BTreeSet<String>| {
            compile_actors(base, actors, &[], mode, rejected, usize::MAX)
        };
    let rejected = BTreeSet::new();
    for (key, value) in [
        ("assumption", json!({"statement":"forged"})),
        ("prov", json!({})),
        ("any_of", json!(["Other"])),
        ("extra", json!(true)),
    ] {
        let mut invalid = facts.clone();
        let OverlayItem::Node(node) = &mut invalid[0].item else {
            unreachable!()
        };
        node.props[key] = value; // keep original identity/hash: full equality is required
        assert!(verify_actor(&source, &invalid[0]).is_none());
        assert!(run(&source, &invalid, crate::ExportMode::BestEffort, &rejected).is_err());
        assert!(
            run(
                &source,
                &invalid,
                crate::ExportMode::VerifiedOnly,
                &rejected
            )
            .is_err()
        );
    }
    // A resealed source site and matching full candidate cannot bypass the
    // producer's corrected property syntax or escaped-Public exclusion.
    for argument in ["<script>bad</script>", "Owner Admin", r"\u0050ublic"] {
        let invalid = base(vec![endpoint("ep:invalid", &[argument])]);
        let endpoint = &invalid.graph.0[0];
        let syntax: PermissionSite =
            serde_json::from_value(endpoint.props["permission_site"].clone()).unwrap();
        let forged = actor(endpoint, &syntax, ACTOR_ALLOW_ASSUMPTION);
        assert!(verify_actor(&invalid, &forged).is_none());
        for mode in [
            crate::ExportMode::BestEffort,
            crate::ExportMode::VerifiedOnly,
        ] {
            assert!(run(&invalid, std::slice::from_ref(&forged), mode, &rejected).is_err());
        }
    }
    let mut duplicate = facts.clone();
    duplicate.push(facts[0].clone());
    assert!(
        run(
            &source,
            &duplicate,
            crate::ExportMode::BestEffort,
            &rejected
        )
        .is_err()
    );
    let mut shadowed = source.clone();
    shadowed.graph.0.push(actor_node(&facts[0]).clone());
    assert!(run(&shadowed, &facts, crate::ExportMode::BestEffort, &rejected).is_err());
    let mut missing = source.clone();
    missing.graph.0.clear();
    assert!(run(&missing, &facts, crate::ExportMode::BestEffort, &rejected).is_err());
    let mut ambiguous = source.clone();
    ambiguous.graph.0.push(source.graph.0[0].clone());
    assert!(run(&ambiguous, &facts, crate::ExportMode::BestEffort, &rejected).is_err());
    let endpoint_hash = source.graph.0[0].props["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    for (mode, rejected) in [
        (crate::ExportMode::VerifiedOnly, BTreeSet::new()),
        (
            crate::ExportMode::BestEffort,
            BTreeSet::from([endpoint_hash]),
        ),
    ] {
        let artifact = produced(run(&source, &facts, mode, &rejected).unwrap());
        assert!(artifact.assertions.is_empty());
        assert!(artifact.content.contains("1 derived stories omitted"));
        assert!(
            !artifact
                .content
                .contains("No Capability facts have been recovered yet")
        );
        assert!(!artifact.content.contains("Actor caller set"));
    }
}

// AC-0518: escaped ordered data and full assertion arrays share exact output admission.
#[test]
fn actor_stories_bound_workspace_and_complete_escaped_output() {
    let source = base(vec![endpoint(
        "ep:<img src=x><script>bad</script>",
        &[r"\u003cscript", "Owner", "Owner"],
    )]);
    let facts = derive_permission_actors(&source).facts;
    assert_eq!(facts.len(), 1);
    let run = |bytes| {
        compile_actors(
            &source,
            &facts,
            &[],
            crate::ExportMode::BestEffort,
            &BTreeSet::new(),
            bytes,
        )
        .unwrap()
    };
    let artifact = produced(run(usize::MAX));
    assert!(!artifact.content.contains("\n<script>"));
    assert!(artifact.content.contains(r"\\u003cscript"));
    assert!(
        artifact
            .content
            .contains("ep:<img src=x><script>bad</script>")
    );
    assert!(artifact.content.contains("    ["));
    let bytes = artifact.content.len() + serde_json::to_vec(&artifact.assertions).unwrap().len();
    assert_eq!(produced(run(bytes)), artifact);
    assert!(matches!(
        run(bytes - 1),
        crate::SelectedArtifactOutcome::Limited { .. }
    ));
    let graph_bytes = serde_json::to_vec(&source.graph).unwrap().len();
    let actor_bytes = serde_json::to_vec(&facts).unwrap().len();
    let bound =
        crate::logical_actor_story_workspace_bound(graph_bytes, facts.len(), actor_bytes).unwrap();
    assert!(bound.canonical_scratch_bytes > serde_json::to_vec(&facts[0]).unwrap().len());
    assert!(
        crate::logical_actor_story_workspace_bound(graph_bytes, facts.len() + 1, actor_bytes)
            .unwrap()
            .retained_bytes
            > bound.retained_bytes
    );
    assert!(
        crate::logical_actor_story_workspace_bound(graph_bytes, facts.len(), actor_bytes + 1)
            .unwrap()
            .canonical_scratch_bytes
            > bound.canonical_scratch_bytes
    );
    assert!(crate::logical_actor_story_workspace_bound(usize::MAX, 1, 1).is_none());
    assert!(crate::logical_actor_story_workspace_bound(0, usize::MAX, 0).is_none());
}

// Shared AC-0534 source-shaped fixture: canonical Actor, Capability, security
// finding, and an ADR drift whose serialized context depends on a real flow.
fn admitted_actor_fixture() -> (
    RevisionedSnapshot,
    Vec<OverlayFact>,
    Node,
    Vec<flowtracer::Flow>,
) {
    let mut endpoint = endpoint("ep:admitted", &["Read"]);
    endpoint.props["authenticated"] = json!(false);
    endpoint.props["method"] = json!("GET");
    endpoint.props["path"] = json!("/orders");
    let prov: Provenance = serde_json::from_value(endpoint.props["prov"].clone()).unwrap();
    let mut source = base(vec![endpoint]);
    source.graph.0.extend([
        Node { id: "sym:target".into(), label: "Symbol".into(), props: json!({"prov":prov}) },
        Node { id: "adr:guard".into(), label: "ADR".into(), props: json!({"origin":"found", "title":"No direct call", "forbids":["CALLS"], "prov":prov}) },
    ]);
    source.graph.1.extend([
        core_graph::Edge {
            src: "adr:guard".into(),
            dst: "ep:admitted".into(),
            label: "DECIDES".into(),
            props: json!({"prov":prov}),
        },
        core_graph::Edge {
            src: "ep:admitted".into(),
            dst: "sym:target".into(),
            label: "CALLS".into(),
            props: json!({"prov":prov}),
        },
    ]);
    let actors = derive_permission_actors(&source).facts;
    assert_eq!(actors.len(), 1);
    let mut capability = actor_node(&actors[0]).clone();
    capability.id = "cap:admitted".into();
    capability.label = "Capability".into();
    let flows = vec![flowtracer::Flow {
        trigger: "ep:admitted".into(),
        trigger_kind: "Endpoint".into(),
        trigger_name: "orders".into(),
        hops: vec![flowtracer::Hop {
            src: "ep:admitted".into(),
            dst: "sym:target".into(),
            label: "CALLS".into(),
            src_name: "orders".into(),
            dst_name: "target".into(),
            tier: "Deterministic".into(),
            confidence: "InferredStrong".into(),
            evidence: None,
            provenance: prov,
            gap_reason: None,
            attempted_tiers: vec![],
        }],
        status: flowtracer::FlowStatus::Verified,
        score: 0.8,
        depth_limited: false,
    }];
    (source, actors, capability, flows)
}
fn admitted_actor_limits() -> crate::SelectedCompileLimits {
    crate::SelectedCompileLimits {
        max_bytes: usize::MAX,
        adr: crate::AdrLimits {
            max_rows: usize::MAX,
            max_output_bytes: usize::MAX,
        },
    }
}
fn admitted_actor_compile(
    source: &RevisionedSnapshot,
    actors: &[OverlayFact],
    caps: &[crate::CapabilityStory<'_>],
    flows: &[flowtracer::Flow],
    limits: crate::SelectedCompileLimits,
    security: crate::SecurityLimits,
) -> Result<crate::SelectedArtifactOutcome, crate::CapabilityStoryInputError> {
    crate::compile_knowledge_user_stories_with_flows_and_security(
        source,
        caps,
        actors,
        flows,
        crate::ExportMode::BestEffort,
        &BTreeSet::new(),
        limits,
        security,
    )
}
fn admitted_actor_security() -> crate::SecurityLimits {
    crate::SecurityLimits {
        max_rows: usize::MAX,
        max_output_bytes: usize::MAX,
    }
}

// AC-0534: empty Actors preserve the explicit flows/security API's exact artifact.
#[test]
fn admitted_actor_empty_input_preserves_flow_security_artifact_identity() {
    let (source, actors, cap, flows) = admitted_actor_fixture();
    let caps = [crate::CapabilityStory {
        capability: &cap,
        cites: &actors[0].cites,
    }];
    let legacy = produced(
        crate::compile_capability_user_stories_with_flows_and_security(
            &source,
            &caps,
            &flows,
            crate::ExportMode::BestEffort,
            &BTreeSet::new(),
            admitted_actor_limits(),
            admitted_actor_security(),
        )
        .unwrap(),
    );
    let admitted = produced(
        admitted_actor_compile(
            &source,
            &[],
            &caps,
            &flows,
            admitted_actor_limits(),
            admitted_actor_security(),
        )
        .unwrap(),
    );
    assert_eq!(admitted.content, legacy.content);
    assert_eq!(admitted.content_id, legacy.content_id);
    assert_eq!(admitted, legacy);
}

// AC-0534: Actors and Capabilities retain the caller's flow-sensitive ADR admission.
#[test]
fn admitted_actor_combined_stories_preserve_flow_sensitive_adr_limit() {
    let (source, actors, cap, flows) = admitted_actor_fixture();
    let before = source.clone();
    let caps = [crate::CapabilityStory {
        capability: &cap,
        cites: &actors[0].cites,
    }];
    let artifact = produced(
        admitted_actor_compile(
            &source,
            &actors,
            &caps,
            &flows,
            admitted_actor_limits(),
            admitted_actor_security(),
        )
        .unwrap(),
    );
    assert!(artifact.content.contains("Derived Capability"));
    assert!(artifact.content.contains("Actor caller set"));
    assert!(
        artifact
            .assertions
            .iter()
            .any(|a| a.subject_id == actor_node(&actors[0]).id)
    );
    let empty = crate::derive_adr_facts(&source.graph.0, &source.graph.1, &[]);
    let contextual = crate::derive_adr_facts(&source.graph.0, &source.graph.1, &flows);
    let mut limits = admitted_actor_limits();
    limits.adr.max_output_bytes = serde_json::to_vec(&empty).unwrap().len();
    assert!(serde_json::to_vec(&contextual).unwrap().len() > limits.adr.max_output_bytes);
    assert!(matches!(
        admitted_actor_compile(
            &source,
            &actors,
            &caps,
            &[],
            limits,
            admitted_actor_security()
        )
        .unwrap(),
        crate::SelectedArtifactOutcome::Produced(_)
    ));
    assert!(matches!(
        admitted_actor_compile(
            &source,
            &actors,
            &caps,
            &flows,
            limits,
            admitted_actor_security()
        )
        .unwrap(),
        crate::SelectedArtifactOutcome::Limited {
            limit: crate::SelectedArtifactLimit::AdrDerivation(_),
            ..
        }
    ));
    assert_eq!(source, before);
}

// AC-0534: restrictive explicit security admission returns no partial artifact.
#[test]
fn admitted_actor_security_refusal_returns_only_limited() {
    let (source, actors, cap, flows) = admitted_actor_fixture();
    let caps = [crate::CapabilityStory {
        capability: &cap,
        cites: &actors[0].cites,
    }];
    assert!(
        crate::security_projection_size(&source.graph.0, &source.graph.1)
            .unwrap()
            .rows
            > 0
    );
    let outcome = admitted_actor_compile(
        &source,
        &actors,
        &caps,
        &flows,
        admitted_actor_limits(),
        crate::SecurityLimits {
            max_rows: 0,
            max_output_bytes: usize::MAX,
        },
    )
    .unwrap();
    assert!(matches!(
        outcome,
        crate::SelectedArtifactOutcome::Limited {
            limit: crate::SelectedArtifactLimit::SecurityDerivation(_),
            ..
        }
    ));
}

// AC-0534: invalid Actor sidecars fail atomically despite valid Capability/context.
#[test]
fn admitted_actor_invalid_sidecar_refuses_complete_artifact() {
    let (source, actors, cap, flows) = admitted_actor_fixture();
    let before = source.clone();
    let caps = [crate::CapabilityStory {
        capability: &cap,
        cites: &actors[0].cites,
    }];
    let mut invalid = actors.clone();
    let OverlayItem::Node(node) = &mut invalid[0].item else {
        unreachable!()
    };
    node.props["assumption"]["statement"] = json!("forged");
    assert_eq!(
        admitted_actor_compile(
            &source,
            &invalid,
            &caps,
            &flows,
            admitted_actor_limits(),
            admitted_actor_security()
        ),
        Err(crate::CapabilityStoryInputError::Candidate)
    );
    assert_eq!(source, before);
}
