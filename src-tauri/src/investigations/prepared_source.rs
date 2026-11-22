//! Guarded captured-source preparation and access. Metadata preparation and
//! candidate construction publish nothing; reservation commits only an allocation.
//! Text access requires the independent durable validation charge and access fence.
//! The worker retains source guards through the final publication acknowledgement.
use super::*;
use crate::jobs::investigations::{
    CommittedSourceReservation, CommittedSourceValidation, InvestigationStoreError,
    SourceAdmission, SourceSizeBinding, SourceValidationBasis,
};

/// Not Clone/Deserialize: preparation owns the actual retained-source leases.
pub(in crate::investigations) struct PreparedCapturedSource {
    base_ledger: InvestigationInputLedger,
    base_ledger_hash: String,
    policy_profile: &'static str,
    evidence_ordinal: usize,
    evidence_bytes: usize,
    target: InvestigationInspectionTarget,
    selected: BTreeMap<FactKey, TaskFactSelection>,
    receipt: Receipt,
    guards: EvidenceGuards,
    validation: Option<CommittedSourceValidation>,
    read_limits: Option<crate::mcp_reads::dispatch::EvidenceWorkLimits>,
    // Attempt provenance only, never a durable charge or access permit.
    attempted_basis: Option<(String, String, String)>,
}

// The adapter borrows this prepared source, retaining its actual receipt and
// leases. There is no owned or deserializable size-claim adapter.
impl SourceSizeBinding for PreparedCapturedSource {
    fn sizes_for(
        &self,
        target: &InvestigationInspectionTarget,
        ledger: &InvestigationInputLedger,
    ) -> Result<Option<core_redact::DirectSourceJsonContentSizes>, InvestigationError> {
        if target != &self.target || ledger != &self.base_ledger {
            return Err(InvestigationError::InvalidInput);
        }
        self.receipt
            .source_sizes(target.receipt_inventory_index as usize)
            .map_err(|_| InvestigationError::InvalidInput)
    }
}

