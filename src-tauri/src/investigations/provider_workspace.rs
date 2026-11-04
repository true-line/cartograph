//! Provider payload phase only; caller input/context, stores, provider config,
//! allocator/native memory and RSS are outside this logical component.
use super::HostError;
use agents::investigation::InvestigationPolicy;
use llm::bounded::{CompletionLimitValues, CompletionLimits};

/// Shared by both live preparation paths and the admission calculation.
pub(crate) fn completion_limit_values(
    policy: &InvestigationPolicy,
) -> Result<CompletionLimitValues, HostError> {
    let mut values = CompletionLimits::default().values();
    values.max_output_tokens = policy.limits().generated_tokens_per_invocation;
    values.max_spans =
        u32::try_from(policy.limits().evidence_items).map_err(|_| HostError::InvalidInput)?;
    Ok(values)
}

/// Includes preparation during authorization, both DTOs during transport, and
/// candidate preparation during sealing (including caller/sealer action clones).
#[cfg(unix)]
pub(crate) fn payload_peak(policy: &InvestigationPolicy) -> Option<usize> {
    let limits = CompletionLimits::new(completion_limit_values(policy).ok()?).ok()?;
    let firewall = limits.firewall_workspace_bytes()?;
    let transport = limits.transport_workspace_bytes()?;
    let authorize = firewall
        .prepared_bytes
        .checked_add(firewall.authorize_peak_bytes)?;
    let invoke = firewall
        .prepared_bytes
        .checked_add(firewall.authorized_bytes)?
        .checked_add(
            transport
                .request_peak_bytes
                .max(transport.response_peak_bytes),
        )?;
    let (_, sealing) =
        agents::investigation::InvestigationInput::sealing_workspace_bytes(policy, &limits)?;
    let sealing = firewall.prepared_bytes.checked_add(sealing)?;
    let preview = agents::investigation::InvestigationInput::context_preview_workspace_bytes(
        policy, &limits,
    )?;
    let source_preview =
        agents::investigation::InvestigationInput::source_preview_workspace_bytes(policy, &limits)?;
    Some(
        firewall
            .prepare_peak_bytes
            .max(authorize)
            .max(invoke)
            .max(sealing)
            .max(preview)
            .max(source_preview),
    )
}

/// Original invocation owners remain live while next_metadata prepares and
/// seals a candidate. Charge these outside the phase maximum: payload_peak
/// then covers candidate firewall/sealing independently. Also retain the outer
/// disclosure and pending-step copy; candidate disclosure is in sealing's peak.
#[cfg(unix)]
pub(crate) fn continuation_retained(policy: &InvestigationPolicy) -> Option<usize> {
    let limits = CompletionLimits::new(completion_limit_values(policy).ok()?).ok()?;
    let firewall = limits.firewall_workspace_bytes()?;
    let transport = limits.transport_workspace_bytes()?;
    firewall
        .prepared_bytes
        .checked_add(firewall.authorized_bytes)?
        .checked_add(transport.completion_bytes)?
        // next_metadata/next_source keep the separately decoded original_action
        // through candidate preparation, sealing and durable publication.
        .checked_add(context_hub::logical::json_bytes(
            usize::try_from(limits.values().output_text_bytes).ok()?,
        )?)?
        // The outer disclosure and pending-step copy survive into continuation.
        .checked_add(
            agents::investigation::InvestigationInput::sealing_workspace_bytes(policy, &limits)?
                .0
                .checked_mul(2)?,
        )
}

/// Candidate preparation and its sealed disclosure coexist with durable source
/// projection/validation. Current invocation owners are charged separately.
#[cfg(unix)]
pub(crate) fn source_settlement_retained(policy: &InvestigationPolicy) -> Option<usize> {
    let limits = CompletionLimits::new(completion_limit_values(policy).ok()?).ok()?;
    limits
        .firewall_workspace_bytes()?
        .prepared_bytes
        .checked_add(
            agents::investigation::InvestigationInput::sealing_workspace_bytes(policy, &limits)?.0,
        )
}

