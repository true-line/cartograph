//! AC-0931 (T-0931): the briefing a topic expert is handed for one area.
use super::*;
use core_graph::Node;
use serde_json::json;

const DOCUMENT: &str = "# Product specification\n\n\
## 1.3 Capabilities (P+A)\n\n| Area | Capability |\n\n\
### Alias\n\n#### Create alias\n\nrow F-00000000000000a1\n\n#### Delete alias\n\nrow F-00000000000000a2\n\n\
### Alias (app)\n\n#### Other\n\nrow\n\n\
### Mailbox\n\n#### Add mailbox\n\nrow\n\n\
## 1.4 Acceptance criteria per capability (P+A)\n\n\
### Mailbox\n\n#### Add mailbox\n\ncriterion\n\n\
## 1.5 Business rules (P+A)\n\n\
### Alias\n\n#### Delete alias\n\nNot stated in domain terms (Gap)\n\n\
### Shared rejections\n\nrow\n\n\
## 1.7 Out of scope (H)\n\n### Alias\n\nnot an area section\n";

fn subsystem(id: &str, name: &str, files: &[&str]) -> OverlayFact {
    OverlayFact {
        item: OverlayItem::Node(Node {
            id: id.into(),
            label: "Subsystem".into(),
            props: json!({"name": name, "kind": "cluster", "files": files.len()}),
        }),
        cites: files
            .iter()
            .map(|file| BaseRef::Node((*file).into()))
            .collect(),
    }
}

fn edge(src: &str, label: &str, dst: &str) -> Edge {
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: json!({}),
    }
}

#[test]
fn a_briefing_holds_the_areas_block_of_each_area_section_as_printed() {
    let neighbours = AreaNeighbours {
        calls: vec!["Mailbox".into()],
        called_by: vec![],
        dispatches: vec![],
    };
    let briefing = area_briefing(DOCUMENT, "Alias", 12, &neighbours, AREA_BRIEFING_BYTES);
    assert_eq!(briefing.sections, ["1.3", "1.5"]);
    assert_eq!(briefing.left_out, 0);
    // What an answer from this briefing may cite (#2057).
    assert_eq!(
        briefing_citation_ids(&briefing.text),
        [
            "F-00000000000000a1".to_owned(),
            "F-00000000000000a2".to_owned()
        ]
        .into()
    );
    let text = &briefing.text;
    assert!(text.starts_with("# Area briefing: Alias\n"));
    assert!(text.contains("a candidate grouping of 12 files, inferred by rule"));
    assert!(text.contains("- Calls into: Mailbox.\n"));
    assert!(text.contains("- Called from other areas: none recovered.\n"));
    // Each block under its section's own heading, with its citations and
    // the gaps the specification states.
    assert!(text.contains(
        "\n## 1.3 Capabilities (P+A)\n\n### Alias\n\n#### Create alias\n\nrow F-00000000000000a1\n\n#### Delete alias\n\nrow F-00000000000000a2\n"
    ));
    assert!(text.contains(
        "\n## 1.5 Business rules (P+A)\n\n### Alias\n\n#### Delete alias\n\nNot stated in domain terms (Gap)\n"
    ));
    // Nothing of another area, of an area whose name only starts the same,
    // of a section with no block for the area, or of a section that is not
    // arranged by area.
    for absent in [
        "Mailbox\n\n####",
        "#### Other",
        "1.4",
        "Shared rejections",
        "1.7",
        "| Area | Capability |",
    ] {
        assert!(!text.contains(absent), "{absent}");
    }
    // A name is matched as the specification prints it.
    let qualified = area_briefing(
        DOCUMENT,
        "Alias (app)",
        1,
        &AreaNeighbours::default(),
        AREA_BRIEFING_BYTES,
    );
    assert_eq!(qualified.sections, ["1.3"]);
    assert!(qualified.text.contains("#### Other") && !qualified.text.contains("Create alias"));
    assert!(qualified.text.starts_with("# Area briefing: Alias (app)\n"));
}

#[test]
fn an_area_the_specification_lists_nothing_under_says_so() {
    let briefing = area_briefing(
        DOCUMENT,
        "Cleanup",
        3,
        &AreaNeighbours::default(),
        AREA_BRIEFING_BYTES,
    );
    assert!(briefing.sections.is_empty());
    assert!(
        briefing
            .text
            .ends_with("\nThe product specification lists nothing under this area.\n")
    );
}

