//! Model debug log (#1624): every model request and response, appended as
//! readable text to a local file that Help → Debug Viewer opens in the
//! system text editor. Entries arrive on the `llm` crate's writer thread,
//! never on a provider call, with bodies already capped at 1 MiB. Nothing here leaves the machine; request headers
//! (where API keys travel) are never recorded.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use llm::debug_log::{ModelExchange, Phase};
use serde_json::Value;

pub(crate) const FILE_NAME: &str = "model-debug.log";
pub(crate) const PREVIOUS_FILE_NAME: &str = "model-debug.previous.log";
/// Each file rotates at this size, so the two files together stay near 50 MB.
pub(crate) const ROTATE_BYTES: u64 = 25 * 1024 * 1024;

const HEADER: &str = "Cartograph model debug log\n\
Each model call appears twice, newest at the end: SENT when the request leaves and\n\
RECEIVED when the response or error arrives. A SENT with no RECEIVED is still in flight.\n\
Kept on this machine only. Older entries move to model-debug.previous.log at 25 MB.\n\n";

pub(crate) struct ModelDebugLog {
    path: PathBuf,
    previous: PathBuf,
    rotate_bytes: u64,
    file: Mutex<Option<File>>,
}

impl ModelDebugLog {
    pub(crate) fn new(dir: &Path) -> Self {
        Self::with_rotation(dir, ROTATE_BYTES)
    }

    pub(crate) fn with_rotation(dir: &Path, rotate_bytes: u64) -> Self {
        Self {
            path: dir.join(FILE_NAME),
            previous: dir.join(PREVIOUS_FILE_NAME),
            rotate_bytes,
            file: Mutex::new(None),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Create the file with its header if missing, so the viewer always has
    /// something to open.
    pub(crate) fn ensure_exists(&self) -> std::io::Result<()> {
        let mut slot = self
            .file
            .lock()
            .map_err(|_| std::io::Error::other("poisoned"))?;
        self.open(&mut slot)?;
        Ok(())
    }

    fn open<'a>(&self, slot: &'a mut Option<File>) -> std::io::Result<&'a mut File> {
        if slot.is_none() || !self.path.exists() {
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            if file.metadata()?.len() == 0 {
                file.write_all(HEADER.as_bytes())?;
            }
            *slot = Some(file);
        }
        Ok(slot.as_mut().expect("opened above"))
    }

    pub(crate) fn append(&self, exchange: &ModelExchange) -> std::io::Result<()> {
        let entry = format_entry(exchange);
        let mut slot = self
            .file
            .lock()
            .map_err(|_| std::io::Error::other("poisoned"))?;
        let file = self.open(&mut slot)?;
        file.write_all(entry.as_bytes())?;
        file.flush()?;
        if file.metadata()?.len() >= self.rotate_bytes {
            *slot = None;
            // Windows rename refuses an existing destination.
            match fs::remove_file(&self.previous) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
            fs::rename(&self.path, &self.previous)?;
        }
        Ok(())
    }

    /// Route every model exchange from the `llm` crate into this file.
    pub(crate) fn install(self: Arc<Self>) {
        llm::debug_log::set_sink(Some(Arc::new(move |exchange: &ModelExchange| {
            if let Err(error) = self.append(exchange) {
                eprintln!("cartograph: model debug log write failed: {error}");
            }
        })));
    }
}

/// Program and arguments that open a text file in the platform's plain
/// editor: TextEdit on macOS, Notepad on Windows, the default handler on Linux.
pub(crate) fn text_viewer(os: &str, path: &Path) -> Option<(&'static str, Vec<String>)> {
    let path = path.to_string_lossy().into_owned();
    match os {
        "macos" => Some(("open", vec!["-e".into(), path])),
        "windows" => Some(("notepad.exe", vec![path])),
        "linux" | "freebsd" | "netbsd" | "openbsd" | "dragonfly" => Some(("xdg-open", vec![path])),
        _ => None,
    }
}

pub(crate) fn format_entry(exchange: &ModelExchange) -> String {
    let mut out = match exchange.phase {
        Phase::Sent => format!(
            "==== #{} SENT  {}  {}  {} ====\n",
            exchange.call,
            utc_timestamp(exchange.at),
            exchange.path,
            exchange.target
        ),
        Phase::Received => format!(
            "==== #{} RECEIVED  {}  {:.2} s  {} ====\n",
            exchange.call,
            utc_timestamp(exchange.at),
            exchange.duration.as_secs_f64(),
            if exchange.error.is_some() {
                "ERROR"
            } else {
                "OK"
            }
        ),
    };
    if exchange.dropped_before > 0 {
        out.push_str(&format!(
            "[{} earlier entries dropped: debug log queue full]\n",
            exchange.dropped_before
        ));
    }
    if let Some(error) = &exchange.error {
        out.push_str(&format!("error: {error}\n"));
    }
    if let Some(request) = &exchange.request {
        out.push_str("--- request ---\n");
        out.push_str(&pretty(request));
        if let Some(messages) = serde_json::from_str::<Value>(request)
            .ok()
            .as_ref()
            .and_then(|value| value.get("messages"))
            .and_then(Value::as_array)
        {
            for message in messages {
                let role = message.get("role").and_then(Value::as_str).unwrap_or("?");
                if let Some(content) = message.get("content").and_then(Value::as_str) {
                    out.push_str(&format!("--- request message: {role} ---\n"));
                    out.push_str(&pretty(content));
                }
            }
        }
    }
    if let Some(response) = &exchange.response {
        out.push_str("--- response ---\n");
        out.push_str(&pretty(response));
        if let Some(text) = model_output(response) {
            out.push_str("--- model output ---\n");
            out.push_str(&pretty(&text));
        }
    }
    out.push('\n');
    out
}

