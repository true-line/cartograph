//! Logical temporary owners of [`Receipt::matches_node`] and
//! [`Receipt::matches_edge`], not allocator layouts or an RSS bound.
#[cfg(doc)]
use super::Receipt;
use super::{MAX_RECEIPT_RANGES, RangeRole};
use core_graph::source::FactKey;
use core_prov::{EvidenceRef, Provenance};
use serde_json::Value;
use std::mem::size_of;

fn sum(roles: &[usize]) -> Option<usize> {
    roles.iter().try_fold(0usize, |a, b| a.checked_add(*b))
}
fn slots<T>(count: usize) -> Option<usize> {
    count.checked_mul(size_of::<T>())
}
fn map<K, V>(count: usize) -> Option<usize> {
    count.checked_mul(sum(&[size_of::<K>(), size_of::<V>(), slots::<usize>(3)?])?)
}
/// One value, string and map-entry role per JSON value, without payload.
fn containers(values: usize) -> Option<usize> {
    sum(&[
        slots::<Value>(values)?,
        slots::<String>(values)?,
        map::<(), ()>(values)?,
    ])
}

/// Peak temporary owners of one fact match, for a node or edge whose compact
/// `serde_json` encoding is at most `fact_wire` bytes. The receipt and the
/// fact are the caller's. Rejected facts are covered up to their refusal.
/// Overflow returns `None`.
pub fn fact_match_workspace_bytes(fact_wire: usize) -> Option<usize> {
    // Every JSON value spends at least one wire byte.
    let values = fact_wire.checked_add(1)?;
    let value = sum(&[fact_wire, containers(values)?])?;
    // The compared key is a temporary of the whole match expression.
    let key = sum(&[slots::<FactKey>(1)?, fact_wire])?;
    // Digest: the fact as a Value, canonicalized through an ordered map into
    // rebuilt containers, encoded behind its domain tuple, then hashed.
    let encoded = sum(&[fact_wire, "[\"cartograph.source.node-v1\",]".len()])?;
    let digest = sum(&[
        value,
        map::<String, Value>(values)?,
        containers(values)?,
        encoded,
        "node-v1:".len().checked_add(64)?.checked_add(64)?,
    ])?;
    // Inventory: the provenance clone decodes into a typed owner whose
    // tagged `method` buffers once. Its evidence is sized by the wire.
    let provenance = sum(&[
        slots::<Provenance>(1)?,
        fact_wire,
        slots::<EvidenceRef>(values)?,
        containers(values)?,
    ])?;
    let provenance_decode = sum(&[value, provenance])?;
    // Ranges are charged in full, with their string clones, while the rule
    // clone decodes and validates beside the retained provenance.
    let ranges = sum(&[
        slots::<(RangeRole, u32, EvidenceRef)>(MAX_RECEIPT_RANGES)?,
        fact_wire,
    ])?;
    let rule = sum(&[
        provenance,
        ranges,
        value,
        core_graph::rules::validation_workspace_bytes(fact_wire)?,
    ])?;
    sum(&[key, digest.max(provenance_decode).max(rule)])
}
