//! Claiming a planned job execution against the app state, split out of
//! `main.rs` so code outside the app crate can start jobs (#1924).
use crate::app_state::AppState;
use crate::job_execution::JobExecution;
use crate::jobs::{self, Job};

pub fn claim_job(state: &AppState, plan: &jobs::ClaimPlan) -> Result<(Job, JobExecution), String> {
    // The plan is owned data: no store mutex spans the OS reservation.
    let reservation = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .map_err(|error| error.to_string())?;
    state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .claim_execution(plan, reservation)
        .map_err(|error| error.to_string())
}
