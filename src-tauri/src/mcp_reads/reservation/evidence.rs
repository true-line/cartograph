//! Additional evidence-read payload roles, excluding the retained session and
//! transport DOMs. These are logical owner/header units, not allocator or RSS
//! bounds.
use crate::mcp_reads::dispatch::EvidenceWorkLimits;
use adapters_lang_ts::captured::{MAX_RECEIPT_BYTES, MAX_RECEIPT_RANGES, Receipt, ReceiptRange};
use agents::investigation::InvestigationInspectionTarget;
use context_hub::logical::{json_bytes, set, slots, string_wire, sum};
use std::mem::size_of;

/// Maximum additional payload during either evidence description or text read.
/// `freshness_bytes` is one graph freshness snapshot workspace: the two checks
/// are sequential. The returned receipt is reused by the subsequent text read.
pub(crate) fn evidence_peak_bytes(
    freshness_bytes: usize,
    limits: EvidenceWorkLimits,
) -> Option<usize> {
    let manifest = usize::try_from(source_capture::MAX_MANIFEST_BYTES).ok()?;
    let receipt = MAX_RECEIPT_BYTES;
    // Decoding precedes manifest/range validation. A malformed input can contain
    // more entries than MAX_FILES/MAX_RECEIPT_RANGES, but each entry consumes at
    // least one raw byte. Strings after JSON unescaping consume <= input bytes.
    let manifest_typed = sum(&[
        manifest,
        size_of::<source_capture::CaptureManifest>(),
        slots::<source_capture::FileEntry>(manifest.checked_add(1)?)?,
    ])?;
    let manifest_parse = sum(&[manifest, manifest_typed, manifest])?; // raw/DTO/canonical
    let receipt_typed = sum(&[
        receipt,
        size_of::<Receipt>(),
        slots::<ReceiptRange>(receipt.checked_add(1)?)?,
    ])?;
    // Receipt.validate first serializes even invalid input; escaping may expand
    // every string byte sixfold. Accepted content_id canonicalizes a Value by
    // moving children through an intermediate map/array, then serializes and
    // copies the bytes into its domain-prefixed hash input. Two Value role sets
    // cover the old/new container headers during that move (not two receipts).
    let receipt_wire = string_wire(receipt)?;
    let receipt_identity = sum(&[
        json_bytes(receipt_wire)?,
        json_bytes(receipt_wire)?,
        receipt_wire,
        receipt_wire,
        b"cartograph:ts-primary-receipt:v2\0".len(),
        set::<(adapters_lang_ts::captured::RangeRole, u32)>(MAX_RECEIPT_RANGES)?,
    ])?;
    let receipt_parse = sum(&[receipt, receipt_typed, receipt_identity])?;
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
    let common = sum(&[targets, bindings, source, source, forgotten])?;
    let object = usize::try_from(
        limits
            .max_file_validation_bytes
            .get()
            .min(source_capture::MAX_FILE_BYTES),
    )
    .ok()?;
    let span = usize::try_from(
        limits
            .max_span_bytes
            .get()
            .min(source_capture::MAX_SPAN_BYTES),
    )
    .ok()?;
    // read_span owns the typed manifest and selected object when it creates the
    // span Vec. UTF-8 conversion moves that Vec into String. Final response
    // serialization holds the String and its escaped wire at the same time.
    let captured_read = sum(&[
        manifest_typed,
        object,
        span,
        size_of::<source_capture::CaptureFileRef>(),
        target_payload,
    ])?;
    let response_wire = sum(&[target_wire, string_wire(span)?, receipt_wire])?;
    // On capture absence the original validated receipt remains borrowed while
    // receipt_status performs its own mandatory stored-receipt validation.
    // The latter decode is sequential with descriptor/file work, not free.
    let receipt_status_peak = sum(&[receipt_typed, receipt_parse])?;
    let peak = [
        receipt_status_peak,
        freshness_bytes,
        receipt_parse,
        sum(&[receipt_typed, manifest_parse])?,
        sum(&[receipt_typed, captured_read])?,
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
