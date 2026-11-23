//! Dispatch against the scripted ACP agent (ADR-0092, AC-1344).

use acp_host::{AgentCommand, DispatchRequest, Limits, StopReason, dispatch};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const PACKET: &str = "0123456789abcdef0123456789abcdef";

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    repo: std::path::PathBuf,
    base: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("fixture.txt"),
        "header\nrust.axum.routes\ntrailer\n",
    )
    .unwrap();
    // A hook in the person's repository must never run for the agent's commit.
    std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
    let hook = format!("#!/bin/sh\ntouch '{}'\n", root.join("hook-ran").display());
    std::fs::write(repo.join(".git/hooks/post-commit"), hook).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let hook = repo.join(".git/hooks/post-commit");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "core.hooksPath=/nonexistent",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    );
    let base = git(&repo, &["rev-parse", "HEAD"]);
    Fixture {
        _dir: dir,
        root,
        repo,
        base,
    }
}

fn agent(script: &str) -> AgentCommand {
    AgentCommand {
        program: env!("CARGO_BIN_EXE_acp-fake-agent").into(),
        args: Vec::new(),
        env: vec![("ACP_FAKE_SCRIPT".into(), script.into())],
    }
}

fn request<'a>(f: &'a Fixture, agent: &'a AgentCommand, limits: Limits) -> DispatchRequest<'a> {
    DispatchRequest {
        packet_id: PACKET,
        prompt: "Write the adapter for rust.axum.routes; fixtures first.",
        repo: &f.repo,
        base_rev: &f.base,
        worktree_root: &f.root,
        agent,
        limits,
    }
}

#[test]
fn a_dispatched_packet_comes_back_as_a_committed_branch_confined_to_its_worktree() {
    let f = fixture();
    let agent = agent("work");
    let submission = dispatch(&request(&f, &agent, Limits::default())).unwrap();

    assert_eq!(submission.stop, StopReason::EndTurn);
    assert_eq!(submission.branch, "cartograph/packet-0123456789ab");
    assert_eq!(submission.changed_paths, ["src/adapter.rs"]);
    let head = submission.head_rev.as_deref().expect("a commit");
    assert_eq!(
        git(&f.repo, &["show", &format!("{head}:src/adapter.rs")]),
        "// covers rust.axum.routes"
    );
    assert_eq!(git(&f.repo, &["rev-parse", &format!("{head}^")]), f.base);
    assert_eq!(
        git(&f.repo, &["rev-parse", "cartograph/packet-0123456789ab"]),
        head
    );

    let report = submission.transcript[0]["update"]["content"]["text"]
        .as_str()
        .unwrap();
    assert_eq!(
        report,
        "read=rust.axum.routes;write=ok;escape=-32000;hook=-32000;execute=reject;edit=allow;terminal=-32601"
    );
    let methods: Vec<&str> = submission
        .refused
        .iter()
        .map(|r| r.method.as_str())
        .collect();
    assert_eq!(
        methods,
        [
            "fs/write_text_file",
            "fs/write_text_file",
            "session/request_permission",
            "terminal/create"
        ]
    );

    assert!(!f.root.join("escape.txt").exists());
    assert!(!f.root.join("hook-ran").exists(), "a repository hook ran");
    // The person's checkout is untouched.
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), f.base);
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
}

#[test]
fn a_turn_past_its_limit_is_cancelled_and_commits_nothing() {
    let f = fixture();
    let agent = agent("hang");
    let limits = Limits {
        turn: Duration::from_millis(300),
        cancel_grace: Duration::from_secs(5),
        ..Limits::default()
    };
    let submission = dispatch(&request(&f, &agent, limits)).unwrap();
    assert_eq!(submission.stop, StopReason::TimedOut);
    assert_eq!(submission.head_rev, None);
    assert_eq!(
        git(&f.repo, &["rev-parse", "cartograph/packet-0123456789ab"]),
        f.base
    );
}

#[test]
fn an_agent_that_exits_mid_turn_commits_nothing() {
    let f = fixture();
    let agent = agent("exit");
    let submission = dispatch(&request(&f, &agent, Limits::default())).unwrap();
    assert_eq!(submission.stop, StopReason::AgentExited);
    assert_eq!(submission.head_rev, None);
}

#[test]
fn a_packet_id_that_is_not_hexadecimal_is_refused_before_any_worktree() {
    let f = fixture();
    let agent = agent("work");
    let mut bad = request(&f, &agent, Limits::default());
    bad.packet_id = "../../outside";
    assert!(dispatch(&bad).is_err());
    let mut bad_base = request(&f, &agent, Limits::default());
    bad_base.base_rev = "--upload-pack=x";
    assert!(dispatch(&bad_base).is_err());
    assert!(!f.root.join("packet-0123456789ab").exists());
}
