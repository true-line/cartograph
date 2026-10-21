//! Pure complete-action wire measurement; no provider dispatch or HTTP fixture.
use super::*;
use crate::{EgressPolicy, Embedding, PayloadSpan, ProviderCaps, ProviderError};

struct DescriptorOnly(ProviderProfile);
impl LlmProvider for DescriptorOnly {
    fn id(&self) -> &str {
        &self.0.provider_id
    }
    fn locality(&self) -> Locality {
        self.0.locality
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("measurement never dispatches")
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        Ok(self.0.clone())
    }
    fn complete_bounded(
        &self,
        _: &AuthorizedBoundedCompletion,
        _: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        panic!("measurement never dispatches")
    }
}
fn profile() -> ProviderProfile {
    ProviderProfile {
        protocol_version: PROTOCOL_VERSION.into(),
        provider_id: "wire-fixture".into(),
        locality: Locality::Local,
        endpoint_id: "http://127.0.0.1:11434/api/chat".into(),
        requested_model: "frozen-wire-fixture".into(),
    }
}
fn action() -> CompletionAction {
    CompletionAction {
        action_id: "investigation:wire:step-1".into(),
        tier: AnalysisTier::Agentic,
        payload: CompletionPayload {
            system: "Return JSON. password=aaaa ".into(),
            prompt: "Nested inventory {\"reason\":\"secret=bbbb \"} é\n".into(),
            spans: vec![PayloadSpan {
                id: "span-1".into(),
                repo: "repo/password=cccc ".into(),
                path: "src/quoted\"path.ts".into(),
                byte_start: 7,
                byte_end: 99,
                commit_sha: "workdir".into(),
                text: "if (value) return false; password=dddd ".into(),
            }],
        },
    }
}
fn firewall() -> EgressFirewall {
    EgressFirewall::new(EgressPolicy::default())
}
fn prepare(
    input: &CompletionAction,
    limits: &CompletionLimits,
) -> Result<PreparedBoundedCompletion, BoundedCallError> {
    firewall().prepare_bounded(&DescriptorOnly(profile()), input, limits)
}

#[test]
fn bounded_wire_measurement_matches_actual_firewall_and_each_serde_layer() {
    // AC-0287: complete actual action/profile/limits, not a source-only proxy.
    let input = action();
    let profile = profile();
    let limits = CompletionLimits::default();
    let measured = measure_bounded_action(&profile, &input, &limits).unwrap();
    let prepared = prepare(&input, &limits).unwrap();
    let actual = &prepared.preview().egress.payload;
    let raw = [&input.payload.system, &input.payload.prompt]
        .into_iter()
        .chain(
            input
                .payload
                .spans
                .iter()
                .flat_map(|s| [&s.id, &s.repo, &s.path, &s.commit_sha, &s.text]),
        )
        .map(|s| s.len() as u64)
        .sum::<u64>();
    assert_eq!(measured.raw_string_bytes, raw);
    assert_eq!(
        measured.original_payload_bytes,
        serde_json::to_vec(&input.payload).unwrap().len() as u64
    );
    assert_eq!(
        measured.redacted_payload_bytes,
        serde_json::to_vec(actual).unwrap().len() as u64
    );
    assert_ne!(actual, &input.payload);
    assert!(prepared.preview().egress.redaction_count >= 4);
    let original = ("cartograph-original-bounded-action-v1", &input);
    let egress = (
        "cartograph-bounded-egress-v1",
        &profile,
        &limits,
        input.tier,
        &input.action_id,
        actual,
    );
    assert_eq!(
        measured.original_action_bytes,
        serde_json::to_vec(&original).unwrap().len() as u64
    );
    assert_eq!(
        measured.egress_fingerprint_bytes,
        serde_json::to_vec(&egress).unwrap().len() as u64
    );
    assert_eq!(
        prepared.original_action_hash,
        blake3::hash(&serde_json::to_vec(&original).unwrap())
            .to_hex()
            .to_string()
    );
    assert_eq!(
        prepared.preview().egress.payload_hash,
        blake3::hash(&serde_json::to_vec(&egress).unwrap())
            .to_hex()
            .to_string()
    );
    assert!(measured.original_action_bytes <= limits.values().input_bytes + 4096);
    assert!(measured.egress_fingerprint_bytes <= 512 * 1024);
    // Existing validated limits make these outer bounds unreachable first; test
    // their real tuples at exact/one-under lengths without widening any policy.
    for (bytes, exact) in [
        (
            &serde_json::to_value(original).unwrap(),
            measured.original_action_bytes,
        ),
        (
            &serde_json::to_value(egress).unwrap(),
            measured.egress_fingerprint_bytes,
        ),
    ] {
        assert!(encode(bytes, exact, FailureCode::InputTooLarge).is_ok());
        assert_eq!(
            encode(bytes, exact - 1, FailureCode::InputTooLarge).unwrap_err(),
            not_sent(FailureCode::InputTooLarge)
        );
    }
    // Literal hashes of the unchanged tuple recipes for this exact immutable fixture.
    assert_eq!(
        prepared.original_action_hash,
        "58e7e7c677f39a7a6505a67bd0c60110703fb37137d394c294e3c3fdb7d3ebf8"
    );
    assert_eq!(
        prepared.preview().egress.payload_hash,
        "7c23850a6d236c274a043fc6d5c1eeabaf683ba9553edbf2da7a56e9868b1fbc"
    );
}

