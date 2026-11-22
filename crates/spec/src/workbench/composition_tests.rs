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

/// AC-1320 (T-1320): a system overview reads each capability's rules from
/// 1.5, round-robin within its byte budget, so the rules are citable
/// (#2375).
#[test]
fn an_overview_reads_the_rules_its_capabilities_enforce() {
    let id = |n: u8| format!("F-{:016x}", u64::from(n));
    let row = |rule: &str, n: u8| format!("| check\\_{n}() | {rule} | when x: 403 | {} |\n", id(n));
    let header = "| Where | Rule | Source | Citation |\n|---|---|---|---|\n";
    let document = format!(
        "# D1\n\n## 1.3 Capabilities (P)\n\nrow\n\n## 1.5 Business rules (F)\n\nLead.\n\n\
         ### Alias\n\n#### Create alias\n\n{header}{}{}{}\
         #### Rules in the Alias area's own code\n\n{header}{}\
         #### Block senders\n\n{header}{}\
         #### Rate limits management\n\n{header}{}\
         ### Shared rejections\n\n{header}{}\
         ### Rate limits\n\n| Capability | Operation | Limit | Citation |\n|---|---|---|---|\n\
         | Create alias | POST /a | 5/min | {} |\n\n## 1.6 Flows (P)\n\nB\n",
        row("A premium user may create an alias.", 1),
        row(
            "Not stated in domain terms (Gap): outside the rendering rules",
            2
        ),
        row("An alias name is unique.", 3),
        row("An area rule.", 4),
        row("A blocked sender is refused.", 5),
        row("A limit has an owner.", 7),
        row("A shared refusal.", 8),
        id(6),
    );
    let excerpt = rule_excerpt(&document, "1.3").unwrap();
    // Every capability's first rule comes before any second one; a Gap, an
    // area's own code and the rate limits are left out.
    let at = |text: &str| excerpt.find(text).unwrap();
    assert!(at("A premium user") < at("A blocked sender") && at("A blocked sender") < at("unique"));
    assert!(excerpt.contains(&format!(
        "| Create alias | A premium user may create an alias. | {} |",
        id(1)
    )));
    assert!(
        !excerpt.contains("(Gap)")
            && !excerpt.contains("An area rule")
            && !excerpt.contains(&id(6))
    );
    // A capability named like a generated heading keeps its rules; rules
    // several capabilities share are not one capability's.
    assert!(excerpt.contains("| Rate limits management | A limit has an owner. |"));
    assert!(!excerpt.contains("A shared refusal") && !excerpt.contains("| Shared"));
    assert!(excerpt.contains("4 of 4"));
    assert!(printed_ids(&excerpt).contains(&id(5)));
    // 1.1 reads a smaller excerpt; other sections and a 1.5 with no stated
    // rule read none.
    assert!(rule_excerpt(&document, "1.1").is_some());
    assert_eq!(rule_excerpt(&document, "1.4"), None);
    assert_eq!(
        rule_excerpt("# D1\n\n## 1.5 Business rules (F)\n\nNone.\n", "1.3"),
        None
    );
}

#[test]
fn a_rule_excerpt_stops_at_its_budget() {
    let header = "| Where | Rule | Source | Citation |\n|---|---|---|---|\n";
    let mut document = String::from("# D1\n\n## 1.5 Business rules (F)\n\n");
    for n in 0..400u16 {
        document.push_str(&format!(
            "### Capability {n}\n\n{header}| f() | Rule number {n} holds for every caller of it. | when x: 403 | F-{:016x} |\n\n",
            u64::from(n)
        ));
    }
    let excerpt = rule_excerpt(&document, "1.1").unwrap();
    assert!(excerpt.len() < 6 * 1024 + 512, "{}", excerpt.len());
    assert!(excerpt.contains(" of 400.") && !excerpt.contains("400 of 400"));
    // A smaller byte allowance shortens it; none leaves no excerpt (#2386).
    let short = rule_excerpt_within(&document, "1.1", 1024).unwrap();
    assert!(short.len() < 1024 + 512 && short.len() < excerpt.len());
    assert_eq!(rule_excerpt_within(&document, "1.1", 0), None);
    assert_eq!(
        (rule_excerpt_budget("1.1"), rule_excerpt_budget("1.4")),
        (Some(6 * 1024), None)
    );
}

#[test]
fn an_excerpt_names_the_capability_each_rule_is_listed_under() {
    // AC-1324 (T-1324, #2416).
    let header = "| Where | Rule | Source | Citation |\n|---|---|---|---|\n";
    let mut document = String::from("# D1\n\n## 1.5 Business rules (F)\n\n");
    for (n, capability) in ["Manage user (API)", "Login (authentication)"]
        .iter()
        .enumerate()
    {
        document.push_str(&format!(
            "### {capability}\n\n{header}| f() | Rule {n} refuses a caller with 401. | when x: 401 | F-{:016x} |\n\n",
            n as u64
        ));
    }
    let excerpt = rule_excerpt(&document, "1.3").unwrap();
    assert!(excerpt.contains("states the rule only for that capability"));
    let text = format!(
        "## 1.3 Capabilities (A)\n\n| Area | Capability |\n|---|---|\n| a | b |\n{excerpt}"
    );
    let found = excerpt_rule_capabilities(&text);
    assert_eq!(
        found,
        BTreeMap::from([
            (format!("F-{:016x}", 0), "Manage user (API)".to_owned()),
            (format!("F-{:016x}", 1), "Login (authentication)".to_owned()),
        ])
    );
    // The section's own table before the excerpt is not read.
    assert_eq!(
        excerpt_rule_capabilities("| a | b | F-0000000000000000 |"),
        BTreeMap::new()
    );
}
