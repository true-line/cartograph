//! Host-only staged captured reads. No durable authority or publication is created
//! here. The owning worker must bind the callback to an exact durable validation
//! charge and keep the returned guards until its commit acknowledgement.
use super::*;

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
}

/// Borrowed exact charge request, not a charge receipt. An Ok callback is trusted
/// host-adapter acknowledgement only; it must come from durable charge validation
/// in the eventual reservation integration, never from model/caller data.
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
    ReadUnavailable,
    KnownReadFailure(HostError),
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
    /// Metadata only: receipts and capture manifests, never task_text/object bytes.
    pub(in crate::investigations) fn prepare_captured_source(
        &self,
        state: &AppState,
        target: &InvestigationInspectionTarget,
    ) -> Result<PreparedCapturedSource, HostError> {
        target.validate()?;
        self.ledger.validate_with_policy(self.policy)?;
        if self.evidence.len() >= self.policy.limits().evidence_items {
            return Err(HostError::LimitExceeded);
        }
        self.ledger
            .revision
            .checked_add(1)
            .ok_or(HostError::LimitExceeded)?;
        let evidence_bytes = source_headroom(self, target)?;
        let base_ledger_hash = self.ledger.fingerprint_with_policy(self.policy)?;
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
        };
        prepared.revalidate(self, state)?;
        Ok(prepared)
    }
}
impl PreparedCapturedSource {
    pub(in crate::investigations) fn target(&self) -> &InvestigationInspectionTarget {
        &self.target
    }
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
        validate_receipt_fact(&receipt, context, &self.target.fact)?;
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
    pub(in crate::investigations) fn read(
        self,
        context: &FrozenContext,
        state: &AppState,
        charge: impl FnOnce(SourceValidationRequest<'_>) -> Result<(), HostError>,
    ) -> Result<PreparedSourceCandidate, PreparedSourceFailure> {
        if let Err(error) = self.revalidate(context, state) {
            return Err(self.failed(SourceReadFailureKind::BeforeAccess(error)));
        }
        if let Err(error) = charge(self.validation_request()) {
            return Err(self.failed(SourceReadFailureKind::BeforeAccess(error)));
        }
        // A charge may have committed even when a subsequent check fails. The
        // failure stage does not release/refund it; durable settlement must query
        // its actual witness instead of treating worker memory as authority.
        if let Err(error) = self.revalidate(context, state) {
            return Err(self.failed(SourceReadFailureKind::BeforeAccess(error)));
        }
        let Some(guard) = self.guards.guards.get(&self.target.registered_source_id) else {
            return Err(self.failed(SourceReadFailureKind::BeforeAccess(HostError::InvalidInput)));
        };
        #[cfg(test)]
        TEXT_READS.with(|count| count.set(count.get() + 1));
        // AccessStarted begins here. Every task_text error is a known attempted
        // read and can never be classified ReleasedBeforeRead.
        let text = match state.primary_sources.task_text(
            guard,
            &self.receipt,
            self.target.receipt_inventory_index as usize,
        ) {
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
    pub(in crate::investigations) fn kind(&self) -> SourceReadFailureKind {
        self.kind
    }
    pub(in crate::investigations) fn stage(&self) -> SourceAccessStage {
        match self.kind {
            SourceReadFailureKind::BeforeAccess(_) => SourceAccessStage::BeforeAccess,
            _ => SourceAccessStage::AccessStarted,
        }
    }
    pub(in crate::investigations) fn target(&self) -> &InvestigationInspectionTarget {
        self.prepared.target()
    }
    pub(in crate::investigations) fn revalidate(
        &self,
        context: &FrozenContext,
        state: &AppState,
    ) -> Result<(), HostError> {
        self.prepared.revalidate(context, state)
    }
}
impl PreparedSourceCandidate {
    pub(in crate::investigations) fn ledger(&self) -> &InvestigationInputLedger {
        &self.ledger
    }
    pub(in crate::investigations) fn evidence(&self) -> &AgentEvidence {
        &self.evidence
    }
    pub(in crate::investigations) fn target(&self) -> &InvestigationInspectionTarget {
        self.prepared.target()
    }
    pub(in crate::investigations) fn base_ledger(&self) -> &InvestigationInputLedger {
        self.prepared.base_ledger()
    }
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
#[path = "prepared_source_tests.rs"]
mod tests;
