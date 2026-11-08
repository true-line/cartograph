use super::*;

#[test]
fn local_finish_policy_is_closed_and_preserves_historical_limits() {
    // AC-0287: only explicit versioned finish allowance changes; V2 bytes stay exact.
    let old = INVESTIGATION_V2_POLICY.limits();
    let bytes = serde_json::to_vec(&old).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("generation_policy"));
    let policy = INVESTIGATION_V2_POLICY
        .with_local_finish_generation()
        .unwrap();
    let mut new = policy.limits();
    assert_eq!(new.generated_token_reservations, 131072);
    assert_eq!(
        policy.output_tokens_for_mode(CompletionCallMode::Choice),
        2048
    );
    assert_eq!(
        policy.output_tokens_for_mode(CompletionCallMode::FinishOnly),
        4096
    );
    assert_eq!(
        INVESTIGATION_V2_POLICY.output_tokens_for_mode(CompletionCallMode::FinishOnly),
        2048
    );
    let serialized = serde_json::to_vec(&new).unwrap();
    let reopened: InvestigationLimits = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(
        InvestigationPolicy::for_effective_limits(&reopened)
            .unwrap()
            .limits(),
        new
    );
    assert!(InvestigationPolicy::for_limits(&new).is_none());
    assert!(policy.with_hosted_generation().is_none());
    assert!(
        INVESTIGATION_V2_POLICY
            .with_hosted_generation()
            .unwrap()
            .with_local_finish_generation()
            .is_none()
    );
    assert!(
        INVESTIGATION_V1_POLICY
            .with_local_finish_generation()
            .is_none()
    );
    new.generation_policy = None;
    assert_eq!(serde_json::to_vec(&new).unwrap(), bytes);
    for field in [
        "generated_tokens_per_invocation",
        "generated_token_reservations",
        "tool_actions",
    ] {
        let mut forged = serde_json::to_value(&reopened).unwrap();
        forged[field] = (forged[field].as_u64().unwrap() + 1).into();
        assert!(
            InvestigationPolicy::for_effective_limits(&serde_json::from_value(forged).unwrap())
                .is_none()
        );
    }
}
