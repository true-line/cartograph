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
            citation_drop: None,
            prose_correction: None,
            schema_version: 1,
            citation_correction: None,
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

#[test]
fn finish_reserve_selects_mode_at_independent_call_and_token_boundaries() {
    // AC-0287: an unrestricted call cannot spend a required finishing slot.
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let limits = policy.limits();
        for refusal in [0, 1] {
            let value = metadata(refusal);
            let required = value.finish_call_reserve_with_policy(policy).unwrap();
            for calls_left in 0..=required.model_invocations + 2 {
                for token_calls_left in 0..=required.model_invocations + 2 {
                    let usage = InvestigationUsage {
                        model_invocations: limits.model_invocations - calls_left,
                        generated_token_reservations: limits.generated_token_reservations
                            - token_calls_left * limits.generated_tokens_per_invocation,
                        ..Default::default()
                    };
                    let before = usage.clone();
                    let expected = if calls_left < required.model_invocations
                        || token_calls_left < required.model_invocations
                    {
                        Err(InvestigationError::LimitExceeded)
                    } else if calls_left == required.model_invocations
                        || token_calls_left == required.model_invocations
                    {
                        Ok(CompletionCallMode::FinishOnly)
                    } else {
                        Ok(CompletionCallMode::Choice)
                    };
                    assert_eq!(value.next_call_mode_with_policy(&usage, policy), expected);
                    assert_eq!(usage, before);
                }
            }
            let mut usage = InvestigationUsage {
                model_invocations: limits.model_invocations - required.model_invocations - 1,
                generated_token_reservations: limits.generated_token_reservations
                    - required.generated_token_reservations,
                ..Default::default()
            };
            assert_eq!(
                value.next_call_mode_with_policy(&usage, policy),
                Ok(CompletionCallMode::FinishOnly)
            );
            usage.generated_token_reservations += 1;
            assert_eq!(
                value.next_call_mode_with_policy(&usage, policy),
                Err(InvestigationError::LimitExceeded)
            );
            usage.model_invocations = u32::MAX;
            assert_eq!(
                value.next_call_mode_with_policy(&usage, policy),
                Err(InvestigationError::LimitExceeded)
            );
        }
    }
}

#[test]
fn local_finish_reserve_keeps_corrections_inside_total_budget() {
    let policy = INVESTIGATION_V2_POLICY
        .with_local_finish_generation()
        .unwrap();
    for count in [0, 1] {
        let value = metadata(count);
        let reserve = value.finish_call_reserve_with_policy(&policy).unwrap();
        assert_eq!(reserve.generated_token_reservations, (2 - count) * 4096);
        let mut usage = InvestigationUsage {
            generated_token_reservations: 131072 - reserve.generated_token_reservations,
            ..Default::default()
        };
        assert_eq!(
            value.next_call_mode_with_policy(&usage, &policy).unwrap(),
            CompletionCallMode::FinishOnly
        );
        usage.generated_token_reservations += 1;
        assert!(value.next_call_mode_with_policy(&usage, &policy).is_err());
        usage.generated_token_reservations -= 2049;
        assert_eq!(
            value.next_call_mode_with_policy(&usage, &policy).unwrap(),
            CompletionCallMode::Choice
        );
    }
}