/// Borrowed exact charge request, not a charge receipt. An Ok callback is trusted
/// host-adapter acknowledgement only; it must come from durable charge validation
/// from the reservation's validation writer, never from model/caller data.
pub(in crate::investigations) struct SourceValidationRequest<'a> {
    pub target: &'a InvestigationInspectionTarget,
    pub base_ledger_hash: &'a str,
    pub validation_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::investigations) enum SourceAccessStage {
    BeforeAccess,
    AccessStarted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::investigations) enum SourceReadFailureKind {
    BeforeAccess(HostError),
    /// Includes unknown commit acknowledgement; never implies a safe retry.
    BeforeStoreAccess(InvestigationStoreError),
    ReadUnavailable,
    KnownReadFailure(HostError),
}
#[derive(Debug, thiserror::Error)]
pub(in crate::investigations) enum SourceReservationError {
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Store(#[from] InvestigationStoreError),
}

enum SourceReadGateError {
    Host(HostError),
    Store(InvestigationStoreError),
}
impl From<HostError> for SourceReadGateError {
    fn from(error: HostError) -> Self {
        Self::Host(error)
    }
}
impl SourceReadGateError {
    fn failure_kind(self) -> SourceReadFailureKind {
        match self {
            Self::Host(error) => SourceReadFailureKind::BeforeAccess(error),
            Self::Store(error) => SourceReadFailureKind::BeforeStoreAccess(error),
        }
    }
}
/// Failure retains the leases. It exposes no prepared descriptor/read retry API.
pub(in crate::investigations) struct PreparedSourceFailure {
    prepared: Box<PreparedCapturedSource>,
    kind: SourceReadFailureKind,
}
/// Exact prospective evidence/ledger, with all leases still held. This neither
/// installs the ledger nor proves prompt capacity, task ownership or cancellation.
pub(in crate::investigations) struct PreparedSourceCandidate {
    prepared: PreparedCapturedSource,
    ledger: InvestigationInputLedger,
    evidence: AgentEvidence,
}

impl FrozenContext {
    /// Resolve a model-selected range using retained receipt metadata from this
    /// frozen ledger. A queried fact need not be an original obligation target.
    pub(in crate::investigations) fn prepare_captured_read(
        &self,
        state: &AppState,
        fact: &TaskFactKey,
        requested_role: TaskRangeRole,
        index: u32,
    ) -> Result<PreparedCapturedSource, HostError> {
        let selection = self
            .ledger
            .selected_facts
            .iter()
            .find(|selected| &selected.fact == fact)
            .ok_or(HostError::InvalidInput)?;
        let keys = self
            .ledger
            .selected_facts
            .iter()
            .map(|selected| crate::task_evidence::graph_fact(&selected.fact))
            .collect::<Vec<_>>();
        let selected = self.select(state, &keys)?;
        drop(keys);
        let guards = EvidenceGuards::acquire(state, &selected)?;
        drop(selected);
        let receipt = guards.receipt(state, selection)?;
        let ordinal = receipt
            .ranges()
            .iter()
            .position(|range| role(range.role) == requested_role && range.index == index)
            .ok_or(HostError::SourceUnavailable)?;
        let target = crate::investigations::inspection::descriptor(
            &receipt,
            &self.ledger.graph_snapshot_id,
            ordinal,
        )
        .map_err(|_| HostError::InvalidInput)?;
        drop(receipt);
        // Re-acquire the complete guard set before releasing these preliminary
        // metadata guards. Preparation rechecks the full exact frozen binding.
        let prepared = self.prepare_captured_source(state, &target)?;
        drop(guards);
        Ok(prepared)
    }
    /// Metadata only: receipts and capture manifests, never task_text/object bytes.
    pub(in crate::investigations) fn prepare_captured_source(
        &self,
        state: &AppState,
        target: &InvestigationInspectionTarget,
    ) -> Result<PreparedCapturedSource, HostError> {
        target.validate()?;
        self.ledger.validate_with_policy(&self.policy)?;
        if self.evidence.len() >= self.policy.limits().evidence_items {
            return Err(HostError::LimitExceeded);
        }
        self.ledger
            .revision
            .checked_add(1)
            .ok_or(HostError::LimitExceeded)?;
        let evidence_bytes = source_headroom(self, target)?;
        let base_ledger_hash = self.ledger.fingerprint_with_policy(&self.policy)?;
        let selection = self
            .ledger
            .selected_facts
            .iter()
            .find(|s| s.fact == target.fact)
            .ok_or(HostError::InvalidInput)?;
        if selection.binding.is_none() {
            return Err(HostError::InvalidInput);
        }
        let keys = self
            .ledger
            .selected_facts
            .iter()
            .map(|s| crate::task_evidence::graph_fact(&s.fact))
            .collect::<Vec<_>>();
        let selected = self.select(state, &keys)?;
        drop(keys);
        let guards = EvidenceGuards::acquire(state, &selected)?;
        let receipt = guards.receipt(state, selection)?;
        let prepared = PreparedCapturedSource {
            base_ledger: self.ledger.clone(),
            base_ledger_hash,
            policy_profile: self.policy.profile(),
            evidence_ordinal: self.evidence.len(),
            evidence_bytes,
            target: target.clone(),
            selected,
            receipt,
            guards,
            validation: None,
            read_limits: None,
            attempted_basis: None,
        };
        prepared.revalidate(self, state)?;
        Ok(prepared)
    }
}
impl PreparedCapturedSource {
    pub(in crate::investigations) fn size_binding(&self) -> &dyn SourceSizeBinding {
        self
    }

    /// Refuse from pinned metadata before reservation/validation charges or text
    /// access. The capture store rechecks the same caps at the actual read.
    pub(in crate::investigations) fn with_read_limits(
        mut self,
        limits: Option<crate::mcp_reads::dispatch::EvidenceWorkLimits>,
    ) -> Result<Self, HostError> {
        if let Some(limits) = limits {
            let captured = &self.target.captured;
            let span = captured
                .byte_end
                .checked_sub(captured.byte_start)
                .ok_or(HostError::InvalidInput)?;
            if captured.file.byte_len > limits.max_file_validation_bytes.get()
                || span > limits.max_span_bytes.get()
            {
                return Err(HostError::LimitExceeded);
            }
        }
        self.read_limits = limits;
        Ok(self)
    }

