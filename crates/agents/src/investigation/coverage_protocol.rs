//! The @6 read envelope. Legacy action decoding is intentionally unchanged.
use super::*;
use serde::{Deserialize, Serialize};

/// A qualified read or an unchanged source/finish action. Inventory data never
/// becomes a source citation merely because this action was admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualifiedInvestigationAction {
    QueryContext {
        context_id: String,
        query: InvestigationQuery,
    },
    ReadCoverageInventory(InvestigationCoverageRead),
    SourceOrFinish(InvestigationAction),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ReadAction {
    QueryContext {
        context_id: String,
        query: InvestigationQuery,
    },
    ReadCoverageInventory {
        context_id: String,
        repo: String,
        inventory_hash: String,
        max_bytes: usize,
    },
}

impl QualifiedInvestigationAction {
    /// Exact metadata-read identity in an authenticated frozen ledger. This is
    /// not a tool result: callers must separately journal and charge a skipped
    /// action, and must not append a query, inventory read, or citation for it.
    /// A changed byte limit, cursor, context, or query remains a distinct read.
    pub fn repeats_acquired_metadata(&self, ledger: &InvestigationInputLedger) -> bool {
        let Some(coverage) = ledger.coverage.as_ref() else {
            return false;
        };
        match self {
            Self::QueryContext { context_id, query } => {
                context_id == &coverage.context_id
                    && ledger.queries.iter().any(|saved| &saved.query == query)
            }
            Self::ReadCoverageInventory(read) => {
                read.context_id == coverage.context_id
                    && ledger
                        .coverage_reads
                        .iter()
                        .any(|saved| &saved.request == read)
            }
            Self::SourceOrFinish(_) => false,
        }
    }

    /// Admit one strict action against the already prepared qualified identity.
    /// The caller obtains that identity from its validated immutable ledger.
    pub fn decode(raw: &str, expected_context_id: &str) -> Result<Self, InvestigationError> {
        Self::decode_detailed(raw, expected_context_id).map_err(|failure| failure.error)
    }
    pub(super) fn decode_detailed(
        raw: &str,
        expected_context_id: &str,
    ) -> Result<Self, ActionDecodeFailure> {
        use ActionDecodeStage as Stage;
        if !expected_context_id
            .strip_prefix("context-v2:")
            .is_some_and(crate::source_basis::hash)
        {
            return Err(Stage::HostBinding.invalid());
        }
        strict::preflight(raw, MAX_ACTION_BYTES, 16, 4096, 32)
            .map_err(|_| Stage::Preflight.invalid())?;
        let original: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| Stage::TypedEnvelope.invalid())?;
        if !matches!(
            original["type"].as_str(),
            Some("query_context" | "read_coverage_inventory")
        ) {
            return InvestigationAction::decode_detailed(raw).map(Self::SourceOrFinish);
        }
        let action: ReadAction =
            serde_json::from_str(raw).map_err(|_| Stage::TypedCoverageRead.invalid())?;
        if serde_json::to_value(&action).map_err(|_| Stage::CanonicalFields.invalid())? != original
        {
            return Err(Stage::CanonicalFields.invalid());
        }
        match action {
            ReadAction::QueryContext { context_id, query } => {
                if context_id != expected_context_id {
                    return Err(ActionSemanticRule::QueryContext.invalid());
                }
                query
                    .validate()
                    .map_err(|error| ActionSemanticRule::QueryShape.failure(error))?;
                Ok(Self::QueryContext { context_id, query })
            }
            ReadAction::ReadCoverageInventory {
                context_id,
                repo,
                inventory_hash,
                max_bytes,
            } => {
                if context_id != expected_context_id {
                    return Err(ActionSemanticRule::InventoryContext.invalid());
                }
                if !text(&repo, 256)
                    || !crate::source_basis::hash(&inventory_hash)
                    || !(1..=MAX_QUERY_BYTES).contains(&max_bytes)
                {
                    return Err(ActionSemanticRule::InventoryShape.invalid());
                }
                Ok(Self::ReadCoverageInventory(InvestigationCoverageRead {
                    context_id,
                    repo,
                    inventory_hash,
                    max_bytes,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // AC-0239: context identity is mandatory even for the first query, and
    // qualified actions cannot slip through the legacy decoder.
    #[test]
    fn qualified_actions_bind_context_and_preserve_closed_legacy_protocol() {
        let context = format!("context-v2:{}", "a".repeat(64));
        let mut query = json!({"type":"query_context","context_id":context,
            "query":{"scope":{"type":"all"},"kind":null,"labels":[],"max_facts":1,"max_bytes":1024,"cursor":null}});
        assert!(QualifiedInvestigationAction::decode(&query.to_string(), &context).is_ok());
        assert!(InvestigationAction::decode(&query.to_string()).is_err());
        assert!(QualifiedInvestigationAction::decode(&query.to_string(), "different").is_err());
        query.as_object_mut().unwrap().remove("context_id");
        assert!(InvestigationAction::decode(&query.to_string()).is_ok());
        assert!(QualifiedInvestigationAction::decode(&query.to_string(), &context).is_err());
        let mut detail = json!({"type":"read_coverage_inventory","context_id":context,
            "repo":"acme/api","inventory_hash":"b".repeat(64),"max_bytes":1024});
        assert!(QualifiedInvestigationAction::decode(&detail.to_string(), &context).is_ok());
        assert!(InvestigationAction::decode(&detail.to_string()).is_err());
        detail["path"] = "/tmp/replacement".into();
        assert!(QualifiedInvestigationAction::decode(&detail.to_string(), &context).is_err());
    }
}
