//! Firewall DTO and string ownership only. Transport request/response bodies,
//! caller actions, allocator slack and regex/native internals are separate.
use super::*;
use std::mem::size_of;

#[derive(Debug, Clone, Copy)]
pub struct FirewallWorkspace {
    /// Returned prepared DTO and its owned string/span payloads.
    pub prepared_bytes: usize,
    /// Returned authorized DTO; the caller may still own the preparation.
    pub authorized_bytes: usize,
    /// Includes the returned preparation, excluding the borrowed action.
    pub prepare_peak_bytes: usize,
    /// Excludes the borrowed preparation, includes the authorized copy.
    pub authorize_peak_bytes: usize,
}

fn sum(values: &[usize]) -> Option<usize> {
    values.iter().try_fold(0usize, |a, b| a.checked_add(*b))
}

impl CompletionLimits {
    /// Checked logical payload bounds for prepare_bounded/authorize_bounded.
    /// A provider's configured profile is validated before these capped owners;
    /// provider implementation/configuration memory is not claimed here.
    /// Compose `prepared_bytes + authorize_peak_bytes` while authorizing a
    /// retained preparation. This does not include HTTP transport or admit work.
    /// Reference/tree slots use a conservative logical header currency; this
    /// is not allocator capacity, regex workspace, native memory or RSS.
    pub fn firewall_workspace_bytes(&self) -> Option<FirewallWorkspace> {
        let input = usize::try_from(self.0.input_bytes).ok()?;
        let spans = usize::try_from(self.0.max_spans).ok()?;
        let profile = sum(&[
            size_of::<ProviderProfile>(),
            PROTOCOL_VERSION.len(),
            MAX_PROVIDER_ID_BYTES,
            MAX_ENDPOINT_BYTES,
            MAX_MODEL_BYTES,
        ])?;
        // Optional schema is owned text within the same serialized-input cap;
        // it introduces no Value tree. Its String header is in CompletionPayload.
        let payload = sum(&[
            size_of::<CompletionPayload>(),
            input,
            spans.checked_mul(size_of::<crate::PayloadSpan>())?,
        ])?;
        let preview = sum(&[
            size_of::<BoundedEgressPreview>(),
            profile,
            payload,
            MAX_PROVIDER_ID_BYTES,
            MAX_ACTION_ID_BYTES,
            blake3::OUT_LEN * 2,
        ])?;
        let prepared_bytes = sum(&[
            preview,
            blake3::OUT_LEN * 2,
            size_of::<PreparedBoundedCompletion>(),
        ])?;
        let authorized_bytes = sum(&[preview, size_of::<AuthorizedBoundedCompletion>()])?;
        // Redaction may expand before the redacted encoding is refused. Both
        // original and redacted ID sets remain borrowed throughout prepare_wire.
        let expanded = core_redact::direct_source_json_content_bound(input)?;
        let ids = sum(&[
            size_of::<std::collections::BTreeSet<&String>>(),
            spans.checked_mul(sum(&[size_of::<&String>(), 3 * size_of::<usize>()])?)?,
        ])?;
        let redaction = sum(&[expanded, expanded, input, payload, 2 * size_of::<String>()])?;
        // The egress fingerprint buffer remains live while original_action_wire
        // constructs its independently bounded envelope. Returned hashes coexist.
        let hashes = sum(&[
            usize::try_from(EGRESS_FINGERPRINT_BYTES).ok()?,
            input,
            input.checked_add(usize::try_from(ORIGINAL_ACTION_OVERHEAD_BYTES).ok()?)?,
            2 * blake3::OUT_LEN * 2,
            3 * size_of::<Vec<u8>>(),
        ])?;
        // URL parsing can percent-encode each endpoint byte. This charges its
        // owned text and public DTO, not the URL implementation's internal cache.
        let profile_check = sum(&[profile, 3 * MAX_ENDPOINT_BYTES, size_of::<reqwest::Url>()])?;
        let prepare_peak_bytes = sum(&[
            prepared_bytes,
            profile,
            ids,
            ids,
            redaction.max(hashes).max(profile_check),
        ])?;
        let authorize_peak_bytes = sum(&[authorized_bytes, profile_check])?;
        Some(FirewallWorkspace {
            prepared_bytes,
            authorized_bytes,
            prepare_peak_bytes,
            authorize_peak_bytes,
        })
    }
}

#[cfg(test)]
#[test]
fn firewall_workspace_rejects_unrepresentable_components() {
    // AC-0327: production callers only obtain validated CompletionLimits.
    // Construct an internal invalid value to exercise checked composition itself.
    let limits = CompletionLimits(CompletionLimitValues {
        input_bytes: u64::MAX,
        ..CompletionLimitValues::default()
    });
    assert!(limits.firewall_workspace_bytes().is_none());
}
