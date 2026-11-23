//! Build and deployment pipelines (#1818, AC-0650, catalogue D7 §7.5):
//! `pipelines.md`.
//!
//! A deterministic (T0) projection of the pipeline facts the ingest reader
//! recovers (ADR-0060): `Pipeline`, `PipelineTrigger`, `PipelineJob` and
//! `PipelineStep` facts, the `PART_OF` relations that place them and the
//! `NEEDS` relations between jobs. No model is called and no fact is
//! recovered here.
//!
//! **Placement.** A trigger or job is listed under the pipeline its admitted
//! `PART_OF` relation names, and a step under its job. A fact whose pipeline,
//! job or `PART_OF` relation is not among the admitted facts is not listed
//! and is counted.
//!
//! **Dependency order.** Per pipeline, a topological order over the `NEEDS`
//! relations that resolve to a listed job of the same pipeline; among jobs
//! that are ready together, the lowest declared position goes first, then
//! the lowest fact id. A job with a need that does not resolve is never
//! placed, nor is a job that needs one, nor a job on a cycle: each is listed
//! after the ordered jobs with the reason (R-INT-4).
//!
//! **Cost.** One index over the admitted facts, then each job is placed
//! once and each `NEEDS` relation read once: linear in the pipeline facts.
//! Nothing is walked to a depth.
//!
//! **Mode.** The producer receives the facts the export mode admitted, like
//! every other graph artifact, so a `verified-only` export holds no
//! InferredWeak row. The caller counts what the mode and rejections withheld
//! ([`withheld`]) and the document states it (R-INT-5).
//!
//! **Files not read.** Listed from the stored coverage inventories, which
//! are facts about the run and are the same in both modes. They are not
//! graph facts, so those rows carry the inventory hash and no assertion.

use super::producers::Output;
use super::render::{Assertions, Budget, Literal, RenderLimit, Text};
use super::{ExportMode, citation_id, included, provenance};
use core_graph::coverage::StoredCoverage;
use core_graph::{Edge, FactKey, Node};
use core_prov::{ConfidenceTier, Provenance};
use serde::Deserialize;
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::{self, Write as _};

const PIPELINE: &str = "Pipeline";
const TRIGGER: &str = "PipelineTrigger";
const JOB: &str = "PipelineJob";
const STEP: &str = "PipelineStep";
const PART_OF: &str = "PART_OF";
const NEEDS: &str = "NEEDS";

/// Coverage finding kinds the pipeline reader records for a file that is
/// present and not read, and what each names. Mirrors `UNREAD_CI` and
/// `UNREADABLE_KIND` in `crates/ingest/src/pipelines.rs`.
const UNREAD_KINDS: &[(&str, &str)] = &[
    ("gitlab-ci", "GitLab CI configuration"),
    ("jenkins-pipeline", "Jenkins pipeline"),
    ("circleci", "CircleCI configuration"),
    ("azure-pipelines", "Azure Pipelines configuration"),
    (
        "unreadable-pipeline-file",
        "GitHub Actions workflow or Dockerfile that could not be read exactly",
    ),
];

