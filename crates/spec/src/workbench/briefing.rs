//! The briefing a topic expert is handed for one area (#2055, AC-0931).
//!
//! A topic expert answers questions about one subsystem area. It starts
//! from that area's part of the product specification instead of searching
//! for it. The briefing is compiled, never written by a model, in two pure
//! steps:
//!
//! - [`area_neighbours`] names the other areas this area's code calls and
//!   the areas that call it, from the base `CALLS` edges and the areas'
//!   member files. Names only: a neighbour's content is its own briefing.
//! - [`area_briefing`] cuts the area's block out of each D1 section the
//!   export arranges by area ([`AREA_SECTIONS`]), exactly as printed, and
//!   puts the neighbours in front. Gaps the specification states inside
//!   those blocks stay where it prints them.
//!
//! **Bound.** The briefing holds at most `max_bytes` of blocks. Budget goes
//! to the smallest blocks first, so a short section is never cut to make
//! room for a long one. A block over its share loses whole capability
//! subsections (`####`) from its end, and says how many. The introduction,
//! the section headings and each block's own heading are outside the bound.
//!
//! **Cost.** One scan of the document per section, one pass over the
//! Subsystem facts and one over the edges.

use super::capabilities::area_name;
use super::composition::section_text;
use super::render::Literal;
use core_graph::Edge;
use core_graph::overlay::{BaseRef, OverlayFact, OverlayItem};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// The D1 sections arranged by area (#260), in document order.
pub const AREA_SECTIONS: &[&str] = &["1.3", "1.4", "1.5", "1.6"];

/// The bound a host compiles a briefing under.
pub const AREA_BRIEFING_BYTES: usize = 96 * 1024;

/// The areas next to one area, by name, each in name order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AreaNeighbours {
    /// Areas whose code this area's code calls.
    pub calls: Vec<String>,
    /// Areas whose code calls this area's.
    pub called_by: Vec<String>,
}

/// One area's briefing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaBriefing {
    /// The briefing, as Markdown.
    pub text: String,
    /// The sections that hold a block for the area.
    pub sections: Vec<&'static str>,
    /// Capability subsections left out to keep the bound.
    pub left_out: usize,
}

/// The named areas next to the Subsystem `area`, over the base `edges` and
/// the overlay's `subsystems`. A file belongs to the first Subsystem, in id
/// order, that cites it. None when `subsystems` holds no such area.
pub fn area_neighbours(
    edges: &[Edge],
    subsystems: &[OverlayFact],
    area: &str,
) -> Option<AreaNeighbours> {
    let mut named: Vec<(&str, &str, &BTreeSet<BaseRef>)> = subsystems
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Node(node) if node.label == "Subsystem" => {
                Some((node.id.as_str(), area_name(node), &fact.cites))
            }
            _ => None,
        })
        .collect();
    named.sort_by_key(|(id, ..)| *id);
    if !named.iter().any(|(id, ..)| *id == area) {
        return None;
    }
    // File id → the (id, name) of its area.
    let mut area_of: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for (id, name, cites) in &named {
        for cite in *cites {
            if let BaseRef::Node(file) = cite {
                area_of.entry(file.as_str()).or_insert((id, name));
            }
        }
    }
    // What is defined in a member file is in that file's area.
    let mut owner: BTreeMap<&str, (&str, &str)> = area_of.clone();
    for edge in edges {
        if edge.label == "DEFINED_IN"
            && let Some(found) = area_of.get(edge.dst.as_str())
        {
            owner.entry(edge.src.as_str()).or_insert(*found);
        }
    }
    let mut calls = BTreeSet::new();
    let mut called_by = BTreeSet::new();
    for edge in edges {
        if edge.label != "CALLS" {
            continue;
        }
        let (Some((from, from_name)), Some((to, to_name))) =
            (owner.get(edge.src.as_str()), owner.get(edge.dst.as_str()))
        else {
            continue;
        };
        if from == to {
            continue;
        }
        if *from == area && !to_name.is_empty() {
            calls.insert((*to_name).to_owned());
        } else if *to == area && !from_name.is_empty() {
            called_by.insert((*from_name).to_owned());
        }
    }
    Some(AreaNeighbours {
        calls: calls.into_iter().collect(),
        called_by: called_by.into_iter().collect(),
    })
}

