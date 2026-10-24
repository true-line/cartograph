//! Closed StoredTask decoding currency, not an arbitrary Deserialize<T> bound.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

/// Retained task and acquisition/decode peak after SQL admits the row byte cap.
/// Covers derived serde's logical content trees, not allocator/native layout or
/// RSS. SQL/cache internals, scalar mirrors, pure validation and nested durable
/// replay are separate. Malformed records are included before family validation.
pub(crate) fn next_prompt_record_workspace(bytes: usize) -> Option<(usize, usize)> {
    if bytes > MAX_RECORD_BYTES {
        return None;
    }
    // Exhaustive dynamic element/box types reachable from StoredTask. Inline
    // nested fields are already in the root or element size. Each element or
    // box requires a distinct admitted JSON value; decoded string payload is
    // no larger than the input bytes. No nonempty defaults synthesize payload:
    // optional fields default None, coverage private maps/vectors default empty.
    let element = [
        size_of::<String>(),
        size_of::<InvestigationCitation>(),
        size_of::<context_hub::coverage::RepoQualification>(),
        size_of::<InvestigationInspectionTarget>(),
        size_of::<InvestigationInspectionObservation>(),
        size_of::<Obligation>(),
        size_of::<ObligationObservation>(),
        size_of::<DisclosedInvestigationCoverageDetail>(),
        size_of::<DisclosedInvestigationFact>(),
        size_of::<DisclosedInvestigationQuery>(),
        size_of::<DisclosedInvestigationSource>(),
    ]
    .into_iter()
    .max()?;
    let retained = sum(&[
        size_of::<StoredTask>(),
        bytes,
        element.checked_mul(RECORD_MAX_VALUES)?,
    ])?;
    // Flattened InvestigationDetail and internally tagged DTOs can retain
    // content buffers while decoding children. Reserve a complete admitted
    // logical JSON tree at each possible nesting level, including the root;
    // this deliberately does not depend on serde's private Content layout.
    // McpClientId's custom visitor validates then copies its 32 input bytes;
    // required_feedback delegates to ordinary Option decoding without expansion.
    let content = json_bytes(bytes)?.checked_mul(RECORD_MAX_DEPTH.checked_add(1)?)?;
    let decoding = record_decode_scratch_bytes()?.max(content);
    let acquisition = sum(&[bytes, slots::<Vec<u8>>(1)?, 64, slots::<String>(1)?])?;
    Some((retained, sum(&[acquisition, retained, decoding])?))
}