/// What the pipeline reader does not cover, stated in every export.
const NOT_COVERED: &[&str] = &[
    "A Dockerfile that uses a heredoc (`<<`) or changes its escape character is not read: it \
     yields no fact and is counted under the files present and not read.",
    "A workflow that uses a YAML alias, anchor or tag is not read, for the same reason.",
    "A workflow or Dockerfile that cannot be read exactly for another reason, such as invalid \
     YAML or needs that form a cycle, is not read either. Nothing is recovered from part of a \
     file.",
    "A file the reader does not recognise by name is not detected at all, so it is not listed \
     as unread either. Workflows are YAML files directly in `.github/workflows`. Dockerfiles \
     are named `Dockerfile` or `Containerfile`, either with a dotted suffix, or end in \
     `.dockerfile`.",
    "GitLab CI, Jenkins, CircleCI and Azure Pipelines files are named as present and are not \
     read. Any other build or deployment system is not detected.",
    "Nothing is evaluated. A matrix is one job with its matrix as written, and expressions \
     and variables stay as written.",
    "A reusable workflow and a third-party action are references. Their own jobs and steps \
     are not resolved.",
    "The values of `with:` inputs, `ENV` and `ARG` are not stored; their names are. `env:` \
     maps are not stored. A secret is recorded by name only.",
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

/// How many pipeline facts the export mode and the rejections withhold,
/// counted over the facts before that filter: the four node labels, and the
/// `PART_OF` and `NEEDS` relations whose source is a pipeline fact id.
pub(super) fn withheld<N: Borrow<Node>, E: Borrow<Edge>>(
    nodes: &[N],
    edges: &[E],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> usize {
    let nodes = nodes
        .iter()
        .map(Borrow::borrow)
        .filter(|node| matches!(node.label.as_str(), PIPELINE | TRIGGER | JOB | STEP))
        .filter(|node| !included(&provenance(&node.props, &node.id), mode, rejected))
        .count();
    let edges = edges
        .iter()
        .map(Borrow::borrow)
        .filter(|edge| {
            matches!(edge.label.as_str(), PART_OF | NEEDS) && edge.src.starts_with("pipeline-")
        })
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

/// The admitted facts the projection reads, by reference.
struct Graph<'a> {
    /// Pipeline facts and Gap nodes by id.
    by_id: BTreeMap<&'a str, &'a Node>,
    /// `PART_OF` relations by the fact they place something under, in
    /// declared order.
    parts: BTreeMap<&'a str, Vec<&'a Edge>>,
    /// `NEEDS` relations by the job that declares them, by needed fact id.
    needs: BTreeMap<&'a str, Vec<&'a Edge>>,
}

fn position(edge: &Edge) -> Option<u64> {
    edge.props["index"].as_u64()
}

impl<'a> Graph<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        let by_id: BTreeMap<&str, &Node> = nodes
            .iter()
            .filter(|node| matches!(node.label.as_str(), PIPELINE | TRIGGER | JOB | STEP | "Gap"))
            .map(|node| (node.id.as_str(), *node))
            .collect();
        let mut parts: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        let mut needs: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        for edge in edges {
            match edge.label.as_str() {
                // A placement that is itself unresolved places nothing.
                PART_OF if confidence(&edge.props) != ConfidenceTier::Gap => {
                    parts.entry(edge.dst.as_str()).or_default().push(edge);
                }
                NEEDS => needs.entry(edge.src.as_str()).or_default().push(edge),
                _ => {}
            }
        }
        // Never the store's or the caller's order: the declared position,
        // then the fact id.
        for edges in parts.values_mut() {
            edges.sort_by(|left, right| {
                (position(left).unwrap_or(u64::MAX), &left.src)
                    .cmp(&(position(right).unwrap_or(u64::MAX), &right.src))
            });
            edges.dedup_by(|left, right| left.src == right.src);
        }
        for edges in needs.values_mut() {
            edges.sort_by(|left, right| left.dst.cmp(&right.dst));
            edges.dedup_by(|left, right| left.dst == right.dst);
        }
        Self {
            by_id,
            parts,
            needs,
        }
    }

    /// The facts labelled `label` placed under `parent`, in declared order.
    fn members(&self, parent: &str, label: &str) -> Vec<Member<'a>> {
        self.parts
            .get(parent)
            .into_iter()
            .flatten()
            .filter_map(|part_of| {
                let node = *self.by_id.get(part_of.src.as_str())?;
                (node.label == label).then_some(Member { node, part_of })
            })
            .collect()
    }
}

/// A trigger, job or step with the relation that places it.
struct Member<'a> {
    node: &'a Node,
    part_of: &'a Edge,
}

impl Member<'_> {
    /// The R-INT-5 annotation of a row that rests on an InferredWeak fact.
    fn note(&self) -> &'static str {
        note(weak(&self.node.props) || weak(&self.part_of.props))
    }
}

fn note(weak: bool) -> &'static str {
    if weak {
        "rests on an InferredWeak fact; a verified-only export omits this row"
    } else {
        "—"
    }
}

