//! Exact caller targets and human-only captured locations. No model tool here.
use crate::{AppState, primary_source::RetentionGuard};
use adapters_lang_ts::captured::Receipt;
use agents::investigation::*;
use agents::{TaskCaptureFileRef, TaskCaptureSpanRef, TaskFactKey};
use context_hub::ContextSnapshot;
use core_graph::{
    SnapshotReadLimits,
    source::{FactSourceState, SourceBinding},
};
use serde::Serialize;
use tauri::Manager;

const REFRESH: &str =
    "Captured inspection target changed or is unavailable; refresh and select the source again.";
/// Source-free evidence failures; absence does not establish why content is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InspectionError {
    Stale,
    Unavailable,
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
    snapshot_typed(state, fact).map_err(|_| REFRESH.into())
}
fn snapshot_typed(
    state: &AppState,
    fact: &TaskFactKey,
) -> Result<(String, SourceBinding), InspectionError> {
    let snapshot = state
        .graph
        .lock()
        .map_err(|_| InspectionError::Operational)?
        .read_source_selection_snapshot_bounded(
            &[crate::task_evidence::graph_fact(fact)],
            SnapshotReadLimits::default(),
        )
        .map_err(|_| InspectionError::Operational)?;
    let binding = match snapshot.selections.into_iter().next().map(|s| s.state) {
        Some(FactSourceState::Present { binding, .. }) => binding,
        _ => return Err(InspectionError::Stale),
    };
    let context = ContextSnapshot::new(snapshot.graph.0, snapshot.graph.1)
        .map_err(|_| InspectionError::Operational)?;
    Ok((context.id().into(), binding))
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
    resolve_typed(state, fact, expected_snapshot, expected_receipt, ordinal)
        .map_err(|_| REFRESH.into())
}
pub(crate) fn resolve_typed(
    state: &AppState,
    fact: &TaskFactKey,
    expected_snapshot: &str,
    expected_receipt: &str,
    ordinal: usize,
) -> Result<(InvestigationInspectionTarget, RetentionGuard), InspectionError> {
    if ordinal >= 1024 {
        return Err(InspectionError::Stale);
    }
    let (snapshot_id, preliminary) = snapshot_typed(state, fact)?;
    if snapshot_id != expected_snapshot || preliminary.receipt_id != expected_receipt {
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
    let (checked_id, binding) = snapshot_typed(state, fact)?;
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
    let receipt = state
        .primary_sources
        .task_receipt(
            &guard,
            &binding.fact,
            &binding.emitted_fact_digest,
            &binding.repo_key,
            &binding.receipt_id,
        )
        .map_err(InspectionError::from)?;
    let target = descriptor(&receipt, &snapshot_id, ordinal).map_err(|_| InspectionError::Stale)?;
    state
        .primary_sources
        .task_capture_present(&guard, &target.captured)
        .map_err(InspectionError::from)?;
    Ok((target, guard))
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
