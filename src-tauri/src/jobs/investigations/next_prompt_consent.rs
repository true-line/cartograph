//! Exact-preview user decisions used by public command orchestration. Approval
//! records consent; the worker separately authorizes and consumes the exact call.
use super::*;

impl JobStore {
    pub fn approve_next_prompt_investigation(
        &mut self,
        id: &str,
        revision: u64,
        step_id: &str,
        payload_hash: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.next_prompt_consent_decision(id, revision, step_id, payload_hash, true)
    }

    pub fn decline_next_prompt_investigation(
        &mut self,
        id: &str,
        revision: u64,
        step_id: &str,
        payload_hash: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.next_prompt_consent_decision(id, revision, step_id, payload_hash, false)
    }

    fn next_prompt_consent_decision(
        &mut self,
        id: &str,
        revision: u64,
        step_id: &str,
        payload_hash: &str,
        approved: bool,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        if !id_valid(id) || !id_valid(step_id) || !hash_valid(payload_hash) {
            return Err(InvestigationStoreError::Invalid);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let mut task = load_next_prompt_task_base(&tx, id)?;
        if task.detail.summary.revision != revision
            || task.phase != Phase::Consent
            || task.approved
            || task.detail.provider.mode != InvestigationProviderMode::Cloud
            || task
                .pending
                .as_ref()
                .is_none_or(|step| step.step_id != step_id || step.payload_hash != payload_hash)
        {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        let call = task
            .admission_state
            .as_ref()
            .and_then(|state| state.current_call.as_ref())
            .ok_or(InvestigationStoreError::Invalid)?;
        call.validate_pending_consent(&tx, &task)?;
        completion::validate_saved(&tx, &task)?;
        let pending = pending_event(&tx, id, task.detail.summary.last_event_sequence)?;
        if pending.kind != InvestigationEventKind::ConsentRequired
            || pending.investigation_id != id
            || pending.revision != revision
            || pending.sequence != task.detail.summary.last_event_sequence
            || pending.step_id.as_deref() != Some(step_id)
            || pending.tool.is_some()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        if approved {
            task.approved = true;
            bump(&tx, &mut task)?;
            event(
                &tx,
                &mut task,
                InvestigationEventKind::ConsentApproved,
                Some(step_id),
                None,
                "Exact next-family step approved; a separate authorization and invocation boundary remain required.",
            )?;
        } else {
            // This prospective stamp has no immutable invocation and no debit.
            // Dropping it is essential: terminal reopen must not reinterpret it
            // as a started call or clear any prior measured provider usage.
            task.admission_state
                .as_mut()
                .ok_or(InvestigationStoreError::Invalid)?
                .current_call = None;
            transitions::stop(&tx, &mut task, InvestigationStopReason::ConsentDeclined)?;
        }
        save_next_prompt_task(&tx, &task, revision, !approved)?;
        #[cfg(any(test, feature = "test-support"))]
        if FAIL_BEFORE_CONSENT_COMMIT.with(|flag| flag.replace(false)) {
            return Err(InvestigationStoreError::Capacity);
        }
        tx.commit()?;
        #[cfg(any(test, feature = "test-support"))]
        if LOSE_CONSENT_ACK.with(|flag| flag.replace(false)) {
            return Err(InvestigationStoreError::Storage);
        }
        Ok(task.detail)
    }
}

fn pending_event(
    connection: &Connection,
    id: &str,
    sequence: u64,
) -> Result<InvestigationEvent, InvestigationStoreError> {
    let row: Option<(Option<Vec<u8>>, Option<String>)> = connection.query_row(
        "SELECT CASE WHEN typeof(body)='blob' AND length(body) BETWEEN 1 AND 16384 THEN body END, CASE WHEN typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64 THEN body_hash END FROM investigation_events WHERE task_id=?1 AND sequence=?2",
        params![id,signed(sequence)?], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
    let (Some(body), Some(hash)) = row.ok_or(InvestigationStoreError::Missing)? else {
        return Err(InvestigationStoreError::Invalid);
    };
    decode(&body, &hash)
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_BEFORE_CONSENT_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LOSE_CONSENT_ACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
#[path = "next_prompt_consent_tests.rs"]
mod tests;