/// One `NEEDS` relation of a job.
struct Need<'a> {
    edge: &'a Edge,
    /// The needed job's declared index in its pipeline; `None` when the need
    /// does not resolve to a listed job of the same pipeline.
    target: Option<usize>,
}

/// Where a job stands in its pipeline's dependency order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Its place, from 1.
    At(usize),
    /// One of its own needs does not resolve.
    Unresolved,
    /// It needs a job that is not ordered.
    Behind,
    /// Its needs form a cycle, or it needs a job on one.
    Cycle,
}

impl fmt::Display for Order {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::At(place) => write!(f, "{place}"),
            Self::Unresolved => f.write_str("not ordered: a need is not resolved"),
            Self::Behind => f.write_str("not ordered: needs a job that is not ordered"),
            Self::Cycle => {
                f.write_str("not ordered: its needs form a cycle, or it needs a job on one")
            }
        }
    }
}

struct Job<'a> {
    member: Member<'a>,
    needs: Vec<Need<'a>>,
    steps: Vec<Member<'a>>,
    order: Order,
}

/// One pipeline, by reference. Provenance is decoded when a row is written.
pub(super) struct Pipeline<'a> {
    node: &'a Node,
    triggers: Vec<Member<'a>>,
    /// In declared order.
    jobs: Vec<Job<'a>>,
    /// Indexes into `jobs`: the ordered jobs in dependency order, then the
    /// jobs that are not ordered, in declared order.
    sequence: Vec<usize>,
}

/// Place every job that can be placed and say why the others cannot. Each
/// job is placed or classified once and each need is read once.
fn order(jobs: &mut [Job<'_>]) -> Vec<usize> {
    let count = jobs.len();
    let mut waiting = vec![0usize; count];
    let mut blocked = vec![false; count];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (index, job) in jobs.iter().enumerate() {
        for need in &job.needs {
            match need.target {
                Some(target) => {
                    waiting[index] += 1;
                    dependents[target].push(index);
                }
                None => blocked[index] = true,
            }
        }
    }
    // Indexes are declared order, so the first ready job is the one declared
    // first.
    let mut ready: BTreeSet<usize> = (0..count)
        .filter(|index| waiting[*index] == 0 && !blocked[*index])
        .collect();
    let mut sequence = Vec::with_capacity(count);
    while let Some(next) = ready.pop_first() {
        sequence.push(next);
        jobs[next].order = Order::At(sequence.len());
        for &dependent in &dependents[next] {
            waiting[dependent] -= 1;
            if waiting[dependent] == 0 && !blocked[dependent] {
                ready.insert(dependent);
            }
        }
    }
    // What is left is unplaced. Tell a job behind an unresolved need from a
    // job on a cycle.
    let mut queue: VecDeque<usize> = (0..count).filter(|index| blocked[*index]).collect();
    for &index in &queue {
        jobs[index].order = Order::Unresolved;
    }
    while let Some(next) = queue.pop_front() {
        for &dependent in &dependents[next] {
            if jobs[dependent].order == Order::Cycle {
                jobs[dependent].order = Order::Behind;
                queue.push_back(dependent);
            }
        }
    }
    sequence.extend((0..count).filter(|index| !matches!(jobs[*index].order, Order::At(_))));
    sequence
}

fn pipelines<'a>(graph: &Graph<'a>) -> Vec<Pipeline<'a>> {
    graph
        .by_id
        .values()
        .filter(|node| node.label == PIPELINE)
        .map(|node| {
            let members = graph.members(&node.id, JOB);
            let declared: BTreeMap<&str, usize> = members
                .iter()
                .enumerate()
                .map(|(index, member)| (member.node.id.as_str(), index))
                .collect();
            let mut jobs: Vec<Job<'a>> = members
                .into_iter()
                .map(|member| {
                    let needs = graph
                        .needs
                        .get(member.node.id.as_str())
                        .into_iter()
                        .flatten()
                        .map(|edge| Need {
                            edge,
                            target: (confidence(&edge.props) != ConfidenceTier::Gap)
                                .then(|| declared.get(edge.dst.as_str()).copied())
                                .flatten(),
                        })
                        .collect();
                    Job {
                        steps: graph.members(&member.node.id, STEP),
                        member,
                        needs,
                        // Until it is placed or classified.
                        order: Order::Cycle,
                    }
                })
                .collect();
            let sequence = order(&mut jobs);
            Pipeline {
                node,
                triggers: graph.members(&node.id, TRIGGER),
                jobs,
                sequence,
            }
        })
        .collect()
}

