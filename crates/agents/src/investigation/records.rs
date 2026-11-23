//! Versioned transport and durable metadata. None of these DTOs grants access to
//! a filesystem path, execution lease, current graph or provider.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::*;
use crate::{
    InputClosureStatus, PrimarySourceScope, TaskCaptureSpanRef, TaskFactKey, TaskFactSelection,
    TaskRangeRole,
};
use core_prov::EvidenceRef;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationProviderMode {
    Local,
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationProvider {
    pub mode: InvestigationProviderMode,
    pub provider_id: String,
    pub model: String,
    pub endpoint: String,
    pub deployment: Option<String>,
    /// Fixed bounded wire behavior used by this task. Older history has no
    /// recorded value; do not reinterpret it as the current protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

/// Closed opt-in generation policy; absence preserves historical wire bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenerationPolicy {
    #[serde(rename = "local-finish-4096-v1")]
    LocalFinish4096V1,
}

/// Host-selected history allowance; absent on all historical records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryPolicy {
    #[serde(rename = "parent-history-32768-v1")]
    ParentHistory32768V1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationLimits {
    pub profile: String,
    pub model_invocations: u32,
    pub tool_actions: u32,
    pub selected_facts: usize,
    pub evidence_requests: u32,
    pub evidence_items: usize,
    pub evidence_bytes: usize,
    pub captured_validation_bytes: u64,
    pub generated_tokens_per_invocation: u32,
    pub generated_token_reservations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_policy: Option<GenerationPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_policy: Option<HistoryPolicy>,
    pub active_seconds: u64,
    pub consent_wait_seconds: u64,
    pub wall_seconds: u64,
}

impl Default for InvestigationLimits {
    fn default() -> Self {
        INVESTIGATION_V1_POLICY.limits()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCatalog {
    pub schema_version: u32,
    pub specialists: Vec<InvestigationSpecialist>,
    pub providers: Vec<InvestigationProvider>,
    pub limits: InvestigationLimits,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<InvestigationPolicyCatalogEntry>,
    /// The offered specialists whose desktop runs use the saved run limits
    /// (#1757). The host decides this; a client shows it and keeps no list
    /// of its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configured_limit_specialists: Vec<SpecialistId>,
}

/// Host-owned closed choices; numeric fields describe the immutable policy,
/// never caller-provided overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationPolicyCatalogEntry {
    pub limits: InvestigationLimits,
    pub request_schema_version: u32,
    pub envelope_schema_version: u32,
    pub envelope_contract: String,
    pub max_completion_obligations: usize,
    pub request_bytes: usize,
    pub envelope_bytes: usize,
    pub specialist_ids: Vec<SpecialistId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartInvestigationRequest {
    pub schema_version: u32,
    /// Caller-supplied idempotency key, not a cryptographic nonce or execution authority.
    pub request_nonce: String,
    pub specialist_id: SpecialistId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_inspection_requirements"
    )]
    pub inspection_requirements: Option<Vec<InvestigationInspectionTarget>>,
    pub question: String,
    pub scope: InvestigationScope,
    pub provider_mode: InvestigationProviderMode,
    pub limit_profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_graph_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

fn present_inspection_requirements<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<InvestigationInspectionTarget>>, D::Error> {
    Vec::<InvestigationInspectionTarget>::deserialize(deserializer).map(Some)
}

/// Borrowed immutable fields shared across the closed request families. Family
/// schema, role, inspection authority and whole-request bounds stay with callers.
pub(super) struct CommonRequestFields<'a> {
    pub request_nonce: &'a str,
    pub question: &'a str,
    pub scope: &'a InvestigationScope,
    pub limit_profile: &'a str,
    pub expected_graph_revision: Option<&'a str>,
    pub expected_context_id: Option<&'a str>,
    pub conversation_id: Option<&'a str>,
    pub parent_id: Option<&'a str>,
}

impl CommonRequestFields<'_> {
    pub fn validate(&self) -> Result<(), InvestigationError> {
        self.validate_with_policy(&INVESTIGATION_V1_POLICY)
    }
    pub fn validate_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        self.scope.validate()?;
        if self.expected_context_id.is_some_and(|id| {
            !id.strip_prefix("context-v2:")
                .is_some_and(crate::source_basis::hash)
        }) || !text(self.question, MAX_QUESTION_BYTES)
            || !opaque_id(self.request_nonce)
            || self.limit_profile != policy.profile()
            || self
                .expected_graph_revision
                .is_some_and(|v| !context_hash(v))
            || self.conversation_id.is_some_and(|v| !opaque_id(v))
            || self.parent_id.is_some_and(|v| !opaque_id(v))
            || (self.parent_id.is_some() && self.conversation_id.is_none())
        {
            return Err(InvestigationError::InvalidRequest);
        }
        Ok(())
    }
}

