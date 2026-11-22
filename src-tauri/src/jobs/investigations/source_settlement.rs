//! Source-only settlement projections. A plan is neither a source-read witness
//! nor a writer/owner capability. Real reads and orphan lock proofs belong to
//! the owning adapter; all effects remain uncommitted until its transaction.
use super::*;
use llm::bounded::PreparedBoundedCompletion;
use read_reservations::{Disposition, DispositionKind, SavedRead};

/// Host-classified known outcomes only; owner loss has a separate lock proof.
#[derive(Clone, Copy)]
pub enum KnownSourceDisposition {
    ReleasedBeforeRead,
    KnownReadDiscarded,
    BoundInvariantDefect,
}

pub struct SourceContinuation<'a> {
    pub size_binding: Option<&'a dyn SourceSizeBinding>,
    pub current: &'a InvestigationInput,
    pub current_prepared: &'a PreparedBoundedCompletion,
    pub candidate: &'a InvestigationInput,
    pub candidate_prepared: &'a PreparedBoundedCompletion,
    pub proof: &'a UnreadSourcePreviewBound,
}

pub(super) enum SourceSettlement<'a> {
    Acquired(SourceContinuation<'a>),
    Unavailable {
        reason: UnavailableReason,
        continuation: SourceContinuation<'a>,
    },
    /// Classification must come from the actual access stage, never a model
    /// proposal. OutcomeUnknown additionally requires real owner-loss proof.
    Terminal(DispositionKind),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SettlementCounts {
    pub events: u64,
    pub attempts: usize,
    pub input_rows: usize,
    pub reference_rows: usize,
    pub disposition_rows: usize,
}

/// Exact private production DTOs; no Clone/Deserialize or public constructor.
/// Sizing-only fabricated hash placeholders never enter this type's public
/// construction path. The writer must revalidate its saved basis and consume
/// its own staged allocation before persisting these same DTOs.
pub(super) struct SettlementPlan {
    before_hash: String,
    projection: SettlementProjection,
}
struct SettlementProjection {
    task: StoredTask,
    disposition: Disposition,
    events: Vec<InvestigationEvent>,
    ledger: Option<InvestigationInputLedger>,
    attempts: Vec<completion::AttemptEntry>,
    attempt_offset: usize,
    additional_bytes: usize,
    counts: SettlementCounts,
}
impl SettlementPlan {
    pub(super) fn terminal(&self) -> bool {
        self.projection.task.phase == Phase::Terminal
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn additional_bytes(&self) -> usize {
        self.projection.additional_bytes
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn counts(&self) -> SettlementCounts {
        self.projection.counts
    }
}

/// Read-only projection against the exact saved pending reservation. This does
/// not prove source access or execution ownership and performs no SQL writes.
pub(super) fn project(
    connection: &Connection,
    before: &StoredTask,
    outcome: SourceSettlement<'_>,
    active_milliseconds: u64,
) -> Result<SettlementPlan, InvestigationStoreError> {
    let saved = source_validation::validate_saved_pending(connection, before)?;
    let call = before
        .admission_state
        .as_ref()
        .and_then(|s| s.current_call.as_ref())
        .ok_or(InvestigationStoreError::Invalid)?;
    let core = admission_contract::load_saved_call_basis_core(connection, before, call)?;
    let response: InvestigationResponseMeta = read_body(
        connection,
        "investigation_responses",
        "step_id",
        &saved.reservation.binding.task_id,
        &call.step_id,
    )?
    .ok_or(InvestigationStoreError::Missing)?;
    if active_milliseconds < before.detail.usage.active_milliseconds {
        return Err(InvestigationStoreError::Invalid);
    }
    let (kind, reason, continuation) = match outcome {
        SourceSettlement::Acquired(c) => (DispositionKind::Acquired, None, Some(c)),
        SourceSettlement::Unavailable {
            reason,
            continuation,
        } => {
            source_unavailable_reason(&reason)?;
            (
                DispositionKind::ReadUnavailable,
                Some(reason),
                Some(continuation),
            )
        }
        SourceSettlement::Terminal(kind) => {
            if matches!(
                kind,
                DispositionKind::Acquired | DispositionKind::ReadUnavailable
            ) {
                return Err(InvestigationStoreError::Invalid);
            }
            (kind, None, None)
        }
    };
    let ledger = match continuation.as_ref() {
        Some(c) => Some(validate_continuation(
            connection,
            before,
            &saved,
            &core.ledger,
            &core.metadata,
            &core.step,
            &response,
            c,
            kind,
            reason.as_ref(),
            active_milliseconds,
        )?),
        None => None,
    };
    Ok(SettlementPlan {
        before_hash: core_prov::content_hash(&encode(before, MAX_RECORD_BYTES)?),
        projection: materialize(
            connection,
            before,
            &saved,
            &response,
            &call.step_id,
            ledger,
            kind,
            reason,
            active_milliseconds,
        )?,
    })
}

#[allow(clippy::too_many_arguments)]
fn materialize(
    connection: &Connection,
    before: &StoredTask,
    saved: &SavedRead,
    response: &InvestigationResponseMeta,
    step_id: &str,
    ledger: Option<InvestigationInputLedger>,
    kind: DispositionKind,
    reason: Option<UnavailableReason>,
    active_milliseconds: u64,
) -> Result<SettlementProjection, InvestigationStoreError> {
    #[cfg(any(test, feature = "test-support"))]
    MATERIALIZATIONS.with(|count| count.set(count.get() + 1));
    let acquired = kind == DispositionKind::Acquired;
    let attempt_outcome = if acquired {
        Some(AttemptOutcome::SourceAcquired {
            citation_id: ledger
                .as_ref()
                .and_then(|l| l.citations.last())
                .ok_or(InvestigationStoreError::Invalid)?
                .citation_id
                .clone(),
        })
    } else {
        reason
            .clone()
            .map(|reason| AttemptOutcome::Unavailable { reason })
    };
    let mut task = before.clone();
    let disposition =
        Disposition::for_reservation(&saved.reservation, saved.charge.as_ref(), kind)?;
    task.detail.usage = usage_for(saved, kind, active_milliseconds)?;
    task.pending = None;
    task.pending_source_read = None;
    task.approved = false;
    task.action_tool = None;
    task.admission_state
        .as_mut()
        .ok_or(InvestigationStoreError::Invalid)?
        .current_call = None;
    task.phase = if matches!(
        kind,
        DispositionKind::Acquired | DispositionKind::ReadUnavailable
    ) {
        Phase::Ready
    } else {
        Phase::Terminal
    };
    if task.phase == Phase::Terminal {
        task.detail.summary.status = if kind == DispositionKind::OutcomeUnknown {
            InvestigationStatus::OutcomeUnknown
        } else if task.detail.summary.cancel_requested {
            InvestigationStatus::Cancelled
        } else {
            InvestigationStatus::Failed
        };
        task.detail.error = Some(terminal_message(kind)?.into());
    }
    if acquired {
        let ledger = ledger.as_ref().ok_or(InvestigationStoreError::Invalid)?;
        task.ledger_hash =
            Some(ledger.fingerprint_with_policy(&super::policy_for_detail(&task.detail)?)?);
        task.ledger_revision = Some(ledger.revision);
        task.detail.citations = ledger.citations.clone();
    }
    bump(connection, &mut task)?;
    let mut events = vec![];
    if matches!(
        kind,
        DispositionKind::Acquired
            | DispositionKind::ReadUnavailable
            | DispositionKind::KnownReadDiscarded
            | DispositionKind::BoundInvariantDefect
    ) {
        events.push(prepare_event(
            &mut task,
            InvestigationEventKind::ToolStarted,
            Some(step_id),
            Some(InvestigationTool::ReadEvidence),
            "A reserved captured-source read executed.",
            timestamp(connection)?,
        )?);
    }
    let (event_kind, message) = if acquired {
        (
            InvestigationEventKind::ToolCompleted,
            "Exact captured source and its completion observations committed together.",
        )
    } else if let Some(reason) = &reason {
        (
            InvestigationEventKind::ReadUnavailable,
            completion::unavailable_message(reason),
        )
    } else {
        (
            if kind == DispositionKind::OutcomeUnknown {
                InvestigationEventKind::OutcomeUnknown
            } else if task.detail.summary.cancel_requested {
                InvestigationEventKind::Cancelled
            } else {
                InvestigationEventKind::Failed
            },
            terminal_message(kind)?,
        )
    };
    events.push(prepare_event(
        &mut task,
        event_kind,
        Some(step_id),
        Some(InvestigationTool::ReadEvidence),
        message,
        timestamp(connection)?,
    )?);
    // Keep the authenticated prefix for observation instead of loading it twice.
    // The builder derives only the same private suffix; it performs no SQL writes.
    let (prior_count, attempts) = completion::project_observation_with_offset(
        connection,
        before,
        &mut task,
        |task, prior_count| {
            let records = match attempt_outcome {
                Some(ref outcome) => completion::derive_attempt_records(
                    task,
                    step_id,
                    response,
                    &saved.reservation.binding.original_read,
                    outcome,
                    ledger.as_ref(),
                    prior_count,
                )?,
                None => vec![],
            };
            completion::project_attempt_entries(
                records,
                response,
                task.detail.summary.last_event_sequence,
            )
        },
    )?;
    super::semantic_audit_state::account_analyst_save(&mut task)?;
    let mut plan = SettlementProjection {
        task,
        disposition,
        events,
        ledger: if acquired { ledger } else { None },
        attempt_offset: prior_count,
        attempts,
        additional_bytes: 0,
        counts: SettlementCounts::default(),
    };
    plan.counts = SettlementCounts {
        events: plan.events.len() as u64,
        attempts: plan.attempts.len(),
        input_rows: usize::from(acquired),
        reference_rows: 0,
        disposition_rows: 1,
    };
    plan.additional_bytes = logical_delta(before, &plan)?;
    Ok(plan)
}

// Exhaustive classification: adding an enum variant requires an explicit
// source-vs-inventory decision and a corresponding sizing-table update. The
// membership check fails closed if a newly classified source reason is missing
// from the exact finite serializer envelope.
const SOURCE_UNAVAILABLE_REASONS: [UnavailableReason; 2] = [
    UnavailableReason::PinnedTargetAbsent,
    UnavailableReason::RangeOutOfBounds,
];
fn source_unavailable_reason(reason: &UnavailableReason) -> Result<(), InvestigationStoreError> {
    match reason {
        UnavailableReason::PinnedTargetAbsent | UnavailableReason::RangeOutOfBounds => {
            if !SOURCE_UNAVAILABLE_REASONS.contains(reason) {
                return Err(InvestigationStoreError::Invalid);
            }
            Ok(())
        }
        UnavailableReason::InventoryExceedsMaxBytes => Err(InvestigationStoreError::Invalid),
    }
}

fn terminal_message(kind: DispositionKind) -> Result<&'static str, InvestigationStoreError> {
    Ok(match kind {
        DispositionKind::ReleasedBeforeRead => {
            "The reserved source read was released before source access; actual validation charges were retained."
        }
        DispositionKind::KnownReadDiscarded => {
            "A captured-source read executed but its candidate was discarded; no acquisition was published."
        }
        DispositionKind::BoundInvariantDefect => {
            "A captured-source read violated its admitted continuation bound; no acquisition was published."
        }
        DispositionKind::OutcomeUnknown => {
            "The reserved source-read outcome is unknown; no read was replayed or classified as acquired."
        }
        _ => return Err(InvestigationStoreError::Invalid),
    })
}

fn logical_delta(
    before: &StoredTask,
    plan: &SettlementProjection,
) -> Result<usize, InvestigationStoreError> {
    let id = &before.detail.summary.investigation_id;
    let saved_step = before
        .pending_source_read
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?
        .step_id
        .as_str();
    let mut bytes = encode(&plan.task, MAX_RECORD_BYTES)?
        .len()
        .saturating_sub(encode(before, MAX_RECORD_BYTES)?.len());
    let add = |total: &mut usize, value: usize| -> Result<(), InvestigationStoreError> {
        *total = total
            .checked_add(value)
            .ok_or(InvestigationStoreError::Capacity)?;
        Ok(())
    };
    add(
        &mut bytes,
        read_reservations::logical_row_bytes(id, saved_step, &plan.disposition)?,
    )?;
    for event in &plan.events {
        add(&mut bytes, encode(event, 16 * 1024)?.len() + 64 + 256)?;
    }
    if let Some(ledger) = &plan.ledger {
        add(
            &mut bytes,
            encode(ledger, MAX_MANIFEST_BYTES)?.len() + 64 + 256,
        )?;
    }
    for entry in &plan.attempts {
        add(
            &mut bytes,
            encode(entry, MAX_RECORD_BYTES)?.len() + 64 + 256,
        )?;
    }
    Ok(bytes)
}

#[cfg(any(test, feature = "test-support"))]
#[path = "source_settlement_tests.rs"]
mod tests;

#[path = "source_settlement_validation.rs"]
mod validation;
use validation::{continuation_metadata, usage_for, validate_continuation, validate_next_ledger};
#[path = "source_settlement_envelope.rs"]
mod envelope_projection;
pub(super) use envelope_projection::{
    SettlementEnvelope, envelope, envelope_with_authenticated_core,
};
#[path = "source_settlement_storage.rs"]
mod storage_writer;
#[cfg(any(test, feature = "test-support"))]
use storage_writer::persist;

#[cfg(unix)]
#[path = "source_snapshot_workspace.rs"]
pub(super) mod snapshot_workspace;

#[cfg(unix)]
#[path = "source_settlement_workspace.rs"]
pub(super) mod writer_workspace;

#[path = "source_continuation_snapshot.rs"]
mod source_snapshot;
pub use source_snapshot::{SourceContinuationOutcome, SourceContinuationSnapshotInput};

#[cfg(any(test, feature = "test-support"))]
pub fn fail_next_publication(lost_ack: bool) {
    storage_writer::fail_once(if lost_ack {
        storage_writer::SettlementFailure::AfterCommitAck
    } else {
        storage_writer::SettlementFailure::AfterTaskSave
    });
}

#[cfg(any(test, feature = "test-support"))]
thread_local! { static MATERIALIZATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(any(test, feature = "test-support"))]
pub(super) fn take_materializations() -> usize {
    MATERIALIZATIONS.with(|count| count.replace(0))
}
#[cfg(any(test, feature = "test-support"))]
pub(super) use envelope_projection::take_envelope_prefix_sweeps;
