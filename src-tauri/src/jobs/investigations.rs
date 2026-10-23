//! Durable investigation coordination on the JobStore state spine (SPEC-10).

use super::{JobStore, JobTransitionError, execution};
use crate::job_execution::{ExecutionNamespace, ExecutionReservation, JobExecution, JobLockTarget};
use agents::investigation::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

#[path = "investigations/admission_contract.rs"]
mod admission_contract;
#[path = "investigations/admission_journal.rs"]
mod admission_journal;
#[path = "investigations/admission_transition.rs"]
mod admission_transition;
#[path = "investigations/completion.rs"]
mod completion;
pub(crate) use completion::NextPromptFinish;
#[path = "investigations/metadata_admission.rs"]
mod metadata_admission;
pub(crate) use metadata_admission::{MetadataAdmission, MetadataContinuationRead};
#[path = "investigations/next_prompt_consent.rs"]
mod next_prompt_consent;
#[path = "investigations/next_prompt_initial.rs"]
mod next_prompt_initial;
#[path = "investigations/next_prompt_invocation.rs"]
mod next_prompt_invocation;
pub(crate) use next_prompt_invocation::NextPromptInvocation;
#[path = "investigations/coverage.rs"]
mod coverage;
#[cfg(test)]
#[path = "investigations/coverage_tests.rs"]
mod coverage_tests;
#[path = "investigations/mcp_clients.rs"]
mod mcp_clients;
#[cfg(test)]
#[path = "investigations/metadata_host_fixture.rs"]
pub(crate) mod metadata_host_fixture;
#[path = "investigations/reads.rs"]
mod reads;
// Internal admission foundation; no historical runtime initializes these tables.
#[path = "investigations/read_reservations.rs"]
mod read_reservations;
#[path = "investigations/source_admission.rs"]
mod source_admission;
#[path = "investigations/source_settlement.rs"]
mod source_settlement;
#[cfg(test)]
pub(crate) use source_admission::lose_next_commit_ack as lose_next_source_admission_ack;
pub(crate) use source_admission::{CommittedSourceReservation, SourceAdmission};
#[path = "investigations/source_validation.rs"]
mod source_validation;
pub(crate) use source_validation::{CommittedSourceValidation, SourceValidationBasis};
#[cfg(test)]
pub(crate) use source_validation::{SourceValidationInitialData, captured_source_fixture};
#[path = "investigations/finding_reviews.rs"]
mod finding_reviews;
#[path = "investigations/staged.rs"]
mod staged;
pub(crate) use finding_reviews::FindingReviewError;
#[path = "investigations/storage.rs"]
mod storage;
#[cfg(test)]
#[path = "investigations/tests.rs"]
mod tests;
#[path = "investigations/transitions.rs"]
mod transitions;
#[path = "investigations/turn.rs"]
mod turn;

use agents::investigation::{staged_attempt::*, staged_finish::*, staged_refusal::*};
use storage::*;

pub(super) use transitions::{attach_execution, cancel_for_job};

pub(crate) const KIND_PREFIX: &str = "investigation-v1:";
const MAX_TASKS: usize = 128;
const MAX_LIVE_TASKS: usize = 2;
const TASK_CAPACITY: usize = 2 * 1024 * 1024;
const TERMINAL_RESERVE: usize = 256 * 1024;
const STORE_CAPACITY: usize = 256 * 1024 * 1024;
// Inline metadata-only disclosure has its own bound plus the legacy step envelope.
// Task/global capacity and MAX_RECORD_BYTES remain authoritative admission caps.
const MAX_INVOCATION_BYTES: usize = MAX_DISCLOSURE_BYTES + 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvestigationStoreError {
    Invalid,
    Missing,
    Conflict,
    Stale,
    Capacity,
    Unavailable,
    Cancelled,
    Ownership,
    Storage,
}

