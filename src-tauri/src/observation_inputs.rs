//! Observation inputs a system manifest declares for one repo: Terraform
//! state, Pulumi exports and OTLP trace files (ADR-0080, AC-1041, #385).
//!
//! They are input artifacts, not repository source, so they never borrow the
//! repo's commit or capture. Each declared path that lies inside the manifest
//! folder joins one working-tree capture of that folder under the input
//! identity `input/<source id>`; the T1 parsers read the captured bytes, and
//! their observed evidence cites that identity and the declared relative path.
//! A declared path that escapes the folder, or that cannot be captured within
//! the capture limits, is still read from disk, but its evidence keeps the
//! uncaptured form (no repo, no commit) and coverage names it as unverified.

use std::path::{Path, PathBuf};

/// The evidence `repo` prefix of a captured observation input.
pub const INPUT_REPO_PREFIX: &str = "input/";

/// The capture source identity of `source_id`'s observation inputs.
pub fn input_source_id(source_id: &str) -> String {
    format!("{INPUT_REPO_PREFIX}{source_id}")
}

/// The revision captured observation evidence cites: a working-tree capture.
const INPUT_COMMIT: &str = "workdir";

/// One declared observation input file.
#[derive(Debug)]
pub struct ObservationInput {
    /// The path as the manifest declares it.
    declared: String,
    /// Where it is read from when it is not captured.
    read_path: PathBuf,
    /// Its path inside the input capture, when it is captured.
    captured: Option<String>,
}

/// Every observation input of one repo, with their capture when there is one.
#[derive(Debug, Default)]
pub struct ObservationInputs {
    pub state_json: Option<ObservationInput>,
    pub pulumi_json: Option<ObservationInput>,
    pub otel_jsonl: Vec<ObservationInput>,
    capture: Option<source_capture::Capture>,
    repo: String,
}

/// An input's bytes as the parser reads them, and the path its evidence cites.
pub struct ReadInput {
    pub raw: String,
    pub cited_path: String,
}

impl ObservationInputs {
    /// Inputs read from disk with uncaptured evidence, as before ADR-0080.
    #[cfg(any(test, feature = "test-support"))]
    pub fn uncaptured(
        state_json: Option<&Path>,
        pulumi_json: Option<&Path>,
        otel_jsonl: &[PathBuf],
    ) -> Self {
        let input = |path: &Path| ObservationInput {
            declared: path.to_string_lossy().into_owned(),
            read_path: path.to_path_buf(),
            captured: None,
        };
        Self {
            state_json: state_json.map(input),
            pulumi_json: pulumi_json.map(input),
            otel_jsonl: otel_jsonl.iter().map(|path| input(path)).collect(),
            capture: None,
            repo: String::new(),
        }
    }

    /// The inputs `entry` declares, relative to the manifest folder `base`.
    /// Those inside `base` are captured together under `input/<source_id>`;
    /// when they cannot all be captured, the subset that fits the capture
    /// limits is kept, and the rest stay uncaptured.
    pub fn declared(
        base: &Path,
        source_id: &str,
        state_json: Option<&str>,
        pulumi_json: Option<&str>,
        otel_jsonl: &[String],
    ) -> Self {
        let declared: Vec<&str> = state_json
            .into_iter()
            .chain(pulumi_json)
            .chain(otel_jsonl.iter().map(String::as_str))
            .collect();
        let repo = input_source_id(source_id);
        let capture = source_capture::SourceId::new(repo.clone())
            .ok()
            .and_then(|id| {
                capture_inputs(
                    base,
                    &id,
                    &declared,
                    source_capture::CaptureLimits::default(),
                )
            });
        let input = |path: &str| {
            let relative = capturable_path(path);
            ObservationInput {
                declared: path.to_owned(),
                read_path: base.join(path),
                captured: relative.filter(|relative| {
                    capture
                        .as_ref()
                        .is_some_and(|capture| capture.file(relative).is_ok())
                }),
            }
        };
        Self {
            state_json: state_json.map(input),
            pulumi_json: pulumi_json.map(input),
            otel_jsonl: otel_jsonl.iter().map(|path| input(path)).collect(),
            capture,
            repo,
        }
    }

    /// Read `input` for the `field` parser: its captured bytes, which must be
    /// UTF-8, or the file on disk when it is not captured.
    pub fn read(&self, input: &ObservationInput, field: &str) -> Result<ReadInput, String> {
        let display = input.read_path.display();
        match (&input.captured, &self.capture) {
            (Some(path), Some(capture)) => {
                let file = capture
                    .file(path)
                    .map_err(|_| format!("{field} {display}: captured input is unavailable"))?;
                let raw = String::from_utf8(file.bytes().to_vec())
                    .map_err(|_| format!("{field} {display}: not valid UTF-8"))?;
                Ok(ReadInput {
                    raw,
                    cited_path: path.clone(),
                })
            }
            _ => {
                let raw = std::fs::read_to_string(&input.read_path)
                    .map_err(|error| format!("{field} {display}: {error}"))?;
                Ok(ReadInput {
                    raw,
                    cited_path: input.read_path.to_string_lossy().into_owned(),
                })
            }
        }
    }

