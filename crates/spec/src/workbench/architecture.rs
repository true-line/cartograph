//! C4 architecture views (#2049, SPEC-19): `architecture.md` draws levels 1
//! to 3 of the C4 model (system context, containers, components) as Mermaid
//! diagrams with element and relationship tables; `architecture.dsl` writes
//! the same model as a Structurizr DSL workspace.
//!
//! The model is a pure function of the admitted facts, the admitted flows,
//! the subsystem areas the host supplied and the repository names the host
//! supplied. No model call is made, nothing is guessed from a name, and
//! nothing is written to the graph.
//!
//! | C4 element | Read from |
//! |---|---|
//! | Container (web application) | a repository's `Endpoint`s and `http-server` `EntryPoint`s |
//! | Container (process) | each other `EntryPoint` that is not a command or an operator shell |
//! | Container (operator commands) | a repository's `command` and `operator-shell` `EntryPoint`s |
//! | Container (scheduled jobs) | a repository's `Schedule`s |
//! | Container (client app) | a repository with screens or an extension and no process start |
//! | Container (no entry point) | a repository with code and none of the above, drawn as a Gap |
//! | Data store | a `Resource` the code uses that holds data |
//! | External system | `external.md`'s outside origins, its unclassified calls in one Gap box, SMTP and other providers |
//! | Person | the operator who runs commands; a user where client screens or server-rendered pages exist (InferredWeak) |
//! | Component | a subsystem area holding files the container's flows pass through |
//!
//! A container owns the facts its flows pass through and the files they are
//! defined in; the only container of a repository owns the whole
//! repository. A use no container's flows reach is drawn from the
//! repository's first container as a dashed, InferredWeak relation. Every
//! element and relation carries the lowest confidence of the facts it rests
//! on and cites them.
use super::capabilities::{Stories, area_name};
use super::external::{self, Fact};
use super::producers::Output;
use super::render::{self, Assertions, Budget, RenderLimit, Safe, Text};
use super::{ExportMode, provenance};
use core_graph::{Edge, Node};
use core_prov::ConfidenceTier;
use flowtracer::Flow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

/// Markdown artifact id.
pub(super) const MARKDOWN_ID: &str = "architecture";
/// Structurizr DSL artifact id.
pub(super) const DSL_ID: &str = "architecture-dsl";

/// Boxes drawn in one diagram before the smallest fold into one.
const MAX_DRAWN: usize = 24;
/// Components drawn for one container before the smallest fold into one.
const MAX_COMPONENTS: usize = 14;
/// Facts cited for one element or relation; the table states the total.
const MAX_CITED: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Person,
    Container,
    Store,
    External,
    Component,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Person => "Person",
            Self::Container => "Container",
            Self::Store => "Data store",
            Self::External => "External system",
            Self::Component => "Component",
        }
    }
}

struct Element<'a> {
    kind: Kind,
    name: String,
    technology: String,
    description: String,
    repo: Option<&'a str>,
    facts: Vec<Fact<'a>>,
    /// Forced to Gap when the element stands for what was not recovered.
    gap: bool,
    /// Forced to InferredWeak when the element is inferred from the shape
    /// of the facts, not stated by one.
    weak: bool,
    /// What the element is drawn by: its size for ordering and folding.
    weight: usize,
}

impl Element<'_> {
    fn confidence(&self) -> ConfidenceTier {
        if self.gap {
            return ConfidenceTier::Gap;
        }
        let lowest = lowest(&self.facts);
        if self.weak && rank(lowest) > rank(ConfidenceTier::InferredWeak) {
            ConfidenceTier::InferredWeak
        } else {
            lowest
        }
    }
}

struct Relation<'a> {
    from: usize,
    to: usize,
    verb: String,
    noun: (&'static str, &'static str),
    targets: BTreeSet<String>,
    technology: String,
    facts: Vec<Fact<'a>>,
    weak: bool,
}

impl Relation<'_> {
    fn label(&self) -> String {
        let n = self.targets.len();
        if self.noun.0.is_empty() || n == 0 {
            self.verb.clone()
        } else {
            format!(
                "{} {n} {}",
                self.verb,
                if n == 1 { self.noun.0 } else { self.noun.1 }
            )
        }
    }
    fn confidence(&self) -> ConfidenceTier {
        let lowest = lowest(&self.facts);
        if self.weak && rank(lowest) > rank(ConfidenceTier::InferredWeak) {
            ConfidenceTier::InferredWeak
        } else {
            lowest
        }
    }
}

/// Relations keyed by (from, to, verb), so repeated facts aggregate.
/// In a `verified-only` export (`strict`) an InferredWeak relation is not
/// drawn (R-INT-5).
#[derive(Default)]
struct Relations<'a>(BTreeMap<(usize, usize, String), Relation<'a>>, bool);

impl<'a> Relations<'a> {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        (from, to): (usize, usize),
        verb: &str,
        noun: (&'static str, &'static str),
        technology: &str,
        target: Option<&str>,
        facts: impl IntoIterator<Item = Fact<'a>>,
        weak: bool,
    ) {
        if from == to || (self.1 && weak) {
            return;
        }
        let relation = self
            .0
            .entry((from, to, verb.to_string()))
            .or_insert_with(|| Relation {
                from,
                to,
                verb: verb.to_string(),
                noun,
                targets: BTreeSet::new(),
                technology: technology.to_string(),
                facts: Vec::new(),
                weak,
            });
        relation.weak &= weak;
        if relation.noun.0.is_empty() {
            relation.noun = noun;
        }
        if let Some(target) = target {
            relation.targets.insert(target.to_string());
        }
        for fact in facts {
            if !relation.facts.iter().any(|held| same(held, &fact)) {
                relation.facts.push(fact);
            }
        }
    }
    fn into_vec(self) -> Vec<Relation<'a>> {
        self.0.into_values().collect()
    }
}

/// The components of one container.
struct Components<'a> {
    container: usize,
    /// Component elements, largest first.
    elements: Vec<Element<'a>>,
    /// Relations among components (indices into `elements`, offset by
    /// `OFFSET`) and with the model's elements.
    relations: Vec<Relation<'a>>,
    /// Files the container owns that are in no area.
    unassigned: usize,
}

/// Component indices in a container's relations start here, so they never
/// collide with model element indices.
const OFFSET: usize = 1 << 20;

/// The C4 model of the admitted facts.
pub(crate) struct Model<'a> {
    system: String,
    elements: Vec<Element<'a>>,
    relations: Vec<Relation<'a>>,
    components: Vec<Components<'a>>,
    /// Whether the host supplied subsystem areas.
    areas_supplied: bool,
}

fn same(a: &Fact<'_>, b: &Fact<'_>) -> bool {
    match (a, b) {
        (Fact::Node(a), Fact::Node(b)) => a.id == b.id,
        (Fact::Edge(a), Fact::Edge(b)) => a.src == b.src && a.dst == b.dst && a.label == b.label,
        _ => false,
    }
}

fn confidence_of(fact: &Fact<'_>) -> ConfidenceTier {
    match fact {
        Fact::Node(node) => provenance(&node.props, &node.id).confidence_tier,
        Fact::Edge(edge) => provenance(&edge.props, &super::edge_identity(edge)).confidence_tier,
    }
}

fn rank(confidence: ConfidenceTier) -> u8 {
    match confidence {
        ConfidenceTier::Confirmed => 3,
        ConfidenceTier::InferredStrong => 2,
        ConfidenceTier::InferredWeak => 1,
        ConfidenceTier::Gap => 0,
    }
}

fn lowest(facts: &[Fact<'_>]) -> ConfidenceTier {
    facts
        .iter()
        .map(confidence_of)
        .min_by_key(|c| rank(*c))
        .unwrap_or(ConfidenceTier::Gap)
}

fn confidence_word(confidence: ConfidenceTier) -> &'static str {
    match confidence {
        ConfidenceTier::Confirmed => "Confirmed",
        ConfidenceTier::InferredStrong => "InferredStrong",
        ConfidenceTier::InferredWeak => "InferredWeak",
        ConfidenceTier::Gap => "Gap",
    }
}

/// The repository key of a node id (`label:{repo}@…`, `gap:kind:{repo}@…`).
fn repo_of(id: &str) -> &str {
    let rest = match id.strip_prefix("gap:") {
        Some(rest) => rest.split_once(':').map_or(rest, |(_, rest)| rest),
        None => id.split_once(':').map_or(id, |(_, rest)| rest),
    };
    rest.split_once('@').map_or("", |(repo, _)| repo)
}

/// A file's stem in words, first letter upper case: `job_runner.py` reads
/// `Job runner`.
fn stem_title(path: &str) -> String {
    let mut parts = path.rsplit('/');
    let name = parts.next().unwrap_or(path);
    let mut stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    // `cmd/worker/main.go`, `src/index.ts`: the directory names the program.
    if matches!(stem, "main" | "index" | "__main__")
        && let Some(dir) = parts.next().filter(|d| !matches!(*d, "src" | "cmd" | ""))
    {
        stem = dir;
    }
    let words = stem.replace(['_', '-'], " ");
    let mut chars = words.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => path.to_string(),
    }
}

/// The path of an absolute http(s) URL, without query or fragment; `None`
/// for a bare origin, which names no endpoint.
fn url_path(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let path = &rest[rest.find('/')?..];
    let path = path.split(['?', '#']).next().unwrap_or(path);
    (path.len() > 1).then_some(path)
}

/// Whether a concrete path matches an endpoint route: equal segments, a
/// parameter (`:id`, `<id>`, `{id}`) matching one segment, and a trailing
/// rest parameter (`{path...}`, `*`) matching one or more.
fn path_matches(pattern: &str, path: &str) -> bool {
    let ps: Vec<&str> = pattern.trim_end_matches('/').split('/').collect();
    let cs: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let param = |p: &str| {
        p.starts_with(':')
            || (p.len() > 2 && p.starts_with('<') && p.ends_with('>'))
            || (p.len() > 2 && p.starts_with('{') && p.ends_with('}'))
    };
    for (i, p) in ps.iter().enumerate() {
        let rest = *p == "*" || (p.starts_with('{') && p.ends_with("...}"));
        if rest && i + 1 == ps.len() {
            return cs.len() > i && cs[i..].iter().all(|c| !c.is_empty());
        }
        match cs.get(i) {
            Some(c) if c == p || (param(p) && !c.is_empty()) => {}
            _ => return false,
        }
    }
    ps.len() == cs.len()
}

/// The language a file's extension names, if it is a common one.
fn language(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1;
    Some(match ext {
        "py" => "Python",
        "go" => "Go",
        "rs" => "Rust",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "rb" => "Ruby",
        "php" => "PHP",
        "cs" => "C#",
        _ => return None,
    })
}

fn framework_name(framework: &str) -> String {
    match framework {
        "flask" => "Flask".into(),
        "fastapi" => "FastAPI".into(),
        "django" => "Django".into(),
        "express" => "Express".into(),
        "nestjs" | "nest" => "NestJS".into(),
        "spring" => "Spring".into(),
        "ktor" => "Ktor".into(),
        "gin" => "Gin".into(),
        "nethttp" | "net/http" => "net/http".into(),
        "axum" => "Axum".into(),
        "actix" => "Actix".into(),
        "koa" => "Koa".into(),
        "hono" => "Hono".into(),
        other => other.to_string(),
    }
}

