//! Exact caller targets and human-only captured locations. No model tool here.
use crate::{AppState, primary_source::RetentionGuard};
use adapters_lang_ts::captured::Receipt;
use agents::investigation::*;
use agents::{TaskCaptureFileRef, TaskCaptureSpanRef, TaskFactKey};
use context_hub::ContextSnapshot;
use core_graph::{
    SnapshotReadLimits,
    source::{
        FactSourceSelection, FactSourceState, OmissionLookup, ReceiptOmissionKind, SourceBinding,
    },
};
use serde::Serialize;
use tauri::Manager;

/// Share the receipt role vocabulary with bounded read-only consumers.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn receipt_range_role(
    value: adapters_lang_ts::captured::RangeRole,
) -> agents::TaskRangeRole {
    super::evidence::role(value)
}

const REFRESH: &str =
    "Captured inspection target changed or is unavailable; refresh and select the source again.";
/// Source-free evidence failures; absence does not establish why content is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InspectionError {
    Stale,
    OrdinalOutOfRange,
    Limit,
    Unavailable,
    Forgotten,
    Omitted(ReceiptOmissionKind),
    Invalid,
    Operational,
}
impl From<crate::primary_source::PinnedReadError> for InspectionError {
    fn from(error: crate::primary_source::PinnedReadError) -> Self {
        use crate::primary_source::PinnedReadError;
        match error {
            PinnedReadError::Unavailable => Self::Unavailable,
            PinnedReadError::Invalid => Self::Invalid,
            PinnedReadError::Operational => Self::Operational,
        }
    }
}
fn snapshot(state: &AppState, fact: &TaskFactKey) -> Result<(String, SourceBinding), String> {
    snapshot_typed(state, fact, SnapshotReadLimits::investigation()).map_err(|_| REFRESH.into())
}
fn snapshot_typed(
    state: &AppState,
    fact: &TaskFactKey,
    limits: SnapshotReadLimits,
) -> Result<(String, SourceBinding), InspectionError> {
    snapshot_typed_at(state, fact, limits, None, false)
}

/// Only MCP distinguishes a current unbound fact from a changed selection.
/// Expected receipt targets still require their original association.
fn snapshot_typed_at(
    state: &AppState,
    fact: &TaskFactKey,
    limits: SnapshotReadLimits,
    expected_snapshot: Option<&str>,
    allow_unbound: bool,
) -> Result<(String, SourceBinding), InspectionError> {
    let (context_id, revision, selection) = selection_snapshot(state, fact, limits)?;
    if expected_snapshot.is_some_and(|expected| expected != context_id) {
        return Err(InspectionError::Stale);
    }
    let binding = match selection.state {
        FactSourceState::Present { binding, .. } => binding,
        FactSourceState::Absent { fact_digest } if allow_unbound => {
            if let OmissionLookup::Omitted { repo_key, kind } = selection.omission {
                return Err(checked_omission(
                    state,
                    fact,
                    limits,
                    &context_id,
                    &revision,
                    &fact_digest,
                    &repo_key,
                    kind,
                )?);
            }
            return Err(InspectionError::Unavailable);
        }
        FactSourceState::Invalid { .. } if expected_snapshot.is_some() => {
            return Err(InspectionError::Invalid);
        }
        _ => return Err(InspectionError::Stale),
    };
    Ok((context_id, binding))
}

fn selection_snapshot(
    state: &AppState,
    fact: &TaskFactKey,
    limits: SnapshotReadLimits,
) -> Result<(String, core_graph::StoreRevision, FactSourceSelection), InspectionError> {
    let snapshot = state
        .graph
        .lock()
        .map_err(|_| InspectionError::Operational)?
        .read_source_selection_snapshot_bounded(&[crate::task_evidence::graph_fact(fact)], limits)
        .map_err(|error| match error {
            core_graph::GraphError::SnapshotBounds(_) => InspectionError::Limit,
            _ => InspectionError::Operational,
        })?;
    let context = ContextSnapshot::new(snapshot.graph.0, snapshot.graph.1)
        .map_err(|_| InspectionError::Operational)?;
    let id = context.id().to_owned();
    // Release graph/projection owners before a guarded second observation.
    drop(context);
    let selection = snapshot
        .selections
        .into_iter()
        .next()
        .ok_or(InspectionError::Operational)?;
    Ok((id, snapshot.revision, selection))
}

