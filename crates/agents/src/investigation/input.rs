//! Transient input ownership. Stored manifests contain only the fingerprints of
//! these exact query/source/history inputs, never their raw text.
use super::*;
use crate::AgentEvidence;
use core_prov::content_hash;
use llm::{AnalysisTier, CompletionAction, CompletionPayload, PayloadSpan};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_PRIVACY_STRINGS: usize = 16384;

/// Prepared once per model step after coherent host acquisition. All fields are
/// private so replacing query/source bytes requires revalidation and a new hash.
#[derive(Clone)]
pub struct InvestigationInput {
    ledger: InvestigationInputLedger,
    payload: CompletionPayload,
    supplied_strings: Vec<String>,
    graph_strings: Vec<String>,
    compact: bool,
    inspection_requirements: Option<Vec<InvestigationInspectionTarget>>,
}

/// Shared compact citation representation. It contains complete source origin
/// metadata but no source text or text hash; sizing views reuse this serializer.
#[derive(serde::Serialize)]
struct CitationPromptHandle<'a> {
    citation_id: &'a str,
    fact: &'a crate::TaskFactKey,
    origin: &'a InvestigationEvidenceOrigin,
    role: &'a Option<crate::TaskRangeRole>,
    index: &'a Option<u32>,
}
impl<'a> From<&'a InvestigationCitation> for CitationPromptHandle<'a> {
    fn from(citation: &'a InvestigationCitation) -> Self {
        Self {
            citation_id: &citation.citation_id,
            fact: &citation.fact,
            origin: &citation.origin,
            role: &citation.role,
            index: &citation.index,
        }
    }
}

impl InvestigationInput {
    /// Provider-observed identity fields are untrusted output too. Tool steps
    /// may retain them only after the same source/metadata admission policy.
    pub fn validate_response_metadata(
        &self,
        model: Option<&str>,
    ) -> Result<(), InvestigationError> {
        if let Some(model) = model {
            if !text(model, 256) {
                return Err(InvestigationError::InvalidInput);
            }
            reject_prose_replay([model], self.supplied_strings.iter().map(String::as_str))?;
            super::privacy::reject_metadata_replay(
                [model],
                self.graph_strings.iter().map(String::as_str),
            )?;
        }
        Ok(())
    }
    /// The host supplies canonical query-response JSON in original response order
    /// and exact source spans. This pure method verifies their hashes and bound
    /// membership; it cannot verify that the host read a database or source file.
    pub fn prepare(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
    ) -> Result<Self, InvestigationError> {
        if specialist.uses_turn_progress() {
            return Err(InvestigationError::InvalidInput);
        }
        Self::prepare_base(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
        )
    }

