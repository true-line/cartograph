//! Environments and what they declare (#1791, AC-0661, AC-0662, catalogue D7
//! §7.2): `environments.md`.
//!
//! A deterministic (T0) projection of the facts `ingest::environments`
//! records (ADR-0070, SPEC-14): `Environment` and `Workload` facts, their
//! `DECLARES`, `DEPENDS_ON` and `PATCHES` relations, and the
//! `gap:environment:` Gaps. No model is called and no fact is recovered here.
//!
//! **A path is never a name.** An environment is shown by the path of the
//! file that declares it. It has a name only where that file states one;
//! otherwise the cell reads "none stated".
//!
//! **Nothing is merged.** A workload name that several environments of one
//! repository declare is listed once per declaration, with the recorded
//! properties that differ between them and each declaration's span. The
//! document does not say which declaration applies where: the files do not
//! say which of them a deployment combines. A Kubernetes workload that
//! several kustomizations reach is one fact; it is listed once for each of
//! them, with what that kustomization's patch files and `images` entries
//! state for it. No patch is applied and no transformer is evaluated.
//!
//! **Names only.** The facts hold the names of environment keys, never a
//! value, and nothing else is read here. A name-only environment file has no
//! span by design: a span would be a readable window into a file that holds
//! secrets.
//!
//! **Cost.** One index over the admitted facts and one pass over the
//! property values of the workloads: linear in both. Nothing is walked.
//!
//! **Mode.** The producer receives the facts the export mode admitted, like
//! every other graph artifact, so a `verified-only` export holds no
//! InferredWeak row. The caller counts what the mode and rejections withheld
//! ([`withheld`]) and the document states it (R-INT-5).

use super::producers::Output;
use super::render::{Assertions, Budget, Literal, RenderLimit, Text};
use super::{ExportMode, included, provenance};
use core_graph::{Edge, Node};
use core_prov::ConfidenceTier;
use serde::Deserialize;
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

const ENVIRONMENT: &str = "Environment";
const WORKLOAD: &str = "Workload";
const DECLARES: &str = "DECLARES";
const DEPENDS_ON: &str = "DEPENDS_ON";
const PATCHES: &str = "PATCHES";
const ENVIRONMENT_ID: &str = "env:";
const WORKLOAD_ID: &str = "workload:";
const GAP_ID: &str = "gap:environment:";
/// `props.kind` of the Gap of a name-only environment file.
const NAME_ONLY: &str = "name-only-environment";

/// The workload properties compared between declarations of one kind (all
/// Compose services, or all Kubernetes workloads), each with the words a row
/// uses for it. The kind has its own column.
const COMPARED: &[(&str, &str)] = &[
    ("namespace", "namespace"),
    ("replicas", "replicas"),
    ("containers", "containers"),
    ("image", "image"),
    ("build", "build context"),
    ("ports", "ports"),
    ("environment_keys", "environment key names"),
    ("env_files", "environment files"),
    ("depends_on", "dependencies"),
    ("profiles", "profiles"),
];

/// What both a Compose service and a Kubernetes workload can state, compared
/// when the declarations of one name are of both kinds.
const COMMON: &[(&str, &str)] = &[
    ("image", "images"),
    ("ports", "ports"),
    ("environment_keys", "environment key names"),
];

/// What the projection does not cover, stated in every export.
const NOT_COVERED: &[&str] = &[
    "Compose files are read under these names: `compose.yaml`, `compose.yml`, \
     `docker-compose.yaml` and `docker-compose.yml`, each also with one dotted infix \
     (`compose.prod.yaml`). A Compose file with any other name is not read.",
    "Of Kubernetes manifests, only `Deployment`, `StatefulSet`, `DaemonSet` and `Job` documents \
     are read. Every other kind is not read; the coverage document lists each kind found as \
     present and not read. A `CronJob` is read as a scheduled job and is not a workload here.",
    "Kustomization files are read for `resources`, `bases`, `namespace`, `images` and patch \
     files. Nothing is applied: a patch and an `images` entry are shown as written beside the \
     workload they name. Inline patches, `target` selectors, `patchesJson6902`, transformers and \
     generators are not evaluated; each is listed under Not established.",
    "A kustomization is followed only inside the ingested repository and at most 16 \
     kustomizations deep. A remote resource is not fetched.",
    "Helm charts are read from `Chart.yaml` and the key paths of their values files; templates \
     are not rendered, so the objects a chart creates are not listed, and a manifest that is not \
     valid YAML (a Helm template) is not a manifest here. A helmfile release is an environment; \
     a templated helmfile, `bases`, `helmfiles`, inline values, `set` and `secrets` are not read. \
     No value of a values file is read or stored. Argo CD applications and kustomize \
     `helmCharts` are not read.",
    "Spring Boot `application` files are read for the profiles they state and the key paths \
     they set. A profile-specific file (`application-prod.yml`) is an environment only where a \
     base file of its directory declares the profile; otherwise it is named only by its file \
     name. Profile expressions, `spring.config.import` and profiles set outside these files \
     (`SPRING_PROFILES_ACTIVE`, `@Profile`) are not read. Other settings modules and \
     configuration files (`settings/production.py`, `config/staging.yml`, \
     `config/environments/production.rb`) are not read; one named like an environment is \
     listed under Not established.",
    "Terraform resources are not attached to an environment.",
    "Nothing is merged. Which Compose files a deployment combines is not written in them, and no \
     kustomize patch or transformer is applied, so the effective configuration of a deployment \
     is not shown.",
    "Which environment is production, staging or any other stage is not stated: no file states \
     it.",
    "Live state is not read. A declared workload may not be running, and a running one may not \
     be declared.",
    "No value of an environment key is read or stored, and no secret. For a Kubernetes container \
     the names of the ConfigMaps and Secrets its `envFrom` references are listed; `valueFrom` \
     and the `data` of any document are not read.",
    "`extends`, `include`, YAML anchors, aliases and tags, and `${...}` variables are not \
     evaluated. Each is listed under Not established or marked on its row.",
    "A variable written as `$NAME`, without braces, is stored as written and is not marked.",
];

/// The confidence a fact records, without validating its whole provenance.
/// A fact with none is unresolved, as [`provenance`] makes it.
fn confidence(props: &serde_json::Value) -> ConfidenceTier {
    ConfidenceTier::deserialize(&crate::stored_prov(props)["confidence_tier"])
        .unwrap_or(ConfidenceTier::Gap)
}

fn weak(props: &serde_json::Value) -> bool {
    confidence(props) == ConfidenceTier::InferredWeak
}

fn edge_identity(edge: &Edge) -> String {
    format!("{} {} {}", edge.src, edge.label, edge.dst)
}

fn is_fact(node: &Node) -> bool {
    (node.label == ENVIRONMENT && node.id.starts_with(ENVIRONMENT_ID))
        || (node.label == WORKLOAD && node.id.starts_with(WORKLOAD_ID))
        || (node.label == "Gap" && node.id.starts_with(GAP_ID))
}

