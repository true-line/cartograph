//! Metadata for what one invocation actually received. Acquisition stays in the
//! unchanged ledger; these hashes cannot reconstruct source or grant authority.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::*;
use crate::TaskFactKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const COMPACT_DISCLOSURE_POLICY: &str = "deduplicated-recovered-context@1";
pub const QUERY_HISTORY_DISCLOSURE_POLICY: &str = "investigation-progressive-query-history@1";
pub const MAX_DISCLOSURE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureOmissionReason {
    /// Reserved; compact projections currently share supplied representations.
    DuplicateRepresentation,
    NotSelected,
    /// Disclosure5 prior query identity is supplied, while its full view is omitted.
    /// Never supplies evidence or makes a citation eligible.
    ReferenceOnly,
    WorkingSetBudget,
}

/// A supplied hash identifies the actual redacted representation, not merely its
/// acquired origin. Omission is explicit and never makes a citation eligible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisclosureRepresentation {
    Supplied {
        representation_hash: String,
    },
    Omitted {
        reason: DisclosureOmissionReason,
    },
    /// The host firewall explicitly refused this acquired source representation
    /// (#679). Distinct from omission and redaction; never makes a citation
    /// eligible and carries no withheld text or hash.
    FirewallRefused,
}

impl DisclosureRepresentation {
    pub fn is_supplied(&self) -> bool {
        matches!(self, Self::Supplied { .. })
    }

    fn validate(&self) -> Result<(), InvestigationError> {
        if matches!(
            self,
            Self::Omitted {
                reason: DisclosureOmissionReason::DuplicateRepresentation
                    | DisclosureOmissionReason::ReferenceOnly
            }
        ) {
            return Err(InvestigationError::InvalidInput);
        }
        if let Self::Supplied {
            representation_hash,
        } = self
            && !crate::source_basis::hash(representation_hash)
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedInvestigationFact {
    pub fact: TaskFactKey,
    pub representation: DisclosureRepresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedInvestigationQuery {
    pub query_index: usize,
    /// Hash of the supplied compact selection/count/continuation summary.
    /// A summary alone never supplies a graph fact's properties.
    pub representation: DisclosureRepresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedInvestigationSource {
    pub citation_id: String,
    /// Hash of the complete supplied span representation, including metadata.
    pub representation: DisclosureRepresentation,
    /// Actual supplied text hash, distinct from the original ledger text hash.
    pub disclosed_text_hash: Option<String>,
    /// True when supplied text differs from, or cannot be compared with, the
    /// ledger text hash (including a missing legacy hash); not proof of a rewrite.
    pub redacted: bool,
}

/// One actual supplied inventory detail; never a graph/source citation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedInvestigationCoverageDetail {
    pub read_index: usize,
    pub representation_hash: String,
    pub disclosed_body_hash: String,
    pub redacted: bool,
}

/// One supplied compiled-artifact page (ADR-0089); never a citation.
/// `disclosed_body_hash` hashes the canonical page as supplied, so an
/// unredacted page equals the ledger's `response_hash`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedInvestigationArtifactPage {
    pub read_index: usize,
    pub representation_hash: String,
    pub disclosed_body_hash: String,
    pub redacted: bool,
}

/// Exact mandatory public qualification supplied in a qualified invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedInvestigationCoverage {
    pub context_id: String,
    pub coverage_id: String,
    pub representation_hash: String,
}

impl DisclosedInvestigationCoverage {
    pub(super) fn from_proof(
        proof: &context_hub::coverage::CoverageProof,
    ) -> Result<Self, InvestigationError> {
        proof
            .validate()
            .map_err(|_| InvestigationError::InvalidInput)?;
        let public = serde_json::to_value(&proof.qualification)
            .map_err(|_| InvestigationError::InvalidInput)?;
        Ok(Self {
            context_id: proof.context_id.clone(),
            coverage_id: proof.qualification.coverage_id.clone(),
            representation_hash: core_prov::content_hash(&bounded_bytes(&public, MAX_INPUT_BYTES)?),
        })
    }
}

/// Supplied references are separate from supplied selector/result views.
/// The host must also bind request_hash to the actual saved task intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedQueryHistory {
    pub request_hash: String,
    pub registry_hash: String,
    pub query_ids: Vec<String>,
}
impl DisclosedQueryHistory {
    pub(super) fn from_ledger(
        request_hash: &str,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        let references = super::query_history::disclosure_references(request_hash, ledger, policy)?;
        if !crate::source_basis::hash(request_hash)
            || policy.profile() != INVESTIGATION_V2_POLICY.profile()
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(Self {
            request_hash: request_hash.into(),
            registry_hash: super::query_history::registry_hash(
                request_hash,
                ledger,
                policy,
                &references,
            )?,
            query_ids: references
                .into_iter()
                .map(|reference| reference.query_id)
                .collect(),
        })
    }
}