#[allow(clippy::too_many_arguments)]
fn checked_omission(
    state: &AppState,
    fact: &TaskFactKey,
    limits: SnapshotReadLimits,
    expected: &str,
    revision: &core_graph::StoreRevision,
    digest: &str,
    repo: &str,
    kind: ReceiptOmissionKind,
) -> Result<InspectionError, InspectionError> {
    let source = state
        .sources
        .lock()
        .map_err(|_| InspectionError::Operational)?
        .get_by_repo(repo)
        .map_err(|_| InspectionError::Operational)?
        .ok_or(InspectionError::Unavailable)?;
    let _guard = state
        .primary_sources
        .task_guard(&source.source_id)
        .map_err(InspectionError::from)?;
    let (checked_id, checked_revision, checked) = selection_snapshot(state, fact, limits)?;
    if checked_id != expected
        || &checked_revision != revision
        || checked.state
            != (FactSourceState::Absent {
                fact_digest: digest.into(),
            })
        || checked.omission
            != (OmissionLookup::Omitted {
                repo_key: repo.into(),
                kind,
            })
    {
        return Err(InspectionError::Stale);
    }
    let after = state
        .sources
        .lock()
        .map_err(|_| InspectionError::Operational)?
        .get_by_repo(repo)
        .map_err(|_| InspectionError::Operational)?
        .ok_or(InspectionError::Stale)?;
    if source.source_id != after.source_id
        || source.repo_key != after.repo_key
        || source.root() != after.root()
        || source.is_managed() != after.is_managed()
        || source.clone_url() != after.clone_url()
    {
        return Err(InspectionError::Stale);
    }
    Ok(InspectionError::Omitted(kind))
}

pub(crate) fn graph_snapshot_for_receipt(
    state: &AppState,
    receipt: &Receipt,
) -> Result<String, String> {
    let fact = crate::task_evidence::task_fact(receipt.fact_key());
    let (snapshot, binding) = snapshot(state, &fact)?;
    if binding.receipt_id != receipt.id()
        || binding.emitted_fact_digest != receipt.fact_digest()
        || binding.repo_key != receipt.repo_key()
    {
        return Err(REFRESH.into());
    }
    Ok(snapshot)
}
pub(super) fn descriptor(
    receipt: &Receipt,
    snapshot: &str,
    ordinal: usize,
) -> Result<InvestigationInspectionTarget, String> {
    let range = receipt.ranges().get(ordinal).ok_or(REFRESH)?;
    let mut target = InvestigationInspectionTarget {
        schema_version: 1,
        target_id: String::new(),
        graph_snapshot_id: snapshot.into(),
        fact: crate::task_evidence::task_fact(receipt.fact_key()),
        fact_digest: receipt.fact_digest().into(),
        registered_source_id: receipt.source_id().as_str().into(),
        receipt_id: receipt.id().into(),
        receipt_inventory_index: u32::try_from(ordinal).map_err(|_| REFRESH)?,
        role: super::evidence::role(range.role),
        index: range.index,
        source: range.evidence.clone(),
        captured: TaskCaptureSpanRef {
            file: TaskCaptureFileRef {
                source_id: range.captured.file.source_id.as_str().into(),
                capture_id: range.captured.file.capture_id.clone(),
                path: range.captured.file.path.clone(),
                digest: range.captured.file.digest.clone(),
                byte_len: range.captured.file.byte_len,
            },
            byte_start: range.captured.byte_start,
            byte_end: range.captured.byte_end,
        },
    };
    target.target_id = target.content_id().map_err(|_| REFRESH)?;
    target.validate().map_err(|_| REFRESH)?;
    Ok(target)
}
pub(crate) fn resolve(
    state: &AppState,
    fact: &TaskFactKey,
    expected_snapshot: &str,
    expected_receipt: &str,
    ordinal: usize,
) -> Result<(InvestigationInspectionTarget, RetentionGuard), String> {
    resolve_with_limits(
        state,
        fact,
        expected_snapshot,
        expected_receipt,
        ordinal,
        SnapshotReadLimits::investigation(),
    )
}

/// [`resolve`] under an explicit complete-graph ceiling: an MCP-started task
/// resolves under the limits its reservation was computed from (#1602).
pub(crate) fn resolve_with_limits(
    state: &AppState,
    fact: &TaskFactKey,
    expected_snapshot: &str,
    expected_receipt: &str,
    ordinal: usize,
    limits: SnapshotReadLimits,
) -> Result<(InvestigationInspectionTarget, RetentionGuard), String> {
    resolve_typed_bounded(
        state,
        fact,
        expected_snapshot,
        expected_receipt,
        ordinal,
        limits,
    )
    .map_err(|_| REFRESH.into())
}
#[cfg(test)]
pub(crate) fn resolve_typed(
    state: &AppState,
    fact: &TaskFactKey,
    expected_snapshot: &str,
    expected_receipt: &str,
    ordinal: usize,
) -> Result<(InvestigationInspectionTarget, RetentionGuard), InspectionError> {
    resolve_typed_bounded(
        state,
        fact,
        expected_snapshot,
        expected_receipt,
        ordinal,
        SnapshotReadLimits::investigation(),
    )
}

