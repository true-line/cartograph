//! Thin IPC boundary. Durable reads and filesystem/provider preparation stay
//! off the UI thread; callers supply IDs, never leases or source payloads.
#[path = "completion_start.rs"]
mod completion_start;

use super::{HostError, emit_changed, history, runtime, worker};
use crate::AppState;
use agents::investigation::*;
use std::time::Instant;
use tauri::Manager;

// The catalog is the capability table's offered rows (ADR-0071, #1751).
fn available_specialists() -> Vec<SpecialistId> {
    offered_specialists()
}

#[cfg(test)]
thread_local! {
    static RETIRED_STARTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test-only: run `f` on this thread with retired versions startable, so the
/// tests of a retired version's own worker behaviour keep running (AC-1062).
/// Production builds have no such switch; every other test sees the gate.
#[cfg(test)]
pub(crate) fn with_retired_starts<T>(f: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            RETIRED_STARTS.with(|flag| flag.set(self.0));
        }
    }
    let _reset = Reset(RETIRED_STARTS.with(|flag| flag.replace(true)));
    f()
}

/// Test-only: run `f` with the real gate, inside a retired-start scope.
#[cfg(test)]
pub(crate) fn without_retired_starts<T>(f: impl FnOnce() -> T) -> T {
    let was = RETIRED_STARTS.with(|flag| flag.replace(false));
    let result = f();
    RETIRED_STARTS.with(|flag| flag.set(was));
    result
}

#[cfg(test)]
fn retired_starts_allowed() -> bool {
    RETIRED_STARTS.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn retired_starts_allowed() -> bool {
    false
}

/// The one availability refusal every start path uses (ADR-0071 decision 2).
fn offered_for_start(id: SpecialistId) -> Result<(), InvestigationStartError> {
    if available_specialists().contains(&id)
        || (retired_starts_allowed() && id.availability() == Availability::Retired)
    {
        Ok(())
    } else {
        Err(rejected(
            "invalid_input",
            "This specialist version is not available.",
        ))
    }
}

/// ADR-0085: a tool-calling version starts only where its runtime can run:
/// on a local model, with the runtime installed. Nothing is dispatched.
fn runnable_for_start(
    id: SpecialistId,
    mode: InvestigationProviderMode,
) -> Result<(), InvestigationStartError> {
    runnable_with(
        id,
        mode,
        id.uses_native_tool_runtime()
            .then(super::tool_runtime::RuntimeCommand::from_env)
            .flatten()
            .as_ref(),
    )
}

/// [`runnable_for_start`] against a given runtime: a tool-calling version
/// needs the runtime, and on Cloud also the pinned Claude Code (AC-1424).
fn runnable_with(
    id: SpecialistId,
    mode: InvestigationProviderMode,
    runtime: Option<&super::tool_runtime::RuntimeCommand>,
) -> Result<(), InvestigationStartError> {
    if !id.uses_native_tool_runtime() {
        return Ok(());
    }
    let Some(runtime) = runtime else {
        return Err(rejected(
            "invalid_input",
            "The tool-calling runtime is not installed.",
        ));
    };
    if mode == InvestigationProviderMode::Cloud && !runtime.has_claude_lane() {
        return Err(rejected(
            "invalid_input",
            "Claude Code is not available for this version on this Mac; \
             Cloud tool-calling runs need Apple silicon.",
        ));
    }
    Ok(())
}

/// The offered specialists whose desktop runs use the saved run limits
/// (#1757): exactly those the worker starts through next-prompt admission.
fn configured_limit_specialists() -> Vec<SpecialistId> {
    available_specialists()
        .into_iter()
        .filter(|id| id.uses_next_prompt_admission())
        .collect()
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
                // Expanded lists the policy-selecting versions; Standard
                // lists the completion versions that keep the v1 contract.
                specialist_ids: available_specialists()
                    .into_iter()
                    .filter(|id| {
                        if v2 {
                            id.uses_policy_selection()
                        } else {
                            !id.uses_policy_selection() && id.uses_completion_obligations()
                        }
                    })
                    .collect(),
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
        let (local_model, context_window, port) = state
            .settings
            .lock()
            .ok()
            .map(|store| {
                (
                    store.local_model().ok().map(|choice| choice.model),
                    store.local_context_window().ok().flatten(),
                    store.local_ollama_port().ok().flatten(),
                )
            })
            .unwrap_or_default();
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
                state.investigations.descriptor_for_start(
                    mode,
                    local_model.as_deref(),
                    context_window,
                    port,
                )
            })
            .collect(),
            limits: InvestigationLimits::default(),
            policies: policy_catalog(),
            configured_limit_specialists: configured_limit_specialists(),
        })
    })
    .await
}

