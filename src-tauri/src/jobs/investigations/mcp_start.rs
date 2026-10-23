//! Host admission owns identity derivation and keeps authority through commit.
use super::*;

// None is the trusted owner-App path, never a value accepted from MCP JSON.
pub(super) fn with_identity<T>(
    session: Option<&McpTaskSession>,
    intent: impl FnOnce() -> Result<String, InvestigationStoreError>,
    operation: impl FnOnce(Option<&McpRequestIdentity>) -> Result<T, InvestigationStoreError>,
) -> Result<T, InvestigationStoreError> {
    match session {
        None => operation(None),
        Some(session) => session
            .with_grant(|client, capacity| {
                let identity = McpRequestIdentity {
                    client: client.clone(),
                    normalized_intent_hash: intent()?,
                    maximum_identities: capacity.get(),
                };
                operation(Some(&identity))
            })
            .map_err(|_| InvestigationStoreError::Missing)?,
    }
}

/// Early recovery reads only saved state; no live graph/provider/capacity lookup.
/// The caller keeps with_identity's grant guard through this transaction.
pub(super) fn existing(
    store: &JobStore,
    nonce: &str,
    identity: &McpRequestIdentity,
) -> Result<Option<InvestigationDetail>, InvestigationStoreError> {
    let namespace = InvestigationOrigin::Mcp {
        client_id: identity.client.clone(),
    }
    .request_namespace();
    let tx = store.conn.unchecked_transaction()?;
    validate(&tx, &store.namespace)?;
    let id: Option<Option<String>> = tx.query_row(
        "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE request_namespace=?1 AND nonce=?2",
        params![namespace, nonce], |row| row.get(0),
    ).optional()?;
    let detail = if let Some(id) = id {
        let task = load_execution_task(&tx, &id.ok_or(InvestigationStoreError::Invalid)?)?;
        if task.mcp_intent_hash.as_deref() != Some(identity.normalized_intent_hash.as_str()) {
            return Err(InvestigationStoreError::Conflict);
        }
        Some(task.detail)
    } else {
        None
    };
    tx.commit()?;
    Ok(detail)
}
