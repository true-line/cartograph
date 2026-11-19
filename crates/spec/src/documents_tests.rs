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
fn references_that_differ_only_in_secret_text_stay_two_entries() {
    let markdown = "# Shop\n\n[a](keys/password=aaaa1111.md) and \
        [b](keys/password=bbbb2222.md)\n";
    let (nodes, edges) = pass(markdown);
    let drift: Vec<&Node> = nodes.iter().filter(|node| node.label == "Drift").collect();
    assert_eq!(drift.len(), 2);
    assert_ne!(drift[0].id, drift[1].id);
    for node in &drift {
        assert_eq!(node.props["occurrences"], 1, "{}", node.id);
        assert_eq!(node.props["key"], drift[0].props["key"]);
        for secret in ["aaaa1111", "bbbb2222"] {
            assert!(!node.id.contains(secret), "{}", node.id);
            assert!(!node.props.to_string().contains(secret), "{}", node.id);
        }
    }
    for edge in &edges {
        for secret in ["aaaa1111", "bbbb2222"] {
            assert!(!edge.src.contains(secret), "{}", edge.src);
        }
    }
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

// AC-0678 (T-0678): exact references and `MENTIONS`.

fn fact(id: &str, label: &str, props: serde_json::Value) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn system() -> Vec<Node> {
    use serde_json::json;
    vec![
        fact("file:local/shop@src/orders.ts", "File", json!({})),
        fact("file:local/shop@docs/schema.sql", "File", json!({})),
        fact("file:local/other@src/orders.ts", "File", json!({})),
        fact(
            "ep:orders:list",
            "Endpoint",
            json!({"method": "get", "path": "/orders/:id"}),
        ),
        fact(
            "ep:users:a",
            "Endpoint",
            json!({"method": "GET", "path": "/users"}),
        ),
        fact(
            "ep:users:b",
            "Endpoint",
            json!({"method": "GET", "path": "/users/"}),
        ),
        fact(
            "ep:graphql",
            "Endpoint",
            json!({"method": "POST", "path": "/graphql", "protocol": "graphql"}),
        ),
        fact("chan:orders", "Channel", json!({"name": "orders-events"})),
        fact("res:a", "Resource", json!({"name": "queue"})),
        fact("res:b", "Resource", json!({"name": "queue"})),
        fact("sym:orders", "Symbol", json!({"name": "listOrders"})),
        fact("gap:x", "Gap", json!({})),
        fact("doc:local/shop@README.md", "Document", json!({})),
    ]
}

fn linked(markdown: &str, path: &str) -> Vec<(String, String, String, String)> {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, markdown).unwrap();
    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    let nodes = system();
    link_references(
        "local/shop",
        "c1",
        &facts.references,
        &ReferenceIndex::new(&nodes),
    )
    .into_iter()
    .map(|edge| {
        assert_eq!(edge.label, "MENTIONS");
        let prov = prov(&edge.props);
        assert_eq!(prov.tier, Tier::Deterministic);
        assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
        // The first cited span is the reference as written; each other
        // one is another reference to the same fact.
        let first = &prov.evidence[0];
        assert_eq!(
            &markdown[first.byte_start as usize..first.byte_end as usize],
            edge.props["reference"].as_str().unwrap()
        );
        assert_eq!(edge.props["occurrences"], prov.evidence.len());
        for span in &prov.evidence {
            assert_eq!(span.path, path);
            assert!(span.byte_end as usize <= markdown.len());
        }
        (
            edge.src,
            edge.dst,
            edge.props["rule"].as_str().unwrap().to_string(),
            edge.props["reference"].as_str().unwrap().to_string(),
        )
    })
    .collect()
}

#[test]
fn an_exact_reference_is_linked_to_the_one_fact_it_names_by_its_rule() {
    let markdown = "# Shop\n\nSee `chan:orders` and `src/orders.ts:12`.\n\n\
        ## Routes\n\n`GET /orders/{orderId}?page=2` lists. Events go to `orders-events`.\n\
        The schema is [here](docs/schema.sql#L4 \"Schema\") and `./docs/schema.sql`.\n";
    let section = |ordinal: usize| format!("docsec:local/shop@README.md#{ordinal}");
    let row = |ordinal: usize, target: &str, rule: &str, text: &str| {
        (
            section(ordinal),
            target.to_string(),
            rule.to_string(),
            text.to_string(),
        )
    };
    assert_eq!(
        linked(markdown, "README.md"),
        [
            row(0, "chan:orders", "fact_id", "chan:orders"),
            row(
                0,
                "file:local/shop@src/orders.ts",
                "file_path",
                "src/orders.ts:12"
            ),
            row(1, "chan:orders", "name", "orders-events"),
            row(1, "ep:orders:list", "route", "GET /orders/{orderId}?page=2"),
            row(
                1,
                "file:local/shop@docs/schema.sql",
                "file_path",
                "docs/schema.sql#L4"
            ),
        ]
    );
}

