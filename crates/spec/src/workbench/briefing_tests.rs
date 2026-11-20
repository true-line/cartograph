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
    };
    let briefing = area_briefing(DOCUMENT, "Alias", 12, &neighbours, AREA_BRIEFING_BYTES);
    assert_eq!(briefing.sections, ["1.3", "1.5"]);
    assert_eq!(briefing.left_out, 0);
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
        })
    );
    assert_eq!(
        area_neighbours(&edges, &subsystems, "subsystem:u"),
        Some(AreaNeighbours::default())
    );
    assert_eq!(area_neighbours(&edges, &subsystems, "subsystem:x"), None);
}
