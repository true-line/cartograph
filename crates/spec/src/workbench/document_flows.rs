//! D1 §1.6 arranged per capability (#1985): what each operation's traced
//! flow does.
//!
//! For each derived Capability, the Endpoints that are the sources of its
//! cited `HANDLES` edges are its operations. The traced flow whose trigger is
//! that Endpoint is described in one line: its status, the handlers it
//! reaches, the further callables in trace order, and the DataEntities that
//! the flow's participants write and read (admitted `WRITES` / `READS`
//! edges), each with the operations its relation records. The handler and
//! data edges are cited. Operations whose flow reaches nothing past its
//! handler are listed together. Flows whose trigger no capability cites keep
//! a compact table; `flow_dossiers.md` keeps every hop.
//!
//! The line restates the trace, nothing more: the order of calls is the
//! tracer's traversal order, not an execution order, and a reached callable
//! is not shown to run on every request.

use crate::workbench::capabilities::{Stories, Support};
use crate::workbench::citation_id;
use crate::workbench::render::{Assertions, Literal, NodeName, RenderLimit, Text};
use core_graph::{Edge, FactKey, Node};
use flowtracer::Flow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Callables listed per flow before "and N more".
pub(super) const LISTED: usize = 8;

/// The admitted nodes and data relations, borrowed from the graph.
struct Index<'a> {
    by_id: BTreeMap<&'a str, &'a Node>,
    /// `WRITES` / `READS` edges into a DataEntity, by source callable.
    data: BTreeMap<&'a str, Vec<&'a Edge>>,
    /// Every admitted edge by `(src, label, dst)`, to cite a hop.
    edges: BTreeMap<(&'a str, &'a str, &'a str), &'a Edge>,
}

impl<'a> Index<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        let by_id: BTreeMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
        let mut data: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        let mut keyed = BTreeMap::new();
        for edge in edges {
            keyed.insert(
                (edge.src.as_str(), edge.label.as_str(), edge.dst.as_str()),
                *edge,
            );
            if matches!(edge.label.as_str(), "WRITES" | "READS")
                && by_id
                    .get(edge.dst.as_str())
                    .is_some_and(|n| n.label == "DataEntity")
            {
                data.entry(edge.src.as_str()).or_default().push(edge);
            }
        }
        Self {
            by_id,
            data,
            edges: keyed,
        }
    }
}

/// The operations a data relation records: `insert/update`, or the single
/// `operation`, or `unspecified`.
fn operations(edge: &Edge) -> String {
    edge.props["operations"]
        .as_array()
        .map(|ops| {
            ops.iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join("/")
        })
        .filter(|ops| !ops.is_empty())
        .or_else(|| edge.props["operation"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "unspecified".to_owned())
}

/// One flow, read for its line.
struct Reading<'a> {
    handlers: Vec<&'a str>,
    calls: Vec<&'a str>,
    /// Entity name → (label → operations).
    data: BTreeMap<String, BTreeMap<&'a str, BTreeSet<String>>>,
    cites: Vec<&'a Edge>,
    /// Hops the trace could not resolve.
    gaps: usize,
}

fn read<'a>(index: &Index<'a>, flow: &'a Flow) -> Reading<'a> {
    let mut reading = Reading {
        handlers: Vec::new(),
        calls: Vec::new(),
        data: BTreeMap::new(),
        cites: Vec::new(),
        gaps: 0,
    };
    let mut participants: Vec<&str> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for hop in &flow.hops {
        if hop.gap_reason.is_some() {
            reading.gaps += 1;
            continue;
        }
        let (src, label, dst) = hop.stored_edge();
        if hop.label == "HANDLES" && hop.src == flow.trigger {
            reading.handlers.push(hop.dst_name.as_str());
            if let Some(edge) = index.edges.get(&(src, label, dst)) {
                reading.cites.push(edge);
            }
        } else if seen.insert(hop.dst.as_str()) && hop.label == "CALLS" {
            reading.calls.push(hop.dst_name.as_str());
        }
        participants.push(hop.dst.as_str());
    }
    let mut cited: BTreeSet<&str> = BTreeSet::new();
    for participant in participants {
        if !cited.insert(participant) {
            continue;
        }
        for edge in index.data.get(participant).into_iter().flatten() {
            let entity = index
                .by_id
                .get(edge.dst.as_str())
                .map_or_else(|| edge.dst.clone(), |n| NodeName(n).to_string());
            reading
                .data
                .entry(entity)
                .or_default()
                .entry(edge.label.as_str())
                .or_default()
                .insert(operations(edge));
            reading.cites.push(edge);
        }
    }
    reading.calls.retain(|c| !reading.handlers.contains(c));
    reading
}

fn list(out: &mut Text, names: &[&str]) -> Result<(), RenderLimit> {
    for (i, name) in names.iter().take(LISTED).enumerate() {
        if i > 0 {
            out.push_str(", ")?;
        }
        write!(out, "{}", Literal(name))?;
    }
    if names.len() > LISTED {
        write!(out, " and {} more", names.len() - LISTED)?;
    }
    Ok(())
}

