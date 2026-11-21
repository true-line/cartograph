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

use super::{Kind, OPERATOR_DIRS, Start, Target};
use core_prov::ConfidenceTier;
use std::collections::BTreeSet;

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
pub(super) fn go_start(
    repo: &str,
    rel: &str,
    text: &str,
    symbols: &BTreeSet<String>,
) -> Option<Start> {
    let mut package_main = false;
    let mut main_at = None;
    let mut offset = 0u64;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if trimmed.starts_with("//go:build") && trimmed.contains("ignore") {
            return None;
        }
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
        .any(|call| text.contains(call));
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