#[test]
fn a_briefing_over_its_bound_leaves_out_whole_subsections_and_counts_them() {
    // Room for the short rules block and the first capability only. The
    // blocks' own `### Alias` headings do not count: with them the rules
    // block would not fit its half.
    let bounded = area_briefing(DOCUMENT, "Alias", 12, &AreaNeighbours::default(), 120);
    assert_eq!(bounded.sections, ["1.3", "1.5"]);
    assert_eq!(bounded.left_out, 1);
    assert!(
        bounded
            .text
            .contains("#### Create alias\n\nrow F-00000000000000a1\n")
    );
    assert!(!bounded.text.contains("F-00000000000000a2"));
    // The smaller block is kept whole.
    assert!(bounded.text.contains("Not stated in domain terms (Gap)"));
    assert!(bounded.text.contains(
        "\n1 capability subsections of this area are left out of this briefing to keep its bound.\n"
    ));
    // With no room every subsection is left out and the headings remain.
    let none = area_briefing(DOCUMENT, "Alias", 12, &AreaNeighbours::default(), 0);
    assert_eq!(none.left_out, 3);
    assert!(
        none.text
            .contains("\n## 1.3 Capabilities (P+A)\n\n### Alias\n")
    );
    assert!(!none.text.contains("####"));
}

#[test]
fn neighbours_are_the_areas_across_a_call_by_name() {
    let subsystems = [
        subsystem("subsystem:a", "Alias", &["file:alias"]),
        subsystem("subsystem:m", "Mailbox", &["file:mailbox"]),
        subsystem("subsystem:e", "Email", &["file:email"]),
        subsystem("subsystem:u", "Unrelated", &["file:other"]),
    ];
    let edges = [
        edge("sym:alias", "DEFINED_IN", "file:alias"),
        edge("sym:alias2", "DEFINED_IN", "file:alias"),
        edge("sym:mailbox", "DEFINED_IN", "file:mailbox"),
        edge("sym:email", "DEFINED_IN", "file:email"),
        edge("sym:other", "DEFINED_IN", "file:other"),
        edge("sym:alias", "CALLS", "sym:mailbox"),
        edge("sym:email", "CALLS", "sym:alias2"),
        // Inside the area, to code no area holds, and not a call.
        edge("sym:alias", "CALLS", "sym:alias2"),
        edge("sym:alias", "CALLS", "sym:nowhere"),
        edge("sym:alias", "REFERENCES", "sym:other"),
    ];
    assert_eq!(
        area_neighbours(&edges, &subsystems, "subsystem:a"),
        Some(AreaNeighbours {
            calls: vec!["Mailbox".into()],
            called_by: vec!["Email".into()],
            dispatches: vec![],
        })
    );
    assert_eq!(
        area_neighbours(&edges, &subsystems, "subsystem:u"),
        Some(AreaNeighbours::default())
    );
    assert_eq!(area_neighbours(&edges, &subsystems, "subsystem:x"), None);
}

// AC-0952: a subsection larger than its block's share keeps its heading,
// its table header and the rows that fit, and counts the rows it loses;
// the rules (§1.5) are fitted last, so they take what the others leave.
#[test]
fn a_subsection_over_its_share_keeps_the_rows_that_fit() {
    let rows = |prefix: &str, n: usize| {
        (0..n)
            .map(|i| format!("| {prefix} row {i:02} | F-00000000000000{i:02} |\n"))
            .collect::<String>()
    };
    let document = format!(
        "# Product specification\n\n\
## 1.4 Acceptance criteria per capability (P+A)\n\n### Order\n\n#### Manage orders\n\n| Scenario | Citation |\n|---|---|\n{}\n\
## 1.5 Business rules (P+A)\n\n### Order\n\n#### Manage orders\n\n| Rule | Citation |\n|---|---|\n{}\n",
        rows("scenario", 40),
        rows("rule", 40)
    );
    let row = "| scenario row 00 | F-0000000000000000 |\n".len();
    // Room for about 30 rows in all: neither block fits whole.
    let briefing = area_briefing(&document, "Order", 4, &AreaNeighbours::default(), 30 * row);
    let text = &briefing.text;
    assert_eq!(briefing.left_out, 0, "{text}");
    for kept in [
        "#### Manage orders\n\n| Scenario | Citation |\n|---|---|\n| scenario row 00 |",
        "#### Manage orders\n\n| Rule | Citation |\n|---|---|\n| rule row 00 |",
    ] {
        assert!(text.contains(kept), "{kept}\n{text}");
    }
    let count = |prefix: &str| text.matches(&format!("| {prefix} row ")).count();
    let (scenarios, rules) = (count("scenario"), count("rule"));
    assert!(
        scenarios >= 5 && rules >= scenarios,
        "{scenarios} {rules}\n{text}"
    );
    assert!(text.contains(&format!(
        "The last subsection above is cut short: {} more of its rows are left out",
        40 - scenarios
    )));
    assert!(text.contains(&format!(
        "The last subsection above is cut short: {} more of its rows are left out",
        40 - rules
    )));
    // The kept blocks fill the bound: no more than one row's room is unused.
    let used: usize = text
        .lines()
        .filter(|l| l.starts_with("| scenario row") || l.starts_with("| rule row"))
        .map(|l| l.len() + 1)
        .sum();
    assert!(used + 3 * row >= 30 * row - 200, "{used}");
}