/// The assistant text inside an Ollama, OpenAI-compatible or Anthropic body.
fn model_output(response: &str) -> Option<String> {
    let value: Value = serde_json::from_str(response).ok()?;
    if let Some(text) = value.pointer("/message/content").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
    {
        return Some(text.to_string());
    }
    let blocks = value.get("content").and_then(Value::as_array)?;
    let text: String = blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then_some(text)
}

/// Pretty-print JSON text; anything else is written as-is.
fn pretty(text: &str) -> String {
    let mut out = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.to_string());
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

pub(crate) fn utc_timestamp(time: SystemTime) -> String {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        duration.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sent(call: u64, request: &str) -> ModelExchange {
        ModelExchange {
            call,
            phase: Phase::Sent,
            at: UNIX_EPOCH + Duration::from_millis(1_793_605_065_123),
            duration: Duration::ZERO,
            path: "bounded",
            target: "http://127.0.0.1:11434/api/chat".into(),
            request: Some(request.into()),
            response: None,
            error: None,
            dropped_before: 0,
        }
    }

    fn received(call: u64, response: Option<&str>, error: Option<&str>) -> ModelExchange {
        ModelExchange {
            call,
            phase: Phase::Received,
            at: UNIX_EPOCH + Duration::from_millis(1_793_605_095_103),
            duration: Duration::from_millis(29_980),
            path: "bounded",
            target: "http://127.0.0.1:11434/api/chat".into(),
            request: None,
            response: response.map(Into::into),
            error: error.map(Into::into),
            dropped_before: 0,
        }
    }

    #[test]
    fn model_debug_entry_shows_exact_request_response_and_decoded_output() {
        // AC-0560: a SENT entry carries the exact request as it leaves and the
        // RECEIVED entry the exact response, with contents unpacked for reading.
        let request = r#"{"model":"qwen3:8b","messages":[{"role":"user","content":"{\"q\":\"aliases\"}"}],"options":{"num_predict":2048}}"#;
        let response = r#"{"model":"qwen3:8b","message":{"role":"assistant","content":"{\"type\":\"replay_query\",\"query_index\":2}"},"done":true,"done_reason":"stop","eval_count":141}"#;
        let out = format_entry(&sent(12, request));
        assert!(out.starts_with(
            "==== #12 SENT  2026-11-02T07:37:45.123Z  bounded  http://127.0.0.1:11434/api/chat ===="
        ));
        for part in [
            "--- request ---",
            "\"num_predict\": 2048",
            "--- request message: user ---",
            "\"q\": \"aliases\"",
        ] {
            assert!(out.contains(part), "missing {part}\n{out}");
        }
        assert!(!out.contains("--- response ---"));
        let back = format_entry(&received(12, Some(response), None));
        assert!(back.starts_with("==== #12 RECEIVED  2026-11-02T07:38:15.103Z  29.98 s  OK ===="));
        for part in [
            "--- response ---",
            "\"done_reason\": \"stop\"",
            "--- model output ---",
            "\"type\": \"replay_query\"",
        ] {
            assert!(back.contains(part), "missing {part}\n{back}");
        }
        assert!(!back.contains("--- request ---"));
        let failed = format_entry(&received(13, None, Some("HttpStatus")));
        assert!(failed.contains("#13 RECEIVED") && failed.contains("  ERROR ===="));
        assert!(failed.contains("error: HttpStatus"));
        assert!(!failed.contains("--- response ---"));
        let plain = format_entry(&sent(14, "SYSTEM:\nnot json"));
        assert!(plain.contains("--- request ---\nSYSTEM:\nnot json\n"));
    }

    #[test]
    fn model_debug_log_appends_with_header_and_rotates_to_one_previous_file() {
        // AC-0560: one header, entries appended in order, and at the size cap
        // the file moves to model-debug.previous.log (replacing any older one)
        // so disk use stays bounded at two files.
        let dir = tempfile::tempdir().unwrap();
        let log = ModelDebugLog::with_rotation(dir.path(), 4096);
        log.ensure_exists().unwrap();
        let first = fs::read_to_string(log.path()).unwrap();
        assert_eq!(first, HEADER);
        log.append(&sent(1, "{\"n\":1}")).unwrap();
        let text = fs::read_to_string(log.path()).unwrap();
        assert!(text.starts_with(HEADER) && text.contains("\"n\": 1"));
        let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(5000));
        log.append(&sent(2, &big)).unwrap();
        let previous = dir.path().join(PREVIOUS_FILE_NAME);
        assert!(fs::read_to_string(&previous).unwrap().contains("\"n\": 1"));
        assert!(!log.path().exists());
        log.append(&sent(3, "{\"n\":3}")).unwrap();
        let fresh = fs::read_to_string(log.path()).unwrap();
        assert!(fresh.starts_with(HEADER) && fresh.contains("\"n\": 3"));
        log.append(&sent(2, &big)).unwrap();
        let replaced = fs::read_to_string(&previous).unwrap();
        assert!(replaced.contains("\"n\": 3") && !replaced.contains("\"n\": 1"));
    }

    #[test]
    fn model_debug_viewer_uses_plain_text_editor_per_os() {
        // AC-0560: TextEdit on macOS, Notepad on Windows, default on Linux.
        let path = Path::new("/tmp/logs/model-debug.log");
        assert_eq!(
            text_viewer("macos", path),
            Some((
                "open",
                vec!["-e".into(), "/tmp/logs/model-debug.log".into()]
            ))
        );
        assert_eq!(
            text_viewer("windows", path).map(|(program, _)| program),
            Some("notepad.exe")
        );
        assert_eq!(
            text_viewer("linux", path).map(|(program, _)| program),
            Some("xdg-open")
        );
        assert_eq!(text_viewer("ios", path), None);
    }
}
