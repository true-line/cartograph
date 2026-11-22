//! Commands over retained primary-source evidence. The stores live in
//! app-store's `primary_source` (#1924).

use adapters_lang_ts::captured::Receipt;
use core_graph::source::FactKey;
use serde::Serialize;
use tauri::Manager;

pub(crate) use app_store::primary_source::*;

use crate::AppState;
use crate::sources::RegisteredSource;

fn bound_receipt(
    state: &AppState,
    fact: &FactKey,
    expected: Option<&str>,
) -> Result<(Receipt, RetentionGuard), String> {
    bound_receipt_as(state, fact, expected, core_graph::BindingRole::Primary)
        .map(|(receipt, guard, _)| (receipt, guard))
}

/// The receipt a fact's current binding in `role` cites. A primary binding
/// admits only the registered source's own receipts; an observed binding
/// admits only that source's input identity (ADR-0079).
fn bound_receipt_as(
    state: &AppState,
    fact: &FactKey,
    expected: Option<&str>,
    role: core_graph::BindingRole,
) -> Result<(Receipt, RetentionGuard, core_graph::source::SourceBinding), String> {
    let current = |graph: &core_graph::SqliteGraphStore| match role {
        core_graph::BindingRole::Primary => graph.current_source_binding(fact),
        core_graph::BindingRole::Observed => graph.current_source_observation(fact),
    };
    // Preliminary metadata locates the guard; release all mutexes before the
    // try-only OS lock, then make the authoritative current-binding check.
    let preliminary = current(&*state.graph.lock().map_err(storage_error)?)
        .map_err(storage_error)?
        .ok_or_else(unavailable)?;
    let source = state
        .sources
        .lock()
        .map_err(storage_error)?
        .get_by_repo(&preliminary.repo_key)?
        .ok_or_else(unavailable)?;
    let guard = state.primary_sources.guard(&source.source_id, false)?;
    let binding = current(&*state.graph.lock().map_err(storage_error)?)
        .map_err(storage_error)?
        .ok_or_else(unavailable)?;
    if binding.repo_key != source.repo_key {
        return Err(unavailable());
    }
    if expected.is_some_and(|id| id != binding.receipt_id) {
        return Err("Captured source selection is stale; select the fact again.".into());
    }
    let receipt = state
        .primary_sources
        .receipts
        .lock()
        .map_err(storage_error)?
        .get(&binding.receipt_id)?;
    let (owner, owner_repo) = match role {
        core_graph::BindingRole::Primary => (source.source_id.clone(), source.repo_key.clone()),
        core_graph::BindingRole::Observed => {
            let input = input_id(&source.source_id)?.as_str().to_owned();
            (input.clone(), input)
        }
    };
    if receipt.source_id().as_str() != owner
        || receipt.repo_key() != owner_repo
        || receipt.fact_key() != fact
        || receipt.fact_digest() != binding.emitted_fact_digest
    {
        return Err(unavailable());
    }
    Ok((receipt, guard, binding))
}

pub(crate) fn describe(state: &AppState, fact: &FactKey) -> Result<CapturedDescription, String> {
    let (receipt, _guard) = bound_receipt(state, fact, None)?;
    Ok(CapturedDescription {
        graph_snapshot_id: crate::investigations::inspection::graph_snapshot_for_receipt(
            state, &receipt,
        )?,
        fact: receipt.fact_key().clone(),
        receipt_id: receipt.id().to_owned(),
        emitted_fact_digest: receipt.fact_digest().to_owned(),
        source_id: receipt.source_id().as_str().to_owned(),
        repo_key: receipt.repo_key().to_owned(),
        ranges: receipt
            .ranges()
            .iter()
            .enumerate()
            .map(|(index, range)| CapturedRange {
                index,
                path: range.evidence.path.clone(),
                byte_start: range.evidence.byte_start,
                byte_end: range.evidence.byte_end,
            })
            .collect(),
        scope: "primary_source_only",
        input_closure: "input_closure_not_established",
    })
}