fn is_relation(edge: &Edge) -> bool {
    (matches!(edge.label.as_str(), DECLARES | PATCHES)
        && edge.src.starts_with(ENVIRONMENT_ID)
        && edge.dst.starts_with(WORKLOAD_ID))
        || (edge.label == DEPENDS_ON
            && edge.src.starts_with(WORKLOAD_ID)
            && edge.dst.starts_with(WORKLOAD_ID))
}

/// How many environment facts the export mode and the rejections withhold,
/// counted over the facts before that filter: the `Environment`, `Workload`
/// and Gap facts and their `DECLARES`, `DEPENDS_ON` and `PATCHES` relations.
pub(super) fn withheld<N: Borrow<Node>, E: Borrow<Edge>>(
    nodes: &[N],
    edges: &[E],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> usize {
    let nodes = nodes
        .iter()
        .map(Borrow::borrow)
        .filter(|node| is_fact(node))
        .filter(|node| !included(&provenance(&node.props, &node.id), mode, rejected))
        .count();
    let edges = edges
        .iter()
        .map(Borrow::borrow)
        .filter(|edge| is_relation(edge))
        .filter(|edge| {
            !included(
                &provenance(&edge.props, &edge_identity(edge)),
                mode,
                rejected,
            )
        })
        .count();
    nodes + edges
}

/// The repository of an id shaped `{scheme}{repo}@{path}{suffix}`, where the
/// fact's own `path` property says where the path starts. A fact whose id
/// does not end that way is not placed.
fn repo_of<'a>(id: &'a str, scheme: &str, path: &str, suffix: &str) -> Option<&'a str> {
    id.strip_prefix(scheme)?
        .strip_suffix(suffix)?
        .strip_suffix(path)?
        .strip_suffix('@')
}

/// True for the `Workload` of a Kubernetes manifest document. It is its own
/// fact, reached by any number of environments; a Compose service belongs
/// to the one file that declares it.
fn is_manifest(node: &Node) -> bool {
    node.props["kind"]
        .as_str()
        .is_some_and(|kind| kind.starts_with("k8s-"))
}

/// True for the `Workload` of a Helm chart: reached by any number of
/// helmfile releases, like a manifest workload.
fn is_chart(node: &Node) -> bool {
    node.props["kind"] == "helm-chart"
}

/// The repository of a manifest workload's id,
/// `workload:{repo}@{path}#{document index}:{Kind}/{name}`.
fn manifest_repo<'a>(id: &'a str, path: &str, name: &str) -> Option<&'a str> {
    let (head, document) = id
        .strip_prefix(WORKLOAD_ID)?
        .strip_suffix(name)?
        .strip_suffix('/')?
        .rsplit_once('#')?;
    let (index, kind) = document.split_once(':')?;
    (index.bytes().all(|b| b.is_ascii_digit()) && !kind.is_empty())
        .then(|| head.strip_suffix(path)?.strip_suffix('@'))
        .flatten()
}

/// One `Workload` fact of a Compose file.
struct Workload<'a> {
    node: &'a Node,
    name: &'a str,
    /// Its `DECLARES` relation, when the export admits it.
    declared: Option<&'a Edge>,
    /// Its `DEPENDS_ON` relations, by the workload they name.
    depends: Vec<&'a Edge>,
}

/// One manifest workload an environment reaches.
struct Reached<'a> {
    /// The workload's id, a key of [`Model::manifests`].
    workload: &'a str,
    declares: &'a Edge,
    /// What the environment's patch files state for it.
    patches: Option<&'a Edge>,
}

/// One `Environment` fact.
struct Environment<'a> {
    node: &'a Node,
    /// The services of a Compose file.
    workloads: Vec<Workload<'a>>,
    /// The manifest workloads a kustomization reaches, by workload id.
    reached: Vec<Reached<'a>>,
    /// The Helm charts a helmfile release declares.
    charts: Vec<Reached<'a>>,
    /// The paths of the environments that include this one.
    included_by: Vec<&'a str>,
    /// `namePrefix` and `nameSuffix`, where the kustomization sets them.
    renames: Vec<&'static str>,
}

/// One `Workload` fact of a Kubernetes manifest document.
struct Manifest<'a> {
    node: &'a Node,
    repo: &'a str,
    path: &'a str,
    name: &'a str,
    /// The paths of the environments that reach it.
    reached_by: Vec<&'a str>,
}

/// One Gap fact.
struct Gap<'a> {
    node: &'a Node,
    repo: &'a str,
    path: &'a str,
    service: Option<&'a str>,
}

/// The admitted facts as the document lists them.
struct Model<'a> {
    /// By repository, then path.
    environments: BTreeMap<(&'a str, &'a str), Environment<'a>>,
    /// Manifest workloads by id.
    manifests: BTreeMap<&'a str, Manifest<'a>>,
    /// Helm chart workloads by id.
    charts: BTreeMap<&'a str, Manifest<'a>>,
    /// By repository, then path, then service.
    gaps: Vec<Gap<'a>>,
    /// Facts whose id does not say where they are.
    unplaced: usize,
    /// Workloads whose environment is not among the facts this export lists.
    loose: usize,
}

