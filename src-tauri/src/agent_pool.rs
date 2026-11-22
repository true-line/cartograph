//! Agent pool controls (#252, AC-1704): pause new agent work, and cap how
//! many agent jobs run at once per provider mode. The controls gate only
//! where agent work starts as a job (topic expert questions, section drafts
//! and gap escalations): work they do not admit yet is held as queued and
//! starts when they do (AC-1705). They never stop work already running.
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tauri::{Manager, State};

use crate::AppState;
use crate::job_execution::{ExecutionReservation, JobExecution};
use crate::jobs::{ClaimMode, ClaimPlan, Job, JobStore};
use agents::investigation::InvestigationProviderMode;

/// Largest per-mode limit offered.
pub const MAX_LIMIT: u32 = 16;

/// The persisted controls. A limit of `0` means no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPoolControls {
    pub paused: bool,
    pub local_limit: u32,
    pub cloud_limit: u32,
}

impl Default for AgentPoolControls {
    /// Nothing paused and no limit: the behavior before these controls.
    fn default() -> Self {
        Self {
            paused: false,
            local_limit: 0,
            cloud_limit: 0,
        }
    }
}

const AGENT_KINDS: [&str; 5] = [
    "area-ask",
    "section-compose",
    "section-compose-gated",
    "escalate",
    "escalate-class",
];

/// The provider mode of an agent job kind, or `None` for other work
/// (ingest, repository adds, plugin gates) the controls never gate.
pub(crate) fn agent_mode(kind: &str) -> Option<&str> {
    let (head, rest) = kind.split_once(':')?;
    if !AGENT_KINDS.contains(&head) {
        return None;
    }
    Some(rest.rsplit(':').next().unwrap_or(rest))
}

/// Agent jobs held in this process, waiting for the controls to admit them,
/// keyed by the app state they belong to. A queued row left by an earlier
/// process has no waiter and never counts against a limit.
static HELD: Mutex<BTreeSet<(usize, i64)>> = Mutex::new(BTreeSet::new());

/// How often a held job checks again whether it may start.
const HOLD_POLL: Duration = Duration::from_millis(250);

fn state_key(state: &AppState) -> usize {
    std::ptr::from_ref(state) as usize
}

fn is_held(state: usize, id: i64) -> bool {
    HELD.lock()
        .map(|held| held.contains(&(state, id)))
        .unwrap_or(false)
}

/// Registers a held job and removes it however the wait ends. Keep it
/// until the job is claimed, so later jobs still count it as ahead.
pub(crate) struct Hold {
    state: usize,
    id: i64,
}

/// Register queued job `id` as held. Call it under the same jobs lock that
/// queued the job, so no later start can be admitted ahead of it.
pub(crate) fn hold(state: &AppState, id: i64) -> Hold {
    Hold::new(state_key(state), id)
}

impl Hold {
    fn new(state: usize, id: i64) -> Self {
        if let Ok(mut held) = HELD.lock() {
            held.insert((state, id));
        }
        Self { state, id }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Ok(mut held) = HELD.lock() {
            held.remove(&(self.state, self.id));
        }
    }
}

/// Whether agent work of `kind` may start now next to `jobs`, or why it is
/// held. Running agent jobs of the same mode count against its limit, and
/// so do queued ones that `ahead` says are waiting before it. Other work is
/// always admitted.
#[cfg(test)]
fn admit(
    controls: &AgentPoolControls,
    jobs: &[Job],
    kind: &str,
    ahead: impl Fn(&Job) -> bool,
) -> Result<(), String> {
    admit_mode(
        controls,
        jobs,
        agent_mode(kind),
        |job| agent_mode(&job.kind).map(str::to_owned),
        ahead,
    )
}

