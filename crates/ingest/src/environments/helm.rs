//! Helm charts and helmfile releases (#2167, ADR-0070 decision 9, SPEC-14
//! increment 3).
//!
//! - A `Chart.yaml` is a `Workload` of kind `helm-chart`, with the key paths
//!   of the chart's default `values.yaml`. Templates are not rendered, so the
//!   Kubernetes objects a chart would create are not recovered.
//! - A release of a `helmfile.yaml` is an `Environment`: a helmfile declares
//!   which chart is installed with which values files. A release whose chart
//!   is a directory of the repository `DECLARES` that chart, and the edge
//!   lists the key paths of every values file the release names.
//! - A values file beside a chart that no helmfile release names, whose name
//!   carries something other than a template word (`values-prod.yaml`),
//!   names an environment that no file declares: a name-only Gap.
//!
//! **No value is stored.** A values file yields key paths only, and it is
//! never cited by span: Helm values commonly hold credentials, so a span would
//! be a readable window into one. A release cites the helmfile entry that
//! names the file. Scalars stored from `Chart.yaml` and the helmfile pass
//! [`sanitize`]. `set`, `setString` and `secrets` entries are named in a Gap
//! and never read.

use super::{Draft, Findings, Props, TEMPLATE_WORDS, mapping_of};
use crate::pipelines::{Field, Span, field, fields, items, sanitize, span_of};
use crate::schedules::{first_mapping, scalar};
use source_walk::WalkedFile;
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::{Node as TsNode, Parser, Tree};

/// The names helmfile reads its state from.
pub(super) const HELMFILE_NAMES: [&str; 2] = ["helmfile.yaml", "helmfile.yml"];

/// The chart's default values file: Helm reads exactly this name.
const DEFAULT_VALUES: &str = "values.yaml";

/// How deep a values mapping is followed for key paths.
const MAX_KEY_DEPTH: usize = 8;

/// How many key paths one values file contributes; the rest are counted.
const MAX_KEYS: usize = 512;

/// Release keys whose content is never read: it is set inline and may be a
/// credential.
const UNREAD_RELEASE_KEYS: &[&str] = &["set", "setString", "secrets", "valuesTemplate"];

/// `values.yaml` and its variants (`values-prod.yaml`, `values.dev.yml`).
pub(super) fn values_name(name: &str) -> bool {
    (name.ends_with(".yaml") || name.ends_with(".yml"))
        && name
            .strip_prefix("values")
            .is_some_and(|rest| rest.starts_with(['.', '-', '_']))
}

