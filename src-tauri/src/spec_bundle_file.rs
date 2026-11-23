//! Workbench "Export bundle", written by the host straight to the chosen
//! file (#1582, AC-0740).
//!
//! The WebView sends only the identities of the preview it shows; this
//! module compiles the full bundle one artifact at a time, checks each is
//! the same selection, and streams the bundle file (ADR-0086, v3) to a temp
//! file that is moved into place after the save panel. The bytes are the
//! ones the browser preview builds (`specBundleFileContent`).

use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::Deserialize;
use spec::bundle_file::{BundleFileError, BundleFileV2};
use tauri::Manager;

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

/// The same message the Workbench shows for a stale copy (`specExport.ts`).
fn stale_artifact_message(file_name: &str) -> String {
    format!(
        "{file_name} changed since this preview was compiled. Refresh the Spec Workbench and try again."
    )
}

const STALE_BUNDLE_MESSAGE: &str =
    "The spec changed since this preview was compiled. Refresh the Spec Workbench and try again.";

fn write_failure(err: impl std::fmt::Display) -> String {
    format!("Failed to write export file: {err}")
}

/// A spool: an unnamed temp file written through a buffer.
fn spool() -> std::io::Result<BufWriter<std::fs::File>> {
    Ok(BufWriter::with_capacity(1 << 16, tempfile::tempfile()?))
}

/// A spool's file, rewound to its start for reading back.
fn rewound(spool: BufWriter<std::fs::File>) -> std::io::Result<std::fs::File> {
    let mut file = spool.into_inner().map_err(|err| err.into_error())?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

/// The bundle file (v3) written as its artifacts compile (#2215, ADR-0086):
/// each artifact is checked against the preview, its line and its new
/// assertions written to their spools and its content to a third, then
/// dropped, so the full bundle is never held at once. [`finish`] joins the
/// spools into the named temp file in the bundle file's order.
///
/// [`finish`]: BundleFileStream::finish
pub(crate) struct BundleFileStream<'a> {
    expected: &'a ExpectedSpecBundle,
    writer: BundleFileV2,
    body: BufWriter<tempfile::NamedTempFile>,
    artifacts: BufWriter<std::fs::File>,
    assertions: BufWriter<std::fs::File>,
    contents: BufWriter<std::fs::File>,
    /// Each artifact's file name and the length of its JSON-encoded content
    /// in `contents`, in push order.
    names: Vec<(String, u64)>,
    failure: Option<String>,
}

impl<'a> BundleFileStream<'a> {
    /// A stream for the preview `expected`, spooled in the temp directory.
    pub(crate) fn new(expected: &'a ExpectedSpecBundle) -> Result<Self, String> {
        Ok(Self {
            expected,
            writer: BundleFileV2::new(),
            body: BufWriter::with_capacity(1 << 16, new_temp().map_err(write_failure)?),
            artifacts: spool().map_err(write_failure)?,
            assertions: spool().map_err(write_failure)?,
            contents: spool().map_err(write_failure)?,
            names: Vec::new(),
            failure: None,
        })
    }

