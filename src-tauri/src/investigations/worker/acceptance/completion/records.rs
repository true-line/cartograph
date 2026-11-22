//! Pure, bounded evidence records for the n=1-per-role V7 control.
//! Semantic review and operator authorization are separate from sealed evidence.
use agents::investigation::{staged_finish::CompletionOutcome, *};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub(super) const MAX_RECEIPT_BYTES: usize = 64 * 1024;
// Historical message remains accepted; new messages come only from the closed
// production reason mapping. Never infer exhaustion from usage counters.
const EXHAUSTION_REASON: &str = "An investigation work or storage limit was reached.";
fn exhaustion_reason(message: &str) -> bool {
    use crate::jobs::investigations::InvestigationStopReason as Stop;
    use llm::bounded::{BoundedCallError, FailureCode, InvocationOutcome};
    message == EXHAUSTION_REASON
        || Stop::is_host_limit_message(message)
        || message == Stop::StoreCapacity.test_message()
        || [
            FailureCode::DeadlineExceeded,
            FailureCode::InputTooLarge,
            FailureCode::RequestTooLarge,
            FailureCode::ResponseTooLarge,
            FailureCode::OutputTooLarge,
        ]
        .iter()
        .any(|code| {
            let reason = Stop::BoundedProvider(BoundedCallError {
                code: *code,
                outcome: InvocationOutcome::ResponseReceived,
            });
            reason.terminal_class() == Stop::LimitExceeded && message == reason.test_message()
        })
}

