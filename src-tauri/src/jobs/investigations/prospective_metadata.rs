//! Pure projection of ordered completed-attempt IDs and refusal feedback.
use super::*;

/// Build the unchanged protected metadata representation. The caller supplies
/// IDs and refusals in saved order, already filtered by the invocation cutoff.
/// A prospective caller may append pure derived attempt IDs before projecting.
///
/// This validates metadata shape and the task's saved intent. It does not attest
/// actual attempts/refusals, authenticate refusal bodies or establish their
/// ordering. Those remain the authoritative journal loader's responsibilities.
pub(in super::super) fn derive_protected_metadata(
    task: &StoredTask,
    completed_attempt_ids: Vec<String>,
    refusals: &[&RefusalRecord],
) -> Result<ProtectedCompletionMetadata, InvestigationStoreError> {
    let policy = saved_policy(task)?;
    let intent = validated_intent(task)?;
    let last = refusals.last();
    let metadata = ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: intent.request_hash.clone(),
        obligations_hash: intent.obligations_hash.clone(),
        completed_attempt_ids,
        refusal_count: u32::try_from(refusals.len())
            .map_err(|_| InvestigationStoreError::Invalid)?,
        last_refusal_id: last.map(|record| record.refusal_id.clone()),
        feedback: last.map(|record| record.feedback.clone()),
    };
    metadata.validate_with_policy(&policy)?;
    Ok(metadata)
}
