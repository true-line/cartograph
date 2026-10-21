//! Closed immutable policy registry with an explicit opt-in expanded profile.
//! Admission bounds are distinct from durable runtime counters; callers cannot
//! deserialize or construct a policy from their own limit values.
use super::InvestigationLimits;

/// Host completion-attempt schema ceiling. Raising it requires an explicit
/// migration; a registry entry may never promise more durable attempt slots.
pub const MAX_STORED_COMPLETION_ATTEMPTS: usize = 112;

#[derive(Debug)]
pub struct InvestigationPolicy {
    profile: &'static str,
    request_bytes: usize,
    journal_events: u64,
    max_completion_obligations: usize,
    model_invocations: u32,
    tool_actions: u32,
    selected_facts: usize,
    evidence_requests: u32,
    evidence_items: usize,
    evidence_bytes: usize,
    captured_validation_bytes: u64,
    generated_tokens_per_invocation: u32,
    generated_token_reservations: u32,
    active_seconds: u64,
    consent_wait_seconds: u64,
    wall_seconds: u64,
}

pub const INVESTIGATION_V1_POLICY: InvestigationPolicy = InvestigationPolicy {
    profile: "investigation-v1",
    request_bytes: 8 * 1024,
    journal_events: 96,
    max_completion_obligations: 12,
    model_invocations: 8,
    tool_actions: 8,
    selected_facts: 64,
    evidence_requests: 64,
    evidence_items: 12,
    evidence_bytes: 48 * 1024,
    captured_validation_bytes: 128 * 1024 * 1024,
    generated_tokens_per_invocation: 2048,
    generated_token_reservations: 16384,
    active_seconds: 600,
    consent_wait_seconds: 900,
    wall_seconds: 3600,
};
/// Owner-approved opt-in policy; per-call and storage limits remain independent.
pub const INVESTIGATION_V2_POLICY: InvestigationPolicy = InvestigationPolicy {
    profile: "investigation-v2",
    request_bytes: 64 * 1024,
    journal_events: 512,
    max_completion_obligations: 56,
    model_invocations: 64,
    tool_actions: 56,
    selected_facts: 64,
    evidence_requests: 64,
    evidence_items: 56,
    evidence_bytes: 64 * 1024,
    captured_validation_bytes: 128 * 1024 * 1024,
    generated_tokens_per_invocation: 2048,
    generated_token_reservations: 131072,
    active_seconds: 600,
    consent_wait_seconds: 900,
    wall_seconds: 3600,
};
const POLICIES: &[&InvestigationPolicy] = &[&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY];

// A registered profile must fit the durable schema and unchanged fact ceiling.
const _: () = {
    let mut index = 0;
    while index < POLICIES.len() {
        // Evaluate checked derived capacities for every registered policy.
        assert!(POLICIES[index].max_completed_attempts() <= MAX_STORED_COMPLETION_ATTEMPTS);
        assert!(POLICIES[index].selected_facts <= super::MAX_SELECTED_FACTS);
        index += 1;
    }
};

impl InvestigationPolicy {
    pub const fn profile(&self) -> &'static str {
        self.profile
    }

    pub const fn envelope_bytes(&self) -> usize {
        self.request_bytes
    }

    /// Preserve the existing journal allowance of twice the tool-action ceiling.
    /// This is record capacity, not an additional tool-action allowance.
    pub const fn max_completed_attempts(&self) -> usize {
        match (self.tool_actions as usize).checked_mul(2) {
            Some(limit) => limit,
            None => panic!("registered investigation attempt capacity overflow"),
        }
    }

    /// Outer raw transport bound before choosing a closed request family.
    pub fn maximum_request_bytes() -> usize {
        POLICIES
            .iter()
            .map(|policy| policy.request_bytes())
            .max()
            .unwrap_or(0)
    }

    pub fn for_profile(profile: &str) -> Option<&'static Self> {
        POLICIES
            .iter()
            .copied()
            .find(|policy| policy.profile == profile)
    }

    /// Resolve only an exact immutable specification, including every counter.
    pub fn for_limits(limits: &InvestigationLimits) -> Option<&'static Self> {
        Self::for_profile(&limits.profile).filter(|policy| policy.limits() == *limits)
    }

    /// Durable event capacity; includes the host reserved terminal slots.
    pub const fn journal_events(&self) -> u64 {
        self.journal_events
    }

    pub const fn request_bytes(&self) -> usize {
        self.request_bytes
    }

    pub const fn max_completion_obligations(&self) -> usize {
        self.max_completion_obligations
    }

    pub fn limits(&self) -> InvestigationLimits {
        InvestigationLimits {
            profile: self.profile.into(),
            model_invocations: self.model_invocations,
            tool_actions: self.tool_actions,
            selected_facts: self.selected_facts,
            evidence_requests: self.evidence_requests,
            evidence_items: self.evidence_items,
            evidence_bytes: self.evidence_bytes,
            captured_validation_bytes: self.captured_validation_bytes,
            generated_tokens_per_invocation: self.generated_tokens_per_invocation,
            generated_token_reservations: self.generated_token_reservations,
            active_seconds: self.active_seconds,
            consent_wait_seconds: self.consent_wait_seconds,
            wall_seconds: self.wall_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_policy_preserves_exact_serialized_limits_and_closed_lookup() {
        // AC-0188: durable limits remain immutable and unknown policies fail closed.
        let expected = r#"{"profile":"investigation-v1","model_invocations":8,"tool_actions":8,"selected_facts":64,"evidence_requests":64,"evidence_items":12,"evidence_bytes":49152,"captured_validation_bytes":134217728,"generated_tokens_per_invocation":2048,"generated_token_reservations":16384,"active_seconds":600,"consent_wait_seconds":900,"wall_seconds":3600}"#;
        assert_eq!(
            serde_json::to_string(&InvestigationLimits::default()).unwrap(),
            expected
        );
        let policy = InvestigationPolicy::for_profile("investigation-v1").unwrap();
        assert_eq!(policy.limits(), InvestigationLimits::default());
        assert_eq!(policy.request_bytes(), 8192);
        assert_eq!(policy.max_completion_obligations(), 12);
        for profile in [
            "",
            "investigation-v3",
            "INVESTIGATION-V1",
            "investigation-v1 ",
        ] {
            assert!(InvestigationPolicy::for_profile(profile).is_none());
        }
    }
}