impl<'a> Model<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        // Never the store's or the caller's order, and each fact once.
        let by_id: BTreeMap<&'a str, &'a Node> = nodes
            .iter()
            .copied()
            .filter(|node| is_fact(node))
            .map(|node| (node.id.as_str(), node))
            .collect();
        let manifest = |id: &str| {
            by_id
                .get(id)
                .is_some_and(|node| node.label == WORKLOAD && is_manifest(node))
        };
        let chart = |id: &str| {
            by_id
                .get(id)
                .is_some_and(|node| node.label == WORKLOAD && is_chart(node))
        };
        let mut installs: BTreeMap<(&'a str, &'a str), &'a Edge> = BTreeMap::new();
        let mut declares: BTreeMap<&'a str, &'a Edge> = BTreeMap::new();
        let mut depends: BTreeMap<&'a str, BTreeMap<&'a str, &'a Edge>> = BTreeMap::new();
        // By environment id, then workload id.
        let mut reaches: BTreeMap<(&'a str, &'a str), &'a Edge> = BTreeMap::new();
        let mut patches: BTreeMap<(&'a str, &'a str), &'a Edge> = BTreeMap::new();
        for edge in edges.iter().copied().filter(|edge| is_relation(edge)) {
            let ends = (edge.src.as_str(), edge.dst.as_str());
            if edge.label == PATCHES {
                patches.insert(ends, edge);
            } else if edge.label == DECLARES && chart(&edge.dst) {
                installs.insert(ends, edge);
            } else if edge.label == DECLARES && manifest(&edge.dst) {
                reaches.insert(ends, edge);
            } else if edge.label == DECLARES {
                declares.insert(edge.dst.as_str(), edge);
            } else if by_id.contains_key(edge.dst.as_str()) {
                depends
                    .entry(edge.src.as_str())
                    .or_default()
                    .insert(edge.dst.as_str(), edge);
            }
        }
        let mut model = Self {
            environments: BTreeMap::new(),
            manifests: BTreeMap::new(),
            charts: BTreeMap::new(),
            gaps: Vec::new(),
            unplaced: 0,
            loose: 0,
        };
        let path_of = |node: &'a Node| node.props["path"].as_str();
        for node in by_id.values().copied().filter(|n| n.label == ENVIRONMENT) {
            match path_of(node)
                .and_then(|path| Some((repo_of(&node.id, ENVIRONMENT_ID, path, "")?, path)))
            {
                Some(key) => {
                    model.environments.insert(
                        key,
                        Environment {
                            node,
                            workloads: Vec::new(),
                            reached: Vec::new(),
                            charts: Vec::new(),
                            included_by: Vec::new(),
                            renames: Vec::new(),
                        },
                    );
                }
                None => model.unplaced += 1,
            }
        }
        // In id order, so the workloads of an environment are in name order.
        for node in by_id.values().copied().filter(|n| n.label == WORKLOAD) {
            let placed = path_of(node).and_then(|path| {
                let name = node.props["name"].as_str()?;
                let repo = if is_chart(node) {
                    repo_of(&node.id, WORKLOAD_ID, path, "")?
                } else if is_manifest(node) {
                    manifest_repo(&node.id, path, name)?
                } else {
                    repo_of(&node.id, WORKLOAD_ID, path, &format!("#{name}"))?
                };
                Some((repo, path, name))
            });
            let Some((repo, path, name)) = placed else {
                model.unplaced += 1;
                continue;
            };
            if is_chart(node) {
                model.charts.insert(
                    node.id.as_str(),
                    Manifest {
                        node,
                        repo,
                        path,
                        name,
                        reached_by: Vec::new(),
                    },
                );
                continue;
            }
            if is_manifest(node) {
                model.manifests.insert(
                    node.id.as_str(),
                    Manifest {
                        node,
                        repo,
                        path,
                        name,
                        reached_by: Vec::new(),
                    },
                );
                continue;
            }
            let Some(environment) = model.environments.get_mut(&(repo, path)) else {
                model.loose += 1;
                continue;
            };
            environment.workloads.push(Workload {
                node,
                name,
                declared: declares.get(node.id.as_str()).copied(),
                depends: depends
                    .remove(node.id.as_str())
                    .map(|by_target| by_target.into_values().collect())
                    .unwrap_or_default(),
            });
        }
        // What each kustomization reaches and includes, in path order.
        let mut included: Vec<((&'a str, &'a str), &'a str)> = Vec::new();
        for ((repo, path), environment) in &mut model.environments {
            let id = environment.node.id.as_str();
            for ((_, workload), declares) in
                reaches.range((id, "")..).take_while(|(k, _)| k.0 == id)
            {
                let Some(manifest) = model
                    .manifests
                    .get_mut(workload)
                    .filter(|manifest| manifest.repo == *repo)
                else {
                    continue;
                };
                manifest.reached_by.push(path);
                environment.reached.push(Reached {
                    workload,
                    declares,
                    patches: patches.get(&(id, *workload)).copied(),
                });
            }
            for ((_, workload), declares) in
                installs.range((id, "")..).take_while(|(k, _)| k.0 == id)
            {
                let Some(chart) = model
                    .charts
                    .get_mut(workload)
                    .filter(|chart| chart.repo == *repo)
                else {
                    continue;
                };
                chart.reached_by.push(path);
                environment.charts.push(Reached {
                    workload,
                    declares,
                    patches: None,
                });
            }
            for include in environment.node.props["includes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|include| include.as_str())
            {
                included.push(((repo, include), path));
            }
        }
        for (key, by) in included {
            if let Some(environment) = model.environments.get_mut(&key)
                && !environment.included_by.contains(&by)
            {
                environment.included_by.push(by);
            }
        }
        for node in by_id.values().copied().filter(|n| n.label == "Gap") {
            let service = node.props["service"].as_str();
            let suffix = match (service, node.props["document"].as_u64()) {
                (Some(service), _) => format!("#{service}"),
                (None, Some(document)) => format!("#{document}"),
                (None, None) => String::new(),
            };
            match path_of(node)
                .and_then(|path| Some((repo_of(&node.id, GAP_ID, path, &suffix)?, path)))
            {
                Some((repo, path)) => {
                    // A kustomization that renames what it deploys.
                    if let Some(environment) = model.environments.get_mut(&(repo, path)) {
                        for key in ["namePrefix", "nameSuffix"] {
                            if node.props["unevaluated"]
                                .as_array()
                                .is_some_and(|keys| keys.iter().any(|stated| stated == key))
                            {
                                environment.renames.push(key);
                            }
                        }
                    }
                    model.gaps.push(Gap {
                        node,
                        repo,
                        path,
                        service,
                    });
                }
                None => model.unplaced += 1,
            }
        }
        model
            .gaps
            .sort_by_key(|gap| (gap.repo, gap.path, gap.service, gap.node.id.as_str()));
        model
    }

    fn compose_workloads(&self) -> usize {
        self.environments
            .values()
            .map(|environment| environment.workloads.len())
            .sum()
    }

    fn workloads(&self) -> usize {
        self.compose_workloads() + self.manifests.len() + self.charts.len()
    }

    fn name_only(&self) -> usize {
        self.gaps
            .iter()
            .filter(|gap| gap.node.props["kind"] == NAME_ONLY)
            .count()
    }
}

/// Tier, confidence and span cells of one fact. The first evidence span,
/// and how many more the fact index lists.
struct Evidence<'a>(&'a Node);
impl fmt::Display for Evidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let absent = if self.0.props["kind"] == NAME_ONLY {
            "none stored for an environment file"
        } else {
            "—"
        };
        evidence(f, &provenance(&self.0.props, &self.0.id), absent)
    }
}

/// [`Evidence`] of a relation.
struct Related<'a>(&'a Edge);
impl fmt::Display for Related<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        evidence(f, &provenance(&self.0.props, &edge_identity(self.0)), "—")
    }
}

fn evidence(
    f: &mut fmt::Formatter<'_>,
    provenance: &core_prov::Provenance,
    absent: &str,
) -> fmt::Result {
    write!(
        f,
        "{:?} | {:?} | ",
        provenance.tier, provenance.confidence_tier
    )?;
    let Some(first) = provenance.evidence.first() else {
        return f.write_str(absent);
    };
    write!(
        f,
        "{}:{} bytes {}..{} @ {}",
        Literal(&first.repo),
        Literal(&first.path),
        first.byte_start,
        first.byte_end,
        Literal(&first.commit_sha)
    )?;
    match provenance.evidence.len() - 1 {
        0 => Ok(()),
        more => write!(f, " (+{more} more in the fact index)"),
    }
}

/// True when the fact marks `key` as holding an unresolved `${`.
fn interpolated(node: &Node, key: &str) -> bool {
    node.props["interpolated"][key] == true
}

