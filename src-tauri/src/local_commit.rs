//! Clean local git checkout commit discovery (#740, AC-0310).
//!
//! Reads the target repository's `.git` state through `libgit2` as untrusted
//! data: never shells out to `git`, never runs hooks, `fsmonitor`, or
//! config-defined filter drivers, and never searches parent directories.

use std::ffi::OsStr;
use std::path::Path;

const NOT_GIT_ROOT: &str = "not a git repository root";
const NO_HEAD_COMMIT: &str = "no HEAD commit";
const UNREADABLE: &str = "git state unreadable";
const MAX_DIRTY_FILES: usize = 10_000;
/// Index mode of a submodule entry.
const GITLINK_MODE: u32 = 0o160000;
/// Deepest submodule nesting checked before failing closed.
const MAX_SUBMODULE_DEPTH: usize = 8;

/// Commit identity recorded for a registered local source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalCommit {
    /// A clean git working tree at `root` whose `HEAD` resolves to `sha`.
    Clean { sha: String },
    /// A dirty, unborn, unreadable, or non-root local directory, recorded as
    /// `"workdir"` with `reason` explaining why.
    Workdir { reason: String },
}

impl LocalCommit {
    /// Error message when a clean local checkout changes before recovery publishes.
    pub(crate) const CHANGED_DURING_RECOVERY: &str =
        "The source changed during recovery. Run it again.";

    /// Commit string to record on extracted facts, graph nodes, coverage, and
    /// metrics (`HEAD`'s SHA when clean, `"workdir"` otherwise).
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Clean { sha } => sha.as_str(),
            Self::Workdir { .. } => "workdir",
        }
    }

    /// Note explaining why `commit` is `"workdir"`, or `None` when clean.
    pub(crate) fn commit_note(&self) -> Option<&str> {
        match self {
            Self::Clean { .. } => None,
            Self::Workdir { reason } => Some(reason.as_str()),
        }
    }

    /// Re-verify `root` before publishing: if the initial state was [`Self::Clean`]
    /// and the working tree is now dirty or at a different commit, fail closed.
    pub(crate) fn ensure_unchanged(&self, root: &Path) -> Result<(), String> {
        if matches!(self, Self::Clean { .. }) && local_commit(root) != *self {
            return Err(Self::CHANGED_DURING_RECOVERY.into());
        }
        Ok(())
    }
}

/// Inspect `root` without parent-directory discovery and return its clean
/// `HEAD` SHA or `"workdir"` with a bounded reason.
pub(crate) fn local_commit(root: &Path) -> LocalCommit {
    local_commit_at(root, 0)
}

