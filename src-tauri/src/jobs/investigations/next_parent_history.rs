//! Internal family-aware saved parent history. Status and optional result are
//! represented exactly; neither implies stronger knowledge or source authority.
use super::*;

pub(crate) struct NextParentHistory {
    history: Vec<String>,
}
impl NextParentHistory {
    pub(crate) fn into_history(self) -> Vec<String> {
        self.history
    }

    #[cfg(test)]
    pub(crate) fn history(&self) -> &[String] {
        &self.history
    }
}

/// Returned history and envelope-construction peak after the family-aware
/// task/result readers return. Their acquisition/validation and schema peaks
/// remain separate; the same retained DTO bounds cover either saved family.
#[cfg(unix)]
pub(crate) fn parent_history_projection_workspace() -> Option<(usize, usize)> {
    use context_hub::logical::{json_bytes, slots, sum};
    let retained = sum(&[16 * 1024, slots::<String>(1)?, slots::<Vec<String>>(1)?])?;
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let (result, _) = next_prompt_result_workspace()?;
    // Borrowed encoding refuses above 16KiB before building a JSON tree.
    // Canonicalization preserves historical key order after that admission.
    let projection = sum(&[
        task,
        result,
        json_bytes(16 * 1024)?.checked_mul(2)?,
        16 * 1024, // admitted input buffer during canonical parsing
        retained,
        16 * 1024, // whole-history re-encoding while the returned string lives
        slots::<Vec<u8>>(1)?,
    ])?;
    Some((retained, projection))
}

/// Family-aware acquisition before parent-envelope construction. The @10
/// reader covers shared row/ledger/result and completion-witness ownership;
/// legacy routing additionally reloads a base task in staged::load, reconstructs
/// metadata from raw attempts/refusals, or sweeps invocation IDs without intent.
#[cfg(unix)]
pub(crate) fn parent_history_acquisition_workspace() -> Option<usize> {
    use context_hub::logical::{slots, sum};
    let (task, decode) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let (_, common) = next_prompt_task_load_workspace()?;
    let metadata = next_prompt_metadata_workspace()?.2;
    let result = next_prompt_result_workspace()?.1;
    let row = metadata_history_workspace::row_workspace()?;
    // Legacy staged::load owns a second task while loading its journal. The
    // base reader does not call completion::validate_saved recursively. Its
    // pending/result checks may reconstruct metadata and read another row.
    let legacy_base = sum(&[
        task,
        decode,
        next_prompt_contract_workspace()?,
        metadata,
        result,
        row,
    ])?;
    let calls = usize::try_from(INVESTIGATION_V2_POLICY.limits().model_invocations).ok()?;
    let keys = calls.checked_add(1)?;
    // validate_legacy_evidence first retains bounded keys, then reads one
    // invocation at a time. Use the admitted row decoder even for malformed
    // steps, not successful disclosure limits. Schema validation is additive.
    let legacy_sweep = sum(&[
        task,
        storage::archive_schema_workspace()?,
        slots::<String>(keys)?,
        slots::<Vec<String>>(1)?,
        keys.checked_mul(128)?,
        row,
        decode,
    ])?;
    sum(&[common, legacy_base, metadata, legacy_sweep])
}

/// Shared by task creation in its writer transaction and by the worker in a
/// readonly snapshot. Public requests use the same saved-family validation.
pub(super) fn load(
    connection: &Connection,
    parent_id: &str,
    conversation_id: &str,
) -> Result<NextParentHistory, InvestigationStoreError> {
    let parent = load_execution_task(connection, parent_id)?;
    if parent.detail.summary.conversation_id != conversation_id {
        return Err(InvestigationStoreError::Invalid);
    }
    let result = load_result(connection, &parent)?;
    // Match the unchanged legacy worker representation and its whole-history
    // caps, including live/failed states with no saved result. No recursive replay.
    let raw = encode_parent_history(parent_id, &parent.detail.summary, &result)?;
    let history = vec![String::from_utf8(raw).map_err(|_| InvestigationStoreError::Invalid)?];
    encode(&history, 16 * 1024)?;
    Ok(NextParentHistory { history })
}
fn encode_parent_history(
    parent_id: &str,
    summary: &InvestigationSummary,
    result: &Option<InvestigationResult>,
) -> Result<Vec<u8>, InvestigationStoreError> {
    #[derive(Serialize)]
    struct Envelope<'a> {
        parent_investigation_id: &'a str,
        original_graph_snapshot_id: &'a Option<String>,
        original_scope: &'a InvestigationScope,
        saved_execution_status: &'a InvestigationStatus,
        cancellation_requested: bool,
        invocation_outcome_pending: bool,
        authority: &'static str,
        saved_result: &'a Option<InvestigationResult>,
    }
    let raw = encode(
        &Envelope {
            parent_investigation_id: parent_id,
            original_graph_snapshot_id: &summary.graph_snapshot_id,
            original_scope: &summary.scope,
            saved_execution_status: &summary.status,
            cancellation_requested: summary.cancel_requested,
            invocation_outcome_pending: summary.invocation_pending,
            authority: "Prior specialist findings remain T3/InferredWeak; execution status does not establish complete knowledge.",
            saved_result: result,
        },
        16 * 1024,
    )?;
    // The previous json! representation sorted every nested object. Direct
    // struct serialization alone would change saved history bytes/fingerprints.
    let canonical: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| InvestigationStoreError::Invalid)?;
    encode(&canonical, 16 * 1024)
}

impl JobStore {
    pub(crate) fn investigation_next_parent_history(
        &self,
        parent_id: &str,
        conversation_id: &str,
    ) -> Result<NextParentHistory, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let history = load(&tx, parent_id, conversation_id)?;
        tx.commit()?;
        Ok(history)
    }
}

#[cfg(test)]
#[path = "next_parent_history_tests.rs"]
mod tests;