/// [`admit`] for work of provider `mode` (`None`: not agent work), with
/// `mode_of` naming each listed job's mode.
fn admit_mode(
    controls: &AgentPoolControls,
    jobs: &[Job],
    mode: Option<&str>,
    mode_of: impl Fn(&Job) -> Option<String>,
    ahead: impl Fn(&Job) -> bool,
) -> Result<(), String> {
    let Some(mode) = mode else {
        return Ok(());
    };
    if controls.paused {
        return Err(
            "Agent work is paused in the agent pool; this starts when it is resumed.".into(),
        );
    }
    let limit = match mode {
        "local" => controls.local_limit,
        "cloud" => controls.cloud_limit,
        _ => 0,
    };
    if limit == 0 {
        return Ok(());
    }
    let active = jobs
        .iter()
        .filter(|job| job.status == "running" || (job.status == "queued" && ahead(job)))
        .filter(|job| mode_of(job).as_deref() == Some(mode))
        .count();
    if active >= limit as usize {
        return Err(format!(
            "The agent pool is at its limit of {limit} {mode} agent {}; this starts when one finishes.",
            if limit == 1 { "job" } else { "jobs" }
        ));
    }
    Ok(())
}

/// The provider mode of a listed job: from its kind for host-run agent
/// work, from its saved task for a specialist investigation (AC-1706).
fn job_mode(store: &JobStore, job: &Job) -> Option<String> {
    match &job.investigation_id {
        Some(id) => store
            .investigation(id)
            .ok()
            .map(|detail| investigation_mode(detail.summary.provider_mode).to_owned()),
        None => agent_mode(&job.kind).map(str::to_owned),
    }
}

/// The pool's name for an investigation's provider mode.
pub(crate) fn investigation_mode(mode: InvestigationProviderMode) -> &'static str {
    match mode {
        InvestigationProviderMode::Local => "local",
        InvestigationProviderMode::Cloud => "cloud",
    }
}

/// The claim plan for held job `id` of provider `mode` if the controls
/// admit it now, or `None` while it has to wait. Held jobs start in the
/// order they were queued. The check and the plan are made under one jobs
/// lock. A job that stops being queued (cancelled from Jobs or its task)
/// is an error.
pub(crate) fn try_place(
    state: &AppState,
    id: i64,
    mode: Option<&str>,
) -> Result<Option<ClaimPlan>, String> {
    let key = state_key(state);
    let controls = state
        .settings
        .lock()
        .map_err(|error| error.to_string())?
        .agent_pool_controls()
        .map_err(|error| error.to_string())?;
    let jobs = state.jobs.lock().map_err(|error| error.to_string())?;
    let listed = jobs.list().map_err(|error| error.to_string())?;
    match listed.iter().find(|row| row.id == id) {
        Some(row) if row.status == "queued" => {}
        Some(row) if row.status == "cancelled" => return Err("cancelled".into()),
        _ => return Err("The held agent job is no longer queued.".into()),
    }
    let admitted = admit_mode(
        &controls,
        &listed,
        mode,
        |job| job_mode(&jobs, job),
        |other| other.id < id && is_held(key, other.id),
    );
    match admitted {
        Ok(()) => jobs
            .claim_plan(id, ClaimMode::StartQueued)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(_) => Ok(None),
    }
}

/// Wait, blocking, until the job `hold` registers may start, and return its
/// claim plan with the hold, which must be kept until the plan is claimed.
/// `on_hold` runs once if the job has to wait. A job that stops being
/// queued ends the wait.
pub(crate) fn wait_for_place(
    state: &AppState,
    hold: Hold,
    mode: Option<&str>,
    on_hold: &dyn Fn(),
) -> Result<(ClaimPlan, Hold), String> {
    let mut reported = false;
    loop {
        if let Some(plan) = try_place(state, hold.id, mode)? {
            return Ok((plan, hold));
        }
        if !reported {
            on_hold();
            reported = true;
        }
        std::thread::sleep(HOLD_POLL);
    }
}