/// The part of a pipeline fact id that names it: `repo@path` of a pipeline,
/// the event of a trigger, the job id or `stage-N` of a job.
fn key(id: &str) -> &str {
    match id.rsplit_once('#') {
        Some((_, key)) => key,
        None => id.split_once(':').map_or(id, |(_, rest)| rest),
    }
}

fn prop<'a>(node: &'a Node, name: &str) -> &'a str {
    node.props[name].as_str().unwrap_or("not recorded")
}

/// The first evidence span, and how many more the fact index lists.
struct Span<'a>(&'a Provenance);
impl fmt::Display for Span<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(first) = self.0.evidence.first() else {
            return f.write_str("—");
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
        match self.0.evidence.len() - 1 {
            0 => Ok(()),
            more => write!(f, " (+{more} more in the fact index)"),
        }
    }
}

/// Tier, confidence and span cells of one fact.
struct Evidence<'a>(&'a Node);
impl fmt::Display for Evidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let provenance = provenance(&self.0.props, &self.0.id);
        write!(
            f,
            "{:?} | {:?} | {}",
            provenance.tier,
            provenance.confidence_tier,
            Span(&provenance)
        )
    }
}

/// The declared position a `PART_OF` relation records.
struct Position<'a>(&'a Edge);
impl fmt::Display for Position<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match position(self.0) {
            Some(index) => write!(f, "{index}"),
            None => f.write_str("not recorded"),
        }
    }
}

/// The citation id of a pipeline fact (ADR-0058).
struct NodeCite<'a>(&'a Node);
impl fmt::Display for NodeCite<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&citation_id(&FactKey::Node {
            id: self.0.id.clone(),
        }))
    }
}

/// The citation id of a relation with its confidence, decoded as its
/// assertion decodes it (ADR-0058, R-INT-2).
struct EdgeCite<'a>(&'a Edge);
impl fmt::Display for EdgeCite<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = citation_id(&FactKey::Edge {
            source: self.0.src.clone(),
            label: self.0.label.clone(),
            destination: self.0.dst.clone(),
        });
        let confidence = provenance(&self.0.props, &edge_identity(self.0)).confidence_tier;
        write!(f, "{id} ({confidence:?})")
    }
}

/// The Citation cell of a trigger, job or step row: the fact, the relation
/// that places it and, for a job, each `NEEDS` relation in the order of the
/// Needs cell. Exactly the facts the row asserts.
struct Cites<'p, 'a>(&'p Member<'a>, &'p [Need<'a>]);
impl fmt::Display for Cites<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(member, needs) = self;
        write!(
            f,
            "{}; placed by {}",
            NodeCite(member.node),
            EdgeCite(member.part_of)
        )?;
        for (index, need) in needs.iter().enumerate() {
            f.write_str(if index == 0 { "; needs " } else { ", " })?;
            write!(f, "{}", EdgeCite(need.edge))?;
        }
        Ok(())
    }
}

/// The properties of a fact that say what it declares, as stored: `label:
/// value` for each one present, in the order given. Stored text is literal.
struct Declares<'a>(&'a Node, &'static [(&'static str, &'static str)]);
impl fmt::Display for Declares<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut any = false;
        for (name, label) in self.1 {
            let value = &self.0.props[*name];
            if value.is_null() {
                continue;
            }
            if any {
                f.write_str("; ")?;
            }
            any = true;
            write!(f, "{label}: ")?;
            match value {
                serde_json::Value::String(text) => write!(f, "{}", Literal(text))?,
                serde_json::Value::Array(items) => {
                    for (index, item) in items.iter().enumerate() {
                        if index > 0 {
                            f.write_str(", ")?;
                        }
                        match item.as_str() {
                            Some(text) => write!(f, "{}", Literal(text))?,
                            None => write!(f, "{}", Literal(item))?,
                        }
                    }
                }
                other => write!(f, "{}", Literal(other))?,
            }
        }
        if !any {
            f.write_str("—")?;
        }
        Ok(())
    }
}

