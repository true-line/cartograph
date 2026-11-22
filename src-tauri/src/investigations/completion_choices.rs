//! Host-owned, metadata-only selection of explicit completion obligations.
use super::{context::FrozenContext, inspection};
use crate::AppState;
use agents::TaskFactKey;
use agents::investigation::{InventoryTarget, InvestigationScope, MAX_MANIFEST_BYTES, Obligation};
use context_hub::coverage::ScopedRepo;
use context_hub::{
    ContextSnapshot, FactReference, QueryRequest, QueryScope,
    coverage::{CoverageQualification, CoverageStatus},
};
use core_graph::{SnapshotReadLimits, coverage::CoverageReadLimits};
use serde::Serialize;
use std::collections::BTreeSet;
use tauri::Manager;

/// Preserve bounded discovery failures until the coordinator assigns a wire code.
#[derive(Debug, thiserror::Error)]
pub(super) enum SelectionError {
    #[error(transparent)]
    Discovery(#[from] crate::mcp_reads::ReadError),
    #[error("{0}")]
    Changed(String),
}

impl From<String> for SelectionError {
    fn from(message: String) -> Self {
        Self::Changed(message)
    }
}

impl SelectionError {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::Discovery(crate::mcp_reads::ReadError::Limit) => "limit_exceeded",
            _ => "refresh_required",
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct InventoryChoice {
    repo: String,
    status: CoverageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    obligation: Option<Obligation>,
}
#[derive(Debug, Serialize)]
pub(crate) struct ObligationChoices {
    schema_version: u32,
    graph_snapshot_id: String,
    context_id: String,
    inventories: Vec<InventoryChoice>,
    #[serde(skip)]
    scoped_facts: Option<BTreeSet<FactReference>>,
}

#[cfg(test)]
impl ObligationChoices {
    /// The scope's fact membership, which never enters the selection wire.
    pub(super) fn scoped_facts(&self) -> Option<&BTreeSet<FactReference>> {
        self.scoped_facts.as_ref()
    }
}

pub(super) fn choices(
    graph: (Vec<core_graph::Node>, Vec<core_graph::Edge>),
    coverage: CoverageQualification,
    scope: &InvestigationScope,
    area: Option<&BTreeSet<String>>,
) -> Result<ObligationChoices, String> {
    scope.validate().map_err(|error| error.to_string())?;
    let complete = ContextSnapshot::new(graph.0.clone(), graph.1.clone())
        .map_err(|error| error.to_string())?;
    // Source targets bind the full graph; context identity binds the requested
    // scope under the exact same qualified projection used at task preparation.
    let graph_snapshot_id = complete.id().to_owned();
    let scoped = FrozenContext::scoped_snapshot(&graph, scope, area, complete)
        .map_err(|error| error.to_string())?;
    choices_in(graph_snapshot_id, scoped, coverage, scope)
}

/// The choices for `scoped`, the requested scope's projection, whose source
/// targets bind the complete graph `graph_snapshot_id`.
fn choices_in(
    graph_snapshot_id: String,
    scoped: ContextSnapshot,
    coverage: CoverageQualification,
    scope: &InvestigationScope,
) -> Result<ObligationChoices, String> {
    // Keep membership from this exact authorized projection, not merely its
    // full-graph identity. This metadata never enters the selection wire.
    let scoped_facts = match scope {
        InvestigationScope::Node { .. } => return Err("Invalid task scope.".into()),
        // Full-graph source existence is already checked by inspection::resolve
        // before start. Only a narrower scope needs its own membership index.
        InvestigationScope::All => None,
        InvestigationScope::Neighborhood { .. } | InvestigationScope::Area { .. } => {
            let mut facts = BTreeSet::new();
            let mut cursor = None;
            loop {
                let page = scoped
                    .query(QueryRequest {
                        scope: QueryScope::All,
                        kind: None,
                        labels: vec![],
                        max_facts: context_hub::MAX_FACTS,
                        max_bytes: context_hub::MAX_RESPONSE_BYTES,
                        cursor,
                    })
                    .map_err(|error| error.to_string())?;
                facts.extend(page.facts.into_iter().map(|fact| fact.reference));
                let Some(next) = page.next_cursor else {
                    break;
                };
                cursor = Some(next);
            }
            Some(facts)
        }
    };
    let qualified = scoped
        .with_coverage(coverage)
        .map_err(|error| error.to_string())?;
    let proof = qualified
        .durable_proof()
        .map_err(|error| error.to_string())?;
    let inventories = proof
        .qualification
        .repos
        .iter()
        .map(|repo| {
            let obligation = if repo.status == CoverageStatus::Recorded {
                let mut obligation = Obligation::Inventory {
                    obligation_id: String::new(),
                    target: InventoryTarget {
                        context_id: proof.context_id.clone(),
                        repo: repo.repo.clone(),
                        inventory_hash: repo
                            .inventory_hash
                            .clone()
                            .ok_or("Recorded inventory identity is missing.")?,
                    },
                };
                let identity = obligation.content_id().map_err(|error| error.to_string())?;
                if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
                    *obligation_id = identity;
                }
                Some(obligation)
            } else {
                None
            };
            Ok(InventoryChoice {
                repo: repo.repo.clone(),
                status: repo.status,
                obligation,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let result = ObligationChoices {
        schema_version: 1,
        graph_snapshot_id,
        context_id: proof.context_id,
        inventories,
        scoped_facts,
    };
    // Never truncate choices or reinterpret omitted entries as unavailable.
    super::context::bounded_json(&result, MAX_MANIFEST_BYTES).map_err(|error| error.to_string())?;
    Ok(result)
}

#[tauri::command]
pub(crate) async fn investigation_obligation_choices(
    scope: InvestigationScope,
    app: tauri::AppHandle,
) -> Result<ObligationChoices, String> {
    scope.validate().map_err(|error| error.to_string())?;
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        current_choices(&app, &state, &scope)
    })
    .await
}

#[tauri::command]
pub(crate) async fn resolve_investigation_source_obligation(
    fact: TaskFactKey,
    expected_graph_snapshot_id: String,
    expected_receipt_id: String,
    inventory_index: usize,
    app: tauri::AppHandle,
) -> Result<Obligation, String> {
    crate::off_ui_thread(move || {
        let (target, _guard) = inspection::resolve(
            &app.state::<AppState>(),
            &fact,
            &expected_graph_snapshot_id,
            &expected_receipt_id,
            inventory_index,
        )?;
        let mut obligation = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(target),
        };
        let identity = obligation.content_id().map_err(|error| error.to_string())?;
        if let Obligation::Source { obligation_id, .. } = &mut obligation {
            *obligation_id = identity;
        }
        Ok(obligation)
    })
    .await
}

fn current_choices<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    scope: &InvestigationScope,
) -> Result<ObligationChoices, String> {
    current_choices_with_scope(app, state, scope, None).map_err(|error| error.to_string())
}

fn current_choices_with_scope<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    scope: &InvestigationScope,
    limits: Option<crate::mcp_reads::scope::ScopeLimits>,
) -> Result<ObligationChoices, SelectionError> {
    let read_scope = || match limits {
        Some(limits) => crate::mcp_reads::scope::coverage_scope(app, state, limits)
            .map_err(SelectionError::from),
        None => crate::context::coverage_scope(app, state).map_err(SelectionError::from),
    };
    choices_at(
        state,
        scope,
        super::context::task_graph_limits(limits.is_some()),
        read_scope,
    )
}

