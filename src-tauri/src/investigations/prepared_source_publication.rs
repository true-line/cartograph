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
    #[cfg(test)]
    pub input: InvestigationInput,
    #[cfg(test)]
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
    #[cfg(test)]
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
            Ok((published, refused)) => {
                if let Self::Acquired(candidate) = self {
                    let PreparedSourceCandidate {
                        ledger, evidence, ..
                    } = *candidate;
                    // Capacity was reserved before entering the transaction;
                    // these are moves, with no fallible work after acknowledgement.
                    if refused {
                        context.withheld.insert(evidence.id.clone());
                    }
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
    ) -> Result<(PublishedSourceContinuation, bool), SourcePublicationError> {
        self.validate(context, state)?;
        // #679: the firewall decides the acquired representation toward this
        // provider. A refused read is withheld from the next prompt, and its
        // witness, bound to the real acquired attempt id, settles with it.
        let acquired = match self {
            Self::Acquired(candidate) => Some((
                llm::read_deny::ReadIdentity {
                    repo: candidate.evidence.source.repo.clone(),
                    path: candidate.evidence.source.path.clone(),
                    commit_sha: candidate.evidence.source.commit_sha.clone(),
                    byte_start: candidate.evidence.source.byte_start,
                    byte_end: candidate.evidence.source.byte_end,
                },
                candidate.evidence.text.as_bytes(),
                candidate.evidence.id.clone(),
            )),
            Self::Unavailable(_) => None,
        };
        let decide = |attempt_id: &str| match &acquired {
            Some((read, text, _)) => {
                input
                    .firewall
                    .evaluate_read(input.provider, attempt_id, read, text)
            }
            None => llm::read_deny::ReadDecision::Allowed,
        };
        // The decision depends on the provider and path only; the attempt id
        // binds the witness the settlement derives in its own transaction.
        let refused =
            acquired.is_some() && matches!(decide(""), llm::read_deny::ReadDecision::Refused(_));
        let refusal: Option<crate::jobs::investigations::ReadDecider<'_>> =
            acquired.is_some().then_some(&decide as _);
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
                refusal,
            })?;
        if snapshot.base_ledger() != &context.ledger || input.current.ledger() != &context.ledger {
            return Err(HostError::InputChanged.into());
        }
        let sizes = if receipt
            .detail()
            .summary
            .specialist_id
            .uses_exact_source_reservation()
        {
            self.prepared()
                .size_binding()
                .sizes_for(&self.prepared().target, input.current.ledger())
                .map_err(HostError::from)?
        } else {
            None
        };
        if let Some(sizes) = sizes {
            input
                .proof
                .verify_measured_with_policy(
                    input.current,
                    input.current_prepared,
                    &self.prepared().target,
                    input.current_prepared.profile(),
                    input.current_prepared.limits(),
                    snapshot.next_action_id(),
                    &context.policy,
                    sizes,
                )
                .map_err(HostError::from)?;
        } else {
            input
                .proof
                .verify_with_policy(
                    input.current,
                    input.current_prepared,
                    &self.prepared().target,
                    input.current_prepared.profile(),
                    input.current_prepared.limits(),
                    snapshot.next_action_id(),
                    &context.policy,
                )
                .map_err(HostError::from)?;
        }

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
        let mut withheld = context.withheld.clone();
        if refused && let Some((_, _, id)) = &acquired {
            withheld.insert(id.clone());
        }
        let candidate = InvestigationInput::prepare_v9_withholding_with_policy(
            &snapshot.request().question,
            snapshot.detail().summary.specialist_id,
            snapshot.ledger().clone(),
            &pages,
            &crate::investigations::context::supplied(&evidence, &withheld),
            input.supplied_history,
            &snapshot.detail().usage,
            snapshot.basis(),
            snapshot.request(),
            snapshot.metadata(),
            &inventories,
            snapshot.mode(),
            snapshot.capacity_refusal_count(),
            snapshot.capacity_feedback(),
            &context.policy,
            &withheld,
        )
        .map_err(|error| {
            if error == InvestigationError::LimitExceeded {
                SourcePublicationError::BoundInvariant
            } else {
                SourcePublicationError::Host(HostError::from(error))
            }
        })?;
        let candidate = snapshot
            .attach_acquisition(candidate)
            .map_err(HostError::from)?;
        // The candidate owns its prompt; construction-only copies need not
        // overlap firewall preparation or durable source settlement.
        drop(evidence);
        drop(pages);
        drop(inventories);
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
            .seal_disclosure_with_policy(&prepared, &context.policy)
            .map_err(|_| SourcePublicationError::BoundInvariant)?
            .ok_or(SourcePublicationError::BoundInvariant)?;
        if serde_json::to_vec(&disclosure)
            .map_err(|_| HostError::InvalidInput)?
            .len()
            > input.proof.disclosure_upper_bytes()
        {
            return Err(SourcePublicationError::BoundInvariant);
        }
        drop(disclosure);
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
        let saved_revision = snapshot.saved_revision();
        // Publication rederives its saved authority. Keep the actual outcome
        // and its guards, but release the completed preparation projections.
        drop(action);
        drop(snapshot);
        self.validate(context, state)?;
        clock.deadline()?;
        let mut retained_error = None;
        let result = state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .publish_next_prompt_source(
                execution,
                saved_revision,
                SourceContinuation {
                    size_binding: Some(self.prepared().size_binding()),
                    current: input.current,
                    current_prepared: input.current_prepared,
                    candidate: &candidate,
                    candidate_prepared: &prepared,
                    proof: input.proof,
                    refusal,
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
        Ok((
            PublishedSourceContinuation {
                detail,
                #[cfg(test)]
                input: candidate,
                #[cfg(test)]
                prepared,
            },
            refused,
        ))
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
