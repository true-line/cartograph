//! `endpoint-permissions.md` (#1787, AC-0599).
use super::*;
use crate::{
    FactIndex, SelectedArtifactOutcome, SelectedCompileLimits, SpecArtifact, citation_id,
    compile_spec, compile_spec_selected, try_compile_spec_with_coverage,
};
use core_graph::FactKey;
use core_prov::{EvidenceRef, Tier};
use serde_json::{Value, json};

const ID: &str = "endpoint-permissions";
const SITED: &str = "Endpoints with recovered permission sites";
const NONE: &str = "Endpoints with none recovered";
const PARTIAL: &str = "Endpoints whose handler path is partial";
const OUTSIDE: &str = "Coverage note: endpoints outside permission-site recovery";

fn prov(tier: Tier, confidence: ConfidenceTier, path: &str, span: (u64, u64), fact: &str) -> Value {
    let evidence = EvidenceRef {
        repo: "acme/shop".into(),
        path: path.into(),
        byte_start: span.0,
        byte_end: span.1,
        commit_sha: "0123abc".into(),
    };
    serde_json::to_value(
        Provenance::new(
            tier,
            confidence,
            vec![evidence],
            "t0.fixture",
            fact.as_bytes(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn confirmed(path: &str, span: (u64, u64), fact: &str) -> Value {
    prov(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        path,
        span,
        fact,
    )
}

fn symbol(id: &str) -> Node {
    Node {
        id: id.into(),
        label: "Symbol".into(),
        props: json!({"name": id, "prov": confirmed("app.py", (0, 1), id)}),
    }
}

fn relation(src: &str, label: &str, dst: &str) -> Edge {
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: json!({"prov": confirmed("app.py", (2, 3), &format!("{src} {label} {dst}"))}),
    }
}

/// An endpoint in the shape the language adapters emit, with its handler.
fn endpoint(route: &str, framework: &str, language: &str, span: (u64, u64)) -> (Node, Node, Edge) {
    let id = format!("ep:acme/shop@GET:{route}");
    let handler = format!("sym:acme/shop@app.py#{route}");
    let node = Node {
        id: id.clone(),
        label: "Endpoint".into(),
        props: json!({
            "method": "GET", "path": route, "handler_sym": handler,
            "framework": framework, "language": language,
            "prov": confirmed("app.py", span, &id),
        }),
    };
    let handles = relation(&id, "HANDLES", &handler);
    (node, symbol(&handler), handles)
}

/// `Endpoint.auth` as `adapters-lang-python::auth_guards` writes a guard.
fn required(span: (u64, u64), confidence: Value) -> Value {
    let mut auth = json!({
        "state": "required", "authenticated": true,
        "guards": ["flask_login.login_required"], "basis": ["framework_guard"],
        "prov": confirmed("app.py", span, "Auth of handler"),
    });
    if !confidence.is_null() {
        auth["prov"] = confidence;
    }
    auth
}

/// `Endpoint.auth` as the adapter writes a complete reading with no guard.
fn no_guard() -> Value {
    json!({
        "state": "none", "authenticated": false,
        "basis": "no_guard_decorator_or_blocking_request_hook",
        "assumptions": ["non_repository_calls_in_request_hooks_do_not_abort"],
        "prov": prov(Tier::Deterministic, ConfidenceTier::InferredStrong, "app.py", (0, 9), "Auth"),
    })
}

struct Fixture {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Add a Flask route; `auth` is its `Endpoint.auth`.
    fn flask(&mut self, route: &str, span: (u64, u64), auth: Value) -> String {
        self.route(route, "flask", "python", span, auth)
    }

    fn route(
        &mut self,
        route: &str,
        framework: &str,
        language: &str,
        span: (u64, u64),
        auth: Value,
    ) -> String {
        let (mut node, handler, handles) = endpoint(route, framework, language, span);
        if !auth.is_null() {
            node.props["auth"] = auth;
        }
        let handler_id = handler.id.clone();
        self.nodes.extend([node, handler]);
        self.edges.push(handles);
        handler_id
    }

    fn artifact(&self, mode: ExportMode) -> SpecArtifact {
        compile_spec(&self.nodes, &self.edges, &[], mode, &BTreeSet::new())
            .artifacts
            .into_iter()
            .find(|artifact| artifact.id == ID)
            .expect("the endpoint permission artifact is exported")
    }
}

/// The guarded route, the unguarded route and a route outside recovery.
fn three_routes() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.flask("/me", (120, 160), required((100, 115), Value::Null));
    fixture.flask("/health", (200, 240), no_guard());
    fixture.route("/orders", "jax-rs", "java", (300, 340), Value::Null);
    fixture
}

/// The body of one `## heading` section.
fn section<'a>(content: &'a str, heading: &str) -> &'a str {
    let start = content
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("section {heading}"));
    let body = &content[start + 1..];
    body[1..].find("\n## ").map_or(body, |end| &body[..=end])
}

fn row<'a>(section: &'a str, endpoint: &str) -> Option<&'a str> {
    section
        .lines()
        .find(|line| line.starts_with(&format!("| GET {endpoint} |")))
}

