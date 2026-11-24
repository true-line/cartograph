//! Go programs (#2019): a `func main()` in a file of `package main` is a
//! process start. It enters the `main` function when the Go adapter
//! emitted it, and the file otherwise.
//!
//! The kind is read from the file: a program that serves HTTP
//! (`ListenAndServe`, `ListenAndServeTLS`, `http.Serve(`) is an HTTP
//! server; one under an operator directory or `cmd/` that does not is a
//! command a person runs; any other is a process. Test, example and
//! end-to-end trees, and a file a `//go:build ignore` line excludes, start
//! nothing.

use super::{Kind, OPERATOR_DIRS, Start, Symbols, Target};
use core_prov::ConfidenceTier;

/// Directories whose programs are not part of the running system.
const NOT_SYSTEM_DIRS: &[&str] = &["e2e", "testdata", "examples", "example", "test", "tests"];

/// Whether `rel` is a Go file this reader considers.
pub(super) fn candidate(rel: &str) -> bool {
    rel.ends_with(".go")
        && !rel.ends_with("_test.go")
        && !rel.contains("vendor/")
        && !rel
            .split('/')
            .rev()
            .skip(1)
            .any(|dir| NOT_SYSTEM_DIRS.contains(&dir))
}

/// The start a Go file declares: none unless it is `package main` with a
/// top-level `func main()`.
pub(super) fn go_start(repo: &str, rel: &str, text: &str, symbols: &Symbols) -> Option<Start> {
    if text.lines().any(|line| {
        let line = line.trim_end();
        line.starts_with("//go:build") && line.contains("ignore")
    }) {
        return None;
    }
    // Comments and string literals state no declarations.
    let code = code_only(text);
    let mut package_main = false;
    let mut main_at = None;
    let mut offset = 0u64;
    for line in code.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if trimmed == "package main" || trimmed.starts_with("package main ") {
            package_main = true;
        }
        if main_at.is_none() && trimmed.starts_with("func main()") {
            main_at = Some((offset, offset + trimmed.len() as u64));
        }
        offset += line.len() as u64;
    }
    let span = main_at.filter(|_| package_main)?;
    let serves = ["ListenAndServe(", "ListenAndServeTLS(", "http.Serve("]
        .iter()
        .any(|call| code.contains(call));
    let dirs: Vec<&str> = rel.split('/').collect();
    let dirs = &dirs[..dirs.len().saturating_sub(1)];
    let operator = dirs
        .iter()
        .any(|dir| OPERATOR_DIRS.contains(dir) || *dir == "cmd");
    let kind = if serves {
        "http-server"
    } else if operator {
        "command"
    } else {
        "process"
    };
    let proven = symbols.contains(&format!("sym:{repo}@{rel}#main"));
    Some(Start {
        declared_in: rel.to_string(),
        span,
        key: "main".into(),
        how: "func main".into(),
        command: None,
        kind: Kind {
            kind,
            evidence: None,
        },
        port: None,
        extra: Vec::new(),
        extra_spans: Vec::new(),
        target: Target::File {
            rel: rel.to_string(),
            function: proven.then(|| "main".to_string()),
            confidence: ConfidenceTier::Confirmed,
            marker: None,
        },
    })
}

/// `text` with every comment and string or rune literal blanked to spaces
/// (newlines kept), so byte offsets stay the source's.
fn code_only(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'`' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'`' {
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
            }
            quote @ (b'"' | b'\'') => {
                i += 1;
                while i < bytes.len() && bytes[i] != quote && bytes[i] != b'\n' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
            }
            _ => {
                i += 1;
                continue;
            }
        }
        blank(&mut out, start, i);
    }
    // Only ASCII bytes were replaced, so the text stays UTF-8.
    String::from_utf8(out).unwrap_or_default()
}
