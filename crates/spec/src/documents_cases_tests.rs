//! AC-1600 (T-1600) and AC-1601 (T-1601): the document reader, the found
//! decision reader and the declared API references are cases of one
//! documentation walk (ADR-0065 decision 6).

use super::*;
use std::fs;

fn write(root: &Path, path: &str, text: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "README.md", b"# Shop\n\nSee `svc:orders`.\n");
    write(
        root,
        "docs/adr/ADR-0001-orders.md",
        b"# Orders own refunds\n\n- Status: Accepted\n- Governs: `svc:orders`\n\n## Context\n\nText.\n",
    );
    write(root, "docs/adr/bad.md", &[0xff, 0xfe]);
    // Build output holds no documents of the target's own, but a decision
    // record there was always read.
    write(root, "build/rfc-0002-cache.md", b"# Cache\n\n`svc:cache`\n");
    write(root, "vendor/lib/README.md", b"# Vendored\n");
    write(root, "notes.txt", b"plain\n");
    dir
}

fn candidate(id: &str) -> bool {
    id.starts_with("svc:")
}

fn without_roles(mut facts: DocumentFacts) -> DocumentFacts {
    for node in &mut facts.nodes {
        if let Some(props) = node.props.as_object_mut() {
            props.remove("roles");
        }
    }
    facts.edges.retain(|edge| edge.label != "RECORDS");
    facts
}

fn both(root: &Path) -> Documentation {
    read_documentation(
        root,
        "local/shop",
        "c1",
        &DocumentationCases {
            documents: true,
            api_references: &[],
            decisions: Some(&candidate),
        },
    )
    .unwrap()
}

#[test]
fn one_walk_yields_what_each_reader_yielded_on_its_own() {
    let dir = tree();
    let root = dir.path();
    let read = both(root);

    let documents = extract_documents(root, "local/shop", "c1").unwrap();
    let decisions = crate::read_found_adrs_among(root, "local/shop", "c1", candidate).unwrap();
    assert_eq!(read.decisions, decisions);
    assert_eq!(
        without_roles(read.documents.clone()),
        without_roles(documents)
    );

    // Counts of each case are its own: the vendored README and the decision
    // under build output are no document, the non-UTF-8 decision is skipped
    // once in each case.
    let ids: Vec<&str> = read
        .documents
        .nodes
        .iter()
        .filter(|node| node.label == "Document")
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "doc:local/shop@README.md",
            "doc:local/shop@docs/adr/ADR-0001-orders.md",
            "doc:local/shop@notes.txt",
        ]
    );
    assert_eq!(read.documents.counts.found, 4);
    assert_eq!(read.documents.counts.skipped.get("not_utf8"), Some(&1));
    let adrs: Vec<&str> = read
        .decisions
        .facts
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect();
    assert_eq!(
        adrs,
        [
            "adr:local/shop@build/rfc-0002-cache.md",
            "adr:local/shop@docs/adr/ADR-0001-orders.md",
        ]
    );
    assert_eq!(read.decisions.files, 2);
    assert_eq!(read.decisions.unread.len(), 1);
    assert_eq!(read.decisions.unread[0].path, "docs/adr/bad.md");
}

#[test]
fn a_decision_records_document_carries_the_role_and_records_its_adr() {
    let dir = tree();
    let read = both(dir.path());
    let document = read
        .documents
        .nodes
        .iter()
        .find(|node| node.id == "doc:local/shop@docs/adr/ADR-0001-orders.md")
        .unwrap();
    assert_eq!(
        document.props["roles"],
        serde_json::json!([ROLE_DECISION_RECORD])
    );
    let readme = read
        .documents
        .nodes
        .iter()
        .find(|node| node.id == "doc:local/shop@README.md")
        .unwrap();
    // A document with no role keeps the fact it had.
    assert!(readme.props.get("roles").is_none());

    let records: Vec<(&str, &str)> = read
        .documents
        .edges
        .iter()
        .filter(|edge| edge.label == "RECORDS")
        .map(|edge| (edge.src.as_str(), edge.dst.as_str()))
        .collect();
    assert_eq!(
        records,
        [(
            "doc:local/shop@docs/adr/ADR-0001-orders.md",
            "adr:local/shop@docs/adr/ADR-0001-orders.md"
        )]
    );
    // Every RECORDS edge names an ADR the same read produced.
    for (_, adr) in &records {
        assert!(
            read.decisions
                .facts
                .nodes
                .iter()
                .any(|node| node.id == *adr)
        );
    }
    // Without the decision case the role stays, the edge does not.
    let alone = extract_documents(dir.path(), "local/shop", "c1").unwrap();
    assert!(alone.edges.iter().all(|edge| edge.label != "RECORDS"));
}

