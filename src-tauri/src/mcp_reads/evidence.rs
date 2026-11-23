//! Receipt-bound retained spans; never current checkout reads.
use super::{ReadError, ReadSession};
use agents::{TaskFactKey, investigation::InvestigationInspectionTarget};
use serde::Serialize;

fn inspection_error(error: crate::investigations::inspection::InspectionError) -> ReadError {
    use crate::investigations::inspection::InspectionError;
    match error {
        InspectionError::Stale => ReadError::StaleBasis,
        InspectionError::OrdinalOutOfRange => ReadError::InvalidRequest,
        InspectionError::RangeTooLarge => ReadError::Limit,
        InspectionError::Limit => ReadError::Limit,
        InspectionError::Forgotten => ReadError::EvidenceForgotten,
        InspectionError::Omitted(kind) => ReadError::EvidenceOmitted(kind),
        InspectionError::Unavailable => ReadError::EvidenceUnavailable,
        InspectionError::Invalid => ReadError::EvidenceInvalid,
        InspectionError::Operational => ReadError::Storage,
    }
}
fn retained_error(error: crate::primary_source::PinnedReadError) -> ReadError {
    inspection_error(error.into())
}

/// Per-read work admission supplied by the service, independently of framing.
pub(crate) struct EvidenceLimits {
    pub max_file_validation_bytes: u64,
    pub max_span_bytes: u64,
    pub max_response_bytes: usize,
}

/// The free-text redaction applied to retained source text before it is
/// returned to an MCP client (#1754). The client is normally a model-backed
/// agent outside the app's egress consent, so it never receives the raw span.
/// `replacements > 0` means `text` is not byte-identical to the captured span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct EvidenceRedaction {
    pub policy: &'static str,
    pub replacements: usize,
}

impl EvidenceRedaction {
    /// The envelope's shape before any text has been read.
    pub(crate) const UNREAD: Self = Self {
        policy: core_redact::TEXT_POLICY_VERSION,
        replacements: 0,
    };

