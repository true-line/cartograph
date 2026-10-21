//! Publication of actual known source outcomes. Preparation is reversible;
//! installation happens only after the exact owner transaction acknowledges.
use super::*;
use crate::investigations::runtime::RunClock;
use crate::job_execution::JobExecution;
use crate::jobs::investigations::{
    SourceContinuation, SourceContinuationOutcome, SourceContinuationSnapshotInput,
};
use agents::investigation::staged_attempt::UnavailableReason;
use llm::bounded::{BoundedCallError, PreparedBoundedCompletion};
use llm::{EgressFirewall, LlmProvider};

pub(in crate::investigations) struct SourcePublicationInput<'a> {
    pub request: &'a StagedObligationRequest,
    pub current: &'a InvestigationInput,
    pub current_prepared: &'a PreparedBoundedCompletion,
    pub proof: &'a UnreadSourcePreviewBound,
    pub supplied_history: &'a [String],
    pub firewall: &'a EgressFirewall,
    pub provider: &'a dyn LlmProvider,
}

pub(in crate::investigations) struct PublishedSourceContinuation {
    pub detail: InvestigationDetail,
    pub input: InvestigationInput,
    pub prepared: PreparedBoundedCompletion,
}
#[derive(Debug, thiserror::Error)]
pub(in crate::investigations) enum SourcePublicationError {
    #[error("actual source continuation violated its certified bound or protected seal")]
    BoundInvariant,
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Store(#[from] InvestigationStoreError),
    #[error(transparent)]
    Bounded(#[from] BoundedCallError),
}

// This enum and its fields stay private: a failed publication cannot be turned
// back into a candidate and retried, including an uncertain commit acknowledgement.
enum RetainedOutcome {
    Acquired(Box<PreparedSourceCandidate>),
    Unavailable(Box<PreparedSourceFailure>),
}
pub(in crate::investigations) struct SourcePublicationFailure {
    outcome: RetainedOutcome,
    error: SourcePublicationError,
}
impl SourcePublicationFailure {
    pub(in crate::investigations) fn error(&self) -> &SourcePublicationError {
        &self.error
    }
    pub(in crate::investigations) fn stage(&self) -> SourceAccessStage {
        match &self.outcome {
            RetainedOutcome::Acquired(_) => SourceAccessStage::AccessStarted,
            RetainedOutcome::Unavailable(failure) => failure.stage(),
        }
    }
    pub(in crate::investigations) fn validation_receipt(
        &self,
    ) -> Option<&CommittedSourceValidation> {
        self.outcome.prepared().validation.as_ref()
    }
}
impl RetainedOutcome {
    fn prepared(&self) -> &PreparedCapturedSource {
        match self {
            Self::Acquired(candidate) => &candidate.prepared,
            Self::Unavailable(failure) => &failure.prepared,
        }
    }
    fn ledger<'a>(&'a self, context: &'a FrozenContext) -> &'a InvestigationInputLedger {
        match self {
            Self::Acquired(candidate) => &candidate.ledger,
            Self::Unavailable(_) => &context.ledger,
        }
    }
    fn outcome(&self) -> SourceContinuationOutcome {
        match self {
            Self::Acquired(_) => SourceContinuationOutcome::Acquired,
            Self::Unavailable(_) => {
                SourceContinuationOutcome::Unavailable(UnavailableReason::PinnedTargetAbsent)
            }
        }
    }
    fn validate(&self, context: &FrozenContext, state: &AppState) -> Result<(), HostError> {
        self.prepared().revalidate(context, state)?;
        if self.prepared().validation.is_none() {
            return Err(HostError::InvalidInput);
        }
        match self {
            Self::Acquired(candidate) => {
                let citation = candidate
                    .ledger
                    .citations
                    .last()
                    .ok_or(HostError::InvalidInput)?;
                if !candidate.prepared.target.matches(citation)
                    || candidate.evidence.id != citation.citation_id
                    || candidate.evidence.source != candidate.prepared.target.source
                    || citation.text_hash.as_deref()
                        != Some(content_hash(candidate.evidence.text.as_bytes()).as_str())
                {
                    return Err(HostError::InvalidInput);
                }
            }
            Self::Unavailable(failure)
                if failure.kind != SourceReadFailureKind::ReadUnavailable =>
            {
                return Err(HostError::InvalidInput);
            }
            Self::Unavailable(_) => {}
        }
        Ok(())
    }
    fn publish(
        self,
        context: &mut FrozenContext,
        state: &AppState,
        execution: &JobExecution,
        clock: &RunClock,
        input: &SourcePublicationInput<'_>,
    ) -> Result<PublishedSourceContinuation, SourcePublicationFailure> {
        let result = self.prepare_and_commit(context, state, execution, clock, input);
        match result {
            Ok(published) => {
                if let Self::Acquired(candidate) = self {
                    let PreparedSourceCandidate {
                        ledger, evidence, ..
                    } = *candidate;
                    // Capacity was reserved before entering the transaction;
                    // these are moves, with no fallible work after acknowledgement.
                    context.evidence.push(evidence);
                    context.ledger = ledger;
                }
                Ok(published)
            }
            Err(error) => Err(SourcePublicationFailure {
                outcome: self,
                error,
            }),
        }
    }
    fn prepare_and_commit(
        &self,
        context: &mut FrozenContext,
        state: &AppState,
        execution: &JobExecution,
        clock: &RunClock,
        input: &SourcePublicationInput<'_>,
    ) -> Result<PublishedSourceContinuation, SourcePublicationError> {
        self.validate(context, state)?;
        clock.deadline()?;
        let receipt = self
            .prepared()
            .validation
            .as_ref()
            .ok_or(HostError::InvalidInput)?;
        let active = clock.active_milliseconds();
        let snapshot = state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .next_source_continuation_snapshot(&SourceContinuationSnapshotInput {
                investigation_id: receipt.task_id(),
                expected_revision: receipt.detail().summary.revision,
                request: input.request,
                candidate_ledger: self.ledger(context),
                outcome: self.outcome(),
                active_milliseconds: active,
            })?;
        if snapshot.base_ledger() != &context.ledger || input.current.ledger() != &context.ledger {
            return Err(HostError::InputChanged.into());
        }
        input
            .proof
            .verify_with_policy(
                input.current,
                input.current_prepared,
                &self.prepared().target,
                input.current_prepared.profile(),
                input.current_prepared.limits(),
                snapshot.next_action_id(),
                context.policy,
            )
            .map_err(HostError::from)?;
        let mut evidence = context.evidence.clone();
        if let Self::Acquired(candidate) = self {
            context
                .evidence
                .try_reserve(1)
                .map_err(|_| HostError::LimitExceeded)?;
            evidence.push(candidate.evidence.clone());
        }
        let pages = context
            .query_pages
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let inventories = context
            .inventory_details
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let candidate = InvestigationInput::prepare_v9_with_policy(
            &snapshot.request().question,
            snapshot.detail().summary.specialist_id,
            snapshot.ledger().clone(),
            &pages,
            &evidence,
            input.supplied_history,
            &snapshot.detail().usage,
            snapshot.basis(),
            snapshot.request(),
            snapshot.metadata(),
            &inventories,
            snapshot.mode(),
            snapshot.capacity_refusal_count(),
            snapshot.capacity_feedback(),
            context.policy,
        )
        .map_err(|error| {
            if error == InvestigationError::LimitExceeded {
                SourcePublicationError::BoundInvariant
            } else {
                SourcePublicationError::Host(HostError::from(error))
            }
        })?;
        let action = candidate
            .action(snapshot.next_action_id().into())
            .map_err(HostError::from)?;
        #[cfg(test)]
        let action = if CORRUPT_PROTECTED_ACTION.with(|flag| flag.replace(false)) {
            let mut action = action;
            let mut prompt: serde_json::Value = serde_json::from_str(&action.payload.prompt)
                .map_err(|_| HostError::InvalidInput)?;
            prompt["schema_version"] = serde_json::json!(0);
            action.payload.prompt =
                serde_json::to_string(&prompt).map_err(|_| HostError::InvalidInput)?;
            action
        } else {
            action
        };
        let prepared = input
            .firewall
            .prepare_bounded(input.provider, &action, input.current_prepared.limits())
            .map_err(|error| {
                if error.code == llm::bounded::FailureCode::InputTooLarge {
                    SourcePublicationError::BoundInvariant
                } else {
                    SourcePublicationError::Bounded(error)
                }
            })?;
        if prepared.profile() != input.current_prepared.profile() {
            return Err(HostError::InvalidInput.into());
        }
        let disclosure = candidate
            .seal_disclosure_with_policy(&prepared, context.policy)
            .map_err(|_| SourcePublicationError::BoundInvariant)?
            .ok_or(SourcePublicationError::BoundInvariant)?;
        if serde_json::to_vec(&disclosure)
            .map_err(|_| HostError::InvalidInput)?
            .len()
            > input.proof.disclosure_upper_bytes()
        {
            return Err(SourcePublicationError::BoundInvariant);
        }
        let measured =
            llm::bounded::measure_bounded_action(prepared.profile(), &action, prepared.limits())
                .map_err(|error| {
                    if error.code == llm::bounded::FailureCode::InputTooLarge {
                        SourcePublicationError::BoundInvariant
                    } else {
                        SourcePublicationError::Bounded(error)
                    }
                })?;
        let bound = if snapshot.mode()
            == agents::investigation::staged_refusal::CompletionCallMode::Choice
        {
            input.proof.continuation().choice
        } else {
            input.proof.continuation().finish_only
        };
        if measured.raw_string_bytes > bound.raw_string_bytes
            || measured.original_payload_bytes > bound.original_payload_bytes
            || measured.redacted_payload_bytes > bound.redacted_payload_bytes
            || measured.original_action_bytes > bound.original_action_bytes
            || measured.egress_fingerprint_bytes > bound.egress_fingerprint_bytes
        {
            return Err(SourcePublicationError::BoundInvariant);
        }
        self.validate(context, state)?;
        clock.deadline()?;
        let mut retained_error = None;
        let result = state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .publish_next_prompt_source(
                execution,
                snapshot.saved_revision(),
                SourceContinuation {
                    current: input.current,
                    current_prepared: input.current_prepared,
                    candidate: &candidate,
                    candidate_prepared: &prepared,
                    proof: input.proof,
                },
                self.outcome(),
                active,
                |view| {
                    let valid = clock.deadline().map(|_| ()).and_then(|()| {
                        self.prepared()
                            .guards
                            .revalidate_in_view(&state.primary_sources, view)
                    });
                    if let Err(error) = valid {
                        retained_error = Some(error);
                        return Err(InvestigationStoreError::Invalid);
                    }
                    Ok(())
                },
            );
        if let Some(error) = retained_error {
            return Err(error.into());
        }
        let detail = result?;
        Ok(PublishedSourceContinuation {
            detail,
            input: candidate,
            prepared,
        })
    }
}
impl PreparedSourceCandidate {
    pub(in crate::investigations) fn publish_acquired(
        self,
        context: &mut FrozenContext,
        state: &AppState,
        execution: &JobExecution,
        clock: &RunClock,
        input: &SourcePublicationInput<'_>,
    ) -> Result<PublishedSourceContinuation, SourcePublicationFailure> {
        RetainedOutcome::Acquired(Box::new(self)).publish(context, state, execution, clock, input)
    }
}
impl PreparedSourceFailure {
    pub(in crate::investigations) fn publish_unavailable(
        self,
        context: &mut FrozenContext,
        state: &AppState,
        execution: &JobExecution,
        clock: &RunClock,
        input: &SourcePublicationInput<'_>,
    ) -> Result<PublishedSourceContinuation, SourcePublicationFailure> {
        RetainedOutcome::Unavailable(Box::new(self))
            .publish(context, state, execution, clock, input)
    }
}

#[path = "prepared_source_cleanup.rs"]
pub(in crate::investigations) mod cleanup;

#[cfg(test)]
thread_local! { static CORRUPT_PROTECTED_ACTION:std::cell::Cell<bool> = const {std::cell::Cell::new(false)}; }
#[cfg(test)]
pub(in crate::investigations) fn corrupt_next_protected_action() {
    CORRUPT_PROTECTED_ACTION.with(|flag| flag.set(true));
}
