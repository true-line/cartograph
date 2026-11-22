use crate::{
    job_execution::JobExecutionLocks,
    jobs::{JobStore, JobTransitionError},
};
use std::sync::Mutex;

/// Coordinator rows survive Jobs cleanup. Only an unchanged recorded execution
/// with its exact existing OS lock proven free can be recovered; never replay it.
pub(crate) fn recover(jobs: &Mutex<JobStore>, locks: &JobExecutionLocks) -> Result<(), String> {
    let candidates = jobs
        .lock()
        .map_err(|e| e.to_string())?
        .investigation_recovery_candidates()
        .map_err(|e| e.to_string())?;
    for candidate in candidates {
        let reservation = match locks.try_reserve(candidate.lock_target()) {
            Ok(reservation) => reservation,
            Err(JobTransitionError::Busy) => continue,
            Err(error) => return Err(error.to_string()),
        };
        jobs.lock()
            .map_err(|e| e.to_string())?
            .recover_investigation(&candidate, reservation)
            .map_err(|e| e.to_string())?;
    }
    // AC-1707: a held task whose waiter stopped is ended, never re-armed; its
    // start's session and read limits are gone. A missing or busy lock is not
    // proof that the waiter stopped, so that task stays queued.
    let held = jobs
        .lock()
        .map_err(|e| e.to_string())?
        .held_investigation_candidates()
        .map_err(|e| e.to_string())?;
    for candidate in held {
        let Ok(reservation) = locks.try_reserve(candidate.lock_target()) else {
            continue;
        };
        jobs.lock()
            .map_err(|e| e.to_string())?
            .recover_investigation(&candidate, reservation)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
