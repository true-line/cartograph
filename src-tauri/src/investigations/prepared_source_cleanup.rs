//! Consuming terminal cleanup of typed host outcomes. No source read, retry,
//! refund, or orphan outcome classification can be requested here.
use super::*;

#[derive(Debug, thiserror::Error)]
pub(in crate::investigations) enum SourceCleanupError {
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Store(#[from] InvestigationStoreError),
}
enum RetainedCleanup {
    Read(Box<PreparedSourceFailure>),
    Publication(Box<SourcePublicationFailure>),
}
pub(in crate::investigations) struct SourceCleanupFailure {
    // Owns source/registry guards through failure handling, even when the
    // caller only inspects the error and then drops the retained outcome.
    _retained: RetainedCleanup,
    error: SourceCleanupError,
}
impl SourceCleanupFailure {
    pub(in crate::investigations) fn error(&self) -> &SourceCleanupError {
        &self.error
    }
    #[cfg(test)]
    pub(in crate::investigations) fn stage(&self) -> SourceAccessStage {
        self._retained.stage()
    }
    #[cfg(test)]
    pub(in crate::investigations) fn validation_receipt(
        &self,
    ) -> Option<&CommittedSourceValidation> {
        self._retained.prepared().validation.as_ref()
    }
}
impl RetainedCleanup {
    fn prepared(&self) -> &PreparedCapturedSource {
        match self {
            Self::Read(failure) => &failure.prepared,
            Self::Publication(failure) => failure.outcome.prepared(),
        }
    }
    fn stage(&self) -> SourceAccessStage {
        match self {
            Self::Read(failure) => failure.stage(),
            Self::Publication(failure) => failure.stage(),
        }
    }
    /// The closed step and error class that discarded an executed read
    /// (AC-1609). A read released before access has none.
    fn discard_diagnostic(&self) -> Option<agents::investigation::InvestigationTerminalDiagnostic> {
        if self.stage() == SourceAccessStage::BeforeAccess {
            return None;
        }
        match self {
            Self::Read(failure) => failure.discard_diagnostic(),
            Self::Publication(failure) => Some(failure.discard_diagnostic()),
        }
    }
    fn settle(
        self,
        state: &AppState,
        execution: &JobExecution,
        reservation: &CommittedSourceReservation,
        clock: &RunClock,
    ) -> Result<InvestigationDetail, SourceCleanupFailure> {
        let result = (|| {
            let prepared = self.prepared();
            let basis = reservation.basis();
            if !reservation.matches(&prepared.target, &prepared.base_ledger_hash)
                || !prepared.attempted_basis_matches(&basis)
                || prepared.validation.as_ref().is_some_and(|receipt| {
                    !receipt.matches(
                        &basis,
                        &prepared.target,
                        &prepared.base_ledger_hash,
                        prepared.target.captured.file.byte_len,
                    )
                })
            {
                return Err(SourceCleanupError::Host(HostError::InvalidInput));
            }
            if self.stage() == SourceAccessStage::AccessStarted && prepared.validation.is_none() {
                return Err(SourceCleanupError::Host(HostError::InvalidInput));
            }
            // Cleanup remains possible after the deadline. This observed time
            // is monotonic-checked and retained by the genuine terminal writer.
            let diagnostic = self.discard_diagnostic();
            let settled = state
                .jobs
                .lock()
                .map_err(|_| HostError::Operational)?
                .discard_next_prompt_source(
                    execution,
                    reservation,
                    if self.stage() == SourceAccessStage::BeforeAccess {
                        crate::jobs::investigations::KnownSourceDisposition::ReleasedBeforeRead
                    } else if matches!(&self, Self::Publication(failure) if matches!(failure.error(), SourcePublicationError::BoundInvariant)) {
                        crate::jobs::investigations::KnownSourceDisposition::BoundInvariantDefect
                    } else {
                        crate::jobs::investigations::KnownSourceDisposition::KnownReadDiscarded
                    },
                    diagnostic,
                    clock.active_milliseconds(),
                );
            if let Some(diagnostic) = diagnostic {
                // A closed record only: step and error class, no source text.
                // A refused settlement (such as a publication that did commit
                // before its acknowledgement was lost) saved no discard.
                eprintln!(
                    "investigation source read discarded: {} ({})",
                    serde_json::to_string(&diagnostic).unwrap_or_default(),
                    match &settled {
                        Ok(_) => "saved",
                        Err(_) => "not saved: settlement refused",
                    }
                );
            }
            settled.map_err(SourceCleanupError::from)
        })();
        match result {
            Ok(detail) => Ok(detail),
            Err(error) => Err(SourceCleanupFailure {
                _retained: self,
                error,
            }),
        }
    }
}
impl PreparedSourceFailure {
    pub(in crate::investigations) fn settle_terminal(
        self,
        state: &AppState,
        execution: &JobExecution,
        reservation: &CommittedSourceReservation,
        clock: &RunClock,
    ) -> Result<InvestigationDetail, SourceCleanupFailure> {
        RetainedCleanup::Read(Box::new(self)).settle(state, execution, reservation, clock)
    }
}
impl SourcePublicationFailure {
    pub(in crate::investigations) fn settle_terminal(
        self,
        state: &AppState,
        execution: &JobExecution,
        reservation: &CommittedSourceReservation,
        clock: &RunClock,
    ) -> Result<InvestigationDetail, SourceCleanupFailure> {
        RetainedCleanup::Publication(Box::new(self)).settle(state, execution, reservation, clock)
    }
}
