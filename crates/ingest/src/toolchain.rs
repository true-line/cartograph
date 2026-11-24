//! Toolchain detection — config files as first-class evidence (#215,
//! AC-0096/AC-0097).
//!
//! One registry turns config files (`package.json`, `tsconfig.json`,
//! lockfiles, Dockerfiles, CI workflows, language manifests) into cited
//! graph facts: `Tool` nodes (`tool:{repo}@{name}`) with resolved settings
//! as props, `DEFINED_IN` edges to the config `File` node(s) that prove
//! them, and provenance citing the exact declaring span. [`detect_in_file`]
//! is the single detection function behind BOTH the graph extraction
//! ([`extract_dir`]) and Preflight's framework chips — the two surfaces can
//! never disagree because they quote the same detection.
//!
//! Redaction is fail-closed (AC-0097, same standard as the T1 state-JSON
//! redaction in AC-0009): `.env*` files contribute **key names only**,
//! plus the values of the keys an author allowlists (ADR-0075, AC-1021),
//! each cited by its line; for every other config only allowlisted setting
//! keys are stored, and any stored string that looks secret-shaped is
//! dropped. JS-authored configs
//! (`vite.config.ts`, `webpack.config.js`, …) are detected by presence
//! only — settings that live behind code surface as an explicit
//! `config-behind-code` Unsupported finding, never as evaluated facts.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::BTreeMap;
use std::path::Path;

/// Extractor id recorded in provenance for every toolchain fact.
pub const TOOLCHAIN_EXTRACTOR_ID: &str = "t0.toolchain";

/// Source identity for one extraction run (same convention as the
/// language adapters' `SourceId`).
#[derive(Debug, Clone, Copy)]
pub struct SourceId<'a> {
    /// Repository identity (`owner/name` or `local/<name>`).
    pub repo: &'a str,
    /// Commit the tree was read at.
    pub commit: &'a str,
}

/// One tool proven by a config file, before graph-fact conversion.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedTool {
    /// Stable tool name — the `{name}` in `tool:{repo}@{name}`. Package
    /// names for dependency-proven tools (`react`, `vite`), the
    /// repo-relative config path for per-file configs (`tsconfig.json`,
    /// `.github/workflows/ci.yml`).
    pub name: String,
    /// Human display name (`React`, `TypeScript config`).
    pub display: String,
    /// Category chip: `framework`, `bundler`, `test-runner`,
    /// `package-manager`, `ci`, `container`, `linter`, `formatter`,
    /// `compiler-config`, `build-manifest`, `package-manifest`,
    /// `environment`, `language`.
    pub category: &'static str,
    /// Repo-relative path of the config file proving this tool.
    pub path: String,
    /// Byte span of the declaring evidence (the dependency line, the
    /// section header — not just "the file").
    pub byte_start: u64,
    /// Exclusive end of the declaring span.
    pub byte_end: u64,
    /// Resolved settings, already redacted (allowlisted keys only).
    pub settings: BTreeMap<String, serde_json::Value>,
    /// True when the config is authored in code and the settings cannot be
    /// read without evaluating it — presence-only detection.
    pub settings_behind_code: bool,
}

/// Everything one file contributes: tools for the graph, framework labels
/// for Preflight's chips, both from the same parse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Detection {
    /// Tools proven by this file.
    pub tools: Vec<DetectedTool>,
    /// Preflight framework chip labels (exact display strings).
    pub framework_labels: Vec<String>,
}

/// Toolchain graph facts for one tree.
#[derive(Debug, Clone, Default)]
pub struct ToolExtraction {
    /// `Tool` nodes plus the config `File` nodes that prove them.
    pub nodes: Vec<Node>,
    /// `DEFINED_IN` edges (tool → config file).
    pub edges: Vec<Edge>,
    /// Config files that contributed at least one detection.
    pub files: u64,
}

/// Dependency names (from `package.json`) that identify a known tool.
/// Order is irrelevant — lookups only. Extend freely; unknown deps are
/// simply not tools.
const KNOWN_JS_TOOLS: &[(&str, &str, &str)] = &[
    ("react", "React", "framework"),
    ("next", "Next.js", "framework"),
    ("express", "Express", "framework"),
    ("fastify", "Fastify", "framework"),
    ("vue", "Vue", "framework"),
    ("svelte", "Svelte", "framework"),
    ("@angular/core", "Angular", "framework"),
    ("vite", "Vite", "bundler"),
    ("webpack", "webpack", "bundler"),
    ("rollup", "Rollup", "bundler"),
    ("esbuild", "esbuild", "bundler"),
    ("jest", "Jest", "test-runner"),
    ("vitest", "Vitest", "test-runner"),
    ("mocha", "Mocha", "test-runner"),
    ("cypress", "Cypress", "test-runner"),
    ("@playwright/test", "Playwright", "test-runner"),
    ("eslint", "ESLint", "linter"),
    ("@biomejs/biome", "Biome", "linter"),
    ("prettier", "Prettier", "formatter"),
    ("typescript", "TypeScript", "language"),
];

/// Dependencies whose presence is also a Preflight framework chip. The
/// labels are the exact strings the chips have always shown.
/// Framework chip for a `manifest.json` carrying `manifest_version`.
pub const WEBEXTENSION_LABEL: &str = "WebExtension (Manifest V2/V3)";

const FRAMEWORK_LABELS: &[(&str, &str)] = &[
    ("react", "React"),
    ("express", "Express"),
    ("next", "Next.js"),
];

/// JS/TS-authored config file stems, with the tool they configure.
/// Matched against the file name minus its final extension, so
/// `vite.config.ts`, `vite.config.mjs`, … all hit `vite.config`.
const CODE_CONFIG_STEMS: &[(&str, &str, &str)] = &[
    ("vite.config", "Vite config", "bundler"),
    ("webpack.config", "webpack config", "bundler"),
    ("rollup.config", "Rollup config", "bundler"),
    ("next.config", "Next.js config", "framework"),
    ("babel.config", "Babel config", "language"),
    ("postcss.config", "PostCSS config", "language"),
    ("tailwind.config", "Tailwind config", "framework"),
    ("eslint.config", "ESLint config", "linter"),
    ("prettier.config", "Prettier config", "formatter"),
    ("jest.config", "Jest config", "test-runner"),
    ("vitest.config", "Vitest config", "test-runner"),
    ("svelte.config", "Svelte config", "framework"),
    ("astro.config", "Astro config", "framework"),
    ("nuxt.config", "Nuxt config", "framework"),
];

/// Lockfile names and the package manager each proves.
const LOCKFILES: &[(&str, &str, &str)] = &[
    ("package-lock.json", "npm", "npm"),
    ("yarn.lock", "yarn", "Yarn"),
    ("pnpm-lock.yaml", "pnpm", "pnpm"),
    ("bun.lock", "bun", "Bun"),
    ("bun.lockb", "bun", "Bun"),
    ("Cargo.lock", "cargo", "Cargo"),
    ("poetry.lock", "poetry", "Poetry"),
    ("uv.lock", "uv", "uv"),
];

/// True for dotenv files anywhere in the tree: `.env`, `.env.*`, `*.env`
/// (`example.env`, `test.env`) and the `env.example`-style templates. Their
/// facts carry **no evidence spans at all** (#216 review): an evidence span
/// — even a zero-length one — lets the evidence reader open a source window
/// around it and disclose the file's values, which would break the AC-0097
/// keys-only guarantee the settings themselves honor. A template is held to
/// the same rule: placeholder values are often real. The one exception is
/// the line of an allowlisted value (ADR-0075), and the evidence reader
/// shows an env-file span alone, never a window around it.
fn is_env_file(rel: &str) -> bool {
    let base = rel.rsplit(['/', '\\']).next().unwrap_or(rel);
    is_env_name(base)
}

/// True for a dotenv file name (see [`is_env_file`]).
pub fn is_env_name(name: &str) -> bool {
    name == ".env"
        || name.starts_with(".env.")
        || (name.len() > ".env".len() && name.ends_with(".env"))
        || matches!(name, "env.example" | "env.sample" | "env.template")
}

/// A stored string is dropped when it looks secret-shaped: one long
/// unbroken token-like run. Fail closed — a dropped setting is recoverable
/// by reading the cited file; a leaked secret is not.
fn secret_shaped(value: &str) -> bool {
    core_redact::is_token_shaped(value)
}