const PIPELINE_PROPS: &[(&str, &str)] = &[
    ("name", "name"),
    ("stages", "stages"),
    ("final_stage", "final stage"),
    ("global_args", "ARG names before the first FROM"),
    ("secrets", "secret names"),
    ("redactions", "redactions"),
];
const TRIGGER_PROPS: &[(&str, &str)] = &[("config", "filters"), ("redactions", "redactions")];
const JOB_PROPS: &[(&str, &str)] = &[
    ("name", "name"),
    ("runs_on", "runs-on"),
    ("environment", "environment"),
    ("if", "if"),
    ("matrix", "matrix"),
    ("uses", "uses"),
    ("secrets_inherit", "secrets inherit"),
    ("base_image", "base image"),
    ("base_stage", "base stage"),
    ("platform", "platform"),
    ("final", "final stage"),
    ("secrets", "secret names"),
    ("redactions", "redactions"),
];
const STEP_PROPS: &[(&str, &str)] = &[
    ("name", "name"),
    ("id", "id"),
    ("uses", "uses"),
    ("run", "run"),
    ("run_style", "run style"),
    ("text", "instruction"),
    ("if", "if"),
    ("with_keys", "input names"),
    ("copy_from", "copy from"),
    ("copy_from_stage", "copy from stage"),
    ("mount_from", "mount from"),
    ("mount_from_stages", "mount from stages"),
    ("values_withheld", "values withheld"),
    ("names_unreadable", "names unreadable"),
    ("secrets", "secret names"),
    ("redactions", "redactions"),
];

/// The needs of one job as declared: each needed job, or why it is not
/// resolved.
struct Needs<'p, 'a>(&'p Job<'a>, &'p Graph<'a>);
impl fmt::Display for Needs<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(job, graph) = self;
        if job.needs.is_empty() {
            return f.write_str("—");
        }
        for (index, need) in job.needs.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            let edge = need.edge;
            let target = graph.by_id.get(edge.dst.as_str());
            if need.target.is_some() {
                write!(f, "{}", Literal(key(&edge.dst)))?;
                if let Some(via) = edge.props["via"].as_array() {
                    f.write_str(" (via ")?;
                    for (index, via) in via.iter().filter_map(|via| via.as_str()).enumerate() {
                        if index > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "{}", Literal(via))?;
                    }
                    f.write_str(")")?;
                }
            } else {
                // The name as declared when the Gap records it, else the id.
                let named = target
                    .and_then(|node| node.props["needs"].as_str())
                    .unwrap_or_else(|| key(&edge.dst));
                let reason = edge.props["reason"]
                    .as_str()
                    .or_else(|| target.and_then(|node| node.props["reason"].as_str()))
                    .unwrap_or(match target {
                        Some(node) if node.label == JOB => "it names a job of another pipeline",
                        _ => "the needed job is not among the facts this export admits",
                    });
                write!(f, "not resolved: {} ({})", Literal(named), Literal(reason))?;
            }
            if weak(&edge.props) {
                f.write_str(" [InferredWeak; a verified-only export leaves this need out]")?;
            }
        }
        Ok(())
    }
}

/// The recorded inventory body, read for the pipeline limits only. A body
/// without the list is one this version cannot read, never an empty list.
#[derive(Deserialize)]
struct Inventory {
    unsupported: Vec<Unsupported>,
}

#[derive(Deserialize)]
struct Unsupported {
    capability: String,
    occurrences: u64,
    first_site: String,
    reason: String,
}

const NO_INVENTORY: &str = "No stored coverage inventory was supplied to this compilation, so \
nothing is stated here about pipeline files that are present and were not read. An empty list \
below would not mean that there are none.\n";

