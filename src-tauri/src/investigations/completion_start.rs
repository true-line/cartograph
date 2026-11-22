//! Generation-four entrypoint: source validation precedes durable intent admission.
use super::super::completion_choices::SelectionError;
use super::*;
use crate::primary_source::RetentionGuard;

fn validate_targets<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    request: &StagedObligationRequest,
    scope_limits: Option<crate::mcp_reads::scope::ScopeLimits>,
) -> Result<Vec<RetentionGuard>, SelectionError> {
    // Refuse an impossible obligation before any durable start/provider lookup.
    match scope_limits {
        Some(limits) => super::super::completion_choices::validate_request_with_scope(
            app,
            state,
            request,
            Some(limits),
        ),
        None => super::super::completion_choices::validate_request(app, state, request)
            .map_err(SelectionError::from),
    }?;
    let policy = InvestigationPolicy::for_profile(&request.limit_profile)
        .ok_or_else(|| "Unknown investigation policy.".to_owned())?;
    let mut guards = Vec::new();
    for target in request
        .obligations
        .sources_with_policy(policy)
        .map_err(|error| error.to_string())?
    {
        let (actual, guard) = super::super::inspection::resolve_with_limits(
            state,
            &target.fact,
            &target.graph_snapshot_id,
            &target.receipt_id,
            target.receipt_inventory_index as usize,
            super::super::context::task_graph_limits(scope_limits.is_some()),
        )?;
        if actual != target {
            return Err("Completion source changed; refresh the selection."
                .to_owned()
                .into());
        }
        guards.push(guard);
    }
    Ok(guards)
}

pub(super) fn start<R: tauri::Runtime>(
    request: StagedObligationRequest,
    app: tauri::AppHandle<R>,
    started: Instant,
) -> Result<InvestigationSummary, InvestigationStartError> {
    start_access(request, app, started, None, None, None)
}

