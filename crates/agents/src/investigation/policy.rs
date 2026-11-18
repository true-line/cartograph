//! Closed immutable policy registry with an explicit opt-in expanded profile.
//! Admission bounds are distinct from durable runtime counters; callers cannot
//! deserialize or construct a policy from their own limit values.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::{
    GenerationPolicy, HistoryPolicy, InvestigationLimits, staged_refusal::CompletionCallMode,
};

/// Host completion-attempt schema ceiling. Raising it requires an explicit
/// migration; a registry entry may never promise more durable attempt slots.
pub const MAX_STORED_COMPLETION_ATTEMPTS: usize = 112;
/// Immutable historical history ceiling and owner-approved versioned successor.
pub const LEGACY_SUPPLIED_HISTORY_BYTES: usize = 16 * 1024;
pub const PARENT_HISTORY_32768_V1_BYTES: usize = 32 * 1024;
const _: () = assert!(PARENT_HISTORY_32768_V1_BYTES <= INVESTIGATION_V2_POLICY.request_bytes);

/// Owner-approved desktop active-time maximum; defaults stay in the registry.
pub const MAX_CONFIGURED_ACTIVE_SECONDS: u64 = 3_600;

#[derive(Debug, Clone)]
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
    generation_policy: Option<GenerationPolicy>,
    history_policy: Option<HistoryPolicy>,
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
    generation_policy: None,
    history_policy: None,
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
    generation_policy: None,
    history_policy: None,
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
        assert!(POLICIES[index].evidence_items <= llm::bounded::MAX_COMPLETION_SPANS as usize);
        // MCP retained-read admission uses V2 as the largest registered policy.
        // Reject a new profile at compile time until that caller is updated.
        assert!(POLICIES[index].tool_actions <= INVESTIGATION_V2_POLICY.tool_actions);
        assert!(POLICIES[index].evidence_items <= INVESTIGATION_V2_POLICY.evidence_items);
        assert!(POLICIES[index].evidence_bytes <= INVESTIGATION_V2_POLICY.evidence_bytes);
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

    /// Freeze an explicitly configured desktop active-time allowance (#1633).
    /// Every other field remains the immutable registry value.
    pub fn with_active_seconds(&self, active_seconds: u64) -> Option<Self> {
        if !(self.active_seconds..=MAX_CONFIGURED_ACTIVE_SECONDS).contains(&active_seconds) {
            return None;
        }
        let mut effective = self.clone();
        effective.active_seconds = active_seconds;
        Some(effective)
    }

    /// Freeze the owner-approved hosted Opus output allowance (#1709): every
    /// call may reserve `llm::bounded::HOSTED_OUTPUT_TOKENS`, and the total
    /// reservation scales with the unchanged invocation ceiling. Only the
    /// expanded profile takes it; the caller decides which provider qualifies.
    pub fn with_hosted_generation(&self) -> Option<Self> {
        if self.generation_policy.is_some()
            || self.profile != INVESTIGATION_V2_POLICY.profile
            || self.generated_tokens_per_invocation
                != INVESTIGATION_V2_POLICY.generated_tokens_per_invocation
        {
            return None;
        }
        let mut effective = self.clone();
        effective.generated_tokens_per_invocation = llm::bounded::HOSTED_OUTPUT_TOKENS;
        effective.generated_token_reservations = self
            .model_invocations
            .checked_mul(llm::bounded::HOSTED_OUTPUT_TOKENS)?;
        Some(effective)
    }

    /// Owner-approved local lane. Host authentication of provider and mode is separate.
    pub fn with_local_finish_generation(&self) -> Option<Self> {
        if self.profile != INVESTIGATION_V2_POLICY.profile
            || self.generated_tokens_per_invocation != 2048
            || self.generated_token_reservations != 131072
            || self.generation_policy.is_some()
        {
            return None;
        }
        let mut effective = self.clone();
        effective.generation_policy = Some(GenerationPolicy::LocalFinish4096V1);
        Some(effective)
    }

    pub fn with_parent_history_32768(&self) -> Option<Self> {
        if self.profile != INVESTIGATION_V2_POLICY.profile {
            return None;
        }
        let mut effective = self.clone();
        effective.history_policy = Some(HistoryPolicy::ParentHistory32768V1);
        Some(effective)
    }

    pub const fn supplied_history_bytes(&self) -> usize {
        match self.history_policy {
            None => LEGACY_SUPPLIED_HISTORY_BYTES,
            Some(HistoryPolicy::ParentHistory32768V1) => PARENT_HISTORY_32768_V1_BYTES,
        }
    }

    pub fn local_finish_generation(&self) -> bool {
        self.generation_policy == Some(GenerationPolicy::LocalFinish4096V1)
    }

    pub fn output_tokens_for_mode(&self, mode: CompletionCallMode) -> u32 {
        if self.local_finish_generation() && mode == CompletionCallMode::FinishOnly {
            4096
        } else {
            self.generated_tokens_per_invocation
        }
    }

    /// Recover a saved effective policy without consulting current settings.
    /// Static lookup remains exact so existing callers cannot opt in silently.
    pub fn for_effective_limits(limits: &InvestigationLimits) -> Option<Self> {
        let mut effective =
            Self::for_profile(&limits.profile)?.with_active_seconds(limits.active_seconds)?;
        if limits.generated_tokens_per_invocation == llm::bounded::HOSTED_OUTPUT_TOKENS {
            effective = effective.with_hosted_generation()?;
        }
        if limits.generation_policy.is_some() {
            effective = effective.with_local_finish_generation()?;
        }
        if limits.history_policy.is_some() {
            effective = effective.with_parent_history_32768()?;
        }
        (effective.limits() == *limits).then_some(effective)
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
            generation_policy: self.generation_policy,
            history_policy: self.history_policy,
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
    #[test]
    fn effective_time_policy_preserves_non_time_fields_and_static_lookup() {
        // AC-0188: effective limits retain immutable non-time policy identity.
        for base in [INVESTIGATION_V1_POLICY, INVESTIGATION_V2_POLICY] {
            for seconds in [600, 601, 3_600] {
                let effective = base.with_active_seconds(seconds).unwrap();
                let saved = effective.limits();
                assert_eq!(saved.active_seconds, seconds);
                assert_eq!(saved.wall_seconds, 3_600);
                assert_eq!(
                    InvestigationPolicy::for_effective_limits(&saved)
                        .unwrap()
                        .limits(),
                    saved
                );
                assert_eq!(
                    InvestigationPolicy::for_limits(&saved).is_some(),
                    seconds == 600
                );
                let canonical = serde_json::to_value(&saved).unwrap();
                for (field, value) in canonical.as_object().unwrap() {
                    if field == "active_seconds" {
                        continue;
                    }
                    let mut forged = canonical.clone();
                    forged[field] = if let Some(number) = value.as_u64() {
                        serde_json::json!(number + 1)
                    } else {
                        serde_json::json!("unknown-profile")
                    };
                    let forged: InvestigationLimits = serde_json::from_value(forged).unwrap();
                    assert!(
                        InvestigationPolicy::for_effective_limits(&forged).is_none(),
                        "accepted altered {field}"
                    );
                }
            }
            for seconds in [0, 599, 3_601, u64::MAX] {
                assert!(base.with_active_seconds(seconds).is_none());
            }
        }
    }
    // AC-0570: the hosted output allowance exists only on the expanded profile,
    // composes with the time override, and is recovered only as exact values.
    #[test]
    fn hosted_generation_is_v2_only_and_recovers_exactly() {
        use llm::bounded::HOSTED_OUTPUT_TOKENS;
        assert!(INVESTIGATION_V1_POLICY.with_hosted_generation().is_none());
        for seconds in [600, 1200, 3_600] {
            let hosted = INVESTIGATION_V2_POLICY
                .with_active_seconds(seconds)
                .unwrap()
                .with_hosted_generation()
                .unwrap();
            assert!(hosted.with_hosted_generation().is_none());
            let saved = hosted.limits();
            let mut expected = INVESTIGATION_V2_POLICY.limits();
            expected.active_seconds = seconds;
            expected.generated_tokens_per_invocation = HOSTED_OUTPUT_TOKENS;
            expected.generated_token_reservations = 64 * HOSTED_OUTPUT_TOKENS;
            assert_eq!(saved, expected);
            assert_eq!(saved.generated_token_reservations, 1_048_576);
            assert_eq!(
                InvestigationPolicy::for_effective_limits(&saved)
                    .unwrap()
                    .limits(),
                saved
            );
            assert!(InvestigationPolicy::for_limits(&saved).is_none());
            for (per_call, total) in [
                (HOSTED_OUTPUT_TOKENS, 131_072),
                (HOSTED_OUTPUT_TOKENS, 64 * HOSTED_OUTPUT_TOKENS + 1),
                (8192, 64 * 8192),
                (2048, 64 * HOSTED_OUTPUT_TOKENS),
            ] {
                let mut forged = saved.clone();
                forged.generated_tokens_per_invocation = per_call;
                forged.generated_token_reservations = total;
                assert!(InvestigationPolicy::for_effective_limits(&forged).is_none());
            }
            let mut v1 = INVESTIGATION_V1_POLICY.limits();
            v1.generated_tokens_per_invocation = HOSTED_OUTPUT_TOKENS;
            v1.generated_token_reservations = 8 * HOSTED_OUTPUT_TOKENS;
            assert!(InvestigationPolicy::for_effective_limits(&v1).is_none());
        }
    }
    #[test]
    fn configured_time_limits_preserve_workspace_reservations() {
        use crate::investigation::InvestigationInput;
        use llm::bounded::CompletionLimits;
        // AC-0188: time overrides must not invalidate shared phase reservations.
        for base in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
            let effective = base.with_active_seconds(3_600).unwrap();
            let ordinary = CompletionLimits::default();
            let mut values = ordinary.values();
            values.request_timeout_ms = 600_000;
            let configured = CompletionLimits::new(values).unwrap();
            assert_eq!(
                InvestigationInput::construction_workspace_bytes(base),
                InvestigationInput::construction_workspace_bytes(&effective),
            );
            assert_eq!(
                InvestigationInput::retained_workspace_bytes(base),
                InvestigationInput::retained_workspace_bytes(&effective),
            );
            for (ordinary_size, configured_size) in [
                (
                    InvestigationInput::context_preview_workspace_bytes(base, &ordinary),
                    InvestigationInput::context_preview_workspace_bytes(&effective, &configured),
                ),
                (
                    InvestigationInput::source_preview_workspace_bytes(base, &ordinary),
                    InvestigationInput::source_preview_workspace_bytes(&effective, &configured),
                ),
            ] {
                assert!(ordinary_size.is_some());
                assert_eq!(ordinary_size, configured_size);
            }
            let ordinary_seal = InvestigationInput::sealing_workspace_bytes(base, &ordinary);
            assert!(ordinary_seal.is_some());
            assert_eq!(
                ordinary_seal,
                InvestigationInput::sealing_workspace_bytes(&effective, &configured)
            );
        }
    }
}

