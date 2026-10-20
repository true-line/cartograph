use super::*;
use core_graph::{Edge, Node};
use core_prov::{EvidenceRef, Provenance, Tier};
use serde_json::{Value, json};

fn prov(repo: &str, path: &str) -> Value {
    serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            vec![EvidenceRef {
                repo: repo.into(),
                path: path.into(),
                byte_start: 0,
                byte_end: 8,
                commit_sha: "a".repeat(40),
            }],
            "discovery.test@1",
            b"recovered",
        )
        .unwrap(),
    )
    .unwrap()
}

fn node(id: &str, label: &str, mut props: Value) -> Node {
    props["prov"] = prov(
        if id.contains("billing") {
            "acme/billing"
        } else {
            "acme/shop"
        },
        "src/app.ts",
    );
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn snapshot() -> ContextSnapshot {
    ContextSnapshot::new(
        vec![
            node("sym:shop/checkout", "Symbol", json!({"name": "checkout"})),
            node(
                "sym:billing/checkout",
                "Symbol",
                json!({"name": "checkout"}),
            ),
            node(
                "ep:shop/POST /checkout",
                "Endpoint",
                json!({"route": "/Checkout"}),
            ),
            node("mod:shop/cart", "Module", json!({})),
        ],
        vec![Edge {
            src: "ep:shop/POST /checkout".into(),
            label: "HANDLED_BY".into(),
            dst: "sym:shop/checkout".into(),
            props: json!({"prov": prov("acme/shop", "src/app.ts")}),
        }],
    )
    .unwrap()
}

fn request(query: DiscoveryQuery) -> DiscoveryRequest {
    DiscoveryRequest {
        query,
        labels: Vec::new(),
        max_candidates: MAX_CANDIDATES,
        max_bytes: MAX_RESPONSE_BYTES,
        cursor: None,
    }
}

fn ids(response: &DiscoveryResponse) -> Vec<String> {
    response
        .candidates
        .iter()
        .map(|candidate| match &candidate.reference {
            FactReference::Node { id } => id.clone(),
            other => panic!("discovery returned a non-node {other:?}"),
        })
        .collect()
}

// AC-0232: exact identifier, exact name and text each match under the
// declared policy and say why, with source, counts, coverage and omissions;
// same-name hits in two repos stay two candidates. (T-0232)
#[test]
fn discovery_returns_bounded_candidates_with_reasons_coverage_and_omissions() {
    let snapshot = snapshot();

    let by_id = snapshot
        .discover(request(DiscoveryQuery::Identifier {
            id: "sym:shop/checkout".into(),
        }))
        .unwrap();
    assert_eq!(ids(&by_id), ["sym:shop/checkout"]);
    let candidate = &by_id.candidates[0];
    assert_eq!(candidate.label, "Symbol");
    assert_eq!(
        candidate.match_reason,
        MatchReason {
            kind: MatchKind::ExactIdentifier,
            field: "id".into(),
            value: "sym:shop/checkout".into(),
        }
    );
    assert_eq!(candidate.source.repo.as_deref(), Some("acme/shop"));
    assert_eq!(candidate.source.path.as_deref(), Some("src/app.ts"));
    assert_eq!((candidate.outgoing_edges, candidate.incoming_edges), (0, 1));
    assert_eq!(candidate.confidence_tier, ConfidenceTier::Confirmed);
    assert_eq!(by_id.policy, DISCOVERY_POLICY);
    assert_eq!(by_id.next_cursor, None);

    // Same name, two repos: two candidates, never merged into one binding.
    let by_name = snapshot
        .discover(request(DiscoveryQuery::Name {
            name: "checkout".into(),
        }))
        .unwrap();
    assert_eq!(ids(&by_name), ["sym:billing/checkout", "sym:shop/checkout"]);
    assert_eq!(
        by_name
            .candidates
            .iter()
            .map(|candidate| candidate.source.repo.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["acme/billing", "acme/shop"]
    );
    // Exact name is case sensitive; the route differs in case.
    assert!(
        by_name
            .candidates
            .iter()
            .all(|candidate| candidate.match_reason.kind == MatchKind::ExactName)
    );

    // Text is trimmed and case-insensitive, and reports the first field
    // that matched: the identity before any name field.
    let by_text = snapshot
        .discover(request(DiscoveryQuery::Text {
            text: "  CHECKOUT ".into(),
        }))
        .unwrap();
    assert_eq!(
        ids(&by_text),
        [
            "ep:shop/POST /checkout",
            "sym:billing/checkout",
            "sym:shop/checkout"
        ]
    );
    assert_eq!(by_text.candidates[0].match_reason.field, "id");

    // A label filter narrows what is examined, and coverage says so.
    let mut endpoints = request(DiscoveryQuery::Text {
        text: "checkout".into(),
    });
    endpoints.labels = vec!["Endpoint".into()];
    let endpoints = snapshot.discover(endpoints).unwrap();
    assert_eq!(ids(&endpoints), ["ep:shop/POST /checkout"]);
    assert_eq!(endpoints.coverage.nodes_examined, 1);

    // Coverage and omissions are stated even when nothing matches, so an
    // empty page never reads as "nothing exists".
    let none = snapshot
        .discover(request(DiscoveryQuery::Name {
            name: "missing".into(),
        }))
        .unwrap();
    assert_eq!(none.total_candidates, 0);
    assert_eq!(none.coverage.nodes_examined, 4);
    assert_eq!(none.coverage.nodes_without_name_fields, 1);
    // Coverage names only the fields this mode compared.
    assert_eq!(
        none.coverage.fields,
        ["name", "route", "path", "identity", "title"]
    );
    assert_eq!(by_id.coverage.fields, ["id"]);
    assert_eq!(
        by_text.coverage.fields,
        ["id", "name", "route", "path", "identity", "title"]
    );
    assert_eq!(
        none.omissions
            .iter()
            .map(|omission| omission.scope.as_str())
            .collect::<Vec<_>>(),
        [
            "edge facts",
            "properties outside the policy fields",
            "source file contents"
        ]
    );

    // Empty and oversized terms are rejected, not widened.
    assert!(matches!(
        snapshot.discover(request(DiscoveryQuery::Text { text: "  ".into() })),
        Err(ContextError::EmptyDiscoveryTerm)
    ));
    assert!(matches!(
        snapshot.discover(request(DiscoveryQuery::Name {
            name: "x".repeat(MAX_TERM_BYTES + 1)
        })),
        Err(ContextError::InvalidLimit {
            name: "term_bytes",
            ..
        })
    ));
}

// AC-0233: a cursor continues only the snapshot, normalized query and policy
// it was issued for; a candidate that cannot fit the budget fails instead of
// being skipped. (T-0233)
#[test]
fn discovery_cursor_binds_snapshot_query_and_policy_and_never_skips() {
    let snapshot = snapshot();
    let mut first = request(DiscoveryQuery::Text {
        text: "checkout".into(),
    });
    first.max_candidates = 2;
    let page = snapshot.discover(first.clone()).unwrap();
    assert_eq!(page.candidates.len(), 2);
    let cursor = page.next_cursor.clone().unwrap();

    // The same query in another spelling normalizes to the same identity.
    let mut next = request(DiscoveryQuery::Text {
        text: " Checkout".into(),
    });
    next.cursor = Some(cursor.clone());
    let rest = snapshot.discover(next).unwrap();
    assert_eq!(ids(&rest), ["sym:shop/checkout"]);
    assert_eq!(rest.next_cursor, None);

    let mut other_query = request(DiscoveryQuery::Text {
        text: "cart".into(),
    });
    other_query.cursor = Some(cursor.clone());
    assert!(matches!(
        snapshot.discover(other_query),
        Err(ContextError::CursorSelectionMismatch)
    ));

    let mut other_labels = first.clone();
    other_labels.labels = vec!["Symbol".into()];
    other_labels.cursor = Some(cursor.clone());
    assert!(matches!(
        snapshot.discover(other_labels),
        Err(ContextError::CursorSelectionMismatch)
    ));

    let mut other_policy = first.clone();
    other_policy.cursor = Some(DiscoveryCursor {
        policy: "discovery-v0".into(),
        ..cursor.clone()
    });
    assert!(matches!(
        snapshot.discover(other_policy),
        Err(ContextError::CursorPolicyMismatch)
    ));

    let changed = ContextSnapshot::new(
        vec![node(
            "sym:shop/checkout",
            "Symbol",
            json!({"name": "checkout"}),
        )],
        Vec::new(),
    )
    .unwrap();
    let mut other_snapshot = first.clone();
    other_snapshot.cursor = Some(cursor.clone());
    assert!(matches!(
        changed.discover(other_snapshot),
        Err(ContextError::CursorSnapshotMismatch)
    ));

    // A budget too small for the first candidate fails explicitly.
    let mut tiny = first;
    tiny.max_bytes = 64;
    assert!(matches!(
        snapshot.discover(tiny),
        Err(ContextError::ResponseBudgetExceeded { max_bytes: 64, .. })
    ));
}

// AC-0233: a matched value larger than the budget is refused before it is
// copied into a page; a smaller earlier candidate still pages normally.
// (T-0233)
#[test]
fn discovery_refuses_an_oversized_match_without_skipping_it() {
    let huge = format!("checkout-{}", "x".repeat(64 * 1024));
    let snapshot = ContextSnapshot::new(
        vec![
            node("sym:shop/a", "Symbol", json!({"name": "checkout"})),
            node("sym:shop/b", "Symbol", json!({ "title": huge })),
        ],
        Vec::new(),
    )
    .unwrap();
    let mut only_huge = request(DiscoveryQuery::Text {
        text: "checkout-x".into(),
    });
    only_huge.max_bytes = 1024;
    assert!(matches!(
        snapshot.discover(only_huge),
        Err(ContextError::ResponseBudgetExceeded { max_bytes: 1024, required_bytes })
            if required_bytes > 1024
    ));

    let mut both = request(DiscoveryQuery::Text {
        text: "checkout".into(),
    });
    both.max_bytes = 4096;
    let page = snapshot.discover(both.clone()).unwrap();
    assert_eq!(ids(&page), ["sym:shop/a"]);
    let mut next = both;
    next.cursor = page.next_cursor;
    assert!(matches!(
        snapshot.discover(next),
        Err(ContextError::ResponseBudgetExceeded { .. })
    ));
}

#[test]
fn text_matching_lowercases_without_widening() {
    assert!(contains_lowercase("Ünïcode CHECKOUT", "checkout"));
    assert!(contains_lowercase("ÜNÏ", "ünï"));
    assert!(!contains_lowercase("check out", "checkout"));
    assert!(!contains_lowercase("", "a"));
}

// AC-0233: a short match whose provenance is oversized is refused before any
// of its strings are copied, and the borrowed size check serializes exactly
// like the owned candidate. (T-0233)
#[test]
fn discovery_bounds_the_whole_candidate_before_copying_it() {
    let mut big = node("sym:shop/c", "Symbol", json!({"name": "c"}));
    big.props["prov"] = prov("acme/shop", &format!("src/{}.ts", "p".repeat(64 * 1024)));
    let snapshot = ContextSnapshot::new(vec![big], Vec::new()).unwrap();
    let mut tiny = request(DiscoveryQuery::Name { name: "c".into() });
    tiny.max_bytes = 2048;
    assert!(matches!(
        snapshot.discover(tiny),
        Err(ContextError::ResponseBudgetExceeded { max_bytes: 2048, required_bytes })
            if required_bytes > 2048
    ));

    let owned = snapshot
        .discover(request(DiscoveryQuery::Name { name: "c".into() }))
        .unwrap()
        .candidates
        .remove(0);
    let view = CandidateView {
        reference: &owned.reference,
        label: &owned.label,
        source: SourceView {
            repo: owned.source.repo.as_deref(),
            path: owned.source.path.as_deref(),
            commit_sha: owned.source.commit_sha.as_deref(),
        },
        match_reason: ReasonView {
            kind: owned.match_reason.kind,
            field: &owned.match_reason.field,
            value: &owned.match_reason.value,
        },
        outgoing_edges: owned.outgoing_edges,
        incoming_edges: owned.incoming_edges,
        confidence_tier: owned.confidence_tier,
    };
    let exact = serde_json::to_vec(&owned).unwrap();
    assert_eq!(serde_json::to_vec(&view).unwrap(), exact);
    assert_eq!(view.to_owned(), owned);
    assert_eq!(bounded_len(&view, exact.len()), Ok(exact.len()));
    assert!(bounded_len(&view, exact.len() - 1).is_err());
}