/// The files present and not read, from the stored inventories.
fn unread(records: &[StoredCoverage], out: &mut Text) -> fmt::Result {
    out.push_str(
        "\n## Files present and not read\n\n\
         These rows come from the coverage inventory stored with each repository's facts, not \
         from the graph, and are the same in both export modes. Each is a limit of the tool, \
         not a gap in the system. The inventory records one row per kind for each repository: \
         how many files, the first of them and the reason recorded for that first file. The \
         other files, and their reasons where they differ, are not stored.\n\n",
    )?;
    if records.is_empty() {
        return out.push_str(NO_INVENTORY);
    }
    // The store reads by repo key already; sorting keeps the artifact
    // independent of the order a caller supplies.
    let mut records: Vec<&StoredCoverage> = records.iter().collect();
    records.sort_by(|left, right| {
        (&left.record.repo_key, &left.inventory_hash)
            .cmp(&(&right.record.repo_key, &right.inventory_hash))
    });
    let mut unreadable = 0usize;
    let mut rows = 0usize;
    out.push_str(
        "| Repository | Present and not read | Files | First file | Recorded reason | Inventory \
         hash |\n|---|---|---|---|---|---|\n",
    )?;
    for stored in &records {
        let Ok(inventory) = serde_json::from_str::<Inventory>(&stored.record.inventory_json) else {
            unreadable += 1;
            continue;
        };
        for (kind, what) in UNREAD_KINDS {
            for entry in inventory
                .unsupported
                .iter()
                .filter(|entry| entry.capability == *kind)
            {
                rows += 1;
                writeln!(
                    out,
                    "| {} | {what} | {} | {} | {} | {} |",
                    Literal(&stored.record.repo_key),
                    entry.occurrences,
                    Literal(&entry.first_site),
                    Literal(&entry.reason),
                    Literal(&stored.inventory_hash)
                )?;
            }
        }
    }
    if rows == 0 {
        out.push_str("| — | None recorded | 0 | — | — | — |\n")?;
    }
    writeln!(
        out,
        "\nStored inventories read: {}. Inventories this version cannot read, about which \
         nothing is stated: {unreadable}.",
        records.len() - unreadable
    )
}

/// Cite a listed fact once, and the relation that places it.
fn cite<'a>(
    assertions: &mut Assertions,
    listed: &mut BTreeSet<&'a str>,
    member: &Member<'a>,
) -> Result<(), RenderLimit> {
    if listed.insert(member.node.id.as_str()) {
        assertions.node(member.node, None)?;
    }
    assertions.edge(member.part_of)
}

const INTRO: &str = "# Build and deployment pipelines\n\n\
A deterministic projection of the pipeline facts recovered from GitHub Actions workflows and \
Dockerfiles: each pipeline with its triggers, its jobs in dependency order and each job's steps \
in declared order. No model is involved. Nothing is run or evaluated: the document states what \
the files declare, not what a run did.\n\n\
## How to read this document\n\n\
- **A pipeline** is one workflow file or one Dockerfile. **A job** is a workflow job or a \
Dockerfile stage. **A step** is a workflow step or a Dockerfile instruction.\n\
- **Dependency order** is computed from the recovered needs: a job is listed after every job it \
needs. Where jobs do not depend on each other, the one declared first comes first.\n\
- **Not ordered.** A job with a need that is not resolved, a job that needs such a job, and a \
job whose needs form a cycle have no place in the order. They are listed after the ordered \
jobs, each with the reason, in declared order.\n\
- **Declared position** is the position the file gives a trigger, job or step, counted from 0.\n\
- Every row carries the tier, confidence and source span of its fact. The fact index lists \
every span.\n\
- **Citation** gives the fact-index citation id of each fact a row states: the fact itself; \
for a trigger, job or step the relation that places it, with its confidence; and for a job each \
need, with its confidence, in the order of the Needs cell. `fact-index.md` lists the tier, \
confidence, method and evidence spans of each id.\n\
- **Files present and not read** are listed from the stored coverage inventory. The last \
section lists what the reader does not cover.\n\n";