/// The block of the area named `name` in one compiled section: from its
/// `### <name>` heading up to the next `###` heading.
fn area_block<'a>(section: &'a str, name: &str) -> Option<&'a str> {
    let heading = format!("\n### {}\n", Literal(name));
    let start = section.find(&heading)? + 1;
    let rest = &section[start..];
    let end = rest[4..].find("\n### ").map_or(rest.len(), |at| at + 4 + 1);
    Some(&rest[..end])
}

/// The length of a block's own heading line, which is outside the bound.
fn heading_len(block: &str) -> usize {
    block.find('\n').map_or(block.len(), |at| at + 1)
}

/// `block` with at most `budget` bytes after its heading, and how many
/// capability subsections that leaves out. A block over budget keeps its
/// heading and its leading subsections that fit.
fn fitted(block: &str, budget: usize) -> (&str, usize) {
    let head = heading_len(block);
    if block.len() - head <= budget {
        return (block, 0);
    }
    let mut starts: Vec<usize> = block
        .match_indices("\n#### ")
        .map(|(at, _)| at + 1)
        .collect();
    if starts.is_empty() {
        // One undivided block: keep the heading line and count it as one.
        return (&block[..head], 1);
    }
    starts.push(block.len());
    let kept = starts
        .iter()
        .rposition(|end| end.saturating_sub(head) <= budget)
        .unwrap_or_default();
    (&block[..starts[kept]], starts.len() - 1 - kept)
}

/// The briefing for the area named `name` of `files` member files, from the
/// compiled D1 `document`, holding at most `max_bytes` of the area's blocks.
pub fn area_briefing(
    document: &str,
    name: &str,
    files: usize,
    neighbours: &AreaNeighbours,
    max_bytes: usize,
) -> AreaBriefing {
    // Each area section with the area's block in it, in document order.
    let blocks: Vec<(&'static str, &str, &str)> = AREA_SECTIONS
        .iter()
        .filter_map(|number| {
            let section = section_text(document, number)?;
            let block = area_block(section, name)?;
            Some((*number, section.lines().next().unwrap_or_default(), block))
        })
        .collect();
    // Smallest first, so what a short block does not use goes to the rest.
    let mut order: Vec<usize> = (0..blocks.len()).collect();
    order.sort_by_key(|at| blocks[*at].2.len());
    let mut kept: Vec<(&str, usize)> = vec![("", 0); blocks.len()];
    let mut remaining = max_bytes;
    for (done, at) in order.iter().enumerate() {
        let share = remaining / (blocks.len() - done);
        kept[*at] = fitted(blocks[*at].2, share);
        // A block's own heading is outside the bound.
        let used = kept[*at].0.len() - heading_len(kept[*at].0);
        remaining = remaining.saturating_sub(used);
    }
    let list = |names: &[String]| {
        names
            .iter()
            .map(|name| Literal(name).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = String::new();
    let _ = writeln!(text, "# Area briefing: {}\n", Literal(name));
    let _ = writeln!(
        text,
        "This is the part of the product specification that the area holds, as the export compiles it. The area is a candidate grouping of {files} files, inferred by rule; it is not a confirmed boundary. A citation id (F-…) is the one the product specification prints.\n"
    );
    if neighbours.calls.is_empty() {
        text.push_str("- Calls into other areas: none recovered.\n");
    } else {
        let _ = writeln!(text, "- Calls into: {}.", list(&neighbours.calls));
    }
    if neighbours.called_by.is_empty() {
        text.push_str("- Called from other areas: none recovered.\n");
    } else {
        let _ = writeln!(text, "- Called from: {}.", list(&neighbours.called_by));
    }
    if blocks.is_empty() {
        text.push_str("\nThe product specification lists nothing under this area.\n");
    }
    let mut left_out = 0;
    for ((_, heading, _), (block, dropped)) in blocks.iter().zip(&kept) {
        let _ = write!(text, "\n{heading}\n\n{}", block.trim_end());
        text.push('\n');
        if *dropped > 0 {
            let _ = writeln!(
                text,
                "\n{dropped} capability subsections of this area are left out of this briefing to keep its bound."
            );
        }
        left_out += dropped;
    }
    AreaBriefing {
        text,
        sections: blocks.iter().map(|(number, ..)| *number).collect(),
        left_out,
    }
}

#[cfg(test)]
#[path = "briefing_tests.rs"]
mod tests;
