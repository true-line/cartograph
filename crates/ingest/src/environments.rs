//! Environments and what they declare (#1791, AC-0661, AC-0662, ADR-0070,
//! SPEC-14): Compose files, kustomization files and Kubernetes manifests as
//! T0 facts, each citing the exact byte span that declares it.
//!
//! An environment is a declared deployable set. A Compose file that parses
//! and has a `services` mapping is one, and so is every kustomization file
//! (the `kustomize` module); the pod-bearing Kubernetes manifest kinds are
//! workloads (the `kubernetes` module). For a Compose file:
//!
//! - `Environment` (`env:{repo}@{path}`): one Compose file. Its identity is
//!   the file's path. It has a `name` only where the file states a top-level
//!   scalar `name`; a path is never turned into a name.
//! - `Workload` (`workload:{repo}@{path}#{service}`): one service, with its
//!   image, build context, ports, environment key names, environment files,
//!   dependencies and profiles as written.
//! - `DECLARES` (environment → workload) and `DEPENDS_ON` (workload →
//!   workload of the same file).
//! - `Gap` (`gap:environment:{repo}@{path}[#{service}]`): a file that cannot
//!   be read, a construct that is not evaluated (`extends`, `include`, an
//!   anchor, alias or tag), a property written in a form this reader does
//!   not read, or a `depends_on` naming a service the file does not declare.
//!   One Gap per file and one per service; it states every such construct.
//!
//! `DEPENDS_ON` between two `Workload` nodes is read by nothing else: the
//! topology map keeps an edge only between `Resource` nodes, and the flow
//! tracer follows `DEPENDS_ON` only into a `Gap`. An undeclared dependency is
//! therefore a Gap and never an edge.
//!
//! **Name-only environments.** An environment file whose name carries
//! something other than a template word (`.env.production`, `staging.env`)
//! names an environment that no file declares. It yields a Gap and no
//! `Environment`. That Gap has **no evidence span**: a span on an environment
//! file is a readable window into a file that holds secrets, which the
//! toolchain reader's rule forbids (AC-0097).
//!
//! **No value of an environment key is stored.** `environment` yields key
//! names only. Every stored scalar passes [`crate::pipelines::sanitize`]:
//! recognized secrets are replaced and any token-shaped run is withheld, with
//! the count disclosed as `redactions`. A scalar containing `${` is stored as
//! written and its property is marked under `interpolated`; nothing is
//! resolved.
//!
//! Nothing is merged across files: which Compose files a deployment combines
//! is not declared in them.
//!
//! **Kubernetes and kustomize.** A `Deployment`, `StatefulSet`, `DaemonSet`
//! or `Job` document is a `Workload`
//! (`workload:{repo}@{path}#{document index}:{Kind}/{name}`) with its
//! containers. A kustomization file is an `Environment`, named only where it
//! states a `namespace`. It `DECLARES` every such workload its `resources`
//! chain reaches inside the repository, and `PATCHES` each one a patch file
//! names, stating what the patch writes. Nothing is merged and no
//! transformer is applied. A file that a kustomization names as a patch and
//! none names as a resource is read only as a patch.
//!
//! **Helm (the `helm` module).** A `Chart.yaml` is a `Workload` with the key
//! paths of its default values file; templates are not rendered. A
//! helmfile release is an `Environment` that `DECLARES` its chart when the
//! chart is a directory of the repository, listing the key paths of each
//! values file it names. A values file named only by its file name is a
//! name-only Gap. No value of a values file is stored or cited.
//!
//! A manifest of any other kind is named as present and not read
//! ([`unread`]), by its kind.
//!
//! This tier never calls an LLM.

use crate::pipelines::{Field, Span, field, fields, items, sanitize, span_of};
use crate::schedules::{contains_kind, first_mapping, scalar, workflow_file};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use kustomize::{Reach, Tree};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

/// Extractor id recorded on every environment fact.
pub const ENVIRONMENT_EXTRACTOR_ID: &str = "t0.environments";

/// Why a name-only environment file is a Gap.
pub const NAME_ONLY_REASON: &str =
    "an environment is named only by this file's name; no file declares it";

/// What every present-and-not-read message ends with.
const NOT_READ: &str = "present and not read: no installed producer recovers environments from it";

/// What the finding kind of an unread manifest document starts with; the
/// document's `kind` follows.
const UNREAD_MANIFEST: &str = "kubernetes-manifest:";

/// YAML constructs that are stepped over and stated, never evaluated.
const UNEVALUATED_YAML: &[(&str, &str)] = &[
    ("anchor", "a YAML anchor is not evaluated"),
    ("alias", "a YAML alias is not evaluated"),
    ("tag", "a YAML tag is not evaluated"),
];

type Props = serde_json::Map<String, serde_json::Value>;

/// Repository identity for environment facts.
#[derive(Debug, Clone, Copy)]
pub struct SourceId<'a> {
    /// Repository identity (`owner/name` or `local/<name>`).
    pub repo: &'a str,
    /// Commit the tree was read at.
    pub commit: &'a str,
}

/// Facts from one directory walk.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentExtraction {
    /// `Environment`, `Workload` and `Gap` nodes.
    pub nodes: Vec<Node>,
    /// `DECLARES`, `DEPENDS_ON` and `PATCHES` edges.
    pub edges: Vec<Edge>,
    /// Compose files, kustomization files, manifest files and name-only
    /// environment files that yielded a fact.
    pub files: u64,
}

/// Something present and not read, as Preflight reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    /// Finding kind: `helm-chart`, `helm-values-file`, or
    /// `kubernetes-manifest:` followed by the document's `kind`.
    pub kind: String,
    /// 1-based line it starts at.
    pub line: u64,
    /// What is present and why it was not read.
    pub message: String,
}

/// True when `kind` is a finding kind this module produces. Coverage quotes
/// such a finding's own message as its reason.
pub fn is_limit_kind(kind: &str) -> bool {
    kind.starts_with(UNREAD_MANIFEST)
}

fn yaml(name: &str) -> bool {
    name.ends_with(".yml") || name.ends_with(".yaml")
}

/// True for the Compose file names this reader reads: `compose` or
/// `docker-compose`, at most one dotted infix, `.yaml` or `.yml`.
pub fn compose_file(name: &str) -> bool {
    let Some(stem) = name
        .strip_suffix(".yaml")
        .or_else(|| name.strip_suffix(".yml"))
    else {
        return false;
    };
    ["docker-compose", "compose"].iter().any(|base| {
        stem.strip_prefix(base).is_some_and(|rest| {
            rest.is_empty()
                || rest
                    .strip_prefix('.')
                    .is_some_and(|infix| !infix.is_empty() && !infix.contains('.'))
        })
    })
}