/// Redact one settings value in place of storing it verbatim: strings are
/// dropped when secret-shaped; arrays/objects are filtered recursively.
fn redacted(value: &serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::String(s) => {
            if secret_shaped(s) {
                None
            } else {
                Some(value.clone())
            }
        }
        serde_json::Value::Array(items) => Some(serde_json::Value::Array(
            items.iter().filter_map(redacted).collect(),
        )),
        serde_json::Value::Object(map) => Some(serde_json::Value::Object(
            map.iter()
                .filter_map(|(k, v)| redacted(v).map(|v| (k.clone(), v)))
                .collect(),
        )),
        _ => Some(value.clone()),
    }
}

/// Insert an allowlisted setting, applying the secret-shape guard.
fn setting(
    settings: &mut BTreeMap<String, serde_json::Value>,
    key: &str,
    value: &serde_json::Value,
) {
    if value.is_null() {
        return;
    }
    if let Some(safe) = redacted(value) {
        settings.insert(key.to_string(), safe);
    }
}

/// First occurrence of `needle` in `text` as a byte span; whole-file span
/// when absent (the file as a whole is still the proof).
fn span_of(text: &str, needle: &str) -> (u64, u64) {
    match text.find(needle) {
        Some(start) => (start as u64, (start + needle.len()) as u64),
        None => (0, text.len() as u64),
    }
}

fn tool(
    name: impl Into<String>,
    display: impl Into<String>,
    category: &'static str,
    rel: &str,
    span: (u64, u64),
    settings: BTreeMap<String, serde_json::Value>,
) -> DetectedTool {
    DetectedTool {
        name: name.into(),
        display: display.into(),
        category,
        path: rel.to_string(),
        byte_start: span.0,
        byte_end: span.1,
        settings,
        settings_behind_code: false,
    }
}

/// Detect every tool and framework label one file proves. `name` is the
/// bare file name, `rel` the repo-relative path. Reads the file only when
/// the name matches a detector — cheap on non-config files. This is the
/// single source of truth for Preflight AND the graph (#215).
pub fn detect_in_file(name: &str, rel: &str, path: &Path) -> Detection {
    detect_in_text(name, rel, &|| std::fs::read_to_string(path).ok())
}

/// A file's text, read only when a detector needs it; `None` when it cannot
/// be read as UTF-8.
type Text<'a> = &'a dyn Fn() -> Option<String>;

/// [`detect_in_file`] over a file whose text `text` supplies (AC-1063).
pub fn detect_in_text(name: &str, rel: &str, text: Text<'_>) -> Detection {
    let mut detection = Detection::default();
    match name {
        "package.json" => detect_package_json(rel, text, &mut detection),
        "go.mod" => {
            detection.framework_labels.push("Go module".into());
            detect_go_mod(rel, text, &mut detection);
        }
        "pyproject.toml" => {
            detection.framework_labels.push("Python project".into());
            detect_pyproject(rel, text, &mut detection);
        }
        "requirements.txt" => {
            detection.framework_labels.push("Python project".into());
            detection.tools.push(tool(
                rel,
                "pip requirements",
                "package-manifest",
                rel,
                (0, 0),
                BTreeMap::new(),
            ));
        }
        "pom.xml" => {
            detection.framework_labels.push("Java project".into());
            detection.tools.push(tool(
                "maven",
                "Maven",
                "build-manifest",
                rel,
                (0, 0),
                BTreeMap::new(),
            ));
        }
        "build.gradle" | "build.gradle.kts" => {
            detection.framework_labels.push("Java project".into());
            detection.tools.push(tool(
                "gradle",
                "Gradle",
                "build-manifest",
                rel,
                (0, 0),
                BTreeMap::new(),
            ));
        }
        "Cargo.toml" => detect_cargo_toml(rel, text, &mut detection),
        "manifest.json" if text().is_some_and(|text| text.contains("\"manifest_version\"")) => {
            detection.framework_labels.push(WEBEXTENSION_LABEL.into());
        }
        _ => {}
    }
    if name == "tsconfig.json"
        || name == "jsconfig.json"
        || (name.starts_with("tsconfig.") && name.ends_with(".json"))
    {
        detect_tsconfig(rel, text, &mut detection);
    }
    if let Some((_, tool_name, display)) = LOCKFILES.iter().find(|(file, _, _)| file == &name) {
        let mut settings = BTreeMap::new();
        settings.insert("lockfile".into(), serde_json::Value::String(rel.into()));
        detection.tools.push(tool(
            *tool_name,
            *display,
            "package-manager",
            rel,
            (0, 0),
            settings,
        ));
    }
    if name == "Dockerfile" || name.starts_with("Dockerfile.") {
        detect_dockerfile(rel, text, &mut detection);
    }
    // Compose files include the common override/profile variants:
    // `docker-compose.override.yml`, `compose.prod.yaml`, … (#216 review).
    if (name.starts_with("docker-compose") || name.starts_with("compose."))
        && (name.ends_with(".yml") || name.ends_with(".yaml"))
    {
        detect_compose(rel, text, &mut detection);
    }
    if (rel.starts_with(".github/workflows/") || rel.starts_with(".github\\workflows\\"))
        && (name.ends_with(".yml") || name.ends_with(".yaml"))
    {
        detect_workflow(rel, text, &mut detection);
    }
    if name.ends_with(".tf") {
        detection.framework_labels.push("Terraform".into());
    }
    if is_env_name(name) {
        detect_env(rel, text, &mut detection);
    }
    detect_code_config(name, rel, &mut detection);
    detection
}

/// `package.json`: the manifest itself (scripts/engines/type as settings)
/// plus one tool per known dependency, each cited at its declaring line.
fn detect_package_json(rel: &str, text: Text<'_>, detection: &mut Detection) {
    let Some(text) = text() else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return; // unparseable manifest: fail closed, no facts
    };
    let mut settings = BTreeMap::new();
    setting(&mut settings, "name", &json["name"]);
    setting(&mut settings, "type", &json["type"]);
    setting(&mut settings, "engines", &json["engines"]);
    setting(&mut settings, "workspaces", &json["workspaces"]);
    setting(&mut settings, "packageManager", &json["packageManager"]);
    // Script *names* only — script bodies are shell commands and can embed
    // anything (fail-closed redaction, AC-0097).
    if let Some(scripts) = json["scripts"].as_object() {
        settings.insert(
            "scripts".into(),
            serde_json::Value::Array(
                scripts
                    .keys()
                    .map(|k| serde_json::Value::String(k.clone()))
                    .collect(),
            ),
        );
    }
    detection.tools.push(tool(
        rel,
        "npm package manifest",
        "package-manifest",
        rel,
        span_of(&text, "\"name\""),
        settings,
    ));
    // `packageManager: "pnpm@9.1.0"` proves the package manager directly.
    if let Some(pm) = json["packageManager"].as_str()
        && let Some((pm_name, version)) = pm.split_once('@')
    {
        let mut settings = BTreeMap::new();
        setting(
            &mut settings,
            "version",
            &serde_json::Value::String(version.into()),
        );
        detection.tools.push(tool(
            pm_name,
            pm_name,
            "package-manager",
            rel,
            span_of(&text, "\"packageManager\""),
            settings,
        ));
    }
    for deps_key in ["dependencies", "devDependencies"] {
        let Some(deps) = json[deps_key].as_object() else {
            continue;
        };
        for (dep, requirement) in deps {
            let Some((_, display, category)) =
                KNOWN_JS_TOOLS.iter().find(|(known, _, _)| known == dep)
            else {
                continue;
            };
            let mut settings = BTreeMap::new();
            setting(&mut settings, "requirement", requirement);
            detection.tools.push(tool(
                dep,
                *display,
                category,
                rel,
                span_of(&text, &format!("\"{dep}\"")),
                settings,
            ));
            if let Some((_, label)) = FRAMEWORK_LABELS.iter().find(|(known, _)| known == dep) {
                detection.framework_labels.push((*label).into());
            }
        }
    }
}

/// `tsconfig.json`/`jsconfig.json`: the compiler options #213's module
/// resolution consumes — parsed once here, cited from this fact.
fn detect_tsconfig(rel: &str, text: Text<'_>, detection: &mut Detection) {
    let Some(text) = text() else {
        return;
    };
    // Parse-once seam (#213): the exact same parse the TS adapter's module
    // resolution consumes — one implementation, so the stored facts and the
    // resolution behavior can never disagree. An unparseable config
    // degrades to presence-only — never guessed settings.
    let facts = adapters_fw::tsconfig::parse(&text);
    let mut settings = BTreeMap::new();
    for (key, value) in &facts.settings {
        setting(&mut settings, key, value);
    }
    detection.tools.push(tool(
        rel,
        "TypeScript config",
        "compiler-config",
        rel,
        facts.span,
        settings,
    ));
}

