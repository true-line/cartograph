//! Private source settlement validation kernel.
use super::*;

pub(super) fn usage_for(
    saved: &SavedRead,
    kind: DispositionKind,
    active: u64,
) -> Result<InvestigationUsage, InvestigationStoreError> {
    let mut usage = saved.reservation.binding.base_usage.clone();
    if let Some(charge) = &saved.charge {
        usage.captured_validation_bytes = usage
            .captured_validation_bytes
            .checked_add(charge.validation_bytes)
            .ok_or(InvestigationStoreError::Capacity)?;
    }
    if !matches!(
        kind,
        DispositionKind::ReleasedBeforeRead | DispositionKind::OutcomeUnknown
    ) {
        usage.tool_actions = usage
            .tool_actions
            .checked_add(1)
            .ok_or(InvestigationStoreError::Capacity)?;
        usage.evidence_requests = usage
            .evidence_requests
            .checked_add(1)
            .ok_or(InvestigationStoreError::Capacity)?;
    }
    if kind == DispositionKind::Acquired {
        usage.evidence_items = usage
            .evidence_items
            .checked_add(1)
            .ok_or(InvestigationStoreError::Capacity)?;
        usage.evidence_bytes = usage
            .evidence_bytes
            .checked_add(
                usize::try_from(
                    saved.reservation.source.source.byte_end
                        - saved.reservation.source.source.byte_start,
                )
                .map_err(|_| InvestigationStoreError::Capacity)?,
            )
            .ok_or(InvestigationStoreError::Capacity)?;
    }
    usage.active_milliseconds = active;
    validate_usage(&usage, &saved.reservation.policy()?.limits())?;
    Ok(usage)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn validate_continuation(
    connection: &Connection,
    before: &StoredTask,
    saved: &SavedRead,
    base: &InvestigationInputLedger,
    metadata: &ProtectedCompletionMetadata,
    step: &PendingInvestigationStep,
    response: &InvestigationResponseMeta,
    c: &SourceContinuation<'_>,
    kind: DispositionKind,
    reason: Option<&UnavailableReason>,
    active: u64,
) -> Result<InvestigationInputLedger, InvestigationStoreError> {
    let effective_policy = super::policy_for_detail(&before.detail)?;
    let policy = &effective_policy;
    validate_next_ledger(before, saved, base, c.candidate.ledger(), kind, active)?;
    if c.current.ledger() != base {
        return Err(InvestigationStoreError::Invalid);
    }
    let b = &saved.reservation.binding;
    let next_id = format!(
        "{}:step-{}",
        b.task_id,
        b.base_usage
            .model_invocations
            .checked_add(1)
            .ok_or(InvestigationStoreError::Capacity)?
    );
    if !c
        .current_prepared
        .matches_action(&c.current.action(format!("{}:{}", b.task_id, b.step_id))?)
        || c.current_prepared.preview().egress.payload_hash != step.payload_hash
        || c.current
            .seal_disclosure_with_policy(c.current_prepared, policy)?
            != step.disclosure
        || !c
            .candidate_prepared
            .matches_action(&c.candidate.action(next_id.clone())?)
        || c.current_prepared.profile() != c.candidate_prepared.profile()
        || c.current_prepared.limits() != c.candidate_prepared.limits()
        || c.proof.basis_hash() != b.certification_basis_hash
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let sizes = if before
        .detail
        .summary
        .specialist_id
        .uses_exact_source_reservation()
    {
        c.size_binding
            .as_ref()
            .map(|binding| binding.sizes_for(&saved.reservation.source, c.current.ledger()))
            .transpose()?
            .flatten()
    } else {
        None
    };
    if let Some(sizes) = sizes {
        c.proof.verify_measured_with_policy(
            c.current,
            c.current_prepared,
            &saved.reservation.source,
            c.current_prepared.profile(),
            c.current_prepared.limits(),
            &next_id,
            policy,
            sizes,
        )?;
    } else {
        c.proof.verify_with_policy(
            c.current,
            c.current_prepared,
            &saved.reservation.source,
            c.current_prepared.profile(),
            c.current_prepared.limits(),
            &next_id,
            policy,
        )?;
    }

    let ledger = c.candidate.ledger();
    let records = completion::derive_attempt_records(
        before,
        &b.step_id,
        response,
        &b.original_read,
        &if kind == DispositionKind::Acquired {
            AttemptOutcome::SourceAcquired {
                citation_id: ledger
                    .citations
                    .last()
                    .ok_or(InvestigationStoreError::Invalid)?
                    .citation_id
                    .clone(),
            }
        } else {
            AttemptOutcome::Unavailable {
                reason: reason.cloned().ok_or(InvestigationStoreError::Invalid)?,
            }
        },
        Some(ledger),
        metadata.completed_attempt_ids.len(),
    )?;
    let refused = match (c.refusal, kind) {
        (Some(decide), DispositionKind::Acquired) => {
            completion::read_refusals::plan_settlement_refusals(before, &records, ledger, decide)?
        }
        _ => None,
    };
    let disclosure = c
        .candidate
        .seal_disclosure_with_policy(c.candidate_prepared, policy)?
        .ok_or(InvestigationStoreError::Invalid)?;
    // The candidate withholds the new read exactly when the firewall refused
    // it and that refusal is recorded here; nothing else may be withheld anew.
    if kind == DispositionKind::Acquired {
        let citation_id = &ledger
            .citations
            .last()
            .ok_or(InvestigationStoreError::Invalid)?
            .citation_id;
        let withheld = disclosure
            .sources
            .iter()
            .find(|source| &source.citation_id == citation_id)
            .ok_or(InvestigationStoreError::Invalid)?
            .representation
            == agents::investigation::DisclosureRepresentation::FirewallRefused;
        if withheld != refused.is_some() {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    let records = match refused {
        Some(refused) => records
            .into_iter()
            .chain(refused.into_iter().map(|planned| planned.record))
            .collect(),
        None => records,
    };
    if encode(&disclosure, MAX_DISCLOSURE_BYTES)?.len() > c.proof.disclosure_upper_bytes() {
        return Err(InvestigationStoreError::Invalid);
    }
    let next_metadata = continuation_metadata(metadata, &records)?;
    let state = disclosure
        .admission
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    if before
        .detail
        .summary
        .specialist_id
        .uses_acquisition_choices()
    {
        let choices = completion::SavedMetadataHistory::load(connection, before)?
            .prospective_choices(ledger, &next_metadata, &records)?
            .ok_or(InvestigationStoreError::Invalid)?;
        source_snapshot::validate_acquisition(&choices, state, policy)?;
    }
    if policy.local_finish_generation() {
        state.validate_mode_with_metadata(&next_metadata, policy)?;
    } else if state.mode
        != next_metadata.next_call_mode_with_policy(&state.consumed_budgets, policy)?
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if state.consumed_budgets != usage_for(saved, kind, active)?
        || state.capacity_feedback.is_some()
        || state.capacity_refusal_count
            != before
                .admission_state
                .as_ref()
                .and_then(|s| s.current_call.as_ref())
                .ok_or(InvestigationStoreError::Invalid)?
                .admission
                .capacity_refusal_count
        || disclosure.completion_metadata_hash.as_deref()
            != Some(next_metadata.fingerprint_with_policy(policy)?.as_str())
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let measured = llm::bounded::measure_bounded_action(
        c.candidate_prepared.profile(),
        &c.candidate.action(next_id)?,
        c.candidate_prepared.limits(),
    )
    .map_err(|_| InvestigationStoreError::Invalid)?;
    let bound = if state.mode == CompletionCallMode::Choice {
        c.proof.continuation().choice
    } else {
        c.proof.continuation().finish_only
    };
    if measured.raw_string_bytes > bound.raw_string_bytes
        || measured.original_payload_bytes > bound.original_payload_bytes
        || measured.redacted_payload_bytes > bound.redacted_payload_bytes
        || measured.original_action_bytes > bound.original_action_bytes
        || measured.egress_fingerprint_bytes > bound.egress_fingerprint_bytes
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(ledger.clone())
}

/// Shared pure post-read ledger validation. A valid ledger is not proof that
/// source access occurred; only the actual adapter may classify its outcome.
pub(super) fn validate_next_ledger(
    before: &StoredTask,
    saved: &SavedRead,
    base: &InvestigationInputLedger,
    ledger: &InvestigationInputLedger,
    kind: DispositionKind,
    active: u64,
) -> Result<(), InvestigationStoreError> {
    let effective_policy = super::policy_for_detail(&before.detail)?;
    let policy = &effective_policy;
    if !matches!(
        kind,
        DispositionKind::Acquired | DispositionKind::ReadUnavailable
    ) {
        return Err(InvestigationStoreError::Invalid);
    }
    if active
        >= before
            .detail
            .limits
            .active_seconds
            .checked_mul(1000)
            .ok_or(InvestigationStoreError::Capacity)?
    {
        return Err(InvestigationStoreError::Capacity);
    }
    if active < before.detail.usage.active_milliseconds
        || before.detail.summary.cancel_requested
        || saved.charge.is_none()
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if kind == DispositionKind::Acquired {
        InvestigationTurnBasis {
            input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
            previous_ledger: Some(base.clone()),
            completed_tool: Some(InvestigationTool::ReadEvidence),
        }
        .validate_with_policy(ledger, policy)?;
        if ledger.receipt_references != base.receipt_references
            || ledger.citations.len() != base.citations.len() + 1
            || !saved.reservation.source.matches(
                ledger
                    .citations
                    .last()
                    .ok_or(InvestigationStoreError::Invalid)?,
            )
        {
            return Err(InvestigationStoreError::Invalid);
        }
    } else if ledger != base {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

pub(super) fn continuation_metadata(
    metadata: &ProtectedCompletionMetadata,
    records: &[AttemptRecord],
) -> Result<ProtectedCompletionMetadata, InvestigationStoreError> {
    let mut next = metadata.clone();
    next.completed_attempt_ids
        .extend(records.iter().map(|record| record.attempt_id.clone()));
    next.validate_with_policy(&INVESTIGATION_V2_POLICY)?;
    Ok(next)
}
