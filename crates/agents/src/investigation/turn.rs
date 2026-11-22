//! V4 turn progress derived from durable ordered admissions, never model claims.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::*;
use crate::{TaskFactKey, TaskRangeRole};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Shared admission/model budget for all acquired raw query pages, including repeats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationQueryBudget {
    pub limit_bytes: usize,
    pub consumed_bytes: usize,
    pub remaining_bytes: usize,
}
impl InvestigationQueryBudget {
    pub fn from_ledger(ledger: &InvestigationInputLedger) -> Result<Self, InvestigationError> {
        let consumed = ledger
            .queries
            .iter()
            .map(|query| query.response_bytes)
            .chain(ledger.coverage_reads.iter().map(|read| read.response_bytes))
            .chain(ledger.artifact_reads.iter().map(|read| read.response_bytes))
            .try_fold(0usize, |sum, bytes| {
                sum.checked_add(bytes)
                    .ok_or(InvestigationError::LimitExceeded)
            })?;
        Self::from_consumed(consumed)
    }
    fn from_consumed(consumed_bytes: usize) -> Result<Self, InvestigationError> {
        let remaining_bytes = MAX_TOTAL_QUERY_BYTES
            .checked_sub(consumed_bytes)
            .ok_or(InvestigationError::LimitExceeded)?;
        Ok(Self {
            limit_bytes: MAX_TOTAL_QUERY_BYTES,
            consumed_bytes,
            remaining_bytes,
        })
    }
    /// Apply the same raw-byte ceiling used in the model-visible budget.
    pub fn admit(&self, additional: usize) -> Result<Self, InvestigationError> {
        Self::from_consumed(
            self.consumed_bytes
                .checked_add(additional)
                .ok_or(InvestigationError::LimitExceeded)?,
        )
    }
}