#[test]
fn a_link_target_resolves_against_the_documents_directory() {
    let markdown = "# Guide\n\n[code](../src/orders.ts) [abs](/src/orders.ts) \
        [schema](<schema.sql>) ![img](./schema.sql) [out](../../x.ts) \
        [web](https://example.com/src/orders.ts) [top](#guide)\n";
    let found = linked(markdown, "docs/guide.md");
    let targets: Vec<(&str, &str)> = found
        .iter()
        .map(|row| (row.1.as_str(), row.3.as_str()))
        .collect();
    // Two links to one file from one section are one edge, as are the two
    // to the schema; the first text is kept.
    assert_eq!(
        targets,
        [
            ("file:local/shop@docs/schema.sql", "schema.sql"),
            ("file:local/shop@src/orders.ts", "../src/orders.ts"),
        ]
    );
}

#[test]
fn an_ambiguous_unmatched_or_fenced_reference_is_not_linked() {
    let markdown = "# Shop\n\n\
        `GET /users` is served twice. `queue` names two resources.\n\
        `POST /orders/1` has another method. `listOrders` is a symbol name.\n\
        `gap:x` and `doc:local/shop@README.md` are not linked. `POST /graphql` is not HTTP.\n\
        `get /orders/1` is not a method. `src/missing.ts` is no file.\n\
        Unclosed `chan:orders and ``chan:orders` do not close.\n\n\
        ```\n`chan:orders`\n[x](src/orders.ts)\n```\n";
    assert_eq!(linked(markdown, "README.md"), []);

    // A reference in another repository's document does not reach this
    // repository's files.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), "`docs/schema.sql`\n").unwrap();
    let facts = extract_documents(dir.path(), "local/other", "c1").unwrap();
    let nodes = system();
    assert!(
        link_references(
            "local/other",
            "c1",
            &facts.references,
            &ReferenceIndex::new(&nodes)
        )
        .is_empty()
    );
}

#[test]
fn references_are_read_in_offset_order_with_their_spans() {
    let text = "a ``x ` y`` b [t](u v) `c`\n~~~\n`no`\n~~~\n[only text] (x) `` `\n";
    let found: Vec<(String, bool)> = markdown_references(text)
        .into_iter()
        .map(|(reference, link, start, end)| {
            assert_eq!(&text[start..end], reference);
            (reference, link)
        })
        .collect();
    assert_eq!(
        found,
        [
            ("x ` y".to_string(), false),
            ("u".to_string(), true),
            ("c".to_string(), false),
        ]
    );
    // Balanced and escaped parentheses belong to a bare target; an
    // angle-bracket target runs to its `>`.
    let text = "[a](app/(shop)/page.tsx) [b](<my (file).md> \"t\") [c](x\\)y.md)\n";
    let targets: Vec<String> = markdown_references(text)
        .into_iter()
        .map(|(reference, _, start, end)| {
            assert_eq!(&text[start..end], reference);
            reference
        })
        .collect();
    assert_eq!(targets, ["app/(shop)/page.tsx", "my (file).md", "x\\)y.md"]);
}

#[test]
fn a_reference_in_a_section_past_the_bound_is_not_credited_to_a_kept_section() {
    let dir = tempfile::tempdir().unwrap();
    let mut text = String::from("# Top\n");
    for index in 0..MAX_SECTIONS_PER_DOCUMENT {
        text.push_str(&format!("## H{index}\n"));
    }
    text.push_str("`kept`\n");
    fs::write(dir.path().join("many.md"), &text).unwrap();
    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    assert_eq!(facts.counts.sections_truncated, 1);
    // The reference sits in the last `##` section, which the bound dropped;
    // `# Top` still spans it but is not its innermost section.
    assert!(facts.references.is_empty(), "{:?}", facts.references);
}

