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

#[cfg(test)]
use serde::Serializer;
#[cfg(test)]
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::SpecArtifactPreview;
#[cfg(test)]
use crate::export_save::write_export_with;
use crate::export_save::{move_export_into, sanitize_export_name};

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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
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

/// Copies `out` and writes `indent` after every newline, so a value
/// serialized pretty on its own lands at the depth it has in the file.
/// JSON text has no raw newline inside a string, so only layout newlines
/// are indented.
struct Indented<'a, W: Write> {
    out: &'a mut W,
    indent: &'static [u8],
}

impl<W: Write> Write for Indented<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut rest = bytes;
        while let Some(at) = rest.iter().position(|byte| *byte == b'\n') {
            self.out.write_all(&rest[..=at])?;
            self.out.write_all(self.indent)?;
            rest = &rest[at + 1..];
        }
        self.out.write_all(rest)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

/// `value` as 2-space pretty JSON at `indent` depth.
fn pretty_at<W: Write>(
    out: &mut W,
    indent: &'static [u8],
    value: &impl Serialize,
) -> std::io::Result<()> {
    serde_json::to_writer_pretty(Indented { out, indent }, value).map_err(std::io::Error::other)
}

/// The bundle file written as its artifacts compile (#2215): each artifact is
/// checked against the preview, written into the file's `artifacts` and its
/// content kept on disk for `files`, then dropped, so the full bundle is
/// never held at once. The finished file is byte for byte
/// [`write_bundle_json`] of [`matching_bundle_file`].
pub(crate) struct BundleFileStream<'a> {
    expected: &'a ExpectedSpecBundle,
    body: BufWriter<tempfile::NamedTempFile>,
    contents: BufWriter<std::fs::File>,
    /// Each artifact's file name and the span of its JSON-encoded content
    /// in `contents`.
    names: Vec<(String, u64, u64)>,
    spooled: u64,
    failure: Option<String>,
}

impl<'a> BundleFileStream<'a> {
    /// A stream for the preview `expected`, spooled in the temp directory.
    pub(crate) fn new(expected: &'a ExpectedSpecBundle) -> Result<Self, String> {
        let fail = |err: std::io::Error| format!("Failed to write export file: {err}");
        let mut body = BufWriter::with_capacity(1 << 16, new_temp().map_err(fail)?);
        let contents = BufWriter::with_capacity(1 << 16, tempfile::tempfile().map_err(fail)?);
        body.write_all(b"{\n  \"mode\": ")
            .and_then(|()| serde_json::to_writer(&mut body, &expected.mode).map_err(Into::into))
            .and_then(|()| body.write_all(b",\n  \"artifacts\": ["))
            .map_err(fail)?;
        Ok(Self {
            expected,
            body,
            contents,
            names: Vec::new(),
            spooled: 0,
            failure: None,
        })
    }

    /// Check one compiled artifact against the preview and write it. After a
    /// refusal or a write failure the rest are dropped unwritten.
    pub(crate) fn push(&mut self, artifact: spec::SpecArtifact) {
        if self.failure.is_some() {
            return;
        }
        let index = self.names.len();
        let Some(preview) = self.expected.artifacts.get(index) else {
            self.failure = Some(STALE_BUNDLE_MESSAGE.to_string());
            return;
        };
        if artifact.id != preview.id
            || artifact.schema != preview.schema
            || artifact.content_id != preview.content_id
        {
            self.failure = Some(stale_artifact_message(&preview.file_name));
            return;
        }
        if let Err(err) = self.write(crate::full_artifact_preview(artifact), index) {
            self.failure = Some(format!("Failed to write export file: {err}"));
        }
    }

    fn write(&mut self, artifact: SpecArtifactPreview, index: usize) -> std::io::Result<()> {
        self.body
            .write_all(if index == 0 { b"\n    " } else { b",\n    " })?;
        pretty_at(&mut self.body, b"    ", &artifact)?;
        let mut encoded = Counted(&mut self.contents, 0);
        serde_json::to_writer(&mut encoded, &artifact.content)?;
        let len = encoded.1;
        self.names.push((artifact.file_name, self.spooled, len));
        self.spooled += len;
        Ok(())
    }

