//! Explicit reconciliation of a staged record against the current recovery
//! (ADR-0072 decision 2, slice 3).
//!
//! The host reads the graph and source associations it holds now, and the
//! retained bytes their receipts name, compares each staged item and
//! candidate exactly, and records one immutable result. It never edits the
//! staged record, never runs on its own, and writes nothing when a read fails.

use super::assessment::SharedObservation;
use super::*;
use agents::reconciliation::{
    GapPresence, RecapturedCandidate, RecapturedItem, ReconciledOrigin, ReconciliationInput,
    ReconciliationOutcome, ReconciliationResultV1, RecoveryBasis, RecoveryCapture, SlotPresence,
    SlotReconciliation, candidate_summary_v1,
};
use agents::{RecordedReconciliation, StagedProposal};
use serde::Serialize;
use std::collections::BTreeSet;
use tauri::Manager;

/// Job kind prefix of one explicit reconciliation.
pub(crate) const JOB_KIND_PREFIX: &str = "reconcile-staged:";
const UNAVAILABLE: &str = "Proposal history is unavailable or invalid.";
const READ_FAILED: &str = "The current recovery could not be read; nothing was recorded.";

/// A record's derived reconciliation state. Never stored on the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReconciliationState {
    AwaitingReconciliation,
    Reconciled,
    Changed,
    Missing,
    Unverifiable,
    ReconciliationStale,
}

impl From<ReconciliationOutcome> for ReconciliationState {
    fn from(outcome: ReconciliationOutcome) -> Self {
        match outcome {
            ReconciliationOutcome::Reconciled => Self::Reconciled,
            ReconciliationOutcome::Changed => Self::Changed,
            ReconciliationOutcome::Missing => Self::Missing,
            ReconciliationOutcome::Unverifiable => Self::Unverifiable,
        }
    }
}

/// A record's state and its results, newest first.
#[derive(Debug, Serialize)]
pub(crate) struct ReconciliationHistory {
    pub proposal_id: String,
    pub state: ReconciliationState,
    pub results: Vec<RecordedReconciliation>,
}

/// The facts whose receipts may hold each staged item, in staged order. A v2
/// item names its fact. A v1 item does not: it may hold the evidence of the
/// slot's source or Gap (source-side items) or of a candidate that lists it.
fn item_facts(staged: &StagedProposal) -> Vec<Vec<FactKey>> {
    let node = |id: &str| FactKey::Node { id: id.to_owned() };
    match &staged.source_basis {
        Some(basis) => staged
            .basis
            .evidence
            .iter()
            .map(|item| {
                origin_of(basis, &item.id)
                    .map(|origin| vec![graph_fact(&origin.fact)])
                    .unwrap_or_default()
            })
            .collect(),
        None => staged
            .basis
            .evidence
            .iter()
            .map(|item| {
                let mut facts = Vec::new();
                if staged.basis.source_evidence_ids.contains(&item.id) {
                    facts.push(node(&staged.basis.source_id));
                    facts.push(node(&staged.basis.gap_id));
                }
                for candidate in &staged.basis.candidates {
                    if candidate.evidence_ids.contains(&item.id) {
                        facts.push(node(&candidate.node_id));
                    }
                }
                facts
            })
            .collect(),
    }
}

/// The origin a v2 basis binds to one staged item, by its evidence id (the
/// basis does not keep the manifest's order).
fn origin_of<'a>(
    basis: &'a TaskSourceBasisV2,
    evidence_id: &str,
) -> Option<&'a TaskEvidenceOrigin> {
    basis
        .evidence
        .iter()
        .find(|origin| origin.evidence_id == evidence_id)
}

/// How one staged item is located in a current receipt.
enum Locate<'a> {
    /// A captured item: the same role and index of its fact's receipt.
    RoleIndex(TaskRangeRole, u32),
    /// A working-tree item: the range citing exactly its path and offsets.
    Span(&'a core_prov::EvidenceRef),
}