/// AC-1707: reserve the execution lock of held job `id` while it waits, so
/// startup recovery can prove a waiter stopped (an existing, free lock) and
/// end its task instead of leaving it queued. Take it outside every mutex;
/// claim with [`claim_reserved`] using the same reservation.
pub(crate) fn reserve_held(state: &AppState, id: i64) -> Result<ExecutionReservation, String> {
    let plan = state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .claim_plan(id, ClaimMode::StartQueued)
        .map_err(|error| error.to_string())?;
    state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .map_err(|error| error.to_string())
}

/// Claim `plan` with the lock [`reserve_held`] already holds.
pub(crate) fn claim_reserved(
    state: &AppState,
    plan: &ClaimPlan,
    reservation: ExecutionReservation,
) -> Result<(Job, JobExecution), String> {
    state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .claim_execution(plan, reservation)
        .map_err(|error| error.to_string())
}

/// Start agent work of `kind` as a job. When the controls do not admit it
/// yet, the job is held as queued (and reported through `emit`) until they
/// do (AC-1705). Cancelling a held job ends the wait without starting it.
pub(crate) fn start_job_blocking(
    state: &AppState,
    kind: &str,
    emit: &dyn Fn(&Job),
) -> Result<(Job, JobExecution), String> {
    let (job, hold) = {
        let mut jobs = state.jobs.lock().map_err(|error| error.to_string())?;
        let job = jobs.enqueue(kind).map_err(|error| error.to_string())?;
        let held = hold(state, job.id);
        (job, held)
    };
    // AC-1709: hold the job's lock while waiting, so a restart can tell
    // that this waiter stopped. A job that cannot be locked never starts.
    let reservation = match reserve_held(state, job.id) {
        Ok(reservation) => reservation,
        Err(error) => {
            if let Ok(mut jobs) = state.jobs.lock() {
                let _ = jobs.cancel(job.id);
            }
            return Err(error);
        }
    };
    let (plan, _hold) = wait_for_place(state, hold, agent_mode(kind), &|| emit(&job))?;
    claim_reserved(state, &plan, reservation)
}

/// Why a held agent job was ended at startup (AC-1709).
pub(crate) const HELD_STOPPED: &str =
    "The app stopped before this held agent job started; it was not started.";

/// AC-1709: at startup, cancel queued host agent jobs whose held waiter
/// stopped. Only an existing, free lock proves that; a missing or busy one
/// leaves the job queued. Investigations have their own pass
/// (`investigations::recover`). Nothing is re-armed.
pub(crate) fn recover_held_jobs(
    jobs: &Mutex<JobStore>,
    locks: &crate::job_execution::JobExecutionLocks,
) -> Result<Vec<Job>, String> {
    let plans = {
        let store = jobs.lock().map_err(|error| error.to_string())?;
        store
            .list()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|job| {
                job.status == "queued"
                    && job.investigation_id.is_none()
                    && agent_mode(&job.kind).is_some()
            })
            .filter_map(|job| store.claim_plan(job.id, ClaimMode::StartQueued).ok())
            .collect::<Vec<_>>()
    };
    let mut ended = Vec::new();
    for plan in plans {
        let Ok(reservation) = locks.try_reserve(&plan.held_lock_target()) else {
            continue;
        };
        if let Some(job) = jobs
            .lock()
            .map_err(|error| error.to_string())?
            .cancel_held(&plan, reservation, HELD_STOPPED)
            .map_err(|error| error.to_string())?
        {
            ended.push(job);
        }
    }
    Ok(ended)
}

/// [`start_job_blocking`] off the async runtime, reporting a held job.
pub(crate) async fn start_job(
    app: &tauri::AppHandle,
    kind: &str,
) -> Result<(Job, JobExecution), String> {
    let (app, kind) = (app.clone(), kind.to_owned());
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        start_job_blocking(&state, &kind, &|job| crate::emit_job(&app, job))
    })
    .await
}

