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
const AVAILABLE_SPECIALISTS: [SpecialistId; 14] = [
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
    SpecialistId::SpecWriterV1,
    SpecialistId::SpecWriterV2,
    SpecialistId::GapTriagerV1,
    SpecialistId::GapTriagerV2,
];

fn available_specialists() -> Vec<SpecialistId> {
    let mut specialists = AVAILABLE_SPECIALISTS.to_vec();
    if InvestigationPolicy::for_profile("investigation-v2").is_some() {
        specialists.extend([
            SpecialistId::DomainAnalystV10,
            SpecialistId::EvidenceAuditorV10,
            SpecialistId::DomainAnalystV11,
            SpecialistId::DomainAnalystV12,
            SpecialistId::EvidenceAuditorV11,
            SpecialistId::DomainAnalystV18, // Preserve historical catalog entry.
            SpecialistId::DomainAnalystV19,
            SpecialistId::DomainAnalystV20,
            SpecialistId::DomainAnalystV21,
            SpecialistId::DomainAnalystV22,
            SpecialistId::DomainAnalystV23,
        ]);
    }
    specialists
}

fn policy_catalog() -> Vec<InvestigationPolicyCatalogEntry> {
    ["investigation-v1", "investigation-v2"]
        .into_iter()
        .filter_map(|name| {
            let policy = InvestigationPolicy::for_profile(name)?;
            let v2 = name == "investigation-v2";
            Some(InvestigationPolicyCatalogEntry {
                limits: policy.limits(),
                request_schema_version: if v2 { 7 } else { 4 },
                envelope_schema_version: if v2 { 2 } else { 1 },
                envelope_contract: if v2 {
                    "investigation-completion-obligations@2"
                } else {
                    "investigation-completion-obligations@1"
                }
                .into(),
                max_completion_obligations: policy.max_completion_obligations(),
                request_bytes: policy.request_bytes(),
                envelope_bytes: policy.envelope_bytes(),
                specialist_ids: if v2 {
                    vec![
                        SpecialistId::DomainAnalystV10,
                        SpecialistId::EvidenceAuditorV10,
                        SpecialistId::DomainAnalystV11,
                        SpecialistId::DomainAnalystV12,
                        SpecialistId::EvidenceAuditorV11,
                        SpecialistId::DomainAnalystV18,
                        SpecialistId::DomainAnalystV19,
                        SpecialistId::DomainAnalystV20,
                        SpecialistId::DomainAnalystV21,
                        SpecialistId::DomainAnalystV22,
                        SpecialistId::DomainAnalystV23,
                    ]
                } else {
                    vec![
                        SpecialistId::DomainAnalystV7,
                        SpecialistId::EvidenceAuditorV7,
                        SpecialistId::SpecWriterV1,
                        SpecialistId::SpecWriterV2,
                        SpecialistId::GapTriagerV1,
                        SpecialistId::GapTriagerV2,
                    ]
                },
            })
        })
        .collect()
}

