use super::*;

fn metadata(refusal_count: u32) -> ProtectedCompletionMetadata {
    ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: "a".repeat(64),
        obligations_hash: "b".repeat(64),
        completed_attempt_ids: vec![],
        refusal_count,
        last_refusal_id: (refusal_count > 0).then(|| "c".repeat(64)),
        feedback: (refusal_count > 0).then(|| RefusalFeedback {
            schema_version: 1,
            unmet: vec![FeedbackEntry {
                obligation_id: "d".repeat(64),
                kind: "source".into(),
                reason: RefusalReason::NotAttempted,
            }],
        }),
    }
}

#[test]
fn finish_reserve_derives_calls_and_tokens_from_valid_saved_metadata() {
    // AC-0287: this component is a reservation requirement, not admission.
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        for count in [0, 1] {
            let value = metadata(count);
            let original = serde_json::to_vec(&value).unwrap();
            let reserve = value.finish_call_reserve_with_policy(policy).unwrap();
            assert_eq!(reserve.model_invocations, 2 - count);
            assert_eq!(
                reserve.generated_token_reservations,
                reserve.model_invocations * policy.limits().generated_tokens_per_invocation
            );
            assert_eq!(serde_json::to_vec(&value).unwrap(), original);
        }
    }
}

#[test]
fn finish_reserve_rejects_terminal_or_invalid_metadata() {
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        for count in [2, u32::MAX] {
            assert!(
                metadata(count)
                    .finish_call_reserve_with_policy(policy)
                    .is_err()
            );
        }
        let mut value = metadata(1);
        value.feedback = None;
        assert!(value.finish_call_reserve_with_policy(policy).is_err());
        let mut value = metadata(0);
        value.request_hash = "unbound".into();
        assert!(value.finish_call_reserve_with_policy(policy).is_err());
    }
}
