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
    write_export_file_with(path, content, |file, bytes| file.write_all(bytes))
}

/// [`write_export_file`] with the byte copy injectable, so tests can fail it
/// part-way.
fn write_export_file_with(
    path: &Path,
    content: &str,
    write: impl FnOnce(&mut std::fs::File, &[u8]) -> std::io::Result<()>,
) -> Result<(), String> {
    let fail = |err: std::io::Error| format!("Failed to write export file: {err}");
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let (temp, mut file) = create_sibling(parent).map_err(fail)?;
    let written = write(&mut file, content.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&temp, path)
        });
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(fail(err));
    }
    Ok(())
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
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
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
        let err = write_export_file_with(&target, "new export content", |file, bytes| {
            file.write_all(&bytes[..4])?;
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
}