    /// Check the bundle's counts and bases against the preview and finish
    /// the file, returned rewound to its start.
    pub(crate) fn finish(
        mut self,
        bundle: &spec::SpecBundle<()>,
    ) -> Result<tempfile::NamedTempFile, String> {
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }
        let expected = self.expected;
        if bundle.mode != expected.mode
            || self.names.len() != expected.artifacts.len()
            || bundle.assertion_count != expected.assertion_count
            || bundle.gap_count != expected.gap_count
            || bundle.drift_count != expected.drift_count
            || bundle.security_count != expected.security_count
            || bundle.graph_basis != expected.graph_basis
            || bundle.curated_basis != expected.curated_basis
        {
            return Err(STALE_BUNDLE_MESSAGE.to_string());
        }
        self.finish_file(bundle)
            .map_err(|err| format!("Failed to write export file: {err}"))
    }

    fn finish_file(
        self,
        bundle: &spec::SpecBundle<()>,
    ) -> std::io::Result<tempfile::NamedTempFile> {
        use std::io::{Read, Seek, SeekFrom};
        let Self {
            mut body,
            contents,
            names,
            ..
        } = self;
        body.write_all(if names.is_empty() { b"]" } else { b"\n  ]" })?;
        for (key, count) in [
            ("assertion_count", bundle.assertion_count),
            ("gap_count", bundle.gap_count),
            ("drift_count", bundle.drift_count),
            ("security_count", bundle.security_count),
        ] {
            write!(body, ",\n  \"{key}\": {count}")?;
        }
        if let Some(basis) = &bundle.graph_basis {
            body.write_all(b",\n  \"graph_basis\": ")?;
            pretty_at(&mut body, b"  ", basis)?;
        }
        if let Some(basis) = &bundle.curated_basis {
            body.write_all(b",\n  \"curated_basis\": ")?;
            pretty_at(&mut body, b"  ", basis)?;
        }
        body.write_all(b",\n  \"files\": ")?;
        let mut contents = contents.into_inner().map_err(|err| err.into_error())?;
        let order = files_order(&names);
        if order.is_empty() {
            body.write_all(b"{}")?;
        } else {
            body.write_all(b"{")?;
            for (i, (name, start, len)) in order.into_iter().enumerate() {
                body.write_all(if i == 0 { b"\n    " } else { b",\n    " })?;
                serde_json::to_writer(&mut body, name)?;
                body.write_all(b": ")?;
                contents.seek(SeekFrom::Start(start))?;
                std::io::copy(&mut (&mut contents).take(len), &mut body)?;
            }
            body.write_all(b"\n  }")?;
        }
        body.write_all(b"\n}")?;
        let mut file = body.into_inner().map_err(|err| err.into_error())?;
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }
}

/// Counts the bytes written through it.
struct Counted<'a, W: Write>(&'a mut W, u64);

impl<W: Write> Write for Counted<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.0.write(bytes)?;
        self.1 += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// The `files` entries in [`FilesByName`] order: a repeated name keeps its
/// first position and its last content, and array-index names come first in
/// ascending order.
fn files_order(names: &[(String, u64, u64)]) -> Vec<(&str, u64, u64)> {
    let mut entries: Vec<(&str, u64, u64)> = Vec::with_capacity(names.len());
    for (name, start, len) in names {
        match entries.iter_mut().find(|(key, _, _)| *key == name.as_str()) {
            Some(entry) => (entry.1, entry.2) = (*start, *len),
            None => entries.push((name, *start, *len)),
        }
    }
    let mut indexed: Vec<(u32, (&str, u64, u64))> = entries
        .iter()
        .filter_map(|entry| is_array_index(entry.0).map(|n| (n, *entry)))
        .collect();
    indexed.sort_by_key(|(n, _)| *n);
    indexed
        .into_iter()
        .map(|(_, entry)| entry)
        .chain(
            entries
                .into_iter()
                .filter(|(key, _, _)| is_array_index(key).is_none()),
        )
        .collect()
}

