//! The target's own documentation as evidence (#1750, AC-0681, ADR-0065,
//! SPEC-18): `documentation-evidence.md`.
//!
//! A deterministic (T0) projection of the facts `crate::documents` records:
//! `Document` and `DocSection` facts, their `HAS_SECTION` relations, the
//! `MENTIONS` links from a section to the fact an exact reference names, and
//! the `documentation-drift` entries. No model is called, no file is read
//! and no fact is recovered here.
//!
//! **Stated intent.** A document says what the target says about itself. A
//! link states that a section names a fact, never that the section is right
//! about it.
//!
//! **Headings and ids only.** The facts hold a title, a heading, a span and
//! a hash, each redacted before it was stored. The text of a section is in
//! no fact, so it cannot be printed here.
//!
//! **Cost.** One index over the admitted facts: linear in them.
//!
//! **Mode.** The producer receives the facts the export mode admitted. A
//! link to a fact the export leaves out is counted and not listed, so no
//! citation id is printed that the fact index does not resolve.

use super::producers::Output;
use super::render::{Assertions, Budget, Literal, RenderLimit, Text, inline_literal};
use super::{ExportMode, citation_id};
use crate::documents::{DocumentationDrift, ReferenceRule};
use core_graph::{Edge, FactKey, Node};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The artifact id.
pub(crate) const ARTIFACT_ID: &str = "documentation-evidence";

const INTRO: &str = "# Documentation evidence\n\n\
The target's own documentation files, their sections, and the recovered facts each section \
names by an exact reference: a fact id, a file path, a route or a unique name. Documentation \
is stated intent: a link says that a section names a fact, not that what the section says \
about it is true, and it changes no recovered fact. Headings are shown as stored, redacted; \
the text of a section is not stored and is not here. Every citation id resolves in the fact \
index.\n\n";

const RULES: [ReferenceRule; 4] = [
    ReferenceRule::FactId,
    ReferenceRule::FilePath,
    ReferenceRule::Route,
    ReferenceRule::Name,
];

const KINDS: [DocumentationDrift; 3] = [
    DocumentationDrift::RouteMethodDiffers,
    DocumentationDrift::RouteNotServed,
    DocumentationDrift::FileMissing,
];

fn node_citation(node: &Node) -> String {
    citation_id(&FactKey::Node {
        id: node.id.clone(),
    })
}

fn edge_citation(edge: &Edge) -> String {
    citation_id(&FactKey::Edge {
        source: edge.src.clone(),
        label: edge.label.clone(),
        destination: edge.dst.clone(),
    })
}

/// The repository of a `doc:{repo}@{path}` id.
fn repo_of(document: &Node) -> &str {
    let path = document.props["path"].as_str().unwrap_or("");
    document
        .id
        .strip_prefix("doc:")
        .and_then(|rest| rest.strip_suffix(path))
        .and_then(|rest| rest.strip_suffix('@'))
        .unwrap_or("")
}

struct Section<'a> {
    node: &'a Node,
    /// The links of the section whose fact this export holds, in rule and
    /// then fact order.
    mentions: Vec<(&'a Edge, &'a Node)>,
}

struct Document<'a> {
    node: &'a Node,
    sections: Vec<Section<'a>>,
}