/// [`local_commit`] for a checkout nested `depth` submodules below the source.
fn local_commit_at(root: &Path, depth: usize) -> LocalCommit {
    let Ok(canonical_root) = crate::paths::canonicalize(root) else {
        return LocalCommit::Workdir {
            reason: NOT_GIT_ROOT.into(),
        };
    };
    let Ok(repo) =
        git2::Repository::open_ext(root, git2::RepositoryOpenFlags::NO_SEARCH, &[] as &[&OsStr])
    else {
        return LocalCommit::Workdir {
            reason: NOT_GIT_ROOT.into(),
        };
    };
    if repo.is_bare() {
        return LocalCommit::Workdir {
            reason: NOT_GIT_ROOT.into(),
        };
    }
    let Some(workdir) = repo.workdir() else {
        return LocalCommit::Workdir {
            reason: NOT_GIT_ROOT.into(),
        };
    };
    let Ok(canonical_workdir) = crate::paths::canonicalize(workdir) else {
        return LocalCommit::Workdir {
            reason: NOT_GIT_ROOT.into(),
        };
    };
    if canonical_workdir != canonical_root {
        return LocalCommit::Workdir {
            reason: NOT_GIT_ROOT.into(),
        };
    }

    let head = match repo.head() {
        Ok(head) => head,
        Err(err) if err.code() == git2::ErrorCode::UnbornBranch => {
            return LocalCommit::Workdir {
                reason: NO_HEAD_COMMIT.into(),
            };
        }
        Err(_) => {
            return LocalCommit::Workdir {
                reason: UNREADABLE.into(),
            };
        }
    };
    let commit = match head.peel_to_commit() {
        Ok(commit) => commit,
        Err(err) if err.code() == git2::ErrorCode::UnbornBranch => {
            return LocalCommit::Workdir {
                reason: NO_HEAD_COMMIT.into(),
            };
        }
        Err(_) => {
            return LocalCommit::Workdir {
                reason: UNREADABLE.into(),
            };
        }
    };
    let sha = commit.id().to_string();
    if !is_valid_sha(&sha) {
        return LocalCommit::Workdir {
            reason: UNREADABLE.into(),
        };
    }

    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .exclude_submodules(false);
    let Ok(statuses) = repo.statuses(Some(&mut options)) else {
        return LocalCommit::Workdir {
            reason: UNREADABLE.into(),
        };
    };

    let mut dirty: usize = 0;
    for _ in statuses.iter() {
        dirty = dirty.saturating_add(1);
        if dirty > MAX_DIRTY_FILES {
            return LocalCommit::Workdir {
                reason: format!("more than {MAX_DIRTY_FILES} modified or untracked files"),
            };
        }
    }
    if dirty > 0 {
        return LocalCommit::Workdir {
            reason: format!("{dirty} modified or untracked files"),
        };
    }
    match untracked_inputs(&repo, root, depth) {
        Ok(0) => {}
        Ok(hidden) => {
            return LocalCommit::Workdir {
                reason: format!("{hidden} modified or untracked files"),
            };
        }
        Err(()) => {
            return LocalCommit::Workdir {
                reason: UNREADABLE.into(),
            };
        }
    }

    LocalCommit::Clean { sha }
}