pub(crate) fn resolve_typed_bounded(
    state: &AppState,
    fact: &TaskFactKey,
    expected_snapshot: &str,
    expected_receipt: &str,
    ordinal: usize,
    limits: SnapshotReadLimits,
) -> Result<(InvestigationInspectionTarget, RetentionGuard), InspectionError> {
    resolve_with_receipt_typed_bounded(
        state,
        fact,
        expected_snapshot,
        Some(expected_receipt),
        ordinal,
        limits,
    )
    .map(|(target, guard, _)| (target, guard))
}

/// Resolve a current receipt through two bounded graph checks around its guard.
/// Describe omits a prior receipt selection; read supplies its exact receipt.
/// Returning the already validated receipt avoids an additional database decode.
pub(crate) fn resolve_with_receipt_typed_bounded(
    state: &AppState,
    fact: &TaskFactKey,
    expected_snapshot: &str,
    expected_receipt: Option<&str>,
    ordinal: usize,
    limits: SnapshotReadLimits,
) -> Result<(InvestigationInspectionTarget, RetentionGuard, Receipt), InspectionError> {
    resolve_receipt_inner(
        state,
        fact,
        expected_snapshot,
        expected_receipt,
        None,
        ordinal,
        limits,
        false,
    )
}

/// MCP-only positive forgotten classification. It identifies retention of the
/// exact checked binding; it does not validate old range fields after reclaim.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn resolve_mcp_with_receipt_typed_bounded(
    state: &AppState,
    fact: &TaskFactKey,
    expected_snapshot: &str,
    expected_target: Option<&InvestigationInspectionTarget>,
    ordinal: usize,
    limits: SnapshotReadLimits,
) -> Result<(InvestigationInspectionTarget, RetentionGuard, Receipt), InspectionError> {
    resolve_receipt_inner(
        state,
        fact,
        expected_snapshot,
        expected_target.map(|t| t.receipt_id.as_str()),
        expected_target,
        ordinal,
        limits,
        true,
    )
}
#[allow(clippy::too_many_arguments)]
fn resolve_receipt_inner(
    state: &AppState,
    fact: &TaskFactKey,
    expected_snapshot: &str,
    expected_receipt: Option<&str>,
    expected_target: Option<&InvestigationInspectionTarget>,
    ordinal: usize,
    limits: SnapshotReadLimits,
    classify_forgotten: bool,
) -> Result<(InvestigationInspectionTarget, RetentionGuard, Receipt), InspectionError> {
    if ordinal >= 1024 {
        return Err(InspectionError::OrdinalOutOfRange);
    }
    let (snapshot_id, preliminary) = if classify_forgotten {
        snapshot_typed_at(
            state,
            fact,
            limits,
            Some(expected_snapshot),
            expected_receipt.is_none(),
        )?
    } else {
        snapshot_typed(state, fact, limits)?
    };
    if snapshot_id != expected_snapshot
        || expected_receipt.is_some_and(|receipt| preliminary.receipt_id != receipt)
    {
        return Err(InspectionError::Stale);
    }
    let source = state
        .sources
        .lock()
        .map_err(|_| InspectionError::Operational)?
        .get_by_repo(&preliminary.repo_key)
        .map_err(|_| InspectionError::Operational)?
        .ok_or(InspectionError::Stale)?;
    let guard = state
        .primary_sources
        .task_guard(&source.source_id)
        .map_err(InspectionError::from)?;
    let (checked_id, binding) = snapshot_typed(state, fact, limits)?;
    if checked_id != snapshot_id || binding != preliminary {
        return Err(InspectionError::Stale);
    }
    let checked_source = state
        .sources
        .lock()
        .map_err(|_| InspectionError::Operational)?
        .get_by_repo(&binding.repo_key)
        .map_err(|_| InspectionError::Operational)?
        .ok_or(InspectionError::Stale)?;
    if checked_source.source_id != source.source_id
        || checked_source.repo_key != source.repo_key
        || checked_source.root() != source.root()
    {
        return Err(InspectionError::Stale);
    }
    if expected_target.is_some_and(|target| {
        target.fact_digest != binding.emitted_fact_digest
            || target.registered_source_id != source.source_id
    }) {
        return Err(InspectionError::Stale);
    }
    let unavailable = |receipt: Option<&Receipt>| -> Result<InspectionError, InspectionError> {
        if classify_forgotten
            && state
                .primary_sources
                .task_binding_forgotten(&guard, &binding, receipt)
                .map_err(InspectionError::from)?
        {
            Ok(InspectionError::Forgotten)
        } else {
            Ok(InspectionError::Unavailable)
        }
    };
    let receipt = match state.primary_sources.task_receipt(
        &guard,
        &binding.fact,
        &binding.emitted_fact_digest,
        &binding.repo_key,
        &binding.receipt_id,
    ) {
        Ok(receipt) => receipt,
        Err(crate::primary_source::PinnedReadError::Unavailable) => return Err(unavailable(None)?),
        Err(error) => return Err(error.into()),
    };
    // A caller-selected range outside this receipt cannot become valid by
    // reconnecting to the same graph; reserve Stale for changed identities.
    if ordinal >= receipt.ranges().len() {
        return Err(InspectionError::OrdinalOutOfRange);
    }
    let target = descriptor(&receipt, &snapshot_id, ordinal).map_err(|_| InspectionError::Stale)?;
    match state
        .primary_sources
        .task_capture_present(&guard, &target.captured)
    {
        Ok(()) => {}
        Err(crate::primary_source::PinnedReadError::Unavailable) => {
            return Err(unavailable(Some(&receipt))?);
        }
        Err(error) => return Err(error.into()),
    }
    Ok((target, guard, receipt))
}
pub(crate) fn validate_targets(
    state: &AppState,
    request: &StartInvestigationRequest,
) -> Result<Vec<RetentionGuard>, String> {
    let mut guards = Vec::new();
    for target in request.inspection_requirements.as_deref().unwrap_or(&[]) {
        let (actual, guard) = resolve(
            state,
            &target.fact,
            &target.graph_snapshot_id,
            &target.receipt_id,
            target.receipt_inventory_index as usize,
        )?;
        if actual != *target {
            return Err(REFRESH.into());
        }
        guards.push(guard);
    }
    Ok(guards)
}

