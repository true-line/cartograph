//! AC-0676 (T-0676): documents and sections as T0 facts, the bounds, and the
//! same facts on a second read.

use super::*;
use core_prov::{ConfidenceTier, Provenance, Tier};
use serde::Deserialize;
use std::fs;

const README: &str = "Intro line.\n\n# Shop\n\nText.\n\n## Routes\n\n`GET /orders`\n\n```sh\n# not a heading\n```\n\n### Detail\n\nMore.\n\n## Deploy ##\n\nEnd.\n";

fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), README).unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(dir.path().join("docs/NOTES.TXT"), "plain notes\n").unwrap();
    fs::write(dir.path().join("docs/empty.md"), "\n").unwrap();
    fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
    fs::write(dir.path().join("node_modules/pkg/README.md"), "# Pkg\n").unwrap();
    dir
}

fn node<'a>(facts: &'a DocumentFacts, id: &str) -> &'a Node {
    facts
        .nodes
        .iter()
        .find(|node| node.id == id)
        .unwrap_or_else(|| panic!("{id} missing"))
}

fn prov(value: &serde_json::Value) -> Provenance {
    let prov = Provenance::deserialize(&value["prov"]).unwrap();
    prov.validate().unwrap();
    prov
}

#[test]
fn each_file_is_a_document_and_each_section_a_fact_with_its_span() {
    let dir = tree();
    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();

    let ids: Vec<&str> = facts.nodes.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "doc:local/shop@README.md",
            "doc:local/shop@docs/NOTES.TXT",
            "doc:local/shop@docs/empty.md",
            "docsec:local/shop@README.md#0",
            "docsec:local/shop@README.md#1",
            "docsec:local/shop@README.md#2",
            "docsec:local/shop@README.md#3",
            "docsec:local/shop@README.md#4",
            "docsec:local/shop@docs/NOTES.TXT#0",
        ]
    );
    let readme = node(&facts, "doc:local/shop@README.md");
    assert_eq!(readme.label, "Document");
    assert_eq!(readme.props["title"], "Shop");
    assert_eq!(readme.props["format"], "markdown");
    assert_eq!(readme.props["sections"], 5);
    assert_eq!(readme.props["bytes"], README.len());
    assert!(readme.props.get("body").is_none());
    let notes = node(&facts, "doc:local/shop@docs/NOTES.TXT");
    assert_eq!(notes.props["title"], "NOTES.TXT");
    assert_eq!(notes.props["format"], "text");

    // (level, heading, the text its span covers)
    let expected = [
        (0, "", "Intro line.\n\n"),
        (1, "Shop", &README["Intro line.\n\n".len()..]),
        (
            2,
            "Routes",
            &README[README.find("## Routes").unwrap()..README.find("## Deploy").unwrap()],
        ),
        (
            3,
            "Detail",
            &README[README.find("### Detail").unwrap()..README.find("## Deploy").unwrap()],
        ),
        (2, "Deploy", &README[README.find("## Deploy").unwrap()..]),
    ];
    for (ordinal, (level, heading, text)) in expected.into_iter().enumerate() {
        let section = node(&facts, &format!("docsec:local/shop@README.md#{ordinal}"));
        assert_eq!(section.label, "DocSection");
        assert_eq!(section.props["level"], level, "{ordinal}");
        assert_eq!(section.props["heading"], heading, "{ordinal}");
        assert_eq!(section.props["ordinal"], ordinal);
        let prov = prov(&section.props);
        assert_eq!(prov.tier, Tier::Deterministic);
        assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
        let span = &prov.evidence[0];
        assert_eq!(
            &README[span.byte_start as usize..span.byte_end as usize],
            text,
            "{ordinal}"
        );
        assert_eq!(
            (span.path.as_str(), span.commit_sha.as_str()),
            ("README.md", "c1")
        );
        // No section text is stored.
        assert!(!section.props.to_string().contains("Text."));
    }

    let edges: Vec<(&str, &str, &str)> = facts
        .edges
        .iter()
        .map(|edge| (edge.src.as_str(), edge.label.as_str(), edge.dst.as_str()))
        .collect();
    assert_eq!(edges.len(), 6);
    assert!(edges.contains(&(
        "doc:local/shop@README.md",
        "HAS_SECTION",
        "docsec:local/shop@README.md#4"
    )));
    for edge in &facts.edges {
        prov(&edge.props);
    }
    assert_eq!(
        facts.counts,
        DocumentCounts {
            found: 3,
            read: 3,
            skipped: BTreeMap::new(),
            sections: 6,
            sections_truncated: 0,
        }
    );
}

