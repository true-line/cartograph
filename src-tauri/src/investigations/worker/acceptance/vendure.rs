//! Preparation only: real ingest of frozen source, with no provider or task.
//! This does not execute MT-H4-02's provider, review or native lifecycle stages.

use crate::*;
use serde::Serialize;
use std::path::Path;
use tauri::Manager;

#[path = "vendure/input.rs"]
mod input;
#[path = "vendure/measurement.rs"]
mod measurement;
#[path = "vendure/profile.rs"]
mod profile;
#[path = "vendure/report.rs"]
mod report;
#[cfg(test)]
#[path = "vendure/tests.rs"]
mod tests;

const MANIFEST_SHA: &str = "96d2595907c4b1ea05991c7f0f2460df2f7b75267c486d5ee3e96c3d6ae49b83";
const PROOF_SHA: &str = "22d1cb69a593d6cf87abf1207cac9a50e82e6df61181874f9ab2fbd79e30a763";
const COMMIT: &str = "2cc309deb5394af176314552ac7320a7b26a0104";

#[derive(Serialize)]
struct Preparation {
    schema_version: u32,
    status: &'static str,
    code_revision: String,
    source_root: std::path::PathBuf,
    native_config_sha256: String,
    input: input::VerifiedInput,
    profile: profile::NativeProfile,
    graph: report::GraphPreparation,
    ingest: IngestSummary,
    provider_calls: u32,
    remaining_stages: [&'static str; 5],
}

fn canonical_future(path: &Path) -> Result<std::path::PathBuf, String> {
    if !path.is_absolute() {
        return Err("preparation paths must be absolute".into());
    }
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("preparation paths must be normalized".into());
    }
    let mut ancestor = path;
    let mut tail = Vec::new();
    loop {
        match crate::paths::canonicalize(ancestor) {
            Ok(mut canonical) => {
                for component in tail.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tail.push(ancestor.file_name().ok_or("invalid preparation path")?);
                ancestor = ancestor.parent().ok_or("invalid preparation path")?;
            }
            Err(_) => return Err("preparation path unavailable".into()),
        }
    }
}

fn validate_paths(
    source: &Path,
    manifest: &Path,
    proof: &Path,
    config: &Path,
    profile: &profile::NativeProfile,
    output: &Path,
) -> Result<(), String> {
    let code = crate::paths::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("code root unavailable")?,
    )
    .map_err(|_| "code root unavailable")?;
    let profile_root = profile.data().parent().ok_or("invalid profile root")?;
    let roots = [source, profile_root, output, code.as_path()];
    let canonical = roots
        .iter()
        .map(|path| canonical_future(path))
        .collect::<Result<Vec<_>, _>>()?;
    for (index, root) in canonical.iter().enumerate() {
        if roots[index] != root {
            return Err("preparation roots must be canonical".into());
        }
        for other in &canonical[..index] {
            if root.starts_with(other) || other.starts_with(root) {
                return Err("preparation roots must be disjoint".into());
            }
        }
    }
    for artifact in [manifest, proof, config] {
        let actual =
            crate::paths::canonicalize(artifact).map_err(|_| "evaluator artifact unavailable")?;
        if artifact != actual || canonical[..3].iter().any(|root| actual.starts_with(root)) {
            return Err("evaluator artifacts must be external canonical files".into());
        }
    }
    Ok(())
}

fn open_directory(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
    }
    options
        .open(path)
        .map_err(|_| "output directory unavailable".into())
}

#[cfg(windows)]
fn directory_identity(file: &std::fs::File) -> Result<(u32, u32, u32), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) } == 0 {
        return Err("output identity unavailable".into());
    }
    Ok((
        information.dwVolumeSerialNumber,
        information.nFileIndexHigh,
        information.nFileIndexLow,
    ))
}