// AC-0679 (T-0679): documentation drift.

/// The drift of one README over [`system`], with `present` the paths that
/// exist in the repository: (id, kind, key, compared), then the edges.
fn drifted(markdown: &str, present: &[&str]) -> (Vec<Node>, Vec<Edge>) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), markdown).unwrap();
    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    let nodes = system();
    let index = ReferenceIndex::new(&nodes);
    let exists = |path: &str| present.contains(&path);
    let (drift, edges) =
        documentation_drift("local/shop", "c1", &facts.references, &index, &exists);
    for node in &drift {
        assert_eq!(node.label, "Drift");
        assert_eq!(node.props["kind"], "documentation-drift");
        let prov = prov(&node.props);
        assert_eq!(prov.tier, Tier::Deterministic);
        assert_eq!(prov.confidence_tier, ConfidenceTier::InferredStrong);
        // The entry cites each occurrence of the reference in the document.
        assert_eq!(node.props["occurrences"], prov.evidence.len());
        let first = &prov.evidence[0];
        assert_eq!(
            &markdown[first.byte_start as usize..first.byte_end as usize],
            node.props["reference"].as_str().unwrap()
        );
    }
    for edge in &edges {
        prov(&edge.props);
    }
    (drift, edges)
}

fn rows(drift: &[Node]) -> Vec<(&str, &str, &str)> {
    drift
        .iter()
        .map(|node| {
            (
                node.props["drift_kind"].as_str().unwrap(),
                node.props["key"].as_str().unwrap(),
                node.props["compared"].as_str().unwrap(),
            )
        })
        .collect()
}

#[test]
fn a_readme_route_that_is_served_is_linked_and_one_that_is_not_is_drift() {
    let markdown = "# Shop\n\n`GET /orders/{id}` lists an order. `GET /refunds/{id}` \
        returns a refund.\n\n## Admin\n\n`DELETE /orders/:id` removes one, and \
        `DELETE /orders/{order}` again. `GET /users` is served twice.\n";
    let (drift, edges) = drifted(markdown, &[]);
    assert_eq!(
        rows(&drift),
        [
            (
                "doc-route-method-differs",
                "DELETE /orders/{}",
                "the path is served as GET /orders/{}"
            ),
            (
                "doc-route-not-served",
                "GET /refunds/{}",
                "none of the 3 recovered endpoints of the system serves this path"
            ),
        ]
    );
    assert_eq!(
        drift[0].id,
        "drift:documentation:local/shop@README.md:doc-route-method-differs:DELETE /orders/{}"
    );
    // Two spellings of one route in one document are one entry.
    assert_eq!(drift[0].props["occurrences"], 2);
    let linked: Vec<(&str, &str, &str)> = edges
        .iter()
        .map(|edge| (edge.src.as_str(), edge.label.as_str(), edge.dst.as_str()))
        .collect();
    assert_eq!(
        linked,
        [
            (
                drift[0].id.as_str(),
                "DOCUMENTED_AS",
                "docsec:local/shop@README.md#1"
            ),
            (drift[0].id.as_str(), "SERVED_AS", "ep:orders:list"),
            (
                drift[1].id.as_str(),
                "DOCUMENTED_AS",
                "docsec:local/shop@README.md#0"
            ),
        ]
    );

    // The served route is linked at T0 and is no finding.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), markdown).unwrap();
    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    let nodes = system();
    let mentions = link_references(
        "local/shop",
        "c1",
        &facts.references,
        &ReferenceIndex::new(&nodes),
    );
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].dst, "ep:orders:list");
    assert_eq!(mentions[0].props["rule"], "route");
}

#[test]
fn with_no_recovered_endpoint_a_route_reference_is_no_finding() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), "`GET /refunds/{id}`\n").unwrap();
    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    let nodes = [fact(
        "file:local/shop@src/orders.ts",
        "File",
        serde_json::json!({}),
    )];
    let (drift, edges) = documentation_drift(
        "local/shop",
        "c1",
        &facts.references,
        &ReferenceIndex::new(&nodes),
        &|_| false,
    );
    assert!(drift.is_empty() && edges.is_empty());
}

