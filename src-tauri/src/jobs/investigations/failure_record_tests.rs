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
