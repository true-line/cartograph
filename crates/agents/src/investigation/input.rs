//! Transient input ownership. Stored manifests contain only the fingerprints of
//! these exact query/source/history inputs, never their raw text.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::*;
#[path = "diagnostic_sidecar_input.rs"]
mod diagnostic_sidecar_input;
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
    // Frozen by the builder after matching the saved request identity.
    specialist: SpecialistId,
    ledger: InvestigationInputLedger,
    payload: CompletionPayload,
    supplied_strings: Vec<String>,
    graph_strings: Vec<String>,
    compact: bool,
    acquisition_choices: Option<std::sync::Arc<acquisition_choices::AcquisitionChoices>>,
    acquisition_mode: Option<staged_refusal::CompletionCallMode>,
    finish_patch: Option<finish_patch::PatchBasis>,
    action_feedback: Option<ActionRefusalFeedback>,
    citation_correction: Option<acquisition_choices::CitationCorrection>,
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
    /// Check withheld finish prose before considering recoverable citation feedback.
    /// This grants neither citation admission nor permission to continue: callers
    /// must separately authenticate the response, classify every invalid citation,
    /// validate narrative references, and enforce the durable correction allowance.
    pub fn validate_correction_prose(
        &self,
        finish: &staged_finish::StagedFinish,
        model: Option<&str>,
    ) -> Result<(), InvestigationError> {
        finish.fingerprint()?;
        self.validate_response_metadata(model)?;
        let InvestigationAction::Finish {
            findings,
            limitations,
            ..
        } = &finish.action
        else {
            return Err(InvestigationError::InvalidAction);
        };
        let prose = findings
            .iter()
            .flat_map(|finding| {
                std::iter::once(finding.title.as_str())
                    .chain(std::iter::once(finding.statement.as_str()))
                    .chain(finding.limitations.iter().map(String::as_str))
            })
            .chain(limitations.iter().map(String::as_str))
            .chain(model)
            .chain(finish.narrative.as_ref().map(|value| value.text.as_str()))
            .collect::<Vec<_>>();
        reject_prose_replay(
            prose.iter().copied(),
            self.supplied_strings.iter().map(String::as_str),
        )?;
        super::privacy::reject_metadata_replay(prose, self.graph_strings.iter().map(String::as_str))
    }

    /// @23 checks only admitted citation spans belonging to each finding.
    /// The returned diagnostic carries positions, never refused authored text.
    pub(super) fn claim_correction(
        &self,
        finish: &staged_finish::StagedFinish,
    ) -> Result<Option<staged_finish::ProseCorrection>, InvestigationError> {
        let InvestigationAction::Finish { findings, .. } = &finish.action else {
            return Err(InvestigationError::InvalidAction);
        };
        for (index, finding) in findings.iter().enumerate() {
            let spans = self
                .payload
                .spans
                .iter()
                .filter(|span| finding.citation_ids.contains(&span.id))
                .map(|span| (span.path.as_str(), span.text.as_str()))
                .collect::<Vec<_>>();
            if let Some((title, sentence, reason)) = if self.specialist.uses_finish_patch() {
                enumeration_claims::finding_with_grammar(
                    &finding.title,
                    &finding.statement,
                    &spans,
                    true,
                )
            } else {
                enumeration_claims::finding(&finding.title, &finding.statement, &spans)
            } {
                let finding =
                    u32::try_from(index).map_err(|_| InvestigationError::LimitExceeded)?;
                let position = ProsePosition {
                    field: if title {
                        ProseField::FindingTitle { finding }
                    } else {
                        ProseField::FindingStatement { finding }
                    },
                    sentence: u32::try_from(sentence)
                        .map_err(|_| InvestigationError::LimitExceeded)?,
                };
                let location = ReplayLocation {
                    start: position.clone(),
                    end: position,
                };
                location.validate()?;
                return Ok(Some(match reason {
                    enumeration_claims::Reason::Enumeration => {
                        staged_finish::ProseCorrection::UnverifiedEnumerationClaimAt(location)
                    }
                    enumeration_claims::Reason::Configuration => {
                        staged_finish::ProseCorrection::UnverifiedConfigurationClaimAt(location)
                    }
                }));
            }
        }
        Ok(None)
    }

    /// Visit every finding-local claim correction for combined feedback. The
    /// host must authenticate citations, charge workspace and bind the emitted
    /// diagnostics. Collection retains only locations, never refused prose.
    pub fn visit_claim_corrections(
        &self,
        finish: &staged_finish::StagedFinish,
        mut visit: impl FnMut(staged_finish::ProseCorrection),
    ) -> Result<(), InvestigationError> {
        let InvestigationAction::Finish { findings, .. } = &finish.action else {
            return Err(InvestigationError::InvalidAction);
        };
        // Validate the admitted shape before invoking callbacks so no invalid
        // finding or sentence index can escape through partial visitation.
        finish.fingerprint()?;
        for (index, finding) in findings.iter().enumerate() {
            let spans = self
                .payload
                .spans
                .iter()
                .filter(|span| finding.citation_ids.contains(&span.id))
                .map(|span| (span.path.as_str(), span.text.as_str()))
                .collect::<Vec<_>>();
            let finding_index =
                u32::try_from(index).map_err(|_| InvestigationError::LimitExceeded)?;
            let mut error = None;
            enumeration_claims::visit_finding_with_grammar(
                &finding.title,
                &finding.statement,
                &spans,
                self.specialist.uses_finish_patch(),
                |title, sentence, reason| {
                    let Ok(sentence) = u32::try_from(sentence) else {
                        error = Some(InvestigationError::LimitExceeded);
                        return;
                    };
                    let position = ProsePosition {
                        field: if title {
                            ProseField::FindingTitle {
                                finding: finding_index,
                            }
                        } else {
                            ProseField::FindingStatement {
                                finding: finding_index,
                            }
                        },
                        sentence,
                    };
                    let location = ReplayLocation {
                        start: position.clone(),
                        end: position,
                    };
                    if let Err(failure) = location.validate() {
                        error = Some(failure);
                        return;
                    }
                    visit(match reason {
                        enumeration_claims::Reason::Enumeration => {
                            staged_finish::ProseCorrection::UnverifiedEnumerationClaimAt(location)
                        }
                        enumeration_claims::Reason::Configuration => {
                            staged_finish::ProseCorrection::UnverifiedConfigurationClaimAt(location)
                        }
                    });
                },
            );
            if let Some(error) = error {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Text-free diagnostic for the exact refused finish. This does not admit
    /// output or authenticate feedback; the host must bind it to the finish hash.
    pub fn correction_replay_location(
        &self,
        finish: &staged_finish::StagedFinish,
        model: Option<&str>,
    ) -> Result<Option<ReplayLocation>, InvestigationError> {
        match self.validate_correction_prose(finish, model) {
            Ok(()) => return Ok(None),
            Err(InvestigationError::SourceReplay) => (),
            Err(error) => return Err(error),
        }
        let fields = Self::correction_prose_fields(finish, model)?;
        let sources = self
            .supplied_strings
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if let Some(location) = locate_prose_replay(&fields, &sources, false)? {
            return Ok(Some(location));
        }
        let metadata = self
            .graph_strings
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        match locate_prose_replay(&fields, &metadata, true)? {
            Some(location) => Ok(Some(location)),
            None => Err(InvestigationError::SourceReplay),
        }
    }

    /// Visit every replay location across source and graph metadata together.
    /// The host must charge complete replay workspace and authenticate the
    /// resulting envelope against the refused finish before dispatch.
    pub fn visit_correction_replay_locations(
        &self,
        finish: &staged_finish::StagedFinish,
        model: Option<&str>,
        visit: impl FnMut(ReplayLocation),
    ) -> Result<(), InvestigationError> {
        finish.fingerprint()?;
        let fields = Self::correction_prose_fields(finish, model)?;
        let sources = self
            .supplied_strings
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let metadata = self
            .graph_strings
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        visit_prose_replays_with_metadata(&fields, &sources, &metadata, visit)
    }

    fn correction_prose_fields<'a>(
        finish: &'a staged_finish::StagedFinish,
        model: Option<&'a str>,
    ) -> Result<Vec<(ProseField, &'a str)>, InvestigationError> {
        let InvestigationAction::Finish {
            findings,
            limitations,
            ..
        } = &finish.action
        else {
            return Err(InvestigationError::InvalidAction);
        };
        let ordinal =
            |index: usize| u32::try_from(index).map_err(|_| InvestigationError::LimitExceeded);
        let mut fields = Vec::new();
        for (index, finding) in findings.iter().enumerate() {
            let finding_index = ordinal(index)?;
            fields.push((
                ProseField::FindingTitle {
                    finding: finding_index,
                },
                finding.title.as_str(),
            ));
            fields.push((
                ProseField::FindingStatement {
                    finding: finding_index,
                },
                finding.statement.as_str(),
            ));
            for (index, limitation) in finding.limitations.iter().enumerate() {
                fields.push((
                    ProseField::FindingLimitation {
                        finding: finding_index,
                        limitation: ordinal(index)?,
                    },
                    limitation.as_str(),
                ));
            }
        }
        for (index, limitation) in limitations.iter().enumerate() {
            fields.push((
                ProseField::Limitation {
                    limitation: ordinal(index)?,
                },
                limitation.as_str(),
            ));
        }
        if let Some(model) = model {
            fields.push((ProseField::Model, model));
        }
        if let Some(narrative) = &finish.narrative {
            fields.push((ProseField::Narrative, narrative.text.as_str()));
        }
        Ok(fields)
    }

    /// Only the new analyst may turn known missing source citations into feedback.
    /// The caller has already authenticated the completion binding and attempts.
    fn citation_correction_targets(
        &self,
        disclosure: &InvestigationDisclosure,
        finish: &staged_finish::StagedFinish,
        model: Option<&str>,
        policy: &InvestigationPolicy,
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    ) -> Result<Option<Vec<String>>, InvestigationError> {
        if self.payload.system != SpecialistId::DomainAnalystV19.prompt()
            && !SpecialistId::is_typed_citation_prompt(&self.payload.system)
        {
            return Ok(None);
        }
        // Analyst finishes never carry a narrative. Do not extend this recovery
        // path to other roles without their separate narrative-reference checks.
        if finish.schema_version != 1 || finish.narrative.is_some() {
            return Err(InvestigationError::InvalidAction);
        }
        self.validate_correction_prose(finish, model)?;
        self.validate_acquisition_projection()?;
        // A finish-only offer cannot become permission to acquire missing source.
        // Choice already reserves the continuation and final finish under the
        // shared refusal allowance; no budget or admission mode is upgraded here.
        if self.acquisition_mode != Some(staged_refusal::CompletionCallMode::Choice) {
            return Ok(None);
        }
        let InvestigationAction::Finish { findings, .. } = &finish.action else {
            return Err(InvestigationError::InvalidAction);
        };
        let citations =
            disclosure.eligible_citations_consistent(&self.ledger, policy, consistency)?;
        let invalid = findings
            .iter()
            .flat_map(|finding| &finding.citation_ids)
            .filter(|id| !citations.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let contract = serde_json::from_value(prompt["completion_contract"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        let metadata = serde_json::from_value(prompt["completion_metadata"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        let choices = self
            .acquisition_choices
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        if SpecialistId::is_typed_citation_prompt(&self.payload.system) {
            choices.unacquired_citation_handles(
                &invalid,
                &contract,
                &metadata,
                &self.ledger,
                policy,
            )
        } else {
            choices.unacquired_citation_targets(
                &invalid,
                &contract,
                &metadata,
                &self.ledger,
                policy,
            )
        }
    }

    pub(super) fn eligible_citation_correction(
        &self,
        disclosure: &InvestigationDisclosure,
        finish: &staged_finish::StagedFinish,
        model: Option<&str>,
        policy: &InvestigationPolicy,
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    ) -> Result<bool, InvestigationError> {
        Ok(self
            .citation_correction_targets(disclosure, finish, model, policy, consistency)?
            .is_some())
    }

    pub(super) fn typed_citation_correction(
        &self,
        disclosure: &InvestigationDisclosure,
        finish: &staged_finish::StagedFinish,
        model: Option<&str>,
        policy: &InvestigationPolicy,
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    ) -> Result<Option<acquisition_choices::CitationCorrection>, InvestigationError> {
        if !SpecialistId::is_typed_citation_prompt(&self.payload.system) {
            return Ok(None);
        }
        let Some(ids) =
            self.citation_correction_targets(disclosure, finish, model, policy, consistency)?
        else {
            return Ok(None);
        };
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let contract = serde_json::from_value(prompt["completion_contract"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        let metadata = serde_json::from_value(prompt["completion_metadata"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        let correction = self
            .acquisition_choices
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?
            .citation_correction(&ids, &contract, &metadata, &self.ledger, policy)?
            .ok_or(InvestigationError::InvalidInput)?;
        consistency
            .ok_or(InvestigationError::InvalidInput)?
            .authenticate_citation_correction(&correction, &ids)?;
        Ok(Some(correction))
    }

    /// #2478 (@31 only): the exact, sorted, deduplicated citation ids of a
    /// finish that the frozen disclosure does not offer, for a drop-only
    /// correction in either acquisition mode. `None` keeps the terminal
    /// refusal: another role, no invalid id, or an id that is not a plain
    /// identifier (rejected authored text is never echoed).
    pub(super) fn citation_drop_ids(
        &self,
        disclosure: &InvestigationDisclosure,
        finish: &staged_finish::StagedFinish,
        model: Option<&str>,
        policy: &InvestigationPolicy,
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    ) -> Result<Option<Vec<String>>, InvestigationError> {
        if self.payload.system != SpecialistId::DomainAnalystV31.prompt() {
            return Ok(None);
        }
        if finish.schema_version != 1 || finish.narrative.is_some() {
            return Err(InvestigationError::InvalidAction);
        }
        self.validate_correction_prose(finish, model)?;
        self.validate_acquisition_projection()?;
        let InvestigationAction::Finish { findings, .. } = &finish.action else {
            return Err(InvestigationError::InvalidAction);
        };
        let citations =
            disclosure.eligible_citations_consistent(&self.ledger, policy, consistency)?;
        let invalid = findings
            .iter()
            .flat_map(|finding| &finding.citation_ids)
            .filter(|id| !citations.contains(*id))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if invalid.is_empty()
            || invalid.len() > staged_finish::MAX_CITATION_DROP_IDS
            || !invalid
                .iter()
                .all(|id| staged_finish::plain_citation_id(id))
        {
            return Ok(None);
        }
        Ok(Some(invalid.into_iter().collect()))
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
            &std::collections::BTreeSet::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_base_projected<P: AsRef<str> + serde::Serialize>(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[P],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        policy: &InvestigationPolicy,
        request_hash: Option<&str>,
        withheld: &std::collections::BTreeSet<String>,
    ) -> Result<Self, InvestigationError> {
        if specialist.uses_query_history_disclosure() != request_hash.is_some()
            || (!withheld.is_empty() && !specialist.uses_compact_disclosure())
        {
            return Err(InvestigationError::InvalidInput);
        }
        if specialist.uses_policy_selection() != (policy.profile() == "investigation-v2") {
            return Err(InvestigationError::InvalidInput);
        }
        ledger.validate_for_specialist(specialist, policy)?;
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
        // A firewall-refused read stays acquired in the ledger, but only the host
        // may withhold it: exactly one source citation with no supplied text.
        if source_citations.len() != evidence.len() + withheld.len()
            || withheld.iter().any(|id| {
                !source_citations.contains_key(id.as_str())
                    || evidence.iter().any(|source| &source.id == id)
            })
        {
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
            let history_bytes = bounded_bytes(&supplied_history, policy.supplied_history_bytes())?;
            if ledger.supplied_history_bytes != history_bytes.len()
                || ledger.supplied_history_hash.as_deref()
                    != Some(content_hash(&history_bytes).as_str())
            {
                return Err(InvestigationError::InvalidInput);
            }
            for raw in supplied_history {
                strict::preflight(raw, policy.supplied_history_bytes(), 32, 16384, 128)?;
                let history: PriorHistory = decode_record(raw)?;
                if history.withholding_reading.is_some()
                    && history.withholding_reading
                        != history
                            .saved_result
                            .as_ref()
                            .and_then(WithholdingReading::of)
                {
                    return Err(InvestigationError::InvalidInput);
                }
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
            let (withheld_sources, citations): (Vec<_>, Vec<_>) = ledger
                .citations
                .iter()
                .map(CitationPromptHandle::from)
                .partition(|handle| withheld.contains(handle.citation_id));
            let mut prompt = serde_json::json!({
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
            });
            // Closed code only: no withheld text, hash or firewall reason prose.
            // Absent when nothing is withheld, so legacy prompt identities hold.
            if !withheld_sources.is_empty() {
                prompt["withheld_sources"] = withheld_sources
                    .into_iter()
                    .map(|handle| {
                        serde_json::json!({"handle": handle, "outcome": "firewall_refused"})
                    })
                    .collect();
            }
            prompt
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
            response_schema: response_schema::select_completion(specialist),
            system: specialist.prompt(),
            prompt: String::new(),
            spans,
        };
        #[cfg(test)]
        assert!(
            context_hub::logical::count(&prompt_value).unwrap()
                <= workspace::base_prompt_wire_bytes().unwrap()
        );
        wire::replace_prompt(&mut payload, &prompt_value, MAX_INPUT_BYTES)?;
        Ok(Self {
            specialist,
            ledger,
            payload,
            supplied_strings,
            graph_strings,
            compact,
            acquisition_choices: None,
            acquisition_mode: None,
            finish_patch: None,
            action_feedback: None,
            citation_correction: None,
            inspection_requirements: None,
        })
    }

    pub(super) fn frozen_specialist(&self) -> Result<SpecialistId, InvestigationError> {
        if self.payload.system != self.specialist.prompt() {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(self.specialist)
    }

    pub fn ledger(&self) -> &InvestigationInputLedger {
        &self.ledger
    }

    /// Prepare the compact semantic review of a host-authenticated candidate.
    /// This does not reserve resources, dispatch, or grant result publication.
    pub fn semantic_audit_payload(
        &self,
        candidate: &InvestigationResult,
        policy: &InvestigationPolicy,
    ) -> Result<super::semantic_audit::PreparedSemanticAudit, InvestigationError> {
        let hash = self.ledger.fingerprint_with_policy(policy)?;
        if candidate.input_ledger_hash != hash {
            return Err(InvestigationError::InvalidInput);
        }
        // A firewall-refused read has no span either: the auditor sees exactly
        // the supplied sources (#679).
        let citations = self
            .citable_ledger()?
            .citations
            .iter()
            // Graph metadata citations have no acquired source span. Keep the
            // same source identity boundary as input construction; every actual
            // acquired source must still be present in the audit projection.
            .filter(|citation| {
                !matches!(citation.origin, InvestigationEvidenceOrigin::GraphMetadata)
            })
            .map(|citation| citation.citation_id.clone())
            .collect();
        super::semantic_audit::prepare_payload(&self.payload, candidate, &citations)
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
        self.validate_acquisition_projection()?;
        if !super::records::opaque_id(&action_id) {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(CompletionAction {
            action_id,
            tier: AnalysisTier::Agentic,
            payload: self.payload.clone(),
        })
    }

    /// Add fixed, non-echoing feedback for the single bounded ordinary
    /// correction after a source-replay refusal. This runs before firewall
    /// preparation and disclosure sealing, and uses the ordinary wire limit.
    pub fn with_source_replay_feedback(mut self) -> Result<Self, InvestigationError> {
        let mut prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let object = prompt
            .as_object_mut()
            .ok_or(InvestigationError::InvalidInput)?;
        if object.contains_key("finish_correction") {
            return Err(InvestigationError::InvalidInput);
        }
        object.insert("finish_correction".into(), serde_json::json!({
            "refusal": "source_replay",
            "instruction": "The prior finish was discarded. For this one correction, return a finish only. Give an original concise formulation and cite already-admitted evidence IDs. Do not quote or paraphrase source text or prior findings. No reads or tools are available."
        }));
        wire::replace_prompt(&mut self.payload, &prompt, MAX_INPUT_BYTES)?;
        Ok(self)
    }

    /// Attach fixed feedback from a host-validated durable refusal before the
    /// firewall and disclosure seal. This method does not attest persistence.
    pub fn with_query_refusal_feedback(
        mut self,
        record: &query_refusal::QueryRefusalRecord,
        specialist: SpecialistId,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        record.validate_binding(
            &record.task_id,
            &record.step_id,
            &record.action_hash,
            &self.ledger,
            specialist,
            policy,
        )?;
        let mut prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let object = prompt
            .as_object_mut()
            .ok_or(InvestigationError::InvalidInput)?;
        if object.contains_key("query_refusal") {
            return Err(InvestigationError::InvalidInput);
        }
        object.insert("query_refusal".into(), serde_json::json!({
            "record_id": record.record_id,
            "step_id": record.step_id,
            "reason": record.reason,
            "instruction": "The prior query was refused and acquired no evidence. Choose a valid query using known context identifiers or finish using already acquired evidence. Existing obligations and remaining budgets still apply."
        }));
        wire::replace_prompt(&mut self.payload, &prompt, MAX_INPUT_BYTES)?;
        Ok(self)
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
        self.validate_acquisition_projection()?;
        if !self.compact {
            return Ok(None);
        }
        let action = self.action(prepared.preview().egress.action_id.clone())?;
        if !prepared.matches_action(&action) {
            return Err(InvestigationError::InvalidInput);
        }
        drop(action);
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let admission = if prompt["schema_version"] == 9 {
            Some(self.disclosed_admission_with_policy(policy)?)
        } else {
            None
        };
        // The sealer parses both payloads; this schema-inspection tree is done.
        drop(prompt);
        match admission {
            Some(admission) if self.acquisition_choices.is_some() => {
                super::sealing::seal_admission_inner(
                    &self.ledger,
                    &self.payload,
                    prepared,
                    policy,
                    self.inspection_requirements.as_deref(),
                    Some(admission),
                    Some(self),
                )
            }
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
        Self::admit_completion_finish_with_policy_using_consistency(
            self, request, task, step, disclosure, finish, attempts, model, timestamp, policy, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_completion_finish_with_policy_using_consistency(
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
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
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
        let decision = staged_finish::admit_finish_with_policy_using_consistency(
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
            consistency,
        )?;
        let staged_finish::FinishDecision::Accepted(candidate) = decision else {
            return Ok(Err(decision));
        };
        let completion = InvestigationCompletionResult {
            withholding: None,
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
            consistency,
        )?;
        let result = self.admit_narrative(result, finish)?;
        result.validate_with_disclosure_and_policy_using_consistency(
            &self.ledger,
            step,
            disclosure,
            policy,
            consistency,
        )?;
        Ok(Ok(result))
    }

    pub(super) fn admit_narrative(
        &self,
        result: InvestigationResult,
        finish: &staged_finish::StagedFinish,
    ) -> Result<InvestigationResult, InvestigationError> {
        if finish.schema_version != 2 {
            return Ok(result);
        }
        let result = result.with_narrative(finish.narrative.clone())?;
        super::privacy::reject_prose_replay(
            result.authored_prose(),
            self.supplied_strings.iter().map(String::as_str),
        )?;
        super::privacy::reject_metadata_replay(
            result.authored_prose(),
            self.graph_strings.iter().map(String::as_str),
        )?;
        Ok(result)
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
            None,
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
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
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
                consistency,
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
    /// Present only for a withheld parent; equal to the saved result's
    /// reading (AC-1336). Histories saved before it omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    withholding_reading: Option<WithholdingReading>,
    saved_result: Option<InvestigationResult>,
}

/// Citation ids the host withheld after an explicit firewall refusal (#679).
/// Absent means none. Present must be a nonempty list of closed entries.
pub(super) fn withheld_source_ids(
    prompt: &Value,
) -> Result<std::collections::BTreeSet<&str>, InvestigationError> {
    let Some(entries) = prompt.get("withheld_sources") else {
        return Ok(Default::default());
    };
    let entries = entries
        .as_array()
        .filter(|entries| !entries.is_empty())
        .ok_or(InvestigationError::InvalidInput)?;
    let mut ids = std::collections::BTreeSet::new();
    for entry in entries {
        let id = entry
            .as_object()
            .filter(|object| object.len() == 2 && entry["outcome"] == "firewall_refused")
            .and_then(|_| entry["handle"]["citation_id"].as_str())
            .ok_or(InvestigationError::InvalidInput)?;
        if !ids.insert(id) {
            return Err(InvestigationError::InvalidInput);
        }
    }
    Ok(ids)
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

#[path = "sealing_workspace.rs"]
mod sealing_workspace;

#[path = "input_coverage_details.rs"]
mod coverage_details;

#[path = "input_query_history.rs"]
mod query_history_input;

#[path = "input_prompt_layout.rs"]
mod prompt_layout;
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

#[cfg(test)]
mod source_replay_feedback_tests {
    use super::*;

    fn prepare() -> InvestigationInput {
        let page = serde_json::json!({"facts":[{"properties":{"name":"fixture"}}]}).to_string();
        let mut basis = super::super::tests::ledger();
        basis.queries[0].response_hash = content_hash(page.as_bytes());
        basis.queries[0].response_bytes = page.len();
        InvestigationInput::prepare(
            "Explain the bounded evidence",
            SpecialistId::DomainAnalyst,
            basis,
            &[page],
            &[],
            &[],
            &InvestigationUsage::default(),
        )
        .unwrap()
    }

    #[test]
    fn combined_replay_input_visits_all_authored_fields_and_metadata() {
        let mut input = prepare();
        input.supplied_strings = vec!["ExactSymbol".into()];
        let metadata = "界".repeat(48);
        input.graph_strings = vec![metadata.clone()];
        let finish = staged_finish::StagedFinish {
            schema_version: 1,
            outcome: staged_finish::CompletionOutcome::Supported,
            narrative: None,
            action: InvestigationAction::Finish {
                findings: vec![ProposedInvestigationFinding {
                    claim_kind: InvestigationClaimKind::ImplementedBehavior,
                    title: "ExactSymbol".into(),
                    statement: "ExactSymbol is referenced.".into(),
                    citation_ids: vec!["fact-1".into()],
                    limitations: vec!["ExactSymbol was assessed.".into()],
                }],
                knowledge_completeness: KnowledgeCompleteness::Partial,
                limitations: vec!["ExactSymbol was assessed.".into()],
            },
        };
        let mut seen = Vec::new();
        input
            .visit_correction_replay_locations(&finish, Some(&metadata), |location| {
                seen.push(location)
            })
            .unwrap();
        assert_eq!(seen.len(), 5);
        assert_eq!(
            seen.iter()
                .map(|location| &location.start.field)
                .collect::<Vec<_>>(),
            vec![
                &ProseField::FindingTitle { finding: 0 },
                &ProseField::FindingStatement { finding: 0 },
                &ProseField::FindingLimitation {
                    finding: 0,
                    limitation: 0
                },
                &ProseField::Limitation { limitation: 0 },
                &ProseField::Model,
            ]
        );
        assert_eq!(
            input
                .correction_replay_location(&finish, Some(&metadata))
                .unwrap(),
            Some(seen[0].clone())
        );
        let mut message_finish = finish;
        message_finish.schema_version = 2;
        message_finish.narrative = Some(InvestigationNarrative {
            text: "ExactSymbol is referenced.".into(),
            finding_ids: vec!["finding-1".into()],
        });
        let mut with_narrative = Vec::new();
        input
            .visit_correction_replay_locations(&message_finish, Some(&metadata), |location| {
                with_narrative.push(location)
            })
            .unwrap();
        assert_eq!(with_narrative[..5], seen);
        assert_eq!(with_narrative.len(), 6);
        assert_eq!(with_narrative[5].start.field, ProseField::Narrative);
    }

    // AC-0287 / #1701: combined diagnostics remain finding-local and reject
    // invalid finish shapes before publishing even the first callback.
    #[test]
    fn combined_claim_corrections_isolate_citations_and_validate_before_visiting() {
        let mut input = prepare();
        let source = "['A', 'B']";
        input.payload.spans.push(PayloadSpan {
            id: "source-1".into(),
            repo: "fixture".into(),
            path: "values.ts".into(),
            byte_start: 0,
            byte_end: source.len() as u64,
            commit_sha: "a".repeat(40),
            text: source.into(),
        });
        let finding = |citation: &str| ProposedInvestigationFinding {
            claim_kind: InvestigationClaimKind::ImplementedBehavior,
            title: "Listed values".into(),
            statement: "Only A is listed.".into(),
            citation_ids: vec![citation.into()],
            limitations: vec!["Only selected evidence was assessed.".into()],
        };
        let mut finish = staged_finish::StagedFinish {
            schema_version: 1,
            outcome: staged_finish::CompletionOutcome::Supported,
            narrative: None,
            action: InvestigationAction::Finish {
                findings: vec![finding("fact-1"), finding("source-1")],
                knowledge_completeness: KnowledgeCompleteness::Partial,
                limitations: vec!["Only selected evidence was assessed.".into()],
            },
        };
        // A model cannot grant itself the host-only partial disposition.
        let mut forged = serde_json::to_value(&finish).unwrap();
        forged["outcome"] = serde_json::json!("findings_withheld_after_correction");
        assert!(staged_finish::StagedFinish::decode(&forged.to_string()).is_err());
        let mut seen = Vec::new();
        input
            .visit_claim_corrections(&finish, |correction| seen.push(correction))
            .unwrap();
        let position = ProsePosition {
            field: ProseField::FindingStatement { finding: 1 },
            sentence: 1,
        };
        assert_eq!(
            seen,
            vec![
                staged_finish::ProseCorrection::UnverifiedEnumerationClaimAt(ReplayLocation {
                    start: position.clone(),
                    end: position
                })
            ]
        );
        // AC-0287 / #1705: message-role narratives cannot enter projection,
        // even if they reference a survivor or repeat the excluded claim.
        for text in ["See the withheld finding.", "Only A is listed."] {
            let mut message = finish.clone();
            message.schema_version = 2;
            message.narrative = Some(InvestigationNarrative {
                text: text.into(),
                finding_ids: vec!["finding-2".into()],
            });
            assert!(
                input
                    .finding_withholding_plan_with_policy(&message, None, &INVESTIGATION_V1_POLICY)
                    .is_err()
            );
            message.schema_version = 1;
            assert!(
                input
                    .finding_withholding_plan_with_policy(&message, None, &INVESTIGATION_V1_POLICY)
                    .is_err()
            );
        }
        // AC-0287 / #1705: derive exclusion authority again after relocation;
        // previous feedback indexes are never an input to this API.
        let plan = input
            .finding_withholding_plan_with_policy(&finish, None, &INVESTIGATION_V1_POLICY)
            .unwrap()
            .unwrap();
        assert_eq!(plan.withheld_indexes(), &[1]);
        let original_hash = plan.finish_hash().to_owned();
        let mut relocated = finish.clone();
        let InvestigationAction::Finish { findings, .. } = &mut relocated.action else {
            panic!()
        };
        findings.swap(0, 1);
        let moved = input
            .finding_withholding_plan_with_policy(&relocated, None, &INVESTIGATION_V1_POLICY)
            .unwrap()
            .unwrap();
        assert_eq!(moved.withheld_indexes(), &[0]);
        assert_ne!(moved.finish_hash(), original_hash);
        assert!(
            input
                .project_withheld_finish_with_policy(plan, &relocated, &INVESTIGATION_V1_POLICY,)
                .is_err()
        );
        let projection = input
            .project_withheld_finish_with_policy(moved, &relocated, &INVESTIGATION_V1_POLICY)
            .unwrap();
        assert_eq!(
            projection.original_finish_hash(),
            relocated.fingerprint().unwrap()
        );
        assert_eq!(projection.survivor_indexes(), &[1]);
        assert!(projection.finish().narrative.is_none());
        let InvestigationAction::Finish { findings, .. } = &projection.finish().action else {
            panic!()
        };
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].citation_ids, vec!["fact-1"]);
        let repeated = input
            .finding_withholding_plan_with_policy(&relocated, None, &INVESTIGATION_V1_POLICY)
            .unwrap()
            .unwrap();
        let repeated = input
            .project_withheld_finish_with_policy(repeated, &relocated, &INVESTIGATION_V1_POLICY)
            .unwrap();
        assert_eq!(
            projection.finish().fingerprint().unwrap(),
            repeated.finish().fingerprint().unwrap()
        );

        let stale = input
            .finding_withholding_plan_with_policy(&relocated, None, &INVESTIGATION_V1_POLICY)
            .unwrap()
            .unwrap();
        input.ledger.revision += 1;
        assert!(
            input
                .project_withheld_finish_with_policy(stale, &relocated, &INVESTIGATION_V1_POLICY,)
                .is_err()
        );
        input.ledger.revision -= 1;
        input.supplied_strings = vec!["listed".into()];
        let combined = staged_finish::RefusalFeedback::collect_input_corrections_with_policy(
            &input,
            &finish,
            None,
            vec![],
            &INVESTIGATION_V1_POLICY,
        )
        .unwrap()
        .unwrap();
        let Some(staged_finish::ProseCorrection::Combined(corrections)) = combined.prose_correction
        else {
            panic!()
        };
        assert_eq!(corrections.entries.len(), 3);
        assert!(matches!(
            corrections.entries[0],
            staged_finish::CombinedCorrection::ReplayAt(_)
        ));
        assert!(matches!(
            corrections.entries[1],
            staged_finish::CombinedCorrection::ReplayAt(_)
        ));
        assert!(matches!(
            corrections.entries[2],
            staged_finish::CombinedCorrection::EnumerationAt(_)
        ));
        assert_eq!(corrections.omitted, 0);
        finish.schema_version = 99;
        let mut callbacks = 0;
        assert!(
            input
                .visit_claim_corrections(&finish, |_| callbacks += 1)
                .is_err()
        );
        assert_eq!(callbacks, 0);
    }

    #[test]
    fn investigation_source_replay_feedback_is_fixed_and_respects_existing_wire_limit() {
        // AC-0184: host-fixed feedback uses the same serialized payload ceiling.
        let corrected = prepare().with_source_replay_feedback().unwrap();
        let prompt: Value = serde_json::from_str(&corrected.payload.prompt).unwrap();
        assert_eq!(prompt["finish_correction"]["refusal"], "source_replay");
        assert!(
            prompt["finish_correction"]["instruction"]
                .as_str()
                .unwrap()
                .contains("No reads or tools are available")
        );
        assert!(
            serde_json::to_vec(&corrected.action("inv-one:step-2".into()).unwrap().payload)
                .unwrap()
                .len()
                <= MAX_INPUT_BYTES
        );

        let mut near_limit = prepare();
        let mut prompt: Value = serde_json::from_str(&near_limit.payload.prompt).unwrap();
        let mut low = 0_usize;
        let mut high = MAX_INPUT_BYTES;
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            prompt["bounded_test_padding"] = Value::String("x".repeat(middle));
            near_limit.payload.prompt = serde_json::to_string(&prompt).unwrap();
            let bytes =
                serde_json::to_vec(&near_limit.action("inv-one:step-2".into()).unwrap().payload)
                    .unwrap()
                    .len();
            if bytes <= MAX_INPUT_BYTES {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        prompt["bounded_test_padding"] = Value::String("x".repeat(low));
        near_limit.payload.prompt = serde_json::to_string(&prompt).unwrap();
        assert!(
            serde_json::to_vec(&near_limit.action("inv-one:step-2".into()).unwrap().payload)
                .unwrap()
                .len()
                <= MAX_INPUT_BYTES
        );
        let mut refusal = query_refusal::QueryRefusalRecord {
            budget: None,
            schema_version: 1,
            record_id: String::new(),
            task_id: "task-1".into(),
            step_id: "step-1".into(),
            action_hash: String::new(),
            input_ledger_hash: near_limit.ledger.fingerprint().unwrap(),
            input_revision: near_limit.ledger.revision,
            context_id: None,
            query: InvestigationQuery {
                scope: InvestigationScope::Neighborhood {
                    anchor: "absent".into(),
                    hops: 1,
                },
                kind: None,
                labels: vec![],
                max_facts: 1,
                max_bytes: 16384,
                cursor: None,
            },
            reason: query_refusal::QueryRejection::UnknownAnchor,
        };
        refusal.action_hash = refusal.query_action_hash().unwrap();
        refusal.record_id = refusal.content_id().unwrap();
        assert!(matches!(
            near_limit.clone().with_query_refusal_feedback(
                &refusal,
                SpecialistId::DomainAnalyst,
                &INVESTIGATION_V1_POLICY
            ),
            Err(InvestigationError::LimitExceeded)
        ));
        assert!(matches!(
            near_limit.with_source_replay_feedback(),
            Err(InvestigationError::LimitExceeded)
        ));
    }
}
