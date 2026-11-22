//! Ordered build and deployment steps (#1790, AC-0598): GitHub Actions
//! workflows and `Dockerfile`s as T0 facts, each citing the exact byte span
//! that declares it.
//!
//! The toolchain reader records *that* a workflow or `Dockerfile` exists;
//! this reader records the order it declares. Four node labels and two edge
//! labels carry both formats:
//!
//! - `Pipeline` (`pipeline:{repo}@{path}`): one workflow file or one
//!   `Dockerfile`; `format` is `github-actions` or `dockerfile`.
//! - `PipelineTrigger` (`pipeline-trigger:{repo}@{path}#{event}`): one `on:`
//!   event of a workflow, with its filter block as declared.
//! - `PipelineJob` (`pipeline-job:{repo}@{path}#{key}`): a workflow job
//!   (`kind: job`, keyed by its job id) or a `Dockerfile` stage (`kind:
//!   stage`, keyed `stage-{index}`).
//! - `PipelineStep` (`pipeline-step:{repo}@{path}#{key}/{index}`): a job step
//!   (`kind: uses` or `run`) or a stage instruction (`kind: instruction`).
//! - `PART_OF` (trigger → pipeline, job → pipeline, step → job) carries the
//!   declared position as `index`; order is never implied by store order.
//! - `NEEDS` (job → job): a workflow `needs`, a stage built `FROM` an
//!   earlier stage, a `COPY --from=` an earlier stage, or a `RUN
//!   --mount=…,from=` an earlier stage (`via` names which).
//!
//! The labels are deliberately not `DEFINED_IN`, `STEP_OF`, `TRIGGERS` or
//! `DEPENDS_ON`: the flow tracer and the topology map walk those, and a
//! pipeline is neither a business flow nor deployed topology.
//!
//! Nothing is evaluated. `${{ … }}` expressions, matrix expansions and
//! reusable workflows stay as declared: a matrix job is one job with its
//! `matrix` source text, and a `uses:` is a reference. A `needs` naming a job
//! the workflow does not declare is an explicit `Gap` (R-INT-4).
//!
//! A file this reader cannot read exactly yields **no** facts and a named
//! limit ([`UNREADABLE_KIND`]) instead of a partial order. Other CI systems
//! are named as present and not read ([`unread`]).
//!
//! Redaction is fail-closed, to the toolchain reader's standard (AC-0097):
//! every stored free-text value passes [`core_redact::redact_text`], and any
//! run of token characters [`core_redact::is_token_shaped`] accepts is
//! withheld, with the count disclosed as `redactions`. `ENV` and `ARG`
//! instructions keep their names only. A `${{ secrets.NAME }}` reference is
//! recorded as `NAME`; a workflow file holds no secret value to store.
//!
//! This tier never calls an LLM.

use crate::schedules::{contains_kind, first_mapping, instructions, scalar, workflow_file};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

/// Extractor id recorded on every pipeline fact.
pub const PIPELINE_EXTRACTOR_ID: &str = "t0.pipeline";

/// Preflight finding kind for a workflow or `Dockerfile` that could not be
/// read exactly, so no step fact was emitted for it.
pub const UNREADABLE_KIND: &str = "unreadable-pipeline-file";

/// Marker left where a token-shaped run was withheld from stored text.
const WITHHELD: &str = "[WITHHELD]";

/// Repository identity for pipeline facts.
#[derive(Debug, Clone, Copy)]
pub struct SourceId<'a> {
    /// Repository identity (`owner/name` or `local/<name>`).
    pub repo: &'a str,
    /// Commit the tree was read at.
    pub commit: &'a str,
}

/// A pipeline file that was present and yielded no facts, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    /// Repo-relative path.
    pub path: String,
    /// Why no fact was emitted.
    pub reason: &'static str,
}

/// Facts from one directory walk.
#[derive(Debug, Clone, Default)]
pub struct PipelineExtraction {
    /// `Pipeline`, `PipelineTrigger`, `PipelineJob`, `PipelineStep` and the
    /// `Gap` of each dangling `needs`.
    pub nodes: Vec<Node>,
    /// `PART_OF` and `NEEDS` edges.
    pub edges: Vec<Edge>,
    /// Workflow files and `Dockerfile`s that yielded facts.
    pub files: u64,
    /// Workflow files and `Dockerfile`s that yielded none, with the reason.
    pub unread: Vec<Unread>,
}

/// A pipeline limit Preflight reports as an unsupported finding, which the
/// coverage inventory then lists with its occurrence count and first site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    /// Finding kind: the unread CI system, or [`UNREADABLE_KIND`].
    pub kind: &'static str,
    /// What is present and why it was not read.
    pub message: String,
}

/// CI systems detected by file name and not read: finding kind and name.
const UNREAD_CI: &[(&str, &str)] = &[
    ("gitlab-ci", "GitLab CI"),
    ("jenkins-pipeline", "Jenkins"),
    ("circleci", "CircleCI"),
    ("azure-pipelines", "Azure Pipelines"),
];

/// True when `kind` is a finding kind this module produces. Coverage quotes
/// such a finding's own message as its reason.
pub fn is_limit_kind(kind: &str) -> bool {
    kind == UNREADABLE_KIND || UNREAD_CI.iter().any(|(known, _)| *known == kind)
}