/// `documentation-evidence.md` over the facts the export mode admitted.
pub(super) fn output(
    nodes: &[&Node],
    edges: &[&Edge],
    mode: ExportMode,
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|node| (node.id.as_str(), *node)).collect();
    let is = |id: &str, label: &str| by_id.get(id).is_some_and(|node| node.label == label);

    // (repository, path) orders the documents; the ordinal orders sections.
    let mut documents: BTreeMap<(&str, &str), Document<'_>> = BTreeMap::new();
    let mut owner: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for node in nodes.iter().filter(|node| node.label == "Document") {
        let key = (repo_of(node), node.props["path"].as_str().unwrap_or(""));
        owner.insert(node.id.as_str(), key);
        documents.insert(
            key,
            Document {
                node,
                sections: Vec::new(),
            },
        );
    }
    let mut section_of: BTreeMap<&str, ((&str, &str), usize)> = BTreeMap::new();
    for edge in edges.iter().filter(|edge| edge.label == "HAS_SECTION") {
        let (Some(key), Some(node)) = (owner.get(edge.src.as_str()), by_id.get(edge.dst.as_str()))
        else {
            continue;
        };
        if node.label != "DocSection" {
            continue;
        }
        if let Some(document) = documents.get_mut(key) {
            document.sections.push(Section {
                node,
                mentions: Vec::new(),
            });
        }
    }
    for (key, document) in &mut documents {
        document
            .sections
            .sort_by_key(|section| (section.node.props["ordinal"].as_u64(), &section.node.id));
        for (at, section) in document.sections.iter().enumerate() {
            section_of.insert(section.node.id.as_str(), (*key, at));
        }
    }

    let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
    let mut left_out = 0usize;
    for edge in edges.iter().filter(|edge| edge.label == "MENTIONS") {
        if !is(&edge.src, "DocSection") {
            continue;
        }
        let rule = edge.props["rule"].as_str().unwrap_or("");
        let (Some(fact), Some((key, at))) = (
            by_id.get(edge.dst.as_str()),
            section_of.get(edge.src.as_str()),
        ) else {
            left_out += 1;
            continue;
        };
        *by_rule.entry(rule).or_default() += 1;
        if let Some(document) = documents.get_mut(key) {
            document.sections[*at].mentions.push((edge, fact));
        }
    }
    let order = |rule: &str| RULES.iter().position(|known| known.code() == rule);
    for document in documents.values_mut() {
        for section in &mut document.sections {
            section.mentions.sort_by_key(|(edge, fact)| {
                (order(edge.props["rule"].as_str().unwrap_or("")), &fact.id)
            });
        }
    }

    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    for node in nodes.iter().filter(|node| {
        node.label == "Drift" && node.props["kind"].as_str() == Some("documentation-drift")
    }) {
        *by_kind
            .entry(node.props["drift_kind"].as_str().unwrap_or(""))
            .or_default() += 1;
    }

    // Each fact once, in the order the document lists it.
    let mut assertions = Assertions::new(&budget);
    let mut cited: BTreeSet<&str> = BTreeSet::new();
    for document in documents.values() {
        assertions.node(document.node, None)?;
        for section in &document.sections {
            assertions.node(section.node, None)?;
        }
        for section in &document.sections {
            for (edge, fact) in &section.mentions {
                assertions.edge(edge)?;
                if cited.insert(fact.id.as_str())
                    && !owner.contains_key(fact.id.as_str())
                    && !section_of.contains_key(fact.id.as_str())
                {
                    assertions.node(fact, None)?;
                }
            }
        }
    }

    let sections: usize = documents.values().map(|d| d.sections.len()).sum();
    let links: usize = by_rule.values().sum();
    let drift: usize = by_kind.values().sum();
    let mut content = Text::new(&budget, INTRO)?;
    writeln!(
        content,
        "Export mode: `{}`. Documents: {}. Sections: {sections}. Exact references linked to a \
         fact: {links}. Documentation drift entries: {drift}.\n",
        match mode {
            ExportMode::VerifiedOnly => "verified-only",
            ExportMode::BestEffort => "best-effort",
        },
        documents.len(),
    )?;
    if left_out > 0 {
        writeln!(
            content,
            "{left_out} links are not listed or counted: the fact each names is not in this \
             export.\n"
        )?;
    }

    content.push_str("## Links by rule\n\n| Rule | Links |\n|---|---|\n")?;
    for rule in RULES {
        writeln!(
            content,
            "| `{}` | {} |",
            rule.code(),
            by_rule.get(rule.code()).copied().unwrap_or(0)
        )?;
    }
    content.push_str(
        "\n## Drift by kind\n\nThe entries are listed in `drift_register.md`.\n\n\
         | Kind | Entries |\n|---|---|\n",
    )?;
    for kind in KINDS {
        writeln!(
            content,
            "| `{}` | {} |",
            kind.code(),
            by_kind.get(kind.code()).copied().unwrap_or(0)
        )?;
    }

    content.push_str("\n## Documents\n\n")?;
    if documents.is_empty() {
        content.push_str("No documentation file is among the facts of this export.\n")?;
    }
    for ((repo, path), document) in &documents {
        // A document's role besides documentation (ADR-0065 decision 6,
        // AC-1600): closed codes, printed only when it has one.
        let roles = document.node.props["roles"]
            .as_array()
            .map(|roles| {
                roles
                    .iter()
                    .filter_map(|role| role.as_str())
                    .filter(|role| {
                        [crate::ROLE_API_REFERENCE, crate::ROLE_DECISION_RECORD].contains(role)
                    })
                    .map(|role| format!("`{role}`"))
                    .collect::<Vec<_>>()
            })
            .filter(|roles| !roles.is_empty())
            .map(|roles| format!(" Roles: {}.", roles.join(", ")))
            .unwrap_or_default();
        writeln!(
            content,
            "### {}: {}\n\nTitle: {}. Format: `{}`.{} Citation: `{}`.\n\n\
             | Section | Level | Heading | Citation | Facts mentioned |\n|---|---|---|---|---|",
            Literal(repo),
            Literal(path),
            Literal(document.node.props["title"].as_str().unwrap_or("—")),
            Literal(document.node.props["format"].as_str().unwrap_or("—")),
            roles,
            node_citation(document.node),
        )?;
        for section in &document.sections {
            let heading = section.node.props["heading"].as_str().unwrap_or("");
            writeln!(
                content,
                "| {} | {} | {} | `{}` | {} |",
                section.node.props["ordinal"].as_u64().unwrap_or(0),
                section.node.props["level"].as_u64().unwrap_or(0),
                if heading.is_empty() {
                    "(no heading)".to_owned()
                } else {
                    Literal(heading).to_string()
                },
                node_citation(section.node),
                section.mentions.len(),
            )?;
        }
        if document.sections.iter().any(|s| !s.mentions.is_empty()) {
            content.push_str(
                "\n| Section | Fact | Kind | Rule | Occurrences | Fact citation | Link citation \
                 |\n|---|---|---|---|---|---|---|\n",
            )?;
        }
        for section in &document.sections {
            for (edge, fact) in &section.mentions {
                writeln!(
                    content,
                    "| {} | {} | {} | `{}` | {} | `{}` | `{}` |",
                    section.node.props["ordinal"].as_u64().unwrap_or(0),
                    Literal(&fact.id),
                    Literal(&fact.label),
                    // A closed code: printed only when it is one of the four.
                    RULES
                        .iter()
                        .map(|rule| rule.code())
                        .find(|code| edge.props["rule"].as_str() == Some(code))
                        .unwrap_or("unknown"),
                    edge.props["occurrences"].as_u64().unwrap_or(1),
                    node_citation(fact),
                    edge_citation(edge),
                )?;
            }
        }
        content.push_str("\n")?;
    }

    inline_literal(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
        attested: Vec::new(),
    })
}