#[derive(Serialize)]
pub(crate) struct CapturedText {
    pub receipt_id: String,
    pub range_index: usize,
    pub text: String,
    pub path: String,
    pub byte_start: u64,
    pub byte_end: u64,
}

pub(crate) fn read(
    state: &AppState,
    fact: &FactKey,
    receipt_id: &str,
    range_index: usize,
) -> Result<CapturedText, String> {
    let (receipt, _guard) = bound_receipt(state, fact, Some(receipt_id))?;
    text_of(state, &receipt, range_index)
}

/// Read one cited range back from its retained capture; the caller holds the
/// source's retention guard.
fn text_of(
    state: &AppState,
    receipt: &Receipt,
    range_index: usize,
) -> Result<CapturedText, String> {
    let range = receipt.ranges().get(range_index).ok_or_else(unavailable)?;
    let text = state
        .primary_sources
        .captures
        .lock()
        .map_err(storage_error)?
        .read_text_span(&range.captured)
        .map_err(|_| unavailable())?;
    Ok(CapturedText {
        receipt_id: receipt.id().to_owned(),
        range_index,
        text,
        path: range.evidence.path.clone(),
        byte_start: range.evidence.byte_start,
        byte_end: range.evidence.byte_end,
    })
}

/// A fact's observed binding: the exact spans of a captured observation
/// input (Terraform state, Pulumi export or trace) that observed it. It is
/// shown and cited beside the declaration, never in its place (ADR-0079).
#[derive(Serialize)]
pub(crate) struct ObservedDescription {
    pub fact: FactKey,
    pub receipt_id: String,
    pub emitted_fact_digest: String,
    /// The input identity, `input/<registered source id>`.
    pub source_id: String,
    pub registered_source_id: String,
    /// The repo that owns the binding.
    pub repo_key: String,
    pub ranges: Vec<CapturedRange>,
    pub scope: &'static str,
    pub input_closure: &'static str,
}

pub(crate) fn describe_observed(
    state: &AppState,
    fact: &FactKey,
) -> Result<ObservedDescription, String> {
    let (receipt, _guard, binding) =
        bound_receipt_as(state, fact, None, core_graph::BindingRole::Observed)?;
    let registered_source_id = receipt
        .source_id()
        .as_str()
        .strip_prefix(crate::observation_inputs::INPUT_REPO_PREFIX)
        .ok_or_else(unavailable)?
        .to_owned();
    Ok(ObservedDescription {
        fact: receipt.fact_key().clone(),
        receipt_id: receipt.id().to_owned(),
        emitted_fact_digest: receipt.fact_digest().to_owned(),
        source_id: receipt.source_id().as_str().to_owned(),
        registered_source_id,
        repo_key: binding.repo_key,
        ranges: receipt
            .ranges()
            .iter()
            .enumerate()
            .map(|(index, range)| CapturedRange {
                index,
                path: range.evidence.path.clone(),
                byte_start: range.evidence.byte_start,
                byte_end: range.evidence.byte_end,
            })
            .collect(),
        scope: "observed_input_only",
        input_closure: "input_closure_not_established",
    })
}

pub(crate) fn read_observed(
    state: &AppState,
    fact: &FactKey,
    receipt_id: &str,
    range_index: usize,
) -> Result<CapturedText, String> {
    let (receipt, _guard, _) = bound_receipt_as(
        state,
        fact,
        Some(receipt_id),
        core_graph::BindingRole::Observed,
    )?;
    text_of(state, &receipt, range_index)
}

#[derive(Serialize)]
pub(crate) struct RetainedSource {
    pub source_id: String,
    pub repo_key: String,
    pub display_name: String,
}