/// One property of a fact as a table cell: a string, or a list joined by
/// commas; `absent` when the fact does not state it.
struct Cell<'a> {
    node: &'a Node,
    key: &'a str,
    absent: &'static str,
}
impl fmt::Display for Cell<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.node.props[self.key] {
            serde_json::Value::String(value) => write!(f, "{}", Literal(value))?,
            serde_json::Value::Number(value) => write!(f, "{value}")?,
            // The containers of a manifest workload, each with what it states.
            serde_json::Value::Array(values) if values.iter().any(|value| value.is_object()) => {
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        f.write_str(" / ")?;
                    }
                    write!(f, "{}", Stated(value))?;
                }
            }
            serde_json::Value::Array(values) if !values.is_empty() => {
                write!(f, "{}", Names(values))?;
            }
            _ => return f.write_str(self.absent),
        }
        if interpolated(self.node, self.key) {
            f.write_str(" (holds `${`; not resolved)")?;
        }
        Ok(())
    }
}

/// A list of names, joined by commas.
struct Names<'a>(&'a [serde_json::Value]);
impl fmt::Display for Names<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, value) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            match value.as_str() {
                Some(value) => write!(f, "{}", Literal(value))?,
                None => write!(f, "{}", Literal(value))?,
            }
        }
        Ok(())
    }
}

/// The container properties a row names, each with the words it uses.
const CONTAINER: &[(&str, &str)] = &[
    ("image", "image"),
    ("ports", "ports"),
    ("environment_keys", "environment key names"),
    ("env_from", "configuration references"),
];

/// One property of a container as a table cell.
struct Part<'a>(&'a serde_json::Value, &'a str);
impl fmt::Display for Part<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0[self.1] {
            serde_json::Value::String(value) => write!(f, "{}", Literal(value))?,
            serde_json::Value::Array(values) if !values.is_empty() => {
                write!(f, "{}", Names(values))?;
            }
            _ => return f.write_str("—"),
        }
        if self.0["interpolated"][self.1] == true {
            f.write_str(" (holds `${`; not resolved)")?;
        }
        Ok(())
    }
}

/// The name of a container, marked when it is an init container.
struct ContainerName<'a>(&'a serde_json::Value);
impl fmt::Display for ContainerName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0["name"].as_str() {
            Some(name) => write!(f, "{}", Literal(name))?,
            None => f.write_str("not named")?,
        }
        if self.0["init"] == true {
            f.write_str(" (init container)")?;
        }
        Ok(())
    }
}

/// What one container entry states, in one cell: its name, then each
/// property it writes.
struct Stated<'a>(&'a serde_json::Value);
impl fmt::Display for Stated<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "container {}", ContainerName(self.0))?;
        let mut any = false;
        for (key, words) in CONTAINER {
            if self.0[*key].is_null() {
                continue;
            }
            f.write_str(if any { ", " } else { ": " })?;
            any = true;
            write!(f, "{words} {}", Part(self.0, key))?;
        }
        Ok(())
    }
}

/// What a `PATCHES` relation states: for each patch document, its file and
/// the properties it writes. Nothing is applied.
struct Patch<'a>(Option<&'a Edge>);
impl fmt::Display for Patch<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(edge) = self.0 else {
            return f.write_str("no patch stated");
        };
        let more = edge.props["more"].as_array();
        let statements = std::iter::once(&edge.props).chain(more.into_iter().flatten());
        for (index, statement) in statements.enumerate() {
            if index > 0 {
                f.write_str(" / ")?;
            }
            match statement["path"].as_str() {
                Some(path) => write!(f, "patch {} states ", Literal(path))?,
                None => f.write_str("a patch states ")?,
            }
            let mut any = false;
            if let Some(replicas) = statement["replicas"].as_u64() {
                write!(f, "replicas {replicas}")?;
                any = true;
            }
            for container in statement["containers"].as_array().into_iter().flatten() {
                if any {
                    f.write_str("; ")?;
                }
                any = true;
                write!(f, "{}", Stated(container))?;
            }
            if !any {
                f.write_str("none of the recorded properties")?;
            }
        }
        Ok(())
    }
}

/// The values files a helmfile release names, each with its key paths.
struct ReleaseValues<'a>(Option<&'a Edge>);
impl fmt::Display for ReleaseValues<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(edge) = self.0 else {
            return f.write_str("—");
        };
        let values = edge.props["values"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        if values.is_empty() {
            return f.write_str("none named");
        }
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                f.write_str(" / ")?;
            }
            match value["path"].as_str() {
                Some(path) => write!(f, "{}: ", Literal(path))?,
                None => f.write_str("a values file: ")?,
            }
            match value["keys"].as_array() {
                Some(keys) if !keys.is_empty() => write!(f, "{}", Names(keys))?,
                _ => f.write_str("no keys")?,
            }
            if let Some(omitted) = value["keys_omitted"].as_u64() {
                write!(f, " and {omitted} more")?;
            }
        }
        Ok(())
    }
}

/// An image reference without its tag and digest: what a kustomize `images`
/// entry names.
fn image_name(image: &str) -> &str {
    let image = image.split('@').next().unwrap_or(image);
    match image.rfind(':') {
        Some(colon) if !image[colon..].contains('/') => &image[..colon],
        _ => image,
    }
}

/// The `images` entries of an environment that name an image of `workload`,
/// each as its file writes it.
struct Images<'a> {
    environment: &'a Node,
    workload: &'a Node,
}
impl fmt::Display for Images<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let images: BTreeSet<&str> = self.workload.props["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|container| container["image"].as_str())
            .map(image_name)
            .collect();
        let mut any = false;
        for entry in self.environment.props["image_overrides"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| {
                entry["name"]
                    .as_str()
                    .is_some_and(|name| images.contains(name))
            })
        {
            if any {
                f.write_str(" / ")?;
            }
            any = true;
            let mut first = true;
            for key in ["name", "newName", "newTag", "digest"] {
                if let Some(value) = entry[key].as_str() {
                    if !first {
                        f.write_str(", ")?;
                    }
                    first = false;
                    write!(f, "{key} {}", Literal(value))?;
                }
            }
        }
        if !any {
            f.write_str("none names its images")?;
        }
        Ok(())
    }
}

/// The chain of kustomizations a `DECLARES` relation states.
struct Via<'a>(&'a Edge);
impl fmt::Display for Via<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.props["via"].as_array() {
            Some(via) if !via.is_empty() => write!(f, "{}", Names(via)),
            _ => f.write_str("its own entry"),
        }
    }
}

/// The Note cell of a row: what rests on an InferredWeak fact, which
/// environments include this one, and a name the kustomization changes.
struct Notes<'p, 'a> {
    weak: bool,
    included_by: &'p [&'a str],
    renames: &'p [&'static str],
}
impl fmt::Display for Notes<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut any = false;
        let mut part = |f: &mut fmt::Formatter<'_>| {
            let separator = if any { "; " } else { "" };
            any = true;
            f.write_str(separator)
        };
        if self.weak {
            part(f)?;
            f.write_str(note(true))?;
        }
        if !self.included_by.is_empty() {
            part(f)?;
            f.write_str("included by ")?;
            for (index, path) in self.included_by.iter().enumerate() {
                if index > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{}", Literal(path))?;
            }
        }
        if !self.renames.is_empty() {
            part(f)?;
            write!(
                f,
                "this environment sets `{}`: the deployed name differs from the recorded one",
                self.renames.join("` and `")
            )?;
        }
        if !any {
            f.write_str("—")?;
        }
        Ok(())
    }
}