impl StartInvestigationRequest {
    /// Cheap immutable validation; mutable availability and capacity checks must
    /// run after the durable deduplication lookup, not inside this method.
    pub fn validate(&self) -> Result<(), InvestigationError> {
        CommonRequestFields {
            request_nonce: &self.request_nonce,
            question: &self.question,
            scope: &self.scope,
            limit_profile: &self.limit_profile,
            expected_graph_revision: self.expected_graph_revision.as_deref(),
            expected_context_id: self.expected_context_id.as_deref(),
            conversation_id: self.conversation_id.as_deref(),
            parent_id: self.parent_id.as_deref(),
        }
        .validate()?;
        if self.specialist_id.uses_completion_obligations() {
            return Err(InvestigationError::InvalidRequest);
        }
        let v6 = self.specialist_id.uses_coverage_qualification();
        let v5 = self.specialist_id.uses_inspection_requirements();
        if (self.schema_version
            != if v6 {
                3
            } else if v5 {
                2
            } else {
                1
            })
            || (!v6 && self.inspection_requirements.is_some() != v5)
            || (!v6 && self.expected_context_id.is_some())
        {
            return Err(InvestigationError::InvalidRequest);
        }
        if let Some(targets) = &self.inspection_requirements {
            let targets = normalize_inspection_targets(targets)?;
            if self.expected_graph_revision.as_deref()
                != Some(targets[0].graph_snapshot_id.as_str())
            {
                return Err(InvestigationError::InvalidRequest);
            }
        }
        bounded_bytes(self, 8 * 1024)?;
        Ok(())
    }

    /// Hash binds original question bytes without storing them. Changing host
    /// provider/role availability must not change an identical nonce's identity.
    pub fn normalized(&self) -> Result<Self, InvestigationError> {
        self.validate()?;
        let mut request = self.clone();
        request.inspection_requirements = self
            .inspection_requirements
            .as_ref()
            .map(|targets| normalize_inspection_targets(targets))
            .transpose()?;
        Ok(request)
    }
    pub fn intent_hash(&self) -> Result<String, InvestigationError> {
        self.validate()?;
        if self.schema_version == 3 {
            return Ok(crate::source_basis::domain_hash(
                b"cartograph:investigation-client-intent:v3\0",
                &bounded_bytes(&self.normalized()?, 8 * 1024)?,
            ));
        }
        if self.schema_version == 2 {
            return Ok(crate::source_basis::domain_hash(
                b"cartograph:investigation-client-intent:v2\0",
                &bounded_bytes(&self.normalized()?, 8 * 1024)?,
            ));
        }
        Ok(crate::source_basis::domain_hash(
            b"cartograph:investigation-client-intent:v1\0",
            &bounded_bytes(self, 8 * 1024)?,
        ))
    }
}