/// Entry-point kinds that run inside an Android app (AC-1110).
fn android_kind(kind: &str) -> bool {
    kind.starts_with("android-") || kind == "background-job"
}

fn router_technology(router: &str) -> Option<&'static str> {
    Some(match router {
        "vue-router" => "Vue",
        "react-router" => "React",
        "next" | "nextjs" | "next-app" | "next-pages" => "Next.js",
        "angular" => "Angular",
        r if r.starts_with("navigation") || r.contains("compose") => "Android (Jetpack Navigation)",
        _ => return None,
    })
}

/// How a data store is named from what its fact records.
/// The store a data-bearing resource is, by its provider; empty for a
/// resource that holds no data the code reads or writes (a compute
/// instance, a function, a role, a module).
fn store_kind(node: &Node) -> &'static str {
    let provider = node.props["provider"]
        .as_str()
        .or(node.props["type"].as_str())
        .unwrap_or_default();
    match provider {
        "sql-database" => "SQL database",
        "postgres" | "postgresql" => "PostgreSQL",
        "mysql" => "MySQL",
        "sqlite" => "SQLite",
        "redis" => "Redis",
        "memcached" => "Memcached",
        "mongodb" => "MongoDB",
        "dynamodb" => "DynamoDB",
        "elasticsearch" => "Elasticsearch",
        "aws-s3" => "Amazon S3",
        "gcs" => "Google Cloud Storage",
        "azure-blob" => "Azure Blob Storage",
        "web-storage" => "Browser storage",
        "webextension-storage" => "Extension storage",
        "indexeddb" => "IndexedDB",
        _ if node.props["kind"] == "browser-storage" => "Browser storage",
        _ => "",
    }
}

fn store_name(node: &Node) -> String {
    let named = store_kind(node);
    let identity = node.props["identity"].as_str();
    match (named, identity) {
        ("", _) => node.props["logical_id"]
            .as_str()
            .map(|id| id.strip_prefix("observed:").unwrap_or(id).to_string())
            .unwrap_or_else(|| node.id.clone()),
        (named, Some(identity)) => format!("{named} ({identity})"),
        (named, None) => named.to_string(),
    }
}

/// Providers whose observed client is a service outside the code base,
/// not a store it owns.
fn is_external_provider(node: &Node) -> bool {
    matches!(
        node.props["provider"].as_str(),
        Some("smtp" | "sendgrid" | "mailgun" | "ses" | "stripe" | "sentry" | "twilio")
    ) || node.props["kind"] == "auth-provider"
}

fn provider_name(provider: &str) -> String {
    match provider {
        "smtp" => "SMTP mail server".into(),
        "sendgrid" => "SendGrid".into(),
        "mailgun" => "Mailgun".into(),
        "ses" => "Amazon SES".into(),
        "stripe" => "Stripe".into(),
        "sentry" => "Sentry".into(),
        "twilio" => "Twilio".into(),
        other => other.to_string(),
    }
}

struct Index<'a> {
    by_id: BTreeMap<&'a str, &'a Node>,
    /// Symbol → the file it is defined in.
    file_of: BTreeMap<&'a str, &'a str>,
    /// Outgoing edges by label.
    out: BTreeMap<&'a str, Vec<&'a Edge>>,
}

impl<'a> Index<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        let by_id = nodes.iter().map(|n| (n.id.as_str(), *n)).collect();
        let mut file_of = BTreeMap::new();
        let mut out: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        for edge in edges.iter().copied() {
            if edge.label == "DEFINED_IN" {
                file_of
                    .entry(edge.src.as_str())
                    .or_insert(edge.dst.as_str());
            }
            out.entry(edge.label.as_str()).or_default().push(edge);
        }
        Self {
            by_id,
            file_of,
            out,
        }
    }
    fn edges(&self, label: &str) -> &[&'a Edge] {
        self.out.get(label).map_or(&[], Vec::as_slice)
    }
    fn label(&self, id: &str) -> Option<&str> {
        self.by_id.get(id).map(|n| n.label.as_str())
    }
    /// The file of a symbol, or the id itself when it is a file.
    fn file(&self, id: &'a str) -> Option<&'a str> {
        if self.label(id) == Some("File") {
            return Some(id);
        }
        self.file_of.get(id).copied()
    }
}

/// What the builder knows about containers while it builds.
struct Owners<'a> {
    /// Trigger id → container.
    trigger: BTreeMap<&'a str, usize>,
    /// Fact id (symbol or file) → containers whose flows reach it.
    touched: BTreeMap<&'a str, BTreeSet<usize>>,
    /// Repository → its containers, in creation order.
    by_repo: BTreeMap<&'a str, Vec<usize>>,
}

impl<'a> Owners<'a> {
    /// Containers that own `id`, and whether the ownership is only by
    /// repository (no flow of theirs reaches it).
    fn of(&self, index: &Index<'a>, id: &'a str) -> (Vec<usize>, bool) {
        let mut found: BTreeSet<usize> = BTreeSet::new();
        for key in [Some(id), index.file(id)].into_iter().flatten() {
            if let Some(owners) = self.touched.get(key) {
                found.extend(owners);
            }
        }
        if !found.is_empty() {
            return (found.into_iter().collect(), false);
        }
        match self.by_repo.get(repo_of(id)).map(Vec::as_slice) {
            Some([only]) => (vec![*only], false),
            Some([first, ..]) => (vec![*first], true),
            _ => (Vec::new(), false),
        }
    }
}

/// What the host supplies beside the facts.
pub(crate) struct Supplied<'s, 'a> {
    pub(crate) knowledge: Option<&'s Stories<'a>>,
    pub(crate) repo_names: &'s [(String, String)],
    /// The export mode: `verified-only` draws nothing InferredWeak.
    pub(crate) mode: ExportMode,
}