#[cfg(test)]
/// Write the bundle file to `path` with the same atomic replace as every
/// other export (#729).
pub(crate) fn write_bundle_file(path: &Path, file: &SpecBundleFile) -> Result<(), String> {
    write_export_with(path, |out| write_bundle_json(file, out))
}

/// The bundle file for the preview `expected`, written to a temp file as
/// `compile` hands over each artifact, refused as [`matching_bundle_file`]
/// refuses a changed selection.
pub(crate) fn stream_bundle_file(
    expected: &ExpectedSpecBundle,
    compile: impl FnOnce(&mut dyn FnMut(spec::SpecArtifact)) -> Result<spec::SpecBundle<()>, String>,
) -> Result<tempfile::NamedTempFile, String> {
    let mut stream = BundleFileStream::new(expected)?;
    let bundle = compile(&mut |artifact| stream.push(artifact))?;
    stream.finish(&bundle)
}

/// Compile the full bundle for the preview the Workbench shows, one artifact
/// at a time into a temp file (#2215), refuse it if it is no longer that
/// selection, then ask where to save it and move it there. Returns
/// `Ok(false)` when the save panel is cancelled. The full bundle never
/// crosses IPC (#1582, AC-0740).
#[tauri::command]
pub(crate) async fn save_spec_bundle_export<R: tauri::Runtime>(
    suggested_name: String,
    expected: ExpectedSpecBundle,
    app: tauri::AppHandle<R>,
) -> Result<bool, String> {
    let file = crate::off_ui_thread(move || {
        let state = app.state::<crate::AppState>();
        stream_bundle_file(&expected, |push| {
            crate::compile_spec_bundle_kept(crate::export_inputs(&state)?, expected.mode, push)
        })
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
    tauri::async_runtime::spawn_blocking(move || move_export_into(&path, file))
        .await
        .map_err(|_| "Failed to write export file: worker failed".to_string())??;
    Ok(true)
}

/// Every bundle temp file's name starts with this, then the creating
/// process's id and a `-`.
const TEMP_PREFIX: &str = ".cartograph-bundle-";
const TEMP_SUFFIX: &str = ".partial";
/// A bundle temp file another process has not written for this long is left
/// over from an export that never finished, such as one whose app was killed
/// while compiling or with the save panel open.
const STALE_TEMP_AGE: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// The bundle file's temp file: named, so the finished file can be renamed
/// into place, and owner-only. A named file outlives a killed app, so
/// creating one first removes the stale ones earlier exports left.
fn new_temp() -> std::io::Result<tempfile::NamedTempFile> {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    sweep_stale_temps(&dir, pid, std::time::SystemTime::now());
    tempfile::Builder::new()
        .prefix(&format!("{TEMP_PREFIX}{pid}-"))
        .suffix(TEMP_SUFFIX)
        .tempfile_in(dir)
}

/// Remove the bundle temp files in `dir` that another process left and has
/// not written for [`STALE_TEMP_AGE`]. This process's own are never touched,
/// since one may be waiting on its save panel. Failures are ignored: a file
/// that cannot be removed now is tried again by the next export.
fn sweep_stale_temps(dir: &Path, pid: u32, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name
            .to_str()
            .and_then(|name| name.strip_prefix(TEMP_PREFIX))
            .filter(|rest| rest.ends_with(TEMP_SUFFIX))
        else {
            continue;
        };
        if rest.split('-').next() == Some(pid.to_string().as_str()) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .filter(|meta| meta.is_file())
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_TEMP_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
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

    /// The file [`stream_bundle_file`] writes for `bundle`, read back.
    fn streamed(preview: &ExpectedSpecBundle, bundle: spec::SpecBundle) -> Result<String, String> {
        let mut file = stream_bundle_file(preview, |push| {
            let spec::SpecBundle {
                mode,
                artifacts,
                assertion_count,
                gap_count,
                drift_count,
                security_count,
                graph_basis,
                curated_basis,
            } = bundle;
            Ok(spec::SpecBundle {
                mode,
                artifacts: artifacts.into_iter().map(push).collect(),
                assertion_count,
                gap_count,
                drift_count,
                security_count,
                graph_basis,
                curated_basis,
            })
        })?;
        let mut text = String::new();
        std::io::Read::read_to_string(&mut file, &mut text).unwrap();
        Ok(text)
    }

    // AC-1213 (T-1213): a bundle temp file left by another process and not
    // written for six hours is removed when an export starts; this
    // process's own, a recent one and any other file are kept.
    #[test]
    fn stale_bundle_temps_from_other_processes_are_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = std::time::SystemTime::now();
        let old = now - STALE_TEMP_AGE - std::time::Duration::from_secs(60);
        let pid = 4242;
        let file = |name: &str, modified: std::time::SystemTime| {
            let path = dir.path().join(name);
            std::fs::File::create(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
            path
        };
        let left = file(".cartograph-bundle-17-abc.partial", old);
        let own = file(".cartograph-bundle-4242-abc.partial", old);
        let recent = file(".cartograph-bundle-17-def.partial", now);
        let other = file(".cartograph-bundle-17-abc.json", old);
        let unrelated = file("export.partial", old);
        sweep_stale_temps(dir.path(), pid, now);
        assert!(!left.exists());
        for kept in [own, recent, other, unrelated] {
            assert!(kept.exists(), "{kept:?} kept");
        }

        let temp = new_temp().unwrap();
        let name = temp.path().file_name().unwrap().to_str().unwrap();
        let mine = format!("{TEMP_PREFIX}{}-", std::process::id());
        assert!(
            name.starts_with(&mine) && name.ends_with(TEMP_SUFFIX),
            "{name}"
        );
    }

    // AC-1212 (T-1212): the file written one artifact at a time is byte for
    // byte the file of the complete bundle, with or without bases, with
    // repeated and array-index file names, and refused alike.
    #[test]
    fn a_bundle_streamed_per_artifact_is_the_complete_bundle_file() {
        let (preview, bundle) = fixture_bundle();
        assert_eq!(streamed(&preview, bundle).unwrap(), EXPECTED);

        type Change = fn(&mut ExpectedSpecBundle, &mut spec::SpecBundle);
        let variants: [Change; 2] = [
            |preview, bundle| {
                (preview.graph_basis, bundle.graph_basis) = (None, None);
                (preview.curated_basis, bundle.curated_basis) = (None, None);
            },
            |_, bundle| {
                let names = ["7", "README.md", "0", "README.md"];
                for (artifact, name) in bundle.artifacts.iter_mut().zip(names) {
                    artifact.file_name = name.into();
                }
            },
        ];
        for change in &variants {
            let (mut preview, mut bundle) = fixture_bundle();
            change(&mut preview, &mut bundle);
            let mut complete = Vec::new();
            let file = matching_bundle_file(&preview, bundle.clone()).unwrap();
            write_bundle_json(&file, &mut complete).unwrap();
            assert_eq!(
                streamed(&preview, bundle).unwrap(),
                String::from_utf8(complete).unwrap()
            );
        }

        let (preview, mut bundle) = fixture_bundle();
        bundle.artifacts[1].content_id = "changed".into();
        let name = preview.artifacts[1].file_name.clone();
        assert_eq!(
            streamed(&preview, bundle).unwrap_err(),
            stale_artifact_message(&name)
        );
        for change in [
            (|bundle: &mut spec::SpecBundle| drop(bundle.artifacts.pop()))
                as fn(&mut spec::SpecBundle),
            |bundle| bundle.gap_count += 1,
            |bundle| bundle.graph_basis = None,
            |bundle| {
                let extra = bundle.artifacts[0].clone();
                bundle.artifacts.push(extra);
            },
        ] {
            let (preview, mut bundle) = fixture_bundle();
            change(&mut bundle);
            assert_eq!(
                streamed(&preview, bundle).unwrap_err(),
                STALE_BUNDLE_MESSAGE
            );
        }
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
