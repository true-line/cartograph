//! Receipt-bound retained spans; never current checkout reads.
use super::{ReadError, ReadSession};
use agents::{TaskFactKey, investigation::InvestigationInspectionTarget};
use serde::Serialize;

fn inspection_error(error: crate::investigations::inspection::InspectionError) -> ReadError {
    use crate::investigations::inspection::InspectionError;
    match error {
        InspectionError::Stale => ReadError::StaleBasis,
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

#[derive(Debug, Serialize)]
pub(crate) struct EvidenceSpan {
    pub basis_id: String,
    pub target: InvestigationInspectionTarget,
    pub scope: &'static str,
    pub input_closure: &'static str,
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
        let binding = state
            .graph
            .lock()
            .map_err(|_| ReadError::Storage)?
            .current_source_binding(&crate::task_evidence::graph_fact(fact))
            .map_err(|_| ReadError::Storage)?
            .ok_or(ReadError::StaleBasis)?;
        let (target, guard) = crate::investigations::inspection::resolve_typed(
            state,
            fact,
            &self.basis.recovered_graph_id,
            &binding.receipt_id,
            ordinal,
        )
        .map_err(inspection_error)?;
        let receipt = state
            .primary_sources
            .task_receipt(
                &guard,
                &binding.fact,
                &target.fact_digest,
                &target.source.repo,
                &target.receipt_id,
            )
            .map_err(retained_error)?;
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
        // Reuse the entire compiler projection, including hidden-rule adjacency.
        let (nodes, edges) = spec::project_recovered_facts(
            &self.raw_nodes,
            &self.raw_edges,
            self.basis.export_mode,
            &self.rejected_hashes,
        );
        match fact {
            TaskFactKey::Node { id } => nodes.iter().any(|n| &n.id == id),
            TaskFactKey::Edge {
                source,
                label,
                destination,
            } => edges
                .iter()
                .any(|e| &e.src == source && &e.label == label && &e.dst == destination),
        }
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
        let (actual, guard) = crate::investigations::inspection::resolve_typed(
            state,
            &target.fact,
            &self.basis.recovered_graph_id,
            &target.receipt_id,
            target.receipt_inventory_index as usize,
        )
        .map_err(inspection_error)?;
        if actual != *target {
            return Err(ReadError::StaleBasis);
        }
        let receipt = state
            .primary_sources
            .task_receipt(
                &guard,
                &crate::task_evidence::graph_fact(&target.fact),
                &target.fact_digest,
                &target.source.repo,
                &target.receipt_id,
            )
            .map_err(retained_error)?;
        response.text = state
            .primary_sources
            .task_text(&guard, &receipt, target.receipt_inventory_index as usize)
            .map_err(retained_error)?;
        // Guard remains alive through the real read. No lease or provider budget
        // is implied; the service must separately charge cumulative read work.
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