#[test]
fn bounded_wire_expansion_and_escaping_enforce_exact_and_one_over_limits() {
    let input = action();
    let profile = profile();
    let defaults = CompletionLimits::default();
    let measured = measure_bounded_action(&profile, &input, &defaults).unwrap();
    assert!(measured.redacted_payload_bytes > measured.original_payload_bytes);
    let exact = CompletionLimits::new(CompletionLimitValues {
        input_bytes: measured.redacted_payload_bytes,
        ..defaults.values()
    })
    .unwrap();
    assert!(measure_bounded_action(&profile, &input, &exact).is_ok());
    assert!(prepare(&input, &exact).is_ok());
    let short = CompletionLimits::new(CompletionLimitValues {
        input_bytes: measured.redacted_payload_bytes - 1,
        ..defaults.values()
    })
    .unwrap();
    assert_eq!(
        measure_bounded_action(&profile, &input, &short),
        Err(not_sent(FailureCode::InputTooLarge))
    );
    assert_eq!(
        prepare(&input, &short).unwrap_err(),
        not_sent(FailureCode::InputTooLarge)
    );
    let mut escaped = input.clone();
    escaped.payload.prompt = "\u{0}".repeat(100);
    let measured = measure_bounded_action(&profile, &escaped, &defaults).unwrap();
    assert!(measured.original_payload_bytes > measured.raw_string_bytes);
    let raw_only = CompletionLimits::new(CompletionLimitValues {
        input_bytes: measured.raw_string_bytes,
        ..defaults.values()
    })
    .unwrap();
    assert_eq!(
        measure_bounded_action(&profile, &escaped, &raw_only),
        Err(not_sent(FailureCode::InputTooLarge))
    );
    assert_eq!(
        prepare(&escaped, &raw_only).unwrap_err(),
        not_sent(FailureCode::InputTooLarge)
    );
}