pub(crate) fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS agent_pool_controls (
             id          INTEGER PRIMARY KEY CHECK (id = 1),
             paused      INTEGER NOT NULL CHECK (paused IN (0, 1)),
             local_limit INTEGER NOT NULL CHECK (local_limit BETWEEN 0 AND {MAX_LIMIT}),
             cloud_limit INTEGER NOT NULL CHECK (cloud_limit BETWEEN 0 AND {MAX_LIMIT})
         ) STRICT;
         INSERT OR IGNORE INTO agent_pool_controls (id, paused, local_limit, cloud_limit)
         VALUES (1, 0, 0, 0);"
    ))
}

pub(crate) fn read(conn: &Connection) -> rusqlite::Result<AgentPoolControls> {
    conn.query_row(
        "SELECT paused, local_limit, cloud_limit FROM agent_pool_controls WHERE id = 1",
        [],
        |row| {
            Ok(AgentPoolControls {
                paused: row.get(0)?,
                local_limit: row.get(1)?,
                cloud_limit: row.get(2)?,
            })
        },
    )
}

pub(crate) fn write(conn: &Connection, controls: &AgentPoolControls) -> Result<(), String> {
    for (name, limit) in [
        ("local", controls.local_limit),
        ("cloud", controls.cloud_limit),
    ] {
        if limit > MAX_LIMIT {
            return Err(format!(
                "The {name} agent limit must be between 0 (no limit) and {MAX_LIMIT}, not {limit}."
            ));
        }
    }
    conn.execute(
        "UPDATE agent_pool_controls SET paused = ?1, local_limit = ?2, cloud_limit = ?3 WHERE id = 1",
        params![controls.paused, controls.local_limit, controls.cloud_limit],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// The agent pool controls.
#[tauri::command]
pub(crate) fn get_agent_pool_controls(
    state: State<'_, AppState>,
) -> Result<AgentPoolControls, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.agent_pool_controls().map_err(|e| e.to_string())
}

/// Persist the agent pool controls. They apply to the next agent job that
/// starts; work already running is not stopped.
#[tauri::command]
pub(crate) fn set_agent_pool_controls(
    controls: AgentPoolControls,
    state: State<'_, AppState>,
) -> Result<AgentPoolControls, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store.set_agent_pool_controls(&controls)?;
    store.agent_pool_controls().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingsStore;

    fn job(id: i64, kind: &str, status: &str) -> Job {
        Job {
            investigation_id: None,
            execution_tracking: crate::jobs::ExecutionTracking::Recorded,
            id,
            kind: kind.into(),
            status: status.into(),
            stage: None,
            progress: None,
            error: None,
            artifacts: Vec::new(),
            created_at: "2026-11-21T00:00:00Z".into(),
            updated_at: "2026-11-21T00:00:00Z".into(),
        }
    }

    // AC-1709 (T-1709): a held host agent job is ended at startup only once
    // its waiter's existing lock is free; other queued work is never touched.
    #[test]
    fn a_held_agent_job_whose_waiter_stopped_is_ended_at_startup() {
        let directory = tempfile::tempdir().unwrap();
        let store = JobStore::open(directory.path().join("state.db")).unwrap();
        let locks = crate::job_execution::JobExecutionLocks::open(
            directory.path(),
            store.execution_namespace(),
        )
        .unwrap();
        let jobs = Mutex::new(store);
        let held = jobs.lock().unwrap().enqueue("area-ask:local").unwrap().id;
        let ingest = jobs
            .lock()
            .unwrap()
            .enqueue("ingest-source-v1:src_1")
            .unwrap()
            .id;
        let status = |id: i64| jobs.lock().unwrap().get(id).unwrap().status;
        let lock = |id: i64| {
            let plan = jobs
                .lock()
                .unwrap()
                .claim_plan(id, ClaimMode::StartQueued)
                .unwrap();
            locks.try_reserve(plan.lock_target()).unwrap()
        };

        // No waiter lock yet: not proof that a waiter stopped.
        assert!(recover_held_jobs(&jobs, &locks).unwrap().is_empty());
        assert_eq!(status(held), "queued");

        // A live waiter holds the lock.
        let waiter = lock(held);
        drop(lock(ingest));
        assert!(recover_held_jobs(&jobs, &locks).unwrap().is_empty());
        assert_eq!(status(held), "queued");

        drop(waiter);
        let ended = recover_held_jobs(&jobs, &locks).unwrap();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].id, held);
        assert_eq!(ended[0].status, "cancelled");
        assert_eq!(ended[0].error.as_deref(), Some(HELD_STOPPED));
        assert_eq!(status(ingest), "queued");
        assert!(recover_held_jobs(&jobs, &locks).unwrap().is_empty());
        // A terminal row: Clear finished removes it, the queued ingest stays.
        assert_eq!(jobs.lock().unwrap().clear_finished().unwrap(), 1);
        assert_eq!(status(ingest), "queued");
    }

    // AC-1704 (T-1704): only agent kinds are gated; the mode is the last segment.
    #[test]
    fn agent_kinds_carry_their_mode() {
        assert_eq!(agent_mode("area-ask:cloud"), Some("cloud"));
        assert_eq!(
            agent_mode("section-compose:D1:1.5:system:local"),
            Some("local")
        );
        assert_eq!(
            agent_mode("section-compose-gated:D1:1.5:system:local"),
            Some("local")
        );
        assert_eq!(agent_mode("escalate:gap:a:cloud"), Some("cloud"));
        assert_eq!(agent_mode("escalate-class:2:local"), Some("local"));
        assert_eq!(agent_mode("ingest-source-v1:src_1"), None);
        assert_eq!(agent_mode("add-repo:https://example.test/a"), None);
        assert_eq!(agent_mode("plugin-gate:p"), None);
    }

    // AC-1704 (T-1704): pause holds agent work only; a limit counts that
    // mode's running agent jobs and the queued ones waiting ahead.
    #[test]
    fn pause_and_limits_admit_or_hold_new_agent_work() {
        let jobs = [
            job(1, "area-ask:local", "running"),
            job(2, "escalate:g:local", "queued"),
            job(3, "area-ask:cloud", "done"),
            job(4, "ingest-source-v1:src_1", "running"),
        ];
        let none_ahead = |_: &Job| false;
        let all_ahead = |_: &Job| true;
        let open = AgentPoolControls::default();
        assert!(admit(&open, &jobs, "area-ask:local", all_ahead).is_ok());

        let paused = AgentPoolControls {
            paused: true,
            ..open
        };
        assert!(
            admit(&paused, &jobs, "area-ask:cloud", none_ahead)
                .unwrap_err()
                .contains("paused")
        );
        assert!(admit(&paused, &jobs, "ingest-source-v1:src_2", none_ahead).is_ok());

        let limited = AgentPoolControls {
            local_limit: 2,
            cloud_limit: 1,
            ..open
        };
        let held = admit(&limited, &jobs, "section-compose:D1:1:all:local", all_ahead).unwrap_err();
        assert!(held.contains("limit of 2 local agent jobs"), "{held}");
        // A queued row with no live waiter ahead (left by an earlier run)
        // does not count, and the finished cloud job does not count either.
        assert!(
            admit(
                &limited,
                &jobs,
                "section-compose:D1:1:all:local",
                none_ahead
            )
            .is_ok()
        );
        assert!(admit(&limited, &jobs, "area-ask:cloud", all_ahead).is_ok());
    }

    // AC-1704 (T-1704): controls persist, default to no gate and reject an
    // out-of-range limit without changing what is stored.
    #[test]
    fn controls_persist_and_reject_out_of_range_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(
            store.agent_pool_controls().unwrap(),
            AgentPoolControls::default()
        );
        let set = AgentPoolControls {
            paused: true,
            local_limit: 1,
            cloud_limit: 4,
        };
        store.set_agent_pool_controls(&set).unwrap();
        let too_many = AgentPoolControls {
            cloud_limit: MAX_LIMIT + 1,
            ..set
        };
        assert!(store.set_agent_pool_controls(&too_many).is_err());
        drop(store);
        assert_eq!(
            SettingsStore::open(&path)
                .unwrap()
                .agent_pool_controls()
                .unwrap(),
            set
        );
    }

    fn set(state: &AppState, controls: AgentPoolControls) {
        state
            .settings
            .lock()
            .unwrap()
            .set_agent_pool_controls(&controls)
            .unwrap();
    }

    fn status(state: &AppState, id: i64) -> String {
        state.jobs.lock().unwrap().get(id).unwrap().status
    }

    // AC-1705 (T-1705): work over the limit is held as queued and starts,
    // in order, when a running job finishes; other modes are not held.
    #[test]
    fn held_agent_work_starts_in_order_when_a_slot_frees() {
        let (_dir, state) = crate::area_briefing::tests::state();
        set(
            &state,
            AgentPoolControls {
                paused: false,
                local_limit: 1,
                cloud_limit: 0,
            },
        );
        let (first, _first) = start_job_blocking(&state, "area-ask:local", &|_| {}).unwrap();
        assert!(start_job_blocking(&state, "area-ask:cloud", &|_| {}).is_ok());
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let second = scope.spawn(|| {
                let tx = held_tx.clone();
                start_job_blocking(&state, "escalate:g:local", &move |job| {
                    tx.send(job.id).unwrap();
                })
                .map(|(job, _)| job.id)
            });
            let second_id = held_rx.recv().unwrap();
            let third = scope.spawn(|| {
                let tx = held_tx.clone();
                start_job_blocking(&state, "area-ask:local", &move |job| {
                    tx.send(job.id).unwrap();
                })
                .map(|(job, _)| job.id)
            });
            let third_id = held_rx.recv().unwrap();
            assert_eq!(status(&state, second_id), "queued");
            assert_eq!(status(&state, third_id), "queued");

            state.jobs.lock().unwrap().finish(first.id, &[]).unwrap();
            assert_eq!(second.join().unwrap().unwrap(), second_id);
            assert_eq!(status(&state, second_id), "running");
            // The later job is still held behind the one that just started.
            std::thread::sleep(HOLD_POLL * 3);
            assert_eq!(status(&state, third_id), "queued");
            state.jobs.lock().unwrap().finish(second_id, &[]).unwrap();
            assert_eq!(third.join().unwrap().unwrap(), third_id);
        });
    }

    // AC-1705 (T-1705): a paused pool holds agent work until it is resumed,
    // a cancelled held job never starts, and other work is not held.
    #[test]
    fn paused_work_waits_for_resume_and_cancelling_ends_the_wait() {
        let (_dir, state) = crate::area_briefing::tests::state();
        let paused = AgentPoolControls {
            paused: true,
            local_limit: 0,
            cloud_limit: 0,
        };
        set(&state, paused);
        assert!(start_job_blocking(&state, "plugin-gate:p", &|_| {}).is_ok());
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let resumed = scope.spawn(|| {
                let tx = held_tx.clone();
                start_job_blocking(&state, "area-ask:cloud", &move |job| {
                    tx.send(job.id).unwrap();
                })
                .map(|(job, _)| job.id)
            });
            let resumed_id = held_rx.recv().unwrap();
            let cancelled = scope.spawn(|| {
                let tx = held_tx.clone();
                start_job_blocking(&state, "area-ask:local", &move |job| {
                    tx.send(job.id).unwrap();
                })
                .map(|(job, _)| job.id)
            });
            let cancelled_id = held_rx.recv().unwrap();
            state.jobs.lock().unwrap().cancel(cancelled_id).unwrap();
            assert_eq!(cancelled.join().unwrap().unwrap_err(), "cancelled");
            assert_eq!(status(&state, resumed_id), "queued");
            set(
                &state,
                AgentPoolControls {
                    paused: false,
                    ..paused
                },
            );
            assert_eq!(resumed.join().unwrap().unwrap(), resumed_id);
        });
    }
}
