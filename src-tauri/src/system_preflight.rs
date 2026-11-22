//! System-manifest preflight preview (#1655, AC-0705..0707): detect each
//! local member's languages, frameworks and adapter coverage before a system
//! recovery, relative to the manifest's directory, and defer every remote or
//! managed entry without fetching it. Preview only: it registers no source,
//! persists no finding and writes no graph fact. Recovery (`add_system`)
//! still scans each repo itself and owns the findings it publishes (AC-0209).

use crate::source_access::SourceOperation;
use crate::{AppState, PREFLIGHT_CANCELLED, PreflightRuns, PreflightToken};
use serde::Serialize;
use tauri::Manager;

/// One manifest entry's preview, in manifest order.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum MemberPreflight {
    /// A local directory, scanned in place.
    Local {
        /// The entry exactly as the manifest declares it.
        entry: String,
        /// The canonical directory it resolved to.
        path: String,
        report: ingest::preflight::PreflightReport,
    },
    /// A remote or managed source: never fetched by a preview; recovery
    /// detects it against its clone.
    Deferred { entry: String },
}

#[derive(Debug, Serialize)]
pub(crate) struct SystemPreflight {
    pub(crate) members: Vec<MemberPreflight>,
}

/// The `preflight_system` command: shares the preflight run slot, so a
/// preview supersedes (and is superseded by) any other preflight, and
/// `cancel_preflight` stops it.
#[tauri::command]
pub(crate) async fn preflight_system(
    path: String,
    run: u64,
    app: tauri::AppHandle,
) -> Result<SystemPreflight, String> {
    // Registered before the worker is queued, like `preflight` (#434).
    let token = app.state::<PreflightRuns>().begin()?;
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let mut progress = crate::preflight_progress_throttle(&app, run);
        preflight_system_blocking(&path, &app, &state, &token, &mut progress)
    })
    .await
}