#[tauri::command]
pub(crate) async fn resolve_investigation_inspection_target(
    fact: TaskFactKey,
    expected_graph_snapshot_id: String,
    expected_receipt_id: String,
    inventory_index: usize,
    app: tauri::AppHandle,
) -> Result<InvestigationInspectionTarget, String> {
    crate::off_ui_thread(move || {
        resolve(
            &app.state::<AppState>(),
            &fact,
            &expected_graph_snapshot_id,
            &expected_receipt_id,
            inventory_index,
        )
        .map(|(target, _)| target)
    })
    .await
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectionCapturedLocation {
    pub target_id: String,
    pub captured: TaskCaptureSpanRef,
    pub byte_start: u64,
    pub byte_end: u64,
    pub start_line: u64,
    pub start_column: u64,
    pub end_line: u64,
    pub end_column: u64,
    pub column_unit: &'static str,
    pub end_exclusive: bool,
}
pub(crate) fn location(
    state: &AppState,
    target: &InvestigationInspectionTarget,
) -> Result<InspectionCapturedLocation, String> {
    target.validate().map_err(|_| REFRESH)?;
    let guard = state
        .primary_sources
        .task_guard(&target.registered_source_id)
        .map_err(|_| REFRESH)?;
    let receipt = state
        .primary_sources
        .task_receipt(
            &guard,
            &crate::task_evidence::graph_fact(&target.fact),
            &target.fact_digest,
            // Receipt validation requires every range evidence repo to equal
            // its retained repo_key; no current graph/registration is needed.
            &target.source.repo,
            &target.receipt_id,
        )
        .map_err(|_| REFRESH)?;
    if descriptor(
        &receipt,
        &target.graph_snapshot_id,
        target.receipt_inventory_index as usize,
    )? != *target
    {
        return Err(REFRESH.into());
    }
    let at = state
        .primary_sources
        .task_location(&guard, &receipt, target.receipt_inventory_index as usize)
        .map_err(|_| REFRESH)?;
    Ok(InspectionCapturedLocation {
        target_id: target.target_id.clone(),
        captured: target.captured.clone(),
        byte_start: at.byte_start,
        byte_end: at.byte_end,
        start_line: at.start_line,
        start_column: at.start_column,
        end_line: at.end_line,
        end_column: at.end_column,
        column_unit: "utf8_bytes",
        end_exclusive: true,
    })
}
#[tauri::command]
pub(crate) async fn inspection_captured_location(
    target: InvestigationInspectionTarget,
    app: tauri::AppHandle,
) -> Result<InspectionCapturedLocation, String> {
    crate::off_ui_thread(move || location(&app.state::<AppState>(), &target)).await
}