/// Audit projection/firewall/transport owners in addition to the original
/// invocation retained by continuation_retained. This does not include the
/// durable start/settlement transactions or their saved-task reconstruction.
#[cfg(unix)]
pub(crate) fn audit_payload_peak(policy: &InvestigationPolicy) -> Option<usize> {
    use agents::investigation::{MAX_RECORD_BYTES, semantic_audit};
    use context_hub::logical::{set, slots, sum};
    let limits = CompletionLimits::new(completion_limit_values(policy).ok()?).ok()?;
    let firewall = limits.firewall_workspace_bytes()?;
    let transport = limits.transport_workspace_bytes()?;
    let (projection, construction) = semantic_audit::projection_workspace_bytes()?;
    // semantic_audit_payload fingerprints the borrowed ledger, then keeps a
    // separately cloned acquired-ID set during projection construction.
    let ids = sum(&[
        MAX_RECORD_BYTES,
        set::<String>(policy.limits().evidence_items)?,
        slots::<std::collections::BTreeSet<String>>(1)?,
        64, // returned ledger fingerprint
    ])?;
    let construct =
        construction
            .checked_add(ids)?
            .max(agents::investigation::ledger_validation_workspace(
                policy,
                MAX_RECORD_BYTES,
            )?);
    // action() clones the projection payload. Keep both through preparation,
    // authorization and transport conservatively; action IDs use opaque_id's
    // existing 128-byte ceiling. No original invocation owner is absorbed here.
    let action = sum(&[
        projection,
        std::mem::size_of::<llm::CompletionAction>(),
        128,
    ])?;
    let caller = projection.checked_add(action)?;
    let authorize = firewall
        .prepared_bytes
        .checked_add(firewall.authorize_peak_bytes)?;
    let invoke = sum(&[
        firewall.prepared_bytes,
        firewall.authorized_bytes,
        transport
            .request_peak_bytes
            .max(transport.response_peak_bytes),
    ])?;
    Some(construct.max(caller.checked_add(firewall.prepare_peak_bytes.max(authorize).max(invoke))?))
}

/// Live audit objects retained across durable begin/settlement. These are in
/// addition to continuation_retained's original analyst invocation. Keep the
/// projection/action conservatively even if a future worker drops them earlier.
#[cfg(unix)]
pub(crate) fn audit_retained(policy: &InvestigationPolicy) -> Option<usize> {
    let limits = CompletionLimits::new(completion_limit_values(policy).ok()?).ok()?;
    let firewall = limits.firewall_workspace_bytes()?;
    context_hub::logical::sum(&[
        agents::investigation::semantic_audit::projection_workspace_bytes()?
            .0
            .checked_mul(2)?,
        std::mem::size_of::<llm::CompletionAction>(),
        128,
        firewall.prepared_bytes,
        firewall.authorized_bytes,
        limits.transport_workspace_bytes()?.completion_bytes,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::investigation::{INVESTIGATION_V1_POLICY, INVESTIGATION_V2_POLICY};

    #[cfg(unix)]
    #[test]
    fn provider_continuation_retains_decoded_action_with_original_completion() {
        // AC-0327: candidate phase maxima cannot absorb the original action.
        let policy = &INVESTIGATION_V2_POLICY;
        let limits = CompletionLimits::new(completion_limit_values(policy).unwrap()).unwrap();
        let cap = usize::try_from(limits.values().output_text_bytes).unwrap();
        let raw = format!("[{}]", vec!["[]"; (cap - 2) / 3].join(","));
        let action: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let entries = action.as_array().unwrap();
        assert!(raw.len() <= cap);
        // A container-dense parsed response owns Value slots independently of
        // completion.text. Use actual decoded entries, not the bound formula.
        let decoded_slots =
            std::mem::size_of_val(&action) + std::mem::size_of_val(entries.as_slice());
        assert!(decoded_slots > raw.len());
        let firewall = limits.firewall_workspace_bytes().unwrap();
        let transport = limits.transport_workspace_bytes().unwrap();
        let original = firewall.prepared_bytes
            + firewall.authorized_bytes
            + transport.completion_bytes
            + 2 * agents::investigation::InvestigationInput::sealing_workspace_bytes(
                policy, &limits,
            )
            .unwrap()
            .0;
        assert!(continuation_retained(policy).unwrap() >= original + decoded_slots);
    }

    // AC-0327: the admission component and live paths use identical policy caps.
    #[test]
    fn provider_limits_preserve_worker_policy_and_transport_caps() {
        for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
            let actual = CompletionLimits::new(completion_limit_values(policy).unwrap())
                .unwrap()
                .values();
            let defaults = CompletionLimits::default().values();
            assert_eq!(
                actual.max_output_tokens,
                policy.limits().generated_tokens_per_invocation
            );
            assert_eq!(actual.max_spans as usize, policy.limits().evidence_items);
            assert_eq!(actual.input_bytes, defaults.input_bytes);
            assert_eq!(actual.request_bytes, defaults.request_bytes);
            assert_eq!(actual.response_bytes, defaults.response_bytes);
            assert_eq!(actual.output_text_bytes, defaults.output_text_bytes);
        }
    }
}
