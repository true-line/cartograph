//! Native save-panel export command and pure validation/write helpers (#729).

use std::io::Write;
use std::path::Path;

/// Fallback file name used when a suggested export name has no usable basename.
pub const FALLBACK_EXPORT_NAME: &str = "cartograph-export";

/// Maximum UTF-8 byte length for a sanitised suggested file name.
pub const MAX_EXPORT_NAME_BYTES: usize = 255;

/// Sanitise a suggested file name: keep only the basename after any `/` or `\`,
/// strip control characters, cap at [`MAX_EXPORT_NAME_BYTES`] on a UTF-8
/// character boundary, and fall back to [`FALLBACK_EXPORT_NAME`].
pub fn sanitize_export_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    let mut end = cleaned.len().min(MAX_EXPORT_NAME_BYTES);
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = cleaned[..end].trim();
    if truncated.is_empty() || truncated == "." || truncated == ".." {
        return FALLBACK_EXPORT_NAME.to_string();
    }
    truncated.to_string()
}

/// Write `content` to `path`, returning a short error message that never
/// includes `content`. There is no size cap: a complete spec bundle is
/// exported untruncated (AC-0218), as the browser download path did.
///
/// The bytes go to a new sibling file that is flushed and then renamed over
/// `path`, so a failed or partial write leaves any existing file at `path`
/// exactly as it was.
pub fn write_export_file(path: &Path, content: &str) -> Result<(), String> {
    write_export_with(path, |file| file.write_all(content.as_bytes()))
}

/// [`write_export_file`] with the bytes produced by `write`, so a large
/// export can be streamed into the file instead of built as one string
/// (#1582), and tests can fail it part-way. The error never includes what
/// `write` wrote.
pub(crate) fn write_export_with(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> Result<(), String> {
    let fail = |err: std::io::Error| format!("Failed to write export file: {err}");
    let (target, existing) = export_target(path).map_err(fail)?;
    let parent = match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let (temp, mut file) = create_sibling(parent).map_err(fail)?;
    let written = write(&mut file)
        .and_then(|()| file.sync_all())
        .and_then(|()| match existing {
            Some(permissions) => file.set_permissions(permissions),
            None => Ok(()),
        })
        .and_then(|()| {
            drop(file);
            std::fs::rename(&temp, &target)
        });
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(fail(err));
    }
    Ok(())
}

/// The file an export to `path` replaces, and its permissions if it exists.
/// Replacing a symlink would swap the link for a file, so a symlink resolves
/// to its target, as the previous in-place write did. An existing
/// destination keeps its permissions; a new export stays owner-only (the
/// temp file's mode) rather than widening to the umask.
fn export_target(
    path: &Path,
) -> std::io::Result<(std::path::PathBuf, Option<std::fs::Permissions>)> {
    let target = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let existing = std::fs::metadata(&target)
        .ok()
        .map(|meta| meta.permissions());
    Ok((target, existing))
}

/// Put a finished export, already written to the temp file `file`, at
/// `path` with the same atomic replace as [`write_export_with`]. The temp
/// file is renamed into place, so the export is never copied; only when
/// `path` is on another filesystem is it copied there through
/// [`write_export_with`] (#2215).
pub(crate) fn move_export_into(path: &Path, file: tempfile::NamedTempFile) -> Result<(), String> {
    move_export_with(path, file, |temp, target| temp.persist(target).map(drop))
}

fn move_export_with(
    path: &Path,
    file: tempfile::NamedTempFile,
    rename: impl FnOnce(tempfile::NamedTempFile, &Path) -> Result<(), tempfile::PersistError>,
) -> Result<(), String> {
    let fail = |err: std::io::Error| format!("Failed to write export file: {err}");
    let (target, existing) = export_target(path).map_err(fail)?;
    file.as_file().sync_all().map_err(fail)?;
    if let Some(permissions) = existing {
        file.as_file().set_permissions(permissions).map_err(fail)?;
    }
    match rename(file, &target) {
        Ok(()) => Ok(()),
        Err(err) if err.error.kind() == std::io::ErrorKind::CrossesDevices => {
            let mut file = err.file;
            std::io::Seek::rewind(&mut file).map_err(fail)?;
            write_export_with(path, |out| std::io::copy(&mut file, out).map(drop))
        }
        Err(err) => Err(fail(err.error)),
    }
}