/// `pipelines.md` over the facts the export mode admitted, the count of
/// pipeline facts it withheld and the stored coverage inventories.
pub(super) fn output(
    nodes: &[&Node],
    edges: &[&Edge],
    coverage: &[StoredCoverage],
    (mode, withheld): (ExportMode, usize),
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let graph = Graph::new(nodes, edges);
    let pipelines = pipelines(&graph);

    let mut assertions = Assertions::new(&budget);
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    for pipeline in &pipelines {
        assertions.node(pipeline.node, None)?;
        for trigger in &pipeline.triggers {
            cite(&mut assertions, &mut listed, trigger)?;
        }
        for job in pipeline.sequence.iter().map(|index| &pipeline.jobs[*index]) {
            cite(&mut assertions, &mut listed, &job.member)?;
            for need in &job.needs {
                assertions.edge(need.edge)?;
            }
            for step in &job.steps {
                cite(&mut assertions, &mut listed, step)?;
            }
        }
    }
    let unlisted = graph
        .by_id
        .values()
        .filter(|node| matches!(node.label.as_str(), TRIGGER | JOB | STEP))
        .filter(|node| !listed.contains(node.id.as_str()))
        .count();

    let total = |count: fn(&Pipeline<'_>) -> usize| pipelines.iter().map(count).sum::<usize>();
    let unordered = |pipeline: &Pipeline<'_>| {
        pipeline
            .jobs
            .iter()
            .filter(|job| !matches!(job.order, Order::At(_)))
            .count()
    };
    let steps = |pipeline: &Pipeline<'_>| {
        pipeline
            .jobs
            .iter()
            .map(|job| job.steps.len())
            .sum::<usize>()
    };
    let mut content = Text::new(&budget, INTRO)?;
    writeln!(
        content,
        "Export mode: `{}`. Pipelines: {}. Triggers: {}. Jobs: {}, of which not ordered: {}. \
         Steps: {}.\n",
        match mode {
            ExportMode::VerifiedOnly => "verified-only",
            ExportMode::BestEffort => "best-effort",
        },
        pipelines.len(),
        total(|pipeline| pipeline.triggers.len()),
        total(|pipeline| pipeline.jobs.len()),
        total(unordered),
        total(steps),
    )?;
    match mode {
        ExportMode::VerifiedOnly => writeln!(
            content,
            "Pipeline facts left out of this export because they are InferredWeak or rejected: \
             {withheld}. A `best-effort` export includes the InferredWeak ones, marked.\n"
        )?,
        ExportMode::BestEffort => writeln!(
            content,
            "InferredWeak rows are included and marked in the Note column. Pipeline facts left \
             out of this export because they are rejected: {withheld}.\n"
        )?,
    }
    if unlisted > 0 {
        writeln!(
            content,
            "{unlisted} admitted trigger, job or step facts are not listed: the pipeline, the \
             job or the placing relation above each is not among the facts this export \
             admits.\n"
        )?;
    }

    content.push_str(
        "## Pipelines\n\n\
         | Pipeline | Format | Declares | Triggers | Jobs | Steps | Tier | Confidence | Span | \
         Citation | Note |\n|---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for pipeline in &pipelines {
        writeln!(
            content,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            Literal(key(&pipeline.node.id)),
            Literal(prop(pipeline.node, "format")),
            Declares(pipeline.node, PIPELINE_PROPS),
            pipeline.triggers.len(),
            pipeline.jobs.len(),
            steps(pipeline),
            Evidence(pipeline.node),
            NodeCite(pipeline.node),
            note(weak(&pipeline.node.props))
        )?;
    }
    if pipelines.is_empty() {
        content.push_str("| — | — | None in this export | 0 | 0 | 0 | — | — | — | — | — |\n")?;
    }

    for pipeline in &pipelines {
        writeln!(
            content,
            "\n## Pipeline {}\n",
            Literal(key(&pipeline.node.id))
        )?;
        match unordered(pipeline) {
            0 => writeln!(
                content,
                "Format: {}. Every job is ordered.",
                Literal(prop(pipeline.node, "format"))
            )?,
            count => writeln!(
                content,
                "Format: {}. {count} of {} jobs are not ordered; each row says why.",
                Literal(prop(pipeline.node, "format")),
                pipeline.jobs.len()
            )?,
        }

        content.push_str(
            "\n### Triggers\n\n\
             | Declared position | Event | Declares | Tier | Confidence | Span | Citation | \
             Note |\n|---|---|---|---|---|---|---|---|\n",
        )?;
        for trigger in &pipeline.triggers {
            writeln!(
                content,
                "| {} | {} | {} | {} | {} | {} |",
                Position(trigger.part_of),
                Literal(key(&trigger.node.id)),
                Declares(trigger.node, TRIGGER_PROPS),
                Evidence(trigger.node),
                Cites(trigger, &[]),
                trigger.note()
            )?;
        }
        if pipeline.triggers.is_empty() {
            content.push_str("| — | None recovered | — | — | — | — | — | — |\n")?;
        }

        content.push_str(
            "\n### Jobs in dependency order\n\n\
             | Order | Job | Kind | Declared position | Needs | Declares | Tier | Confidence | \
             Span | Citation | Note |\n|---|---|---|---|---|---|---|---|---|---|---|\n",
        )?;
        let jobs = || pipeline.sequence.iter().map(|index| &pipeline.jobs[*index]);
        for job in jobs() {
            writeln!(
                content,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                job.order,
                Literal(key(&job.member.node.id)),
                Literal(prop(job.member.node, "kind")),
                Position(job.member.part_of),
                Needs(job, &graph),
                Declares(job.member.node, JOB_PROPS),
                Evidence(job.member.node),
                Cites(&job.member, &job.needs),
                job.member.note()
            )?;
        }
        if pipeline.jobs.is_empty() {
            content.push_str("| — | None recovered | — | — | — | — | — | — | — | — | — |\n")?;
        }

        content.push_str(
            "\n### Steps in declared order\n\n\
             Jobs are in the order of the table above.\n\n\
             | Job | Declared position | Kind | Declares | Tier | Confidence | Span | Citation | \
             Note |\n|---|---|---|---|---|---|---|---|---|\n",
        )?;
        for job in jobs() {
            for step in &job.steps {
                writeln!(
                    content,
                    "| {} | {} | {} | {} | {} | {} | {} |",
                    Literal(key(&job.member.node.id)),
                    Position(step.part_of),
                    Literal(prop(step.node, "kind")),
                    Declares(step.node, STEP_PROPS),
                    Evidence(step.node),
                    Cites(step, &[]),
                    step.note()
                )?;
            }
        }
        if steps(pipeline) == 0 {
            content.push_str("| — | — | — | None recovered | — | — | — | — | — |\n")?;
        }
    }

    unread(coverage, &mut content)?;

    content.push_str("\n## Coverage note: what the pipeline reader does not cover\n\n")?;
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
/// stored coverage inventories are admitted by their own read limits.
pub(super) fn logical_index_bytes(rows: usize) -> Option<usize> {
    use super::accounting::{map_slots, set_slots, slots, sum};
    sum(&[
        map_slots::<&str, &Node>(rows)?,      // by_id
        map_slots::<&str, Vec<&Edge>>(rows)?, // PART_OF by parent
        slots::<&Edge>(rows)?,                // their elements
        map_slots::<&str, Vec<&Edge>>(rows)?, // NEEDS by job
        slots::<&Edge>(rows)?,                // their elements
        slots::<Pipeline<'static>>(rows)?,    // one per pipeline
        slots::<Member<'static>>(rows)?,      // triggers and steps
        slots::<Job<'static>>(rows)?,         // one per job
        slots::<Need<'static>>(rows)?,        // one per NEEDS relation
        map_slots::<&str, usize>(rows)?,      // declared job indexes
        slots::<usize>(rows)?,                // waiting
        slots::<bool>(rows)?,                 // blocked
        slots::<Vec<usize>>(rows)?,           // dependents
        slots::<usize>(rows)?,                // their elements
        set_slots::<usize>(rows)?,            // ready
        slots::<usize>(rows)?,                // sequence
        slots::<usize>(rows)?,                // classification queue
        set_slots::<&str>(rows)?,             // listed facts
    ])
}

#[cfg(test)]
#[path = "pipelines_tests.rs"]
mod tests;
