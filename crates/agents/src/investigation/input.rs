//! Transient input ownership. Stored manifests contain only the fingerprints of
//! these exact query/source/history inputs, never their raw text.
use super::*;
use crate::AgentEvidence;
use core_prov::content_hash;
use llm::{AnalysisTier, CompletionAction, CompletionPayload, PayloadSpan};
use serde_json::Value;
use std::collections::BTreeMap;

/// Prepared once per model step after coherent host acquisition. All fields are
/// private so replacing query/source bytes requires revalidation and a new hash.
pub struct InvestigationInput {
    ledger: InvestigationInputLedger,
    payload: CompletionPayload,
    supplied_strings: Vec<String>,
    graph_strings: Vec<String>,
    compact: bool,
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

    /// V4 requires a coherently read durable turn basis; legacy inputs never
    /// acquire progress fields or change their serialized representation.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v4(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
    ) -> Result<Self, InvestigationError> {
        if !specialist.uses_turn_progress() {
            return Err(InvestigationError::InvalidInput);
        }
        let mut input = Self::prepare_base(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
        )?;
        let mut prompt: Value = serde_json::from_str(&input.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let progress = basis.progress(&input.ledger, &prompt["compact_context"])?;
        prompt["schema_version"] = 3.into();
        prompt["turn_progress"] =
            serde_json::to_value(progress).map_err(|_| InvestigationError::InvalidInput)?;
        input.payload.prompt = String::from_utf8(bounded_bytes(&prompt, MAX_INPUT_BYTES)?)
            .map_err(|_| InvestigationError::InvalidInput)?;
        bounded_bytes(&input.payload, MAX_INPUT_BYTES)?;
        Ok(input)
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
        ledger.validate()?;
        if !text(question, MAX_QUESTION_BYTES)
            || query_pages.len() != ledger.queries.len()
            || evidence.len() > 12
            || supplied_history.len() > 12
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut pages = Vec::new();
        let mut supplied_strings = Vec::new();
        let mut graph_strings = Vec::new();
        let mut total_query_bytes = 0usize;
        for (raw, manifest) in query_pages.iter().zip(&ledger.queries) {
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
            if source_bytes > 48 * 1024 {
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
        if supplied_strings.len() + graph_strings.len() > 16384
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
            let context = CompactInvestigationContext::prepare(&ledger, query_pages)?;
            let citations: Vec<_> = ledger
                .citations
                .iter()
                .map(|citation| {
                    serde_json::json!({
                        "citation_id": citation.citation_id,
                        "fact": citation.fact,
                        "origin": citation.origin,
                        "role": citation.role,
                        "index": citation.index,
                    })
                })
                .collect();
            serde_json::json!({
                "schema_version": 2,
                "policy": COMPACT_DISCLOSURE_POLICY,
                "question": question,
                "input_ledger_hash": ledger.fingerprint()?,
                "graph_snapshot_id": ledger.graph_snapshot_id,
                "scope_snapshot_id": ledger.scope_snapshot_id,
                "compact_context": context,
                "citations": citations,
                "prior_findings_as_t3_history": supplied_history,
                "consumed_budgets": usage,
                "limits": InvestigationLimits::default(),
                "coverage": "Bounded recovered context only. Captured source attests its primary source, not complete producer input closure. Unknown evidence is not absence. Findings remain T3/InferredWeak."
            })
        } else {
            serde_json::json!({
                "schema_version": 1,
                "question": question,
                "input_ledger": ledger,
                "input_ledger_hash": ledger.fingerprint()?,
                "context_pages": pages,
                "prior_findings_as_t3_history": supplied_history,
                "consumed_budgets": usage,
                "limits": InvestigationLimits::default(),
                "coverage": "This is a bounded recovered-context investigation. Captured source attests only its primary source; full producer input closure is not established. Unknown evidence is not proof of absence. All generated findings remain T3/InferredWeak."
            })
        };
        let prompt = String::from_utf8(bounded_bytes(&prompt_value, MAX_INPUT_BYTES)?)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let payload = CompletionPayload {
            system: specialist.prompt(),
            prompt,
            spans,
        };
        bounded_bytes(&payload, MAX_INPUT_BYTES)?;
        Ok(Self {
            ledger,
            payload,
            supplied_strings,
            graph_strings,
            compact,
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
        if !self.compact || reason == DisclosureOmissionReason::DuplicateRepresentation {
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
        self.payload.prompt = String::from_utf8(bounded_bytes(&prompt, MAX_INPUT_BYTES)?)
            .map_err(|_| InvestigationError::InvalidInput)?;
        bounded_bytes(&self.payload, MAX_INPUT_BYTES)?;
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
        if !self.compact {
            return Ok(None);
        }
        let action = self.action(prepared.preview().egress.action_id.clone())?;
        if !prepared.matches_action(&action) {
            return Err(InvestigationError::InvalidInput);
        }
        super::sealing::seal(&self.ledger, &self.payload, prepared).map(Some)
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

    #[allow(clippy::too_many_arguments)]
    pub fn admit_result_for_step(
        &self,
        investigation_id: &str,
        step_id: &str,
        disclosure: Option<&InvestigationDisclosure>,
        action: &InvestigationAction,
        observed_response_model: Option<String>,
        created_at: String,
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
        if let Some(disclosure) = disclosure {
            return InvestigationResult::admit_with_disclosure(
                investigation_id,
                &self.ledger,
                step_id,
                disclosure,
                action,
                self.supplied_strings.iter().map(String::as_str),
                observed_response_model,
                created_at,
            );
        }
        InvestigationResult::admit(
            investigation_id,
            &self.ledger,
            action,
            self.supplied_strings.iter().map(String::as_str),
            observed_response_model,
            created_at,
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
