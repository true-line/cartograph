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