/// The cells of a manifest workload's rows after the cells that say where
/// it is: one row for each container, or one row when it states none.
fn manifest_rows(
    content: &mut Text,
    lead: &dyn fmt::Display,
    manifest: &Manifest,
    notes: &dyn fmt::Display,
) -> fmt::Result {
    let node = manifest.node;
    let none = [serde_json::Value::Null];
    let containers = match node.props["containers"].as_array() {
        Some(containers) if !containers.is_empty() => containers.as_slice(),
        _ => &none,
    };
    for container in containers {
        write!(
            content,
            "{lead}| {} | {} | ",
            cell(node, "namespace"),
            cell(node, "replicas")
        )?;
        if container.is_null() {
            content.push_str("none recorded | — | — | — | —")?;
        } else {
            write!(
                content,
                "{} | {} | {} | {} | {}",
                ContainerName(container),
                Part(container, "image"),
                Part(container, "ports"),
                Part(container, "environment_keys"),
                Part(container, "env_from")
            )?;
        }
        writeln!(content, " | {} | {notes} |", Evidence(node))?;
    }
    Ok(())
}

/// The kind of a workload: of a manifest workload as its file writes it.
fn manifest_kind(node: &Node) -> &'static str {
    match node.props["kind"].as_str() {
        Some("compose-service") => "Compose service",
        Some("k8s-deployment") => "Deployment",
        Some("k8s-statefulset") => "StatefulSet",
        Some("k8s-daemonset") => "DaemonSet",
        Some("k8s-job") => "Job",
        _ => "not recorded",
    }
}

fn cell<'a>(node: &'a Node, key: &'a str) -> Cell<'a> {
    Cell {
        node,
        key,
        absent: "—",
    }
}

/// The dependencies a workload writes, each marked when the export holds no
/// relation for it.
struct Dependencies<'p, 'a>(&'p Workload<'a>);
impl fmt::Display for Dependencies<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.0.node.props["depends_on"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|name| name.as_str())
            .collect();
        if names.is_empty() {
            return f.write_str("—");
        }
        let prefix = self
            .0
            .node
            .id
            .strip_suffix(self.0.name)
            .unwrap_or(&self.0.node.id);
        for (index, name) in names.iter().enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}", Literal(name))?;
            let related = self
                .0
                .depends
                .iter()
                .any(|edge| edge.dst.strip_prefix(prefix) == Some(name));
            if !related {
                f.write_str(" (no relation: see Not established)")?;
            }
        }
        Ok(())
    }
}

/// One of the [`COMMON`] properties as a workload of either kind states it:
/// its values, and whether any holds an unresolved `${`. A Kubernetes
/// workload states it once for each container; the values are listed in
/// container order, and its environment key names sorted, each once.
fn common(node: &Node, key: &str) -> (Vec<String>, bool) {
    let mut values = Vec::new();
    let mut take = |value: &serde_json::Value| match value {
        serde_json::Value::String(value) => values.push(value.clone()),
        serde_json::Value::Array(entries) => values.extend(entries.iter().map(|entry| {
            entry
                .as_str()
                .map_or_else(|| entry.to_string(), str::to_owned)
        })),
        _ => {}
    };
    let marked = if is_manifest(node) {
        let mut marked = false;
        for container in node.props["containers"].as_array().into_iter().flatten() {
            take(&container[key]);
            marked |= container["interpolated"][key] == true;
        }
        marked
    } else {
        take(&node.props[key]);
        interpolated(node, key)
    };
    if is_manifest(node) && key == "environment_keys" {
        values.sort();
        values.dedup();
    }
    (values, marked)
}

/// The properties of `workload` that differ among `declarations`, the
/// distinct `Workload` facts of one name, with this declaration's value of
/// each. `mixed` when they are not all Compose services or all Kubernetes
/// workloads: then only what both kinds can state is compared.
struct Differing<'p, 'a> {
    workload: &'a Node,
    declarations: &'p [&'a Node],
    mixed: bool,
}
impl fmt::Display for Differing<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut any = false;
        if self.mixed {
            for (key, words) in COMMON {
                let (values, marked) = common(self.workload, key);
                let differs = self.declarations.iter().any(|other| {
                    let (theirs, their_mark) = common(other, key);
                    theirs != values || their_mark != marked
                });
                if !differs {
                    continue;
                }
                f.write_str(if any { "; " } else { "" })?;
                any = true;
                write!(f, "{words}: ")?;
                if values.is_empty() {
                    f.write_str("not stated")?;
                }
                for (index, value) in values.iter().enumerate() {
                    f.write_str(if index > 0 { ", " } else { "" })?;
                    write!(f, "{}", Literal(value))?;
                }
                if marked {
                    f.write_str(" (holds `${`; not resolved)")?;
                }
            }
            if !any {
                f.write_str(
                    "none: the images, ports and environment key names are the same in each \
                     declaration",
                )?;
            }
            return Ok(());
        }
        for (key, words) in COMPARED {
            let own = &self.workload.props[*key];
            let own_mark = interpolated(self.workload, key);
            let differs = self
                .declarations
                .iter()
                .any(|other| other.props[*key] != *own || interpolated(other, key) != own_mark);
            if !differs {
                continue;
            }
            if any {
                f.write_str("; ")?;
            }
            any = true;
            write!(
                f,
                "{words}: {}",
                Cell {
                    node: self.workload,
                    key,
                    absent: "not stated",
                }
            )?;
        }
        if !any {
            f.write_str("none: the recorded properties are the same in each declaration")?;
        }
        Ok(())
    }
}

/// One `Workload` fact among the declarations of a name.
struct Declaration<'a> {
    node: &'a Node,
    /// The paths of the environments with a `DECLARES` relation to it.
    reached_by: Vec<&'a str>,
}

/// The declarations of one workload name, by file and then fact id.
type Declarations<'a> = BTreeMap<(&'a str, &'a str), Declaration<'a>>;

/// The environments with a `DECLARES` relation to a workload, by path.
struct ReachedBy<'p, 'a>(&'p [&'a str]);
impl fmt::Display for ReachedBy<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("no environment");
        }
        for (index, path) in self.0.iter().enumerate() {
            f.write_str(if index > 0 { ", " } else { "" })?;
            write!(f, "{}", Literal(path))?;
        }
        Ok(())
    }
}

fn note(weak: bool) -> &'static str {
    if weak {
        "rests on an InferredWeak fact; a verified-only export omits this row"
    } else {
        "—"
    }
}

