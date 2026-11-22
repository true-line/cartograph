//! Owner-authenticated first-context publication, separate from task creation
//! and calls. Public starts cannot bypass these durable boundaries.
use super::*;

impl JobStore {
    /// Read-only starting metadata for the actual first prompt. The caller keeps
    /// its original request; redacted history cannot reconstruct that authority.
    pub fn investigation_next_prompt_initial_state(
        &self,
        execution: &JobExecution,
        revision: u64,
        request: &StagedObligationRequest,
    ) -> Result<
        (
            InvestigationDetail,
            ProtectedCompletionMetadata,
            Option<InvestigationCompletionContract>,
        ),
        InvestigationStoreError,
    > {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let id = transitions::task_id_for_job(&tx, execution.id())?
            .ok_or(InvestigationStoreError::Missing)?;
        let task = load_next_prompt_task(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        if task.detail.summary.revision != revision || task.phase != Phase::Preparing {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        transitions::check_job(&tx, execution, false)?;
        if request.intent_hash_with_policy(&super::policy_for_detail(&task.detail)?)?
            != task.intent_hash
        {
            return Err(InvestigationStoreError::Conflict);
        }
        if task.ledger_revision.is_some() || task.ledger_hash.is_some() {
            return Err(InvestigationStoreError::Invalid);
        }
        let metadata = completion::derive_protected_metadata(&task, vec![], &[])?;
        let contract = if task.detail.summary.specialist_id.uses_acquisition_choices() {
            Some(
                task.completion_intent
                    .as_ref()
                    .ok_or(InvestigationStoreError::Invalid)?
                    .contract(),
            )
        } else {
            None
        };
        tx.commit()?;
        Ok((task.detail, metadata, contract))
    }

    pub fn admit_next_prompt_initial_context(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        input: &NextPromptInvocation<'_>,
        archive: &InvestigationCoverageArchive,
        validate_retained: impl FnOnce(
            &crate::sources::RetainedSourceView<'_>,
        ) -> Result<(), InvestigationStoreError>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let id = transitions::task_id_for_job(&tx, execution.id())?
            .ok_or(InvestigationStoreError::Missing)?;
        let mut task = load_next_prompt_task_base(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        transitions::check_job(&tx, execution, false)?;
        if task.detail.summary.revision != revision {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        if task.phase != Phase::Preparing
            || task.ledger_revision.is_some()
            || task.ledger_hash.is_some()
            || task.pending.is_some()
            || task.approved
            || task.detail.usage.model_invocations != 0
            || task
                .admission_state
                .as_ref()
                .and_then(|state| state.current_call.as_ref())
                .is_some()
        {
            return Err(InvestigationStoreError::Stale);
        }
        let effective_policy = super::policy_for_detail(&task.detail)?;
        let policy = &effective_policy;
        admission_contract::validate_pairing(&task)?;
        let ledger = input.input.ledger();
        InvestigationTurnBasis {
            input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
            previous_ledger: None,
            completed_tool: None,
        }
        .validate_with_policy(ledger, policy)?;
        archive.validate()?;
        let intent = task
            .completion_intent
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        if ledger.schema_version != 2
            || ledger.coverage.as_ref() != Some(archive.proof())
            || archive.proof().context_id != intent.expected_context
            || task
                .expected_graph_revision
                .as_ref()
                .is_some_and(|expected| expected != &ledger.graph_snapshot_id)
        {
            return Err(InvestigationStoreError::Conflict);
        }
        for table in [
            "investigation_inputs",
            "investigation_invocations",
            "investigation_responses",
        ] {
            let exists: bool = tx.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE task_id=?1)"),
                [&id],
                |row| row.get(0),
            )?;
            if exists {
                return Err(InvestigationStoreError::Conflict);
            }
        }
        if input.active_milliseconds < task.detail.usage.active_milliseconds {
            return Err(InvestigationStoreError::Invalid);
        }
        let mut usage = task.detail.usage.clone();
        usage.active_milliseconds = input.active_milliseconds;
        // These rows are staged only in this transaction. Authentic pre-call
        // verification below needs the exact saved archive+named ledger; every
        // failure, including callback rejection, rolls them and the event back.
        coverage::insert(&tx, &id, archive)?;
        transitions::admit_input(&tx, &mut task, ledger, usage, true)?;
        let (_, call) = next_prompt_invocation::prepare_saved_call(&tx, &mut task, input)?;
        if call.invocation_ordinal != 1
            || call.admission.capacity_refusal_count != 0
            || call.admission.capacity_feedback.is_some()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        validate_retained(&crate::sources::RetainedSourceView::from_transaction(&tx))?;
        // Preparing the first input is not an invocation or cloud approval.
        // Do not retain this prospective call as though it had been charged.
        save_next_prompt_task(&tx, &task, revision, false)?;
        #[cfg(any(test, feature = "test-support"))]
        if FAIL_BEFORE_INITIAL_COMMIT.with(|flag| flag.replace(false)) {
            return Err(InvestigationStoreError::Capacity);
        }
        tx.commit()?;
        #[cfg(any(test, feature = "test-support"))]
        if LOSE_INITIAL_ACK.with(|flag| flag.replace(false)) {
            return Err(InvestigationStoreError::Storage);
        }
        Ok(task.detail)
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_BEFORE_INITIAL_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LOSE_INITIAL_ACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
#[path = "next_prompt_initial_tests.rs"]
mod tests;
