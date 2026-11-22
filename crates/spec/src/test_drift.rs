//! Test and contract drift (#953, AC-0345).
//!
//! Compares what target tests expect of an endpoint (the statuses asserted
//! and the response keys read, carried on `REQUESTS` edges from `TestCase`
//! nodes) with what its recovered `responses` return. Both sides are static:
//! a test reference is not an execution, and a recovered response is bounded
//! by its recorded `scope`. Findings are emitted only where both sides are
//! complete for that scope; otherwise the endpoint is a Gap naming the side.

use crate::contract::ContractFacts;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};

/// Spec rule id for test and contract drift findings and their gaps.
pub const TEST_DRIFT_EXTRACTOR_ID: &str = "t0.test-drift";
/// Gap reason when the endpoint's recovered responses are partial.
pub const PARTIAL_RESPONSES_REASON: &str =
    "recovered responses are partial, so test expectations cannot be compared";
/// Gap reason when a referencing call follows redirects.
pub const FOLLOWS_REDIRECTS_REASON: &str =
    "a referencing test call follows redirects, so its expectations belong to another response";
/// Gap reason when the endpoint's recovered responses carry no valid
/// provenance, or only Gap provenance.
pub const UNPROVEN_RESPONSES_REASON: &str =
    "recovered responses carry no usable provenance, so the returned side cannot be cited";
/// Gap reason when the endpoint itself carries no usable provenance.
pub const UNPROVEN_ENDPOINT_REASON: &str =
    "the endpoint carries no usable provenance, so neither side can be attributed to it";
/// Why a returned status no test asserts is only InferredWeak.
pub const UNASSERTED_REASON: &str = "no resolved test reference asserts it; unresolved test requests and calls in helper functions are not read";

#[derive(Default)]
struct Expectations<'a> {
    /// Each referencing test with its `REQUESTS` provenance (one edge per
    /// test and endpoint).
    tests: BTreeMap<&'a str, Provenance>,
    statuses: BTreeSet<u64>,
    keys: BTreeSet<&'a str>,
    follows_redirects: bool,
}

fn typed_provenance(value: &serde_json::Value) -> Option<Provenance> {
    serde::Deserialize::deserialize(&*crate::stored_prov(value))
        .ok()
        .filter(|provenance: &Provenance| provenance.validate().is_ok())
}

/// Provenance that can carry a finding: valid, not a Gap, and citing
/// evidence (`validate` checks the tier ceiling, not evidence presence).
fn usable_provenance(value: &serde_json::Value) -> Option<Provenance> {
    typed_provenance(value)
        .filter(|p| p.confidence_tier != ConfidenceTier::Gap && !p.evidence.is_empty())
}

/// The weaker of two confidence tiers: a finding is never stronger than the
/// evidence on either side of it.
fn weaker(a: ConfidenceTier, b: ConfidenceTier) -> ConfidenceTier {
    let rank = |tier| match tier {
        ConfidenceTier::Confirmed => 0,
        ConfidenceTier::InferredStrong => 1,
        ConfidenceTier::InferredWeak => 2,
        ConfidenceTier::Gap => 3,
    };
    if rank(a) >= rank(b) { a } else { b }
}

fn merged_evidence<'a>(provenances: impl IntoIterator<Item = &'a Provenance>) -> Vec<EvidenceRef> {
    let mut map = BTreeMap::new();
    for evidence in provenances.into_iter().flat_map(|p| &p.evidence) {
        let key = (
            evidence.repo.clone(),
            evidence.path.clone(),
            evidence.byte_start,
            evidence.byte_end,
            evidence.commit_sha.clone(),
        );
        map.entry(key).or_insert_with(|| evidence.clone());
    }
    map.into_values().collect()
}

/// Provenance over `content`, the fact's canonical identity and content, so
/// a changed finding never inherits a curation keyed by its old hash.
fn rule_provenance(
    confidence: ConfidenceTier,
    evidence: Vec<EvidenceRef>,
    content: &[u8],
) -> Option<serde_json::Value> {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        evidence,
        TEST_DRIFT_EXTRACTOR_ID,
        content,
    )
    .ok()?
    .with_method(crate::spec_rule(TEST_DRIFT_EXTRACTOR_ID));
    serde_json::to_value(provenance).ok()
}

