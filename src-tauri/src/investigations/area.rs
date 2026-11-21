//! Area scope (#2044): an investigation task scoped to one overlay
//! `Subsystem`. The task freezes the base graph only, so the host resolves
//! the area's member files from the knowledge overlay at the revision of the
//! graph it just read, and keeps those files, what they define and the edges
//! between them. An area that is absent or was derived from another revision
//! refuses the task; it never widens to the whole graph.
use super::HostError;
use crate::AppState;
use agents::investigation::InvestigationScope;
use core_graph::overlay::{BaseRef, MAX_BYTES, MAX_FACTS, OverlayItem, OverlayRead};
use core_graph::scoped::{Remaining, ScopedGraphReader};
use core_graph::{Edge, GraphError, Node, StoreRevision};
use std::collections::{BTreeMap, BTreeSet};

/// The member file ids of the area a scope names, or `None` for any other
/// scope. The caller holds no graph lock.
pub(super) fn files(
    state: &AppState,
    revision: &StoreRevision,
    scope: &InvestigationScope,
) -> Result<Option<BTreeSet<String>>, HostError> {
    let InvestigationScope::Area { id } = scope else {
        return Ok(None);
    };
    let overlay = state
        .knowledge_overlay
        .as_deref()
        .ok_or(HostError::InvalidInput)?;
    let read = overlay
        .lock()
        .map_err(|_| HostError::Operational)?
        .read_story_nodes_bounded(revision, MAX_FACTS, MAX_BYTES)
        .map_err(|_| HostError::Operational)?;
    let facts = match read {
        OverlayRead::Current { facts, .. } => facts,
        // Derived from another graph revision.
        OverlayRead::Stale { .. } => return Err(HostError::InputChanged),
        // Nothing published: there is no such area.
        OverlayRead::Absent => return Err(HostError::InvalidInput),
    };
    let files = facts
        .into_iter()
        .find(|fact| {
            matches!(&fact.item, OverlayItem::Node(node) if node.label == "Subsystem" && &node.id == id)
        })
        .ok_or(HostError::InvalidInput)?
        .cites
        .into_iter()
        .filter_map(|cite| match cite {
            BaseRef::Node(id) => Some(id),
            BaseRef::Edge(..) => None,
        })
        .collect();
    Ok(Some(files))
}

/// The area's part of a graph: its files, the facts defined in them and the
/// edges whose two ends are both among those.
pub(super) fn members(
    graph: &(Vec<Node>, Vec<Edge>),
    files: &BTreeSet<String>,
) -> (Vec<Node>, Vec<Edge>) {
    let mut ids: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    for edge in &graph.1 {
        if edge.label == "DEFINED_IN" && files.contains(&edge.dst) {
            ids.insert(&edge.src);
        }
    }
    let nodes = graph
        .0
        .iter()
        .filter(|node| ids.contains(node.id.as_str()))
        .cloned()
        .collect();
    let edges = graph
        .1
        .iter()
        .filter(|edge| ids.contains(edge.src.as_str()) && ids.contains(edge.dst.as_str()))
        .cloned()
        .collect();
    (nodes, edges)
}

/// [`members`] read through one pinned scoped reader (#1602, AC-1301): the
/// same files, facts and edges, found from the files' incoming `DEFINED_IN`
/// edges and each member's outgoing edges, so the rest of the graph is never
/// read. Every page charges the reader's cumulative limits; running out is an
/// error, never a smaller area.
pub(super) fn scoped_members(
    reader: &mut ScopedGraphReader<'_>,
    files: &BTreeSet<String>,
) -> Result<(Vec<Node>, Vec<Edge>), GraphError> {
    let page_rows = reader.limits().max_rows;
    let mut ids: BTreeSet<String> = files.clone();
    for file in files {
        let mut after = None;
        loop {
            let page = reader.incoming(file, after, page_rows)?;
            for edge in page.edges {
                if edge.label == "DEFINED_IN" {
                    ids.insert(edge.src);
                }
            }
            match page.remaining {
                Remaining::Exhausted => break,
                Remaining::More => after = page.next_incoming,
            }
        }
    }
    let mut nodes = Vec::new();
    for id in &ids {
        if let Some(node) = reader.node(id)? {
            nodes.push(node);
        }
    }
    let mut edges = BTreeMap::new();
    for id in &ids {
        let mut after = None;
        loop {
            let page = reader.outgoing(id, after.as_ref(), page_rows)?;
            for edge in page.edges {
                if ids.contains(&edge.dst) {
                    edges.insert(
                        (edge.src.clone(), edge.label.clone(), edge.dst.clone()),
                        edge,
                    );
                }
            }
            match page.remaining {
                Remaining::Exhausted => break,
                Remaining::More => after = page.next_outgoing,
            }
        }
    }
    Ok((nodes, edges.into_values().collect()))
}