/// Host read-transaction result. Previous/current ledgers and completed event
/// must be read coherently; this pure type cannot attest a database operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationTurnBasis {
    pub input_ledger_hash: String,
    pub previous_ledger: Option<InvestigationInputLedger>,
    pub completed_tool: Option<InvestigationTool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationTurnPhase {
    Initial,
    AfterQuery,
    AfterRead,
    AfterCoverageRead,
    AfterArtifactRead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvestigationCompletedAction {
    ReadCoverageInventory {
        read_index: usize,
        repo: String,
        inventory_hash: String,
        response_bytes: usize,
    },
    ReadArtifact {
        read_index: usize,
        artifact_id: String,
        content_id: String,
        offset: usize,
        next_offset: Option<usize>,
        response_bytes: usize,
    },
    QueryContext {
        query_index: usize,
        query: InvestigationQuery,
        response_hash: String,
        returned_facts: usize,
        new_fact_count: usize,
        total_selected: usize,
        next_cursor: Option<InvestigationQueryCursor>,
        repeated_query: bool,
        unchanged_result: bool,
        no_progress: bool,
    },
    ReadEvidence {
        fact: TaskFactKey,
        role: TaskRangeRole,
        index: u32,
        citation_id: String,
        /// Original admitted source range, before firewall redaction/omission.
        source_range_bytes: u64,
        previously_acquired: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvestigationTurnProgress {
    pub contract: &'static str,
    pub input_revision: u64,
    pub phase: InvestigationTurnPhase,
    pub last_completed_action: Option<InvestigationCompletedAction>,
    pub raw_query_budget: InvestigationQueryBudget,
}

impl InvestigationTurnBasis {
    /// Reconcile durable input ordering and tool identity. Unavailable reads do
    /// not publish an input; repeated successful reads append distinct citations.
    pub fn validate(&self, current: &InvestigationInputLedger) -> Result<(), InvestigationError> {
        self.validate_with_policy(current, &INVESTIGATION_V1_POLICY)
    }
    pub fn validate_with_policy(
        &self,
        current: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        current.validate_with_policy(policy)?;
        if self.input_ledger_hash != current.fingerprint_with_policy(policy)? {
            return Err(InvestigationError::InvalidInput);
        }
        let Some(previous) = &self.previous_ledger else {
            return if current.revision == 0
                && self.completed_tool.is_none()
                && current.coverage_reads.is_empty()
                && current.artifact_reads.is_empty()
                && current.queries.is_empty()
                && current.citations.is_empty()
                && current.selected_facts.is_empty()
            {
                Ok(())
            } else {
                Err(InvestigationError::InvalidInput)
            };
        };
        previous.validate_with_policy(policy)?;
        if previous.schema_version != current.schema_version
            || previous.coverage != current.coverage
            || previous.revision.checked_add(1) != Some(current.revision)
            || previous.graph_snapshot_id != current.graph_snapshot_id
            || previous.scope_snapshot_id != current.scope_snapshot_id
            || previous.supplied_history_hash != current.supplied_history_hash
            || previous.supplied_history_bytes != current.supplied_history_bytes
            || !current.coverage_reads.starts_with(&previous.coverage_reads)
            || !current.artifact_reads.starts_with(&previous.artifact_reads)
            || !current.queries.starts_with(&previous.queries)
            || !current.citations.starts_with(&previous.citations)
            || !previous
                .selected_facts
                .iter()
                .all(|fact| current.selected_facts.contains(fact))
            || !previous
                .receipt_references
                .iter()
                .all(|receipt| current.receipt_references.contains(receipt))
        {
            return Err(InvestigationError::InvalidInput);
        }
        let added = &current.citations[previous.citations.len()..];
        let valid = match self.completed_tool {
            Some(InvestigationTool::QueryContext) => {
                current.coverage_reads == previous.coverage_reads
                    && current.artifact_reads == previous.artifact_reads
                    && current.queries.len() == previous.queries.len() + 1
                    && added.iter().all(|citation| {
                        matches!(citation.origin, InvestigationEvidenceOrigin::GraphMetadata)
                    })
            }
            Some(InvestigationTool::ReadEvidence) => {
                current.coverage_reads == previous.coverage_reads
                    && current.artifact_reads == previous.artifact_reads
                    && current.queries == previous.queries
                    && current.selected_facts == previous.selected_facts
                    && added.len() == 1
                    && !matches!(added[0].origin, InvestigationEvidenceOrigin::GraphMetadata)
            }
            Some(InvestigationTool::ReadCoverageInventory) => {
                current.schema_version == 2
                    && current.coverage_reads.len() == previous.coverage_reads.len() + 1
                    && current.artifact_reads == previous.artifact_reads
                    && current.queries == previous.queries
                    && current.selected_facts == previous.selected_facts
                    && current.receipt_references == previous.receipt_references
                    && current.citations == previous.citations
            }
            Some(InvestigationTool::ReadArtifact) => {
                current.schema_version == 2
                    && current.artifact_reads.len() == previous.artifact_reads.len() + 1
                    && current.coverage_reads == previous.coverage_reads
                    && current.queries == previous.queries
                    && current.selected_facts == previous.selected_facts
                    && current.receipt_references == previous.receipt_references
                    && current.citations == previous.citations
            }
            None => false,
        };
        if valid {
            Ok(())
        } else {
            Err(InvestigationError::InvalidInput)
        }
    }

    #[cfg(test)]
    pub(super) fn progress(
        &self,
        current: &InvestigationInputLedger,
        compact: &serde_json::Value,
    ) -> Result<InvestigationTurnProgress, InvestigationError> {
        self.progress_with_policy(current, compact, &INVESTIGATION_V1_POLICY)
    }
    pub(super) fn progress_with_policy(
        &self,
        current: &InvestigationInputLedger,
        compact: &serde_json::Value,
        policy: &InvestigationPolicy,
    ) -> Result<InvestigationTurnProgress, InvestigationError> {
        self.validate_with_policy(current, policy)?;
        let (phase, last_completed_action) = match self.completed_tool {
            None => (InvestigationTurnPhase::Initial, None),
            Some(InvestigationTool::QueryContext) => (
                InvestigationTurnPhase::AfterQuery,
                Some(self.query_result(current, compact)?),
            ),
            Some(InvestigationTool::ReadEvidence) => (
                InvestigationTurnPhase::AfterRead,
                Some(self.read_result(current)?),
            ),
            Some(InvestigationTool::ReadCoverageInventory) => {
                let read_index = current
                    .coverage_reads
                    .len()
                    .checked_sub(1)
                    .ok_or(InvestigationError::InvalidInput)?;
                let read = &current.coverage_reads[read_index];
                (
                    InvestigationTurnPhase::AfterCoverageRead,
                    Some(InvestigationCompletedAction::ReadCoverageInventory {
                        read_index,
                        repo: read.request.repo.clone(),
                        inventory_hash: read.request.inventory_hash.clone(),
                        response_bytes: read.response_bytes,
                    }),
                )
            }
            Some(InvestigationTool::ReadArtifact) => {
                let read_index = current
                    .artifact_reads
                    .len()
                    .checked_sub(1)
                    .ok_or(InvestigationError::InvalidInput)?;
                let read = &current.artifact_reads[read_index];
                (
                    InvestigationTurnPhase::AfterArtifactRead,
                    Some(InvestigationCompletedAction::ReadArtifact {
                        read_index,
                        artifact_id: read.request.artifact_id.clone(),
                        content_id: read.content_id.clone(),
                        offset: read.request.offset,
                        next_offset: read.next_offset,
                        response_bytes: read.response_bytes,
                    }),
                )
            }
        };
        Ok(InvestigationTurnProgress {
            contract: if current.schema_version == 2 {
                "investigation-turn-progress@2"
            } else {
                "investigation-turn-progress@1"
            },
            input_revision: current.revision,
            phase,
            last_completed_action,
            raw_query_budget: InvestigationQueryBudget::from_ledger(current)?,
        })
    }

    fn query_result(
        &self,
        current: &InvestigationInputLedger,
        compact: &serde_json::Value,
    ) -> Result<InvestigationCompletedAction, InvestigationError> {
        let previous = self
            .previous_ledger
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        let query_index = previous.queries.len();
        let last = &current.queries[query_index];
        let context = compact
            .get("queries")
            .and_then(|q| q.get(query_index))
            .and_then(|q| q.get("context"))
            .ok_or(InvestigationError::InvalidInput)?;
        let next_cursor = serde_json::from_value(
            context
                .get("next_cursor")
                .ok_or(InvestigationError::InvalidInput)?
                .clone(),
        )
        .map_err(|_| InvestigationError::InvalidInput)?;
        let previous_facts: BTreeSet<_> = previous
            .selected_facts
            .iter()
            .map(|fact| &fact.fact)
            .collect();
        let new_fact_count = current
            .selected_facts
            .iter()
            .filter(|fact| !previous_facts.contains(&fact.fact))
            .count();
        let repeated_query = previous
            .queries
            .iter()
            .any(|query| query.query == last.query);
        let unchanged_result = previous.queries.iter().any(|query| {
            query.query == last.query
                && query.response_hash == last.response_hash
                && query.response_bytes == last.response_bytes
        });
        Ok(InvestigationCompletedAction::QueryContext {
            query_index,
            query: last.query.clone(),
            response_hash: last.response_hash.clone(),
            returned_facts: last.returned_facts,
            new_fact_count,
            total_selected: last.total_selected,
            next_cursor,
            repeated_query,
            unchanged_result,
            no_progress: repeated_query && unchanged_result && new_fact_count == 0,
        })
    }

    fn read_result(
        &self,
        current: &InvestigationInputLedger,
    ) -> Result<InvestigationCompletedAction, InvestigationError> {
        let previous = self
            .previous_ledger
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        let citation = &current.citations[previous.citations.len()];
        let source = citation
            .source
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        let previously_acquired = previous.citations.iter().any(|old| {
            old.fact == citation.fact
                && old.role == citation.role
                && old.index == citation.index
                && old.source == citation.source
                && old.text_hash == citation.text_hash
                && old.origin == citation.origin
        });
        Ok(InvestigationCompletedAction::ReadEvidence {
            fact: citation.fact.clone(),
            role: citation.role.ok_or(InvestigationError::InvalidInput)?,
            index: citation.index.ok_or(InvestigationError::InvalidInput)?,
            citation_id: citation.citation_id.clone(),
            source_range_bytes: source
                .byte_end
                .checked_sub(source.byte_start)
                .ok_or(InvestigationError::InvalidInput)?,
            previously_acquired,
        })
    }
}