/// `Cargo.toml`: package name/edition (or workspace members).
fn detect_cargo_toml(rel: &str, text: Text<'_>, detection: &mut Detection) {
    let Some(text) = text() else {
        return;
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return;
    };
    let mut settings = BTreeMap::new();
    if let Some(package) = value.get("package") {
        if let Some(name) = package.get("name").and_then(|v| v.as_str()) {
            settings.insert("package".into(), serde_json::Value::String(name.into()));
        }
        if let Some(edition) = package.get("edition").and_then(|v| v.as_str()) {
            settings.insert("edition".into(), serde_json::Value::String(edition.into()));
        }
    }
    if let Some(members) = value
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
    {
        settings.insert(
            "workspace_members".into(),
            serde_json::Value::Array(
                members
                    .iter()
                    .filter_map(|m| m.as_str())
                    .map(|m| serde_json::Value::String(m.into()))
                    .collect(),
            ),
        );
    }
    detection.tools.push(tool(
        "cargo",
        "Cargo",
        "build-manifest",
        rel,
        span_of(&text, "[package]"),
        settings,
    ));
}

/// `pyproject.toml`: project name, Python requirement, build backend.
fn detect_pyproject(rel: &str, text: Text<'_>, detection: &mut Detection) {
    let Some(text) = text() else {
        return;
    };
    let mut settings = BTreeMap::new();
    if let Ok(value) = text.parse::<toml::Value>() {
        if let Some(project) = value.get("project") {
            if let Some(name) = project.get("name").and_then(|v| v.as_str()) {
                settings.insert("project".into(), serde_json::Value::String(name.into()));
            }
            if let Some(requires) = project.get("requires-python").and_then(|v| v.as_str()) {
                settings.insert(
                    "requires-python".into(),
                    serde_json::Value::String(requires.into()),
                );
            }
        }
        if let Some(backend) = value
            .get("build-system")
            .and_then(|b| b.get("build-backend"))
            .and_then(|v| v.as_str())
        {
            settings.insert(
                "build-backend".into(),
                serde_json::Value::String(backend.into()),
            );
        }
    }
    detection.tools.push(tool(
        rel,
        "Python project manifest",
        "build-manifest",
        rel,
        span_of(&text, "[project]"),
        settings,
    ));
}

/// `go.mod`: module path and Go version from the two directive lines.
fn detect_go_mod(rel: &str, text: Text<'_>, detection: &mut Detection) {
    let Some(text) = text() else {
        return;
    };
    let mut settings = BTreeMap::new();
    for line in text.lines() {
        if let Some(module) = line.trim().strip_prefix("module ") {
            settings.insert(
                "module".into(),
                serde_json::Value::String(module.trim().into()),
            );
        } else if let Some(version) = line.trim().strip_prefix("go ") {
            settings.insert(
                "go".into(),
                serde_json::Value::String(version.trim().into()),
            );
        }
    }
    detection.tools.push(tool(
        "go",
        "Go module",
        "build-manifest",
        rel,
        span_of(&text, "module "),
        settings,
    ));
}

/// `Dockerfile`: base images (`FROM`) and exposed ports (`EXPOSE`) by
/// deterministic line scan.
fn detect_dockerfile(rel: &str, text: Text<'_>, detection: &mut Detection) {
    let Some(text) = text() else {
        return;
    };
    let mut base_images = Vec::new();
    let mut ports = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(from) = trimmed
            .strip_prefix("FROM ")
            .or_else(|| trimmed.strip_prefix("from "))
        {
            // `FROM node:22 AS build` — the image is the first word.
            if let Some(image) = from.split_whitespace().next() {
                base_images.push(serde_json::Value::String(image.into()));
            }
        }
        if let Some(exposed) = trimmed.strip_prefix("EXPOSE ") {
            for port in exposed.split_whitespace() {
                ports.push(serde_json::Value::String(port.into()));
            }
        }
    }
    let mut settings = BTreeMap::new();
    if !base_images.is_empty() {
        settings.insert("base_images".into(), serde_json::Value::Array(base_images));
    }
    if !ports.is_empty() {
        settings.insert("exposed_ports".into(), serde_json::Value::Array(ports));
    }
    detection.tools.push(tool(
        rel,
        "Docker",
        "container",
        rel,
        span_of(&text, "FROM "),
        settings,
    ));
}

/// `docker-compose.yml`: service names and images by indentation scan —
/// enough for evidence without a YAML dependency.
fn detect_compose(rel: &str, text: Text<'_>, detection: &mut Detection) {
    let Some(text) = text() else {
        return;
    };
    let mut services = Vec::new();
    let mut images = Vec::new();
    let mut in_services = false;
    for line in text.lines() {
        if line.starts_with("services:") {
            in_services = true;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if in_services && !line.trim().is_empty() && indent == 0 {
            in_services = false; // next top-level section
        }
        if in_services
            && indent == 2
            && let Some(service) = line.trim().strip_suffix(':')
            && !service.is_empty()
        {
            services.push(serde_json::Value::String(service.into()));
        }
        if let Some(image) = line.trim().strip_prefix("image:") {
            images.push(serde_json::Value::String(image.trim().into()));
        }
    }
    let mut settings = BTreeMap::new();
    if !services.is_empty() {
        settings.insert("services".into(), serde_json::Value::Array(services));
    }
    if !images.is_empty() {
        settings.insert("images".into(), serde_json::Value::Array(images));
    }
    detection.tools.push(tool(
        rel,
        "Docker Compose",
        "container",
        rel,
        span_of(&text, "services:"),
        settings,
    ));
}

/// `.github/workflows/*.yml`: workflow name, triggers, and job names by
/// indentation scan.
fn detect_workflow(rel: &str, text: Text<'_>, detection: &mut Detection) {
    let Some(text) = text() else {
        return;
    };
    let mut settings = BTreeMap::new();
    let mut triggers = Vec::new();
    let mut jobs = Vec::new();
    let mut section = "";
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if indent == 0 {
            if let Some(workflow_name) = trimmed.strip_prefix("name:") {
                settings.insert(
                    "workflow".into(),
                    serde_json::Value::String(workflow_name.trim().trim_matches('"').into()),
                );
            }
            if let Some(inline) = trimmed.strip_prefix("on:") {
                // `on: push` / `on: [push, pull_request]` inline forms.
                for trigger in inline.trim_matches(['[', ']', ' ']).split(',') {
                    if !trigger.trim().is_empty() {
                        triggers.push(serde_json::Value::String(trigger.trim().into()));
                    }
                }
            }
            section = if trimmed.starts_with("on:") {
                "on"
            } else if trimmed.starts_with("jobs:") {
                "jobs"
            } else {
                ""
            };
            continue;
        }
        if indent == 2 && !trimmed.is_empty() {
            match section {
                "on" => {
                    let key = trimmed.trim_start_matches('-').trim();
                    let key = key.split(':').next().unwrap_or(key).trim();
                    if !key.is_empty() {
                        triggers.push(serde_json::Value::String(key.into()));
                    }
                }
                "jobs" => {
                    if let Some(job) = trimmed.strip_suffix(':') {
                        jobs.push(serde_json::Value::String(job.into()));
                    }
                }
                _ => {}
            }
        }
    }
    if !triggers.is_empty() {
        settings.insert("triggers".into(), serde_json::Value::Array(triggers));
    }
    if !jobs.is_empty() {
        settings.insert("jobs".into(), serde_json::Value::Array(jobs));
    }
    detection.tools.push(tool(
        rel,
        "GitHub Actions workflow",
        "ci",
        rel,
        span_of(&text, "on:"),
        settings,
    ));
}

/// One `KEY=value` assignment of a dotenv text.
struct EnvAssignment<'a> {
    key: &'a str,
    /// The value, unquoted; `None` for a quoted value left open on its
    /// line (a multiline value is never read).
    value: Option<&'a str>,
    /// Byte span of the assignment's line, without its line break.
    line: (u64, u64),
}

/// The assignments of a dotenv text, in order. A line counts only as
/// `[export ]KEY=…` with a well-formed key; anything else (a stray
/// fragment, a comment) is skipped. A quoted value left open on its line
/// swallows every following line until it closes, so the lines of a
/// multiline secret (a PEM body, which may itself contain `=`) are never
/// read as keys. An unterminated quote skips the rest of the file: fail
/// closed.
fn env_assignments(text: &str) -> Vec<EnvAssignment<'_>> {
    let mut assignments = Vec::new();
    let mut open: Option<char> = None;
    let mut offset = 0usize;
    for raw in text.split_inclusive('\n') {
        let start = offset;
        offset += raw.len();
        let raw = raw.strip_suffix('\n').unwrap_or(raw);
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(quote) = open {
            if closing_quote(raw, quote).is_some() {
                open = None;
            }
            continue;
        }
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        let assignment = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = assignment.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let mut chars = key.chars();
        let well_formed = chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        if !well_formed {
            continue;
        }
        let value = value.trim_start();
        let parsed = match value
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\'' | '`'))
        {
            Some(quote) => match closing_quote(&value[1..], quote) {
                Some(end) => Some(&value[1..1 + end]),
                None => {
                    open = Some(quote);
                    None
                }
            },
            // An unquoted value ends at an inline ` #` comment.
            None => Some(value.split_once(" #").map_or(value, |(v, _)| v).trim_end()),
        };
        assignments.push(EnvAssignment {
            key,
            value: parsed,
            line: (start as u64, (start + raw.len()) as u64),
        });
    }
    assignments
}