fn gap_kind(node: &Node) -> &'static str {
    match node.props["kind"].as_str() {
        Some(NAME_ONLY) => "environment named only by a file name",
        Some("compose-file") => "Compose file",
        Some("compose-service") => "Compose service",
        Some("kustomization") => "kustomization",
        Some("manifest-document") => "Kubernetes manifest document",
        Some("helm-chart") => "Helm chart",
        Some("helmfile") => "helmfile",
        Some("helmfile-release") => "helmfile release",
        Some("spring-config") => "Spring configuration file",
        _ => "not recorded",
    }
}

const INTRO: &str = "# Environments\n\n\
A deterministic projection of the environments the ingested sources declare and the workloads \
each declares. No model is involved.\n\n\
## How to read this document\n\n\
- **An environment** here is a declared deployable set: one file that declares a set of things \
to run together. That is a Compose file with a `services` mapping, a kustomization file \
(`kustomization.yaml`, `kustomization.yml` or `Kustomization`), a release of a helmfile, or a \
Spring Boot profile that a configuration file states.\n\
- **An environment is shown by the path of its file.** It has a name only where the file \
states one (a top-level `name` in a Compose file, a `namespace` in a kustomization or a \
helmfile release, the profile a Spring Boot file states). Where the \
file states none, the name cell reads \"none stated\". A file or directory name is never used \
as an environment name.\n\
- **A workload** is one thing an environment declares to run. For Compose that is one service, \
with its image, build context, ports, the names of its environment keys, its environment files, \
its dependencies and its profiles, as the file writes them. For Kubernetes it is one \
`Deployment`, `StatefulSet`, `DaemonSet` or `Job` document, with its containers.\n\
- **A kustomization declares** the Kubernetes workloads in the manifest files its `resources` \
and `bases` entries name, and in the kustomization directories they name, followed inside the \
repository. A kustomization that another one names is an environment too; its row says \
\"included by\" with the paths that include it.\n\
- **Declared in more than one environment** lists a workload name that two or more workload \
facts of one repository share, one row for each declaration (each workload fact) with the \
environments that reach it; and a Kubernetes workload that two or more kustomizations reach, \
one row for each of them. The document does not say which declaration is used where: no file \
says which files a deployment combines.\n\
- **A patch is shown, not applied.** Where a kustomization names a patch file whose document \
names a workload it reaches, the row states what that patch writes. The workload's own row \
keeps what its own file writes. `images` entries are shown the same way, as written.\n\
- **Workloads no environment declares** lists a Kubernetes workload that no kustomization in \
this export reaches.\n\
- **Not established** lists what was found and could not be stated: a file that could not be \
read, a construct that is not evaluated, a dependency on a service the file does not declare, \
an entry of a kustomization that is remote, missing or outside the repository, a patch that \
matches nothing, a transformer or generator that is not evaluated, and an environment file \
whose name is the only thing that names an environment.\n\
- Every row carries the tier, confidence and source span of its fact. The fact index lists \
every span. An environment file has no span: these files hold secrets, so no byte range of one \
is stored, except the line of a value its author allowlists (ADR-0075).\n\
- Only the names of environment keys are listed. No value is shown.\n\
- The last section states what is not read.\n\n";

