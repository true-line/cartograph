//! Private source settlement envelope kernel.
use super::*;

/// Private production-serialized envelope. No synthetic input/attempt/ledger
/// escapes: only stage bytes and count requirements can be obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::jobs::investigations) struct SettlementEnvelope {
    pub bytes: usize,
    pub events: usize,
    pub attempts: usize,
    pub input_rows: usize,
    pub reference_rows: usize,
}

/// Size every possible settlement before admitting the source reservation.
/// The caller supplies its owner-authenticated prospective Reservation task;
/// its task/response/descriptor binding must be revalidated by admission.
/// Hash placeholders are fixed-width lowercase hex. Unknown source bytes are
/// never needed by these persisted metadata serializers.
pub(in crate::jobs::investigations) fn envelope(
    connection: &Connection,
    reservation_task: &StoredTask,
    reservation: &read_reservations::Reservation,
    response: &InvestigationResponseMeta,
) -> Result<SettlementEnvelope, InvestigationStoreError> {
    reservation.validate()?;
    let pending = reservation_task
        .pending_source_read
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    if pending.reservation_hash != reservation.fingerprint()?
        || pending.step_id != reservation.binding.step_id
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let call = reservation_task
        .admission_state
        .as_ref()
        .and_then(|s| s.current_call.as_ref())
        .ok_or(InvestigationStoreError::Invalid)?;
    let core = admission_contract::load_saved_call_basis_core(connection, reservation_task, call)?;
    if response.step_id != call.step_id
        || response.response_hash != reservation.binding.original_response_hash
        || response.action_hash != reservation.binding.original_action_hash
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let charged = source_validation::project_validation(connection, reservation_task, reservation)?;
    let mut acquired = core.ledger.clone();
    let source = &reservation.source;
    let source_count = acquired
        .citations
        .iter()
        .filter(|c| c.source.is_some())
        .count();
    acquired.revision = acquired
        .revision
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    acquired.citations.push(InvestigationCitation {
        citation_id: format!(
            "evidence-{}",
            source_count
                .checked_add(1)
                .ok_or(InvestigationStoreError::Capacity)?
        ),
        fact: source.fact.clone(),
        fact_digest: source.fact_digest.clone(),
        source: Some(source.source.clone()),
        role: Some(source.role),
        index: Some(source.index),
        text_hash: Some("f".repeat(64)),
        origin: InvestigationEvidenceOrigin::CapturedPrimarySource {
            registered_source_id: source.registered_source_id.clone(),
            receipt_id: source.receipt_id.clone(),
            receipt_inventory_index: source.receipt_inventory_index,
            captured: source.captured.clone(),
            scope: agents::PrimarySourceScope::PrimarySourceOnly,
            input_closure: agents::InputClosureStatus::InputClosureNotEstablished,
        },
    });
    acquired.validate_with_policy(&INVESTIGATION_V2_POLICY)?;
    let mut envelope = SettlementEnvelope {
        bytes: 0,
        events: 0,
        attempts: 0,
        input_rows: 0,
        reference_rows: 0,
    };
    for (before, charge) in [
        (reservation_task, None),
        (&charged.task, Some(&charged.charge)),
    ] {
        let saved = SavedRead {
            reservation: reservation.clone(),
            charge: charge.cloned(),
            disposition: None,
        };
        let mut alternatives = vec![
            (DispositionKind::ReleasedBeforeRead, None, None),
            (DispositionKind::OutcomeUnknown, None, None),
        ];
        if charge.is_some() {
            alternatives.extend([
                (DispositionKind::Acquired, None, Some(acquired.clone())),
                (
                    DispositionKind::ReadUnavailable,
                    Some(UnavailableReason::PinnedTargetAbsent),
                    Some(core.ledger.clone()),
                ),
                (
                    DispositionKind::ReadUnavailable,
                    Some(UnavailableReason::RangeOutOfBounds),
                    Some(core.ledger.clone()),
                ),
                (DispositionKind::KnownReadDiscarded, None, None),
                (DispositionKind::BoundInvariantDefect, None, None),
            ]);
        }
        for (kind, reason, ledger) in alternatives {
            for cancelled in [false, true] {
                if cancelled
                    && matches!(
                        kind,
                        DispositionKind::Acquired | DispositionKind::ReadUnavailable
                    )
                {
                    continue;
                }
                let mut sizing_before = before.clone();
                if cancelled {
                    sizing_before.detail.summary.cancel_requested = true;
                    sizing_before.detail.summary.actions.can_cancel = false;
                }
                // Internal projection only: never wrap this in SettlementPlan.
                let mut sizing = materialize(
                    connection,
                    &sizing_before,
                    &saved,
                    response,
                    &call.step_id,
                    ledger.clone(),
                    kind,
                    reason.clone(),
                    u64::MAX,
                )?;
                // One cancellation/owner-loss barrier can advance the revision.
                // Reserve full SQL integer widths and the pinned journal ceiling,
                // not an optimistic next sequence's decimal width.
                sizing.task.detail.summary.revision = i64::MAX as u64;
                sizing.task.detail.summary.last_event_sequence =
                    INVESTIGATION_V2_POLICY.journal_events();
                for event in &mut sizing.events {
                    event.revision = i64::MAX as u64;
                    event.sequence = INVESTIGATION_V2_POLICY.journal_events();
                }
                for entry in &mut sizing.attempts {
                    completion::size_attempt_event_sequence(
                        entry,
                        INVESTIGATION_V2_POLICY.journal_events(),
                    );
                }
                sizing.additional_bytes = logical_delta(&sizing_before, &sizing)?;
                envelope.bytes = envelope.bytes.max(sizing.additional_bytes);
                envelope.events = envelope.events.max(sizing.counts.events as usize);
                envelope.attempts = envelope.attempts.max(sizing.counts.attempts);
                envelope.input_rows = envelope.input_rows.max(sizing.counts.input_rows);
                envelope.reference_rows = envelope.reference_rows.max(sizing.counts.reference_rows);
            }
        }
    }
    let prior = completion::attempts(connection, reservation_task)?.len();
    if prior
        .checked_add(envelope.attempts)
        .is_none_or(|n| n > INVESTIGATION_V2_POLICY.max_completed_attempts())
        || reservation_task
            .detail
            .summary
            .last_event_sequence
            .checked_add(1 + envelope.events as u64)
            .is_none_or(|n| n > INVESTIGATION_V2_POLICY.journal_events() - 3)
    {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(envelope)
}