/// The assigned key names of a dotenv text, in order (see
/// [`env_assignments`]).
fn env_keys(text: &str) -> Vec<&str> {
    env_assignments(text).into_iter().map(|a| a.key).collect()
}

/// The values a dotenv text assigns to allowlisted keys (ADR-0075,
/// AC-1021), each with the byte span of its line: the first assignment of a
/// key wins, as in the events resolver. A multiline value, a secret
/// (AC-0990's test) or a token-shaped value is never kept, and a URL loses
/// any `user@`. Key order.
fn env_values(
    text: &str,
    read: &std::collections::BTreeSet<String>,
) -> BTreeMap<String, (String, (u64, u64))> {
    let mut values = BTreeMap::new();
    // The first assignment claims its key even when its value is dropped, so
    // a later duplicate never takes its place.
    let mut seen = std::collections::BTreeSet::new();
    for assignment in env_assignments(text) {
        if !read.contains(assignment.key) || !seen.insert(assignment.key) {
            continue;
        }
        let Some(value) = assignment.value else {
            continue;
        };
        if core_redact::env_secret_reason(assignment.key, value).is_some() {
            continue;
        }
        let value = core_redact::without_userinfo(value);
        if secret_shaped(&value) {
            continue;
        }
        values.insert(assignment.key.to_string(), (value, assignment.line));
    }
    values
}

/// Byte offset of the first `quote` in `text` that a backslash does not
/// escape (single quotes take no escapes).
fn closing_quote(text: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (offset, c) in text.char_indices() {
        if c == quote && !escaped {
            return Some(offset);
        }
        escaped = quote != '\'' && c == '\\' && !escaped;
    }
    None
}

/// `.env*`: key names ONLY (AC-0097, fail closed). Preflight and every
/// other caller of [`detect_in_file`] see no value; only
/// [`extract_dir_reading`] adds the values of allowlisted keys.
fn detect_env(rel: &str, text: Text<'_>, detection: &mut Detection) {
    let Some(text) = text() else {
        return;
    };
    let keys: Vec<serde_json::Value> = env_keys(&text)
        .into_iter()
        .map(|key| serde_json::Value::String(key.into()))
        .collect();
    let mut settings = BTreeMap::new();
    settings.insert("keys".into(), serde_json::Value::Array(keys));
    detection.tools.push(tool(
        rel,
        "Environment file",
        "environment",
        rel,
        (0, 0),
        settings,
    ));
}

/// JS/TS-authored configs: presence only — settings live behind code and
/// are never evaluated (#215 non-goal). Preflight surfaces the limitation
/// as a `config-behind-code` finding from this same detection.
fn detect_code_config(name: &str, rel: &str, detection: &mut Detection) {
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    let Some((_, display, category)) = CODE_CONFIG_STEMS
        .iter()
        .find(|(config_stem, _, _)| config_stem == &stem)
    else {
        return;
    };
    let mut detected = tool(rel, *display, category, rel, (0, 0), BTreeMap::new());
    detected.settings_behind_code = true;
    detection.tools.push(detected);
}

/// Walk `root` (sorted, same skip set as Preflight) and convert every
/// detection into graph facts: `Tool` nodes, config `File` nodes, and
/// `DEFINED_IN` edges, all with T0/Confirmed provenance citing declaring
/// spans (AC-0096). Deterministic for a given tree.
pub fn extract_dir(
    root: &Path,
    id: &SourceId,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<ToolExtraction> {
    extract_dir_reading(root, id, &std::collections::BTreeSet::new(), on_file)
}

/// [`extract_dir`], also reading the values of the env keys `env_read`
/// allowlists (ADR-0075, AC-1021). An env-file `Tool` then carries
/// `settings.values` (key → value) and cites each value's line, in key
/// order; a template file's `Tool` carries `settings.template = true`. Every
/// other env-file fact stays span-free, and an env file with no allowlisted
/// value is unchanged.
pub fn extract_dir_reading(
    root: &Path,
    id: &SourceId,
    env_read: &std::collections::BTreeSet<String>,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<ToolExtraction> {
    // The same walk as Preflight (#248), so the two never disagree about
    // what is part of the system.
    let walked = source_walk::files(
        root,
        &crate::preflight::skip_dir,
        source_walk::Gitignores::Honor,
    )?;
    let checkout = Checkout(root);
    Ok(extract_walked(&walked, &checkout, id, env_read, on_file))
}

/// True when the toolchain reader may open `rel` and a capture retains it:
/// every file a detector names, other than an env file, which holds secrets
/// (AC-0097) and is never retained.
pub fn is_toolchain_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    !is_env_name(name) && detector_names(name, rel)
}

/// True for every file [`detect_in_text`] has a detector for, by name.
fn detector_names(name: &str, rel: &str) -> bool {
    let yaml = name.ends_with(".yml") || name.ends_with(".yaml");
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    matches!(
        name,
        "package.json"
            | "go.mod"
            | "pyproject.toml"
            | "requirements.txt"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "Cargo.toml"
            | "manifest.json"
            | "tsconfig.json"
            | "jsconfig.json"
    ) || (name.starts_with("tsconfig.") && name.ends_with(".json"))
        || LOCKFILES.iter().any(|(file, _, _)| *file == name)
        || name == "Dockerfile"
        || name.starts_with("Dockerfile.")
        || ((name.starts_with("docker-compose") || name.starts_with("compose.")) && yaml)
        || ((rel.starts_with(".github/workflows/") || rel.starts_with(".github\\workflows\\"))
            && yaml)
        || name.ends_with(".tf")
        || CODE_CONFIG_STEMS
            .iter()
            .any(|(config, _, _)| *config == stem)
}

/// The files [`extract_dir`] may open under `root`, in path order: a source
/// capture retains them (AC-1063).
pub fn toolchain_paths(root: &Path) -> std::io::Result<Vec<String>> {
    // Every detector reads only its own file, so the facts cite no file
    // beyond the selection.
    crate::captured_walk::selected(root, is_toolchain_file, || Ok((Vec::new(), Vec::new())))
}

/// [`extract_dir_reading`] over a source capture (AC-1063, #385):
/// `captured` is the capture's [`is_toolchain_file`] selection and
/// `captured_read` its bytes. The reader lists the capture's own paths, and
/// lists and reads the checkout only for env files, which no capture retains.
pub fn extract_from_capture(
    root: &Path,
    captured: &[String],
    captured_read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    id: &SourceId,
    env_read: &std::collections::BTreeSet<String>,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<ToolExtraction> {
    // The capture's own paths, and the checkout's env files, which no
    // capture retains (AC-0097).
    let mut walked = crate::captured_walk::listing(root, captured);
    walked.extend(
        crate::captured_walk::walk(root)?
            .into_iter()
            .filter(|file| is_env_name(&file.name)),
    );
    walked.sort_by(|a, b| a.rel.cmp(&b.rel));
    let captured = Captured {
        root,
        read: captured_read,
    };
    Ok(extract_walked(&walked, &captured, id, env_read, on_file))
}

/// [`extract_from_capture`], reporting which captured files the reader
/// read and whether its facts are bound to them (AC-1536, #385). Each
/// detector reads only its own file, so they are unless a `Tool` merges a
/// proof from a checkout env file, which no capture retains, with one from
/// a captured file: its settings then depend on the checkout. Env-file
/// facts themselves cite no captured file.
pub fn extract_from_capture_reading(
    root: &Path,
    captured: &[String],
    captured_read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    id: &SourceId,
    env_read: &std::collections::BTreeSet<String>,
    on_file: &mut dyn FnMut(&str),
) -> std::io::Result<(ToolExtraction, crate::captured_walk::CaptureReads)> {
    let opened = std::cell::RefCell::new(std::collections::BTreeSet::new());
    let recorded = |rel: &str| {
        opened.borrow_mut().insert(rel.to_string());
        captured_read(rel)
    };
    let facts = extract_from_capture(root, captured, &recorded, id, env_read, on_file)?;
    // Each Tool's DEFINED_IN files: env or not.
    let file_prefix = format!("file:{}@", id.repo);
    let mut defined_in: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
    for edge in facts.edges.iter().filter(|edge| edge.label == "DEFINED_IN") {
        let Some(path) = edge.dst.strip_prefix(&file_prefix) else {
            continue;
        };
        let (env, other) = defined_in.entry(edge.src.as_str()).or_default();
        if is_env_file(path) {
            *env = true;
        } else {
            *other = true;
        }
    }
    let bound = !defined_in.values().any(|(env, other)| *env && *other);
    let read = opened.into_inner().into_iter().collect();
    Ok((facts, crate::captured_walk::CaptureReads { bound, read }))
}

/// Where the toolchain reader's bytes come from, by repo-relative path.
trait Bytes {
    fn bytes(&self, rel: &str) -> Option<Vec<u8>>;
    fn len(&self, rel: &str) -> u64;
    fn text(&self, rel: &str) -> Option<String> {
        String::from_utf8(self.bytes(rel)?).ok()
    }
}

struct Checkout<'a>(&'a Path);

impl Bytes for Checkout<'_> {
    fn bytes(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.0.join(rel)).ok()
    }
    fn len(&self, rel: &str) -> u64 {
        std::fs::metadata(self.0.join(rel)).map_or(0, |meta| meta.len())
    }
}

