//! `endpoint-inventory.md` (#1658, AC-0694).
use super::*;
use crate::{
    FactIndex, SelectedArtifactLimit, SelectedArtifactOutcome, SelectedCompileLimits, SpecArtifact,
    citation_id, compile_spec, compile_spec_selected, try_compile_spec_with_coverage,
};
use core_graph::{Edge, FactKey};
use core_prov::{EvidenceRef, Tier};
use serde_json::{Value, json};

const ID: &str = "endpoint-inventory";
const MARK: &str = "| rests on an InferredWeak fact; a verified-only export omits this row |";

fn prov(tier: Tier, confidence: ConfidenceTier, span: (u64, u64), fact: &str) -> Value {
    let evidence = EvidenceRef {
        repo: "acme/shop".into(),
        path: "app.py".into(),
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

fn confirmed(span: (u64, u64), fact: &str) -> Value {
    prov(Tier::Deterministic, ConfidenceTier::Confirmed, span, fact)
}

/// An endpoint in the shape the language adapters emit.
fn endpoint(id: &str, props: Value, span: (u64, u64)) -> Node {
    let mut props = props;
    props["prov"] = confirmed(span, id);
    Node {
        id: id.into(),
        label: "Endpoint".into(),
        props,
    }
}

/// A tested endpoint, an isolated one no test references, one that records
/// no method or path, and the test that references the first.
fn fixture() -> (Vec<Node>, Vec<Edge>) {
    let nodes = vec![
        endpoint(
            "ep:acme/shop@GET:/orders",
            json!({"method": "GET", "path": "/orders", "framework": "flask", "language": "python"}),
            (10, 40),
        ),
        endpoint(
            "ep:acme/shop@POST:/refunds",
            json!({"method": "POST", "path": "/refunds", "framework": "flask", "language": "python"}),
            (50, 90),
        ),
        endpoint(
            "ep:acme/shop@route:7",
            json!({"framework": "express", "language": "typescript"}),
            (100, 120),
        ),
        Node {
            id: "test:acme/shop@tests/test_orders.py#test_list".into(),
            label: "TestCase".into(),
            props: json!({
                "name": "test_list",
                "prov": confirmed((200, 240), "test_list"),
            }),
        },
    ];
    let edges = vec![Edge {
        src: "test:acme/shop@tests/test_orders.py#test_list".into(),
        dst: "ep:acme/shop@GET:/orders".into(),
        label: "REQUESTS".into(),
        props: json!({"prov": confirmed((210, 230), "requests")}),
    }];
    (nodes, edges)
}

fn find(bundle: crate::SpecBundle) -> SpecArtifact {
    bundle
        .artifacts
        .into_iter()
        .find(|artifact| artifact.id == ID)
        .expect("the endpoint inventory is exported")
}

fn artifact(nodes: &[Node], edges: &[Edge], mode: ExportMode) -> SpecArtifact {
    find(compile_spec(nodes, edges, &[], mode, &BTreeSet::new()))
}

fn row<'a>(content: &'a str, id: &str) -> &'a str {
    content
        .lines()
        .find(|line| line.starts_with(&format!("| {id} |")))
        .unwrap_or_else(|| panic!("{id} has a row"))
}

// AC-0694: every recovered endpoint is listed, including one no test
// references, with method and path as recorded or unknown, and tier,
// confidence and span; no authentication or response schema is stated.
#[test]
fn every_endpoint_is_listed_including_isolated_ones_and_absent_fields_are_unknown() {
    let (nodes, edges) = fixture();
    let content = artifact(&nodes, &edges, ExportMode::BestEffort).content;
    assert!(content.contains("Endpoints listed: 3. Recorded as a Gap: 0."));
    assert_eq!(
        row(&content, "ep:acme/shop@GET:/orders"),
        "| ep:acme/shop@GET:/orders | GET | /orders | flask | python | Deterministic | \
         Confirmed | acme/shop:app.py bytes 10..40 @ 0123abc | — |"
    );
    assert!(row(&content, "ep:acme/shop@POST:/refunds").contains("| POST | /refunds | flask |"));
    assert!(
        row(&content, "ep:acme/shop@route:7")
            .starts_with("| ep:acme/shop@route:7 | unknown | unknown | express | typescript |")
    );
    // The tested endpoint's test is not this document's subject.
    assert!(!content.contains("test_list"));
    for absent in ["unauthenticated", "requires auth", "response schema:"] {
        assert!(!content.contains(absent), "{absent}");
    }
}