/// Inputs recovery reads whose bytes `git status` does not vouch for.
///
/// Recovery reads every file the tree's own `.gitignore` files allow, plus
/// `.env` files regardless of `.gitignore` (`events::env_files`). Status also
/// honors `.git/info/exclude` and `core.excludesFile`, which the untrusted
/// `.git/config` controls, and skips ignored `.env` files, so such an input
/// would otherwise stamp a SHA on facts it does not contain. Three more cases
/// are counted:
/// - each `.gitignore` the walk applies must itself be tracked, since an
///   untracked one can ignore itself and still change what recovery reads;
/// - index entries marked assume-unchanged or skip-worktree, which libgit2
///   reports unmodified whatever their bytes (fail closed on any);
/// - files inside a submodule are tracked by its gitlink only when the
///   submodule checkout is itself clean at the gitlink's commit. Status cannot
///   vouch for that: the untrusted config's `diff.ignoreSubmodules` or
///   `submodule.<name>.ignore` can hide submodule changes, so each populated
///   submodule is checked recursively here and counts once when it is not.
fn untracked_inputs(repo: &git2::Repository, root: &Path, depth: usize) -> Result<usize, ()> {
    let index = repo.index().map_err(drop)?;
    let mut stale_submodules: usize = 0;
    for entry in index.iter().filter(|entry| entry.mode == GITLINK_MODE) {
        let rel = String::from_utf8(entry.path.clone()).map_err(drop)?;
        let sub_root = root.join(&rel);
        // An uninitialized submodule is an empty directory (or absent), so
        // recovery reads nothing from it.
        let populated = match std::fs::read_dir(&sub_root) {
            Ok(mut entries) => entries.next().is_some(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => true,
        };
        if !populated {
            continue;
        }
        let clean_at_gitlink = depth < MAX_SUBMODULE_DEPTH
            && matches!(
                local_commit_at(&sub_root, depth + 1),
                LocalCommit::Clean { sha } if sha == entry.id.to_string()
            );
        if !clean_at_gitlink {
            stale_submodules = stale_submodules.saturating_add(1);
        }
    }
    let flagged = index
        .iter()
        .filter(|entry| {
            git2::IndexEntryFlag::from_bits_truncate(entry.flags).is_valid()
                || git2::IndexEntryExtendedFlag::from_bits_truncate(entry.flags_extended)
                    .is_skip_worktree()
        })
        .count();
    let tracked = |rel: &str| {
        let path = Path::new(rel);
        index.get_path(path, 0).is_some()
            || path.ancestors().skip(1).any(|dir| {
                // git2 panics on an empty path; the root is never a gitlink.
                !dir.as_os_str().is_empty()
                    && index
                        .get_path(dir, 0)
                        .is_some_and(|entry| entry.mode == GITLINK_MODE)
            })
    };
    let mut dirs = Vec::new();
    let walked = source_walk::files_until(
        root,
        &|name| name == ".git",
        source_walk::Gitignores::Honor,
        &mut |step| {
            dirs.push(step.dir.to_owned());
            std::ops::ControlFlow::Continue(())
        },
    )
    .map_err(drop)?
    .ok_or(())?;
    // The walk applies a directory's `.gitignore` when it is a regular file,
    // even one that ignores itself and so never appears in `walked`.
    let gitignores = dirs.into_iter().filter_map(|dir| {
        let rel = if dir.is_empty() {
            ".gitignore".to_owned()
        } else {
            format!("{dir}/.gitignore")
        };
        std::fs::symlink_metadata(root.join(&rel))
            .is_ok_and(|meta| meta.is_file())
            .then_some(rel)
    });
    let inputs: std::collections::BTreeSet<String> = walked
        .into_iter()
        .map(|file| file.rel)
        .chain(gitignores)
        .chain(events::env_files(root).map_err(drop)?)
        // The env-read allowlist changes what recovery resolves even when the
        // repository ignores it (ADR-0075).
        .chain(
            std::fs::symlink_metadata(root.join(ingest::manifest::REPO_ENV_REL_PATH))
                .is_ok_and(|meta| meta.is_file())
                .then(|| ingest::manifest::REPO_ENV_REL_PATH.to_owned()),
        )
        // So does the dependency-types allowlist (ADR-0078).
        .chain(
            std::fs::symlink_metadata(root.join(adapters_lang_ts::DEPENDENCY_TYPES_REL_PATH))
                .is_ok_and(|meta| meta.is_file())
                .then(|| adapters_lang_ts::DEPENDENCY_TYPES_REL_PATH.to_owned()),
        )
        .collect();
    Ok(flagged
        .saturating_add(stale_submodules)
        .saturating_add(inputs.iter().filter(|rel| !tracked(rel)).count()))
}

fn is_valid_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64)
        && sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Build a clean git working tree with `files` committed on `HEAD`.
    pub(crate) fn init_clean_repo(root: &Path, files: &[(&str, &[u8])]) -> String {
        fs::create_dir_all(root).unwrap();
        let repo = git2::Repository::init(root).unwrap();
        let mut index = repo.index().unwrap();
        for (rel, contents) in files {
            let full = root.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, contents).unwrap();
            index.add_path(Path::new(rel)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature =
            git2::Signature::new("fixture", "fixture@example.invalid", &git2::Time::new(1, 0))
                .unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap()
            .to_string()
    }

    // AC-0310: a clean git checkout at the source root records HEAD's full SHA. (T-0310)
    #[test]
    fn clean_repo_returns_exact_head_sha() {
        let dir = tempfile::tempdir().unwrap();
        let sha = init_clean_repo(dir.path(), &[("src/app.ts", b"export const ok = 1;\n")]);
        assert_eq!(sha.len(), 40);
        assert_eq!(local_commit(dir.path()), LocalCommit::Clean { sha });
    }

    // AC-0310: modified, untracked, and deleted files each report `workdir`
    // with the count of modified or untracked files in the reason. (T-0310)
    #[test]
    fn dirty_repo_reports_modified_untracked_and_deleted_counts() {
        // 1. Modified tracked file.
        let modified = tempfile::tempdir().unwrap();
        init_clean_repo(modified.path(), &[("app.ts", b"export const a = 1;\n")]);
        fs::write(modified.path().join("app.ts"), b"export const a = 2;\n").unwrap();
        assert_eq!(
            local_commit(modified.path()),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into(),
            }
        );

        // 2. Untracked file.
        let untracked = tempfile::tempdir().unwrap();
        init_clean_repo(untracked.path(), &[("app.ts", b"export const a = 1;\n")]);
        fs::write(untracked.path().join("extra.ts"), b"export const b = 2;\n").unwrap();
        assert_eq!(
            local_commit(untracked.path()),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into(),
            }
        );

        // 3. Deleted tracked file (plus an untracked file to verify count > 1).
        let deleted = tempfile::tempdir().unwrap();
        init_clean_repo(
            deleted.path(),
            &[
                ("app.ts", b"export const a = 1;\n"),
                (" gone.ts".trim(), b"export const b = 2;\n"),
            ],
        );
        fs::remove_file(deleted.path().join("gone.ts")).unwrap();
        assert_eq!(
            local_commit(deleted.path()),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into(),
            }
        );
        fs::write(deleted.path().join("new.ts"), b"export const c = 3;\n").unwrap();
        assert_eq!(
            local_commit(deleted.path()),
            LocalCommit::Workdir {
                reason: "2 modified or untracked files".into(),
            }
        );
    }

    // AC-0310: a plain non-git directory stays `workdir` with reason
    // "not a git repository root". (T-0310)
    #[test]
    fn non_repo_directory_reports_not_a_git_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("app.ts"), b"export const a = 1;\n").unwrap();
        assert_eq!(
            local_commit(dir.path()),
            LocalCommit::Workdir {
                reason: "not a git repository root".into(),
            }
        );
    }

    // AC-0310: opening a subdirectory of a git repo does not search parent
    // directories and reports "not a git repository root". (T-0310)
    #[test]
    fn parent_repo_subdirectory_does_not_pick_up_parent_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_clean_repo(
            dir.path(),
            &[("packages/sub/app.ts", b"export const a = 1;\n")],
        );
        let subdir = dir.path().join("packages/sub");
        assert_eq!(
            local_commit(&subdir),
            LocalCommit::Workdir {
                reason: "not a git repository root".into(),
            }
        );
    }

    // AC-0310: an empty `git init` with an unborn branch reports "no HEAD commit". (T-0310)
    #[test]
    fn unborn_head_reports_no_head_commit() {
        let dir = tempfile::tempdir().unwrap();
        git2::Repository::init(dir.path()).unwrap();
        assert_eq!(
            local_commit(dir.path()),
            LocalCommit::Workdir {
                reason: "no HEAD commit".into(),
            }
        );
    }

    // AC-0310: an untracked input hidden from `git status` by
    // `.git/info/exclude` or `.gitignore` (for `.env`) still makes the source
    // `workdir`; a `.gitignore`d build output does not. (T-0310)
    #[test]
    fn untracked_inputs_status_does_not_report_keep_workdir() {
        let dir = tempfile::tempdir().unwrap();
        init_clean_repo(
            dir.path(),
            &[
                (".gitignore", b"target/\n.env\n"),
                ("src/app.ts", b"export const ok = 1;\n"),
            ],
        );
        fs::create_dir_all(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target/out.js"), b"built\n").unwrap();
        assert!(matches!(
            local_commit(dir.path()),
            LocalCommit::Clean { .. }
        ));

        fs::write(dir.path().join(".git/info/exclude"), b"hidden.ts\n").unwrap();
        fs::write(dir.path().join("hidden.ts"), b"export const h = 1;\n").unwrap();
        assert_eq!(
            local_commit(dir.path()),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into()
            }
        );

        fs::remove_file(dir.path().join("hidden.ts")).unwrap();
        fs::write(
            dir.path().join(".env"),
            b"API_URL=https://example.invalid\n",
        )
        .unwrap();
        assert_eq!(
            local_commit(dir.path()),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into()
            }
        );
    }

    // AC-1018: an ignored `.cartograph/env.toml` changes which env values
    // recovery reads, so it keeps `workdir`. (T-1018)
    #[test]
    fn ignored_env_read_allowlist_keeps_workdir() {
        let dir = tempfile::tempdir().unwrap();
        init_clean_repo(
            dir.path(),
            &[
                (".gitignore", b".cartograph/\n"),
                ("src/app.ts", b"export const ok = 1;\n"),
            ],
        );
        fs::create_dir_all(dir.path().join(".cartograph")).unwrap();
        fs::write(
            dir.path().join(ingest::manifest::REPO_ENV_REL_PATH),
            b"read = [\"QUEUE_URL\"]\n",
        )
        .unwrap();
        assert_eq!(
            local_commit(dir.path()),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into()
            }
        );
    }

    // AC-1506: an ignored `.cartograph/types.toml` changes which dependency
    // calls recovery proves, so it keeps `workdir`. (T-1506)
    #[test]
    fn ignored_dependency_types_allowlist_keeps_workdir() {
        let dir = tempfile::tempdir().unwrap();
        init_clean_repo(
            dir.path(),
            &[
                (".gitignore", b".cartograph/\n"),
                ("src/app.ts", b"export const ok = 1;\n"),
            ],
        );
        fs::create_dir_all(dir.path().join(".cartograph")).unwrap();
        fs::write(
            dir.path().join(adapters_lang_ts::DEPENDENCY_TYPES_REL_PATH),
            b"read = [\"stripe\"]\n",
        )
        .unwrap();
        assert_eq!(
            local_commit(dir.path()),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into()
            }
        );
    }

    // AC-0310: an untracked `.gitignore` that ignores itself (and tracked
    // sources) still changes what recovery reads, so it keeps `workdir`.
    // (T-0310)
    #[test]
    fn untracked_self_ignoring_gitignore_keeps_workdir() {
        let dir = tempfile::tempdir().unwrap();
        init_clean_repo(
            dir.path(),
            &[
                ("src/app.ts", b"export const ok = 1;\n"),
                ("src/b.ts", b"export const b = 1;\n"),
            ],
        );
        fs::write(dir.path().join("src/.gitignore"), b".gitignore\nb.ts\n").unwrap();
        assert_eq!(
            local_commit(dir.path()),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into()
            }
        );
    }

    // AC-0310: assume-unchanged and skip-worktree entries hide modified bytes
    // from status, so either flag keeps `workdir`. (T-0310)
    #[test]
    fn assume_unchanged_or_skip_worktree_entries_keep_workdir() {
        for extended in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            init_clean_repo(dir.path(), &[("src/app.ts", b"export const ok = 1;\n")]);
            let repo = git2::Repository::open(dir.path()).unwrap();
            let mut index = repo.index().unwrap();
            let mut entry = index.get_path(Path::new("src/app.ts"), 0).unwrap();
            if extended {
                entry.flags |= git2::IndexEntryFlag::EXTENDED.bits();
                entry.flags_extended |= git2::IndexEntryExtendedFlag::SKIP_WORKTREE.bits();
            } else {
                entry.flags |= git2::IndexEntryFlag::VALID.bits();
            }
            index.add(&entry).unwrap();
            index.write().unwrap();
            fs::write(dir.path().join("src/app.ts"), b"export const ok = 2;\n").unwrap();
            assert_eq!(
                local_commit(dir.path()),
                LocalCommit::Workdir {
                    reason: "1 modified or untracked files".into()
                },
                "skip-worktree={extended}"
            );
        }
    }

    // AC-0310: a submodule's consumed files keep the checkout at workdir when the
    // submodule is modified, even if the repo config hides submodule changes. (T-0310)
    #[test]
    fn modified_submodule_hidden_by_config_keeps_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sub_sha = init_clean_repo(&root.join("sub"), &[("app.ts", b"export const ok = 1;\n")]);
        let repo = git2::Repository::init(root).unwrap();
        fs::write(root.join("main.ts"), b"export const main = 1;\n").unwrap();
        fs::write(
            root.join(".gitmodules"),
            b"[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n",
        )
        .unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("main.ts")).unwrap();
        index.add_path(Path::new(".gitmodules")).unwrap();
        index
            .add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: GITLINK_MODE,
                uid: 0,
                gid: 0,
                file_size: 0,
                id: git2::Oid::from_str(&sub_sha).unwrap(),
                flags: 0,
                flags_extended: 0,
                path: b"sub".to_vec(),
            })
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature =
            git2::Signature::new("fixture", "fixture@example.invalid", &git2::Time::new(1, 0))
                .unwrap();
        let sha = repo
            .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
            .unwrap()
            .to_string();
        assert_eq!(local_commit(root), LocalCommit::Clean { sha });

        repo.config()
            .unwrap()
            .set_str("diff.ignoreSubmodules", "all")
            .unwrap();
        fs::write(root.join("sub/app.ts"), b"export const ok = 2;\n").unwrap();
        assert_eq!(
            local_commit(root),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into()
            }
        );
    }

    // AC-0310: hostile `.git/config` (`core.fsmonitor`, `filter.*.clean`),
    // `.gitattributes`, and executable `.git/hooks/*` are never executed when
    // inspecting a local repository. (T-0310)
    #[test]
    fn hostile_git_config_and_hooks_are_never_executed() {
        let dir = tempfile::tempdir().unwrap();
        let repo_root = dir.path().join("repo");
        let marker: PathBuf = dir.path().join("PWNED");
        let sha = init_clean_repo(
            &repo_root,
            &[
                (".gitattributes", b"* filter=x\n"),
                ("app.ts", b"export const ok = 1;\n"),
            ],
        );

        // Forward slashes: git config reads a backslash as an escape, and a
        // Windows path would make the whole config unreadable.
        let cmd = format!(
            "touch '{}'",
            marker.display().to_string().replace('\\', "/")
        );
        let config_path = repo_root.join(".git/config");
        let mut config = fs::read_to_string(&config_path).unwrap();
        config.push_str(&format!(
            "\n[core]\n\tfsmonitor = {cmd}\n[filter \"x\"]\n\tclean = {cmd}\n\tsmudge = {cmd}\n"
        ));
        fs::write(&config_path, config).unwrap();

        let hooks_dir = repo_root.join(".git/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
        for hook in ["post-checkout", "pre-status", "post-index-change"] {
            let hook_path = hooks_dir.join(hook);
            fs::write(&hook_path, &script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        // First check on the clean tree with hostile config/hooks installed.
        assert_eq!(local_commit(&repo_root), LocalCommit::Clean { sha });
        assert!(
            !marker.exists(),
            "hostile fsmonitor/filter/hook must never execute on clean repo"
        );

        // Modify a tracked file so status diffing runs against the working tree.
        fs::write(repo_root.join("app.ts"), b"export const ok = 2;\n").unwrap();
        assert_eq!(
            local_commit(&repo_root),
            LocalCommit::Workdir {
                reason: "1 modified or untracked files".into(),
            }
        );
        assert!(
            !marker.exists(),
            "hostile fsmonitor/filter/hook must never execute on dirty repo"
        );
    }

    // AC-0310: local recovery of a clean git checkout records HEAD's full SHA
    // in coverage (with no commit_note) and on the Repo node, a dirty checkout
    // records `workdir` with `commit_note` explaining why, and a checkout that
    // changes mid-recovery fails before publishing. (T-0310)
    #[test]
    fn local_commit_recovery_records_clean_sha_dirty_coverage_note_and_rejects_mid_run_edits() {
        use core_graph::GraphStore;
        use tauri::{Listener, Manager};

        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path().join("app-data");
        fs::create_dir_all(&app_data).unwrap();
        let project = dir.path().join("project");
        let sha = init_clean_repo(&project, &[("app.ts", b"export const ok = 1;\n")]);

        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app"),
        );
        app.manage(crate::PreflightRuns::default());
        app.manage(crate::registered_source_tests::app_state(&app_data));
        let handle = app.handle().clone();
        let state = app.state::<crate::AppState>();
        let source = crate::register_local_source(&state, &project).unwrap();

        // 1. Clean git checkout records the exact 40-hex HEAD SHA.
        let summary = {
            let operation = crate::source_operation(&state, vec![(source.clone(), false)]).unwrap();
            let (_, execution) = crate::start_job(&state, &source.ingest_job_kind()).unwrap();
            crate::run_ingest(&source, &operation, &execution, &handle, &state).unwrap()
        };
        assert_eq!(summary.coverage.len(), 1);
        assert_eq!(summary.coverage[0].commit, sha);
        assert_eq!(summary.coverage[0].commit_note, None);
        let stored = state.graph.lock().unwrap().coverage_records().unwrap();
        let inventory: ingest::coverage::RepoCoverage =
            serde_json::from_str(&stored[0].record.inventory_json).unwrap();
        assert_eq!(inventory.commit, sha);
        assert_eq!(inventory.commit_note, None);
        let repo_node = state
            .graph
            .lock()
            .unwrap()
            .get_node(&format!("repo:{}", source.repo_key))
            .unwrap()
            .expect("Repo node");
        assert_eq!(repo_node.props["commit"].as_str(), Some(sha.as_str()));

        // 2. Dirty checkout records `workdir` with `commit_note` in coverage.
        fs::write(
            project.join("untracked.ts"),
            b"export const dirty = true;\n",
        )
        .unwrap();
        let dirty_summary = {
            let operation = crate::source_operation(&state, vec![(source.clone(), false)]).unwrap();
            let (_, execution) = crate::start_job(&state, &source.ingest_job_kind()).unwrap();
            crate::run_ingest(&source, &operation, &execution, &handle, &state).unwrap()
        };
        assert_eq!(dirty_summary.coverage[0].commit, "workdir");
        assert_eq!(
            dirty_summary.coverage[0].commit_note.as_deref(),
            Some("1 modified or untracked files")
        );
        let stored_dirty = state.graph.lock().unwrap().coverage_records().unwrap();
        let dirty_inventory: ingest::coverage::RepoCoverage =
            serde_json::from_str(&stored_dirty[0].record.inventory_json).unwrap();
        assert_eq!(dirty_inventory.commit, "workdir");
        assert_eq!(
            dirty_inventory.commit_note.as_deref(),
            Some("1 modified or untracked files")
        );

        // 3. Clean checkout that becomes dirty during recovery fails before publish.
        fs::remove_file(project.join("untracked.ts")).unwrap();
        assert_eq!(
            local_commit(&project),
            LocalCommit::Clean { sha: sha.clone() }
        );
        let project_for_hook = project.clone();
        let listener = handle.listen("job://detail", move |event| {
            let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            if payload["detail"] == crate::RECONCILE_PREFLIGHT_DETAIL {
                fs::write(
                    project_for_hook.join("mid-run.ts"),
                    b"export const changed = 1;\n",
                )
                .unwrap();
            }
        });
        let (job_id, err) = {
            let operation = crate::source_operation(&state, vec![(source.clone(), false)]).unwrap();
            let (job, execution) = crate::start_job(&state, &source.ingest_job_kind()).unwrap();
            let err = crate::run_ingest(&source, &operation, &execution, &handle, &state)
                .err()
                .expect("mid-recovery source change must fail");
            (job.id, err)
        };
        handle.unlisten(listener);
        assert_eq!(err, LocalCommit::CHANGED_DURING_RECOVERY);
        let failed_job = state.jobs.lock().unwrap().get(job_id).unwrap();
        assert_eq!(failed_job.status, "failed");
        assert_eq!(
            failed_job.error.as_deref(),
            Some(LocalCommit::CHANGED_DURING_RECOVERY)
        );
    }
}
