//! Worktree confinement for agent file access and permission requests.

use serde_json::Value;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Why a path was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathRefusal {
    /// ACP paths must be absolute.
    #[error("path is not absolute")]
    NotAbsolute,
    /// The path names a parent or root component after its last existing ancestor.
    #[error("path climbs out of its existing ancestor")]
    Climbs,
    /// The resolved path lies outside the worktree.
    #[error("path is outside the worktree")]
    Outside,
    /// The resolved path is the worktree's git metadata.
    #[error("path is git metadata")]
    GitMetadata,
    /// The path could not be resolved.
    #[error("path could not be resolved: {0}")]
    Unresolved(String),
}

/// Confines paths to one worktree root.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    root: PathBuf,
}

impl PathPolicy {
    /// Policy for an existing directory, resolved through symlinks.
    pub fn new(root: &Path) -> io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    /// The resolved worktree root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves `path` through symlinks of its longest existing ancestor and
    /// returns it if it lies inside the root and outside `.git`.
    pub fn resolve(&self, path: &Path) -> Result<PathBuf, PathRefusal> {
        if !path.is_absolute() {
            return Err(PathRefusal::NotAbsolute);
        }
        let mut existing = path;
        let mut tail = Vec::new();
        while !existing.exists() {
            let Some(name) = existing.file_name() else {
                return Err(PathRefusal::Climbs);
            };
            tail.push(name.to_owned());
            existing = existing.parent().ok_or(PathRefusal::Climbs)?;
        }
        let mut resolved = existing
            .canonicalize()
            .map_err(|error| PathRefusal::Unresolved(error.to_string()))?;
        for name in tail.iter().rev() {
            let part = Path::new(name);
            if !matches!(part.components().next(), Some(Component::Normal(_))) {
                return Err(PathRefusal::Climbs);
            }
            resolved.push(part);
        }
        let Ok(relative) = resolved.strip_prefix(&self.root) else {
            return Err(PathRefusal::Outside);
        };
        if relative
            .components()
            .next()
            .is_some_and(|first| first.as_os_str() == ".git")
        {
            return Err(PathRefusal::GitMetadata);
        }
        Ok(resolved)
    }
}

/// The host's answer to `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Select this `allow_once` option.
    Allow(String),
    /// Select this reject option, for the reason given.
    Reject(String, String),
    /// Answer `cancelled`, for the reason given.
    Cancel(String),
}

/// Tool kinds the first slice allows, when every location is inside the worktree.
const ALLOWED_KINDS: &[&str] = &["read", "edit", "search"];

/// Decides a permission request (ADR-0092 decision 2): only a `read`, `edit`
/// or `search` call whose locations are all inside the worktree is allowed,
/// and only through an `allow_once` option.
pub fn decide_permission(params: &Value, policy: &PathPolicy) -> PermissionDecision {
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let option = |kinds: &[&str]| {
        kinds.iter().find_map(|kind| {
            options.iter().find_map(|option| {
                (option.get("kind").and_then(Value::as_str) == Some(kind))
                    .then(|| option.get("optionId").and_then(Value::as_str))
                    .flatten()
                    .map(str::to_owned)
            })
        })
    };
    let reject = |reason: String| match option(&["reject_once", "reject_always"]) {
        Some(id) => PermissionDecision::Reject(id, reason),
        None => PermissionDecision::Cancel(reason),
    };
    let call = params.get("toolCall").unwrap_or(&Value::Null);
    let kind = call.get("kind").and_then(Value::as_str).unwrap_or("other");
    if !ALLOWED_KINDS.contains(&kind) {
        return reject(format!("tool kind {kind} is not allowed"));
    }
    let locations = call
        .get("locations")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if locations.is_empty() {
        return reject("tool call names no location".into());
    }
    for location in locations {
        let Some(path) = location.get("path").and_then(Value::as_str) else {
            return reject("location has no path".into());
        };
        if let Err(refusal) = policy.resolve(Path::new(path)) {
            return reject(format!("{path}: {refusal}"));
        }
    }
    match option(&["allow_once"]) {
        Some(id) => PermissionDecision::Allow(id),
        None => PermissionDecision::Cancel("no allow_once option offered".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn options() -> Value {
        json!([
            {"optionId": "always", "name": "Always", "kind": "allow_always"},
            {"optionId": "once", "name": "Once", "kind": "allow_once"},
            {"optionId": "no", "name": "No", "kind": "reject_once"}
        ])
    }

    #[test]
    fn paths_resolve_only_inside_the_worktree_and_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("wt");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let policy = PathPolicy::new(&root).unwrap();
        // Agents send plain absolute paths; the policy answers resolved ones.
        assert_eq!(
            policy.resolve(&root.join("src/new/lib.rs")),
            Ok(policy.root().join("src/new/lib.rs"))
        );
        assert_eq!(
            policy.resolve(Path::new("src/lib.rs")),
            Err(PathRefusal::NotAbsolute)
        );
        assert_eq!(
            policy.resolve(&root.join("../escape.txt")),
            Err(PathRefusal::Outside)
        );
        assert_eq!(
            policy.resolve(&root.join("missing/../../escape.txt")),
            Err(PathRefusal::Climbs)
        );
        assert_eq!(
            policy.resolve(&root.join(".git/hooks/pre-commit")),
            Err(PathRefusal::GitMetadata)
        );
        assert_eq!(
            policy.resolve(&root.join(".git")),
            Err(PathRefusal::GitMetadata)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_worktree_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("wt");
        std::fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("out")).unwrap();
        let policy = PathPolicy::new(&root).unwrap();
        assert_eq!(
            policy.resolve(&root.join("out/secret.txt")),
            Err(PathRefusal::Outside)
        );
    }

    #[test]
    fn only_contained_read_edit_and_search_calls_are_allowed_once() {
        let dir = tempfile::tempdir().unwrap();
        let policy = PathPolicy::new(dir.path()).unwrap();
        let inside = dir.path().join("src/lib.rs");
        let inside = inside.to_str().unwrap();
        let request = |kind: &str, locations: Value| {
            json!({"sessionId": "s", "options": options(),
                   "toolCall": {"toolCallId": "t", "kind": kind, "locations": locations}})
        };
        assert_eq!(
            decide_permission(&request("edit", json!([{"path": inside}])), &policy),
            PermissionDecision::Allow("once".into())
        );
        for kind in [
            "execute",
            "fetch",
            "delete",
            "move",
            "think",
            "other",
            "switch_mode",
        ] {
            assert!(matches!(
                decide_permission(&request(kind, json!([{"path": inside}])), &policy),
                PermissionDecision::Reject(id, _) if id == "no"
            ));
        }
        assert!(matches!(
            decide_permission(&request("read", json!([])), &policy),
            PermissionDecision::Reject(..)
        ));
        assert!(matches!(
            decide_permission(&request("edit", json!([{"path": "/etc/passwd"}])), &policy),
            PermissionDecision::Reject(..)
        ));
        let only_always = json!({"options": [{"optionId": "a", "kind": "allow_always"}],
            "toolCall": {"kind": "read", "locations": [{"path": inside}]}});
        assert!(matches!(
            decide_permission(&only_always, &policy),
            PermissionDecision::Cancel(_)
        ));
        let no_reject = json!({"options": [], "toolCall": {"kind": "execute"}});
        assert!(matches!(
            decide_permission(&no_reject, &policy),
            PermissionDecision::Cancel(_)
        ));
    }
}