// AC-0694 (R-INT-5): verified-only leaves a weak endpoint out and counts
// it; best-effort marks it; a rejected inferred one is withheld in both.
#[test]
fn verified_only_omits_and_counts_weak_rows_and_best_effort_marks_them() {
    let (mut nodes, edges) = fixture();
    nodes[1].props["prov"] = prov(
        Tier::Agentic,
        ConfidenceTier::InferredWeak,
        (50, 90),
        "weak refunds",
    );
    let verified = artifact(&nodes, &edges, ExportMode::VerifiedOnly);
    assert!(verified.content.contains("Endpoints listed: 2."));
    assert!(verified.content.contains(
        "Endpoint facts left out of this export because they are InferredWeak or rejected: 1."
    ));
    assert!(!verified.content.contains("/refunds"));
    assert!(
        verified
            .assertions
            .iter()
            .all(|a| a.provenance.confidence_tier != ConfidenceTier::InferredWeak)
    );

    let best = artifact(&nodes, &edges, ExportMode::BestEffort).content;
    let weak = row(&best, "ep:acme/shop@POST:/refunds");
    assert!(weak.contains("| Agentic | InferredWeak |") && weak.ends_with(MARK));
    assert_eq!(best.matches(MARK).count(), 1);
    assert!(best.contains("because they are rejected: 0."));

    let strong = prov(
        Tier::Semantic,
        ConfidenceTier::InferredStrong,
        (50, 90),
        "strong refunds",
    );
    let hash = strong["content_hash"].as_str().unwrap().to_owned();
    nodes[1].props["prov"] = strong;
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let rejected = find(compile_spec(
            &nodes,
            &edges,
            &[],
            mode,
            &BTreeSet::from([hash.clone()]),
        ));
        assert!(rejected.content.contains("rejected: 1."));
        assert!(!rejected.content.contains("/refunds"));
    }

    // An endpoint recorded as a Gap is counted, never listed as recovered.
    let (mut nodes, edges) = fixture();
    nodes[2].props["prov"] = prov(Tier::Deterministic, ConfidenceTier::Gap, (100, 120), "gap");
    let content = artifact(&nodes, &edges, ExportMode::BestEffort).content;
    assert!(content.contains("Endpoints listed: 2. Recorded as a Gap: 1."));
    assert!(!content.contains("| ep:acme/shop@route:7 |"));
}

// AC-0694: recorded text cannot break the table or inject Markdown.
#[test]
fn recorded_text_is_escaped() {
    let nodes = vec![endpoint(
        "ep:acme/shop@GET:/a|b",
        json!({"method": "GET", "path": "/a|b\n# heading <b>x</b> `c`", "framework": "flask"}),
        (1, 2),
    )];
    let content = artifact(&nodes, &[], ExportMode::BestEffort).content;
    let table: Vec<&str> = content
        .lines()
        .skip_while(|line| *line != "## Endpoints")
        .skip(4)
        .take_while(|line| line.starts_with('|'))
        .collect();
    assert_eq!(table.len(), 1, "{content}");
    assert!(!content.lines().any(|line| line.starts_with("# heading")));
    assert!(!content.contains("<b>"), "{content}");
    assert!(table[0].starts_with(
        "| ep:acme/shop@GET:/a\\|b | GET | /a\\|b ⏎ \\# heading &lt;b&gt;x&lt;/b&gt; \\`c\\` | \
         flask | unknown |"
    ));
}

