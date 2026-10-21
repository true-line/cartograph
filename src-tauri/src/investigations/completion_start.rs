//! Generation-four entrypoint: source validation precedes durable intent admission.
use super::*;
use crate::primary_source::RetentionGuard;

fn validate_targets<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    request: &StagedObligationRequest,
) -> Result<Vec<RetentionGuard>, String> {
    let mut guards = Vec::new();
    for target in request
        .obligations
        .sources()
        .map_err(|error| error.to_string())?
    {
        let (actual, guard) = super::super::inspection::resolve(
            state,
            &target.fact,
            &target.graph_snapshot_id,
            &target.receipt_id,
            target.receipt_inventory_index as usize,
        )?;
        if actual != target {
            return Err("Completion source changed; refresh the selection.".into());
        }
        guards.push(guard);
    }
    super::super::completion_choices::validate_request(app, state, request)?;
    Ok(guards)
}

pub(super) fn start<R: tauri::Runtime>(
    request: StagedObligationRequest,
    app: tauri::AppHandle<R>,
    started: Instant,
) -> Result<InvestigationSummary, InvestigationStartError> {
    let request = request
        .normalized()
        .map_err(|error| rejected("invalid_input", error))?;
    let state = app.state::<AppState>();
    // Recover a committed nonce before consulting mutable source/provider state.
    if let Some(prior) = state
        .jobs
        .lock()
        .map_err(|_| HostError::Operational.to_string())?
        .existing_investigation_completion(&request)
        .map_err(|error| error.to_string())?
    {
        return Ok(prior.summary);
    }
    let specialist = if request.specialist_id == "domain-analyst@7" {
        SpecialistId::DomainAnalystV7
    } else {
        SpecialistId::EvidenceAuditorV7
    };
    if !AVAILABLE_SPECIALISTS.contains(&specialist) {
        return Err(rejected(
            "invalid_input",
            "This specialist version is not available.",
        ));
    }
    let _guards = match validate_targets(&app, &state, &request) {
        Ok(guards) => guards,
        Err(error) => {
            if let Some(prior) = state
                .jobs
                .lock()
                .map_err(|_| HostError::Operational.to_string())?
                .existing_investigation_completion(&request)
                .map_err(|error| error.to_string())?
            {
                return Ok(prior.summary);
            }
            return Err(rejected("refresh_required", error));
        }
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
        .start_completion_investigation(&request, descriptor.as_ref())
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
        worker::run_completion(
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
