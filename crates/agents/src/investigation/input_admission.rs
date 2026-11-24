//! Internal prompt9 construction. This representation grants no dispatch or
//! acquisition authority; the host must bind it to its saved admission state.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::*;
use crate::{InputClosureStatus, PrimarySourceScope};
use serde::{Deserialize, Serialize};
use staged_refusal::{CompletionCallMode, ProtectedCompletionMetadata};
use std::collections::BTreeSet;

/// A finite byte-capacity reason, independent of completion refusal feedback.
/// Storage and tool/evidence-budget failures retain their terminal semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityRefusalReason {
    PromptCapacity,
    FinishCallReserve,
    FinishTokenReserve,
    UnprovenContribution,
}

impl CapacityRefusalReason {
    // Exhaustive proof table: adding a code requires revisiting its wire name
    // and the maximal variant used by CapacityFeedbackSizeEnvelope.
    fn encoded_name(self) -> &'static str {
        match self {
            Self::PromptCapacity => "prompt_capacity",
            Self::FinishCallReserve => "finish_call_reserve",
            Self::FinishTokenReserve => "finish_token_reserve",
            Self::UnprovenContribution => "unproven_contribution",
        }
    }
}

/// Latest refused action only. Clearing this slot after an accepted acquisition
/// does not reset the separately supplied cumulative refusal count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionCapacityFeedback {
    pub schema_version: u32,
    pub action_hash: String,
    pub reason: CapacityRefusalReason,
}

impl AdmissionCapacityFeedback {
    pub(in crate::investigation) fn validate(&self) -> Result<(), InvestigationError> {
        if self.schema_version != 1 || !crate::source_basis::hash(&self.action_hash) {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}

const CHOICE_INSTRUCTIONS: &str = "Choose one permitted action. Capacity feedback describes only the latest refused acquisition and grants no evidence or authority. The refusal count persists when an accepted acquisition clears that feedback.";
const PATCH_INSTRUCTIONS: &str = "Return only a finish_patch object with a patches list. Each entry names an authenticated flagged finding_index and either replaces that full finding or withdraws it with withdraw:true. Never emit tools, a full answer, duplicate indices or unflagged indices. Omitted findings are retained by the host and the entire merged answer is verified again.";
const FINISH_ONLY_INSTRUCTIONS: &str = "Finish only: return a finish action using the already supplied evidence. Do not query context, read evidence, or read coverage inventory. Capacity feedback grants no evidence or authority.";

impl InvestigationInput {
    /// The ledger as citation schemas see it: a firewall-refused read stays
    /// acquired but is not citable, so its id leaves every citation enum (#679).
    pub(super) fn citable_ledger(
        &self,
    ) -> Result<std::borrow::Cow<'_, InvestigationInputLedger>, InvestigationError> {
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let withheld = withheld_source_ids(&prompt)?;
        if withheld.is_empty() {
            return Ok(std::borrow::Cow::Borrowed(&self.ledger));
        }
        let mut ledger = self.ledger.clone();
        ledger
            .citations
            .retain(|citation| !withheld.contains(citation.citation_id.as_str()));
        Ok(std::borrow::Cow::Owned(ledger))
    }

    /// Distinct @10/request7 path. Exact family selection does not grant
    /// dispatch or acquisition authority; the host must admit each operation.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v9_with_policy(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[&str],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
        request: &obligations::StagedObligationRequest,
        metadata: &ProtectedCompletionMetadata,
        coverage_details: &[&str],
        mode: CompletionCallMode,
        capacity_refusal_count: u32,
        capacity_feedback: Option<&AdmissionCapacityFeedback>,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::prepare_v9_withholding_with_policy(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            basis,
            request,
            metadata,
            coverage_details,
            mode,
            capacity_refusal_count,
            capacity_feedback,
            policy,
            &BTreeSet::new(),
        )
    }

    /// As [`Self::prepare_v9_with_policy`], with host-withheld source citations
    /// (#679). Each withheld id is an acquired source citation whose exact read
    /// the host firewall refused; it is listed only as a closed
    /// `firewall_refused` handle with no text. The caller owns the refusal
    /// witness; this projection grants no denial or completion authority.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v9_withholding_with_policy(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[&str],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
        request: &obligations::StagedObligationRequest,
        metadata: &ProtectedCompletionMetadata,
        coverage_details: &[&str],
        mode: CompletionCallMode,
        capacity_refusal_count: u32,
        capacity_feedback: Option<&AdmissionCapacityFeedback>,
        policy: &InvestigationPolicy,
        withheld: &BTreeSet<String>,
    ) -> Result<Self, InvestigationError> {
        if !specialist.uses_next_prompt_admission()
            || request.schema_version != 7
            || capacity_refusal_count > policy.limits().model_invocations
            || capacity_refusal_count > usage.model_invocations
            || (capacity_feedback.is_some() && capacity_refusal_count == 0)
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(feedback) = capacity_feedback {
            feedback.validate()?;
        }
        if metadata.next_call_mode_with_policy(usage, policy)? != mode {
            return Err(InvestigationError::InvalidInput);
        }
        let (mut input, mut prompt) = Self::prepare_progressive_parts_with_policy(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            basis,
            request,
            metadata,
            policy,
            7,
            withheld,
        )?;
        prompt["schema_version"] = 9.into();
        apply_admission_progress(
            &mut prompt["turn_progress"],
            mode,
            capacity_refusal_count,
            &capacity_feedback,
        )?;
        input.payload.response_schema =
            response_schema::select_admitted(specialist, mode, &input.ledger)?;
        input.finish_progressive_input(&prompt, coverage_details)
    }
}

/// Wire ceilings for feedback-only continuations at one exact acquired state.
/// These numbers neither bind a saved host transaction nor authorize dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionReserveMeasurements {
    pub choice: llm::bounded::BoundedWireMeasurements,
    pub finish_only: llm::bounded::BoundedWireMeasurements,
}

impl InvestigationInput {
    /// Measure both instruction variants through the actual bounded wire path.
    /// This bound names the exact immediate action_id/profile/limits supplied;
    /// a later call with a changed step ID must be measured again.
    /// Graph, query, source, history, target and system strings remain exact.
    /// New acquisitions (including optional cursor/action transitions) require a
    /// fresh prospective input; this is not a bound on their unknown contents.
    /// Reserved replacements contain only decimal numbers, lower-hex hashes and
    /// closed ASCII codes. They add no secret recognizer prefix or private-key
    /// delimiter; quoted JSON boundaries isolate them from unchanged source
    /// strings. The actual redactor is rerun on each complete reserved prompt.
    /// All optional fields that can grow without acquisition are represented:
    /// both feedback slots, refusal id, attempt ids and reported token counts.
    /// Synthetic completion/capacity feedback cannot escape this method as an
    /// InvestigationInput, CompletionAction, sealed receipt or dispatch permit.
    pub fn measure_v9_reserve_with_policy(
        &self,
        profile: &llm::bounded::ProviderProfile,
        limits: &llm::bounded::CompletionLimits,
        action_id: &str,
        policy: &InvestigationPolicy,
    ) -> Result<AdmissionReserveMeasurements, InvestigationError> {
        let (prompt, contract) = self.admission_reserve_basis(policy)?;
        self.measure_reserved_projection(
            prompt,
            contract,
            self.action(action_id.into())?,
            profile,
            limits,
            policy,
            None,
            None,
            false,
        )
    }