/// The current choices for `scope`, with the coverage scope read before and
/// after the graph read so a change between them is never qualified.
pub(super) fn choices_at(
    state: &AppState,
    scope: &InvestigationScope,
    graph_limits: SnapshotReadLimits,
    mut read_scope: impl FnMut() -> Result<Vec<ScopedRepo>, SelectionError>,
) -> Result<ObligationChoices, SelectionError> {
    let before = read_scope()?;
    if super::context::scope_aware(scope).map_err(|error| error.to_string())? {
        // #1602 option A (AC-1311): a neighborhood or area reads only its
        // scope, at the coverage revision, with the complete-graph identity
        // streamed inside the same scoped reader, as task preparation does.
        let (revision, recorded) = state
            .graph
            .lock()
            .map_err(|_| "Graph store is unavailable.".to_owned())?
            .read_coverage_records_with_revision_bounded(CoverageReadLimits::default())
            .map_err(|error| error.to_string())?;
        let area =
            super::area::files(state, &revision, scope).map_err(|error| error.to_string())?;
        let (graph_snapshot_id, scoped) =
            FrozenContext::scoped_snapshot_at(state, scope, area.as_ref(), graph_limits, &revision)
                .map_err(|error| error.to_string())?;
        let stable = crate::context::stable_scope(&before, read_scope()?);
        let coverage =
            CoverageQualification::new(&stable, &recorded).map_err(|error| error.to_string())?;
        return choices_in(graph_snapshot_id, scoped, coverage, scope)
            .map_err(SelectionError::from);
    }
    let (base, recorded) = state
        .graph
        .lock()
        .map_err(|_| "Graph store is unavailable.".to_owned())?
        .read_snapshot_with_coverage_and_revision_bounded(
            graph_limits,
            CoverageReadLimits::default(),
        )
        .map_err(|error| error.to_string())?;
    let area =
        super::area::files(state, &base.revision, scope).map_err(|error| error.to_string())?;
    let graph = base.graph;
    let stable = crate::context::stable_scope(&before, read_scope()?);
    let coverage =
        CoverageQualification::new(&stable, &recorded).map_err(|error| error.to_string())?;
    choices(graph, coverage, scope, area.as_ref()).map_err(SelectionError::from)
}