/// The CI system `rel` configures when this reader does not cover it.
fn unread_ci(name: &str, rel: &str) -> Option<(&'static str, &'static str)> {
    let yaml = name.ends_with(".yml") || name.ends_with(".yaml");
    let index = if name == ".gitlab-ci.yml" {
        0
    } else if name == "Jenkinsfile" || name.starts_with("Jenkinsfile.") {
        1
    } else if rel == ".circleci/config.yml" || rel.ends_with("/.circleci/config.yml") {
        2
    } else if yaml
        && (name.starts_with("azure-pipelines")
            || rel.starts_with(".azure-pipelines/")
            || rel.contains("/.azure-pipelines/"))
    {
        3
    } else {
        return None;
    };
    Some(UNREAD_CI[index])
}

/// The limit `rel` carries, if any: another CI system's configuration
/// (present, not read), or a workflow or `Dockerfile` that cannot be read
/// exactly. Reads the file only when its name is a workflow or `Dockerfile`.
pub fn unread(name: &str, rel: &str, path: &Path) -> Option<Limit> {
    if let Some((kind, system)) = unread_ci(name, rel) {
        return Some(Limit {
            kind,
            message: format!(
                "{system} configuration is present and not read: no installed producer \
                 recovers its pipeline order"
            ),
        });
    }
    let format = format_of(name, rel)?;
    let reason = match std::fs::read_to_string(path) {
        Ok(text) => parse(format, &text).err()?,
        Err(_) => "file is not readable UTF-8 text",
    };
    Some(Limit {
        kind: UNREADABLE_KIND,
        message: format!(
            "{} is present and its steps were not recovered: {reason}",
            format.display()
        ),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Workflow,
    Dockerfile,
}

impl Format {
    fn id(self) -> &'static str {
        match self {
            Self::Workflow => "github-actions",
            Self::Dockerfile => "dockerfile",
        }
    }

    fn display(self) -> &'static str {
        match self {
            Self::Workflow => "GitHub Actions workflow",
            Self::Dockerfile => "Dockerfile",
        }
    }
}

/// Which format `rel` is. A workflow is a YAML file directly in
/// `.github/workflows`. A `Dockerfile` is named `Dockerfile` or
/// `Containerfile`, either of them with a dotted suffix (`Dockerfile.dev`),
/// or anything ending in `.dockerfile` (`api.dockerfile`, any case; #1818).
fn format_of(name: &str, rel: &str) -> Option<Format> {
    if workflow_file(rel) && (name.ends_with(".yml") || name.ends_with(".yaml")) {
        return Some(Format::Workflow);
    }
    const SUFFIX: &str = ".dockerfile";
    let named = ["Dockerfile", "Containerfile"].iter().any(|base| {
        name.strip_prefix(base)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    });
    let suffixed = name.len() > SUFFIX.len()
        && name.is_char_boundary(name.len() - SUFFIX.len())
        && name[name.len() - SUFFIX.len()..].eq_ignore_ascii_case(SUFFIX);
    // `Dockerfile.dockerignore` is an ignore list, not a build.
    ((named || suffixed) && !name.ends_with(".dockerignore")).then_some(Format::Dockerfile)
}

pub(crate) type Span = (u64, u64);
type Props = serde_json::Map<String, serde_json::Value>;

/// One pipeline file, parsed and redacted, before graph-fact conversion.
#[derive(Debug)]
struct Decl {
    props: Props,
    spans: Vec<Span>,
    triggers: Vec<Part>,
    jobs: Vec<JobDecl>,
}

/// A trigger or a step: an identity suffix, its props and its span.
#[derive(Debug)]
struct Part {
    key: String,
    props: Props,
    span: Span,
}

#[derive(Debug)]
struct JobDecl {
    key: String,
    props: Props,
    span: Span,
    needs: Vec<Need>,
    steps: Vec<Part>,
}

#[derive(Debug)]
struct Need {
    /// The name as declared.
    name: String,
    /// Index of the job it names; `None` when no such job is declared.
    target: Option<usize>,
    via: &'static str,
    span: Span,
}

fn parse(format: Format, text: &str) -> Result<Decl, &'static str> {
    let mut decl = match format {
        Format::Workflow => workflow(text)?,
        Format::Dockerfile => dockerfile(text)?,
    };
    depths(&mut decl.jobs)?;
    Ok(decl)
}

/// Record each job's `depth`: the longest resolved `needs` chain below it, so
/// dependency order is a stored property. A job with a dangling `needs`, and
/// every job above it, has none. A cycle is not a pipeline.
fn depths(jobs: &mut [JobDecl]) -> Result<(), &'static str> {
    #[derive(Clone, Copy)]
    enum State {
        Unvisited,
        Visiting,
        Done(Option<u64>),
    }
    fn visit(index: usize, jobs: &[JobDecl], state: &mut [State]) -> Result<Option<u64>, ()> {
        match state[index] {
            State::Done(depth) => return Ok(depth),
            State::Visiting => return Err(()),
            State::Unvisited => {}
        }
        state[index] = State::Visiting;
        let mut depth = Some(0u64);
        for need in &jobs[index].needs {
            let below = match need.target {
                Some(target) => visit(target, jobs, state)?,
                None => None,
            };
            depth = match (depth, below) {
                (Some(depth), Some(below)) => Some(depth.max(below + 1)),
                _ => None,
            };
        }
        state[index] = State::Done(depth);
        Ok(depth)
    }
    let mut state = vec![State::Unvisited; jobs.len()];
    for index in 0..jobs.len() {
        let depth = visit(index, jobs, &mut state).map_err(|()| "job needs form a cycle")?;
        if let Some(depth) = depth {
            jobs[index].props.insert("depth".into(), depth.into());
        }
    }
    Ok(())
}

