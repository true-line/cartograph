//! One explicit disposable directory map, shared with a future native launch.
use super::*;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct NativeProfile {
    product_name: String,
    identifier: String,
    app: ProfileApp,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProfileApp {
    app_directories_override: Directories,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Directories {
    data: PathBuf,
    config: PathBuf,
    local_data: PathBuf,
    cache: PathBuf,
    log: PathBuf,
}

impl NativeProfile {
    pub(super) fn data(&self) -> &Path {
        &self.app.app_directories_override.data
    }
    pub(super) fn read(path: &Path) -> Result<(Self, String), String> {
        let bytes = input::read_bounded(path, 16 * 1024)?;
        let profile = agents::investigation::decode_record(
            std::str::from_utf8(&bytes).map_err(|_| "invalid profile")?,
        )
        .map_err(|_| "invalid explicit native profile")?;
        use sha2::{Digest, Sha256};
        Ok((profile, format!("{:x}", Sha256::digest(&bytes))))
    }

    pub(super) fn create_fresh(&self, source: &Path) -> Result<(), String> {
        let paths = &self.app.app_directories_override;
        let root = paths.data.parent().ok_or("invalid profile root")?;
        if !root.is_absolute()
            || root.parent().is_none()
            || root.to_string_lossy().len() > 4096
            || !self
                .identifier
                .starts_with("com.qwtm.cartograph.acceptance598.")
            || self.identifier.len() > 128
            || self.product_name.len() > 128
        {
            return Err("invalid disposable profile".into());
        }
        let source = crate::paths::canonicalize(source).map_err(|_| "invalid source root")?;
        if root.starts_with(&source) || source.starts_with(root) {
            return Err("profile and source must be separate".into());
        }
        let expected = ["data", "config", "local-data", "cache", "logs"];
        for (path, name) in self.paths().into_iter().zip(expected) {
            if path != root.join(name) {
                return Err("profile must use an explicit sibling directory map".into());
            }
        }
        if root.exists() {
            let metadata = std::fs::symlink_metadata(root).map_err(|_| "invalid profile root")?;
            if !metadata.is_dir()
                || std::fs::read_dir(root)
                    .map_err(|_| "invalid profile root")?
                    .next()
                    .is_some()
            {
                return Err("disposable profile must be empty".into());
            }
        } else {
            std::fs::create_dir(root).map_err(|_| "cannot create profile root")?;
        }
        if crate::paths::canonicalize(root).map_err(|_| "invalid profile root")? != root {
            return Err("profile root must be canonical".into());
        }
        for path in self.paths() {
            std::fs::create_dir(path).map_err(|_| "cannot create profile directory")?;
        }
        Ok(())
    }

    pub(super) fn verify_existing(&self) -> Result<(), String> {
        let root = self.data().parent().ok_or("invalid prepared profile")?;
        if !root.is_absolute()
            || root.parent().is_none()
            || !self
                .identifier
                .starts_with("com.qwtm.cartograph.acceptance598.")
            || self.identifier.len() > 128
            || self.product_name.len() > 128
        {
            return Err("invalid disposable prepared profile".into());
        }
        for (path, name) in
            self.paths()
                .into_iter()
                .zip(["data", "config", "local-data", "cache", "logs"])
        {
            if path != root.join(name)
                || !path.is_dir()
                || crate::paths::canonicalize(path).map_err(|_| "prepared profile missing")? != path
            {
                return Err("prepared profile directory map changed".into());
            }
        }
        Ok(())
    }

    pub(super) fn paths(&self) -> [&Path; 5] {
        let paths = &self.app.app_directories_override;
        [
            &paths.data,
            &paths.config,
            &paths.local_data,
            &paths.cache,
            &paths.log,
        ]
    }

    pub(super) fn app(&self) -> Result<tauri::App<tauri::test::MockRuntime>, String> {
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().identifier = self.identifier.clone();
        context.config_mut().app.app_directories_override = Some(
            serde_json::from_value(
                serde_json::to_value(&self.app.app_directories_override)
                    .map_err(|_| "invalid profile")?,
            )
            .map_err(|_| "invalid Tauri directory map")?,
        );
        let app = tauri::test::mock_builder()
            .build(context)
            .map_err(|_| "cannot build mock app")?;
        let resolved = [
            app.path().app_data_dir(),
            app.path().app_config_dir(),
            app.path().app_local_data_dir(),
            app.path().app_cache_dir(),
            app.path().app_log_dir(),
        ];
        for (actual, expected) in resolved.into_iter().zip(self.paths()) {
            if actual.map_err(|_| "cannot resolve profile")? != expected {
                return Err("Tauri and storage profile paths disagree".into());
            }
        }
        app.manage(crate::registered_source_tests::app_state(
            &self.app.app_directories_override.data,
        ));
        app.manage(crate::PreflightRuns::default());
        Ok(app)
    }
}