impl std::fmt::Display for InvestigationStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "Investigation metadata is invalid; work stopped.",
            Self::Missing => "Investigation history was not found.",
            Self::Conflict => "This investigation request identity already names different input.",
            Self::Stale => "Investigation state changed; reload its current status.",
            Self::Capacity => {
                "Investigation capacity is exhausted; no additional work was started."
            }
            Self::Unavailable => "The selected investigation provider is unavailable.",
            Self::Cancelled => "Investigation cancellation was requested; no new work may start.",
            Self::Ownership => "Investigation execution ownership could not be verified.",
            Self::Storage => "Investigation storage failed; no successful answer was acknowledged.",
        })
    }
}

impl std::error::Error for InvestigationStoreError {}
impl From<rusqlite::Error> for InvestigationStoreError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}
impl From<InvestigationError> for InvestigationStoreError {
    fn from(_: InvestigationError) -> Self {
        Self::Invalid
    }
}
impl From<JobTransitionError> for InvestigationStoreError {
    fn from(_: JobTransitionError) -> Self {
        Self::Ownership
    }
}

#[derive(Debug)]
pub(crate) struct InvestigationStart {
    pub detail: InvestigationDetail,
    pub created: bool,
}

/// Safe identity and optional metadata-only disclosure. The exact payload/preview
/// exists exclusively in its live worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingInvestigationStep {
    pub step_id: String,
    pub payload_hash: String,
    pub input_ledger_hash: String,
    pub input_bytes: usize,
    pub generated_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosure: Option<InvestigationDisclosure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvestigationResponseMeta {
    pub step_id: String,
    pub response_hash: String,
    pub action_hash: String,
    pub observed_model: Option<String>,
    pub reported_input_tokens: Option<u64>,
    pub reported_output_tokens: Option<u64>,
    pub active_milliseconds: u64,
}