/// Stored free text: recognized secrets replaced, token-shaped runs
/// withheld. Returns the text and how many replacements were made.
pub(crate) fn sanitize(input: &str) -> (String, u64) {
    let (redacted, mut count) = core_redact::redact_text(input);
    let token_char =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '_' | '-' | '=' | '.');
    let mut out = String::with_capacity(redacted.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String, count: &mut usize| {
        if core_redact::is_token_shaped(run) {
            out.push_str(WITHHELD);
            *count += 1;
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in redacted.chars() {
        if token_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out, &mut count);
            out.push(c);
        }
    }
    flush(&mut run, &mut out, &mut count);
    (out, count as u64)
}

/// Props under construction, counting what redaction replaced.
#[derive(Default)]
struct Draft {
    props: Props,
    redactions: u64,
}

impl Draft {
    fn set(&mut self, key: &str, value: impl Into<serde_json::Value>) {
        self.props.insert(key.into(), value.into());
    }

    /// Store free text from the source, redacted.
    fn text(&mut self, key: &str, value: &str) {
        let (safe, count) = sanitize(value);
        self.redactions += count;
        self.set(key, safe);
    }

    fn finish(mut self) -> Props {
        if self.redactions > 0 {
            self.set("redactions", self.redactions);
        }
        self.props
    }
}

pub(crate) fn span_of(node: TsNode) -> Span {
    (node.start_byte() as u64, node.end_byte() as u64)
}

/// `node` beneath its `block_node` / `flow_node` wrappers.
fn unwrap(node: TsNode) -> Option<TsNode> {
    let mut node = node;
    while matches!(node.kind(), "block_node" | "flow_node") {
        let mut walk = node.walk();
        let children: Vec<TsNode> = node
            .named_children(&mut walk)
            .filter(|child| child.kind() != "comment")
            .collect();
        let [only] = children.as_slice() else {
            return None;
        };
        node = *only;
    }
    Some(node)
}

/// One mapping entry: its key, its value when it has one, and its span.
pub(crate) struct Field<'t> {
    pub(crate) name: String,
    pub(crate) value: Option<TsNode<'t>>,
    pub(crate) span: Span,
    /// The key alone.
    pub(crate) key_span: Span,
}

/// The entries of a block or flow mapping in declared order. A computed or
/// repeated key is not read.
pub(crate) fn fields<'t>(text: &str, node: TsNode<'t>) -> Result<Vec<Field<'t>>, &'static str> {
    let mapping = unwrap(node)
        .filter(|node| matches!(node.kind(), "block_mapping" | "flow_mapping"))
        .ok_or("a mapping was expected")?;
    let mut walk = mapping.walk();
    let children: Vec<TsNode> = mapping
        .named_children(&mut walk)
        .filter(|child| child.kind() != "comment")
        .collect();
    let mut out: Vec<Field> = Vec::new();
    for child in children {
        let (key, value) = match child.kind() {
            "block_mapping_pair" | "flow_pair" => (
                child.child_by_field_name("key"),
                child.child_by_field_name("value"),
            ),
            // A bare key in a flow mapping: `{push, pull_request}`.
            "flow_node" => (Some(child), None),
            _ => return Err("a mapping entry is not a key and value"),
        };
        let name = key
            .and_then(|key| scalar(text, key))
            .ok_or("a mapping key is not a literal scalar")?;
        if out.iter().any(|field| field.name == name) {
            return Err("a mapping repeats a key");
        }
        out.push(Field {
            name,
            value,
            span: span_of(child),
            key_span: key.map_or_else(|| span_of(child), span_of),
        });
    }
    Ok(out)
}

pub(crate) fn field<'a, 't>(fields: &'a [Field<'t>], name: &str) -> Option<&'a Field<'t>> {
    fields.iter().find(|field| field.name == name)
}

/// The items of a block or flow sequence; `None` when `node` is not one.
pub(crate) fn items(node: TsNode) -> Option<Vec<TsNode>> {
    let sequence = unwrap(node)?;
    let mut walk = sequence.walk();
    let children: Vec<TsNode> = sequence
        .named_children(&mut walk)
        .filter(|child| child.kind() != "comment")
        .collect();
    match sequence.kind() {
        "block_sequence" => children
            .into_iter()
            .map(|item| {
                let mut walk = item.walk();
                let inner: Vec<TsNode> = item
                    .named_children(&mut walk)
                    .filter(|child| child.kind() != "comment")
                    .collect();
                match inner.as_slice() {
                    [only] => Some(*only),
                    _ => None,
                }
            })
            .collect(),
        "flow_sequence" => children
            .into_iter()
            .map(|item| (item.kind() == "flow_node").then_some(item))
            .collect(),
        _ => None,
    }
}

/// One literal scalar, or a sequence of them, each with its span.
fn scalars(text: &str, node: TsNode) -> Option<Vec<(String, Span)>> {
    if let Some(value) = scalar(text, node) {
        return Some(vec![(value, span_of(node))]);
    }
    items(node)?
        .into_iter()
        .map(|item| Some((scalar(text, item)?, span_of(item))))
        .collect()
}

/// The content of a block scalar (`|`, `>`) with its indentation removed,
/// and its style. An explicit indentation indicator keeps the source text.
fn block_scalar(raw: &str) -> (String, &'static str) {
    let (header, body) = raw.split_once('\n').unwrap_or((raw, ""));
    let style = if header.starts_with('>') {
        "folded"
    } else {
        "literal"
    };
    let indicators = header.split('#').next().unwrap_or(header);
    if indicators.contains(|c: char| c.is_ascii_digit()) {
        return (raw.trim().to_string(), style);
    }
    let indent = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = body
        .lines()
        .map(|line| line.get(indent..).unwrap_or("").trim_end())
        .collect();
    (lines.join("\n").trim_end().to_string(), style)
}