/// True for a values file whose name carries something other than a
/// template word: `values-prod.yaml`, not `values.yaml` or
/// `values.example.yaml`.
pub(super) fn values_names_an_environment(name: &str) -> bool {
    let stem = name
        .strip_suffix(".yaml")
        .or_else(|| name.strip_suffix(".yml"))
        .and_then(|stem| stem.strip_prefix("values"))
        .unwrap_or_default();
    stem.split(['.', '-', '_'])
        .filter(|part| !part.is_empty())
        .any(|part| !TEMPLATE_WORDS.contains(&part.to_ascii_lowercase().as_str()))
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `entry` resolved lexically against `dir`; `None` when it leaves the
/// repository or is absolute.
fn normalise(dir: &str, entry: &str) -> Option<String> {
    if entry.starts_with('/') || entry.starts_with('~') || entry.contains('\\') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|part| !part.is_empty()).collect();
    for part in entry.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// A chart reference Helm resolves on the local file system.
fn local_chart(chart: &str) -> bool {
    chart.starts_with("./") || chart.starts_with("../") || chart == "." || chart == ".."
}

fn parse_yaml(text: &str) -> Result<Tree, (&'static str, Span)> {
    let whole = (0, text.len() as u64);
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .map_err(|_| ("the YAML grammar is unavailable", whole))?;
    let tree = parser
        .parse(text, None)
        .ok_or(("the file is not valid YAML", whole))?;
    if tree.root_node().has_error() {
        return Err(("the file is not valid YAML", whole));
    }
    let documents = {
        let root = tree.root_node();
        let mut walk = root.walk();
        root.named_children(&mut walk)
            .filter(|node| node.kind() == "document")
            .count()
    };
    if documents > 1 {
        return Err(("the file is not one YAML document", whole));
    }
    Ok(tree)
}

/// One `Chart.yaml`.
#[derive(Debug)]
pub(super) struct Chart {
    pub(super) props: Props,
    pub(super) span: Span,
}

/// A `Chart.yaml`, or why it yields no `Workload`.
pub(super) fn chart(text: &str) -> Result<Chart, (&'static str, Span)> {
    let whole = (0, text.len() as u64);
    let tree = parse_yaml(text)?;
    let top = first_mapping(tree.root_node()).ok_or(("the file is not a mapping", whole))?;
    let span = span_of(top);
    let top = fields(text, top)
        .map_err(|_| ("the file is not a mapping of distinct literal keys", whole))?;
    let name = field(&top, "name")
        .and_then(|name| scalar(text, name.value?))
        .filter(|name| !name.is_empty() && !name.contains("{{"))
        .ok_or(("the chart states no plain scalar `name`", span))?;
    let mut draft = Draft {
        props: Props::new(),
        redactions: 0,
        interpolated: BTreeSet::new(),
    };
    draft.set("kind", "helm-chart");
    draft.text("name", &name);
    for (key, prop) in [
        ("version", "version"),
        ("appVersion", "app_version"),
        ("type", "chart_type"),
    ] {
        if let Some(value) = field(&top, key).and_then(|f| scalar(text, f.value?)) {
            draft.text(prop, &value);
        }
    }
    if let Some(dependencies) = field(&top, "dependencies").and_then(|f| items(f.value?)) {
        let names: Vec<String> = dependencies
            .into_iter()
            .filter_map(|item| {
                let entry = fields(text, item).ok()?;
                scalar(text, field(&entry, "name")?.value?)
            })
            .collect();
        draft.list("dependencies", names);
    }
    Ok(Chart {
        props: draft.finish(),
        span,
    })
}

/// The key paths a values file declares: leaf paths through mappings, a
/// sequence ending its path with `[]`, sorted. The second value counts the
/// paths left out beyond [`MAX_KEYS`].
pub(super) fn values_keys(text: &str) -> Result<(Vec<String>, usize), &'static str> {
    let tree = parse_yaml(text).map_err(|(reason, _)| reason)?;
    let Some(top) = first_mapping(tree.root_node()) else {
        // An empty or comment-only values file declares nothing.
        return if text.lines().all(|line| {
            let line = line.trim();
            line.is_empty() || line.starts_with('#') || line == "---" || line == "{}"
        }) {
            Ok((Vec::new(), 0))
        } else {
            Err("the file is not a mapping")
        };
    };
    let mut keys = BTreeSet::new();
    collect_keys(text, top, "", 0, &mut keys)?;
    let total = keys.len();
    let kept: Vec<String> = keys.into_iter().take(MAX_KEYS).collect();
    Ok((kept, total.saturating_sub(MAX_KEYS)))
}

fn collect_keys(
    text: &str,
    node: TsNode,
    prefix: &str,
    depth: usize,
    out: &mut BTreeSet<String>,
) -> Result<(), &'static str> {
    let entries =
        fields(text, node).map_err(|_| "a mapping in the file is not of distinct literal keys")?;
    for entry in entries {
        let (key, _) = sanitize(&entry.name);
        let path = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        match entry.value {
            Some(value) if depth + 1 < MAX_KEY_DEPTH && mapping_of(value).is_some() => {
                let before = out.len();
                collect_keys(text, value, &path, depth + 1, out)?;
                if out.len() == before {
                    // An empty mapping is still a declared key.
                    out.insert(path);
                }
            }
            Some(value) if items(value).is_some() => {
                out.insert(format!("{path}[]"));
            }
            _ => {
                out.insert(path);
            }
        }
    }
    Ok(())
}

/// One release of a helmfile.
#[derive(Debug)]
pub(super) struct Release {
    pub(super) release: String,
    pub(super) props: Props,
    pub(super) span: Span,
    pub(super) chart: Option<(String, Span)>,
    pub(super) values: Vec<(String, Span)>,
    pub(super) findings: Findings,
}

/// One helmfile: its releases, the environment names it declares, and what
/// it states that is not read.
#[derive(Debug)]
pub(super) struct Helmfile {
    pub(super) releases: Vec<Release>,
    pub(super) environments: Vec<String>,
    pub(super) findings: Findings,
}