    /// Bind the uncaptured-form evidence the T1 passes wrote for captured
    /// inputs to the input identity: each `prov` or `observed_prov` entry
    /// with no repo and no commit whose path is a captured input's.
    pub fn cite<'a>(&self, props: impl IntoIterator<Item = &'a mut serde_json::Value>) {
        let Some(capture) = &self.capture else {
            return;
        };
        for value in props {
            for key in ["prov", "observed_prov"] {
                let Some(evidence) = value
                    .get_mut(key)
                    .and_then(|prov| prov.get_mut("evidence"))
                    .and_then(serde_json::Value::as_array_mut)
                else {
                    continue;
                };
                for entry in evidence {
                    let uncaptured = entry["repo"] == "" && entry["commit_sha"] == "";
                    let captured = entry["path"]
                        .as_str()
                        .is_some_and(|path| capture.file(path).is_ok());
                    if uncaptured && captured {
                        entry["repo"] = self.repo.clone().into();
                        entry["commit_sha"] = INPUT_COMMIT.into();
                    }
                }
            }
        }
    }

    /// The input capture, retained beside the owning source's captures
    /// (ADR-0080 slice 2a).
    pub fn capture(&self) -> Option<&source_capture::Capture> {
        self.capture.as_ref()
    }

    /// The declared paths read without a capture, as the manifest writes
    /// them, for coverage (AC-1041).
    pub fn uncaptured_paths(&self) -> Vec<String> {
        self.state_json
            .iter()
            .chain(&self.pulumi_json)
            .chain(&self.otel_jsonl)
            .filter(|input| input.captured.is_none())
            .map(|input| input.declared.clone())
            .collect()
    }
}

/// A declared path in the form a capture of the manifest folder admits:
/// relative, `/`-separated, without `.` or `..` components. On Windows a
/// native backslash separator is read as `/`; a drive or root prefix never is.
fn capturable_path(path: &str) -> Option<String> {
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let relative = path.strip_prefix("./").unwrap_or(&path);
    let valid = !relative.is_empty()
        && !relative.starts_with('/')
        && !relative.contains('\\')
        && !relative.contains(':')
        && relative
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    valid.then(|| relative.to_owned())
}

/// Capture the capturable `declared` paths together. When the whole set is
/// refused (one file past the limits, or the set past a joint limit), keep a
/// bounded subset chosen from file metadata alone: regular files within the
/// per-file bound, admitted in order while the joint count and byte bounds
/// hold, then captured once. Each file is read at most twice, never once per
/// admitted prefix.
fn capture_inputs(
    base: &Path,
    id: &source_capture::SourceId,
    declared: &[&str],
    limits: source_capture::CaptureLimits,
) -> Option<source_capture::Capture> {
    let mut paths: Vec<String> = declared
        .iter()
        .filter_map(|path| capturable_path(path))
        .collect();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return None;
    }
    let capture = |paths: &[String]| source_capture::capture_working_tree(base, id, paths, limits);
    capture(&paths).ok().or_else(|| {
        let mut total = 0u64;
        let kept: Vec<String> = paths
            .into_iter()
            .filter(|path| {
                let Ok(meta) = std::fs::symlink_metadata(base.join(path)) else {
                    return false;
                };
                let fits = meta.is_file()
                    && path.len() <= limits.max_path_bytes
                    && meta.len() <= limits.max_file_bytes
                    && total.saturating_add(meta.len()) <= limits.max_total_bytes;
                if fits {
                    total += meta.len();
                }
                fits
            })
            .take(limits.max_files)
            .collect();
        (!kept.is_empty()).then(|| capture(&kept).ok()).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_relative_paths_inside_the_manifest_folder_are_capturable() {
        assert_eq!(capturable_path("state.json").as_deref(), Some("state.json"));
        assert_eq!(
            capturable_path("./infra/state.json").as_deref(),
            Some("infra/state.json")
        );
        for path in [
            "../state.json",
            "/abs/state.json",
            "a/../b.json",
            "a//b",
            "",
            "C:/state.json",
            "C:state.json",
        ] {
            assert_eq!(capturable_path(path), None, "{path}");
        }
        // Codex on #2222: a native Windows separator is a separator there,
        // never a way to escape the folder.
        if cfg!(windows) {
            assert_eq!(
                capturable_path("traces\\run.jsonl").as_deref(),
                Some("traces/run.jsonl")
            );
            assert_eq!(capturable_path("..\\state.json"), None);
            assert_eq!(capturable_path("\\state.json"), None);
        } else {
            assert_eq!(capturable_path("a\\b"), None);
        }
    }

    #[test]
    fn inputs_past_a_joint_capture_limit_keep_the_subset_that_fits() {
        // Codex on #2220: every file fits alone, but not all together.
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.jsonl", "b.jsonl", "c.jsonl"] {
            std::fs::write(dir.path().join(name), "0123456789").unwrap();
        }
        let id = source_capture::SourceId::new("input/s".to_owned()).unwrap();
        let limits = source_capture::CaptureLimits {
            max_total_bytes: 25,
            ..source_capture::CaptureLimits::default()
        };
        let capture = capture_inputs(
            dir.path(),
            &id,
            &["a.jsonl", "b.jsonl", "c.jsonl", "../out.jsonl"],
            limits,
        )
        .unwrap();
        assert!(capture.file("a.jsonl").is_ok());
        assert!(capture.file("b.jsonl").is_ok());
        assert!(capture.file("c.jsonl").is_err());
        // A missing or oversized file is left out without replaying the
        // capture for every admitted prefix (Codex on #2222).
        std::fs::write(dir.path().join("big.jsonl"), "x".repeat(40)).unwrap();
        let limits = source_capture::CaptureLimits {
            max_file_bytes: 20,
            ..source_capture::CaptureLimits::default()
        };
        let capture = capture_inputs(
            dir.path(),
            &id,
            &["a.jsonl", "big.jsonl", "missing.jsonl", "c.jsonl"],
            limits,
        )
        .unwrap();
        assert!(capture.file("a.jsonl").is_ok());
        assert!(capture.file("c.jsonl").is_ok());
        assert!(capture.file("big.jsonl").is_err());
    }
}