// AC-0694 (ADR-0058): every row is an assertion whose fact resolves in the
// export's fact index with the same tier, confidence and evidence.
#[test]
fn every_row_cites_a_fact_that_resolves_in_the_fact_index() {
    let (nodes, edges) = fixture();
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let bundle =
            try_compile_spec_with_coverage(&nodes, &edges, &[], mode, &BTreeSet::new(), &[])
                .expect("the citations resolve");
        let artifact = bundle.artifacts.iter().find(|a| a.id == ID).unwrap();
        assert_eq!(artifact.assertions.len(), 3);
        assert!(
            artifact
                .content
                .contains("## Assertions and inline provenance")
        );
        let index = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "fact-index-json")
            .unwrap();
        let index: FactIndex = serde_json::from_str(&index.content).unwrap();
        for assertion in &artifact.assertions {
            let key = FactKey::Node {
                id: assertion.subject_id.clone(),
            };
            let row = index
                .rows
                .iter()
                .find(|row| row.fact == key)
                .unwrap_or_else(|| panic!("{} has a row", assertion.subject_id));
            assert_eq!(row.citation_id, citation_id(&key));
            assert!(row.cited_by.iter().any(|by| by == ID));
            assert!(row.records.iter().any(|record| {
                record.tier == assertion.provenance.tier
                    && record.confidence == assertion.provenance.confidence_tier
                    && record.evidence == assertion.provenance.evidence
            }));
        }
    }
}

// AC-0694: the same graph in any order gives the same bytes, the artifact
// compiled alone equals the bundle's, and the output bound refuses it whole.
#[test]
fn deterministic_selected_alone_and_bounded() {
    let (nodes, edges) = fixture();
    let limits = |max_bytes| SelectedCompileLimits {
        max_bytes,
        adr: crate::AdrLimits {
            max_rows: usize::MAX,
            max_output_bytes: usize::MAX,
        },
    };
    for mode in [ExportMode::VerifiedOnly, ExportMode::BestEffort] {
        let first = artifact(&nodes, &edges, mode);
        let reversed: Vec<Node> = nodes.iter().rev().cloned().collect();
        let mut doubled = reversed.clone();
        doubled.push(nodes[0].clone());
        assert_eq!(artifact(&reversed, &edges, mode), first);
        assert_eq!(artifact(&doubled, &edges, mode), first);
        assert!(first.verify_identity());

        let select = |max_bytes| {
            compile_spec_selected(
                &reversed,
                &edges,
                &[],
                mode,
                &BTreeSet::new(),
                &[ID],
                limits(max_bytes),
            )
            .unwrap()
        };
        assert_eq!(
            select(usize::MAX),
            [SelectedArtifactOutcome::Produced(first.clone())]
        );
        let size = first.content.len() + serde_json::to_vec(&first.assertions).unwrap().len();
        assert_eq!(
            select(size),
            [SelectedArtifactOutcome::Produced(first.clone())]
        );
        assert_eq!(
            select(size - 1),
            [SelectedArtifactOutcome::Limited {
                artifact_id: ID.into(),
                limit: SelectedArtifactLimit::OutputBytes {
                    remaining: size - 1
                },
            }]
        );
    }
}

// AC-0694: an export with no endpoint says so in the table.
#[test]
fn an_export_with_no_endpoint_says_none() {
    let content = artifact(&[], &[], ExportMode::VerifiedOnly).content;
    assert!(content.contains("Endpoints listed: 0. Recorded as a Gap: 0."));
    assert!(content.contains("| — | — | — | — | — | — | — | — | None in this export |"));
}

/// The fixture with the shapes the Python adapter records on the first two
/// endpoints: inputs and responses on one, a partial response on the other.
fn shaped() -> (Vec<Node>, Vec<Edge>) {
    let (mut nodes, edges) = fixture();
    nodes[0].props["request_inputs"] = json!({
        "query": ["page", "q"],
        "body": ["note"],
        "scope": "handler_body",
        "prov": confirmed((12, 20), "inputs of list"),
    });
    nodes[0].props["responses"] = json!({
        "kinds": ["json"],
        "statuses": [200, 400],
        "keys": ["error", "orders"],
        "scope": "handler_body",
        "prov": confirmed((30, 38), "responses of list"),
    });
    nodes[0].props["rate_limits"] = json!({
        "entries": [{
            "text": "limiter.limit(ALIAS_LIMIT)",
            "value": "100/day;50/hour;5/minute",
            "limits": [
                {"count": 100, "per": 1, "unit": "day"},
                {"count": 50, "per": 1, "unit": "hour"},
                {"count": 5, "per": 1, "unit": "minute"},
            ],
            "source": "environment_default",
            "environment_key": "ALIAS_LIMIT",
        }],
        "prov": prov(Tier::Deterministic, ConfidenceTier::InferredStrong, (5, 9), "limits of list"),
    });
    nodes[1].props["responses"] = json!({
        "kinds": ["json"],
        "statuses": [201],
        "partial": ["response_built_elsewhere", "status_computed"],
        "scope": "handler_body",
        "prov": confirmed((60, 80), "responses of refund"),
    });
    (nodes, edges)
}

