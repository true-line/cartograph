//! AC-0862 (T-0862): the context a section composer is handed.
use super::*;
use core_graph::overlay::BaseRef;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::json;

fn prov(path: &str, fact: &str) -> Provenance {
    Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: "acme/shop".into(),
            path: path.into(),
            byte_start: 1,
            byte_end: 9,
            commit_sha: "abc123".into(),
        }],
        "spec.composition.test",
        fact.as_bytes(),
    )
    .unwrap()
}

fn node(id: &str, label: &str, props: serde_json::Value) -> Node {
    let mut props = props;
    props["prov"] = serde_json::to_value(prov("app/api/orders.py", id)).unwrap();
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn cite(id: &str) -> String {
    citation_id(&FactKey::Node { id: id.into() })
}

#[test]
fn a_section_runs_from_its_heading_to_the_next() {
    let document = "# D1\n\n## 1.2 Actors (P)\n\nA\n\n## 1.3 Capabilities (P)\n\nrow\n### sub\n\n## 1.4 Next (P)\n\nB\n";
    assert_eq!(
        section_text(document, "1.3"),
        Some("## 1.3 Capabilities (P)\n\nrow\n### sub\n\n")
    );
    assert_eq!(
        section_text(document, "1.4"),
        Some("## 1.4 Next (P)\n\nB\n")
    );
    assert_eq!(section_text(document, "1.1"), None);
}

/// AC-1313 (T-1313): §1.1 prints no fact of its own, so it is composed
/// from its heading, §1.2 and the §1.3 overview (#2319).
#[test]
fn purpose_and_scope_is_composed_from_the_sections_it_summarises() {
    let document = "# D1\n\n## 1.1 Purpose (A)\n\n_Not drafted._\n\n## 1.2 Actors (P)\n\nA\n\n## 1.3 Capabilities (P)\n\nrow\n### sub\n\n## 1.4 Next (P)\n\nB\n";
    let text = composition_source(document, "1.1").unwrap();
    assert!(text.starts_with("## 1.1 Purpose (A)\n\n"));
    assert!(text.contains("## 1.2 Actors (P)\n\nA\n"));
    assert!(text.contains("## 1.3 Capabilities (P)\n\nrow\n"));
    assert!(!text.contains("### sub") && !text.contains("_Not drafted._"));
    assert!(!text.contains("## 1.4"));
    // Any other section reads as its composition text.
    assert_eq!(
        composition_source(document, "1.4").as_deref(),
        Some("## 1.4 Next (P)\n\nB\n")
    );
    assert_eq!(composition_source("# D1\n", "1.1"), None);
}

/// AC-1316 (T-1316): a summarised section is read up to its first `###`,
/// with each table cut to 24 rows and a count of the rest (#2335).
#[test]
fn a_summary_reads_each_source_up_to_its_subsections_with_short_tables() {
    let rows: String = (1..=30).map(|n| format!("| r{n} | x |\n")).collect();
    let document = format!(
        "# D1\n\n## 1.1 Purpose (A)\n\n_Not drafted._\n\n## 1.2 Actors (P)\n\n| Role | Ops |\n|---|---|\n{rows}\nAfter.\n\n### Detail\n\nhidden\n\n## 1.3 Capabilities (P)\n\n| A | B |\n|---|---|\n| a | b |\n\n## 1.4 Next (P)\n\nB\n"
    );
    let text = composition_source(&document, "1.1").unwrap();
    assert!(
        text.contains("| Role | Ops |\n|---|---|\n| r1 | x |\n"),
        "{text}"
    );
    assert!(
        text.contains("| r24 | x |\n(6 more rows are left out.)\n"),
        "{text}"
    );
    assert!(
        !text.contains("| r25 |") && !text.contains("hidden"),
        "{text}"
    );
    assert!(
        text.contains("After.") && !text.contains("### Detail"),
        "{text}"
    );
    // A short table is kept whole, with no count line.
    assert!(text.contains("| a | b |\n") && text.matches("left out").count() == 1);
}

#[test]
fn an_overview_section_is_read_up_to_its_per_capability_detail() {
    let text = "## 1.3 Capabilities (P)\n\n| Area | Capability |\n\n### Orders\n\n#### Place order\n\nrow\n\n### Billing\n\nrow\n";
    let (kept, left_out) = composition_text(text, "1.3");
    // Capabilities are counted by their `####` headings when there are any,
    // and by `###` headings otherwise.
    assert_eq!(left_out, 1);
    assert!(kept.starts_with("## 1.3 Capabilities (P)\n\n| Area | Capability |\n"));
    assert!(!kept.contains("### Orders") && !kept.contains("#### Place order"));
    assert!(kept.contains("Per-capability detail (1 subsections) is left out"));
    let flat_areas = "## 1.3 C (P)\n\nrow\n\n### Orders\n\nrow\n\n### Billing\n\nrow\n";
    assert_eq!(composition_text(flat_areas, "1.3").1, 2);
    // Other sections, and an overview without subsections, are read whole.
    assert_eq!(composition_text(text, "1.4"), (text.to_owned(), 0));
    let flat = "## 1.3 Capabilities (P)\n\nrow\n";
    assert_eq!(composition_text(flat, "1.3"), (flat.to_owned(), 0));
}

#[test]
fn printed_ids_are_whole_citation_ids() {
    let id = cite("endpoint:a");
    let text = format!("| {id} | x{id} | {id}0 | F-12 |");
    assert_eq!(printed_ids(&text), BTreeSet::from([id]));
}

#[test]
fn the_composer_cites_base_facts_and_never_sees_a_source_identifier() {
    let endpoint = node(
        "endpoint:orders",
        "Endpoint",
        json!({"method": "POST", "path": "/orders"}),
    );
    let rule = node("rule:limit", "BusinessRule", json!({"name": "Order limit"}));
    let file = node(
        "file:app/api/orders.py",
        "File",
        json!({"path": "app/api/orders.py"}),
    );
    let symbol = node("sym:create", "Symbol", json!({"name": "create_order"}));
    let capability = node("cap:orders", "Capability", json!({"name": "Orders"}));
    let cites = BTreeSet::from([
        BaseRef::Node("endpoint:orders".into()),
        BaseRef::Node("sym:create".into()),
    ]);
    let section = format!(
        "## 1.3 Capabilities (P)\n\n| Orders | {} |\n| rule | {} |\n| file | {} |\n",
        cite("cap:orders"),
        cite("rule:limit"),
        cite("file:app/api/orders.py"),
    );
    let nodes = vec![endpoint, rule, file, symbol];
    let composed = composition_facts(&section, &nodes, &[], &[(&capability, &cites)]);

    // The derived Capability is citable and a group heading over the base
    // fact it rests on; its cited code declaration is left out (AC-1011).
    assert_eq!(
        composed.derived,
        vec![DerivedGroup {
            citation_id: cite("cap:orders"),
            label: "Capability".into(),
            name: "Orders".into(),
            rests_on: vec![cite("endpoint:orders")],
        }]
    );
    let citable = composed.citable();
    assert!(citable.contains(&cite("cap:orders")));
    assert!(!citable.contains(&cite("sym:create")));
    let row = |id: &str| {
        composed
            .facts
            .iter()
            .find(|fact| fact.citation_id == cite(id))
            .unwrap()
            .clone()
    };
    assert_eq!(row("cap:orders").label, "Capability");
    assert_eq!(row("cap:orders").name.as_deref(), Some("Orders"));
    assert_eq!(row("endpoint:orders").name.as_deref(), Some("POST /orders"));
    assert_eq!(row("rule:limit").name.as_deref(), Some("Order limit"));
    assert_eq!(row("rule:limit").tier, "Deterministic");
    // A printed File is citable but shown without its path.
    assert_eq!(row("file:app/api/orders.py").label, "File");
    assert_eq!(row("file:app/api/orders.py").name, None);
}

// AC-1011: a derived Actor named after the endpoint it rests on is citable
// without that name, so no node id reaches the composer.
#[test]
fn a_derived_name_that_carries_a_node_id_is_not_shown() {
    let endpoint = node(
        "endpoint:app/api/orders.py#post",
        "Endpoint",
        json!({"method": "POST", "path": "/orders"}),
    );
    let actor = node(
        "actor:allow-callers:abc",
        "Actor",
        json!({"name": "Caller set for endpoint endpoint:app/api/orders.py#post"}),
    );
    let cites = BTreeSet::from([BaseRef::Node("endpoint:app/api/orders.py#post".into())]);
    let section = format!(
        "## 1.2 Actors (P)\n\n| caller | {} |\n",
        cite("actor:allow-callers:abc")
    );
    let nodes = vec![endpoint];
    let composed = composition_facts(&section, &nodes, &[], &[(&actor, &cites)]);
    let row = composed
        .facts
        .iter()
        .find(|fact| fact.citation_id == cite("actor:allow-callers:abc"))
        .unwrap();
    assert_eq!((row.label.as_str(), row.name.as_deref()), ("Actor", None));
    assert_eq!(composed.derived[0].name, "Actor");
    assert!(
        !format!("{composed:?}").contains("app/api/orders.py"),
        "{composed:?}"
    );
}