    /// Check one compiled artifact against the preview and write it. After a
    /// refusal or a write failure the rest are dropped unwritten.
    pub(crate) fn push(&mut self, artifact: spec::SpecArtifact) {
        if self.failure.is_some() {
            return;
        }
        let Some(preview) = self.expected.artifacts.get(self.names.len()) else {
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
        if let Err(err) = self.write(artifact) {
            self.failure = Some(write_failure(err));
        }
    }

    fn write(&mut self, artifact: spec::SpecArtifact) -> Result<(), BundleFileError> {
        self.writer
            .push(&artifact, &mut self.artifacts, &mut self.assertions)?;
        let mut encoded = Counted(&mut self.contents, 0);
        serde_json::to_writer(&mut encoded, &artifact.content)?;
        let len = encoded.1;
        self.names.push((artifact.file_name, len));
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
        self.finish_file(bundle).map_err(write_failure)
    }

    fn finish_file(
        self,
        bundle: &spec::SpecBundle<()>,
    ) -> Result<tempfile::NamedTempFile, BundleFileError> {
        let Self {
            writer,
            mut body,
            artifacts,
            assertions,
            contents,
            names,
            ..
        } = self;
        let mut artifacts = rewound(artifacts)?;
        let mut assertions = rewound(assertions)?;
        let mut contents = rewound(contents)?;
        // An artifact the reader renders from another (ADR-0086 v3) is
        // carried by name; its spooled content is skipped.
        let rendered = writer.rendered_from();
        writer.finish(bundle, &mut body, &mut artifacts, &mut assertions, |out| {
            for (i, (name, len)) in names.iter().enumerate() {
                let content = &mut (&mut contents).take(*len);
                if let Some(from) = &rendered[i] {
                    BundleFileV2::rendered_entry(out, i, name, from)?;
                    std::io::copy(content, &mut std::io::sink())?;
                } else {
                    BundleFileV2::file_entry(out, i, name, b"")?;
                    std::io::copy(content, out)?;
                }
            }
            Ok(())
        })?;
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

/// The bundle file for the preview `expected`, written to a temp file as
/// `compile` hands over each artifact, refused if it is no longer the
/// previewed selection.
pub(crate) fn stream_bundle_file(
    expected: &ExpectedSpecBundle,
    compile: impl FnOnce(&mut dyn FnMut(spec::SpecArtifact)) -> Result<spec::SpecBundle<()>, String>,
) -> Result<tempfile::NamedTempFile, String> {
    let mut stream = BundleFileStream::new(expected)?;
    let bundle = compile(&mut |artifact| stream.push(artifact))?;
    stream.finish(&bundle)
}

/// One bundle compile at a time. A cancelled save panel leaves its compile
/// running, so the next export's compile waits for it instead of running
/// alongside it.
static ONE_COMPILE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Start `compile` on a blocking worker, then wait for `choose` while it runs
/// (#1601): the save panel opened only after the whole bundle compiled, about
/// 10 s on a real system. `None` from `choose` (a cancelled panel) returns
/// `Ok(None)` at once; a compile already running finishes and drops its
/// result, so its temp file is removed. The worker compiles only while it
/// holds `gate`, and skips its compile if the panel was cancelled before it
/// got the gate, so cancelling and exporting again never runs two compiles at
/// once. A compile error is reported after the choice.
pub(crate) async fn compile_while_choosing<T: Send + 'static, P>(
    gate: &'static std::sync::Mutex<()>,
    compile: impl FnOnce() -> Result<T, String> + Send + 'static,
    choose: impl std::future::Future<Output = Option<P>>,
) -> Result<Option<(T, P)>, String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let cancelled = std::sync::Arc::new(AtomicBool::new(false));
    let skip = cancelled.clone();
    let compiled = tauri::async_runtime::spawn_blocking(move || {
        let _one = gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if skip.load(Ordering::Acquire) {
            return Ok(None);
        }
        compile().map(Some)
    });
    let Some(chosen) = choose.await else {
        cancelled.store(true, Ordering::Release);
        return Ok(None);
    };
    let value = compiled.await.map_err(|error| error.to_string())??;
    Ok(value.map(|value| (value, chosen)))
}

/// Compile the full bundle for the preview the Workbench shows, one artifact
/// at a time into a temp file (#2215), while the save panel asks where to
/// save it; refuse it if it is no longer that selection, then move it there.
/// Returns `Ok(false)` when the save panel is cancelled. The full bundle
/// never crosses IPC (#1582, AC-0740).
#[tauri::command]
pub(crate) async fn save_spec_bundle_export<R: tauri::Runtime>(
    suggested_name: String,
    expected: ExpectedSpecBundle,
    app: tauri::AppHandle<R>,
) -> Result<bool, String> {
    let compile = move || {
        let state = app.state::<crate::AppState>();
        stream_bundle_file(&expected, |push| {
            crate::compile_spec_bundle_kept(crate::export_inputs(&state)?, expected.mode, push)
        })
    };
    let choose = rfd::AsyncFileDialog::new()
        .set_file_name(sanitize_export_name(&suggested_name))
        .save_file();
    let Some((file, handle)) = compile_while_choosing(&ONE_COMPILE, compile, choose).await? else {
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
    use crate::SpecArtifactPreview;

    /// The save panel opens while the bundle compiles (#1601): the compile
    /// waits on a signal only the panel sends, so a sequential save would
    /// never finish.
    #[test]
    fn save_panel_opens_while_the_bundle_compiles() {
        let (opened, panel) = std::sync::mpsc::channel::<()>();
        let compile = move || {
            panel
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|_| "the panel never opened during the compile".to_string())?;
            Ok("bundle")
        };
        let choose = async move {
            opened.send(()).unwrap();
            Some("/tmp/out.json")
        };
        static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let result = tauri::async_runtime::block_on(compile_while_choosing(&GATE, compile, choose));
        assert_eq!(result, Ok(Some(("bundle", "/tmp/out.json"))));
    }

    /// Cancelling and exporting again never runs two compiles at once: an
    /// export cancelled while another compile runs never starts its own.
    #[test]
    fn an_export_cancelled_behind_a_running_compile_never_compiles() {
        static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let running = GATE.lock().unwrap();
        let (ran, compiled) = std::sync::mpsc::channel::<()>();
        let compile = move || {
            ran.send(()).unwrap();
            Ok(())
        };
        let cancelled =
            tauri::async_runtime::block_on(compile_while_choosing(&GATE, compile, async {
                None::<()>
            }));
        assert_eq!(cancelled, Ok(None));
        drop(running);
        assert!(compiled.recv().is_err(), "the cancelled export compiled");
    }

    #[test]
    fn a_cancelled_panel_returns_at_once_and_a_failed_compile_is_reported() {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let slow = move || {
            let _ = wait.recv();
            Ok(())
        };
        static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let cancelled =
            tauri::async_runtime::block_on(compile_while_choosing(&GATE, slow, async {
                None::<()>
            }));
        assert_eq!(cancelled, Ok(None));
        drop(release);
        let failed = tauri::async_runtime::block_on(compile_while_choosing(
            &GATE,
            || Err::<(), _>("selection changed".to_string()),
            async { Some(()) },
        ));
        assert_eq!(failed, Err("selection changed".to_string()));
    }

    /// `ui/src/fixtures/spec-bundle-file.input.json`: a Workbench preview
    /// and the full artifacts `read_spec_artifact` returns for it.
    #[derive(Deserialize)]
    struct Fixture {
        preview: ExpectedSpecBundle,
        full_artifacts: Vec<SpecArtifactPreview>,
    }

    const INPUT: &str = include_str!("../../ui/src/fixtures/spec-bundle-file.input.json");
    /// The v3 file of `INPUT`, which `ui/src/specExport.test.ts` checks the
    /// browser preview builds too (ADR-0086).
    const EXPECTED: &str = include_str!("../../ui/src/fixtures/spec-bundle-file.expected.json");

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
        file.read_to_string(&mut text).unwrap();
        Ok(text)
    }

