//! Scheduled jobs (#770): declared schedule files become `Schedule` flow
//! triggers with an `ENTRY` into the file each job runs.
//!
//! The first reader is the yacron format (`jobs:` as a sequence of
//! `{name, command, schedule, …}` mappings). A job's entry is resolved only
//! from what the repository proves:
//!
//! - an absolute script path under the container directory that the root
//!   `Dockerfile`'s final stage copies the repository into (`COPY . .` at a
//!   `WORKDIR`) is `Confirmed`, citing both files;
//! - a relative path is resolved against that same directory when the
//!   mapping exists (`Confirmed`), otherwise against the repository root
//!   (`InferredStrong`, marked `entry_relative_to_repo_root`);
//! - an absolute path matched only by a unique repository suffix is
//!   `InferredStrong`, marked `entry_path_suffix`;
//! - anything else (a shell-composed command, an unknown file, an
//!   ambiguous suffix, a YAML alias or merge key) is a `Gap` naming the job.
//!
//! This tier never calls an LLM.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

/// Extractor id recorded on every schedule fact.
pub const SCHEDULE_EXTRACTOR_ID: &str = "t0.schedule";

/// Repository identity for schedule facts.
#[derive(Debug, Clone, Copy)]
pub struct SourceId<'a> {
    /// Repository identity (`owner/name` or `local/<name>`).
    pub repo: &'a str,
    /// Commit the tree was read at.
    pub commit: &'a str,
}

/// Facts from one directory walk.
#[derive(Debug, Clone, Default)]
pub struct ScheduleExtraction {
    /// `Schedule` triggers, the entry `File`s they run and their `Gap`s.
    pub nodes: Vec<Node>,
    /// `ENTRY` edges (schedule → file or gap).
    pub edges: Vec<Edge>,
    /// Schedule files that declared at least one job.
    pub files: u64,
}

/// Where the repository root sits inside the container, as the root
/// `Dockerfile` proves it: the directory and the instruction spans.
#[derive(Debug, Clone, PartialEq)]
struct ContainerRoot {
    dir: String,
    spans: Vec<(u64, u64)>,
}

/// One job read from a schedule file.
#[derive(Debug, Clone, PartialEq)]
struct Job {
    name: Option<String>,
    schedule: Option<String>,
    command: Option<String>,
    span: (u64, u64),
    /// Why the job's fields cannot be trusted as written.
    unreadable: Option<&'static str>,
}

/// How a job's entry resolved.
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    File {
        rel: String,
        confidence: ConfidenceTier,
        marker: Option<&'static str>,
        via_container: bool,
    },
    Gap(&'static str),
}