/// Owned form of [`RecapturedItem`], so the bytes outlive the reads.
enum Gathered {
    Simple(RecapturedItem<'static>),
    Range {
        origin: ReconciledOrigin,
        fact_digest: String,
        bytes: Vec<u8>,
    },
}

impl Gathered {
    fn borrowed(&self) -> RecapturedItem<'_> {
        match self {
            Self::Simple(item) => item.clone(),
            Self::Range {
                origin,
                fact_digest,
                bytes,
            } => RecapturedItem::Range {
                origin: origin.clone(),
                fact_digest,
                bytes,
            },
        }
    }

    /// Among several facts, the most specific non-match explains the item.
    fn rank(&self) -> u8 {
        match self {
            Self::Range { .. } => 0,
            Self::Simple(RecapturedItem::ValidationByteLimit) => 1,
            Self::Simple(RecapturedItem::SpanOutOfBounds) => 2,
            Self::Simple(RecapturedItem::PathNotCaptured) => 3,
            Self::Simple(RecapturedItem::NoParticipatingReceipt) => 4,
            Self::Simple(_) => 5,
        }
    }
}

/// Retention guards for the registered sources that current bindings name,
/// held until every read is done.
#[derive(Default)]
struct Guards(BTreeMap<String, Result<RetentionGuard, PinnedReadError>>);

impl Guards {
    fn for_repo(
        &mut self,
        state: &AppState,
        repo: &str,
    ) -> Result<&RetentionGuard, PinnedReadError> {
        self.0
            .entry(repo.to_owned())
            .or_insert_with(|| {
                let source = state
                    .sources
                    .lock()
                    .map_err(|_| PinnedReadError::Operational)?
                    .get_by_repo(repo)
                    .map_err(|_| PinnedReadError::Operational)?
                    .ok_or(PinnedReadError::Unavailable)?;
                state.primary_sources.task_guard(&source.source_id)
            })
            .as_ref()
            .map_err(|error| *error)
    }
}

fn same_role(left: TaskRangeRole, right: adapters_lang_ts::captured::RangeRole) -> bool {
    matches!(
        (serde_json::to_value(left), serde_json::to_value(right)),
        (Ok(a), Ok(b)) if a == b
    )
}

struct Reader<'s> {
    state: &'s AppState,
    selections: BTreeMap<FactKey, FactSourceSelection>,
    guards: Guards,
    charged: u64,
    budget: u64,
    captures: BTreeSet<(String, String)>,
}