/// Scan every local member of the manifest at `path`. Any member failing —
/// a missing directory, an unavailable registered source, a cancel — fails
/// the whole preview, so no partial result is ever returned.
pub(crate) fn preflight_system_blocking<R: tauri::Runtime>(
    path: &str,
    app: &tauri::AppHandle<R>,
    state: &AppState,
    token: &PreflightToken,
    on_progress: &mut dyn FnMut(ingest::preflight::ScanStep<'_>),
) -> Result<SystemPreflight, String> {
    let manifest_path = crate::paths::canonicalize(path).map_err(|e| e.to_string())?;
    let manifest =
        ingest::manifest::SystemManifest::load(&manifest_path).map_err(|e| e.to_string())?;
    let base = crate::manifest_dir(&manifest_path);
    let mut members = Vec::with_capacity(manifest.repos.len());
    for entry in &manifest.repos {
        if token.is_cancelled() {
            return Err(PREFLIGHT_CANCELLED.into());
        }
        if crate::manifest_entry_is_remote(&entry.url, base) {
            members.push(MemberPreflight::Deferred {
                entry: entry.url.clone(),
            });
            continue;
        }
        let named = |e: String| format!("{}: {e}", entry.url);
        let (resolved, report) = preview_member(
            &base.join(&entry.url),
            &entry.url,
            app,
            state,
            token,
            on_progress,
        )
        .map_err(|e| {
            if e == PREFLIGHT_CANCELLED {
                e
            } else {
                named(e)
            }
        })?;
        members.push(MemberPreflight::Local {
            entry: entry.url.clone(),
            path: resolved,
            report,
        });
    }
    // The same last-step race as a register write (AC-0215): a cancel that
    // lands first wins; one that arrives after the claim loses.
    if !token.state.claim_commit() {
        return Err(PREFLIGHT_CANCELLED.into());
    }
    Ok(SystemPreflight { members })
}

fn preview_member<R: tauri::Runtime>(
    dir: &std::path::Path,
    entry: &str,
    app: &tauri::AppHandle<R>,
    state: &AppState,
    token: &PreflightToken,
    on_progress: &mut dyn FnMut(ingest::preflight::ScanStep<'_>),
) -> Result<(String, ingest::preflight::PreflightReport), String> {
    let canonical = crate::paths::canonicalize(dir).map_err(|e| e.to_string())?;
    if !canonical.is_dir() {
        return Err("not a directory".into());
    }
    // A member that is already a registered source is read only under its
    // access guard (an unavailable registration fails the preview); an
    // unregistered one is read in place and stays unregistered — a preview
    // never admits a source.
    let registered = state
        .sources
        .lock()
        .map_err(|e| e.to_string())?
        .get_by_root(&canonical)?;
    let operation = registered
        .as_ref()
        .map(|source| SourceOperation::acquire(&state.sources, [(source.clone(), false)]))
        .transpose()?;
    let root = match (&operation, &registered) {
        (Some(operation), Some(source)) => operation.root(&source.repo_key)?,
        _ => canonical.as_path(),
    };
    let plugins = crate::plugin_coverage(&crate::active_plugins_for_root(app, state, root)?);
    let mut report = ingest::preflight::preflight_scan(
        root,
        &plugins,
        ingest::preflight::EvalProofSource::PendingRecovery,
        &mut |step| {
            if token.is_cancelled() {
                return std::ops::ControlFlow::Break(());
            }
            let path = if step.path.is_empty() {
                entry.to_string()
            } else {
                format!("{entry}/{}", step.path)
            };
            on_progress(ingest::preflight::ScanStep {
                path: &path,
                ..step
            });
            std::ops::ControlFlow::Continue(())
        },
    )
    .map_err(|e| e.to_string())?
    .ok_or(PREFLIGHT_CANCELLED)?;
    report.refresh_adapter_lookup(&crate::plugin_declarations_for_root(app, state, root)?);
    Ok((canonical.display().to_string(), report))
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod tests {
    use super::{MemberPreflight, preflight_system_blocking};
    use tauri::Manager;

    fn write_manifest(dir: &std::path::Path, entries: &[&str]) -> String {
        let body: String = entries
            .iter()
            .map(|url| format!("[[repos]]\nurl = \"{url}\"\n\n"))
            .collect();
        let manifest = dir.join("cartograph.system.toml");
        std::fs::write(&manifest, body).unwrap();
        manifest.to_string_lossy().into_owned()
    }

    fn system_fixture(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("services/api")).unwrap();
        std::fs::write(dir.join("services/api/server.ts"), "eval(getCode());\n").unwrap();
        std::fs::write(
            dir.join("services/api/package.json"),
            r#"{"dependencies":{"react":"^19"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("worker")).unwrap();
        std::fs::write(dir.join("worker/main.go"), "package main\nfunc main() {}\n").unwrap();
    }

    fn graph_nodes(state: &crate::AppState) -> usize {
        state.graph.lock().unwrap().read_snapshot().unwrap().0.len()
    }

    #[test]
    fn system_preflight_previews_local_members_and_defers_remote_ones() {
        // AC-0705 / AC-0706 (#1655): each local member is scanned relative
        // to the manifest directory, in manifest order; remote and managed
        // entries are deferred without fetching; nothing is registered,
        // recorded or materialized.
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        system_fixture(&system);
        let manifest = write_manifest(
            &system,
            &[
                "services/api",
                "https://github.com/example/remote",
                "file:///nonexistent/managed.git",
                "worker",
                "owner/name",
            ],
        );
        let app = crate::tests::preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<crate::AppState>();
        let runs = app.state::<crate::PreflightRuns>();
        let token = runs.begin().unwrap();

        let preview =
            preflight_system_blocking(&manifest, &handle, &state, &token, &mut |_| {}).unwrap();

        let kinds: Vec<(&str, &str)> = preview
            .members
            .iter()
            .map(|member| match member {
                MemberPreflight::Local { entry, .. } => ("local", entry.as_str()),
                MemberPreflight::Deferred { entry } => ("deferred", entry.as_str()),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("local", "services/api"),
                ("deferred", "https://github.com/example/remote"),
                ("deferred", "file:///nonexistent/managed.git"),
                ("local", "worker"),
                ("deferred", "owner/name"),
            ]
        );
        let MemberPreflight::Local { path, report, .. } = &preview.members[0] else {
            unreachable!()
        };
        assert_eq!(
            std::path::PathBuf::from(path),
            crate::paths::canonicalize(system.join("services/api")).unwrap()
        );
        assert!(
            report
                .languages
                .iter()
                .any(|lang| lang.language == "TypeScript"
                    && lang.adapter.as_deref() == Some("t0.adapter-ts")),
            "{report:?}"
        );
        assert!(
            report.frameworks.contains(&"React".to_string()),
            "{report:?}"
        );
        assert_eq!(report.unsupported.len(), 1, "the pending inline eval");
        let MemberPreflight::Local { report, .. } = &preview.members[3] else {
            unreachable!()
        };
        assert!(
            report
                .languages
                .iter()
                .any(|lang| lang.language == "Go"
                    && lang.adapter.as_deref() == Some("t0.adapter-go"))
        );

        assert!(state.sources.lock().unwrap().list().unwrap().is_empty());
        assert!(state.findings.lock().unwrap().list().unwrap().is_empty());
        assert_eq!(graph_nodes(&state), 0);
    }

    #[test]
    fn system_preflight_reads_a_registered_member_under_its_guard() {
        // AC-0706: a member already registered is resolved through the
        // registered-source guard (one checkout access); an unregistered
        // member is read in place and stays unregistered.
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        system_fixture(&system);
        let manifest = write_manifest(&system, &["services/api", "worker"]);
        let app = crate::tests::preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<crate::AppState>();
        let runs = app.state::<crate::PreflightRuns>();
        let registered = crate::register_local_source(&state, &system.join("worker")).unwrap();
        let before = state.sources.lock().unwrap().checkout_access_attempts();

        let token = runs.begin().unwrap();
        let preview =
            preflight_system_blocking(&manifest, &handle, &state, &token, &mut |_| {}).unwrap();
        assert_eq!(preview.members.len(), 2);
        assert_eq!(
            state.sources.lock().unwrap().checkout_access_attempts() - before,
            1,
            "only the registered member goes through the access guard"
        );
        let sources = state.sources.lock().unwrap().list().unwrap();
        assert_eq!(
            sources.iter().map(|s| &s.source_id).collect::<Vec<_>>(),
            vec![&registered.source_id],
            "the preview registers nothing new"
        );
        assert!(state.findings.lock().unwrap().list().unwrap().is_empty());
    }

    #[test]
    fn system_preflight_cancel_and_failure_return_no_partial_result() {
        // AC-0707: a cancel or supersession mid-preview, or a later member
        // that cannot be read, yields an error and no members at all.
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        system_fixture(&system);
        let manifest = write_manifest(&system, &["services/api", "worker"]);
        let app = crate::tests::preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<crate::AppState>();
        let runs = app.state::<crate::PreflightRuns>();

        // Cancelled while reading the second member's file.
        let token = runs.begin().unwrap();
        let mut seen = Vec::new();
        let result = preflight_system_blocking(&manifest, &handle, &state, &token, &mut |step| {
            if step.total.is_none() {
                return;
            }
            seen.push(step.path.to_string());
            if step.path.starts_with("worker/") {
                runs.cancel().unwrap();
            }
        });
        assert_eq!(result.unwrap_err(), crate::PREFLIGHT_CANCELLED);
        assert!(seen.iter().any(|path| path.starts_with("services/api/")));
        assert!(seen.iter().any(|path| path == "worker/main.go"), "{seen:?}");

        // A newer preflight supersedes the preview in flight.
        let preview = runs.begin().unwrap();
        let _newer = runs.begin().unwrap();
        assert_eq!(
            preflight_system_blocking(&manifest, &handle, &state, &preview, &mut |_| {})
                .unwrap_err(),
            crate::PREFLIGHT_CANCELLED
        );

        // A missing later member fails the preview after the first scanned.
        let broken = write_manifest(&system, &["services/api", "missing"]);
        let token = runs.begin().unwrap();
        let error =
            preflight_system_blocking(&broken, &handle, &state, &token, &mut |_| {}).unwrap_err();
        assert!(error.starts_with("missing: "), "{error}");
        assert!(state.sources.lock().unwrap().list().unwrap().is_empty());
        assert!(state.findings.lock().unwrap().list().unwrap().is_empty());
    }
}
