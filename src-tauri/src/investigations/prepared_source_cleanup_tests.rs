//! AC-0287: real reservation/charge/owner cleanup, preserving truthful access
//! stage. Recovery never repeats a source read or refunds observed work.
use super::*;
use crate::investigations::evidence::prepared_source::publication::cleanup::SourceCleanupError;
use crate::jobs::investigations::CommittedSourceReservation;

fn reserved(
    saved: &CapturedSourceInvocationFixture,
    state: &AppState,
    context: &FrozenContext,
    target: &InvestigationInspectionTarget,
) -> (PreparedCapturedSource, CommittedSourceReservation) {
    let prepared = context.prepare_captured_source(state, target).unwrap();
    let reservation = prepared
        .reserve(
            context,
            state,
            saved.execution(),
            saved.revision(),
            &clock(),
            &saved.admission(),
        )
        .unwrap();
    (prepared, reservation)
}
#[test]
fn prepared_source_cleanup_before_access_actual_cancel_and_unknown_charge_ack_preserve_debits() {
    for case in [
        "cancel_before_charge",
        "cancel_after_charge",
        "unknown_charge_ack",
    ] {
        let (mut saved, state, context, target, _) = fixture();
        reset_reads();
        let base = context.ledger.clone();
        let (prepared, reservation) = reserved(&saved, &state, &context, &target);
        if case == "cancel_before_charge" {
            saved.cancel();
        }
        if case == "cancel_after_charge" {
            AFTER_CHARGE
                .with(|slot| *slot.borrow_mut() = Some(Box::new(saved.cancel_after_charge())));
        }
        if case == "unknown_charge_ack" {
            saved.lose_charge_ack();
        }
        let failure =
            match prepared.read_reserved(&context, &state, saved.execution(), &reservation.basis())
            {
                Err(failure) => failure,
                Ok(_) => panic!("fault accessed source"),
            };
        assert_eq!(failure.stage(), SourceAccessStage::BeforeAccess);
        assert_eq!(read_count(), 0);
        let detail =
            match failure.settle_terminal(&state, saved.execution(), &reservation, &clock()) {
                Ok(detail) => detail,
                Err(error) => panic!("cleanup failed: {}", error.error()),
            };
        assert_eq!(
            detail.usage.captured_validation_bytes,
            if case == "cancel_before_charge" {
                0
            } else {
                target.captured.file.byte_len
            }
        );
        assert_eq!(
            detail.usage.tool_actions,
            reservation.detail().usage.tool_actions
        );
        assert_eq!(
            detail.usage.evidence_requests,
            reservation.detail().usage.evidence_requests
        );
        assert_eq!(detail.usage.evidence_items, 0);
        // AC-1609: a read released before access keeps no discard record.
        assert_eq!(detail.terminal_diagnostic, None);
        assert_eq!(context.ledger, base);
        assert!(context.evidence.is_empty());
        assert_eq!(read_count(), 0);
        assert_eq!(saved.publication_after_reopen(), (base.revision, 0, 0));
    }
}
#[test]
fn prepared_source_cleanup_accessed_cancelled_publication_discards_without_acquisition() {
    let (mut saved, state, mut context, target, _) = fixture();
    reset_reads();
    let base = context.ledger.clone();
    let (prepared, reservation) = reserved(&saved, &state, &context, &target);
    let candidate =
        match prepared.read_reserved(&context, &state, saved.execution(), &reservation.basis()) {
            Ok(candidate) => candidate,
            Err(error) => panic!("read failed: {:?}", error.kind()),
        };
    saved.cancel();
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let provider = provider();
    let failure = match candidate.publish_acquired(
        &mut context,
        &state,
        saved.execution(),
        &clock(),
        &publication(&saved, &firewall, &provider),
    ) {
        Err(failure) => failure,
        Ok(_) => panic!("cancelled read published"),
    };
    let detail = match failure.settle_terminal(&state, saved.execution(), &reservation, &clock()) {
        Ok(detail) => detail,
        Err(error) => panic!("known-read cleanup failed: {}", error.error()),
    };
    // AC-1609: the run keeps the closed step and class that discarded the read:
    // cancellation advanced the revision the snapshot expected.
    assert_eq!(
        detail.terminal_diagnostic,
        Some(InvestigationTerminalDiagnostic::SourceReadDiscarded {
            step: agents::investigation::SourceDiscardStep::PublishSnapshot,
            error_class: agents::investigation::SourceDiscardClass::StoreStale,
        })
    );
    assert_eq!(saved.disposition_after_reopen(), "\"known_read_discarded\"");
    assert_eq!(
        detail.usage.tool_actions,
        reservation.detail().usage.tool_actions + 1
    );
    assert_eq!(
        detail.usage.evidence_requests,
        reservation.detail().usage.evidence_requests + 1
    );
    assert_eq!(
        detail.usage.captured_validation_bytes,
        target.captured.file.byte_len
    );
    assert_eq!(detail.usage.evidence_items, 0);
    assert_eq!(context.ledger, base);
    assert!(context.evidence.is_empty());
    assert_eq!(read_count(), 1);
    assert_eq!(saved.publication_after_reopen(), (base.revision, 0, 0));
}
#[test]
fn prepared_source_cleanup_unknown_publication_ack_cannot_erase_acquired_ready_state() {
    let (saved, state, mut context, target, _) = fixture();
    reset_reads();
    let base = context.ledger.clone();
    let (prepared, reservation) = reserved(&saved, &state, &context, &target);
    let candidate =
        match prepared.read_reserved(&context, &state, saved.execution(), &reservation.basis()) {
            Ok(candidate) => candidate,
            Err(error) => panic!("read failed: {:?}", error.kind()),
        };
    let next = candidate.ledger().revision;
    crate::jobs::investigations::fail_next_source_publication(true);
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let provider = provider();
    let failure = match candidate.publish_acquired(
        &mut context,
        &state,
        saved.execution(),
        &clock(),
        &publication(&saved, &firewall, &provider),
    ) {
        Err(failure) => failure,
        Ok(_) => panic!("unknown ACK succeeded"),
    };
    let failure = match failure.settle_terminal(&state, saved.execution(), &reservation, &clock()) {
        Err(failure) => failure,
        Ok(_) => panic!("cleanup erased committed acquisition"),
    };
    assert!(matches!(
        failure.error(),
        SourceCleanupError::Store(InvestigationStoreError::Stale)
    ));
    assert_eq!(failure.stage(), SourceAccessStage::AccessStarted);
    assert!(failure.validation_receipt().is_some());
    assert_eq!(saved.publication_after_reopen(), (next, 1, 1));
    assert_eq!(context.ledger, base);
    assert!(context.evidence.is_empty());
    assert_eq!(read_count(), 1);
}
#[test]
fn prepared_source_cleanup_rejects_foreign_reservation_without_touching_either_task() {
    let (saved, state, context, target, _) = fixture();
    let (other, other_state, other_context, other_target, _) = fixture();
    let (prepared, reservation) = reserved(&saved, &state, &context, &target);
    let (_other_prepared, foreign) = reserved(&other, &other_state, &other_context, &other_target);
    reset_reads();
    saved.lose_charge_ack();
    let failure =
        match prepared.read_reserved(&context, &state, saved.execution(), &reservation.basis()) {
            Err(failure) => failure,
            Ok(_) => panic!("unknown ACK read source"),
        };
    let failure = match failure.settle_terminal(&state, saved.execution(), &foreign, &clock()) {
        Err(failure) => failure,
        Ok(_) => panic!("foreign cleanup succeeded"),
    };
    assert!(matches!(
        failure.error(),
        SourceCleanupError::Host(HostError::InvalidInput)
    ));
    assert_eq!(failure.stage(), SourceAccessStage::BeforeAccess);
    assert_eq!(read_count(), 0);
    assert_eq!(saved.counts(), (1, 1, target.captured.file.byte_len));
    assert_eq!(other.counts(), (1, 0, 0));
}

