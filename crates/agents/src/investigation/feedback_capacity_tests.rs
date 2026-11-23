//! Component sizing only: no provider, refusal decision or acquisition is run.
use super::*;
use llm::CompletionPayload;
use serde_json::json;

fn reasons() -> [RefusalReason; 6] {
    [
        RefusalReason::NotAttempted,
        RefusalReason::AcquiredNotSupplied,
        RefusalReason::Redacted,
        RefusalReason::HashMismatch,
        RefusalReason::Unavailable,
        RefusalReason::ExcludedByCapacity,
    ]
}

// Deliberately exhaustive: a new reason must extend the sizing dominance tests.
fn reason_index(reason: RefusalReason) -> usize {
    match reason {
        RefusalReason::NotAttempted => 0,
        RefusalReason::AcquiredNotSupplied => 1,
        RefusalReason::Redacted => 2,
        RefusalReason::HashMismatch => 3,
        RefusalReason::Unavailable => 4,
        RefusalReason::ExcludedByCapacity => 5,
    }
}

fn envelope(policy: &InvestigationPolicy, count: usize) -> ObligationEnvelope {
    let target = super::super::tests::inspection_tests::fixture().0;
    let mut source = Obligation::Source {
        obligation_id: String::new(),
        target: Box::new(target),
    };
    let id = source.content_id().unwrap();
    if let Obligation::Source { obligation_id, .. } = &mut source {
        *obligation_id = id;
    }
    let mut obligations = vec![source];
    for index in 1..count {
        let mut item = Obligation::Inventory {
            obligation_id: String::new(),
            target: InventoryTarget {
                context_id: format!("context-v2:{}", "b".repeat(64)),
                repo: format!("fixture/{index:03}-quoted-\"\\-é"),
                inventory_hash: "a".repeat(64),
            },
        };
        let id = item.content_id().unwrap();
        if let Obligation::Inventory { obligation_id, .. } = &mut item {
            *obligation_id = id;
        }
        obligations.push(item);
    }
    let legacy = policy.profile() == INVESTIGATION_V1_POLICY.profile();
    ObligationEnvelope {
        schema_version: if legacy { 1 } else { 2 },
        contract: if legacy {
            "investigation-completion-obligations@1"
        } else {
            "investigation-completion-obligations@2"
        }
        .into(),
        obligations,
    }
    .normalized_with_policy(policy)
    .unwrap()
}

/// Exercise the real feedback and protected-metadata DTO serializers, the
/// prompt JSON value, its escaped string, and the outer production payload.
/// Other fields are identical across comparisons, including escaped content.
fn serialized_sizes(
    feedback: &RefusalFeedback,
    obligations: &ObligationEnvelope,
    policy: &InvestigationPolicy,
) -> [usize; 4] {
    let raw = feedback.bytes_with_policy(policy).unwrap();
    let metadata = staged_refusal::ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: "a".repeat(64),
        obligations_hash: obligations.fingerprint_with_policy(policy).unwrap(),
        completed_attempt_ids: (0..policy.max_completed_attempts())
            .map(|index| format!("{index:064x}"))
            .collect(),
        refusal_count: 1,
        last_refusal_id: Some("c".repeat(64)),
        feedback: Some(feedback.clone()),
    };
    metadata.validate_with_policy(policy).unwrap();
    let prompt = json!({"schema_version":8,"completion_metadata":metadata,
        "unchanged_context":"quotes: \" slash: \\ control: \u{1} unicode: é"});
    let prompt_bytes = bounded_bytes(&prompt, MAX_INPUT_BYTES).unwrap();
    let prompt_string = String::from_utf8(prompt_bytes.clone()).unwrap();
    let escaped_prompt = bounded_bytes(&prompt_string, MAX_INPUT_BYTES).unwrap();
    let payload = CompletionPayload {
        response_schema: None,
        system: "Fixed system text with \"quotes\" and \\slashes.".into(),
        prompt: prompt_string,
        spans: vec![],
    };
    [
        raw.len(),
        prompt_bytes.len(),
        escaped_prompt.len(),
        bounded_bytes(&payload, MAX_INPUT_BYTES).unwrap().len(),
    ]
}

fn dominates(actual: [usize; 4], upper: [usize; 4]) {
    for (layer, (actual, upper)) in actual.into_iter().zip(upper).enumerate() {
        assert!(
            actual <= upper,
            "serialization layer{layer}: {actual}>{upper}"
        );
    }
}