    fn attempted_basis_matches(&self, basis: &SourceValidationBasis<'_>) -> bool {
        self.attempted_basis
            .as_ref()
            .is_some_and(|(task, step, hash)| {
                task == basis.task_id && step == basis.step_id && hash == basis.reservation_hash
            })
    }
    /// Reserve the exact unread candidate while retaining its actual source
    /// guards. The resulting capability still requires the separate durable
    /// validation charge and access fence before reading source text.
    pub(in crate::investigations) fn reserve(
        &self,
        context: &FrozenContext,
        state: &AppState,
        execution: &crate::job_execution::JobExecution,
        revision: u64,
        clock: &crate::investigations::runtime::RunClock,
        admission: &SourceAdmission<'_>,
    ) -> Result<CommittedSourceReservation, SourceReservationError> {
        self.revalidate(context, state)?;
        if admission.target != &self.target
            || admission.current.ledger() != &self.base_ledger
            || admission.active_milliseconds > clock.active_milliseconds()
        {
            return Err(HostError::InputChanged.into());
        }
        clock.deadline()?;
        let mut retained_error = None;
        let result = state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .admit_next_prompt_source(execution, revision, admission, |view| {
                let checked = clock
                    .deadline()
                    .map(|_| ())
                    .and_then(|()| self.guards.revalidate_in_view(&state.primary_sources, view));
                if let Err(error) = checked {
                    retained_error = Some(error);
                    return Err(InvestigationStoreError::Invalid);
                }
                Ok(())
            });
        if let Some(error) = retained_error {
            return Err(error.into());
        }
        // No context installation or source access occurs, including after an
        // uncertain COMMIT acknowledgement. Callers must stop on store errors.
        Ok(result?)
    }

    pub(in crate::investigations) fn target(&self) -> &InvestigationInspectionTarget {
        &self.target
    }
    #[cfg(test)]
    pub(in crate::investigations) fn base_ledger(&self) -> &InvestigationInputLedger {
        &self.base_ledger
    }
    pub(in crate::investigations) fn validation_request(&self) -> SourceValidationRequest<'_> {
        SourceValidationRequest {
            target: &self.target,
            base_ledger_hash: &self.base_ledger_hash,
            validation_bytes: self.target.captured.file.byte_len,
        }
    }
    /// Frozen association/held-lock recheck, not latest-graph linearization. This
    /// opens no replacement leases and intentionally ignores checkout freshness.
    pub(in crate::investigations) fn revalidate(
        &self,
        context: &FrozenContext,
        state: &AppState,
    ) -> Result<(), HostError> {
        if context.ledger != self.base_ledger
            || context.policy.profile() != self.policy_profile
            || context.evidence.len() != self.evidence_ordinal
        {
            return Err(HostError::InputChanged);
        }
        if source_headroom(context, &self.target)? != self.evidence_bytes {
            return Err(HostError::InputChanged);
        }
        let keys = self.selected.keys().cloned().collect::<Vec<_>>();
        if context.select(state, &keys)? != self.selected {
            return Err(HostError::InputChanged);
        }
        drop(keys);
        self.guards.revalidate(state)?;
        let selected = self
            .base_ledger
            .selected_facts
            .iter()
            .find(|s| s.fact == self.target.fact)
            .ok_or(HostError::InvalidInput)?;
        let receipt = self.guards.receipt(state, selected)?;
        if receipt != self.receipt {
            return Err(HostError::InputChanged);
        }
        validate_receipt_fact(
            &receipt,
            context,
            crate::investigations::context::NO_ROWS,
            &self.target.fact,
        )?;
        if self.target.graph_snapshot_id != self.base_ledger.graph_snapshot_id
            || self.target.fact_digest != selected.fact_digest
        {
            return Err(HostError::InputChanged);
        }
        let ordinal = self.target.receipt_inventory_index as usize;
        let range = receipt
            .ranges()
            .get(ordinal)
            .ok_or(HostError::InvalidInput)?;
        let source = self
            .guards
            .sources
            .get(receipt.repo_key())
            .ok_or(HostError::InvalidInput)?;
        let origin = captured_origin(source, &receipt, ordinal)?;
        if range.evidence != self.target.source
            || role(range.role) != self.target.role
            || range.index != self.target.index
            || !matches!(origin,InvestigationEvidenceOrigin::CapturedPrimarySource {
                registered_source_id,receipt_id,receipt_inventory_index,captured,..}
                if registered_source_id==self.target.registered_source_id && receipt_id==self.target.receipt_id
                    && receipt_inventory_index==self.target.receipt_inventory_index && captured==self.target.captured)
        {
            return Err(HostError::InputChanged);
        }
        let guard = self
            .guards
            .guards
            .get(&self.target.registered_source_id)
            .ok_or(HostError::InvalidInput)?;
        // Inventory is metadata only; presence does not validate captured TEXT.
        state
            .primary_sources
            .task_capture_present(guard, &self.target.captured)
            .map_err(pinned_error)
    }
    /// Bind charge and immediate access checks to the real execution owner.
    /// The supplied expected identities are rechecked against saved authority;
    /// no receipt from a different task/step/reservation can authorize access.
    pub(in crate::investigations) fn read_reserved(
        mut self,
        context: &FrozenContext,
        state: &AppState,
        execution: &crate::job_execution::JobExecution,
        basis: &SourceValidationBasis<'_>,
    ) -> Result<PreparedSourceCandidate, PreparedSourceFailure> {
        // Retain the attempted immutable identity even when the charge commits
        // but its acknowledgement is lost. Cleanup must not swap reservations.
        self.attempted_basis = Some((
            basis.task_id.to_owned(),
            basis.step_id.to_owned(),
            basis.reservation_hash.to_owned(),
        ));
        self.read_after_charge(
            context,
            state,
            |request| {
                let receipt = state
                    .jobs
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .charge_prepared_source_validation(
                        execution,
                        basis,
                        request.target,
                        request.base_ledger_hash,
                    )
                    .map_err(SourceReadGateError::Store)?;
                if !receipt.matches(
                    basis,
                    request.target,
                    request.base_ledger_hash,
                    request.validation_bytes,
                ) {
                    return Err(HostError::InputChanged.into());
                }
                #[cfg(test)]
                AFTER_CHARGE.with(|slot| {
                    if let Some(hook) = slot.borrow_mut().take() {
                        hook();
                    }
                });
                Ok(Some(receipt))
            },
            |receipt| {
                let receipt = receipt.ok_or(HostError::InvalidInput)?;
                state
                    .jobs
                    .lock()
                    .map_err(|_| HostError::Operational)?
                    .verify_source_validation_access(execution, receipt)
                    .map_err(SourceReadGateError::Store)
            },
        )
    }

