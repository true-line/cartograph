//! Cloudflare Worker configs (`wrangler.toml`, `wrangler.json`,
//! `wrangler.jsonc`) as entry points (#2065).
//!
//! A config's `main` names the module Cloudflare runs on each request; it
//! resolves against the config's directory. The worker `name` and the first
//! route or custom domain are recorded as stated, with every `vars` value
//! that is an http(s) URL (the addresses the worker is configured to call).
//! A main module that reads a known webhook signature header names the
//! service that sends it requests. A config with no `main`, or a `main`
//! that is not a repository file, is a gap.

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
    let mut extra: Vec<(&'static str, serde_json::Value)> = Vec::new();
    if let Some(worker) = doc["name"].as_str() {
        extra.push(("worker", worker.into()));
    }
    let route = doc["routes"]
        .as_array()
        .and_then(|routes| routes.first())
        .and_then(|route| route.as_str().or_else(|| route["pattern"].as_str()))
        .or_else(|| doc["route"].as_str())
        .or_else(|| doc["route"]["pattern"].as_str());
    if let Some(route) = route {
        extra.push(("route", route.into()));
    }
    let urls: serde_json::Map<String, serde_json::Value> = doc["vars"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, value)| {
            value
                .as_str()
                .is_some_and(|v| v.starts_with("https://") || v.starts_with("http://"))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if !urls.is_empty() {
        extra.push(("configured_urls", urls.into()));
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
        extra_spans: Vec::new(),
        target,
    })
}

/// Signature headers only one service's webhooks carry, by that service.
const WEBHOOK_HEADERS: &[(&str, &str)] = &[
    ("x-hub-signature-256", "GitHub"),
    ("x-github-event", "GitHub"),
    ("x-gitlab-token", "GitLab"),
    ("x-gitlab-event", "GitLab"),
    ("stripe-signature", "Stripe"),
    ("x-slack-signature", "Slack"),
    ("x-shopify-hmac-sha256", "Shopify"),
    ("x-twilio-signature", "Twilio"),
];

/// Record the service whose webhooks the worker's main module reads, from
/// the first quoted signature header in it (case-insensitive), with its span.
pub(super) fn webhook_sender(start: &mut Start, module: &str, code: &str) {
    let lower = code.to_ascii_lowercase();
    let found = WEBHOOK_HEADERS
        .iter()
        .filter_map(|(header, sender)| {
            ['"', '\'', '`']
                .iter()
                .filter_map(|q| lower.find(&format!("{q}{header}{q}")))
                .min()
                .map(|at| (at + 1, header.len(), *sender))
        })
        .min();
    if let Some((at, len, sender)) = found {
        start.extra.push(("webhook_from", sender.into()));
        start
            .extra_spans
            .push((module.to_string(), (at as u64, (at + len) as u64)));
    }
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