/// Words that mark an environment file as a template for any environment.
const TEMPLATE_WORDS: &[&str] = &["example", "sample", "template"];

/// True when the environment file `name` carries an infix or prefix that is
/// not a template word, so its name alone names an environment. `name` is
/// one the toolchain reader recognises ([`crate::toolchain::is_env_name`]).
fn names_an_environment(name: &str) -> bool {
    if !crate::toolchain::is_env_name(name) {
        return false;
    }
    let rest = if let Some(infix) = name.strip_prefix(".env") {
        infix
    } else if let Some(prefix) = name.strip_suffix(".env") {
        prefix
    } else {
        // `env.example`, `env.sample`, `env.template`.
        return false;
    };
    rest.split('.')
        .filter(|part| !part.is_empty())
        .any(|part| !TEMPLATE_WORDS.contains(&part.to_ascii_lowercase().as_str()))
}

/// The largest YAML file opened to see whether it is a Kubernetes manifest.
const MAX_MANIFEST_PROBE_BYTES: u64 = 1024 * 1024;

/// True for a YAML file that may hold Kubernetes manifest documents: not a
/// Compose file, a workflow or a kustomization.
fn manifest_candidate(name: &str, rel: &str) -> bool {
    yaml(name)
        && !compose_file(name)
        && !workflow_file(rel)
        && !kustomize::FILE_NAMES.contains(&name)
}

/// What `rel` holds that is present and not read: a Helm chart or values
/// file, or one entry for each Kubernetes manifest document of a kind this
/// reader does not read. A YAML file is read only to see which documents of
/// it have top-level `apiVersion` and `kind` scalars. The pod-bearing kinds
/// are read, a `batch/*` `CronJob` is the schedule reader's, and a
/// kustomization is read, so none of them is named.
pub fn unread(name: &str, rel: &str, path: &Path) -> Vec<Unread> {
    // Helm charts, values files and helmfiles are read (the `helm` module).
    if name == "Chart.yaml"
        || helm::HELMFILE_NAMES.contains(&name)
        || (helm::values_name(name) && path.with_file_name("Chart.yaml").is_file())
        || !manifest_candidate(name, rel)
    {
        return Vec::new();
    }
    // Bounded: a manifest is small, and a larger YAML file is not opened to
    // look for two keys.
    if std::fs::metadata(path).map_or(true, |meta| meta.len() > MAX_MANIFEST_PROBE_BYTES) {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    if !kubernetes::may_hold_manifest(&text) {
        return Vec::new();
    }
    kubernetes::documents(&text)
        .unwrap_or_default()
        .into_iter()
        .filter(|document| document.read.is_none() && !document.cronjob)
        .map(|document| match kubernetes::plain_kind(&document.kind) {
            Some(kind) => Unread {
                kind: format!("{UNREAD_MANIFEST}{kind}"),
                line: document.line,
                message: format!("Kubernetes `{kind}` manifest is {NOT_READ}"),
            },
            // A kind is quoted only when it is a plain name.
            None => Unread {
                kind: format!("{UNREAD_MANIFEST}other"),
                line: document.line,
                message: format!(
                    "Kubernetes manifest of a kind not written as a plain name is {NOT_READ}"
                ),
            },
        })
        .collect()
}

/// Props under construction: what redaction replaced and which properties
/// hold an unresolved `${`.
#[derive(Default)]
struct Draft {
    props: Props,
    redactions: u64,
    interpolated: BTreeSet<&'static str>,
}

impl Draft {
    fn set(&mut self, key: &str, value: impl Into<serde_json::Value>) {
        self.props.insert(key.into(), value.into());
    }

    /// One scalar from the source, redacted. An unresolved `${` marks `key`.
    fn safe(&mut self, key: &'static str, value: &str) -> String {
        if value.contains("${") {
            self.interpolated.insert(key);
        }
        let (safe, count) = sanitize(value);
        self.redactions += count;
        safe
    }

    fn text(&mut self, key: &'static str, value: &str) {
        let safe = self.safe(key, value);
        self.set(key, safe);
    }

    fn list(&mut self, key: &'static str, values: impl IntoIterator<Item = String>) {
        let safe: Vec<String> = values
            .into_iter()
            .map(|value| self.safe(key, &value))
            .collect();
        self.set(key, safe);
    }

    fn finish(mut self) -> Props {
        if !self.interpolated.is_empty() {
            let marks: Props = std::mem::take(&mut self.interpolated)
                .into_iter()
                .map(|key| (key.to_string(), true.into()))
                .collect();
            self.set("interpolated", marks);
        }
        if self.redactions > 0 {
            self.set("redactions", self.redactions);
        }
        self.props
    }
}

/// What a Gap states: the constructs, in the order found, and their spans.
#[derive(Debug, Clone, Default)]
struct Findings {
    reasons: Vec<String>,
    spans: BTreeSet<Span>,
}

impl Findings {
    fn add(&mut self, reason: impl Into<String>, span: Span) {
        let reason = reason.into();
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
        self.spans.insert(span);
    }
}

#[derive(Debug)]
struct Service {
    name: String,
    props: Props,
    /// The service key.
    key_span: Span,
    /// The service's mapping; the whole entry when it has none.
    span: Span,
    /// `depends_on` entries: the name as written and its span.
    depends_on: Vec<(String, Span)>,
    findings: Findings,
}

/// One Compose file with a `services` mapping.
#[derive(Debug)]
struct Compose {
    props: Props,
    /// The `services` key.
    span: Span,
    services: Vec<Service>,
    findings: Findings,
}

/// The first node of `kind` at or beneath `node`.
fn find_kind<'t>(node: TsNode<'t>, kind: &str) -> Option<TsNode<'t>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut walk = node.walk();
    let children: Vec<TsNode<'t>> = node.named_children(&mut walk).collect();
    children
        .into_iter()
        .find_map(|child| find_kind(child, kind))
}

/// The mapping `node` holds, beneath its wrappers. An anchor or tag on it is
/// stepped over here and reported by the caller.
fn mapping_of(node: TsNode) -> Option<TsNode> {
    let mut node = node;
    loop {
        match node.kind() {
            "block_mapping" | "flow_mapping" => return Some(node),
            "block_node" | "flow_node" => {
                let mut walk = node.walk();
                let children: Vec<TsNode> = node
                    .named_children(&mut walk)
                    .filter(|child| !matches!(child.kind(), "comment" | "anchor" | "tag"))
                    .collect();
                let [only] = children.as_slice() else {
                    return None;
                };
                node = *only;
            }
            _ => return None,
        }
    }
}