    fn prepare_base(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
    ) -> Result<Self, InvestigationError> {
        Self::prepare_base_with_policy(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            &INVESTIGATION_V1_POLICY,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn prepare_base_with_policy(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::prepare_base_projected(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            policy,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_base_projected<P: AsRef<str> + serde::Serialize>(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[P],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        policy: &InvestigationPolicy,
        request_hash: Option<&str>,
    ) -> Result<Self, InvestigationError> {
        if specialist.uses_query_history_disclosure() != request_hash.is_some() {
            return Err(InvestigationError::InvalidInput);
        }
        if specialist.uses_policy_selection() != (policy.profile() == "investigation-v2") {
            return Err(InvestigationError::InvalidInput);
        }
        ledger.validate_with_policy(policy)?;
        // Qualified ledgers require their own preparation path; legacy prompts
        // must never silently discard mandatory coverage.
        if ledger.schema_version
            != if specialist.uses_coverage_qualification() {
                2
            } else {
                1
            }
            || !text(question, MAX_QUESTION_BYTES)
            || query_pages.len() != ledger.queries.len()
            || evidence.len() > policy.limits().evidence_items
            || supplied_history.len() > 12
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut pages = Vec::new();
        let mut supplied_strings = Vec::new();
        let mut graph_strings = Vec::new();
        let mut total_query_bytes = 0usize;
        for (raw, manifest) in query_pages.iter().zip(&ledger.queries) {
            let raw = raw.as_ref();
            if raw.len() != manifest.response_bytes
                || content_hash(raw.as_bytes()) != manifest.response_hash
            {
                return Err(InvestigationError::InvalidInput);
            }
            total_query_bytes = total_query_bytes
                .checked_add(raw.len())
                .ok_or(InvestigationError::LimitExceeded)?;
            if total_query_bytes > MAX_TOTAL_QUERY_BYTES {
                return Err(InvestigationError::LimitExceeded);
            }
            strict::preflight(raw, MAX_QUERY_BYTES, 32, 16384, 16384)?;
            let page: Value =
                serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidInput)?;
            collect_strings(&page, &mut graph_strings);
            pages.push(page);
        }
        let source_citations = ledger
            .citations
            .iter()
            .filter(|c| !matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata))
            .map(|c| (c.citation_id.as_str(), c))
            .collect::<BTreeMap<_, _>>();
        if source_citations.len() != evidence.len() {
            return Err(InvestigationError::InvalidInput);
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut spans = Vec::new();
        let mut source_bytes = 0usize;
        for source in evidence {
            let citation = source_citations
                .get(source.id.as_str())
                .ok_or(InvestigationError::InvalidInput)?;
            if !seen.insert(&source.id)
                || citation.source.as_ref() != Some(&source.source)
                || source.text.len() > 8192
                || source.text.is_empty()
                || source.source.byte_end - source.source.byte_start != source.text.len() as u64
                || citation.text_hash.as_deref()
                    != Some(content_hash(source.text.as_bytes()).as_str())
            {
                return Err(InvestigationError::InvalidInput);
            }
            source_bytes = source_bytes
                .checked_add(source.text.len())
                .ok_or(InvestigationError::LimitExceeded)?;
            if source_bytes > policy.limits().evidence_bytes {
                return Err(InvestigationError::LimitExceeded);
            }
            supplied_strings.push(source.text.clone());
            spans.push(PayloadSpan {
                id: source.id.clone(),
                repo: source.source.repo.clone(),
                path: source.source.path.clone(),
                byte_start: source.source.byte_start,
                byte_end: source.source.byte_end,
                commit_sha: source.source.commit_sha.clone(),
                text: source.text.clone(),
            });
        }
        if supplied_history.is_empty() {
            if ledger.supplied_history_hash.is_some() || ledger.supplied_history_bytes != 0 {
                return Err(InvestigationError::InvalidInput);
            }
        } else {
            let history_bytes = bounded_bytes(&supplied_history, 16 * 1024)?;
            if ledger.supplied_history_bytes != history_bytes.len()
                || ledger.supplied_history_hash.as_deref()
                    != Some(content_hash(&history_bytes).as_str())
            {
                return Err(InvestigationError::InvalidInput);
            }
            for raw in supplied_history {
                strict::preflight(raw, 16 * 1024, 32, 16384, 128)?;
                let history: PriorHistory = decode_record(raw)?;
                if let Some(result) = history.saved_result {
                    for finding in result.findings {
                        supplied_strings.push(finding.title);
                        supplied_strings.push(finding.statement);
                        supplied_strings.extend(finding.limitations);
                    }
                    supplied_strings.extend(result.limitations);
                }
                // Metadata IDs/status are valid vocabulary. Nested finding
                // prose above retains complete-short-item source protection.
                let value: Value =
                    serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidInput)?;
                collect_strings(&value, &mut graph_strings);
            }
        }
        if supplied_strings.len() + graph_strings.len() > MAX_PRIVACY_STRINGS
            || supplied_strings
                .iter()
                .chain(&graph_strings)
                .map(String::len)
                .sum::<usize>()
                > MAX_INPUT_BYTES
        {
            return Err(InvestigationError::LimitExceeded);
        }
        let compact = specialist.uses_compact_disclosure();
        let prompt_value = if compact {
            // Compact projection reads the original query wires. The parsed
            // pages have already supplied the privacy strings above.
            drop(pages);
            // Select before compaction: routing a new history through the old
            // cumulative selector representation would reject a fitting new input.
            let context = match request_hash {
                Some(hash) => serde_json::to_value(ProgressiveInvestigationContext::prepare_raw(
                    hash,
                    &ledger,
                    query_pages,
                    policy,
                )?),
                None => serde_json::to_value(CompactInvestigationContext::prepare_raw_with_policy(
                    &ledger,
                    query_pages,
                    policy,
                )?),
            }
            .map_err(|_| InvestigationError::InvalidInput)?;
            let citations: Vec<_> = ledger
                .citations
                .iter()
                .map(CitationPromptHandle::from)
                .collect();
            serde_json::json!({
                "schema_version": 2,
                "policy": COMPACT_DISCLOSURE_POLICY,
                "question": question,
                "input_ledger_hash": ledger.fingerprint_with_policy(policy)?,
                "graph_snapshot_id": ledger.graph_snapshot_id,
                "scope_snapshot_id": ledger.scope_snapshot_id,
                "compact_context": context,
                "citations": citations,
                "prior_findings_as_t3_history": supplied_history,
                "consumed_budgets": usage,
                "limits": policy.limits(),
                "coverage": "Bounded recovered context only. Captured source attests its primary source, not complete producer input closure. Unknown evidence is not absence. Findings remain T3/InferredWeak."
            })
        } else {
            serde_json::json!({
                "schema_version": 1,
                "question": question,
                "input_ledger": ledger,
                "input_ledger_hash": ledger.fingerprint_with_policy(policy)?,
                "context_pages": pages,
                "prior_findings_as_t3_history": supplied_history,
                "consumed_budgets": usage,
                "limits": policy.limits(),
                "coverage": "This is a bounded recovered-context investigation. Captured source attests only its primary source; full producer input closure is not established. Unknown evidence is not proof of absence. All generated findings remain T3/InferredWeak."
            })
        };
        let mut prompt_value = prompt_value;
        if specialist.uses_coverage_qualification() {
            let proof = ledger
                .coverage
                .as_ref()
                .ok_or(InvestigationError::InvalidInput)?;
            // Admission validates the proof and its exact scoped snapshot. Only
            // the public block is supplied, never the recorded host-only basis.
            prompt_value["coverage_qualification"] = serde_json::to_value(&proof.qualification)
                .map_err(|_| InvestigationError::InvalidInput)?;
            prompt_value["context_id"] = proof.context_id.clone().into();
            collect_strings(&prompt_value["coverage_qualification"], &mut graph_strings);
            if graph_strings.len() + supplied_strings.len() > MAX_PRIVACY_STRINGS
                || graph_strings
                    .iter()
                    .chain(&supplied_strings)
                    .map(String::len)
                    .sum::<usize>()
                    > MAX_INPUT_BYTES
            {
                return Err(InvestigationError::LimitExceeded);
            }
        }
        let mut payload = CompletionPayload {
            system: specialist.prompt(),
            prompt: String::new(),
            spans,
        };
        wire::replace_prompt(&mut payload, &prompt_value, MAX_INPUT_BYTES)?;
        Ok(Self {
            ledger,
            payload,
            supplied_strings,
            graph_strings,
            compact,
            inspection_requirements: None,
        })
    }

    pub fn ledger(&self) -> &InvestigationInputLedger {
        &self.ledger
    }

    /// Host-selected bounded working set. Acquisition and replay guards retain
    /// every source; omitted source handles remain visible for reacquisition.
    pub fn select_sources(
        mut self,
        selected: &std::collections::BTreeSet<String>,
        reason: DisclosureOmissionReason,
    ) -> Result<Self, InvestigationError> {
        if !self.compact
            || matches!(
                reason,
                DisclosureOmissionReason::DuplicateRepresentation
                    | DisclosureOmissionReason::ReferenceOnly
            )
        {
            return Err(InvestigationError::InvalidInput);
        }
        let available = self.payload.spans.iter().map(|s| s.id.clone()).collect();
        if !selected.is_subset(&available) {
            return Err(InvestigationError::InvalidInput);
        }
        let mut prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        // One selection per prepared input: omitted sources cannot be silently
        // reintroduced without revalidating their original acquired bytes.
        if prompt.get("omitted_sources").is_some() {
            return Err(InvestigationError::InvalidInput);
        }
        let citations = prompt
            .get_mut("citations")
            .and_then(Value::as_array_mut)
            .ok_or(InvestigationError::InvalidInput)?;
        let omitted = citations
            .iter()
            .filter(|c| {
                c.get("citation_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| {
                        self.payload.spans.iter().any(|s| s.id == id) && !selected.contains(id)
                    })
            })
            .map(|c| serde_json::json!({"handle": c, "reason": reason}))
            .collect::<Vec<_>>();
        citations.retain(|c| !omitted.iter().any(|o| o["handle"] == *c));
        prompt["omitted_sources"] = serde_json::json!(omitted);
        self.payload
            .spans
            .retain(|span| selected.contains(&span.id));
        wire::replace_prompt(&mut self.payload, &prompt, MAX_INPUT_BYTES)?;
        Ok(self)
    }

    pub fn action(&self, action_id: String) -> Result<CompletionAction, InvestigationError> {
        if !super::records::opaque_id(&action_id) {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(CompletionAction {
            action_id,
            tier: AnalysisTier::Agentic,
            payload: self.payload.clone(),
        })
    }

    /// Seals metadata against the exact firewall-prepared representation.
    pub fn seal_disclosure(
        &self,
        prepared: &llm::bounded::PreparedBoundedCompletion,
    ) -> Result<Option<InvestigationDisclosure>, InvestigationError> {
        self.seal_disclosure_with_policy(prepared, &INVESTIGATION_V1_POLICY)
    }

    /// The host passes the policy resolved from the exact saved task limits.
    pub fn seal_disclosure_with_policy(
        &self,
        prepared: &llm::bounded::PreparedBoundedCompletion,
        policy: &InvestigationPolicy,
    ) -> Result<Option<InvestigationDisclosure>, InvestigationError> {
        if !self.compact {
            return Ok(None);
        }
        let action = self.action(prepared.preview().egress.action_id.clone())?;
        if !prepared.matches_action(&action) {
            return Err(InvestigationError::InvalidInput);
        }
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let admission = if prompt["schema_version"] == 9 {
            Some(self.disclosed_admission_with_policy(policy)?)
        } else {
            None
        };
        match admission {
            Some(admission) => super::sealing::seal_with_admission_policy(
                &self.ledger,
                &self.payload,
                prepared,
                policy,
                self.inspection_requirements.as_deref(),
                Some(admission),
            ),
            None => super::sealing::seal_with_policy(
                &self.ledger,
                &self.payload,
                prepared,
                policy,
                self.inspection_requirements.as_deref(),
            ),
        }
        .map(Some)
    }

    pub fn admit_result(
        &self,
        investigation_id: &str,
        action: &InvestigationAction,
        observed_response_model: Option<String>,
        created_at: String,
    ) -> Result<InvestigationResult, InvestigationError> {
        if self.compact {
            return Err(InvestigationError::InvalidInput);
        }
        self.admit_result_for_step(
            investigation_id,
            "legacy",
            None,
            action,
            observed_response_model,
            created_at,
        )
    }

    /// Binds the sealed completion contract and metadata to this request
    /// under the policy the host resolved from the exact saved task limits.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn validate_completion_binding_with_policy(
        &self,
        request: &obligations::StagedObligationRequest,
        disclosure: &InvestigationDisclosure,
        attempts: &[staged_attempt::AttemptEvidence<'_>],
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let contract: InvestigationCompletionContract =
            serde_json::from_value(prompt["completion_contract"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        let metadata: staged_refusal::ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        let request = request.normalized_with_policy(policy)?;
        let progressive = matches!(request.schema_version, 6 | 7);
        let admission = request.schema_version == 7;
        let schema = match request.schema_version {
            4 => 6,
            5 => 7,
            6 => 8,
            7 => 9,
            _ => return Err(InvestigationError::InvalidInput),
        };
        if prompt["schema_version"] != schema
            || disclosure.schema_version
                != if admission {
                    6
                } else if progressive {
                    5
                } else {
                    4
                }
            || (progressive
                && disclosure
                    .query_history
                    .as_ref()
                    .is_none_or(|binding| binding.request_hash != contract.request_hash))
            || contract.request_hash != request.intent_hash_with_policy(policy)?
            || contract.obligations != request.obligations
            || contract.obligations_hash != request.obligations.fingerprint_with_policy(policy)?
            || metadata.request_hash != contract.request_hash
            || metadata.obligations_hash != contract.obligations_hash
            || metadata.completed_attempt_ids
                != attempts
                    .iter()
                    .map(|item| item.record.attempt_id.clone())
                    .collect::<Vec<_>>()
            || disclosure.completion_contract_hash.as_deref()
                != Some(contract.fingerprint_with_policy(policy)?.as_str())
            || disclosure.completion_metadata_hash.as_deref()
                != Some(metadata.fingerprint_with_policy(policy)?.as_str())
        {
            return Err(InvestigationError::InvalidInput);
        }
        if admission {
            if disclosure.admission.as_ref() != Some(&self.disclosed_admission_with_policy(policy)?)
            {
                return Err(InvestigationError::InvalidInput);
            }
        } else if disclosure.admission.is_some() {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }

    /// V7 finish admission returns a genuine schema5 result only after exact
    /// obligations are satisfied; a refused finish carries no publishable result.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_completion_finish(
        &self,
        request: &obligations::StagedObligationRequest,
        task: &str,
        step: &str,
        disclosure: &InvestigationDisclosure,
        finish: &staged_finish::StagedFinish,
        attempts: &[staged_attempt::AttemptEvidence<'_>],
        model: Option<String>,
        timestamp: String,
    ) -> Result<Result<InvestigationResult, staged_finish::FinishDecision>, InvestigationError>
    {
        self.admit_completion_finish_with_policy(
            request,
            task,
            step,
            disclosure,
            finish,
            attempts,
            model,
            timestamp,
            &INVESTIGATION_V1_POLICY,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admit_completion_finish_with_policy(
        &self,
        request: &obligations::StagedObligationRequest,
        task: &str,
        step: &str,
        disclosure: &InvestigationDisclosure,
        finish: &staged_finish::StagedFinish,
        attempts: &[staged_attempt::AttemptEvidence<'_>],
        model: Option<String>,
        timestamp: String,
        policy: &InvestigationPolicy,
    ) -> Result<Result<InvestigationResult, staged_finish::FinishDecision>, InvestigationError>
    {
        let request = request.normalized_with_policy(policy)?;
        if disclosure.schema_version
            != match request.schema_version {
                7 => 6,
                6 => 5,
                _ => 4,
            }
        {
            return Err(InvestigationError::InvalidInput);
        }
        let request = &request;
        let decision = staged_finish::admit_finish_with_policy(
            request,
            task,
            step,
            self,
            disclosure,
            finish,
            attempts,
            model.clone(),
            timestamp.clone(),
            policy,
        )?;
        let staged_finish::FinishDecision::Accepted(candidate) = decision else {
            return Ok(Err(decision));
        };
        let completion = InvestigationCompletionResult {
            schema_version: 1,
            outcome: candidate.outcome,
            request_hash: request.intent_hash_with_policy(policy)?,
            obligations_hash: request.obligations.fingerprint_with_policy(policy)?,
            finishing_attempts_hash: completed_attempts_hash_with_policy(attempts, policy)?,
            assessment: candidate.assessment,
        };
        let result = self.admit_result_bound_with_policy(
            task,
            step,
            Some(disclosure),
            &finish.action,
            model,
            timestamp,
            Some(completion),
            policy,
        )?;
        result.validate_with_disclosure_and_policy(&self.ledger, step, disclosure, policy)?;
        Ok(Ok(result))
    }

    pub fn admit_result_for_step(
        &self,
        investigation_id: &str,
        step_id: &str,
        disclosure: Option<&InvestigationDisclosure>,
        action: &InvestigationAction,
        observed_response_model: Option<String>,
        created_at: String,
    ) -> Result<InvestigationResult, InvestigationError> {
        self.admit_result_bound(
            investigation_id,
            step_id,
            disclosure,
            action,
            observed_response_model,
            created_at,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_result_bound(
        &self,
        investigation_id: &str,
        step_id: &str,
        disclosure: Option<&InvestigationDisclosure>,
        action: &InvestigationAction,
        observed_response_model: Option<String>,
        created_at: String,
        completion: Option<InvestigationCompletionResult>,
    ) -> Result<InvestigationResult, InvestigationError> {
        self.admit_result_bound_with_policy(
            investigation_id,
            step_id,
            disclosure,
            action,
            observed_response_model,
            created_at,
            completion,
            &INVESTIGATION_V1_POLICY,
        )
    }

    /// As [`Self::admit_result_bound`]; every admission branch is bounded by
    /// the policy the host resolved from the exact saved task limits.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_result_bound_with_policy(
        &self,
        investigation_id: &str,
        step_id: &str,
        disclosure: Option<&InvestigationDisclosure>,
        action: &InvestigationAction,
        observed_response_model: Option<String>,
        created_at: String,
        completion: Option<InvestigationCompletionResult>,
        policy: &InvestigationPolicy,
    ) -> Result<InvestigationResult, InvestigationError> {
        if self.compact != disclosure.is_some() {
            return Err(InvestigationError::InvalidInput);
        }
        let InvestigationAction::Finish {
            findings,
            limitations,
            ..
        } = action
        else {
            return Err(InvestigationError::InvalidAction);
        };
        let prose = findings
            .iter()
            .flat_map(|f| {
                std::iter::once(f.title.as_str())
                    .chain(std::iter::once(f.statement.as_str()))
                    .chain(f.limitations.iter().map(String::as_str))
            })
            .chain(limitations.iter().map(String::as_str))
            .chain(observed_response_model.as_deref());
        super::privacy::reject_metadata_replay(
            prose,
            self.graph_strings.iter().map(String::as_str),
        )?;
        if let Some(completion) = completion {
            return InvestigationResult::admit_with_completion_and_policy(
                investigation_id,
                &self.ledger,
                step_id,
                disclosure.ok_or(InvestigationError::InvalidInput)?,
                self.inspection_requirements.as_deref(),
                completion,
                action,
                self.supplied_strings.iter().map(String::as_str),
                observed_response_model,
                created_at,
                policy,
            );
        }
        if let Some(disclosure) = disclosure {
            if let Some(targets) = &self.inspection_requirements {
                return InvestigationResult::admit_with_inspection_and_policy(
                    investigation_id,
                    &self.ledger,
                    step_id,
                    disclosure,
                    action,
                    self.supplied_strings.iter().map(String::as_str),
                    observed_response_model,
                    created_at,
                    targets,
                    policy,
                );
            }
            return InvestigationResult::admit_with_disclosure_and_policy(
                investigation_id,
                &self.ledger,
                step_id,
                disclosure,
                action,
                self.supplied_strings.iter().map(String::as_str),
                observed_response_model,
                created_at,
                policy,
            );
        }
        InvestigationResult::admit_with_policy(
            investigation_id,
            &self.ledger,
            action,
            self.supplied_strings.iter().map(String::as_str),
            observed_response_model,
            created_at,
            policy,
        )
    }
}

/// Host-built parent envelope; the parent result was already validated against
/// its own saved ledger by JobStore. This shape never confers graph authority.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PriorHistory {
    parent_investigation_id: String,
    original_graph_snapshot_id: Option<String>,
    original_scope: InvestigationScope,
    saved_execution_status: InvestigationStatus,
    cancellation_requested: bool,
    invocation_outcome_pending: bool,
    authority: String,
    saved_result: Option<InvestigationResult>,
}

/// Source-authored graph object keys are input too. Closed protocol field names
/// are keys of this wrapper rather than source text, so visit property keys only.
fn collect_strings(value: &Value, out: &mut Vec<String>) {
    fn visit(value: &Value, out: &mut Vec<String>, source_keys: bool) {
        match value {
            Value::String(value) => out.push(value.clone()),
            Value::Array(values) => values.iter().for_each(|v| visit(v, out, source_keys)),
            Value::Object(values) => values.iter().for_each(|(k, v)| {
                if source_keys {
                    out.push(k.clone());
                }
                visit(v, out, source_keys || k == "properties");
            }),
            _ => {}
        }
    }
    visit(value, out, false);
}

#[path = "input_versions.rs"]
mod versions;

#[path = "input_workspace.rs"]
mod workspace;

#[path = "input_coverage_details.rs"]
mod coverage_details;

#[path = "input_query_history.rs"]
mod query_history_input;

#[path = "input_wire.rs"]
mod wire;

#[path = "input_admission.rs"]
mod admission_input;
pub use admission_input::{
    AdmissionCapacityFeedback, AdmissionReserveMeasurements, CapacityRefusalReason,
};

#[path = "input_context_preview.rs"]
mod context_preview;
pub use context_preview::{ContextPreviewBound, ContextPreviewKind, ContextPreviewMeasurements};

#[path = "input_source_preview.rs"]
mod source_preview;
pub use source_preview::UnreadSourcePreviewBound;