// AC-0952: a cut counts only the data rows it leaves out (never a later
// table's header or separator), and keeps a subsection when just its first
// data row fits.
#[test]
fn a_cut_counts_only_the_data_rows_left_out() {
    let block = "### Order\n\n#### Manage orders\n\n| Rule | Citation |\n|---|---|\n\
| rule one | F-01 |\n| rule two | F-02 |\n\nChanges:\n\n| Where | Change |\n|---|---|\n\
| here | F-03 |\n";
    let head = heading_len(block);
    let one_row = block.find("| rule two").unwrap();
    let fit = fitted(block, one_row - head);
    assert_eq!(fit.text, &block[..one_row]);
    assert_eq!(fit.left_out, 0);
    // `rule two` and `here`: the second table's header and separator are
    // not rows.
    assert_eq!(fit.rows_cut, 2);
    // Room for the heading and table header only keeps no partial row.
    let header_only = block.find("| rule one").unwrap();
    let fit = fitted(block, header_only - head);
    assert_eq!(fit.text, &block[..block.find("#### ").unwrap()]);
    assert_eq!((fit.left_out, fit.rows_cut), (1, 0));
}

#[test]
fn a_section_splits_into_its_area_blocks_and_the_rest() {
    let section = section_text(DOCUMENT, "1.3").unwrap();
    let (blocks, rest) = area_blocks(section, &["Mailbox", "Alias", "Billing"]);
    let names: Vec<&str> = blocks.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["Mailbox", "Alias"]);
    assert!(blocks[1].1.starts_with("### Alias\n"));
    assert!(blocks[1].1.contains("F-00000000000000a2"));
    assert!(!blocks[1].1.contains("### Alias (app)"));
    assert!(rest.starts_with("## 1.3 Capabilities"));
    assert!(rest.contains("### Alias (app)"));
    assert!(!rest.contains("F-00000000000000a1"));
    // Every line is in exactly one part.
    let total: usize = blocks.iter().map(|(_, block)| block.len()).sum::<usize>() + rest.len();
    assert_eq!(total, section.len());
    let rules = section_text(DOCUMENT, "1.5").unwrap();
    let (blocks, rest) = area_blocks(rules, &["Alias"]);
    assert_eq!(blocks.len(), 1);
    assert!(rest.contains("### Shared rejections"));
}