/// The entries of the mapping under `name` in `body`.
fn section<'t>(text: &str, body: &[Field<'t>], name: &str) -> Option<Vec<Field<'t>>> {
    fields(text, mapping_of(field(body, name)?.value?)?).ok()
}

/// A service name as Compose allows it, and short enough not to be withheld.
fn service_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !core_redact::is_token_shaped(name)
}

fn not_read(key: &str) -> String {
    format!("`{key}` is not written in a form this reader reads")
}

/// A Compose file, or why it yields no `Environment`.
fn compose(text: &str) -> Result<Compose, (&'static str, Span)> {
    let whole = (0, text.len() as u64);
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .map_err(|_| ("the YAML grammar is unavailable", whole))?;
    let tree = parser
        .parse(text, None)
        .ok_or(("the file is not valid YAML", whole))?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(("the file is not valid YAML", whole));
    }
    let mut walk = root.walk();
    if root
        .named_children(&mut walk)
        .filter(|node| node.kind() == "document")
        .count()
        != 1
    {
        return Err(("the file is not one YAML document", whole));
    }
    let no_services = ("the file has no `services` mapping", whole);
    let top = first_mapping(root).ok_or(no_services)?;
    let top = fields(text, top)
        .map_err(|_| ("the file is not a mapping of distinct literal keys", whole))?;
    let services = field(&top, "services").ok_or(no_services)?;
    let declared = services
        .value
        .and_then(mapping_of)
        .ok_or((no_services.0, services.span))?;
    let declared = fields(text, declared).map_err(|_| {
        (
            "`services` is not a mapping of distinct literal service names",
            services.span,
        )
    })?;

    let mut draft = Draft::default();
    let mut findings = Findings::default();
    if let Some(name) = field(&top, "name")
        && let Some(value) = name.value.and_then(|value| scalar(text, value))
    {
        draft.text("name", &value);
        draft.set("name_span", vec![name.span.0, name.span.1]);
    }
    if let Some(include) = field(&top, "include") {
        findings.add("top-level `include` is not evaluated", include.span);
    }
    let names: BTreeSet<&str> = declared
        .iter()
        .map(|service| service.name.as_str())
        .filter(|name| service_name(name))
        .collect();
    let mut out = Vec::new();
    for entry in &declared {
        if !service_name(&entry.name) {
            findings.add(
                "a service name is not a plain name; that service is not recorded",
                entry.key_span,
            );
            continue;
        }
        out.push(service(text, entry, &names));
    }
    Ok(Compose {
        props: draft.finish(),
        span: services.key_span,
        services: out,
        findings,
    })
}

/// One service: the properties written literally, and what was not read.
fn service(text: &str, entry: &Field, names: &BTreeSet<&str>) -> Service {
    let mut draft = Draft::default();
    let mut findings = Findings::default();
    draft.set("kind", "compose-service");
    draft.set("name", entry.name.clone());
    let body = entry.value.and_then(mapping_of);
    let span = body.map_or(entry.span, span_of);
    if let Some(value) = entry.value {
        for (kind, what) in [
            ("anchor", "a YAML anchor is not evaluated"),
            ("alias", "a YAML alias is not evaluated"),
            ("tag", "a YAML tag is not evaluated"),
        ] {
            if contains_kind(value, &[kind])
                && let Some(found) = find_kind(value, kind)
            {
                findings.add(what, span_of(found));
            }
        }
    }
    let mut depends_on = Vec::new();
    let body = match body.map(|body| fields(text, body)) {
        Some(Ok(body)) => body,
        _ => {
            findings.add(
                "the service is not a mapping of distinct literal keys",
                entry.span,
            );
            Vec::new()
        }
    };
    if let Some(extends) = field(&body, "extends") {
        findings.add("`extends` is not evaluated", extends.span);
    }
    let value = |name: &str| field(&body, name).and_then(|found| Some((found, found.value?)));

    if let Some((found, node)) = value("image") {
        match scalar(text, node) {
            Some(image) => draft.text("image", &image),
            None => findings.add(not_read("image"), found.span),
        }
    }
    // The context path only: a string, or the `context` of a mapping.
    if let Some((found, node)) = value("build") {
        let context = scalar(text, node).map(Some).or_else(|| {
            let build = fields(text, mapping_of(node)?).ok()?;
            Some(
                match field(&build, "context").and_then(|context| context.value) {
                    Some(context) => Some(scalar(text, context)?),
                    None => None,
                },
            )
        });
        match context {
            Some(Some(context)) => draft.text("build", &context),
            Some(None) => {}
            None => findings.add(not_read("build"), found.span),
        }
    }
    if let Some((found, node)) = value("ports") {
        match items(node).and_then(|ports| {
            ports
                .into_iter()
                .map(|item| port(text, item))
                .collect::<Option<Vec<String>>>()
        }) {
            Some(ports) => draft.list("ports", ports),
            None => findings.add(not_read("ports"), found.span),
        }
    }
    if let Some((found, node)) = value("environment") {
        match environment_keys(text, node) {
            Some(keys) => draft.list("environment_keys", keys),
            None => findings.add(not_read("environment"), found.span),
        }
    }
    if let Some((found, node)) = value("env_file") {
        match env_files(text, node) {
            Some(files) => draft.list("env_files", files),
            None => findings.add(not_read("env_file"), found.span),
        }
    }
    if let Some((found, node)) = value("depends_on") {
        match dependencies(text, node) {
            Some(entries) => {
                let named: BTreeSet<String> =
                    entries.iter().map(|(name, _)| name.clone()).collect();
                draft.list("depends_on", named);
                for (name, span) in &entries {
                    if !names.contains(name.as_str()) {
                        findings.add(
                            format!(
                                "`depends_on` names `{}`, a service this file does not declare",
                                sanitize(name).0
                            ),
                            *span,
                        );
                    }
                }
                depends_on = entries;
            }
            None => findings.add(not_read("depends_on"), found.span),
        }
    }
    if let Some((found, node)) = value("profiles") {
        match items(node).and_then(|profiles| {
            profiles
                .into_iter()
                .map(|item| scalar(text, item))
                .collect::<Option<Vec<String>>>()
        }) {
            Some(profiles) => draft.list("profiles", profiles),
            None => findings.add(not_read("profiles"), found.span),
        }
    }
    Service {
        name: entry.name.clone(),
        props: draft.finish(),
        key_span: entry.key_span,
        span,
        depends_on,
        findings,
    }
}

