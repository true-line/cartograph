//! Pure completion-observation derivation shared by preview and publication.
use super::*;

/// Derive exactly the records commit will persist, without reading or writing a
/// journal. The caller supplies the saved step/response and validated prior
/// count; this computation does not attest that any read actually completed.
/// Commit must reload that authority and rederive on its transaction basis.
///
/// `ledger` may be absent only for a read matching no saved obligation. This
/// preserves the historical incidental-read path, which never fetched a ledger.
pub(in super::super) fn derive_attempt_records(
    task: &StoredTask,
    step_id: &str,
    response: &InvestigationResponseMeta,
    read: &ReadIntent,
    outcome: &AttemptOutcome,
    ledger: Option<&InvestigationInputLedger>,
    completed_count: usize,
) -> Result<Vec<AttemptRecord>, InvestigationStoreError> {
    let policy = saved_policy(task)?;
    // Decode-only until a production firewall supplies a genuine read witness.
    if matches!(outcome, AttemptOutcome::FirewallRefused { .. }) {
        return Err(InvestigationStoreError::Invalid);
    }
    let intent = validated_intent(task)?;
    if response.step_id != step_id || response.action_hash != staged::read_action_hash(read)? {
        return Err(InvestigationStoreError::Invalid);
    }
    if completed_count > policy.max_completed_attempts() {
        return Err(InvestigationStoreError::Capacity);
    }
    let matching = intent
        .obligations
        .obligations
        .iter()
        .filter(|obligation| read.matches(obligation))
        .collect::<Vec<_>>();
    if completed_count
        .checked_add(matching.len())
        .is_none_or(|count| count > policy.max_completed_attempts())
    {
        return Err(InvestigationStoreError::Capacity);
    }
    if matching.is_empty() {
        return Ok(vec![]);
    }
    let ledger = ledger.ok_or(InvestigationStoreError::Invalid)?;
    let input_ledger_hash = ledger.fingerprint_with_policy(&policy)?;
    let id = &task.detail.summary.investigation_id;
    matching
        .into_iter()
        .map(|obligation| {
            let mut record = AttemptRecord {
                schema_version: 1,
                attempt_id: String::new(),
                task_id: id.clone(),
                request_hash: intent.request_hash.clone(),
                obligations_hash: intent.obligations_hash.clone(),
                obligation_id: obligation.key().1.into(),
                step_id: step_id.into(),
                read: read.clone(),
                input_ledger_hash: input_ledger_hash.clone(),
                input_revision: ledger.revision,
                outcome: outcome.clone(),
            };
            record.attempt_id = record.content_id()?;
            record.validate_binding_with_policy(
                &intent.request_hash,
                &intent.obligations,
                id,
                ledger,
                &policy,
            )?;
            Ok(record)
        })
        .collect()
}