/// A new, exclusively created temporary file next to the export target.
/// The name is fixed-length, so a long export name never exceeds the
/// platform's file-name limit.
fn create_sibling(parent: &Path) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    loop {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp = parent.join(format!(
            ".{FALLBACK_EXPORT_NAME}-{}-{n}.partial",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // Owner-only from creation, so the content is never briefly readable
        // by others before its final permissions are applied.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
}

/// Show a native save panel with the sanitised suggested file name and write
/// `content` if the user confirms. Returns `Ok(false)` when cancelled.
#[tauri::command]
pub(crate) async fn save_export_file(
    suggested_name: String,
    content: String,
) -> Result<bool, String> {
    let file_name = sanitize_export_name(&suggested_name);
    let Some(handle) = rfd::AsyncFileDialog::new()
        .set_file_name(&file_name)
        .save_file()
        .await
    else {
        return Ok(false);
    };
    let path = handle.path().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || write_export_file(&path, &content))
        .await
        .map_err(|_| "Failed to write export file: worker failed".to_string())??;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Seek;

    #[test]
    fn sanitizes_basename_and_strips_path_separators() {
        assert_eq!(
            sanitize_export_name("cartograph-spec-best-effort.json"),
            "cartograph-spec-best-effort.json"
        );
        assert_eq!(sanitize_export_name("../../etc/passwd"), "passwd");
        assert_eq!(
            sanitize_export_name(r"C:\Users\dev\exports\packet.md"),
            "packet.md"
        );
        assert_eq!(
            sanitize_export_name("nested/path\\bundle.json"),
            "bundle.json"
        );
        assert_eq!(sanitize_export_name("bad\0name\n.json"), "badname.json");
    }

    #[test]
    fn falls_back_when_name_is_empty_or_dot_segment() {
        for raw in ["", "   ", ".", "..", "/", "\\", "dir/", "../", "\0\r\n"] {
            assert_eq!(
                sanitize_export_name(raw),
                FALLBACK_EXPORT_NAME,
                "expected fallback for {raw:?}"
            );
        }
    }

    #[test]
    fn bounds_name_to_255_bytes_on_utf8_boundary() {
        let exact = "a".repeat(MAX_EXPORT_NAME_BYTES);
        assert_eq!(sanitize_export_name(&exact), exact);

        let over = "b".repeat(MAX_EXPORT_NAME_BYTES + 50);
        let sanitized = sanitize_export_name(&over);
        assert_eq!(sanitized.len(), MAX_EXPORT_NAME_BYTES);
        assert!(sanitized.chars().all(|c| c == 'b'));

        let multibyte = "é".repeat(200);
        let sanitized_mb = sanitize_export_name(&multibyte);
        assert!(sanitized_mb.len() <= MAX_EXPORT_NAME_BYTES);
        assert_eq!(sanitized_mb.len(), 254);

        let emoji = "🚀".repeat(100);
        let sanitized_emoji = sanitize_export_name(&emoji);
        assert!(sanitized_emoji.len() <= MAX_EXPORT_NAME_BYTES);
        assert_eq!(sanitized_emoji.len(), 252);
    }

    #[test]
    fn writes_export_file_and_never_includes_content_in_write_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("cartograph-spec-verified-only.json");
        let payload = "{\"mode\":\"verified-only\"}";
        write_export_file(&target, payload).expect("write succeeds");
        assert_eq!(
            std::fs::read_to_string(&target).expect("read back"),
            payload
        );

        let secret = "SENSITIVE_EXPORT_CONTENT_DO_NOT_LEAK";
        let err = write_export_file(dir.path(), secret).expect_err("writing to directory fails");
        assert!(!err.contains(secret));
        assert!(err.starts_with("Failed to write export file:"));
    }

    #[test]
    fn a_failed_write_leaves_the_existing_export_untouched() {
        // #736 review: a partial write must never truncate the file the user
        // chose to overwrite, and must leave no temporary file behind.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("cartograph-spec-best-effort.json");
        std::fs::write(&target, "previous export").expect("seed");
        let err = write_export_with(&target, |file| {
            file.write_all(&b"new export content"[..4])?;
            Err(std::io::Error::other("disk full"))
        })
        .expect_err("injected failure");
        assert!(err.starts_with("Failed to write export file:"));
        assert!(!err.contains("new export content"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "previous export");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");

        write_export_file(&target, "new export content").expect("replace");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "new export content"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replacing_an_export_keeps_its_permissions_and_follows_symlinks() {
        // #743 review: the atomic replace must not widen a restrictive mode,
        // and a symlinked destination is written through, not replaced.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("private-export.json");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_export_file(&target, "new").unwrap();
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let fresh = dir.path().join("fresh-export.json");
        write_export_file(&fresh, "new").unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o077,
            0
        );

        let link = dir.path().join("linked-export.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_export_file(&link, "through link").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "through link");
    }

    /// A temp file holding `text`, as the bundle stream leaves it.
    #[cfg(unix)]
    fn finished_temp(text: &str) -> tempfile::NamedTempFile {
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        temp.write_all(text.as_bytes()).unwrap();
        temp
    }

    // AC-1213 (T-1213): a finished export is renamed into place, replacing
    // the destination atomically, keeping its permissions and following a
    // symlink; across filesystems it is copied there instead, and its temp
    // file is removed either way.
    #[cfg(unix)]
    #[test]
    fn a_finished_export_is_moved_into_place_or_copied_across_filesystems() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("bundle.json");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let temp = finished_temp("moved");
        let temp_path = temp.path().to_path_buf();
        move_export_into(&target, temp).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "moved");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert!(!temp_path.exists());

        let fresh = dir.path().join("fresh.json");
        move_export_into(&fresh, finished_temp("fresh")).unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o077,
            0
        );

        let link = dir.path().join("linked.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        move_export_into(&link, finished_temp("through link")).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "through link");

        let mut temp = finished_temp("copied");
        temp.seek(std::io::SeekFrom::End(0)).unwrap();
        let temp_path = temp.path().to_path_buf();
        move_export_with(&target, temp, |file, _| {
            Err(tempfile::PersistError {
                error: std::io::ErrorKind::CrossesDevices.into(),
                file,
            })
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "copied");
        assert!(!temp_path.exists());

        let error = move_export_with(&target, finished_temp("refused"), |file, _| {
            Err(tempfile::PersistError {
                error: std::io::ErrorKind::PermissionDenied.into(),
                file,
            })
        })
        .unwrap_err();
        assert!(error.starts_with("Failed to write export file:"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "copied");
    }
}
