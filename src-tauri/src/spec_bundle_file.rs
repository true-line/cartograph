//! Workbench "Export bundle", written by the host straight to the chosen
//! file (#1582, AC-0740).
//!
//! The Workbench used to fetch every full artifact, build the whole bundle
//! as one pretty-printed JSON string in the WebView and send it back over
//! IPC. On a large repository that string is close to a gigabyte, above
//! V8's maximum string length. The WebView now sends only the identities of
//! the preview it shows; this module compiles the full bundle once, checks
//! it is the same selection, and streams the file to disk. The bytes are the
//! ones the WebView produced: `JSON.stringify({ ...bundle, artifacts, files },
//! null, 2)`.

use std::io::{BufWriter, Write};
use std::path::Path;

use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use tauri::Manager;

use crate::SpecArtifactPreview;
use crate::export_save::{sanitize_export_name, write_export_with};

/// One artifact as the Workbench preview showed it (AC-0242 identity).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExpectedArtifact {
    id: String,
    file_name: String,
    schema: String,
    content_id: String,
}

/// The preview the user exported from: its mode, its artifacts' identities
/// and its bundle-level fields, but no artifact content.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExpectedSpecBundle {
    mode: spec::ExportMode,
    artifacts: Vec<ExpectedArtifact>,
    assertion_count: usize,
    gap_count: usize,
    drift_count: usize,
    security_count: usize,
    #[serde(default)]
    graph_basis: Option<spec::GraphBasis>,
    #[serde(default)]
    curated_basis: Option<spec::CuratedBasis>,
}

/// The complete bundle file: the preview's fields with every artifact in
/// full, in the wire shape `read_spec_artifact` returns.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct SpecBundleFile {
    mode: spec::ExportMode,
    artifacts: Vec<SpecArtifactPreview>,
    assertion_count: usize,
    gap_count: usize,
    drift_count: usize,
    security_count: usize,
    graph_basis: Option<spec::GraphBasis>,
    curated_basis: Option<spec::CuratedBasis>,
}

/// The same message the Workbench shows for a stale copy (`specExport.ts`).
fn stale_artifact_message(file_name: &str) -> String {
    format!(
        "{file_name} changed since this preview was compiled. Refresh the Spec Workbench and try again."
    )
}

const STALE_BUNDLE_MESSAGE: &str =
    "The spec changed since this preview was compiled. Refresh the Spec Workbench and try again.";

/// The full bundle, if it is the selection the preview stands for: same
/// artifacts in the same order with the same id, schema and content
/// identity, and the same counts and bases. Anything else is refused, as a
/// stale full fetch was refused in the WebView (AC-0242).
pub(crate) fn matching_bundle_file(
    expected: &ExpectedSpecBundle,
    bundle: spec::SpecBundle,
) -> Result<SpecBundleFile, String> {
    if bundle.mode != expected.mode || bundle.artifacts.len() != expected.artifacts.len() {
        return Err(STALE_BUNDLE_MESSAGE.to_string());
    }
    for (full, preview) in bundle.artifacts.iter().zip(&expected.artifacts) {
        if full.id != preview.id
            || full.schema != preview.schema
            || full.content_id != preview.content_id
        {
            return Err(stale_artifact_message(&preview.file_name));
        }
    }
    if bundle.assertion_count != expected.assertion_count
        || bundle.gap_count != expected.gap_count
        || bundle.drift_count != expected.drift_count
        || bundle.security_count != expected.security_count
        || bundle.graph_basis != expected.graph_basis
        || bundle.curated_basis != expected.curated_basis
    {
        return Err(STALE_BUNDLE_MESSAGE.to_string());
    }
    Ok(SpecBundleFile {
        mode: bundle.mode,
        artifacts: bundle
            .artifacts
            .into_iter()
            .map(crate::full_artifact_preview)
            .collect(),
        assertion_count: bundle.assertion_count,
        gap_count: bundle.gap_count,
        drift_count: bundle.drift_count,
        security_count: bundle.security_count,
        graph_basis: bundle.graph_basis,
        curated_basis: bundle.curated_basis,
    })
}

/// The file's top level, in the key order `{ ...bundle, artifacts, files }`
/// gives: the preview's own keys, then `files`.
#[derive(Serialize)]
struct BundleFileView<'a> {
    mode: spec::ExportMode,
    artifacts: &'a [SpecArtifactPreview],
    assertion_count: usize,
    gap_count: usize,
    drift_count: usize,
    security_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph_basis: Option<&'a spec::GraphBasis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    curated_basis: Option<&'a spec::CuratedBasis>,
    files: FilesByName<'a>,
}

