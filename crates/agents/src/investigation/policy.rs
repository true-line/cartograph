//! Closed immutable policy registry. No new profile or execution path is enabled.
//! Admission bounds are distinct from durable runtime counters; callers cannot
//! deserialize or construct a policy from their own limit values.
use super::InvestigationLimits;

#[derive(Debug)]
pub struct InvestigationPolicy {
    _private: (),
}

pub const INVESTIGATION_V1_POLICY: InvestigationPolicy = InvestigationPolicy { _private: () };

impl InvestigationPolicy {
    pub fn for_profile(profile: &str) -> Option<&'static Self> {
        match profile {
            "investigation-v1" => Some(&INVESTIGATION_V1_POLICY),
            _ => None,
        }
    }

    pub const fn request_bytes(&self) -> usize {
        8 * 1024
    }

    pub const fn max_completion_obligations(&self) -> usize {
        12
    }

    pub fn limits(&self) -> InvestigationLimits {
        InvestigationLimits {
            profile: "investigation-v1".into(),
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
            "investigation-v2",
            "INVESTIGATION-V1",
            "investigation-v1 ",
        ] {
            assert!(InvestigationPolicy::for_profile(profile).is_none());
        }
    }
}