impl Reader<'_> {
    /// What the current recovery shows for `locate` under one fact.
    fn gather(&mut self, fact: &FactKey, locate: &Locate<'_>) -> Result<Gathered, String> {
        let (fact_digest, binding) = match self.selections.get(fact).map(|value| &value.state) {
            None | Some(FactSourceState::Missing) => {
                return Ok(Gathered::Simple(RecapturedItem::FactAbsent));
            }
            Some(FactSourceState::Absent { .. }) => {
                return Ok(Gathered::Simple(RecapturedItem::NoParticipatingReceipt));
            }
            Some(FactSourceState::Invalid { .. }) => return Err(READ_FAILED.into()),
            Some(FactSourceState::Present {
                fact_digest,
                binding,
            }) => (fact_digest.clone(), binding.clone()),
        };
        let guard = match self.guards.for_repo(self.state, &binding.repo_key) {
            Ok(guard) => guard,
            Err(PinnedReadError::Unavailable) => {
                return Ok(Gathered::Simple(RecapturedItem::NoParticipatingReceipt));
            }
            Err(_) => return Err(READ_FAILED.into()),
        };
        let receipt = match self.state.primary_sources.task_receipt(
            guard,
            fact,
            &fact_digest,
            &binding.repo_key,
            &binding.receipt_id,
        ) {
            Ok(receipt) => receipt,
            Err(PinnedReadError::Unavailable) => {
                return Ok(Gathered::Simple(RecapturedItem::NoParticipatingReceipt));
            }
            Err(_) => return Err(READ_FAILED.into()),
        };
        let file = receipt.file();
        self.captures
            .insert((file.source_id.as_str().to_owned(), file.capture_id.clone()));
        let found = receipt
            .ranges()
            .iter()
            .enumerate()
            .find(|(_, range)| match locate {
                Locate::RoleIndex(role, index) => {
                    same_role(*role, range.role) && range.index == *index
                }
                Locate::Span(reference) => {
                    range.evidence.path == reference.path
                        && range.evidence.byte_start == reference.byte_start
                        && range.evidence.byte_end == reference.byte_end
                }
            });
        let Some((index, range)) = found else {
            let path_captured = match locate {
                Locate::RoleIndex(..) => true,
                Locate::Span(reference) => receipt
                    .ranges()
                    .iter()
                    .any(|range| range.evidence.path == reference.path),
            };
            return Ok(Gathered::Simple(if path_captured {
                RecapturedItem::SpanOutOfBounds
            } else {
                RecapturedItem::PathNotCaptured
            }));
        };
        let Some(charged) = self
            .charged
            .checked_add(range.captured.file.byte_len)
            .filter(|charged| *charged <= self.budget)
        else {
            return Ok(Gathered::Simple(RecapturedItem::ValidationByteLimit));
        };
        self.charged = charged;
        let bytes = self
            .state
            .primary_sources
            .task_bytes(guard, &receipt, index)
            .map_err(|_| READ_FAILED.to_owned())?;
        let file = &range.captured.file;
        self.captures
            .insert((file.source_id.as_str().to_owned(), file.capture_id.clone()));
        Ok(Gathered::Range {
            origin: ReconciledOrigin {
                receipt_id: receipt.id().to_owned(),
                receipt_inventory_index: index as u32,
                captured: TaskCaptureSpanRef {
                    file: TaskCaptureFileRef {
                        source_id: file.source_id.as_str().to_owned(),
                        capture_id: file.capture_id.clone(),
                        path: file.path.clone(),
                        digest: file.digest.clone(),
                        byte_len: file.byte_len,
                    },
                    byte_start: range.captured.byte_start,
                    byte_end: range.captured.byte_end,
                },
                input_closure: InputClosureStatus::InputClosureNotEstablished,
            },
            fact_digest,
            bytes,
        })
    }
}

/// The slot's directed relation: `(from, to)`. A v2 record selected the
/// exact edge fact. A v1 record kept no direction, so either direction
/// between its source and Gap is its relation.
fn slot_edges(staged: &StagedProposal) -> Vec<(String, String)> {
    let basis = &staged.basis;
    let selected = staged.source_basis.as_ref().and_then(|source| {
        source
            .selected_facts
            .iter()
            .find_map(|selected| match &selected.fact {
                TaskFactKey::Edge {
                    source,
                    label,
                    destination,
                } if *label == basis.edge_label
                    && ((*source == basis.source_id && *destination == basis.gap_id)
                        || (*source == basis.gap_id && *destination == basis.source_id)) =>
                {
                    Some((source.clone(), destination.clone()))
                }
                _ => None,
            })
    });
    match selected {
        Some(edge) => vec![edge],
        None => vec![
            (basis.source_id.clone(), basis.gap_id.clone()),
            (basis.gap_id.clone(), basis.source_id.clone()),
        ],
    }
}