/// `Object.fromEntries(artifacts.map(a => [a.file_name, a.content]))`: a
/// repeated name keeps its first position and its last content, and names
/// that are array indices come first in ascending order, as in a JS object.
struct FilesByName<'a>(&'a [SpecArtifactPreview]);

/// Whether a JS engine orders `key` as an array index (a canonical
/// non-negative integer below 2^32 - 1).
fn is_array_index(key: &str) -> Option<u32> {
    let n: u32 = key.parse().ok()?;
    (n != u32::MAX && n.to_string() == key).then_some(n)
}

impl Serialize for FilesByName<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries: Vec<(&str, &str)> = Vec::with_capacity(self.0.len());
        for artifact in self.0 {
            let name = artifact.file_name.as_str();
            match entries.iter_mut().find(|(key, _)| *key == name) {
                Some(entry) => entry.1 = &artifact.content,
                None => entries.push((name, &artifact.content)),
            }
        }
        let mut indexed: Vec<(u32, &str, &str)> = entries
            .iter()
            .filter_map(|(key, value)| is_array_index(key).map(|n| (n, *key, *value)))
            .collect();
        indexed.sort_by_key(|(n, _, _)| *n);
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (_, key, value) in &indexed {
            map.serialize_entry(key, value)?;
        }
        for (key, value) in entries
            .iter()
            .filter(|(key, _)| is_array_index(key).is_none())
        {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Stream `file` as 2-space pretty JSON into `out`. Nothing is built as one
/// string: the serializer writes through a buffer.
pub(crate) fn write_bundle_json(file: &SpecBundleFile, out: impl Write) -> std::io::Result<()> {
    let view = BundleFileView {
        mode: file.mode,
        artifacts: &file.artifacts,
        assertion_count: file.assertion_count,
        gap_count: file.gap_count,
        drift_count: file.drift_count,
        security_count: file.security_count,
        graph_basis: file.graph_basis.as_ref(),
        curated_basis: file.curated_basis.as_ref(),
        files: FilesByName(&file.artifacts),
    };
    let mut writer = BufWriter::with_capacity(1 << 16, out);
    serde_json::to_writer_pretty(&mut writer, &view)?;
    writer.flush()
}

/// Write the bundle file to `path` with the same atomic replace as every
/// other export (#729).
pub(crate) fn write_bundle_file(path: &Path, file: &SpecBundleFile) -> Result<(), String> {
    write_export_with(path, |out| write_bundle_json(file, out))
}

/// Compile the full bundle for the preview the Workbench shows, refuse it if
/// it is no longer that selection, then ask where to save it and stream it
/// there. Returns `Ok(false)` when the save panel is cancelled. The full
/// bundle never crosses IPC (#1582, AC-0740).
#[tauri::command]
pub(crate) async fn save_spec_bundle_export<R: tauri::Runtime>(
    suggested_name: String,
    expected: ExpectedSpecBundle,
    app: tauri::AppHandle<R>,
) -> Result<bool, String> {
    let file = crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        let bundle = crate::compile_spec_bundle(crate::export_inputs(&state)?, expected.mode)?;
        matching_bundle_file(&expected, bundle)
    })
    .await?;
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_file_name(sanitize_export_name(&suggested_name))
        .save_file()
        .await
    else {
        return Ok(false);
    };
    let path = handle.path().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || write_bundle_file(&path, &file))
        .await
        .map_err(|_| "Failed to write export file: worker failed".to_string())??;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ui/src/fixtures/spec-bundle-file.input.json`: a Workbench preview
    /// and the full artifacts `read_spec_artifact` returns for it.
    #[derive(Deserialize)]
    struct Fixture {
        preview: ExpectedSpecBundle,
        full_artifacts: Vec<SpecArtifactPreview>,
    }

    const INPUT: &str = include_str!("../../ui/src/fixtures/spec-bundle-file.input.json");
    /// What the Workbench built before #1582 from `INPUT`, checked against
    /// that formula by `ui/src/specExport.test.ts`.
    const EXPECTED: &str = include_str!("../../ui/src/fixtures/spec-bundle-file.expected.json");

    fn fixture_file() -> SpecBundleFile {
        let fixture: Fixture = serde_json::from_str(INPUT).expect("fixture parses");
        let preview = fixture.preview;
        SpecBundleFile {
            mode: preview.mode,
            artifacts: fixture.full_artifacts,
            assertion_count: preview.assertion_count,
            gap_count: preview.gap_count,
            drift_count: preview.drift_count,
            security_count: preview.security_count,
            graph_basis: preview.graph_basis,
            curated_basis: preview.curated_basis,
        }
    }

    fn spec_artifact(full: &SpecArtifactPreview) -> spec::SpecArtifact {
        spec::SpecArtifact {
            id: full.id.clone(),
            file_name: full.file_name.clone(),
            title: full.title.clone(),
            format: full.format.clone(),
            schema: full.schema.clone(),
            content_id: full.content_id.clone(),
            content: full.content.clone(),
            assertions: full.assertions.clone(),
        }
    }

    fn fixture_bundle() -> (ExpectedSpecBundle, spec::SpecBundle) {
        let fixture: Fixture = serde_json::from_str(INPUT).expect("fixture parses");
        let preview = fixture.preview;
        let bundle = spec::SpecBundle {
            mode: preview.mode,
            artifacts: fixture.full_artifacts.iter().map(spec_artifact).collect(),
            assertion_count: preview.assertion_count,
            gap_count: preview.gap_count,
            drift_count: preview.drift_count,
            security_count: preview.security_count,
            graph_basis: preview.graph_basis.clone(),
            curated_basis: preview.curated_basis.clone(),
        };
        (preview, bundle)
    }

    // AC-0740: the streamed file is byte for byte the JSON the Workbench
    // built in the WebView before #1582.
    #[test]
    fn streamed_bundle_file_is_byte_identical_to_the_workbench_built_json() {
        let mut bytes = Vec::new();
        write_bundle_json(&fixture_file(), &mut bytes).expect("serializes");
        assert_eq!(String::from_utf8(bytes).unwrap(), EXPECTED);

        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("cartograph-spec-best-effort.json");
        write_bundle_file(&target, &fixture_file()).expect("writes");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), EXPECTED);
    }

    // AC-0740: the fixture covers every artifact field, so a field the
    // typed path dropped would show here rather than pass unnoticed.
    #[test]
    fn the_fixture_round_trips_through_the_wire_types() {
        let raw: serde_json::Value = serde_json::from_str(INPUT).unwrap();
        let fixture: Fixture = serde_json::from_str(INPUT).unwrap();
        assert_eq!(
            serde_json::to_value(&fixture.full_artifacts).unwrap(),
            raw["full_artifacts"]
        );
    }

    // AC-0740: a compiled bundle that is still the preview's selection is
    // written in full, in the `read_spec_artifact` shape.
    #[test]
    fn a_matching_compiled_bundle_becomes_the_bundle_file() {
        let (preview, bundle) = fixture_bundle();
        assert_eq!(
            matching_bundle_file(&preview, bundle).unwrap(),
            fixture_file()
        );
    }

    // AC-0740: a changed artifact or bundle is refused, never written in
    // place of the selection the user exported (AC-0242).
    #[test]
    fn a_changed_selection_is_refused_and_names_the_artifact() {
        let (preview, mut bundle) = fixture_bundle();
        bundle.artifacts[1].content_id = "changed".into();
        let name = preview.artifacts[1].file_name.clone();
        assert_eq!(
            matching_bundle_file(&preview, bundle).unwrap_err(),
            stale_artifact_message(&name)
        );

        let (preview, mut bundle) = fixture_bundle();
        bundle.artifacts[0].schema = "cartograph.spec/other@1".into();
        assert!(matching_bundle_file(&preview, bundle).is_err());

        let (preview, mut bundle) = fixture_bundle();
        bundle.artifacts.pop();
        assert_eq!(
            matching_bundle_file(&preview, bundle).unwrap_err(),
            STALE_BUNDLE_MESSAGE
        );

        let (preview, mut bundle) = fixture_bundle();
        bundle.gap_count += 1;
        assert_eq!(
            matching_bundle_file(&preview, bundle).unwrap_err(),
            STALE_BUNDLE_MESSAGE
        );

        let (preview, mut bundle) = fixture_bundle();
        bundle.graph_basis = None;
        assert_eq!(
            matching_bundle_file(&preview, bundle).unwrap_err(),
            STALE_BUNDLE_MESSAGE
        );
    }

    #[test]
    fn array_index_names_follow_js_key_order() {
        assert_eq!(is_array_index("0"), Some(0));
        assert_eq!(is_array_index("10"), Some(10));
        assert_eq!(is_array_index("010"), None);
        assert_eq!(is_array_index("-1"), None);
        assert_eq!(is_array_index("4294967295"), None);
        assert_eq!(is_array_index("README.md"), None);
    }
}
