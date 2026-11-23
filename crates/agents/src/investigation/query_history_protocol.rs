//! @9-only replay decoding. Resolving an identity is pure and spends no budget.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::*;
use serde::{Deserialize, Serialize};

/// Closed attribution only; never contains response text or parser diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionDecodeStage {
    Preflight,
    TypedEnvelope,
    TypedStagedFinish,
    TypedCoverageRead,
    TypedLegacyAction,
    TypedReplay,
    BareFinish,
    CanonicalFields,
    /// Includes nested shape checks; never an exact rejected field or value.
    Semantic,
    SemanticRule(ActionSemanticRule),
    HostBinding,
}
/// Closed rule families, not rejected fields or values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionSemanticRule {
    QueryContext,
    QueryShape,
    InventoryContext,
    InventoryShape,
    ArtifactContext,
    ArtifactShape,
    ReplayContext,
    ReplayReference,
    EvidenceShape,
    FinishRole,
    FinishShape,
    FinishOverall,
    FindingTextLimits,
    FindingCitations,
    FinishOutcome,
}
impl ActionSemanticRule {
    pub(super) fn failure(self, error: InvestigationError) -> ActionDecodeFailure {
        Stage::SemanticRule(self).failure(error)
    }
    pub(super) fn invalid(self) -> ActionDecodeFailure {
        self.failure(InvestigationError::InvalidAction)
    }
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

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ExactNodeQuery {
    QueryContext {
        context_id: String,
        query: InvestigationQuery,
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
        Self::decode_resolved_detailed_for_specialist(
            raw,
            context_id,
            request_hash,
            ledger,
            policy,
            SpecialistId::DomainAnalystV9,
        )
    }
    pub fn decode_resolved_for_specialist(
        raw: &str,
        context_id: &str,
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
        role: SpecialistId,
    ) -> Result<CompletionInvestigationAction, InvestigationError> {
        Self::decode_resolved_detailed_for_specialist(
            raw,
            context_id,
            request_hash,
            ledger,
            policy,
            role,
        )
        .map_err(|failure| failure.error)
    }
    pub fn decode_resolved_detailed_for_specialist(
        raw: &str,
        context_id: &str,
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
        role: SpecialistId,
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
        if role.uses_exact_node_queries()
            && value["type"] == "query_context"
            && value["query"]["scope"]["type"] == "node"
        {
            let action: ExactNodeQuery =
                serde_json::from_str(raw).map_err(|_| Stage::TypedCoverageRead.invalid())?;
            if serde_json::to_value(&action).map_err(|_| Stage::CanonicalFields.invalid())? != value
            {
                return Err(Stage::CanonicalFields.invalid());
            }
            let ExactNodeQuery::QueryContext {
                context_id: actual,
                query,
            } = action;
            if actual != context_id {
                return Err(ActionSemanticRule::QueryContext.invalid());
            }
            query
                .validate_for_specialist(role)
                .map_err(|e| ActionSemanticRule::QueryShape.failure(e))?;
            return Ok(CompletionInvestigationAction::Read(
                QualifiedInvestigationAction::QueryContext {
                    context_id: actual,
                    query,
                },
            ));
        }
        if value["type"] != "replay_query" {
            return CompletionInvestigationAction::decode_detailed_surface(
                raw,
                context_id,
                QualifiedReadSurface {
                    artifact_reads: role.uses_artifact_reads(),
                },
            );
        }
        let action: ReplayAction =
            serde_json::from_str(raw).map_err(|_| Stage::TypedReplay.invalid())?;
        if serde_json::to_value(&action).map_err(|_| Stage::CanonicalFields.invalid())? != value {
            return Err(Stage::CanonicalFields.invalid());
        }
        let ReplayAction::ReplayQuery {
            context_id: actual,
            query_index,
            query_id,
        } = action;
        if actual != context_id {
            return Err(ActionSemanticRule::ReplayContext.invalid());
        }
        let query = QueryHistoryReference {
            query_index,
            query_id,
        }
        .resolve(request_hash, ledger, policy)
        .map_err(|_| ActionSemanticRule::ReplayReference.invalid())?;
        query
            .validate_for_specialist(role)
            .map_err(|e| ActionSemanticRule::QueryShape.failure(e))?;
        Ok(CompletionInvestigationAction::Read(
            QualifiedInvestigationAction::QueryContext {
                context_id: actual,
                query,
            },
        ))
    }
}
