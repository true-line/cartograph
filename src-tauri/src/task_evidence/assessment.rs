//! Read-only selected-history assessment. Never changes immutable task meaning.

use super::*;
use serde::Serialize;
use tauri::Manager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BasisComparison {
    Unchanged,
    Changed,
    LegacyUnverified,
    OperationalFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Availability {
    Available,
    Unavailable,
    Invalid,
    OperationalFailure,
    ValidationByteLimit,
    WorkingTreeUnverified,
}

impl From<PinnedReadError> for Availability {
    fn from(error: PinnedReadError) -> Self {
        match error {
            PinnedReadError::Unavailable => Self::Unavailable,
            PinnedReadError::Invalid => Self::Invalid,
            PinnedReadError::Operational => Self::OperationalFailure,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct EvidenceAvailability {
    pub evidence_id: String,
    pub status: Availability,
}

#[derive(Debug, Serialize)]
pub(crate) struct BasisAssessment {
    pub schema_version: u32,
    pub proposal_id: String,
    pub current_graph_snapshot_id: Option<String>,
    pub graph_comparison: BasisComparison,
    pub association_comparison: BasisComparison,
    /// The files the captured evidence cites, at the current associations,
    /// against the ones the task was made with (#2251). Compared only when
    /// the associations could be read; `legacy_unverified` otherwise.
    pub cited_files: BasisComparison,
    pub evidence: Vec<EvidenceAvailability>,
    pub unverified_evidence: usize,
    pub captured_validation_bytes: u64,
    pub max_captured_validation_bytes: u64,
}

fn checked_availability(
    state: &AppState,
    basis: &TaskSourceBasisV2,
    staged: &agents::StagedProposal,
    origin: &TaskEvidenceOrigin,
    guard: &RetentionGuard,
    charged: &mut u64,
) -> Availability {
    let TaskEvidenceKind::CapturedPrimarySource {
        receipt_id,
        receipt_inventory_index,
        captured,
        ..
    } = &origin.origin
    else {
        return Availability::WorkingTreeUnverified;
    };
    let Some(selected) = basis
        .selected_facts
        .iter()
        .find(|selected| selected.fact == origin.fact)
    else {
        return Availability::Invalid;
    };
    let Some(binding) = &selected.binding else {
        return Availability::Invalid;
    };
    let Some(evidence) = staged
        .basis
        .evidence
        .iter()
        .find(|item| item.id == origin.evidence_id)
    else {
        return Availability::Invalid;
    };
    let receipt = match state.primary_sources.task_receipt(
        guard,
        &graph_fact(&origin.fact),
        &selected.fact_digest,
        &binding.repo_key,
        receipt_id,
    ) {
        Ok(receipt) => receipt,
        Err(error) => return error.into(),
    };
    let Some(range) = receipt.ranges().get(*receipt_inventory_index as usize) else {
        return Availability::Invalid;
    };
    // Compare the exact historical occurrence, not an arbitrary equal byte span.
    let same_role = matches!(
        (serde_json::to_value(range.role), serde_json::to_value(origin.role)),
        (Ok(left), Ok(right)) if left == right
    );
    let same_capture = matches!(
        (serde_json::to_value(&range.captured), serde_json::to_value(captured)),
        (Ok(left), Ok(right)) if left == right
    );
    if !same_role
        || !same_capture
        || range.index != origin.index
        || range.evidence != evidence.source
    {
        return Availability::Invalid;
    }
    let Some(next_charge) = charged
        .checked_add(range.captured.file.byte_len)
        .filter(|next| *next <= basis.selection.limits.captured_validation_bytes)
    else {
        return Availability::ValidationByteLimit;
    };
    *charged = next_charge;
    match state
        .primary_sources
        .task_text(guard, &receipt, *receipt_inventory_index as usize)
    {
        Ok(text)
            if text.len() == evidence.text_bytes
                && core_prov::content_hash(text.as_bytes()) == evidence.text_hash =>
        {
            Availability::Available
        }
        Ok(_) => Availability::Invalid,
        Err(error) => error.into(),
    }
}

pub(crate) fn assess(state: &AppState, proposal_id: &str) -> Result<BasisAssessment, String> {
    assess_observed(state, proposal_id, &mut SharedObservation::default())
        .map(|(assessment, _)| assessment)
}

/// The graph one or more assessments observed: the rows of one store
/// revision and the snapshot identity of those rows. A caller that makes
/// many assessments in one read passes the same value to each, so the graph
/// is copied and its identity computed once for all of them instead of once
/// for each (#1834). It is in memory only and ends with that read.
///
/// Every assessment still observes the store itself. Its source
/// associations are read in a transaction that first checks the store is at
/// the revision of these rows; when the store has moved, the rows are read
/// again and replace these. So an assessment never reports the identity of
/// a graph the store did not hold when it was made, and it reports the
/// revision it was made at.
#[derive(Default)]
pub(crate) struct SharedObservation(Option<Observation>);

pub(super) struct Observation {
    /// The complete graph and its store revision, with no selection kept.
    pub(super) rows: core_graph::source::SourceSelectionSnapshot,
    /// `ContextSnapshot::id` of `rows.graph`. `Err` when it has none.
    pub(super) identity: Result<String, ()>,
}

#[cfg(test)]
thread_local! {
    static GRAPH_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times an assessment on this thread copied the graph.
#[cfg(test)]
pub(crate) fn graph_copies() -> usize {
    GRAPH_COPIES.with(std::cell::Cell::get)
}

impl SharedObservation {
    /// The observation the store is at now and the source-selection states
    /// of `keys` in it. The graph lock is held for the store read alone and
    /// not while the identity is computed. `Err` when the store read failed.
    pub(super) fn observe(
        &mut self,
        state: &AppState,
        keys: &[core_graph::source::FactKey],
    ) -> Result<(&Observation, Vec<core_graph::source::FactSourceSelection>), ()> {
        let graph = state.graph.lock().map_err(|_| ())?;
        let current = match &self.0 {
            Some(observed) => match graph.read_source_selections_at(&observed.rows, keys) {
                Ok(selections) => Some(selections),
                // The store moved since these rows were read: read again.
                Err(core_graph::GraphError::Generation(core_graph::GenerationError::Changed)) => {
                    None
                }
                Err(_) => return Err(()),
            },
            None => None,
        };
        let selections = match current {
            Some(selections) => selections,
            None => {
                #[cfg(test)]
                GRAPH_COPIES.with(|count| count.set(count.get() + 1));
                let mut rows = graph.read_source_selection_snapshot(keys).map_err(|_| ())?;
                drop(graph);
                let selections = std::mem::take(&mut rows.selections);
                let identity =
                    context_hub::ContextSnapshot::new(rows.graph.0.clone(), rows.graph.1.clone())
                        .map(|snapshot| snapshot.id().to_owned())
                        .map_err(|_| ());
                self.0 = Some(Observation { rows, identity });
                selections
            }
        };
        self.0
            .as_ref()
            .map(|observed| (observed, selections))
            .ok_or(())
    }
}

/// [`assess`], with the store revision its graph and association read
/// observed. `None` when that read failed. A caller that combines the
/// assessment with a later graph read compares the two revisions (SPEC-12).
/// `shared` is the observation kept between the assessments of one read.
pub(crate) fn assess_observed(
    state: &AppState,
    proposal_id: &str,
    shared: &mut SharedObservation,
) -> Result<(BasisAssessment, Option<core_graph::StoreRevision>), String> {
    let staged = state
        .proposals
        .lock()
        .map_err(|_| "Proposal history is unavailable.")?
        .get(proposal_id)
        .map_err(|_| "Proposal history is unavailable or invalid.")?
        .ok_or("The selected staged proposal is unavailable.")?;
    let mut result = BasisAssessment {
        schema_version: 1,
        proposal_id: staged.proposal_id.clone(),
        current_graph_snapshot_id: None,
        graph_comparison: BasisComparison::LegacyUnverified,
        association_comparison: BasisComparison::LegacyUnverified,
        cited_files: BasisComparison::LegacyUnverified,
        evidence: Vec::new(),
        unverified_evidence: 0,
        captured_validation_bytes: 0,
        max_captured_validation_bytes: SelectionLimits::default().captured_validation_bytes,
    };
    let Some(basis) = &staged.source_basis else {
        result.unverified_evidence = staged.basis.evidence.len();
        result.evidence = staged
            .basis
            .evidence
            .iter()
            .map(|item| EvidenceAvailability {
                evidence_id: item.id.clone(),
                status: Availability::WorkingTreeUnverified,
            })
            .collect();
        return Ok((result, None));
    };
    let mut sources = BTreeMap::new();
    for origin in &basis.evidence {
        if let TaskEvidenceKind::CapturedPrimarySource {
            registered_source_id,
            ..
        } = &origin.origin
        {
            sources
                .entry(registered_source_id.clone())
                .or_insert_with(|| {
                    state
                        .sources
                        .lock()
                        .map_err(|_| PinnedReadError::Operational)?
                        .get_by_id(registered_source_id)
                        .map_err(|_| PinnedReadError::Operational)?
                        .ok_or(PinnedReadError::Unavailable)
                });
        }
    }
    // No source roots or managed readiness are required for historical bytes.
    // Keep all leases through the current graph/association observation.
    let guards: BTreeMap<_, _> = sources
        .iter()
        .map(|(id, source)| {
            let guard = source
                .as_ref()
                .map_err(|error| *error)
                .and_then(|_| state.primary_sources.task_guard(id));
            (id.clone(), guard)
        })
        .collect();
    for origin in &basis.evidence {
        let status = match &origin.origin {
            TaskEvidenceKind::WorkingTreeUnverified => {
                result.unverified_evidence += 1;
                Availability::WorkingTreeUnverified
            }
            TaskEvidenceKind::CapturedPrimarySource {
                registered_source_id,
                ..
            } => {
                let expected_repo = basis
                    .selected_facts
                    .iter()
                    .find(|selected| selected.fact == origin.fact)
                    .and_then(|selected| selected.binding.as_ref())
                    .map(|binding| &binding.repo_key);
                let registration_matches = sources
                    .get(registered_source_id)
                    .and_then(|value| value.as_ref().ok())
                    .is_some_and(|source| Some(&source.repo_key) == expected_repo);
                match guards.get(registered_source_id) {
                    Some(Ok(guard)) if registration_matches => checked_availability(
                        state,
                        basis,
                        &staged,
                        origin,
                        guard,
                        &mut result.captured_validation_bytes,
                    ),
                    Some(Ok(_)) => Availability::Invalid,
                    Some(Err(error)) => (*error).into(),
                    None => Availability::OperationalFailure,
                }
            }
        };
        result.evidence.push(EvidenceAvailability {
            evidence_id: origin.evidence_id.clone(),
            status,
        });
    }
    let keys: Vec<_> = basis
        .selected_facts
        .iter()
        .map(|selected| graph_fact(&selected.fact))
        .collect();
    let mut revision = None;
    match shared.observe(state, &keys) {
        Ok((observed, selections)) => {
            revision = Some(observed.rows.revision.clone());
            match &observed.identity {
                Ok(identity) => {
                    result.graph_comparison = if *identity == basis.graph_snapshot_id {
                        BasisComparison::Unchanged
                    } else {
                        BasisComparison::Changed
                    };
                    result.current_graph_snapshot_id = Some(identity.clone());
                }
                Err(()) => result.graph_comparison = BasisComparison::OperationalFailure,
            }
            result.association_comparison = BasisComparison::Unchanged;
            for value in &selections {
                if matches!(value.state, FactSourceState::Invalid { .. }) {
                    result.association_comparison = BasisComparison::OperationalFailure;
                    break;
                }
                let current = selection(value).ok();
                let expected = basis
                    .selected_facts
                    .iter()
                    .find(|selected| selected.fact == task_fact(&value.fact));
                if current.as_ref() != expected {
                    result.association_comparison = BasisComparison::Changed;
                }
            }
            if result.association_comparison != BasisComparison::OperationalFailure {
                result.cited_files = cited_files(state, basis, &selections, &guards);
            }
        }
        Err(()) => {
            result.graph_comparison = BasisComparison::OperationalFailure;
            result.association_comparison = BasisComparison::OperationalFailure;
        }
    }
    drop(guards);
    Ok((result, revision))
}

/// Whether every file the captured evidence cites is byte-identical at the
/// current source associations (#2251): each origin's fact keeps its digest,
/// and the receipt its current association names cites the same path with
/// the same full-file digest. Any read that fails makes it an operational
/// failure, so nothing is shown unchanged that was not compared.
fn cited_files(
    state: &AppState,
    basis: &TaskSourceBasisV2,
    selections: &[core_graph::source::FactSourceSelection],
    guards: &BTreeMap<String, Result<RetentionGuard, PinnedReadError>>,
) -> BasisComparison {
    let mut compared = false;
    for origin in &basis.evidence {
        let TaskEvidenceKind::CapturedPrimarySource {
            registered_source_id,
            captured,
            ..
        } = &origin.origin
        else {
            continue;
        };
        let Some(Ok(guard)) = guards.get(registered_source_id) else {
            return BasisComparison::OperationalFailure;
        };
        let original = basis
            .selected_facts
            .iter()
            .find(|selected| selected.fact == origin.fact);
        let current = selections
            .iter()
            .find(|value| task_fact(&value.fact) == origin.fact)
            .and_then(|value| selection(value).ok());
        let (Some(original), Some(current)) = (original, current) else {
            return BasisComparison::OperationalFailure;
        };
        let Some(binding) = &current.binding else {
            return BasisComparison::Changed;
        };
        if current.fact_digest != original.fact_digest {
            return BasisComparison::Changed;
        }
        let receipt = match state.primary_sources.task_receipt(
            guard,
            &graph_fact(&origin.fact),
            &current.fact_digest,
            &binding.repo_key,
            &binding.receipt_id,
        ) {
            Ok(receipt) => receipt,
            Err(_) => return BasisComparison::OperationalFailure,
        };
        let same = receipt.ranges().iter().any(|range| {
            range.captured.file.path == captured.file.path
                && range.captured.file.digest == captured.file.digest
                && range.captured.file.byte_len == captured.file.byte_len
        });
        if !same {
            return BasisComparison::Changed;
        }
        compared = true;
    }
    if compared {
        BasisComparison::Unchanged
    } else {
        BasisComparison::LegacyUnverified
    }
}

#[tauri::command]
pub(crate) async fn assess_staged_basis(
    proposal_id: String,
    app: tauri::AppHandle,
) -> Result<BasisAssessment, String> {
    // Invalid caller IDs must not cause an unbounded SQL/string lookup.
    if proposal_id.len() > 128 {
        return Err("Invalid staged proposal identity.".into());
    }
    crate::off_ui_thread(move || assess(&app.state::<AppState>(), &proposal_id)).await
}
