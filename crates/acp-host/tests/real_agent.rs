//! Opt-in smoke against a real, already-installed ACP agent (AC-1344, ADR-0092, #998).
//!
//! Ignored by default and never run in CI. On a machine with an ACP adapter:
//!
//! ```text
//! ACP_SMOKE_AGENT=/absolute/path/to/acp-adapter \
//! ACP_SMOKE_ARGS=$'--flag\nvalue' \
//! ACP_SMOKE_REPO=/absolute/path/to/clean/checkout \
//! cargo test -p acp-host --test real_agent -- --ignored --nocapture
//! ```
//!
//! `ACP_SMOKE_ARGS` is optional, one argument per line. The checkout must be a
//! clean git repository; it gains a `cartograph/packet-*` branch and worktree
//! for the run, both removed afterwards unless `ACP_SMOKE_KEEP` is set.

use acp_host::{AgentCommand, DispatchRequest, Limits, StopReason, dispatch};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TARGET: &str = "CARTOGRAPH_ACP_SMOKE.md";

fn git(dir: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    )
}

/// Removes the dispatch's worktree and branch from the person's checkout.
fn clean_up(repo: &Path, worktree: &Path, branch: &str) {
    let worktree = worktree.to_string_lossy().into_owned();
    git(repo, &["worktree", "remove", "--force", &worktree]);
    git(repo, &["worktree", "prune"]);
    git(repo, &["branch", "-D", branch]);
}

fn env_path(name: &str) -> PathBuf {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let path = PathBuf::from(value);
    assert!(path.is_absolute(), "{name} must be an absolute path");
    path
}

#[test]
#[ignore = "needs an installed ACP agent; run by hand with ACP_SMOKE_* set"]
fn a_real_acp_agent_returns_a_committed_branch_confined_to_its_worktree() {
    let program = env_path("ACP_SMOKE_AGENT");
    assert!(program.is_file(), "ACP_SMOKE_AGENT is not a file");
    let repo = env_path("ACP_SMOKE_REPO");
    let args: Vec<String> = std::env::var("ACP_SMOKE_ARGS")
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    let (ok, status) = git(&repo, &["status", "--porcelain"]);
    assert!(
        ok && status.is_empty(),
        "ACP_SMOKE_REPO must be a clean git checkout"
    );
    let (ok, base) = git(&repo, &["rev-parse", "HEAD"]);
    assert!(ok, "ACP_SMOKE_REPO has no HEAD");

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let packet = format!("{nanos:016x}");
    let keep = std::env::var_os("ACP_SMOKE_KEEP").is_some();
    // With ACP_SMOKE_KEEP the worktree must outlive the test, so the root is
    // kept rather than deleted when the TempDir drops.
    let roots = tempfile::tempdir().unwrap();
    let root = if keep {
        roots.keep()
    } else {
        roots.path().to_owned()
    };
    // The host's own naming (ADR-0092 decision 2), for cleanup when it fails.
    let expected_branch = format!("cartograph/packet-{}", &packet[..12]);
    let expected_worktree = root.join(format!("packet-{}", &packet[..12]));
    let agent = AgentCommand {
        program,
        args,
        env: Vec::new(),
    };
    let prompt = format!(
        "Cartograph ACP smoke test. Read README.md at the repository root if it exists. \
         Then create the file {TARGET} at the repository root containing one sentence that \
         says what this repository is. Do not run commands and do not change any other file."
    );
    let limits = Limits {
        turn: Duration::from_secs(600),
        ..Limits::default()
    };
    let submission = dispatch(&DispatchRequest {
        packet_id: &packet,
        prompt: &prompt,
        repo: &repo,
        base_rev: &base,
        worktree_root: &root,
        agent: &agent,
        limits,
    });
    let submission = match submission {
        Ok(submission) => submission,
        Err(error) => {
            clean_up(&repo, &expected_worktree, &expected_branch);
            panic!("dispatch failed: {error:?}");
        }
    };

    println!("branch: {}", submission.branch);
    println!("stop: {:?}", submission.stop);
    println!("head: {:?}", submission.head_rev);
    println!("changed: {:?}", submission.changed_paths);
    println!(
        "transcript: {} updates{}",
        submission.transcript.len(),
        if submission.transcript_truncated {
            " (truncated)"
        } else {
            ""
        }
    );
    for refusal in &submission.refused {
        println!("refused: {} ({})", refusal.method, refusal.detail);
    }

    let (_, after) = git(&repo, &["status", "--porcelain"]);
    if keep {
        println!(
            "kept: {} at {}",
            submission.branch,
            submission.worktree.display()
        );
    } else {
        clean_up(&repo, &submission.worktree, &submission.branch);
    }

    assert!(after.is_empty(), "the person's checkout changed: {after}");
    assert_eq!(submission.stop, StopReason::EndTurn);
    let head = submission.head_rev.expect("a committed branch");
    assert_ne!(head, base);
    assert!(
        submission.changed_paths.iter().any(|path| path == TARGET),
        "the agent did not write {TARGET}"
    );
    assert!(
        submission
            .changed_paths
            .iter()
            .all(|path| !path.starts_with('/')
                && !path.split('/').any(|part| part == ".." || part == ".git")),
        "a changed path escaped the worktree"
    );
}
