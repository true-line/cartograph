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
use core_graph::{Edge, Node, StoreRevision};
use std::collections::BTreeSet;

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
    let OverlayRead::Current { facts, .. } = read else {
        return Err(HostError::InputChanged);
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
