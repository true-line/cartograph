//! ACP v1 client host: dispatches one reviewed work packet to a coding agent
//! confined to its own git worktree, and returns the agent's change as a
//! local branch for human review (ADR-0092, #998).
//!
//! The host never pushes, opens a pull request or merges, never runs
//! repository hooks, and allows the agent only file reads and writes inside
//! the worktree.

mod policy;
mod rpc;
mod worktree;

pub use policy::{PathPolicy, PathRefusal, PermissionDecision, decide_permission};
pub use rpc::PROTOCOL_VERSION;
pub use worktree::{Worktree, WorktreeError};

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The ACP agent to launch. It comes from operator settings, never from
/// packet or repository text.
#[derive(Debug, Clone)]
pub struct AgentCommand {
    /// Program to run.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
}

/// Bounds on one dispatch.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Wall-clock limit for the whole session.
    pub turn: Duration,
    /// How long the agent has to answer after `session/cancel`.
    pub cancel_grace: Duration,
    /// Session updates kept in the transcript.
    pub max_updates: usize,
    /// Largest file the agent may read or write, in bytes.
    pub max_file_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            turn: Duration::from_secs(30 * 60),
            cancel_grace: Duration::from_secs(10),
            max_updates: 2_000,
            max_file_bytes: 1024 * 1024,
        }
    }
}

/// How the agent's turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The agent finished its turn.
    EndTurn,
    /// The agent ran out of tokens.
    MaxTokens,
    /// The agent hit its own request limit.
    MaxTurnRequests,
    /// The agent refused.
    Refusal,
    /// The agent reported the turn cancelled without the host cancelling it.
    Cancelled,
    /// The host's turn limit passed; the host cancelled the turn.
    TimedOut,
    /// The agent exited or closed its output before the turn ended.
    AgentExited,
    /// The agent broke the protocol.
    ProtocolError(String),
}

/// A request the host refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The agent's method.
    pub method: String,
    /// Why it was refused.
    pub detail: String,
}

/// One packet to dispatch.
#[derive(Debug, Clone)]
pub struct DispatchRequest<'a> {
    /// The packet id (hexadecimal, at least 12 characters).
    pub packet_id: &'a str,
    /// The prompt: the packet's exported text.
    pub prompt: &'a str,
    /// The person's repository.
    pub repo: &'a Path,
    /// The revision the packet was made from.
    pub base_rev: &'a str,
    /// Directory that holds dispatch worktrees, outside the person's checkout.
    pub worktree_root: &'a Path,
    /// The agent to run.
    pub agent: &'a AgentCommand,
    /// Bounds.
    pub limits: Limits,
}

/// The agent's work, as a local branch for review.
#[derive(Debug, Clone)]
pub struct Submission {
    /// The packet dispatched.
    pub packet_id: String,
    /// The worktree's branch.
    pub branch: String,
    /// The worktree directory.
    pub worktree: PathBuf,
    /// The revision the branch starts from.
    pub base_rev: String,
    /// The commit holding the agent's change; `None` when the turn did not
    /// end with `end_turn` or changed nothing.
    pub head_rev: Option<String>,
    /// Paths the commit changed.
    pub changed_paths: Vec<String>,
    /// How the turn ended.
    pub stop: StopReason,
    /// The agent's `session/update` params, in order.
    pub transcript: Vec<Value>,
    /// Whether updates past the limit were dropped.
    pub transcript_truncated: bool,
    /// Requests the host refused.
    pub refused: Vec<Refusal>,
}

/// Dispatch failed before the agent's turn could end.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    /// The packet id is not hexadecimal or is shorter than 12 characters.
    #[error("packet id must be hexadecimal and at least 12 characters")]
    InvalidPacketId,
    /// The worktree could not be made or committed.
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    /// The agent could not be started.
    #[error("agent could not be started: {0}")]
    Spawn(#[source] std::io::Error),
}

/// Characters of the packet id used in branch and directory names.
const ID_PREFIX: usize = 12;

/// Dispatches one packet: adds a worktree on `cartograph/packet-<id>` at the
/// base revision, runs one ACP prompt turn there, and commits the change
/// only when the turn ends with `end_turn`.
pub fn dispatch(request: &DispatchRequest<'_>) -> Result<Submission, DispatchError> {
    let id = request.packet_id;
    if id.len() < ID_PREFIX || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DispatchError::InvalidPacketId);
    }
    let short = &id[..ID_PREFIX];
    let branch = format!("cartograph/packet-{short}");
    let worktree = Worktree::add(
        request.repo,
        &request.worktree_root.join(format!("packet-{short}")),
        &branch,
        request.base_rev,
    )?;
    // The agent gets the plain absolute path (not a Windows verbatim one);
    // the policy compares resolved paths.
    let cwd = std::path::absolute(worktree.path()).map_err(DispatchError::Spawn)?;
    let policy = PathPolicy::new(&cwd).map_err(DispatchError::Spawn)?;
    let outcome = rpc::run_session(
        request.agent,
        &cwd,
        request.prompt,
        &policy,
        &request.limits,
    )
    .map_err(DispatchError::Spawn)?;
    let (head_rev, changed_paths) = if outcome.stop == StopReason::EndTurn {
        worktree.commit_all(&format!(
            "Work packet {id}\n\nDispatched over ACP by Cartograph."
        ))?
    } else {
        (None, Vec::new())
    };
    Ok(Submission {
        packet_id: id.to_owned(),
        branch,
        worktree: worktree.path().to_owned(),
        base_rev: request.base_rev.to_owned(),
        head_rev,
        changed_paths,
        stop: outcome.stop,
        transcript: outcome.transcript,
        transcript_truncated: outcome.transcript_truncated,
        refused: outcome.refused,
    })
}
