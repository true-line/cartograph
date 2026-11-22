//! Agent pool controls (#252, AC-1704): pause new agent work, and cap how
//! many agent jobs run at once per provider mode. The controls gate only
//! where agent work starts as a job (topic expert questions, section drafts
//! and gap escalations); they never stop work already running, and they do
//! not apply to specialist investigations, which keep their own runner.
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::AppState;
use crate::job_execution::JobExecution;
use crate::jobs::{ClaimMode, Job};

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

/// Whether agent work of `kind` may start next to `jobs`. Other work is
/// always admitted.
pub(crate) fn admit(controls: &AgentPoolControls, jobs: &[Job], kind: &str) -> Result<(), String> {
    let Some(mode) = agent_mode(kind) else {
        return Ok(());
    };
    if controls.paused {
        return Err(
            "Agent work is paused in the agent pool. Resume it there to start this.".into(),
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
        .filter(|job| matches!(job.status.as_str(), "queued" | "running"))
        .filter(|job| agent_mode(&job.kind) == Some(mode))
        .count();
    if active >= limit as usize {
        return Err(format!(
            "The agent pool already runs {active} {mode} agent {}, its limit. Wait for one to finish or raise the limit.",
            if active == 1 { "job" } else { "jobs" }
        ));
    }
    Ok(())
}

/// Start agent work of `kind` as a job when the controls admit it. The
/// check and the enqueue happen under one jobs lock, so two starts cannot
/// both take the last free place.
pub(crate) fn start_job(state: &AppState, kind: &str) -> Result<(Job, JobExecution), String> {
    let controls = state
        .settings
        .lock()
        .map_err(|error| error.to_string())?
        .agent_pool_controls()
        .map_err(|error| error.to_string())?;
    let plan = {
        let mut jobs = state.jobs.lock().map_err(|error| error.to_string())?;
        let listed = jobs.list().map_err(|error| error.to_string())?;
        admit(&controls, &listed, kind)?;
        let job = jobs.enqueue(kind).map_err(|error| error.to_string())?;
        jobs.claim_plan(job.id, ClaimMode::StartQueued)
            .map_err(|error| error.to_string())?
    };
    crate::claim_job(state, &plan)
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

    // AC-1704 (T-1704): pause refuses agent work only; a limit counts that
    // mode's queued and running agent jobs.
    #[test]
    fn pause_and_limits_admit_or_refuse_new_agent_work() {
        let jobs = [
            job(1, "area-ask:local", "running"),
            job(2, "escalate:g:local", "queued"),
            job(3, "area-ask:cloud", "done"),
            job(4, "ingest-source-v1:src_1", "running"),
        ];
        let open = AgentPoolControls::default();
        assert!(admit(&open, &jobs, "area-ask:local").is_ok());

        let paused = AgentPoolControls {
            paused: true,
            ..open
        };
        assert!(
            admit(&paused, &jobs, "area-ask:cloud")
                .unwrap_err()
                .contains("paused")
        );
        assert!(admit(&paused, &jobs, "ingest-source-v1:src_2").is_ok());

        let limited = AgentPoolControls {
            local_limit: 2,
            cloud_limit: 1,
            ..open
        };
        let refused = admit(&limited, &jobs, "section-compose:D1:1:all:local").unwrap_err();
        assert!(
            refused.contains("2 local agent jobs, its limit"),
            "{refused}"
        );
        // The finished cloud job does not count.
        assert!(admit(&limited, &jobs, "area-ask:cloud").is_ok());
        let raised = AgentPoolControls {
            local_limit: 3,
            ..limited
        };
        assert!(admit(&raised, &jobs, "area-ask:local").is_ok());
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

    // AC-1704 (T-1704): the host's agent start path applies the stored
    // controls; work that is not agent work still starts while paused.
    #[test]
    fn agent_jobs_start_through_the_stored_controls() {
        let refused = |state: &AppState, kind: &str| match start_job(state, kind) {
            Ok(_) => panic!("{kind} started"),
            Err(error) => error,
        };
        let (_dir, state) = crate::area_briefing::tests::state();
        let limited = AgentPoolControls {
            paused: false,
            local_limit: 1,
            cloud_limit: 0,
        };
        state
            .settings
            .lock()
            .unwrap()
            .set_agent_pool_controls(&limited)
            .unwrap();
        let (first, _execution) = start_job(&state, "area-ask:local").unwrap();
        assert!(refused(&state, "escalate:g:local").contains("its limit"));
        assert!(start_job(&state, "area-ask:cloud").is_ok());
        state.jobs.lock().unwrap().finish(first.id, &[]).unwrap();
        assert!(start_job(&state, "escalate:g:local").is_ok());

        let paused = AgentPoolControls {
            paused: true,
            ..limited
        };
        state
            .settings
            .lock()
            .unwrap()
            .set_agent_pool_controls(&paused)
            .unwrap();
        assert!(refused(&state, "area-ask:cloud").contains("paused"));
        assert!(start_job(&state, "plugin-gate:p").is_ok());
    }
}