#[derive(Serialize)]
pub(crate) struct RetentionPreview {
    pub source_id: String,
    pub repo_key: String,
    pub display_name: String,
    pub capture_ids: Vec<String>,
    /// Retained captures of the source's manifest observation inputs
    /// (ADR-0080), forgotten with it.
    pub input_capture_ids: Vec<String>,
    pub captures: usize,
    pub files: u64,
    pub bytes: u64,
    pub receipts: usize,
    /// Of `receipts`, the observed receipts its observation inputs own
    /// (ADR-0079), forgotten with it.
    pub input_receipts: usize,
    pub current_references: usize,
    pub historical_references: usize,
    /// Actual v2 evidence references in immutable staged proposals.
    pub staged_references: usize,
    /// Actual investigation/receipt uses, including selected present bindings
    /// whose original source text has not been requested.
    pub investigation_references: usize,
    /// Receipts of this source cited by saved work packets.
    pub packet_references: usize,
    pub fingerprint: String,
}

struct LockedPreview {
    preview: RetentionPreview,
    pins: std::collections::BTreeSet<String>,
    /// This source's receipts, and its inputs', the current graph binds.
    bound: std::collections::BTreeSet<String>,
}

fn preview_locked(state: &AppState, source: &RegisteredSource) -> Result<LockedPreview, String> {
    let inventory = state
        .primary_sources
        .captures
        .lock()
        .map_err(storage_error)?
        .source_inventory(&source_id(&source.source_id)?)
        .map_err(storage_error)?;
    let inputs = state
        .primary_sources
        .captures
        .lock()
        .map_err(storage_error)?
        .source_inventory(&input_id(&source.source_id)?)
        .map_err(storage_error)?;
    let (receipt_ids, input_receipt_ids) = {
        let store = state
            .primary_sources
            .receipts
            .lock()
            .map_err(storage_error)?;
        (
            store.ids(&source.source_id)?,
            store.ids(input_id(&source.source_id)?.as_str())?,
        )
    };
    let (current, observed) = {
        let graph = state.graph.lock().map_err(storage_error)?;
        (
            graph
                .source_bindings_for_repo(&source.repo_key)
                .map_err(storage_error)?,
            // The observed role binds the input's receipts (ADR-0079).
            graph
                .source_observations_for_repo(&source.repo_key)
                .map_err(storage_error)?,
        )
    };
    let current_count = current
        .iter()
        .filter(|binding| receipt_ids.binary_search(&binding.receipt_id).is_ok())
        .count()
        + observed
            .iter()
            .filter(|binding| input_receipt_ids.binary_search(&binding.receipt_id).is_ok())
            .count();
    // Bind the complete associations, not only their count: retained receipts
    // can become current again without changing the capture/receipt inventory.
    // This records the associations observed at preview/recheck; it does not
    // freeze independent graph mutations after the recheck.
    let mut current_bindings: Vec<_> = current
        .iter()
        .map(|binding| {
            (
                &binding.fact,
                &binding.repo_key,
                &binding.receipt_id,
                &binding.emitted_fact_digest,
            )
        })
        .collect();
    current_bindings.sort_unstable();
    let mut current_observations: Vec<_> = observed
        .iter()
        .map(|binding| {
            (
                &binding.fact,
                &binding.repo_key,
                &binding.receipt_id,
                &binding.emitted_fact_digest,
            )
        })
        .collect();
    current_observations.sort_unstable();
    let capture_ids: Vec<_> = inventory
        .iter()
        .map(|capture| capture.capture_id.clone())
        .collect();
    let input_capture_ids: Vec<_> = inputs
        .iter()
        .map(|capture| capture.capture_id.clone())
        .collect();
    let staged_references = state
        .proposals
        .lock()
        .map_err(storage_error)?
        .captured_receipt_references(&source.source_id)
        .map_err(storage_error)?;
    // The caller already holds this source's retention guard. Acquisition
    // publishes its ledger/index under the same shared guard, so an exclusive
    // forget cannot race a newly admitted reference. Hold no other store mutex
    // across this short validated JobStore read.
    let investigation_references = state
        .jobs
        .lock()
        .map_err(storage_error)?
        .investigation_receipt_references(&source.source_id)
        .map_err(storage_error)?;
    let packet_references: Vec<String> = state
        .work_packets
        .store
        .lock()
        .map_err(storage_error)?
        .referenced_receipts(
            &receipt_ids
                .iter()
                .chain(&input_receipt_ids)
                .cloned()
                .collect::<Vec<_>>(),
        )
        .map_err(storage_error)?
        .into_iter()
        .collect();
    let fingerprint = core_prov::content_hash(
        &serde_json::to_vec(&(
            "retained-source-preview-v6",
            &source.source_id,
            &source.repo_key,
            &capture_ids,
            &input_capture_ids,
            &receipt_ids,
            &input_receipt_ids,
            &current_bindings,
            &current_observations,
            &staged_references,
            &investigation_references,
            &packet_references,
        ))
        .map_err(storage_error)?,
    );
    let pins = staged_references
        .iter()
        .map(|reference| reference.receipt_id.clone())
        .chain(
            investigation_references
                .iter()
                .map(|reference| reference.receipt_id.clone()),
        )
        .chain(packet_references.iter().cloned())
        .collect();
    let bound = current
        .iter()
        .filter(|binding| receipt_ids.binary_search(&binding.receipt_id).is_ok())
        .chain(
            observed
                .iter()
                .filter(|binding| input_receipt_ids.binary_search(&binding.receipt_id).is_ok()),
        )
        .map(|binding| binding.receipt_id.clone())
        .collect();
    Ok(LockedPreview {
        preview: RetentionPreview {
            source_id: source.source_id.clone(),
            repo_key: source.repo_key.clone(),
            display_name: source.display_name.clone(),
            captures: inventory.len() + inputs.len(),
            files: inventory
                .iter()
                .chain(&inputs)
                .map(|capture| capture.file_count)
                .sum(),
            bytes: inventory
                .iter()
                .chain(&inputs)
                .map(|capture| capture.byte_len)
                .sum(),
            receipts: receipt_ids.len() + input_receipt_ids.len(),
            input_receipts: input_receipt_ids.len(),
            current_references: current_count,
            historical_references: (receipt_ids.len() + input_receipt_ids.len())
                .saturating_sub(current_count),
            staged_references: staged_references.len(),
            investigation_references: investigation_references.len(),
            packet_references: packet_references.len(),
            capture_ids,
            input_capture_ids,
            fingerprint,
        },
        pins,
        bound,
    })
}

