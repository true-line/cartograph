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
pub(crate) const INPUT_REPO_PREFIX: &str = "input/";

/// The revision captured observation evidence cites: a working-tree capture.
const INPUT_COMMIT: &str = "workdir";

/// One declared observation input file.
#[derive(Debug)]
pub(crate) struct ObservationInput {
    /// The path as the manifest declares it.
    declared: String,
    /// Where it is read from when it is not captured.
    read_path: PathBuf,
    /// Its path inside the input capture, when it is captured.
    captured: Option<String>,
}

/// Every observation input of one repo, with their capture when there is one.
#[derive(Debug, Default)]
pub(crate) struct ObservationInputs {
    pub(crate) state_json: Option<ObservationInput>,
    pub(crate) pulumi_json: Option<ObservationInput>,
    pub(crate) otel_jsonl: Vec<ObservationInput>,
    capture: Option<source_capture::Capture>,
    repo: String,
}

/// An input's bytes as the parser reads them, and the path its evidence cites.
pub(crate) struct ReadInput {
    pub(crate) raw: String,
    pub(crate) cited_path: String,
}

impl ObservationInputs {
    /// Inputs read from disk with uncaptured evidence, as before ADR-0080.
    #[cfg(test)]
    pub(crate) fn uncaptured(
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
    /// when they cannot all be captured, each that can be captured on its own
    /// is kept, and the rest stay uncaptured.
    pub(crate) fn declared(
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
        let repo = format!("{INPUT_REPO_PREFIX}{source_id}");
        let capture = source_capture::SourceId::new(repo.clone())
            .ok()
            .and_then(|id| capture_inputs(base, &id, &declared));
        let input = |path: &str| {
            let relative = capturable_path(path);
            ObservationInput {
                declared: path.to_owned(),
                read_path: base.join(path),
                captured: relative
                    .filter(|relative| {
                        capture
                            .as_ref()
                            .is_some_and(|capture| capture.file(relative).is_ok())
                    })
                    .map(str::to_owned),
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
    pub(crate) fn read(&self, input: &ObservationInput, field: &str) -> Result<ReadInput, String> {
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
    pub(crate) fn cite<'a>(&self, props: impl IntoIterator<Item = &'a mut serde_json::Value>) {
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

    /// The declared paths read without a capture, as the manifest writes
    /// them, for coverage (AC-1041).
    pub(crate) fn uncaptured_paths(&self) -> Vec<String> {
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
/// relative, `/`-separated, without `.` or `..` components.
fn capturable_path(path: &str) -> Option<&str> {
    let relative = path.strip_prefix("./").unwrap_or(path);
    let valid = !relative.is_empty()
        && !relative.starts_with('/')
        && !relative.contains('\\')
        && relative
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    valid.then_some(relative)
}

fn capture_inputs(
    base: &Path,
    id: &source_capture::SourceId,
    declared: &[&str],
) -> Option<source_capture::Capture> {
    let mut paths: Vec<String> = declared
        .iter()
        .filter_map(|path| capturable_path(path))
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return None;
    }
    let capture = |paths: &[String]| {
        source_capture::capture_working_tree(
            base,
            id,
            paths,
            source_capture::CaptureLimits::default(),
        )
    };
    capture(&paths).ok().or_else(|| {
        let kept: Vec<String> = paths
            .iter()
            .filter(|path| capture(std::slice::from_ref(path)).is_ok())
            .cloned()
            .collect();
        (!kept.is_empty()).then(|| capture(&kept).ok()).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_relative_paths_inside_the_manifest_folder_are_capturable() {
        assert_eq!(capturable_path("state.json"), Some("state.json"));
        assert_eq!(
            capturable_path("./infra/state.json"),
            Some("infra/state.json")
        );
        for path in [
            "../state.json",
            "/abs/state.json",
            "a/../b.json",
            "a//b",
            "",
            "a\\b",
        ] {
            assert_eq!(capturable_path(path), None, "{path}");
        }
    }
}