// AC-0599: the guarded route lists its permission site with tier, confidence
// and span; the unguarded one is under "none recovered" and nowhere else.
#[test]
fn guarded_route_lists_its_site_and_unguarded_route_is_none_recovered() {
    let artifact = three_routes().artifact(ExportMode::VerifiedOnly);
    assert_eq!(artifact.file_name, "endpoint-permissions.md");
    let content = &artifact.content;

    let guarded = row(section(content, SITED), "/me").expect("guarded route is in table one");
    // Endpoint tier, confidence and span, then the site's.
    assert!(
        guarded
            .contains("| Deterministic | Confirmed | acme/shop:app.py bytes 120..160 @ 0123abc |")
    );
    assert!(guarded.contains("guard: flask\\_login.login\\_required (basis: framework\\_guard)"));
    assert!(
        guarded
            .contains("| Deterministic | Confirmed | acme/shop:app.py bytes 100..115 @ 0123abc |")
    );
    assert!(guarded.contains("| read: 1 facts, no unresolved hop |"));
    for other in [NONE, PARTIAL, OUTSIDE] {
        assert!(row(section(content, other), "/me").is_none(), "{other}");
    }

    let open = row(section(content, NONE), "/health").expect("unguarded route: none recovered");
    assert!(
        open.contains("| Deterministic | Confirmed | acme/shop:app.py bytes 200..240 @ 0123abc |")
    );
    assert!(open.contains("no\\_guard\\_decorator\\_or\\_blocking\\_request\\_hook"));
    for other in [SITED, PARTIAL, OUTSIDE] {
        assert!(row(section(content, other), "/health").is_none(), "{other}");
    }

    // The site is its own cited fact with its own provenance (R-INT-2).
    let site = artifact
        .assertions
        .iter()
        .find(|assertion| assertion.subject_kind == SITE_KIND)
        .expect("the site is asserted");
    assert_eq!(site.subject_id, "permission-site:ep:acme/shop@GET:/me");
    assert_eq!(site.provenance.evidence[0].byte_start, 100);
    assert_eq!(site.provenance.confidence_tier, ConfidenceTier::Confirmed);
}

// AC-0599: a route in a framework with no permission-site recovery is under
// the coverage note and in neither table, and the document names what is
// covered.
#[test]
fn route_outside_recovery_is_under_the_coverage_note_only() {
    let content = three_routes().artifact(ExportMode::BestEffort).content;
    let outside = section(&content, OUTSIDE);
    assert!(outside.contains("| java | jax-rs | 1 |"));
    let listed = row(outside, "/orders").expect("listed under the coverage note");
    assert!(
        listed
            .contains("| Deterministic | Confirmed | acme/shop:app.py bytes 300..340 @ 0123abc |")
    );
    for table in [SITED, NONE, PARTIAL] {
        assert!(
            row(section(&content, table), "/orders").is_none(),
            "{table}"
        );
    }
    let covered = section(&content, "Permission-site recovery covered today");
    for recovery in RECOVERY {
        assert!(covered.contains(&format!(
            "| `{}` | {} |",
            recovery.framework, recovery.language
        )));
    }
    assert!(!covered.contains("jax-rs"));
    assert!(content.contains(
        "Endpoints read: 3. With a recovered permission site: 1. None recovered: 1. Handler path partial: 0. Outside permission-site recovery: 1."
    ));
}

// AC-0599: the covered-framework list is what the adapters emit. The real
// TypeScript adapter's NestJS GraphQL site is listed with its decorator span,
// and an operation without one is not placed in table one.
#[test]
fn nestjs_graphql_site_from_the_real_adapter_is_listed() {
    let facts = adapters_lang_ts::extract_source(
        b"import { Query, Resolver } from '@nestjs/graphql';\nimport { Permission } from '@vendure/common/lib/generated-shop-types';\nimport { Allow } from '../../decorators/allow.decorator';\n@Resolver() export class ShopOrderResolver {\n@Query()\n@Allow(Permission.Owner)\nactiveOrder() {}\n@Query()\nnextOrderStates() {}\n}",
        "src/api/resolvers/shop/shop-order.resolver.ts",
        &adapters_lang_ts::SourceId {
            repo: "acme/shop",
            commit: "0123abc",
        },
    )
    .unwrap();
    let guarded = facts
        .nodes
        .iter()
        .find(|node| node.label == "Endpoint" && node.props["field"] == "activeOrder")
        .expect("the adapter emits the operation");
    let recovery = recovery(guarded).expect("its framework is in the covered list");
    assert_eq!(recovery.framework, "nestjs-graphql");
    assert!(guarded.props[recovery.property].is_object());
    let site: Provenance =
        serde_json::from_value(guarded.props["permission_site"]["prov"].clone()).unwrap();

    let fixture = Fixture {
        nodes: facts.nodes.clone(),
        edges: facts.edges.clone(),
    };
    let artifact = fixture.artifact(ExportMode::VerifiedOnly);
    let sited = section(&artifact.content, SITED);
    let listed = sited
        .lines()
        .find(|line| line.starts_with("| QUERY shop.activeOrder |"))
        .expect("the guarded operation is in table one");
    assert!(listed.contains("Allow decorator: Owner"));
    assert!(listed.contains(&format!(
        "bytes {}..{} @ 0123abc",
        site.evidence[0].byte_start, site.evidence[0].byte_end
    )));
    assert!(!sited.contains("shop.nextOrderStates"));
    assert!(
        section(&artifact.content, NONE).contains("| QUERY shop.nextOrderStates |")
            || section(&artifact.content, PARTIAL).contains("| QUERY shop.nextOrderStates |")
    );
    assert!(!section(&artifact.content, OUTSIDE).contains("shop."));
}

