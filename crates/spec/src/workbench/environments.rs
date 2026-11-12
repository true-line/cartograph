//! Environments and what they declare (#1791, AC-0661, catalogue D7 §7.2):
//! `environments.md`.
//!
//! A deterministic (T0) projection of the facts `ingest::environments`
//! records (ADR-0070, SPEC-14): `Environment` and `Workload` facts, their
//! `DECLARES` and `DEPENDS_ON` relations, and the `gap:environment:` Gaps. No
//! model is called and no fact is recovered here.
//!
//! **A path is never a name.** An environment is shown by the path of the
//! file that declares it. It has a name only where that file states one;
//! otherwise the cell reads "none stated".
//!
//! **Nothing is merged.** A workload name that several environments of one
//! repository declare is listed once per declaration, with the recorded
//! properties that differ between them and each declaration's span. The
//! document does not say which declaration applies where: the files do not
//! say which of them a deployment combines.
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
const ENVIRONMENT_ID: &str = "env:";
const WORKLOAD_ID: &str = "workload:";
const GAP_ID: &str = "gap:environment:";
/// `props.kind` of the Gap of a name-only environment file.
const NAME_ONLY: &str = "name-only-environment";

/// The workload properties compared between declarations, each with the
/// words a row uses for it.
const COMPARED: &[(&str, &str)] = &[
    ("image", "image"),
    ("build", "build context"),
    ("ports", "ports"),
    ("environment_keys", "environment key names"),
    ("env_files", "environment files"),
    ("depends_on", "dependencies"),
    ("profiles", "profiles"),
];

/// What the projection does not cover, stated in every export.
const NOT_COVERED: &[&str] = &[
    "Only Compose files are read: `compose.yaml`, `compose.yml`, `docker-compose.yaml` and \
     `docker-compose.yml`, each also with one dotted infix (`compose.prod.yaml`). A Compose file \
     with any other name is not read.",
    "Kubernetes manifests, kustomize files, Helm charts and Helm values files are not read. The \
     coverage document lists each kind found as present and not read.",
    "Terraform resources are not attached to an environment.",
    "Nothing is merged. Which Compose files a deployment combines is not written in them, so \
     the effective configuration of a deployment is not shown.",
    "Which environment is production, staging or any other stage is not stated: no file states \
     it.",
    "Live state is not read. A declared workload may not be running, and a running one may not \
     be declared.",
    "No value of an environment key is read or stored, and no secret.",
    "`extends`, `include`, YAML anchors, aliases and tags, and `${...}` variables are not \
     evaluated. Each is listed under Not established or marked on its row.",
    "A variable written as `$NAME`, without braces, is stored as written and is not marked.",
];

/// The confidence a fact records, without decoding its whole provenance. A
/// fact with none is unresolved, as [`provenance`] makes it.
fn confidence(props: &serde_json::Value) -> ConfidenceTier {
    ConfidenceTier::deserialize(&props["prov"]["confidence_tier"]).unwrap_or(ConfidenceTier::Gap)
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
    (edge.label == DECLARES
        && edge.src.starts_with(ENVIRONMENT_ID)
        && edge.dst.starts_with(WORKLOAD_ID))
        || (edge.label == DEPENDS_ON
            && edge.src.starts_with(WORKLOAD_ID)
            && edge.dst.starts_with(WORKLOAD_ID))
}

/// How many environment facts the export mode and the rejections withhold,
/// counted over the facts before that filter: the `Environment`, `Workload`
/// and Gap facts and their `DECLARES` and `DEPENDS_ON` relations.
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

/// One `Workload` fact.
struct Workload<'a> {
    node: &'a Node,
    name: &'a str,
    /// Its `DECLARES` relation, when the export admits it.
    declared: Option<&'a Edge>,
    /// Its `DEPENDS_ON` relations, by the workload they name.
    depends: Vec<&'a Edge>,
}

/// One `Environment` fact.
struct Environment<'a> {
    node: &'a Node,
    workloads: Vec<Workload<'a>>,
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
        let mut declares: BTreeMap<&'a str, &'a Edge> = BTreeMap::new();
        let mut depends: BTreeMap<&'a str, BTreeMap<&'a str, &'a Edge>> = BTreeMap::new();
        for edge in edges.iter().copied().filter(|edge| is_relation(edge)) {
            if edge.label == DECLARES {
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
                let repo = repo_of(&node.id, WORKLOAD_ID, path, &format!("#{name}"))?;
                Some((repo, path, name))
            });
            let Some((repo, path, name)) = placed else {
                model.unplaced += 1;
                continue;
            };
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
        for node in by_id.values().copied().filter(|n| n.label == "Gap") {
            let service = node.props["service"].as_str();
            let suffix = service
                .map(|service| format!("#{service}"))
                .unwrap_or_default();
            match path_of(node)
                .and_then(|path| Some((repo_of(&node.id, GAP_ID, path, &suffix)?, path)))
            {
                Some((repo, path)) => model.gaps.push(Gap {
                    node,
                    repo,
                    path,
                    service,
                }),
                None => model.unplaced += 1,
            }
        }
        model
            .gaps
            .sort_by_key(|gap| (gap.repo, gap.path, gap.service, gap.node.id.as_str()));
        model
    }

    fn workloads(&self) -> usize {
        self.environments
            .values()
            .map(|environment| environment.workloads.len())
            .sum()
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
        let provenance = provenance(&self.0.props, &self.0.id);
        write!(
            f,
            "{:?} | {:?} | ",
            provenance.tier, provenance.confidence_tier
        )?;
        let Some(first) = provenance.evidence.first() else {
            return f.write_str(if self.0.props["kind"] == NAME_ONLY {
                "none stored for an environment file"
            } else {
                "—"
            });
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
            serde_json::Value::Array(values) if !values.is_empty() => {
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    match value.as_str() {
                        Some(value) => write!(f, "{}", Literal(value))?,
                        None => write!(f, "{}", Literal(value))?,
                    }
                }
            }
            _ => return f.write_str(self.absent),
        }
        if interpolated(self.node, self.key) {
            f.write_str(" (holds `${`; not resolved)")?;
        }
        Ok(())
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