pub(super) fn start_access<R: tauri::Runtime>(
    request: StagedObligationRequest,
    app: tauri::AppHandle<R>,
    started: Instant,
    session: Option<&McpTaskSession>,
    task_limits: Option<crate::investigations::mcp_dispatch::TaskReadLimits>,
    reservations: Option<&crate::mcp_reads::task_reservation::TaskReservations>,
) -> Result<InvestigationSummary, InvestigationStartError> {
    let policy = InvestigationPolicy::for_profile(&request.limit_profile)
        .ok_or_else(|| rejected("invalid_input", "Unknown investigation policy."))?;
    let request = request
        .normalized_with_policy(policy)
        .map_err(|error| rejected("invalid_input", error))?;
    let state = app.state::<AppState>();
    let _admission = reservations
        .map(|reservations| reservations.admission())
        .transpose()
        .map_err(|error| rejected("limit_exceeded", error))?;
    let existing = || {
        let jobs = state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational.to_string())?;
        match session {
            Some(session) => jobs.existing_investigation_completion_access(&request, Some(session)),
            None => jobs.existing_desktop_investigation_completion(&request),
        }
        .map_err(|error| error.to_string())
    };
    // Recover a committed nonce before consulting mutable source/provider state.
    if let Some(prior) = existing()? {
        return Ok(prior.summary);
    }
    // A retry above needs no new execution reservation. New work must retain
    // the host's already-admitted permit through preparation and worker exit.
    let reservation = reservations
        .map(|reservations| reservations.try_acquire())
        .transpose()
        .map_err(|error| rejected("limit_exceeded", error))?;
    let specialist: SpecialistId =
        serde_json::from_value(serde_json::Value::String(request.specialist_id.clone()))
            .map_err(|error| rejected("invalid_input", error))?;
    offered_for_start(specialist)?;
    runnable_for_start(specialist, request.provider_mode)?;
    // A drafting run names one drafted catalogue section (SPEC-15 slice 6).
    // An outside agent starts one over MCP under the same check (ADR-0066).
    super::super::section_drafts::validate_start(&request.specialist_id, &request.question)
        .map_err(|error| rejected("invalid_input", error))?;
    let _guards = match validate_targets(
        &app,
        &state,
        &request,
        task_limits.map(|limits| limits.scope),
    ) {
        Ok(guards) => guards,
        Err(error) => {
            if let Some(prior) = existing()? {
                return Ok(prior.summary);
            }
            return Err(rejected(error.code(), error));
        }
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
    // Capture desktop settings only for new work, after nonce recovery and
    // before taking the jobs lock. MCP retains its existing default contract.
    let configured = if session.is_none() && specialist.uses_next_prompt_admission() {
        Some(
            state
                .settings
                .lock()
                .map_err(|e| e.to_string())?
                .run_limits()
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let (committed, hold) = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let committed = match session {
            Some(session) => jobs.start_completion_access(
                &request,
                descriptor.as_ref(),
                specialist.uses_next_prompt_admission(),
                Some(session),
            ),
            None if specialist.uses_next_prompt_admission() => {
                let limits = configured
                    .as_ref()
                    .ok_or_else(|| "Missing desktop run limits.".to_owned())?;
                jobs.start_desktop_next_prompt_investigation(
                    &request,
                    descriptor.as_ref(),
                    limits.active_seconds,
                    limits.request_timeout_ms,
                )
            }
            None => jobs.start_completion_investigation(&request, descriptor.as_ref()),
        }
        .map_err(|e| e.to_string())?;
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
    // AC-1706: held by the agent pool like any other agent work (see
    // `commands::start_access`); the run's clocks start once it is claimed.
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
            let _reservation = reservation;
            let state = worker_app.state::<AppState>();
            let claimed = crate::agent_pool::wait_for_place(&state, hold, Some(mode), &|| {})
                .and_then(|(plan, _hold)| crate::agent_pool::claim_reserved(&state, &plan, held));
            let execution = match claimed {
                Ok((_, execution)) => execution,
                Err(_) => {
                    state.investigations.remove(&worker_id);
                    return;
                }
            };
            let started = Instant::now();
            match task_limits {
                Some(limits) => worker::run_completion_with_limits(
                    worker_app.clone(),
                    worker_id,
                    request,
                    execution,
                    provider,
                    live,
                    started,
                    Some(limits),
                ),
                None => worker::run_completion(
                    worker_app.clone(),
                    worker_id,
                    request,
                    execution,
                    provider,
                    live,
                    started,
                ),
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
        let _reservation = reservation;
        match task_limits {
            Some(limits) => worker::run_completion_with_limits(
                worker_app,
                worker_id,
                request,
                execution,
                provider,
                live,
                started,
                Some(limits),
            ),
            None => worker::run_completion(
                worker_app, worker_id, request, execution, provider, live, started,
            ),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use context_hub::{ContextSnapshot, coverage::CoverageQualification};
    use core_graph::{GraphStore, Node};
    use serde_json::json;

    // AC-0266: a full-graph source target cannot authorize an unrelated scoped
    // task. The real coordinator refuses before provider lookup or durable start.
    #[test]
    fn completion_start_rejects_source_outside_requested_neighborhood() {
        let directory = tempfile::tempdir().unwrap();
        let state = crate::registered_source_tests::app_state(directory.path());
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../ui/src/fixtures/investigation-inspection-backend.json"
        ))
        .unwrap();
        let mut target: InvestigationInspectionTarget =
            serde_json::from_value(fixture["target"].clone()).unwrap();
        let agents::TaskFactKey::Node { id: source_id } = &target.fact else {
            panic!("node target fixture");
        };
        {
            let mut graph = state.graph.lock().unwrap();
            for id in [source_id.as_str(), "unrelated-anchor"] {
                graph
                    .put_node(&Node {
                        id: id.into(),
                        label: "BusinessRule".into(),
                        props: json!({}),
                    })
                    .unwrap();
            }
        }
        let graph = state.graph.lock().unwrap().read_snapshot().unwrap();
        let complete = ContextSnapshot::new(graph.0.clone(), graph.1.clone()).unwrap();
        target.graph_snapshot_id = complete.id().to_owned();
        target.target_id = target.content_id().unwrap();
        target.validate().unwrap();
        let scope = InvestigationScope::Neighborhood {
            anchor: "unrelated-anchor".into(),
            hops: 1,
        };
        let scoped = crate::investigations::context::FrozenContext::scoped_snapshot(
            &graph, &scope, None, complete,
        )
        .unwrap();
        let context_id = scoped
            .with_coverage(CoverageQualification::new(&[], &[]).unwrap())
            .unwrap()
            .context_id()
            .to_owned();
        let mut obligation = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(target.clone()),
        };
        let identity = obligation.content_id().unwrap();
        if let Obligation::Source { obligation_id, .. } = &mut obligation {
            *obligation_id = identity;
        }
        let request = StagedObligationRequest {
            schema_version: 4,
            request_nonce: "outside-scoped-source".into(),
            specialist_id: "domain-analyst@7".into(),
            question: "Inspect the explicitly selected source.".into(),
            scope,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: "investigation-v1".into(),
            expected_graph_revision: Some(target.graph_snapshot_id),
            expected_context_id: context_id,
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
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap(),
        );
        app.manage(state);
        // @7 is retired (AC-1062); this checks its own scope refusal.
        let error =
            super::with_retired_starts(|| start(request, app.handle().clone(), Instant::now()))
                .unwrap_err();
        assert!(
            matches!(error, InvestigationStartError::Rejected { code: "refresh_required", definitive: true, ref message } if message.contains("outside the requested scope"))
        );
        assert!(
            app.state::<AppState>()
                .jobs
                .lock()
                .unwrap()
                .investigation_history(None, None)
                .unwrap()
                .items
                .is_empty()
        );
    }
}