impl<'a> Model<'a> {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn new(
        nodes: &[&'a Node],
        edges: &[&'a Edge],
        flows: &'a [Flow],
        supplied: &Supplied<'_, 'a>,
    ) -> Self {
        // Never the caller's order, and each fact once.
        let mut sorted_nodes: Vec<&'a Node> = nodes.to_vec();
        sorted_nodes.sort_by(|a, b| a.id.cmp(&b.id));
        sorted_nodes.dedup_by(|a, b| a.id == b.id);
        let mut sorted_edges: Vec<&'a Edge> = edges.to_vec();
        sorted_edges.sort_by(|a, b| (&a.src, &a.label, &a.dst).cmp(&(&b.src, &b.label, &b.dst)));
        sorted_edges.dedup_by(|a, b| a.src == b.src && a.label == b.label && a.dst == b.dst);
        let mut sorted_flows: Vec<&'a Flow> = flows.iter().collect();
        sorted_flows.sort_by(|a, b| a.trigger.cmp(&b.trigger));
        let (nodes, edges) = (sorted_nodes.as_slice(), sorted_edges.as_slice());
        let index = Index::new(nodes, edges);
        let name_of: BTreeMap<&str, &str> = supplied
            .repo_names
            .iter()
            .map(|(key, name)| (key.as_str(), name.as_str()))
            .collect();
        // Registered repositories: the Repo facts, else the files' repos.
        let mut repos: BTreeSet<&'a str> = nodes
            .iter()
            .filter(|n| n.label == "Repo")
            .filter_map(|n| n.id.strip_prefix("repo:"))
            .collect();
        if repos.is_empty() {
            repos = nodes
                .iter()
                .filter(|n| n.label == "File")
                .map(|n| repo_of(&n.id))
                .filter(|r| !r.is_empty())
                .collect();
        }
        let repo_name = |repo: &str| -> String {
            name_of.get(repo).map_or_else(
                || repo.rsplit('/').next().unwrap_or(repo).to_string(),
                |name| (*name).to_string(),
            )
        };
        let by_label = |label: &str, repo: &str| -> Vec<&'a Node> {
            nodes
                .iter()
                .copied()
                .filter(|n| n.label == label && repo_of(&n.id) == repo)
                .collect()
        };
        // Declared services, from the binding citations on resolved calls:
        // service name and base URL by the repository they bind.
        let mut services: BTreeMap<&'a str, BTreeSet<(&'a str, &'a str)>> = BTreeMap::new();
        for edge in index.edges("FETCHES") {
            for citation in edge.props["binding"].as_array().into_iter().flatten() {
                if let (Some(repo), Some(service), Some(base)) = (
                    citation["repo"].as_str(),
                    citation["service"].as_str(),
                    citation["base_url"].as_str(),
                ) {
                    services.entry(repo).or_default().insert((service, base));
                }
            }
        }

        let mut elements: Vec<Element<'a>> = Vec::new();
        let mut owners = Owners {
            trigger: BTreeMap::new(),
            touched: BTreeMap::new(),
            by_repo: BTreeMap::new(),
        };
        let push = |elements: &mut Vec<Element<'a>>,
                    owners: &mut Owners<'a>,
                    element: Element<'a>,
                    triggers: &[&'a str]|
         -> usize {
            let at = elements.len();
            if let Some(repo) = element.repo {
                owners.by_repo.entry(repo).or_default().push(at);
            }
            for trigger in triggers {
                owners.trigger.insert(trigger, at);
            }
            elements.push(element);
            at
        };
        let web_repos = repos
            .iter()
            .filter(|repo| {
                !by_label("Endpoint", repo).is_empty()
                    || by_label("EntryPoint", repo)
                        .iter()
                        .any(|e| e.props["kind"] == "http-server")
            })
            .count();
        let mut operator_containers: Vec<(usize, Vec<&'a Node>)> = Vec::new();
        let mut smtp_listeners: Vec<(usize, &'a Node)> = Vec::new();
        let mut db_listeners: Vec<(usize, &'a Node)> = Vec::new();
        let mut workers: Vec<(usize, &'a Node)> = Vec::new();
        let mut client_containers: Vec<(usize, Vec<&'a Node>)> = Vec::new();
        let mut page_containers: Vec<(usize, Vec<&'a Node>)> = Vec::new();
        let mut web_of: BTreeMap<&'a str, usize> = BTreeMap::new();
        // Repositories in the order of their names, so the picture does not
        // follow random local registry keys.
        let mut ordered: Vec<&'a str> = repos.iter().copied().collect();
        ordered.sort_by_key(|repo| (repo_name(repo), *repo));
        for &repo in &ordered {
            let endpoints = by_label("Endpoint", repo);
            let entries = by_label("EntryPoint", repo);
            let http: Vec<&Node> = entries
                .iter()
                .copied()
                .filter(|e| e.props["kind"] == "http-server")
                .collect();
            let pages: Vec<&Node> = by_label("Screen", repo)
                .into_iter()
                .filter(|s| s.props["server_rendered"] == true)
                .collect();
            if !endpoints.is_empty() || !http.is_empty() {
                let frameworks: BTreeSet<String> = endpoints
                    .iter()
                    .filter_map(|e| e.props["framework"].as_str())
                    .map(framework_name)
                    .collect();
                let languages: BTreeSet<&str> = http
                    .iter()
                    .filter_map(|e| e.props["file"].as_str())
                    .filter_map(language)
                    .collect();
                let mut technology: Vec<String> =
                    languages.iter().map(|l| (*l).to_string()).collect();
                technology.extend(frameworks);
                let name = if web_repos > 1 {
                    format!("{} web application", repo_name(repo))
                } else {
                    "Web application".into()
                };
                let mut description = format!(
                    "Serves {} HTTP endpoint{}",
                    endpoints.len(),
                    if endpoints.len() == 1 { "" } else { "s" }
                );
                if !pages.is_empty() {
                    let _ = write!(description, ", {} of them pages", pages.len());
                }
                if let Some(bases) = services.get(repo) {
                    let bases: Vec<&str> = bases.iter().map(|(_, base)| *base).collect();
                    let _ = write!(description, ", at {}", bases.join(", "));
                }
                let triggers: Vec<&str> = endpoints
                    .iter()
                    .chain(&http)
                    .map(|n| n.id.as_str())
                    .collect();
                let mut facts: Vec<Fact<'a>> = http.iter().map(|n| Fact::Node(n)).collect();
                facts.extend(endpoints.iter().map(|n| Fact::Node(n)));
                let at = push(
                    &mut elements,
                    &mut owners,
                    Element {
                        kind: Kind::Container,
                        name,
                        technology: technology.join(", "),
                        description,
                        repo: Some(repo),
                        facts,
                        gap: false,
                        weak: false,
                        weight: endpoints.len().max(1) * 4,
                    },
                    &triggers,
                );
                web_of.insert(repo, at);
                if !pages.is_empty() {
                    page_containers.push((at, pages));
                }
            }
            let mut operator: Vec<&Node> = Vec::new();
            for entry in &entries {
                let kind = entry.props["kind"].as_str().unwrap_or("process");
                // Android components and workers run inside the app, which
                // the client application below is (AC-1110).
                if kind == "http-server" || android_kind(kind) {
                    continue;
                }
                if matches!(kind, "command" | "operator-shell") {
                    operator.push(entry);
                    continue;
                }
                let mut file = entry.props["file"].as_str().unwrap_or_default();
                if kind == "edge-worker" {
                    // The module the worker config names, not the config.
                    if let Some(module) = index
                        .edges("ENTRY")
                        .iter()
                        .find(|e| e.src == entry.id)
                        .and_then(|e| e.dst.strip_prefix("file:"))
                        .and_then(|dst| dst.split_once('@'))
                        .map(|(_, path)| path)
                    {
                        file = module;
                    }
                }
                let what = match kind {
                    "smtp-listener" => "SMTP listener",
                    "database-listener" => "Database listener",
                    "queue-consumer" => "Queue consumer",
                    "polling-loop" => "Polling loop",
                    "edge-worker" => "Cloudflare Worker",
                    _ => "Process",
                };
                let mut technology = language(file).map(str::to_string).unwrap_or_default();
                if !technology.is_empty() {
                    technology.push_str(", ");
                }
                technology.push_str(what);
                let port = entry.props["port"]
                    .as_u64()
                    .map(|port| format!(" on port {port}"))
                    .unwrap_or_default();
                let description = match kind {
                    "smtp-listener" => format!("Runs `{file}`; receives mail over SMTP{port}"),
                    "database-listener" => {
                        format!("Runs `{file}`; woken by database notifications (LISTEN)")
                    }
                    "queue-consumer" => format!("Runs `{file}`; consumes queue messages"),
                    "polling-loop" => format!("Runs `{file}` in a loop until it is stopped"),
                    "edge-worker" => match entry.props["route"].as_str() {
                        Some(route) => {
                            format!("Runs `{file}` on Cloudflare's edge; serves `{route}`")
                        }
                        None => format!("Runs `{file}` on Cloudflare's edge"),
                    },
                    _ => format!("Runs `{file}` as its own process{port}"),
                };
                let at = push(
                    &mut elements,
                    &mut owners,
                    Element {
                        kind: Kind::Container,
                        name: entry.props["worker"]
                            .as_str()
                            .filter(|_| kind == "edge-worker")
                            .map_or_else(|| stem_title(file), stem_title),
                        technology,
                        description,
                        repo: Some(repo),
                        facts: vec![Fact::Node(entry)],
                        gap: false,
                        weak: false,
                        weight: 3,
                    },
                    &[entry.id.as_str()],
                );
                match kind {
                    "smtp-listener" => smtp_listeners.push((at, entry)),
                    "database-listener" => db_listeners.push((at, entry)),
                    "edge-worker" => workers.push((at, entry)),
                    _ => {}
                }
            }
            if !operator.is_empty() {
                let commands = operator
                    .iter()
                    .filter(|e| e.props["kind"] == "command")
                    .count();
                let shells = operator.len() - commands;
                let languages: BTreeSet<&str> = operator
                    .iter()
                    .filter_map(|e| e.props["file"].as_str())
                    .filter_map(language)
                    .collect();
                let mut description = format!(
                    "{commands} command{} a person runs",
                    if commands == 1 { "" } else { "s" }
                );
                if shells > 0 {
                    description.push_str(", and an interactive operator shell");
                }
                let triggers: Vec<&str> = operator.iter().map(|n| n.id.as_str()).collect();
                let at = push(
                    &mut elements,
                    &mut owners,
                    Element {
                        kind: Kind::Container,
                        name: "Operator commands".into(),
                        technology: languages.into_iter().collect::<Vec<_>>().join(", "),
                        description,
                        repo: Some(repo),
                        facts: operator.iter().map(|n| Fact::Node(n)).collect(),
                        gap: false,
                        weak: false,
                        weight: 2,
                    },
                    &triggers,
                );
                operator_containers.push((at, operator));
            }
            let schedules = by_label("Schedule", repo);
            if !schedules.is_empty() {
                let formats: BTreeSet<&str> = schedules
                    .iter()
                    .filter_map(|s| s.props["format"].as_str())
                    .collect();
                let files: BTreeSet<&str> = schedules
                    .iter()
                    .filter_map(|s| s.props["file"].as_str())
                    .collect();
                let description = format!(
                    "{} job{} on cron schedules, declared in {}",
                    schedules.len(),
                    if schedules.len() == 1 { "" } else { "s" },
                    files
                        .iter()
                        .map(|f| format!("`{f}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let triggers: Vec<&str> = schedules.iter().map(|n| n.id.as_str()).collect();
                push(
                    &mut elements,
                    &mut owners,
                    Element {
                        kind: Kind::Container,
                        name: "Scheduled jobs".into(),
                        technology: formats.into_iter().collect::<Vec<_>>().join(", "),
                        description,
                        repo: Some(repo),
                        facts: schedules.iter().map(|n| Fact::Node(n)).collect(),
                        gap: false,
                        weak: false,
                        weight: 2,
                    },
                    &triggers,
                );
            }
            let android: Vec<&Node> = by_label("EntryPoint", repo)
                .into_iter()
                .filter(|e| e.props["kind"].as_str().is_some_and(android_kind))
                .collect();
            // A repository another container already owns keeps its
            // Android components and workers in an app container of their
            // own (AC-1110).
            let owned = owners.by_repo.contains_key(repo);
            if owned && android.is_empty() {
                continue;
            }
            let unowned = |label: &str| {
                if owned {
                    Vec::new()
                } else {
                    by_label(label, repo)
                }
            };
            // A client application: screens or an extension, no process.
            let extension = unowned("Extension").into_iter().next();
            let screens = unowned("Screen");
            let contexts = unowned("ExtensionContext");
            let commands = unowned("Command");
            if extension.is_some()
                || !screens.is_empty()
                || !contexts.is_empty()
                || !android.is_empty()
            {
                let routers: BTreeSet<&str> = screens
                    .iter()
                    .filter_map(|s| s.props["router"].as_str())
                    .filter_map(router_technology)
                    .collect();
                let mut technology: Vec<String> = Vec::new();
                let name = match extension {
                    Some(ext) => {
                        let version = ext.props["manifest_version"].as_u64();
                        technology.push(match version {
                            Some(v) => format!("Browser extension, Manifest V{v}"),
                            None => "Browser extension".into(),
                        });
                        ext.props["name"]
                            .as_str()
                            .filter(|n| !n.is_empty())
                            .map_or_else(|| repo_name(repo), str::to_string)
                    }
                    None if owned => format!("{} Android app", repo_name(repo)),
                    None => repo_name(repo),
                };
                if !android.is_empty() && routers.iter().all(|r| !r.starts_with("Android")) {
                    technology.push("Android".into());
                }
                technology.extend(routers.iter().map(|r| (*r).to_string()));
                let mut parts = vec![format!(
                    "{} screen{}",
                    screens.len(),
                    if screens.len() == 1 { "" } else { "s" }
                )];
                if screens.is_empty() && !android.is_empty() {
                    parts.clear();
                }
                let scripts = contexts
                    .iter()
                    .filter(|c| c.props["kind"] == "content-script")
                    .count();
                if scripts > 0 {
                    parts.push(format!(
                        "{scripts} content script{}",
                        if scripts == 1 { "" } else { "s" }
                    ));
                }
                let components = android
                    .iter()
                    .filter(|e| e.props["kind"] != "background-job")
                    .count();
                if components > 0 {
                    parts.push(format!(
                        "{components} Android component{}",
                        if components == 1 { "" } else { "s" }
                    ));
                }
                let jobs = android.len() - components;
                if jobs > 0 {
                    parts.push(format!(
                        "{jobs} background job{}",
                        if jobs == 1 { "" } else { "s" }
                    ));
                }
                if !commands.is_empty() {
                    parts.push(format!(
                        "{} keyboard command{}",
                        commands.len(),
                        if commands.len() == 1 { "" } else { "s" }
                    ));
                }
                let mut facts: Vec<Fact<'a>> = extension.into_iter().map(Fact::Node).collect();
                facts.extend(contexts.iter().map(|n| Fact::Node(n)));
                facts.extend(screens.iter().map(|n| Fact::Node(n)));
                facts.extend(android.iter().map(|n| Fact::Node(n)));
                let triggers: Vec<&str> = screens
                    .iter()
                    .chain(&contexts)
                    .chain(&commands)
                    .chain(&android)
                    .map(|n| n.id.as_str())
                    .collect();
                let at = push(
                    &mut elements,
                    &mut owners,
                    Element {
                        kind: Kind::Container,
                        name,
                        technology: technology.join(", "),
                        description: format!("Client application: {}", parts.join(", ")),
                        repo: Some(repo),
                        facts,
                        gap: false,
                        weak: false,
                        weight: screens.len().max(1) * 2,
                    },
                    &triggers,
                );
                client_containers.push((at, screens));
                continue;
            }
            // Code with no recovered way in: drawn, as a Gap.
            let files = by_label("File", repo);
            if files.is_empty() {
                continue;
            }
            let mut languages: BTreeMap<&str, usize> = BTreeMap::new();
            for file in &files {
                if let Some(lang) = file.props["path"].as_str().and_then(language) {
                    *languages.entry(lang).or_default() += 1;
                }
            }
            let mut ranked: Vec<(&str, usize)> = languages.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            push(
                &mut elements,
                &mut owners,
                Element {
                    kind: Kind::Container,
                    name: repo_name(repo),
                    technology: ranked
                        .iter()
                        .take(2)
                        .map(|(l, _)| *l)
                        .collect::<Vec<_>>()
                        .join(", "),
                    description: format!(
                        "{} file{}; no process start, endpoint or screen recovered",
                        files.len(),
                        if files.len() == 1 { "" } else { "s" }
                    ),
                    repo: Some(repo),
                    facts: Vec::new(),
                    gap: true,
                    weak: false,
                    weight: 1,
                },
                &[],
            );
        }

        // What each container's flows pass through, and the files those
        // facts are defined in.
        // A flow that crosses into another repository (a client's call to
        // a server endpoint) does not make the server's code the client's:
        // past the endpoint it is the endpoint's container's.
        for flow in sorted_flows {
            let Some(&container) = owners.trigger.get(flow.trigger.as_str()) else {
                continue;
            };
            let mut within: BTreeMap<&str, usize> = BTreeMap::new();
            within.insert(elements[container].repo.unwrap_or_default(), container);
            for hop in &flow.hops {
                for id in [hop.src.as_str(), hop.dst.as_str()] {
                    if let Some(&entered) = owners.trigger.get(id) {
                        within.insert(repo_of(id), entered);
                    }
                    let Some(&container) = within.get(repo_of(id)) else {
                        continue;
                    };
                    owners.touched.entry(id).or_default().insert(container);
                    if let Some(file) = index.file(id) {
                        owners.touched.entry(file).or_default().insert(container);
                    }
                }
            }
        }
        for label in ["ENTRY", "HANDLES"] {
            for edge in index.edges(label) {
                if let Some(&container) = owners.trigger.get(edge.src.as_str()) {
                    owners
                        .touched
                        .entry(edge.dst.as_str())
                        .or_default()
                        .insert(container);
                    if let Some(file) = index.file(edge.dst.as_str()) {
                        owners.touched.entry(file).or_default().insert(container);
                    }
                }
            }
        }

        let strict = supplied.mode == ExportMode::VerifiedOnly;
        let mut relations = Relations(BTreeMap::new(), strict);

        // Persons.
        if !operator_containers.is_empty() {
            let facts: Vec<Fact<'a>> = operator_containers
                .iter()
                .flat_map(|(_, entries)| entries.iter().map(|n| Fact::Node(n)))
                .collect();
            let at = push(
                &mut elements,
                &mut owners,
                Element {
                    kind: Kind::Person,
                    name: "Operator".into(),
                    technology: String::new(),
                    description: "Runs maintenance commands and the operator shell".into(),
                    repo: None,
                    facts,
                    gap: false,
                    weak: false,
                    weight: 1,
                },
                &[],
            );
            for (container, entries) in &operator_containers {
                for entry in entries {
                    relations.add(
                        (at, *container),
                        "Runs",
                        ("command", "commands"),
                        "",
                        Some(&entry.id),
                        [Fact::Node(entry)],
                        false,
                    );
                }
            }
        }
        if !strict && (!client_containers.is_empty() || !page_containers.is_empty()) {
            let facts: Vec<Fact<'a>> = client_containers
                .iter()
                .chain(&page_containers)
                .flat_map(|(container, screens)| {
                    let shown = screens.first().map(|n| Fact::Node(n));
                    shown.into_iter().chain(
                        elements[*container]
                            .facts
                            .first()
                            .copied()
                            .filter(|_| screens.is_empty()),
                    )
                })
                .collect();
            let at = push(
                &mut elements,
                &mut owners,
                Element {
                    kind: Kind::Person,
                    name: "User".into(),
                    technology: String::new(),
                    description: "Uses the system's screens and pages; no Actor fact names who"
                        .into(),
                    repo: None,
                    facts,
                    gap: false,
                    weak: true,
                    weight: 1,
                },
                &[],
            );
            for (container, screens) in client_containers.iter().chain(&page_containers) {
                let facts: Vec<Fact<'a>> = if screens.is_empty() {
                    elements[*container].facts.iter().take(1).copied().collect()
                } else {
                    screens.iter().map(|n| Fact::Node(n)).collect()
                };
                let pages = page_containers.iter().any(|(c, _)| c == container);
                relations.add(
                    (at, *container),
                    "Uses",
                    ("", ""),
                    if pages { "Web browser" } else { "" },
                    None,
                    facts,
                    true,
                );
            }
        }
        for (container, entry) in &smtp_listeners {
            let mut description = "Deliver mail to the system's addresses".to_string();
            if let Some(port) = entry.props["port"].as_u64() {
                let _ = write!(description, " (SMTP port {port})");
            }
            let at = push(
                &mut elements,
                &mut owners,
                Element {
                    kind: Kind::External,
                    name: "Mail senders".into(),
                    technology: "SMTP".into(),
                    description,
                    repo: None,
                    facts: vec![Fact::Node(entry)],
                    gap: false,
                    weak: false,
                    weight: 2,
                },
                &[],
            );
            relations.add(
                (at, *container),
                "Delivers mail to",
                ("", ""),
                "SMTP",
                None,
                [Fact::Node(entry)],
                false,
            );
        }

        // A Cloudflare Worker: who sends it webhooks, and which endpoints
        // of the system the URLs in its config name (a configured address,
        // not a traced call, so the arrow is inferred).
        let endpoints: Vec<&'a Node> = nodes
            .iter()
            .copied()
            .filter(|n| n.label == "Endpoint")
            .collect();
        for (worker, entry) in &workers {
            if let Some(sender) = entry.props["webhook_from"].as_str() {
                let at = push(
                    &mut elements,
                    &mut owners,
                    Element {
                        kind: Kind::External,
                        name: sender.to_string(),
                        technology: "HTTPS webhooks".into(),
                        description: format!("Sends signed {sender} webhooks"),
                        repo: None,
                        facts: vec![Fact::Node(entry)],
                        gap: false,
                        weak: false,
                        weight: 2,
                    },
                    &[],
                );
                relations.add(
                    (at, *worker),
                    "Sends webhooks to",
                    ("", ""),
                    "HTTPS",
                    None,
                    [Fact::Node(entry)],
                    false,
                );
            }
            let urls = entry.props["configured_urls"].as_object();
            for url in urls
                .into_iter()
                .flat_map(|m| m.values())
                .filter_map(|v| v.as_str())
            {
                let Some(path) = url_path(url) else { continue };
                // The most specific routes the path matches: fewest
                // parameter segments.
                let matched: Vec<(usize, &&'a Node)> = endpoints
                    .iter()
                    .filter_map(|endpoint| {
                        let pattern = endpoint.props["path"].as_str()?;
                        path_matches(pattern, path).then(|| {
                            let params = pattern
                                .split('/')
                                .filter(|p| p.starts_with([':', '<', '{']) || *p == "*")
                                .count();
                            (params, endpoint)
                        })
                    })
                    .collect();
                let fewest = matched.iter().map(|(n, _)| *n).min();
                for (_, endpoint) in matched.iter().filter(|(n, _)| Some(*n) == fewest) {
                    let Some(&container) = owners.trigger.get(endpoint.id.as_str()) else {
                        continue;
                    };
                    if container == *worker {
                        continue;
                    }
                    relations.add(
                        (*worker, container),
                        "Calls",
                        ("", ""),
                        "HTTPS",
                        None,
                        [Fact::Node(entry), Fact::Node(endpoint)],
                        true,
                    );
                }
            }
        }

        // Data stores and providers, from observed and declared resources.
        let mut store_of: BTreeMap<String, usize> = BTreeMap::new();
        let mut store_repo: Vec<(usize, &'a str)> = Vec::new();
        let mut provider_of: BTreeMap<String, usize> = BTreeMap::new();
        let mut uses: BTreeMap<&str, Vec<&'a Edge>> = BTreeMap::new();
        for edge in index.edges("USES") {
            uses.entry(edge.dst.as_str()).or_default().push(edge);
        }
        let mut sql_stores: BTreeMap<&'a str, Vec<usize>> = BTreeMap::new();
        for node in nodes.iter().copied() {
            let observed_gap = node.label == "Gap" && node.id.starts_with("gap:observed:");
            if node.label != "Resource" && !observed_gap {
                continue;
            }
            if node.props["kind"] == "http-api" {
                continue; // `external.md`'s origins
            }
            let users = uses.get(node.id.as_str()).cloned().unwrap_or_default();
            if node.label == "Resource" && !is_external_provider(node) {
                // Only a resource that holds data is a store; other
                // infrastructure (instances, functions, roles, modules)
                // belongs to a deployment view, not this one.
                if store_kind(node).is_empty() {
                    continue;
                }
                // A logical id names a store within its repository: two
                // repositories reading the same variable name are two
                // stores unless binding evidence says otherwise.
                // A store known only by its URL variable, read where another
                // variable of the same provider is read as its fallback, is
                // one store located by either.
                let fallback = node.props["store_from_env_key"] == true;
                let key = if fallback {
                    let mut readers: Vec<&str> = users.iter().map(|e| e.src.as_str()).collect();
                    readers.sort_unstable();
                    format!(
                        "{}|env-store|{}|{}",
                        repo_of(&node.id),
                        node.props["provider"].as_str().unwrap_or_default(),
                        readers.join(",")
                    )
                } else {
                    format!(
                        "{}|{}",
                        repo_of(&node.id),
                        node.props["logical_id"].as_str().unwrap_or(&node.id)
                    )
                };
                let at = if let Some(&at) = store_of.get(&key) {
                    if let (true, Some(env)) = (fallback, node.props["env_key"].as_str()) {
                        let _ = write!(elements[at].description, " or `{env}`");
                    }
                    elements[at].facts.push(Fact::Node(node));
                    at
                } else {
                    let mut description = String::new();
                    if let Some(env) = node.props["env_key"].as_str() {
                        let _ = write!(description, "Located by `{env}`");
                    }
                    if node.props["kind"] == "browser-storage" {
                        description = "Held in the user's browser".into();
                    }
                    let at = push(
                        &mut elements,
                        &mut owners,
                        Element {
                            kind: Kind::Store,
                            name: store_name(node),
                            technology: node.props["module"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            description,
                            repo: None,
                            facts: vec![Fact::Node(node)],
                            gap: false,
                            weak: false,
                            weight: 2,
                        },
                        &[],
                    );
                    store_repo.push((at, repo_of(&node.id)));
                    store_of.insert(key, at);
                    at
                };
                if node.props["provider"] == "sql-database" {
                    sql_stores.entry(repo_of(&node.id)).or_default().push(at);
                }
                for edge in users {
                    let (containers, weak) = owners.of(&index, edge.src.as_str());
                    for container in containers {
                        relations.add(
                            (container, at),
                            "Reads and writes",
                            ("", ""),
                            node.props["module"].as_str().unwrap_or_default(),
                            None,
                            [Fact::Edge(edge)],
                            weak,
                        );
                    }
                }
                continue;
            }
            // A provider outside the code base: SMTP, an auth provider, an
            // SDK of a hosted service. A client whose instance is computed is
            // the same provider with its host not known.
            let provider = node.props["provider"]
                .as_str()
                .unwrap_or("provider")
                .to_string();
            let unresolved = observed_gap || node.props["instance_unresolved"] == true;
            let at = if let Some(&at) = provider_of.get(&provider) {
                elements[at].facts.push(Fact::Node(node));
                at
            } else {
                let at = push(
                    &mut elements,
                    &mut owners,
                    Element {
                        kind: Kind::External,
                        name: provider_name(&provider),
                        technology: node.props["module"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        description: String::new(),
                        repo: None,
                        facts: vec![Fact::Node(node)],
                        gap: false,
                        weak: false,
                        weight: 2,
                    },
                    &[],
                );
                provider_of.insert(provider.clone(), at);
                at
            };
            if unresolved {
                elements[at].description =
                    "Host computed at run time; not known from source".into();
            }
            let protocol = if provider == "smtp" { "SMTP" } else { "" };
            for edge in users {
                let (containers, weak) = owners.of(&index, edge.src.as_str());
                for container in containers {
                    relations.add(
                        (container, at),
                        if provider == "smtp" {
                            "Sends mail through"
                        } else {
                            "Calls"
                        },
                        ("", ""),
                        protocol,
                        Some(&edge.src),
                        [Fact::Edge(edge)],
                        weak,
                    );
                }
            }
        }
        // A repository's one SQL store holds its mapped data entities: a
        // container whose code reads or writes them uses the store.
        for (repo, stores) in &sql_stores {
            let [store] = stores.as_slice() else {
                continue;
            };
            for label in ["READS", "WRITES"] {
                for edge in index.edges(label) {
                    if repo_of(&edge.src) != *repo
                        || index.label(&edge.dst) != Some("DataEntity")
                        || index.label(&edge.src) != Some("Symbol")
                    {
                        continue;
                    }
                    let (containers, weak) = owners.of(&index, edge.src.as_str());
                    if weak {
                        continue;
                    }
                    for container in containers {
                        relations.add(
                            (container, *store),
                            "Reads and writes",
                            ("table", "tables"),
                            elements[*store].technology.as_str(),
                            Some(&edge.dst),
                            [Fact::Edge(edge)],
                            false,
                        );
                    }
                }
            }
        }
        // Stores of one name in two repositories are told apart by their
        // repository.
        let mut named: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for (at, repo) in &store_repo {
            named
                .entry(elements[*at].name.clone())
                .or_default()
                .insert(repo);
        }
        for (at, repo) in &store_repo {
            if named[&elements[*at].name].len() > 1 {
                elements[*at].name = format!("{} ({})", elements[*at].name, repo_name(repo));
            }
        }
        for (container, entry) in &db_listeners {
            if let Some([store]) = sql_stores.get(repo_of(&entry.id)).map(Vec::as_slice) {
                relations.add(
                    (*store, *container),
                    "Notifies",
                    ("", ""),
                    "LISTEN/NOTIFY",
                    None,
                    [Fact::Node(entry)],
                    true,
                );
            }
        }

        // Outside origins and unclassified calls, as `external.md` reads them.
        let boundary = external::Model::new(nodes, edges);
        for (origin, sites) in &boundary.outside {
            let host = origin
                .split_once("://")
                .map_or(origin.as_str(), |(_, host)| host);
            let facts: Vec<Fact<'a>> = sites.iter().flat_map(|s| s.facts()).collect();
            let at = push(
                &mut elements,
                &mut owners,
                Element {
                    kind: Kind::External,
                    name: host.to_string(),
                    technology: "HTTPS".into(),
                    description: format!(
                        "{} call site{} in the code",
                        sites.len(),
                        if sites.len() == 1 { "" } else { "s" }
                    ),
                    repo: None,
                    facts,
                    gap: false,
                    weak: false,
                    weight: sites.len(),
                },
                &[],
            );
            for site in sites {
                let Some(caller) = site.caller() else {
                    continue;
                };
                let (containers, weak) = owners.of(&index, caller);
                for container in containers {
                    relations.add(
                        (container, at),
                        "Calls",
                        ("", ""),
                        if origin.starts_with("https:") {
                            "HTTPS"
                        } else {
                            "HTTP"
                        },
                        Some(caller),
                        site.facts(),
                        weak,
                    );
                }
            }
        }
        let unresolved: Vec<&external::Row<'a>> = boundary
            .unclassified
            .iter()
            .filter(|r| r.is_http())
            .collect();
        if !unresolved.is_empty() {
            let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
            for row in &unresolved {
                *reasons.entry(row.reason_text()).or_default() += 1;
            }
            let (reason, _) = reasons
                .iter()
                .max_by_key(|(_, n)| **n)
                .map(|(r, n)| (*r, *n))
                .unwrap_or_default();
            let at = push(
                &mut elements,
                &mut owners,
                Element {
                    kind: Kind::External,
                    name: "Unresolved HTTP hosts".into(),
                    technology: "HTTP".into(),
                    description: format!(
                        "{} outbound call{} whose host is not classified; most: {reason}",
                        unresolved.len(),
                        if unresolved.len() == 1 { "" } else { "s" }
                    ),
                    repo: None,
                    facts: unresolved.iter().map(|r| Fact::Node(r.node)).collect(),
                    gap: true,
                    weak: false,
                    weight: unresolved.len(),
                },
                &[],
            );
            for row in &unresolved {
                for edge in &row.edges {
                    let (containers, weak) = owners.of(&index, edge.src.as_str());
                    for container in containers {
                        relations.add(
                            (container, at),
                            "Calls",
                            ("", ""),
                            "HTTP",
                            Some(&row.node.id),
                            [Fact::Edge(edge), Fact::Node(row.node)],
                            weak,
                        );
                    }
                }
            }
        }

        // Container to container: client calls bound to served endpoints,
        // and channels a container publishes and another subscribes to.
        for edge in index.edges("FETCHES") {
            if index.label(&edge.dst) != Some("Endpoint") {
                continue;
            }
            let Some(&server) = owners.trigger.get(edge.dst.as_str()) else {
                continue;
            };
            let (callers, weak) = owners.of(&index, edge.src.as_str());
            for caller in callers {
                if caller == server {
                    continue;
                }
                let secure = edge.props["url"]
                    .as_str()
                    .is_some_and(|u| u.starts_with("https:"));
                relations.add(
                    (caller, server),
                    "Calls",
                    ("API endpoint", "API endpoints"),
                    if secure { "HTTPS" } else { "HTTP" },
                    Some(&edge.dst),
                    [Fact::Edge(edge)],
                    weak,
                );
            }
        }
        let mut subscribers: BTreeMap<&str, Vec<&'a Edge>> = BTreeMap::new();
        for edge in index.edges("SUBSCRIBES") {
            subscribers.entry(edge.dst.as_str()).or_default().push(edge);
        }
        for publish in index.edges("PUBLISHES") {
            let Some(subs) = subscribers.get(publish.dst.as_str()) else {
                continue;
            };
            let channel = index
                .by_id
                .get(publish.dst.as_str())
                .map_or(publish.dst.as_str(), |n| {
                    n.props["name"].as_str().unwrap_or(&n.id)
                });
            let (from, weak_from) = owners.of(&index, publish.src.as_str());
            for sub in subs {
                let (to, weak_to) = owners.of(&index, sub.src.as_str());
                for &a in &from {
                    for &b in &to {
                        relations.add(
                            (a, b),
                            &format!("Sends events via {channel} to"),
                            ("", ""),
                            "",
                            None,
                            [Fact::Edge(publish), Fact::Edge(sub)],
                            weak_from || weak_to,
                        );
                    }
                }
            }
        }
        let relations = relations.into_vec();

        // Components: the areas of the files each container owns.
        let file_areas = supplied.knowledge.map(|k| &k.file_areas);
        let areas_supplied = file_areas.is_some_and(|f| !f.is_empty());
        let mut components = Vec::new();
        if let Some(file_areas) = file_areas.filter(|f| !f.is_empty()) {
            for (container, element) in elements.iter().enumerate() {
                if element.kind != Kind::Container || element.gap {
                    continue;
                }
                let Some(repo) = element.repo else {
                    continue;
                };
                let only = owners.by_repo.get(repo).is_some_and(|c| c.len() == 1);
                let owned: BTreeSet<&'a str> = nodes
                    .iter()
                    .filter(|n| n.label == "File" && repo_of(&n.id) == repo)
                    .map(|n| n.id.as_str())
                    .filter(|id| {
                        only || owners
                            .touched
                            .get(id)
                            .is_some_and(|c| c.contains(&container))
                    })
                    .collect();
                components.push(Self::components(
                    container, &owned, file_areas, &index, &relations, &elements,
                ));
            }
        }

        let system = {
            let named: BTreeSet<&str> = services
                .values()
                .flatten()
                .map(|(service, _)| *service)
                .collect();
            match named.iter().next() {
                Some(only) if named.len() == 1 => (*only).to_string(),
                _ if repos.len() == 1 => repos
                    .iter()
                    .next()
                    .map(|r| repo_name(r))
                    .unwrap_or_default(),
                _ => "The system".into(),
            }
        };
        Self {
            system,
            elements,
            relations,
            components,
            areas_supplied,
        }
    }

    fn components(
        container: usize,
        owned: &BTreeSet<&'a str>,
        file_areas: &BTreeMap<&'a str, &'a Node>,
        index: &Index<'a>,
        relations: &[Relation<'a>],
        elements: &[Element<'a>],
    ) -> Components<'a> {
        let mut groups: BTreeMap<(&str, &str), (&'a Node, Vec<&'a str>)> = BTreeMap::new();
        let mut unassigned = 0;
        for file in owned {
            match file_areas.get(file) {
                Some(area) => groups
                    .entry((area_name(area), area.id.as_str()))
                    .or_insert((area, Vec::new()))
                    .1
                    .push(file),
                None => unassigned += 1,
            }
        }
        let mut ranked: Vec<(&'a Node, Vec<&'a str>)> = groups.into_values().collect();
        ranked.sort_by(|a, b| {
            b.1.len()
                .cmp(&a.1.len())
                .then_with(|| area_name(a.0).cmp(area_name(b.0)))
        });
        let mut component_of: BTreeMap<&'a str, usize> = BTreeMap::new();
        let mut parts: Vec<Element<'a>> = Vec::new();
        for (i, (area, files)) in ranked.iter().enumerate() {
            let slot = i.min(MAX_COMPONENTS);
            if slot == MAX_COMPONENTS && parts.len() == MAX_COMPONENTS {
                parts.push(Element {
                    kind: Kind::Component,
                    name: String::new(),
                    technology: String::new(),
                    description: String::new(),
                    repo: None,
                    facts: Vec::new(),
                    gap: false,
                    weak: true,
                    weight: 0,
                });
            }
            if slot < MAX_COMPONENTS {
                let terms: Vec<&str> = area.props["terms"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.as_str())
                    .take(4)
                    .collect();
                let mut description = format!(
                    "{} file{}",
                    files.len(),
                    if files.len() == 1 { "" } else { "s" }
                );
                if !terms.is_empty() {
                    let _ = write!(description, "; terms: {}", terms.join(", "));
                }
                parts.push(Element {
                    kind: Kind::Component,
                    name: area_name(area).to_string(),
                    technology: String::new(),
                    description,
                    repo: None,
                    facts: vec![Fact::Node(area)],
                    gap: false,
                    weak: false,
                    weight: files.len(),
                });
            } else {
                let folded = &mut parts[MAX_COMPONENTS];
                folded.facts.push(Fact::Node(area));
                folded.weight += files.len();
            }
            for file in files {
                component_of.insert(file, slot);
            }
        }
        if let Some(folded) = parts.get_mut(MAX_COMPONENTS) {
            let areas = folded.facts.len();
            folded.name = if areas == 1 {
                "1 more area".to_string()
            } else {
                format!("{areas} more areas")
            };
            folded.description = if folded.weight == 1 {
                "1 file".to_string()
            } else {
                format!("{} files", folded.weight)
            };
        }
        let area_of = |id: &'a str| -> Option<usize> {
            index
                .file(id)
                .and_then(|file| component_of.get(file).copied())
        };
        let mut out = Relations::default();
        // Between components: calls from a file of one area into another.
        for edge in index.edges("CALLS") {
            if let (Some(a), Some(b)) = (area_of(&edge.src), area_of(&edge.dst)) {
                out.add(
                    (OFFSET + a, OFFSET + b),
                    "Calls",
                    ("function", "functions"),
                    "",
                    Some(&edge.dst),
                    [Fact::Edge(edge)],
                    false,
                );
            }
        }
        // With the container's neighbours: each fact of a container-level
        // relation placed at the component that holds its code.
        for relation in relations {
            let outward = relation.from == container;
            let inward = relation.to == container;
            if !outward && !inward {
                continue;
            }
            let other = if outward { relation.to } else { relation.from };
            if elements[other].kind == Kind::Person {
                continue;
            }
            for fact in &relation.facts {
                let Fact::Edge(edge) = fact else {
                    continue;
                };
                let code = if outward {
                    edge.src.as_str()
                } else {
                    // A client's call enters at the endpoint's handler.
                    match index.edges("HANDLES").iter().find(|h| h.src == edge.dst) {
                        Some(handles) => handles.dst.as_str(),
                        None => continue,
                    }
                };
                let Some(component) = area_of(code) else {
                    continue;
                };
                let ends = if outward {
                    (OFFSET + component, other)
                } else {
                    (other, OFFSET + component)
                };
                let target = if outward { edge.dst.as_str() } else { code };
                out.add(
                    ends,
                    &relation.verb,
                    relation.noun,
                    &relation.technology,
                    Some(target),
                    [*fact],
                    relation.weak,
                );
            }
        }
        Components {
            container,
            elements: parts,
            relations: out.into_vec(),
            unassigned,
        }
    }
}

// ----- Mermaid ---------------------------------------------------------

/// Opens a diagram. Labels carry their own line breaks, so Mermaid's own
/// wrapping is widened past them; the denser levels use the ELK layout,
/// which routes edges around boxes (a renderer without it falls back).
fn open(out: &mut impl fmt::Write, elk: bool) -> fmt::Result {
    out.write_str("```mermaid\n---\nconfig:\n")?;
    if elk {
        out.write_str("  layout: elk\n")?;
    }
    out.write_str("  flowchart:\n    wrappingWidth: 400\n---\nflowchart TB\n")
}

/// One drawn arrow: its verbs, its technologies, and whether every
/// relation under it is weak.
type Arrow = (BTreeSet<String>, BTreeSet<String>, bool);

const CLASSES: &str = "    classDef person fill:#08427b,stroke:#052e56,color:#ffffff\n    classDef system fill:#1168bd,stroke:#0b4884,color:#ffffff\n    classDef container fill:#438dd5,stroke:#2e6295,color:#ffffff\n    classDef store fill:#438dd5,stroke:#2e6295,color:#ffffff\n    classDef component fill:#85bbf0,stroke:#5d82a8,color:#000000\n    classDef external fill:#8c8c8c,stroke:#6b6b6b,color:#ffffff\n    classDef gap fill:#ffffff,stroke:#c0392b,stroke-width:2px,stroke-dasharray:5 4,color:#222222\n    classDef weak stroke-dasharray:4 3\n";

/// Mermaid label text: entities for what would end or break a label.
struct Label<'t>(&'t str);
impl fmt::Display for Label<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            match c {
                '"' => f.write_str("#quot;")?,
                '#' => f.write_str("#35;")?,
                '<' => f.write_str("#lt;")?,
                '>' => f.write_str("#gt;")?,
                '`' => {}
                '\n' | '\r' => f.write_str(" ")?,
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}

/// Words broken into lines of about `width` characters, joined by `<br/>`.
fn wrapped(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = 0;
    for word in text.split_whitespace() {
        if line > 0 && line + 1 + word.chars().count() > width {
            out.push_str("<br/>");
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        let _ = write!(out, "{}", Label(word));
        line += word.chars().count();
    }
    out
}

fn class_of(element: &Element<'_>) -> &'static str {
    if element.gap {
        return "gap";
    }
    match element.kind {
        Kind::Person => "person",
        Kind::Container => "container",
        Kind::Store => "store",
        Kind::External => "external",
        Kind::Component => "component",
    }
}

fn box_label(element: &Element<'_>, kind: &str) -> String {
    let mut label = format!("<b>{}</b><br/>", wrapped(&element.name, 28));
    if element.technology.is_empty() {
        let _ = write!(label, "<i>[{kind}]</i>");
    } else {
        let _ = write!(
            label,
            "<i>[{kind}: {}]</i>",
            wrapped(&element.technology, 32)
        );
    }
    if !element.description.is_empty() {
        let _ = write!(label, "<br/><br/>{}", wrapped(&element.description, 34));
    }
    label
}

fn shape(out: &mut impl fmt::Write, id: &str, element: &Element<'_>, kind: &str) -> fmt::Result {
    let label = box_label(element, kind);
    let class = class_of(element);
    match element.kind {
        Kind::Person => writeln!(out, "    {id}([\"{label}\"]):::{class}"),
        Kind::Store => writeln!(out, "    {id}[(\"{label}\")]:::{class}"),
        _ => writeln!(out, "    {id}[\"{label}\"]:::{class}"),
    }
}

fn arrow(
    out: &mut impl fmt::Write,
    from: &str,
    to: &str,
    label: &str,
    technology: &str,
    weak: bool,
) -> fmt::Result {
    let mut text = wrapped(label, 26);
    if !technology.is_empty() {
        let _ = write!(text, "<br/><i>[{}]</i>", Label(technology));
    }
    let line = if weak { "-.->" } else { "-->" };
    writeln!(out, "    {from} {line}|\"{text}\"| {to}")
}

impl Model<'_> {
    fn indices(&self, kind: Kind) -> Vec<usize> {
        self.elements
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind == kind)
            .map(|(i, _)| i)
            .collect()
    }

    fn is_inside(&self, i: usize) -> bool {
        matches!(self.elements[i].kind, Kind::Container | Kind::Store)
    }

    /// Level 1: persons, the system as one box, external systems.
    fn context(&self, out: &mut impl fmt::Write) -> fmt::Result {
        open(out, false)?;
        for i in self.indices(Kind::Person) {
            shape(out, &format!("e{i}"), &self.elements[i], "Person")?;
        }
        let containers = self.indices(Kind::Container).len();
        let stores = self.indices(Kind::Store).len();
        writeln!(
            out,
            "    sys[\"<b>{}</b><br/><i>[Software System]</i><br/><br/>{} container{} and {} data store{}\"]:::system",
            Label(&self.system),
            containers,
            if containers == 1 { "" } else { "s" },
            stores,
            if stores == 1 { "" } else { "s" },
        )?;
        let externals = self.drawn(self.indices(Kind::External));
        for &i in &externals.0 {
            shape(out, &format!("e{i}"), &self.elements[i], "External System")?;
        }
        if let Some(folded) = &externals.1 {
            writeln!(
                out,
                "    more[\"<b>{} more external systems</b>\"]:::external",
                folded.len()
            )?;
        }
        let end = |i: usize| -> Option<String> {
            if self.is_inside(i) {
                Some("sys".into())
            } else if externals.0.contains(&i) || self.elements[i].kind == Kind::Person {
                Some(format!("e{i}"))
            } else if externals.1.as_ref().is_some_and(|f| f.contains(&i)) {
                Some("more".into())
            } else {
                None
            }
        };
        // One arrow per pair of ends: the verbs joined, weak only if all are.
        let mut arrows: BTreeMap<(String, String), Arrow> = BTreeMap::new();
        for relation in &self.relations {
            let (Some(a), Some(b)) = (end(relation.from), end(relation.to)) else {
                continue;
            };
            if a == b {
                continue;
            }
            let entry = arrows
                .entry((a, b))
                .or_insert((BTreeSet::new(), BTreeSet::new(), true));
            entry.0.insert(relation.verb.clone());
            if !relation.technology.is_empty() {
                entry.1.insert(relation.technology.clone());
            }
            entry.2 &= relation.weak || relation.confidence() == ConfidenceTier::InferredWeak;
        }
        for ((a, b), (verbs, technology, weak)) in &arrows {
            let verbs: Vec<&str> = verbs.iter().map(String::as_str).collect();
            let technology: Vec<&str> = technology.iter().map(String::as_str).collect();
            arrow(out, a, b, &verbs.join("; "), &technology.join(", "), *weak)?;
        }
        out.write_str(CLASSES)?;
        out.write_str("```\n")
    }

    /// The elements to draw, largest first, and the rest folded.
    fn drawn(&self, mut indices: Vec<usize>) -> (Vec<usize>, Option<Vec<usize>>) {
        if indices.len() <= MAX_DRAWN {
            return (indices, None);
        }
        indices.sort_by(|a, b| {
            self.elements[*b]
                .weight
                .cmp(&self.elements[*a].weight)
                .then(a.cmp(b))
        });
        let rest = indices.split_off(MAX_DRAWN - 1);
        indices.sort_unstable();
        (indices, Some(rest))
    }

    /// Level 2: containers and stores inside the boundary.
    fn containers(&self, out: &mut impl fmt::Write) -> fmt::Result {
        open(out, true)?;
        for i in self.indices(Kind::Person) {
            shape(out, &format!("e{i}"), &self.elements[i], "Person")?;
        }
        writeln!(
            out,
            "    subgraph sys[\"{} [Software System]\"]",
            Label(&self.system)
        )?;
        for i in self.indices(Kind::Container) {
            shape(out, &format!("e{i}"), &self.elements[i], "Container")?;
        }
        for i in self.indices(Kind::Store) {
            shape(out, &format!("e{i}"), &self.elements[i], "Data store")?;
        }
        out.write_str("    end\n")?;
        let externals = self.drawn(self.indices(Kind::External));
        for &i in &externals.0 {
            shape(out, &format!("e{i}"), &self.elements[i], "External System")?;
        }
        if let Some(folded) = &externals.1 {
            writeln!(
                out,
                "    more[\"<b>{} more external systems</b>\"]:::external",
                folded.len()
            )?;
        }
        // A Gap box is reached from the boundary once, so its many callers
        // do not cross the picture; the table lists each.
        let mut to_gap: BTreeSet<usize> = BTreeSet::new();
        for relation in &self.relations {
            let end = |i: usize| -> String {
                if externals.1.as_ref().is_some_and(|f| f.contains(&i)) {
                    "more".into()
                } else {
                    format!("e{i}")
                }
            };
            let target = &self.elements[relation.to];
            if target.gap && target.kind == Kind::External {
                to_gap.insert(relation.to);
                continue;
            }
            // Technology is drawn where it says how two parts talk; for a
            // store or an outside call it is in the table.
            let between = matches!(
                (self.elements[relation.from].kind, target.kind),
                (Kind::Container, Kind::Container) | (Kind::External | Kind::Person, _)
            );
            arrow(
                out,
                &end(relation.from),
                &end(relation.to),
                &relation.label(),
                if between { &relation.technology } else { "" },
                relation.weak || relation.confidence() == ConfidenceTier::InferredWeak,
            )?;
        }
        for gap in to_gap {
            arrow(out, "sys", &format!("e{gap}"), "Calls", "", true)?;
        }
        out.write_str(CLASSES)?;
        out.write_str(
            "    style sys fill:none,stroke:#666666,stroke-dasharray:6 4,color:#333333\n",
        )?;
        out.write_str("```\n")
    }

    /// Level 3: one container's components and its neighbours.
    fn component_diagram(&self, parts: &Components<'_>, out: &mut impl fmt::Write) -> fmt::Result {
        let container = &self.elements[parts.container];
        open(out, true)?;
        let neighbours: BTreeSet<usize> = parts
            .relations
            .iter()
            .flat_map(|r| [r.from, r.to])
            .filter(|i| *i < OFFSET)
            .collect();
        for &i in &neighbours {
            let element = &self.elements[i];
            let kind = match element.kind {
                Kind::Container => "Container",
                Kind::Store => "Data store",
                Kind::External => "External System",
                Kind::Person => "Person",
                Kind::Component => "Component",
            };
            shape(out, &format!("e{i}"), element, kind)?;
        }
        writeln!(
            out,
            "    subgraph box[\"{} [Container]\"]",
            Label(&container.name)
        )?;
        for (i, part) in parts.elements.iter().enumerate() {
            shape(out, &format!("c{i}"), part, "Component")?;
        }
        out.write_str("    end\n")?;
        let id = |i: usize| {
            if i >= OFFSET {
                format!("c{}", i - OFFSET)
            } else {
                format!("e{i}")
            }
        };
        // Calls between components: the strongest only, so the picture
        // stays readable; the table lists every one.
        let mut internal: Vec<&Relation<'_>> = parts
            .relations
            .iter()
            .filter(|r| r.from >= OFFSET && r.to >= OFFSET)
            .collect();
        internal.sort_by(|a, b| {
            b.targets
                .len()
                .cmp(&a.targets.len())
                .then((a.from, a.to).cmp(&(b.from, b.to)))
        });
        let keep: BTreeSet<(usize, usize)> = internal
            .iter()
            .take(parts.elements.len() + 4)
            .map(|r| (r.from, r.to))
            .collect();
        for relation in &parts.relations {
            if relation.from >= OFFSET
                && relation.to >= OFFSET
                && !keep.contains(&(relation.from, relation.to))
            {
                continue;
            }
            arrow(
                out,
                &id(relation.from),
                &id(relation.to),
                &relation.label(),
                &relation.technology,
                relation.weak || relation.confidence() == ConfidenceTier::InferredWeak,
            )?;
        }
        out.write_str(CLASSES)?;
        out.write_str(
            "    style box fill:none,stroke:#666666,stroke-dasharray:6 4,color:#333333\n",
        )?;
        out.write_str("```\n")
    }
}

// ----- Markdown --------------------------------------------------------

const INTRO: &str = "# Architecture (C4)\n\n\
A deterministic projection of the recovered facts onto levels 1 to 3 of the \
[C4 model](https://c4model.com): system context, containers and components. \
No model is involved, and no fact is added or changed. The same model is in \
`architecture.dsl` as a Structurizr DSL workspace.\n\n\
## How to read it\n\n\
- **Containers** are the parts that run on their own: a repository's web \
application (its endpoints), each process its entry points start, its \
operator commands, its scheduled jobs, and each client application (screens \
or an extension). A repository with code and none of these is drawn as a Gap.\n\
- **Data stores** are the resources the code uses that hold data. **External \
systems** are the outside origins of `external.md`, the providers the code \
calls, and one Gap box for the outbound calls whose host is not known from \
source.\n\
- **Components** are the subsystem areas (`knowledge.subsystems`) holding the \
files a container's flows pass through. Their names and boundaries are \
InferredWeak, so a `verified-only` export draws none. A component diagram \
draws the strongest calls between components; its table lists every one.\n\
- **People** are inferred from how the system is entered: an operator where a \
person runs its commands, and a user where it has screens or pages. No \
`Actor` fact is read yet, so the user is InferredWeak.\n\
- Every box and arrow carries the lowest confidence of the facts it rests on. \
An outside origin rests on outbound calls that matched no endpoint, which are \
Gap facts, so its row says Gap; the origin itself is written in the source. \
A dashed arrow is InferredWeak; a dashed red box is a Gap. A use that no \
container's traced flow reaches is drawn from the repository's first \
container, dashed.\n\
- Each table cites at most 12 facts per row in the assertions below; the \
count column gives the total.\n\n";

/// An id without its `label:{repo}@` prefix, for a table cell.
fn short(id: &str) -> &str {
    id.split_once('@').map_or(id, |(_, rest)| rest)
}

fn facts_cell(facts: &[Fact<'_>]) -> String {
    let mut parts: Vec<String> = facts
        .iter()
        .take(3)
        .map(|f| match f {
            Fact::Node(n) => {
                let name = render::NodeName(n).to_string();
                let name = if name == n.id {
                    short(&n.id).to_string()
                } else {
                    name
                };
                format!("{} `{name}`", n.label)
            }
            Fact::Edge(e) => format!("{} from `{}`", e.label, short(&e.src)),
        })
        .collect();
    if facts.len() > 3 {
        parts.push(format!("and {} more", facts.len() - 3));
    }
    if parts.is_empty() {
        "none recovered".into()
    } else {
        parts.join("; ")
    }
}

impl Model<'_> {
    fn element_table(
        &self,
        out: &mut impl fmt::Write,
        indices: &[usize],
        elements: &[Element<'_>],
    ) -> fmt::Result {
        out.write_str("| Element | Kind | Technology | Description | Confidence | Facts | Basis |\n|---|---|---|---|---|---|---|\n")?;
        for &i in indices {
            let e = &elements[i];
            writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} |",
                Safe(&e.name),
                e.kind.label(),
                Safe(if e.technology.is_empty() {
                    "—"
                } else {
                    &e.technology
                }),
                Safe(if e.description.is_empty() {
                    "—"
                } else {
                    &e.description
                }),
                confidence_word(e.confidence()),
                e.facts.len(),
                Safe(facts_cell(&e.facts)),
            )?;
        }
        out.write_str("\n")
    }

    fn name(&self, i: usize, parts: Option<&Components<'_>>) -> String {
        match parts {
            Some(parts) if i >= OFFSET => parts.elements[i - OFFSET].name.clone(),
            _ => self.elements[i].name.clone(),
        }
    }

    fn relation_table(
        &self,
        out: &mut impl fmt::Write,
        relations: &[Relation<'_>],
        parts: Option<&Components<'_>>,
    ) -> fmt::Result {
        if relations.is_empty() {
            return out.write_str("No relationship recovered.\n\n");
        }
        out.write_str("| From | Relationship | To | Technology | Confidence | Facts | Basis |\n|---|---|---|---|---|---|---|\n")?;
        for r in relations {
            writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} |",
                Safe(self.name(r.from, parts)),
                Safe(r.label()),
                Safe(self.name(r.to, parts)),
                Safe(if r.technology.is_empty() {
                    "—"
                } else {
                    &r.technology
                }),
                confidence_word(r.confidence()),
                r.facts.len(),
                Safe(facts_cell(&r.facts)),
            )?;
        }
        out.write_str("\n")
    }

    fn cite(&self, assertions: &mut Assertions) -> Result<(), RenderLimit> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut cite = |assertions: &mut Assertions, fact: &Fact<'_>| -> Result<(), RenderLimit> {
            let key = match fact {
                Fact::Node(n) => format!("n {}", n.id),
                Fact::Edge(e) => format!("e {} {} {}", e.src, e.label, e.dst),
            };
            if !seen.insert(key) {
                return Ok(());
            }
            match fact {
                Fact::Node(n) => assertions.node(n, None),
                Fact::Edge(e) => assertions.edge(e),
            }
        };
        for element in &self.elements {
            for fact in element.facts.iter().take(MAX_CITED) {
                cite(assertions, fact)?;
            }
        }
        for relation in &self.relations {
            for fact in relation.facts.iter().take(MAX_CITED) {
                cite(assertions, fact)?;
            }
        }
        for parts in &self.components {
            for element in &parts.elements {
                for fact in element.facts.iter().take(MAX_CITED) {
                    cite(assertions, fact)?;
                }
            }
            for relation in &parts.relations {
                for fact in relation.facts.iter().take(MAX_CITED) {
                    cite(assertions, fact)?;
                }
            }
        }
        Ok(())
    }
}

/// `architecture.md`.
pub(super) fn markdown(
    model: &Model<'_>,
    mode: ExportMode,
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut content = Text::new(&budget, INTRO)?;
    let count = |kind| model.indices(kind).len();
    writeln!(
        content,
        "Export mode: `{}`. System: {}. People: {}. Containers: {}. Data stores: {}. External systems: {}. Containers with components: {}.\n",
        match mode {
            ExportMode::VerifiedOnly => "verified-only",
            ExportMode::BestEffort => "best-effort",
        },
        Safe(&model.system),
        count(Kind::Person),
        count(Kind::Container),
        count(Kind::Store),
        count(Kind::External),
        model
            .components
            .iter()
            .filter(|c| !c.elements.is_empty())
            .count(),
    )?;

    content.push_str("## Level 1: System context\n\n")?;
    writeln!(
        content,
        "Who uses {} and which systems outside it it depends on.\n",
        Safe(&model.system)
    )?;
    model.context(&mut content)?;
    content.push_str("\n")?;
    let mut outer = model.indices(Kind::Person);
    outer.extend(model.indices(Kind::External));
    model.element_table(&mut content, &outer, &model.elements)?;

    content.push_str("## Level 2: Containers\n\n")?;
    content.push_str(
        "The parts that run on their own, the stores they keep data in, and what they call.\n\n",
    )?;
    model.containers(&mut content)?;
    content.push_str("\n")?;
    let mut inner = model.indices(Kind::Container);
    inner.extend(model.indices(Kind::Store));
    model.element_table(&mut content, &inner, &model.elements)?;
    content.push_str("### Relationships\n\n")?;
    model.relation_table(&mut content, &model.relations, None)?;

    content.push_str("## Level 3: Components\n\n")?;
    if !model.areas_supplied {
        content.push_str(match mode {
            ExportMode::VerifiedOnly => "No component is drawn: subsystem areas are InferredWeak, and a `verified-only` export admits none.\n\n",
            ExportMode::BestEffort => "No component is drawn: no subsystem area was supplied with this export. The knowledge phase computes them after an ingest.\n\n",
        })?;
    }
    for parts in &model.components {
        let container = &model.elements[parts.container];
        writeln!(content, "### {}\n", Safe(&container.name))?;
        if parts.elements.is_empty() {
            writeln!(
                content,
                "None of the {} files this container owns is in a subsystem area.\n",
                parts.unassigned
            )?;
            continue;
        }
        model.component_diagram(parts, &mut content)?;
        content.push_str("\n")?;
        let indices: Vec<usize> = (0..parts.elements.len()).collect();
        model.element_table(&mut content, &indices, &parts.elements)?;
        if parts.unassigned > 0 {
            writeln!(
                content,
                "{} file{} this container owns {} in no area and {} not drawn.\n",
                parts.unassigned,
                if parts.unassigned == 1 { "" } else { "s" },
                if parts.unassigned == 1 { "is" } else { "are" },
                if parts.unassigned == 1 { "is" } else { "are" },
            )?;
        }
        model.relation_table(&mut content, &parts.relations, Some(parts))?;
    }

    let mut assertions = Assertions::new(&budget);
    model.cite(&mut assertions)?;
    render::inline(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
        attested: Vec::new(),
    })
}

// ----- Structurizr DSL -------------------------------------------------

/// A DSL string literal.
struct Quoted<'t>(&'t str);
impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_char('"')?;
        for c in self.0.chars() {
            match c {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\n' | '\r' => f.write_char(' ')?,
                c => f.write_char(c)?,
            }
        }
        f.write_char('"')
    }
}

/// `architecture.dsl`: the model as a Structurizr DSL workspace.
pub(super) fn dsl(model: &Model<'_>, max: Option<usize>) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let mut out = Text::new(
        &budget,
        "/*\n * C4 model recovered by Cartograph from the ingested sources (architecture.md).\n * Deterministic: no model call. Confidence is in each element's and relationship's properties.\n */\n",
    )?;
    writeln!(
        out,
        "workspace {} {} {{\n\n    model {{",
        Quoted(&model.system),
        Quoted("Recovered architecture")
    )?;
    let tags = |e: &Element<'_>| -> String {
        let mut tags = vec![];
        if e.kind == Kind::Store {
            tags.push("Database");
        }
        if e.kind == Kind::External {
            tags.push("External");
        }
        if e.gap {
            tags.push("Gap");
        }
        tags.join(",")
    };
    let props = |out: &mut Text, e: &Element<'_>, indent: &str| -> fmt::Result {
        writeln!(
            out,
            "{indent}    properties {{\n{indent}        \"confidence\" {}\n{indent}        \"facts\" \"{}\"\n{indent}    }}",
            Quoted(confidence_word(e.confidence())),
            e.facts.len()
        )
    };
    for i in model.indices(Kind::Person) {
        let e = &model.elements[i];
        writeln!(
            out,
            "        e{i} = person {} {} {{",
            Quoted(&e.name),
            Quoted(&e.description)
        )?;
        props(&mut out, e, "        ")?;
        out.push_str("        }\n")?;
    }
    for i in model.indices(Kind::External) {
        let e = &model.elements[i];
        writeln!(
            out,
            "        e{i} = softwareSystem {} {} {} {{",
            Quoted(&e.name),
            Quoted(&e.description),
            Quoted(&tags(e))
        )?;
        props(&mut out, e, "        ")?;
        out.push_str("        }\n")?;
    }
    writeln!(
        out,
        "        sys = softwareSystem {} {{",
        Quoted(&model.system)
    )?;
    for kind in [Kind::Container, Kind::Store] {
        for i in model.indices(kind) {
            let e = &model.elements[i];
            writeln!(
                out,
                "            e{i} = container {} {} {} {} {{",
                Quoted(&e.name),
                Quoted(&e.description),
                Quoted(&e.technology),
                Quoted(&tags(e))
            )?;
            props(&mut out, e, "            ")?;
            if let Some(parts) = model.components.iter().find(|c| c.container == i) {
                for (j, part) in parts.elements.iter().enumerate() {
                    writeln!(
                        out,
                        "                e{i}c{j} = component {} {} {{",
                        Quoted(&part.name),
                        Quoted(&part.description)
                    )?;
                    props(&mut out, part, "                ")?;
                    out.push_str("                }\n")?;
                }
            }
            out.push_str("            }\n")?;
        }
    }
    out.push_str("        }\n\n")?;
    // Each relationship keeps its confidence: a property, and a tag the
    // views style dashed, as the Mermaid diagrams draw it.
    let relation = |out: &mut Text, from: &str, to: &str, r: &Relation<'_>| -> fmt::Result {
        let confidence = r.confidence();
        let tag = match confidence {
            ConfidenceTier::Gap => "Relationship,Gap",
            ConfidenceTier::InferredWeak => "Relationship,InferredWeak",
            _ => "Relationship",
        };
        writeln!(
            out,
            "        {from} -> {to} {} {} {} {{\n            properties {{\n                \"confidence\" {}\n                \"facts\" \"{}\"\n            }}\n        }}",
            Quoted(&r.label()),
            Quoted(&r.technology),
            Quoted(tag),
            Quoted(confidence_word(confidence)),
            r.facts.len()
        )
    };
    for r in &model.relations {
        relation(&mut out, &format!("e{}", r.from), &format!("e{}", r.to), r)?;
    }
    for parts in &model.components {
        let id = |i: usize| {
            if i >= OFFSET {
                format!("e{}c{}", parts.container, i - OFFSET)
            } else {
                format!("e{i}")
            }
        };
        for r in &parts.relations {
            relation(&mut out, &id(r.from), &id(r.to), r)?;
        }
    }
    out.push_str("    }\n\n    views {\n        systemContext sys \"SystemContext\" {\n            include *\n            autolayout tb\n        }\n        container sys \"Containers\" {\n            include *\n            autolayout tb\n        }\n")?;
    for parts in &model.components {
        if parts.elements.is_empty() {
            continue;
        }
        writeln!(
            out,
            "        component e{0} \"Components-{0}\" {{\n            include *\n            autolayout tb\n        }}",
            parts.container
        )?;
    }
    out.push_str("        styles {\n            element \"Person\" {\n                shape Person\n                background #08427b\n                color #ffffff\n            }\n            element \"Software System\" {\n                background #1168bd\n                color #ffffff\n            }\n            element \"Container\" {\n                background #438dd5\n                color #ffffff\n            }\n            element \"Component\" {\n                background #85bbf0\n                color #000000\n            }\n            element \"Database\" {\n                shape Cylinder\n            }\n            element \"External\" {\n                background #8c8c8c\n                color #ffffff\n            }\n            element \"Gap\" {\n                background #ffffff\n                color #222222\n                stroke #c0392b\n                border dashed\n            }\n            relationship \"InferredWeak\" {\n                style dashed\n            }\n            relationship \"Gap\" {\n                style dashed\n                color #c0392b\n            }\n        }\n    }\n}\n")?;
    Ok(Output {
        content: out.finish(),
        assertions: Vec::new(),
        subjects: Vec::new(),
        attested: Vec::new(),
    })
}

/// The model as the app's Architecture view reads it (#2049 slice 2):
/// every element and relation with its confidence and the facts it rests
/// on, at most [`MAX_CITED`] of them each with its provenance, bound to the
/// `architecture.md` artifact it was compiled beside.
pub(crate) fn view(
    model: &Model<'_>,
    artifact: &crate::SpecArtifact,
    mode: ExportMode,
) -> crate::views::ArchitectureView {
    use crate::views::{ArchitectureComponents, ArchitectureView};
    let id = |i: usize| format!("e{i}");
    ArchitectureView {
        artifact_id: artifact.id.clone(),
        schema: artifact.schema.clone(),
        content_id: artifact.content_id.clone(),
        mode,
        system: model.system.clone(),
        elements: model
            .elements
            .iter()
            .enumerate()
            .map(|(i, e)| element_view(id(i), e))
            .collect(),
        relations: model
            .relations
            .iter()
            .map(|r| relation_view(id(r.from), id(r.to), r))
            .collect(),
        components: model
            .components
            .iter()
            .map(|parts| {
                let end = |i: usize| {
                    if i >= OFFSET {
                        format!("e{}c{}", parts.container, i - OFFSET)
                    } else {
                        id(i)
                    }
                };
                ArchitectureComponents {
                    container: id(parts.container),
                    elements: parts
                        .elements
                        .iter()
                        .enumerate()
                        .map(|(j, e)| element_view(format!("e{}c{j}", parts.container), e))
                        .collect(),
                    relations: parts
                        .relations
                        .iter()
                        .map(|r| relation_view(end(r.from), end(r.to), r))
                        .collect(),
                    unassigned_files: parts.unassigned,
                }
            })
            .collect(),
        areas_supplied: model.areas_supplied,
    }
}

fn kind_key(kind: Kind) -> &'static str {
    match kind {
        Kind::Person => "person",
        Kind::Container => "container",
        Kind::Store => "store",
        Kind::External => "external",
        Kind::Component => "component",
    }
}

