//! Thin IPC boundary. Durable reads and filesystem/provider preparation stay
//! off the UI thread; callers supply IDs, never leases or source payloads.
#[path = "completion_start.rs"]
mod completion_start;

use super::{HostError, emit_changed, history, runtime, worker};
use crate::{AppState, jobs::ClaimMode};
use agents::investigation::*;
use std::time::Instant;
use tauri::Manager;

// Shared by the production catalog and staged-version boundary test.
const AVAILABLE_SPECIALISTS: [SpecialistId; 10] = [
    SpecialistId::DomainAnalystV3,
    SpecialistId::EvidenceAuditorV3,
    SpecialistId::DomainAnalystV4,
    SpecialistId::EvidenceAuditorV4,
    SpecialistId::DomainAnalystV5,
    SpecialistId::EvidenceAuditorV5,
    SpecialistId::DomainAnalystV6,
    SpecialistId::EvidenceAuditorV6,
    SpecialistId::DomainAnalystV7,
    SpecialistId::EvidenceAuditorV7,
];

#[tauri::command]
pub(crate) async fn investigation_specialists(
    app: tauri::AppHandle,
) -> Result<InvestigationCatalog, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        Ok(InvestigationCatalog {
            schema_version: 1,
            specialists: AVAILABLE_SPECIALISTS
                .into_iter()
                .map(|id| id.definition())
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?,
            providers: [
                InvestigationProviderMode::Local,
                InvestigationProviderMode::Cloud,
            ]
            .into_iter()
            .map(|mode| state.investigations.descriptor(mode))
            .collect(),
            limits: InvestigationLimits::default(),
        })
    })
    .await
}

#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum InvestigationStartError {
    Legacy(String),
    Rejected {
        code: &'static str,
        message: String,
        definitive: bool,
    },
}
impl From<String> for InvestigationStartError {
    fn from(error: String) -> Self {
        Self::Legacy(error)
    }
}
fn rejected(code: &'static str, message: impl ToString) -> InvestigationStartError {
    InvestigationStartError::Rejected {
        code,
        message: message.to_string(),
        definitive: true,
    }
}
#[tauri::command]
pub(crate) async fn start_investigation(
    request: InvestigationStartRequest,
    app: tauri::AppHandle,
) -> Result<InvestigationSummary, InvestigationStartError> {
    let started = Instant::now();
    tauri::async_runtime::spawn_blocking(move || match request {
        InvestigationStartRequest::Legacy(request) => start(request, app, started),
        InvestigationStartRequest::Completion(request) => {
            completion_start::start(request, app, started)
        }
    })
    .await
    .map_err(|e| InvestigationStartError::Legacy(e.to_string()))?
}

pub(super) fn start<R: tauri::Runtime>(
    request: StartInvestigationRequest,
    app: tauri::AppHandle<R>,
    started: Instant,
) -> Result<InvestigationSummary, InvestigationStartError> {
    let structured_errors = request.schema_version >= 2
        || request.specialist_id.uses_inspection_requirements()
        || request.specialist_id.uses_coverage_qualification();
    let request = request.normalized().map_err(|e| {
        if structured_errors {
            rejected("invalid_input", e)
        } else {
            InvestigationStartError::Legacy(e.to_string())
        }
    })?;
    let state = app.state::<AppState>();
    let _target_guards = if request.inspection_requirements.is_some() {
        if let Some(prior) = state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational.to_string())?
            .existing_investigation(&request)
            .map_err(|e| e.to_string())?
        {
            return Ok(prior.summary);
        }
        match super::inspection::validate_targets(&state, &request) {
            Ok(guards) => guards,
            Err(error) => {
                if let Some(prior) = state
                    .jobs
                    .lock()
                    .map_err(|_| HostError::Operational.to_string())?
                    .existing_investigation(&request)
                    .map_err(|e| e.to_string())?
                {
                    return Ok(prior.summary);
                }
                return Err(rejected("refresh_required", error));
            }
        }
    } else {
        Vec::new()
    };
    // This resolves app-lifetime configuration only, without network activity.
    // An unavailable provider is passed through: durable deduplication must win
    // over a later configuration change for an already committed request.
    let provider = state.investigations.provider(request.provider_mode).ok();
    let descriptor = provider
        .as_ref()
        .and_then(|provider| runtime::descriptor(provider.as_ref(), request.provider_mode).ok());
    let committed = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .start_investigation(&request, descriptor.as_ref())
        .map_err(|e| e.to_string())?;
    if !committed.created {
        return Ok(committed.detail.summary);
    }
    emit_changed(&app, &committed.detail);
    let id = committed.detail.summary.investigation_id.clone();
    let provider = provider.ok_or_else(|| HostError::Operational.to_string())?;
    let live = state
        .investigations
        .insert(&id)
        .map_err(|e| e.to_string())?;
    let claim = (|| {
        let plan = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .claim_plan(committed.detail.summary.job_id, ClaimMode::StartQueued)
            .map_err(|e| e.to_string())?;
        crate::claim_job(&state, &plan)
    })();
    let (_, execution) = match claim {
        Ok(claim) => claim,
        Err(error) => {
            state.investigations.remove(&id);
            // The nonce still recovers the committed identity. No second worker
            // or automatic re-claim is allowed after an uncertain start.
            return Err(error.into());
        }
    };
    // Keep the execution in the detached blocking worker even if the invoking
    // async waiter goes away. Persisted history is the response authority.
    let worker_app = app.clone();
    let worker_id = id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        worker::run(
            worker_app, worker_id, request, execution, provider, live, started,
        );
    });
    let detail = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .investigation(&id)
        .map_err(|e| e.to_string())?;
    emit_changed(&app, &detail);
    Ok(detail.summary)
}

