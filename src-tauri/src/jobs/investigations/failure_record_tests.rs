use super::*;
use agents::investigation::{
    ActionDecodeFailure, ActionDecodeStage, ActionSemanticRule,
    InvestigationActionDecodeDiagnostic as Stage, InvestigationFailurePhase,
    InvestigationFinishRejection,
};

// AC-0725: an action-decode refusal and a phased preparation refusal persist a
// closed machine-readable cause; the terminal message and status are unchanged
// and no response or query text enters the saved detail.
#[test]
fn classified_stop_persists_text_free_failure_record_and_reopens() {
    let cases = [
        (
            InvestigationStopReason::ActionDecode(ActionDecodeFailure {
                stage: ActionDecodeStage::SemanticRule(ActionSemanticRule::QueryShape),
                error: InvestigationError::InvalidAction,
            }),
            InvestigationTerminalDiagnostic::RejectedAction {
                stage: Stage::QueryShape,
                reason: InvestigationFinishRejection::InvalidAction,
            },
        ),
        (
            InvestigationStopReason::ActionDecode(ActionDecodeFailure {
                stage: ActionDecodeStage::TypedEnvelope,
                error: InvestigationError::SensitiveOutput,
            }),
            InvestigationTerminalDiagnostic::RejectedAction {
                stage: Stage::TypedEnvelope,
                reason: InvestigationFinishRejection::SensitiveOutput,
            },
        ),
        (
            InvestigationStopReason::PreparationAt(PreparationPhase::Query),
            InvestigationTerminalDiagnostic::Preparation {
                phase: InvestigationFailurePhase::Query,
            },
        ),
        (
            InvestigationStopReason::PreparationAt(PreparationPhase::SourceReservation),
            InvestigationTerminalDiagnostic::Preparation {
                phase: InvestigationFailurePhase::SourceReservation,
            },
        ),
    ];
    for (index, (reason, expected)) in cases.into_iter().enumerate() {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start(
            &format!("failure-record-{index}"),
            InvestigationProviderMode::Local,
        );
        fixture.prepare(&id, &execution, false);
        let revision = fixture.store.investigation(&id).unwrap().summary.revision;
        let detail = fixture
            .store
            .advance_investigation(
                &execution,
                revision,
                InvestigationTransition::StopWithElapsed {
                    reason,
                    active_milliseconds: 0,
                },
            )
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        assert_eq!(detail.error.as_deref(), Some(reason.test_message()));
        assert_eq!(detail.terminal_diagnostic, Some(expected));
        let raw = serde_json::to_value(detail.terminal_diagnostic).unwrap();
        let keys: Vec<_> = raw.as_object().unwrap().keys().cloned().collect();
        assert!(keys.iter().any(|key| key == "schema"));
        assert!(
            keys.iter()
                .all(|key| ["schema", "stage", "reason", "phase"].contains(&key.as_str()))
        );
        let saved = fixture.store.investigation(&id).unwrap();
        assert_eq!(saved.terminal_diagnostic, Some(expected));
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&id).unwrap(), saved);
    }
}

// AC-0725: unclassified causes keep their historical omitted diagnostic.
#[test]
fn unclassified_stop_keeps_failure_record_omitted() {
    for reason in [
        InvestigationStopReason::InvalidResponse,
        InvestigationStopReason::PreparationFailed,
        InvestigationStopReason::InvalidNextAction(InvestigationError::InvalidAction),
    ] {
        let mut fixture = Fixture::new();
        let (id, execution) =
            fixture.start("failure-record-legacy", InvestigationProviderMode::Local);
        fixture.prepare(&id, &execution, false);
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let mut task = load_task(&tx, &id).unwrap();
        super::transitions::stop(&tx, &mut task, reason).unwrap();
        assert_eq!(task.detail.terminal_diagnostic, None);
        assert_eq!(task.detail.error.as_deref(), Some(reason.test_message()));
        tx.rollback().unwrap();
    }
}