/// The canonical bytes of a fact's id and its props (before `prov`).
fn content(id: &str, props: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&(id, props)).expect("drift content serializes")
}

fn strings(value: &serde_json::Value) -> impl Iterator<Item = &str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
}

fn statuses(value: &serde_json::Value) -> BTreeSet<u64> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_u64)
        .collect()
}

fn endpoint_name(node: &Node) -> String {
    match (node.props["method"].as_str(), node.props["path"].as_str()) {
        (Some(method), Some(path)) => format!("{method} {path}"),
        _ => node.id.clone(),
    }
}

struct Finding<'a> {
    kind: &'static str,
    confidence: ConfidenceTier,
    statuses: Vec<u64>,
    keys: Vec<&'a str>,
    reason: Option<&'static str>,
}

/// Derive test and contract drift for one repository's recovered facts.
pub fn derive_test_drift(nodes: &[Node], edges: &[Edge]) -> ContractFacts {
    let tests: BTreeSet<&str> = nodes
        .iter()
        .filter(|n| n.label == "TestCase")
        .map(|n| n.id.as_str())
        .collect();
    let endpoints: BTreeMap<&str, &Node> = nodes
        .iter()
        .filter(|n| n.label == "Endpoint" && n.props["responses"].is_object())
        .map(|n| (n.id.as_str(), n))
        .collect();
    let mut expected: BTreeMap<&str, Expectations> = BTreeMap::new();
    for edge in edges {
        if edge.label != "REQUESTS"
            || !tests.contains(edge.src.as_str())
            || !endpoints.contains_key(edge.dst.as_str())
        {
            continue;
        }
        // A request without usable proof contributes nothing: response
        // evidence never stands in for the test's own.
        let Some(provenance) = usable_provenance(&edge.props) else {
            continue;
        };
        let entry = expected.entry(edge.dst.as_str()).or_default();
        entry.statuses.extend(statuses(&edge.props["statuses"]));
        entry.keys.extend(strings(&edge.props["keys"]));
        entry.follows_redirects |= edge.props["follows_redirects"] == true;
        entry.tests.insert(edge.src.as_str(), provenance);
    }
    let mut facts = ContractFacts::default();
    for (endpoint_id, expectations) in &expected {
        let endpoint = endpoints[endpoint_id];
        let responses = &endpoint.props["responses"];
        let response_provenance = usable_provenance(responses);
        let endpoint_provenance = usable_provenance(&endpoint.props);
        // The floor every side allows: the endpoint's, the response's and
        // every referencing test's tier.
        let floor = expectations
            .tests
            .values()
            .chain(endpoint_provenance.as_ref())
            .fold(
                response_provenance
                    .as_ref()
                    .map_or(ConfidenceTier::Gap, |p| p.confidence_tier),
                |floor, p| weaker(floor, p.confidence_tier),
            );
        let evidence = merged_evidence(
            expectations
                .tests
                .values()
                .chain(response_provenance.as_ref()),
        );
        let unproven = response_provenance.is_none();
        let response_evidence = response_provenance.map(|p| p.evidence).unwrap_or_default();
        let name = endpoint_name(endpoint);
        let partial: Vec<&str> = strings(&responses["partial"]).collect();
        let incomplete = if endpoint_provenance.is_none() {
            Some(UNPROVEN_ENDPOINT_REASON)
        } else if unproven {
            Some(UNPROVEN_RESPONSES_REASON)
        } else if !partial.is_empty() {
            Some(PARTIAL_RESPONSES_REASON)
        } else if expectations.follows_redirects {
            Some(FOLLOWS_REDIRECTS_REASON)
        } else {
            None
        };
        if let Some(reason) = incomplete {
            let id = format!("gap:test-drift:{endpoint_id}");
            let mut props = serde_json::json!({
                "kind": "test-drift-gap",
                "endpoint": name,
                "reason": reason,
                "partial": partial,
                "attempted_tiers": ["T0"],
            });
            let Some(prov) = rule_provenance(ConfidenceTier::Gap, evidence, &content(&id, &props))
            else {
                continue;
            };
            props["prov"] = prov;
            facts.nodes.push(Node {
                id: id.clone(),
                label: "Gap".into(),
                props,
            });
            push_edges(
                &mut facts,
                &id,
                (endpoint_id, &response_evidence),
                expectations,
                ConfidenceTier::Gap,
            );
            continue;
        }
        let returned = statuses(&responses["statuses"]);
        let returned_keys: BTreeSet<&str> = strings(&responses["keys"]).collect();
        let findings = [
            Finding {
                kind: "asserted-not-returned",
                confidence: ConfidenceTier::InferredStrong,
                statuses: expectations
                    .statuses
                    .difference(&returned)
                    .copied()
                    .collect(),
                keys: expectations
                    .keys
                    .difference(&returned_keys)
                    .copied()
                    .collect(),
                reason: None,
            },
            Finding {
                kind: "returned-not-asserted",
                confidence: ConfidenceTier::InferredWeak,
                statuses: returned
                    .difference(&expectations.statuses)
                    .copied()
                    .collect(),
                keys: Vec::new(),
                reason: Some(UNASSERTED_REASON),
            },
        ];
        for finding in findings {
            if finding.statuses.is_empty() && finding.keys.is_empty() {
                continue;
            }
            let id = format!("drift:test:{endpoint_id}@{}", finding.kind);
            let confidence = weaker(finding.confidence, floor);
            let mut props = serde_json::json!({
                "kind": "test-drift",
                "drift_kind": finding.kind,
                "endpoint": name,
                "title": format!("{}: {name}", finding.kind),
                "statuses": finding.statuses,
                "keys": finding.keys,
                "asserted_statuses": expectations.statuses,
                "returned_statuses": returned,
                "scope": responses["scope"],
                "tests": expectations.tests.keys().collect::<Vec<_>>(),
            });
            if let Some(reason) = finding.reason {
                props["reason"] = reason.into();
            }
            let Some(prov) = rule_provenance(confidence, evidence.clone(), &content(&id, &props))
            else {
                continue;
            };
            props["prov"] = prov;
            facts.nodes.push(Node {
                id: id.clone(),
                label: "Drift".into(),
                props,
            });
            push_edges(
                &mut facts,
                &id,
                (endpoint_id, &response_evidence),
                expectations,
                confidence,
            );
        }
    }
    facts
}

