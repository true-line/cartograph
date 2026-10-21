//! Caller-bound captured inspection requirements; never inferred from prose.
use super::*;
use crate::{TaskCaptureSpanRef, TaskFactKey, TaskRangeRole};
use core_prov::EvidenceRef;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationInspectionTarget {
    pub schema_version: u32,
    pub target_id: String,
    pub graph_snapshot_id: String,
    pub fact: TaskFactKey,
    pub fact_digest: String,
    pub registered_source_id: String,
    pub receipt_id: String,
    pub receipt_inventory_index: u32,
    pub role: TaskRangeRole,
    pub index: u32,
    pub source: EvidenceRef,
    pub captured: TaskCaptureSpanRef,
}
impl InvestigationInspectionTarget {
    pub fn content_id(&self) -> Result<String, InvestigationError> {
        bounded_bytes(self, 8 * 1024)?;
        let mut target = self.clone();
        target.target_id.clear();
        Ok(crate::source_basis::domain_hash(
            b"cartograph:inspection-target:v1\0",
            &bounded_bytes(&target, 8 * 1024)?,
        ))
    }
    pub fn validate(&self) -> Result<(), InvestigationError> {
        let file = &self.captured.file;
        let digest = self.fact_digest.strip_prefix(match self.fact {
            TaskFactKey::Node { .. } => "node-v1:",
            TaskFactKey::Edge { .. } => "edge-v1:",
        });
        let path_valid = !self.source.path.is_empty()
            && self.source.path.len() <= 1024
            && !self.source.path.contains(['\\', ':'])
            && !self
                .source
                .path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..");
        if !crate::source_basis::hash(&self.target_id)
            || self.schema_version != 1
            || !super::protocol::valid_fact(&self.fact)
            || !super::records::context_hash(&self.graph_snapshot_id)
            || !digest.is_some_and(crate::source_basis::hash)
            || !self
                .registered_source_id
                .strip_prefix("src_")
                .is_some_and(|s| {
                    s.len() == 32
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            || !self
                .receipt_id
                .strip_prefix("ts-primary-v2:")
                .or_else(|| self.receipt_id.strip_prefix("ts-primary-v1:"))
                .is_some_and(crate::source_basis::hash)
            || !text(&self.source.repo, 256)
            || !text(&self.source.commit_sha, 256)
            || !path_valid
            || self.index >= 1024
            || self.receipt_inventory_index >= 1024
            || self.source.byte_start >= self.source.byte_end
            || self.source.byte_end - self.source.byte_start > 8192
            || self.captured.byte_start != self.source.byte_start
            || self.captured.byte_end != self.source.byte_end
            || file.source_id != self.registered_source_id
            || file.path != self.source.path
            || file.byte_len > 16 * 1024 * 1024
            || self.captured.byte_end > file.byte_len
            || !file
                .capture_id
                .strip_prefix("capture-v1:")
                .is_some_and(crate::source_basis::hash)
            || !crate::source_basis::hash(&file.digest)
            || self.target_id != self.content_id()?
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
    pub fn matches(&self, citation: &InvestigationCitation) -> bool {
        citation.fact == self.fact
            && citation.fact_digest == self.fact_digest
            && citation.role == Some(self.role)
            && citation.index == Some(self.index)
            && citation.source.as_ref() == Some(&self.source)
            && matches!(&citation.origin, InvestigationEvidenceOrigin::CapturedPrimarySource {
                registered_source_id,receipt_id,receipt_inventory_index,captured,..}
                if registered_source_id==&self.registered_source_id && receipt_id==&self.receipt_id
                    && *receipt_inventory_index==self.receipt_inventory_index && captured==&self.captured)
    }
}

pub fn normalize_inspection_targets(
    targets: &[InvestigationInspectionTarget],
) -> Result<Vec<InvestigationInspectionTarget>, InvestigationError> {
    if targets.is_empty() || targets.len() > 12 {
        return Err(InvestigationError::InvalidRequest);
    }
    bounded_bytes(&targets, 8 * 1024)?;
    let mut identities = BTreeMap::new();
    let mut exact = BTreeMap::new();
    for target in targets {
        target.validate()?;
        if target.graph_snapshot_id != targets[0].graph_snapshot_id {
            return Err(InvestigationError::InvalidRequest);
        }
        let key = bounded_bytes(&(&target.fact, target.role, target.index), 4096)?;
        if identities
            .insert(key, target.target_id.clone())
            .is_some_and(|old| old != target.target_id)
        {
            return Err(InvestigationError::InvalidRequest);
        }
        exact.insert(target.target_id.clone(), target.clone());
    }
    Ok(exact.into_values().collect())
}
pub fn inspection_requirements_hash(
    targets: &[InvestigationInspectionTarget],
) -> Result<String, InvestigationError> {
    let targets = normalize_inspection_targets(targets)?;
    Ok(crate::source_basis::domain_hash(
        b"cartograph:inspection-requirements:v1\0",
        &bounded_bytes(&targets, 8 * 1024)?,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationInspectionStatus {
    Pending,
    NotAssessed,
    Supplied,
    Unmet,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationInspectionObservation {
    pub target_id: String,
    pub target: InvestigationInspectionTarget,
    pub status: InvestigationInspectionStatus,
    pub acquired: bool,
    pub supplied: bool,
    pub cited: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationInspectionAssessment {
    pub schema_version: u32,
    pub requirements_hash: String,
    pub input_ledger_hash: String,
    pub finishing_step_id: String,
    pub disclosure_hash: String,
    pub requirements: Vec<InvestigationInspectionObservation>,
}
impl InvestigationInspectionAssessment {
    pub fn derive(
        targets: &[InvestigationInspectionTarget],
        ledger: &InvestigationInputLedger,
        step: &str,
        disclosure: &InvestigationDisclosure,
        findings: &[InvestigationFinding],
    ) -> Result<Self, InvestigationError> {
        disclosure.validate(ledger)?;
        let targets = normalize_inspection_targets(targets)?;
        if !super::records::opaque_id(step)
            || targets[0].graph_snapshot_id != ledger.graph_snapshot_id
        {
            return Err(InvestigationError::InvalidInput);
        }
        let requirements_hash = inspection_requirements_hash(&targets)?;
        if disclosure.inspection_requirements_hash.as_deref() != Some(requirements_hash.as_str()) {
            return Err(InvestigationError::InvalidInput);
        }
        let cited: BTreeSet<_> = findings
            .iter()
            .flat_map(|f| f.citation_ids.iter())
            .collect();
        let requirements = targets
            .into_iter()
            .map(|target| {
                let matches = ledger
                    .citations
                    .iter()
                    .filter(|c| target.matches(c))
                    .collect::<Vec<_>>();
                let acquired = !matches.is_empty();
                let supplied = matches.iter().any(|citation| {
                    disclosure.sources.iter().any(|source| {
                        source.citation_id == citation.citation_id
                            && source.representation.is_supplied()
                            && !source.redacted
                            && matches!(
                                (&source.disclosed_text_hash, &citation.text_hash),
                                (Some(disclosed), Some(original)) if disclosed == original
                            )
                    })
                });
                let cited = matches
                    .iter()
                    .any(|citation| cited.contains(&citation.citation_id));
                InvestigationInspectionObservation {
                    target_id: target.target_id.clone(),
                    target,
                    status: if supplied {
                        InvestigationInspectionStatus::Supplied
                    } else {
                        InvestigationInspectionStatus::Unmet
                    },
                    acquired,
                    supplied,
                    cited,
                }
            })
            .collect();
        Ok(Self {
            schema_version: 1,
            requirements_hash,
            input_ledger_hash: ledger.fingerprint()?,
            finishing_step_id: step.into(),
            disclosure_hash: disclosure.fingerprint(ledger)?,
            requirements,
        })
    }
    pub fn targets(&self) -> Result<Vec<InvestigationInspectionTarget>, InvestigationError> {
        if self.schema_version != 1 || self.requirements.is_empty() || self.requirements.len() > 12
        {
            return Err(InvestigationError::InvalidInput);
        }
        bounded_bytes(self, MAX_RESULT_BYTES)?;
        Ok(self.requirements.iter().map(|r| r.target.clone()).collect())
    }
    pub fn validate(
        &self,
        ledger: &InvestigationInputLedger,
        step: &str,
        disclosure: &InvestigationDisclosure,
        findings: &[InvestigationFinding],
    ) -> Result<(), InvestigationError> {
        if *self != Self::derive(&self.targets()?, ledger, step, disclosure, findings)? {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}