/// A string value as declared: a literal scalar's value, a block scalar's
/// content, or the source text of a double-quoted scalar whose escapes are
/// not decoded. `None` for a mapping or sequence. The second value is the
/// block style, when it is a block scalar.
fn declared(text: &str, node: TsNode) -> Option<(String, Option<&'static str>)> {
    if let Some(value) = scalar(text, node) {
        return Some((value, None));
    }
    let inner = unwrap(node)?;
    match inner.kind() {
        "block_scalar" => {
            let (value, style) = block_scalar(&text[inner.byte_range()]);
            Some((value, Some(style)))
        }
        "double_quote_scalar" => Some((text[inner.byte_range()].to_string(), None)),
        _ => None,
    }
}

/// A descriptive value as declared: a string, or the source text of a
/// mapping or sequence. Never evaluated.
fn described(text: &str, node: TsNode) -> String {
    declared(text, node)
        .map(|(value, _)| value)
        .unwrap_or_else(|| text[node.byte_range()].trim().to_string())
}

fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// Secret names an expression reads: `secrets.NAME` and `secrets['NAME']`.
fn expression_secrets(expression: &str, out: &mut BTreeSet<String>) {
    let name_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(found) = expression[from..].find("secrets") {
        let at = from + found;
        from = at + "secrets".len();
        let previous = expression[..at].chars().next_back();
        if previous.is_some_and(|c| name_char(c) || matches!(c, '.' | '-')) {
            continue;
        }
        let after = &expression[from..];
        let name: Option<&str> = if let Some(dotted) = after.strip_prefix('.') {
            let end = dotted.find(|c| !name_char(c)).unwrap_or(dotted.len());
            Some(&dotted[..end])
        } else if let Some(indexed) = after.trim_start().strip_prefix('[') {
            let indexed = indexed.trim_start();
            indexed
                .strip_prefix('\'')
                .and_then(|quoted| quoted.split_once('\''))
                .map(|(name, _)| name)
        } else {
            None
        };
        if let Some(name) = name.filter(|name| !name.is_empty() && name.chars().all(name_char)) {
            out.insert(name.to_string());
        }
    }
}

/// Secret names referenced inside the `${{ … }}` expressions of `source`.
fn secret_names(source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = source;
    while let Some(open) = rest.find("${{") {
        let body = &rest[open + 3..];
        let close = body.find("}}").unwrap_or(body.len());
        expression_secrets(&body[..close], &mut out);
        rest = &body[close..];
    }
    out
}

fn set_secrets(draft: &mut Draft, mut names: BTreeSet<String>, condition: Option<&str>) {
    // An `if:` is an expression even without `${{ }}`.
    if let Some(condition) = condition {
        expression_secrets(condition, &mut names);
    }
    if !names.is_empty() {
        draft.set("secrets", names.into_iter().collect::<Vec<_>>());
    }
}

/// A GitHub Actions workflow, all of it or none of it.
fn workflow(text: &str) -> Result<Decl, &'static str> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .map_err(|_| "the YAML grammar is unavailable")?;
    let tree = parser.parse(text, None).ok_or("file is not valid YAML")?;
    let root = tree.root_node();
    if root.has_error() {
        return Err("file is not valid YAML");
    }
    let mut walk = root.walk();
    if root
        .named_children(&mut walk)
        .filter(|node| node.kind() == "document")
        .count()
        != 1
    {
        return Err("file is not one YAML document");
    }
    // An alias, anchor or tag means the text is not the structure that runs.
    if contains_kind(root, &["alias", "anchor", "tag"]) {
        return Err("workflow uses a YAML alias, anchor or tag");
    }
    let top = first_mapping(root).ok_or("workflow is not a mapping")?;
    let top = fields(text, top)?;
    let mut draft = Draft::default();
    let mut spans = Vec::new();
    if let Some(name) = field(&top, "name") {
        if let Some(value) = name.value {
            draft.text("name", &described(text, value));
        }
        spans.push(name.span);
    }
    let on = field(&top, "on").ok_or("workflow declares no `on` trigger")?;
    spans.push(on.span);
    let triggers = triggers(text, on.value.ok_or("workflow `on` is empty")?)?;
    let jobs_field = field(&top, "jobs").ok_or("workflow declares no `jobs`")?;
    let job_fields = fields(
        text,
        jobs_field.value.ok_or("workflow `jobs` is not a mapping")?,
    )
    .map_err(|_| "workflow `jobs` is not a mapping of distinct literal job ids")?;
    if job_fields.is_empty() {
        return Err("workflow declares no job");
    }
    let ids: Vec<&str> = job_fields.iter().map(|job| job.name.as_str()).collect();
    let mut jobs = Vec::new();
    for (index, declared_job) in job_fields.iter().enumerate() {
        jobs.push(job(text, index, declared_job, &ids)?);
    }
    set_secrets(&mut draft, secret_names(text), None);
    Ok(Decl {
        props: draft.finish(),
        spans,
        triggers,
        jobs,
    })
}