/// `environments.md` over the facts the export mode admitted and the count
/// of environment facts it withheld.
pub(super) fn output(
    nodes: &[&Node],
    edges: &[&Edge],
    (mode, withheld): (ExportMode, usize),
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let model = Model::new(nodes, edges);

    // Each fact once, in the order the document lists it.
    let mut assertions = Assertions::new(&budget);
    for environment in model.environments.values() {
        assertions.node(environment.node, None)?;
        for workload in &environment.workloads {
            assertions.node(workload.node, None)?;
            if let Some(edge) = workload.declared {
                assertions.edge(edge)?;
            }
            for edge in &workload.depends {
                assertions.edge(edge)?;
            }
        }
        for reached in &environment.reached {
            assertions.edge(reached.declares)?;
            if let Some(edge) = reached.patches {
                assertions.edge(edge)?;
            }
        }
        for installed in &environment.charts {
            assertions.edge(installed.declares)?;
        }
    }
    for manifest in model.manifests.values() {
        assertions.node(manifest.node, None)?;
    }
    for chart in model.charts.values() {
        assertions.node(chart.node, None)?;
    }
    for gap in &model.gaps {
        assertions.node(gap.node, None)?;
    }

    let name_only = model.name_only();
    let undeclared = model
        .manifests
        .values()
        .filter(|manifest| manifest.reached_by.is_empty())
        .count();
    let mut content = Text::new(&budget, INTRO)?;
    writeln!(
        content,
        "Export mode: `{}`. Environments: {}. Workloads: {}. Environments named only by a file \
         name: {}. Other items not established: {}. Workloads no environment declares: \
         {undeclared}.\n",
        match mode {
            ExportMode::VerifiedOnly => "verified-only",
            ExportMode::BestEffort => "best-effort",
        },
        model.environments.len(),
        model.workloads(),
        name_only,
        model.gaps.len() - name_only,
    )?;
    match mode {
        ExportMode::VerifiedOnly => writeln!(
            content,
            "Environment facts left out of this export because they are InferredWeak or \
             rejected: {withheld}. A `best-effort` export includes the InferredWeak ones, \
             marked.\n"
        )?,
        ExportMode::BestEffort => writeln!(
            content,
            "InferredWeak rows are included and marked in the Note column. Environment facts \
             left out of this export because they are rejected: {withheld}.\n"
        )?,
    }
    for (count, what) in [
        (
            model.unplaced,
            "admitted environment facts are not listed: the fact's id does not name its \
             repository and file",
        ),
        (
            model.loose,
            "admitted workloads are not listed: the environment each belongs to is not among \
             the facts this export lists",
        ),
    ] {
        if count > 0 {
            writeln!(content, "{count} {what}.\n")?;
        }
    }

    content.push_str(
        "## Environments\n\n\
         | Repository | Path | Stated name | Declared by | Workloads | Tier | Confidence | Span \
         | Note |\n\
         |---|---|---|---|---|---|---|---|---|\n",
    )?;
    for ((repo, path), environment) in &model.environments {
        writeln!(
            content,
            "| {} | {} | {} | {} | {} | {} | {} |",
            Literal(repo),
            Literal(path),
            Cell {
                node: environment.node,
                key: "name",
                absent: "none stated",
            },
            cell(environment.node, "declared_by"),
            environment.workloads.len() + environment.reached.len() + environment.charts.len(),
            Evidence(environment.node),
            Notes {
                weak: weak(&environment.node.props),
                included_by: &environment.included_by,
                renames: &environment.renames,
            }
        )?;
    }
    if model.environments.is_empty() {
        content.push_str("| — | None in this export | — | — | 0 | — | — | — | — |\n")?;
    }

    content.push_str(
        "\n## Workloads by environment\n\n\
         One row for each workload, under the path of the environment that declares it. Each \
         cell is what the file writes; nothing is resolved.\n\n\
         | Repository | Environment path | Workload | Image | Build context | Ports | \
         Environment key names | Environment files | Dependencies | Profiles | Tier | Confidence \
         | Span | Note |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for ((repo, path), environment) in &model.environments {
        for workload in &environment.workloads {
            let node = workload.node;
            writeln!(
                content,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                Literal(repo),
                Literal(path),
                Literal(workload.name),
                cell(node, "image"),
                cell(node, "build"),
                cell(node, "ports"),
                cell(node, "environment_keys"),
                cell(node, "env_files"),
                Dependencies(workload),
                cell(node, "profiles"),
                Evidence(node),
                note(weak(&node.props))
            )?;
        }
    }
    if model.compose_workloads() == 0 {
        content.push_str(
            "| — | None in this export | — | — | — | — | — | — | — | — | — | — | — | — |\n",
        )?;
    }

    content.push_str(
        "\nKubernetes workloads, under the path of each kustomization that reaches them: one row \
         for each container. \"Reached through\" lists the kustomizations between the \
         environment and the manifest file. Each cell is what the manifest file writes; no \
         patch, `images` entry or transformer of the environment is applied.\n\n\
         | Repository | Environment path | Reached through | Workload | Kind | Declared in | \
         Namespace | Replicas | Container | Image | Ports | Environment key names | \
         Configuration references | Tier | Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    let mut reached_rows = false;
    for ((repo, path), environment) in &model.environments {
        for reached in &environment.reached {
            let manifest = &model.manifests[reached.workload];
            reached_rows = true;
            manifest_rows(
                &mut content,
                &format_args!(
                    "| {} | {} | {} | {} | {} | {} ",
                    Literal(repo),
                    Literal(path),
                    Via(reached.declares),
                    Literal(manifest.name),
                    manifest_kind(manifest.node),
                    Literal(manifest.path)
                ),
                manifest,
                &Notes {
                    weak: weak(&manifest.node.props) || weak(&reached.declares.props),
                    included_by: &[],
                    renames: &environment.renames,
                },
            )?;
        }
    }
    if !reached_rows {
        content.push_str(
            "| — | None in this export | — | — | — | — | — | — | — | — | — | — | — | — | — | — \
             | — |\n",
        )?;
    }

    content.push_str(
        "\nHelm charts, under the path of each helmfile release that installs them, then the \
         charts no release installs. Templates are not rendered: a chart's objects are not \
         listed. Values files are shown by their key paths only; no value is read.\n\n\
         | Repository | Environment path | Chart | Declared in | Version | App version | \
         Default values keys | Release values files and their keys | Tier | Confidence | Span \
         | Note |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    let mut chart_rows = false;
    let mut chart_row = |content: &mut Text,
                         repo: &str,
                         environment: &str,
                         chart: &Manifest,
                         declares: Option<&Edge>|
     -> fmt::Result {
        chart_rows = true;
        writeln!(
            content,
            "| {} | {environment} | {} | {} | {} | {} | {} | {} | {} | {} |",
            Literal(repo),
            Literal(chart.name),
            Literal(chart.path),
            cell(chart.node, "version"),
            cell(chart.node, "app_version"),
            cell(chart.node, "values_keys"),
            ReleaseValues(declares),
            Evidence(chart.node),
            note(weak(&chart.node.props) || declares.is_some_and(|edge| weak(&edge.props)))
        )
    };
    for ((repo, path), environment) in &model.environments {
        for installed in &environment.charts {
            let chart = &model.charts[installed.workload];
            chart_row(
                &mut content,
                repo,
                &Literal(path).to_string(),
                chart,
                Some(installed.declares),
            )?;
        }
    }
    let mut lone: Vec<&Manifest> = model
        .charts
        .values()
        .filter(|chart| chart.reached_by.is_empty())
        .collect();
    lone.sort_by_key(|chart| (chart.repo, chart.path, chart.node.id.as_str()));
    for chart in lone {
        chart_row(&mut content, chart.repo, "no environment", chart, None)?;
    }
    if !chart_rows {
        content
            .push_str("| — | None in this export | — | — | — | — | — | — | — | — | — | — |\n")?;
    }

    content.push_str(
        "\nSpring Boot profiles, one row per profile environment, with the key paths its \
         settings set. No value is read; the span is where the profile is stated.\n\n\
         | Repository | Environment path | Profile | Profile declared in | Settings keys | Tier \
         | Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|---|---|\n",
    )?;
    let mut profile_rows = false;
    for ((repo, path), environment) in &model.environments {
        if environment.node.props["declared_by"] != "spring-profile" {
            continue;
        }
        profile_rows = true;
        let declared_in = environment.node.props["profile_declared_in"]
            .as_str()
            .or_else(|| environment.node.props["file"].as_str())
            .unwrap_or("—");
        let omitted = environment.node.props["settings_keys_omitted"].as_u64();
        writeln!(
            content,
            "| {} | {} | {} | {} | {}{} | {} | {} |",
            Literal(repo),
            Literal(path),
            cell(environment.node, "name"),
            Literal(declared_in),
            cell(environment.node, "settings_keys"),
            omitted.map_or(String::new(), |n| format!(" and {n} more")),
            Evidence(environment.node),
            note(weak(&environment.node.props))
        )?;
    }
    if !profile_rows {
        content.push_str("| — | None in this export | — | — | — | — | — | — | — |\n")?;
    }

    content.push_str(
        "\n## Declared in more than one environment\n\n\
         A workload name that two or more workload facts of one repository share, one row for \
         each declaration (each workload fact). \"Declared in\" is the file that writes it; \
         \"Reached by\" lists the environments that declare it, or \"no environment\". \
         \"Properties that differ\" lists the recorded properties that are not the same in every \
         declaration of that name, with this declaration's value; where a name is declared \
         both as a Compose service and as a Kubernetes workload, only the images, ports and \
         environment key names are compared. A workload that several environments reach is one \
         declaration and has one row. This section does not say which declaration applies \
         where.\n\n\
         | Repository | Workload | Kind | Declared in | Reached by | Properties that differ | \
         Tier | Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    // By repository, then workload name, then file, then fact id: each
    // `Workload` fact once, however many environments reach it.
    let mut by_name: BTreeMap<(&str, &str), Declarations> = BTreeMap::new();
    for ((repo, path), environment) in &model.environments {
        for workload in &environment.workloads {
            by_name.entry((repo, workload.name)).or_default().insert(
                (path, workload.node.id.as_str()),
                Declaration {
                    node: workload.node,
                    reached_by: workload.declared.map(|_| *path).into_iter().collect(),
                },
            );
        }
    }
    for manifest in model.manifests.values() {
        by_name
            .entry((manifest.repo, manifest.name))
            .or_default()
            .insert(
                (manifest.path, manifest.node.id.as_str()),
                Declaration {
                    node: manifest.node,
                    reached_by: manifest.reached_by.clone(),
                },
            );
    }
    let mut shared = 0usize;
    for ((repo, name), declarations) in &by_name {
        if declarations.len() < 2 {
            continue;
        }
        shared += 1;
        let all: Vec<&Node> = declarations.values().map(|fact| fact.node).collect();
        let mixed = all
            .iter()
            .any(|node| is_manifest(node) != is_manifest(all[0]));
        for ((path, _), fact) in declarations {
            let node = fact.node;
            writeln!(
                content,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                Literal(repo),
                Literal(name),
                manifest_kind(node),
                Literal(path),
                ReachedBy(&fact.reached_by),
                Differing {
                    workload: node,
                    declarations: &all,
                    mixed,
                },
                Evidence(node),
                note(weak(&node.props))
            )?;
        }
    }
    if shared == 0 {
        content.push_str("| — | None in this export | — | — | — | — | — | — | — | — |\n")?;
    }

    content.push_str(
        "\nA Kubernetes workload that two or more kustomizations reach, one row for each of \
         them. \"Patch states\" is what that environment's patch files write for the workload, \
         or \"no patch stated\"; \"`images` entries\" are the entries of that environment's \
         `images` field that name one of the workload's images, as written. Nothing is applied, \
         and this section does not say which declaration applies where. Tier, confidence and \
         span are those of the relation from the environment to the workload.\n\n\
         | Repository | Workload | Kind | Declared in | Environment path | Reached through | \
         Patch states | `images` entries | Tier | Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    let mut several = false;
    for manifest in model
        .manifests
        .values()
        .filter(|manifest| manifest.reached_by.len() > 1)
    {
        several = true;
        for path in &manifest.reached_by {
            let environment = &model.environments[&(manifest.repo, *path)];
            let Some(reached) = environment
                .reached
                .iter()
                .find(|reached| reached.workload == manifest.node.id)
            else {
                continue;
            };
            writeln!(
                content,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                Literal(manifest.repo),
                Literal(manifest.name),
                manifest_kind(manifest.node),
                Literal(manifest.path),
                Literal(path),
                Via(reached.declares),
                Patch(reached.patches),
                Images {
                    environment: environment.node,
                    workload: manifest.node,
                },
                Related(reached.declares),
                Notes {
                    weak: weak(&reached.declares.props)
                        || reached.patches.is_some_and(|edge| weak(&edge.props)),
                    included_by: &[],
                    renames: &environment.renames,
                }
            )?;
        }
    }
    if !several {
        content
            .push_str("| — | None in this export | — | — | — | — | — | — | — | — | — | — |\n")?;
    }

    content.push_str(
        "\n## Workloads no environment declares\n\n\
         A Kubernetes workload that no kustomization in this export reaches: one row for each \
         container. It is a declared workload; no file read here places it in an environment.\n\n\
         | Repository | Path | Workload | Kind | Namespace | Replicas | Container | Image | \
         Ports | Environment key names | Configuration references | Tier | Confidence | Span | \
         Note |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    // By repository, then path, then id.
    let mut alone: Vec<&Manifest> = model
        .manifests
        .values()
        .filter(|manifest| manifest.reached_by.is_empty())
        .collect();
    alone.sort_by_key(|manifest| (manifest.repo, manifest.path, manifest.node.id.as_str()));
    for manifest in &alone {
        manifest_rows(
            &mut content,
            &format_args!(
                "| {} | {} | {} | {} ",
                Literal(manifest.repo),
                Literal(manifest.path),
                Literal(manifest.name),
                manifest_kind(manifest.node)
            ),
            manifest,
            &note(weak(&manifest.node.props)),
        )?;
    }
    if alone.is_empty() {
        content.push_str(
            "| — | None in this export | — | — | — | — | — | — | — | — | — | — | — | — | — |\n",
        )?;
    }

    content.push_str(
        "\n## Not established\n\n\
         | Repository | Path | Workload | What | Reason | Tier | Confidence | Span |\n\
         |---|---|---|---|---|---|---|---|\n",
    )?;
    for gap in &model.gaps {
        write!(
            content,
            "| {} | {} | ",
            Literal(gap.repo),
            Literal(gap.path)
        )?;
        match (
            gap.service,
            gap.node.props["workload"].as_str(),
            gap.node.props["document"].as_u64(),
        ) {
            (Some(service), _, _) => write!(content, "{}", Literal(service))?,
            (None, Some(workload), _) => write!(content, "{}", Literal(workload))?,
            (None, None, Some(document)) => write!(content, "document {document} of the file")?,
            (None, None, None) => content.push_str("—")?,
        }
        writeln!(
            content,
            " | {} | {} | {} |",
            gap_kind(gap.node),
            Literal(gap.node.props["reason"].as_str().unwrap_or("not recorded")),
            Evidence(gap.node)
        )?;
    }
    if model.gaps.is_empty() {
        content.push_str("| — | None in this export | — | — | — | — | — | — |\n")?;
    }

    content.push_str("\n## Coverage note: what this document does not cover\n\n")?;
    for line in NOT_COVERED {
        writeln!(content, "- {line}")?;
    }

    super::render::inline_literal(&mut content, &assertions.values)?;
    Ok(Output {
        content: content.finish(),
        assertions: assertions.values,
        subjects: assertions.subjects,
        attested: Vec::new(),
    })
}

/// Borrowed index roles of this producer, for the compile workspace model.
/// Rows hold references only; provenance is decoded one row at a time. The
/// names a workload's properties hold are JSON values of its fact, and the
/// comparison reads them in place, so nothing is charged by values beyond
/// the list of declarations.
pub(super) fn logical_index_bytes(rows: usize, values: usize) -> Option<usize> {
    use super::accounting::{map_slots, slots, sum};
    sum(&[
        map_slots::<&str, &Node>(rows)?,                        // by_id
        map_slots::<&str, &Edge>(rows)?,                        // DECLARES by workload
        map_slots::<(&str, &str), &Edge>(rows)?,                // DECLARES by environment
        map_slots::<(&str, &str), &Edge>(rows)?,                // PATCHES by environment
        map_slots::<&str, Manifest<'static>>(rows)?,            // manifest workloads
        slots::<Reached<'static>>(rows)?,                       // one per DECLARES
        slots::<&str>(rows)?,                                   // reached by
        slots::<((&str, &str), &str)>(rows)?,                   // includes
        slots::<&str>(rows)?,                                   // included by
        slots::<&Manifest<'static>>(rows)?,                     // undeclared
        map_slots::<&str, BTreeMap<&str, &Edge>>(rows)?,        // DEPENDS_ON by source
        map_slots::<&str, &Edge>(rows)?,                        // their elements
        map_slots::<(&str, &str), Environment<'static>>(rows)?, // environments
        slots::<Workload<'static>>(rows)?,                      // one per workload
        slots::<&Edge>(rows)?,                                  // its dependencies
        slots::<Gap<'static>>(rows)?,                           // one per Gap
        map_slots::<(&str, &str), Vec<(&str, &Node)>>(rows)?,   // by name
        slots::<(&str, &Node)>(rows)?,                          // its declarations
        slots::<&Node>(rows)?,                                  // one comparison set
        slots::<&str>(values)?,                                 // dependency names of one row
    ])
}

#[cfg(test)]
#[path = "environments_tests.rs"]
mod tests;
