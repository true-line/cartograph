//! Immutable generation-4 requests and mixed completion-obligation identities.
//! Acquisition, execution and public availability remain host responsibilities.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// Share the existing request/inspection admission bounds without changing old encodings.
/// Generation-4 whole-request, envelope and individual target encoding cap.
pub const COMPLETION_REQUEST_BYTES: usize = 8 * 1024;
/// Raw mixed-obligation admission cap, applied before ordering or deduplication.
pub const MAX_COMPLETION_OBLIGATIONS: usize = 12;
const CONTRACT: &str = "investigation-completion-obligations@1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryTarget {
    pub context_id: String,
    pub repo: String,
    pub inventory_hash: String,
}
impl InventoryTarget {
    fn validate(&self) -> Result<(), InvestigationError> {
        if !self
            .context_id
            .strip_prefix("context-v2:")
            .is_some_and(crate::source_basis::hash)
            || (self.repo.is_empty()
                || self.repo.len() > core_graph::coverage::MAX_IDENTITY_BYTES
                || self.repo.contains('\0'))
            || !crate::source_basis::hash(&self.inventory_hash)
        {
            return Err(InvestigationError::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Obligation {
    Source {
        obligation_id: String,
        target: Box<InvestigationInspectionTarget>,
    },
    Inventory {
        obligation_id: String,
        target: InventoryTarget,
    },
}
impl Obligation {
    pub fn content_id(&self) -> Result<String, InvestigationError> {
        let (domain, bytes) = match self {
            Self::Source { target, .. } => {
                target.validate()?;
                (
                    b"cartograph:source-obligation:v1\0".as_slice(),
                    bounded_bytes(target, COMPLETION_REQUEST_BYTES)?,
                )
            }
            Self::Inventory { target, .. } => {
                target.validate()?;
                (
                    b"cartograph:inventory-obligation:v1\0".as_slice(),
                    bounded_bytes(target, COMPLETION_REQUEST_BYTES)?,
                )
            }
        };
        Ok(crate::source_basis::domain_hash(domain, &bytes))
    }
    pub fn key(&self) -> (&str, &str) {
        match self {
            Self::Source { obligation_id, .. } => ("source", obligation_id),
            Self::Inventory { obligation_id, .. } => ("inventory", obligation_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationEnvelope {
    pub schema_version: u32,
    pub contract: String,
    pub obligations: Vec<Obligation>,
}
impl ObligationEnvelope {
    pub fn normalized(&self) -> Result<Self, InvestigationError> {
        if self.schema_version != 1
            || self.contract != CONTRACT
            || !(1..=MAX_COMPLETION_OBLIGATIONS).contains(&self.obligations.len())
        {
            return Err(InvestigationError::InvalidRequest);
        }
        bounded_bytes(self, COMPLETION_REQUEST_BYTES)?;
        // Admit the complete raw list before any target/partition ownership.
        let mut entries = BTreeMap::new();
        let mut source_keys = BTreeMap::new();
        let mut inventory_keys = BTreeMap::new();
        let mut snapshot = None;
        for item in &self.obligations {
            if item.key().1 != item.content_id()? {
                return Err(InvestigationError::InvalidRequest);
            }
            if let Obligation::Source { target, .. } = item {
                if snapshot.is_some_and(|old| old != &target.graph_snapshot_id) {
                    return Err(InvestigationError::InvalidRequest);
                }
                snapshot = Some(&target.graph_snapshot_id);
                let key = bounded_bytes(&(&target.fact, target.role, target.index), 4096)?;
                if source_keys
                    .insert(key, &target.target_id)
                    .is_some_and(|old| old != &target.target_id)
                {
                    return Err(InvestigationError::InvalidRequest);
                }
            }
            if let Obligation::Inventory { target, .. } = item
                && inventory_keys
                    .insert((&target.context_id, &target.repo), &target.inventory_hash)
                    .is_some_and(|old| old != &target.inventory_hash)
            {
                return Err(InvestigationError::InvalidRequest);
            }
            if entries
                .insert(item.key(), item)
                .is_some_and(|old| old != item)
            {
                return Err(InvestigationError::InvalidRequest);
            }
        }
        Ok(Self {
            schema_version: 1,
            contract: CONTRACT.into(),
            obligations: entries.into_values().cloned().collect(),
        })
    }
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        Ok(crate::source_basis::domain_hash(
            b"cartograph:completion-obligations:v1\0",
            &bounded_bytes(&self.normalized()?, COMPLETION_REQUEST_BYTES)?,
        ))
    }
    /// Projection is for future authoritative host validation, not another authority.
    pub fn sources(&self) -> Result<Vec<InvestigationInspectionTarget>, InvestigationError> {
        Ok(self
            .normalized()?
            .obligations
            .into_iter()
            .filter_map(|item| match item {
                Obligation::Source { target, .. } => Some(*target),
                _ => None,
            })
            .collect())
    }
}

/// Generation4 is a distinct closed request family. The transport selects this
/// arm explicitly; legacy StartInvestigationRequest never accepts V7 roles.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedObligationRequest {
    pub schema_version: u32,
    pub request_nonce: String,
    pub specialist_id: String,
    pub obligations: ObligationEnvelope,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_legacy"
    )]
    pub inspection_requirements: Option<Vec<InvestigationInspectionTarget>>,
    pub question: String,
    pub scope: InvestigationScope,
    pub provider_mode: InvestigationProviderMode,
    pub limit_profile: String,
    pub expected_graph_revision: Option<String>,
    pub expected_context_id: String,
    pub conversation_id: Option<String>,
    pub parent_id: Option<String>,
}
// Presence-only decoder: a present array is rejected later, while null must
// never collapse to the absent legacy field and bypass the authority check.
fn present_legacy<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<InvestigationInspectionTarget>>, D::Error> {
    Vec::deserialize(d).map(Some)
}
impl StagedObligationRequest {
    pub fn decode(raw: &str) -> Result<Self, InvestigationError> {
        super::strict::preflight(raw, COMPLETION_REQUEST_BYTES, 32, 4096, 4096)?;
        let value: Self =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidRequest)?;
        value.normalized()
    }
    pub fn normalized(&self) -> Result<Self, InvestigationError> {
        // Whole request admission must precede envelope partition clones.
        bounded_bytes(self, COMPLETION_REQUEST_BYTES)?;
        if self.schema_version != 4
            || self.inspection_requirements.is_some()
            || !matches!(
                self.specialist_id.as_str(),
                "domain-analyst@7" | "evidence-auditor@7"
            )
            || !self
                .expected_context_id
                .strip_prefix("context-v2:")
                .is_some_and(crate::source_basis::hash)
        {
            return Err(InvestigationError::InvalidRequest);
        }
        // Reuse immutable field validation without changing any legacy branches.
        StartInvestigationRequest {
            schema_version: 3,
            request_nonce: self.request_nonce.clone(),
            specialist_id: SpecialistId::DomainAnalystV6,
            inspection_requirements: None,
            question: self.question.clone(),
            scope: self.scope.clone(),
            provider_mode: self.provider_mode,
            limit_profile: self.limit_profile.clone(),
            expected_graph_revision: self.expected_graph_revision.clone(),
            expected_context_id: Some(self.expected_context_id.clone()),
            conversation_id: self.conversation_id.clone(),
            parent_id: self.parent_id.clone(),
        }
        .validate()?;
        for item in &self.obligations.obligations {
            match item {
                Obligation::Source { target, .. }
                    if self.expected_graph_revision.as_deref()
                        != Some(target.graph_snapshot_id.as_str()) =>
                {
                    return Err(InvestigationError::InvalidRequest);
                }
                Obligation::Inventory { target, .. }
                    if target.context_id != self.expected_context_id =>
                {
                    return Err(InvestigationError::InvalidRequest);
                }
                _ => (),
            }
        }
        let obligations = self.obligations.normalized()?;
        Ok(Self {
            obligations,
            ..self.clone()
        })
    }
    /// Bind a normalized V7 intent to its retained qualified acquisition basis.
    pub fn validate_ledger(
        &self,
        ledger: &InvestigationInputLedger,
    ) -> Result<(), InvestigationError> {
        let request = self.normalized()?;
        ledger.validate()?;
        let proof = ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        if proof.context_id != request.expected_context_id
            || request
                .expected_graph_revision
                .as_deref()
                .is_some_and(|graph| graph != ledger.graph_snapshot_id)
            || request
                .obligations
                .obligations
                .iter()
                .any(|item| match item {
                    Obligation::Inventory { target, .. } => {
                        !proof.qualification.repos.iter().any(|repo| {
                            repo.repo == target.repo
                                && repo.status == context_hub::coverage::CoverageStatus::Recorded
                                && repo.inventory_hash.as_deref()
                                    == Some(target.inventory_hash.as_str())
                        })
                    }
                    Obligation::Source { target, .. } => {
                        target.graph_snapshot_id != ledger.graph_snapshot_id
                    }
                })
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
    pub fn intent_hash(&self) -> Result<String, InvestigationError> {
        Ok(crate::source_basis::domain_hash(
            b"cartograph:investigation-client-intent:v4\0",
            &bounded_bytes(&self.normalized()?, COMPLETION_REQUEST_BYTES)?,
        ))
    }
}

#[cfg(test)]
#[path = "obligations_tests.rs"]
mod tests;
