//! Internal family-aware saved parent history. Status and optional result are
//! represented exactly; neither implies stronger knowledge or source authority.
use super::*;

pub(crate) struct NextParentHistory {
    history: Vec<String>,
}
impl NextParentHistory {
    pub(crate) fn history(&self) -> &[String] {
        &self.history
    }
}

/// Called by the internal factory in its existing writer transaction and by
/// the internal worker in a readonly snapshot. All public family gates stay closed.
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
    let raw = encode(
        &serde_json::json!({
            "parent_investigation_id":parent_id,
            "original_graph_snapshot_id":parent.detail.summary.graph_snapshot_id,
            "original_scope":parent.detail.summary.scope,
            "saved_execution_status":parent.detail.summary.status,
            "cancellation_requested":parent.detail.summary.cancel_requested,
            "invocation_outcome_pending":parent.detail.summary.invocation_pending,
            "authority":"Prior specialist findings remain T3/InferredWeak; execution status does not establish complete knowledge.",
            "saved_result":result,
        }),
        16 * 1024,
    )?;
    let history = vec![String::from_utf8(raw).map_err(|_| InvestigationStoreError::Invalid)?];
    encode(&history, 16 * 1024)?;
    Ok(NextParentHistory { history })
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