fn contract_row<'a>(content: &'a str, id: &str) -> &'a str {
    content
        .lines()
        .skip_while(|line| *line != "## Requests and responses")
        .find(|line| line.starts_with(&format!("| {id} |")))
        .unwrap_or_else(|| panic!("{id} has a contract row"))
}

// AC-0890: each endpoint's recorded request inputs and responses are
// printed in D4 §4.2 terms, with the reasons the adapter gave for not
// proving more, and an endpoint with neither reads "not recovered".
#[test]
fn request_inputs_and_responses_are_printed_per_endpoint() {
    let (nodes, edges) = shaped();
    let content = artifact(&nodes, &edges, ExportMode::BestEffort).content;
    assert!(content.contains("Endpoints with a recorded request or response shape: 2 of 3."));
    let cite = |id: &str| citation_id(&FactKey::Node { id: id.into() });
    assert_eq!(
        contract_row(&content, "ep:acme/shop@GET:/orders"),
        format!(
            "| ep:acme/shop@GET:/orders | GET | /orders | query page, q; body note | \
             200, 400 (json) | error, orders | at most 100 a day, 50 an hour, 5 a minute (the \
             default; the environment variable ALIAS\\_LIMIT replaces it) | — | {}, {}, {} |",
            cite("request-inputs:ep:acme/shop@GET:/orders"),
            cite("responses:ep:acme/shop@GET:/orders"),
            cite("rate-limits:ep:acme/shop@GET:/orders"),
        )
    );
    assert_eq!(
        contract_row(&content, "ep:acme/shop@POST:/refunds"),
        format!(
            "| ep:acme/shop@POST:/refunds | POST | /refunds | not recovered | 201 (json) | — | \
             none recorded | a JSON body is built outside the handler; a status is computed | {} |",
            cite("responses:ep:acme/shop@POST:/refunds"),
        )
    );
    assert_eq!(
        contract_row(&content, "ep:acme/shop@route:7"),
        "| ep:acme/shop@route:7 | unknown | unknown | not recovered | not recovered | — | \
         none recorded | — | — |"
    );
}

// AC-0890 (ADR-0058): each printed shape is its own assertion and resolves
// in the fact index under the id the row prints; a weak shape is left out
// of verified-only.
#[test]
fn printed_shapes_resolve_in_the_fact_index_and_follow_the_mode() {
    let (mut nodes, edges) = shaped();
    let bundle = try_compile_spec_with_coverage(
        &nodes,
        &edges,
        &[],
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
        &[],
    )
    .expect("the citations resolve");
    let artifact_out = bundle.artifacts.iter().find(|a| a.id == ID).unwrap();
    let index = bundle
        .artifacts
        .iter()
        .find(|a| a.id == "fact-index-json")
        .unwrap();
    let index: FactIndex = serde_json::from_str(&index.content).unwrap();
    for id in [
        "rate-limits:ep:acme/shop@GET:/orders",
        "request-inputs:ep:acme/shop@GET:/orders",
        "responses:ep:acme/shop@GET:/orders",
        "responses:ep:acme/shop@POST:/refunds",
    ] {
        assert!(
            artifact_out.assertions.iter().any(|a| a.subject_id == id),
            "{id}"
        );
        let key = FactKey::Node { id: id.into() };
        let row = index.rows.iter().find(|row| row.fact == key).unwrap();
        assert!(artifact_out.content.contains(&row.citation_id));
    }

    nodes[1].props["responses"]["prov"] = prov(
        Tier::Agentic,
        ConfidenceTier::InferredWeak,
        (60, 80),
        "weak responses",
    );
    let verified = artifact(&nodes, &edges, ExportMode::VerifiedOnly).content;
    assert_eq!(
        contract_row(&verified, "ep:acme/shop@POST:/refunds"),
        "| ep:acme/shop@POST:/refunds | POST | /refunds | not recovered | not recovered | — | \
         none recorded | — | — |"
    );
    let best = artifact(&nodes, &edges, ExportMode::BestEffort).content;
    assert!(contract_row(&best, "ep:acme/shop@POST:/refunds").contains("| 201 (json) |"));
}