fn registered(state: &AppState, id: &str) -> Result<RegisteredSource, String> {
    source_id(id)?;
    state
        .sources
        .lock()
        .map_err(storage_error)?
        .get_by_id(id)?
        .ok_or_else(unavailable)
}

pub(crate) fn preview(state: &AppState, id: &str) -> Result<RetentionPreview, String> {
    let source = registered(state, id)?;
    let _guard = state.primary_sources.guard(id, false)?;
    Ok(preview_locked(state, &source)?.preview)
}

pub(crate) fn forget(state: &AppState, id: &str, fingerprint: &str) -> Result<u64, String> {
    let source = registered(state, id)?;
    let _guard = state.primary_sources.guard(id, true)?;
    let locked = preview_locked(state, &source)?;
    if locked.preview.fingerprint != fingerprint {
        return Err("Retained source changed; refresh the preview before forgetting.".into());
    }
    // Serialize cache generation invalidation with preview snapshot and publication.
    // Rotate before capture deletion; a later failure leaves only safe cold misses.
    let graph_guard = state.graph.lock().map_err(storage_error)?;
    graph_guard
        .invalidate_derived_state()
        .map_err(storage_error)?;
    let forgotten = {
        let mut captures = state
            .primary_sources
            .captures
            .lock()
            .map_err(storage_error)?;
        // Inputs first: they carry no receipt, so a failure after them leaves
        // the source's own captures and receipts intact.
        let inputs = captures
            .forget_source(&input_id(id)?, &locked.preview.input_capture_ids)
            .map_err(storage_error)?;
        inputs
            + captures
                .forget_source(&source_id(id)?, &locked.preview.capture_ids)
                .map_err(storage_error)?
    };
    state
        .primary_sources
        .receipts
        .lock()
        .map_err(storage_error)?
        .reclaim_forgotten(&source, &locked.preview.capture_ids, &locked.pins)?;
    if locked.preview.input_receipts > 0 {
        let owner = source.owning_as(input_id(id)?.as_str().to_owned());
        state
            .primary_sources
            .receipts
            .lock()
            .map_err(storage_error)?
            .reclaim_forgotten(&owner, &locked.preview.input_capture_ids, &locked.pins)?;
    }
    Ok(forgotten)
}

