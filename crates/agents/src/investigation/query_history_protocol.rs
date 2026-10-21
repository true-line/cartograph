//! @9-only replay decoding. Resolving an identity is pure and spends no budget.
use super::*;
use serde::{Deserialize, Serialize};

pub struct ProgressiveInvestigationAction;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ReplayAction {
    ReplayQuery {
        context_id: String,
        query_index: usize,
        query_id: String,
    },
}

impl ProgressiveInvestigationAction {
    pub fn decode_resolved(
        raw: &str,
        context_id: &str,
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<CompletionInvestigationAction, InvestigationError> {
        if policy.profile() != INVESTIGATION_V2_POLICY.profile()
            || ledger
                .coverage
                .as_ref()
                .is_none_or(|proof| proof.context_id != context_id)
            || !crate::source_basis::hash(request_hash)
        {
            return Err(InvestigationError::InvalidAction);
        }
        strict::preflight(raw, MAX_ACTION_BYTES, 32, 4096, 32)?;
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidAction)?;
        if value["type"] != "replay_query" {
            return CompletionInvestigationAction::decode(raw, context_id);
        }
        let action: ReplayAction =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidAction)?;
        if serde_json::to_value(&action).map_err(|_| InvestigationError::InvalidAction)? != value {
            return Err(InvestigationError::InvalidAction);
        }
        let ReplayAction::ReplayQuery {
            context_id: actual,
            query_index,
            query_id,
        } = action;
        if actual != context_id {
            return Err(InvestigationError::InvalidAction);
        }
        let query = QueryHistoryReference {
            query_index,
            query_id,
        }
        .resolve(request_hash, ledger, policy)
        .map_err(|_| InvestigationError::InvalidAction)?;
        Ok(CompletionInvestigationAction::Read(
            QualifiedInvestigationAction::QueryContext {
                context_id: actual,
                query,
            },
        ))
    }
}