// AC-1403: a stop caused by a rejected response keeps a bounded redacted
// excerpt in its own immutable row; a sensitive or replayed response keeps
// only its size and hash, and any other cause keeps nothing.
#[test]
fn rejected_response_stop_keeps_bounded_redacted_record() {
    use agents::investigation::rejected_response::{ExcerptRetention, WithheldReason};
    let response = "{\"type\":\"query_context\",\"token\":\"AKIA0000000000000000\"}";
    let cases = [
        (
            InvestigationStopReason::ActionDecode(ActionDecodeFailure {
                stage: ActionDecodeStage::SemanticRule(ActionSemanticRule::QueryShape),
                error: InvestigationError::InvalidAction,
            }),
            Some(None),
        ),
        (InvestigationStopReason::InvalidResponse, Some(None)),
        (
            InvestigationStopReason::ActionDecode(ActionDecodeFailure {
                stage: ActionDecodeStage::TypedEnvelope,
                error: InvestigationError::SensitiveOutput,
            }),
            Some(Some(WithheldReason::SensitiveOutput)),
        ),
        (
            InvestigationStopReason::PreparationAt(PreparationPhase::Query),
            None,
        ),
    ];
    for (index, (reason, expected)) in cases.into_iter().enumerate() {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start(
            &format!("rejected-response-{index}"),
            InvestigationProviderMode::Local,
        );
        fixture.prepare(&id, &execution, false);
        let revision = fixture.store.investigation(&id).unwrap().summary.revision;
        let detail = fixture
            .store
            .advance_investigation(
                &execution,
                revision,
                InvestigationTransition::StopRejected {
                    reason,
                    active_milliseconds: 0,
                    rejected: RejectedCompletion {
                        step_id: "step-1".into(),
                        text: response.into(),
                        action: None,
                    },
                },
            )
            .unwrap();
        // The terminal outcome is the one a plain stop records.
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        assert_eq!(detail.error.as_deref(), Some(reason.test_message()));
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        let kept = reopened.investigation_rejected_response(&id).unwrap();
        let Some(withheld) = expected else {
            assert_eq!(kept, None);
            continue;
        };
        let kept = kept.unwrap();
        assert_eq!(kept.step_id, "step-1");
        assert_eq!(
            kept.response_hash,
            core_prov::content_hash(response.as_bytes())
        );
        match (withheld, kept.retention) {
            (Some(reason), retention) => {
                assert_eq!(retention, ExcerptRetention::Withheld { reason })
            }
            (
                None,
                ExcerptRetention::Excerpt {
                    text,
                    truncated,
                    redactions,
                },
            ) => {
                assert!(text.starts_with("{\"type\":\"query_context\""));
                assert!(!text.contains("AKIA0000000000000000"));
                assert!(!truncated);
                assert_eq!(redactions, 1);
            }
            (None, retention) => panic!("expected an excerpt, got {retention:?}"),
        }
    }
}

// AC-1403: a record that cannot be stored never vetoes the stop.
#[test]
fn unstorable_rejected_response_still_stops() {
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start(
        "rejected-response-unstorable",
        InvestigationProviderMode::Local,
    );
    fixture.prepare(&id, &execution, false);
    fixture
        .store
        .conn
        .execute_batch("CREATE TABLE rejected_response_records (unexpected TEXT)")
        .unwrap();
    let revision = fixture.store.investigation(&id).unwrap().summary.revision;
    let reason = InvestigationStopReason::InvalidResponse;
    let detail = fixture
        .store
        .advance_investigation(
            &execution,
            revision,
            InvestigationTransition::StopRejected {
                reason,
                active_milliseconds: 0,
                rejected: RejectedCompletion {
                    step_id: "step-1".into(),
                    text: "not an action".into(),
                    action: None,
                },
            },
        )
        .unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(detail.error.as_deref(), Some(reason.test_message()));
    let count: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM rejected_response_records",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(fixture.store.investigation(&id).unwrap(), detail);
}

// AC-1405: a host failure after a decoded action keeps the exact canonical
// bytes the worker hashed into the saved action hash, even where a JSON map
// would order the fields differently; a body with a secret keeps only its size
// and hash; a stop before any action was hashed keeps nothing. The terminal
// outcome is a plain stop's in every case.
#[test]
fn host_failure_stop_keeps_canonical_admitted_action() {
    use agents::investigation::admitted_action::{ActionRetention, ActionWithheldReason};
    let response = "{ \"type\": \"query_context\", \"query\": {\"kind\": \"entity\"} }";
    // Typed order: `type` before `query`, unlike a sorted map.
    let typed = "{\"type\":\"query_context\",\"query\":{\"kind\":\"entity\"}}";
    let secret = "{\"type\":\"query_context\",\"labels\":[\"AKIA0000000000000000\"]}";
    let cases = [
        (
            InvestigationStopReason::PreparationAt(PreparationPhase::Query),
            Some(typed),
            None,
        ),
        (
            InvestigationStopReason::HostLimitAt {
                phase: PreparationPhase::Inventory,
                limit: HostLimitKind::Unspecified,
            },
            Some(secret),
            Some(ActionWithheldReason::Sensitive),
        ),
        (
            InvestigationStopReason::PreparationAt(PreparationPhase::Query),
            None,
            None,
        ),
    ];
    for (index, (reason, action, withheld)) in cases.into_iter().enumerate() {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start(
            &format!("admitted-action-{index}"),
            InvestigationProviderMode::Local,
        );
        fixture.prepare(&id, &execution, false);
        let revision = fixture.store.investigation(&id).unwrap().summary.revision;
        let detail = fixture
            .store
            .advance_investigation(
                &execution,
                revision,
                InvestigationTransition::StopRejected {
                    reason,
                    active_milliseconds: 0,
                    rejected: RejectedCompletion {
                        step_id: "step-2".into(),
                        text: response.into(),
                        action: action.map(|bytes| bytes.as_bytes().to_vec()),
                    },
                },
            )
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        assert_eq!(detail.error.as_deref(), Some(reason.test_message()));
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation_rejected_response(&id).unwrap(), None);
        let kept = reopened.investigation_admitted_action(&id).unwrap();
        let Some(canonical) = action else {
            assert_eq!(kept, None);
            continue;
        };
        let kept = kept.unwrap();
        assert_eq!(kept.step_id, "step-2");
        assert_eq!(
            kept.action_hash,
            core_prov::content_hash(canonical.as_bytes())
        );
        match withheld {
            None => assert_eq!(
                kept.retention,
                ActionRetention::Body {
                    json: canonical.into()
                }
            ),
            Some(reason) => assert_eq!(kept.retention, ActionRetention::Withheld { reason }),
        }
    }
}