    pub(super) fn measure_unchanged_protected_reserve(
        &self,
        target: &InvestigationInspectionTarget,
        profile: &llm::bounded::ProviderProfile,
        limits: &llm::bounded::CompletionLimits,
        action_id: &str,
        policy: &InvestigationPolicy,
    ) -> Result<AdmissionReserveMeasurements, InvestigationError> {
        let (prompt, contract) = self.admission_reserve_basis(policy)?;
        self.measure_reserved_projection(
            prompt,
            contract,
            self.action(action_id.into())?,
            profile,
            limits,
            policy,
            None,
            Some(target),
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn measure_reserved_projection(
        &self,
        prompt: Value,
        contract: InvestigationCompletionContract,
        mut action: CompletionAction,
        profile: &llm::bounded::ProviderProfile,
        limits: &llm::bounded::CompletionLimits,
        policy: &InvestigationPolicy,
        unread_source: Option<(
            usize,
            usize,
            Option<core_redact::DirectSourceJsonContentSizes>,
        )>,
        source_target: Option<&InvestigationInspectionTarget>,
        protect: bool,
    ) -> Result<AdmissionReserveMeasurements, InvestigationError> {
        if core_redact::TEXT_POLICY_VERSION != "redact-text@2" {
            return Err(InvestigationError::InvalidInput);
        }
        let metadata: ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        // The reserve implementation owns the protocol repair allowance.
        // Removing the one final finish opportunity recovers its maximal
        // continuable refusal count without duplicating that allowance here.
        let maximum_refusals = metadata
            .finish_call_reserve_with_policy(policy)?
            .model_invocations
            .checked_add(metadata.refusal_count)
            .and_then(|calls| calls.checked_sub(1))
            .ok_or(InvestigationError::LimitExceeded)?;
        // redact-text@2 proof for controlled replacements: decimal/lowerhex
        // fields and finite underscore-separated codes contain no recognizer
        // prefix, whitespace or quote. Every replacement is in its own JSON
        // value; token recognizers stop at quotes/separators. An assignment
        // match may now cross one closing key quote (#1753), so it can reach
        // a value only from a key that ends in a credential word. The keys
        // that hold controlled replacements (completed_attempt_ids,
        // refusal_count, last_refusal_id, feedback and the feedback
        // envelope's own fields) are fixed protocol names, none of which ends
        // in one, and no finite code does either. A value is followed by a
        // comma or a closing bracket, never by a colon, so a controlled value
        // cannot become an assignment name. Only private-key matching spans
        // fields; its delimiters occur solely in unchanged authored strings,
        // so the same regions collapse to the same marker. No controlled
        // replacement can introduce another delimiter.
        // Re-prove this if the redactor or these field classes change.
        let mut reserved = prompt;
        let completion = reserved["completion_metadata"]
            .as_object_mut()
            .ok_or(InvestigationError::InvalidInput)?;
        completion.insert(
            "completed_attempt_ids".into(),
            serde_json::json!(vec!["f".repeat(64); policy.max_completed_attempts()]),
        );
        completion.insert("refusal_count".into(), maximum_refusals.into());
        completion.insert("last_refusal_id".into(), "f".repeat(64).into());
        completion.insert(
            "feedback".into(),
            serde_json::to_value(
                staged_finish::RefusalFeedback::sizing_upper_envelope_with_policy(
                    &contract.obligations,
                    policy,
                )?,
            )
            .map_err(|_| InvestigationError::InvalidInput)?,
        );
        if SpecialistId::is_typed_citation_prompt(&self.payload.system) {
            let upper = serde_json::to_value(
                if self.frozen_specialist()? == SpecialistId::EvidenceAuditorV14 {
                    staged_finish::RefusalFeedback::sizing_closed_contract_envelope(
                        &contract,
                        &self.ledger,
                        policy,
                    )?
                } else if self.frozen_specialist()?.uses_combined_finish_feedback() {
                    staged_finish::RefusalFeedback::sizing_combined_contract_envelope(
                        &contract,
                        &self.ledger,
                        policy,
                    )?
                } else if self.frozen_specialist()? == SpecialistId::DomainAnalystV23 {
                    staged_finish::RefusalFeedback::sizing_claim_contract_envelope(
                        &contract,
                        &self.ledger,
                        policy,
                    )?
                } else if matches!(
                    self.frozen_specialist()?,
                    SpecialistId::DomainAnalystV22 | SpecialistId::EvidenceAuditorV13
                ) {
                    staged_finish::RefusalFeedback::sizing_replay_contract_envelope(
                        &contract,
                        &self.ledger,
                        policy,
                    )?
                } else {
                    staged_finish::RefusalFeedback::sizing_typed_contract_envelope(
                        &contract,
                        &self.ledger,
                        policy,
                    )?
                },
            )
            .map_err(|_| InvestigationError::InvalidInput)?;
            // Both the immutable historical mapping and the current-action
            // projection may coexist. The mapping keeps the whole-feedback
            // ceiling. The projection holds only a citation correction, so a
            // combined-feedback role reserves the contract's exact superset of
            // it instead (#1653), whenever that is no larger at every escape
            // layer. Either value alone bounds every real projection.
            let projection = if self.frozen_specialist()?.uses_combined_finish_feedback()
                && self.frozen_specialist()? != SpecialistId::EvidenceAuditorV14
            {
                let exact = acquisition_choices::citation_projection_upper(
                    &contract,
                    &self.ledger,
                    policy,
                )?;
                let tighter = acquisition_choices::embedded_lengths(&exact)?
                    .iter()
                    .zip(acquisition_choices::embedded_lengths(&upper)?)
                    .all(|(exact, upper)| *exact <= upper);
                if tighter { exact } else { upper.clone() }
            } else {
                upper.clone()
            };
            reserved["completion_metadata"]["feedback"] = upper;
            reserved["turn_progress"]["citation_correction"] = projection;
        }
        // Numeric width, not invented usage authority. The actual usage DTO
        // admits optional token counters which may become present without
        // acquisition; unknown optional/structured fields fail closed.
        let usage = reserved["consumed_budgets"]
            .as_object_mut()
            .ok_or(InvestigationError::InvalidInput)?;
        // This counter is absent from legacy/unknown serialization, but can
        // become known after the admitted response. Reserve it explicitly.
        usage.insert("reported_cache_read_tokens".into(), u64::MAX.into());
        for (key, value) in usage {
            if value.is_number()
                || matches!(
                    key.as_str(),
                    "reported_input_tokens"
                        | "reported_output_tokens"
                        | "reported_cache_read_tokens"
                )
            {
                *value = u64::MAX.into();
            } else {
                return Err(InvestigationError::InvalidInput);
            }
        }
        maximize_progress_numbers(&mut reserved["turn_progress"]);
        if [
            CapacityRefusalReason::PromptCapacity,
            CapacityRefusalReason::FinishCallReserve,
            CapacityRefusalReason::FinishTokenReserve,
            CapacityRefusalReason::UnprovenContribution,
        ]
        .iter()
        .any(|reason| {
            reason.encoded_name().len()
                > CapacityRefusalReason::UnprovenContribution
                    .encoded_name()
                    .len()
        }) {
            return Err(InvestigationError::InvalidInput);
        }
        let feedback = CapacityFeedbackSizeEnvelope {
            schema_version: 1,
            action_hash: "f".repeat(64),
            reason: CapacityRefusalReason::UnprovenContribution,
        };
        // admission_reserve_basis already authenticated exact role/schema.
        let schema_role = response_schema::schema_role(&action.payload.system);
        let mut measure = |mode| -> Result<_, InvestigationError> {
            let mut prompt = reserved.clone();
            apply_admission_progress(
                &mut prompt["turn_progress"],
                mode,
                policy.limits().model_invocations,
                &Some(&feedback),
            )?;
            if self.acquisition_choices.is_some() {
                let feedback = ActionRefusalFeedback {
                    query_budget: self
                        .frozen_specialist()?
                        .uses_query_budget_refusals()
                        .then_some(query_refusal::QueryBudgetDiagnostic::reservation()),
                    reason: if self.frozen_specialist()?.uses_query_budget_refusals() {
                        ActionRefusalReason::QueryPageBudget
                    } else {
                        ActionRefusalReason::ActionNotOffered
                    },
                    patch_rejection: None,
                    invocation_ordinal: policy.limits().model_invocations,
                    action_hash: "f".repeat(64),
                    choices_hash: "f".repeat(64),
                };
                prompt["turn_progress"]["action_feedback"] =
                    serde_json::to_value(feedback).map_err(|_| InvestigationError::InvalidInput)?;
            }
            action.payload.response_schema = if let Some(choices) = &self.acquisition_choices {
                if let Some(target) = source_target {
                    let (future, schema) =
                        choices.source_projection(&contract, &self.ledger, target, mode, policy)?;
                    prompt["turn_progress"]["acquisition_choices"] = future;
                    schema
                } else {
                    response_schema::select_acquisition_with_policy(
                        mode,
                        choices,
                        &self.ledger,
                        &contract.request_hash,
                        policy,
                    )?
                }
            } else if let Some(role) = schema_role {
                response_schema::select_admitted(role, mode, &self.ledger)?
            } else {
                None
            };
            if SpecialistId::is_typed_citation_prompt(&action.payload.system) {
                action.payload.response_schema = response_schema::reserve_eligible_acquisition(
                    action.payload.response_schema.take(),
                    mode,
                    &prompt,
                )?;
            }
            // @28 reserves charge the visible copy in both modes.
            action.payload.response_schema = response_schema::attach_visible(
                self.frozen_specialist()?,
                &mut prompt,
                action.payload.response_schema.take(),
            )?;
            #[cfg(test)]
            assert!(
                context_hub::logical::count(&prompt).unwrap()
                    <= reserve_projection_bytes(policy).unwrap()
            );
            wire::replace_prompt(&mut action.payload, &prompt, MAX_INPUT_BYTES)?;
            if protect {
                let inspected = llm::bounded::inspect_bounded_action(profile, &action, limits)
                    .map_err(|error| {
                        if error.code == llm::bounded::FailureCode::InputTooLarge {
                            InvestigationError::LimitExceeded
                        } else {
                            InvestigationError::InvalidInput
                        }
                    })?;
                super::source_preview::protected_representation_with_policy(
                    &self.ledger,
                    &action.payload,
                    inspected.payload(),
                    if self.acquisition_choices.is_some() {
                        action.payload.response_schema.as_deref()
                    } else {
                        None
                    },
                    policy,
                )?;
            }
            let measurements = match unread_source {
                Some((index, bytes, Some(sizes))) => {
                    llm::bounded::bound_measured_unread_source_action(
                        profile, &action, limits, index, bytes, sizes,
                    )
                }
                Some((index, bytes, None)) => {
                    llm::bounded::bound_unread_source_action(profile, &action, limits, index, bytes)
                }
                None => llm::bounded::measure_bounded_action(profile, &action, limits),
            }
            .map_err(|error| {
                if error.code == llm::bounded::FailureCode::InputTooLarge {
                    InvestigationError::LimitExceeded
                } else {
                    InvestigationError::InvalidInput
                }
            })?;
            // The actual agents serializer has its own original-payload cap,
            // independent of the firewall's configurable input ceiling.
            if measurements.original_payload_bytes > MAX_INPUT_BYTES as u64 {
                return Err(InvestigationError::LimitExceeded);
            }
            Ok(measurements)
        };
        Ok(AdmissionReserveMeasurements {
            choice: measure(CompletionCallMode::Choice)?,
            finish_only: measure(CompletionCallMode::FinishOnly)?,
        })
    }

    /// Bound one captured read before source access, including both complete
    /// continuation envelopes for the given exact immediate action ID/profile/
    /// limits. A changed later step ID requires a fresh measurement. Only a
    /// private serialization projection is made:
    /// no fabricated evidence, citation ledger, acquired input or action escapes.
    /// The host must separately bind this descriptor to retained source guards,
    /// saved authority, usage/storage reservations and the actual read result.
    pub fn measure_unread_source_v9_with_policy(
        &self,
        target: &InvestigationInspectionTarget,
        profile: &llm::bounded::ProviderProfile,
        limits: &llm::bounded::CompletionLimits,
        action_id: &str,
        policy: &InvestigationPolicy,
    ) -> Result<AdmissionReserveMeasurements, InvestigationError> {
        self.measure_source_projection(target, profile, limits, action_id, policy, false)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn measure_source_projection(
        &self,
        target: &InvestigationInspectionTarget,
        profile: &llm::bounded::ProviderProfile,
        limits: &llm::bounded::CompletionLimits,
        action_id: &str,
        policy: &InvestigationPolicy,
        protect: bool,
    ) -> Result<AdmissionReserveMeasurements, InvestigationError> {
        self.measure_source_projection_with_sizes(
            target, profile, limits, action_id, policy, protect, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn measure_source_projection_with_sizes(
        &self,
        target: &InvestigationInspectionTarget,
        profile: &llm::bounded::ProviderProfile,
        limits: &llm::bounded::CompletionLimits,
        action_id: &str,
        policy: &InvestigationPolicy,
        protect: bool,
        source_sizes: Option<core_redact::DirectSourceJsonContentSizes>,
    ) -> Result<AdmissionReserveMeasurements, InvestigationError> {
        let (mut prompt, contract) = self.admission_reserve_basis(policy)?;
        target.validate()?;
        // This certificate bounds bytes, not source authority. Selected facts
        // discovered through query are readable even when not named by an
        // original obligation. Bind their frozen receipt identity below; the
        // host must separately authenticate the exact range under retained
        // guards before reservation and again before actual source access.
        // An explicitly pinned obligation keeps its exact descriptor. The
        // broader selected-fact path cannot rebind that existing contract slot.
        if contract
            .obligations
            .sources_with_policy(policy)?
            .iter()
            .any(|pinned| {
                pinned.fact == target.fact
                    && pinned.role == target.role
                    && pinned.index == target.index
                    && pinned != target
            })
        {
            return Err(InvestigationError::InvalidInput);
        }
        if target.graph_snapshot_id != self.ledger.graph_snapshot_id
            || prompt.get("omitted_sources").is_some()
        {
            return Err(InvestigationError::InvalidInput);
        }
        let selected = self
            .ledger
            .selected_facts
            .iter()
            .find(|item| item.fact == target.fact)
            .ok_or(InvestigationError::InvalidInput)?;
        let binding = selected
            .binding
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        if selected.fact_digest != target.fact_digest
            || binding.repo_key != target.source.repo
            || binding.receipt_id != target.receipt_id
            || binding.emitted_fact_digest != target.fact_digest
            || !self.ledger.receipt_references.iter().any(|receipt| {
                receipt.repo_key == target.source.repo
                    && receipt.source_id == target.registered_source_id
                    && receipt.receipt_id == target.receipt_id
            })
        {
            return Err(InvestigationError::InvalidInput);
        }
        let withheld = withheld_source_ids(&prompt)?;
        let source_citations: Vec<_> = self
            .ledger
            .citations
            .iter()
            .filter(|citation| {
                !matches!(citation.origin, InvestigationEvidenceOrigin::GraphMetadata)
            })
            .collect();
        // Withheld reads stay acquired (ordinals and limits count them) but
        // have no supplied span.
        let supplied_citations: Vec<_> = source_citations
            .iter()
            .filter(|citation| !withheld.contains(citation.citation_id.as_str()))
            .collect();
        // Complete current source set only. A changed working set needs its own
        // explicit prospective projection and cannot inherit this certificate.
        if supplied_citations.len() != self.payload.spans.len()
            || supplied_citations.iter().any(|citation| {
                self.payload
                    .spans
                    .iter()
                    .filter(|span| {
                        span.id == citation.citation_id
                            && citation.source.as_ref().is_some_and(|source| {
                                span.repo == source.repo
                                    && span.path == source.path
                                    && span.commit_sha == source.commit_sha
                                    && span.byte_start == source.byte_start
                                    && span.byte_end == source.byte_end
                            })
                            && citation.text_hash.as_deref()
                                == Some(content_hash(span.text.as_bytes()).as_str())
                    })
                    .count()
                    != 1
            })
        {
            return Err(InvestigationError::InvalidInput);
        }
        let expected_handles = serde_json::to_value(
            self.ledger
                .citations
                .iter()
                .filter(|citation| !withheld.contains(citation.citation_id.as_str()))
                .map(CitationPromptHandle::from)
                .collect::<Vec<_>>(),
        )
        .map_err(|_| InvestigationError::InvalidInput)?;
        if prompt["citations"] != expected_handles {
            return Err(InvestigationError::InvalidInput);
        }
        let bytes = usize::try_from(target.source.byte_end - target.source.byte_start)
            .map_err(|_| InvestigationError::LimitExceeded)?;
        let retained_bytes = self
            .supplied_strings
            .iter()
            .chain(&self.graph_strings)
            .try_fold(0usize, |sum, text| sum.checked_add(text.len()))
            .and_then(|sum| sum.checked_add(bytes))
            .ok_or(InvestigationError::LimitExceeded)?;
        if source_citations.len() >= policy.limits().evidence_items
            || source_citations
                .iter()
                .try_fold(bytes, |sum, citation| {
                    sum.checked_add(
                        (citation.source.as_ref()?.byte_end - citation.source.as_ref()?.byte_start)
                            as usize,
                    )
                })
                .is_none_or(|sum| sum > policy.limits().evidence_bytes)
            || retained_bytes > MAX_INPUT_BYTES
            || self
                .supplied_strings
                .len()
                .checked_add(self.graph_strings.len())
                .and_then(|count| count.checked_add(1))
                .is_none_or(|count| count > 16384)
        {
            return Err(InvestigationError::LimitExceeded);
        }
        // Production read_evidence appends a fresh ordinal even for repeated
        // successful reads; it never deduplicates acquired source citations.
        let id = format!(
            "evidence-{}",
            source_citations
                .len()
                .checked_add(1)
                .ok_or(InvestigationError::LimitExceeded)?
        );
        if self
            .ledger
            .citations
            .iter()
            .any(|citation| citation.citation_id == id)
        {
            return Err(InvestigationError::InvalidInput);
        }
        self.ledger
            .revision
            .checked_add(1)
            .ok_or(InvestigationError::LimitExceeded)?;
        let origin = InvestigationEvidenceOrigin::CapturedPrimarySource {
            registered_source_id: target.registered_source_id.clone(),
            receipt_id: target.receipt_id.clone(),
            receipt_inventory_index: target.receipt_inventory_index,
            captured: target.captured.clone(),
            scope: PrimarySourceScope::PrimarySourceOnly,
            input_closure: InputClosureStatus::InputClosureNotEstablished,
        };
        let role = Some(target.role);
        let index = Some(target.index);
        let handle = CitationPromptHandle {
            citation_id: &id,
            fact: &target.fact,
            origin: &origin,
            role: &role,
            index: &index,
        };
        prompt["citations"]
            .as_array_mut()
            .ok_or(InvestigationError::InvalidInput)?
            .push(serde_json::to_value(handle).map_err(|_| InvestigationError::InvalidInput)?);
        // Both recipes emit bare lower64hex (no prefix); their input domains
        // change with the new text hash, their encoded widths do not. Query IDs
        // exclude source citations and therefore remain unchanged.
        if !prompt["input_ledger_hash"]
            .as_str()
            .is_some_and(crate::source_basis::hash)
        {
            return Err(InvestigationError::InvalidInput);
        }
        prompt["input_ledger_hash"] = "f".repeat(64).into();
        let registry = &mut prompt["compact_context"]["query_history"]["registry_hash"];
        if !registry.as_str().is_some_and(crate::source_basis::hash) {
            return Err(InvestigationError::InvalidInput);
        }
        *registry = "f".repeat(64).into();
        prompt["turn_progress"]["last_completed_action"] =
            serde_json::to_value(InvestigationCompletedAction::ReadEvidence {
                fact: target.fact.clone(),
                role: target.role,
                index: target.index,
                citation_id: id.clone(),
                source_range_bytes: bytes as u64,
                // Unknown equality with a prior text hash: false is the longer
                // JSON spelling, including at every subsequent embedding.
                previously_acquired: false,
            })
            .map_err(|_| InvestigationError::InvalidInput)?;
        if let Some(requirements) = prompt.get_mut("inspection_progress") {
            let targets = self
                .inspection_requirements
                .as_ref()
                .ok_or(InvestigationError::InvalidInput)?;
            let expected = serde_json::json!({"requirements":targets.iter().map(|item| serde_json::json!({
                "target_id":item.target_id,
                "acquired":self.ledger.citations.iter().any(|citation| item.matches(citation)),
                "exact_supply":"determined_by_finishing_disclosure",
            })).collect::<Vec<_>>()});
            if *requirements != expected {
                return Err(InvestigationError::InvalidInput);
            }
            for (entry, item) in requirements["requirements"]
                .as_array_mut()
                .ok_or(InvestigationError::InvalidInput)?
                .iter_mut()
                .zip(targets)
            {
                // Matching requires no unknown source-text hash.
                if item == target {
                    entry["acquired"] = true.into();
                }
            }
        } else if self.inspection_requirements.is_some() {
            return Err(InvestigationError::InvalidInput);
        }
        let mut action = self.action(action_id.into())?;
        let span_index = action.payload.spans.len();
        action.payload.spans.push(PayloadSpan {
            id,
            repo: target.source.repo.clone(),
            path: target.source.path.clone(),
            byte_start: target.source.byte_start,
            byte_end: target.source.byte_end,
            commit_sha: target.source.commit_sha.clone(),
            text: String::new(),
        });
        // AC-0327: check the source descriptor envelope before reserve growth
        // and the final wire check, including source-preview rejection fixtures.
        #[cfg(test)]
        assert!(context_hub::logical::count(&prompt).unwrap() <= MAX_INPUT_BYTES + 4 * 8 * 1024);
        self.measure_reserved_projection(
            prompt,
            contract,
            action,
            profile,
            limits,
            policy,
            Some((span_index, bytes, source_sizes)),
            Some(target),
            protect,
        )
    }

    /// The actual prompt's disclosed state, never a synthetic reserve view.
    /// Host journal comparison remains necessary to authenticate feedback/count.
    pub(in crate::investigation) fn disclosed_admission_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<DisclosedAdmissionState, InvestigationError> {
        let (prompt, _) = self.admission_reserve_basis(policy)?;
        let progress = &prompt["turn_progress"];
        let state = DisclosedAdmissionState {
            acquisition: self
                .acquisition_choices
                .as_ref()
                .map(|_| self.acquisition_stamp(&self.payload))
                .transpose()?,
            schema_version: 1,
            mode: serde_json::from_value(progress["admission_mode"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?,
            consumed_budgets: serde_json::from_value(prompt["consumed_budgets"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?,
            capacity_refusal_count: serde_json::from_value(
                progress["capacity_refusal_count"].clone(),
            )
            .map_err(|_| InvestigationError::InvalidInput)?,
            capacity_feedback: serde_json::from_value(progress["capacity_feedback"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?,
            turn_progress_hash: core_prov::content_hash(&bounded_bytes(progress, MAX_INPUT_BYTES)?),
        };
        let metadata: ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        if self.acquisition_choices.is_some() {
            state.validate_saved_role_shape(self.frozen_specialist()?, policy)?;
            state.validate_mode_with_metadata(&metadata, policy)?;
        } else {
            state.validate_with_metadata(&metadata, policy)?;
        }
        Ok(state)
    }

    pub(super) fn admission_reserve_basis(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<(Value, InvestigationCompletionContract), InvestigationError> {
        if policy.profile() != INVESTIGATION_V2_POLICY.profile() {
            return Err(InvestigationError::InvalidInput);
        }
        let role = self.frozen_specialist()?;
        if ![
            SpecialistId::DomainAnalystV10,
            SpecialistId::EvidenceAuditorV10,
            SpecialistId::DomainAnalystV11,
            SpecialistId::DomainAnalystV12,
            SpecialistId::DomainAnalystV13,
            SpecialistId::DomainAnalystV14,
            SpecialistId::DomainAnalystV15,
            SpecialistId::DomainAnalystV17,
            SpecialistId::DomainAnalystV18,
            SpecialistId::DomainAnalystV19,
            SpecialistId::DomainAnalystV20,
            SpecialistId::DomainAnalystV21,
            SpecialistId::DomainAnalystV22,
            SpecialistId::DomainAnalystV23,
            SpecialistId::DomainAnalystV24,
            SpecialistId::DomainAnalystV25,
            SpecialistId::DomainAnalystV26,
            SpecialistId::DomainAnalystV27,
            SpecialistId::DomainAnalystV28,
            SpecialistId::DomainAnalystV29,
            SpecialistId::DomainAnalystV30,
            SpecialistId::DomainAnalystV31,
            SpecialistId::DomainAnalystV32,
            SpecialistId::EvidenceAuditorV11,
            SpecialistId::EvidenceAuditorV12,
            SpecialistId::EvidenceAuditorV13,
            SpecialistId::EvidenceAuditorV14,
        ]
        .contains(&role)
        {
            return Err(InvestigationError::InvalidInput);
        }
        self.ledger.validate_for_specialist(role, policy)?;
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        if role.uses_acquisition_choices() {
            self.validate_acquisition_projection()?;
        } else {
            response_schema::validate(&self.ledger, &self.payload, &prompt, policy)?;
        }
        let proof = self
            .ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        if prompt["schema_version"] != 9
            || prompt["policy"] != QUERY_HISTORY_DISCLOSURE_POLICY
            || prompt["input_ledger_hash"] != self.ledger.fingerprint_with_policy(policy)?
            || prompt["context_id"] != proof.context_id
            || prompt["coverage_qualification"]
                != serde_json::to_value(&proof.qualification)
                    .map_err(|_| InvestigationError::InvalidInput)?
            || prompt["turn_progress"]["contract"] != "investigation-turn-progress@4"
        {
            return Err(InvestigationError::InvalidInput);
        }
        exact_admission_fields_extended(
            &prompt["turn_progress"],
            &[
                "contract",
                "input_revision",
                "phase",
                "last_completed_action",
                "raw_query_budget",
                "admission_mode",
                "admission_instructions",
                "capacity_refusal_count",
                "capacity_feedback",
            ],
            &[
                if role.uses_finish_patch() && self.finish_patch.is_some() {
                    &[
                        "acquisition_choices",
                        "action_feedback",
                        "citation_correction",
                        "finish_patch",
                    ][..]
                } else if role.uses_typed_citation_correction() {
                    &[
                        "acquisition_choices",
                        "action_feedback",
                        "citation_correction",
                    ][..]
                } else if role.uses_acquisition_choices() {
                    &["acquisition_choices", "action_feedback"][..]
                } else {
                    &[][..]
                },
                // @28 requires its model-visible selected schema.
                if role.uses_visible_action_schema() {
                    &["response_schema"][..]
                } else {
                    &[][..]
                },
            ]
            .concat(),
        )?;
        exact_admission_fields(
            &prompt["completion_metadata"],
            &[
                "schema_version",
                "request_hash",
                "obligations_hash",
                "completed_attempt_ids",
                "refusal_count",
                "last_refusal_id",
                "feedback",
            ],
        )?;
        let contract: InvestigationCompletionContract =
            serde_json::from_value(prompt["completion_contract"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        contract.validate_with_policy(policy)?;
        let metadata: ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        metadata.validate_with_policy(policy)?;
        let usage: InvestigationUsage = serde_json::from_value(prompt["consumed_budgets"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        if metadata.request_hash != contract.request_hash
            || metadata.obligations_hash != contract.obligations_hash
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mode = if self.finish_patch.is_some()
            || self.finish_policy_complete(policy)
            || self.capacity_exhausted()
            || self
                .action_feedback
                .as_ref()
                .is_some_and(|feedback| feedback.reason == ActionRefusalReason::RepeatedRead)
        {
            CompletionCallMode::FinishOnly
        } else {
            metadata.next_call_mode_with_policy(&usage, policy)?
        };
        let count = prompt["turn_progress"]["capacity_refusal_count"]
            .as_u64()
            .filter(|value| {
                *value <= u64::from(usage.model_invocations)
                    && *value <= u64::from(policy.limits().model_invocations)
            })
            .ok_or(InvestigationError::InvalidInput)? as u32;
        let feedback: Option<AdmissionCapacityFeedback> =
            serde_json::from_value(prompt["turn_progress"]["capacity_feedback"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        if let Some(value) = &feedback {
            value.validate()?;
            if count == 0 {
                return Err(InvestigationError::InvalidInput);
            }
        }
        let mut expected = prompt["turn_progress"].clone();
        apply_admission_progress(&mut expected, mode, count, &feedback)?;
        if self.finish_patch.is_some() {
            expected["admission_instructions"] = PATCH_INSTRUCTIONS.into();
        }
        if expected != prompt["turn_progress"] {
            return Err(InvestigationError::InvalidInput);
        }
        Ok((prompt, contract))
    }
}

/// No conversion to the real capacity DTO: this is serialization data only.
#[derive(Serialize)]
struct CapacityFeedbackSizeEnvelope {
    schema_version: u32,
    action_hash: String,
    reason: CapacityRefusalReason,
}

// Bound the synthetic prompt BEFORE replace_prompt applies the wire cap. Each
// original numeric/null byte can grow to at most 20 decimal digits. Count new
// protocol fields using their actual serializers and production instructions.
pub(super) fn reserve_projection_bytes(policy: &InvestigationPolicy) -> Option<usize> {
    use context_hub::logical::{count, sum};
    let metadata = serde_json::json!({
        "completed_attempt_ids": vec!["f".repeat(64); policy.max_completed_attempts()],
        "refusal_count": u64::MAX,
        "last_refusal_id": "f".repeat(64),
        "feedback": null
    });
    let feedback = CapacityFeedbackSizeEnvelope {
        schema_version: 1,
        action_hash: "f".repeat(64),
        reason: CapacityRefusalReason::UnprovenContribution,
    };
    let mut progress_bytes = 0;
    for mode in [CompletionCallMode::Choice, CompletionCallMode::FinishOnly] {
        // Longest genuine phase; the action itself is already in the input.
        let mut progress = serde_json::json!({
            "last_completed_action": {"type": "read_coverage_inventory"}
        });
        apply_admission_progress(&mut progress, mode, u32::MAX, &Some(&feedback)).ok()?;
        progress["action_feedback"] = serde_json::to_value(ActionRefusalFeedback {
            query_budget: Some(query_refusal::QueryBudgetDiagnostic::reservation()),
            reason: ActionRefusalReason::QueryPageBudget,
            patch_rejection: None,
            invocation_ordinal: u32::MAX,
            action_hash: "f".repeat(64),
            choices_hash: "f".repeat(64),
        })
        .ok()?;
        progress_bytes = progress_bytes.max(count(&progress)?);
    }
    sum(&[
        MAX_INPUT_BYTES.checked_mul(20)?,
        MAX_ACTION_BYTES, // future source choices can expose a larger bounded prefix
        MAX_INPUT_BYTES,  // @28 visible selected schema (bounded transport encoding)
        count(&metadata)?,
        progress_bytes,
        staged_finish::RefusalFeedback::max_bytes_with_policy(policy).ok()?,
        // Two version-two sizing strings: each encodes to 2B+2, plus key/frame.
        staged_finish::RefusalFeedback::max_bytes_with_policy(policy)
            .ok()?
            .checked_mul(4)?
            .checked_add(128)?,
    ])
}

fn exact_admission_fields(value: &Value, names: &[&str]) -> Result<(), InvestigationError> {
    exact_admission_fields_extended(value, names, &[])
}
fn exact_admission_fields_extended(
    value: &Value,
    names: &[&str],
    extension: &[&str],
) -> Result<(), InvestigationError> {
    let object = value.as_object().ok_or(InvestigationError::InvalidInput)?;
    if object.len() != names.len() + extension.len()
        || object
            .keys()
            .any(|key| !names.contains(&key.as_str()) && !extension.contains(&key.as_str()))
    {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

fn maximize_progress_numbers(value: &mut Value) {
    match value {
        Value::Number(_) => *value = u64::MAX.into(),
        Value::Array(items) => items.iter_mut().for_each(maximize_progress_numbers),
        Value::Object(fields) => fields.values_mut().for_each(maximize_progress_numbers),
        _ => {}
    }
}

/// The actual builder and a future conservative sizing view use this same
/// enclosing representation. A Serialize value here is not admission authority;
/// this helper is private and only the validated DTO reaches the real builder.
pub(super) fn apply_admission_progress(
    progress: &mut Value,
    mode: CompletionCallMode,
    count: u32,
    feedback: &impl Serialize,
) -> Result<(), InvestigationError> {
    let fields = progress
        .as_object_mut()
        .ok_or(InvestigationError::InvalidInput)?;
    fields.insert("contract".into(), "investigation-turn-progress@4".into());
    let genuine_phase = match fields.get("last_completed_action") {
        Some(Value::Null) => "initial",
        Some(action) => match action["type"].as_str() {
            Some("query_context") => "after_query",
            Some("read_evidence") => "after_read",
            Some("read_coverage_inventory") => "after_coverage_read",
            _ => return Err(InvestigationError::InvalidInput),
        },
        None => return Err(InvestigationError::InvalidInput),
    };
    let (name, instructions) = match mode {
        CompletionCallMode::Choice => {
            fields.insert("phase".into(), genuine_phase.into());
            ("choice", CHOICE_INSTRUCTIONS)
        }
        CompletionCallMode::FinishOnly => {
            fields.insert("phase".into(), "finish_only".into());
            ("finish_only", FINISH_ONLY_INSTRUCTIONS)
        }
    };
    fields.insert("admission_mode".into(), name.into());
    fields.insert("admission_instructions".into(), instructions.into());
    fields.insert("capacity_refusal_count".into(), count.into());
    fields.insert(
        "capacity_feedback".into(),
        serde_json::to_value(feedback).map_err(|_| InvestigationError::InvalidInput)?,
    );
    Ok(())
}

#[cfg(test)]
#[path = "input_admission_tests.rs"]
mod tests;

impl InvestigationInput {
    /// Defense against responses that bypass @14/@15 transport hints. This grants
    /// no source access; guarded frozen receipts still authenticate ranges.
    pub fn validate_admitted_source_fact(
        &self,
        fact: &crate::TaskFactKey,
    ) -> Result<(), ActionDecodeFailure> {
        if matches!(
            response_schema::schema_role(&self.payload.system),
            Some(
                SpecialistId::DomainAnalystV14
                    | SpecialistId::DomainAnalystV15
                    | SpecialistId::DomainAnalystV17
            )
        ) && !self
            .ledger
            .selected_facts
            .iter()
            .any(|selection| &selection.fact == fact && selection.binding.is_some())
        {
            return Err(ActionSemanticRule::EvidenceShape.invalid());
        }
        Ok(())
    }
}

impl InvestigationInput {
    fn capacity_exhausted(&self) -> bool {
        matches!(
            self.specialist,
            SpecialistId::EvidenceAuditorV14
                | SpecialistId::DomainAnalystV21
                | SpecialistId::DomainAnalystV22
                | SpecialistId::DomainAnalystV23
                | SpecialistId::DomainAnalystV24
                | SpecialistId::DomainAnalystV25
                | SpecialistId::DomainAnalystV26
                | SpecialistId::DomainAnalystV27
                | SpecialistId::DomainAnalystV28
                | SpecialistId::DomainAnalystV29
                | SpecialistId::DomainAnalystV30
                | SpecialistId::DomainAnalystV31
                | SpecialistId::DomainAnalystV32
        ) && self
            .acquisition_choices
            .as_ref()
            .is_some_and(|choices| choices.capacity_exhausted())
    }

    fn uses_acquisition_prompt(&self) -> bool {
        self.frozen_specialist()
            .is_ok_and(SpecialistId::uses_acquisition_choices)
    }

    /// Acquisition input assembly; task activation is a separate host gate.
    /// The caller must authenticate the host journal before deriving choices.
    pub fn with_acquisition_choices(
        self,
        choices: std::sync::Arc<acquisition_choices::AcquisitionChoices>,
    ) -> Result<Self, InvestigationError> {
        self.with_acquisition_admission(choices, None)
    }
    pub fn with_acquisition_admission(
        self,
        choices: std::sync::Arc<acquisition_choices::AcquisitionChoices>,
        feedback: Option<ActionRefusalFeedback>,
    ) -> Result<Self, InvestigationError> {
        self.with_acquisition_correction(choices, feedback, None)
    }

    /// The host authenticates the saved refusal at its original cutoff before
    /// passing this witness. Current actions are rederived, never replayed.
    pub fn with_acquisition_correction(
        mut self,
        choices: std::sync::Arc<acquisition_choices::AcquisitionChoices>,
        feedback: Option<ActionRefusalFeedback>,
        correction: Option<&acquisition_choices::AuthenticatedCitationCorrection>,
    ) -> Result<Self, InvestigationError> {
        if !self.uses_acquisition_prompt() {
            return Err(InvestigationError::InvalidInput);
        }
        let mut prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let metadata: ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        let saved = metadata
            .feedback
            .as_ref()
            .and_then(|feedback| feedback.citation_correction.as_ref());
        let typed_role = self.frozen_specialist()?.uses_typed_citation_correction();
        if !typed_role
            && metadata
                .feedback
                .as_ref()
                .is_some_and(|f| f.prose_correction.is_some())
        {
            return Err(InvestigationError::InvalidInput);
        }

        self.citation_correction = match (saved, correction) {
            (Some(saved), Some(authenticated))
                if typed_role && saved == authenticated.original() =>
            {
                let contract = serde_json::from_value(prompt["completion_contract"].clone())
                    .map_err(|_| InvestigationError::InvalidInput)?;
                authenticated.project(
                    &choices,
                    &contract,
                    &metadata,
                    &self.ledger,
                    &Self::policy_from_protected_prompt(&prompt)?,
                )?
            }
            (None, None) => None,
            _ => return Err(InvestigationError::InvalidInput),
        };
        if typed_role {
            prompt["turn_progress"]["citation_correction"] =
                serde_json::to_value(&self.citation_correction)
                    .map_err(|_| InvestigationError::InvalidInput)?;
        }
        prompt["turn_progress"]["acquisition_choices"] =
            serde_json::to_value(&*choices).map_err(|_| InvestigationError::InvalidInput)?;
        if feedback
            .as_ref()
            .is_some_and(|value| value.query_budget.is_some())
            && !self.frozen_specialist()?.uses_query_budget_refusals()
        {
            return Err(InvestigationError::InvalidInput);
        }
        prompt["turn_progress"]["action_feedback"] =
            serde_json::to_value(&feedback).map_err(|_| InvestigationError::InvalidInput)?;
        self.action_feedback = feedback;
        if (self.input_generation_policy()?.local_finish_generation()
            && choices.attempts_complete())
            || (matches!(
                self.frozen_specialist()?,
                SpecialistId::EvidenceAuditorV14
                    | SpecialistId::DomainAnalystV21
                    | SpecialistId::DomainAnalystV22
                    | SpecialistId::DomainAnalystV23
                    | SpecialistId::DomainAnalystV24
                    | SpecialistId::DomainAnalystV25
                    | SpecialistId::DomainAnalystV26
                    | SpecialistId::DomainAnalystV27
                    | SpecialistId::DomainAnalystV28
                    | SpecialistId::DomainAnalystV29
                    | SpecialistId::DomainAnalystV30
                    | SpecialistId::DomainAnalystV31
                    | SpecialistId::DomainAnalystV32
            ) && choices.capacity_exhausted())
            || self
                .action_feedback
                .as_ref()
                .is_some_and(|value| value.reason == ActionRefusalReason::RepeatedRead)
        {
            prompt["turn_progress"]["admission_mode"] =
                serde_json::to_value(CompletionCallMode::FinishOnly)
                    .map_err(|_| InvestigationError::InvalidInput)?;
            prompt["turn_progress"]["admission_instructions"] = FINISH_ONLY_INSTRUCTIONS.into();
            prompt["turn_progress"]["phase"] = "finish_only".into();
        }
        let mode = serde_json::from_value(prompt["turn_progress"]["admission_mode"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        self.payload.response_schema = response_schema::select_acquisition_recovery_with_policy(
            mode,
            &choices,
            &self.ledger,
            prompt["completion_contract"]["request_hash"]
                .as_str()
                .ok_or(InvestigationError::InvalidInput)?,
            self.action_feedback.as_ref(),
            &Self::policy_from_protected_prompt(&prompt)?,
        )?;
        if typed_role {
            self.payload.response_schema = response_schema::select_eligible_acquisition(
                self.payload.response_schema.take(),
                mode,
                &choices,
                &*self.citable_ledger()?,
            )?;
        }
        self.payload.response_schema = response_schema::attach_visible(
            self.frozen_specialist()?,
            &mut prompt,
            self.payload.response_schema.take(),
        )?;
        self.acquisition_choices = Some(choices);
        self.acquisition_mode = Some(mode);
        wire::replace_prompt(&mut self.payload, &prompt, MAX_INPUT_BYTES)?;
        self.validate_acquisition_projection()?;
        Ok(self)
    }
    /// Projection only. The host authenticates this basis against the saved
    /// refusal and live holder before dispatch, reservation and settlement.
    pub fn with_finish_patch(
        mut self,
        basis: &finish_patch::PatchBasis,
    ) -> Result<Self, InvestigationError> {
        if !self.frozen_specialist()?.uses_finish_patch()
            || self.acquisition_choices.is_none()
            || self.finish_patch.is_some()
        {
            return Err(InvestigationError::InvalidInput);
        }
        basis
            .validate()
            .map_err(|_| InvestigationError::InvalidInput)?;
        let mut prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let metadata: ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        if metadata.refusal_count != 1
            || metadata.last_refusal_id.as_ref() != Some(&basis.refusal_hash)
        {
            return Err(InvestigationError::InvalidInput);
        }
        prompt["turn_progress"]["finish_patch"] =
            serde_json::to_value(basis).map_err(|_| InvestigationError::InvalidInput)?;
        prompt["turn_progress"]["admission_mode"] =
            serde_json::to_value(CompletionCallMode::FinishOnly)
                .map_err(|_| InvestigationError::InvalidInput)?;
        prompt["turn_progress"]["phase"] = "finish_only".into();
        prompt["turn_progress"]["admission_instructions"] = PATCH_INSTRUCTIONS.into();
        self.payload.response_schema = response_schema::attach_visible(
            self.frozen_specialist()?,
            &mut prompt,
            Some(
                basis
                    .response_schema(&*self.citable_ledger()?)
                    .map_err(|_| InvestigationError::InvalidInput)?,
            ),
        )?;
        self.finish_patch = Some(basis.clone());
        self.acquisition_mode = Some(CompletionCallMode::FinishOnly);
        wire::replace_prompt(&mut self.payload, &prompt, MAX_INPUT_BYTES)?;
        self.validate_acquisition_projection()?;
        Ok(self)
    }

    fn validate_finish_patch_projection(
        &self,
        prompt: &Value,
        metadata: &ProtectedCompletionMetadata,
    ) -> Result<(), InvestigationError> {
        let basis = self
            .finish_patch
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        basis
            .validate()
            .map_err(|_| InvestigationError::InvalidInput)?;
        if !self.frozen_specialist()?.uses_finish_patch()
            || self.acquisition_mode != Some(CompletionCallMode::FinishOnly)
            || metadata.refusal_count != 1
            || metadata.last_refusal_id.as_ref() != Some(&basis.refusal_hash)
            || prompt["turn_progress"]["admission_instructions"] != PATCH_INSTRUCTIONS
            || prompt["turn_progress"]["finish_patch"]
                != serde_json::to_value(basis).map_err(|_| InvestigationError::InvalidInput)?
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }

    /// Read the frozen policy embedded in the protected prompt. Host comparison
    /// to saved limits remains mandatory before any invocation is published.
    fn input_generation_policy(&self) -> Result<InvestigationPolicy, InvestigationError> {
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        Self::policy_from_protected_prompt(&prompt)
    }

    fn policy_from_protected_prompt(
        prompt: &Value,
    ) -> Result<InvestigationPolicy, InvestigationError> {
        let limits: InvestigationLimits = serde_json::from_value(prompt["limits"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        InvestigationPolicy::for_effective_limits(&limits).ok_or(InvestigationError::InvalidInput)
    }

    pub fn invocation_output_tokens(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<u32, InvestigationError> {
        if !policy.local_finish_generation() {
            return Ok(policy.limits().generated_tokens_per_invocation);
        }
        let state = self.disclosed_admission_with_policy(policy)?;
        Ok(
            if state
                .acquisition
                .as_ref()
                // AC-1443: a capacity-exhausted run is finish-only for good, so
                // its finish gets the same allowance as a completed one.
                .is_some_and(|stamp| stamp.finish_policy_complete || stamp.capacity_exhausted)
            {
                policy.output_tokens_for_mode(state.mode)
            } else {
                policy.limits().generated_tokens_per_invocation
            },
        )
    }

    fn finish_policy_complete(&self, policy: &InvestigationPolicy) -> bool {
        policy.local_finish_generation()
            && (self.finish_patch.is_some()
                || self
                    .acquisition_choices
                    .as_ref()
                    .is_some_and(|c| c.attempts_complete()))
    }

    /// An exact host-offered obligation query already at the page ceiling
    /// cannot recover by increasing its budget or shrinking its selection.
    pub fn is_maximum_obligation_query(&self, query: &InvestigationQuery) -> bool {
        query.max_bytes == MAX_QUERY_BYTES
            && query.max_facts == 1
            && matches!(query.scope, InvestigationScope::Node { .. })
            && self.validate_acquisition_projection().is_ok()
            && self.acquisition_choices.as_ref().is_some_and(|choices| {
                choices.entries().iter().any(|entry| {
                    matches!(
                        &entry.action,
                        acquisition_choices::AcquisitionAction::QueryContext { query: offered, .. }
                            if offered == query
                    )
                })
            })
    }

    pub fn permits_acquisition(&self, action: &QualifiedInvestigationAction) -> bool {
        !self.uses_acquisition_prompt()
            || (self.acquisition_mode == Some(staged_refusal::CompletionCallMode::Choice)
                && self.validate_acquisition_projection().is_ok()
                && self
                    .acquisition_choices
                    .as_ref()
                    .is_some_and(|choices| choices.permits_acquisition(action)))
    }
    pub(in crate::investigation) fn acquisition_stamp(
        &self,
        original: &llm::CompletionPayload,
    ) -> Result<AcquisitionAdmission, InvestigationError> {
        if self.payload != *original || !self.uses_acquisition_prompt() {
            return Err(InvestigationError::InvalidInput);
        }
        self.validate_acquisition_projection()?;
        Ok(AcquisitionAdmission {
            finish_patch: self.finish_patch.clone(),
            finish_policy_complete: self.finish_policy_complete(&self.input_generation_policy()?),
            capacity_exhausted: self.capacity_exhausted(),
            finish_reason: self
                .action_feedback
                .as_ref()
                .filter(|value| value.reason == ActionRefusalReason::RepeatedRead)
                .map(|value| value.reason),
            choices_hash: self
                .acquisition_choices
                .as_ref()
                .ok_or(InvestigationError::InvalidInput)?
                .stamp_hash()?,
            action_feedback_hash: self
                .action_feedback
                .as_ref()
                .map(|feedback| feedback.stamp_hash())
                .transpose()?,
        })
    }
    pub(super) fn validate_acquisition_projection(&self) -> Result<(), InvestigationError> {
        if !self.uses_acquisition_prompt() {
            return if self.acquisition_choices.is_none()
                && self.action_feedback.is_none()
                && self.citation_correction.is_none()
                && self.acquisition_mode.is_none()
                && self.finish_patch.is_none()
            {
                Ok(())
            } else {
                Err(InvestigationError::InvalidInput)
            };
        }
        let choices = self
            .acquisition_choices
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let contract = serde_json::from_value(prompt["completion_contract"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        let metadata: ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        choices.validate_basis(
            &contract,
            &metadata,
            &self.ledger,
            &Self::policy_from_protected_prompt(&prompt)?,
        )?;
        let mode = serde_json::from_value(prompt["turn_progress"]["admission_mode"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        if SpecialistId::is_typed_citation_prompt(&self.payload.system) {
            if prompt["turn_progress"].get("citation_correction")
                != Some(
                    &serde_json::to_value(&self.citation_correction)
                        .map_err(|_| InvestigationError::InvalidInput)?,
                )
            {
                return Err(InvestigationError::InvalidInput);
            }
        } else if self.citation_correction.is_some()
            || prompt["turn_progress"].get("citation_correction").is_some()
            || metadata
                .feedback
                .as_ref()
                .is_some_and(|feedback| feedback.citation_correction.is_some())
        {
            return Err(InvestigationError::InvalidInput);
        }
        if self.finish_patch.is_none() && prompt["turn_progress"].get("finish_patch").is_some() {
            return Err(InvestigationError::InvalidInput);
        }
        let ordinary_schema = response_schema::select_acquisition_recovery_with_policy(
            mode,
            choices,
            &self.ledger,
            &contract.request_hash,
            self.action_feedback.as_ref(),
            &Self::policy_from_protected_prompt(&prompt)?,
        )?;
        let expected_schema = if let Some(patch) = &self.finish_patch {
            self.validate_finish_patch_projection(&prompt, &metadata)?;
            Some(
                patch
                    .response_schema(&*self.citable_ledger()?)
                    .map_err(|_| InvestigationError::InvalidInput)?,
            )
        } else if SpecialistId::is_typed_citation_prompt(&self.payload.system) {
            response_schema::select_eligible_acquisition(
                ordinary_schema,
                mode,
                choices,
                &*self.citable_ledger()?,
            )?
        } else {
            ordinary_schema
        };
        let (expected_schema, visible) =
            response_schema::visible_projection(self.frozen_specialist()?, expected_schema)?;
        if self.acquisition_mode != Some(mode)
            || prompt["turn_progress"].get("response_schema") != visible.as_ref()
            || prompt["turn_progress"]["acquisition_choices"]
                != serde_json::to_value(&**choices).map_err(|_| InvestigationError::InvalidInput)?
            || prompt["turn_progress"].get("action_feedback")
                != Some(
                    &serde_json::to_value(&self.action_feedback)
                        .map_err(|_| InvestigationError::InvalidInput)?,
                )
            || self.payload.response_schema != expected_schema
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}