struct OutputDirectory {
    path: std::path::PathBuf,
    handle: std::fs::File,
}
impl OutputDirectory {
    fn pin(path: &Path) -> Result<Self, String> {
        let handle = open_directory(path)?;
        let pinned = Self {
            path: path.to_owned(),
            handle,
        };
        pinned.verify()?;
        Ok(pinned)
    }
    fn verify(&self) -> Result<(), String> {
        let current =
            std::fs::symlink_metadata(&self.path).map_err(|_| "output directory unavailable")?;
        let held = self
            .handle
            .metadata()
            .map_err(|_| "output directory unavailable")?;
        if !current.is_dir()
            || current.file_type().is_symlink()
            || !held.is_dir()
            || crate::paths::canonicalize(&self.path).map_err(|_| "output directory unavailable")?
                != self.path
        {
            return Err("output directory changed".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (current.dev(), current.ino()) != (held.dev(), held.ino()) {
                return Err("output directory changed".into());
            }
        }
        #[cfg(windows)]
        if directory_identity(&open_directory(&self.path)?)? != directory_identity(&self.handle)? {
            return Err("output directory changed".into());
        }
        #[cfg(not(any(unix, windows)))]
        if current
            .created()
            .map_err(|_| "output identity unavailable")?
            != held.created().map_err(|_| "output identity unavailable")?
        {
            return Err("output directory changed".into());
        }
        Ok(())
    }
    fn write(&self, name: &str, value: &impl Serialize) -> Result<(), String> {
        self.verify()?;
        // Portable pathname checks detect swaps at stage boundaries. They are
        // not an atomic openat capability against an actively racing writer.
        report::write_new(&self.path.join(name), value)?;
        self.verify()
    }
}
fn verify_config(config: &Path, expected_hash: &str) -> Result<(), String> {
    if profile::NativeProfile::read(config)?.1 != expected_hash {
        return Err("native configuration changed".into());
    }
    Ok(())
}

fn prepare(
    source: &Path,
    manifest: &Path,
    proof: &Path,
    config: &Path,
    revision: String,
    output: &Path,
    frozen_vendure: bool,
) -> Result<Preparation, String> {
    let input = input::verify(source, manifest, proof)?;
    if frozen_vendure
        && (input.repo != "vendurehq/vendure"
            || input.commit_sha != COMMIT
            || input.manifest_sha256 != MANIFEST_SHA
            || input.proof_sha256 != PROOF_SHA
            || input.files.len() != 17
            || input.files.values().map(|file| file.byte_len).sum::<u64>() != 270_721)
    {
        return Err("frozen Vendure input identity mismatch".into());
    }
    let (profile, native_config_sha256) = profile::NativeProfile::read(config)?;
    validate_paths(source, manifest, proof, config, &profile, output)?;
    verify_config(config, &native_config_sha256)?;
    profile.create_fresh(source)?;
    let app = profile.app()?;
    let state = app.state::<AppState>();
    let registered = register_local_source(&state, source)?;
    let operation = source_operation(&state, vec![(registered.clone(), false)])?;
    let (_, execution) = start_job(&state, &registered.ingest_job_kind())?;
    let ingest = run_ingest(&registered, &operation, &execution, app.handle(), &state)
        .map_err(|_| "production ingest failed; inspect retained job events".to_string())?;
    if input::verify(source, manifest, proof)? != input {
        return Err("source or input proof changed during production ingest".into());
    }
    validate_paths(source, manifest, proof, config, &profile, output)?;
    verify_config(config, &native_config_sha256)?;
    let graph = report::collect(&state, &registered, &input, profile.data())?;
    if input::verify(source, manifest, proof)? != input {
        return Err("source or input proof changed during preparation".into());
    }
    validate_paths(source, manifest, proof, config, &profile, output)?;
    verify_config(config, &native_config_sha256)?;
    Ok(Preparation {
        schema_version: 1,
        status: "captured_preparation_only",
        code_revision: revision,
        source_root: crate::paths::canonicalize(source).map_err(|_| "source root unavailable")?,
        native_config_sha256,
        input,
        profile,
        graph,
        ingest,
        provider_calls: 0,
        remaining_stages: [
            "discovery_contract_and_frozen_dispatch_review",
            "actual_provider_analyst_and_independent_auditor",
            "withheld_oracle_semantic_review_and_negative_cases",
            "native_process_restart_and_retained_source_reopen",
            "disposable_capture_forgetting",
        ],
    })
}

/// Manual, non-provider preparation of the original 17-file source closure.
/// Output directory is exclusive; retained databases remain in the explicit
/// native profile, ready for a later reviewed handoff without database copying.
#[test]
#[ignore = "manual frozen Vendure capture preparation; no provider calls"]
fn vendure_capture_preparation() {
    let required = |name| std::path::PathBuf::from(super::required(name));
    let source = required("CARTOGRAPH_VENDURE_SOURCE");
    let manifest = required("CARTOGRAPH_VENDURE_MANIFEST");
    let proof = required("CARTOGRAPH_VENDURE_PROOF");
    let config = required("CARTOGRAPH_VENDURE_NATIVE_CONFIG");
    let output = required("CARTOGRAPH_VENDURE_PREPARATION_OUTPUT");
    assert!(output.is_absolute());
    let source_root = crate::paths::canonicalize(&source).unwrap();
    let parent = crate::paths::canonicalize(output.parent().unwrap()).unwrap();
    assert_eq!(output.parent().unwrap(), parent);
    assert!(!output.starts_with(&source_root));
    let (profile, _) = profile::NativeProfile::read(&config).unwrap();
    validate_paths(&source, &manifest, &proof, &config, &profile, &output).unwrap();
    std::fs::create_dir(&output).expect("Preparation output must be new");
    let output_guard = OutputDirectory::pin(&output).unwrap();
    let config_hash = profile::NativeProfile::read(&config).unwrap().1;
    let revision = super::source_revision();
    match prepare(&source, &manifest, &proof, &config, revision, &output, true) {
        Ok(preparation) => {
            validate_paths(&source, &manifest, &proof, &config, &profile, &output).unwrap();
            verify_config(&config, &config_hash).unwrap();
            output_guard
                .write("preparation.json", &preparation)
                .unwrap()
        }
        Err(error) => {
            validate_paths(&source, &manifest, &proof, &config, &profile, &output).unwrap();
            verify_config(&config, &config_hash).unwrap();
            output_guard
                .write(
                    "preparation-failed.json",
                    &serde_json::json!({"schema_version":1,"status":"preparation_failed",
                    "provider_calls":0,"acceptance_passed":false,"reason":error}),
                )
                .unwrap();
            panic!("Vendure preparation failed; no acceptance or provider run occurred");
        }
    }
}