/// A capture's bytes, and the checkout's for an env file.
struct Captured<'a> {
    root: &'a Path,
    read: &'a dyn Fn(&str) -> std::io::Result<Vec<u8>>,
}

impl Bytes for Captured<'_> {
    fn bytes(&self, rel: &str) -> Option<Vec<u8>> {
        if is_env_name(rel.rsplit('/').next().unwrap_or(rel)) {
            Checkout(self.root).bytes(rel)
        } else if is_toolchain_file(rel) {
            (self.read)(rel).ok()
        } else {
            None
        }
    }
    fn len(&self, rel: &str) -> u64 {
        if is_env_name(rel.rsplit('/').next().unwrap_or(rel)) {
            Checkout(self.root).len(rel)
        } else {
            self.bytes(rel).map_or(0, |bytes| bytes.len() as u64)
        }
    }
}

fn extract_walked(
    walked: &[source_walk::WalkedFile],
    source: &dyn Bytes,
    id: &SourceId,
    env_read: &std::collections::BTreeSet<String>,
    on_file: &mut dyn FnMut(&str),
) -> ToolExtraction {
    // One declaring proof: (config path, span start, span end).
    type Proof = (String, u64, u64);
    // Env-file tool name → the line spans of its allowlisted values.
    let mut value_lines: BTreeMap<String, Vec<(u64, u64)>> = BTreeMap::new();
    // name → merged tool (a package manager proven by both `packageManager`
    // and a lockfile is one Tool with two proofs).
    let mut tools: BTreeMap<String, (DetectedTool, Vec<Proof>)> = BTreeMap::new();
    let mut config_files: BTreeMap<String, u64> = BTreeMap::new();

    for source_walk::WalkedFile { rel, name, .. } in walked {
        let mut detection = detect_in_text(name, rel, &|| source.text(rel));
        if detection.tools.is_empty() {
            continue;
        }
        if !env_read.is_empty() && is_env_file(rel) {
            let text = source.text(rel).unwrap_or_default();
            let values = env_values(&text, env_read);
            if let Some(env) = detection
                .tools
                .iter_mut()
                .find(|tool| tool.category == "environment" && !values.is_empty())
            {
                let lines = value_lines.entry(env.name.clone()).or_default();
                let mut map = serde_json::Map::new();
                for (key, (value, line)) in values {
                    map.insert(key, serde_json::Value::String(value));
                    lines.push(line);
                }
                env.settings
                    .insert("values".into(), serde_json::Value::Object(map));
                if core_redact::is_env_template(name) {
                    env.settings
                        .insert("template".into(), serde_json::Value::Bool(true));
                }
            }
        }
        on_file(rel);
        let file_len = source.len(rel);
        config_files.entry(rel.clone()).or_insert(file_len);
        for detected in detection.tools {
            let proof = (
                detected.path.clone(),
                detected.byte_start,
                detected.byte_end,
            );
            match tools.entry(detected.name.clone()) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert((detected, vec![proof]));
                }
                std::collections::btree_map::Entry::Occupied(mut slot) => {
                    let (merged, proofs) = slot.get_mut();
                    for (key, value) in detected.settings {
                        merged.settings.entry(key).or_insert(value);
                    }
                    merged.settings_behind_code =
                        merged.settings_behind_code || detected.settings_behind_code;
                    proofs.push(proof);
                }
            }
        }
    }

    let mut extraction = ToolExtraction {
        files: config_files.len() as u64,
        ..Default::default()
    };
    // A tool proven by a file's name alone (a lockfile, `requirements.txt`,
    // `pom.xml`, a code-authored config) cites the whole file: its presence
    // is the proof, and an empty span would cite no bytes (AC-1536).
    let evidence = |path: &str, start: u64, end: u64| {
        let end = if (start, end) == (0, 0) {
            config_files.get(path).copied().unwrap_or(0)
        } else {
            end
        };
        EvidenceRef {
            repo: id.repo.to_string(),
            path: path.to_string(),
            byte_start: start,
            byte_end: end,
            commit_sha: id.commit.to_string(),
        }
    };
    for (rel, len) in &config_files {
        // Env files get no evidence span: a span is a readable window into
        // the secret-bearing file (#216 review, AC-0097 fail-closed).
        let spans = if is_env_file(rel) {
            vec![]
        } else {
            vec![evidence(rel, 0, *len)]
        };
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            spans,
            TOOLCHAIN_EXTRACTOR_ID,
            format!("file:{}@{rel}", id.repo).as_bytes(),
        )
        .expect("within ceiling");
        let mut props = serde_json::json!({
            "path": rel,
            "config": true,
            "prov": serde_json::to_value(prov).expect("serializes"),
        });
        if is_env_file(rel) {
            props["redacted"] = serde_json::Value::Bool(true);
        }
        extraction.nodes.push(Node {
            id: format!("file:{}@{rel}", id.repo),
            label: "File".into(),
            props,
        });
    }
    for (name, (detected, proofs)) in &tools {
        let tool_id = format!("tool:{}@{name}", id.repo);
        let settings = serde_json::Value::Object(
            detected
                .settings
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        // An env file's only spans are the lines of its allowlisted values
        // (ADR-0075): the evidence reader shows exactly that line.
        let spans: Vec<EvidenceRef> = proofs
            .iter()
            .filter(|(path, _, _)| !is_env_file(path))
            .map(|(path, start, end)| evidence(path, *start, *end))
            .chain(
                value_lines
                    .get(name)
                    .into_iter()
                    .flatten()
                    .map(|(start, end)| evidence(&detected.path, *start, *end)),
            )
            .collect();
        let prov = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
            spans.clone(),
            TOOLCHAIN_EXTRACTOR_ID,
            format!("{tool_id}:{settings}").as_bytes(),
        )
        .expect("within ceiling");
        let mut props = serde_json::json!({
            "name": name,
            "display": detected.display,
            "category": detected.category,
            "settings": settings,
            "prov": serde_json::to_value(&prov).expect("serializes"),
        });
        if detected.settings_behind_code {
            props["settings_behind_code"] = serde_json::Value::Bool(true);
        }
        extraction.nodes.push(Node {
            id: tool_id.clone(),
            label: "Tool".into(),
            props,
        });
        let mut proven_paths: Vec<&String> = proofs.iter().map(|(path, _, _)| path).collect();
        proven_paths.sort();
        proven_paths.dedup();
        for path in proven_paths {
            let span = proofs
                .iter()
                .find(|(proof_path, _, _)| proof_path == path)
                .map(|(_, start, end)| (*start, *end))
                .unwrap_or((0, 0));
            let edge_spans = if is_env_file(path) {
                vec![] // no readable window into a secret-bearing file
            } else {
                vec![evidence(path, span.0, span.1)]
            };
            let edge_prov = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                edge_spans,
                TOOLCHAIN_EXTRACTOR_ID,
                format!("{tool_id} DEFINED_IN {path}").as_bytes(),
            )
            .expect("within ceiling");
            extraction.edges.push(Edge {
                src: tool_id.clone(),
                dst: format!("file:{}@{path}", id.repo),
                label: "DEFINED_IN".into(),
                props: serde_json::json!({
                    "prov": serde_json::to_value(edge_prov).expect("serializes"),
                }),
            });
        }
    }
    extraction
}