/// Keys of a long-form port, in the order a row states them.
const PORT_KEYS: &[&str] = &[
    "name",
    "target",
    "published",
    "host_ip",
    "protocol",
    "app_protocol",
    "mode",
];

/// One `ports` entry as written: the short form's scalar, or the long form's
/// known keys as `key=value` in the order the file writes them.
fn port(text: &str, item: TsNode) -> Option<String> {
    if let Some(short) = scalar(text, item) {
        return Some(short);
    }
    let long = fields(text, mapping_of(item)?).ok()?;
    let mut parts = Vec::new();
    for part in &long {
        if PORT_KEYS.contains(&part.name.as_str()) {
            parts.push(format!("{}={}", part.name, scalar(text, part.value?)?));
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The key names of `environment`, sorted, each once. A list entry is
/// `KEY=value` or `KEY`; only the text before the first `=` is kept. A
/// mapping gives its keys. No value is read.
fn environment_keys(text: &str, node: TsNode) -> Option<Vec<String>> {
    let mut names = BTreeSet::new();
    if let Some(entries) = items(node) {
        for entry in entries {
            // The source text, so a quoted entry with escapes is still cut
            // at its `=` and never decoded.
            let raw = text[entry.byte_range()].trim();
            let raw = raw.trim_start_matches(['"', '\'']);
            let name = raw.split('=').next().unwrap_or("").trim();
            let name = name.trim_end_matches(['"', '\'']);
            let plain = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
            if !plain {
                return None;
            }
            names.insert(name.to_string());
        }
    } else {
        for entry in fields(text, mapping_of(node)?).ok()? {
            names.insert(entry.name);
        }
    }
    Some(names.into_iter().collect())
}

/// `env_file` paths in the order written: a string, a list of strings, or a
/// list of mappings with a `path`.
fn env_files(text: &str, node: TsNode) -> Option<Vec<String>> {
    if let Some(path) = scalar(text, node) {
        return Some(vec![path]);
    }
    items(node)?
        .into_iter()
        .map(|item| {
            scalar(text, item).or_else(|| {
                let long = fields(text, mapping_of(item)?).ok()?;
                scalar(text, field(&long, "path")?.value?)
            })
        })
        .collect()
}

/// `depends_on` entries, each with its span: a list of service names, or a
/// mapping keyed by service name.
fn dependencies(text: &str, node: TsNode) -> Option<Vec<(String, Span)>> {
    if let Some(entries) = items(node) {
        return entries
            .into_iter()
            .map(|entry| Some((scalar(text, entry)?, span_of(entry))))
            .collect();
    }
    Some(
        fields(text, mapping_of(node)?)
            .ok()?
            .into_iter()
            .map(|entry| (entry.name, entry.span))
            .collect(),
    )
}

/// Walk `root` and turn every Compose file, kustomization file and
/// Kubernetes manifest into environment facts, and every name-only
/// environment file into a Gap. Deterministic for a given tree: the walk is
/// sorted, and every id derives from the path and the service name, or from
/// the path and the document.
pub fn extract_dir(root: &Path, id: &SourceId) -> std::io::Result<EnvironmentExtraction> {
    let walked = walk(root)?;
    let read = |rel: &str| std::fs::read(root.join(rel));
    let len = |rel: &str| std::fs::metadata(root.join(rel)).map(|meta| meta.len());
    Ok(extract_walked(
        &walked,
        &Source {
            read: &read,
            len: &len,
        },
        id,
    ))
}

/// The files [`extract_dir`] would open under `root`, in path order: every
/// file [`is_environment_file`] selects and every walked file a selected
/// helmfile names as a values file. A source capture retains them (AC-1059).
pub fn environment_paths(root: &Path) -> std::io::Result<Vec<String>> {
    let walked: BTreeSet<String> = walk(root)?.into_iter().map(|file| file.rel).collect();
    let mut paths: BTreeSet<String> = walked
        .iter()
        .filter(|rel| is_environment_file(rel))
        .cloned()
        .collect();
    let named = named_values(paths.iter().map(|rel| {
        let text = helmfile_rel(rel)
            .then(|| std::fs::read_to_string(root.join(rel)).ok())
            .flatten();
        (rel.as_str(), text)
    }));
    paths.extend(named.into_iter().filter(|rel| walked.contains(rel)));
    Ok(paths.into_iter().collect())
}

fn helmfile_rel(rel: &str) -> bool {
    helm::HELMFILE_NAMES.contains(&rel.rsplit('/').next().unwrap_or(rel))
}

/// The values files the helmfiles among `files` name.
fn named_values<'a>(files: impl Iterator<Item = (&'a str, Option<String>)>) -> BTreeSet<String> {
    files
        .filter_map(|(rel, text)| Some(helm::named_values(rel, &text?)))
        .flatten()
        .collect()
}

/// True when this reader may open `rel` and a capture retains it: a Compose
/// file, a kustomization, a Helm chart, helmfile or values file, a YAML file
/// that may hold a Kubernetes manifest, or a settings file. Every file the
/// reader opens is one of these or an env file, which no capture retains: it
/// holds secrets (AC-0097), and the reader only checks that it is readable.
pub fn is_environment_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    !crate::toolchain::is_env_name(name) && is_read_file(name, rel)
}

fn is_read_file(name: &str, rel: &str) -> bool {
    compose_file(name)
        || kustomize::FILE_NAMES.contains(&name)
        || name == "Chart.yaml"
        || helm::HELMFILE_NAMES.contains(&name)
        || helm::values_name(name)
        || manifest_candidate(name, rel)
        || settings::spring_file(name).is_some()
        || settings::names_an_environment(rel)
}

/// [`extract_dir`] over a source capture (AC-1059, #385): `captured` is the
/// capture's [`is_environment_file`] selection and `captured_read` its
/// bytes. The checkout is listed for its directories and other files, and
/// read only to check that an env file is readable: a selected file the
/// capture does not hold is not among the files read, and a captured file
/// the checkout no longer lists is.
pub fn extract_from_capture(
    root: &Path,
    captured: &[String],
    captured_read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    id: &SourceId,
) -> std::io::Result<EnvironmentExtraction> {
    let read = |rel: &str| {
        if crate::toolchain::is_env_name(rel.rsplit('/').next().unwrap_or(rel)) {
            std::fs::read(root.join(rel))
        } else {
            captured_read(rel)
        }
    };
    let read: &dyn Fn(&str) -> std::io::Result<Vec<u8>> = &read;
    let held: BTreeSet<&str> = captured.iter().map(String::as_str).collect();
    // A values file a captured helmfile names is selected too, so whether
    // it is read never depends on the checkout.
    let named = named_values(held.iter().map(|rel| {
        let text = helmfile_rel(rel)
            .then(|| read(rel).ok())
            .flatten()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        (*rel, text)
    }));
    let mut walked = walk(root)?;
    walked.retain(|file| {
        !(is_environment_file(&file.rel) || named.contains(&file.rel))
            || held.contains(file.rel.as_str())
    });
    let listed: BTreeSet<String> = walked.iter().map(|file| file.rel.clone()).collect();
    for rel in held {
        if !listed.contains(rel) {
            walked.push(source_walk::WalkedFile {
                rel: rel.to_string(),
                path: root.join(rel),
                name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            });
        }
    }
    walked.sort_by(|a, b| a.rel.cmp(&b.rel));
    let len = |rel: &str| read(rel).map(|bytes| bytes.len() as u64);
    Ok(extract_walked(&walked, &Source { read, len: &len }, id))
}

