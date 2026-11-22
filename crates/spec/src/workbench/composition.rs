//! The context a section composer is handed (#1991, AC-0862).
//!
//! A composer drafts one D1 section from that section as the export
//! compiles it and the facts the section cites. It does not search the
//! graph. This module holds the two pure steps that pick that context.
//!
//! - [`section_text`] takes one section out of a compiled D1: from its
//!   `## <number> ` heading up to the next `## ` heading.
//! - [`composition_facts`] lists the facts the composer may cite: every base
//!   fact whose citation id the section text prints, and for each derived
//!   knowledge fact the text prints (a Capability or an Actor), that fact
//!   and the base facts it rests on. The derived fact is citable (#2040): the
//!   export lists it in the fact index and a draft citing it stages and
//!   resolves. It is also listed as a group heading over its base facts.
//!
//! **What a row shows.** The fact's label, tier and confidence, and its
//! display name, unless that name is one of the fact's source identifiers
//! (a file path or a code symbol, [`SourceIdentifiers`]). A statement may not
//! name those (SPEC-16 rule 3), so they are not shown to the composer either.
//! Node ids are never shown: they encode paths. A derived fact's cited code
//! declarations ([`CODE_LABELS`]) are left out: without a name they carry
//! nothing a reader can use.
//!
//! **Cost.** One scan of the section text, then one pass over the nodes and
//! one over the edges.

use super::neutrality::{CODE_LABELS, SourceIdentifiers};
use super::render::NodeName;
use super::{citation_id, provenance};
use core_graph::{Edge, FactKey, Node};
use std::collections::{BTreeMap, BTreeSet};

/// One citable fact as the composer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionFact {
    /// Its ADR-0058 citation id.
    pub citation_id: String,
    /// Node label or edge label.
    pub label: String,
    /// A name that is not a source identifier, if it has one.
    pub name: Option<String>,
    /// Producing tier, such as `Deterministic`.
    pub tier: String,
    /// Confidence tier, such as `Verified`.
    pub confidence: String,
}

/// A derived fact the section prints, with the base facts it rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedGroup {
    /// The derived fact's citation id; citable too (#2040).
    pub citation_id: String,
    /// Its label, such as `Capability`.
    pub label: String,
    /// Its display name, or its label when the name is its id or names a
    /// fact it rests on by id.
    pub name: String,
    /// The citable base facts it rests on, in citation id order.
    pub rests_on: Vec<String>,
}

/// What a composer may cite for one section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompositionFacts {
    /// Every citable fact, in citation id order.
    pub facts: Vec<CompositionFact>,
    /// Derived facts the section prints, in citation id order.
    pub derived: Vec<DerivedGroup>,
}

impl CompositionFacts {
    /// The citable ids.
    pub fn citable(&self) -> BTreeSet<String> {
        self.facts
            .iter()
            .map(|fact| fact.citation_id.clone())
            .collect()
    }
}

/// Section `number` of the compiled D1 `document`, heading included, or
/// none when the document has no such section.
pub fn section_text<'a>(document: &'a str, number: &str) -> Option<&'a str> {
    let heading = format!("\n## {number} ");
    let start = document.find(&heading)? + 1;
    let rest = &document[start..];
    let end = rest[3..].find("\n## ").map_or(rest.len(), |at| at + 3 + 1);
    Some(&rest[..end])
}

/// Sections a composer reads as an overview: up to the first `###`
/// heading. D1 1.3 prints the capability and area tables first, then one
/// operation table per capability (#2012); those tables are detail an
/// overview does not need, and on a large system they push the request past
/// its cap (#1991).
const OVERVIEW_SECTIONS: &[&str] = &["1.3"];

