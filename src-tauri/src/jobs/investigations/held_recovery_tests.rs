use super::*;
use std::sync::Mutex;

/// The held pass of `investigations::recover` (app crate), which this
/// shared test crate cannot reach: end a candidate only with its lock.
fn recover_held(jobs: &Mutex<JobStore>, locks: &JobExecutionLocks) {
    let held = jobs
        .lock()
        .unwrap()
        .held_investigation_candidates()
        .unwrap();
    for candidate in held {
        let Ok(reservation) = locks.try_reserve(candidate.lock_target()) else {
            continue;
        };
        jobs.lock()
            .unwrap()
            .recover_investigation(&candidate, reservation)
            .unwrap();
    }
}

// AC-1707 (T-1707): a task the agent pool held and never claimed is ended at
// startup only once its waiter's lock exists and is free; it is never
// re-armed, never calls a provider, and a missing or busy lock keeps it queued.
#[test]
fn a_held_investigation_whose_waiter_stopped_is_ended_at_startup_without_running() {
    let Fixture {
        directory: _directory,
        store,
        locks,
    } = Fixture::new();
    let jobs = Mutex::new(store);
    let queued = jobs
        .lock()
        .unwrap()
        .start_investigation(
            &request("held", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    let id = queued.summary.investigation_id.clone();
    let job_id = queued.summary.job_id;
    let status = |jobs: &Mutex<JobStore>| {
        jobs.lock()
            .unwrap()
            .investigation(&id)
            .unwrap()
            .summary
            .status
    };

    // No waiter lock yet: the start→claim gap is not proof of death.
    recover_held(&jobs, &locks);
    assert_eq!(status(&jobs), InvestigationStatus::Queued);

    // A live waiter holds the lock.
    let plan = jobs
        .lock()
        .unwrap()
        .claim_plan(job_id, ClaimMode::StartQueued)
        .unwrap();
    let waiter = locks.try_reserve(plan.lock_target()).unwrap();
    recover_held(&jobs, &locks);
    assert_eq!(status(&jobs), InvestigationStatus::Queued);

    // The waiter stopped: its existing lock is free.
    drop(waiter);
    recover_held(&jobs, &locks);
    let ended = jobs.lock().unwrap().investigation(&id).unwrap();
    assert_eq!(ended.summary.status, InvestigationStatus::Interrupted);
    assert_eq!(
        ended.error.as_deref(),
        Some("The app stopped before this held investigation started; it was not started.")
    );
    assert_eq!(
        jobs.lock().unwrap().get(job_id).unwrap().status,
        "interrupted"
    );
    let events = jobs.lock().unwrap().investigation_events(&id, 0).unwrap();
    assert!(
        !events
            .items
            .iter()
            .any(|event| event.kind == InvestigationEventKind::ModelStarted)
    );
    let revision = ended.summary.revision;
    recover_held(&jobs, &locks);
    assert_eq!(
        jobs.lock()
            .unwrap()
            .investigation(&id)
            .unwrap()
            .summary
            .revision,
        revision
    );
}