/// A complete accounting in acquisition order. Each acquired fact, query and
/// source citation has exactly one supplied/omitted entry, including uncited
/// evidence. Original source bindings are resolved through input_ledger_hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationDisclosure {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<DisclosedAdmissionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_history: Option<DisclosedQueryHistory>,
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<DisclosedInvestigationCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage_details: Vec<DisclosedInvestigationCoverageDetail>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_pages: Vec<DisclosedInvestigationArtifactPage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_contract_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_metadata_hash: Option<String>,
    pub policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_requirements_hash: Option<String>,
    pub input_ledger_hash: String,
    pub graph_snapshot_id: String,
    pub scope_snapshot_id: String,
    /// The firewall's exact redacted payload identity, also bound by consent.
    pub payload_hash: String,
    pub facts: Vec<DisclosedInvestigationFact>,
    pub queries: Vec<DisclosedInvestigationQuery>,
    pub sources: Vec<DisclosedInvestigationSource>,
    pub history: Option<DisclosureRepresentation>,
}

impl InvestigationDisclosure {
    /// Validates durable metadata against its saved acquisition basis. The live
    /// preparation path must separately check actual representation bytes before
    /// sealing this record; metadata does not attest unavailable old source.
    pub fn validate(&self, ledger: &InvestigationInputLedger) -> Result<(), InvestigationError> {
        self.validate_with_policy(ledger, &INVESTIGATION_V1_POLICY)
    }

    pub fn validate_with_policy(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        self.validate_admission_basis(ledger, policy, None)
    }

    /// Role/shape consistency for a saved-call core, not stamp authentication.
    pub fn validate_saved_role_shape(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
        role: SpecialistId,
    ) -> Result<(), InvestigationError> {
        self.validate_admission_basis(ledger, policy, Some(role))
    }