#[test]
fn feedback_capacity_mixed_envelope_preserves_exact_sorted_kind_and_id_output() {
    // AC-0287 / T-0287: obligation normalization orders mixed entries by the
    // same (kind,id) contract required by feedback validation and serialization.
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let obligations = envelope(policy, 3);
        let source_id = obligations
            .obligations
            .iter()
            .find_map(|item| match item {
                Obligation::Source { obligation_id, .. } => Some(obligation_id.clone()),
                _ => None,
            })
            .unwrap();
        let mut inventory_ids = obligations
            .obligations
            .iter()
            .filter_map(|item| match item {
                Obligation::Inventory { obligation_id, .. } => Some(obligation_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        inventory_ids.sort();
        assert_eq!(inventory_ids.len(), 2);
        let expected = RefusalFeedback {
            citation_drop: None,
            prose_correction: None,
            schema_version: 1,
            citation_correction: None,
            unmet: vec![
                FeedbackEntry {
                    kind: "inventory".into(),
                    obligation_id: inventory_ids[0].clone(),
                    reason: RefusalReason::AcquiredNotSupplied,
                },
                FeedbackEntry {
                    kind: "inventory".into(),
                    obligation_id: inventory_ids[1].clone(),
                    reason: RefusalReason::AcquiredNotSupplied,
                },
                FeedbackEntry {
                    kind: "source".into(),
                    obligation_id: source_id,
                    reason: RefusalReason::AcquiredNotSupplied,
                },
            ],
        };
        let upper =
            RefusalFeedback::sizing_upper_envelope_with_policy(&obligations, policy).unwrap();
        assert_eq!(upper.feedback, expected);
        assert_eq!(
            upper
                .feedback
                .unmet
                .iter()
                .map(|entry| entry.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["inventory", "inventory", "source"]
        );
        assert_eq!(
            serde_json::to_vec(&upper).unwrap(),
            expected.bytes_with_policy(policy).unwrap()
        );
    }
}

#[test]
fn feedback_capacity_upper_envelope_covers_every_reason_and_mixed_subset() {
    // AC-0287 / T-0287: sizing envelopes never assert actual unmet obligations.
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let obligations = envelope(policy, 3);
        let upper =
            RefusalFeedback::sizing_upper_envelope_with_policy(&obligations, policy).unwrap();
        assert_eq!(
            serde_json::to_vec(&upper).unwrap(),
            upper.feedback.bytes_with_policy(policy).unwrap()
        );
        let upper_sizes = serialized_sizes(&upper.feedback, &obligations, policy);
        for (entry, obligation) in upper.feedback.unmet.iter().zip(&obligations.obligations) {
            assert_eq!(
                (entry.kind.as_str(), entry.obligation_id.as_str()),
                obligation.key()
            );
        }
        let longest = serde_json::to_vec(&RefusalReason::AcquiredNotSupplied).unwrap();
        for (index, reason) in reasons().into_iter().enumerate() {
            assert_eq!(reason_index(reason), index);
            assert!(serde_json::to_vec(&reason).unwrap().len() <= longest.len());
        }
        // Six possibilities per entry: absent, or any of the five enum reasons.
        // Exhaust all215 nonempty assignments, including mixed reasons/subsets.
        for mut assignment in 1usize..6usize.pow(3) {
            let mut unmet = vec![];
            for entry in &upper.feedback.unmet {
                let selected = assignment % 6;
                assignment /= 6;
                if selected != 0 {
                    let mut actual = entry.clone();
                    actual.reason = reasons()[selected - 1];
                    unmet.push(actual);
                }
            }
            dominates(
                serialized_sizes(
                    &RefusalFeedback {
                        citation_drop: None,
                        prose_correction: None,
                        schema_version: 1,
                        citation_correction: None,
                        unmet,
                    },
                    &obligations,
                    policy,
                ),
                upper_sizes,
            );
        }
    }
}

#[test]
fn feedback_capacity_upper_envelope_covers_maximum_saved_policy_sets() {
    // AC-0287 / T-0287: include all56 V2 obligations and its112 attempt slots.
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let obligations = envelope(policy, policy.max_completion_obligations());
        let upper =
            RefusalFeedback::sizing_upper_envelope_with_policy(&obligations, policy).unwrap();
        assert_eq!(
            upper.feedback.unmet.len(),
            policy.max_completion_obligations()
        );
        let bound = serialized_sizes(&upper.feedback, &obligations, policy);
        assert!(bound[0] <= RefusalFeedback::max_bytes_with_policy(policy).unwrap());
        assert!(bound[2] > bound[1]);
        for count in 1..=upper.feedback.unmet.len() {
            for reason in reasons() {
                let actual = RefusalFeedback {
                    citation_drop: None,
                    prose_correction: None,
                    schema_version: 1,
                    citation_correction: None,
                    unmet: upper.feedback.unmet[..count]
                        .iter()
                        .cloned()
                        .map(|mut entry| {
                            entry.reason = reason;
                            entry
                        })
                        .collect(),
                };
                dominates(serialized_sizes(&actual, &obligations, policy), bound);
            }
        }
        let mixed = RefusalFeedback {
            citation_drop: None,
            prose_correction: None,
            schema_version: 1,
            citation_correction: None,
            unmet: upper
                .feedback
                .unmet
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let mut entry = entry.clone();
                    entry.reason = reasons()[index % reasons().len()];
                    entry
                })
                .collect(),
        };
        dominates(serialized_sizes(&mixed, &obligations, policy), bound);
    }
}