fn data_clause(out: &mut Text, reading: &Reading<'_>, label: &str) -> Result<(), RenderLimit> {
    let entities: Vec<(&String, &BTreeSet<String>)> = reading
        .data
        .iter()
        .filter_map(|(entity, by_label)| by_label.get(label).map(|ops| (entity, ops)))
        .collect();
    if entities.is_empty() {
        return Ok(());
    }
    out.push_str(if label == "WRITES" {
        "; writes "
    } else {
        "; reads "
    })?;
    for (i, (entity, ops)) in entities.iter().enumerate() {
        if i > 0 {
            out.push_str(", ")?;
        }
        let ops: Vec<&str> = ops.iter().map(String::as_str).collect();
        write!(out, "{} ({})", Literal(entity), Literal(ops.join(", ")))?;
    }
    Ok(())
}

/// §1.6 per capability. Returns the facts newly cited; `None` when no
/// capability's operation has a traced flow, so the caller keeps the
/// trigger table.
pub(super) fn flows_per_capability(
    (nodes, edges): (&[&Node], &[&Edge]),
    flows: &[Flow],
    knowledge: &Stories<'_>,
    (assertions, asserted): (&mut Assertions, &mut BTreeSet<String>),
    out: &mut Text,
) -> Result<Option<usize>, RenderLimit> {
    let by_trigger: BTreeMap<&str, &Flow> = flows.iter().map(|f| (f.trigger.as_str(), f)).collect();
    let mut sections: Vec<(&Node, Vec<&Flow>)> = Vec::new();
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    for story in knowledge
        .included
        .iter()
        .filter(|s| s.node.label == "Capability")
    {
        let mut operations: Vec<&Flow> = Vec::new();
        for (_, fact) in &story.support {
            if let Support::Edge(e) = fact
                && e.label == "HANDLES"
                && let Some(flow) = by_trigger.get(e.src.as_str())
                && !operations.iter().any(|f| f.trigger == flow.trigger)
            {
                operations.push(flow);
            }
        }
        if operations.is_empty() {
            continue;
        }
        placed.extend(operations.iter().map(|f| f.trigger.as_str()));
        sections.push((story.node, operations));
    }
    if sections.is_empty() {
        return Ok(None);
    }
    let index = Index::new(nodes, edges);
    writeln!(
        out,
        "Traced flows per capability: {} operations across {} capabilities. Each line \
         restates its flow's trace: the handler, the further callables in trace order (not \
         an execution order), and the entities the flow's participants write and read, with \
         the operations each relation records. Every hop, with its tier, confidence and \
         citation, is in `flow_dossiers.md`.\n",
        placed.len(),
        sections.len()
    )?;
    let mut cited = 0usize;
    for (capability, operations) in sections {
        writeln!(out, "### {}\n", Literal(NodeName(capability)))?;
        let mut handled_only: Vec<String> = Vec::new();
        for flow in operations {
            let reading = read(&index, flow);
            let name = by_name(&index, flow);
            if reading.calls.is_empty() && reading.data.is_empty() && reading.gaps == 0 {
                handled_only.push(name);
                continue;
            }
            write!(out, "- **{}** ({})", Literal(&name), flow.status.as_str())?;
            if !reading.handlers.is_empty() {
                out.push_str(": handled by ")?;
                list(out, &reading.handlers)?;
            }
            if !reading.calls.is_empty() {
                out.push_str("; calls ")?;
                list(out, &reading.calls)?;
            }
            data_clause(out, &reading, "WRITES")?;
            data_clause(out, &reading, "READS")?;
            match reading.gaps {
                0 => {}
                1 => out.push_str("; 1 hop is unresolved")?,
                n => write!(out, "; {n} hops are unresolved")?,
            }
            if flow.depth_limited {
                out.push_str("; the trace stops at its depth bound")?;
            }
            out.push_str(".")?;
            let mut ids: Vec<String> = Vec::new();
            for edge in &reading.cites {
                let id = citation_id(&FactKey::Edge {
                    source: edge.src.clone(),
                    label: edge.label.clone(),
                    destination: edge.dst.clone(),
                });
                if !ids.contains(&id) {
                    ids.push(id.clone());
                }
                if asserted.insert(id) {
                    assertions.edge(edge)?;
                    cited += 1;
                }
            }
            if !ids.is_empty() {
                write!(out, " Cites {}.", ids.join(", "))?;
            }
            out.push_str("\n")?;
        }
        if !handled_only.is_empty() {
            let names: Vec<&str> = handled_only.iter().map(String::as_str).collect();
            out.push_str("- Handled with nothing further traced: ")?;
            list(out, &names)?;
            out.push_str(".\n")?;
        }
        out.push_str("\n")?;
    }
    let other: Vec<&Flow> = flows
        .iter()
        .filter(|f| !placed.contains(f.trigger.as_str()))
        .collect();
    if !other.is_empty() {
        writeln!(
            out,
            "### Flows outside the capabilities\n\n| Trigger | Kind | Status | Hops |\n|---|---|---|---|"
        )?;
        for flow in other {
            writeln!(
                out,
                "| {} | {} | {} | {} |",
                Literal(&flow.trigger_name),
                Literal(&flow.trigger_kind),
                flow.status.as_str(),
                flow.hops.len()
            )?;
        }
    }
    Ok(Some(cited))
}

fn by_name(index: &Index<'_>, flow: &Flow) -> String {
    index
        .by_id
        .get(flow.trigger.as_str())
        .map_or_else(|| flow.trigger_name.clone(), |n| NodeName(n).to_string())
}

#[cfg(test)]
#[path = "document_flows_tests.rs"]
mod tests;
