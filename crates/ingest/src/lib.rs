//! GitHub repo ingest (SPEC-00 §10, US-0001): auth ladder, shallow
//! read-only clone, real repo identity.
//!
//! The auth ladder for v1 (ADR-0009): an explicit token from
//! `GH_TOKEN`/`GITHUB_TOKEN`, else a `gh auth token` shell-out for
//! environments already authenticated. Clones are `git2` shallow
//! ([`HISTORY_DEPTH`]), land in a temp directory and only move into place on
//! success — an auth failure leaves **no partial clone** (AC-0003) and
//! maps to a typed error carrying remediation text.

pub mod adapter_lookup;
pub mod adapter_packet;
pub mod api_docs;
mod captured_walk;
pub mod coverage;
pub mod entry_points;
pub mod environments;
pub mod managed;
pub mod manifest;
pub mod manifest_draft;
pub mod matrix;
pub mod pipelines;
pub mod preflight;
pub mod schedules;
pub mod toolchain;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Ingest errors. `Auth` carries remediation (AC-0003).
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// Authentication/authorization failure — no partial clone remains.
    #[error("auth failure cloning {url}: {message}. {remediation}")]
    Auth {
        /// The URL that failed.
        url: String,
        /// Underlying git message.
        message: String,
        /// What the user can do about it.
        remediation: String,
    },
    /// The URL is not a recognized GitHub (or file://) repo reference.
    #[error("unrecognized repo URL: {0}")]
    InvalidUrl(String),
    /// A managed origin is unsupported, inconsistent or exceeds its bound.
    #[error("invalid managed source origin")]
    InvalidManagedOrigin,
    /// Registry/source IDs or the host storage path are invalid.
    #[error("invalid managed source identity or host storage path")]
    InvalidManagedIdentity,
    /// Another participating process currently holds an incompatible source guard.
    #[error("managed source is busy")]
    SourceBusy,
    /// A private slot, lock or checkout does not have the expected ownership.
    #[error("managed source ownership is invalid or unavailable")]
    SourceOwnership,
    /// Publication could not complete; preserved attempt data may require recovery.
    #[error("managed source publication failed; source remains unavailable")]
    SourcePublication,
    /// Any other git failure.
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A completed read-only clone.
#[derive(Debug, Clone)]
pub struct ClonedRepo {
    /// Registered repo identity (`owner/name`, or `local/<source_id>`).
    pub repo: String,
    /// Resolved HEAD commit SHA (AC-0001: listed with commit SHA).
    pub commit_sha: String,
    /// Where the working tree lives.
    pub path: PathBuf,
}

/// Where `gh` is commonly installed outside an app's PATH. A macOS app
/// opened from Finder or the Dock gets launchd's PATH
/// (`/usr/bin:/bin:/usr/sbin:/sbin`), not the shell's, so a Homebrew `gh`
/// is invisible to it by name (#992).
pub const GH_FALLBACK_PATHS: &[&str] = &[
    "/opt/homebrew/bin/gh",
    "/usr/local/bin/gh",
    "/home/linuxbrew/.linuxbrew/bin/gh",
];

/// Discover a token from the v1 auth ladder: `GH_TOKEN`, `GITHUB_TOKEN`,
/// then `gh auth token` (ADR-0009), by name and then at
/// [`GH_FALLBACK_PATHS`]. `None` means anonymous (public repos).
pub fn discover_token() -> Option<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(t) = std::env::var(var)
            && !t.trim().is_empty()
        {
            return Some(t.trim().to_string());
        }
    }
    std::iter::once("gh")
        .chain(GH_FALLBACK_PATHS.iter().copied())
        .find_map(gh_token)
}

/// `program auth token`, when it runs and prints a token.
fn gh_token(program: &str) -> Option<String> {
    let mut gh = Command::new(program);
    gh.args(["auth", "token"]);
    hide_console_window(&mut gh);
    let out = gh.output().ok()?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !token.is_empty()).then_some(token)
}

/// On Windows, a GUI app spawning a console program (`gh`) flashes a console
/// window unless the child is created without one (AC-0222, #226). A no-op
/// everywhere else.
fn hide_console_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Parse a repo reference into `(identity, clone_url)`. Accepts
/// `https://github.com/o/n(.git)`, `git@github.com:o/n(.git)`, bare
/// `o/n` shorthand, and `file://` URLs (offline tests, local mirrors).
pub fn parse_repo_url(url: &str) -> Result<(String, String), IngestError> {
    let url = url.trim().trim_end_matches('/');
    if let Some(rest) = url.strip_prefix("file://") {
        let name = rest.rsplit('/').next().filter(|s| !s.is_empty());
        let name = name.ok_or_else(|| IngestError::InvalidUrl(url.into()))?;
        let name = name.strip_suffix(".git").unwrap_or(name);
        return Ok((format!("local/{name}"), url.to_string()));
    }
    let path = if let Some(rest) = url.strip_prefix("https://github.com/") {
        Some(rest)
    } else if let Some(rest) = url.strip_prefix("git@github.com:") {
        Some(rest)
    } else if url.split('/').count() == 2 && !url.contains(':') && !url.contains(' ') {
        Some(url)
    } else {
        None
    };
    let path = path.ok_or_else(|| IngestError::InvalidUrl(url.into()))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty() => Ok((
            format!("{owner}/{name}"),
            format!("https://github.com/{owner}/{name}.git"),
        )),
        _ => Err(IngestError::InvalidUrl(url.into())),
    }
}