/// The directed slot in the current graph.
fn slot(staged: &StagedProposal, nodes: &[Node], edges: &[Edge]) -> SlotReconciliation {
    let basis = &staged.basis;
    let node = |id: &str| nodes.iter().find(|node| node.id == id);
    let present = |value: bool| {
        if value {
            SlotPresence::Present
        } else {
            SlotPresence::Missing
        }
    };
    let directions = slot_edges(staged);
    let gap = match node(&basis.gap_id) {
        Some(gap) if spec::is_gap_node(gap) => GapPresence::Present,
        Some(_) => GapPresence::GapResolved,
        None => {
            // The Gap is gone: recovery resolved the slot when the source now
            // has the relation, in the slot's own direction, to a real node.
            let real = |id: &str| node(id).is_some_and(|target| !spec::is_gap_node(target));
            let resolved = edges.iter().any(|edge| {
                edge.label == basis.edge_label
                    && directions.iter().any(|(from, to)| {
                        (*from == basis.source_id
                            && edge.src == *from
                            && *to == basis.gap_id
                            && real(&edge.dst))
                            || (*to == basis.source_id
                                && edge.dst == *to
                                && *from == basis.gap_id
                                && real(&edge.src))
                    })
            });
            if resolved {
                GapPresence::GapResolved
            } else {
                GapPresence::Missing
            }
        }
    };
    let relation = edges.iter().any(|edge| {
        edge.label == basis.edge_label
            && directions
                .iter()
                .any(|(from, to)| edge.src == *from && edge.dst == *to)
    });
    SlotReconciliation {
        source: present(node(&basis.source_id).is_some()),
        gap,
        relation: present(relation),
    }
}

fn staged(state: &AppState, proposal_id: &str) -> Result<StagedProposal, String> {
    state
        .proposals
        .lock()
        .map_err(|_| UNAVAILABLE)?
        .get(proposal_id)
        .map_err(|_| UNAVAILABLE)?
        .ok_or_else(|| "The selected staged proposal is unavailable.".into())
}

/// Compare one staged record with the current recovery and record the
/// result under `job_id`, the durable job that made the comparison.
pub(crate) fn reconcile(
    state: &AppState,
    proposal_id: &str,
    job_id: i64,
) -> Result<RecordedReconciliation, String> {
    let staged = staged(state, proposal_id)?;
    let facts = item_facts(&staged);
    let keys: Vec<FactKey> = facts
        .iter()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut shared = SharedObservation::default();
    let (observation, selections) = shared
        .observe(state, &keys)
        .map_err(|()| READ_FAILED.to_owned())?;
    let graph_snapshot_id = observation
        .identity
        .clone()
        .map_err(|()| READ_FAILED.to_owned())?;
    let (nodes, edges) = &observation.rows.graph;
    let mut reader = Reader {
        state,
        selections: selections
            .into_iter()
            .map(|value| (value.fact.clone(), value))
            .collect(),
        guards: Guards::default(),
        charged: 0,
        budget: SelectionLimits::default().captured_validation_bytes,
        captures: BTreeSet::new(),
    };
    let mut gathered = Vec::with_capacity(staged.basis.evidence.len());
    for (index, item) in staged.basis.evidence.iter().enumerate() {
        let locate = match staged
            .source_basis
            .as_ref()
            .and_then(|basis| origin_of(basis, &item.id))
        {
            Some(TaskEvidenceOrigin {
                role,
                index,
                origin: TaskEvidenceKind::CapturedPrimarySource { .. },
                ..
            }) => Locate::RoleIndex(*role, *index),
            _ => Locate::Span(&item.source),
        };
        let mut best: Option<Gathered> = None;
        for fact in &facts[index] {
            let next = reader.gather(fact, &locate)?;
            if best.as_ref().is_none_or(|best| next.rank() < best.rank()) {
                best = Some(next);
            }
            if best.as_ref().is_some_and(|best| best.rank() == 0) {
                break;
            }
        }
        gathered.push(best.unwrap_or(Gathered::Simple(RecapturedItem::NoParticipatingReceipt)));
    }
    let candidates = staged
        .basis
        .candidates
        .iter()
        .map(
            |candidate| match nodes.iter().find(|node| node.id == candidate.node_id) {
                Some(node) => RecapturedCandidate::Present {
                    label: node.label.clone(),
                    summary: candidate_summary_v1(&node.label, &node.props, &node.id),
                },
                None => RecapturedCandidate::Absent,
            },
        )
        .collect();
    let input = ReconciliationInput {
        recovery: RecoveryBasis {
            job_id,
            graph_snapshot_id,
            captures: reader
                .captures
                .iter()
                .map(|(source_id, capture_id)| RecoveryCapture {
                    source_id: source_id.clone(),
                    capture_id: capture_id.clone(),
                })
                .collect(),
        },
        slot: slot(&staged, nodes, edges),
        evidence: gathered.iter().map(Gathered::borrowed).collect(),
        candidates,
    };
    let result =
        ReconciliationResultV1::derive(&staged, input).map_err(|error| error.to_string())?;
    drop(reader);
    state
        .proposals
        .lock()
        .map_err(|_| UNAVAILABLE)?
        .record_reconciliation(&result)
        .map_err(|error| error.to_string())
}