// AC-0964: Spring method-security annotations from the real Java adapter are
// permission sites, and D1 §1.2 names the caller each one admits; an
// endpoint with no annotation is partial (URL rules are not read), never
// "none recovered".
#[test]
fn spring_method_security_from_the_real_adapter_names_callers() {
    let facts = adapters_lang_java::extract_source(
        br#"package com.acme.web;
import org.springframework.security.access.prepost.PreAuthorize;
import jakarta.annotation.security.PermitAll;
import org.springframework.web.bind.annotation.*;
final class Roles { static final String ADMIN = "ROLE_ADMIN"; }
@RestController
@RequestMapping("/api")
class UserResource {
    @GetMapping("/admin/users")
    @PreAuthorize("hasAuthority(\"" + Roles.ADMIN + "\")")
    public String all() { return ""; }
    @GetMapping("/status")
    @PermitAll
    public String status() { return ""; }
    @GetMapping("/me")
    public String me() { return ""; }
}
"#,
        "src/main/java/com/acme/web/UserResource.java",
        &adapters_lang_java::SourceId {
            repo: "acme/shop",
            commit: "0123abc",
        },
    )
    .unwrap();
    let endpoint = |path: &str| {
        facts
            .nodes
            .iter()
            .find(|n| n.label == "Endpoint" && n.props["path"] == path)
            .unwrap_or_else(|| panic!("{path}: {:?}", facts.nodes))
    };
    assert_eq!(
        recovery(endpoint("/api/admin/users")).map(|r| r.framework),
        Some("spring")
    );
    assert_eq!(
        endpoint("/api/admin/users").props["auth"]["caller"],
        "caller with authority ROLE_ADMIN"
    );
    let fixture = Fixture {
        nodes: facts.nodes.clone(),
        edges: facts.edges.clone(),
    };
    let artifact = fixture.artifact(ExportMode::BestEffort);
    let sited = row(section(&artifact.content, SITED), "/api/admin/users")
        .expect("the annotated endpoint is in table one");
    assert!(sited.contains("@PreAuthorize"), "{sited}");
    assert!(row(section(&artifact.content, NONE), "/api/status").is_some());
    assert!(row(section(&artifact.content, PARTIAL), "/api/me").is_some());
    assert!(!section(&artifact.content, OUTSIDE).contains("/api/"));

    let bundle = compile_spec(
        &facts.nodes,
        &facts.edges,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
    );
    let d1 = &bundle
        .artifacts
        .iter()
        .find(|a| a.id == "d1-product-specification")
        .unwrap()
        .content;
    let actors = &d1[d1.find("## 1.2 ").unwrap()..d1.find("## 1.3 ").unwrap()];
    assert!(
        actors.contains("| Caller with authority ROLE\\_ADMIN | @PreAuthorize"),
        "{actors}"
    );
    assert!(
        actors.contains("Spring method-security annotations"),
        "{actors}"
    );
}

// AC-0599 (R-INT-4): a route with no recovered site whose handler path hits a
// Gap, has no handler, reaches the walk bound or whose guards the adapter
// could not classify is partial, never "none recovered". A cycle ends.
#[test]
fn handler_path_hitting_a_gap_is_partial() {
    let mut fixture = Fixture::new();
    // /gap: handler -> helper -> Gap.
    let handler = fixture.flask("/gap", (10, 20), no_guard());
    fixture.nodes.extend([
        symbol("sym:helper"),
        Node {
            id: "gap:call".into(),
            label: "Gap".into(),
            props: json!({
                "reason": "dynamic dispatch",
                "prov": prov(Tier::Deterministic, ConfidenceTier::Gap, "app.py", (30, 40), "gap"),
            }),
        },
    ]);
    fixture.edges.extend([
        relation(&handler, "CALLS", "sym:helper"),
        relation("sym:helper", "CALLS", "gap:call"),
    ]);
    // /cycle: handler -> a -> handler, read to its end.
    let handler = fixture.flask("/cycle", (50, 60), no_guard());
    fixture.nodes.push(symbol("sym:a"));
    fixture.edges.extend([
        relation(&handler, "CALLS", "sym:a"),
        relation("sym:a", "CALLS", &handler),
    ]);
    // /deep: a call chain longer than the depth bound.
    let mut last = fixture.flask("/deep", (70, 80), no_guard());
    for index in 0..=ENDPOINT_PATH_MAX_DEPTH {
        let next = format!("sym:deep:{index:02}");
        fixture.nodes.push(symbol(&next));
        fixture.edges.push(relation(&last, "CALLS", &next));
        last = next;
    }
    // /nohandler: the HANDLES relation is not among the facts.
    let handler = fixture.flask("/nohandler", (90, 95), no_guard());
    fixture.edges.retain(|edge| edge.dst != handler);
    // /unknown: the adapter could not classify a decorator.
    fixture.flask(
        "/unknown",
        (96, 99),
        json!({"state": "unknown", "reasons": ["unclassified_decorator:@audit"]}),
    );
    // /guarded-gap: a recovered site keeps the route in table one.
    let handler = fixture.flask("/guarded-gap", (100, 110), required((97, 99), Value::Null));
    fixture.edges.push(relation(&handler, "CALLS", "gap:call"));

    let artifact = fixture.artifact(ExportMode::VerifiedOnly);
    let content = &artifact.content;
    let partial = section(content, PARTIAL);
    let none = section(content, NONE);

    let gap = row(partial, "/gap").expect("a path into a Gap is partial");
    assert!(gap.contains("complete: none recovered"));
    assert!(gap.contains(
        "partial: 2 facts read; unresolved hop sym:helper CALLS gap:call (dynamic dispatch)"
    ));
    assert!(row(none, "/gap").is_none());
    // The unresolved hop is cited, so the reader can find it in the index.
    assert!(
        artifact
            .assertions
            .iter()
            .any(|assertion| assertion.subject_id == "sym:helper CALLS gap:call")
    );

    assert!(
        row(none, "/cycle")
            .expect("a cycle is read to its end")
            .contains("| read: 2 facts, no unresolved hop |")
    );
    assert!(
        row(partial, "/deep")
            .expect("a path past the bound is partial")
            .contains("walk bound reached (16 calls deep, 256 facts or 2048 relations)")
    );
    assert!(
        row(partial, "/nohandler")
            .expect("an endpoint without a handler is partial")
            .contains("partial: no handler is among the facts this export admits")
    );
    assert!(
        row(partial, "/unknown")
            .expect("an unclassified guard reading is partial")
            .contains("incomplete: the adapter could not classify the endpoint's guards: unclassified\\_decorator:@audit")
    );
    for route in ["/deep", "/nohandler", "/unknown"] {
        assert!(row(none, route).is_none(), "{route}");
    }
    let guarded = row(section(content, SITED), "/guarded-gap").expect("a site keeps table one");
    assert!(guarded.contains("partial: 1 facts read; unresolved hop"));
    assert!(row(partial, "/guarded-gap").is_none());
}

