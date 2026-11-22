//! Coherent durable ordering for V4. No continuation relies on process memory.
use super::*;

impl JobStore {
    pub fn existing_investigation(
        &self,
        request: &StartInvestigationRequest,
    ) -> Result<Option<InvestigationDetail>, InvestigationStoreError> {
        self.existing_investigation_access(request, None)
    }

    pub fn existing_investigation_access(
        &self,
        request: &StartInvestigationRequest,
        session: Option<&McpTaskSession>,
    ) -> Result<Option<InvestigationDetail>, InvestigationStoreError> {
        mcp_start::with_identity(
            session,
            || Ok(request.intent_hash()?),
            |identity| match identity {
                Some(identity) => mcp_start::existing(self, &request.request_nonce, identity),
                None => self.existing_app_investigation(request),
            },
        )
    }

    fn existing_app_investigation(
        &self,
        request: &StartInvestigationRequest,
    ) -> Result<Option<InvestigationDetail>, InvestigationStoreError> {
        let hash = request.intent_hash()?;
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let id:Option<Option<String>>=tx.query_row("SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE request_namespace='app' AND nonce=?1",[&request.request_nonce],|r|r.get(0)).optional()?;
        let detail = if let Some(id) = id {
            let task = load_task(&tx, &id.ok_or(InvestigationStoreError::Invalid)?)?;
            if task.intent_hash != hash {
                return Err(InvestigationStoreError::Conflict);
            }
            Some(task.detail)
        } else {
            None
        };
        tx.commit()?;
        Ok(detail)
    }

    pub fn investigation_turn_basis(
        &self,
        id: &str,
    ) -> Result<InvestigationTurnBasis, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_task(&tx, id)?;
        let basis = basis(&tx, &task)?;
        tx.commit()?;
        Ok(basis)
    }
}

/// Derive the existing turn basis inside the caller's saved-task read snapshot.
/// This validates durable ordering; it grants no execution ownership or latest
/// graph claim and performs no writes.
pub(super) fn basis(
    connection: &Connection,
    task: &StoredTask,
) -> Result<InvestigationTurnBasis, InvestigationStoreError> {
    let id = &task.detail.summary.investigation_id;
    let policy = super::policy_for_detail(&task.detail)?;
    let current = load_ledger(connection, task)?.ok_or(InvestigationStoreError::Invalid)?;
    let previous_ledger = if let Some(revision) = current.revision.checked_sub(1) {
        Some(
            read_body::<InvestigationInputLedger>(
                connection,
                "investigation_inputs",
                "revision",
                id,
                &signed(revision)?,
            )?
            .ok_or(InvestigationStoreError::Invalid)?,
        )
    } else {
        None
    };
    let completed_tool = completed_tool(connection, task, current.revision)?;
    let basis = InvestigationTurnBasis {
        input_ledger_hash: current.fingerprint_with_policy(&policy)?,
        previous_ledger,
        completed_tool,
    };
    basis.validate_with_policy(&current, &policy)?;
    Ok(basis)
}

fn completed_tool(
    connection: &Connection,
    task: &StoredTask,
    input_revision: u64,
) -> Result<Option<InvestigationTool>, InvestigationStoreError> {
    let id = &task.detail.summary.investigation_id;
    let event_limit = journal_event_limit(task)?;
    let query_limit = event_limit
        .checked_add(1)
        .ok_or(InvestigationStoreError::Invalid)?;
    let sequences = connection
        .prepare(
            "SELECT sequence FROM investigation_events WHERE task_id=?1 ORDER BY sequence LIMIT ?2",
        )?
        .query_map(params![id, signed(query_limit)?], |row| {
            row.get::<_, i64>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if sequences.len() as u64 != task.detail.summary.last_event_sequence
        || sequences.len() as u64 > event_limit
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut completed = 0u64;
    let mut tool = None;
    for (index, sequence) in sequences.into_iter().enumerate() {
        let event: InvestigationEvent = read_body(
            connection,
            "investigation_events",
            "sequence",
            id,
            &sequence,
        )?
        .ok_or(InvestigationStoreError::Invalid)?;
        if sequence != index as i64 + 1
            || event.sequence != sequence as u64
            || event.investigation_id != *id
            || event.revision > task.detail.summary.revision
        {
            return Err(InvestigationStoreError::Invalid);
        }
        super::query_refusal::validate_event(connection, task, &event)?;
        match event.kind {
            InvestigationEventKind::ContextPrepared if completed == 0 && event.tool.is_none() => {
                completed = 1;
            }
            InvestigationEventKind::ToolCompleted if completed > 0 && event.tool.is_some() => {
                tool = event.tool;
                completed += 1;
            }
            InvestigationEventKind::ContextPrepared | InvestigationEventKind::ToolCompleted => {
                return Err(InvestigationStoreError::Invalid);
            }
            _ => (),
        }
    }
    if input_revision.checked_add(1) != Some(completed) {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(tool)
}