/// The state a record's latest result gives now: its outcome while the
/// recovery it read is still current, `reconciliation_stale` once the graph
/// snapshot moved, a reconciled span's capture is no longer retained, or its
/// fact's current association names another receipt.
pub(crate) fn current_state(
    state: &AppState,
    staged: &StagedProposal,
    latest: Option<&RecordedReconciliation>,
    shared: &mut SharedObservation,
) -> Result<ReconciliationState, String> {
    let Some(latest) = latest else {
        return Ok(ReconciliationState::AwaitingReconciliation);
    };
    let result = &latest.result;
    let facts = item_facts(staged);
    let keys: Vec<FactKey> = facts
        .iter()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let (observation, selections) = shared
        .observe(state, &keys)
        .map_err(|()| READ_FAILED.to_owned())?;
    let identity = observation
        .identity
        .as_ref()
        .map_err(|()| READ_FAILED.to_owned())?;
    if *identity != result.recovery.graph_snapshot_id {
        return Ok(ReconciliationState::ReconciliationStale);
    }
    let mut guards = Guards::default();
    for (item, item_facts) in result.evidence.iter().zip(&facts) {
        let Some(origin) = &item.reconciled else {
            continue;
        };
        let mut bound = None;
        for fact in item_facts {
            if let Some(FactSourceState::Present { binding, .. }) = selections
                .iter()
                .find(|value| &value.fact == fact)
                .map(|value| &value.state)
                && binding.receipt_id == origin.receipt_id
            {
                bound = Some(binding.repo_key.clone());
            }
        }
        let Some(repo) = bound else {
            return Ok(ReconciliationState::ReconciliationStale);
        };
        let retained = guards.for_repo(state, &repo).and_then(|guard| {
            state
                .primary_sources
                .task_capture_present(guard, &origin.captured)
        });
        match retained {
            Ok(()) => {}
            Err(PinnedReadError::Operational) => return Err(READ_FAILED.into()),
            Err(_) => return Ok(ReconciliationState::ReconciliationStale),
        }
    }
    Ok(result.outcome.into())
}

/// What the curated read took from a record's results: the id of the
/// latest result it read (`None` when there is none), and that result's
/// read with the store revision its graph read observed, when it has one.
pub(crate) type CuratedReconciliation = (
    Option<String>,
    Option<(
        context_hub::curated::RecordReconciliation,
        Option<core_graph::StoreRevision>,
    )>,
);

/// The id of the record's latest result, `None` when it has none. An MCP
/// read compares it with the one its projection read (AC-1076).
pub(crate) fn latest_result_id(
    store: &agents::ProposalStore,
    proposal_id: &str,
) -> Result<Option<String>, String> {
    Ok(store
        .reconciliations(proposal_id, 1)
        .map_err(|error| error.to_string())?
        .first()
        .map(|latest| latest.result.result_id.clone()))
}

