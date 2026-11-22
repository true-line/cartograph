#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::{
    ActionDecodeFailure, ActionDecodeStage, ActionSemanticRule, InvestigationError,
    MAX_ACTION_BYTES, MAX_QUERY_BYTES, MAX_QUERY_FACTS, bounded_bytes, reject_prose_replay, strict,
    text,
};
use crate::{TaskFactKey, TaskRangeRole};
use core_prov::{ConfidenceTier, Tier};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Host-normalized recovered context scope; never a filesystem capability.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InvestigationScope {
    #[default]
    All,
    Node {
        id: String,
    },
    Neighborhood {
        anchor: String,
        hops: u32,
    },
    /// The files of one overlay `Subsystem` and what they define (#2044).
    Area {
        id: String,
    },
}

impl InvestigationScope {
    pub fn validate(&self) -> Result<(), InvestigationError> {
        match self {
            Self::All => Ok(()),
            Self::Neighborhood { anchor, hops } if text(anchor, 1024) && (1..=3).contains(hops) => {
                Ok(())
            }
            Self::Area { id } if text(id, 1024) => Ok(()),
            _ => Err(InvestigationError::InvalidRequest),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationFactKind {
    Node,
    Edge,
}

/// The existing context query cursor meaning, with a strict new action decoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationQueryCursor {
    pub snapshot_id: String,
    pub selection_id: String,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationQuery {
    pub scope: InvestigationScope,
    pub kind: Option<InvestigationFactKind>,
    pub labels: Vec<String>,
    pub max_facts: usize,
    pub max_bytes: usize,
    pub cursor: Option<InvestigationQueryCursor>,
}

impl InvestigationQuery {
    pub fn validate(&self) -> Result<(), InvestigationError> {
        self.scope.validate()?;
        self.validate_retained()
    }

    pub fn validate_for_specialist(
        &self,
        role: super::SpecialistId,
    ) -> Result<(), InvestigationError> {
        if role.uses_exact_node_queries() {
            self.validate_retained()
        } else {
            self.validate()
        }
    }

    pub(super) fn validate_retained(&self) -> Result<(), InvestigationError> {
        match &self.scope {
            InvestigationScope::Node { id } if text(id, 1024) => {}
            // An area scopes a task, never a query inside one.
            InvestigationScope::Area { .. } => return Err(InvestigationError::InvalidRequest),
            scope => scope.validate()?,
        }
        if !(1..=MAX_QUERY_FACTS).contains(&self.max_facts)
            || !(1..=MAX_QUERY_BYTES).contains(&self.max_bytes)
            || self.labels.len() > 16
            || self.labels.iter().any(|label| !text(label, 128))
        {
            return Err(InvestigationError::InvalidAction);
        }
        if let Some(cursor) = &self.cursor
            && (!text(&cursor.snapshot_id, 128)
                || !text(&cursor.selection_id, 128)
                || cursor.offset > 50_000)
        {
            return Err(InvestigationError::InvalidAction);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationTool {
    QueryContext,
    ReadEvidence,
    ReadCoverageInventory,
    /// A compiled-artifact page (ADR-0089).
    ReadArtifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationClaimKind {
    ImplementedBehavior,
    DocumentedIntent,
    InferredInterpretation,
    ProposedDesign,
}

/// An execution can finish with an explicitly incomplete knowledge result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeCompleteness {
    Partial,
    InsufficientEvidence,
}

/// Model-authored fields only. The model cannot assign producing authority or
/// result identity; those fields are stamped after admission by the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedInvestigationFinding {
    pub claim_kind: InvestigationClaimKind,
    pub title: String,
    pub statement: String,
    pub citation_ids: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InvestigationAction {
    QueryContext {
        query: InvestigationQuery,
    },
    ReadEvidence {
        fact: TaskFactKey,
        role: TaskRangeRole,
        index: u32,
    },
    Finish {
        findings: Vec<ProposedInvestigationFinding>,
        knowledge_completeness: KnowledgeCompleteness,
        limitations: Vec<String>,
    },
}

impl InvestigationAction {
    /// Admit exactly one complete strict JSON value, without extraction or repair.
    pub fn decode(raw: &str) -> Result<Self, InvestigationError> {
        Self::decode_detailed(raw).map_err(|failure| failure.error)
    }
    pub(super) fn decode_detailed(raw: &str) -> Result<Self, ActionDecodeFailure> {
        use ActionDecodeStage as Stage;
        strict::preflight(raw, MAX_ACTION_BYTES, 16, 4096, 32)
            .map_err(|_| Stage::Preflight.invalid())?;
        let action: Self =
            serde_json::from_str(raw).map_err(|_| Stage::TypedLegacyAction.invalid())?;
        // Close optional-field omissions and reused nested decoder behavior.
        let original: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| Stage::TypedEnvelope.invalid())?;
        if serde_json::to_value(&action).map_err(|_| Stage::CanonicalFields.invalid())? != original
        {
            return Err(Stage::CanonicalFields.invalid());
        }
        action.validate_shape_detailed()?;
        Ok(action)
    }

    pub fn tool(&self) -> Option<InvestigationTool> {
        match self {
            Self::QueryContext { .. } => Some(InvestigationTool::QueryContext),
            Self::ReadEvidence { .. } => Some(InvestigationTool::ReadEvidence),
            Self::Finish { .. } => None,
        }
    }

    fn validate_shape(&self) -> Result<(), InvestigationError> {
        self.validate_shape_detailed()
            .map_err(|failure| failure.error)
    }
    fn validate_shape_detailed(&self) -> Result<(), ActionDecodeFailure> {
        match self {
            Self::QueryContext { query } => query
                .validate()
                .map_err(|error| ActionSemanticRule::QueryShape.failure(error)),
            Self::ReadEvidence { fact, index, .. } => {
                if !valid_fact(fact) || *index >= 1024 {
                    return Err(ActionSemanticRule::EvidenceShape.invalid());
                }
                Ok(())
            }
            Self::Finish {
                findings,
                knowledge_completeness,
                limitations,
            } => {
                if findings.len() > 12
                    || (findings.is_empty()
                        != (*knowledge_completeness == KnowledgeCompleteness::InsufficientEvidence))
                    || !valid_limitations(limitations)
                {
                    return Err(ActionSemanticRule::FinishOverall.invalid());
                }
                for f in findings {
                    if !text(&f.title, 160) || !text(&f.statement, 2048) {
                        return Err(ActionSemanticRule::FindingTextLimits.invalid());
                    }
                    if f.citation_ids.is_empty()
                        || f.citation_ids.len() > 8
                        || f.citation_ids.iter().any(|id| !text(id, 128))
                        || f.citation_ids.iter().collect::<BTreeSet<_>>().len()
                            != f.citation_ids.len()
                    {
                        return Err(ActionSemanticRule::FindingCitations.invalid());
                    }
                    if !valid_limitations(&f.limitations) {
                        return Err(ActionSemanticRule::FindingTextLimits.invalid());
                    }
                }
                Ok(())
            }
        }
    }

    /// Validate final output against the exact last supplied input. This method
    /// performs no current graph/source read and cannot broaden its citation set.
    pub fn admit_finish<'a>(
        &self,
        admitted_citation_ids: &BTreeSet<String>,
        supplied_strings: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<InvestigationFinding>, InvestigationError> {
        self.validate_shape()?;
        let Self::Finish {
            findings,
            limitations,
            ..
        } = self
        else {
            return Err(InvestigationError::InvalidAction);
        };
        if findings
            .iter()
            .flat_map(|f| &f.citation_ids)
            .any(|id| !admitted_citation_ids.contains(id))
        {
            return Err(InvestigationError::InvalidCitation);
        }
        let prose: Vec<&str> = findings
            .iter()
            .flat_map(|f| {
                std::iter::once(f.title.as_str())
                    .chain(std::iter::once(f.statement.as_str()))
                    .chain(f.limitations.iter().map(String::as_str))
            })
            .chain(limitations.iter().map(String::as_str))
            .collect();
        reject_prose_replay(prose, supplied_strings)?;
        let admitted = findings
            .iter()
            .enumerate()
            .map(|(index, finding)| InvestigationFinding {
                finding_id: format!("finding-{}", index + 1),
                claim_kind: finding.claim_kind,
                title: finding.title.clone(),
                statement: finding.statement.clone(),
                citation_ids: finding.citation_ids.clone(),
                limitations: finding.limitations.clone(),
                tier: Tier::Agentic,
                confidence_tier: ConfidenceTier::InferredWeak,
            })
            .collect::<Vec<_>>();
        bounded_bytes(&admitted, super::MAX_RESULT_BYTES)?;
        Ok(admitted)
    }
}

fn valid_limitations(values: &[String]) -> bool {
    !values.is_empty() && values.len() <= 8 && values.iter().all(|value| text(value, 512))
}

pub(crate) fn valid_fact(fact: &TaskFactKey) -> bool {
    match fact {
        TaskFactKey::Node { id } => text(id, 8192),
        TaskFactKey::Edge {
            source,
            label,
            destination,
        } => text(source, 8192) && text(label, 256) && text(destination, 8192),
    }
}

/// An admitted answer fragment. Reading it does not activate curated context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationFinding {
    pub finding_id: String,
    pub claim_kind: InvestigationClaimKind,
    pub title: String,
    pub statement: String,
    pub citation_ids: Vec<String>,
    pub limitations: Vec<String>,
    pub tier: Tier,
    pub confidence_tier: ConfidenceTier,
}
