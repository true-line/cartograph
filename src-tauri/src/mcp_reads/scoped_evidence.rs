//! Receipt-bound scoped reads; no complete snapshot IDs or checkout fallback.
use super::{ReadError, evidence::EvidenceLimits, scoped_session::ScopedEvidenceProof};
use crate::{
    AppState,
    primary_source::{PinnedReadError, RetentionGuard},
};
use adapters_lang_ts::captured::Receipt;
use agents::{TaskCaptureFileRef, TaskCaptureSpanRef, TaskFactKey};
use core_graph::BindingRole;
use core_graph::source::{FactSourceSelection, FactSourceState, OmissionLookup, SourceBinding};
use serde::Serialize;
use std::io::{self, Write};

mod target;
pub(crate) use target::ScopedEvidenceTarget;

/// Count borrowed escaped JSON before allocating a descriptor/output buffer.
fn admit(value: &impl Serialize, limit: usize) -> Result<usize, ReadError> {
    struct Counter {
        written: usize,
        limit: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.written = self
                .written
                .checked_add(bytes.len())
                .filter(|n| *n <= self.limit)
                .ok_or_else(|| io::Error::other("bounded JSON"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { written: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| ReadError::Limit)?;
    Ok(counter.written)
}
fn retained(error: PinnedReadError) -> ReadError {
    match error {
        PinnedReadError::Unavailable => ReadError::EvidenceUnavailable,
        PinnedReadError::Invalid => ReadError::EvidenceInvalid,
        PinnedReadError::Operational => ReadError::Storage,
    }
}
/// The scope label of a role's evidence: a declaration's retained primary
/// source, or a captured observation input (ADR-0079).
fn scope_of(role: BindingRole) -> &'static str {
    match role {
        BindingRole::Primary => "primary_source_only",
        BindingRole::Observed => "observed_input_only",
    }
}
fn selected(
    state: &AppState,
    proof: &ScopedEvidenceProof,
    role: BindingRole,
) -> Result<FactSourceSelection, ReadError> {
    let selected = state
        .graph
        .lock()
        .map_err(|_| ReadError::Storage)?
        .read_selected_facts_at_revision_as(
            std::slice::from_ref(proof.fact()),
            proof.revision(),
            proof.graph_limits(),
            role,
        )
        .map_err(|error| match error {
            core_graph::GraphError::Generation(core_graph::GenerationError::Changed) => {
                ReadError::StaleBasis
            }
            other => super::graph_error(other),
        })?;
    selected
        .selections
        .into_iter()
        .next()
        .ok_or(ReadError::EvidenceInvalid)
}
fn binding(
    selection: FactSourceSelection,
    proof: &ScopedEvidenceProof,
) -> Result<SourceBinding, ReadError> {
    match selection.state {
        FactSourceState::Present {
            fact_digest,
            binding,
        } if binding.fact == *proof.fact() && binding.emitted_fact_digest == fact_digest => {
            Ok(binding)
        }
        FactSourceState::Invalid { .. } => Err(ReadError::EvidenceInvalid),
        FactSourceState::Missing | FactSourceState::Absent { .. } => {
            Err(ReadError::EvidenceUnavailable)
        }
        _ => Err(ReadError::EvidenceInvalid),
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ScopedEvidenceDescription {
    pub basis_id: String,
    pub target: ScopedEvidenceTarget,
    pub total_ranges: usize,
    pub next_ordinal: Option<usize>,
    pub scope: &'static str,
    pub input_closure: &'static str,
}
#[derive(Debug, Serialize)]
pub(crate) struct ScopedEvidenceSpan {
    pub basis_id: String,
    pub target: ScopedEvidenceTarget,
    pub scope: &'static str,
    pub input_closure: &'static str,
    /// #1754: the text is redacted before it leaves the host.
    pub redaction: super::evidence::EvidenceRedaction,
    pub text: String,
}

fn descriptor(
    proof: &ScopedEvidenceProof,
    receipt: &Receipt,
    ordinal: usize,
) -> Result<ScopedEvidenceTarget, ReadError> {
    let range = receipt
        .ranges()
        .get(ordinal)
        .ok_or(ReadError::InvalidRequest)?;
    let role = crate::investigations::inspection::receipt_range_role(range.role);
    #[derive(Serialize)]
    struct View<'a> {
        schema_version: u32,
        target_id: &'a str,
        basis_id: &'a str,
        revision: &'a core_graph::StoreRevision,
        fact: &'a core_graph::source::FactKey,
        fact_digest: &'a str,
        registered_source_id: &'a str,
        receipt_id: &'a str,
        receipt_inventory_index: u32,
        role: agents::TaskRangeRole,
        index: u32,
        source: &'a core_prov::EvidenceRef,
        captured: &'a source_capture::CaptureSpanRef,
    }
    // Account for the final fixed-size target ID before copying any receipt
    // strings. CaptureFileRef and TaskCaptureFileRef have identical wire fields.
    let placeholder = format!("mcp-scoped-evidence-target-v1:{}", "0".repeat(64));
    let ordinal = u32::try_from(ordinal).map_err(|_| ReadError::InvalidRequest)?;
    admit(
        &View {
            schema_version: 1,
            target_id: &placeholder,
            basis_id: proof.basis_id(),
            revision: proof.revision(),
            fact: receipt.fact_key(),
            fact_digest: receipt.fact_digest(),
            registered_source_id: receipt.source_id().as_str(),
            receipt_id: receipt.id(),
            receipt_inventory_index: ordinal,
            role,
            index: range.index,
            source: &range.evidence,
            captured: &range.captured,
        },
        target::MAX_TARGET_BYTES,
    )?;
    let file = &range.captured.file;
    let mut target = ScopedEvidenceTarget {
        schema_version: 1,
        target_id: String::new(),
        basis_id: proof.basis_id().into(),
        revision: proof.revision().clone(),
        fact: crate::task_evidence::task_fact(receipt.fact_key()),
        fact_digest: receipt.fact_digest().into(),
        registered_source_id: receipt.source_id().as_str().into(),
        receipt_id: receipt.id().into(),
        receipt_inventory_index: ordinal,
        role,
        index: range.index,
        source: range.evidence.clone(),
        captured: TaskCaptureSpanRef {
            file: TaskCaptureFileRef {
                source_id: file.source_id.as_str().into(),
                capture_id: file.capture_id.clone(),
                path: file.path.clone(),
                digest: file.digest.clone(),
                byte_len: file.byte_len,
            },
            byte_start: range.captured.byte_start,
            byte_end: range.captured.byte_end,
        },
    };
    target.target_id = target.content_id()?;
    target.validate()?;
    Ok(target)
}

/// Selected identity and source registration are rechecked around the actual
/// retention lock. The callback brackets external policy/config observations;
/// neither it nor the graph generation claims a cross-store transaction.
fn resolve(
    state: &AppState,
    proof: &ScopedEvidenceProof,
    role: BindingRole,
    expected_target: Option<&ScopedEvidenceTarget>,
    ordinal: usize,
    revalidate: impl FnOnce() -> Result<(), ReadError>,
) -> Result<(ScopedEvidenceTarget, RetentionGuard, Receipt), ReadError> {
    if ordinal >= 1024 {
        return Err(ReadError::InvalidRequest);
    }
    let selection = selected(state, proof, role)?;
    if matches!(selection.state, FactSourceState::Absent { .. }) {
        if expected_target.is_some() {
            return Err(ReadError::StaleBasis);
        }
        if let OmissionLookup::Omitted { repo_key, kind } = &selection.omission {
            let source = state
                .sources
                .lock()
                .map_err(|_| ReadError::Storage)?
                .get_by_repo(repo_key)
                .map_err(|_| ReadError::Storage)?
                .ok_or(ReadError::EvidenceUnavailable)?;
            let _guard = state
                .primary_sources
                .task_guard(&source.source_id)
                .map_err(retained)?;
            let checked = selected(state, proof, role)?;
            if checked != selection {
                return Err(ReadError::StaleBasis);
            }
            let after = state
                .sources
                .lock()
                .map_err(|_| ReadError::Storage)?
                .get_by_repo(repo_key)
                .map_err(|_| ReadError::Storage)?
                .ok_or(ReadError::StaleBasis)?;
            if source.source_id != after.source_id
                || source.repo_key != after.repo_key
                || source.root() != after.root()
                || source.is_managed() != after.is_managed()
                || source.clone_url() != after.clone_url()
            {
                return Err(ReadError::StaleBasis);
            }
            revalidate()?;
            return Err(ReadError::EvidenceOmitted(*kind));
        }
    }
    let preliminary = binding(selection, proof)?;
    if expected_target.is_some_and(|target| target.receipt_id != preliminary.receipt_id) {
        return Err(ReadError::StaleBasis);
    }
    let source = state
        .sources
        .lock()
        .map_err(|_| ReadError::Storage)?
        .get_by_repo(&preliminary.repo_key)
        .map_err(|_| ReadError::Storage)?
        .ok_or(ReadError::EvidenceUnavailable)?;
    let guard = state
        .primary_sources
        .task_guard(&source.source_id)
        .map_err(retained)?;
    let checked = binding(selected(state, proof, role)?, proof)?;
    if checked != preliminary {
        return Err(ReadError::StaleBasis);
    }
    let after = state
        .sources
        .lock()
        .map_err(|_| ReadError::Storage)?
        .get_by_repo(&checked.repo_key)
        .map_err(|_| ReadError::Storage)?
        .ok_or(ReadError::StaleBasis)?;
    if source.source_id != after.source_id
        || source.repo_key != after.repo_key
        || source.root() != after.root()
        || source.is_managed() != after.is_managed()
        || source.clone_url() != after.clone_url()
    {
        return Err(ReadError::StaleBasis);
    }
    revalidate()?;
    let owner = match role {
        BindingRole::Primary => source.source_id.clone(),
        BindingRole::Observed => crate::observation_inputs::input_source_id(&source.source_id),
    };
    if expected_target.is_some_and(|target| {
        target.fact_digest != checked.emitted_fact_digest || target.registered_source_id != owner
    }) {
        return Err(ReadError::StaleBasis);
    }
    let unavailable = |receipt: Option<&Receipt>| -> Result<ReadError, ReadError> {
        if state
            .primary_sources
            .task_binding_forgotten_as(&guard, role, &checked, receipt)
            .map_err(retained)?
        {
            Ok(ReadError::EvidenceForgotten)
        } else {
            Ok(ReadError::EvidenceUnavailable)
        }
    };
    let receipt = match state.primary_sources.task_receipt_as(
        &guard,
        role,
        &checked.fact,
        &checked.emitted_fact_digest,
        &checked.repo_key,
        &checked.receipt_id,
    ) {
        Ok(receipt) => receipt,
        Err(PinnedReadError::Unavailable) => return Err(unavailable(None)?),
        Err(error) => return Err(retained(error)),
    };
    let target = descriptor(proof, &receipt, ordinal)?;
    match state
        .primary_sources
        .task_capture_present(&guard, &target.captured)
    {
        Ok(()) => {}
        Err(PinnedReadError::Unavailable) => return Err(unavailable(Some(&receipt))?),
        Err(error) => return Err(retained(error)),
    }
    Ok((target, guard, receipt))
}

pub(crate) fn describe(
    state: &AppState,
    proof: &ScopedEvidenceProof,
    role: BindingRole,
    ordinal: usize,
    max_response_bytes: usize,
    revalidate: impl FnOnce() -> Result<(), ReadError>,
) -> Result<ScopedEvidenceDescription, ReadError> {
    if max_response_bytes == 0 {
        return Err(ReadError::Limit);
    }
    let (target, _guard, receipt) = resolve(state, proof, role, None, ordinal, revalidate)?;
    let total_ranges = receipt.ranges().len();
    let response = ScopedEvidenceDescription {
        basis_id: proof.basis_id().into(),
        target,
        total_ranges,
        next_ordinal: ordinal.checked_add(1).filter(|next| *next < total_ranges),
        scope: scope_of(role),
        input_closure: "input_closure_not_established",
    };
    admit(&response, max_response_bytes)?;
    Ok(response)
}

pub(crate) fn read(
    state: &AppState,
    proof: &ScopedEvidenceProof,
    target: &ScopedEvidenceTarget,
    limits: EvidenceLimits,
    revalidate: impl FnOnce() -> Result<(), ReadError>,
) -> Result<ScopedEvidenceSpan, ReadError> {
    target.validate()?;
    let role = target.binding_role();
    if target.basis_id != proof.basis_id()
        || &target.revision != proof.revision()
        || crate::task_evidence::graph_fact(&target.fact) != *proof.fact()
    {
        return Err(ReadError::StaleBasis);
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
    #[derive(Serialize)]
    struct View<'a> {
        basis_id: &'a str,
        target: &'a ScopedEvidenceTarget,
        scope: &'static str,
        input_closure: &'static str,
        redaction: super::evidence::EvidenceRedaction,
        text: &'static str,
    }
    let envelope = admit(
        &View {
            basis_id: proof.basis_id(),
            target,
            scope: scope_of(role),
            input_closure: "input_closure_not_established",
            redaction: super::evidence::EvidenceRedaction::UNREAD,
            text: "",
        },
        limits.max_response_bytes,
    )?;
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
    let (actual, guard, receipt) = resolve(
        state,
        proof,
        role,
        Some(target),
        target.receipt_inventory_index as usize,
        revalidate,
    )?;
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
        .map_err(retained)?;
    // #1754: an MCP client never receives the raw span. Redaction can
    // lengthen the text, so the size is admitted on what is sent.
    let (text, redaction) = super::evidence::EvidenceRedaction::apply(&raw);
    drop(raw);
    let response = ScopedEvidenceSpan {
        basis_id: proof.basis_id().into(),
        target: actual,
        scope: scope_of(role),
        input_closure: "input_closure_not_established",
        redaction,
        text,
    };
    admit(&response, limits.max_response_bytes)?;
    Ok(response)
}

#[cfg(test)]
#[path = "scoped_evidence/tests.rs"]
mod tests;