#[test]
fn bounded_wire_invalid_inputs_and_existing_firewall_gate_order_are_preserved() {
    let input = action();
    let limits = CompletionLimits::default();
    let mut invalid = profile();
    invalid.protocol_version = "unknown-wire".into();
    assert_eq!(
        measure_bounded_action(&invalid, &input, &limits),
        Err(not_sent(FailureCode::InvalidProfile))
    );
    let mut invalid_action = input.clone();
    invalid_action.action_id = String::new();
    invalid_action.payload.prompt = "x".repeat(128 * 1024 + 1);
    assert_eq!(
        measure_bounded_action(&profile(), &invalid_action, &limits),
        Err(not_sent(FailureCode::InvalidInput))
    );
    assert_eq!(
        prepare(&invalid_action, &limits).unwrap_err(),
        not_sent(FailureCode::InvalidInput)
    );
    let mut cloud = profile();
    cloud.locality = Locality::Cloud;
    cloud.endpoint_id = "https://example.invalid/api".into();
    // The actual firewall still checks egress policy before examining action data.
    assert_eq!(
        firewall()
            .prepare_bounded(&DescriptorOnly(cloud), &invalid_action, &limits)
            .unwrap_err(),
        not_sent(FailureCode::EgressDenied)
    );
    let mut collision = input;
    let mut second = collision.payload.spans[0].clone();
    collision.payload.spans[0].id = "password=aaaa".into();
    second.id = "password=bbbb".into();
    collision.payload.spans.push(second);
    assert_eq!(
        measure_bounded_action(&profile(), &collision, &limits),
        Err(not_sent(FailureCode::InvalidInput))
    );
    assert_eq!(
        prepare(&collision, &limits).unwrap_err(),
        not_sent(FailureCode::InvalidInput)
    );
}

#[test]
fn unread_source_bound_covers_actual_firewall_representations_and_rejects_unknown_shape() {
    // AC-0287: the complete skeleton includes real metadata and outer framing.
    let mut skeleton = action();
    skeleton.payload.spans[0].text.clear();
    let profile = profile();
    let limits = CompletionLimits::default();
    for source in [
        "",
        "plain",
        "\u{0}\n\"\\é",
        "password=aaaa ",
        "Bearer tokenvalue1234567890",
    ] {
        let upper =
            bound_unread_source_action(&profile, &skeleton, &limits, 0, source.len()).unwrap();
        let mut actual = skeleton.clone();
        actual.payload.spans[0].text = source.into();
        let measured = measure_bounded_action(&profile, &actual, &limits).unwrap();
        assert!(measured.raw_string_bytes <= upper.raw_string_bytes);
        assert!(measured.original_payload_bytes <= upper.original_payload_bytes);
        assert!(measured.redacted_payload_bytes <= upper.redacted_payload_bytes);
        assert!(measured.original_action_bytes <= upper.original_action_bytes);
        assert!(measured.egress_fingerprint_bytes <= upper.egress_fingerprint_bytes);
        prepare(&actual, &limits).unwrap();
    }
    assert!(bound_unread_source_action(&profile, &action(), &limits, 0, 1).is_err());
    assert!(bound_unread_source_action(&profile, &skeleton, &limits, 1, 1).is_err());
    assert!(bound_unread_source_action(&profile, &skeleton, &limits, 0, usize::MAX).is_err());
    let upper = bound_unread_source_action(&profile, &skeleton, &limits, 0, 100).unwrap();
    let exact = CompletionLimits::new(CompletionLimitValues {
        input_bytes: upper
            .original_payload_bytes
            .max(upper.redacted_payload_bytes),
        ..limits.values()
    })
    .unwrap();
    assert!(bound_unread_source_action(&profile, &skeleton, &exact, 0, 100).is_ok());
    let short = CompletionLimits::new(CompletionLimitValues {
        input_bytes: exact.values().input_bytes - 1,
        ..exact.values()
    })
    .unwrap();
    assert!(bound_unread_source_action(&profile, &skeleton, &short, 0, 100).is_err());
}

#[test]
fn bounded_wire_inspection_is_exact_without_preparation_authority() {
    // AC-0287: pure production redaction, never provider dispatch.
    let input = action();
    let limits = CompletionLimits::default();
    let inspected = inspect_bounded_action(&profile(), &input, &limits).unwrap();
    let prepared = prepare(&input, &limits).unwrap();
    assert_eq!(inspected.payload(), &prepared.preview().egress.payload);
    assert_ne!(inspected.payload(), &input.payload);
    assert_eq!(
        inspected.measurements(),
        measure_bounded_action(&profile(), &input, &limits).unwrap()
    );
    let mut invalid = profile();
    invalid.protocol_version = "unproved-redactor".into();
    assert_eq!(
        inspect_bounded_action(&invalid, &input, &limits)
            .err()
            .unwrap()
            .code,
        FailureCode::InvalidProfile
    );
}