// AC-1032 (T-1032): a briefing carries the state machines other areas
// declare, each heading naming its area, the area's neighbours first, and
// none of the area's own machines twice.
#[test]
fn a_briefing_carries_other_areas_state_machines() {
    let document = "# Product specification\n\n\
## 1.5 Business rules (P+A)\n\n\
### Order\n\n#### State machine: orderProcess\n\n| State | May move to | Citation |\n|---|---|---|\n| Created | Paid | F-00000000000000a1 |\n\n#### Manage orders\n\nrow F-00000000000000a2\n\n\
### Payment\n\n#### State machine: paymentProcess\n\n| State | May move to | Citation |\n|---|---|---|\n| Authorized | Settled | F-00000000000000b1 |\n\nGuards in `onTransitionStart`:\n\n| onTransitionStart | When the to state is “Settled”, the move is refused. | F-00000000000000b2 |\n\n#### Manage payments\n\nrow F-00000000000000b3\n\n\
### Refund\n\n#### State machine: refundProcess\n\n| State | May move to | Citation |\n|---|---|---|\n| Pending | Settled | F-00000000000000c1 |\n\n\
### State machines\n\n#### State machine: looseProcess\n\n| State | May move to | Citation |\n|---|---|---|\n| A | B | F-00000000000000d1 |\n\n\
### Shared rejections\n\nrow\n";
    let neighbours = AreaNeighbours {
        calls: vec!["Payment".into()],
        called_by: vec![],
        dispatches: vec![],
    };
    let briefing = area_briefing(document, "Order", 4, &neighbours, AREA_BRIEFING_BYTES);
    let text = &briefing.text;
    assert_eq!(briefing.sections, ["1.5"], "{text}");
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("missing {needle}\n{text}"))
    };
    assert!(
        at("## 1.5 Business rules (P+A): other areas\n\n### State machines of other areas (")
            > at("#### Manage orders")
    );
    // The neighbour first, with its guards, then the rest; not their
    // capabilities.
    assert!(
        at("#### State machine: paymentProcess (area Payment)\n")
            < at(
                "| onTransitionStart | When the to state is “Settled”, the move is refused. | F-00000000000000b2 |"
            )
    );
    assert!(at("F-00000000000000b2") < at("#### State machine: refundProcess (area Refund)\n"));
    assert!(text.contains("| Pending | Settled | F-00000000000000c1 |"));
    assert!(!text.contains("Manage payments"), "{text}");
    assert_eq!(text.matches("#### State machine: orderProcess").count(), 1);
    // What an answer may cite includes the other areas' machines.
    assert!(briefing_citation_ids(text).contains("F-00000000000000c1"));

    // The block is charged against the bound: with none left, no machine.
    let tight = area_briefing(document, "Order", 4, &neighbours, 0);
    assert!(
        !tight.text.contains("#### State machine: paymentProcess"),
        "{}",
        tight.text
    );
    // Machines left out are said apart from the area's own capabilities.
    assert!(
        tight
            .text
            .contains("2 state machines of other areas are left out"),
        "{}",
        tight.text
    );
    // Only the area's own subsections (orderProcess, Manage orders) count.
    assert!(tight.left_out <= 2, "{}", tight.text);
    // D1's block of machines with no area is no other area's.
    assert!(!text.contains("looseProcess"), "{text}");

    // An area with no block of its own gets no other areas' machines.
    let none = area_briefing(
        document,
        "Customer",
        0,
        &AreaNeighbours::default(),
        AREA_BRIEFING_BYTES,
    );
    assert!(
        !none.text.contains("State machines of other areas"),
        "{}",
        none.text
    );
}

fn dispatch(src: &str, dst: &str, props: serde_json::Value) -> Edge {
    let mut edge = edge(src, "CALLS", dst);
    edge.props = props;
    edge
}