    fn validate_admission_basis(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
        saved_role: Option<SpecialistId>,
    ) -> Result<(), InvestigationError> {
        ledger.validate_with_policy(policy)?;
        let expected_coverage = ledger
            .coverage
            .as_ref()
            .map(DisclosedInvestigationCoverage::from_proof)
            .transpose()?;
        if self.coverage != expected_coverage {
            return Err(InvestigationError::InvalidInput);
        }
        let completion =
            self.completion_contract_hash.is_some() || self.completion_metadata_hash.is_some();
        if completion
            && (self.coverage.is_none()
                || self
                    .completion_contract_hash
                    .as_deref()
                    .is_none_or(|hash| !crate::source_basis::hash(hash))
                || self
                    .completion_metadata_hash
                    .as_deref()
                    .is_none_or(|hash| !crate::source_basis::hash(hash)))
        {
            return Err(InvestigationError::InvalidInput);
        }
        let admission = self.admission.is_some();
        if let Some(state) = &self.admission {
            if let Some(role) = saved_role {
                state.validate_saved_role_shape(role, policy)?;
            } else {
                state.validate_with_policy(policy)?;
            }
            if !completion || self.query_history.is_none() {
                return Err(InvestigationError::InvalidInput);
            }
        }
        let progressive = self.query_history.is_some();
        if let Some(history) = &self.query_history
            && (!completion
                || history
                    != &DisclosedQueryHistory::from_ledger(&history.request_hash, ledger, policy)?)
        {
            return Err(InvestigationError::InvalidInput);
        }
        if self.schema_version
            != if admission {
                6
            } else if progressive {
                5
            } else if completion {
                4
            } else if self.coverage.is_some() {
                3
            } else if self.inspection_requirements_hash.is_some() {
                2
            } else {
                1
            }
            || self
                .inspection_requirements_hash
                .as_deref()
                .is_some_and(|hash| !crate::source_basis::hash(hash))
            || self.policy
                != if progressive {
                    QUERY_HISTORY_DISCLOSURE_POLICY
                } else {
                    COMPACT_DISCLOSURE_POLICY
                }
            || self.input_ledger_hash != ledger.fingerprint_with_policy(policy)?
            || self.graph_snapshot_id != ledger.graph_snapshot_id
            || self.scope_snapshot_id != ledger.scope_snapshot_id
            || !crate::source_basis::hash(&self.payload_hash)
            || self.facts.len() != ledger.selected_facts.len()
            || self.queries.len() != ledger.queries.len()
            || self.history.is_some() != ledger.supplied_history_hash.is_some()
        {
            return Err(InvestigationError::InvalidInput);
        }
        if self.coverage_details.len() != ledger.coverage_reads.len()
            || self.artifact_pages.len() != ledger.artifact_reads.len()
        {
            return Err(InvestigationError::InvalidInput);
        }
        for (index, (entry, read)) in self
            .artifact_pages
            .iter()
            .zip(&ledger.artifact_reads)
            .enumerate()
        {
            if entry.read_index != index
                || !crate::source_basis::hash(&entry.representation_hash)
                || !crate::source_basis::hash(&entry.disclosed_body_hash)
                || entry.redacted != (entry.disclosed_body_hash != read.response_hash)
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        for (index, (entry, read)) in self
            .coverage_details
            .iter()
            .zip(&ledger.coverage_reads)
            .enumerate()
        {
            if entry.read_index != index
                || !crate::source_basis::hash(&entry.representation_hash)
                || !crate::source_basis::hash(&entry.disclosed_body_hash)
                || entry.redacted != (entry.disclosed_body_hash != read.request.inventory_hash)
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        for (entry, acquired) in self.facts.iter().zip(&ledger.selected_facts) {
            if entry.fact != acquired.fact
                || entry.representation == DisclosureRepresentation::FirewallRefused
            {
                return Err(InvestigationError::InvalidInput);
            }
            entry.representation.validate()?;
        }
        for (index, entry) in self.queries.iter().enumerate() {
            if entry.query_index != index
                || entry.representation == DisclosureRepresentation::FirewallRefused
            {
                return Err(InvestigationError::InvalidInput);
            }
            let prior_reference = progressive && index + 1 < self.queries.len();
            if !prior_reference {
                entry.representation.validate()?;
            }
            if progressive {
                let latest = index + 1 == self.queries.len();
                if (latest && !entry.representation.is_supplied())
                    || (!latest
                        && entry.representation
                            != DisclosureRepresentation::Omitted {
                                reason: DisclosureOmissionReason::ReferenceOnly,
                            })
                {
                    return Err(InvestigationError::InvalidInput);
                }
            }
        }
        let acquired_sources = ledger
            .citations
            .iter()
            .filter(|citation| {
                !matches!(citation.origin, InvestigationEvidenceOrigin::GraphMetadata)
            })
            .collect::<Vec<_>>();
        if self.sources.len() != acquired_sources.len() {
            return Err(InvestigationError::InvalidInput);
        }
        for (entry, acquired) in self.sources.iter().zip(acquired_sources) {
            entry.representation.validate()?;
            if entry.citation_id != acquired.citation_id {
                return Err(InvestigationError::InvalidInput);
            }
            match &entry.representation {
                DisclosureRepresentation::Supplied { .. } => {
                    let disclosed_hash = entry
                        .disclosed_text_hash
                        .as_deref()
                        .ok_or(InvestigationError::InvalidInput)?;
                    if !crate::source_basis::hash(disclosed_hash)
                        || entry.redacted != (Some(disclosed_hash) != acquired.text_hash.as_deref())
                        || !self.fact_supplied(&acquired.fact)
                    {
                        return Err(InvestigationError::InvalidInput);
                    }
                }
                DisclosureRepresentation::Omitted { .. }
                | DisclosureRepresentation::FirewallRefused => {
                    if entry.disclosed_text_hash.is_some() || entry.redacted {
                        return Err(InvestigationError::InvalidInput);
                    }
                }
            }
        }
        if let Some(history) = &self.history {
            if *history == DisclosureRepresentation::FirewallRefused {
                return Err(InvestigationError::InvalidInput);
            }
            history.validate()?;
        }
        bounded_bytes(self, MAX_DISCLOSURE_BYTES)?;
        Ok(())
    }

    fn fact_supplied(&self, fact: &TaskFactKey) -> bool {
        self.facts
            .iter()
            .any(|entry| &entry.fact == fact && entry.representation.is_supplied())
    }

    pub fn fingerprint(
        &self,
        ledger: &InvestigationInputLedger,
    ) -> Result<String, InvestigationError> {
        self.fingerprint_with_policy(ledger, &INVESTIGATION_V1_POLICY)
    }

    pub fn fingerprint_with_policy(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<String, InvestigationError> {
        self.validate_with_policy(ledger, policy)?;
        self.fingerprint_bytes()
    }
    pub(super) fn fingerprint_saved_role_shape(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
        role: SpecialistId,
    ) -> Result<String, InvestigationError> {
        self.validate_saved_role_shape(ledger, policy, role)?;
        self.fingerprint_bytes()
    }
    fn fingerprint_bytes(&self) -> Result<String, InvestigationError> {
        let (prefix, domain): (&str, &[u8]) = if self.schema_version == 6 {
            (
                "investigation-disclosure-v6",
                b"cartograph:investigation-disclosure:v6\0",
            )
        } else if self.schema_version == 5 {
            (
                "investigation-disclosure-v5",
                b"cartograph:investigation-disclosure:v5\0",
            )
        } else if self.schema_version == 4 {
            (
                "investigation-disclosure-v4",
                b"cartograph:investigation-disclosure:v4\0",
            )
        } else if self.schema_version == 3 {
            (
                "investigation-disclosure-v3",
                b"cartograph:investigation-disclosure:v3\0",
            )
        } else {
            (
                "investigation-disclosure-v1",
                b"cartograph:investigation-disclosure:v1\0",
            )
        };
        Ok(format!(
            "{prefix}:{}",
            crate::source_basis::domain_hash(domain, &bounded_bytes(self, MAX_DISCLOSURE_BYTES)?,)
        ))
    }

    pub fn eligible_citations(
        &self,
        ledger: &InvestigationInputLedger,
    ) -> Result<BTreeSet<String>, InvestigationError> {
        self.eligible_citations_with_policy(ledger, &INVESTIGATION_V1_POLICY)
    }

    pub fn eligible_citations_with_policy(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<BTreeSet<String>, InvestigationError> {
        self.validate_with_policy(ledger, policy)?;
        Ok(self.eligible_citations_unchecked(ledger))
    }
    fn eligible_citations_unchecked(&self, ledger: &InvestigationInputLedger) -> BTreeSet<String> {
        ledger
            .citations
            .iter()
            .filter(|citation| match citation.origin {
                InvestigationEvidenceOrigin::GraphMetadata => self.fact_supplied(&citation.fact),
                _ => self.sources.iter().any(|entry| {
                    entry.citation_id == citation.citation_id && entry.representation.is_supplied()
                }),
            })
            .map(|citation| citation.citation_id.clone())
            .collect()
    }
}

#[path = "disclosure_admission.rs"]
mod admission;
pub use admission::{
    AcquisitionAdmission, ActionRefusalFeedback, ActionRefusalReason, DisclosedAdmissionState,
    MAX_INVALID_ACTION_REFUSALS,
};

/// Pure consistency witness over typed derivation inputs, NOT authentication of
/// a saved call or SQLite. The host must authenticate all witnesses at the exact
/// saved cutoff before using this explicit path; generic validators stay strict.
/// Fields are private and borrowed so a witness cannot be retargeted to a call.
pub struct AcquisitionDisclosureConsistency<'a> {
    pub(super) task: &'a str,
    policy: &'a InvestigationPolicy,
    pub(super) step: &'a str,
    pub(super) ledger: &'a InvestigationInputLedger,
    pub(super) disclosure: &'a InvestigationDisclosure,
    choices: &'a acquisition_choices::AcquisitionChoices,
    contract: &'a InvestigationCompletionContract,
    metadata: &'a staged_refusal::ProtectedCompletionMetadata,
}
impl acquisition_choices::AcquisitionChoices {
    #[allow(clippy::too_many_arguments)]
    pub fn bind_completion_disclosure<'a>(
        &'a self,
        task: &'a str,
        step: &'a str,
        contract: &'a InvestigationCompletionContract,
        metadata: &'a staged_refusal::ProtectedCompletionMetadata,
        ledger: &'a InvestigationInputLedger,
        disclosure: &'a InvestigationDisclosure,
        feedback: Option<&ActionRefusalFeedback>,
        policy: &'a InvestigationPolicy,
    ) -> Result<AcquisitionDisclosureConsistency<'a>, InvestigationError> {
        self.bind_completion_disclosure_with_patch(
            task, step, contract, metadata, ledger, disclosure, feedback, policy, None,
        )
    }
    /// Shape consistency only. A patch basis additionally needs saved-refusal
    /// authentication and an exact live-holder comparison in the host.
    #[allow(clippy::too_many_arguments)]
    pub fn bind_completion_disclosure_with_patch<'a>(
        &'a self,
        task: &'a str,
        step: &'a str,
        contract: &'a InvestigationCompletionContract,
        metadata: &'a staged_refusal::ProtectedCompletionMetadata,
        ledger: &'a InvestigationInputLedger,
        disclosure: &'a InvestigationDisclosure,
        feedback: Option<&ActionRefusalFeedback>,
        policy: &'a InvestigationPolicy,
        patch: Option<&'a finish_patch::PatchBasis>,
    ) -> Result<AcquisitionDisclosureConsistency<'a>, InvestigationError> {
        self.validate_basis(contract, metadata, ledger, policy)?;
        if let Some(patch) = patch {
            patch
                .validate()
                .map_err(|_| InvestigationError::InvalidInput)?;
            if metadata.refusal_count != 1
                || metadata.last_refusal_id.as_ref() != Some(&patch.refusal_hash)
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        disclosure.validate_saved_role_shape(
            ledger,
            policy,
            if patch.is_some() {
                SpecialistId::DomainAnalystV27
            } else if self.capacity_exhausted() {
                SpecialistId::DomainAnalystV21
            } else {
                SpecialistId::DomainAnalystV18
            },
        )?;
        let expected = AcquisitionAdmission {
            finish_patch: patch.cloned(),
            finish_policy_complete: policy.local_finish_generation()
                && (self.attempts_complete() || patch.is_some()),
            capacity_exhausted: self.capacity_exhausted(),
            finish_reason: feedback
                .filter(|value| value.reason == ActionRefusalReason::RepeatedRead)
                .map(|value| value.reason),
            choices_hash: self.stamp_hash()?,
            action_feedback_hash: feedback
                .map(ActionRefusalFeedback::stamp_hash)
                .transpose()?,
        };
        if !super::records::opaque_id(task)
            || !super::records::opaque_id(step)
            || disclosure
                .admission
                .as_ref()
                .and_then(|state| state.acquisition.as_ref())
                != Some(&expected)
            || disclosure.completion_contract_hash.as_deref()
                != Some(contract.fingerprint_with_policy(policy)?.as_str())
            || disclosure.completion_metadata_hash.as_deref()
                != Some(metadata.fingerprint_with_policy(policy)?.as_str())
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(AcquisitionDisclosureConsistency {
            task,
            step,
            ledger,
            disclosure,
            policy,
            choices: self,
            contract,
            metadata,
        })
    }
}
impl AcquisitionDisclosureConsistency<'_> {
    /// The host must already authenticate the refusal journal and response
    /// binding. This reconstructs owned guidance from its original cutoff.
    pub fn authenticate_stored_citation_correction(
        &self,
        correction: &acquisition_choices::CitationCorrection,
    ) -> Result<acquisition_choices::AuthenticatedCitationCorrection, InvestigationError> {
        correction.authenticate_stored(
            self.choices,
            self.contract,
            self.metadata,
            self.ledger,
            self.policy,
        )
    }

    pub(super) fn validate_stored_citation_correction(
        &self,
        correction: &acquisition_choices::CitationCorrection,
    ) -> Result<(), InvestigationError> {
        correction.validate_stored_mapping(
            self.choices,
            self.contract,
            self.metadata,
            self.ledger,
            self.policy,
        )
    }

    /// Authenticate correction bytes against this exact invocation's sealed
    /// choices and metadata. The caller supplies the complete invalid-ID set
    /// derived from the refused response; this witness supplies no response.
    pub fn authenticate_citation_correction(
        &self,
        correction: &acquisition_choices::CitationCorrection,
        invalid_ids: &[String],
    ) -> Result<acquisition_choices::AuthenticatedCitationCorrection, InvestigationError> {
        self.check(self.task, self.step, self.ledger, self.disclosure)?;
        correction.authenticate(
            invalid_ids,
            self.choices,
            self.contract,
            self.metadata,
            self.ledger,
            self.policy,
        )
    }

    pub(super) fn check(
        &self,
        task: &str,
        step: &str,
        ledger: &InvestigationInputLedger,
        disclosure: &InvestigationDisclosure,
    ) -> Result<(), InvestigationError> {
        if self.task != task
            || self.step != step
            || self.ledger != ledger
            || self.disclosure != disclosure
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
    /// Canonical fingerprint for the later authenticated host finish/reopen path.
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.disclosure.fingerprint_bytes()
    }
}
impl InvestigationDisclosure {
    // Construction ran full disclosure validation under this policy. The bound
    // path may reuse it only for a value-equal ledger/disclosure and the same
    // policy below; loosening construction invalidates this shortcut.
    pub(super) fn validate_consistent(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    ) -> Result<(), InvestigationError> {
        if let Some(bound) = consistency {
            if bound.policy.profile() != policy.profile()
                || bound.policy.limits() != policy.limits()
            {
                return Err(InvestigationError::InvalidInput);
            }
            bound.check(bound.task, bound.step, ledger, self)
        } else {
            self.validate_with_policy(ledger, policy)
        }
    }
    pub(super) fn fingerprint_consistent(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    ) -> Result<String, InvestigationError> {
        self.validate_consistent(ledger, policy, consistency)?;
        self.fingerprint_bytes()
    }
    pub(super) fn eligible_citations_consistent(
        &self,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
        consistency: Option<&AcquisitionDisclosureConsistency<'_>>,
    ) -> Result<BTreeSet<String>, InvestigationError> {
        self.validate_consistent(ledger, policy, consistency)?;
        Ok(self.eligible_citations_unchecked(ledger))
    }
}

