//! Shared receipt decode/validation logical owners, not allocator or RSS.
use adapters_lang_ts::captured::{
    MAX_RECEIPT_BYTES, MAX_RECEIPT_RANGES, RangeRole, Receipt, ReceiptRange,
};
use context_hub::logical::{json_bytes, set, slots, string_wire, sum};

pub(crate) struct ReceiptWorkspace {
    pub retained: usize,
    /// Includes the borrowed raw receipt; callers retain their other SQL cells.
    pub peak: usize,
}

pub(crate) fn workspace_bytes() -> Option<ReceiptWorkspace> {
    let raw = MAX_RECEIPT_BYTES;
    // Full v1/v2 input payload plus worst-case pre-validation range headers.
    let full = sum(&[
        raw,
        slots::<Receipt>(1)?,
        slots::<ReceiptRange>(raw.checked_add(1)?)?,
    ])?;
    // V3 preflights expanded range wire <= raw before cloning shared strings.
    // Common fields still own at most raw; accepted range payload adds <= raw.
    let expanded = sum(&[
        raw,
        raw,
        slots::<Receipt>(1)?,
        slots::<ReceiptRange>(MAX_RECEIPT_RANGES)?,
    ])?;
    let retained = full.max(expanded);
    // A WireRange holds a full range or a compact range plus its enum tag.
    // Two full-range headers dominate that layout without exposing adapter
    // internals. Untagged serde also buffers the candidate value before trying
    // variants; malformed arrays are decoded before the range-count refusal.
    let wire = sum(&[
        raw,
        slots::<Receipt>(1)?,
        slots::<ReceiptRange>(raw.checked_add(1)?.checked_mul(2)?)?,
    ])?;
    let decode = sum(&[raw, wire, json_bytes(raw)?])?;
    // Expansion preflight clones one template, including a second path copy,
    // and serializes it. Conversion may retain both vector allocations.
    let template = sum(&[raw, raw, slots::<ReceiptRange>(1)?])?;
    let template_peak = sum(&[raw, wire, template, string_wire(template)?])?;
    let conversion = sum(&[raw, wire, expanded])?;
    // Validation retains the range-index set and compact serializer vector.
    // Escaping is charged before refusal. Canonicalization moves children
    // through old/new Value containers, then encodes and copies into the
    // domain-prefixed hash input. These phases conservatively share a peak.
    let typed_wire = string_wire(sum(&[raw, raw])?)?;
    let validation = sum(&[
        raw,
        retained,
        set::<(RangeRole, u32)>(MAX_RECEIPT_RANGES)?,
        slots::<ReceiptRange>(MAX_RECEIPT_RANGES)?, // dominates CompactRange
        slots::<&str>(3)?,                          // fact identity validation
        json_bytes(typed_wire)?,
        json_bytes(typed_wire)?,
        typed_wire,
        typed_wire,
        b"cartograph:ts-primary-receipt:v3\0".len(),
    ])?;
    Some(ReceiptWorkspace {
        retained,
        peak: decode.max(template_peak).max(conversion).max(validation),
    })
}