#[test]
fn a_reference_to_a_file_that_does_not_exist_is_drift_with_the_path_looked_for() {
    let markdown = "# Shop\n\n[guide](docs/guide.md) [gone](docs/gone.md#setup) \
        [web](https://example.com/x.md) [top](#shop) [setup](docs/setup%20guide.md)\n\n\
        `src/orders.ts` `src/missing.ts:10` `docs/guide.md` `lib/absent.ts` \
        `application/json` `src/*.ts` `src/<name>.ts` `sym:src/a.ts` `v1.2` `Makefile`\n";
    // A link target is percent-decoded before it is looked for.
    let present = [
        "src",
        "docs",
        "src/orders.ts",
        "docs/guide.md",
        "docs/setup guide.md",
    ];
    let (drift, edges) = drifted(markdown, &present);
    assert_eq!(
        rows(&drift),
        [
            (
                "doc-file-missing",
                "docs/gone.md",
                "no file at this path in local/shop@c1"
            ),
            (
                "doc-file-missing",
                "src/missing.ts",
                "no file at this path in local/shop@c1"
            ),
        ]
    );
    // `lib/absent.ts` is not checked: no `lib` directory is in the tree.
    assert_eq!(edges.len(), 2);
    assert!(edges.iter().all(|edge| edge.label == "DOCUMENTED_AS"));

    // A code span may be relative to the document's directory.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(
        dir.path().join("docs/guide.md"),
        "`img/flow.png` `img/none.png`\n",
    )
    .unwrap();
    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    let nodes = system();
    let exists = |path: &str| ["docs/img", "docs/img/flow.png"].contains(&path);
    let (drift, _) = documentation_drift(
        "local/shop",
        "c1",
        &facts.references,
        &ReferenceIndex::new(&nodes),
        &exists,
    );
    assert_eq!(
        rows(&drift),
        [(
            "doc-file-missing",
            "docs/img/none.png",
            "no file at this path in local/shop@c1"
        )]
    );
}

#[test]
fn the_drift_register_lists_documentation_drift_with_what_it_was_compared_with() {
    let markdown = "# Shop\n\n`GET /refunds/{id}` and [gone](docs/gone.md).\n";
    let (drift, edges) = drifted(markdown, &[]);
    let register = |nodes: &[Node], edges: &[Edge], mode| {
        crate::compile_spec(nodes, edges, &[], mode, &BTreeSet::new())
            .artifacts
            .into_iter()
            .find(|artifact| artifact.id == "drift-register")
            .unwrap()
            .content
    };
    for mode in [
        crate::ExportMode::BestEffort,
        crate::ExportMode::VerifiedOnly,
    ] {
        let content = register(&drift, &edges, mode);
        assert!(
            content.contains(
                "\n## Documentation drift\n\nExact route and file references in the target's \
                 own documentation that no longer match what was recovered. A reference is \
                 stated intent: it changes no recovered fact.\n\n\
                 | Finding | Document | Reference | Compared with | Confidence |\n\
                 |---|---|---|---|---|\n\
                 | doc-file-missing | `README.md` | `docs/gone.md` | no file at this path in \
                 local/shop@c1 | InferredStrong |\n\
                 | doc-route-not-served | `README.md` | `GET /refunds/{}` | none of the 3 \
                 recovered endpoints of the system serves this path | InferredStrong |\n"
            ),
            "{content}"
        );
        // It is not listed as an ADR conflict.
        assert!(content.contains("| — | No ADR/code conflicts recovered |"));
    }
    // A register without such a finding does not have the section.
    assert!(!register(&[], &[], crate::ExportMode::BestEffort).contains("Documentation drift"));
}

// AC-0680 (T-0680): the stated-intent boundary and redaction.

/// Every fact the documentation pass yields for one README over [`system`].
fn pass(markdown: &str) -> (Vec<Node>, Vec<Edge>) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("README.md"), markdown).unwrap();
    let facts = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    let nodes = system();
    let index = ReferenceIndex::new(&nodes);
    let mut out_nodes = facts.nodes;
    let mut out_edges = facts.edges;
    out_edges.extend(link_references(
        "local/shop",
        "c1",
        &facts.references,
        &index,
    ));
    let (drift, compared) =
        documentation_drift("local/shop", "c1", &facts.references, &index, &|_| false);
    out_nodes.extend(drift);
    out_edges.extend(compared);
    (out_nodes, out_edges)
}

