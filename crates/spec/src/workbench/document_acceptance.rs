//! D1 §1.4 arranged per capability (#1443): the test scenarios that
//! exercise a capability's operations.
//!
//! For each derived Capability, the Endpoints that are the sources of its
//! cited `HANDLES` edges are its operations. Every admitted `REQUESTS` edge
//! from a recovered TestCase to one of those Endpoints makes the test an
//! acceptance scenario of the capability: its qualified name, as the test
//! author wrote it, is listed with the operations it requests and the
//! citation of each `REQUESTS` edge.
//!
//! A reference is static: it does not show that the test runs or passes,
//! and the test's assertions are not read. `test_arrangements.md` keeps every
//! reference with its asserted statuses and response keys.

use crate::workbench::capabilities::{Stories, Story, Support};
use crate::workbench::citation_id;
use crate::workbench::render::{Assertions, Literal, NodeName, RenderLimit, Text};
use core_graph::{Edge, FactKey, Node};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The admitted test references, borrowed from the graph.
struct Index<'a> {
    by_id: BTreeMap<&'a str, &'a Node>,
    requests: BTreeMap<&'a str, Vec<&'a Edge>>,
}

impl<'a> Index<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
        let mut requests: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        for edge in edges {
            if edge.label != "REQUESTS" {
                continue;
            }
            let (Some(src), Some(dst)) =
                (by_id.get(edge.src.as_str()), by_id.get(edge.dst.as_str()))
            else {
                continue;
            };
            if src.label == "TestCase" && dst.label == "Endpoint" {
                requests.entry(&dst.id).or_default().push(edge);
            }
        }
        Self { by_id, requests }
    }

    /// The tests that request a capability's operations, by test id, each
    /// with its `REQUESTS` edges. Operations are the sources of the
    /// `HANDLES` edges it cites.
    fn scenarios(&self, story: &Story<'a>) -> BTreeMap<&'a str, Vec<&'a Edge>> {
        let operations: BTreeSet<&str> = story
            .support
            .iter()
            .filter_map(|(_, fact)| match fact {
                Support::Edge(e) if e.label == "HANDLES" => Some(e.src.as_str()),
                _ => None,
            })
            .collect();
        let mut tests: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        for operation in operations {
            for edge in self.requests.get(operation).into_iter().flatten() {
                tests.entry(edge.src.as_str()).or_default().push(edge);
            }
        }
        tests
    }
}

/// A test name as words: each `test_snake_case` segment of the qualified
/// name is shown as `snake case`; a segment that already has spaces is kept.
fn readable(qualified: &str) -> String {
    qualified
        .split(" > ")
        .map(|segment| {
            if segment.contains(char::is_whitespace) || !segment.contains('_') {
                return segment.to_owned();
            }
            let bare = segment.strip_prefix("test_").unwrap_or(segment);
            bare.split('_')
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(" > ")
}

fn edge_citation(edge: &Edge) -> String {
    citation_id(&FactKey::Edge {
        source: edge.src.clone(),
        label: edge.label.clone(),
        destination: edge.dst.clone(),
    })
}

fn write_row(
    index: &Index<'_>,
    (test, references): (&str, &[&Edge]),
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<(), RenderLimit> {
    let node = index.by_id.get(test).copied();
    let scenario = node.and_then(|n| n.props["qualified_name"].as_str());
    let file = node.and_then(|n| n.props["path"].as_str()).unwrap_or("—");
    match (scenario, node) {
        (Some(scenario), _) => write!(out, "| {} ", Literal(&readable(scenario)))?,
        (None, Some(node)) => write!(out, "| {} ", Literal(NodeName(node)))?,
        (None, None) => write!(out, "| {} ", Literal(test))?,
    }
    write!(out, "| {} | ", Literal(file))?;
    for (i, edge) in references.iter().enumerate() {
        if i > 0 {
            out.push_str("; ")?;
        }
        match index.by_id.get(edge.dst.as_str()) {
            Some(endpoint) => write!(out, "{}", Literal(NodeName(endpoint)))?,
            None => write!(out, "{}", Literal(&edge.dst))?,
        }
    }
    out.push_str(" | ")?;
    for (i, edge) in references.iter().enumerate() {
        if i > 0 {
            out.push_str(", ")?;
        }
        let id = edge_citation(edge);
        out.push_str(&id)?;
        if asserted.insert(id) {
            assertions.edge(edge)?;
        }
    }
    out.push_str(" |\n")?;
    Ok(())
}

const HEADER: &str = "| Scenario | Test file | Operations | Citation |\n|---|---|---|---|\n";

/// The test scenarios each Capability's operations are requested by.
/// Returns the number of rows printed; prints nothing when no capability's
/// operation is requested by a recovered test. The retained workspace is
/// [`acceptance_per_capability_workspace`].
pub(super) fn acceptance_per_capability(
    (nodes, edges): (&[&Node], &[&Edge]),
    knowledge: &Stories<'_>,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<usize, RenderLimit> {
    let index = Index::new(nodes, edges);
    if index.requests.is_empty() {
        return Ok(0);
    }
    let capabilities = || {
        knowledge
            .included
            .iter()
            .filter(|s| s.node.label == "Capability")
    };
    // Counted first so the lead states the total before any row.
    let total: usize = capabilities().map(|s| index.scenarios(s).len()).sum();
    if total == 0 {
        return Ok(0);
    }
    writeln!(
        out,
        "Every test reference, with its asserted statuses and response keys, is in \
         `test_arrangements.md` of this export, and every rule observation is in \
         `rule-evidence.md`.\n\nAcceptance scenarios per capability: {total}. Each is a recovered test that issues \
         a request to one of the capability's operations, named as its author wrote it. The \
         reference is static: it is not evidence that the test runs or passes, and the test's \
         assertions are not restated here.\n"
    )?;
    let mut printed = 0;
    for story in capabilities() {
        let scenarios = index.scenarios(story);
        if scenarios.is_empty() {
            continue;
        }
        writeln!(out, "### {}\n", Literal(NodeName(story.node)))?;
        out.push_str(HEADER)?;
        for (test, references) in &scenarios {
            write_row(
                &index,
                (test, references),
                (&mut *assertions, &mut *asserted),
                out,
            )?;
            printed += 1;
        }
        out.push_str("\n")?;
    }
    Ok(printed)
}

/// Logical workspace of [`acceptance_per_capability`] beyond the borrowed
/// graph, stories and output: the borrowed id and request indexes, one
/// capability's operation set and scenario map, and one row's citation ids.
/// Charged at the graph's row and byte ceilings; no allocator or RSS claim.
/// `None` on overflow.
pub(crate) fn acceptance_per_capability_workspace(
    graph: flowtracer::TraceGraphBound,
) -> Option<usize> {
    use crate::workbench::accounting::{map_slots, set_slots, slots, sum};
    let rows = graph.rows;
    let index = sum(&[
        map_slots::<&str, &Node>(rows)?,
        map_slots::<&str, Vec<&Edge>>(rows)?,
        slots::<&Edge>(rows)?,
    ])?;
    let capability = sum(&[
        set_slots::<&str>(rows)?,
        map_slots::<&str, Vec<&Edge>>(rows)?,
        slots::<&Edge>(rows)?,
    ])?;
    // One row: the citation id hashed from the cloned edge key.
    let row = graph.bytes.checked_mul(2)?;
    sum(&[index, capability, row])
}

#[cfg(test)]
#[path = "document_acceptance_tests.rs"]
mod tests;
