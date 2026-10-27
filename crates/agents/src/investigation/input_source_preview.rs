//! Pre-read continuation closure for one exact captured-source descriptor.
//!
//! This is a pure representation proof, not authority to read, a provider
//! preparation, a persisted reservation, or an acquisition publication.
use super::*;
use llm::bounded::{CompletionLimits, PreparedBoundedCompletion, ProviderProfile};
use staged_refusal::CompletionCallMode;

/// Opaque and non-deserializable. Synthetic sizing values never become an
/// InvestigationInput, disclosure receipt, or PreparedBoundedCompletion.
///
/// ```compile_fail
/// fn promote(value: agents::investigation::UnreadSourcePreviewBound) -> llm::bounded::PreparedBoundedCompletion {
///     value.into()
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadSourcePreviewBound {
    basis_hash: String,
    continuation: AdmissionReserveMeasurements,
    disclosure_upper_bytes: usize,
}
impl UnreadSourcePreviewBound {
    pub fn basis_hash(&self) -> &str {
        &self.basis_hash
    }
    pub fn continuation(&self) -> AdmissionReserveMeasurements {
        self.continuation
    }
    pub fn disclosure_upper_bytes(&self) -> usize {
        self.disclosure_upper_bytes
    }

    /// Recompute the exact basis. Host ownership, saved response/action,
    /// descriptor guards, usage/storage claims and atomic settlement remain
    /// independent mandatory checks.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_with_policy(
        &self,
        input: &InvestigationInput,
        current: &PreparedBoundedCompletion,
        target: &InvestigationInspectionTarget,
        profile: &ProviderProfile,
        limits: &CompletionLimits,
        next_action_id: &str,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if *self
            != input.certify_unread_source_v9_with_policy(
                current,
                target,
                profile,
                limits,
                next_action_id,
                policy,
            )?
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}
impl InvestigationInput {
    /// Prove wire/protected-field/disclosure closure for both continuation
    /// modes after this one pinned source append. The current preparation must
    /// be real and sealable. Future completion/capacity feedback uses controlled
    /// decimal/lowerhex/finite-code replacements under pinned redact-text@1.
    /// Source text is redacted as a separate string and cannot alter prompt
    /// matching regions; its direct-string bound is 6N, including escaping.
    /// No synthetic envelope is returned or accepted by a sealing API.
    #[allow(clippy::too_many_arguments)]
    pub fn certify_unread_source_v9_with_policy(
        &self,
        current: &PreparedBoundedCompletion,
        target: &InvestigationInspectionTarget,
        profile: &ProviderProfile,
        limits: &CompletionLimits,
        next_action_id: &str,
        policy: &InvestigationPolicy,
    ) -> Result<UnreadSourcePreviewBound, InvestigationError> {
        let (prompt, _) = self.admission_reserve_basis(policy)?;
        if prompt["turn_progress"]["admission_mode"] != "choice"
            || current.profile() != profile
            || current.limits() != limits
        {
            return Err(InvestigationError::InvalidInput);
        }
        drop(prompt);
        let disclosure = self
            .seal_disclosure_with_policy(current, policy)?
            .ok_or(InvestigationError::InvalidInput)?;
        // This projection contains the complete prospective fixed fields and
        // every maximal controlled value. The shared actual-sealer comparator
        // checks real production redaction in both modes. Controlled values
        // cannot create regex prefixes, JSON separators, or private-key
        // delimiters; any cross-field private-key regions thus remain fixed.
        let acquired =
            self.measure_source_projection(target, profile, limits, next_action_id, policy, true)?;
        // A genuinely unavailable read advances usage/attempt metadata without
        // acquiring a citation or revising the ledger. Bound that complete
        // protected representation independently; no size ordering is assumed.
        let unavailable =
            self.measure_unchanged_protected_reserve(profile, limits, next_action_id, policy)?;
        let continuation = AdmissionReserveMeasurements {
            choice: maximum(acquired.choice, unavailable.choice),
            finish_only: maximum(acquired.finish_only, unavailable.finish_only),
        };
        let disclosure_upper_bytes = disclosure_upper_bound(
            disclosure.clone(),
            self.payload.spans.len(),
            MAX_DISCLOSURE_BYTES,
        )?;
        let basis_hash = crate::source_basis::domain_hash(
            b"cartograph:unread-source-preview:v1\0",
            &bounded_bytes(
                &(
                    self.ledger.fingerprint_with_policy(policy)?,
                    content_hash(&bounded_bytes(&self.payload, MAX_INPUT_BYTES)?),
                    disclosure.fingerprint_with_policy(&self.ledger, policy)?,
                    &current.preview().egress.action_id,
                    target,
                    profile,
                    limits,
                    next_action_id,
                    policy.profile(),
                    core_redact::TEXT_POLICY_VERSION,
                ),
                MAX_INPUT_BYTES,
            )?,
        );
        Ok(UnreadSourcePreviewBound {
            basis_hash,
            continuation,
            disclosure_upper_bytes,
        })
    }
}