/// `on:` as a string, a list of strings, or a mapping of events.
fn triggers(text: &str, on: TsNode) -> Result<Vec<Part>, &'static str> {
    const SHAPE: &str = "workflow `on` is not an event, a list of events or a mapping of events";
    let mut out: Vec<Part> = Vec::new();
    let mut push = |event: String, config: Option<TsNode>, span: Span| {
        if !identifier(&event) {
            return Err("workflow `on` names an event that is not a plain identifier");
        }
        if out.iter().any(|known| known.key == event) {
            return Err("workflow `on` repeats an event");
        }
        let mut draft = Draft::default();
        draft.set("event", event.clone());
        draft.set("index", out.len() as u64);
        if let Some(config) = config {
            // Filters (`branches`, `paths`, `cron`, inputs) as declared.
            draft.text("config", text[config.byte_range()].trim());
        }
        out.push(Part {
            key: event,
            props: draft.finish(),
            span,
        });
        Ok(())
    };
    if let Some(events) = scalars(text, on) {
        if events.is_empty() {
            return Err("workflow `on` names no event");
        }
        for (event, span) in events {
            push(event, None, span)?;
        }
        return Ok(out);
    }
    let events = fields(text, on).map_err(|_| SHAPE)?;
    if events.is_empty() {
        return Err("workflow `on` names no event");
    }
    for event in events {
        push(event.name, event.value, event.span)?;
    }
    Ok(out)
}

fn job(
    text: &str,
    index: usize,
    declared_job: &Field,
    ids: &[&str],
) -> Result<JobDecl, &'static str> {
    if !identifier(&declared_job.name) {
        return Err("a job id is not a plain identifier");
    }
    let body = fields(text, declared_job.value.ok_or("a job is not a mapping")?)
        .map_err(|_| "a job is not a mapping of distinct literal keys")?;
    let mut draft = Draft::default();
    draft.set("kind", "job");
    draft.set("id", declared_job.name.clone());
    draft.set("index", index as u64);
    let value = |name: &str| field(&body, name).and_then(|field| field.value);
    for (key, prop) in [
        ("name", "name"),
        ("runs-on", "runs_on"),
        ("environment", "environment"),
        ("if", "if"),
    ] {
        if let Some(node) = value(key) {
            draft.text(prop, &described(text, node));
        }
    }
    // A matrix is recorded as declared, never expanded.
    if let Some(strategy) = value("strategy")
        && let Ok(strategy) = fields(text, strategy)
        && let Some(matrix) = field(&strategy, "matrix").and_then(|matrix| matrix.value)
    {
        draft.text("matrix", text[matrix.byte_range()].trim());
    }
    let mut needs = Vec::new();
    if let Some(node) = value("needs") {
        let names =
            scalars(text, node).ok_or("a job `needs` is not a job id or a list of job ids")?;
        draft.set(
            "needs",
            names
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>(),
        );
        for (name, span) in names {
            needs.push(Need {
                target: ids.iter().position(|id| *id == name),
                name,
                via: "needs",
                span,
            });
        }
    }
    let mut steps = Vec::new();
    match (value("uses"), value("steps")) {
        // A reusable workflow is a reference; its jobs are not resolved.
        (Some(uses), None) => {
            let uses = scalar(text, uses).ok_or("a job `uses` is not a literal reference")?;
            draft.set("uses", core_redact::redact_text(&uses).0);
            if value("secrets")
                .and_then(|secrets| scalar(text, secrets))
                .as_deref()
                == Some("inherit")
            {
                draft.set("secrets_inherit", true);
            }
        }
        (None, Some(sequence)) => {
            let declared_steps = items(sequence).ok_or("a job `steps` is not a sequence")?;
            for (position, item) in declared_steps.into_iter().enumerate() {
                steps.push(step(text, &declared_job.name, position, item)?);
            }
        }
        (None, None) => return Err("a job declares neither `steps` nor `uses`"),
        (Some(_), Some(_)) => return Err("a job declares both `steps` and `uses`"),
    }
    let source = &text[declared_job.span.0 as usize..declared_job.span.1 as usize];
    let condition = value("if").map(|node| described(text, node));
    set_secrets(&mut draft, secret_names(source), condition.as_deref());
    Ok(JobDecl {
        key: declared_job.name.clone(),
        props: draft.finish(),
        span: declared_job.span,
        needs,
        steps,
    })
}

fn step(text: &str, job: &str, index: usize, item: TsNode) -> Result<Part, &'static str> {
    let body =
        fields(text, item).map_err(|_| "a step is not a mapping of distinct literal keys")?;
    let value = |name: &str| field(&body, name).and_then(|field| field.value);
    let mut draft = Draft::default();
    draft.set("index", index as u64);
    match (value("uses"), value("run")) {
        (Some(uses), None) => {
            let uses = scalar(text, uses).ok_or("a step `uses` is not a literal reference")?;
            draft.set("kind", "uses");
            // A reference, not a command: redacted, but a pinned commit SHA
            // is not withheld as a token.
            draft.set("uses", core_redact::redact_text(&uses).0);
        }
        (None, Some(run)) => {
            let (command, style) = declared(text, run).ok_or("a step `run` is not a string")?;
            draft.set("kind", "run");
            draft.text("run", &command);
            if let Some(style) = style {
                draft.set("run_style", style);
            }
        }
        (None, None) => return Err("a step declares neither `uses` nor `run`"),
        (Some(_), Some(_)) => return Err("a step declares both `uses` and `run`"),
    }
    for (key, prop) in [("name", "name"), ("id", "id"), ("if", "if")] {
        if let Some(node) = value(key) {
            draft.text(prop, &described(text, node));
        }
    }
    // Input names only: values may carry credentials.
    if let Some(with) = value("with")
        && let Ok(inputs) = fields(text, with)
    {
        draft.set(
            "with_keys",
            inputs
                .into_iter()
                .map(|input| input.name)
                .collect::<Vec<_>>(),
        );
    }
    let span = span_of(item);
    let condition = value("if").map(|node| described(text, node));
    set_secrets(
        &mut draft,
        secret_names(&text[item.byte_range()]),
        condition.as_deref(),
    );
    Ok(Part {
        key: format!("{job}/{index}"),
        props: draft.finish(),
        span,
    })
}