/// `SERVED_AS` to the endpoint, citing its responses, and `ASSERTED_BY` to
/// each referencing test, citing its calls.
fn push_edges(
    facts: &mut ContractFacts,
    src: &str,
    (endpoint, response_evidence): (&str, &[EvidenceRef]),
    expectations: &Expectations,
    confidence: ConfidenceTier,
) {
    let mut push = |dst: &str, label: &str, evidence: Vec<EvidenceRef>| {
        let identity = format!("{src} {label} {dst}");
        if let Some(prov) = rule_provenance(confidence, evidence, identity.as_bytes()) {
            facts.edges.push(Edge {
                src: src.into(),
                dst: dst.into(),
                label: label.into(),
                props: serde_json::json!({ "prov": prov }),
            });
        }
    };
    push(endpoint, "SERVED_AS", response_evidence.to_vec());
    for (test, provenance) in &expectations.tests {
        push(test, "ASSERTED_BY", provenance.evidence.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(confidence: ConfidenceTier, path: &str) -> serde_json::Value {
        let evidence = EvidenceRef {
            repo: "local/app".into(),
            path: path.into(),
            byte_start: 1,
            byte_end: 9,
            commit_sha: "abc".into(),
        };
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                confidence,
                vec![evidence],
                "test",
                path.as_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn endpoint(id: &str, path: &str, responses: serde_json::Value) -> Node {
        let mut responses = responses;
        responses["prov"] = prov(ConfidenceTier::Confirmed, &format!("app{path}.py"));
        Node {
            id: id.into(),
            label: "Endpoint".into(),
            props: serde_json::json!({
                "method": "GET",
                "path": path,
                "responses": responses,
                "prov": prov(ConfidenceTier::Confirmed, "app/routes.py"),
            }),
        }
    }

    fn request(test: &str, endpoint: &str, props: serde_json::Value, tier: ConfidenceTier) -> Edge {
        let mut props = props;
        props["prov"] = prov(tier, &format!("tests/{test}.py"));
        Edge {
            src: format!("test:local/app@tests/{test}.py#{test}"),
            dst: endpoint.into(),
            label: "REQUESTS".into(),
            props,
        }
    }

    fn test_case(name: &str) -> Node {
        Node {
            id: format!("test:local/app@tests/{name}.py#{name}"),
            label: "TestCase".into(),
            props: serde_json::json!({ "prov": prov(ConfidenceTier::Confirmed, name) }),
        }
    }

    fn fixture() -> (Vec<Node>, Vec<Edge>) {
        let nodes = vec![
            test_case("test_a"),
            test_case("test_b"),
            endpoint(
                "ep:items",
                "/items",
                serde_json::json!({ "statuses": [200, 401], "keys": ["items"], "scope": "handler_body" }),
            ),
            endpoint(
                "ep:partial",
                "/partial",
                serde_json::json!({ "statuses": [200], "partial": ["response_not_proven"] }),
            ),
            endpoint(
                "ep:redirect",
                "/redirect",
                serde_json::json!({ "statuses": [302] }),
            ),
            endpoint(
                "ep:untested",
                "/untested",
                serde_json::json!({ "statuses": [200] }),
            ),
        ];
        let edges = vec![
            request(
                "test_a",
                "ep:items",
                serde_json::json!({ "statuses": [200, 404], "keys": ["items", "total"] }),
                ConfidenceTier::InferredStrong,
            ),
            request(
                "test_b",
                "ep:items",
                serde_json::json!({ "statuses": [200], "keys": [] }),
                ConfidenceTier::InferredStrong,
            ),
            // An unresolved request never contributes expectations.
            request(
                "test_b",
                "ep:untested",
                serde_json::json!({ "reason": "request URL is computed" }),
                ConfidenceTier::Gap,
            ),
            request(
                "test_a",
                "ep:partial",
                serde_json::json!({ "statuses": [500] }),
                ConfidenceTier::InferredStrong,
            ),
            request(
                "test_a",
                "ep:redirect",
                serde_json::json!({ "statuses": [], "follows_redirects": true }),
                ConfidenceTier::InferredStrong,
            ),
        ];
        (nodes, edges)
    }

    // AC-0345 (T-0345).
    #[test]
    fn test_drift_compares_complete_sides_and_names_incomplete_evidence() {
        let (nodes, edges) = fixture();
        let facts = derive_test_drift(&nodes, &edges);
        let by_id: BTreeMap<_, _> = facts.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let asserted = by_id["drift:test:ep:items@asserted-not-returned"];
        assert_eq!(asserted.props["statuses"], serde_json::json!([404]));
        assert_eq!(asserted.props["keys"], serde_json::json!(["total"]));
        assert_eq!(asserted.props["tests"].as_array().unwrap().len(), 2);
        let tier = |n: &Node| typed_provenance(&n.props).unwrap().confidence_tier;
        assert_eq!(tier(asserted), ConfidenceTier::InferredStrong);
        // Cited on both sides: the test calls and the handler's responses.
        let paths: BTreeSet<_> = typed_provenance(&asserted.props)
            .unwrap()
            .evidence
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert!(paths.contains("tests/test_a.py") && paths.contains("app/items.py"));
        let unasserted = by_id["drift:test:ep:items@returned-not-asserted"];
        assert_eq!(unasserted.props["statuses"], serde_json::json!([401]));
        assert_eq!(tier(unasserted), ConfidenceTier::InferredWeak);
        assert_eq!(unasserted.props["reason"], UNASSERTED_REASON);
        // Incomplete evidence is a Gap, never drift.
        assert_eq!(
            by_id["gap:test-drift:ep:partial"].props["reason"],
            PARTIAL_RESPONSES_REASON
        );
        assert_eq!(
            by_id["gap:test-drift:ep:redirect"].props["reason"],
            FOLLOWS_REDIRECTS_REASON
        );
        assert!(
            !by_id
                .keys()
                .any(|id| id.contains("ep:partial@") || id.contains("ep:redirect@"))
        );
        // An endpoint only an unresolved request reaches has no finding.
        assert!(!by_id.keys().any(|id| id.contains("ep:untested")));
        assert_eq!(facts.nodes.len(), 4);
        let labels: Vec<_> = facts
            .edges
            .iter()
            .filter(|e| e.src == asserted.id)
            .map(|e| (e.label.as_str(), e.dst.as_str()))
            .collect();
        assert_eq!(
            labels,
            [
                ("SERVED_AS", "ep:items"),
                ("ASSERTED_BY", "test:local/app@tests/test_a.py#test_a"),
                ("ASSERTED_BY", "test:local/app@tests/test_b.py#test_b"),
            ]
        );

        let (mut nodes, mut edges) = (nodes, edges);
        nodes.extend(facts.nodes);
        edges.extend(facts.edges);
        let register = crate::compile_spec(
            &nodes,
            &edges,
            &[],
            crate::ExportMode::BestEffort,
            &BTreeSet::new(),
        )
        .artifacts
        .into_iter()
        .find(|a| a.file_name == "drift_register.md")
        .unwrap();
        let content = &register.content;
        assert!(content.contains("## Test and contract drift"));
        assert!(content.contains(
            "| asserted-not-returned | `GET /items` | 404 | total | 2 | InferredStrong |"
        ));
        assert!(
            content
                .contains("| returned-not-asserted | `GET /items` | 401 | — | 2 | InferredWeak |")
        );
        assert!(content.contains(&format!(
            "| incomplete evidence | `GET /partial` | — | — | — | {PARTIAL_RESPONSES_REASON} |"
        )));
        assert!(
            register
                .assertions
                .iter()
                .any(|a| a.subject_id == "drift:test:ep:items@asserted-not-returned")
        );
        // Verified-only keeps the strong finding and drops the weak one.
        let verified = crate::compile_spec(
            &nodes,
            &edges,
            &[],
            crate::ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        )
        .artifacts
        .into_iter()
        .find(|a| a.file_name == "drift_register.md")
        .unwrap();
        assert!(verified.content.contains("asserted-not-returned"));
        assert!(!verified.content.contains("returned-not-asserted"));
    }

    // AC-0345 (T-0345): a finding is never stronger than either side.
    #[test]
    fn test_drift_keeps_the_weaker_side_and_gaps_unproven_responses() {
        let (nodes, mut edges) = fixture();
        // test_a, the source of 404 and `total`, is only InferredWeak.
        edges[0].props["prov"] = prov(ConfidenceTier::InferredWeak, "tests/test_a.py");
        let facts = derive_test_drift(&nodes, &edges);
        let asserted = facts
            .nodes
            .iter()
            .find(|n| n.id == "drift:test:ep:items@asserted-not-returned")
            .unwrap();
        let tier = typed_provenance(&asserted.props).unwrap().confidence_tier;
        assert_eq!(tier, ConfidenceTier::InferredWeak);
        assert!(
            facts
                .edges
                .iter()
                .filter(|e| e.src == asserted.id)
                .all(|e| {
                    typed_provenance(&e.props).unwrap().confidence_tier
                        == ConfidenceTier::InferredWeak
                })
        );

        // Removed, invalid or Gap response provenance: the returned side is
        // unproven, so the endpoint is a Gap and nothing is drift.
        for responses_prov in [
            serde_json::Value::Null,
            serde_json::json!({ "tier": "nonsense" }),
            prov(ConfidenceTier::Gap, "app/items.py"),
        ] {
            let (mut nodes, edges) = fixture();
            let items = nodes.iter_mut().find(|n| n.id == "ep:items").unwrap();
            items.props["responses"]["prov"] = responses_prov;
            let facts = derive_test_drift(&nodes, &edges);
            assert!(
                !facts
                    .nodes
                    .iter()
                    .any(|n| n.id.starts_with("drift:test:ep:items@"))
            );
            let gap = facts
                .nodes
                .iter()
                .find(|n| n.id == "gap:test-drift:ep:items")
                .unwrap();
            assert_eq!(gap.props["reason"], UNPROVEN_RESPONSES_REASON);
        }
    }

    // AC-0345 (T-0345): a curation keyed by a finding's hash never carries
    // over to a materially different finding with the same id.
    #[test]
    fn test_drift_hashes_finding_content() {
        let hash = |nodes: &[Node], edges: &[Edge], id: &str| {
            let facts = derive_test_drift(nodes, edges);
            let node = facts.nodes.iter().find(|n| n.id == id).unwrap();
            typed_provenance(&node.props).unwrap().content_hash
        };
        let asserted = "drift:test:ep:items@asserted-not-returned";
        let gap = "gap:test-drift:ep:partial";
        let (nodes, edges) = fixture();
        let (before, gap_before) = (hash(&nodes, &edges, asserted), hash(&nodes, &edges, gap));
        assert_eq!(hash(&nodes, &edges, asserted), before);
        let (mut nodes, mut edges) = fixture();
        edges[0].props["statuses"] = serde_json::json!([200, 410]);
        nodes
            .iter_mut()
            .find(|n| n.id == "ep:partial")
            .unwrap()
            .props["responses"]["partial"] =
            serde_json::json!(["response_not_proven", "dynamic_status"]);
        assert_ne!(hash(&nodes, &edges, asserted), before);
        assert_ne!(hash(&nodes, &edges, gap), gap_before);
    }

    // AC-0345 (T-0345): the endpoint's own proof caps the finding, and a
    // request without evidence contributes nothing.
    #[test]
    fn test_drift_needs_endpoint_proof_and_request_evidence() {
        let finding =
            |facts: &ContractFacts, id: &str| facts.nodes.iter().find(|n| n.id == id).cloned();
        // A weak endpoint caps both findings and leaves verified-only.
        let (mut nodes, edges) = fixture();
        let items = nodes.iter_mut().find(|n| n.id == "ep:items").unwrap();
        items.props["prov"] = prov(ConfidenceTier::InferredWeak, "app/routes.py");
        let facts = derive_test_drift(&nodes, &edges);
        let asserted = finding(&facts, "drift:test:ep:items@asserted-not-returned").unwrap();
        assert_eq!(
            typed_provenance(&asserted.props).unwrap().confidence_tier,
            ConfidenceTier::InferredWeak
        );
        let (mut all_nodes, mut all_edges) = (nodes.clone(), edges.clone());
        all_nodes.extend(facts.nodes);
        all_edges.extend(facts.edges);
        let verified = crate::compile_spec(
            &all_nodes,
            &all_edges,
            &[],
            crate::ExportMode::VerifiedOnly,
            &BTreeSet::new(),
        )
        .artifacts
        .into_iter()
        .find(|a| a.file_name == "drift_register.md")
        .unwrap();
        assert!(!verified.content.contains("asserted-not-returned"));

        // Missing or Gap endpoint proof: a Gap, never drift.
        for endpoint_prov in [
            serde_json::Value::Null,
            prov(ConfidenceTier::Gap, "app/routes.py"),
        ] {
            let (mut nodes, edges) = fixture();
            let items = nodes.iter_mut().find(|n| n.id == "ep:items").unwrap();
            items.props["prov"] = endpoint_prov;
            let facts = derive_test_drift(&nodes, &edges);
            assert!(
                !facts
                    .nodes
                    .iter()
                    .any(|n| n.id.starts_with("drift:test:ep:items@"))
            );
            let gap = finding(&facts, "gap:test-drift:ep:items").unwrap();
            assert_eq!(gap.props["reason"], UNPROVEN_ENDPOINT_REASON);
        }

        // test_a, the only source of 404 and `total`, cites no evidence: its
        // expectations are dropped, and test_b alone asserts nothing missing.
        let (nodes, mut edges) = fixture();
        edges[0].props["prov"]["evidence"] = serde_json::json!([]);
        let facts = derive_test_drift(&nodes, &edges);
        assert!(finding(&facts, "drift:test:ep:items@asserted-not-returned").is_none());
        let unasserted = finding(&facts, "drift:test:ep:items@returned-not-asserted").unwrap();
        assert_eq!(unasserted.props["statuses"], serde_json::json!([401]));
        assert_eq!(unasserted.props["tests"].as_array().unwrap().len(), 1);
        // The fully supported fixture still yields the strong finding.
        let (nodes, edges) = fixture();
        assert!(
            finding(
                &derive_test_drift(&nodes, &edges),
                "drift:test:ep:items@asserted-not-returned"
            )
            .is_some()
        );
    }
}
