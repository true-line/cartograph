//! Finite logical owners for the streaming Capability selection read.
use super::*;
use core_prov::{EvidenceRef, Provenance};
use std::mem::size_of;

/// Logical payload/slot roles, not allocator capacity, SQLite caches or RSS.
/// Base graph, host/compiler indexes, candidate borrow vector and host basis
/// copies/digests are separate; retained roles cover only the returned OverlayRead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityReadWorkspace {
    /// Selected facts' decoded properties, citation strings/set slots and DTOs.
    pub retained_selection_bytes: usize,
    /// Returned basis/identity (or stale revision) payload and result header.
    pub retained_metadata_bytes: usize,
    /// Metadata retained during scanning, including the original basis string.
    pub scan_metadata_bytes: usize,
    /// Metadata decode/canonical identity-construction peak, before selection.
    pub metadata_peak_bytes: usize,
    /// One bounded row plus decode, key reconstruction or typed provenance.
    pub row_scratch_bytes: usize,
    /// Returned Current owners; host copies/transformations are not included.
    pub retained_bytes: usize,
    /// Complete read peak, including still-retained selected facts during scan.
    pub peak_bytes: usize,
}
fn sum(parts: &[usize]) -> Option<usize> {
    parts.iter().try_fold(0usize, |a, b| a.checked_add(*b))
}
fn slots<T>(n: usize) -> Option<usize> {
    n.checked_mul(size_of::<T>())
}
fn set_slots(n: usize) -> Option<usize> {
    n.checked_mul(sum(&[size_of::<BaseRef>(), slots::<usize>(2)?])?)
}
// The same uniform Value/string/logical-map currency used by read consumers;
// these headers also charge typed decoders, not imaginary retained Value clones.
fn json_bytes(bytes: usize) -> Option<usize> {
    sum(&[
        bytes,
        bytes.checked_add(1)?.checked_mul(sum(&[
            size_of::<serde_json::Value>(),
            size_of::<String>(),
            slots::<usize>(3)?,
        ])?)?,
    ])
}
fn count(value: &impl Serialize) -> Option<usize> {
    // Only fixed, empty templates are counted here, never caller input.
    Some(serde_json::to_vec(value).ok()?.len())
}

/// Derive ownership from the exact selection caps passed to read_capabilities_bounded
/// or read_capabilities_and_actors_bounded, which share one combined charge.
/// Caps above publication maxima cannot enlarge the result. Even zero selection
/// caps require full single-row verification; neither metadata nor row scratch
/// scales down with the selected-byte limit. Checked overflow returns None.
/// Citation entries and Value/container structure are charged separately from
/// stored JSON bytes; no graph, provider, host policy or output artifact is owned.
pub fn capability_read_workspace(
    max_facts: usize,
    max_bytes: usize,
) -> Option<CapabilityReadWorkspace> {
    let facts = max_facts.min(MAX_FACTS);
    let bytes = max_bytes.min(MAX_BYTES);
    let minimum_cite = count(&BaseRef::Node(String::new()))?.min(count(&BaseRef::Edge(
        String::new(),
        String::new(),
        String::new(),
    ))?);
    let citations = |b: usize| b.checked_div(minimum_cite);
    let decoded = |b, n| {
        sum(&[
            json_bytes(b)?,
            slots::<OverlayFact>(n)?,
            set_slots(citations(b)?)?,
        ])
    };
    let retained_selection_bytes = decoded(bytes, facts)?;
    // Basis is fixed typed metadata with three possible string payloads, whose
    // aggregate decoded bytes cannot exceed its admitted JSON. In particular,
    // an arbitrary Stale.graph_instance is NOT assumed to contain only 32 bytes.
    let retained_metadata_bytes = sum(&[MAX_BASIS_BYTES, 64, size_of::<OverlayRead>()])?;
    let scan_metadata_bytes = sum(&[
        retained_metadata_bytes,
        MAX_BASIS_BYTES,
        size_of::<String>(),
        size_of::<IdentityHash>(),
    ])?;
    let template = |embedding_namespace| OverlayBasis {
        base: StoreRevision {
            graph_instance: String::new(),
            generation: u64::MAX,
        },
        recipe: OverlayRecipe {
            recipe: String::new(),
            embedding_namespace,
            provider_ran: false,
        },
    };
    let envelope = count(&template(None))?.max(count(&template(Some(String::new())))?);
    let canonical_basis = MAX_BASIS_BYTES
        .checked_mul("\\u0000".len())?
        .checked_add(envelope)?;
    let metadata_peak_bytes = sum(&[
        scan_metadata_bytes,
        MAX_BASIS_BYTES.max(canonical_basis),
        size_of::<String>(),
    ])?;
    let provenance = sum(&[
        json_bytes(MAX_FACT_BYTES)?,
        slots::<EvidenceRef>(MAX_FACT_BYTES.checked_add(1)?)?,
        size_of::<Provenance>(),
    ])?;
    // SQLite row text is borrowed and checked before Rust copies/decode. Include
    // its bounded payload, but no database page-cache or rejected oversized cell.
    let row_scratch_bytes = sum(&[
        MAX_FACT_BYTES,
        MAX_FACT_BYTES,
        decoded(MAX_FACT_BYTES, 1)?,
        provenance.max(sum(&[MAX_FACT_BYTES, size_of::<String>()])?),
    ])?;
    let retained_bytes = sum(&[retained_selection_bytes, retained_metadata_bytes])?;
    let peak_bytes = metadata_peak_bytes.max(sum(&[
        retained_selection_bytes,
        scan_metadata_bytes,
        row_scratch_bytes,
    ])?);
    Some(CapabilityReadWorkspace {
        retained_selection_bytes,
        retained_metadata_bytes,
        scan_metadata_bytes,
        metadata_peak_bytes,
        row_scratch_bytes,
        retained_bytes,
        peak_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // AC-0509 / T-0509: inspect named roles before phase maxima can hide growth.
    #[test]
    fn capability_workspace_separates_growth_fixed_scratch_and_hard_caps() {
        let tiny = capability_read_workspace(0, 0).unwrap();
        let one = capability_read_workspace(1, 1024).unwrap();
        let count = capability_read_workspace(2, 1024).unwrap();
        let bytes = capability_read_workspace(1, 2048).unwrap();
        assert!(count.retained_selection_bytes > one.retained_selection_bytes);
        assert!(bytes.retained_selection_bytes > one.retained_selection_bytes);
        for bound in [one, count, bytes] {
            assert_eq!(bound.row_scratch_bytes, tiny.row_scratch_bytes);
            assert_eq!(bound.metadata_peak_bytes, tiny.metadata_peak_bytes);
            assert!(bound.peak_bytes >= bound.retained_bytes);
        }
        assert!(tiny.row_scratch_bytes > MAX_FACT_BYTES);
        assert_eq!(
            capability_read_workspace(usize::MAX, usize::MAX),
            capability_read_workspace(MAX_FACTS, MAX_BYTES)
        );
        assert!(json_bytes(usize::MAX).is_none());
        assert!(slots::<OverlayFact>(usize::MAX).is_none());
        assert!(set_slots(usize::MAX).is_none());
    }
}
