//! Explicit new-manifest preview/save. No graph, source or provider access.
use ingest::manifest_draft::ManifestDraft;
use std::{io::Write, path::Path};

#[tauri::command]
pub(crate) fn preview_system_manifest(draft: ManifestDraft) -> Result<String, String> {
    draft.render()
}

fn write_new_manifest(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let fail = |_: std::io::Error| {
        "Could not save the new manifest; choose an unused writable path.".to_string()
    };
    // NamedTempFile creates owner-only on Unix. Never rename over an existing
    // destination: persist_noclobber also refuses an existing symlink.
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(fail)?;
    temp.write_all(content.as_bytes()).map_err(fail)?;
    temp.as_file().sync_all().map_err(fail)?;
    temp.persist_noclobber(path)
        .map_err(|err| fail(err.error))?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn save_system_manifest(draft: ManifestDraft) -> Result<Option<String>, String> {
    // Revalidate on save; a frontend preview is never authority to write bytes.
    let content = draft.render()?;
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_file_name(ingest::manifest::MANIFEST_NAME)
        .add_filter("System manifest", &["toml"])
        .save_file()
        .await
    else {
        return Ok(None);
    };
    let path = handle.path().to_path_buf();
    let selected = path
        .to_str()
        .ok_or("Selected path is not valid Unicode.")?
        .to_owned();
    tauri::async_runtime::spawn_blocking(move || write_new_manifest(&path, &content))
        .await
        .map_err(|_| "Manifest save worker failed.".to_string())??;
    Ok(Some(selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0002: new-file publication cannot erase advanced author input.
    #[test]
    fn manifest_save_never_overwrites_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cartograph.system.toml");
        write_new_manifest(&path, "original").unwrap();
        let err = write_new_manifest(&path, "secret replacement").unwrap_err();
        assert!(!err.contains("secret"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(write_new_manifest(&dir.path().join("missing/file"), "secret").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn manifest_save_refuses_symlinks_including_dangling_links() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("manifest");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(write_new_manifest(&link, "new").is_err());
        assert!(!target.exists());
        std::fs::write(&target, "original").unwrap();
        assert!(write_new_manifest(&link, "new").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}