/// Classify a git2 error: auth-shaped failures become [`IngestError::Auth`]
/// with remediation (AC-0003); anything else passes through.
fn classify_git_error(url: &str, e: git2::Error, token_found: bool) -> IngestError {
    let msg = e.message().to_ascii_lowercase();
    let authish = matches!(
        e.class(),
        git2::ErrorClass::Http | git2::ErrorClass::Ssh | git2::ErrorClass::Callback
    ) && (msg.contains("auth")
        || msg.contains("401")
        || msg.contains("403")
        || msg.contains("credential")
        || msg.contains("permission"));
    if authish {
        IngestError::Auth {
            url: url.to_string(),
            message: e.message().to_string(),
            remediation: if token_found {
                "A GitHub credential was found but refused. Check that its account can \
                 read the repo, then set GH_TOKEN (or GITHUB_TOKEN) to a token with repo \
                 read scope, or run `gh auth login`."
                    .to_string()
            } else {
                format!(
                    "No GitHub credential was found: GH_TOKEN and GITHUB_TOKEN are unset, \
                     and `gh auth token` gave no token on PATH or at {}. Set GH_TOKEN (or \
                     GITHUB_TOKEN) to a token with repo read scope, or run `gh auth login`.",
                    GH_FALLBACK_PATHS.join(", ")
                )
            },
        }
    } else {
        IngestError::Git(e)
    }
}

/// Commits a remote clone fetches: still shallow, but deep enough for the
/// knowledge phase's commit units (AC-1516), 2,048 first-parent commits
/// and the tree of the oldest one's parent.
pub const HISTORY_DEPTH: i32 = 2049;

/// Clone only into a caller-owned fresh attempt path. This primitive cannot
/// replace an existing destination; managed publication owns that separate step.
fn clone_into_new(
    clone_url: &str,
    destination: &Path,
    token: Option<&str>,
) -> Result<String, IngestError> {
    match std::fs::symlink_metadata(destination) {
        Ok(_) => return Err(IngestError::SourceOwnership),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let callbacks = || {
        let mut callbacks = git2::RemoteCallbacks::new();
        if let Some(token) = token {
            let token = token.to_string();
            callbacks.credentials(move |_url, _user, _kinds| {
                // GitHub accepts a token as the password with any username.
                git2::Cred::userpass_plaintext("x-access-token", &token)
            });
        }
        callbacks
    };
    let mut fetch = git2::FetchOptions::new();
    fetch.remote_callbacks(callbacks());
    if !clone_url.starts_with("file://") {
        fetch.depth(HISTORY_DEPTH); // shallow per SPEC §10 (local transport can't)
    }
    // Fetch only the default branch. A full clone wrote one loose ref file per
    // remote branch, and libgit2 caps ref paths at MAX_PATH on Windows even
    // with core.longpaths, so a deep dependabot branch name failed the whole
    // recovery with "path too long" (Windows recovery fix). It also fetches less.
    let branch = default_branch(clone_url, callbacks());

    // Clone without writing the tree, then check it out with the platform's
    // newline conversion off (#227): Git for Windows sets core.autocrlf=true
    // system-wide, which rewrote every LF as CRLF and moved each cited byte
    // offset and content hash away from the committed blob.
    let mut deferred = git2::build::CheckoutBuilder::new();
    deferred.dry_run();
    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fetch).with_checkout(deferred);
    if let Some(branch) = &branch {
        builder.branch(branch);
    }
    builder.remote_create(|repo, name, url| {
        // Working-tree paths past MAX_PATH check out on Windows (a no-op
        // elsewhere); ref paths stay capped, hence the single-branch refspec.
        repo.config()?.set_bool("core.longpaths", true)?;
        match &branch {
            Some(branch) => repo.remote_with_fetch(
                name,
                url,
                &format!("+refs/heads/{branch}:refs/remotes/{name}/{branch}"),
            ),
            None => repo.remote(name, url),
        }
    });
    let result = builder.clone(clone_url, destination);
    let repo = match result {
        Ok(repo) => repo,
        Err(e) => {
            return Err(classify_git_error(clone_url, e, token.is_some()));
        }
    };
    repo.config()
        .and_then(|mut config| config.set_bool("core.autocrlf", false))
        .and_then(|()| repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force())))
        .map_err(|e| classify_git_error(clone_url, e, token.is_some()))?;
    let commit_sha = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map(|c| c.id().to_string())
        .map_err(|e| classify_git_error(clone_url, e, token.is_some()))?;
    drop(repo);
    Ok(commit_sha)
}

/// The remote's default branch name (`main` for `refs/heads/main`), from a
/// ref advertisement alone. `None` (an empty remote, a detached HEAD, a
/// failed connection) leaves the clone to libgit2's defaults, which then
/// report any connection failure through the usual classification.
fn default_branch(clone_url: &str, callbacks: git2::RemoteCallbacks<'_>) -> Option<String> {
    let mut remote = git2::Remote::create_detached(clone_url).ok()?;
    let connection = remote
        .connect_auth(git2::Direction::Fetch, Some(callbacks), None)
        .ok()?;
    let head = connection.default_branch().ok()?;
    let name = head.as_str().ok()?.strip_prefix("refs/heads/")?;
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests;