#[tauri::command]
pub(crate) async fn investigation_specialists(
    app: tauri::AppHandle,
) -> Result<InvestigationCatalog, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        // The desktop catalog names the saved local model (#1643); an
        // unreadable setting shows the default, and a start reports the error.
        let local_model = state
            .settings
            .lock()
            .ok()
            .and_then(|store| store.local_model().ok())
            .map(|choice| choice.model);
        Ok(InvestigationCatalog {
            schema_version: 1,
            specialists: available_specialists()
                .into_iter()
                .map(|id| id.definition())
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?,
            providers: [
                InvestigationProviderMode::Local,
                InvestigationProviderMode::Cloud,
            ]
            .into_iter()
            .map(|mode| {
                state
                    .investigations
                    .descriptor_for_start(mode, local_model.as_deref())
            })
            .collect(),
            limits: InvestigationLimits::default(),
            policies: policy_catalog(),
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
/// Authenticated transport entrypoint, already on the blocking worker. Authority
/// and admitted scope/slot ownership come from the host, never request fields.
pub(crate) fn start_mcp<R: tauri::Runtime>(
    request: StagedObligationRequest,
    app: tauri::AppHandle<R>,
    session: &McpTaskSession,
    limits: super::mcp_dispatch::TaskReadLimits,
    reservations: &crate::mcp_reads::task_reservation::TaskReservations,
) -> Result<InvestigationSummary, InvestigationStartError> {
    completion_start::start_access(
        request,
        app,
        Instant::now(),
        Some(session),
        Some(limits),
        Some(reservations),
    )
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
    start_access(request, app, started, None)
}

pub(super) fn start_access<R: tauri::Runtime>(
    request: StartInvestigationRequest,
    app: tauri::AppHandle<R>,
    started: Instant,
    session: Option<&McpTaskSession>,
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
    let existing = || {
        let jobs = state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational.to_string())?;
        match session {
            Some(session) => jobs.existing_investigation_access(&request, Some(session)),
            None => jobs.existing_investigation(&request),
        }
        .map_err(|error| error.to_string())
    };
    if (session.is_some() || request.inspection_requirements.is_some())
        && let Some(prior) = existing()?
    {
        return Ok(prior.summary);
    }
    let _target_guards = if request.inspection_requirements.is_some() {
        match super::inspection::validate_targets(&state, &request) {
            Ok(guards) => guards,
            Err(error) => {
                if let Some(prior) = existing()? {
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
    // Desktop starts use the configured local completion model (#1643);
    // MCP keeps the shipped default.
    let local_model =
        if session.is_none() && request.provider_mode == InvestigationProviderMode::Local {
            Some(
                state
                    .settings
                    .lock()
                    .map_err(|e| e.to_string())?
                    .local_model()
                    .map_err(|e| e.to_string())?
                    .model,
            )
        } else {
            None
        };
    let provider = state
        .investigations
        .provider_for_start(request.provider_mode, local_model.as_deref())
        .ok();
    let descriptor = provider
        .as_ref()
        .and_then(|provider| runtime::descriptor(provider.as_ref(), request.provider_mode).ok());
    let committed = {
        let mut jobs = state.jobs.lock().map_err(|error| error.to_string())?;
        match session {
            Some(session) => {
                jobs.start_investigation_access(&request, descriptor.as_ref(), Some(session))
            }
            None => jobs.start_investigation(&request, descriptor.as_ref()),
        }
        .map_err(|error| error.to_string())?
    };
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
    search: Option<String>,
    active_only: Option<bool>,
    app: tauri::AppHandle,
) -> Result<InvestigationPage, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        match (active_only.unwrap_or(false), search.as_deref()) {
            (false, None) => jobs
                .investigation_history(None, cursor.as_deref())
                .map_err(|e| e.to_string()),
            (false, Some(search)) => jobs
                .search_investigation_history(search, cursor.as_deref())
                .map_err(|e| e.to_string()),
            (active_only, search) => jobs
                .investigation_history_selection(search, active_only, cursor.as_deref())
                .map_err(|e| e.to_string()),
        }
    })
    .await
}

/// App-only conversation navigation over durable investigation summaries.
/// Results and citations remain task-scoped and are loaded by the existing
/// selected-investigation commands.
#[tauri::command]
pub(crate) async fn list_investigation_conversation(
    conversation_id: String,
    cursor: Option<String>,
    app: tauri::AppHandle,
) -> Result<InvestigationPage, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_history(Some(&conversation_id), cursor.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub(crate) async fn list_specialist_task_activity(
    cursor: Option<String>,
    app: tauri::AppHandle,
) -> Result<crate::jobs::investigations::SpecialistActivityPage, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .specialist_task_activity(cursor.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalOllamaHealth {
    MetadataReceived,
    Unverified,
    Busy,
}

static LOCAL_OLLAMA_CHECK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
struct LocalOllamaCheck;
impl LocalOllamaCheck {
    fn acquire() -> Option<Self> {
        LOCAL_OLLAMA_CHECK
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .ok()
            .map(|_| Self)
    }
}
impl Drop for LocalOllamaCheck {
    fn drop(&mut self) {
        LOCAL_OLLAMA_CHECK.store(false, std::sync::atomic::Ordering::Release);
    }
}

async fn run_local_ollama_check(
    probe: impl FnOnce() -> bool + Send + 'static,
) -> LocalOllamaHealth {
    let Some(permit) = LocalOllamaCheck::acquire() else {
        return LocalOllamaHealth::Busy;
    };
    crate::off_ui_thread(move || {
        // The physical worker owns the slot, independently of its async waiter.
        let _permit = permit;
        Ok(if probe() {
            LocalOllamaHealth::MetadataReceived
        } else {
            LocalOllamaHealth::Unverified
        })
    })
    .await
    .unwrap_or(LocalOllamaHealth::Unverified)
}

#[tauri::command]
pub(crate) async fn check_local_ollama() -> LocalOllamaHealth {
    run_local_ollama_check(|| {
        llm::OllamaProvider::with_models(
            llm::OllamaProvider::DEFAULT_URL,
            llm::OllamaProvider::DEFAULT_EMBEDDING_MODEL,
            llm::OllamaProvider::DEFAULT_COMPLETION_MODEL,
            llm::bounded::CONNECT_TIMEOUT,
        )
        .and_then(|provider| provider.list_local_models())
        .is_ok()
    })
    .await
}

#[tauri::command]
pub(crate) async fn get_investigation(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<crate::jobs::investigations::InvestigationDetailRead, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation(&investigation_id)
            .and_then(crate::jobs::investigations::InvestigationDetailRead::from_detail)
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

#[tauri::command]
pub(crate) async fn get_investigation_chat(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<Option<crate::jobs::investigations::StoredChatTranscript>, String> {
    crate::off_ui_thread(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_chat(&investigation_id)
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
        let next = jobs
            .investigation(id)
            .map_err(|e| e.to_string())?
            .summary
            .specialist_id
            .uses_next_prompt_admission();
        match (next, approved) {
            (true, true) => {
                jobs.approve_next_prompt_investigation(id, revision, step_id, payload_hash)
            }
            (true, false) => {
                jobs.decline_next_prompt_investigation(id, revision, step_id, payload_hash)
            }
            (false, true) => jobs.approve_investigation(id, revision, step_id, payload_hash),
            (false, false) => jobs.decline_investigation(id, revision, step_id, payload_hash),
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
        let detail = {
            let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
            let next = jobs
                .investigation(&investigation_id)
                .map_err(|e| e.to_string())?
                .summary
                .specialist_id
                .uses_next_prompt_admission();
            if next {
                jobs.cancel_next_prompt_investigation(&investigation_id)
            } else {
                jobs.cancel_investigation(&investigation_id)
            }
            .map_err(|e| e.to_string())?
        };
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
    // rejection path, preserving the UI draft/retry contract for all generations.
    // AC-0286: the current schema-6 family must reach the same command boundary.
    #[test]
    fn completion_ipc_semantic_errors_reach_structured_invalid_input() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        for generation in [3, 4, 5, 6, 7] {
            let mut raw = serde_json::json!({
                "schema_version": generation, "request_nonce": "invalid-semantic",
                "specialist_id": format!("domain-analyst@{}", generation + 3),
                "question": "", "scope": {"type": "all"}, "provider_mode": "local",
                "limit_profile": if generation >= 5 { "investigation-v2" } else { "investigation-v1" }, "expected_context_id": format!("context-v2:{}", "a".repeat(64))
            });
            if generation >= 4 {
                let obligation_version = if generation >= 5 { 2 } else { 1 };
                raw["obligations"] = serde_json::json!({"schema_version": obligation_version,
                    "contract": format!("investigation-completion-obligations@{obligation_version}"), "obligations": []});
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

#[cfg(test)]
mod policy_catalog_tests {
    use super::*;

    #[test]
    fn admission_generation_is_advertised_only_with_registered_policy() {
        // AC-0287: new Expanded starts use the exact request7 family.
        let expanded = policy_catalog()
            .into_iter()
            .find(|p| p.limits.profile == "investigation-v2");
        assert_eq!(
            expanded.is_some(),
            InvestigationPolicy::for_profile("investigation-v2").is_some()
        );
        if let Some(policy) = expanded {
            assert_eq!(policy.request_schema_version, 7);
            assert!(
                policy
                    .specialist_ids
                    .iter()
                    .all(|id| id.uses_next_prompt_admission())
            );
        }
        assert!(!available_specialists().contains(&SpecialistId::DomainAnalystV13));
        assert!(!available_specialists().contains(&SpecialistId::DomainAnalystV14));
        assert!(!available_specialists().contains(&SpecialistId::DomainAnalystV15));
        assert!(!available_specialists().contains(&SpecialistId::DomainAnalystV9));
        assert!(!available_specialists().contains(&SpecialistId::EvidenceAuditorV9));
    }

    #[test]
    fn investigation_policy_catalog_advertises_only_registered_exact_choices() {
        // AC-0286: numeric v2 stays immutable while new starts use the @9 family.
        assert!(!available_specialists().contains(&SpecialistId::DomainAnalystV8));
        assert!(!available_specialists().contains(&SpecialistId::EvidenceAuditorV8));
        // AC-0281: catalog choices cannot activate an unregistered policy.
        assert_eq!(InvestigationLimits::default().profile, "investigation-v1");
        let choices = policy_catalog();
        for choice in &choices {
            let policy = InvestigationPolicy::for_limits(&choice.limits).unwrap();
            assert_eq!(choice.request_bytes, policy.request_bytes());
            assert_eq!(choice.envelope_bytes, policy.envelope_bytes());
            assert_eq!(
                choice.max_completion_obligations,
                policy.max_completion_obligations()
            );
            assert!(
                choice
                    .specialist_ids
                    .iter()
                    .all(|id| available_specialists().contains(id))
            );
            let expanded = policy.profile() == "investigation-v2";
            assert_eq!(choice.request_schema_version, if expanded { 7 } else { 4 });
            assert_eq!(choice.envelope_schema_version, if expanded { 2 } else { 1 });
            assert!(
                choice
                    .specialist_ids
                    .iter()
                    .all(|id| id.uses_policy_selection() == expanded)
            );
        }
        assert_eq!(
            choices
                .iter()
                .any(|p| p.limits.profile == "investigation-v2"),
            InvestigationPolicy::for_profile("investigation-v2").is_some()
        );
        assert_eq!(
            available_specialists().contains(&SpecialistId::DomainAnalystV10),
            InvestigationPolicy::for_profile("investigation-v2").is_some()
        );
        for role in [
            SpecialistId::DomainAnalystV11,
            SpecialistId::DomainAnalystV12,
            SpecialistId::EvidenceAuditorV11,
            SpecialistId::DomainAnalystV18,
            SpecialistId::DomainAnalystV19,
            SpecialistId::DomainAnalystV20,
            SpecialistId::DomainAnalystV21,
            SpecialistId::DomainAnalystV22,
            SpecialistId::DomainAnalystV23,
        ] {
            assert_eq!(
                available_specialists().contains(&role),
                InvestigationPolicy::for_profile("investigation-v2").is_some()
            );
        }
    }

    #[test]
    fn spec_writer_is_listed_only_in_standard_completion_family() {
        let choices = policy_catalog();
        let standard = choices
            .iter()
            .find(|choice| choice.limits.profile == "investigation-v1")
            .unwrap();
        let expanded = choices
            .iter()
            .find(|choice| choice.limits.profile == "investigation-v2");

        assert!(available_specialists().contains(&SpecialistId::SpecWriterV1));
        assert_eq!(standard.request_schema_version, 4);
        assert!(
            standard
                .specialist_ids
                .contains(&SpecialistId::SpecWriterV1)
        );
        assert!(expanded.is_none_or(|choice| {
            choice.request_schema_version != 4
                && !choice.specialist_ids.contains(&SpecialistId::SpecWriterV1)
        }));
    }

    #[test]
    fn gap_triager_is_listed_only_in_standard_completion_family() {
        let choices = policy_catalog();
        let standard = choices
            .iter()
            .find(|choice| choice.limits.profile == "investigation-v1")
            .unwrap();
        let expanded = choices
            .iter()
            .find(|choice| choice.limits.profile == "investigation-v2");
        assert!(available_specialists().contains(&SpecialistId::GapTriagerV1));
        assert_eq!(standard.request_schema_version, 4);
        assert!(
            standard
                .specialist_ids
                .contains(&SpecialistId::GapTriagerV1)
        );
        assert!(
            expanded.is_none_or(|choice| choice.request_schema_version != 4
                && !choice.specialist_ids.contains(&SpecialistId::GapTriagerV1))
        );
    }
}

#[cfg(test)]
#[path = "commands/activation_tests.rs"]
mod activation_tests;

// AC-0422: aborting a waiter cannot refund the slot held by a real blocked HTTP probe.
#[cfg(test)]
#[test]
fn local_ollama_check_slot_survives_waiter_cancellation_and_releases_on_all_exits() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        time::Duration,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (started, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0; 4096];
        let n = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..n]);
        assert!(request.starts_with("GET /api/tags HTTP/1.1\r\n"));
        assert!(!request.to_ascii_lowercase().contains("content-length:"));
        assert!(!request.contains("question") && !request.contains("task"));
        started.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(5)).unwrap();
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{{\"models\":[]}}"
        )
        .unwrap();
    });
    tauri::async_runtime::block_on(async move {
        let waiter = tauri::async_runtime::spawn(run_local_ollama_check(move || {
            llm::OllamaProvider::new(&endpoint, "synthetic", Duration::from_secs(5))
                .unwrap()
                .list_local_models()
                .is_ok()
        }));
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(
            run_local_ollama_check(|| panic!("duplicate probe started")).await,
            LocalOllamaHealth::Busy
        );
        release.send(()).unwrap();
        server.join().unwrap();
        for _ in 0..200 {
            if !LOCAL_OLLAMA_CHECK.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!LOCAL_OLLAMA_CHECK.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(
            run_local_ollama_check(|| false).await,
            LocalOllamaHealth::Unverified
        );
        assert_eq!(
            run_local_ollama_check(|| panic!("synthetic worker failure")).await,
            LocalOllamaHealth::Unverified
        );
        assert_eq!(
            run_local_ollama_check(|| true).await,
            LocalOllamaHealth::MetadataReceived
        );
    });
}