fn fact_views(facts: &[Fact<'_>]) -> Vec<crate::views::ArchitectureFact> {
    facts
        .iter()
        .take(MAX_CITED)
        .map(|fact| match fact {
            Fact::Node(n) => {
                let name = render::NodeName(n).to_string();
                crate::views::ArchitectureFact {
                    id: n.id.clone(),
                    kind: n.label.clone(),
                    label: if name == n.id {
                        short(&n.id).to_string()
                    } else {
                        name
                    },
                    provenance: provenance(&n.props, &n.id),
                }
            }
            Fact::Edge(e) => {
                let identity = super::edge_identity(e);
                crate::views::ArchitectureFact {
                    label: format!("{} {} {}", short(&e.src), e.label, short(&e.dst)),
                    kind: e.label.clone(),
                    provenance: provenance(&e.props, &identity),
                    id: identity,
                }
            }
        })
        .collect()
}

fn element_view(id: String, e: &Element<'_>) -> crate::views::ArchitectureElement {
    crate::views::ArchitectureElement {
        id,
        kind: kind_key(e.kind).into(),
        name: e.name.clone(),
        technology: e.technology.clone(),
        description: e.description.clone(),
        confidence: e.confidence(),
        facts: fact_views(&e.facts),
        facts_total: e.facts.len(),
    }
}

fn relation_view(from: String, to: String, r: &Relation<'_>) -> crate::views::ArchitectureRelation {
    crate::views::ArchitectureRelation {
        from,
        to,
        label: r.label(),
        technology: r.technology.clone(),
        confidence: r.confidence(),
        facts: fact_views(&r.facts),
        facts_total: r.facts.len(),
    }
}

#[cfg(test)]
#[path = "architecture_tests.rs"]
mod tests;
