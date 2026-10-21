//! Coherent durable ordering for V4. No continuation relies on process memory.
use super::*;

impl JobStore {
    pub(crate) fn existing_investigation(
        &self,
        request: &StartInvestigationRequest,
    ) -> Result<Option<InvestigationDetail>, InvestigationStoreError> {
        let hash = request.intent_hash()?;
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let id:Option<Option<String>>=tx.query_row("SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE nonce=?1",[&request.request_nonce],|r|r.get(0)).optional()?;
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

    pub(crate) fn investigation_turn_basis(
        &self,
        id: &str,
    ) -> Result<InvestigationTurnBasis, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_task(&tx, id)?;
        let current = load_ledger(&tx, &task)?.ok_or(InvestigationStoreError::Invalid)?;
        let previous_ledger = if let Some(revision) = current.revision.checked_sub(1) {
            Some(
                read_body::<InvestigationInputLedger>(
                    &tx,
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
        let completed_tool = completed_tool(&tx, &task, current.revision)?;
        let basis = InvestigationTurnBasis {
            input_ledger_hash: current.fingerprint()?,
            previous_ledger,
            completed_tool,
        };
        basis.validate(&current)?;
        tx.commit()?;
        Ok(basis)
    }
}

fn completed_tool(
    connection: &Connection,
    task: &StoredTask,
    input_revision: u64,
) -> Result<Option<InvestigationTool>, InvestigationStoreError> {
    let id = &task.detail.summary.investigation_id;
    let event_limit = journal_event_limit(task)?;
    let sequences = connection
        .prepare(
            "SELECT sequence FROM investigation_events WHERE task_id=?1 ORDER BY sequence LIMIT ?2",
        )?
        .query_map(params![id, signed(event_limit + 1)?], |row| {
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
