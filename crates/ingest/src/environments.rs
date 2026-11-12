//! Environments and what they declare (#1791, AC-0661, ADR-0070, SPEC-14):
//! Compose files as T0 facts, each citing the exact byte span that declares
//! it.
//!
//! An environment is a declared deployable set. A Compose file that parses
//! and has a `services` mapping is one:
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
//! Kubernetes manifests, kustomize files and Helm charts are not read. They
//! are named as present and not read ([`unread`]).
//!
//! This tier never calls an LLM.

use crate::pipelines::{Field, Limit, Span, field, fields, items, sanitize, span_of};
use crate::schedules::{contains_kind, first_mapping, scalar, workflow_file};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
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

/// Files detected and not read: finding kind and what the file is.
const UNREAD: &[(&str, &str)] = &[
    ("kubernetes-manifest", "Kubernetes manifest"),
    ("kustomize-file", "kustomize file"),
    ("helm-chart", "Helm chart"),
    ("helm-values-file", "Helm values file"),
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
    /// `DECLARES` and `DEPENDS_ON` edges.
    pub edges: Vec<Edge>,
    /// Compose files and name-only environment files that yielded a fact.
    pub files: u64,
}

/// True when `kind` is a finding kind this module produces. Coverage quotes
/// such a finding's own message as its reason.
pub fn is_limit_kind(kind: &str) -> bool {
    UNREAD.iter().any(|(known, _)| *known == kind)
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

/// The limit `rel` carries, if any: a Kubernetes manifest, a kustomize file
/// or a Helm chart or values file, present and not read. A YAML file is read
/// only to see whether a document of it has top-level `apiVersion` and `kind`
/// scalars; nothing else of it is looked at. A file whose only such documents
/// are the `batch/*` `CronJob`s the schedule reader covers is not named.
pub fn unread(name: &str, rel: &str, path: &Path) -> Option<Limit> {
    let index = if matches!(
        name,
        "kustomization.yaml" | "kustomization.yml" | "Kustomization"
    ) {
        1
    } else if name == "Chart.yaml" {
        2
    } else if yaml(name) && helm_values(name) && path.with_file_name("Chart.yaml").is_file() {
        3
    } else if yaml(name) && !compose_file(name) && !workflow_file(rel) {
        let text = std::fs::read_to_string(path).ok()?;
        if !kubernetes_manifest(&text) {
            return None;
        }
        0
    } else {
        return None;
    };
    let (kind, what) = UNREAD[index];
    Some(Limit {
        kind,
        message: format!("{what} is {NOT_READ}"),
    })
}

/// `values.yaml` and its variants (`values-prod.yaml`, `values.dev.yml`).
fn helm_values(name: &str) -> bool {
    name.strip_prefix("values")
        .is_some_and(|rest| rest.starts_with(['.', '-', '_']))
}

/// True when a document of `text` has top-level `apiVersion` and `kind`
/// scalars and is not a `batch/*` `CronJob`.
fn kubernetes_manifest(text: &str) -> bool {
    // Both keys start a line in any block-style manifest; most YAML has
    // neither and is not parsed.
    let starts = |key: &str| text.lines().any(|line| line.starts_with(key));
    if !starts("apiVersion:") || !starts("kind:") {
        return false;
    }
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .is_err()
    {
        return false;
    }
    let Some(tree) = parser.parse(text, None) else {
        return false;
    };
    let root = tree.root_node();
    if root.has_error() {
        return false;
    }
    let mut walk = root.walk();
    let documents: Vec<TsNode> = root
        .named_children(&mut walk)
        .filter(|node| node.kind() == "document")
        .collect();
    documents.into_iter().any(|document| {
        let Some(top) = first_mapping(document) else {
            return false;
        };
        let mut walk = top.walk();
        let mut api = None;
        let mut kind = None;
        for pair in top
            .named_children(&mut walk)
            .filter(|pair| pair.kind() == "block_mapping_pair")
        {
            let (Some(key), Some(value)) = (
                pair.child_by_field_name("key"),
                pair.child_by_field_name("value"),
            ) else {
                continue;
            };
            match scalar(text, key).as_deref() {
                Some("apiVersion") => api = scalar(text, value),
                Some("kind") => kind = scalar(text, value),
                _ => {}
            }
        }
        match (api, kind) {
            (Some(api), Some(kind)) => !(kind == "CronJob" && api.starts_with("batch/")),
            _ => false,
        }
    })
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
#[derive(Debug, Default)]
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

/// Walk `root` and turn every Compose file into environment facts, and every
/// name-only environment file into a Gap. Deterministic for a given tree: the
/// walk is sorted, and every id derives from the path and the service name.
pub fn extract_dir(root: &Path, id: &SourceId) -> std::io::Result<EnvironmentExtraction> {
    let walked = source_walk::files(
        root,
        &crate::preflight::skip_dir,
        source_walk::Gitignores::Honor,
    )?;
    let mut out = EnvironmentExtraction::default();
    for file in &walked {
        let emitter = Emitter { id, rel: &file.rel };
        if compose_file(&file.name) {
            out.files += 1;
            match std::fs::read_to_string(&file.path) {
                Ok(text) => match compose(&text) {
                    Ok(decl) => emitter.compose(decl, &mut out),
                    Err((reason, span)) => {
                        let mut findings = Findings::default();
                        findings.add(reason, span);
                        out.nodes.push(emitter.gap("compose-file", None, findings));
                    }
                },
                Err(_) => {
                    let len = std::fs::metadata(&file.path).map_or(0, |meta| meta.len());
                    let mut findings = Findings::default();
                    findings.add("the file is not readable UTF-8 text", (0, len));
                    out.nodes.push(emitter.gap("compose-file", None, findings));
                }
            }
        } else if names_an_environment(&file.name)
            // The toolchain reader records an environment file only when it
            // can read it; this Gap exists for exactly those files.
            && std::fs::read_to_string(&file.path).is_ok()
        {
            out.files += 1;
            out.nodes.push(emitter.name_only());
        }
    }
    Ok(out)
}

struct Emitter<'a> {
    id: &'a SourceId<'a>,
    rel: &'a str,
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
        let mut props = serde_json::Value::Object(props);
        let input = serde_json::to_vec(&serde_json::json!([identity, &props])).expect("serializes");
        let evidence = spans
            .iter()
            .map(|&(start, end)| EvidenceRef {
                repo: self.id.repo.to_string(),
                path: self.rel.to_string(),
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
}

#[cfg(test)]
mod tests;