/// The part of section `number`'s compiled `text` a composer reads, and how
/// many capability subsections it leaves out. An overview section ends before its
/// first `###` heading, with a line saying how many subsections are left
/// out; any other section is read whole.
pub fn composition_text(text: &str, number: &str) -> (String, usize) {
    if !OVERVIEW_SECTIONS.contains(&number) {
        return (text.to_owned(), 0);
    }
    let Some(at) = text.find("\n### ") else {
        return (text.to_owned(), 0);
    };
    // With areas (#260) each capability is a `####` under its area's `###`.
    let rest = &text[at..];
    let left_out = match rest.matches("\n#### ").count() {
        0 => rest.matches("\n### ").count(),
        capabilities => capabilities,
    };
    let mut kept = text[..=at].to_owned();
    kept.push_str(&format!(
        "\nPer-capability detail ({left_out} subsections) is left out of this composition.\n"
    ));
    (kept, left_out)
}

/// Sections that print no fact of their own and are composed from the
/// sections they summarise (#2319). D1 1.1 Purpose and scope is
/// agent-authored only, so it reads the actors of 1.2 and the 1.3 overview.
const SUMMARY_SECTIONS: &[(&str, &[&str])] = &[("1.1", &["1.2", "1.3"])];

/// What a composer reads for section `number` of the compiled `document`:
/// the section's [`composition_text`], or, for a section that summarises
/// others, its heading line followed by each summarised section's
/// composition text. `None` when the section is not compiled.
pub fn composition_source(document: &str, number: &str) -> Option<String> {
    let own = section_text(document, number)?;
    let Some((_, summarised)) = SUMMARY_SECTIONS.iter().find(|(n, _)| *n == number) else {
        return Some(composition_text(own, number).0);
    };
    let heading = own.lines().next().unwrap_or_default();
    let mut text = format!("{heading}\n\nThis section summarises the sections below.\n\n");
    for source in *summarised {
        if let Some(found) = section_text(document, source) {
            text.push_str(&composition_text(found, source).0);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
    }
    Some(text)
}

/// Every `F-` citation id `text` prints.
pub(super) fn printed_ids(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut ids = BTreeSet::new();
    let mut at = 0;
    while let Some(found) = text[at..].find("F-") {
        let start = at + found;
        let hex = &bytes[start + 2..];
        let whole = hex.len() >= 16
            && hex[..16]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            && !hex.get(16).is_some_and(u8::is_ascii_alphanumeric)
            && !(start > 0 && bytes[start - 1].is_ascii_alphanumeric());
        if whole {
            ids.insert(text[start..start + 18].to_owned());
            at = start + 18;
        } else {
            at = start + 2;
        }
    }
    ids
}

fn base_key(cite: &core_graph::overlay::BaseRef) -> FactKey {
    match cite {
        core_graph::overlay::BaseRef::Node(id) => FactKey::Node { id: id.clone() },
        core_graph::overlay::BaseRef::Edge(source, destination, label) => FactKey::Edge {
            source: source.clone(),
            label: label.clone(),
            destination: destination.clone(),
        },
    }
}

/// A derived fact's display name, unless it is its id or names a fact it
/// rests on by id: an Actor derived from one endpoint's callers is named
/// after that endpoint's node id, which encodes source paths.
fn derived_name(node: &Node, cites: &BTreeSet<core_graph::overlay::BaseRef>) -> Option<String> {
    let name = NodeName(node).to_string();
    let names_an_id = cites.iter().any(|cite| match cite {
        core_graph::overlay::BaseRef::Node(id) => name.contains(id.as_str()),
        core_graph::overlay::BaseRef::Edge(source, destination, _) => {
            name.contains(source.as_str()) || name.contains(destination.as_str())
        }
    });
    (name != node.id && !names_an_id).then_some(name)
}

/// The facts a composer may cite for `section`, a section of the compiled
/// D1, over the base `nodes` and `edges` and the derived knowledge `derived`
/// (each derived node with the base facts it rests on).
pub fn composition_facts(
    section: &str,
    nodes: &[Node],
    edges: &[Edge],
    derived: &[(&Node, &BTreeSet<core_graph::overlay::BaseRef>)],
) -> CompositionFacts {
    let printed = printed_ids(section);
    // Derived facts the text prints, and the base facts behind each.
    let mut groups: BTreeMap<String, (&Node, Option<String>, BTreeSet<String>)> = BTreeMap::new();
    for (node, cites) in derived {
        let id = citation_id(&FactKey::Node {
            id: node.id.clone(),
        });
        if printed.contains(&id) {
            let behind = cites
                .iter()
                .map(|cite| citation_id(&base_key(cite)))
                .collect();
            groups.insert(id, (node, derived_name(node, cites), behind));
        }
    }
    let behind: BTreeSet<&String> = groups.values().flat_map(|(_, _, ids)| ids).collect();
    let wanted = |id: &String| printed.contains(id) || behind.contains(id);
    let mut rows: BTreeMap<String, (String, Option<String>, core_prov::Provenance)> =
        BTreeMap::new();
    let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let mut cited = BTreeSet::new();
    for node in nodes {
        let id = citation_id(&FactKey::Node {
            id: node.id.clone(),
        });
        if !wanted(&id) {
            continue;
        }
        // A derived fact's code declarations add nothing a reader can use.
        if !printed.contains(&id) && CODE_LABELS.contains(&node.label.as_str()) {
            continue;
        }
        cited.insert(id.clone());
        let name = NodeName(node).to_string();
        let name = (name != node.id).then_some(name);
        rows.insert(
            id,
            (
                node.label.clone(),
                name,
                provenance(&node.props, &format!("node:{}", node.id)),
            ),
        );
    }
    for edge in edges {
        let id = citation_id(&FactKey::Edge {
            source: edge.src.clone(),
            label: edge.label.clone(),
            destination: edge.dst.clone(),
        });
        if !wanted(&id) {
            continue;
        }
        cited.insert(id.clone());
        let end = |node_id: &str| {
            by_id
                .get(node_id)
                .map(|node| NodeName(node).to_string())
                .filter(|name| name != node_id)
        };
        let name = match (end(&edge.src), end(&edge.dst)) {
            (Some(source), Some(destination)) => Some(format!("{source} → {destination}")),
            _ => None,
        };
        let identity = format!("edge:{} {} {}", edge.src, edge.label, edge.dst);
        rows.insert(
            id,
            (edge.label.clone(), name, provenance(&edge.props, &identity)),
        );
    }
    // Each derived fact the text prints is citable as itself (#2040).
    for (id, (node, name, _)) in &groups {
        cited.insert(id.clone());
        let name = name.clone();
        rows.insert(
            id.clone(),
            (
                node.label.clone(),
                name,
                provenance(&node.props, &format!("node:{}", node.id)),
            ),
        );
    }
    // No row shows a name a statement may not use.
    let source = SourceIdentifiers::of(nodes, edges, &cited);
    let facts = rows
        .into_iter()
        .map(|(id, (label, name, prov))| {
            let citation = std::slice::from_ref(&id);
            let name = name.filter(|name| !source.named_in(name, citation));
            CompositionFact {
                citation_id: id,
                label,
                name,
                tier: format!("{:?}", prov.tier),
                confidence: format!("{:?}", prov.confidence_tier),
            }
        })
        .collect::<Vec<_>>();
    let citable: BTreeSet<&str> = facts.iter().map(|f| f.citation_id.as_str()).collect();
    let derived = groups
        .into_iter()
        .map(|(id, (node, name, behind))| DerivedGroup {
            citation_id: id,
            label: node.label.clone(),
            name: name.unwrap_or_else(|| node.label.clone()),
            rests_on: behind
                .into_iter()
                .filter(|id| citable.contains(id.as_str()))
                .collect(),
        })
        .collect();
    CompositionFacts { facts, derived }
}

#[cfg(test)]
#[path = "composition_tests.rs"]
mod tests;
