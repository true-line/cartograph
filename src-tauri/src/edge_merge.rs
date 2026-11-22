//! Collapse repeated `(src, dst, label)` edge occurrences into one edge per
//! key (AC-0203, #436), owned, borrowed and packed. Split out of the app's
//! `main.rs` so the primary source store can use it from app-store (#1924).

use core_graph::Edge;
use core_graph::packed::PackedEdge;

/// One relation: `(src, dst, label)`.
pub type EdgeKey = (String, String, String);

pub fn edge_key(edge: &Edge) -> (String, String, String) {
    (edge.src.clone(), edge.dst.clone(), edge.label.clone())
}

/// Evidence spans one collapsed relation keeps. Truncation happens after the
/// deterministic sort and is recorded as `evidence_omitted` on the edge.
pub const MAX_MERGED_EDGE_EVIDENCE: usize = 32;

/// Collapse repeated `(src, dst, label)` occurrences to one edge per key
/// (AC-0203, #436). The last occurrence in extraction order still supplies
/// the props, tier and confidence; the evidence of every occurrence with the
/// same tier, confidence and extractor is unioned, sorted, de-duplicated and
/// bounded, so no call or import site loses its citation. When more than one
/// occurrence is unioned, the edge's hash is derived from their sorted
/// distinct hashes and the canonical span list (spans beyond the bound and
/// their count included), so adding, removing or moving any site changes it.
pub fn merge_edge_occurrences(edges: Vec<Edge>) -> std::collections::BTreeMap<EdgeKey, Edge> {
    let groups = edge_groups(&edges);
    let mut slots = edges.into_iter().map(Some).collect::<Vec<_>>();
    groups
        .into_iter()
        .map(|(key, indices)| {
            let replacement = {
                let occurrences = indices
                    .iter()
                    .map(|&index| slots[index].as_ref().expect("each index is grouped once"))
                    .collect::<Vec<_>>();
                merged_prov(&occurrences)
            };
            // The last occurrence moves; only a union rewrites its props.
            let last = *indices.last().expect("groups are non-empty");
            let mut merged = slots[last].take().expect("each index is grouped once");
            if let Some(replacement) = replacement {
                replacement.apply(&mut merged);
            }
            (key, merged)
        })
        .collect()
}

/// [`merge_edge_occurrences`] over packed edges: a key with one occurrence
/// keeps its packed edge as it is; only repeated keys are decoded, merged and
/// repacked (#1574).
pub fn merge_packed_edge_occurrences(
    edges: Vec<PackedEdge>,
) -> Result<std::collections::BTreeMap<EdgeKey, PackedEdge>, core_graph::GraphError> {
    let mut groups = std::collections::BTreeMap::<EdgeKey, Vec<usize>>::new();
    for (index, edge) in edges.iter().enumerate() {
        groups
            .entry((edge.src.clone(), edge.dst.clone(), edge.label.clone()))
            .or_default()
            .push(index);
    }
    let mut slots = edges.into_iter().map(Some).collect::<Vec<_>>();
    groups
        .into_iter()
        .map(|(key, indices)| {
            let mut take = |index: usize| slots[index].take().expect("each index is grouped once");
            if let [only] = indices[..] {
                return Ok((key, take(only)));
            }
            let occurrences = indices
                .iter()
                .map(|&index| take(index).unpack())
                .collect::<Result<Vec<_>, _>>()?;
            let merged = merge_edge_occurrences(occurrences)
                .remove(&key)
                .expect("every occurrence shares the key");
            Ok((key, PackedEdge::pack(merged)?))
        })
        .collect()
}

/// [`merge_edge_occurrences`] without taking ownership (#1235): a group that
/// merges to its last occurrence unchanged stays borrowed, so only a
/// synthesized union is copied.
pub fn merged_edge_view(
    edges: &[Edge],
) -> std::collections::BTreeMap<EdgeKey, std::borrow::Cow<'_, Edge>> {
    edge_groups(edges)
        .into_iter()
        .map(|(key, indices)| {
            let occurrences = indices
                .iter()
                .map(|&index| &edges[index])
                .collect::<Vec<_>>();
            let last = *occurrences.last().expect("groups are non-empty");
            let merged = match merged_prov(&occurrences) {
                None => std::borrow::Cow::Borrowed(last),
                Some(replacement) => {
                    let mut merged = last.clone();
                    replacement.apply(&mut merged);
                    std::borrow::Cow::Owned(merged)
                }
            };
            (key, merged)
        })
        .collect()
}

/// Occurrence indices per key, in extraction order.
fn edge_groups(edges: &[Edge]) -> std::collections::BTreeMap<EdgeKey, Vec<usize>> {
    let mut groups = std::collections::BTreeMap::<EdgeKey, Vec<usize>>::new();
    for (index, edge) in edges.iter().enumerate() {
        groups.entry(edge_key(edge)).or_default().push(index);
    }
    groups
}

/// The props a collapsed relation replaces on its last occurrence.
struct MergedProv {
    prov: serde_json::Value,
    omitted: usize,
}

impl MergedProv {
    fn apply(self, edge: &mut Edge) {
        edge.props["prov"] = self.prov;
        if self.omitted > 0 {
            edge.props["evidence_omitted"] = serde_json::json!(self.omitted);
        }
    }
}

/// `None` when the group merges to its last occurrence unchanged: a single
/// occurrence, or a last occurrence without readable provenance.
fn merged_prov(occurrences: &[&Edge]) -> Option<MergedProv> {
    let last = *occurrences.last().expect("groups are non-empty");
    if occurrences.len() == 1 {
        return None;
    }
    let parse = |edge: &Edge| {
        serde_json::from_value::<core_prov::Provenance>(edge.props.get("prov")?.clone()).ok()
    };
    let mut prov = parse(last)?;
    let mut evidence = Vec::new();
    let mut hashes = std::collections::BTreeSet::new();
    let mut unioned = 0usize;
    for occurrence in occurrences
        .iter()
        .filter_map(|edge| parse(edge))
        .filter(|other| {
            other.tier == prov.tier
                && other.confidence_tier == prov.confidence_tier
                && other.extractor_id == prov.extractor_id
        })
    {
        evidence.extend(occurrence.evidence);
        hashes.insert(occurrence.content_hash);
        unioned += 1;
    }
    evidence.sort_by(|a, b| {
        (&a.repo, &a.path, a.byte_start, a.byte_end, &a.commit_sha).cmp(&(
            &b.repo,
            &b.path,
            b.byte_start,
            b.byte_end,
            &b.commit_sha,
        ))
    });
    evidence.dedup();
    let omitted = evidence.len().saturating_sub(MAX_MERGED_EDGE_EVIDENCE);
    if unioned > 1 {
        // Occurrences can share one fact hash while citing different sites
        // (duplicate imports), so the canonical span list — omitted spans
        // included — is part of the merged hash alongside the source hashes.
        let canonical = serde_json::to_vec(&("merged-edge-v1", &hashes, &evidence, omitted))
            .expect("merged evidence serializes");
        prov.content_hash = core_prov::content_hash(&canonical);
    }
    evidence.truncate(MAX_MERGED_EDGE_EVIDENCE);
    prov.evidence = evidence;
    Some(MergedProv {
        prov: serde_json::to_value(prov).expect("provenance serializes"),
        omitted,
    })
}