pub(crate) enum InvestigationTransition {
    InputPrepared {
        coverage_archive: Option<Box<InvestigationCoverageArchive>>,
        ledger: Box<InvestigationInputLedger>,
        usage: InvestigationUsage,
    },
    InputAdmitted {
        ledger: Box<InvestigationInputLedger>,
        usage: InvestigationUsage,
    },
    AwaitConsent {
        step: PendingInvestigationStep,
    },
    BeginInvocation {
        step: PendingInvestigationStep,
    },
    ActionAdmitted {
        response: InvestigationResponseMeta,
        tool: Option<InvestigationTool>,
        result: Option<Box<InvestigationResult>>,
    },
    FinishRefused {
        request: Box<StagedObligationRequest>,
        response: InvestigationResponseMeta,
        finish: Box<StagedFinish>,
        input: Box<InvestigationInput>,
    },
    ReadUnavailable {
        read: ReadIntent,
        reason: UnavailableReason,
    },
    BeginTool {
        tool: InvestigationTool,
    },
    ChargeEvidence {
        validation_bytes: u64,
    },
    StopWithElapsed {
        reason: InvestigationStopReason,
        active_milliseconds: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InvestigationStopReason {
    Cancelled,
    PreparationFailed,
    InputChanged,
    SourceUnavailable,
    InvalidResponse,
    FinishOnlyToolProposal,
    LimitExceeded,
    ProviderFailure,
    OutcomeUnknown,
    ConsentExpired,
    ConsentDeclined,
    Operational,
}

impl InvestigationStopReason {
    fn message(self) -> &'static str {
        match self {
            Self::Cancelled => "Cancellation requested; no further work was started.",
            Self::PreparationFailed => "Investigation input preparation failed.",
            Self::InputChanged => "The selected graph or source binding changed.",
            Self::SourceUnavailable => "Selected source evidence is unavailable.",
            Self::InvalidResponse => {
                "The completed provider response was not an admissible action."
            }
            Self::FinishOnlyToolProposal => {
                "The provider proposed a tool when only a finishing response was permitted."
            }
            Self::LimitExceeded => "An investigation work or storage limit was reached.",
            Self::ProviderFailure => "The provider call failed without a usable response.",
            Self::OutcomeUnknown => "A dispatched provider call has no durable admitted outcome.",
            Self::ConsentExpired => "The exact step consent wait expired.",
            Self::ConsentDeclined => {
                "The exact cloud step was declined; no fallback was authorized."
            }
            Self::Operational => {
                "An investigation operation failed; no additional work was started."
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvestigationReceiptUse {
    pub investigation_id: String,
    pub source_id: String,
    pub repo_key: String,
    pub receipt_id: String,
}

/// Private copied recovery identity. A caller may only try its exact OS target,
/// outside all application/database mutexes, then return the retained reservation.
pub(crate) struct InvestigationRecoveryCandidate {
    id: String,
    revision: u64,
    identity: Option<StoredExecution>,
    target: JobLockTarget,
}

impl InvestigationRecoveryCandidate {
    pub(crate) fn lock_target(&self) -> &JobLockTarget {
        &self.target
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Queued,
    Preparing,
    Ready,
    Consent,
    Invocation,
    Action,
    Tool,
    Reservation,
    Terminal,
}

// Deliberately no Debug: private owner tokens are never part of public records.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredExecution {
    namespace: String,
    job_id: i64,
    generation: i64,
    owner: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTask {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admission_state: Option<admission_contract::StoredAdmissionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completion_intent: Option<completion::CompletionIntent>,
    nonce: String,
    intent_hash: String,
    expected_graph_revision: Option<String>,
    detail: InvestigationDetail,
    phase: Phase,
    execution: Option<StoredExecution>,
    ledger_hash: Option<String>,
    ledger_revision: Option<u64>,
    pending: Option<PendingInvestigationStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_source_read: Option<source_validation::PendingSourceRead>,
    approved: bool,
    action_tool: Option<InvestigationTool>,
}

impl JobStore {
    /// Display metadata only; elapsed time and ownership use separate authorities.
    pub(crate) fn investigation_timestamp(
        &self,
        offset_seconds: u32,
    ) -> Result<String, InvestigationStoreError> {
        if offset_seconds > 3600 {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(self.conn.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now',?1)",
            [format!("+{offset_seconds} seconds")],
            |row| row.get(0),
        )?)
    }

    /// Resolve immutable request identity before mutable provider/capacity checks.
    pub(crate) fn start_investigation(
        &mut self,
        request: &StartInvestigationRequest,
        provider: Option<&InvestigationProvider>,
    ) -> Result<InvestigationStart, InvestigationStoreError> {
        let normalized = request.normalized()?;
        self.start_investigation_internal(&normalized, provider, None, false)
    }
    fn start_investigation_internal(
        &mut self,
        request: &StartInvestigationRequest,
        provider: Option<&InvestigationProvider>,
        completion_intent: Option<completion::CompletionIntent>,
        next_prompt: bool,
    ) -> Result<InvestigationStart, InvestigationStoreError> {
        let policy = InvestigationPolicy::for_profile(&request.limit_profile)
            .ok_or(InvestigationStoreError::Invalid)?;
        if request.specialist_id.uses_next_prompt_admission() != next_prompt
            || (next_prompt
                && (request.schema_version != 7
                    || completion_intent
                        .as_ref()
                        .is_none_or(|intent| intent.schema_version != 4)))
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let intent_hash = match &completion_intent {
            Some(intent) => intent.request_hash.clone(),
            None => request.intent_hash()?,
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let prior: Option<Option<String>> = tx.query_row(
            "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))<=128 THEN id END FROM investigation_tasks WHERE nonce = ?1", [&request.request_nonce], |row| row.get(0),
        ).optional()?;
        if let Some(id) = prior {
            let id = id.ok_or(InvestigationStoreError::Invalid)?;
            let record = if next_prompt {
                load_next_prompt_task(&tx, &id)?
            } else {
                load_task(&tx, &id)?
            };
            if record.intent_hash != intent_hash {
                return Err(InvestigationStoreError::Conflict);
            }
            tx.commit()?;
            return Ok(InvestigationStart {
                detail: record.detail,
                created: false,
            });
        }
        if completion_intent.is_some() {
            completion::initialize(&tx)?;
        }
        let provider = provider
            .filter(|p| p.available && p.mode == request.provider_mode)
            .ok_or(InvestigationStoreError::Unavailable)?;
        validate_provider(provider)?;
        check_start_capacity(&tx, request.conversation_id.is_none())?;
        let conversation_id = match &request.conversation_id {
            Some(id) => {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM investigation_conversations WHERE id = ?1)",
                    [id],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(InvestigationStoreError::Missing);
                }
                id.clone()
            }
            None => new_id(&tx, "conversation")?,
        };
        if let Some(parent) = &request.parent_id {
            if next_prompt {
                next_parent_history::load(&tx, parent, &conversation_id)?;
            } else {
                let parent = load_execution_task(&tx, parent)?;
                if parent.detail.summary.conversation_id != conversation_id {
                    return Err(InvestigationStoreError::Invalid);
                }
            }
        }
        let id = new_id(&tx, "investigation")?;
        let now = timestamp(&tx)?;
        if request.conversation_id.is_none() {
            one(tx.execute(
                "INSERT INTO investigation_conversations(id, created_at) VALUES (?1, ?2)",
                params![conversation_id, now],
            )?)?;
        }
        one(tx.execute(
            "INSERT INTO jobs(kind) VALUES (?1)",
            [format!("{KIND_PREFIX}{id}")],
        )?)?;
        let job_id = tx.last_insert_rowid();
        one(tx.execute(
            "INSERT INTO job_attempts(job_id,generation,owner) VALUES (?1,0,NULL)",
            [job_id],
        )?)?;
        let record = StoredTask {
            schema_version: if next_prompt {
                3
            } else if completion_intent.is_some() {
                2
            } else {
                1
            },
            admission_state: next_prompt.then_some(admission_contract::StoredAdmissionState {
                schema_version: 1,
                current_call: None,
            }),
            completion_intent: completion_intent.clone(),
            nonce: request.request_nonce.clone(),
            intent_hash,
            expected_graph_revision: request.expected_graph_revision.clone(),
            detail: InvestigationDetail {
                inspection_requirements: if completion_intent.is_some() {
                    None
                } else {
                    request.inspection_requirements.clone()
                },
                inspection_assessment: None,
                summary: InvestigationSummary {
                    inspection_status: if completion_intent.is_some() {
                        None
                    } else {
                        request
                            .inspection_requirements
                            .as_ref()
                            .map(|_| InvestigationInspectionStatus::Pending)
                    },
                    inspection_requirement_count: if completion_intent.is_some() {
                        None
                    } else {
                        request.inspection_requirements.as_ref().map(Vec::len)
                    },
                    completion_outcome: None,
                    obligation_status: completion_intent
                        .as_ref()
                        .map(|_| CompletionStatus::NotAssessed),
                    schema_version: request.schema_version,
                    investigation_id: id.clone(),
                    conversation_id,
                    parent_id: request.parent_id.clone(),
                    job_id,
                    specialist_id: request.specialist_id,
                    question: redacted_history_text(&request.question, MAX_QUESTION_BYTES)?,
                    scope: request.scope.clone(),
                    provider_mode: request.provider_mode,
                    status: InvestigationStatus::Queued,
                    revision: 0,
                    cancel_requested: false,
                    invocation_pending: false,
                    actions: InvestigationActions {
                        can_cancel: true,
                        can_follow_up: true,
                    },
                    graph_snapshot_id: None,
                    last_event_sequence: 0,
                    has_result: false,
                    created_at: now.clone(),
                    updated_at: now,
                },
                context_owner: self.namespace.value.clone(),
                origin: InvestigationOrigin::App,
                specialist: request.specialist_id.definition()?,
                provider: provider.clone(),
                scope_snapshot_id: None,
                coverage_context: None,
                limits: policy.limits(),
                usage: InvestigationUsage::default(),
                citations: Vec::new(),
                error: None,
                completion: completion_intent.as_ref().map(completion::initial_detail),
            },
            phase: Phase::Queued,
            execution: None,
            ledger_hash: None,
            ledger_revision: None,
            pending: None,
            pending_source_read: None,
            approved: false,
            action_tool: None,
        };
        if next_prompt {
            insert_next_prompt_task(&tx, &record)?;
        } else {
            insert_task(&tx, &record)?;
        }
        if let Some(targets) = &request.inspection_requirements {
            for target in targets {
                tx.execute(
                    "INSERT OR IGNORE INTO investigation_refs VALUES (?1,0,?2,?3,?4)",
                    params![
                        id,
                        target.registered_source_id,
                        target.source.repo,
                        target.receipt_id
                    ],
                )?;
            }
        }
        let mut record = record;
        event(
            &tx,
            &mut record,
            InvestigationEventKind::Created,
            None,
            None,
            "Investigation created.",
        )?;
        if next_prompt {
            save_next_prompt_task(&tx, &record, 0, true)?;
        } else {
            save_task(&tx, &record, 0, true)?;
        }
        check_task_capacity(&tx, &id, false)?;
        tx.commit()?;
        Ok(InvestigationStart {
            detail: record.detail,
            created: true,
        })
    }

    pub(crate) fn investigation(
        &self,
        id: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_execution_task(&tx, id)?;
        tx.commit()?;
        Ok(task.detail)
    }

    /// Worker counterpart of the family-aware public read, requiring a live owner.
    pub(crate) fn investigation_owned_detail(
        &self,
        execution: &JobExecution,
        id: &str,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_execution_task(&tx, id)?;
        transitions::check_identity(&task, execution)?;
        tx.commit()?;
        Ok(task.detail)
    }

    pub(crate) fn investigation_ledger(
        &self,
        id: &str,
    ) -> Result<Option<InvestigationInputLedger>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_execution_task(&tx, id)?;
        let ledger = load_ledger(&tx, &task)?;
        tx.commit()?;
        Ok(ledger)
    }

    pub(crate) fn investigation_result(
        &self,
        id: &str,
    ) -> Result<Option<InvestigationResult>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_execution_task(&tx, id)?;
        let result = load_result(&tx, &task)?;
        tx.commit()?;
        Ok(result)
    }

    /// Owned counterpart for the worker, including next-family consent.
    pub(crate) fn investigation_owned_pending_step(
        &self,
        execution: &JobExecution,
        id: &str,
    ) -> Result<Option<(u64, PendingInvestigationStep, bool)>, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_execution_task(&tx, id)?;
        transitions::check_identity(&task, execution)?;
        let pending = if task.phase == Phase::Consent && !task.detail.summary.cancel_requested {
            task.pending
                .map(|step| (task.detail.summary.revision, step, task.approved))
        } else {
            None
        };
        tx.commit()?;
        Ok(pending)
    }

    pub(crate) fn investigation_pending_step(
        &self,
        id: &str,
    ) -> Result<Option<(u64, PendingInvestigationStep, bool)>, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_execution_task(&tx, id)?;
        let pending = if task.phase == Phase::Consent && !task.detail.summary.cancel_requested {
            task.pending
                .map(|step| (task.detail.summary.revision, step, task.approved))
        } else {
            None
        };
        tx.commit()?;
        Ok(pending)
    }
}

pub(super) fn initialize(
    connection: &mut Connection,
    namespace: &ExecutionNamespace,
) -> rusqlite::Result<()> {
    storage::initialize(connection, namespace).map_err(sql_error)
}

fn sql_error(_: InvestigationStoreError) -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}

fn id_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
}

fn hash_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn one(rows: usize) -> Result<(), InvestigationStoreError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(InvestigationStoreError::Storage)
    }
}

#[cfg(test)]
#[path = "investigations/captured_source_invocation_fixture.rs"]
mod captured_source_invocation_fixture;
#[cfg(test)]
pub(crate) use captured_source_invocation_fixture::CapturedSourceInvocationFixture;

pub(crate) use source_settlement::{SourceContinuationOutcome, SourceContinuationSnapshotInput};

pub(crate) use source_settlement::{KnownSourceDisposition, SourceContinuation};

#[cfg(test)]
pub(crate) use source_settlement::fail_next_publication as fail_next_source_publication;

#[path = "investigations/next_parent_history.rs"]
mod next_parent_history;