pub(crate) fn opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
}
pub(crate) fn context_hash(value: &str) -> bool {
    value
        .strip_prefix("context-v1:")
        .is_some_and(crate::source_basis::hash)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationStatus {
    Queued,
    Preparing,
    Running,
    AwaitingConsent,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    OutcomeUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationActions {
    pub can_cancel: bool,
    pub can_follow_up: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationSummary {
    pub schema_version: u32,
    pub investigation_id: String,
    pub conversation_id: String,
    pub parent_id: Option<String>,
    pub job_id: i64,
    pub specialist_id: SpecialistId,
    pub question: String,
    pub scope: InvestigationScope,
    pub provider_mode: InvestigationProviderMode,
    pub status: InvestigationStatus,
    pub revision: u64,
    pub cancel_requested: bool,
    pub invocation_pending: bool,
    pub actions: InvestigationActions,
    pub graph_snapshot_id: Option<String>,
    pub last_event_sequence: u64,
    pub has_result: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_status: Option<InvestigationInspectionStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_requirement_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_outcome: Option<InvestigationCompletionOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obligation_status: Option<CompletionStatus>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationUsage {
    pub model_invocations: u32,
    pub tool_actions: u32,
    pub selected_facts: usize,
    pub evidence_requests: u32,
    pub evidence_items: usize,
    pub evidence_bytes: usize,
    pub captured_validation_bytes: u64,
    pub generated_token_reservations: u32,
    pub reported_input_tokens: Option<u64>,
    pub reported_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_cache_read_tokens: Option<u64>,
    pub active_milliseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvestigationDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_diagnostic: Option<InvestigationTerminalDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advisory_audit: Option<super::semantic_audit::AdvisoryAudit>,
    #[serde(flatten)]
    pub summary: InvestigationSummary,
    pub context_owner: String,
    pub origin: InvestigationOrigin,
    pub specialist: InvestigationSpecialist,
    pub provider: InvestigationProvider,
    pub scope_snapshot_id: Option<String>,
    /// Saved public qualification only; never the host's private proof basis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_context: Option<InvestigationCoverageContext>,
    pub limits: InvestigationLimits,
    pub usage: InvestigationUsage,
    pub citations: Vec<InvestigationCitation>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_requirements: Option<Vec<InvestigationInspectionTarget>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_assessment: Option<InvestigationInspectionAssessment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<InvestigationCompletionDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCoverageContext {
    pub context_id: String,
    pub coverage: context_hub::coverage::CoverageQualification,
}

/// Durable attribution only; an origin is never an execution grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationOrigin {
    App,
    Mcp { client_id: super::McpClientId },
}

impl InvestigationOrigin {
    pub fn request_namespace(&self) -> String {
        match self {
            Self::App => "app".into(),
            Self::Mcp { client_id } => format!("mcp:{}", client_id.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationPage {
    pub items: Vec<InvestigationSummary>,
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answer_match_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name_match_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationEventKind {
    Created,
    PreparationStarted,
    ContextPrepared,
    ModelStarted,
    ModelCompleted,
    ActionAdmitted,
    ToolStarted,
    ToolCompleted,
    ConsentRequired,
    ConsentApproved,
    ConsentDeclined,
    CancelRequested,
    ResultPersisted,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    OutcomeUnknown,
    EvidenceValidationReserved,
    FinishRefused,
    SourceReplayRefused,
    CapacityRefused,
    ActionRefused,
    ReadUnavailable,
    QueryRefused,
    /// The firewall withheld an acquired read from a cloud provider (#679).
    ReadFirewallRefused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_refusal: Option<Box<super::query_refusal::QueryRefusalRecord>>,
    pub investigation_id: String,
    pub sequence: u64,
    pub revision: u64,
    pub kind: InvestigationEventKind,
    pub step_id: Option<String>,
    pub tool: Option<InvestigationTool>,
    pub summary: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationEventPage {
    pub investigation_id: String,
    pub items: Vec<InvestigationEvent>,
    pub next_sequence: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationNarrative {
    /// Reuses the existing finding-statement prose bound, not a new policy family.
    pub text: String,
    pub finding_ids: Vec<String>,
}

impl InvestigationNarrative {
    // An omitted historical field stays absent; an explicit null is not a message.
    fn present<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Option<Self>, D::Error> {
        Self::deserialize(decoder).map(Some)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationResult {
    pub schema_version: u32,
    /// Exact mandatory qualification representation of the finishing invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<DisclosedInvestigationCoverage>,
    pub investigation_id: String,
    pub result_id: String,
    pub input_ledger_hash: String,
    pub graph_snapshot_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosure_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_assessment: Option<InvestigationInspectionAssessment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<InvestigationCompletionResult>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "InvestigationNarrative::present"
    )]
    pub narrative: Option<InvestigationNarrative>,
    pub findings: Vec<InvestigationFinding>,
    pub knowledge_completeness: KnowledgeCompleteness,
    pub limitations: Vec<String>,
    pub observed_response_model: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InvestigationEvidenceOrigin {
    GraphMetadata,
    WorkingTreeUnverified,
    CapturedPrimarySource {
        registered_source_id: String,
        receipt_id: String,
        receipt_inventory_index: u32,
        captured: TaskCaptureSpanRef,
        scope: PrimarySourceScope,
        input_closure: InputClosureStatus,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCitation {
    pub citation_id: String,
    pub fact: TaskFactKey,
    pub fact_digest: String,
    pub source: Option<EvidenceRef>,
    pub role: Option<TaskRangeRole>,
    pub index: Option<u32>,
    pub text_hash: Option<String>,
    pub origin: InvestigationEvidenceOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationConsent {
    pub investigation_id: String,
    pub step_id: String,
    pub revision: u64,
    pub preview: llm::EgressPreview,
    pub provider_profile: llm::bounded::ProviderProfile,
    /// May carry the hosted output allowance (AC-0570).
    #[serde(deserialize_with = "llm::bounded::deserialize_investigation_limits")]
    pub completion_limits: llm::bounded::CompletionLimits,
    pub provider: InvestigationProvider,
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationCitationStatus {
    Available,
    MetadataOnly,
    WorkingTreeUnverified,
    Unavailable,
    Invalid,
    OperationalFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationCitationRead {
    pub investigation_id: String,
    pub citation_id: String,
    pub status: InvestigationCitationStatus,
    pub citation: InvestigationCitation,
    pub text: Option<String>,
}

/// Raw query results and source bytes are not stored here. Their content hashes
/// bind the exact supplied payloads; they cannot reconstruct a lost transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationQueryManifest {
    pub query: InvestigationQuery,
    pub response_hash: String,
    pub response_bytes: usize,
    pub returned_facts: usize,
    pub total_selected: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationInputLedger {
    pub schema_version: u32,
    /// Historical preparation-time coverage, mandatory only for ledger v2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<context_hub::coverage::CoverageProof>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage_reads: Vec<InvestigationCoverageReadManifest>,
    /// Compiled-artifact pages (ADR-0089). Absent in every older ledger, so
    /// their bytes and fingerprints are unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_reads: Vec<InvestigationArtifactReadManifest>,
    pub graph_snapshot_id: String,
    pub scope_snapshot_id: String,
    pub revision: u64,
    pub selected_facts: Vec<TaskFactSelection>,
    pub receipt_references: Vec<InvestigationReceiptReference>,
    pub citations: Vec<InvestigationCitation>,
    pub queries: Vec<InvestigationQueryManifest>,
    pub supplied_history_hash: Option<String>,
    pub supplied_history_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationReceiptReference {
    pub source_id: String,
    pub repo_key: String,
    pub receipt_id: String,
}

impl InvestigationInputLedger {
    pub fn fingerprint(&self) -> Result<String, InvestigationError> {
        self.fingerprint_with_policy(&INVESTIGATION_V1_POLICY)
    }

    /// Policy validation changes admission, never the legacy wire or hash domain.
    pub fn fingerprint_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<String, InvestigationError> {
        self.validate_with_policy(policy)?;
        Ok(crate::source_basis::domain_hash(
            if self.schema_version == 2 {
                b"cartograph:investigation-input-ledger:v2\0"
            } else {
                b"cartograph:investigation-input-ledger:v1\0"
            },
            &bounded_bytes(self, MAX_MANIFEST_BYTES)?,
        ))
    }
}

/// Content-free failure provenance. Unknown versions, fields and enum values fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum InvestigationTerminalDiagnostic {
    /// A completed response that failed action decoding: the closed decode
    /// stage or semantic rule family plus error class, never response text.
    #[serde(rename = "investigation-terminal-diagnostic@5")]
    RejectedAction {
        stage: InvestigationActionDecodeDiagnostic,
        reason: InvestigationFinishRejection,
    },
    /// A host input-preparation refusal at a closed boundary, never a field,
    /// query or error string.
    #[serde(rename = "investigation-terminal-diagnostic@6")]
    Preparation { phase: InvestigationFailurePhase },
    #[serde(rename = "investigation-terminal-diagnostic@4")]
    CapacityCompletion { reason: CapacityCompletionReason },
    #[serde(rename = "investigation-terminal-diagnostic@3")]
    RejectedFinish {
        reason: InvestigationFinishRejection,
    },
    #[serde(rename = "investigation-terminal-diagnostic@2")]
    QueryBudget {
        termination: QueryBudgetTermination,
        budget: query_refusal::QueryBudgetDiagnostic,
    },
    #[serde(rename = "investigation-terminal-diagnostic@1")]
    Operational {
        phase: InvestigationFailurePhase,
        error_class: InvestigationOperationalClass,
    },
    /// An executed source read whose candidate was discarded (AC-1609): the
    /// closed read or publication step and the error class it failed with.
    #[serde(rename = "investigation-terminal-diagnostic@7")]
    SourceReadDiscarded {
        step: SourceDiscardStep,
        error_class: SourceDiscardClass,
    },
}
impl InvestigationTerminalDiagnostic {
    /// The longest serialized `@7` record, for sizing a settlement before the
    /// actual step and class are known.
    pub fn widest_source_discard() -> Self {
        fn width(value: impl Serialize) -> usize {
            serde_json::to_vec(&value).map_or(usize::MAX, |bytes| bytes.len())
        }
        let step = SourceDiscardStep::ALL
            .into_iter()
            .max_by_key(|step| width(step))
            .unwrap_or(SourceDiscardStep::ReadText);
        let error_class = SourceDiscardClass::ALL
            .into_iter()
            .max_by_key(|class| width(class))
            .unwrap_or(SourceDiscardClass::HostOperational);
        Self::SourceReadDiscarded { step, error_class }
    }
}
/// Where an executed source read was discarded (AC-1609).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceDiscardStep {
    /// The pinned text read itself failed.
    ReadText,
    /// The text read does not span the captured byte range.
    ReadLength,
    /// The receipt understated the measured source sizes.
    ReadSizes,
    /// The source could not be appended to the evidence ledger.
    ReadEvidence,
    PublishValidate,
    PublishSnapshot,
    PublishProof,
    PublishPrompt,
    PublishPrepare,
    PublishSeal,
    PublishMeasure,
    PublishCommit,
}
impl SourceDiscardStep {
    pub const ALL: [Self; 12] = [
        Self::ReadText,
        Self::ReadLength,
        Self::ReadSizes,
        Self::ReadEvidence,
        Self::PublishValidate,
        Self::PublishSnapshot,
        Self::PublishProof,
        Self::PublishPrompt,
        Self::PublishPrepare,
        Self::PublishSeal,
        Self::PublishMeasure,
        Self::PublishCommit,
    ];
}
/// The closed error class of a discarded source read (AC-1609).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceDiscardClass {
    HostInputChanged,
    HostSourceUnavailable,
    HostInvalidInput,
    HostLimitExceeded,
    HostStoreCapacity,
    HostOperational,
    StoreInvalid,
    StoreMissing,
    StoreConflict,
    StoreStale,
    StoreCapacity,
    StoreUnavailable,
    StoreCancelled,
    StoreOwnership,
    StoreStorage,
    ProviderBounded,
    BoundInvariant,
}
impl SourceDiscardClass {
    pub const ALL: [Self; 17] = [
        Self::HostInputChanged,
        Self::HostSourceUnavailable,
        Self::HostInvalidInput,
        Self::HostLimitExceeded,
        Self::HostStoreCapacity,
        Self::HostOperational,
        Self::StoreInvalid,
        Self::StoreMissing,
        Self::StoreConflict,
        Self::StoreStale,
        Self::StoreCapacity,
        Self::StoreUnavailable,
        Self::StoreCancelled,
        Self::StoreOwnership,
        Self::StoreStorage,
        Self::ProviderBounded,
        Self::BoundInvariant,
    ];
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityCompletionReason {
    UnmetObligationsAfterCapacityExclusion,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryBudgetTermination {
    PageBudget,
    ToolActions,
    ModelInvocations,
    ActiveTime,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationFailurePhase {
    Unspecified,
    Continuation,
    EgressRecording,
    Finish,
    Metadata,
    Query,
    Inventory,
    Candidate,
    Certification,
    Seal,
    Publication,
    Source,
    SourcePrepare,
    SourceCertification,
    SourceReservation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationOperationalClass {
    HostOperational,
    StoreInvalid,
    StoreMissing,
    StoreConflict,
    StoreStale,
    StoreUnavailable,
    StoreOwnership,
    StoreStorage,
}

/// Closed, text-free decode boundary of a rejected action (AC-0725). Semantic
/// rule families are flattened into the same vocabulary as the terminal error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationActionDecodeDiagnostic {
    Preflight,
    TypedEnvelope,
    TypedStagedFinish,
    TypedCoverageRead,
    TypedLegacyAction,
    TypedReplay,
    BareFinish,
    CanonicalFields,
    Semantic,
    QueryContext,
    QueryShape,
    InventoryContext,
    InventoryShape,
    ArtifactContext,
    ArtifactShape,
    ReplayContext,
    ReplayReference,
    EvidenceShape,
    FinishRole,
    FinishShape,
    FinishOverall,
    FindingTextLimits,
    FindingCitations,
    FinishOutcome,
    HostBinding,
}
impl From<ActionDecodeStage> for InvestigationActionDecodeDiagnostic {
    fn from(stage: ActionDecodeStage) -> Self {
        use ActionDecodeStage as Stage;
        use ActionSemanticRule as Rule;
        match stage {
            Stage::Preflight => Self::Preflight,
            Stage::TypedEnvelope => Self::TypedEnvelope,
            Stage::TypedStagedFinish => Self::TypedStagedFinish,
            Stage::TypedCoverageRead => Self::TypedCoverageRead,
            Stage::TypedLegacyAction => Self::TypedLegacyAction,
            Stage::TypedReplay => Self::TypedReplay,
            Stage::BareFinish => Self::BareFinish,
            Stage::CanonicalFields => Self::CanonicalFields,
            Stage::Semantic => Self::Semantic,
            Stage::SemanticRule(rule) => match rule {
                Rule::QueryContext => Self::QueryContext,
                Rule::QueryShape => Self::QueryShape,
                Rule::InventoryContext => Self::InventoryContext,
                Rule::InventoryShape => Self::InventoryShape,
                Rule::ArtifactContext => Self::ArtifactContext,
                Rule::ArtifactShape => Self::ArtifactShape,
                Rule::ReplayContext => Self::ReplayContext,
                Rule::ReplayReference => Self::ReplayReference,
                Rule::EvidenceShape => Self::EvidenceShape,
                Rule::FinishRole => Self::FinishRole,
                Rule::FinishShape => Self::FinishShape,
                Rule::FinishOverall => Self::FinishOverall,
                Rule::FindingTextLimits => Self::FindingTextLimits,
                Rule::FindingCitations => Self::FindingCitations,
                Rule::FinishOutcome => Self::FinishOutcome,
            },
            Stage::HostBinding => Self::HostBinding,
        }
    }
}
impl From<ActionDecodeFailure> for InvestigationTerminalDiagnostic {
    fn from(failure: ActionDecodeFailure) -> Self {
        Self::RejectedAction {
            stage: failure.stage.into(),
            reason: failure.error.into(),
        }
    }
}

/// Closed, text-free origin of a rejected authenticated finish (AC-0591).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestigationFinishRejection {
    InvalidRequest,
    InvalidAction,
    LimitExceeded,
    InvalidInput,
    InvalidCitation,
    SourceReplay,
    SensitiveOutput,
}
impl From<InvestigationError> for InvestigationFinishRejection {
    fn from(error: InvestigationError) -> Self {
        match error {
            InvestigationError::InvalidRequest => Self::InvalidRequest,
            InvestigationError::InvalidAction => Self::InvalidAction,
            InvestigationError::LimitExceeded => Self::LimitExceeded,
            InvestigationError::InvalidInput => Self::InvalidInput,
            InvestigationError::InvalidCitation => Self::InvalidCitation,
            InvestigationError::SourceReplay => Self::SourceReplay,
            InvestigationError::SensitiveOutput => Self::SensitiveOutput,
        }
    }
}
