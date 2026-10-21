//! Validate complete immutable metadata and result identities independently of
//! the current graph. Raw source remains necessary only for first admission.
use super::*;
use crate::{TaskFactKey, TaskRangeRole};
use core_prov::{ConfidenceTier, Tier};
use std::collections::{BTreeMap, BTreeSet};

fn hash(value: &str) -> bool {
    crate::source_basis::hash(value)
}
fn digest(fact: &TaskFactKey, value: &str) -> bool {
    value
        .strip_prefix(match fact {
            TaskFactKey::Node { .. } => "node-v1:",
            TaskFactKey::Edge { .. } => "edge-v1:",
        })
        .is_some_and(hash)
}
fn path(value: &str) -> bool {
    text(value, 1024)
        && !value.starts_with('/')
        && !value.contains(['\\', ':'])
        && value
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

impl InvestigationInputLedger {
    /// Enforce bounded, complete metadata before publication or use as a saved
    /// historical basis. This does not certify source bytes or current freshness.
    pub fn validate(&self) -> Result<(), InvestigationError> {
        self.validate_with_policy(&INVESTIGATION_V1_POLICY)
    }

    /// The host resolves this closed policy from exact saved task limits. The
    /// ledger wire does not carry caller-selected validation allowances.
    pub fn validate_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        let limits = policy.limits();
        let max_citations = limits
            .selected_facts
            .checked_add(limits.evidence_items)
            .ok_or(InvestigationError::LimitExceeded)?;
        match (self.schema_version, &self.coverage) {
            (1, None) => {}
            (2, Some(proof)) if proof.snapshot_id == self.scope_snapshot_id => {
                proof
                    .validate()
                    .map_err(|_| InvestigationError::InvalidInput)?;
            }
            _ => return Err(InvestigationError::InvalidInput),
        }
        if !super::records::context_hash(&self.graph_snapshot_id)
            || !super::records::context_hash(&self.scope_snapshot_id)
            || self.selected_facts.len() > limits.selected_facts
            || self.receipt_references.len() > limits.selected_facts
            || self.queries.len() > limits.tool_actions as usize
            || self.citations.len() > max_citations
            || self.supplied_history_bytes > 16 * 1024
            || (self.supplied_history_bytes == 0) != self.supplied_history_hash.is_none()
            || self
                .supplied_history_hash
                .as_deref()
                .is_some_and(|v| !hash(v))
        {
            return Err(InvestigationError::InvalidInput);
        }
        if self
            .queries
            .len()
            .checked_add(self.coverage_reads.len())
            .ok_or(InvestigationError::LimitExceeded)?
            > limits.tool_actions as usize
        {
            return Err(InvestigationError::LimitExceeded);
        }
        if let Some(proof) = &self.coverage {
            for read in &self.coverage_reads {
                read.validate(proof)?;
            }
        } else if !self.coverage_reads.is_empty() {
            return Err(InvestigationError::InvalidInput);
        }
        InvestigationQueryBudget::from_ledger(self)?;
        let mut selections = BTreeMap::new();
        let mut previous = None;
        for fact in &self.selected_facts {
            if !super::protocol::valid_fact(&fact.fact)
                || !digest(&fact.fact, &fact.fact_digest)
                || previous.is_some_and(|p| p >= &fact.fact)
            {
                return Err(InvestigationError::InvalidInput);
            }
            if let Some(binding) = &fact.binding
                && (!text(&binding.repo_key, 256)
                    || !receipt_id(&binding.receipt_id)
                    || binding.emitted_fact_digest != fact.fact_digest)
            {
                return Err(InvestigationError::InvalidInput);
            }
            previous = Some(&fact.fact);
            selections.insert(&fact.fact, fact);
        }
        let mut citation_ids = BTreeSet::new();
        let expected_receipts = self
            .selected_facts
            .iter()
            .filter_map(|f| f.binding.as_ref())
            .map(|b| (&b.repo_key, &b.receipt_id))
            .collect::<BTreeSet<_>>();
        let mut referenced = BTreeSet::new();
        let mut previous_receipt = None;
        for receipt in &self.receipt_references {
            if !text(&receipt.source_id, 256)
                || !text(&receipt.repo_key, 256)
                || !receipt_id(&receipt.receipt_id)
                || previous_receipt.is_some_and(|previous| previous >= receipt)
                || !referenced.insert((&receipt.repo_key, &receipt.receipt_id))
            {
                return Err(InvestigationError::InvalidInput);
            }
            previous_receipt = Some(receipt);
        }
        if referenced != expected_receipts {
            return Err(InvestigationError::InvalidInput);
        }
        let mut graph_citations = BTreeSet::new();
        let mut evidence_items = 0usize;
        let mut source_bytes = 0u64;
        let mut validation_bytes = 0u64;
        for citation in &self.citations {
            let selected = selections
                .get(&citation.fact)
                .ok_or(InvestigationError::InvalidInput)?;
            if !super::records::opaque_id(&citation.citation_id)
                || !citation_ids.insert(&citation.citation_id)
                || selected.fact_digest != citation.fact_digest
            {
                return Err(InvestigationError::InvalidInput);
            }
            if matches!(citation.origin, InvestigationEvidenceOrigin::GraphMetadata) {
                if citation.source.is_some()
                    || citation.role.is_some()
                    || citation.index.is_some()
                    || citation.text_hash.is_some()
                    || !graph_citations.insert(&citation.fact)
                {
                    return Err(InvestigationError::InvalidInput);
                }
                continue;
            }
            evidence_items += 1;
            let source = citation
                .source
                .as_ref()
                .ok_or(InvestigationError::InvalidInput)?;
            if !text(&source.repo, 256)
                || !path(&source.path)
                || !text(&source.commit_sha, 256)
                || source.byte_start >= source.byte_end
                || source.byte_end - source.byte_start > 8192
                || citation.role.is_none()
                || citation.index.is_none_or(|i| i >= 1024)
                || !citation.text_hash.as_deref().is_some_and(hash)
            {
                return Err(InvestigationError::InvalidInput);
            }
            source_bytes = source_bytes
                .checked_add(source.byte_end - source.byte_start)
                .ok_or(InvestigationError::LimitExceeded)?;
            match &citation.origin {
                InvestigationEvidenceOrigin::WorkingTreeUnverified => {
                    if selected.binding.is_some() {
                        return Err(InvestigationError::InvalidInput);
                    }
                }
                InvestigationEvidenceOrigin::CapturedPrimarySource {
                    registered_source_id,
                    receipt_id,
                    receipt_inventory_index,
                    captured,
                    ..
                } => {
                    let binding = selected
                        .binding
                        .as_ref()
                        .ok_or(InvestigationError::InvalidInput)?;
                    if binding.repo_key != source.repo
                        || &binding.receipt_id != receipt_id
                        || !self.receipt_references.iter().any(|r| {
                            r.source_id == *registered_source_id
                                && r.repo_key == binding.repo_key
                                && r.receipt_id == *receipt_id
                        })
                        || !text(registered_source_id, 256)
                        || *receipt_inventory_index >= 1024
                        || &captured.file.source_id != registered_source_id
                        || !captured
                            .file
                            .capture_id
                            .strip_prefix("capture-v1:")
                            .is_some_and(hash)
                        || !hash(&captured.file.digest)
                        || captured.file.path != source.path
                        || captured.file.byte_len > 16 * 1024 * 1024
                        || captured.byte_start != source.byte_start
                        || captured.byte_end != source.byte_end
                        || captured.byte_end > captured.file.byte_len
                        || (receipt_id.starts_with("ts-primary-v1:")
                            && matches!(
                                citation.role,
                                Some(
                                    TaskRangeRole::DefinitionDeclaration
                                        | TaskRangeRole::DefinitionUse
                                        | TaskRangeRole::DefinitionInitializer
                                        | TaskRangeRole::DefinitionExpression
                                        | TaskRangeRole::DefinitionDependency
                                        | TaskRangeRole::DefinitionDependencyDeclaration
                                )
                            ))
                    {
                        return Err(InvestigationError::InvalidInput);
                    }
                    validation_bytes = validation_bytes
                        .checked_add(captured.file.byte_len)
                        .ok_or(InvestigationError::LimitExceeded)?;
                }
                InvestigationEvidenceOrigin::GraphMetadata => unreachable!(),
            }
        }
        if graph_citations.len() != selections.len()
            || evidence_items > limits.evidence_items
            || source_bytes > limits.evidence_bytes as u64
            || validation_bytes > limits.captured_validation_bytes
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut query_bytes = 0usize;
        let mut returned_facts = 0usize;
        for query in &self.queries {
            query.query.validate()?;
            if !hash(&query.response_hash)
                || query.response_bytes > query.query.max_bytes
                || query.response_bytes == 0
                || query.returned_facts > query.query.max_facts
                || query.total_selected > 50_000
                || query.returned_facts > query.total_selected
                || query
                    .query
                    .cursor
                    .as_ref()
                    .is_some_and(|c| c.snapshot_id != self.scope_snapshot_id)
            {
                return Err(InvestigationError::InvalidInput);
            }
            query_bytes = query_bytes
                .checked_add(query.response_bytes)
                .ok_or(InvestigationError::LimitExceeded)?;
            returned_facts += query.returned_facts;
        }
        if query_bytes > MAX_TOTAL_QUERY_BYTES || returned_facts < selections.len() {
            return Err(InvestigationError::InvalidInput);
        }
        bounded_bytes(self, MAX_MANIFEST_BYTES)?;
        Ok(())
    }
}