/// Walk `root` and turn every yacron-format schedule file into facts.
pub fn extract_dir(root: &Path, id: &SourceId) -> std::io::Result<ScheduleExtraction> {
    let walked = source_walk::files(
        root,
        &crate::preflight::skip_dir,
        source_walk::Gitignores::Honor,
    )?;
    let files: BTreeSet<String> = walked.iter().map(|file| file.rel.clone()).collect();
    let container = std::fs::read_to_string(root.join("Dockerfile"))
        .ok()
        .and_then(|text| container_root(&text));
    let mut out = ScheduleExtraction::default();
    let mut emitted_files = BTreeSet::new();
    for file in &walked {
        if !(file.name.ends_with(".yml") || file.name.ends_with(".yaml")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file.path) else {
            continue;
        };
        let jobs = yacron_jobs(&text);
        if jobs.is_empty() {
            continue;
        }
        out.files += 1;
        for (index, job) in jobs.iter().enumerate() {
            emit(
                id,
                &file.rel,
                index,
                job,
                &files,
                container.as_ref(),
                &mut emitted_files,
                &mut out,
            );
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn emit(
    id: &SourceId,
    rel: &str,
    index: usize,
    job: &Job,
    files: &BTreeSet<String>,
    container: Option<&ContainerRoot>,
    emitted_files: &mut BTreeSet<String>,
    out: &mut ScheduleExtraction,
) {
    let evidence = |path: &str, (start, end): (u64, u64)| EvidenceRef {
        repo: id.repo.to_string(),
        path: path.to_string(),
        byte_start: start,
        byte_end: end,
        commit_sha: id.commit.to_string(),
    };
    let prov = |confidence, spans: Vec<EvidenceRef>, identity: &str| {
        serde_json::to_value(
            Provenance::new(
                Tier::Deterministic,
                confidence,
                spans,
                SCHEDULE_EXTRACTOR_ID,
                identity.as_bytes(),
            )
            .expect("within ceiling"),
        )
        .expect("serializes")
    };
    let schedule_id = format!("sched:{}@{rel}#{index}", id.repo);
    let display = job
        .name
        .clone()
        .unwrap_or_else(|| format!("{rel} job {}", index + 1));
    out.nodes.push(Node {
        id: schedule_id.clone(),
        label: "Schedule".into(),
        props: serde_json::json!({
            "name": display,
            "schedule": job.schedule,
            "command": job.command,
            "file": rel,
            "format": "yacron",
            "prov": prov(ConfidenceTier::Confirmed, vec![evidence(rel, job.span)], &schedule_id),
        }),
    });
    let entry = match (job.unreadable, job.command.as_deref()) {
        (Some(reason), _) => Entry::Gap(reason),
        (None, None) => Entry::Gap("schedule job without a literal command"),
        (None, Some(command)) => resolve_entry(command, files, container),
    };
    match entry {
        Entry::File {
            rel: entry_rel,
            confidence,
            marker,
            via_container,
        } => {
            let file_id = format!("file:{}@{entry_rel}", id.repo);
            let mut spans = vec![evidence(rel, job.span)];
            if via_container && let Some(container) = container {
                spans.extend(
                    container
                        .spans
                        .iter()
                        .map(|span| evidence("Dockerfile", *span)),
                );
            }
            if emitted_files.insert(file_id.clone()) {
                // An adapter that owns this file supersedes this node.
                out.nodes.push(Node {
                    id: file_id.clone(),
                    label: "File".into(),
                    props: serde_json::json!({
                        "path": entry_rel,
                        "prov": prov(ConfidenceTier::Confirmed, spans.clone(), &file_id),
                    }),
                });
            }
            let identity = format!("{schedule_id} ENTRY {file_id}");
            let mut props = serde_json::json!({
                "command": job.command,
                "prov": prov(confidence, spans, &identity),
            });
            if let Some(marker) = marker {
                props[marker] = true.into();
            }
            if via_container && let Some(container) = container {
                props["container_dir"] = container.dir.clone().into();
            }
            out.edges.push(Edge {
                src: schedule_id,
                dst: file_id,
                label: "ENTRY".into(),
                props,
            });
        }
        Entry::Gap(reason) => {
            let gap_id = format!("gap:schedule:{}@{rel}#{index}", id.repo);
            let spans = vec![evidence(rel, job.span)];
            out.nodes.push(Node {
                id: gap_id.clone(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "reason": reason,
                    "command": job.command,
                    "attempted_tiers": ["T0"],
                    "prov": prov(ConfidenceTier::Gap, spans.clone(), &gap_id),
                }),
            });
            let identity = format!("{schedule_id} ENTRY {gap_id}");
            out.edges.push(Edge {
                src: schedule_id,
                dst: gap_id,
                label: "ENTRY".into(),
                props: serde_json::json!({
                    "reason": reason,
                    "prov": prov(ConfidenceTier::Gap, spans, &identity),
                }),
            });
        }
    }
}

/// The repository path a job's command runs, or why it can't be proven.
fn resolve_entry(
    command: &str,
    files: &BTreeSet<String>,
    container: Option<&ContainerRoot>,
) -> Entry {
    if command.chars().any(|c| {
        matches!(
            c,
            '|' | '&' | ';' | '<' | '>' | '$' | '`' | '(' | ')' | '\'' | '"' | '\\' | '*' | '?'
        )
    }) {
        return Entry::Gap("shell-composed schedule command");
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    let Some(script) = script_of(&words) else {
        return Entry::Gap("schedule command runs no script file");
    };
    let candidates: Vec<String> = match script {
        Script::Path(path) => vec![path.to_string()],
        Script::Module(module) => {
            let base = module.replace('.', "/");
            vec![format!("{base}.py"), format!("{base}/__main__.py")]
        }
    };
    let exists = |rel: &str| files.contains(rel).then(|| rel.to_string());
    let mut resolved = Vec::new();
    for candidate in &candidates {
        if let Some(rest) = candidate.strip_prefix('/') {
            if let Some(container) = container
                && let Some(rel) = rest.strip_prefix(container.dir.trim_start_matches('/'))
                && let Some(rel) = rel.strip_prefix('/')
                && let Some(rel) = exists(rel)
            {
                resolved.push(Entry::File {
                    rel,
                    confidence: ConfidenceTier::Confirmed,
                    marker: None,
                    via_container: true,
                });
            }
        } else {
            let relative = candidate.trim_start_matches("./");
            if let Some(rel) = exists(relative) {
                resolved.push(match container {
                    Some(_) => Entry::File {
                        rel,
                        confidence: ConfidenceTier::Confirmed,
                        marker: None,
                        via_container: true,
                    },
                    None => Entry::File {
                        rel,
                        confidence: ConfidenceTier::InferredStrong,
                        marker: Some("entry_relative_to_repo_root"),
                        via_container: false,
                    },
                });
            }
        }
    }
    if let Some(entry) = resolved.into_iter().next() {
        return entry;
    }
    // An absolute path no container mapping proves: a unique suffix only.
    if let Script::Path(path) = script
        && path.starts_with('/')
    {
        let matches: Vec<&String> = files
            .iter()
            .filter(|rel| path.ends_with(&format!("/{rel}")))
            .collect();
        return match matches.as_slice() {
            [rel] => Entry::File {
                rel: (*rel).clone(),
                confidence: ConfidenceTier::InferredStrong,
                marker: Some("entry_path_suffix"),
                via_container: false,
            },
            [] => Entry::Gap("schedule entry not found in source tree"),
            _ => Entry::Gap("schedule entry matches several files"),
        };
    }
    Entry::Gap("schedule entry not found in source tree")
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Script<'a> {
    Path(&'a str),
    Module(&'a str),
}

/// The script a command runs: `python[3[.x]] [-u…] path`, `python -m mod`,
/// `node|bash|sh|ruby|perl path`, or a path executed directly.
fn script_of<'a>(words: &[&'a str]) -> Option<Script<'a>> {
    let (first, rest) = words.split_first()?;
    let program = first.rsplit('/').next().unwrap_or(first);
    let python = program
        .strip_prefix("python")
        .is_some_and(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'));
    if python || matches!(program, "node" | "bash" | "sh" | "ruby" | "perl") {
        let mut args = rest.iter();
        while let Some(arg) = args.next() {
            if python && *arg == "-m" {
                return args.next().map(|module| Script::Module(module));
            }
            if !arg.starts_with('-') {
                return Some(Script::Path(arg));
            }
        }
        return None;
    }
    // A bare program name is looked up on PATH, not in the repository.
    first.contains('/').then_some(Script::Path(first))
}

/// The container directory the final stage copies the repository into.
fn container_root(text: &str) -> Option<ContainerRoot> {
    let mut workdir: Option<(String, (u64, u64))> = None;
    let mut found: Vec<ContainerRoot> = Vec::new();
    let mut offset = 0u64;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len() as u64;
        let trimmed = line.trim();
        let span = (start, start + line.trim_end().len() as u64);
        let mut words = trimmed.split_whitespace();
        let Some(instruction) = words.next() else {
            continue;
        };
        let args: Vec<&str> = words.collect();
        match instruction.to_ascii_uppercase().as_str() {
            "FROM" => {
                workdir = None;
                found.clear();
            }
            "WORKDIR" => {
                let [dir] = args.as_slice() else {
                    workdir = None;
                    continue;
                };
                workdir = match (&workdir, dir.starts_with('/')) {
                    (_, true) => Some((dir.trim_end_matches('/').to_string(), span)),
                    (Some((base, _)), false) => {
                        Some((format!("{base}/{}", dir.trim_end_matches('/')), span))
                    }
                    (None, false) => None,
                };
            }
            "COPY" | "ADD" => {
                if args.iter().any(|arg| arg.starts_with("--from")) || trimmed.ends_with('\\') {
                    continue;
                }
                let paths: Vec<&str> = args
                    .iter()
                    .copied()
                    .filter(|a| !a.starts_with("--"))
                    .collect();
                let [source, destination] = paths.as_slice() else {
                    continue;
                };
                if !matches!(*source, "." | "./") {
                    continue;
                }
                let dir = match (*destination, &workdir) {
                    ("." | "./", Some((dir, _))) => dir.clone(),
                    (dest, _) if dest.starts_with('/') => dest.trim_end_matches('/').to_string(),
                    _ => continue,
                };
                let mut spans: Vec<(u64, u64)> = workdir.iter().map(|(_, span)| *span).collect();
                spans.push(span);
                found.push(ContainerRoot { dir, spans });
            }
            _ => {}
        }
    }
    // Two whole-repository copies in the final stage prove no single root.
    match found.as_slice() {
        [root] if !root.dir.is_empty() => Some(root.clone()),
        _ => None,
    }
}

/// Jobs of a yacron-format file: top-level `jobs` is a block sequence of
/// mappings, at least one with a `schedule`. Anything else yields none.
fn yacron_jobs(text: &str) -> Vec<Job> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    if root.has_error() {
        return Vec::new();
    }
    let Some(top) = first_mapping(root) else {
        return Vec::new();
    };
    let Some(sequence) = pairs(top)
        .into_iter()
        .find(|(key, _)| scalar(text, *key).as_deref() == Some("jobs"))
        .and_then(|(_, value)| descend(value, "block_sequence"))
    else {
        return Vec::new();
    };
    let mut walk = sequence.walk();
    let items: Vec<TsNode> = sequence
        .named_children(&mut walk)
        .filter(|item| item.kind() == "block_sequence_item")
        .collect();
    let jobs: Vec<Job> = items
        .into_iter()
        .filter_map(|item| job(text, item))
        .collect();
    if jobs.iter().any(|job| job.schedule.is_some()) {
        jobs
    } else {
        Vec::new()
    }
}

fn job(text: &str, item: TsNode) -> Option<Job> {
    let mapping = descend(item, "block_mapping")?;
    let mut fields: BTreeMap<String, TsNode> = BTreeMap::new();
    let mut unreadable = None;
    for (key, value) in pairs(mapping) {
        match scalar(text, key) {
            Some(name) if name == "<<" => unreadable = Some("schedule job uses a YAML merge key"),
            Some(name) => {
                fields.insert(name, value);
            }
            None => unreadable = Some("schedule job has a computed key"),
        }
    }
    let field = |name: &str| fields.get(name).copied();
    if [field("name"), field("command"), field("schedule")]
        .into_iter()
        .flatten()
        .any(|value| contains_kind(value, &["alias", "anchor", "tag"]))
    {
        unreadable = Some("schedule job uses a YAML alias, anchor or tag");
    }
    let text_of = |node: TsNode| text[node.byte_range()].trim().to_string();
    Some(Job {
        name: field("name").and_then(|value| scalar(text, value)),
        // A mapping schedule (`{minute: …}`) keeps its source text.
        schedule: field("schedule")
            .map(|value| scalar(text, value).unwrap_or_else(|| text_of(value))),
        command: field("command").and_then(|value| scalar(text, value)),
        span: (item.start_byte() as u64, item.end_byte() as u64),
        unreadable,
    })
}

fn first_mapping(node: TsNode) -> Option<TsNode> {
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        match child.kind() {
            "block_mapping" => return Some(child),
            "document" | "block_node" | "stream" => {
                if let Some(found) = first_mapping(child) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

/// The first `kind` beneath `node` through `block_node` wrappers only.
fn descend<'t>(node: TsNode<'t>, kind: &str) -> Option<TsNode<'t>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut walk = node.walk();
    let children: Vec<TsNode<'t>> = node.named_children(&mut walk).collect();
    children.into_iter().find_map(|child| match child.kind() {
        k if k == kind => Some(child),
        "block_node" => descend(child, kind),
        _ => None,
    })
}

fn pairs(mapping: TsNode) -> Vec<(TsNode, TsNode)> {
    let mut walk = mapping.walk();
    mapping
        .named_children(&mut walk)
        .filter(|pair| pair.kind() == "block_mapping_pair")
        .filter_map(|pair| {
            Some((
                pair.child_by_field_name("key")?,
                pair.child_by_field_name("value")?,
            ))
        })
        .collect()
}

fn contains_kind(node: TsNode, kinds: &[&str]) -> bool {
    if kinds.contains(&node.kind()) {
        return true;
    }
    let mut walk = node.walk();
    let children: Vec<TsNode> = node.named_children(&mut walk).collect();
    children
        .into_iter()
        .any(|child| contains_kind(child, kinds))
}

/// A plain or quoted scalar's value; `None` for anything else, including a
/// double-quoted scalar with escapes.
fn scalar(text: &str, node: TsNode) -> Option<String> {
    let mut node = node;
    loop {
        match node.kind() {
            "flow_node" | "block_node" => {
                let mut walk = node.walk();
                let children: Vec<TsNode> = node.named_children(&mut walk).collect();
                let [only] = children.as_slice() else {
                    return None;
                };
                node = *only;
            }
            "plain_scalar" | "string_scalar" => {
                return Some(text[node.byte_range()].trim().to_string());
            }
            "double_quote_scalar" => {
                let raw = &text[node.byte_range()];
                let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
                return (!inner.contains('\\')).then(|| inner.to_string());
            }
            "single_quote_scalar" => {
                let raw = &text[node.byte_range()];
                let inner = raw.strip_prefix('\'')?.strip_suffix('\'')?;
                return Some(inner.replace("''", "'"));
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests;
