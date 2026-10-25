//! Additional evidence-read payload roles, excluding the retained session and
//! transport DOMs. These are logical owner/header units, not allocator or RSS
//! bounds.
use crate::mcp_reads::dispatch::EvidenceWorkLimits;
use adapters_lang_ts::captured::MAX_RECEIPT_BYTES;
use agents::investigation::InvestigationInspectionTarget;
use context_hub::logical::{slots, string_wire, sum};
use std::mem::size_of;

/// Maximum additional payload during either evidence description or text read.
/// `freshness_bytes` is one graph freshness snapshot workspace: the two checks
/// are sequential. The returned receipt is reused by the subsequent text read.
pub(crate) fn evidence_peak_bytes(
    freshness_bytes: usize,
    limits: EvidenceWorkLimits,
) -> Option<usize> {
    let receipt = MAX_RECEIPT_BYTES;
    let receipt_workspace = super::receipt::workspace_bytes()?;
    let receipt_typed = receipt_workspace.retained;
    let receipt_parse = receipt_workspace.peak;
    let receipt_wire = string_wire(receipt)?;
    let hash = core_prov::content_hash(&[]);
    let snapshot_id = format!("context-v1:{hash}").len();
    // A target selects one receipt range plus its receipt/fact identities. Its
    // added graph snapshot and target hashes are not part of the receipt wire.
    let target_payload = sum(&[receipt, snapshot_id, hash.len()])?;
    let target = sum(&[target_payload, size_of::<InvestigationInspectionTarget>()])?;
    let target_wire = string_wire(target_payload)?;
    // Response target clone, resolver target, and content_id's cleared-ID clone
    // can coexist; each role follows an actual clone in the selected path.
    let targets = sum(&[target, target, target, target_wire])?;
    let bindings = sum(&[
        receipt,
        receipt,
        slots::<core_graph::source::SourceBinding>(2)?,
        snapshot_id,
        snapshot_id,
    ])?;
    let source = crate::sources::selected_source_peak_bytes()?;
    // Exact forgotten-status lookup can retain this bounded summary while
    // validating a pinned receipt; source36 + RFC3339 timestamp20 bytes.
    let forgotten = sum(&[36, 20, size_of::<crate::primary_source::TombstoneSummary>()])?;
    // Two same-revision selections can overlap on the positive omission path.
    // Each owns its typed key, digest and bounded repository key; graph copies
    // are released between observations. The enum kind adds no dynamic owner.
    let selection = sum(&[
        size_of::<core_graph::source::FactSourceSelection>(),
        // Maximum typed edge key: source8192 + label256 + destination8192.
        sum(&[8192, 256, 8192])?,
        72,
        256,
    ])?;
    let omission = sum(&[
        selection,
        selection,
        slots::<core_graph::StoreRevision>(2)?,
        32,
        32,
        // Exact comparison temporarily constructs one digest and omission DTO.
        72,
        256,
        size_of::<core_graph::source::OmissionLookup>(),
        // SQL CASE admits these three BLOB cells before Vec/UTF-8 conversion.
        slots::<Option<Vec<u8>>>(3)?,
        slots::<String>(3)?,
        256,
        16,
        72,
    ])?;
    let common = sum(&[
        targets,
        bindings,
        source,
        source,
        forgotten,
        omission,
        super::schema::workspace_bytes()?,
    ])?;
    let capture = super::capture::workspace(limits, target_payload)?;
    let span = usize::try_from(
        limits
            .max_span_bytes
            .get()
            .min(source_capture::MAX_SPAN_BYTES),
    )
    .ok()?;
    let response_wire = sum(&[target_wire, string_wire(span)?, receipt_wire])?;
    // On capture absence the original validated receipt remains borrowed while
    // receipt_status performs its own mandatory stored-receipt validation.
    // The latter decode is sequential with descriptor/file work, not free.
    let receipt_status_peak = sum(&[receipt_typed, receipt_parse])?;
    let peak = [
        receipt_status_peak,
        freshness_bytes,
        receipt_parse,
        sum(&[receipt_typed, capture.peak()])?,
        sum(&[receipt_typed, span, response_wire])?,
    ]
    .into_iter()
    .max()?;
    sum(&[common, peak])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;
    fn limits(file: u64, span: u64) -> EvidenceWorkLimits {
        EvidenceWorkLimits {
            max_file_validation_bytes: NonZeroU64::new(file).unwrap(),
            max_span_bytes: NonZeroU64::new(span).unwrap(),
        }
    }
    #[test]
    fn evidence_roles_are_checked_and_freshness_is_sequential() {
        let base = evidence_peak_bytes(1, limits(1, 1)).unwrap();
        assert!(base > source_capture::MAX_MANIFEST_BYTES as usize);
        let freshness = base.checked_mul(2).unwrap();
        let with_snapshot = evidence_peak_bytes(freshness, limits(1, 1)).unwrap();
        assert!(with_snapshot >= freshness);
        assert!(with_snapshot < freshness * 2);
        assert!(evidence_peak_bytes(usize::MAX, limits(1, 1)).is_none());
    }
    #[test]
    fn selected_object_and_span_respect_authoritative_hard_caps() {
        let caps = limits(
            source_capture::MAX_FILE_BYTES,
            source_capture::MAX_SPAN_BYTES,
        );
        assert_eq!(
            evidence_peak_bytes(1, caps),
            evidence_peak_bytes(1, limits(u64::MAX, u64::MAX))
        );
        assert!(
            evidence_peak_bytes(1, limits(1, 1)).unwrap() <= evidence_peak_bytes(1, caps).unwrap()
        );
    }
}