impl AcquisitionDisclosureConsistency<'_> {
    #[allow(clippy::too_many_arguments)]
    pub fn admit_finish(
        &self,
        input: &InvestigationInput,
        request: &obligations::StagedObligationRequest,
        finish: &staged_finish::StagedFinish,
        attempts: &[staged_attempt::AttemptEvidence<'_>],
        model: Option<String>,
        timestamp: String,
    ) -> Result<Result<InvestigationResult, staged_finish::FinishDecision>, InvestigationError>
    {
        self.check(self.task, self.step, input.ledger(), self.disclosure)?;
        input.admit_completion_finish_with_policy_using_consistency(
            request,
            self.task,
            self.step,
            self.disclosure,
            finish,
            attempts,
            model,
            timestamp,
            self.policy,
            Some(self),
        )
    }
    pub fn validate_result(&self, result: &InvestigationResult) -> Result<(), InvestigationError> {
        self.check(
            &result.investigation_id,
            result
                .invocation_step_id
                .as_deref()
                .ok_or(InvestigationError::InvalidInput)?,
            self.ledger,
            self.disclosure,
        )?;
        result.validate_with_disclosure_and_policy_using_consistency(
            self.ledger,
            self.step,
            self.disclosure,
            self.policy,
            Some(self),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn validate_completion(
        &self,
        result: &InvestigationResult,
        request_hash: &str,
        envelope: &obligations::ObligationEnvelope,
        context: &str,
        graph: Option<&str>,
        attempts: &[staged_attempt::AttemptEvidence<'_>],
    ) -> Result<(), InvestigationError> {
        self.validate_result(result)?;
        result.validate_completion_for_binding_with_policy_using_consistency(
            request_hash,
            envelope,
            context,
            graph,
            self.task,
            self.step,
            self.ledger,
            self.disclosure,
            attempts,
            self.policy,
            Some(self),
        )
    }
}

impl AcquisitionDisclosureConsistency<'_> {
    /// Pure refusal consistency; saved-row provenance remains the host's job.
    pub fn validate_refusal(
        &self,
        record: &staged_refusal::RefusalRecord,
        request_hash: &str,
        envelope: &obligations::ObligationEnvelope,
        attempts: &[staged_attempt::AttemptEvidence<'_>],
        previous: Option<&staged_refusal::RefusalRecord>,
    ) -> Result<staged_refusal::RefusalDisposition, InvestigationError> {
        self.check(
            &record.task_id,
            &record.step_id,
            self.ledger,
            self.disclosure,
        )?;
        let assessment =
            staged_finish::derive_assessment_for_binding_with_policy_using_consistency(
                request_hash,
                envelope,
                self.task,
                self.step,
                self.ledger,
                self.disclosure,
                record.outcome,
                attempts,
                self.policy,
                Some(self),
            )?;
        if record.assessment != assessment {
            return Err(InvestigationError::InvalidInput);
        }
        record.validate_stored_basis_using_consistency(
            request_hash,
            envelope,
            self.ledger,
            self.disclosure,
            previous,
            self.policy,
            Some(self),
        )
    }
}

impl AcquisitionDisclosureConsistency<'_> {
    /// Host-authenticated exact-cutoff projection; generic blocked IDs are excluded.
    pub(super) fn capacity_excluded_obligations(&self) -> &[String] {
        self.choices.capacity_excluded_obligations()
    }
}