/// At most this many pages of in-flight investigations are read for the
/// roster; the live-task cap keeps the real number far below it.
const ROSTER_ACTIVE_PAGES: usize = 64;

/// The specialist agent roster with each agent's in-flight work (#256,
/// AC-1408). Read-only: dispatch stays the existing investigation start.
#[tauri::command]
pub(crate) async fn list_agents(
    app: tauri::AppHandle,
) -> Result<Vec<agents::roster::RosterAgentStatus>, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let roster = agents::roster::roster().map_err(|e| e.to_string())?;
        let work = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .active_investigation_work(ROSTER_ACTIVE_PAGES)
            .map_err(|e| e.to_string())?
            .ok_or("Too many active investigations to list the agent roster.")?;
        Ok(agents::roster::roster_status(roster, &work))
    })
    .await
}

/// The roster's read-only pipeline agents (Flow Tracer, Contract Matcher,
/// IaC Grapher) with the work each is part of in flight (#256, AC-1808).
/// None is dispatchable.
#[tauri::command]
pub(crate) async fn list_pipeline_agents(
    app: tauri::AppHandle,
) -> Result<Vec<agents::pipeline_roster::PipelineAgentStatus>, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let agents = agents::pipeline_roster::pipeline_roster().map_err(|e| e.to_string())?;
        let active = crate::pipeline_activity::active_by_pass(&state)?;
        Ok(agents::pipeline_roster::pipeline_status(agents, active))
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
#[cfg_attr(not(unix), allow(dead_code))]
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
    // ADR-0071 decision 2: every start path offers only catalog versions. A
    // committed retry still recovers its saved run.
    if let Err(refused) = offered_for_start(request.specialist_id)
        .and_then(|()| runnable_for_start(request.specialist_id, request.provider_mode))
    {
        if let Some(prior) = existing()? {
            return Ok(prior.summary);
        }
        return Err(refused);
    }
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
    // Desktop starts use the configured local completion model (#1643),
    // context window and Ollama port (#1653); MCP keeps the shipped defaults.
    let (local_model, context_window, port) =
        if session.is_none() && request.provider_mode == InvestigationProviderMode::Local {
            let store = state.settings.lock().map_err(|e| e.to_string())?;
            (
                Some(store.local_model().map_err(|e| e.to_string())?.model),
                store.local_context_window().map_err(|e| e.to_string())?,
                store.local_ollama_port().map_err(|e| e.to_string())?,
            )
        } else {
            (None, None, None)
        };
    let provider = state
        .investigations
        .provider_for_start(
            request.provider_mode,
            local_model.as_deref(),
            context_window,
            port,
        )
        .ok();
    let descriptor = provider
        .as_ref()
        .and_then(|provider| runtime::descriptor(provider.as_ref(), request.provider_mode).ok());
    let (committed, hold) = {
        let mut jobs = state.jobs.lock().map_err(|error| error.to_string())?;
        let committed = match session {
            Some(session) => {
                jobs.start_investigation_access(&request, descriptor.as_ref(), Some(session))
            }
            None => jobs.start_investigation(&request, descriptor.as_ref()),
        }
        .map_err(|error| error.to_string())?;
        // AC-1706: registered under the lock that queued the job, so no later
        // start is admitted ahead of it.
        let hold = committed
            .created
            .then(|| crate::agent_pool::hold(&state, committed.detail.summary.job_id));
        (committed, hold)
    };
    let Some(hold) = hold else {
        return Ok(committed.detail.summary);
    };
    emit_changed(&app, &committed.detail);
    let id = committed.detail.summary.investigation_id.clone();
    let provider = provider.ok_or_else(|| HostError::Operational.to_string())?;
    let live = state
        .investigations
        .insert(&id)
        .map_err(|e| e.to_string())?;
    let job_id = committed.detail.summary.job_id;
    let mode = crate::agent_pool::investigation_mode(request.provider_mode);
    // AC-1706: a start the agent pool does not admit yet stays queued and
    // cancellable; a detached waiter claims it when a place frees and the
    // run's clocks start then, not while it waited.
    let placed = crate::agent_pool::try_place(&state, job_id, Some(mode));
    if matches!(placed, Ok(None)) {
        // AC-1707: hold the job's lock while waiting, so a restart can tell
        // that this waiter stopped.
        let held = match crate::agent_pool::reserve_held(&state, job_id) {
            Ok(held) => held,
            Err(error) => {
                state.investigations.remove(&id);
                return Err(error.into());
            }
        };
        let worker_app = app.clone();
        let worker_id = id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let state = worker_app.state::<AppState>();
            let claimed = crate::agent_pool::wait_for_place(&state, hold, Some(mode), &|| {})
                .and_then(|(plan, _hold)| crate::agent_pool::claim_reserved(&state, &plan, held));
            match claimed {
                Ok((_, execution)) => worker::run(
                    worker_app.clone(),
                    worker_id,
                    request,
                    execution,
                    provider,
                    live,
                    Instant::now(),
                ),
                Err(_) => state.investigations.remove(&worker_id),
            }
        });
        return Ok(committed.detail.summary);
    }
    let claim = placed.and_then(|plan| {
        let plan = plan.ok_or_else(|| "The investigation was not admitted.".to_owned())?;
        crate::claim_job(&state, &plan)
    });
    drop(hold);
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
pub(crate) async fn check_local_ollama(app: tauri::AppHandle) -> LocalOllamaHealth {
    // The saved loopback port (#1653); an unreadable setting checks the default.
    let port = app
        .state::<AppState>()
        .settings
        .lock()
        .ok()
        .and_then(|store| store.local_ollama_port().ok().flatten())
        .unwrap_or(llm::OllamaProvider::DEFAULT_PORT);
    run_local_ollama_check(move || {
        llm::OllamaProvider::with_models(
            &llm::OllamaProvider::loopback_url(port),
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
        let state = app.state::<AppState>();
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let read = jobs
            .investigation(&investigation_id)
            .and_then(crate::jobs::investigations::InvestigationDetailRead::from_detail)
            .map_err(|e| e.to_string())?;
        let runtime = jobs
            .investigation_runtime(&investigation_id)
            .map_err(|e| e.to_string())?;
        Ok(read.with_runtime(runtime))
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

pub(crate) fn pending(state: &AppState, id: &str) -> Result<Option<InvestigationConsent>, String> {
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

/// Test-only: the public completion start, for harness tests outside this module.
#[cfg(test)]
pub(crate) fn start_completion_for_test(
    request: StagedObligationRequest,
    app: tauri::AppHandle<tauri::test::MockRuntime>,
) -> Result<InvestigationSummary, String> {
    completion_start::start(request, app, std::time::Instant::now()).map_err(|e| format!("{e:?}"))
}

pub(crate) fn consent_decision<R: tauri::Runtime>(
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

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod staged_tests {
    use super::*;

    // AC-0266: shape-valid IPC arguments keep semantic errors on the structured
    // rejection path, preserving the UI draft/retry contract for all generations.
    // AC-0286: the current schema-6 family must reach the same command boundary.
    #[test]
    fn completion_ipc_semantic_errors_reach_structured_invalid_input() {
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
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
            // Retired by AC-1062: no longer offered, but every decode below is unchanged.
            assert!(
                !available_specialists()
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
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
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

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod policy_catalog_tests {
    use super::*;

    // AC-1400 (#1751): the legacy path refuses a version the catalog does not
    // offer, even one its own request validation accepts.
    #[test]
    fn legacy_start_refuses_a_version_the_catalog_does_not_offer() {
        let request: StartInvestigationRequest = serde_json::from_value(serde_json::json!({
            "schema_version":1,"request_nonce":"retired-version","specialist_id":"domain-analyst@1",
            "question":"Inspect selected evidence","scope":{"type":"all"},"provider_mode":"local",
            "limit_profile":"investigation-v1"}))
        .unwrap();
        request.validate().unwrap();
        assert!(matches!(
            offered_for_start(request.specialist_id),
            Err(InvestigationStartError::Rejected {
                code: "invalid_input",
                definitive: true,
                ..
            })
        ));
        for id in available_specialists() {
            assert!(offered_for_start(id).is_ok(), "{id:?}");
        }
        for id in SpecialistId::all().filter(|id| !available_specialists().contains(id)) {
            assert!(offered_for_start(id).is_err(), "{id:?}");
        }
    }

    // AC-1429, AC-1424 (ADR-0085): a tool-calling version is refused before
    // anything runs when its runtime cannot run; a Cloud one also needs the
    // pinned Claude Code; bounded-loop versions are unaffected.
    #[test]
    fn a_tool_calling_start_needs_the_runtime_and_for_cloud_claude_code() {
        use super::super::tool_runtime::RuntimeCommand;
        let refused = |id, mode, runtime: Option<&RuntimeCommand>| {
            matches!(
                runnable_with(id, mode, runtime),
                Err(InvestigationStartError::Rejected {
                    code: "invalid_input",
                    definitive: true,
                    ..
                })
            )
        };
        let (local, cloud) = (
            InvestigationProviderMode::Local,
            InvestigationProviderMode::Cloud,
        );
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        std::fs::write(&claude, b"checked at spawn").unwrap();
        let runtime = |claude_code: Option<std::path::PathBuf>| RuntimeCommand {
            program: "node".into(),
            args: Vec::new(),
            claude_code,
        };
        let (bare, with_claude) = (runtime(None), runtime(Some(claude)));
        let missing = runtime(Some(dir.path().join("missing")));
        for mode in [local, cloud] {
            assert!(runnable_with(SpecialistId::DomainAnalystV28, mode, None).is_ok());
            assert!(refused(SpecialistId::DomainAnalystV29, mode, None));
        }
        assert!(!refused(SpecialistId::DomainAnalystV29, local, Some(&bare)));
        assert!(refused(SpecialistId::DomainAnalystV29, cloud, Some(&bare)));
        assert!(refused(
            SpecialistId::DomainAnalystV29,
            cloud,
            Some(&missing)
        ));
        let pinned = cfg!(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64")
        ));
        assert_eq!(
            refused(SpecialistId::DomainAnalystV29, cloud, Some(&with_claude)),
            !pinned,
            "Cloud runs where a Claude Code build is pinned, never on Intel Macs"
        );
        // AC-1815: @30 starts exactly where @29 does.
        for (mode, runtime) in [
            (local, None),
            (cloud, None),
            (local, Some(&bare)),
            (cloud, Some(&bare)),
            (cloud, Some(&missing)),
            (cloud, Some(&with_claude)),
        ] {
            assert_eq!(
                refused(SpecialistId::DomainAnalystV30, mode, runtime),
                refused(SpecialistId::DomainAnalystV29, mode, runtime)
            );
        }
    }

    // AC-0584 (#1757): the catalog, not the client, says which offered
    // specialists use the saved run limits, and it says exactly what the
    // worker's admission predicate says.
    #[test]
    fn desktop_catalog_names_the_specialists_that_use_saved_run_limits() {
        let named = configured_limit_specialists();
        for id in available_specialists() {
            assert_eq!(
                named.contains(&id),
                id.uses_next_prompt_admission(),
                "{id:?}"
            );
        }
        if InvestigationPolicy::for_profile("investigation-v2").is_some() {
            // The offered expanded analysts (@21-@23 are retired, AC-1062).
            for id in [
                SpecialistId::DomainAnalystV27,
                SpecialistId::DomainAnalystV28,
                SpecialistId::DomainAnalystV29,
            ] {
                assert!(named.contains(&id), "{id:?}");
            }
        }
        assert!(!named.contains(&SpecialistId::DomainAnalystV7));
        assert!(!named.contains(&SpecialistId::SpecWriterV2));

        // An older catalog without the field still reads, as naming none.
        let old: InvestigationCatalog = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "specialists": [], "providers": [],
            "limits": InvestigationLimits::default(),
        }))
        .unwrap();
        assert!(old.configured_limit_specialists.is_empty());
    }

    // AC-0574: rollout changes creation, never saved identity validation.
    #[test]
    fn desktop_catalog_advertises_analyst27_but_not_development_roles() {
        let expanded = policy_catalog()
            .into_iter()
            .find(|policy| policy.limits.profile == "investigation-v2")
            .unwrap();
        assert!(available_specialists().contains(&SpecialistId::DomainAnalystV27));
        assert!(
            expanded
                .specialist_ids
                .contains(&SpecialistId::DomainAnalystV27)
        );
        for role in [
            SpecialistId::DomainAnalystV24,
            SpecialistId::DomainAnalystV25,
            SpecialistId::DomainAnalystV26,
        ] {
            assert!(!available_specialists().contains(&role));
            assert!(!expanded.specialist_ids.contains(&role));
        }
    }

    // AC-0830 (#1335): @28 is offered beside @27, in @27's expanded profile
    // only, and @27's catalog entry and definition are unchanged.
    #[test]
    fn desktop_catalog_advertises_analyst28_under_analyst27_profile() {
        if InvestigationPolicy::for_profile("investigation-v2").is_none() {
            assert!(!available_specialists().contains(&SpecialistId::DomainAnalystV28));
            return;
        }
        let catalog = policy_catalog();
        for entry in &catalog {
            assert_eq!(
                entry
                    .specialist_ids
                    .contains(&SpecialistId::DomainAnalystV28),
                entry
                    .specialist_ids
                    .contains(&SpecialistId::DomainAnalystV27),
                "{}",
                entry.limits.profile
            );
        }
        let expanded = catalog
            .iter()
            .find(|entry| entry.limits.profile == "investigation-v2")
            .unwrap();
        let at = |id| expanded.specialist_ids.iter().position(|x| *x == id);
        assert_eq!(
            at(SpecialistId::DomainAnalystV28),
            at(SpecialistId::DomainAnalystV27).map(|i| i + 1)
        );
        let offered = available_specialists();
        assert!(offered.contains(&SpecialistId::DomainAnalystV27));
        assert!(offered.contains(&SpecialistId::DomainAnalystV28));
        assert!(configured_limit_specialists().contains(&SpecialistId::DomainAnalystV28));
        let v27 = SpecialistId::DomainAnalystV27.definition().unwrap();
        let v28 = SpecialistId::DomainAnalystV28.definition().unwrap();
        assert_eq!(v27.version, 27);
        assert_eq!(v28.version, 28);
        assert_ne!(v27.prompt_fingerprint, v28.prompt_fingerprint);
    }

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
        // AC-1062: of the expanded family, only the current versions and the
        // named exceptions are offered.
        for role in [
            SpecialistId::EvidenceAuditorV14,
            SpecialistId::DomainAnalystV27,
            SpecialistId::DomainAnalystV28,
            SpecialistId::DomainAnalystV29,
            SpecialistId::DomainAnalystV30,
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

        assert!(available_specialists().contains(&SpecialistId::SpecWriterV2));
        assert_eq!(standard.request_schema_version, 4);
        assert!(
            standard
                .specialist_ids
                .contains(&SpecialistId::SpecWriterV2)
        );
        assert!(expanded.is_none_or(|choice| {
            choice.request_schema_version != 4
                && !choice.specialist_ids.contains(&SpecialistId::SpecWriterV2)
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
        assert!(available_specialists().contains(&SpecialistId::GapTriagerV2));
        assert_eq!(standard.request_schema_version, 4);
        assert!(
            standard
                .specialist_ids
                .contains(&SpecialistId::GapTriagerV2)
        );
        assert!(
            expanded.is_none_or(|choice| choice.request_schema_version != 4
                && !choice.specialist_ids.contains(&SpecialistId::GapTriagerV2))
        );
    }
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
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