/// Edge from an observed resource to an env file declaring its key.
pub const CONFIGURED_IN: &str = "CONFIGURED_IN";

/// `CONFIGURED_IN` edges from each `Resource` whose identity is an env
/// reference (`props.env_key`, #734) to every env file whose key names
/// include it: the code reads `KEY`, and the file declares `KEY`. Which file
/// a deployment loads is not proven, so each declaring file gets its own
/// edge. Like the env facts, the edge carries no evidence span (AC-0097).
pub fn link_env_resources(nodes: &[Node], id: &SourceId) -> Vec<Edge> {
    let mut declared: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in nodes
        .iter()
        .filter(|node| node.label == "Tool" && node.props["category"] == "environment")
    {
        let Some(path) = node.props["name"].as_str().filter(|path| is_env_file(path)) else {
            continue;
        };
        for key in node.props["settings"]["keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
        {
            declared.entry(key).or_default().push(path);
        }
    }
    let mut edges = Vec::new();
    for node in nodes.iter().filter(|node| node.label == "Resource") {
        let Some(key) = node.props["env_key"].as_str() else {
            continue;
        };
        let mut paths = declared.get(key).cloned().unwrap_or_default();
        paths.sort_unstable();
        paths.dedup();
        for path in paths {
            let prov = Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![],
                TOOLCHAIN_EXTRACTOR_ID,
                format!("{} {CONFIGURED_IN} {path}:{key}", node.id).as_bytes(),
            )
            .expect("within ceiling");
            edges.push(Edge {
                src: node.id.clone(),
                dst: format!("file:{}@{path}", id.repo),
                label: CONFIGURED_IN.into(),
                props: serde_json::json!({
                    "env_key": key,
                    "prov": serde_json::to_value(prov).expect("serializes"),
                }),
            });
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn extract(root: &Path) -> ToolExtraction {
        extract_dir(
            root,
            &SourceId {
                repo: "local/tools",
                commit: "c0ffee",
            },
            &mut |_| {},
        )
        .unwrap()
    }

    fn tool_node<'a>(extraction: &'a ToolExtraction, id: &str) -> &'a Node {
        extraction
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("no node {id}"))
    }

    #[test]
    fn package_json_yields_cited_tool_facts_with_settings() {
        // AC-0096: dependency-proven tools carry the declaring span and the
        // manifest's resolved settings as citable props.
        let dir = tempfile::tempdir().unwrap();
        let text = r#"{
  "name": "shop",
  "type": "module",
  "packageManager": "pnpm@9.1.0",
  "scripts": { "build": "vite build", "test": "vitest run" },
  "dependencies": { "react": "^19.0.0", "left-pad": "^1.0.0" },
  "devDependencies": { "vite": "^6.0.0" }
}"#;
        write(dir.path(), "package.json", text);
        let extraction = extract(dir.path());

        let react = tool_node(&extraction, "tool:local/tools@react");
        assert_eq!(react.label, "Tool");
        assert_eq!(react.props["category"], "framework");
        assert_eq!(react.props["settings"]["requirement"], "^19.0.0");
        // Evidence cites the `"react"` declaring span, not just the file.
        let prov: Provenance = serde_json::from_value(react.props["prov"].clone()).unwrap();
        let span = &prov.evidence[0];
        assert_eq!(
            &text[span.byte_start as usize..span.byte_end as usize],
            "\"react\""
        );
        assert_eq!(prov.extractor_id, TOOLCHAIN_EXTRACTOR_ID);

        // Unknown dependencies are not tools.
        assert!(!extraction.nodes.iter().any(|n| n.id.contains("left-pad")));

        // The manifest itself carries scripts (names only), type, engines.
        let manifest = tool_node(&extraction, "tool:local/tools@package.json");
        assert_eq!(manifest.props["settings"]["type"], "module");
        assert_eq!(
            manifest.props["settings"]["scripts"],
            serde_json::json!(["build", "test"])
        );
        // Script bodies (shell commands) are never stored.
        assert!(!manifest.props.to_string().contains("vite build"));

        // packageManager proves pnpm; the lockfile would merge into it.
        let pnpm = tool_node(&extraction, "tool:local/tools@pnpm");
        assert_eq!(pnpm.props["settings"]["version"], "9.1.0");

        // Every tool is DEFINED_IN the config file that proves it.
        assert!(
            extraction
                .edges
                .iter()
                .any(|e| e.src == "tool:local/tools@react"
                    && e.dst == "file:local/tools@package.json"
                    && e.label == "DEFINED_IN")
        );
        // And the config File node exists so the edge can never dangle.
        assert!(
            extraction
                .nodes
                .iter()
                .any(|n| n.id == "file:local/tools@package.json" && n.label == "File")
        );
    }

    #[test]
    fn tsconfig_settings_are_the_parse_once_source_for_resolution() {
        // #213 seam: paths/baseUrl parsed once here, cited from this fact.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "tsconfig.json",
            r#"{
  "compilerOptions": {
    "target": "ES2022",
    "moduleResolution": "bundler",
    "strict": true,
    "baseUrl": ".",
    "paths": { "@/*": ["src/*"] }
  }
}"#,
        );
        let extraction = extract(dir.path());
        let tsconfig = tool_node(&extraction, "tool:local/tools@tsconfig.json");
        assert_eq!(tsconfig.props["category"], "compiler-config");
        assert_eq!(tsconfig.props["settings"]["moduleResolution"], "bundler");
        assert_eq!(tsconfig.props["settings"]["strict"], true);
        assert_eq!(
            tsconfig.props["settings"]["paths"]["@/*"],
            serde_json::json!(["src/*"])
        );
    }

    #[test]
    fn env_files_contribute_key_names_and_never_values() {
        // AC-0097 fail-closed redaction: the secret value must not appear
        // anywhere in the serialized extraction — props, evidence, hashes.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ".env.production",
            "# comment\nDATABASE_URL=postgres://user:hunter2@db/prod\nexport API_TOKEN=sk-live-abcdef123456\n",
        );
        let extraction = extract(dir.path());
        let env = tool_node(&extraction, "tool:local/tools@.env.production");
        assert_eq!(env.props["category"], "environment");
        assert_eq!(
            env.props["settings"]["keys"],
            serde_json::json!(["DATABASE_URL", "API_TOKEN"])
        );
        let serialized = serde_json::to_string(&extraction.nodes).unwrap();
        assert!(!serialized.contains("hunter2"));
        assert!(!serialized.contains("sk-live"));

        // #216 review: env facts carry NO evidence spans — a span (even
        // zero-length) is a readable window the evidence reader would open
        // into the secret-bearing file. Tool node, File node, and the
        // DEFINED_IN edge are all span-free; the File node is marked
        // redacted so the UI knows why there is nothing to jump to.
        let env_prov: Provenance = serde_json::from_value(env.props["prov"].clone()).unwrap();
        assert!(env_prov.evidence.is_empty());
        let file = extraction
            .nodes
            .iter()
            .find(|n| n.id == "file:local/tools@.env.production")
            .unwrap();
        assert_eq!(file.props["redacted"], true);
        let file_prov: Provenance = serde_json::from_value(file.props["prov"].clone()).unwrap();
        assert!(file_prov.evidence.is_empty());
        let defined_in = extraction
            .edges
            .iter()
            .find(|e| e.src == "tool:local/tools@.env.production")
            .unwrap();
        let edge_prov: Provenance =
            serde_json::from_value(defined_in.props["prov"].clone()).unwrap();
        assert!(edge_prov.evidence.is_empty());
    }

    // AC-1021 (ADR-0075): an allowlisted key's value is a toolchain setting
    // citing its line; unlisted keys, secrets, multiline values and other
    // env files stay keys-only and span-free; a template is labelled; a URL
    // loses `user@`. Preflight's detection never sees a value.
    #[test]
    fn allowlisted_env_values_are_settings_citing_their_line() {
        let dir = tempfile::tempdir().unwrap();
        let env = "# local\n\
            QUEUE_URL=amqp://svc@mq:5672/orders # broker\n\
            OTHER=hidden\n\
            API_BASE=\"https://api.example.test\"\n\
            DB_PASSWORD=s3cr3t-pass\n\
            NOTE=\"two\nlines\"\n\
            NOTE=later\n\
            DB_PASSWORD=later\n";
        write(dir.path(), ".env", env);
        write(dir.path(), ".env.example", "QUEUE_URL=amqp://localhost\n");
        write(dir.path(), "config/test.env", "OTHER=x\n");
        let read: std::collections::BTreeSet<String> =
            ["QUEUE_URL", "API_BASE", "DB_PASSWORD", "NOTE"]
                .into_iter()
                .map(String::from)
                .collect();
        let extraction = extract_dir_reading(
            dir.path(),
            &SourceId {
                repo: "local/tools",
                commit: "c0ffee",
            },
            &read,
            &mut |_| {},
        )
        .unwrap();

        let real = tool_node(&extraction, "tool:local/tools@.env");
        assert_eq!(
            real.props["settings"]["values"],
            serde_json::json!({
                "API_BASE": "https://api.example.test",
                "QUEUE_URL": "amqp://mq:5672/orders",
            })
        );
        assert!(real.props["settings"].get("template").is_none());
        let prov: Provenance = serde_json::from_value(real.props["prov"].clone()).unwrap();
        let lines: Vec<&str> = prov
            .evidence
            .iter()
            .map(|span| &env[span.byte_start as usize..span.byte_end as usize])
            .collect();
        assert_eq!(
            lines,
            [
                "API_BASE=\"https://api.example.test\"",
                "QUEUE_URL=amqp://svc@mq:5672/orders # broker",
            ]
        );
        assert!(prov.evidence.iter().all(|span| span.path == ".env"));

        let template = tool_node(&extraction, "tool:local/tools@.env.example");
        assert_eq!(template.props["settings"]["template"], true);
        assert_eq!(
            template.props["settings"]["values"]["QUEUE_URL"],
            "amqp://localhost"
        );

        // No allowlisted value: unchanged, keys only, no span.
        let other = tool_node(&extraction, "tool:local/tools@config/test.env");
        assert!(other.props["settings"].get("values").is_none());
        let prov: Provenance = serde_json::from_value(other.props["prov"].clone()).unwrap();
        assert!(prov.evidence.is_empty());
        // The File node and DEFINED_IN edge of the env file stay span-free.
        let file = tool_node(&extraction, "file:local/tools@.env");
        let prov: Provenance = serde_json::from_value(file.props["prov"].clone()).unwrap();
        assert!(prov.evidence.is_empty());

        // Without an allowlist nothing changes, and Preflight sees no value.
        let plain = extract(dir.path());
        assert!(
            tool_node(&plain, "tool:local/tools@.env").props["settings"]
                .get("values")
                .is_none()
        );
        let detection = detect_in_file(".env", ".env", &dir.path().join(".env"));
        assert!(!detection.tools[0].settings.contains_key("values"));
    }

    #[test]
    fn env_keys_skip_multiline_values_and_non_assignments() {
        // A PEM body inside a quoted value, including a `==`-padded line,
        // must never surface as a key (AC-0097).
        let text = "A=1\n\
            PRIVATE_KEY=\"-----BEGIN KEY-----\n\
            MIIBOgIBAAJBAKj34GkxFhD90vcNLYLInFEX6Ppy1tPf9Cnzj4p4WGeKLs1Pt8Qu\n\
            KUpRKfFLfRYC9AIKjbJTWit+CqvjWYzvQwECAwEAAQ==\n\
            -----END KEY-----\"\n\
            B='one line'\n\
            stray fragment without assignment\n\
            9BAD=x\n\
            export C.D = \"esc \\\" still open\nE=inside\n\" \n\
            F=`x`\n\
            G=\"never closed\nH=1\n";
        assert_eq!(env_keys(text), ["A", "PRIVATE_KEY", "B", "C.D", "F", "G"]);
    }

    #[test]
    fn env_templates_are_env_files_and_link_env_keyed_resources() {
        // `example.env`-style templates hold the same keys-only line as
        // `.env*`: placeholder values are often real credentials.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "example.env",
            "DB_URI=postgresql://myuser:mypassword@sl-db:5432/sl\nUNUSED=1\n",
        );
        write(dir.path(), "tests/test.env", "DB_URI=sqlite://\n");
        write(dir.path(), "config/env.example", "OTHER=x\n");
        // Not env files: no `.env` suffix boundary, or not a template name.
        write(dir.path(), "myenv", "A=1\n");
        write(dir.path(), "environment.yml", "name: x\n");
        let extraction = extract(dir.path());
        for path in ["example.env", "tests/test.env", "config/env.example"] {
            let env = tool_node(&extraction, &format!("tool:local/tools@{path}"));
            assert_eq!(env.props["category"], "environment");
            let prov: Provenance = serde_json::from_value(env.props["prov"].clone()).unwrap();
            assert!(prov.evidence.is_empty());
        }
        assert!(!is_env_file("src/.envrc.ts") && !is_env_file("environment.yml"));
        assert!(!is_env_name("env") && !is_env_name("myenv") && is_env_name(".env"));
        assert!(
            !serde_json::to_string(&extraction.nodes)
                .unwrap()
                .contains("mypassword")
        );
        assert!(
            !serde_json::to_string(&extraction.edges)
                .unwrap()
                .contains("mypassword")
        );

        let resource = |id: &str, key: Option<&str>| Node {
            id: id.into(),
            label: "Resource".into(),
            props: key.map_or(
                serde_json::json!({}),
                |key| serde_json::json!({ "env_key": key }),
            ),
        };
        let mut nodes = extraction.nodes.clone();
        nodes.push(resource(
            "res:local/tools@observed:sql:env:DB_URI",
            Some("DB_URI"),
        ));
        nodes.push(resource(
            "res:local/tools@observed:redis:env:MISSING",
            Some("MISSING"),
        ));
        nodes.push(resource("res:local/tools@observed:s3", None));
        let id = SourceId {
            repo: "local/tools",
            commit: "c0ffee",
        };
        let links = link_env_resources(&nodes, &id);
        let targets: Vec<(&str, &str)> = links
            .iter()
            .map(|edge| (edge.src.as_str(), edge.dst.as_str()))
            .collect();
        assert_eq!(
            targets,
            [
                (
                    "res:local/tools@observed:sql:env:DB_URI",
                    "file:local/tools@example.env"
                ),
                (
                    "res:local/tools@observed:sql:env:DB_URI",
                    "file:local/tools@tests/test.env"
                ),
            ]
        );
        for edge in &links {
            assert_eq!(edge.label, CONFIGURED_IN);
            assert_eq!(edge.props["env_key"], "DB_URI");
            let prov: Provenance = serde_json::from_value(edge.props["prov"].clone()).unwrap();
            assert!(prov.evidence.is_empty());
            // Every link lands on a File node the toolchain produced.
            assert!(
                extraction
                    .nodes
                    .iter()
                    .any(|n| n.id == edge.dst && n.label == "File")
            );
        }
        assert_eq!(link_env_resources(&nodes, &id), links);
    }

    #[test]
    fn compose_override_files_are_detected() {
        // #216 review: AC-0096 promises `docker-compose.*` coverage — the
        // common override/profile variants included.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "docker-compose.override.yml",
            "services:\n  web:\n    image: shop:dev\n",
        );
        write(
            dir.path(),
            "compose.prod.yaml",
            "services:\n  api:\n    image: shop:prod\n",
        );
        let extraction = extract(dir.path());
        let override_file = tool_node(&extraction, "tool:local/tools@docker-compose.override.yml");
        assert_eq!(
            override_file.props["settings"]["services"],
            serde_json::json!(["web"])
        );
        let prod = tool_node(&extraction, "tool:local/tools@compose.prod.yaml");
        assert_eq!(
            prod.props["settings"]["services"],
            serde_json::json!(["api"])
        );
    }

    #[test]
    fn secret_shaped_values_are_dropped_from_any_config() {
        // A token-like string in an allowlisted setting is dropped, not
        // stored (fail closed) — the cited file remains the way to read it.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{ "name": "x", "engines": { "node": "AKIAIOSFODNN7EXAMPLEAKIAIOSFODNN7EXAMPLE12" } }"#,
        );
        let extraction = extract(dir.path());
        let manifest = tool_node(&extraction, "tool:local/tools@package.json");
        assert!(manifest.props["settings"]["engines"]["node"].is_null());
        assert!(
            !serde_json::to_string(&extraction.nodes)
                .unwrap()
                .contains("AKIAIOSFODNN7EXAMPLE")
        );
    }

    #[test]
    fn code_authored_configs_are_presence_only() {
        // #215 non-goal: JS-authored configs are never evaluated — the tool
        // exists, flagged as settings-behind-code, with empty settings.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "vite.config.ts",
            "export default { base: '/' }\n",
        );
        let extraction = extract(dir.path());
        let vite = tool_node(&extraction, "tool:local/tools@vite.config.ts");
        assert_eq!(vite.props["settings_behind_code"], true);
        assert_eq!(vite.props["settings"], serde_json::json!({}));
    }

    #[test]
    fn docker_ci_lockfiles_and_manifests_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Dockerfile",
            "FROM node:22-alpine AS build\nEXPOSE 3000 9229\n",
        );
        write(
            dir.path(),
            "docker-compose.yml",
            "services:\n  web:\n    image: shop:latest\n  db:\n    image: postgres:16\n",
        );
        write(
            dir.path(),
            ".github/workflows/ci.yml",
            "name: CI\non:\n  push:\n  pull_request:\njobs:\n  build:\n    runs-on: ubuntu-latest\n  test:\n    runs-on: ubuntu-latest\n",
        );
        write(dir.path(), "pnpm-lock.yaml", "lockfileVersion: 9\n");
        write(
            dir.path(),
            "go.mod",
            "module github.com/acme/shop\n\ngo 1.23\n",
        );
        let extraction = extract(dir.path());

        let docker = tool_node(&extraction, "tool:local/tools@Dockerfile");
        assert_eq!(
            docker.props["settings"]["base_images"],
            serde_json::json!(["node:22-alpine"])
        );
        assert_eq!(
            docker.props["settings"]["exposed_ports"],
            serde_json::json!(["3000", "9229"])
        );

        let compose = tool_node(&extraction, "tool:local/tools@docker-compose.yml");
        assert_eq!(
            compose.props["settings"]["services"],
            serde_json::json!(["web", "db"])
        );

        let workflow = tool_node(&extraction, "tool:local/tools@.github/workflows/ci.yml");
        assert_eq!(workflow.props["settings"]["workflow"], "CI");
        assert_eq!(
            workflow.props["settings"]["triggers"],
            serde_json::json!(["push", "pull_request"])
        );
        assert_eq!(
            workflow.props["settings"]["jobs"],
            serde_json::json!(["build", "test"])
        );

        let pnpm = tool_node(&extraction, "tool:local/tools@pnpm");
        assert_eq!(pnpm.props["settings"]["lockfile"], "pnpm-lock.yaml");

        let go = tool_node(&extraction, "tool:local/tools@go");
        assert_eq!(go.props["settings"]["module"], "github.com/acme/shop");
        assert_eq!(go.props["settings"]["go"], "1.23");
    }

    #[test]
    fn extraction_is_deterministic_and_skips_vendored_trees() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "package.json", r#"{ "name": "x" }"#);
        write(
            dir.path(),
            "node_modules/dep/package.json",
            r#"{ "name": "dep" }"#,
        );
        let first = extract(dir.path());
        let second = extract(dir.path());
        assert_eq!(first.nodes, second.nodes);
        assert_eq!(first.edges, second.edges);
        assert!(!first.nodes.iter().any(|n| n.id.contains("node_modules")));
        assert_eq!(first.files, 1);
    }

    #[test]
    fn preflight_frameworks_quote_the_same_detection() {
        // #215 single-source invariant: the framework chip strings come out
        // of detect_in_file, so Preflight can never disagree with the graph.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{ "dependencies": { "react": "^19", "express": "^5" } }"#,
        );
        let detection = detect_in_file(
            "package.json",
            "package.json",
            &dir.path().join("package.json"),
        );
        assert_eq!(detection.framework_labels, vec!["Express", "React"]);
        // And the same call produced the graph-fact tools.
        assert!(detection.tools.iter().any(|t| t.name == "react"));
    }

    // AC-1063 (T-1063, #385): over a source capture the toolchain reader
    // reads every config file from the captured bytes and never the
    // checkout, except env files, which no capture retains (AC-0097). The
    // selection covers every file a detector proves something in.
    #[test]
    fn captured_config_files_are_read_instead_of_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let files = [
            (
                "package.json",
                "{\"name\":\"shop\",\"dependencies\":{\"react\":\"^18\"}}",
            ),
            ("tsconfig.json", "{\"compilerOptions\":{\"strict\":true}}"),
            (
                "Cargo.toml",
                "[package]\nname = \"shop\"\nedition = \"2021\"\n",
            ),
            ("go.mod", "module github.com/acme/shop\n\ngo 1.23\n"),
            ("pyproject.toml", "[project]\nname = \"shop\"\n"),
            ("requirements.txt", "flask\n"),
            ("pom.xml", "<project/>\n"),
            ("Dockerfile", "FROM node:22-alpine\nEXPOSE 3000\n"),
            (
                "docker-compose.yml",
                "services:\n  web:\n    image: shop:1\n",
            ),
            (
                ".github/workflows/ci.yml",
                "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n",
            ),
            ("pnpm-lock.yaml", "lockfileVersion: 9\n"),
            ("main.tf", "resource \"x\" \"y\" {}\n"),
            ("vite.config.ts", "export default {}\n"),
            ("ext/manifest.json", "{\"manifest_version\": 3}"),
            (".env", "PORT=3000\nAPI_KEY=hunter2\n"),
            ("README.md", "notes\n"),
        ];
        for (rel, text) in files {
            write(dir.path(), rel, text);
        }
        let id = SourceId {
            repo: "local/tools",
            commit: "c0ffee",
        };
        let allow: std::collections::BTreeSet<String> = ["PORT".to_string()].into();
        let facts = |out: &ToolExtraction| format!("{:?} {:?} {}", out.nodes, out.edges, out.files);
        let checkout = extract_dir_reading(dir.path(), &id, &allow, &mut |_| {}).unwrap();
        let before = facts(&checkout);

        let paths = toolchain_paths(dir.path()).unwrap();
        // Every file a detector proved something in is selected, but an env
        // file is never retained.
        for node in checkout.nodes.iter().filter(|node| node.label == "File") {
            let rel = node.props["path"].as_str().unwrap();
            assert_eq!(paths.contains(&rel.to_string()), rel != ".env", "{rel}");
        }
        assert!(!paths.contains(&"README.md".to_string()));
        let captured: BTreeMap<String, Vec<u8>> = paths
            .iter()
            .map(|rel| (rel.clone(), std::fs::read(dir.path().join(rel)).unwrap()))
            .collect();

        // After capture: edits, a deletion and a new config file.
        write(dir.path(), "package.json", "{\"name\":\"other\"}");
        write(dir.path(), "Dockerfile", "FROM python:3.12\n");
        write(dir.path(), "yarn.lock", "# yarn\n");
        std::fs::remove_file(dir.path().join("go.mod")).unwrap();
        let read = |rel: &str| {
            captured
                .get(rel)
                .cloned()
                .ok_or_else(|| std::io::Error::other("not captured"))
        };
        let keys: Vec<String> = captured.keys().cloned().collect();
        let after =
            extract_from_capture(dir.path(), &keys, &read, &id, &allow, &mut |_| {}).unwrap();
        assert_eq!(facts(&after), before);
        assert_ne!(
            facts(&extract_dir_reading(dir.path(), &id, &allow, &mut |_| {}).unwrap()),
            before
        );
    }

    // AC-1536: over a capture, toolchain facts are bound to the captured
    // config files the reader read, which exclude the checkout's env
    // files and every file no detector names.
    #[test]
    fn capture_reads_report_toolchain_facts_bound_to_the_captured_files() {
        let dir = tempfile::tempdir().unwrap();
        let files = [
            ("package.json", "{\"name\":\"web\"}"),
            ("Dockerfile", "FROM node:20\n"),
            (".env", "PORT=3000\n"),
            ("README.md", "notes\n"),
        ];
        for (rel, text) in files {
            write(dir.path(), rel, text);
        }
        let id = SourceId {
            repo: "local/tools",
            commit: "c0ffee",
        };
        let paths = toolchain_paths(dir.path()).unwrap();
        let captured: BTreeMap<String, Vec<u8>> = paths
            .iter()
            .map(|rel| (rel.clone(), std::fs::read(dir.path().join(rel)).unwrap()))
            .collect();
        let read = |rel: &str| {
            captured
                .get(rel)
                .cloned()
                .ok_or_else(|| std::io::Error::other("not captured"))
        };
        let keys: Vec<String> = captured.keys().cloned().collect();
        let none = std::collections::BTreeSet::new();
        let (facts, reads) =
            extract_from_capture_reading(dir.path(), &keys, &read, &id, &none, &mut |_| {})
                .unwrap();
        let plain =
            extract_from_capture(dir.path(), &keys, &read, &id, &none, &mut |_| {}).unwrap();
        assert_eq!(format!("{:?}", facts.nodes), format!("{:?}", plain.nodes));
        assert_eq!(format!("{:?}", facts.edges), format!("{:?}", plain.edges));
        assert!(reads.bound, "{reads:?}");
        assert_eq!(reads.read, ["Dockerfile", "package.json"]);
        assert!(
            facts
                .nodes
                .iter()
                .any(|node| node.id == "file:local/tools@.env"),
            "the checkout's env file is still read"
        );
    }
}