    /// The spec crate's bundle file of the complete bundle.
    fn whole(bundle: &spec::SpecBundle) -> String {
        let mut out = Vec::new();
        spec::bundle_file::write_bundle_file(bundle, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    // AC-0740 (ADR-0086): the streamed file is byte for byte the file the
    // browser preview builds for the fixture, and reads back to the full
    // bundle the host compiled.
    #[test]
    fn streamed_bundle_file_is_the_file_the_preview_builds() {
        let (preview, bundle) = fixture_bundle();
        let text = streamed(&preview, bundle.clone()).unwrap();
        assert_eq!(text, EXPECTED);
        assert_eq!(spec::bundle_file::read_bundle_file(&text).unwrap(), bundle);
    }

    // AC-1214 (ADR-0086 v3): the streamed file carries `fact-index.md` by
    // the name of the file it is rendered from, as the spec crate's writer
    // does, and skips its spooled content.
    #[test]
    fn streamed_bundle_file_carries_the_markdown_index_by_name() {
        let (mut preview, mut bundle) = fixture_bundle();
        for (id, file_name, content) in [
            ("fact-index", "fact-index.md", "MARKDOWN-INDEX-BODY"),
            ("fact-index-json", "fact-index.json", "{\"rows\":[]}"),
        ] {
            let mut artifact = bundle.artifacts[0].clone();
            artifact.id = id.into();
            artifact.file_name = file_name.into();
            artifact.content = content.into();
            preview.artifacts.push(ExpectedArtifact {
                id: id.into(),
                file_name: file_name.into(),
                schema: artifact.schema.clone(),
                content_id: artifact.content_id.clone(),
            });
            bundle.artifacts.push(artifact);
        }
        let text = streamed(&preview, bundle.clone()).unwrap();
        assert_eq!(text, whole(&bundle));
        assert!(text.contains("\"fact-index.md\":{\"rendered_from\":\"fact-index.json\"}"));
        assert!(!text.contains("MARKDOWN-INDEX-BODY"));
        assert!(text.contains("\"fact-index.json\":\"{\\\"rows\\\":[]}\""));
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
    // byte the v2 file of the complete bundle, with or without bases, and a
    // changed selection or a repeated file name is refused.
    #[test]
    fn a_bundle_streamed_per_artifact_is_the_complete_bundle_file() {
        let (preview, bundle) = fixture_bundle();
        assert_eq!(streamed(&preview, bundle.clone()).unwrap(), whole(&bundle));

        let (mut preview, mut bundle) = fixture_bundle();
        (preview.graph_basis, bundle.graph_basis) = (None, None);
        (preview.curated_basis, bundle.curated_basis) = (None, None);
        assert_eq!(streamed(&preview, bundle.clone()).unwrap(), whole(&bundle));

        let (preview, mut bundle) = fixture_bundle();
        bundle.artifacts[1].content_id = "changed".into();
        let name = preview.artifacts[1].file_name.clone();
        assert_eq!(
            streamed(&preview, bundle).unwrap_err(),
            stale_artifact_message(&name)
        );
        let (preview, mut bundle) = fixture_bundle();
        bundle.artifacts[0].schema = "cartograph.spec/other@1".into();
        let name = preview.artifacts[0].file_name.clone();
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

        let (mut preview, mut bundle) = fixture_bundle();
        let name = bundle.artifacts[0].file_name.clone();
        bundle.artifacts[1].file_name = name.clone();
        preview.artifacts[1].file_name = name.clone();
        assert_eq!(
            streamed(&preview, bundle).unwrap_err(),
            write_failure(BundleFileError::RepeatedFileName(name))
        );
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
}