#[test]
fn a_decision_past_the_document_bound_is_still_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for index in 0..MAX_DOCUMENTS_PER_REPO {
        write(root, &format!("a/{index:05}.txt"), b"x\n");
    }
    write(root, "z/adr/ADR-0009.md", b"# Late\n");
    let read = both(root);
    assert_eq!(read.documents.counts.read as usize, MAX_DOCUMENTS_PER_REPO);
    assert_eq!(
        read.documents.counts.skipped.get("over_file_limit"),
        Some(&1)
    );
    assert_eq!(read.decisions.files, 1);
    assert_eq!(
        read.decisions.facts.nodes[0].id,
        "adr:local/shop@z/adr/ADR-0009.md"
    );
}

#[test]
fn the_same_tree_yields_the_same_read() {
    let dir = tree();
    assert_eq!(both(dir.path()), both(dir.path()));
}

#[test]
fn a_large_declared_reference_keeps_the_decision_bound() {
    // AC-1804 (T-1804): a project may allow API references past 1 MiB, but
    // one that is also a decision record is still too large to be an ADR
    // (AC-0685); it stays a document with its API reference role.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let path = "docs/adr/ADR-0001-api.md";
    let text = format!(
        "# Orders API\n\n- Status: Accepted\n\n{}\n",
        "x".repeat(crate::adr::MAX_FOUND_ADR_BYTES as usize)
    );
    write(root, path, text.as_bytes());
    let references = [declared(path, &text)];
    let read = read_documentation(
        root,
        "local/shop",
        "c1",
        &DocumentationCases {
            documents: true,
            api_references: &references,
            decisions: Some(&candidate),
        },
    )
    .unwrap();
    assert!(read.decisions.facts.nodes.is_empty());
    assert_eq!(read.decisions.files, 0);
    assert_eq!(
        read.decisions.unread,
        [crate::UnreadAdr {
            path: path.into(),
            reason: crate::UnreadAdrReason::TooLarge,
        }]
    );
    let document = read
        .documents
        .nodes
        .iter()
        .find(|node| node.id == format!("doc:local/shop@{path}"))
        .expect("the reference is a document");
    assert_eq!(
        document.props["roles"],
        serde_json::json!([ROLE_API_REFERENCE, ROLE_DECISION_RECORD])
    );
    assert!(
        read.documents
            .edges
            .iter()
            .all(|edge| edge.label != "RECORDS")
    );
}

fn declared(path: &str, contents: &str) -> crate::DeclaredApiDoc {
    crate::DeclaredApiDoc {
        path: path.into(),
        prefix: None,
        contents: contents.into(),
    }
}

#[test]
fn a_declared_api_reference_is_a_document_with_its_role() {
    // AC-1601 (T-1601).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let api_md = "# API\n\n## GET /orders\n\nList.\n";
    let openapi = "{\"openapi\":\"3.0.0\",\"paths\":{}}";
    let hidden = "# Internal API\n\n`GET /health`\n";
    write(root, "docs/api.md", api_md.as_bytes());
    write(root, "openapi.json", openapi.as_bytes());
    write(root, ".cartograph/internal.md", hidden.as_bytes());
    write(root, "README.md", b"# Shop\n");
    let references = [
        declared("docs/api.md", api_md),
        declared("openapi.json", openapi),
        declared(".cartograph/internal.md", hidden),
    ];
    let read = read_documentation(
        root,
        "local/shop",
        "c1",
        &DocumentationCases {
            documents: true,
            api_references: &references,
            decisions: None,
        },
    )
    .unwrap();
    let document = |id: &str| {
        read.documents
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("{id} missing"))
    };
    for id in [
        "doc:local/shop@docs/api.md",
        "doc:local/shop@openapi.json",
        "doc:local/shop@.cartograph/internal.md",
    ] {
        assert_eq!(
            document(id).props["roles"],
            serde_json::json!([ROLE_API_REFERENCE]),
            "{id}"
        );
    }
    assert!(
        document("doc:local/shop@README.md")
            .props
            .get("roles")
            .is_none()
    );

    // An OpenAPI file is a document with no sections; a declared Markdown
    // file in a dot directory has its sections and references.
    let openapi_doc = document("doc:local/shop@openapi.json");
    assert_eq!(openapi_doc.props["format"], "openapi");
    assert_eq!(openapi_doc.props["sections"], 0);
    assert_eq!(openapi_doc.props["bytes"], openapi.len());
    assert_eq!(
        document("doc:local/shop@.cartograph/internal.md").props["sections"],
        1
    );
    assert!(
        read.documents
            .references
            .iter()
            .any(|reference| reference.path == ".cartograph/internal.md"
                && reference.text == "GET /health")
    );

    // Each declared file is counted once, whether the walk reached it or
    // not.
    assert_eq!(read.documents.counts.found, 4);
    assert_eq!(read.documents.counts.read, 4);
    // References stay in file and offset order.
    let mut sorted = read.documents.references.clone();
    sorted.sort_by(|left, right| (&left.path, left.start).cmp(&(&right.path, right.start)));
    assert_eq!(sorted, read.documents.references);
}

