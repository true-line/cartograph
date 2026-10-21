//! Immutable staged read observations. Only a host can attest completion; these
//! validators bind that observation to actual read intent and saved acquisition.
use super::{obligations::*, *};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReadIntent {
    Source {
        fact: crate::TaskFactKey,
        role: crate::TaskRangeRole,
        index: u32,
    },
    Inventory {
        context_id: String,
        repo: String,
        inventory_hash: String,
        max_bytes: usize,
    },
}
impl ReadIntent {
    pub fn matches(&self, obligation: &Obligation) -> bool {
        match (self, obligation) {
            (Self::Source { fact, role, index }, Obligation::Source { target, .. }) => {
                fact == &target.fact && *role == target.role && *index == target.index
            }
            (
                Self::Inventory {
                    context_id,
                    repo,
                    inventory_hash,
                    max_bytes,
                },
                Obligation::Inventory { target, .. },
            ) => {
                context_id == &target.context_id
                    && repo == &target.repo
                    && inventory_hash == &target.inventory_hash
                    && (1..=MAX_QUERY_BYTES).contains(max_bytes)
            }
            _ => false,
        }
    }
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        Ok(crate::source_basis::domain_hash(
            b"cartograph:obligation-read-intent:v1\0",
            &bounded_bytes(self, MAX_ACTION_BYTES)?,
        ))
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    PinnedTargetAbsent,
    RangeOutOfBounds,
    InventoryExceedsMaxBytes,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttemptOutcome {
    SourceAcquired {
        citation_id: String,
    },
    InventoryAcquired {
        read_index: usize,
    },
    Unavailable {
        reason: UnavailableReason,
    },
    /// This references a real completed read and a real firewall refusal
    /// observation. Its authenticity must be rechecked by the host journal.
    FirewallRefused {
        acquired_attempt_id: String,
        observation_hash: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRecord {
    pub schema_version: u32,
    pub attempt_id: String,
    pub task_id: String,
    pub request_hash: String,
    pub obligations_hash: String,
    pub obligation_id: String,
    pub step_id: String,
    pub read: ReadIntent,
    pub input_ledger_hash: String,
    pub input_revision: u64,
    pub outcome: AttemptOutcome,
}
impl AttemptRecord {
    pub fn content_id(&self) -> Result<String, InvestigationError> {
        bounded_bytes(self, MAX_MANIFEST_BYTES)?;
        let mut record = self.clone();
        record.attempt_id.clear();
        Ok(crate::source_basis::domain_hash(
            b"cartograph:obligation-attempt:v1\0",
            &bounded_bytes(&record, MAX_MANIFEST_BYTES)?,
        ))
    }
    /// The ledger is the saved revision named by this record, not a newly
    /// reconstructed/current graph. Completion itself needs durable host evidence.
    pub fn validate(
        &self,
        request: &StagedObligationRequest,
        task: &str,
        ledger: &InvestigationInputLedger,
    ) -> Result<(), InvestigationError> {
        let request = request.normalized()?;
        self.validate_binding(&request.intent_hash()?, &request.obligations, task, ledger)
    }
    pub fn validate_binding(
        &self,
        request_hash: &str,
        envelope: &ObligationEnvelope,
        task: &str,
        ledger: &InvestigationInputLedger,
    ) -> Result<(), InvestigationError> {
        bounded_bytes(self, MAX_MANIFEST_BYTES)?;
        ledger.validate()?;
        let envelope = envelope.normalized()?;
        let obligation = envelope
            .obligations
            .iter()
            .find(|item| item.key().1 == self.obligation_id)
            .ok_or(InvestigationError::InvalidInput)?;
        if self.schema_version != 1
            || self.task_id != task
            || !super::records::opaque_id(task)
            || !super::records::opaque_id(&self.step_id)
            || self.request_hash != request_hash
            || self.obligations_hash != envelope.fingerprint()?
            || self.input_ledger_hash != ledger.fingerprint()?
            || self.input_revision != ledger.revision
            || self.attempt_id != self.content_id()?
            || !self.read.matches(obligation)
        {
            return Err(InvestigationError::InvalidInput);
        }
        let valid = match (&self.outcome, obligation) {
            (AttemptOutcome::SourceAcquired { citation_id }, Obligation::Source { target, .. }) => {
                ledger.citations.iter().any(|citation| {
                    citation.citation_id == *citation_id && target.matches(citation)
                })
            }
            (
                AttemptOutcome::InventoryAcquired { read_index },
                Obligation::Inventory { target, .. },
            ) => ledger.coverage.as_ref().is_some_and(|proof| {
                ledger.coverage_reads.get(*read_index).is_some_and(|read| {
                    read.validate(proof).is_ok()
                        && read.request.context_id == target.context_id
                        && read.request.repo == target.repo
                        && read.request.inventory_hash == target.inventory_hash
                        && self.read
                            == ReadIntent::Inventory {
                                context_id: read.request.context_id.clone(),
                                repo: read.request.repo.clone(),
                                inventory_hash: read.request.inventory_hash.clone(),
                                max_bytes: read.request.max_bytes,
                            }
                })
            }),
            (
                AttemptOutcome::Unavailable {
                    reason: UnavailableReason::InventoryExceedsMaxBytes,
                },
                Obligation::Inventory { .. },
            ) => true,
            (
                AttemptOutcome::Unavailable {
                    reason:
                        UnavailableReason::PinnedTargetAbsent | UnavailableReason::RangeOutOfBounds,
                },
                Obligation::Source { .. },
            ) => true,
            (
                AttemptOutcome::FirewallRefused {
                    acquired_attempt_id,
                    observation_hash,
                },
                _,
            ) => {
                crate::source_basis::hash(acquired_attempt_id)
                    && crate::source_basis::hash(observation_hash)
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(InvestigationError::InvalidInput)
        }
    }
}

/// The admission caller must load and validate each record's completion, action,
/// step and ledger from the host journal. This type never attests a DB operation.
pub struct AttemptEvidence<'a> {
    pub record: &'a AttemptRecord,
    pub ledger: &'a InvestigationInputLedger,
}