/// Words of an instruction's arguments, quotes and backslash escapes kept
/// together; `None` when a quote is left open.
fn shell_words(rest: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some('"') | None, '\\') => {
                current.push(chars.next()?);
                started = true;
            }
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(current);
    }
    Some(words)
}

/// The variable names an `ENV` or `ARG` instruction declares; `None` when
/// they cannot be read exactly.
fn assigned_names(keyword: &str, rest: &str) -> Option<Vec<String>> {
    let words = shell_words(rest)?;
    let name = |word: &str| {
        let name = word.split('=').next().unwrap_or(word);
        (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .then(|| name.to_string())
    };
    let first = words.first()?;
    if keyword == "ENV" && !first.contains('=') {
        // The legacy `ENV NAME value…` form declares one variable.
        return Some(vec![name(first)?]);
    }
    if keyword == "ENV" && words.iter().any(|word| !word.contains('=')) {
        return None;
    }
    words.iter().map(|word| name(word)).collect()
}

/// The stored props of one instruction's text. `ENV` and `ARG` keep names
/// only, the standard `.env` files are held to (AC-0097).
fn instruction_text(draft: &mut Draft, keyword: &str, instruction: &str) {
    let rest = instruction[keyword.len()..].trim();
    let upper = keyword.to_ascii_uppercase();
    match upper.as_str() {
        "ENV" | "ARG" => {
            draft.set("values_withheld", true);
            match assigned_names(&upper, rest) {
                Some(names) => {
                    draft.set("text", format!("{upper} {}", names.join(" ")));
                    draft.set("names", names);
                }
                None => {
                    draft.set("text", upper);
                    draft.set("names_unreadable", true);
                }
            }
        }
        // `ONBUILD ENV …` holds the same values one level down.
        "ONBUILD" => {
            let inner = rest.split_whitespace().next().unwrap_or("");
            let mut nested = Draft::default();
            instruction_text(&mut nested, inner, rest);
            draft.redactions += nested.redactions;
            for (key, value) in nested.props {
                match (key.as_str(), value.as_str()) {
                    ("text", Some(text)) => draft.set("text", format!("ONBUILD {text}")),
                    _ => draft.set(&key, value),
                }
            }
        }
        _ => draft.text("text", instruction),
    }
}

/// The earlier stage `source` names: by its `AS` name, else by its index.
/// Anything else is an external image or build context, left as declared.
fn earlier_stage(earlier: &[Option<String>], source: &str) -> Option<usize> {
    let lowered = source.to_ascii_lowercase();
    earlier
        .iter()
        .rposition(|known| known.as_deref() == Some(&lowered))
        .or_else(|| {
            source
                .parse::<usize>()
                .ok()
                .filter(|target| *target < earlier.len())
        })
}

/// A `Dockerfile`, all of it or none of it: stages in declared order, each
/// with its base image and instructions; the last stage is the default
/// build target.
fn dockerfile(text: &str) -> Result<Decl, &'static str> {
    // A changed escape character or a heredoc changes how lines parse.
    if text
        .lines()
        .any(|line| line.trim().to_ascii_lowercase().starts_with("# escape="))
    {
        return Err("Dockerfile changes its escape character");
    }
    let mut global_args: Vec<String> = Vec::new();
    let mut stages: Vec<JobDecl> = Vec::new();
    // Lowercased `AS` names, by stage index.
    let mut names: Vec<Option<String>> = Vec::new();
    for (span, instruction) in instructions(text) {
        if instruction.contains("<<") {
            return Err("Dockerfile uses a heredoc");
        }
        let mut words = instruction.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        let upper = keyword.to_ascii_uppercase();
        let args: Vec<&str> = words.collect();
        if upper == "FROM" {
            let index = stages.len();
            let mut draft = Draft::default();
            draft.set("kind", "stage");
            draft.set("index", index as u64);
            let mut rest = args.as_slice();
            while let [flag, tail @ ..] = rest
                && flag.starts_with("--")
            {
                let platform = flag
                    .strip_prefix("--platform=")
                    .ok_or("a FROM instruction carries an unmodeled flag")?;
                draft.text("platform", platform);
                rest = tail;
            }
            let (image, name) = match rest {
                [image] => (*image, None),
                [image, keyword, name] if keyword.eq_ignore_ascii_case("as") => {
                    (*image, Some(*name))
                }
                _ => return Err("a FROM instruction is not `image [AS name]`"),
            };
            draft.text("base_image", image);
            let mut needs = Vec::new();
            // A stage built on an earlier stage, by its `AS` name.
            if let Some(target) = names
                .iter()
                .rposition(|known| known.as_deref() == Some(&image.to_ascii_lowercase()))
            {
                draft.set("base_stage", format!("stage-{target}"));
                needs.push(Need {
                    name: image.to_string(),
                    target: Some(target),
                    via: "from",
                    span,
                });
            }
            if let Some(name) = name {
                draft.text("name", name);
            }
            names.push(name.map(str::to_ascii_lowercase));
            stages.push(JobDecl {
                key: format!("stage-{index}"),
                props: draft.finish(),
                span,
                needs,
                steps: Vec::new(),
            });
        }
        let Some(stage_index) = stages.len().checked_sub(1) else {
            // Only `ARG` may precede the first `FROM`.
            if upper != "ARG" {
                return Err("Dockerfile has an instruction before its first FROM");
            }
            match assigned_names("ARG", instruction[keyword.len()..].trim()) {
                Some(found) => global_args.extend(found),
                None => return Err("Dockerfile has an unreadable ARG before its first FROM"),
            }
            continue;
        };
        let mut draft = Draft::default();
        let index = stages[stage_index].steps.len();
        draft.set("kind", "instruction");
        draft.set("index", index as u64);
        draft.set("instruction", upper.clone());
        instruction_text(&mut draft, keyword, &instruction);
        if upper == "COPY" {
            let sources: Vec<&str> = args
                .iter()
                .filter_map(|arg| arg.strip_prefix("--from="))
                .collect();
            if let [source] = sources.as_slice() {
                draft.text("copy_from", source);
                if let Some(target) = earlier_stage(&names[..stage_index], source) {
                    draft.set("copy_from_stage", format!("stage-{target}"));
                    stages[stage_index].needs.push(Need {
                        name: source.to_string(),
                        target: Some(target),
                        via: "copy-from",
                        span,
                    });
                }
            } else if !sources.is_empty() {
                return Err("a COPY instruction carries more than one --from");
            }
        }
        if upper == "RUN" {
            // `RUN --mount=type=bind,from=build,…`: a mount whose `from`
            // names an earlier stage builds on that stage (#1818).
            let mut declared: Vec<String> = Vec::new();
            let mut mounted: Vec<String> = Vec::new();
            for flag in args.iter().take_while(|arg| arg.starts_with("--")) {
                let Some(options) = flag.strip_prefix("--mount=") else {
                    continue;
                };
                // A quoted option list is CSV-quoted; it is not split here.
                if options.contains(['"', '\'']) {
                    return Err("a RUN --mount option is quoted");
                }
                let mut sources = options
                    .split(',')
                    .filter_map(|option| option.split_once('='))
                    .filter(|(key, _)| key.eq_ignore_ascii_case("from"))
                    .map(|(_, source)| source);
                let Some(source) = sources.next() else {
                    continue;
                };
                if sources.next().is_some() {
                    return Err("a RUN --mount carries more than one from");
                }
                let (safe, count) = sanitize(source);
                draft.redactions += count;
                declared.push(safe);
                if let Some(target) = earlier_stage(&names[..stage_index], source) {
                    mounted.push(format!("stage-{target}"));
                    stages[stage_index].needs.push(Need {
                        name: source.to_string(),
                        target: Some(target),
                        via: "mount-from",
                        span,
                    });
                }
            }
            if !declared.is_empty() {
                draft.set("mount_from", declared);
            }
            if !mounted.is_empty() {
                draft.set("mount_from_stages", mounted);
            }
        }
        let key = format!("stage-{stage_index}/{index}");
        stages[stage_index].steps.push(Part {
            key,
            props: draft.finish(),
            span,
        });
    }
    let last = stages
        .len()
        .checked_sub(1)
        .ok_or("Dockerfile has no FROM instruction")?;
    stages[last].props.insert("final".into(), true.into());
    let mut draft = Draft::default();
    draft.set("final_stage", format!("stage-{last}"));
    draft.set("stages", stages.len() as u64);
    if !global_args.is_empty() {
        draft.set("global_args", global_args);
    }
    let mut spans = vec![stages[0].span];
    if last != 0 {
        spans.push(stages[last].span);
    }
    Ok(Decl {
        props: draft.finish(),
        spans,
        triggers: Vec::new(),
        jobs: stages,
    })
}