// AC-0599 (R-INT-5): verified-only omits the row resting on an InferredWeak
// permission site, counts it, and builds no assertion for it, so the export
// is not refused; best-effort lists it annotated.
#[test]
fn verified_only_omits_a_weak_site_and_best_effort_annotates_it() {
    let mut fixture = three_routes();
    let weak = prov(
        Tier::Agentic,
        ConfidenceTier::InferredWeak,
        "app.py",
        (400, 410),
        "weak site",
    );
    fixture.flask("/weak", (420, 440), required((0, 0), weak));

    let verified = try_compile_spec_with_coverage(
        &fixture.nodes,
        &fixture.edges,
        &[],
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
        &[],
    )
    .expect("a withheld row cites nothing, so the export is not refused");
    let verified = verified
        .artifacts
        .iter()
        .find(|artifact| artifact.id == ID)
        .unwrap();
    assert!(!verified.content.contains("GET /weak"));
    assert!(verified.content.contains(
        "1 endpoints are omitted from every table: the permission site recovered for each is InferredWeak or rejected, which this export withholds."
    ));
    assert!(
        verified
            .assertions
            .iter()
            .all(|assertion| !assertion.subject_id.contains("/weak")
                && assertion.provenance.confidence_tier != ConfidenceTier::InferredWeak)
    );
    assert!(!verified.content.contains("rests on an InferredWeak fact"));

    let best = fixture.artifact(ExportMode::BestEffort);
    let listed = row(section(&best.content, SITED), "/weak").expect("best-effort lists the row");
    assert!(
        listed.contains("| Agentic | InferredWeak | acme/shop:app.py bytes 400..410 @ 0123abc |")
    );
    assert!(
        listed
            .ends_with("| rests on an InferredWeak fact; a verified-only export omits this row |")
    );
    assert!(!best.content.contains("are omitted from every table"));
    // Rows that rest on no weak fact carry no annotation.
    assert!(
        row(section(&best.content, SITED), "/me")
            .unwrap()
            .ends_with("| — |")
    );

    // A rejected InferredStrong site is withheld the same way in both modes.
    let mut rejected_fixture = three_routes();
    let strong = prov(
        Tier::Semantic,
        ConfidenceTier::InferredStrong,
        "app.py",
        (400, 410),
        "strong site",
    );
    let hash = strong["content_hash"].as_str().unwrap().to_owned();
    rejected_fixture.flask("/rejected", (420, 440), required((0, 0), strong));
    let rejected = compile_spec(
        &rejected_fixture.nodes,
        &rejected_fixture.edges,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::from([hash]),
    );
    let rejected = rejected.artifacts.iter().find(|a| a.id == ID).unwrap();
    assert!(!rejected.content.contains("GET /rejected"));
    assert!(
        rejected
            .content
            .contains("1 endpoints are omitted from every table")
    );
}

// AC-0599: the same graph yields the same bytes whatever order the store or
// the caller hands the facts over in, and the selected compiler agrees.
#[test]
fn two_exports_are_byte_identical_whatever_the_input_order() {
    let mut fixture = three_routes();
    let handler = fixture.flask("/gap", (10, 20), no_guard());
    fixture.nodes.push(symbol("sym:helper"));
    fixture.edges.extend([
        relation(&handler, "CALLS", "sym:helper"),
        relation(&handler, "CALLS", "gap:first"),
        relation("sym:helper", "CALLS", "gap:second"),
    ]);
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let first = fixture.artifact(mode);
        assert_eq!(fixture.artifact(mode), first);
        let reversed = Fixture {
            nodes: fixture.nodes.iter().rev().cloned().collect(),
            edges: fixture.edges.iter().rev().cloned().collect(),
        };
        assert_eq!(reversed.artifact(mode), first);
        assert!(first.verify_identity());

        let selected = compile_spec_selected(
            &reversed.nodes,
            &reversed.edges,
            &[],
            mode,
            &BTreeSet::new(),
            &[ID],
            SelectedCompileLimits {
                max_bytes: 1 << 20,
                adr: crate::AdrLimits {
                    max_rows: 1000,
                    max_output_bytes: 1 << 20,
                },
            },
        )
        .unwrap();
        assert_eq!(selected, [SelectedArtifactOutcome::Produced(first)]);
    }
}