#[test]
fn a_second_read_of_the_same_tree_yields_the_same_facts() {
    let dir = tree();
    let first = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    let second = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    assert_eq!(first, second);
    assert_eq!(
        crate::graph_content_hash(&first.nodes, &first.edges),
        crate::graph_content_hash(&second.nodes, &second.edges)
    );

    // An edit changes the hash of the section it is in and of those that
    // contain it, and of no other.
    fs::write(
        dir.path().join("README.md"),
        README.replace("More.", "More, edited."),
    )
    .unwrap();
    let edited = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    let changed: Vec<&str> = first
        .nodes
        .iter()
        .zip(&edited.nodes)
        .filter(|(before, after)| {
            prov(&before.props).content_hash != prov(&after.props).content_hash
        })
        .map(|(before, _)| before.id.as_str())
        .collect();
    assert_eq!(
        changed,
        [
            "doc:local/shop@README.md",
            "docsec:local/shop@README.md#1",
            "docsec:local/shop@README.md#2",
            "docsec:local/shop@README.md#3",
        ]
    );
}

#[test]
fn fenced_code_and_malformed_headings_are_not_sections() {
    let text = "~~~\n# in tildes\n```\n# still fenced\n~~~\n#no-space\n    # indented code\n####### seven\n#\n# Real #\n";
    let found = markdown_sections(text);
    let headings: Vec<(u8, &str)> = found
        .iter()
        .map(|section| (section.level, section.heading.as_str()))
        .collect();
    assert_eq!(headings, [(0, ""), (1, ""), (1, "Real")]);
    // Inline code that starts with three backticks opens no fence.
    let inline: Vec<(u8, String)> = markdown_sections("```literal```\n\n# Real heading\ntext\n")
        .into_iter()
        .map(|section| (section.level, section.heading))
        .collect();
    assert_eq!(
        inline,
        [(0, String::new()), (1, "Real heading".to_string())]
    );
    // An unclosed fence runs to the end of the file.
    assert!(
        markdown_sections("```\n# a\n# b\n")
            .iter()
            .all(|s| s.level == 0)
    );
}

#[test]
fn the_bounds_hold_and_every_skip_is_counted_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("big.md"),
        vec![b'a'; MAX_DOCUMENT_BYTES as usize + 1],
    )
    .unwrap();
    fs::write(dir.path().join("binary.txt"), [0xff, 0xfe, 0x00]).unwrap();
    let many: String = (0..MAX_SECTIONS_PER_DOCUMENT + 5)
        .map(|index| format!("# H{index}\n"))
        .collect();
    fs::write(dir.path().join("many.md"), many).unwrap();
    fs::write(dir.path().join(".gitignore"), "ignored.md\n").unwrap();
    fs::write(dir.path().join("ignored.md"), "# Ignored\n").unwrap();

    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    assert_eq!(facts.counts.found, 3);
    assert_eq!(facts.counts.read, 1);
    assert_eq!(
        facts.counts.skipped,
        BTreeMap::from([("not_utf8", 1), ("too_large", 1)])
    );
    assert_eq!(facts.counts.sections, MAX_SECTIONS_PER_DOCUMENT as u64);
    assert_eq!(facts.counts.sections_truncated, 1);
    assert_eq!(facts.nodes.len(), 1 + MAX_SECTIONS_PER_DOCUMENT);
    assert_eq!(
        node(&facts, "doc:local/shop@many.md").props["sections"],
        MAX_SECTIONS_PER_DOCUMENT
    );
}

#[test]
fn a_title_or_heading_holding_a_secret_is_stored_redacted() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("README.md"),
        "# Deploy with password=hunter2secret\n",
    )
    .unwrap();
    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    for node in &facts.nodes {
        assert!(
            !node.props.to_string().contains("hunter2secret"),
            "{}",
            node.id
        );
    }
    assert_eq!(
        node(&facts, "docsec:local/shop@README.md#0").props["heading"],
        "Deploy with password=[REDACTED]"
    );
}