    // Stage/byte-equivalence tests do not fabricate production charge receipts.
    #[cfg(test)]
    fn read(
        self,
        context: &FrozenContext,
        state: &AppState,
        charge: impl FnOnce(SourceValidationRequest<'_>) -> Result<(), HostError>,
    ) -> Result<PreparedSourceCandidate, PreparedSourceFailure> {
        self.read_after_charge(
            context,
            state,
            |request| charge(request).map(|()| None).map_err(Into::into),
            |_| Ok(()),
        )
    }

    fn read_after_charge(
        mut self,
        context: &FrozenContext,
        state: &AppState,
        charge: impl FnOnce(
            SourceValidationRequest<'_>,
        ) -> Result<Option<CommittedSourceValidation>, SourceReadGateError>,
        before_access: impl FnOnce(
            Option<&CommittedSourceValidation>,
        ) -> Result<(), SourceReadGateError>,
    ) -> Result<PreparedSourceCandidate, PreparedSourceFailure> {
        if let Err(error) = self.revalidate(context, state) {
            return Err(self.failed(SourceReadFailureKind::BeforeAccess(error)));
        }
        match charge(self.validation_request()) {
            Ok(receipt) => self.validation = receipt,
            Err(error) => return Err(self.failed(error.failure_kind())),
        }
        // A charge may have committed even when a subsequent check fails. The
        // failure stage does not release/refund it; durable settlement must query
        // its actual witness instead of treating worker memory as authority.
        if let Err(error) = self.revalidate(context, state) {
            return Err(self.failed(SourceReadFailureKind::BeforeAccess(error)));
        }
        if let Err(error) = before_access(self.validation.as_ref()) {
            return Err(self.failed(error.failure_kind()));
        }
        let Some(guard) = self.guards.guards.get(&self.target.registered_source_id) else {
            return Err(self.failed(SourceReadFailureKind::BeforeAccess(HostError::InvalidInput)));
        };
        #[cfg(test)]
        TEXT_READS.with(|count| count.set(count.get() + 1));
        // AccessStarted begins here. Every task_text error is a known attempted
        // read and can never be classified ReleasedBeforeRead.
        let index = self.target.receipt_inventory_index as usize;
        let read = match self.read_limits {
            Some(limits) => state.primary_sources.task_text_bounded(
                guard,
                &self.receipt,
                index,
                limits.max_file_validation_bytes.get(),
                limits.max_span_bytes.get(),
            ),
            None => state.primary_sources.task_text(guard, &self.receipt, index),
        };
        let text = match read {
            Ok(text) => text,
            Err(PinnedReadError::Unavailable) => {
                return Err(self.failed(SourceReadFailureKind::ReadUnavailable));
            }
            Err(error) => {
                return Err(
                    self.failed(SourceReadFailureKind::KnownReadFailure(pinned_error(error)))
                );
            }
        };
        if text.len() as u64 != self.target.captured.byte_end - self.target.captured.byte_start {
            return Err(self.failed(SourceReadFailureKind::KnownReadFailure(
                HostError::InvalidInput,
            )));
        }
        // Receipt identity authenticates a stored claim, not the correctness of
        // its sizes. Recompute only after the charged, digest-verified read and
        // reject understatement before source can enter context (AC-0287).
        let measured_fits = self.receipt.source_sizes_fit(index, &text).unwrap_or(false);
        if !measured_fits {
            return Err(self.failed(SourceReadFailureKind::KnownReadFailure(
                HostError::InvalidInput,
            )));
        }
        let result = (|| {
            let registered = self
                .guards
                .sources
                .get(&self.target.source.repo)
                .ok_or(HostError::InvalidInput)?;
            let origin = captured_origin(
                registered,
                &self.receipt,
                self.target.receipt_inventory_index as usize,
            )?;
            append_source_evidence(
                context,
                self.target.fact.clone(),
                self.target.fact_digest.clone(),
                self.target.role,
                self.target.index,
                self.target.source.clone(),
                text,
                origin,
            )
        })();
        match result {
            Ok((ledger, evidence)) => Ok(PreparedSourceCandidate {
                prepared: self,
                ledger,
                evidence,
            }),
            Err(error) => Err(self.failed(SourceReadFailureKind::KnownReadFailure(error))),
        }
    }
    fn failed(self, kind: SourceReadFailureKind) -> PreparedSourceFailure {
        PreparedSourceFailure {
            prepared: Box::new(self),
            kind,
        }
    }
}

/// Bound the actual already-held source text and next exact span before cloning
/// a ledger or reading/allocating the next source response. Durable task usage
/// can include failed reads, so its independent budget check remains mandatory.
fn source_headroom(
    context: &FrozenContext,
    target: &InvestigationInspectionTarget,
) -> Result<usize, HostError> {
    let bytes = context
        .evidence
        .iter()
        .try_fold(0usize, |bytes, evidence| {
            bytes.checked_add(evidence.text.len())
        })
        .ok_or(HostError::LimitExceeded)?;
    let span = usize::try_from(target.captured.byte_end - target.captured.byte_start)
        .map_err(|_| HostError::LimitExceeded)?;
    if bytes
        .checked_add(span)
        .is_none_or(|total| total > context.policy.limits().evidence_bytes)
    {
        return Err(HostError::LimitExceeded);
    }
    Ok(bytes)
}
impl PreparedSourceFailure {
    #[cfg(test)]
    pub(in crate::investigations) fn validation_receipt(
        &self,
    ) -> Option<&CommittedSourceValidation> {
        self.prepared.validation.as_ref()
    }
    pub(in crate::investigations) fn kind(&self) -> SourceReadFailureKind {
        self.kind
    }
    pub(in crate::investigations) fn stage(&self) -> SourceAccessStage {
        match self.kind {
            SourceReadFailureKind::BeforeAccess(_)
            | SourceReadFailureKind::BeforeStoreAccess(_) => SourceAccessStage::BeforeAccess,
            _ => SourceAccessStage::AccessStarted,
        }
    }
    #[cfg(test)]
    pub(in crate::investigations) fn target(&self) -> &InvestigationInspectionTarget {
        self.prepared.target()
    }
    #[cfg(test)]
    pub(in crate::investigations) fn revalidate(
        &self,
        context: &FrozenContext,
        state: &AppState,
    ) -> Result<(), HostError> {
        self.prepared.revalidate(context, state)
    }
}
impl PreparedSourceCandidate {
    #[cfg(test)]
    pub(in crate::investigations) fn validation_receipt(
        &self,
    ) -> Option<&CommittedSourceValidation> {
        self.prepared.validation.as_ref()
    }
    #[cfg(test)]
    pub(in crate::investigations) fn ledger(&self) -> &InvestigationInputLedger {
        &self.ledger
    }
    #[cfg(test)]
    pub(in crate::investigations) fn evidence(&self) -> &AgentEvidence {
        &self.evidence
    }
    #[cfg(test)]
    pub(in crate::investigations) fn target(&self) -> &InvestigationInspectionTarget {
        self.prepared.target()
    }
    #[cfg(test)]
    pub(in crate::investigations) fn base_ledger(&self) -> &InvestigationInputLedger {
        self.prepared.base_ledger()
    }
    #[cfg(test)]
    pub(in crate::investigations) fn revalidate(
        &self,
        context: &FrozenContext,
        state: &AppState,
    ) -> Result<(), HostError> {
        self.prepared.revalidate(context, state)
    }
}
#[cfg(test)]
thread_local! { static TEXT_READS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)}; }
#[cfg(test)]
thread_local! { static AFTER_CHARGE:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=const {std::cell::RefCell::new(None)}; }

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "prepared_source_tests.rs"]
mod tests;

#[path = "prepared_source_publication.rs"]
pub(in crate::investigations) mod publication;