#[test]
fn prepared_source_cleanup_rejects_same_descriptor_reservation_from_another_task() {
    let (saved, state, context, target, _) = fixture();
    let mut other = CapturedSourceInvocationFixture::new(|_| {
        crate::jobs::investigations::SourceValidationInitialData {
            ledger: context.ledger.clone(),
            query_pages: context.query_pages.clone(),
            archive: context.coverage_archive.clone().unwrap(),
            target: target.clone(),
        }
    });
    // A second real native owner transaction uses a scripted retention callback
    // solely to supply a foreign binding. It grants no actual host read access.
    let foreign = other.reserve_for_binding_test();
    let (prepared, reservation) = reserved(&saved, &state, &context, &target);
    assert!(
        foreign.matches(
            &target,
            &context
                .ledger
                .fingerprint_with_policy(&context.policy)
                .unwrap()
        )
    );
    assert_ne!(foreign.basis().task_id, reservation.basis().task_id);
    reset_reads();
    saved.lose_charge_ack();
    let failure =
        match prepared.read_reserved(&context, &state, saved.execution(), &reservation.basis()) {
            Err(failure) => failure,
            Ok(_) => panic!("unknown ACK read source"),
        };
    let failure = match failure.settle_terminal(&state, saved.execution(), &foreign, &clock()) {
        Err(failure) => failure,
        Ok(_) => panic!("same-context foreign reservation settled"),
    };
    assert!(matches!(
        failure.error(),
        SourceCleanupError::Host(HostError::InvalidInput)
    ));
    assert_eq!(failure.stage(), SourceAccessStage::BeforeAccess);
    assert_eq!(read_count(), 0);
    assert_eq!(saved.counts(), (1, 1, target.captured.file.byte_len));
    assert_eq!(other.counts(), (1, 0, 0));
}