/// A helmfile, or why it yields no `Environment`.
/// The repository paths the helmfile at `rel` names as values files, in
/// the order its releases name them.
pub(super) fn named_values(rel: &str, text: &str) -> Vec<String> {
    let Ok(helmfile) = helmfile(text) else {
        return Vec::new();
    };
    helmfile
        .releases
        .iter()
        .flat_map(|release| release.values.iter())
        .filter_map(|(path, _)| normalise(dir_of(rel), path))
        .collect()
}

pub(super) fn helmfile(text: &str) -> Result<Helmfile, (&'static str, Span)> {
    let whole = (0, text.len() as u64);
    if text.contains("{{") {
        return Err((
            "the helmfile is a template; templates are not rendered",
            whole,
        ));
    }
    let tree = parse_yaml(text)?;
    let top = first_mapping(tree.root_node()).ok_or(("the file is not a mapping", whole))?;
    let top = fields(text, top)
        .map_err(|_| ("the file is not a mapping of distinct literal keys", whole))?;
    let mut findings = Findings::default();
    let environments = field(&top, "environments")
        .and_then(|f| fields(text, f.value?).ok())
        .map(|entries| {
            entries
                .into_iter()
                .filter_map(|entry| {
                    // Stored names pass `sanitize`; a withheld one is not kept.
                    let (safe, redacted) = sanitize(&entry.name);
                    if redacted == 0 {
                        Some(safe)
                    } else {
                        findings.add(
                            "an environment name is withheld, so it is not recorded",
                            entry.key_span,
                        );
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    for key in ["bases", "helmfiles"] {
        if let Some(entry) = field(&top, key) {
            findings.add(
                format!("`{key}` names other helmfiles, which are not followed"),
                entry.key_span,
            );
        }
    }
    let mut releases = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(entry) = field(&top, "releases") {
        let Some(list) = entry.value.and_then(items) else {
            findings.add("`releases` is not a sequence", entry.span);
            return Ok(Helmfile {
                releases,
                environments,
                findings,
            });
        };
        for item in list {
            let span = span_of(item);
            let Ok(body) = fields(text, item) else {
                findings.add("a release is not a mapping of distinct literal keys", span);
                continue;
            };
            let Some(name) = field(&body, "name").and_then(|f| scalar(text, f.value?)) else {
                findings.add("a release states no plain scalar `name`", span);
                continue;
            };
            let (safe, redacted) = sanitize(&name);
            if redacted > 0 || name.is_empty() || !seen.insert(name.clone()) {
                findings.add(
                    "a release name is empty, repeated or withheld, so it is not recorded",
                    span,
                );
                continue;
            }
            releases.push(release(text, safe, &body, span));
        }
    }
    Ok(Helmfile {
        releases,
        environments,
        findings,
    })
}

fn release(text: &str, name: String, body: &[Field], span: Span) -> Release {
    let mut findings = Findings::default();
    let mut draft = Draft {
        props: Props::new(),
        redactions: 0,
        interpolated: BTreeSet::new(),
    };
    draft.set("release", name.clone());
    if let Some(namespace) = field(body, "namespace")
        && let Some(value) = namespace.value.and_then(|value| scalar(text, value))
    {
        draft.text("name", &value);
        draft.set(
            "name_span",
            serde_json::json!([namespace.span.0, namespace.span.1]),
        );
    }
    let chart = field(body, "chart").and_then(|f| Some((scalar(text, f.value?)?, f.span)));
    match &chart {
        Some((chart, _)) => draft.text("chart", chart),
        None => findings.add("the release states no plain scalar `chart`", span),
    }
    let mut values = Vec::new();
    if let Some(entry) = field(body, "values") {
        match entry.value.and_then(items) {
            Some(list) => {
                for item in list {
                    match scalar(text, item) {
                        Some(path) => values.push((path, span_of(item))),
                        None => findings.add(
                            "an inline `values` entry is not read; only values files are",
                            span_of(item),
                        ),
                    }
                }
            }
            None => findings.add("`values` is not a sequence", entry.span),
        }
    }
    for key in UNREAD_RELEASE_KEYS {
        if let Some(entry) = field(body, key) {
            findings.add(
                format!("`{key}` is not read: it may hold credentials"),
                entry.key_span,
            );
        }
    }
    let files: Vec<String> = values.iter().map(|(path, _)| path.clone()).collect();
    if !files.is_empty() {
        draft.list("values_files", files);
    }
    Release {
        release: name,
        props: draft.finish(),
        span,
        chart,
        values,
        findings,
    }
}

/// Every chart, helmfile and values file of the walk, parsed once.
pub(super) struct Index<'a> {
    files: BTreeMap<&'a str, &'a WalkedFile>,
    pub(super) helmfiles: BTreeMap<&'a str, Result<Helmfile, (&'static str, Span)>>,
    /// Values files some helmfile release names.
    named: BTreeSet<String>,
}

/// What a release's local chart reference resolves to.
pub(super) enum ChartTarget {
    /// The repo-relative path of the chart's `Chart.yaml`.
    Chart(String),
    /// Why no chart in the repository is declared.
    Problem(String),
}

impl<'a> Index<'a> {
    pub(super) fn read(walked: &'a [WalkedFile], src: &super::Source) -> Self {
        let mut index = Self {
            files: walked
                .iter()
                .map(|file| (file.rel.as_str(), file))
                .collect(),
            helmfiles: BTreeMap::new(),
            named: BTreeSet::new(),
        };
        for file in walked {
            if !HELMFILE_NAMES.contains(&file.name.as_str()) {
                continue;
            }
            let parsed = match src.text(&file.rel) {
                Ok(text) => helmfile(&text),
                Err(_) => Err((
                    "the file is not readable UTF-8 text",
                    (0, src.len_or_zero(&file.rel)),
                )),
            };
            if let Ok(helmfile) = &parsed {
                for release in &helmfile.releases {
                    for (path, _) in &release.values {
                        if let Some(path) = normalise(dir_of(&file.rel), path) {
                            index.named.insert(path);
                        }
                    }
                }
            }
            index.helmfiles.insert(&file.rel, parsed);
        }
        index
    }

    /// True when `rel` is a values file of a chart: a values file name in a
    /// directory that holds a walked `Chart.yaml`.
    pub(super) fn chart_values(&self, rel: &str, name: &str) -> bool {
        values_name(name)
            && self
                .files
                .contains_key(join(dir_of(rel), "Chart.yaml").as_str())
    }

    pub(super) fn named(&self, rel: &str) -> bool {
        self.named.contains(rel)
    }

    /// Where the walked file `rel` is on disk.
    pub(super) fn path(&self, rel: &str) -> Option<&'a std::path::Path> {
        self.files.get(rel).map(|file| file.path.as_path())
    }

    /// The chart's default values file, when the walk found one.
    pub(super) fn default_values(&self, chart_rel: &str) -> Option<String> {
        Some(join(dir_of(chart_rel), DEFAULT_VALUES))
            .filter(|rel| self.files.contains_key(rel.as_str()))
    }

    pub(super) fn chart_of(&self, helmfile_rel: &str, chart: &str) -> ChartTarget {
        if !local_chart(chart) {
            return ChartTarget::Problem(format!(
                "chart `{}` is not a directory of the repository; its objects are not recovered",
                sanitize(chart).0
            ));
        }
        let Some(dir) = normalise(dir_of(helmfile_rel), chart) else {
            return ChartTarget::Problem("the chart path leaves the repository".into());
        };
        let rel = join(&dir, "Chart.yaml");
        if self.files.contains_key(rel.as_str()) {
            ChartTarget::Chart(rel)
        } else {
            ChartTarget::Problem("the chart path names no `Chart.yaml` among the files read".into())
        }
    }

    /// A values entry of a release, resolved to a walked file.
    pub(super) fn values_file(&self, helmfile_rel: &str, entry: &str) -> Result<String, String> {
        if entry.contains("{{") {
            return Err(format!(
                "values file `{}` is templated and not rendered",
                sanitize(entry).0
            ));
        }
        let Some(rel) = normalise(dir_of(helmfile_rel), entry) else {
            return Err("a values file path leaves the repository".into());
        };
        if self.files.contains_key(rel.as_str()) {
            Ok(rel)
        } else {
            Err(format!(
                "values file `{}` is not among the files read",
                sanitize(entry).0
            ))
        }
    }
}