/// Acceptance preparation reuses the production qualified selection, requiring
/// exactly one recorded repository rather than selecting an arbitrary entry.
#[cfg(test)]
pub(super) fn control_inventory<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    scope: &InvestigationScope,
) -> Result<Obligation, String> {
    let choices = current_choices(app, state, scope)?;
    if choices.inventories.len() != 1 {
        return Err("Control requires exactly one repository qualification.".into());
    }
    let choice = choices
        .inventories
        .into_iter()
        .next()
        .ok_or("Control inventory is missing.")?;
    if choice.status != CoverageStatus::Recorded {
        return Err("Control inventory must be Recorded.".into());
    }
    choice
        .obligation
        .ok_or_else(|| "Control inventory identity is missing.".into())
}

pub(super) fn validate_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    request: &agents::investigation::StagedObligationRequest,
) -> Result<(), String> {
    validate_request_with_scope(app, state, request, None).map_err(|error| error.to_string())
}

pub(super) fn validate_request_with_scope<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    request: &agents::investigation::StagedObligationRequest,
    limits: Option<crate::mcp_reads::scope::ScopeLimits>,
) -> Result<(), SelectionError> {
    let current = current_choices_with_scope(app, state, &request.scope, limits)?;
    validate_selection(&current, request).map_err(SelectionError::from)
}