    /// Redact `text` and say how much was replaced.
    pub(crate) fn apply(text: &str) -> (String, Self) {
        let (text, replacements) = core_redact::redact_text(text);
        (
            text,
            Self {
                policy: core_redact::TEXT_POLICY_VERSION,
                replacements,
            },
        )
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct EvidenceSpan {
    pub basis_id: String,
    pub target: InvestigationInspectionTarget,
    pub scope: &'static str,
    pub input_closure: &'static str,
    pub redaction: EvidenceRedaction,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct EvidenceDescription {
    pub basis_id: String,
    pub target: InvestigationInspectionTarget,
    pub total_ranges: usize,
    pub next_ordinal: Option<usize>,
    pub scope: &'static str,
    pub input_closure: &'static str,
}

impl ReadSession {
    /// Describe exactly one retained range, not an unbounded receipt inventory.
    /// The caller binds this to an authenticated connection and admits the outer
    /// MCP envelope separately. No captured source bytes are read here.
    pub(crate) fn describe_evidence(
        &self,
        state: &crate::AppState,
        expected_basis: &str,
        fact: &TaskFactKey,
        ordinal: usize,
        max_response_bytes: usize,
    ) -> Result<EvidenceDescription, ReadError> {
        let basis_id = self.basis_id()?;
        if expected_basis != basis_id {
            return Err(ReadError::StaleBasis);
        }
        if !self.evidence_fact_visible(fact) {
            return Err(ReadError::InvalidRequest);
        }
        if max_response_bytes == 0 {
            return Err(ReadError::Limit);
        }
        let (target, _guard, receipt) =
            crate::investigations::inspection::resolve_mcp_with_receipt_typed_bounded(
                state,
                fact,
                &self.basis.recovered_graph_id,
                None,
                ordinal,
                self.graph_limits,
            )
            .map_err(inspection_error)?;
        let total_ranges = receipt.ranges().len();
        let response = EvidenceDescription {
            basis_id,
            target,
            total_ranges,
            next_ordinal: ordinal.checked_add(1).filter(|next| *next < total_ranges),
            scope: "primary_source_only",
            input_closure: "input_closure_not_established",
        };
        if serde_json::to_vec(&response)
            .map_err(|_| ReadError::Storage)?
            .len()
            > max_response_bytes
        {
            return Err(ReadError::Limit);
        }
        Ok(response)
    }

    fn evidence_fact_visible(&self, fact: &TaskFactKey) -> bool {
        // Frozen from the complete compiler projection, including hidden-rule adjacency.
        let reference = match fact {
            TaskFactKey::Node { id } => context_hub::FactReference::Node { id: id.clone() },
            TaskFactKey::Edge {
                source,
                label,
                destination,
            } => context_hub::FactReference::Edge {
                source: source.clone(),
                label: label.clone(),
                destination: destination.clone(),
            },
        };
        self.visible_facts.contains(&reference)
    }

    pub(crate) fn read_evidence(
        &self,
        state: &crate::AppState,
        expected_basis: &str,
        target: &InvestigationInspectionTarget,
        limits: EvidenceLimits,
    ) -> Result<EvidenceSpan, ReadError> {
        let basis_id = self.basis_id()?;
        if expected_basis != basis_id || target.graph_snapshot_id != self.basis.recovered_graph_id {
            return Err(ReadError::StaleBasis);
        }
        target.validate().map_err(|_| ReadError::InvalidRequest)?;
        if !self.evidence_fact_visible(&target.fact) {
            return Err(ReadError::InvalidRequest);
        }
        let span_bytes = target
            .captured
            .byte_end
            .checked_sub(target.captured.byte_start)
            .ok_or(ReadError::InvalidRequest)?;
        if limits.max_file_validation_bytes == 0
            || limits.max_span_bytes == 0
            || limits.max_response_bytes == 0
            || target.captured.file.byte_len > limits.max_file_validation_bytes
            || span_bytes > limits.max_span_bytes
        {
            return Err(ReadError::Limit);
        }
        let mut response = EvidenceSpan {
            basis_id,
            target: target.clone(),
            scope: "primary_source_only",
            input_closure: "input_closure_not_established",
            redaction: EvidenceRedaction::UNREAD,
            text: String::new(),
        };
        // Admit worst-case JSON escaping before reading/validating captured bytes.
        // Six bytes per input byte is conservative even for control characters.
        let envelope = serde_json::to_vec(&response)
            .map_err(|_| ReadError::Storage)?
            .len();
        let worst_text = usize::try_from(span_bytes)
            .ok()
            .and_then(|n| n.checked_mul(6))
            .ok_or(ReadError::Limit)?;
        if envelope
            .checked_add(worst_text)
            .is_none_or(|n| n > limits.max_response_bytes)
        {
            return Err(ReadError::Limit);
        }
        let (actual, guard, receipt) =
            crate::investigations::inspection::resolve_mcp_with_receipt_typed_bounded(
                state,
                &target.fact,
                &self.basis.recovered_graph_id,
                Some(target),
                target.receipt_inventory_index as usize,
                self.graph_limits,
            )
            .map_err(inspection_error)?;
        if actual != *target {
            return Err(ReadError::StaleBasis);
        }
        let raw = state
            .primary_sources
            .task_text_bounded(
                &guard,
                &receipt,
                target.receipt_inventory_index as usize,
                limits.max_file_validation_bytes,
                limits.max_span_bytes,
            )
            .map_err(retained_error)?;
        (response.text, response.redaction) = EvidenceRedaction::apply(&raw);
        drop(raw);
        // Guard remains alive through the real read. No lease or provider budget
        // is implied; the service must separately charge cumulative read work.
        // Redaction can lengthen the text, so the size is checked on what is sent.
        if serde_json::to_vec(&response)
            .map_err(|_| ReadError::Storage)?
            .len()
            > limits.max_response_bytes
        {
            return Err(ReadError::Limit);
        }
        Ok(response)
    }
}
