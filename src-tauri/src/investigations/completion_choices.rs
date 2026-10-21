//! Host-owned, metadata-only selection of explicit completion obligations.
use super::{context::FrozenContext, inspection};
use crate::AppState;
use agents::TaskFactKey;
use agents::investigation::{InventoryTarget, InvestigationScope, MAX_MANIFEST_BYTES, Obligation};
use context_hub::{
    ContextSnapshot,
    coverage::{CoverageQualification, CoverageStatus},
};
use core_graph::{SnapshotReadLimits, coverage::CoverageReadLimits};
use serde::Serialize;
use tauri::Manager;

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
}

fn choices(
    graph: (Vec<core_graph::Node>, Vec<core_graph::Edge>),
    coverage: CoverageQualification,
    scope: &InvestigationScope,
) -> Result<ObligationChoices, String> {
    scope.validate().map_err(|error| error.to_string())?;
    let complete = ContextSnapshot::new(graph.0.clone(), graph.1.clone())
        .map_err(|error| error.to_string())?;
    // Source targets bind the full graph; context identity binds the requested
    // scope under the exact same qualified projection used at task preparation.
    let graph_snapshot_id = complete.id().to_owned();
    let scoped = FrozenContext::scoped_snapshot(&graph, scope, complete)
        .map_err(|error| error.to_string())?;
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
    let before = crate::context::coverage_scope(app, state)?;
    let (graph, recorded) = state
        .graph
        .lock()
        .map_err(|_| "Graph store is unavailable.")?
        .read_snapshot_with_coverage_bounded(
            SnapshotReadLimits::default(),
            CoverageReadLimits::default(),
        )
        .map_err(|error| error.to_string())?;
    let stable = crate::context::stable_scope(&before, crate::context::coverage_scope(app, state)?);
    let coverage =
        CoverageQualification::new(&stable, &recorded).map_err(|error| error.to_string())?;
    choices(graph, coverage, scope)
}

pub(super) fn validate_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    request: &agents::investigation::StagedObligationRequest,
) -> Result<(), String> {
    let current = current_choices(app, state, &request.scope)?;
    validate_selection(&current, request)
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
            Obligation::Source { target, .. }
                if target.graph_snapshot_id != current.graph_snapshot_id =>
            {
                return Err("Completion source graph changed; refresh the selection.".into());
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
    use context_hub::coverage::ScopedRepo;

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
        let result = choices((vec![], vec![]), coverage, &InvestigationScope::All).unwrap();
        assert_eq!(result.inventories.len(), 1);
        assert_eq!(result.inventories[0].status, CoverageStatus::Unrecorded);
        assert!(result.inventories[0].obligation.is_none());
        let json = serde_json::to_value(result).unwrap();
        assert!(json["inventories"][0].get("obligation").is_none());
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