/// Walk `root` and turn every GitHub Actions workflow and `Dockerfile` into
/// ordered facts. Deterministic for a given tree: the walk is sorted, and
/// every id derives from the path and the declared position.
pub fn extract_dir(root: &Path, id: &SourceId) -> std::io::Result<PipelineExtraction> {
    let paths = pipeline_paths(root)?;
    Ok(extract_from_sources(
        &paths,
        &|rel| std::fs::read(root.join(rel)),
        id,
    ))
}

/// The workflow and `Dockerfile` paths the walk of `root` selects, sorted:
/// the files a source's capture retains for this reader (AC-1057).
pub fn pipeline_paths(root: &Path) -> std::io::Result<Vec<String>> {
    let walked = source_walk::files(
        root,
        &crate::preflight::skip_dir,
        source_walk::Gitignores::Honor,
    )?;
    Ok(walked
        .into_iter()
        .filter(|file| format_of(&file.name, &file.rel).is_some())
        .map(|file| file.rel)
        .collect())
}

/// Whether the repo-relative `rel` is a file this reader parses.
pub fn is_pipeline_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    format_of(name, rel).is_some()
}

/// [`extract_dir`] over the repo-relative `paths`, each read through
/// `read`, so a captured source's pipelines come from its retained bytes
/// rather than the checkout (AC-1057, #385). A path this reader does not
/// parse is ignored; bytes that are not UTF-8 are unread, as on disk.
pub fn extract_from_sources(
    paths: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    id: &SourceId,
) -> PipelineExtraction {
    let mut paths: Vec<&String> = paths.iter().filter(|rel| is_pipeline_file(rel)).collect();
    paths.sort();
    paths.dedup();
    let mut out = PipelineExtraction::default();
    for rel in paths {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let Some(format) = format_of(name, rel) else {
            continue;
        };
        let parsed = match read(rel).map(String::from_utf8) {
            Ok(Ok(text)) => parse(format, &text),
            _ => Err("file is not readable UTF-8 text"),
        };
        match parsed {
            Ok(decl) => {
                out.files += 1;
                emit(id, rel, format, decl, &mut out);
            }
            Err(reason) => out.unread.push(Unread {
                path: rel.clone(),
                reason,
            }),
        }
    }
    out
}