/// Reclaim receipts of `source` that the just-published graph no longer binds
/// and nothing pins (ADR-0046 decision 5). The caller must have released its
/// prepared input first. The reclaim needs the exclusive retention guard, so
/// no other recovery of this source can be holding fresh, not yet published
/// receipts, and no task can prepare against it; a busy source is skipped
/// and settles on its next publish. Receipts a prepared task cites stay
/// pinned until it has staged ([`InFlightPin`]).
#[cfg(test)]
pub(crate) fn reclaim_superseded(
    state: &AppState,
    source: &RegisteredSource,
    capture_id: &str,
) -> Result<u64, String> {
    reclaim_superseded_with(state, source, Some(capture_id), None, || {})
}

/// [`reclaim_superseded`] for a recovery that captured `capture_id` for the
/// source's code and `input_capture_id` for its observation inputs, either of
/// which may be absent. Each owner's superseded receipts are tombstoned with
/// the capture that superseded them (ADR-0079).
pub(crate) fn reclaim_superseded_by(
    state: &AppState,
    source: &RegisteredSource,
    capture_id: Option<&str>,
    input_capture_id: Option<&str>,
) -> Result<u64, String> {
    reclaim_superseded_with(state, source, capture_id, input_capture_id, || {})
}

/// [`reclaim_superseded_by`], pausing between its two keep-set snapshots so
/// tests can stage a task in the handoff.
pub(crate) fn reclaim_superseded_with(
    state: &AppState,
    source: &RegisteredSource,
    capture_id: Option<&str>,
    input_capture_id: Option<&str>,
    between_snapshots: impl FnOnce(),
) -> Result<u64, String> {
    let Ok(_guard) = state.primary_sources.guard(&source.source_id, true) else {
        return Ok(0);
    };
    // In-flight pins first, durable pins second (#762 review). The
    // exclusive guard admits no new preparation, so a pin missing here was
    // dropped already, after its task staged: the durable snapshot below
    // then holds that staged pin. Reading them the other way round lets a
    // task stage after the durable read and drop before the in-flight one.
    let in_flight = state
        .primary_sources
        .in_flight
        .lock()
        .map_err(storage_error)?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    between_snapshots();
    let locked = preview_locked(state, source)?;
    let keep = locked
        .pins
        .union(&locked.bound)
        .cloned()
        .chain(in_flight)
        .collect();
    let mut store = state
        .primary_sources
        .receipts
        .lock()
        .map_err(storage_error)?;
    let mut reclaimed = 0;
    if let Some(capture_id) = capture_id {
        reclaimed += store.reclaim_superseded(source, &[capture_id.to_owned()], &keep)?;
    }
    // Observed receipts the published observed role no longer binds are
    // reclaimed with the input capture that superseded them (ADR-0079,
    // AC-1047).
    if let Some(input_capture_id) = input_capture_id
        && locked.preview.input_receipts > 0
    {
        let owner = source.owning_as(input_id(&source.source_id)?.as_str().to_owned());
        reclaimed += store.reclaim_superseded(&owner, &[input_capture_id.to_owned()], &keep)?;
    }
    Ok(reclaimed)
}