#[test]
fn feedback_capacity_upper_envelope_rejects_noncanonical_or_foreign_inputs() {
    // AC-0287 / T-0287: an upper envelope cannot silently replace frozen intent.
    let policy = &INVESTIGATION_V2_POLICY;
    let good = envelope(policy, 3);
    for change in 0..5 {
        let mut invalid = good.clone();
        match change {
            0 => invalid.obligations.reverse(),
            1 => invalid.obligations.push(invalid.obligations[0].clone()),
            2 => invalid.obligations.clear(),
            3 => invalid.schema_version = 1,
            _ => match &mut invalid.obligations[0] {
                Obligation::Source { obligation_id, .. }
                | Obligation::Inventory { obligation_id, .. } => *obligation_id = "0".repeat(64),
            },
        }
        assert!(RefusalFeedback::sizing_upper_envelope_with_policy(&invalid, policy).is_err());
    }
    assert!(
        RefusalFeedback::sizing_upper_envelope_with_policy(&good, &INVESTIGATION_V1_POLICY)
            .is_err()
    );
    let mut excess = envelope(policy, policy.max_completion_obligations());
    excess.obligations.push(excess.obligations[0].clone());
    assert!(RefusalFeedback::sizing_upper_envelope_with_policy(&excess, policy).is_err());
    assert_eq!(
        RefusalFeedback::max_bytes_with_policy(&INVESTIGATION_V1_POLICY).unwrap(),
        2048
    );
    assert_eq!(
        RefusalFeedback::max_bytes_with_policy(policy).unwrap(),
        9088
    );
}

// AC-0566: sizing only, through the production CompletionPayload serializer.
// Admitted feedback is already bounded JSON: even hostile escaped values
// cannot outgrow the synthetic envelope at subsequent embedding layers.
#[test]
fn typed_feedback_upper_envelope_dominates_escaped_payload_layers() {
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let bound = RefusalFeedback::max_bytes_with_policy(policy).unwrap();
        let upper = serde_json::to_value(
            RefusalFeedback::sizing_typed_upper_envelope_with_policy(policy).unwrap(),
        )
        .unwrap();
        for pattern in ["plain", "\"\\", "\u{1}\n\r\t", "é漢🦀"] {
            let mut text = String::new();
            while serde_json::to_vec(&text).unwrap().len() + pattern.len() * 6 + 2 <= bound {
                text.push_str(pattern);
            }
            let actual = serde_json::json!({"mapping": text});
            // Include the mapping frame in the existing cap.
            if serde_json::to_vec(&actual).unwrap().len() > bound {
                continue;
            }
            let payload = |feedback: serde_json::Value| CompletionPayload {
                response_schema: None,
                system: "fixed bounded role".into(),
                prompt: serde_json::json!({
                    "completion_metadata": {"feedback": feedback},
                    "unchanged": "quoted \" context"
                })
                .to_string(),
                spans: vec![],
            };
            let mut actual = serde_json::to_vec(&payload(actual)).unwrap();
            let mut ceiling = serde_json::to_vec(&payload(upper.clone())).unwrap();
            for _ in 0..4 {
                assert!(actual.len() <= ceiling.len());
                actual = serde_json::to_vec(&String::from_utf8(actual).unwrap()).unwrap();
                ceiling = serde_json::to_vec(&String::from_utf8(ceiling).unwrap()).unwrap();
            }
        }
    }
}