// AC-0599 (ADR-0058): every assertion of the artifact, the permission site
// included, has a row in the export's fact index that names this artifact.
#[test]
fn every_assertion_resolves_in_the_fact_index() {
    let mut fixture = three_routes();
    let handler = fixture.flask("/gap", (10, 20), no_guard());
    fixture.edges.push(relation(&handler, "CALLS", "gap:call"));
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let bundle = compile_spec(&fixture.nodes, &fixture.edges, &[], mode, &BTreeSet::new());
        let artifact = bundle.artifacts.iter().find(|a| a.id == ID).unwrap();
        let index = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "fact-index-json")
            .unwrap();
        let index: FactIndex = serde_json::from_str(&index.content).unwrap();
        assert!(artifact.assertions.len() >= 8);
        for assertion in &artifact.assertions {
            let subject = |key: &FactKey| match key {
                FactKey::Node { id } => id.clone(),
                FactKey::Edge {
                    source,
                    label,
                    destination,
                } => format!("{source} {label} {destination}"),
            };
            let row = index
                .rows
                .iter()
                .find(|row| subject(&row.fact) == assertion.subject_id)
                .unwrap_or_else(|| panic!("{} has a row", assertion.subject_id));
            assert_eq!(row.citation_id, citation_id(&row.fact));
            assert!(row.cited_by.iter().any(|by| by == ID));
            assert!(row.records.iter().any(|record| {
                record.tier == assertion.provenance.tier
                    && record.confidence == assertion.provenance.confidence_tier
                    && record.evidence == assertion.provenance.evidence
            }));
        }
        let site = FactKey::Node {
            id: "permission-site:ep:acme/shop@GET:/me".into(),
        };
        assert!(index.rows.iter().any(|row| row.fact == site));
    }
}

// AC-0599: absence of a recovered check is stated as "none recovered" and
// never as a statement about the endpoint.
#[test]
fn text_never_calls_an_endpoint_unauthenticated_or_unprotected() {
    let mut fixture = three_routes();
    let handler = fixture.flask("/gap", (10, 20), no_guard());
    fixture.edges.push(relation(&handler, "CALLS", "gap:call"));
    fixture.flask(
        "/unknown",
        (96, 99),
        json!({"state": "unknown", "reasons": []}),
    );
    fixture.flask("/unread", (90, 95), Value::Null);
    for fixture in [fixture, Fixture::new()] {
        for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
            let artifact = fixture.artifact(mode);
            let text = serde_json::to_string(&artifact).unwrap().to_lowercase();
            for word in ["unauthenticated", "unprotected", "\"authenticated\""] {
                assert!(!text.contains(word), "{word}");
            }
            assert!(
                artifact
                    .content
                    .contains("## Endpoints with none recovered")
            );
            assert!(
                artifact
                    .content
                    .contains("**None recovered means exactly that.**")
            );
        }
    }
}

// AC-0656 (#1820): one fact, one wording. The route with no recovered check
// is under "none recovered" in endpoint-permissions.md and under "No
// authentication check recovered" in security.md, which never calls it an
// unauthenticated endpoint.
#[test]
fn security_and_endpoint_permissions_word_a_missing_check_alike() {
    let fixture = three_routes();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let bundle = compile_spec(&fixture.nodes, &fixture.edges, &[], mode, &BTreeSet::new());
        let content = |file: &str| {
            &bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.file_name == file)
                .unwrap_or_else(|| panic!("{file} is exported"))
                .content
        };

        let permissions = content("endpoint-permissions.md");
        assert!(row(section(permissions, NONE), "/health").is_some());

        let security = content("security.md");
        let finding = security
            .lines()
            .find(|line| line.starts_with("| No authentication check recovered: GET /health |"))
            .expect("the same route is a security finding under the recovered wording");
        assert!(finding.contains("| InferredStrong |"));
        assert!(!security.to_lowercase().contains("unauthenticated"));
        assert!(finding.contains("| `no_auth_check_recovered` |"));
        assert!(security.contains("is not visible to this finding"));
        // The guarded route and the route outside recovery are not findings.
        assert!(!security.contains("GET /me |"));
        assert!(!security.contains("GET /orders |"));
    }
}

/// `security.md` with one Capability citing the `HANDLES` edge of each
/// endpoint in `fixture`.
fn security_with_capability(fixture: &Fixture, mode: ExportMode) -> String {
    use crate::workbench::capabilities::{Stories, Story, Support};
    use core_graph::overlay::BaseRef;
    let handles: Vec<&Edge> = fixture
        .edges
        .iter()
        .filter(|e| e.label == "HANDLES")
        .collect();
    let cites: Vec<BaseRef> = handles
        .iter()
        .map(|e| BaseRef::Edge(e.src.clone(), e.dst.clone(), e.label.clone()))
        .collect();
    let capability = Node {
        id: "capability:test:orders".into(),
        label: "Capability".into(),
        props: json!({ "title": "Manage orders (shop API)" }),
    };
    let knowledge = Stories {
        included: vec![Story {
            node: &capability,
            support: cites
                .iter()
                .zip(&handles)
                .map(|(cite, edge)| (cite, Support::Edge(edge)))
                .collect(),
        }],
        filtered: 0,
        areas: BTreeMap::new(),
        file_areas: BTreeMap::new(),
    };
    let nodes: Vec<&Node> = fixture.nodes.iter().collect();
    let edges: Vec<&Edge> = fixture.edges.iter().collect();
    crate::workbench::producers::security(
        (&nodes, &edges),
        Some(&knowledge),
        (mode, &BTreeSet::new()),
        None,
    )
    .unwrap()
    .content
}

