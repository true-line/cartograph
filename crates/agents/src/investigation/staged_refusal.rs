//! Staged refusal identity and protected metadata. The host owns transactions,
//! completion evidence, response usage and terminal publication.
use super::{obligations::*, staged_finish::*, *};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefusalRecord {
    pub schema_version: u32,
    pub refusal_id: String,
    pub task_id: String,
    pub request_hash: String,
    pub obligations_hash: String,
    pub step_id: String,
    pub input_ledger_hash: String,
    pub disclosure_hash: String,
    pub finish_hash: String,
    pub outcome: CompletionOutcome,
    pub response_hash: String,
    pub previous_refusal: Option<String>,
    pub refusal_count: u32,
    pub assessment: Vec<ObligationObservation>,
    pub feedback: RefusalFeedback,
}
#[derive(Debug, PartialEq, Eq)]
pub enum RefusalDisposition {
    Continue,
    TerminalRequired,
}
impl RefusalRecord {
    pub fn content_id(&self) -> Result<String, InvestigationError> {
        bounded_bytes(self, MAX_MANIFEST_BYTES)?;
        let mut record = self.clone();
        record.refusal_id.clear();
        Ok(crate::source_basis::domain_hash(
            b"cartograph:obligation-refusal:v1\0",
            &bounded_bytes(&record, MAX_MANIFEST_BYTES)?,
        ))
    }
    /// Content/basis validation complements, never replaces, host comparison
    /// with the immutable task, actual invoked step and rederived finish decision.
    pub fn validate(
        &self,
        request: &StagedObligationRequest,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        finish: &StagedFinish,
        previous: Option<&RefusalRecord>,
    ) -> Result<RefusalDisposition, InvestigationError> {
        if self.finish_hash != finish.fingerprint()? || self.outcome != finish.outcome {
            return Err(InvestigationError::InvalidInput);
        }
        self.validate_basis(request, ledger, disclosure, previous)
    }
    pub fn validate_basis(
        &self,
        request: &StagedObligationRequest,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        previous: Option<&RefusalRecord>,
    ) -> Result<RefusalDisposition, InvestigationError> {
        self.validate_stored_basis(
            &request.intent_hash()?,
            &request.obligations,
            ledger,
            disclosure,
            previous,
        )
    }
    pub fn validate_stored_basis(
        &self,
        request_hash: &str,
        envelope: &ObligationEnvelope,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        previous: Option<&RefusalRecord>,
    ) -> Result<RefusalDisposition, InvestigationError> {
        bounded_bytes(self, MAX_MANIFEST_BYTES)?;
        self.feedback.bytes()?;
        let obligations = envelope.normalized()?;
        if self.schema_version != 1
            || !super::records::opaque_id(&self.task_id)
            || !super::records::opaque_id(&self.step_id)
            || self.request_hash != request_hash
            || self.obligations_hash != obligations.fingerprint()?
            || self.input_ledger_hash != ledger.fingerprint()?
            || self.disclosure_hash != disclosure.fingerprint(ledger)?
            || !crate::source_basis::hash(&self.finish_hash)
            || !crate::source_basis::hash(&self.response_hash)
            || self.refusal_id != self.content_id()?
            || self.assessment.len() != obligations.obligations.len()
            || self
                .assessment
                .iter()
                .zip(&obligations.obligations)
                .any(|(observed, target)| {
                    observed.obligation_id != target.key().1 || observed.kind != target.key().0
                })
        {
            return Err(InvestigationError::InvalidInput);
        }
        let feedback: Vec<_> = self
            .assessment
            .iter()
            .filter_map(|observed| {
                observed.unmet.map(|reason| FeedbackEntry {
                    obligation_id: observed.obligation_id.clone(),
                    kind: observed.kind.clone(),
                    reason,
                })
            })
            .collect();
        if feedback != self.feedback.unmet {
            return Err(InvestigationError::InvalidInput);
        }
        match previous {
            None if self.refusal_count == 1 && self.previous_refusal.is_none() => {
                Ok(RefusalDisposition::Continue)
            }
            Some(previous)
                if previous.refusal_count == 1
                    && self.refusal_count == 2
                    && self.previous_refusal.as_deref() == Some(previous.refusal_id.as_str())
                    && previous.refusal_id == previous.content_id()?
                    && previous.task_id == self.task_id
                    && previous.request_hash == self.request_hash
                    && previous.obligations_hash == self.obligations_hash
                    && previous.step_id != self.step_id =>
            {
                Ok(RefusalDisposition::TerminalRequired)
            }
            _ => Err(InvestigationError::InvalidInput),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedCompletionMetadata {
    pub schema_version: u32,
    pub request_hash: String,
    pub obligations_hash: String,
    pub completed_attempt_ids: Vec<String>,
    pub refusal_count: u32,
    pub last_refusal_id: Option<String>,
    pub feedback: Option<RefusalFeedback>,
}
impl ProtectedCompletionMetadata {
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.validate()?;
        Ok(crate::source_basis::domain_hash(
            b"cartograph:completion-metadata:v1\0",
            &bounded_bytes(self, MAX_INPUT_BYTES)?,
        ))
    }

    pub fn validate(&self) -> Result<(), InvestigationError> {
        bounded_bytes(self, MAX_MANIFEST_BYTES)?;
        if self.schema_version != 1 || !crate::source_basis::hash(&self.request_hash) || !crate::source_basis::hash(&self.obligations_hash)
            || self.completed_attempt_ids.len() > 2 * MAX_TOOL_ACTIONS as usize
            || self.completed_attempt_ids.iter().any(|id| !crate::source_basis::hash(id))
            || self.completed_attempt_ids.iter().collect::<std::collections::BTreeSet<_>>().len() != self.completed_attempt_ids.len()
            || self.refusal_count > 1 // count two cannot authorize another input
            || (self.refusal_count == 0) != self.last_refusal_id.is_none()
            || (self.refusal_count == 0) != self.feedback.is_none()
            || self.last_refusal_id.as_deref().is_some_and(|id| !crate::source_basis::hash(id))
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(feedback) = &self.feedback {
            feedback.bytes()?;
        }
        Ok(())
    }
    /// Bind this exact metadata to an actual firewall preparation. Does not
    /// authorize dispatch or make feedback a citation/discharge representation.
    pub fn seal(
        &self,
        original: &llm::CompletionAction,
        prepared: &llm::bounded::PreparedBoundedCompletion,
    ) -> Result<String, InvestigationError> {
        self.validate()?;
        if !prepared.matches_action(original) {
            return Err(InvestigationError::InvalidInput);
        }
        let parse = |raw: &str| -> Result<serde_json::Value, InvestigationError> {
            strict::preflight(raw, MAX_INPUT_BYTES, 40, 16384, 32768)?;
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidInput)
        };
        let before = parse(&original.payload.prompt)?;
        let after = parse(&prepared.preview().egress.payload.prompt)?;
        let expected = serde_json::to_value(self).map_err(|_| InvestigationError::InvalidInput)?;
        if before.get("completion_metadata") != Some(&expected)
            || after.get("completion_metadata") != Some(&expected)
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(crate::source_basis::domain_hash(
            b"cartograph:completion-metadata-seal:v1\0",
            &bounded_bytes(
                &(self, &prepared.preview().egress.payload_hash),
                MAX_INPUT_BYTES,
            )?,
        ))
    }
}