fn emit(id: &SourceId, rel: &str, format: Format, decl: Decl, out: &mut PipelineExtraction) {
    let evidence = |(start, end): Span| EvidenceRef {
        repo: id.repo.to_string(),
        path: rel.to_string(),
        byte_start: start,
        byte_end: end,
        commit_sha: id.commit.to_string(),
    };
    // The content hash covers the fact's properties as well as its identity,
    // so an edited command, order or reference changes the graph fingerprint.
    let seal = |props: Props, confidence, spans: &[Span], identity: &str| {
        let mut props = serde_json::Value::Object(props);
        let input = serde_json::to_vec(&serde_json::json!([identity, &props])).expect("serializes");
        props["prov"] = serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                confidence,
                spans.iter().copied().map(evidence).collect(),
                PIPELINE_EXTRACTOR_ID,
                &input,
            )
            .expect("within ceiling"),
        )
        .expect("serializes");
        props
    };
    let node = |node_id: &str, label: &str, mut props: Props, spans: &[Span]| {
        props.insert("file".into(), rel.into());
        Node {
            id: node_id.to_string(),
            label: label.into(),
            props: seal(props, ConfidenceTier::Confirmed, spans, node_id),
        }
    };
    let part_of = |src: &str, dst: &str, index: Option<&serde_json::Value>, span: Span| {
        let mut props = Props::new();
        if let Some(index) = index {
            props.insert("index".into(), index.clone());
        }
        Edge {
            src: src.to_string(),
            dst: dst.to_string(),
            label: "PART_OF".into(),
            props: seal(
                props,
                ConfidenceTier::Confirmed,
                &[span],
                &format!("{src} PART_OF {dst}"),
            ),
        }
    };
    let pipeline_id = format!("pipeline:{}@{rel}", id.repo);
    let mut props = decl.props;
    props.insert("format".into(), format.id().into());
    out.nodes
        .push(node(&pipeline_id, "Pipeline", props, &decl.spans));
    for trigger in decl.triggers {
        let trigger_id = format!("pipeline-trigger:{}@{rel}#{}", id.repo, trigger.key);
        let index = trigger.props.get("index").cloned();
        out.nodes.push(node(
            &trigger_id,
            "PipelineTrigger",
            trigger.props,
            &[trigger.span],
        ));
        out.edges.push(part_of(
            &trigger_id,
            &pipeline_id,
            index.as_ref(),
            trigger.span,
        ));
    }
    let job_id = |key: &str| format!("pipeline-job:{}@{rel}#{key}", id.repo);
    let keys: Vec<String> = decl.jobs.iter().map(|job| job.key.clone()).collect();
    for job in decl.jobs {
        let this = job_id(&job.key);
        let index = job.props.get("index").cloned();
        out.nodes
            .push(node(&this, "PipelineJob", job.props, &[job.span]));
        out.edges
            .push(part_of(&this, &pipeline_id, index.as_ref(), job.span));
        for step in job.steps {
            let step_id = format!("pipeline-step:{}@{rel}#{}", id.repo, step.key);
            let index = step.props.get("index").cloned();
            out.nodes
                .push(node(&step_id, "PipelineStep", step.props, &[step.span]));
            out.edges
                .push(part_of(&step_id, &this, index.as_ref(), step.span));
        }
        // One edge per needed job: the store keys an edge by its ends and
        // label, so every declaration of the same dependency is cited on it.
        let mut resolved: BTreeMap<usize, (BTreeSet<&'static str>, BTreeSet<Span>)> =
            BTreeMap::new();
        let mut dangling: BTreeMap<String, BTreeSet<Span>> = BTreeMap::new();
        for need in job.needs {
            match need.target {
                Some(target) => {
                    let (via, spans) = resolved.entry(target).or_default();
                    via.insert(need.via);
                    spans.insert(need.span);
                }
                None => {
                    dangling.entry(need.name).or_default().insert(need.span);
                }
            }
        }
        for (target, (via, spans)) in resolved {
            let dst = job_id(&keys[target]);
            let spans: Vec<Span> = spans.into_iter().collect();
            let mut props = Props::new();
            props.insert("via".into(), via.into_iter().collect::<Vec<_>>().into());
            out.edges.push(Edge {
                props: seal(
                    props,
                    ConfidenceTier::Confirmed,
                    &spans,
                    &format!("{this} NEEDS {dst}"),
                ),
                src: this.clone(),
                dst,
                label: "NEEDS".into(),
            });
        }
        // R-INT-4: a `needs` naming no declared job is an explicit Gap, never
        // a silently dropped or guessed dependency.
        for (name, spans) in dangling {
            const REASON: &str = "`needs` names a job this workflow does not declare";
            let gap_id = format!("gap:pipeline:{}@{rel}#{}/needs/{name}", id.repo, job.key);
            let spans: Vec<Span> = spans.into_iter().collect();
            let mut props = Props::new();
            props.insert("reason".into(), REASON.into());
            props.insert("needs".into(), name.into());
            props.insert("attempted_tiers".into(), serde_json::json!(["T0"]));
            out.nodes.push(Node {
                id: gap_id.clone(),
                label: "Gap".into(),
                props: seal(props, ConfidenceTier::Gap, &spans, &gap_id),
            });
            let mut props = Props::new();
            props.insert("reason".into(), REASON.into());
            props.insert("via".into(), serde_json::json!(["needs"]));
            out.edges.push(Edge {
                props: seal(
                    props,
                    ConfidenceTier::Gap,
                    &spans,
                    &format!("{this} NEEDS {gap_id}"),
                ),
                src: this.clone(),
                dst: gap_id,
                label: "NEEDS".into(),
            });
        }
    }
}

#[cfg(test)]
mod tests;
