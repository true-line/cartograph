//! Cloudflare Worker configs (`wrangler.toml`, `wrangler.json`,
//! `wrangler.jsonc`) as entry points (#2065).
//!
//! A config's `main` names the module Cloudflare runs on each request; it
//! resolves against the config's directory. The worker `name` and the first
//! route or custom domain are recorded as stated. A config with no `main`,
//! or a `main` that is not a repository file, is a gap.

use super::*;

/// Whether a file is a Wrangler config.
pub(super) fn config(name: &str) -> bool {
    matches!(name, "wrangler.toml" | "wrangler.json" | "wrangler.jsonc")
}

pub(super) fn worker_start(
    rel: &str,
    name: &str,
    text: &str,
    files: &BTreeSet<String>,
) -> Option<Start> {
    let doc: serde_json::Value = if name.ends_with(".toml") {
        let table: toml::Table = toml::from_str(text).ok()?;
        serde_json::to_value(table).ok()?
    } else {
        serde_json::from_str(&strip_jsonc(text)).ok()?
    };
    let dir = rel
        .rsplit_once('/')
        .map_or(String::new(), |(dir, _)| format!("{dir}/"));
    let main = doc["main"].as_str();
    let target = match main {
        Some(main) => {
            let joined = format!("{dir}{}", main.trim_start_matches("./"));
            if files.contains(&joined) {
                Target::File {
                    rel: joined,
                    function: None,
                    confidence: ConfidenceTier::Confirmed,
                    marker: None,
                }
            } else {
                Target::Gap(format!("worker `main` `{main}` is not a repository file"))
            }
        }
        None => Target::Gap("worker config names no `main` module".into()),
    };
    let mut extra = Vec::new();
    if let Some(worker) = doc["name"].as_str() {
        extra.push(("worker", worker.to_string()));
    }
    let route = doc["routes"]
        .as_array()
        .and_then(|routes| routes.first())
        .and_then(|route| route.as_str().or_else(|| route["pattern"].as_str()))
        .or_else(|| doc["route"].as_str())
        .or_else(|| doc["route"]["pattern"].as_str());
    if let Some(route) = route {
        extra.push(("route", route.to_string()));
    }
    let span = main.map_or((0, text.len().max(1) as u64), |_| key_span(text, "main"));
    Some(Start {
        declared_in: rel.to_string(),
        span,
        key: "worker".into(),
        how: "Cloudflare Worker".into(),
        command: None,
        kind: Kind {
            kind: "edge-worker",
            evidence: None,
        },
        port: None,
        extra,
        target,
    })
}

/// JSON with comments and trailing commas as plain JSON. Strings are kept
/// as written, so `//` inside a URL survives.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for next in chars.by_ref() {
                    if last == '*' && next == '/' {
                        break;
                    }
                    last = next;
                }
            }
            _ => out.push(c),
        }
    }
    // Drop a comma that only whitespace separates from `}` or `]`.
    let mut cleaned = String::with_capacity(out.len());
    let bytes: Vec<char> = out.chars().collect();
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            cleaned.push(c);
            if c == '\\' {
                if let Some(next) = bytes.get(i + 1) {
                    cleaned.push(*next);
                    i += 1;
                }
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            cleaned.push(c);
        } else if c == ',' {
            let next = bytes[i + 1..].iter().find(|ch| !ch.is_whitespace());
            if !matches!(next, Some('}' | ']')) {
                cleaned.push(c);
            }
        } else {
            cleaned.push(c);
        }
        i += 1;
    }
    cleaned
}