#[test]
fn prepared_source_cleanup_actual_protected_seal_defect_records_bound_invariant_without_replay() {
    let (saved, state, mut context, target, _) = fixture();
    reset_reads();
    let base = context.ledger.clone();
    let (prepared, reservation) = reserved(&saved, &state, &context, &target);
    let candidate =
        match prepared.read_reserved(&context, &state, saved.execution(), &reservation.basis()) {
            Ok(candidate) => candidate,
            Err(error) => panic!("read failed: {:?}", error.kind()),
        };
    // Inject an internal fault in the actual post-read action after genuine
    // certification/access. Real wire preparation fits; actual sealing rejects
    // the changed protected field. No certificate is synthesized or promoted.
    crate::investigations::evidence::prepared_source::publication::corrupt_next_protected_action();
    let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
    let provider = provider();
    let failure = match candidate.publish_acquired(
        &mut context,
        &state,
        saved.execution(),
        &clock(),
        &publication(&saved, &firewall, &provider),
    ) {
        Err(failure) => failure,
        Ok(_) => panic!("protected seal defect published"),
    };
    assert!(matches!(
        failure.error(),
        SourcePublicationError::BoundInvariant
    ));
    assert_eq!(failure.stage(), SourceAccessStage::AccessStarted);
    let detail = match failure.settle_terminal(&state, saved.execution(), &reservation, &clock()) {
        Ok(detail) => detail,
        Err(error) => panic!("bound-defect cleanup failed: {}", error.error()),
    };
    assert_eq!(
        detail.terminal_diagnostic,
        Some(InvestigationTerminalDiagnostic::SourceReadDiscarded {
            step: agents::investigation::SourceDiscardStep::PublishSeal,
            error_class: agents::investigation::SourceDiscardClass::BoundInvariant,
        })
    );
    assert_eq!(
        saved.disposition_after_reopen(),
        "\"bound_invariant_defect\""
    );
    assert_eq!(
        detail.usage.tool_actions,
        reservation.detail().usage.tool_actions + 1
    );
    assert_eq!(
        detail.usage.captured_validation_bytes,
        target.captured.file.byte_len
    );
    assert_eq!(saved.publication_after_reopen(), (base.revision, 0, 0));
    assert_eq!(context.ledger, base);
    assert!(context.evidence.is_empty());
    assert_eq!(read_count(), 1);
}