#[test]
fn documentation_that_contradicts_a_recovered_fact_yields_only_its_own_facts() {
    // The README states another method for a served path and names a route
    // that is not served. Each fact of the pass is a document, a section or
    // a drift entry, each edge starts at one, and no fact of the system is
    // yielded again with other content.
    let markdown = "# Shop\n\n`DELETE /orders/{id}` is how an order is listed. \
        `GET /refunds/{id}` returns a refund. `GET /orders/{id}` too.\n";
    let (nodes, edges) = pass(markdown);
    let own = |id: &str| {
        id.starts_with("doc:local/shop@")
            || id.starts_with("docsec:local/shop@")
            || id.starts_with("drift:documentation:local/shop@")
    };
    let recovered: BTreeSet<String> = system()
        .into_iter()
        .map(|node| node.id)
        .filter(|id| !own(id))
        .collect();
    assert!(nodes.iter().any(|node| node.label == "Drift"));
    for node in &nodes {
        assert!(
            own(&node.id) && !recovered.contains(&node.id),
            "{}",
            node.id
        );
        assert!(
            matches!(node.label.as_str(), "Document" | "DocSection" | "Drift"),
            "{}",
            node.label
        );
    }
    for edge in &edges {
        assert!(own(&edge.src), "{} {}", edge.src, edge.label);
        assert!(
            matches!(
                edge.label.as_str(),
                "HAS_SECTION" | "MENTIONS" | "DOCUMENTED_AS" | "SERVED_AS"
            ),
            "{}",
            edge.label
        );
    }
    // A statement in documentation is never a confirmed fact about the
    // system: a drift entry is below Confirmed.
    for node in nodes.iter().filter(|node| node.label == "Drift") {
        assert_eq!(
            prov(&node.props).confidence_tier,
            ConfidenceTier::InferredStrong
        );
    }
}

#[test]
fn no_section_text_is_in_a_fact_or_in_an_export() {
    let markdown = "# Shop\n\nRefunds are settled nightly by the ledger job. See \
        `GET /orders/{id}` and `GET /refunds/{id}`.\n\n## Deploy\n\n\
        Operators rotate the signing key every quarter.\n";
    let (nodes, edges) = pass(markdown);
    let held = |text: &str| text.contains("settled nightly") || text.contains("rotate the signing");
    for node in &nodes {
        assert!(!held(&node.props.to_string()), "{}", node.id);
    }
    for edge in &edges {
        assert!(!held(&edge.props.to_string()), "{}", edge.label);
    }
    // The heading, the span and the hash are what a section holds.
    let section = nodes
        .iter()
        .find(|node| node.id == "docsec:local/shop@README.md#1")
        .unwrap();
    assert_eq!(section.props["heading"], "Deploy");
    let cited = &prov(&section.props).evidence[0];
    assert_eq!(
        &markdown[cited.byte_start as usize..cited.byte_end as usize],
        "## Deploy\n\nOperators rotate the signing key every quarter.\n"
    );
    let mut all = system();
    all.extend(nodes);
    for mode in [
        crate::ExportMode::BestEffort,
        crate::ExportMode::VerifiedOnly,
    ] {
        let compiled = crate::compile_spec(&all, &edges, &[], mode, &BTreeSet::new());
        assert!(!compiled.artifacts.is_empty());
        for artifact in &compiled.artifacts {
            assert!(!held(&artifact.content), "{}", artifact.id);
        }
    }
}

#[test]
fn a_reference_holding_a_secret_is_stored_redacted() {
    let markdown = "# Shop\n\n[key](keys/password=hunter2secret.md) and \
        `GET /hooks/password=hunter2secret`\n";
    let (nodes, edges) = pass(markdown);
    let drift: Vec<&Node> = nodes.iter().filter(|node| node.label == "Drift").collect();
    assert_eq!(drift.len(), 2);
    for node in &nodes {
        assert!(!node.id.contains("hunter2secret"), "{}", node.id);
        assert!(
            !node.props.to_string().contains("hunter2secret"),
            "{}",
            node.id
        );
    }
    for edge in &edges {
        assert!(!edge.src.contains("hunter2secret"), "{}", edge.src);
        assert!(
            !edge.props.to_string().contains("hunter2secret"),
            "{}",
            edge.label
        );
    }
    assert!(drift.iter().all(|node| {
        node.props["key"].as_str().unwrap().contains("[REDACTED]")
            && node.id.ends_with(node.props["key"].as_str().unwrap())
    }));
}