// AC-0853: security.md lists under each Capability who may call each of its
// operations, as the recovered site states: a Flask guard, a NestJS Allow
// permission set, none recovered, or not established outside recovery.
#[test]
fn security_lists_access_control_per_capability() {
    let mut fixture = three_routes();
    let ts = adapters_lang_ts::extract_source(
        b"import { Query, Resolver } from '@nestjs/graphql';\nimport { Permission } from '@vendure/common/lib/generated-shop-types';\nimport { Allow } from '../../decorators/allow.decorator';\n@Resolver() export class ShopOrderResolver {\n@Query()\n@Allow(Permission.Owner, Permission.UpdateOrder)\nactiveOrder() {}\n}",
        "src/api/resolvers/shop/shop-order.resolver.ts",
        &adapters_lang_ts::SourceId {
            repo: "acme/shop",
            commit: "0123abc",
        },
    )
    .unwrap();
    fixture.nodes.extend(ts.nodes);
    fixture.edges.extend(ts.edges);
    let content = security_with_capability(&fixture, ExportMode::BestEffort);
    let section = section(&content, "Access control per capability");
    assert!(
        section.contains("### Manage orders (shop API)"),
        "{section}"
    );
    assert!(
        section.contains(
            "2 with a recovered guard or permission set, 1 with none recovered, 1 not established"
        ),
        "{section}"
    );
    let line = |op: &str| {
        section
            .lines()
            .find(|l| l.starts_with(&format!("| {op} |")))
            .unwrap_or_else(|| panic!("{op}: {section}"))
            .to_owned()
    };
    assert!(line("GET /me").contains("| flask_login.login_required |"));
    assert!(line("GET /health").contains("| none recovered |"));
    assert!(line("GET /orders").contains("| not established: outside permission-site recovery |"));
    assert!(line("QUERY shop.activeOrder").contains("| any of Owner, UpdateOrder |"));
    // Each row cites its endpoint fact.
    let id = citation_id(&FactKey::Node {
        id: "ep:acme/shop@GET:/me".into(),
    });
    assert!(line("GET /me").ends_with(&format!("| {id} |")));
    // Without derived Capabilities the section is not written.
    let nodes: Vec<&Node> = fixture.nodes.iter().collect();
    let edges: Vec<&Edge> = fixture.edges.iter().collect();
    let plain = crate::workbench::producers::security(
        (&nodes, &edges),
        None,
        (ExportMode::BestEffort, &BTreeSet::new()),
        None,
    )
    .unwrap()
    .content;
    assert!(!plain.contains("Access control per capability"));
}

/// A Flask application from real Python adapter output: a repository guard
/// returning 401, Flask-Login, and an unguarded route (#1979).
fn flask_callers() -> (Vec<Node>, Vec<Edge>) {
    let dir = tempfile::tempdir().unwrap();
    let write = |path: &str, source: &str| {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    };
    write(
        "app/base.py",
        r#"
from functools import wraps
from flask import Blueprint, jsonify

api_bp = Blueprint("api", __name__, url_prefix="/api")

def authorize():
    if missing():
        return jsonify(error="Wrong api key"), 401
    return None

def require_api_auth(f):
    @wraps(f)
    def decorated(*args, **kwargs):
        error_return = authorize()
        if error_return:
            return error_return
        return f(*args, **kwargs)
    return decorated
"#,
    );
    write(
        "app/views.py",
        r#"
from flask import jsonify, render_template
from flask_login import login_required
from app.base import api_bp, require_api_auth

@api_bp.route("/aliases/<int:alias_id>", methods=["DELETE"])
@require_api_auth
def delete_alias(alias_id):
    return jsonify(deleted=True)

@api_bp.route("/coupon", methods=["GET", "POST"])
@login_required
def coupon_route():
    return render_template("coupon.html")

@api_bp.route("/health")
def health():
    return jsonify(ok=True)
"#,
    );
    write(
        "app/main.py",
        "from flask import Flask\nfrom app.base import api_bp\napp = Flask(__name__)\napp.register_blueprint(api_bp)\n",
    );
    let out = adapters_lang_python::extract_dir(
        dir.path(),
        &adapters_lang_python::SourceId {
            repo: "acme/shop",
            commit: "0123abc",
        },
    )
    .unwrap();
    (out.nodes, out.edges)
}

/// The guarded Flask-Admin view endpoint from real Python adapter output: a
/// `BaseView` whose `is_accessible` requires a signed-in user, on checked
/// Flask-Admin and flask-login releases.
fn flask_admin_view() -> Node {
    let dir = tempfile::tempdir().unwrap();
    for (path, source) in [
        (
            "app.py",
            r#"
from flask import Flask
from flask_admin import Admin, BaseView, expose
from flask_login import current_user

class Guarded(BaseView):
    def is_accessible(self):
        return current_user.is_authenticated

class Reports(Guarded):
    @expose("/")
    def index(self):
        return "ok"

app = Flask(__name__)
admin = Admin(app, url="/back")
admin.add_view(Reports(endpoint="reports"))
"#,
        ),
        (
            "auth.py",
            "from flask_login import LoginManager\n\nlogin_manager = LoginManager()\n",
        ),
        (
            "uv.lock",
            "[[package]]\nname = \"flask-admin\"\nversion = \"1.5.7\"\n\n[[package]]\nname = \"flask-login\"\nversion = \"0.5.0\"\n",
        ),
    ] {
        std::fs::write(dir.path().join(path), source).unwrap();
    }
    let out = adapters_lang_python::extract_dir(
        dir.path(),
        &adapters_lang_python::SourceId {
            repo: "acme/shop",
            commit: "0123abc",
        },
    )
    .unwrap();
    out.nodes
        .into_iter()
        .find(|n| n.label == "Endpoint" && n.props["path"] == "/back/reports/")
        .unwrap()
}

