//! The git worktree a dispatched agent works in.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A git step failed.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    /// The base revision is not a hexadecimal object id.
    #[error("base revision must be a hexadecimal object id")]
    InvalidBase,
    /// The worktree directory already exists.
    #[error("worktree path already exists: {0}")]
    Exists(PathBuf),
    /// git could not be run.
    #[error("git could not be run: {0}")]
    Spawn(#[from] std::io::Error),
    /// git exited unsuccessfully.
    #[error("git {args} failed: {stderr}")]
    Git {
        /// The git arguments.
        args: String,
        /// git's error output.
        stderr: String,
    },
}

/// A worktree on its own branch, outside the person's checkout.
#[derive(Debug, Clone)]
pub struct Worktree {
    path: PathBuf,
    branch: String,
    hooks: PathBuf,
}

/// Runs git in `dir` with repository hooks disabled: `hooks` names a
/// directory that is never created, so no hook from repository content runs.
fn git(dir: &Path, hooks: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !output.status.success() {
        return Err(WorktreeError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

impl Worktree {
    /// Adds a worktree at `path` on a new `branch` at `base`, a hexadecimal
    /// object id of 7 to 64 characters.
    pub fn add(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<Self, WorktreeError> {
        if !(7..=64).contains(&base.len()) || !base.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(WorktreeError::InvalidBase);
        }
        if path.exists() {
            return Err(WorktreeError::Exists(path.to_owned()));
        }
        let hooks = path.with_extension("no-hooks");
        let path_arg = path.to_string_lossy();
        git(
            repo,
            &hooks,
            &["worktree", "add", "-q", "-b", branch, &path_arg, base],
        )?;
        Ok(Self {
            path: path.to_owned(),
            branch: branch.to_owned(),
            hooks,
        })
    }

    /// The worktree directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The worktree's branch.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Commits every change in the worktree with `message` and returns the
    /// new head and the changed paths, or `None` and no paths when nothing
    /// changed.
    pub fn commit_all(
        &self,
        message: &str,
    ) -> Result<(Option<String>, Vec<String>), WorktreeError> {
        git(&self.path, &self.hooks, &["add", "-A"])?;
        let changed: Vec<String> = git(
            &self.path,
            &self.hooks,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--cached",
                "--name-only",
                "--no-renames",
            ],
        )?
        .lines()
        .map(str::to_owned)
        .collect();
        if changed.is_empty() {
            return Ok((None, changed));
        }
        git(
            &self.path,
            &self.hooks,
            &[
                "-c",
                "user.name=Cartograph",
                "-c",
                "user.email=cartograph@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--no-verify",
                "-m",
                message,
            ],
        )?;
        let head = git(&self.path, &self.hooks, &["rev-parse", "HEAD"])?;
        Ok((Some(head), changed))
    }
}