fn validate_selection(
    current: &ObligationChoices,
    request: &agents::investigation::StagedObligationRequest,
) -> Result<(), String> {
    if current.context_id != request.expected_context_id
        || request
            .expected_graph_revision
            .as_ref()
            .is_some_and(|expected| expected != &current.graph_snapshot_id)
    {
        return Err("Completion context changed; refresh the selection.".into());
    }
    for obligation in &request.obligations.obligations {
        match obligation {
            Obligation::Inventory { .. }
                if !current.inventories.iter().any(|choice| {
                    choice.status == CoverageStatus::Recorded
                        && choice.obligation.as_ref() == Some(obligation)
                }) =>
            {
                return Err("Completion inventory changed; refresh the selection.".into());
            }
            Obligation::Source { target, .. } => {
                if target.graph_snapshot_id != current.graph_snapshot_id {
                    return Err("Completion source graph changed; refresh the selection.".into());
                }
                let fact = match &target.fact {
                    TaskFactKey::Node { id } => FactReference::Node { id: id.clone() },
                    TaskFactKey::Edge {
                        source,
                        label,
                        destination,
                    } => FactReference::Edge {
                        source: source.clone(),
                        label: label.clone(),
                        destination: destination.clone(),
                    },
                };
                if current
                    .scoped_facts
                    .as_ref()
                    .is_some_and(|facts| !facts.contains(&fact))
                {
                    return Err("Completion source is outside the requested scope; select an in-scope source or change the scope.".into());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::investigation::{
        InvestigationProviderMode, ObligationEnvelope, StagedObligationRequest,
    };

    // AC-0266: an unrecorded repository supplies no selectable inventory authority.
    #[test]
    fn completion_choices_never_offer_unrecorded_inventory() {
        let scope = vec![ScopedRepo {
            repo_key: "app/service".into(),
            registered_source_id: None,
            config_hash: "a".repeat(64),
            layer_hints: None,
        }];
        let coverage = CoverageQualification::new(&scope, &[]).unwrap();
        let result = choices((vec![], vec![]), coverage, &InvestigationScope::All, None).unwrap();
        assert_eq!(result.inventories.len(), 1);
        assert_eq!(result.inventories[0].status, CoverageStatus::Unrecorded);
        assert!(result.inventories[0].obligation.is_none());
        let json = serde_json::to_value(result).unwrap();
        assert!(json["inventories"][0].get("obligation").is_none());
    }

    // AC-0266: membership is the exact node/edge projection, not just the full
    // graph hash or presence of an edge's endpoints.
    #[test]
    fn completion_source_selection_checks_exact_scoped_nodes_and_edges() {
        use core_graph::{Edge, Node};
        let graph = (
            (["a", "b", "c", "d"]
                .into_iter()
                .map(|id| Node {
                    id: id.into(),
                    label: "Rule".into(),
                    props: serde_json::json!({}),
                })
                .collect()),
            vec![
                Edge {
                    src: "a".into(),
                    dst: "b".into(),
                    label: "CALLS".into(),
                    props: serde_json::json!({}),
                },
                Edge {
                    src: "c".into(),
                    dst: "d".into(),
                    label: "CALLS".into(),
                    props: serde_json::json!({}),
                },
            ],
        );
        let scope = InvestigationScope::Neighborhood {
            anchor: "a".into(),
            hops: 1,
        };
        let all = choices(
            graph.clone(),
            CoverageQualification::new(&[], &[]).unwrap(),
            &InvestigationScope::All,
            None,
        )
        .unwrap();
        assert!(all.scoped_facts.is_none());
        let current = choices(
            graph,
            CoverageQualification::new(&[], &[]).unwrap(),
            &scope,
            None,
        )
        .unwrap();
        assert!(current.scoped_facts.is_some());
        assert!(
            serde_json::to_value(&current)
                .unwrap()
                .get("scoped_facts")
                .is_none()
        );
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../ui/src/fixtures/investigation-inspection-backend.json"
        ))
        .unwrap();
        for (fact, allowed) in [
            (TaskFactKey::Node { id: "a".into() }, true),
            (TaskFactKey::Node { id: "c".into() }, false),
            (
                TaskFactKey::Edge {
                    source: "a".into(),
                    label: "CALLS".into(),
                    destination: "b".into(),
                },
                true,
            ),
            (
                TaskFactKey::Edge {
                    source: "c".into(),
                    label: "CALLS".into(),
                    destination: "d".into(),
                },
                false,
            ),
            (
                TaskFactKey::Edge {
                    source: "a".into(),
                    label: "OTHER".into(),
                    destination: "b".into(),
                },
                false,
            ),
        ] {
            let mut target: agents::investigation::InvestigationInspectionTarget =
                serde_json::from_value(fixture["target"].clone()).unwrap();
            target.fact_digest = format!(
                "{}:{}",
                if matches!(fact, TaskFactKey::Node { .. }) {
                    "node-v1"
                } else {
                    "edge-v1"
                },
                "a".repeat(64)
            );
            target.fact = fact;
            target.graph_snapshot_id = current.graph_snapshot_id.clone();
            target.target_id = target.content_id().unwrap();
            let mut obligation = Obligation::Source {
                obligation_id: String::new(),
                target: Box::new(target),
            };
            let identity = obligation.content_id().unwrap();
            if let Obligation::Source { obligation_id, .. } = &mut obligation {
                *obligation_id = identity;
            }
            let request = StagedObligationRequest {
                schema_version: 4,
                request_nonce: "scope-membership".into(),
                specialist_id: "domain-analyst@7".into(),
                question: "Inspect a scoped source.".into(),
                scope: scope.clone(),
                provider_mode: InvestigationProviderMode::Local,
                limit_profile: "investigation-v1".into(),
                expected_graph_revision: Some(current.graph_snapshot_id.clone()),
                expected_context_id: current.context_id.clone(),
                conversation_id: None,
                parent_id: None,
                inspection_requirements: None,
                obligations: ObligationEnvelope {
                    schema_version: 1,
                    contract: "investigation-completion-obligations@1".into(),
                    obligations: vec![obligation],
                },
            }
            .normalized()
            .unwrap();
            assert_eq!(validate_selection(&current, &request).is_ok(), allowed);
            // AC-0266: All adds no membership index; exact source existence is
            // still resolved by the production start path before admission.
            if !matches!(request.obligations.sources().unwrap()[0].fact,
                TaskFactKey::Edge { ref label, .. } if label == "OTHER")
            {
                let mut all_request = request.clone();
                all_request.scope = InvestigationScope::All;
                all_request.expected_context_id = all.context_id.clone();
                assert!(validate_selection(&all, &all_request).is_ok());
            }
        }
    }

    // AC-0266: selectable inventory identity is compared in full at start.
    #[test]
    fn completion_selection_rejects_changed_context_inventory_and_status() {
        let context = format!("context-v2:{}", "a".repeat(64));
        let mut obligation = Obligation::Inventory {
            obligation_id: String::new(),
            target: InventoryTarget {
                context_id: context.clone(),
                repo: "app/service".into(),
                inventory_hash: "b".repeat(64),
            },
        };
        let identity = obligation.content_id().unwrap();
        if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
            *obligation_id = identity;
        }
        let request = StagedObligationRequest {
            schema_version: 4,
            request_nonce: "selection-test".into(),
            specialist_id: "domain-analyst@7".into(),
            question: "Inspect selected inventory".into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: "investigation-v1".into(),
            expected_graph_revision: None,
            expected_context_id: context.clone(),
            conversation_id: None,
            parent_id: None,
            inspection_requirements: None,
            obligations: ObligationEnvelope {
                schema_version: 1,
                contract: "investigation-completion-obligations@1".into(),
                obligations: vec![obligation.clone()],
            },
        };
        // Tauri deserializes command arguments from a JSON Value. Exercise that
        // path as well as the core raw-string decoder before enabling the IPC.
        let wire = serde_json::to_value(&request).unwrap();
        assert!(matches!(
            serde_json::from_value::<agents::investigation::InvestigationStartRequest>(wire),
            Ok(agents::investigation::InvestigationStartRequest::Completion(_))
        ));
        let mut current = ObligationChoices {
            schema_version: 1,
            context_id: context,
            graph_snapshot_id: "c".repeat(64),
            scoped_facts: None,
            inventories: vec![InventoryChoice {
                repo: "app/service".into(),
                status: CoverageStatus::Recorded,
                obligation: Some(obligation),
            }],
        };
        validate_selection(&current, &request).unwrap();
        current.inventories[0].status = CoverageStatus::Stale;
        assert!(validate_selection(&current, &request).is_err());
        current.inventories[0].status = CoverageStatus::Recorded;
        if let Some(Obligation::Inventory { target, .. }) = &mut current.inventories[0].obligation {
            target.inventory_hash = "d".repeat(64);
        }
        assert!(validate_selection(&current, &request).is_err());
        current.inventories[0].obligation = Some(request.obligations.obligations[0].clone());
        current.context_id = format!("context-v2:{}", "e".repeat(64));
        assert!(validate_selection(&current, &request).is_err());
    }
}