fn receipt_id(value: &str) -> bool {
    value
        .strip_prefix("ts-primary-v1:")
        .or_else(|| value.strip_prefix("ts-primary-v2:"))
        .is_some_and(hash)
}

impl InvestigationResult {
    /// Only this initial admission checks raw input replay; subsequent historical
    /// reads check immutable identity and provenance against the saved ledger.
    pub fn admit<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        observed_response_model: Option<String>,
        created_at: String,
    ) -> Result<Self, InvestigationError> {
        Self::admit_with_policy(
            investigation_id,
            ledger,
            action,
            supplied_strings,
            observed_response_model,
            created_at,
            &INVESTIGATION_V1_POLICY,
        )
    }

    pub fn admit_with_policy<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        observed_response_model: Option<String>,
        created_at: String,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::admit_bound(
            investigation_id,
            ledger,
            None,
            None,
            None,
            action,
            supplied_strings,
            observed_response_model,
            created_at,
            policy,
        )
    }

    /// Admit a finish only against representations disclosed to this invocation.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_with_disclosure<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        step_id: &str,
        disclosure: &InvestigationDisclosure,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        observed_response_model: Option<String>,
        created_at: String,
    ) -> Result<Self, InvestigationError> {
        Self::admit_with_disclosure_and_policy(
            investigation_id,
            ledger,
            step_id,
            disclosure,
            action,
            supplied_strings,
            observed_response_model,
            created_at,
            &INVESTIGATION_V1_POLICY,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admit_with_disclosure_and_policy<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        step_id: &str,
        disclosure: &InvestigationDisclosure,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        observed_response_model: Option<String>,
        created_at: String,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::admit_bound(
            investigation_id,
            ledger,
            Some((step_id, disclosure)),
            None,
            None,
            action,
            supplied_strings,
            observed_response_model,
            created_at,
            policy,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admit_with_inspection<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        step_id: &str,
        disclosure: &InvestigationDisclosure,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        observed_response_model: Option<String>,
        created_at: String,
        targets: &[InvestigationInspectionTarget],
    ) -> Result<Self, InvestigationError> {
        Self::admit_with_inspection_and_policy(
            investigation_id,
            ledger,
            step_id,
            disclosure,
            action,
            supplied_strings,
            observed_response_model,
            created_at,
            targets,
            &INVESTIGATION_V1_POLICY,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn admit_with_inspection_and_policy<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        step_id: &str,
        disclosure: &InvestigationDisclosure,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        observed_response_model: Option<String>,
        created_at: String,
        targets: &[InvestigationInspectionTarget],
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::admit_bound(
            investigation_id,
            ledger,
            Some((step_id, disclosure)),
            Some(targets),
            None,
            action,
            supplied_strings,
            observed_response_model,
            created_at,
            policy,
        )
    }

    // Retain the legacy v1 bridge while production callers carry a policy.
    #[allow(dead_code, clippy::too_many_arguments)]
    pub(super) fn admit_with_completion<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        step_id: &str,
        disclosure: &InvestigationDisclosure,
        targets: Option<&[InvestigationInspectionTarget]>,
        completion: InvestigationCompletionResult,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        model: Option<String>,
        created_at: String,
    ) -> Result<Self, InvestigationError> {
        Self::admit_with_completion_and_policy(
            investigation_id,
            ledger,
            step_id,
            disclosure,
            targets,
            completion,
            action,
            supplied_strings,
            model,
            created_at,
            &INVESTIGATION_V1_POLICY,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_with_completion_and_policy<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        step_id: &str,
        disclosure: &InvestigationDisclosure,
        targets: Option<&[InvestigationInspectionTarget]>,
        completion: InvestigationCompletionResult,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        model: Option<String>,
        created_at: String,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        Self::admit_bound(
            investigation_id,
            ledger,
            Some((step_id, disclosure)),
            targets,
            Some(completion),
            action,
            supplied_strings,
            model,
            created_at,
            policy,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn admit_bound<'a>(
        investigation_id: &str,
        ledger: &InvestigationInputLedger,
        binding: Option<(&str, &InvestigationDisclosure)>,
        inspection_targets: Option<&[InvestigationInspectionTarget]>,
        completion: Option<InvestigationCompletionResult>,
        action: &InvestigationAction,
        supplied_strings: impl IntoIterator<Item = &'a str>,
        observed_response_model: Option<String>,
        created_at: String,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        ledger.validate_with_policy(policy)?;
        if ledger.schema_version == 2 && binding.is_none() {
            return Err(InvestigationError::InvalidInput);
        }
        if binding.is_some_and(|(_, d)| d.inspection_requirements_hash.is_some())
            != inspection_targets.is_some()
        {
            return Err(InvestigationError::InvalidInput);
        }
        let citations = match binding {
            Some((step, disclosure)) => {
                if !super::records::opaque_id(step) {
                    return Err(InvestigationError::InvalidInput);
                }
                disclosure.eligible_citations_with_policy(ledger, policy)?
            }
            None => ledger
                .citations
                .iter()
                .map(|c| c.citation_id.clone())
                .collect(),
        };
        let supplied = supplied_strings.into_iter().collect::<Vec<_>>();
        let findings = action.admit_finish(&citations, supplied.iter().copied())?;
        let InvestigationAction::Finish {
            findings: proposed,
            limitations: overall,
            ..
        } = action
        else {
            return Err(InvestigationError::InvalidAction);
        };
        let all_prose = proposed
            .iter()
            .flat_map(|f| {
                std::iter::once(f.title.as_str())
                    .chain(std::iter::once(f.statement.as_str()))
                    .chain(f.limitations.iter().map(String::as_str))
            })
            .chain(overall.iter().map(String::as_str))
            .chain(observed_response_model.as_deref());
        reject_prose_replay(all_prose, supplied.iter().copied())?;
        let InvestigationAction::Finish {
            knowledge_completeness,
            limitations,
            ..
        } = action
        else {
            return Err(InvestigationError::InvalidAction);
        };
        if binding.is_some_and(|(_, d)| matches!(d.schema_version, 4..=6)) != completion.is_some() {
            return Err(InvestigationError::InvalidInput);
        }
        if binding.is_some_and(|(_, disclosure)| {
            disclosure.query_history.as_ref().is_some_and(|history| {
                completion
                    .as_ref()
                    .is_none_or(|value| value.request_hash != history.request_hash)
            })
        }) {
            return Err(InvestigationError::InvalidInput);
        }
        let mut result = Self {
            schema_version: if binding.is_some_and(|(_, d)| d.schema_version == 6) {
                7
            } else if binding.is_some_and(|(_, d)| d.schema_version == 5) {
                6
            } else if completion.is_some() {
                5
            } else if ledger.schema_version == 2 {
                4
            } else if inspection_targets.is_some() {
                3
            } else if binding.is_some() {
                2
            } else {
                1
            },
            coverage: binding.and_then(|(_, disclosure)| disclosure.coverage.clone()),
            inspection_assessment: None,
            completion,
            investigation_id: investigation_id.into(),
            result_id: String::new(),
            input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
            graph_snapshot_id: ledger.graph_snapshot_id.clone(),
            invocation_step_id: binding.map(|(step, _)| step.to_owned()),
            disclosure_hash: binding
                .map(|(_, disclosure)| disclosure.fingerprint_with_policy(ledger, policy))
                .transpose()?,
            findings,
            knowledge_completeness: *knowledge_completeness,
            limitations: limitations.clone(),
            observed_response_model,
            created_at,
        };
        if let Some(targets) = inspection_targets {
            let (step, disclosure) = binding.ok_or(InvestigationError::InvalidInput)?;
            result.inspection_assessment =
                Some(InvestigationInspectionAssessment::derive_with_policy(
                    targets,
                    ledger,
                    step,
                    disclosure,
                    &result.findings,
                    policy,
                )?);
        }
        result.result_id = result.content_id()?;
        match binding {
            Some((_, disclosure)) if matches!(result.schema_version, 5..=7) => {
                result.validate_bound(
                    ledger,
                    &disclosure.eligible_citations_with_policy(ledger, policy)?,
                    policy,
                )?;
            }
            Some((step, disclosure)) => {
                result.validate_with_disclosure_and_policy(ledger, step, disclosure, policy)?
            }
            None => result.validate_with_policy(ledger, policy)?,
        }
        Ok(result)
    }

    fn content_id(&self) -> Result<String, InvestigationError> {
        let mut content = self.clone();
        content.result_id.clear();
        let (prefix, domain): (&str, &[u8]) = match self.schema_version {
            1 => (
                "investigation-result-v1",
                b"cartograph:investigation-result:v1\0",
            ),
            2 => (
                "investigation-result-v2",
                b"cartograph:investigation-result:v2\0",
            ),
            3 => (
                "investigation-result-v3",
                b"cartograph:investigation-result:v3\0",
            ),
            4 => (
                "investigation-result-v4",
                b"cartograph:investigation-result:v4\0",
            ),
            5 => (
                "investigation-result-v5",
                b"cartograph:investigation-result:v5\0",
            ),
            6 => (
                "investigation-result-v6",
                b"cartograph:investigation-result:v6\0",
            ),
            7 => (
                "investigation-result-v7",
                b"cartograph:investigation-result:v7\0",
            ),
            _ => return Err(InvestigationError::InvalidInput),
        };
        Ok(format!(
            "{prefix}:{}",
            crate::source_basis::domain_hash(domain, &bounded_bytes(&content, MAX_RESULT_BYTES)?,)
        ))
    }

    pub fn validate(&self, ledger: &InvestigationInputLedger) -> Result<(), InvestigationError> {
        self.validate_with_policy(ledger, &INVESTIGATION_V1_POLICY)
    }

    pub fn validate_with_policy(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if self.schema_version != 1
            || self.completion.is_some()
            || ledger.schema_version != 1
            || self.coverage.is_some()
            || self.inspection_assessment.is_some()
            || self.invocation_step_id.is_some()
            || self.disclosure_hash.is_some()
        {
            return Err(InvestigationError::InvalidInput);
        }
        ledger.validate_with_policy(policy)?;
        let citations = ledger
            .citations
            .iter()
            .map(|c| c.citation_id.clone())
            .collect();
        self.validate_bound(ledger, &citations, policy)
    }

    /// Validate saved disclosed results against their exact finishing invocation.
    pub fn validate_with_disclosure(
        &self,
        ledger: &InvestigationInputLedger,
        step_id: &str,
        disclosure: &InvestigationDisclosure,
    ) -> Result<(), InvestigationError> {
        self.validate_with_disclosure_and_policy(
            ledger,
            step_id,
            disclosure,
            &INVESTIGATION_V1_POLICY,
        )
    }

    pub fn validate_with_disclosure_and_policy(
        &self,
        ledger: &InvestigationInputLedger,
        step_id: &str,
        disclosure: &InvestigationDisclosure,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if self.schema_version
            != if disclosure.schema_version == 6 {
                7
            } else if disclosure.schema_version == 5 {
                6
            } else if disclosure.schema_version == 4 {
                5
            } else if ledger.schema_version == 2 {
                4
            } else if self.inspection_assessment.is_some() {
                3
            } else {
                2
            }
            || matches!(self.schema_version, 5..=7) != self.completion.is_some()
            || self.coverage != disclosure.coverage
            || self.inspection_assessment.is_some()
                != disclosure.inspection_requirements_hash.is_some()
            || !super::records::opaque_id(step_id)
            || self.invocation_step_id.as_deref() != Some(step_id)
            || self.disclosure_hash.as_deref()
                != Some(disclosure.fingerprint_with_policy(ledger, policy)?.as_str())
        {
            return Err(InvestigationError::InvalidInput);
        }
        if disclosure.query_history.as_ref().is_some_and(|history| {
            self.completion
                .as_ref()
                .is_none_or(|value| value.request_hash != history.request_hash)
        }) {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(completion) = &self.completion {
            completion.validate_with_policy(
                &self.findings,
                self.knowledge_completeness,
                &self.limitations,
                policy,
            )?;
        }
        if let Some(assessment) = &self.inspection_assessment {
            assessment.validate_with_policy(ledger, step_id, disclosure, &self.findings, policy)?;
        }
        self.validate_bound(
            ledger,
            &disclosure.eligible_citations_with_policy(ledger, policy)?,
            policy,
        )
    }

    fn validate_bound(
        &self,
        ledger: &InvestigationInputLedger,
        citations: &BTreeSet<String>,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        ledger.validate_with_policy(policy)?;
        if !super::records::opaque_id(&self.investigation_id)
            || !text(&self.created_at, 64)
            || self.graph_snapshot_id != ledger.graph_snapshot_id
            || self.input_ledger_hash != ledger.fingerprint_with_policy(policy)?
            || self.result_id != self.content_id()?
            || self
                .observed_response_model
                .as_deref()
                .is_some_and(|v| !text(v, 256) || core_redact::redact_text(v).1 != 0)
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut ids = BTreeSet::new();
        for (index, finding) in self.findings.iter().enumerate() {
            if finding.finding_id != format!("finding-{}", index + 1)
                || !ids.insert(&finding.finding_id)
                || finding.tier != Tier::Agentic
                || finding.confidence_tier != ConfidenceTier::InferredWeak
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        let action = InvestigationAction::Finish {
            findings: self
                .findings
                .iter()
                .map(|f| ProposedInvestigationFinding {
                    claim_kind: f.claim_kind,
                    title: f.title.clone(),
                    statement: f.statement.clone(),
                    citation_ids: f.citation_ids.clone(),
                    limitations: f.limitations.clone(),
                })
                .collect(),
            knowledge_completeness: self.knowledge_completeness,
            limitations: self.limitations.clone(),
        };
        action.admit_finish(citations, std::iter::empty())?;
        bounded_bytes(self, MAX_RESULT_BYTES)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "query_history_result_tests.rs"]
mod query_history_result_tests;