// AC-1308 (T-1308): an area's briefing carries the rules other blocks of
// §1.5 print for the implementations it dispatches to through
// configuration, from a call site in the area or through an interface the
// area declares, as inferred, and names an implementation no block prints.
#[test]
fn a_briefing_carries_the_rules_of_the_implementations_its_area_dispatches_to() {
    let subsystems = [
        subsystem("subsystem:tax", "Tax", &["file:tax-strategy"]),
        subsystem("subsystem:order", "Order", &["file:calculator"]),
        subsystem("subsystem:config", "Config", &["file:zones"]),
    ];
    let edges = [
        edge(
            "sym:r@tax-strategy.ts#TaxZoneStrategy",
            "DEFINED_IN",
            "file:tax-strategy",
        ),
        edge(
            "sym:r@calculator.ts#OrderCalculator.apply",
            "DEFINED_IN",
            "file:calculator",
        ),
        edge(
            "sym:r@zones.ts#DefaultTaxZoneStrategy.determineTaxZone",
            "DEFINED_IN",
            "file:zones",
        ),
        edge(
            "sym:r@zones.ts#AddressTaxZoneStrategy.determineTaxZone",
            "DEFINED_IN",
            "file:zones",
        ),
        edge(
            "sym:r@zones.ts#Calculator.calculate",
            "DEFINED_IN",
            "file:zones",
        ),
        // Called from the Order area through the Tax area's interface.
        dispatch(
            "sym:r@calculator.ts#OrderCalculator.apply",
            "sym:r@zones.ts#DefaultTaxZoneStrategy.determineTaxZone",
            json!({"resolution": "strategy-implementation", "interface": "TaxZoneStrategy"}),
        ),
        dispatch(
            "sym:r@calculator.ts#OrderCalculator.apply",
            "sym:r@zones.ts#AddressTaxZoneStrategy.determineTaxZone",
            json!({"resolution": "strategy-implementation", "interface": "TaxZoneStrategy"}),
        ),
        // The Order area's own call through another area's class.
        dispatch(
            "sym:r@calculator.ts#OrderCalculator.apply",
            "sym:r@zones.ts#Calculator.calculate",
            json!({"resolution": "configured-lookup", "through": "Calculator.calculate"}),
        ),
        // Another repository's same-named interface is not the Tax area's.
        dispatch(
            "sym:other@x.ts#Caller.run",
            "sym:other@x.ts#ElsewhereZoneStrategy.determineTaxZone",
            json!({"resolution": "strategy-implementation", "interface": "TaxZoneStrategy"}),
        ),
        // A direct call is not a dispatch.
        edge(
            "sym:r@calculator.ts#OrderCalculator.apply",
            "CALLS",
            "sym:r@zones.ts#Calculator.calculate",
        ),
    ];
    let tax = area_neighbours(&edges, &subsystems, "subsystem:tax").unwrap();
    assert_eq!(
        tax.dispatches,
        [
            "AddressTaxZoneStrategy.determineTaxZone",
            "DefaultTaxZoneStrategy.determineTaxZone"
        ]
    );
    let order = area_neighbours(&edges, &subsystems, "subsystem:order").unwrap();
    assert_eq!(order.dispatches.len(), 3, "{order:?}");
    let config = area_neighbours(&edges, &subsystems, "subsystem:config").unwrap();
    assert!(config.dispatches.is_empty(), "{config:?}");

    let document = "# Product specification\n\n\
## 1.5 Business rules (P+A)\n\n\
### Tax\n\n#### Manage tax rates\n\n| Where | Rule | Source | Citation |\n|---|---|---|---|\n| TaxRateService.update | When the rate is negative, it fails. | when (rate < 0) | F-00000000000000a1 |\n\n\
### Config\n\n#### Rules in the Config area's own code\n\n| Where | Rule | Source | Citation |\n|---|---|---|---|\n\
| AddressTaxZoneStrategy.determineTaxZone | When the order has no shipping country, the channel's default zone is used. | when (!order.shippingAddress.countryCode): returns defaultZone | F-00000000000000b1 |\n\
| ↳ | The check “zone for country” decides as follows: | zoneForCountry() | — |\n\
| OtherStrategy.run | Unrelated. | always | F-00000000000000b2 |\n\
| ↳ | Not under a wanted row. | x | F-00000000000000b3 |\n";
    let briefing = area_briefing(document, "Tax", 1, &tax, AREA_BRIEFING_BYTES);
    let text = &briefing.text;
    assert_eq!(briefing.sections, ["1.5"], "{text}");
    assert!(
        text.contains("## 1.5 Business rules (P+A): implementations dispatched to\n\n### Rules of the implementations this area dispatches to (inferred"),
        "{text}"
    );
    assert!(text.contains(
        "#### Implementations in Config\n\n| Where | Rule | Source | Citation |\n|---|---|---|---|\n| AddressTaxZoneStrategy.determineTaxZone | When the order has no shipping country, the channel's default zone is used. | when (!order.shippingAddress.countryCode): returns defaultZone | F-00000000000000b1 |\n| ↳ | The check “zone for country” decides as follows: | zoneForCountry() | — |\n"
    ), "{text}");
    assert!(
        !text.contains("OtherStrategy") && !text.contains("F-00000000000000b3"),
        "{text}"
    );
    assert!(
        text.contains(
            "No block of §1.5 prints a rule for: DefaultTaxZoneStrategy.determineTaxZone."
        ),
        "{text}"
    );
    assert!(briefing_citation_ids(text).contains("F-00000000000000b1"));
    // The area's own block stays first and whole.
    assert!(
        text.find("TaxRateService.update").unwrap()
            < text.find("Implementations in Config").unwrap()
    );

    // No dispatches, no block.
    let plain = area_briefing(
        document,
        "Tax",
        1,
        &AreaNeighbours::default(),
        AREA_BRIEFING_BYTES,
    );
    assert!(!plain.text.contains("dispatches to"), "{}", plain.text);
}