// AC-0856, AC-1603: a Python endpoint's guard reading names its caller in D1 §1.2,
// in its user story and, by the guard's own name, in `security.md` and
// `endpoint-permissions.md`.
#[test]
fn python_callers_are_named_by_their_recovered_guards() {
    use crate::workbench::capabilities::{Stories, Story, Support};
    use core_graph::overlay::BaseRef;
    let (nodes, edges) = flask_callers();
    let endpoints: Vec<&Node> = nodes.iter().filter(|n| n.label == "Endpoint").collect();
    assert_eq!(endpoints.len(), 4, "{endpoints:?}");

    // D1 §1.2: one row per caller, most operations first, any caller last.
    let bundle = compile_spec(
        &nodes,
        &edges,
        &[],
        ExportMode::BestEffort,
        &BTreeSet::new(),
    );
    let d1 = &bundle
        .artifacts
        .iter()
        .find(|a| a.id == "d1-product-specification")
        .unwrap()
        .content;
    let actors = &d1[d1.find("## 1.2 ").unwrap()..d1.find("## 1.3 ").unwrap()];
    assert!(
        actors.contains(
            "Callers by recovered guard: 2 callers over 3 guarded operations; 1 operations with no guard recovered."
        ),
        "{actors}"
    );
    let (callers, checks) = actors
        .split_once("### What each guard checks")
        .expect("the repository guard's checks follow the callers (AC-0901)");
    // AC-0901: the repository guard's wrapper exit and the exit of what it
    // calls are listed under it; the library guard is not.
    assert!(checks.contains("#### require\\_api\\_auth\n"), "{checks}");
    assert!(
        checks.contains("when error\\_return: returns error\\_return |"),
        "{checks}"
    );
    assert!(!checks.contains("login\\_required"), "{checks}");
    let rows: Vec<&str> = callers
        .lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| Caller |"))
        .collect();
    // AC-0907: each caller is named in words; the guard is the evidence.
    for row in &rows {
        let caller = row.split(" | ").next().unwrap().trim_start_matches("| ");
        assert_eq!(
            crate::workbench::neutrality::projected_code_token(&caller.replace('\\', "")),
            None,
            "{row}"
        );
    }
    // AC-1603: each row cites the operations it shows, in the order shown.
    let id_of = |method: &str, path: &str| {
        let endpoint = endpoints
            .iter()
            .find(|n| n.props["method"] == method && n.props["path"] == path)
            .unwrap();
        crate::citation_id(&FactKey::Node {
            id: endpoint.id.clone(),
        })
    };
    let (rows, citations): (Vec<&str>, Vec<&str>) = rows
        .iter()
        .map(|row| {
            let (head, cited) = row.trim_end_matches(" |").rsplit_once(" | ").unwrap();
            (head, cited)
        })
        .unzip();
    assert_eq!(
        citations,
        [
            format!(
                "{}, {}",
                id_of("GET", "/api/coupon"),
                id_of("POST", "/api/coupon")
            ),
            id_of("DELETE", "/api/aliases/<int:alias_id>"),
            id_of("GET", "/api/health"),
        ]
    );
    for id in citations.iter().flat_map(|cell| cell.split(", ")) {
        assert!(d1.contains(id), "{id}");
    }
    let rows: Vec<String> = rows.iter().map(|row| format!("{row} |")).collect();
    assert_eq!(
        rows,
        [
            "| Signed-in user | flask\\_login.login\\_required | Confirmed | 2 | GET /api/coupon; POST /api/coupon |",
            "| API client | require\\_api\\_auth | InferredStrong | 1 | DELETE /api/aliases/&lt;int:alias\\_id&gt; |",
            "| Any caller | none recovered | InferredStrong | 1 | GET /api/health |",
        ],
        "{actors}"
    );

    // User stories: the caller from the guard reading, the method's verb for
    // a handler named for its subject, an action word kept as the verb.
    let cited_nodes: Vec<(BaseRef, &Node)> = nodes
        .iter()
        .filter(|n| n.label == "Endpoint" || n.label == "Symbol")
        .map(|n| (BaseRef::Node(n.id.clone()), n))
        .collect();
    let cited_edges: Vec<(BaseRef, &Edge)> = edges
        .iter()
        .filter(|e| e.label == "HANDLES")
        .map(|e| {
            (
                BaseRef::Edge(e.src.clone(), e.dst.clone(), e.label.clone()),
                e,
            )
        })
        .collect();
    let capability = Node {
        id: "capability:test:api".into(),
        label: "Capability".into(),
        props: json!({ "title": "Manage API" }),
    };
    let knowledge = Stories {
        included: vec![Story {
            node: &capability,
            support: cited_nodes
                .iter()
                .map(|(c, n)| (c, Support::Node(n)))
                .chain(cited_edges.iter().map(|(c, e)| (c, Support::Edge(e))))
                .collect(),
        }],
        filtered: 0,
        areas: BTreeMap::new(),
        file_areas: BTreeMap::new(),
    };
    let refs: Vec<&Node> = nodes.iter().collect();
    let stories = crate::workbench::producers::stories_with_support(
        &refs,
        Some(&knowledge),
        (ExportMode::BestEffort, &BTreeSet::new()),
        None,
    )
    .unwrap()
    .content;
    for line in [
        "- As a signed-in user, I can view coupon (GET /api/coupon).",
        "- As a signed-in user, I can submit coupon (POST /api/coupon).",
        "- As an API client, I can delete alias (DELETE /api/aliases/&lt;int:alias\\_id&gt;).",
        "- As any caller (no guard recovered), I can view health (GET /api/health).",
    ] {
        assert!(stories.contains(line), "{line}\n{stories}");
    }

    // A rejected inferred guard reading names no caller.
    let guard = endpoints
        .iter()
        .find(|n| n.props["method"] == "DELETE")
        .unwrap();
    assert!(super::caller(guard, ExportMode::BestEffort, &BTreeSet::new()).is_some());
    let rejected = BTreeSet::from([guard.props["auth"]["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned()]);
    assert!(super::caller(guard, ExportMode::BestEffort, &rejected).is_none());
    // So does a rejected inferred "no guard" reading.
    let open = endpoints
        .iter()
        .find(|n| n.props["path"] == "/api/health")
        .unwrap();
    assert_eq!(
        super::caller(open, ExportMode::BestEffort, &BTreeSet::new()),
        Some(super::Caller::Unguarded)
    );
    let rejected = BTreeSet::from([open.props["auth"]["prov"]["content_hash"]
        .as_str()
        .unwrap()
        .to_owned()]);
    assert!(super::caller(open, ExportMode::BestEffort, &rejected).is_none());
    // Every operation a caller row counts is a statement of D1, with its tier.
    let statements = &d1[d1.find("## Statements in this document").unwrap()..];
    assert!(
        // Four operations and the guard's two exit rows (AC-0901).
        statements.contains("Cited facts printed in the frames above: 6."),
        "{statements}"
    );
    for operation in ["GET /api/health", "GET /api/coupon", "POST /api/coupon"] {
        assert!(
            statements.contains(&format!("| Endpoint: {operation} | Deterministic |")),
            "{statements}"
        );
    }

    // A Flask-Admin view, recorded as a `flask` endpoint under the
    // `flask_admin` mount, is admitted by its view's `is_accessible`.
    let admin = flask_admin_view();
    assert_eq!(
        super::caller(&admin, ExportMode::BestEffort, &BTreeSet::new()),
        Some(super::Caller::Guarded {
            role: "administrator".into(),
            guards: vec!["Guarded.is_accessible".into()],
        })
    );

    // Guards by their own names, never graph ids.
    let permissions = &bundle
        .artifacts
        .iter()
        .find(|a| a.id == ID)
        .unwrap()
        .content;
    assert!(
        permissions.contains("guard: require\\_api\\_auth (basis: "),
        "{permissions}"
    );
    assert!(!permissions.contains("guard: sym:"), "{permissions}");
}

// AC-0906: a repository guard's caller is named by the role its name
// speaks of, the guard kept as the evidence.
#[test]
fn a_repository_guard_names_a_role() {
    for (guard, role) in [
        ("require_api_auth", "API client"),
        ("require_api_sudo", "API client in sudo mode"),
        ("admin_required", "administrator"),
        ("validate_twilio_request", "Twilio webhook"),
        ("verify_stripe_signature", "Stripe webhook"),
        ("auth.login_required_custom", "signed-in user"),
        ("check_quota", "caller the “check quota” check admits"),
    ] {
        assert_eq!(super::guard_role(guard), role, "{guard}");
    }
}

// AC-1304 (#2163): a Go route's middleware guard is its permission site, and
// a Go route with no recovered guard is never listed as "none recovered".
#[test]
fn go_middleware_guards_are_permission_sites_and_go_is_never_none() {
    let mut fixture = Fixture::new();
    fixture.route(
        "/me",
        "chi",
        "go",
        (10, 40),
        json!({
            "state": "required", "authenticated": true,
            "guards": ["jwtauth.Authenticator"],
            "basis": ["library guard github.com/go-chi/jwtauth/v5.Authenticator"],
            "prov": confirmed("main.go", (50, 71), "auth"),
        }),
    );
    fixture.route(
        "/feed",
        "gin",
        "go",
        (80, 120),
        json!({"state": "unknown", "reasons": ["no_guard_recovered"]}),
    );
    let content = fixture.artifact(ExportMode::VerifiedOnly).content;
    let guarded = row(section(&content, SITED), "/me").expect("Go guard is a site");
    assert!(
        guarded.contains("guard: jwtauth.Authenticator"),
        "{guarded}"
    );
    assert!(
        guarded
            .contains("| Deterministic | Confirmed | acme/shop:main.go bytes 50..71 @ 0123abc |")
    );
    for other in [NONE, OUTSIDE] {
        assert!(row(section(&content, other), "/feed").is_none(), "{other}");
    }
    assert!(content.contains("/feed"), "{content}");
}

// AC-1305 (#2163): a Rust extractor or layer guard is its permission site and
// names its caller; a Rust route with none is never "none recovered".
#[test]
fn rust_auth_guards_are_permission_sites_and_rust_is_never_none() {
    let mut fixture = Fixture::new();
    fixture.route(
        "/me",
        "axum",
        "rust",
        (10, 40),
        json!({
            "state": "required", "authenticated": true,
            "guards": ["TypedHeader<Authorization<Bearer>>"],
            "basis": ["library extractor TypedHeader<Authorization<Bearer>>"],
            "prov": confirmed("src/lib.rs", (50, 71), "auth"),
        }),
    );
    fixture.route(
        "/feed",
        "actix-web",
        "rust",
        (80, 120),
        json!({"state": "unknown", "reasons": ["no_guard_recovered"]}),
    );
    let content = fixture.artifact(ExportMode::VerifiedOnly).content;
    let guarded = row(section(&content, SITED), "/me").expect("Rust guard is a site");
    assert!(
        guarded.contains("guard: TypedHeader&lt;Authorization&lt;Bearer&gt;&gt;"),
        "{guarded}"
    );
    for other in [NONE, OUTSIDE] {
        assert!(row(section(&content, other), "/feed").is_none(), "{other}");
    }
    assert!(content.contains("/feed"), "{content}");
    for (guard, role) in [
        (
            "actix_web_httpauth.BearerAuth",
            "caller with a bearer token",
        ),
        (
            "tower_http.ValidateRequestHeaderLayer::basic",
            "caller with HTTP Basic credentials",
        ),
    ] {
        let endpoint = &fixture.nodes[0];
        assert_eq!(super::role(endpoint, &[guard]), role, "{guard}");
    }
}
