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
    /// Digest for an independent immutable response-metadata witness (@22).
    /// Admission establishes diagnostic correctness before discarding text;
    /// this digest establishes only integrity and binding during reopen.
    /// Store it outside this record to avoid a circular refusal identity.
    pub fn feedback_witness_hash(&self) -> Result<String, InvestigationError> {
        if !crate::source_basis::hash(&self.response_hash)
            || !crate::source_basis::hash(&self.finish_hash)
            || !super::records::opaque_id(&self.step_id)
            || self.refusal_id != self.content_id()?
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(crate::source_basis::domain_hash(
            b"cartograph:finish-feedback-witness:v1\0",
            &bounded_bytes(
                &(
                    &self.response_hash,
                    &self.finish_hash,
                    &self.refusal_id,
                    &self.step_id,
                ),
                MAX_MANIFEST_BYTES,
            )?,
        ))
    }

    /// Bind a separately retained diagnostic sidecar through the existing
    /// fixed-length response witness. The host validates and bounds the sidecar
    /// before supplying its content hash. This does not alter refusal identity.
    pub fn diagnostic_witness_hash(
        &self,
        sidecar_hash: &str,
    ) -> Result<String, InvestigationError> {
        if !crate::source_basis::hash(sidecar_hash) {
            return Err(InvestigationError::InvalidInput);
        }
        let feedback = self.feedback_witness_hash()?;
        Ok(crate::source_basis::domain_hash(
            b"cartograph:finish-diagnostic-witness:v1\0",
            &bounded_bytes(&(feedback, sidecar_hash), MAX_MANIFEST_BYTES)?,
        ))
    }

    /// The host must read the witness and response identity from the independent
    /// immutable invocation row, and separately authenticate the call history.
    /// Passing copies from this record alone would grant no authentication.
    pub fn validate_feedback_witness(
        &self,
        witness: &str,
        response_hash: &str,
        step_id: &str,
    ) -> Result<(), InvestigationError> {
        if self.response_hash != response_hash
            || self.step_id != step_id
            || self.feedback_witness_hash()? != witness
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
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
        self.validate_with_policy(
            request,
            ledger,
            disclosure,
            finish,
            previous,
            &INVESTIGATION_V1_POLICY,
        )
    }
    /// As [`Self::validate`], bounded by the policy the host resolved from the
    /// exact saved task limits; it must name the request's profile.
    pub fn validate_with_policy(
        &self,
        request: &StagedObligationRequest,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        finish: &StagedFinish,
        previous: Option<&RefusalRecord>,
        policy: &InvestigationPolicy,
    ) -> Result<RefusalDisposition, InvestigationError> {
        if self.finish_hash != finish.fingerprint()? || self.outcome != finish.outcome {
            return Err(InvestigationError::InvalidInput);
        }
        self.validate_basis_with_policy(request, ledger, disclosure, previous, policy)
    }
    pub fn validate_basis(
        &self,
        request: &StagedObligationRequest,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        previous: Option<&RefusalRecord>,
    ) -> Result<RefusalDisposition, InvestigationError> {
        self.validate_basis_with_policy(
            request,
            ledger,
            disclosure,
            previous,
            &INVESTIGATION_V1_POLICY,
        )
    }
    pub fn validate_basis_with_policy(
        &self,
        request: &StagedObligationRequest,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        previous: Option<&RefusalRecord>,
        policy: &InvestigationPolicy,
    ) -> Result<RefusalDisposition, InvestigationError> {
        let combined = matches!(
            self.feedback.prose_correction,
            Some(ProseCorrection::Combined(_))
        );
        if (combined
            && !matches!(
                request.specialist_id.as_str(),
                "domain-analyst@24"
                    | "domain-analyst@25"
                    | "domain-analyst@26"
                    | "domain-analyst@27"
                    | "domain-analyst@28"
                    | "domain-analyst@29"
                    | "domain-analyst@30"
                    | "domain-analyst@31"
            ))
            || (matches!(
                request.specialist_id.as_str(),
                "domain-analyst@24"
                    | "domain-analyst@25"
                    | "domain-analyst@26"
                    | "domain-analyst@27"
                    | "domain-analyst@28"
                    | "domain-analyst@29"
                    | "domain-analyst@30"
                    | "domain-analyst@31"
            ) && self.feedback.prose_correction.is_some()
                && !combined)
        {
            return Err(InvestigationError::InvalidInput);
        }
        if matches!(
            self.feedback.prose_correction,
            Some(
                ProseCorrection::UnverifiedEnumerationClaimAt(_)
                    | ProseCorrection::UnverifiedConfigurationClaimAt(_)
            )
        ) && request.specialist_id != "domain-analyst@23"
        {
            return Err(InvestigationError::InvalidInput);
        }
        // @13 borrows replay locations and their witness, not capacity exclusions.
        if request.specialist_id == "evidence-auditor@13"
            && self
                .feedback
                .unmet
                .iter()
                .any(|entry| entry.reason == RefusalReason::ExcludedByCapacity)
        {
            return Err(InvestigationError::InvalidInput);
        }
        if ((matches!(
            self.feedback.prose_correction,
            Some(ProseCorrection::ParaphraseReplayAt(_))
        ) || self
            .feedback
            .unmet
            .iter()
            .any(|entry| entry.reason == RefusalReason::ExcludedByCapacity))
            && !matches!(
                request.specialist_id.as_str(),
                "domain-analyst@22"
                    | "evidence-auditor@13"
                    | "evidence-auditor@14"
                    | "domain-analyst@23"
                    | "domain-analyst@24"
                    | "domain-analyst@25"
                    | "domain-analyst@26"
                    | "domain-analyst@27"
                    | "domain-analyst@28"
                    | "domain-analyst@29"
                    | "domain-analyst@30"
                    | "domain-analyst@31"
            ))
            || request.limit_profile != policy.profile()
            // #2478: the drop-only correction is @31's alone.
            || (self.feedback.citation_drop.is_some()
                && request.specialist_id != "domain-analyst@31")
            || ((self.feedback.citation_correction.is_some()
                || self.feedback.prose_correction.is_some())
                && !matches!(
                    request.specialist_id.as_str(),
                    "domain-analyst@20"
                        | "domain-analyst@21"
                        | "domain-analyst@22"
                        | "evidence-auditor@13"
                        | "evidence-auditor@14"
                        | "domain-analyst@23"
                        | "domain-analyst@24"
                        | "domain-analyst@25"
                        | "domain-analyst@26"
                        | "domain-analyst@27"
                        | "domain-analyst@28"
                        | "domain-analyst@29"
                        | "domain-analyst@30"
                        | "domain-analyst@31"
                ))
        {
            return Err(InvestigationError::InvalidInput);
        }
        self.validate_stored_basis_with_policy(
            &request.intent_hash_with_policy(policy)?,
            &request.obligations,
            ledger,
            disclosure,
            previous,
            policy,
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
        self.validate_stored_basis_with_policy(
            request_hash,
            envelope,
            ledger,
            disclosure,
            previous,
            &INVESTIGATION_V1_POLICY,
        )
    }
    /// Stored records carry no profile; the host supplies the saved policy.
    pub fn validate_stored_basis_with_policy(
        &self,
        request_hash: &str,
        envelope: &ObligationEnvelope,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        previous: Option<&RefusalRecord>,
        policy: &InvestigationPolicy,
    ) -> Result<RefusalDisposition, InvestigationError> {
        self.validate_stored_basis_using_consistency(
            request_hash,
            envelope,
            ledger,
            disclosure,
            previous,
            policy,
            None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn validate_stored_basis_using_consistency(
        &self,
        request_hash: &str,
        envelope: &ObligationEnvelope,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
        previous: Option<&RefusalRecord>,
        policy: &InvestigationPolicy,
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    ) -> Result<RefusalDisposition, InvestigationError> {
        if let Some(bound) = consistency {
            bound.check(&self.task_id, &self.step_id, ledger, disclosure)?;
        }
        bounded_bytes(self, MAX_MANIFEST_BYTES)?;
        self.feedback.bytes_with_policy(policy)?;
        // Generic replay cannot authenticate acquisition choices. The host
        // must supply the original invocation witness and enforce role @20.
        if let Some(correction) = &self.feedback.citation_correction {
            consistency
                .ok_or(InvestigationError::InvalidInput)?
                .validate_stored_citation_correction(correction)?;
        }
        if (self.feedback.prose_correction.is_some() || self.feedback.citation_drop.is_some())
            && consistency.is_none()
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(ProseCorrection::LimitationDeniesAcquiredEvidence(conflict)) =
            &self.feedback.prose_correction
            && !conflict.evidence_ids.iter().all(|id| {
                ledger.citations.iter().any(|c| {
                    c.citation_id == *id
                        && matches!(
                            c.origin,
                            InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
                        )
                })
            })
        {
            return Err(InvestigationError::InvalidInput);
        }
        let obligations = envelope.normalized_with_policy(policy)?;
        if self.schema_version != 1
            || !super::records::opaque_id(&self.task_id)
            || !super::records::opaque_id(&self.step_id)
            || self.request_hash != request_hash
            || self.obligations_hash != obligations.fingerprint_with_policy(policy)?
            || self.input_ledger_hash != ledger.fingerprint_with_policy(policy)?
            || self.disclosure_hash
                != disclosure.fingerprint_consistent(ledger, policy, consistency)?
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
                    reason: if !observed.attempted
                        && !observed.acquired
                        && reason == RefusalReason::NotAttempted
                        && consistency.is_some_and(|bound| {
                            bound
                                .capacity_excluded_obligations()
                                .contains(&observed.obligation_id)
                        }) {
                        RefusalReason::ExcludedByCapacity
                    } else {
                        reason
                    },
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
/// Schema-one metadata permits one repair after a refused finish. This is a
/// protocol allowance, not an additional numeric policy budget.
const MAX_CONTINUABLE_REFUSALS: u32 = 1;

/// Remaining supported finishing opportunities, derived from saved metadata.
/// This neither reserves usage nor certifies byte capacity or provider context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinishCallReserve {
    pub model_invocations: u32,
    pub generated_token_reservations: u32,
}

/// Resource-only next-turn decision. Neither variant authorizes dispatch or
/// proves byte capacity; the host must also certify that mode's exact input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionCallMode {
    Choice,
    FinishOnly,
}

impl ProtectedCompletionMetadata {
    /// A future admission controller must retain these finish opportunities
    /// before spending another unrestricted call. Each finish can still be
    /// invalid or fail operationally; no successful result is promised.
    pub fn finish_call_reserve_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<FinishCallReserve, InvestigationError> {
        self.validate_with_policy(policy)?;
        let model_invocations = MAX_CONTINUABLE_REFUSALS
            .checked_sub(self.refusal_count)
            .and_then(|repairs| repairs.checked_add(1))
            .ok_or(InvestigationError::InvalidInput)?;
        let generated_token_reservations = model_invocations
            .checked_mul(policy.output_tokens_for_mode(CompletionCallMode::FinishOnly))
            .ok_or(InvestigationError::LimitExceeded)?;
        Ok(FinishCallReserve {
            model_invocations,
            generated_token_reservations,
        })
    }

    /// Preserve the remaining repair/finish opportunities before allowing an
    /// unrestricted choice. Count and generated-token capacity are independent.
    /// This pure projection charges nothing and cannot attest saved usage.
    pub fn next_call_mode_with_policy(
        &self,
        usage: &InvestigationUsage,
        policy: &InvestigationPolicy,
    ) -> Result<CompletionCallMode, InvestigationError> {
        let reserve = self.finish_call_reserve_with_policy(policy)?;
        let limits = policy.limits();
        let calls = limits
            .model_invocations
            .checked_sub(usage.model_invocations)
            .ok_or(InvestigationError::LimitExceeded)?;
        let tokens = limits
            .generated_token_reservations
            .checked_sub(usage.generated_token_reservations)
            .ok_or(InvestigationError::LimitExceeded)?;
        if calls < reserve.model_invocations || tokens < reserve.generated_token_reservations {
            return Err(InvestigationError::LimitExceeded);
        }
        let choice_calls = reserve
            .model_invocations
            .checked_add(1)
            .ok_or(InvestigationError::LimitExceeded)?;
        let choice_tokens = reserve
            .generated_token_reservations
            .checked_add(limits.generated_tokens_per_invocation)
            .ok_or(InvestigationError::LimitExceeded)?;
        Ok(if calls >= choice_calls && tokens >= choice_tokens {
            CompletionCallMode::Choice
        } else {
            CompletionCallMode::FinishOnly
        })
    }

    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.fingerprint_with_policy(&INVESTIGATION_V1_POLICY)
    }
    /// The host resolves `policy` from the exact saved task limits.
    pub fn fingerprint_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<String, InvestigationError> {
        self.validate_with_policy(policy)?;
        Ok(crate::source_basis::domain_hash(
            b"cartograph:completion-metadata:v1\0",
            &bounded_bytes(self, MAX_INPUT_BYTES)?,
        ))
    }

    pub fn validate(&self) -> Result<(), InvestigationError> {
        self.validate_with_policy(&INVESTIGATION_V1_POLICY)
    }
    pub fn validate_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        bounded_bytes(self, MAX_MANIFEST_BYTES)?;
        if self.schema_version != 1 || !crate::source_basis::hash(&self.request_hash) || !crate::source_basis::hash(&self.obligations_hash)
            || self.completed_attempt_ids.len() > policy.max_completed_attempts()
            || self.completed_attempt_ids.iter().any(|id| !crate::source_basis::hash(id))
            || self.completed_attempt_ids.iter().collect::<std::collections::BTreeSet<_>>().len() != self.completed_attempt_ids.len()
            || self.refusal_count > MAX_CONTINUABLE_REFUSALS // count two cannot authorize another input
            || (self.refusal_count == 0) != self.last_refusal_id.is_none()
            || (self.refusal_count == 0) != self.feedback.is_none()
            || self.last_refusal_id.as_deref().is_some_and(|id| !crate::source_basis::hash(id))
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(feedback) = &self.feedback {
            feedback.bytes_with_policy(policy)?;
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
        self.seal_with_policy(original, prepared, &INVESTIGATION_V1_POLICY)
    }
    pub fn seal_with_policy(
        &self,
        original: &llm::CompletionAction,
        prepared: &llm::bounded::PreparedBoundedCompletion,
        policy: &InvestigationPolicy,
    ) -> Result<String, InvestigationError> {
        self.validate_with_policy(policy)?;
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

#[cfg(test)]
#[path = "finish_reserve_tests.rs"]
mod finish_reserve_tests;
