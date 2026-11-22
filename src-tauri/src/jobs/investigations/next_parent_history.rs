//! Internal family-aware saved parent history. Status and optional result are
//! represented exactly; neither implies stronger knowledge or source authority.
use super::*;

pub struct NextParentHistory {
    history: Vec<String>,
}
impl NextParentHistory {
    pub fn into_history(self) -> Vec<String> {
        self.history
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn history(&self) -> &[String] {
        &self.history
    }
}

/// Returned history and envelope-construction peak after the family-aware
/// task/result readers return. Their acquisition/validation and schema peaks
/// remain separate; the same retained DTO bounds cover either saved family.
#[cfg(unix)]
pub fn parent_history_projection_workspace() -> Option<(usize, usize)> {
    use context_hub::logical::{json_bytes, slots, sum};
    let retained = sum(&[
        agents::investigation::PARENT_HISTORY_32768_V1_BYTES,
        slots::<String>(1)?,
        slots::<Vec<String>>(1)?,
    ])?;
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let (result, _) = next_prompt_result_workspace()?;
    // Borrowed encoding refuses above the selected history ceiling before building a JSON tree.
    // Canonicalization preserves historical key order after that admission.
    let projection = sum(&[
        task,
        result,
        json_bytes(agents::investigation::PARENT_HISTORY_32768_V1_BYTES)?.checked_mul(2)?,
        agents::investigation::PARENT_HISTORY_32768_V1_BYTES, // admitted input buffer during canonical parsing
        retained,
        agents::investigation::PARENT_HISTORY_32768_V1_BYTES, // whole-history re-encoding while the returned string lives
        slots::<Vec<u8>>(1)?,
    ])?;
    Some((retained, projection))
}

/// Family-aware acquisition before parent-envelope construction. The @10
/// reader covers shared row/ledger/result and completion-witness ownership;
/// legacy routing additionally reloads a base task in staged::load, reconstructs
/// metadata from raw attempts/refusals, or sweeps invocation IDs without intent.
#[cfg(unix)]
pub fn parent_history_acquisition_workspace() -> Option<usize> {
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
        legacy_parent_contract_workspace()?,
        legacy_parent_pending_workspace()?.0,
        legacy_parent_pending_workspace()?.1,
        next_prompt_ledger_workspace()?,
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

/// Legacy validate_task_contract(false) shares definition, redaction and
/// obligation validation with @10, but also normalizes the saved standalone
/// inspection targets. The historical-definition regression pins the shared
/// catalog component. Charge raw admitted-row currency for the extra target
/// clones rather than assuming successful semantic limits before validation.
#[cfg(unix)]
fn legacy_parent_contract_workspace() -> Option<usize> {
    use context_hub::logical::{json_bytes, slots, sum};
    sum(&[
        next_prompt_contract_workspace()?,
        json_bytes(MAX_RECORD_BYTES)?.checked_mul(3)?,
        MAX_RECORD_BYTES.checked_mul(2)?,
        slots::<Vec<u8>>(2)?,
    ])
}

/// Pending basis validation keeps its ledger and obligation sources alive
/// across legacy metadata reconstruction. The invoked-row equality check then
/// decodes another complete step while the task still owns its pending step.
#[cfg(unix)]
fn legacy_parent_pending_workspace() -> Option<(usize, usize)> {
    use context_hub::logical::{json_bytes, slots, sum};
    let policy = &INVESTIGATION_V2_POLICY;
    let (ledger, _) = InvestigationInputLedger::decode_workspace_bytes()?;
    let basis = sum(&[
        ledger,
        next_prompt_ledger_workspace()?,
        agents::investigation::disclosure_validation_workspace(policy, MAX_RECORD_BYTES)?,
        json_bytes(policy.envelope_bytes())?.checked_mul(3)?,
        legacy_parent_contract_workspace()?,
        next_prompt_metadata_workspace()?.2,
    ])?;
    let invoked = sum(&[
        metadata_history_workspace::row_workspace()?,
        record_decode_scratch_bytes()?
            .max(json_bytes(MAX_RECORD_BYTES)?.checked_mul(RECORD_MAX_DEPTH.checked_add(1)?)?),
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        128,
        slots::<String>(3)?,
        4096,
    ])?;
    Some((basis, invoked))
}

/// Shared by task creation in its writer transaction and by the worker in a
/// readonly snapshot. Public requests use the same saved-family validation.
pub(super) fn load_with_policy(
    connection: &Connection,
    parent_id: &str,
    conversation_id: &str,
    policy: &InvestigationPolicy,
) -> Result<NextParentHistory, InvestigationStoreError> {
    let parent = load_execution_task(connection, parent_id)?;
    if parent.detail.summary.conversation_id != conversation_id {
        return Err(InvestigationStoreError::Invalid);
    }
    let result = load_result(connection, &parent)?;
    // Match the unchanged legacy worker representation and its whole-history
    // caps, including live/failed states with no saved result. No recursive replay.
    let raw = encode_parent_history_with_limit(
        parent_id,
        &parent.detail.summary,
        &result,
        policy.supplied_history_bytes(),
    )?;
    let history = vec![String::from_utf8(raw).map_err(|_| InvestigationStoreError::Invalid)?];
    encode(&history, policy.supplied_history_bytes())?;
    Ok(NextParentHistory { history })
}
#[cfg(any(test, feature = "test-support"))]
fn encode_parent_history(
    parent_id: &str,
    summary: &InvestigationSummary,
    result: &Option<InvestigationResult>,
) -> Result<Vec<u8>, InvestigationStoreError> {
    encode_parent_history_with_limit(
        parent_id,
        summary,
        result,
        agents::investigation::LEGACY_SUPPLIED_HISTORY_BYTES,
    )
}
fn encode_parent_history_with_limit(
    parent_id: &str,
    summary: &InvestigationSummary,
    result: &Option<InvestigationResult>,
    limit: usize,
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
        limit,
    )?;
    // The previous json! representation sorted every nested object. Direct
    // struct serialization alone would change saved history bytes/fingerprints.
    let canonical: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| InvestigationStoreError::Invalid)?;
    encode(&canonical, limit)
}

impl JobStore {
    #[cfg(any(test, feature = "test-support"))]
    pub fn investigation_next_parent_history(
        &self,
        parent_id: &str,
        conversation_id: &str,
    ) -> Result<NextParentHistory, InvestigationStoreError> {
        self.investigation_next_parent_history_with_policy(
            parent_id,
            conversation_id,
            &INVESTIGATION_V2_POLICY,
        )
    }
    pub fn investigation_next_parent_history_with_policy(
        &self,
        parent_id: &str,
        conversation_id: &str,
        policy: &InvestigationPolicy,
    ) -> Result<NextParentHistory, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let history = load_with_policy(&tx, parent_id, conversation_id, policy)?;
        tx.commit()?;
        Ok(history)
    }
}

#[cfg(any(test, feature = "test-support"))]
#[path = "next_parent_history_tests.rs"]
mod tests;
