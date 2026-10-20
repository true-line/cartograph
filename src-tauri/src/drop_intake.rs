//! Folder drop intake (#587, AC-0237). Dropped paths are classified before
//! anything is ingested, so each one gets an explicit outcome. An accepted
//! path names the route a typed target would take: a folder ingests as a
//! local tree, and a folder holding `cartograph.system.toml` (or the file
//! itself) ingests as a system. Nothing here reads file contents.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Most paths one drop can carry; the rest are rejected, never dropped.
pub const MAX_DROP_PATHS: usize = 64;

/// The ingest route of an accepted path, matching the Connect sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DropSource {
    /// A folder ingested as a local tree (`ingest_path`).
    Local,
    /// A system manifest, or a folder holding one (`add_system`).
    Manifest,
}

/// One dropped path and its intake decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DropCandidate {
    /// The path as dropped.
    pub path: String,
    /// Set when accepted: the route the path ingests through.
    pub source: Option<DropSource>,
    /// Set when rejected: why it will not be ingested.
    pub reason: Option<String>,
}

impl DropCandidate {
    fn accepted(path: &str, source: DropSource) -> Self {
        Self {
            path: path.to_string(),
            source: Some(source),
            reason: None,
        }
    }

    fn rejected(path: &str, reason: String) -> Self {
        Self {
            path: path.to_string(),
            source: None,
            reason: Some(reason),
        }
    }
}

/// Classify every dropped path, in drop order. A path is rejected when it is
/// missing, a file other than a system manifest, a repeat of an earlier
/// path, a plain folder inside another accepted folder (that folder already
/// ingests it), or beyond `MAX_DROP_PATHS`.
pub fn classify(paths: &[String]) -> Vec<DropCandidate> {
    let manifest_name = ingest::manifest::MANIFEST_NAME;
    let mut decided: Vec<(DropCandidate, Option<PathBuf>)> = Vec::new();
    for (index, raw) in paths.iter().enumerate() {
        let path = raw.trim();
        if index >= MAX_DROP_PATHS {
            decided.push((
                DropCandidate::rejected(
                    raw,
                    format!("more than {MAX_DROP_PATHS} paths in one drop"),
                ),
                None,
            ));
            continue;
        }
        if path.is_empty() {
            decided.push((DropCandidate::rejected(raw, "empty path".into()), None));
            continue;
        }
        let Ok(canonical) = Path::new(path).canonicalize() else {
            decided.push((DropCandidate::rejected(raw, "not found".into()), None));
            continue;
        };
        if let Some((first, _)) = decided
            .iter()
            .find(|(_, seen)| seen.as_deref() == Some(canonical.as_path()))
        {
            decided.push((
                DropCandidate::rejected(raw, format!("already dropped as {}", first.path)),
                None,
            ));
            continue;
        }
        let candidate = if canonical.is_dir() {
            if canonical.join(manifest_name).is_file() {
                DropCandidate::accepted(raw, DropSource::Manifest)
            } else {
                DropCandidate::accepted(raw, DropSource::Local)
            }
        } else if canonical.file_name().and_then(|name| name.to_str()) == Some(manifest_name) {
            DropCandidate::accepted(raw, DropSource::Manifest)
        } else {
            DropCandidate::rejected(
                raw,
                format!("not a folder: drop a folder or a {manifest_name}"),
            )
        };
        let seen = candidate.source.is_some().then_some(canonical);
        decided.push((candidate, seen));
    }
    // A folder inside another accepted folder is already part of that
    // ingest, whichever was dropped first.
    let folders: Vec<(String, PathBuf)> = decided
        .iter()
        .filter(|(candidate, _)| candidate.source == Some(DropSource::Local))
        .filter_map(|(candidate, seen)| Some((candidate.path.clone(), seen.clone()?)))
        .collect();
    decided
        .into_iter()
        .map(|(candidate, seen)| {
            // A system manifest names repos that may live elsewhere, so it
            // is never absorbed by a folder.
            let (Some(seen), Some(DropSource::Local)) = (seen, candidate.source) else {
                return candidate;
            };
            match folders
                .iter()
                .find(|(_, folder)| seen != *folder && seen.starts_with(folder))
            {
                Some((outer, _)) => DropCandidate::rejected(
                    &candidate.path,
                    format!("inside dropped folder {outer}, which already ingests it"),
                ),
                None => candidate,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    // AC-0237: every dropped path gets an explicit outcome and the route a
    // typed target would take. (T-0237)
    #[test]
    fn dropped_paths_are_accepted_by_route_or_rejected_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let system = dir.path().join("system");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&system).unwrap();
        let manifest = system.join(ingest::manifest::MANIFEST_NAME);
        std::fs::write(&manifest, "[[repos]]\nurl = \"a\"\n").unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "x").unwrap();
        let paths = vec![
            s(&repo),
            s(&system),
            s(&manifest),
            s(&file),
            s(&dir.path().join("missing")),
            "  ".into(),
            format!("{}/", s(&repo)),
        ];
        let out = classify(&paths);
        let summary: Vec<(Option<DropSource>, Option<&str>)> = out
            .iter()
            .map(|c| (c.source, c.reason.as_deref()))
            .collect();
        let repeat = format!("already dropped as {}", s(&repo));
        assert_eq!(
            summary,
            [
                (Some(DropSource::Local), None),
                (Some(DropSource::Manifest), None),
                (Some(DropSource::Manifest), None),
                (
                    None,
                    Some("not a folder: drop a folder or a cartograph.system.toml")
                ),
                (None, Some("not found")),
                (None, Some("empty path")),
                (None, Some(repeat.as_str())),
            ]
        );
        // Each outcome keeps the path exactly as dropped.
        assert!(out.iter().zip(&paths).all(|(c, p)| &c.path == p));
    }

    // AC-0237: a folder inside another dropped folder is not ingested twice,
    // whichever order they arrive in; a drop past the bound is rejected, not
    // silently cut. (T-0237)
    #[test]
    fn nested_and_excess_paths_are_rejected_not_double_ingested() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("outer");
        let inner = outer.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        let nested_system = outer.join("system");
        std::fs::create_dir_all(&nested_system).unwrap();
        std::fs::write(
            nested_system.join(ingest::manifest::MANIFEST_NAME),
            "[[repos]]\nurl = \"a\"\n",
        )
        .unwrap();
        let out = classify(&[s(&inner), s(&outer), s(&nested_system)]);
        assert_eq!(out[1].source, Some(DropSource::Local));
        assert_eq!(out[2].source, Some(DropSource::Manifest));
        assert_eq!(out[0].source, None);
        assert_eq!(
            out[0].reason.as_deref(),
            Some(
                format!(
                    "inside dropped folder {}, which already ingests it",
                    s(&outer)
                )
                .as_str()
            )
        );

        let many: Vec<String> = (0..=MAX_DROP_PATHS)
            .map(|i| {
                let folder = dir.path().join(format!("r{i}"));
                std::fs::create_dir_all(&folder).unwrap();
                s(&folder)
            })
            .collect();
        let out = classify(&many);
        assert_eq!(out.len(), MAX_DROP_PATHS + 1);
        assert!(out[..MAX_DROP_PATHS].iter().all(|c| c.source.is_some()));
        assert_eq!(
            out[MAX_DROP_PATHS].reason.as_deref(),
            Some("more than 64 paths in one drop")
        );
    }
}