#[tauri::command]
pub(crate) async fn list_investigations(
    cursor: Option<String>,
    app: tauri::AppHandle,
) -> Result<InvestigationPage, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_history(None, cursor.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_investigation(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<InvestigationDetail, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation(&investigation_id)
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn investigation_events(
    investigation_id: String,
    after_sequence: Option<u64>,
    app: tauri::AppHandle,
) -> Result<InvestigationEventPage, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_events(&investigation_id, after_sequence.unwrap_or(0))
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn investigation_result(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<Option<InvestigationResult>, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_result(&investigation_id)
            .map_err(|e| e.to_string())
    })
    .await
}

fn pending(state: &AppState, id: &str) -> Result<Option<InvestigationConsent>, String> {
    let Some(live) = state.investigations.get(id).map_err(|e| e.to_string())? else {
        // A different or restarted process cannot reconstruct the raw preview.
        return Ok(None);
    };
    let preview = live
        .pending
        .lock()
        .map_err(|_| HostError::Operational.to_string())?
        .clone();
    let Some(preview) = preview else {
        return Ok(None);
    };
    let durable = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .investigation_pending_step(id)
        .map_err(|e| e.to_string())?;
    Ok(durable
        .filter(|(revision, step, approved)| {
            !approved
                && *revision == preview.revision
                && id == preview.investigation_id
                && step.step_id == preview.step_id
                && step.payload_hash == preview.preview.payload_hash
        })
        .map(|_| preview))
}

#[tauri::command]
pub(crate) async fn investigation_consent(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<Option<InvestigationConsent>, String> {
    crate::off_ui_thread(move || pending(&app.state::<AppState>(), &investigation_id)).await
}

fn consent_decision<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: &str,
    step_id: &str,
    revision: u64,
    payload_hash: &str,
    approved: bool,
) -> Result<InvestigationSummary, String> {
    let state = app.state::<AppState>();
    let preview = pending(&state, id)?.ok_or_else(|| {
        "This exact step preview is no longer available. Reload the investigation.".to_string()
    })?;
    if preview.step_id != step_id
        || preview.revision != revision
        || preview.preview.payload_hash != payload_hash
    {
        return Err("The investigation step changed. Reload its current preview.".into());
    }
    let detail = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        if approved {
            jobs.approve_investigation(id, revision, step_id, payload_hash)
        } else {
            jobs.decline_investigation(id, revision, step_id, payload_hash)
        }
        .map_err(|e| e.to_string())?
    };
    state.investigations.wake(id);
    emit_changed(app, &detail);
    Ok(detail.summary)
}

