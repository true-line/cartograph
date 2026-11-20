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

use crate::workbench::capabilities::{Stories, Story, Support};
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
    /// `(node id, display name)` of each handler and further callable.
    handlers: Vec<(&'a str, &'a str)>,
    calls: Vec<(&'a str, &'a str)>,
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
    // Files a process start or scheduled job enters (#1997): the callables
    // defined in them are what handles it.
    let mut entered: BTreeSet<&str> = BTreeSet::new();
    for hop in &flow.hops {
        // A Gap destination or a Gap-confidence edge is unresolved: its
        // destination is not named as a reached callable.
        if hop.gap_reason.is_some() || hop.confidence == "Gap" {
            reading.gaps += 1;
            continue;
        }
        let (src, label, dst) = hop.stored_edge();
        let entry = hop.label == "ENTRY" && hop.src == flow.trigger;
        if entry {
            if let Some(edge) = index.edges.get(&(src, label, dst)) {
                reading.cites.push(edge);
            }
            match index.by_id.get(hop.dst.as_str()).map(|n| n.label.as_str()) {
                Some("File") => {
                    entered.insert(hop.dst.as_str());
                }
                _ => reading
                    .handlers
                    .push((hop.dst.as_str(), hop.dst_name.as_str())),
            }
        } else if hop.label == "DEFINED_IN" && entered.contains(hop.src.as_str()) {
            reading
                .handlers
                .push((hop.dst.as_str(), hop.dst_name.as_str()));
        } else if hop.label == "HANDLES" && hop.src == flow.trigger {
            reading
                .handlers
                .push((hop.dst.as_str(), hop.dst_name.as_str()));
            if let Some(edge) = index.edges.get(&(src, label, dst)) {
                reading.cites.push(edge);
            }
        } else if seen.insert(hop.dst.as_str()) && hop.label == "CALLS" {
            reading
                .calls
                .push((hop.dst.as_str(), hop.dst_name.as_str()));
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
    // By node identity: another callable may share a handler's name.
    reading
        .calls
        .retain(|(id, _)| !reading.handlers.iter().any(|(h, _)| h == id));
    reading
}

fn list<'n>(
    out: &mut Text,
    names: impl ExactSizeIterator<Item = &'n str>,
) -> Result<(), RenderLimit> {
    let total = names.len();
    for (i, name) in names.take(LISTED).enumerate() {
        if i > 0 {
            out.push_str(", ")?;
        }
        write!(out, "{}", Literal(name))?;
    }
    if total > LISTED {
        write!(out, " and {} more", total - LISTED)?;
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
    // A capability's operations with a traced flow, in citation order. Built
    // per capability, once to count and once to render, so no capability's
    // list outlives its own section.
    let operations_of = |story: &Story<'_>| -> Vec<&Flow> {
        let mut operations: Vec<&Flow> = Vec::new();
        for (_, fact) in &story.support {
            // An operation is the source of a cited `HANDLES` edge, or a
            // cited process start or scheduled job that triggers a flow
            // itself (#1997).
            let trigger = match fact {
                Support::Edge(e) if e.label == "HANDLES" => e.src.as_str(),
                Support::Node(n) if matches!(n.label.as_str(), "EntryPoint" | "Schedule") => {
                    n.id.as_str()
                }
                _ => continue,
            };
            if let Some(flow) = by_trigger.get(trigger)
                && !operations.iter().any(|f| f.trigger == flow.trigger)
            {
                operations.push(flow);
            }
        }
        operations
    };
    let listed = knowledge.capabilities();
    let capabilities = || listed.iter().map(|(_, story)| *story);
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    let mut sections = 0usize;
    for story in capabilities() {
        let operations = operations_of(story);
        if !operations.is_empty() {
            sections += 1;
            placed.extend(operations.iter().map(|f| f.trigger.as_str()));
        }
    }
    if sections == 0 {
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
        sections
    )?;
    let mut cited = 0usize;
    let mut headings = crate::workbench::document::AreaHeadings::new(knowledge);
    for &(area, story) in &listed {
        let operations = operations_of(story);
        if operations.is_empty() {
            continue;
        }
        let marks = headings.before(area, out)?;
        writeln!(out, "{marks} {}\n", Literal(NodeName(story.node)))?;
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
                list(out, reading.handlers.iter().map(|(_, name)| *name))?;
            }
            if !reading.calls.is_empty() {
                out.push_str("; calls ")?;
                list(out, reading.calls.iter().map(|(_, name)| *name))?;
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
            out.push_str("- Handled with nothing further traced: ")?;
            list(out, handled_only.iter().map(String::as_str))?;
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

/// Logical workspace of [`flows_per_capability`] beyond the borrowed graph,
/// flows, stories and output: the borrowed id, data-relation and edge
/// indexes, the trigger map and placed set, one capability's operation list,
/// and one flow's reading, citation ids and handler-only names. A flow's hops
/// and an entity's operations are graph rows; names and operations are graph
/// bytes. Charged at the graph's row and byte ceilings; no allocator or RSS
/// claim. `None` on overflow.
pub(crate) fn flows_per_capability_workspace(graph: flowtracer::TraceGraphBound) -> Option<usize> {
    use crate::workbench::accounting::{map_slots, set_slots, slots, sum};
    let rows = graph.rows;
    let index = sum(&[
        map_slots::<&str, &Node>(rows)?,
        map_slots::<&str, Vec<&Edge>>(rows)?,
        slots::<&Edge>(rows)?,
        map_slots::<(&str, &str, &str), &Edge>(rows)?,
    ])?;
    let placement = sum(&[
        map_slots::<&str, &Flow>(rows)?,
        set_slots::<&str>(rows)?,
        slots::<&Flow>(rows)?,
    ])?;
    // One flow: handlers, calls and participants, the seen and cited sets,
    // the entity map with its label maps and operation sets, the cited edges
    // and their ids, then the handler-only names of the section.
    let reading = sum(&[
        slots::<(&str, &str)>(rows)?.checked_mul(2)?,
        slots::<&str>(rows)?,
        set_slots::<&str>(rows)?.checked_mul(2)?,
        map_slots::<String, BTreeMap<&str, BTreeSet<String>>>(rows)?,
        map_slots::<&str, BTreeSet<String>>(rows)?,
        set_slots::<String>(rows)?,
        graph.bytes.checked_mul(2)?,
        slots::<&Edge>(rows)?,
        slots::<String>(rows)?,
        // `F-` plus 16 hex digits, and one cloned fact key at a time.
        rows.checked_mul(18)?,
        graph.bytes,
        slots::<String>(rows)?,
        graph.bytes,
    ])?;
    sum(&[index, placement, reading])
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
