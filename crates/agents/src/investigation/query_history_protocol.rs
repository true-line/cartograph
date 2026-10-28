//! @9-only replay decoding. Resolving an identity is pure and spends no budget.
use super::*;
use serde::{Deserialize, Serialize};

/// Closed attribution only; never contains response text or parser diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionDecodeStage {
    Preflight,
    TypedEnvelope,
    CanonicalFields,
    /// Includes nested shape checks; never an exact rejected field or value.
    Semantic,
    HostBinding,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionDecodeFailure {
    pub stage: ActionDecodeStage,
    pub error: InvestigationError,
}
impl ActionDecodeStage {
    pub(super) fn invalid(self) -> ActionDecodeFailure {
        self.failure(InvestigationError::InvalidAction)
    }
    pub(super) fn failure(self, error: InvestigationError) -> ActionDecodeFailure {
        ActionDecodeFailure { stage: self, error }
    }
}
use ActionDecodeStage as Stage;

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
        Self::decode_resolved_detailed(raw, context_id, request_hash, ledger, policy)
            .map_err(|failure| failure.error)
    }
    pub fn decode_resolved_detailed(
        raw: &str,
        context_id: &str,
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<CompletionInvestigationAction, ActionDecodeFailure> {
        if policy.profile() != INVESTIGATION_V2_POLICY.profile()
            || ledger
                .coverage
                .as_ref()
                .is_none_or(|proof| proof.context_id != context_id)
            || !crate::source_basis::hash(request_hash)
        {
            return Err(Stage::HostBinding.invalid());
        }
        strict::preflight(raw, MAX_ACTION_BYTES, 32, 4096, 32)
            .map_err(|error| Stage::Preflight.failure(error))?;
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| Stage::TypedEnvelope.invalid())?;
        if value["type"] != "replay_query" {
            return CompletionInvestigationAction::decode_detailed(raw, context_id);
        }
        let action: ReplayAction =
            serde_json::from_str(raw).map_err(|_| Stage::TypedEnvelope.invalid())?;
        if serde_json::to_value(&action).map_err(|_| Stage::CanonicalFields.invalid())? != value {
            return Err(Stage::CanonicalFields.invalid());
        }
        let ReplayAction::ReplayQuery {
            context_id: actual,
            query_index,
            query_id,
        } = action;
        if actual != context_id {
            return Err(Stage::Semantic.invalid());
        }
        let query = QueryHistoryReference {
            query_index,
            query_id,
        }
        .resolve(request_hash, ledger, policy)
        .map_err(|_| Stage::Semantic.invalid())?;
        Ok(CompletionInvestigationAction::Read(
            QualifiedInvestigationAction::QueryContext {
                context_id: actual,
                query,
            },
        ))
    }
}
