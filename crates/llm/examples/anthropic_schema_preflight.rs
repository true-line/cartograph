//! Owner-run hosted schema preflight (#1709 / #1710). It checks that the
//! Anthropic Messages API compiles each bounded analyst response schema as
//! `output_config.format`, before the one authorized calibration run.
//!
//! It sends ONLY each projected schema plus a fixed one-word message, never
//! source code, ledger content or prompts.
//!
//!   cargo run -p llm --example anthropic_schema_preflight -- <schema-dir>            # offline: project and count only
//!   ANTHROPIC_API_KEY=… cargo run -p llm --example anthropic_schema_preflight -- <schema-dir> --send
//!
//! Offline mode also reports whether OpenAI strict mode can represent each
//! schema (the GPT frontier path). It sends nothing to OpenAI.
//!
//! The key is read from the environment and is never printed. Output is one
//! line per schema: its name, the HTTP status, and the API error message on
//! failure.
use serde_json::{Value, json};
use std::path::PathBuf;

const MODEL: &str = "claude-opus-5-5";
const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
// Documented combined limits for JSON output schemas.
const MAX_OPTIONAL: usize = 24;
const MAX_UNION_PARAMETERS: usize = 16;

fn counts(node: &Value, optional: &mut usize, unions: &mut usize) {
    match node {
        Value::Object(map) => {
            if let Some(Value::Object(properties)) = map.get("properties") {
                let required: Vec<&str> = map
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                *optional += properties
                    .keys()
                    .filter(|key| !required.contains(&key.as_str()))
                    .count();
            }
            if map.contains_key("anyOf") || map.get("type").is_some_and(Value::is_array) {
                *unions += 1;
            }
            map.values()
                .for_each(|value| counts(value, optional, unions));
        }
        Value::Array(items) => items
            .iter()
            .for_each(|value| counts(value, optional, unions)),
        _ => {}
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: <schema-dir> [--send]"));
    let send = args.next().as_deref() == Some("--send");
    let key = send.then(|| std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY unset"));
    let client = send.then(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(240))
            .build()
            .expect("client")
    });
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("schema dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    let mut failures = 0;
    for path in paths {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let raw = std::fs::read_to_string(&path).expect("read schema");
        let Some(schema) = llm::bounded::anthropic_wire_schema(&raw) else {
            println!("{name}: NOT REPRESENTABLE (fails closed before sending)");
            failures += 1;
            continue;
        };
        let (mut optional, mut unions) = (0, 0);
        counts(&schema, &mut optional, &mut unions);
        let wire = schema.to_string().len();
        let over = optional > MAX_OPTIONAL || unions > MAX_UNION_PARAMETERS;
        let strict = llm::bounded::openai_cloud_wire_schema(&raw).is_some();
        print!(
            "{name}: wire {wire} bytes, optional {optional}/{MAX_OPTIONAL}, union params {unions}/{MAX_UNION_PARAMETERS}{}, openai strict {}",
            if over { " OVER DOCUMENTED LIMIT" } else { "" },
            if strict { "ok" } else { "NOT REPRESENTABLE" }
        );
        failures += usize::from(!strict);
        let (Some(client), Some(key)) = (&client, &key) else {
            println!();
            failures += usize::from(over);
            continue;
        };
        let body = json!({
            "model": MODEL,
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "ok"}],
            "output_config": {"effort": "low", "format": {"type": "json_schema", "schema": schema}},
        });
        match client
            .post(ENDPOINT)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
        {
            Ok(response) => {
                let status = response.status();
                let text = response.text().unwrap_or_default();
                if status.is_success() {
                    println!(" -> {status} accepted");
                } else {
                    failures += 1;
                    let message = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
                        .unwrap_or_else(|| "unparsed error body".into());
                    println!(" -> {status} REJECTED: {message}");
                }
            }
            Err(error) => {
                failures += 1;
                println!(" -> transport error: {error}");
            }
        }
    }
    if failures > 0 {
        std::process::exit(1);
    }
}