#[cfg(test)]
#[path = "local_finish_policy_tests.rs"]
mod local_finish_policy_tests;

#[cfg(test)]
#[test]
fn versioned_history_policy_preserves_old_limits_and_other_allowances() {
    // AC-0578: only the closed opt-in marker differs, no historical wire change.
    for old in [
        INVESTIGATION_V2_POLICY.clone(),
        INVESTIGATION_V2_POLICY.with_hosted_generation().unwrap(),
        INVESTIGATION_V2_POLICY
            .with_local_finish_generation()
            .unwrap(),
    ] {
        let original = serde_json::to_value(old.limits()).unwrap();
        assert!(original.get("history_policy").is_none());
        let expanded = old.with_parent_history_32768().unwrap();
        assert_eq!(old.supplied_history_bytes(), LEGACY_SUPPLIED_HISTORY_BYTES);
        assert_eq!(
            expanded.supplied_history_bytes(),
            PARENT_HISTORY_32768_V1_BYTES
        );
        let mut changed = serde_json::to_value(expanded.limits()).unwrap();
        assert_eq!(
            changed
                .as_object_mut()
                .unwrap()
                .remove("history_policy")
                .unwrap(),
            "parent-history-32768-v1"
        );
        assert_eq!(changed, original);
        let wire = serde_json::to_vec(&expanded.limits()).unwrap();
        let limits = serde_json::from_slice(&wire).unwrap();
        assert_eq!(
            InvestigationPolicy::for_effective_limits(&limits)
                .unwrap()
                .limits(),
            expanded.limits()
        );
        assert!(InvestigationPolicy::for_limits(&limits).is_none());
        let mut bad = serde_json::to_value(limits).unwrap();
        bad["history_policy"] = "parent-history-unknown".into();
        assert!(serde_json::from_value::<InvestigationLimits>(bad).is_err());
    }
    assert!(
        INVESTIGATION_V1_POLICY
            .with_parent_history_32768()
            .is_none()
    );
}