#[derive(Debug)]
pub(super) enum RecordError {
    Invalid,
    Capacity,
    Io(std::io::Error),
}
impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => f.write_str("Invalid acceptance record"),
            Self::Capacity => f.write_str("Acceptance record capacity exceeded"),
            Self::Io(error) => write!(f, "Acceptance record I/O failed: {error}"),
        }
    }
}
impl std::error::Error for RecordError {}
impl From<std::io::Error> for RecordError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Role {
    Analyst,
    Auditor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CheckStatus {
    Passed,
    Failed,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MechanicalCheck {
    pub name: String,
    pub status: CheckStatus,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SemanticGate {
    Pending,
    Passed,
    Failed,
    Unreviewable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ControlClassification {
    SupportedFirstShot,
    SupportedAfterOneRefusal,
    Insufficient,
    ObligationsUnmet,
    Failed,
    Cancelled,
    Exhausted,
    Unknown,
    Unreviewable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunBinding {
    pub run_id: String,
    pub protocol_hash: String,
    pub source_commit: String,
    pub source_tree: String,
    pub context_id: String,
    pub request_hash: String,
    pub obligations_hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RoleReceipt {
    pub schema_version: u32,
    pub role: Role,
    pub samples_per_role: u8,
    pub automatic_retry: bool,
    pub binding: RunBinding,
    pub investigation_id: String,
    pub terminal_status: InvestigationStatus,
    pub stop_cause: Option<String>,
    pub classification: ControlClassification,
    pub durable_refusal_count: u32,
    pub refusals_before_finish: Option<u32>,
    pub detail_hash: String,
    pub result_hash: Option<String>,
    pub events_hash: String,
    pub usage: InvestigationUsage,
    pub mechanical_checks: Vec<MechanicalCheck>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReceiptRef {
    pub body_hash: String,
    pub byte_len: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Evaluation {
    pub classification: ControlClassification,
    pub mechanical: CheckStatus,
    pub semantic: SemanticGate,
    pub control_pass: bool,
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
impl RoleReceipt {
    pub(super) fn validate(&self) -> Result<(), RecordError> {
        let binding = &self.binding;
        if self.schema_version != 1
            || self.samples_per_role != 1
            || self.automatic_retry
            || self
                .stop_cause
                .as_ref()
                .is_some_and(|cause| cause.len() > 512 || cause.chars().any(char::is_control))
            || !identifier(&binding.run_id)
            || !identifier(&self.investigation_id)
            || !hex(&binding.protocol_hash, 64)
            || !hex(&binding.source_commit, 40)
            || !hex(&binding.source_tree, 40)
            || !binding
                .context_id
                .strip_prefix("context-v2:")
                .is_some_and(|id| hex(id, 64))
            || !hex(&binding.request_hash, 64)
            || !hex(&binding.obligations_hash, 64)
            || !hex(&self.detail_hash, 64)
            || !hex(&self.events_hash, 64)
            || self.result_hash.as_ref().is_some_and(|hash| !hex(hash, 64))
            || self.durable_refusal_count > 2
            || self.mechanical_checks.is_empty()
            || self.mechanical_checks.len() > 64
        {
            return Err(RecordError::Invalid);
        }
        let mut names = BTreeSet::new();
        for check in &self.mechanical_checks {
            if !identifier(&check.name) || !names.insert(&check.name) {
                return Err(RecordError::Invalid);
            }
        }
        let has_result = self.result_hash.is_some();
        if has_result != (self.terminal_status == InvestigationStatus::Completed)
            || self.refusals_before_finish != has_result.then_some(self.durable_refusal_count)
        {
            return Err(RecordError::Invalid);
        }
        let valid = match self.classification {
            ControlClassification::SupportedFirstShot => {
                has_result && self.durable_refusal_count == 0
            }
            ControlClassification::SupportedAfterOneRefusal => {
                has_result && self.durable_refusal_count == 1
            }
            ControlClassification::Insufficient => has_result && self.durable_refusal_count <= 1,
            ControlClassification::ObligationsUnmet => {
                self.terminal_status == InvestigationStatus::Failed
                    && self.durable_refusal_count == 2
            }
            ControlClassification::Failed => {
                self.terminal_status == InvestigationStatus::Failed && !has_result
            }
            ControlClassification::Exhausted => {
                self.terminal_status == InvestigationStatus::Failed
                    && !has_result
                    && self.stop_cause.as_deref().is_some_and(exhaustion_reason)
            }
            ControlClassification::Cancelled => {
                self.terminal_status == InvestigationStatus::Cancelled && !has_result
            }
            ControlClassification::Unknown => {
                matches!(
                    self.terminal_status,
                    InvestigationStatus::OutcomeUnknown | InvestigationStatus::Interrupted
                ) && !has_result
            }
            // Unreviewable is a separate evaluation, never the immutable runtime observation.
            ControlClassification::Unreviewable => false,
        };
        if !valid {
            return Err(RecordError::Invalid);
        }
        Ok(())
    }
}

/// Inputs must come from fresh coordinator reads, including the complete ordered
/// event stream. This derives outcome/counts; it does not claim semantic approval.
pub(super) fn record_role(
    role: Role,
    binding: RunBinding,
    detail: &InvestigationDetail,
    result: Option<&InvestigationResult>,
    events: &[InvestigationEvent],
    mechanical_checks: Vec<MechanicalCheck>,
) -> Result<RoleReceipt, RecordError> {
    let completion = detail.completion.as_ref().ok_or(RecordError::Invalid)?;
    if completion.validate().map_err(|_| RecordError::Invalid)?
        != detail
            .summary
            .obligation_status
            .ok_or(RecordError::Invalid)?
    {
        return Err(RecordError::Invalid);
    }
    let expected_role = match role {
        Role::Analyst => SpecialistId::DomainAnalystV7,
        Role::Auditor => SpecialistId::EvidenceAuditorV7,
    };
    if detail.summary.schema_version != 4
        || detail.summary.specialist_id != expected_role
        || detail.summary.has_result != result.is_some()
        || detail.summary.invocation_pending
        || completion.request_hash != binding.request_hash
        || completion.obligations_hash != binding.obligations_hash
        || match detail.coverage_context.as_ref() {
            Some(coverage) => coverage.context_id != binding.context_id,
            None => {
                result.is_some()
                    || detail.usage.model_invocations != 0
                    || completion.refusal_count != 0
            }
        }
        || events.is_empty()
        || events.len() > 96
        || events.len() as u64 != detail.summary.last_event_sequence
    {
        return Err(RecordError::Invalid);
    }
    let terminal_kind = match detail.summary.status {
        InvestigationStatus::Completed => InvestigationEventKind::ResultPersisted,
        InvestigationStatus::Failed => InvestigationEventKind::Failed,
        InvestigationStatus::Cancelled => InvestigationEventKind::Cancelled,
        InvestigationStatus::Interrupted => InvestigationEventKind::Interrupted,
        InvestigationStatus::OutcomeUnknown => InvestigationEventKind::OutcomeUnknown,
        _ => return Err(RecordError::Invalid),
    };
    if events
        .last()
        .is_none_or(|event| event.kind != terminal_kind)
    {
        return Err(RecordError::Invalid);
    }
    let mut refusals = 0;
    for (index, event) in events.iter().enumerate() {
        if event.investigation_id != detail.summary.investigation_id
            || event.sequence != index as u64 + 1
        {
            return Err(RecordError::Invalid);
        }
        if event.kind == InvestigationEventKind::FinishRefused {
            refusals += 1;
        }
    }
    if refusals != completion.refusal_count || refusals > 2 {
        return Err(RecordError::Invalid);
    }
    let classification = match detail.summary.status {
        InvestigationStatus::Completed => {
            let result = result.ok_or(RecordError::Invalid)?;
            let result_completion = result.completion.as_ref().ok_or(RecordError::Invalid)?;
            if result.schema_version != 5
                || result.investigation_id != detail.summary.investigation_id
                || result_completion.request_hash != binding.request_hash
                || result_completion.obligations_hash != binding.obligations_hash
                || result_completion.assessment != completion.assessment
                || refusals > 1
            {
                return Err(RecordError::Invalid);
            }
            let CompletionAssessmentBasis::FinishingDisclosure {
                step_id,
                input_ledger_hash,
                disclosure_hash,
                finishing_attempts_hash,
            } = &completion.assessment_basis
            else {
                return Err(RecordError::Invalid);
            };
            if result.invocation_step_id.as_ref() != Some(step_id)
                || &result.input_ledger_hash != input_ledger_hash
                || result.disclosure_hash.as_ref() != Some(disclosure_hash)
                || &result_completion.finishing_attempts_hash != finishing_attempts_hash
                || result
                    .coverage
                    .as_ref()
                    .is_none_or(|coverage| coverage.context_id != binding.context_id)
            {
                return Err(RecordError::Invalid);
            }
            result_completion
                .validate(
                    &result.findings,
                    result.knowledge_completeness,
                    &result.limitations,
                )
                .map_err(|_| RecordError::Invalid)?;
            match (result_completion.outcome, detail.summary.completion_outcome) {
                (CompletionOutcome::Supported, Some(InvestigationCompletionOutcome::Supported)) => {
                    if detail.summary.obligation_status != Some(CompletionStatus::Satisfied)
                        || result.findings.is_empty()
                    {
                        return Err(RecordError::Invalid);
                    }
                    if refusals == 0 {
                        ControlClassification::SupportedFirstShot
                    } else {
                        ControlClassification::SupportedAfterOneRefusal
                    }
                }
                (
                    CompletionOutcome::InsufficientEvidence,
                    Some(InvestigationCompletionOutcome::InsufficientEvidence),
                ) if result.findings.is_empty()
                    && result.knowledge_completeness
                        == KnowledgeCompleteness::InsufficientEvidence =>
                {
                    ControlClassification::Insufficient
                }
                _ => return Err(RecordError::Invalid),
            }
        }
        InvestigationStatus::Failed
            if detail.summary.completion_outcome
                == Some(InvestigationCompletionOutcome::ObligationsUnmet)
                && refusals == 2 =>
        {
            ControlClassification::ObligationsUnmet
        }
        InvestigationStatus::Failed
            if detail.error.as_deref().is_some_and(exhaustion_reason)
                && events.last().is_some_and(|event| {
                    event.kind == InvestigationEventKind::Failed
                        && Some(event.summary.as_str()) == detail.error.as_deref()
                }) =>
        {
            ControlClassification::Exhausted
        }
        InvestigationStatus::Failed => ControlClassification::Failed,
        InvestigationStatus::Cancelled => ControlClassification::Cancelled,
        InvestigationStatus::Interrupted | InvestigationStatus::OutcomeUnknown => {
            ControlClassification::Unknown
        }
        _ => return Err(RecordError::Invalid),
    };
    let receipt = RoleReceipt {
        schema_version: 1,
        role,
        samples_per_role: 1,
        automatic_retry: false,
        binding,
        investigation_id: detail.summary.investigation_id.clone(),
        terminal_status: detail.summary.status,
        stop_cause: detail.error.clone(),
        classification,
        durable_refusal_count: refusals,
        refusals_before_finish: result.map(|_| refusals),
        detail_hash: hash_json(detail)?,
        result_hash: result.map(hash_json).transpose()?,
        events_hash: hash_json(events)?,
        usage: detail.usage.clone(),
        mechanical_checks,
    };
    receipt.validate()?;
    Ok(receipt)
}
pub(super) fn evaluate(
    receipt: &RoleReceipt,
    semantic: SemanticGate,
) -> Result<Evaluation, RecordError> {
    receipt.validate()?;
    let mechanical = if receipt
        .mechanical_checks
        .iter()
        .any(|check| check.status == CheckStatus::Failed)
    {
        CheckStatus::Failed
    } else if receipt
        .mechanical_checks
        .iter()
        .any(|check| check.status == CheckStatus::Unknown)
    {
        CheckStatus::Unknown
    } else {
        CheckStatus::Passed
    };
    let supported = matches!(
        receipt.classification,
        ControlClassification::SupportedFirstShot | ControlClassification::SupportedAfterOneRefusal
    );
    Ok(Evaluation {
        classification: if supported && semantic == SemanticGate::Unreviewable {
            ControlClassification::Unreviewable
        } else {
            receipt.classification
        },
        mechanical,
        semantic,
        control_pass: supported
            && mechanical == CheckStatus::Passed
            && semantic == SemanticGate::Passed,
    })
}
struct BoundedWriter {
    bytes: Vec<u8>,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > MAX_RECEIPT_BYTES {
            return Err(std::io::Error::other("receipt capacity"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn json_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, RecordError> {
    let mut writer = BoundedWriter { bytes: Vec::new() };
    serde_json::to_writer(&mut writer, value).map_err(|_| RecordError::Capacity)?;
    Ok(writer.bytes)
}
fn hash_json<T: Serialize + ?Sized>(value: &T) -> Result<String, RecordError> {
    Ok(core_prov::content_hash(&json_bytes(value)?))
}
/// Exclusive creation is intentionally not retried or overwritten after a partial failure.
pub(super) fn write_new_receipt(
    path: &Path,
    receipt: &RoleReceipt,
) -> Result<ReceiptRef, RecordError> {
    receipt.validate()?;
    if !path.is_absolute() {
        return Err(RecordError::Invalid);
    }
    let bytes = json_bytes(receipt)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    // Portable directory sync: a plain `File::open` of a directory is
    // refused on Windows.
    super::super::expanded::vendure_prepare::sync_directory(
        path.parent().ok_or(RecordError::Invalid)?,
    )
    .map_err(|error| RecordError::Io(std::io::Error::other(error)))?;
    Ok(ReceiptRef {
        body_hash: core_prov::content_hash(&bytes),
        byte_len: bytes.len() as u64,
    })
}
pub(super) fn read_verified_receipt(
    path: &Path,
    expected: &ReceiptRef,
) -> Result<RoleReceipt, RecordError> {
    if expected.byte_len == 0
        || expected.byte_len > MAX_RECEIPT_BYTES as u64
        || !hex(&expected.body_hash, 64)
    {
        return Err(RecordError::Invalid);
    }
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(RecordError::Invalid);
    }
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != expected.byte_len
        || core_prov::content_hash(&bytes) != expected.body_hash
    {
        return Err(RecordError::Invalid);
    }
    let receipt: RoleReceipt = serde_json::from_slice(&bytes).map_err(|_| RecordError::Invalid)?;
    receipt.validate()?;
    if json_bytes(&receipt)? != bytes {
        return Err(RecordError::Invalid);
    }
    Ok(receipt)
}
#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