/// The properties of `workload` that differ among `declarations`, with this
/// declaration's value of each.
struct Differing<'p, 'a> {
    workload: &'p Workload<'a>,
    declarations: &'p [&'p Workload<'a>],
}
impl fmt::Display for Differing<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut any = false;
        for (key, words) in COMPARED {
            let own = &self.workload.node.props[*key];
            let own_mark = interpolated(self.workload.node, key);
            let differs = self.declarations.iter().any(|other| {
                other.node.props[*key] != *own || interpolated(other.node, key) != own_mark
            });
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
                    node: self.workload.node,
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
        _ => "not recorded",
    }
}

const INTRO: &str = "# Environments\n\n\
A deterministic projection of the environments the ingested sources declare and the workloads \
each declares. No model is involved.\n\n\
## How to read this document\n\n\
- **An environment** here is a declared deployable set: one file that declares a set of things \
to run together. Today that is a Compose file with a `services` mapping. Each such file is one \
environment.\n\
- **An environment is shown by the path of its file.** It has a name only where the file \
states one (a top-level `name` in a Compose file). Where the file states none, the name cell \
reads \"none stated\". A file or directory name is never used as an environment name.\n\
- **A workload** is one thing an environment declares to run: one Compose service, with its \
image, build context, ports, the names of its environment keys, its environment files, its \
dependencies and its profiles, as the file writes them.\n\
- **Declared in more than one environment** lists a workload name that two or more environments \
of one repository declare. Each declaration has its own row. The document does not say which \
declaration is used where: no file says which files a deployment combines.\n\
- **Not established** lists what was found and could not be stated: a file that could not be \
read, a construct that is not evaluated, a dependency on a service the file does not declare, \
and an environment file whose name is the only thing that names an environment.\n\
- Every row carries the tier, confidence and source span of its fact. The fact index lists \
every span. An environment file has no span: these files hold secrets, so no byte range of one \
is stored.\n\
- Only the names of environment keys are listed. No value is stored or shown.\n\
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
    }
    for gap in &model.gaps {
        assertions.node(gap.node, None)?;
    }

    let name_only = model.name_only();
    let mut content = Text::new(&budget, INTRO)?;
    writeln!(
        content,
        "Export mode: `{}`. Environments: {}. Workloads: {}. Environments named only by a file \
         name: {}. Other items not established: {}.\n",
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
            environment.workloads.len(),
            Evidence(environment.node),
            note(weak(&environment.node.props))
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
    if model.workloads() == 0 {
        content.push_str(
            "| — | None in this export | — | — | — | — | — | — | — | — | — | — | — | — |\n",
        )?;
    }

    content.push_str(
        "\n## Declared in more than one environment\n\n\
         A workload name that two or more environments of one repository declare, one row for \
         each declaration. \"Properties that differ\" lists the recorded properties that are \
         not the same in every declaration of that name, with this declaration's value. This \
         section does not say which declaration applies where.\n\n\
         | Repository | Workload | Environment path | Properties that differ | Tier | \
         Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|---|\n",
    )?;
    // By repository, then workload name, then environment path.
    let mut by_name: BTreeMap<(&str, &str), Vec<(&str, &Workload)>> = BTreeMap::new();
    for ((repo, path), environment) in &model.environments {
        for workload in &environment.workloads {
            by_name
                .entry((repo, workload.name))
                .or_default()
                .push((path, workload));
        }
    }
    let mut shared = 0usize;
    for ((repo, name), declarations) in &by_name {
        if declarations.len() < 2 {
            continue;
        }
        shared += 1;
        let all: Vec<&Workload> = declarations.iter().map(|(_, workload)| *workload).collect();
        for (path, workload) in declarations {
            writeln!(
                content,
                "| {} | {} | {} | {} | {} | {} |",
                Literal(repo),
                Literal(name),
                Literal(path),
                Differing {
                    workload,
                    declarations: &all,
                },
                Evidence(workload.node),
                note(weak(&workload.node.props))
            )?;
        }
    }
    if shared == 0 {
        content.push_str("| — | None in this export | — | — | — | — | — | — |\n")?;
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
        match gap.service {
            Some(service) => write!(content, "{}", Literal(service))?,
            None => content.push_str("—")?,
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
        map_slots::<&str, BTreeMap<&str, &Edge>>(rows)?,        // DEPENDS_ON by source
        map_slots::<&str, &Edge>(rows)?,                        // their elements
        map_slots::<(&str, &str), Environment<'static>>(rows)?, // environments
        slots::<Workload<'static>>(rows)?,                      // one per workload
        slots::<&Edge>(rows)?,                                  // its dependencies
        slots::<Gap<'static>>(rows)?,                           // one per Gap
        map_slots::<(&str, &str), Vec<(&str, &Workload<'static>)>>(rows)?, // by name
        slots::<(&str, &Workload<'static>)>(rows)?,             // its declarations
        slots::<&Workload<'static>>(rows)?,                     // one comparison set
        slots::<&str>(values)?,                                 // dependency names of one row
    ])
}

#[cfg(test)]
#[path = "environments_tests.rs"]
mod tests;
