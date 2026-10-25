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
    Some(
        firewall
            .prepare_peak_bytes
            .max(authorize)
            .max(invoke)
            .max(sealing),
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
        // The outer disclosure and pending-step copy survive into continuation.
        .checked_add(
            agents::investigation::InvestigationInput::sealing_workspace_bytes(policy, &limits)?
                .0
                .checked_mul(2)?,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::investigation::{INVESTIGATION_V1_POLICY, INVESTIGATION_V2_POLICY};

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