#[tauri::command]
pub(crate) async fn approve_investigation_step(
    investigation_id: String,
    step_id: String,
    revision: u64,
    payload_hash: String,
    app: tauri::AppHandle,
) -> Result<InvestigationSummary, String> {
    crate::off_ui_thread(move || {
        consent_decision(
            &app,
            &investigation_id,
            &step_id,
            revision,
            &payload_hash,
            true,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn decline_investigation_step(
    investigation_id: String,
    step_id: String,
    revision: u64,
    payload_hash: String,
    app: tauri::AppHandle,
) -> Result<InvestigationSummary, String> {
    crate::off_ui_thread(move || {
        consent_decision(
            &app,
            &investigation_id,
            &step_id,
            revision,
            &payload_hash,
            false,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn cancel_investigation(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<InvestigationSummary, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let detail = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .cancel_investigation(&investigation_id)
            .map_err(|e| e.to_string())?;
        state.investigations.wake(&investigation_id);
        emit_changed(&app, &detail);
        Ok(detail.summary)
    })
    .await
}

#[tauri::command]
pub(crate) async fn read_investigation_citation(
    investigation_id: String,
    citation_id: String,
    app: tauri::AppHandle,
) -> Result<InvestigationCitationRead, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let ledger = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_ledger(&investigation_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "This investigation has no admitted citations.".to_string())?;
        history::read(&state, &investigation_id, &ledger, &citation_id).map_err(|e| e.to_string())
    })
    .await
}

#[cfg(test)]
mod staged_tests {
    use super::*;

    // AC-0266: shape-valid IPC arguments keep semantic errors on the structured
    // rejection path, preserving the UI draft/retry contract for both generations.
    #[test]
    fn completion_ipc_semantic_errors_reach_structured_invalid_input() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        for generation in [3, 4] {
            let mut raw = serde_json::json!({
                "schema_version": generation, "request_nonce": "invalid-semantic",
                "specialist_id": if generation == 4 { "domain-analyst@7" } else { "domain-analyst@6" },
                "question": "", "scope": {"type": "all"}, "provider_mode": "local",
                "limit_profile": "investigation-v1", "expected_context_id": format!("context-v2:{}", "a".repeat(64))
            });
            if generation == 4 {
                raw["obligations"] = serde_json::json!({"schema_version": 1,
                    "contract": "investigation-completion-obligations@1", "obligations": []});
            }
            // Tauri supplies a parsed Value to this exact command argument type.
            let request = serde_json::from_value::<InvestigationStartRequest>(raw.clone()).unwrap();
            assert!(InvestigationStartRequest::decode(&raw.to_string()).is_err());
            let result = match request {
                InvestigationStartRequest::Legacy(request) => {
                    start(request, app.handle().clone(), Instant::now())
                }
                InvestigationStartRequest::Completion(request) => {
                    completion_start::start(request, app.handle().clone(), Instant::now())
                }
            };
            assert!(
                matches!(
                    result,
                    Err(InvestigationStartError::Rejected {
                        code: "invalid_input",
                        definitive: true,
                        ..
                    })
                ),
                "generation {generation}: {result:?}"
            );
            raw["unexpected_alias"] = serde_json::json!(true);
            assert!(serde_json::from_value::<InvestigationStartRequest>(raw).is_err());
        }
    }

    // AC-0264, AC-0266: final activation requires request4 authority; legacy starts remain closed.
    #[test]
    fn v7_catalog_requires_completion_contract_and_rejects_legacy_start() {
        for id in ["domain-analyst@7", "evidence-auditor@7"] {
            assert!(
                AVAILABLE_SPECIALISTS
                    .iter()
                    .any(|specialist| serde_json::to_value(specialist).unwrap() == id)
            );
            let raw = serde_json::json!({"schema_version":4,"request_nonce":"staged-boundary",
                "specialist_id":id,"question":"Inspect selected evidence","scope":{"type":"all"},
                "provider_mode":"local","limit_profile":"investigation-v1"});
            // AC-0266: the generation-discriminated IPC cannot accept V7 without
            // the required completion envelope/context; legacy start never gains
            // completion authority just because SpecialistId recognizes V7.
            assert!(serde_json::from_value::<InvestigationStartRequest>(raw.clone()).is_err());
            let mut legacy = raw.clone();
            legacy["schema_version"] = serde_json::json!(3);
            assert!(
                serde_json::from_value::<StartInvestigationRequest>(legacy)
                    .unwrap()
                    .validate()
                    .is_err()
            );
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
            let mut complete = raw.clone();
            complete["expected_context_id"] = serde_json::json!(context);
            complete["obligations"] = serde_json::json!({"schema_version":1,
                "contract":"investigation-completion-obligations@1", "obligations":[obligation]});
            assert!(matches!(
                serde_json::from_value::<InvestigationStartRequest>(complete),
                Ok(InvestigationStartRequest::Completion(_))
            ));
            let mut known = raw;
            known["specialist_id"] = serde_json::json!("domain-analyst@6");
            known["schema_version"] = serde_json::json!(3);
            serde_json::from_value::<StartInvestigationRequest>(known)
                .unwrap()
                .validate()
                .unwrap();
        }
        // Even a recognized role cannot smuggle staged request generation 4
        // through the actual coordinator entrypoint. It rejects before AppState.
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let request: StartInvestigationRequest = serde_json::from_value(serde_json::json!({
            "schema_version":4,"request_nonce":"staged-boundary","specialist_id":"domain-analyst@6",
            "question":"Inspect selected evidence","scope":{"type":"all"},"provider_mode":"local",
            "limit_profile":"investigation-v1"}))
        .unwrap();
        let mut known = request.clone();
        known.schema_version = 3;
        known.validate().unwrap();
        assert!(matches!(
            start(request, app.handle().clone(), Instant::now()),
            Err(InvestigationStartError::Rejected {
                code: "invalid_input",
                definitive: true,
                ..
            })
        ));
    }
}