fn walk(root: &Path) -> std::io::Result<Vec<source_walk::WalkedFile>> {
    source_walk::files(
        root,
        &crate::preflight::skip_dir,
        source_walk::Gitignores::Honor,
    )
}

/// Where the reader's bytes come from, by repo-relative path: the checkout,
/// or a source capture.
struct Source<'a> {
    read: &'a dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    len: &'a dyn Fn(&str) -> std::io::Result<u64>,
}

impl Source<'_> {
    /// The file's text, or an error when it cannot be read or is not UTF-8.
    fn text(&self, rel: &str) -> std::io::Result<String> {
        String::from_utf8((self.read)(rel)?)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))
    }

    /// The file's length in bytes, or 0 when it cannot be read.
    fn len_or_zero(&self, rel: &str) -> u64 {
        (self.len)(rel).unwrap_or(0)
    }

    /// The file's length in bytes.
    fn len(&self, rel: &str) -> std::io::Result<u64> {
        (self.len)(rel)
    }
}

fn extract_walked(
    walked: &[source_walk::WalkedFile],
    src: &Source,
    id: &SourceId,
) -> EnvironmentExtraction {
    let tree = Tree::read(walked, src);
    let charts = helm::Index::read(walked, src);
    let spring = settings::Index::read(walked, src);
    let mut out = EnvironmentExtraction::default();
    for file in walked {
        let emitter = Emitter {
            id,
            rel: &file.rel,
            src,
        };
        if compose_file(&file.name) {
            out.files += 1;
            match src.text(&file.rel) {
                Ok(text) => match compose(&text) {
                    Ok(decl) => emitter.compose(decl, &mut out),
                    Err((reason, span)) => {
                        let mut findings = Findings::default();
                        findings.add(reason, span);
                        out.nodes.push(emitter.gap("compose-file", None, findings));
                    }
                },
                Err(_) => {
                    let len = src.len_or_zero(&file.rel);
                    let mut findings = Findings::default();
                    findings.add("the file is not readable UTF-8 text", (0, len));
                    out.nodes.push(emitter.gap("compose-file", None, findings));
                }
            }
        } else if names_an_environment(&file.name)
            // The toolchain reader records an environment file only when it
            // can read it; this Gap exists for exactly those files.
            && src.text(&file.rel).is_ok()
        {
            out.files += 1;
            out.nodes.push(emitter.name_only());
        } else if let Some(parsed) = tree.kustomization(&file.rel) {
            out.files += 1;
            match parsed {
                Ok(kustomization) => {
                    let reach = tree.reach(&file.rel, kustomization);
                    emitter.kustomization(kustomization, reach, &tree, &mut out);
                }
                Err((reason, span)) => {
                    let mut findings = Findings::default();
                    findings.add(*reason, *span);
                    out.nodes.push(emitter.kustomization_gap(findings, &[]));
                }
            }
        } else if file.name == "Chart.yaml" {
            out.files += 1;
            emitter.chart(file, &charts, &mut out);
        } else if let Some(parsed) = charts.helmfiles.get(file.rel.as_str()) {
            out.files += 1;
            emitter.helmfile(parsed, &charts, &mut out);
        } else if charts.chart_values(&file.rel, &file.name) {
            // A chart's values file is read through its chart or a release;
            // one named only by its file name names no declared environment.
            if !charts.named(&file.rel) && helm::values_names_an_environment(&file.name) {
                out.files += 1;
                out.nodes.push(emitter.name_only());
            }
        } else if emitter.manifest(tree.documents(&file.rel), &mut out) {
            // Read first: a Kubernetes manifest may be named like a Spring
            // Boot configuration file (`application.yml`).
            out.files += 1;
        } else if let Some(parsed) = spring.files.get(&file.rel) {
            if emitter.spring(&file.name, parsed, &spring, &mut out) {
                out.files += 1;
            }
        } else if settings::names_an_environment(&file.rel) && src.text(&file.rel).is_ok() {
            // A settings module or configuration file named like an
            // environment: the name alone declares nothing.
            out.files += 1;
            out.nodes.push(emitter.name_only());
        }
    }
    out
}

struct Emitter<'a> {
    id: &'a SourceId<'a>,
    rel: &'a str,
    src: &'a Source<'a>,
}

