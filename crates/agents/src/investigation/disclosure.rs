//! Metadata for what one invocation actually received. Acquisition stays in the
//! unchanged ledger; these hashes cannot reconstruct source or grant authority.
use super::*;
use crate::TaskFactKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const COMPACT_DISCLOSURE_POLICY: &str = "deduplicated-recovered-context@1";
pub const MAX_DISCLOSURE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureOmissionReason {
    /// Reserved; compact projections currently share supplied representations.
    DuplicateRepresentation,
    NotSelected,
    WorkingSetBudget,
}

/// A supplied hash identifies the actual redacted representation, not merely its
/// acquired origin. Omission is explicit and never makes a citation eligible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisclosureRepresentation {
    Supplied { representation_hash: String },
    Omitted { reason: DisclosureOmissionReason },
}

impl DisclosureRepresentation {
    pub fn is_supplied(&self) -> bool {
        matches!(self, Self::Supplied { .. })
    }

    fn validate(&self) -> Result<(), InvestigationError> {
        if matches!(
            self,
            Self::Omitted {
                reason: DisclosureOmissionReason::DuplicateRepresentation
            }
        ) {
            return Err(InvestigationError::InvalidInput);
        }
        if let Self::Supplied {
            representation_hash,
        } = self
            && !crate::source_basis::hash(representation_hash)
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedInvestigationFact {
    pub fact: TaskFactKey,
    pub representation: DisclosureRepresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedInvestigationQuery {
    pub query_index: usize,
    /// Hash of the supplied compact selection/count/continuation summary.
    /// A summary alone never supplies a graph fact's properties.
    pub representation: DisclosureRepresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedInvestigationSource {
    pub citation_id: String,
    /// Hash of the complete supplied span representation, including metadata.
    pub representation: DisclosureRepresentation,
    /// Actual supplied text hash, distinct from the original ledger text hash.
    pub disclosed_text_hash: Option<String>,
    /// True when supplied text differs from, or cannot be compared with, the
    /// ledger text hash (including a missing legacy hash); not proof of a rewrite.
    pub redacted: bool,
}

/// A complete accounting in acquisition order. Each acquired fact, query and
/// source citation has exactly one supplied/omitted entry, including uncited
/// evidence. Original source bindings are resolved through input_ledger_hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationDisclosure {
    pub schema_version: u32,
    pub policy: String,
    pub input_ledger_hash: String,
    pub graph_snapshot_id: String,
    pub scope_snapshot_id: String,
    /// The firewall's exact redacted payload identity, also bound by consent.
    pub payload_hash: String,
    pub facts: Vec<DisclosedInvestigationFact>,
    pub queries: Vec<DisclosedInvestigationQuery>,
    pub sources: Vec<DisclosedInvestigationSource>,
    pub history: Option<DisclosureRepresentation>,
}

impl InvestigationDisclosure {
    /// Validates durable metadata against its saved acquisition basis. The live
    /// preparation path must separately check actual representation bytes before
    /// sealing this record; metadata does not attest unavailable old source.
    pub fn validate(&self, ledger: &InvestigationInputLedger) -> Result<(), InvestigationError> {
        ledger.validate()?;
        if self.schema_version != 1
            || self.policy != COMPACT_DISCLOSURE_POLICY
            || self.input_ledger_hash != ledger.fingerprint()?
            || self.graph_snapshot_id != ledger.graph_snapshot_id
            || self.scope_snapshot_id != ledger.scope_snapshot_id
            || !crate::source_basis::hash(&self.payload_hash)
            || self.facts.len() != ledger.selected_facts.len()
            || self.queries.len() != ledger.queries.len()
            || self.history.is_some() != ledger.supplied_history_hash.is_some()
        {
            return Err(InvestigationError::InvalidInput);
        }
        for (entry, acquired) in self.facts.iter().zip(&ledger.selected_facts) {
            if entry.fact != acquired.fact {
                return Err(InvestigationError::InvalidInput);
            }
            entry.representation.validate()?;
        }
        for (index, entry) in self.queries.iter().enumerate() {
            if entry.query_index != index {
                return Err(InvestigationError::InvalidInput);
            }
            entry.representation.validate()?;
        }
        let acquired_sources = ledger
            .citations
            .iter()
            .filter(|citation| {
                !matches!(citation.origin, InvestigationEvidenceOrigin::GraphMetadata)
            })
            .collect::<Vec<_>>();
        if self.sources.len() != acquired_sources.len() {
            return Err(InvestigationError::InvalidInput);
        }
        for (entry, acquired) in self.sources.iter().zip(acquired_sources) {
            entry.representation.validate()?;
            if entry.citation_id != acquired.citation_id {
                return Err(InvestigationError::InvalidInput);
            }
            match &entry.representation {
                DisclosureRepresentation::Supplied { .. } => {
                    let disclosed_hash = entry
                        .disclosed_text_hash
                        .as_deref()
                        .ok_or(InvestigationError::InvalidInput)?;
                    if !crate::source_basis::hash(disclosed_hash)
                        || entry.redacted != (Some(disclosed_hash) != acquired.text_hash.as_deref())
                        || !self.fact_supplied(&acquired.fact)
                    {
                        return Err(InvestigationError::InvalidInput);
                    }
                }
                DisclosureRepresentation::Omitted { .. } => {
                    if entry.disclosed_text_hash.is_some() || entry.redacted {
                        return Err(InvestigationError::InvalidInput);
                    }
                }
            }
        }
        if let Some(history) = &self.history {
            history.validate()?;
        }
        bounded_bytes(self, MAX_DISCLOSURE_BYTES)?;
        Ok(())
    }

    fn fact_supplied(&self, fact: &TaskFactKey) -> bool {
        self.facts
            .iter()
            .any(|entry| &entry.fact == fact && entry.representation.is_supplied())
    }

    pub fn fingerprint(
        &self,
        ledger: &InvestigationInputLedger,
    ) -> Result<String, InvestigationError> {
        self.validate(ledger)?;
        Ok(format!(
            "investigation-disclosure-v1:{}",
            crate::source_basis::domain_hash(
                b"cartograph:investigation-disclosure:v1\0",
                &bounded_bytes(self, MAX_DISCLOSURE_BYTES)?,
            )
        ))
    }

    pub fn eligible_citations(
        &self,
        ledger: &InvestigationInputLedger,
    ) -> Result<BTreeSet<String>, InvestigationError> {
        self.validate(ledger)?;
        Ok(ledger
            .citations
            .iter()
            .filter(|citation| match citation.origin {
                InvestigationEvidenceOrigin::GraphMetadata => self.fact_supplied(&citation.fact),
                _ => self.sources.iter().any(|entry| {
                    entry.citation_id == citation.citation_id && entry.representation.is_supplied()
                }),
            })
            .map(|citation| citation.citation_id.clone())
            .collect())
    }
}