/// The record's latest reconciliation as the curated projection reads it
/// (ADR-0072 slice 4, AC-1076). A record never reconciled has no read. A
/// current `reconciled` result reports how many items matched their
/// retained bytes and how many a recapture corroborated.
pub(crate) fn curated_reconciliation(
    state: &AppState,
    proposal_id: &str,
    shared: &mut SharedObservation,
) -> Result<CuratedReconciliation, String> {
    use agents::reconciliation::ItemOutcome;
    use context_hub::curated::RecordReconciliation as Read;
    let results = state
        .proposals
        .lock()
        .map_err(|_| UNAVAILABLE)?
        .reconciliations(proposal_id, 1)
        .map_err(|error| error.to_string())?;
    let Some(latest) = results.first() else {
        return Ok((None, None));
    };
    let id = Some(latest.result.result_id.clone());
    let staged = staged(state, proposal_id)?;
    let read = match current_state(state, &staged, Some(latest), shared)? {
        ReconciliationState::AwaitingReconciliation => return Ok((id, None)),
        ReconciliationState::Reconciled => {
            let count = |kind: fn(&ItemOutcome) -> bool| {
                latest
                    .result
                    .evidence
                    .iter()
                    .filter(|item| kind(&item.outcome))
                    .count()
            };
            Read::Reconciled {
                retained_match: count(|o| matches!(o, ItemOutcome::RetainedMatch)),
                recapture_corroborated: count(|o| matches!(o, ItemOutcome::RecaptureCorroborated)),
            }
        }
        ReconciliationState::Changed => Read::Changed,
        ReconciliationState::Missing => Read::Missing,
        ReconciliationState::Unverifiable => Read::Unverifiable,
        ReconciliationState::ReconciliationStale => Read::Stale,
    };
    Ok((id, Some((read, shared.revision()))))
}

/// A record's state and up to `limit` results, newest first.
pub(crate) fn history(
    state: &AppState,
    proposal_id: &str,
    limit: usize,
) -> Result<ReconciliationHistory, String> {
    let staged = staged(state, proposal_id)?;
    let results = state
        .proposals
        .lock()
        .map_err(|_| UNAVAILABLE)?
        .reconciliations(proposal_id, limit)
        .map_err(|error| error.to_string())?;
    let state_now = current_state(
        state,
        &staged,
        results.first(),
        &mut SharedObservation::default(),
    )?;
    Ok(ReconciliationHistory {
        proposal_id: staged.proposal_id,
        state: state_now,
        results,
    })
}

fn checked_id(proposal_id: &str) -> Result<(), String> {
    if proposal_id.is_empty() || proposal_id.len() > 128 {
        Err("Invalid staged proposal identity.".into())
    } else {
        Ok(())
    }
}

/// Explicitly reconcile one staged record against the current recovery, as
/// its own durable job. A failed read records nothing.
#[tauri::command]
pub(crate) async fn reconcile_staged(
    proposal_id: String,
    app: tauri::AppHandle,
) -> Result<ReconciliationHistory, String> {
    checked_id(&proposal_id)?;
    let state = app.state::<AppState>();
    let (running, execution) =
        crate::start_job(&state, &format!("{JOB_KIND_PREFIX}{proposal_id}"))?;
    crate::emit_job(&app, &running);
    crate::off_ui_thread_for_job(execution, move |execution| {
        let state = app.state::<AppState>();
        let outcome = crate::ensure_running_job(&state, execution)
            .and_then(|()| reconcile(&state, &proposal_id, execution.id()));
        match outcome {
            Ok(_) => {
                let job = crate::updated_job(
                    state
                        .jobs
                        .lock()
                        .map_err(|error| error.to_string())?
                        .finish_execution(execution, &[])
                        .map_err(|error| error.to_string())?,
                );
                crate::emit_job(&app, &job);
                history(&state, &proposal_id, 1)
            }
            Err(error) => {
                crate::report_failure(&app, &state, execution, &error);
                Err(error)
            }
        }
    })
    .await
}

/// A record's derived state and its results, newest first.
#[tauri::command]
pub(crate) async fn staged_reconciliation_history(
    proposal_id: String,
    limit: usize,
    app: tauri::AppHandle,
) -> Result<ReconciliationHistory, String> {
    checked_id(&proposal_id)?;
    if !(1..=agents::MAX_RECONCILIATION_HISTORY).contains(&limit) {
        return Err("reconciliation history limit must be 1..=64".into());
    }
    crate::off_ui_thread(move || history(&app.state::<AppState>(), &proposal_id, limit)).await
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "reconcile_tests.rs"]
mod tests;