impl Emitter<'_> {
    /// Seal `props` with provenance. The content hash covers the fact's
    /// properties as well as its identity, so an edited image, port or
    /// dependency changes the graph fingerprint.
    fn seal(
        &self,
        props: Props,
        confidence: ConfidenceTier,
        spans: &[Span],
        identity: &str,
    ) -> serde_json::Value {
        let evidence: Vec<(&str, Span)> = spans.iter().map(|span| (self.rel, *span)).collect();
        self.seal_at(props, confidence, &evidence, identity)
    }

    /// [`Emitter::seal`] over spans that may lie in other files of the
    /// repository: a patch document is cited where it is written.
    fn seal_at(
        &self,
        props: Props,
        confidence: ConfidenceTier,
        evidence: &[(&str, Span)],
        identity: &str,
    ) -> serde_json::Value {
        let mut props = serde_json::Value::Object(props);
        let input = serde_json::to_vec(&serde_json::json!([identity, &props])).expect("serializes");
        let evidence = evidence
            .iter()
            .map(|&(path, (start, end))| EvidenceRef {
                repo: self.id.repo.to_string(),
                path: path.to_string(),
                byte_start: start,
                byte_end: end,
                commit_sha: self.id.commit.to_string(),
            })
            .collect();
        props["prov"] = serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                confidence,
                evidence,
                ENVIRONMENT_EXTRACTOR_ID,
                &input,
            )
            .expect("within ceiling"),
        )
        .expect("serializes");
        props
    }

    fn gap_id(&self, service: Option<&str>) -> String {
        let mut id = format!("gap:environment:{}@{}", self.id.repo, self.rel);
        if let Some(service) = service {
            id.push('#');
            id.push_str(service);
        }
        id
    }

    fn gap_node(&self, kind: &str, service: Option<&str>, reason: String, spans: &[Span]) -> Node {
        let id = self.gap_id(service);
        let mut props = Props::new();
        props.insert("kind".into(), kind.into());
        props.insert("path".into(), self.rel.into());
        if let Some(service) = service {
            props.insert("service".into(), service.into());
        }
        props.insert("reason".into(), reason.into());
        props.insert("attempted_tiers".into(), serde_json::json!(["T0"]));
        Node {
            props: self.seal(props, ConfidenceTier::Gap, spans, &id),
            id,
            label: "Gap".into(),
        }
    }

    fn gap(&self, kind: &str, service: Option<&str>, findings: Findings) -> Node {
        let spans: Vec<Span> = findings.spans.into_iter().collect();
        self.gap_node(kind, service, findings.reasons.join("; "), &spans)
    }

    /// The Gap of a name-only environment file. No span: see the module
    /// documentation.
    fn name_only(&self) -> Node {
        self.gap_node(
            "name-only-environment",
            None,
            NAME_ONLY_REASON.to_string(),
            &[],
        )
    }

    fn compose(&self, decl: Compose, out: &mut EnvironmentExtraction) {
        let environment = format!("env:{}@{}", self.id.repo, self.rel);
        let mut props = decl.props;
        props.insert("declared_by".into(), "compose".into());
        props.insert("path".into(), self.rel.into());
        out.nodes.push(Node {
            props: self.seal(props, ConfidenceTier::Confirmed, &[decl.span], &environment),
            id: environment.clone(),
            label: "Environment".into(),
        });
        if !decl.findings.reasons.is_empty() {
            out.nodes
                .push(self.gap("compose-file", None, decl.findings));
        }
        let workload = |name: &str| format!("workload:{}@{}#{name}", self.id.repo, self.rel);
        let declared: BTreeSet<String> = decl
            .services
            .iter()
            .map(|service| service.name.clone())
            .collect();
        let edge = |src: &str, label: &str, dst: &str, spans: &[Span]| Edge {
            props: self.seal(
                Props::new(),
                ConfidenceTier::Confirmed,
                spans,
                &format!("{src} {label} {dst}"),
            ),
            src: src.to_string(),
            dst: dst.to_string(),
            label: label.into(),
        };
        for service in decl.services {
            let this = workload(&service.name);
            let mut props = service.props;
            props.insert("path".into(), self.rel.into());
            out.nodes.push(Node {
                props: self.seal(props, ConfidenceTier::Confirmed, &[service.span], &this),
                id: this.clone(),
                label: "Workload".into(),
            });
            out.edges
                .push(edge(&environment, "DECLARES", &this, &[service.key_span]));
            // One edge per dependency: the store keys an edge by its ends
            // and label, so every entry naming it is cited on it.
            let mut resolved: BTreeMap<&str, BTreeSet<Span>> = BTreeMap::new();
            for (name, span) in &service.depends_on {
                if declared.contains(name) {
                    resolved.entry(name).or_default().insert(*span);
                }
            }
            for (name, spans) in resolved {
                let spans: Vec<Span> = spans.into_iter().collect();
                out.edges
                    .push(edge(&this, "DEPENDS_ON", &workload(name), &spans));
            }
            if !service.findings.reasons.is_empty() {
                out.nodes
                    .push(self.gap("compose-service", Some(&service.name), service.findings));
            }
        }
    }

    /// One Spring Boot configuration file: an `Environment` per profile it
    /// is activated for, or, for a profile-specific file, one environment
    /// when a base file of its directory declares the profile; a name-only
    /// Gap when none does. True when a fact was emitted.
    fn spring(
        &self,
        name: &str,
        parsed: &Result<Vec<settings::Document>, (&'static str, Span)>,
        index: &settings::Index,
        out: &mut EnvironmentExtraction,
    ) -> bool {
        let Some((profile, _)) = settings::spring_file(name) else {
            return false;
        };
        let documents = match parsed {
            Ok(documents) => documents,
            Err((reason, span)) => {
                let mut findings = Findings::default();
                findings.add(*reason, *span);
                out.nodes.push(self.gap("spring-config", None, findings));
                return true;
            }
        };
        let mut findings = Findings::default();
        for document in documents {
            for reason in &document.findings.reasons {
                if !findings.reasons.contains(reason) {
                    findings.reasons.push(reason.clone());
                }
            }
            findings
                .spans
                .extend(document.findings.spans.iter().copied());
        }
        let mut emitted = false;
        match profile {
            None => {
                // Documents activated for one profile are one environment.
                let mut by_profile: BTreeMap<&str, (Vec<Span>, BTreeSet<&str>, usize)> =
                    BTreeMap::new();
                for document in documents {
                    let Some((profile, span)) = &document.on_profile else {
                        continue;
                    };
                    let entry = by_profile.entry(profile.as_str()).or_default();
                    entry.0.push(*span);
                    entry.1.extend(document.keys.iter().map(String::as_str));
                    entry.2 += document.omitted;
                }
                for (profile, (spans, keys, omitted)) in by_profile {
                    let environment = format!("env:{}@{}#{profile}", self.id.repo, self.rel);
                    let mut props = Props::new();
                    props.insert("declared_by".into(), "spring-profile".into());
                    props.insert("path".into(), format!("{}#{profile}", self.rel).into());
                    props.insert("file".into(), self.rel.into());
                    props.insert("name".into(), profile.into());
                    props.insert(
                        "name_span".into(),
                        serde_json::json!([spans[0].0, spans[0].1]),
                    );
                    props.insert("settings_keys".into(), serde_json::json!(keys));
                    if omitted > 0 {
                        props.insert("settings_keys_omitted".into(), omitted.into());
                    }
                    out.nodes.push(Node {
                        props: self.seal(props, ConfidenceTier::Confirmed, &spans, &environment),
                        id: environment,
                        label: "Environment".into(),
                    });
                    emitted = true;
                }
            }
            Some(profile) => {
                for document in documents {
                    if let Some((_, span)) = &document.on_profile {
                        findings.add(
                            "a profile-specific file states a profile activation, which Spring \
                             Boot rejects",
                            *span,
                        );
                    }
                }
                match index.declared(self.rel, profile) {
                    Some((declared_in, span)) => {
                        let environment = format!("env:{}@{}", self.id.repo, self.rel);
                        let keys: BTreeSet<&str> = documents
                            .iter()
                            .flat_map(|document| document.keys.iter().map(String::as_str))
                            .collect();
                        let omitted: usize =
                            documents.iter().map(|document| document.omitted).sum();
                        let mut props = Props::new();
                        props.insert("declared_by".into(), "spring-profile".into());
                        props.insert("path".into(), self.rel.into());
                        props.insert("name".into(), profile.into());
                        props.insert("profile_declared_in".into(), declared_in.as_str().into());
                        props.insert("settings_keys".into(), serde_json::json!(keys));
                        if omitted > 0 {
                            props.insert("settings_keys_omitted".into(), omitted.into());
                        }
                        // Cited where the profile is declared: the file itself
                        // may hold credentials.
                        out.nodes.push(Node {
                            props: self.seal_at(
                                props,
                                ConfidenceTier::Confirmed,
                                &[(declared_in.as_str(), *span)],
                                &environment,
                            ),
                            id: environment,
                            label: "Environment".into(),
                        });
                        emitted = true;
                    }
                    None if settings::profile_names_an_environment(profile) => {
                        out.nodes.push(self.name_only());
                        emitted = true;
                    }
                    None => {}
                }
            }
        }
        if !findings.reasons.is_empty() {
            out.nodes.push(self.gap("spring-config", None, findings));
            emitted = true;
        }
        emitted
    }

    /// The key paths of the values file `rel`, or why none are read.
    fn values_keys(&self, rel: &str) -> Result<(Vec<String>, usize), String> {
        let text = self
            .src
            .text(rel)
            .map_err(|_| "the values file is not readable UTF-8 text".to_string())?;
        helm::values_keys(&text).map_err(|reason| format!("the values file: {reason}"))
    }

    /// One `Chart.yaml`: a `Workload` of kind `helm-chart` with the key paths
    /// of the chart's default values file, or a Gap.
    fn chart(
        &self,
        file: &source_walk::WalkedFile,
        index: &helm::Index,
        out: &mut EnvironmentExtraction,
    ) {
        let parsed = self
            .src
            .text(&file.rel)
            .map_err(|_| ("the file is not readable UTF-8 text", (0, 0)))
            .and_then(|text| helm::chart(&text));
        let decl = match parsed {
            Ok(decl) => decl,
            Err((reason, span)) => {
                let mut findings = Findings::default();
                findings.add(reason, span);
                out.nodes.push(self.gap("helm-chart", None, findings));
                return;
            }
        };
        let id = format!("workload:{}@{}", self.id.repo, self.rel);
        let mut props = decl.props;
        props.insert("path".into(), self.rel.into());
        let mut findings = Findings::default();
        if let Some(values) = index.default_values(self.rel) {
            let keys = index
                .path(&values)
                .ok_or_else(|| "the default values file is not readable".to_string())
                .and_then(|_| self.values_keys(&values));
            match keys {
                Ok((keys, omitted)) => {
                    props.insert("values_file".into(), values.into());
                    props.insert("values_keys".into(), keys.into());
                    if omitted > 0 {
                        props.insert("values_keys_omitted".into(), omitted.into());
                    }
                }
                Err(reason) => findings.add(reason, decl.span),
            }
        }
        out.nodes.push(Node {
            props: self.seal(props, ConfidenceTier::Confirmed, &[decl.span], &id),
            id,
            label: "Workload".into(),
        });
        if !findings.reasons.is_empty() {
            out.nodes.push(self.gap("helm-chart", None, findings));
        }
    }

    /// One helmfile: an `Environment` per release, `DECLARES` to each
    /// release's chart in the repository with its values files' key paths,
    /// and a Gap per file and per release for what is not read.
    fn helmfile(
        &self,
        parsed: &Result<helm::Helmfile, (&'static str, Span)>,
        index: &helm::Index,
        out: &mut EnvironmentExtraction,
    ) {
        let decl = match parsed {
            Ok(decl) => decl,
            Err((reason, span)) => {
                let mut findings = Findings::default();
                findings.add(*reason, *span);
                out.nodes.push(self.gap("helmfile", None, findings));
                return;
            }
        };
        if !decl.findings.reasons.is_empty() {
            out.nodes
                .push(self.gap("helmfile", None, decl.findings.clone()));
        }
        for release in &decl.releases {
            let environment = format!("env:{}@{}#{}", self.id.repo, self.rel, release.release);
            let mut props = release.props.clone();
            props.insert("declared_by".into(), "helmfile".into());
            // The environment is the release, so its path names it within
            // the helmfile; `file` is the helmfile itself.
            props.insert(
                "path".into(),
                format!("{}#{}", self.rel, release.release).into(),
            );
            props.insert("file".into(), self.rel.into());
            if !decl.environments.is_empty() {
                props.insert(
                    "helmfile_environments".into(),
                    decl.environments.clone().into(),
                );
            }
            out.nodes.push(Node {
                props: self.seal(
                    props,
                    ConfidenceTier::Confirmed,
                    &[release.span],
                    &environment,
                ),
                id: environment.clone(),
                label: "Environment".into(),
            });
            let mut findings = release.findings.clone();
            let mut values = Vec::new();
            let mut cited = vec![release.span];
            for (entry, span) in &release.values {
                match index.values_file(self.rel, entry) {
                    Ok(rel) => match index.path(&rel).and_then(|_| self.values_keys(&rel).ok()) {
                        Some((keys, omitted)) => {
                            let mut value = serde_json::json!({ "path": rel, "keys": keys });
                            if omitted > 0 {
                                value["keys_omitted"] = omitted.into();
                            }
                            values.push(value);
                            cited.push(*span);
                        }
                        None => findings.add(
                            format!("values file `{rel}` does not parse as a mapping"),
                            *span,
                        ),
                    },
                    Err(reason) => findings.add(reason, *span),
                }
            }
            if let Some((chart, span)) = &release.chart {
                match index.chart_of(self.rel, chart) {
                    helm::ChartTarget::Chart(rel) => {
                        let dst = format!("workload:{}@{rel}", self.id.repo);
                        let mut props = Props::new();
                        if !values.is_empty() {
                            props.insert("values".into(), values.into());
                        }
                        cited.push(*span);
                        cited.sort();
                        cited.dedup();
                        out.edges.push(Edge {
                            props: self.seal(
                                props,
                                ConfidenceTier::Confirmed,
                                &cited,
                                &format!("{environment} DECLARES {dst}"),
                            ),
                            src: environment.clone(),
                            dst,
                            label: "DECLARES".into(),
                        });
                    }
                    helm::ChartTarget::Problem(reason) => findings.add(reason, *span),
                }
            }
            if !findings.reasons.is_empty() {
                out.nodes
                    .push(self.gap("helmfile-release", Some(&release.release), findings));
            }
        }
    }

    /// The id of the workload a manifest document of `rel` declares.
    fn workload_id(&self, rel: &str, index: usize, kind: &str, name: &str) -> String {
        format!("workload:{}@{rel}#{index}:{kind}/{name}", self.id.repo)
    }

    /// The Gap of a kustomization file: every finding, and the transformer
    /// and generator keys that are present and not evaluated.
    fn kustomization_gap(&self, findings: Findings, unevaluated: &[&str]) -> Node {
        let id = self.gap_id(None);
        let mut props = Props::new();
        props.insert("kind".into(), "kustomization".into());
        props.insert("path".into(), self.rel.into());
        if !unevaluated.is_empty() {
            props.insert("unevaluated".into(), unevaluated.into());
        }
        props.insert("reason".into(), findings.reasons.join("; ").into());
        props.insert("attempted_tiers".into(), serde_json::json!(["T0"]));
        let spans: Vec<Span> = findings.spans.into_iter().collect();
        Node {
            props: self.seal(props, ConfidenceTier::Gap, &spans, &id),
            id,
            label: "Gap".into(),
        }
    }

    /// One kustomization file: an `Environment`, `DECLARES` to each workload
    /// it reaches, `PATCHES` for each one a patch file names, and one Gap
    /// for everything that was not followed, matched or evaluated.
    fn kustomization(
        &self,
        decl: &kustomize::Kustomization,
        reach: Reach,
        tree: &Tree,
        out: &mut EnvironmentExtraction,
    ) {
        let environment = format!("env:{}@{}", self.id.repo, self.rel);
        let mut props = decl.props.clone();
        props.insert("declared_by".into(), "kustomize".into());
        props.insert("path".into(), self.rel.into());
        if !reach.includes.is_empty() {
            props.insert("includes".into(), reach.includes.clone().into());
        }
        out.nodes.push(Node {
            props: self.seal(props, ConfidenceTier::Confirmed, &[decl.span], &environment),
            id: environment.clone(),
            label: "Environment".into(),
        });
        if !reach.findings.reasons.is_empty() {
            out.nodes
                .push(self.kustomization_gap(reach.findings, &decl.unevaluated));
        }
        let workload = |path: &str, index: usize| {
            let document = tree
                .documents(path)
                .iter()
                .find(|document| document.index == index)?;
            let name = document.read.as_ref()?.name.as_ref()?;
            Some(self.workload_id(path, index, &document.kind, name))
        };
        for ((path, index), declared) in &reach.declares {
            let Some(dst) = workload(path, *index) else {
                continue;
            };
            let mut props = Props::new();
            props.insert("via".into(), declared.via.clone().into());
            let spans: Vec<Span> = declared.spans.iter().copied().collect();
            out.edges.push(Edge {
                props: self.seal(
                    props,
                    ConfidenceTier::Confirmed,
                    &spans,
                    &format!("{environment} DECLARES {dst}"),
                ),
                src: environment.clone(),
                dst,
                label: "DECLARES".into(),
            });
        }
        // One edge per workload: the store keys an edge by its ends and
        // label. The first patch document gives the edge its properties;
        // each further one is listed under `more`, in the order written.
        for ((path, index), statements) in reach.patches {
            let Some(dst) = workload(path, index) else {
                continue;
            };
            let evidence: Vec<(&str, Span)> = statements
                .iter()
                .map(|statement| (statement.path, statement.span))
                .collect();
            let mut stated = statements.into_iter().map(|statement| {
                let mut props = statement.props;
                props.insert("path".into(), statement.path.into());
                props
            });
            let Some(mut props) = stated.next() else {
                continue;
            };
            let more: Vec<serde_json::Value> = stated.map(serde_json::Value::Object).collect();
            if !more.is_empty() {
                props.insert("more".into(), more.into());
            }
            out.edges.push(Edge {
                props: self.seal_at(
                    props,
                    ConfidenceTier::Confirmed,
                    &evidence,
                    &format!("{environment} PATCHES {dst}"),
                ),
                src: environment.clone(),
                dst,
                label: "PATCHES".into(),
            });
        }
    }

    /// The workloads of one manifest file, and a Gap for each document that
    /// states something this reader does not read. True when it yielded a
    /// fact.
    fn manifest(
        &self,
        documents: &[kubernetes::Document],
        out: &mut EnvironmentExtraction,
    ) -> bool {
        let before = out.nodes.len();
        for document in documents {
            let Some(read) = &document.read else {
                continue;
            };
            let mut findings = read.findings.clone();
            let workload = read.name.as_ref().map(|name| {
                (
                    name,
                    self.workload_id(self.rel, document.index, &document.kind, name),
                )
            });
            match &workload {
                Some((name, id)) => {
                    let mut props = read.props.clone();
                    props.insert("kind".into(), read.fact_kind.into());
                    props.insert("name".into(), name.as_str().into());
                    props.insert("path".into(), self.rel.into());
                    out.nodes.push(Node {
                        props: self.seal(props, ConfidenceTier::Confirmed, &[document.span], id),
                        id: id.clone(),
                        label: "Workload".into(),
                    });
                    if !read.containers_stated {
                        findings.add(
                            "`spec.template.spec.containers` is not a sequence of mappings",
                            document.span,
                        );
                    }
                }
                None => findings.add(
                    "the document has no plain scalar `metadata.name`, so no workload is recorded",
                    document.span,
                ),
            }
            if findings.reasons.is_empty() {
                continue;
            }
            let id = format!("{}#{}", self.gap_id(None), document.index);
            let mut props = Props::new();
            props.insert("kind".into(), "manifest-document".into());
            props.insert("path".into(), self.rel.into());
            props.insert("document".into(), document.index.into());
            if let Some((name, _)) = &workload {
                props.insert(
                    "workload".into(),
                    format!("{}/{name}", document.kind).into(),
                );
            }
            props.insert("reason".into(), findings.reasons.join("; ").into());
            props.insert("attempted_tiers".into(), serde_json::json!(["T0"]));
            let spans: Vec<Span> = findings.spans.into_iter().collect();
            out.nodes.push(Node {
                props: self.seal(props, ConfidenceTier::Gap, &spans, &id),
                id,
                label: "Gap".into(),
            });
        }
        out.nodes.len() > before
    }
}

mod helm;
mod kubernetes;
mod kustomize;
mod settings;

#[cfg(test)]
mod tests;
