//! Reopen authentication for a host-admitted partial projection.
use super::*;

pub(super) fn validate(
    connection: &Connection,
    task: &StoredTask,
    result: &InvestigationResult,
) -> Result<(), InvestigationStoreError> {
    let Some(witness) = result
        .completion
        .as_ref()
        .and_then(|c| c.withholding.as_ref())
    else {
        return Ok(());
    };
    if task.detail.summary.specialist_id != SpecialistId::DomainAnalystV25 {
        return Err(InvestigationStoreError::Invalid);
    }
    witness.validate_shape()?;
    let id = &task.detail.summary.investigation_id;
    let step = result
        .invocation_step_id
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let response: InvestigationResponseMeta =
        read_body(connection, "investigation_responses", "step_id", id, step)?
            .ok_or(InvestigationStoreError::Missing)?;
    // This host-only slot uses a distinct hash domain for partial projections.
    // Never accept a digest copied from the result as an independent response.
    if response.step_id != *step
        || response.response_hash != witness.original_response_hash
        || response.finish_feedback_witness.as_deref() != Some(witness.fingerprint()?.as_str())
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let entries = completion::raw_refusals(connection, task)?;
    let [prior] = entries.as_slice() else {
        return Err(InvestigationStoreError::Invalid);
    };
    if prior.record.refusal_count != 1
        || prior.record.previous_refusal.is_some()
        || prior.record.task_id != *id
        || prior.record.step_id == *step
        || prior.record.request_hash != witness.request_hash
        || prior.record.obligations_hash != witness.obligations_hash
        || prior.record.feedback_witness_hash()? != witness.prior_correction_witness_hash
    {
        return Err(InvestigationStoreError::Invalid);
    }
    // Reload immutable response and refusal rows and authenticate their actual
    // saved call and timeline. The result's self-described history is insufficient.
    admission_journal::AuthenticatedRefusalBasis::load(connection, task, 1, prior)?
        .ok_or(InvestigationStoreError::Invalid)?;
    Ok(())
}