/// The complete digest of the fact the caller selected, which must be the
/// fact it names.
fn expected_digest(
    fact: &FactKey,
    expected_node: Option<core_graph::Node>,
    expected_edge: Option<core_graph::Edge>,
) -> Result<String, String> {
    match (fact, expected_node, expected_edge) {
        (FactKey::Node { id }, Some(node), None) if node.id == *id => {
            core_graph::source::node_digest(&node).map_err(storage_error)
        }
        (
            FactKey::Edge {
                source,
                label,
                destination,
            },
            None,
            Some(edge),
        ) if edge.src == *source && edge.label == *label && edge.dst == *destination => {
            core_graph::source::edge_digest(&edge).map_err(storage_error)
        }
        _ => Err(unavailable()),
    }
}

#[tauri::command]
pub(crate) async fn describe_captured_source(
    fact: FactKey,
    expected_node: Option<core_graph::Node>,
    expected_edge: Option<core_graph::Edge>,
    app: tauri::AppHandle,
) -> Result<CapturedDescription, String> {
    crate::off_ui_thread(move || {
        let expected_digest = expected_digest(&fact, expected_node, expected_edge)?;
        let description = describe(&app.state::<AppState>(), &fact)?;
        if description.emitted_fact_digest != expected_digest {
            return Err(
                "Selected fact changed; refresh the graph before inspecting captured source."
                    .into(),
            );
        }
        Ok(description)
    })
    .await
}

#[tauri::command]
pub(crate) async fn read_captured_source(
    fact: FactKey,
    receipt_id: String,
    range_index: usize,
    app: tauri::AppHandle,
) -> Result<CapturedText, String> {
    crate::off_ui_thread(move || read(&app.state::<AppState>(), &fact, &receipt_id, range_index))
        .await
}

/// The fact's observed binding, when a captured observation input observed
/// it (ADR-0079, AC-1049); `None` when it has none.
#[tauri::command]
pub(crate) async fn describe_observed_source(
    fact: FactKey,
    expected_node: Option<core_graph::Node>,
    expected_edge: Option<core_graph::Edge>,
    app: tauri::AppHandle,
) -> Result<Option<ObservedDescription>, String> {
    crate::off_ui_thread(move || {
        let expected_digest = expected_digest(&fact, expected_node, expected_edge)?;
        let state = app.state::<AppState>();
        let present = state
            .graph
            .lock()
            .map_err(storage_error)?
            .current_source_observation(&fact)
            .map_err(storage_error)?
            .is_some();
        if !present {
            return Ok(None);
        }
        let description = describe_observed(&state, &fact)?;
        if description.emitted_fact_digest != expected_digest {
            return Err(
                "Selected fact changed; refresh the graph before inspecting observed source."
                    .into(),
            );
        }
        Ok(Some(description))
    })
    .await
}

#[tauri::command]
pub(crate) async fn read_observed_source(
    fact: FactKey,
    receipt_id: String,
    range_index: usize,
    app: tauri::AppHandle,
) -> Result<CapturedText, String> {
    crate::off_ui_thread(move || {
        read_observed(&app.state::<AppState>(), &fact, &receipt_id, range_index)
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_retained_sources(
    app: tauri::AppHandle,
) -> Result<Vec<RetainedSource>, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let sources = state.sources.lock().map_err(storage_error)?.list()?;
        Ok(sources
            .into_iter()
            .map(|source| RetainedSource {
                source_id: source.source_id,
                repo_key: source.repo_key,
                display_name: source.display_name,
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub(crate) async fn preview_forget_source(
    source_id: String,
    app: tauri::AppHandle,
) -> Result<RetentionPreview, String> {
    crate::off_ui_thread(move || preview(&app.state::<AppState>(), &source_id)).await
}

#[tauri::command]
pub(crate) async fn forget_retained_source(
    source_id: String,
    fingerprint: String,
    app: tauri::AppHandle,
) -> Result<u64, String> {
    crate::off_ui_thread(move || forget(&app.state::<AppState>(), &source_id, &fingerprint)).await
}