/// All source metadata survives redaction; only each independently redacted
/// source text may change. This includes both existing and appended spans.
pub(super) fn protected_representation(
    before: &CompletionPayload,
    after: &CompletionPayload,
) -> Result<(), InvestigationError> {
    super::super::sealing::admission_protected_fields(before, after)?;
    if before.spans.len() != after.spans.len()
        || before.spans.iter().zip(&after.spans).any(|(a, b)| {
            a.id != b.id
                || a.repo != b.repo
                || a.path != b.path
                || a.commit_sha != b.commit_sha
                || a.byte_start != b.byte_start
                || a.byte_end != b.byte_end
        })
    {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

pub(super) fn disclosure_upper_bound(
    mut sizing: InvestigationDisclosure,
    source_count: usize,
    max_bytes: usize,
) -> Result<usize, InvestigationError> {
    // Internal sizing-only use of the actual production Serialize DTO. Hash
    // recipes have fixed width. Query/fact/inventory/history cardinalities and
    // supplied/omitted states remain unchanged by one source append. Feedback
    // arrays appear only through fixed-width hashes in disclosure6.
    let state = sizing
        .admission
        .as_mut()
        .ok_or(InvestigationError::InvalidInput)?;
    state.mode = CompletionCallMode::FinishOnly;
    state.capacity_refusal_count = u32::MAX;
    state.capacity_feedback = Some(AdmissionCapacityFeedback {
        schema_version: 1,
        action_hash: "f".repeat(64),
        reason: CapacityRefusalReason::UnprovenContribution,
    });
    state.consumed_budgets = InvestigationUsage {
        model_invocations: u32::MAX,
        tool_actions: u32::MAX,
        selected_facts: usize::MAX,
        evidence_requests: u32::MAX,
        evidence_items: usize::MAX,
        evidence_bytes: usize::MAX,
        captured_validation_bytes: u64::MAX,
        generated_token_reservations: u32::MAX,
        reported_input_tokens: Some(u64::MAX),
        reported_output_tokens: Some(u64::MAX),
        reported_cache_read_tokens: Some(u64::MAX),
        active_milliseconds: u64::MAX,
    };
    for source in &mut sizing.sources {
        source.redacted = false;
    }
    sizing.sources.push(DisclosedInvestigationSource {
        citation_id: format!(
            "evidence-{}",
            source_count
                .checked_add(1)
                .ok_or(InvestigationError::LimitExceeded)?
        ),
        representation: DisclosureRepresentation::Supplied {
            representation_hash: "f".repeat(64),
        },
        disclosed_text_hash: Some("f".repeat(64)),
        redacted: false,
    });
    // These values intentionally cannot validate as a receipt. They never leave
    // this function; only the serialized upper size does.
    Ok(bounded_bytes(&sizing, max_bytes)?.len())
}

fn maximum(
    a: llm::bounded::BoundedWireMeasurements,
    b: llm::bounded::BoundedWireMeasurements,
) -> llm::bounded::BoundedWireMeasurements {
    llm::bounded::BoundedWireMeasurements {
        raw_string_bytes: a.raw_string_bytes.max(b.raw_string_bytes),
        original_payload_bytes: a.original_payload_bytes.max(b.original_payload_bytes),
        redacted_payload_bytes: a.redacted_payload_bytes.max(b.redacted_payload_bytes),
        original_action_bytes: a.original_action_bytes.max(b.original_action_bytes),
        egress_fingerprint_bytes: a.egress_fingerprint_bytes.max(b.egress_fingerprint_bytes),
    }
}