#[test]
fn a_declared_reference_the_walk_skipped_is_not_counted_twice() {
    // AC-1601 (T-1601): past the file bound the walk counts the declared
    // file as skipped; its validated contents do not read it again.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for index in 0..MAX_DOCUMENTS_PER_REPO {
        write(root, &format!("a/{index:05}.txt"), b"x\n");
    }
    write(root, "z/api.md", b"# API\n");
    let references = [declared("z/api.md", "# API\n")];
    let read = read_documentation(
        root,
        "local/shop",
        "c1",
        &DocumentationCases {
            documents: true,
            api_references: &references,
            decisions: None,
        },
    )
    .unwrap();
    assert_eq!(
        read.documents.counts.found as usize,
        MAX_DOCUMENTS_PER_REPO + 1
    );
    assert_eq!(
        read.documents.counts.skipped.get("over_file_limit"),
        Some(&1)
    );
    assert!(
        read.documents
            .nodes
            .iter()
            .all(|node| node.id != "doc:local/shop@z/api.md")
    );
}

#[test]
fn documentation_evidence_names_a_documents_role() {
    // AC-1600 (T-1600): the compiled artifact prints the role of a
    // decision record, and a document without one keeps its line.
    let dir = tree();
    let read = both(dir.path());
    let mut nodes = read.documents.nodes.clone();
    nodes.extend(read.decisions.facts.nodes.clone());
    let mut edges = read.documents.edges.clone();
    edges.extend(read.decisions.facts.edges.clone());
    for mode in [
        crate::ExportMode::BestEffort,
        crate::ExportMode::VerifiedOnly,
    ] {
        let bundle = crate::compile_spec(&nodes, &edges, &[], mode, &BTreeSet::new());
        let content = &bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "documentation-evidence")
            .unwrap()
            .content;
        let adr = content
            .split("### local/shop: docs/adr/ADR-0001-orders.md")
            .nth(1)
            .unwrap();
        assert!(
            adr.starts_with("\n\nTitle: Orders own refunds. Format: `markdown`. Roles: `decision_record`. Citation:"),
            "{content}"
        );
        let readme = content.split("### local/shop: README.md").nth(1).unwrap();
        assert!(
            readme.starts_with("\n\nTitle: Shop. Format: `markdown`. Citation:"),
            "{content}"
        );
    }
}

fn with_references(root: &Path, references: &[crate::DeclaredApiDoc]) -> Documentation {
    read_documentation(
        root,
        "local/shop",
        "c1",
        &DocumentationCases {
            documents: true,
            api_references: references,
            decisions: Some(&candidate),
        },
    )
    .unwrap()
}

#[test]
fn a_declared_reference_past_the_file_bound_is_skipped_like_any_file() {
    // AC-1601 (T-1601): a reference the walk did not reach is held to the
    // same file bound.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for index in 0..MAX_DOCUMENTS_PER_REPO {
        write(root, &format!("a/{index:05}.txt"), b"x\n");
    }
    write(root, "openapi.json", b"{}");
    let read = with_references(root, &[declared("openapi.json", "{}")]);
    assert_eq!(read.documents.counts.read as usize, MAX_DOCUMENTS_PER_REPO);
    assert_eq!(
        read.documents.counts.found as usize,
        MAX_DOCUMENTS_PER_REPO + 1
    );
    assert_eq!(
        read.documents.counts.skipped.get("over_file_limit"),
        Some(&1)
    );
    assert!(
        read.documents
            .nodes
            .iter()
            .all(|node| node.id != "doc:local/shop@openapi.json")
    );
}

#[test]
fn a_declared_decision_under_build_output_keeps_both_roles_and_its_adr() {
    // AC-1601 (T-1601).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let text = "# Cache\n\n## GET /cache\n";
    write(root, "build/rfc-0002-cache.md", text.as_bytes());
    let read = with_references(root, &[declared("build/rfc-0002-cache.md", text)]);
    let document = read
        .documents
        .nodes
        .iter()
        .find(|node| node.id == "doc:local/shop@build/rfc-0002-cache.md")
        .unwrap();
    assert_eq!(
        document.props["roles"],
        serde_json::json!([ROLE_API_REFERENCE, ROLE_DECISION_RECORD])
    );
    assert!(read.documents.edges.iter().any(
        |edge| edge.label == "RECORDS" && edge.dst == "adr:local/shop@build/rfc-0002-cache.md"
    ));
    assert_eq!(read.decisions.files, 1);
}

#[test]
fn a_walked_declared_reference_is_read_from_its_validated_contents() {
    // AC-1601 (T-1601): the walk does not read a declared file again, so
    // the document and the contract see the same bytes.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "docs/api.md", b"# Changed on disk since validation\n");
    let validated = "# API\n\n## GET /orders\n";
    let read = with_references(root, &[declared("docs/api.md", validated)]);
    let document = read
        .documents
        .nodes
        .iter()
        .find(|node| node.id == "doc:local/shop@docs/api.md")
        .unwrap();
    assert_eq!(document.props["title"], "API");
    assert_eq!(document.props["bytes"], validated.len());
    assert_eq!(read.documents.counts.found, 1);
}
