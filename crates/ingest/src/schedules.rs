//! Scheduled jobs (#770): declared schedule files become `Schedule` flow
//! triggers with an `ENTRY` into the file each job runs.
//!
//! Three readers share one node shape: the yacron format (`jobs:` as a
//! sequence of `{name, command, schedule, …}` mappings), crontab(5) files
//! (`crontab`, `*.crontab`, `*.cron`, and files directly in a `cron.d`
//! directory, whose lines carry a user field), and Kubernetes `CronJob`
//! documents (one job per container, `command` then `args`). A job's entry
//! is resolved only from what the repository proves:
//!
//! - a script path, relative ones joined to the final stage's proven
//!   `WORKDIR`, that lies under the directory the root `Dockerfile`'s final
//!   stage copies the repository into (`COPY . <dir>`) is `Confirmed`,
//!   citing both files, unless a later `COPY`/`ADD`/`RUN` in that stage may
//!   replace it (`InferredStrong`, marked `entry_may_be_replaced_in_image`),
//!   or the job is a `CronJob`, whose image is only assumed to be that
//!   build (`InferredStrong`, marked `entry_image_assumed_from_dockerfile`);
//! - a relative path without a proven working directory resolves against
//!   the repository root (`InferredStrong`, marked
//!   `entry_relative_to_repo_root`);
//! - an absolute path matched only by a unique repository suffix is
//!   `InferredStrong`, marked `entry_path_suffix`;
//! - anything else (a shell-composed command, inline code, an interpreter
//!   option outside the modeled grammar, an unknown file, an ambiguous
//!   suffix, a YAML alias or merge key) is a `Gap` naming the job.
//!
//! This tier never calls an LLM.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

mod dispatch;

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
    /// The final stage's working directory, when proven, and its `WORKDIR`.
    cwd: Option<(String, (u64, u64))>,
    /// Later instructions that may replace copied files: the destination
    /// they write, or `None` when it could be anywhere (`RUN`).
    writes: Vec<(Option<String>, (u64, u64))>,
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
    /// The schedule file format (`yacron`, `crontab`, `kubernetes`,
    /// `github-actions`).
    format: &'static str,
}

/// A scheduled entry's argument dispatch: the selected function with the
/// repository file that defines it, or why its options select none.
type Dispatched = Result<(dispatch::Dispatch, String), &'static str>;

/// How a job's entry resolved.
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    File {
        rel: String,
        confidence: ConfidenceTier,
        marker: Option<&'static str>,
        /// `Dockerfile` instruction spans the entry rests on.
        dockerfile: Vec<(u64, u64)>,
        container_dir: Option<String>,
    },
    Gap(&'static str),
}

/// Walk `root` and turn every schedule file (yacron, crontab, `CronJob`)
/// into facts. `symbols` holds the `Symbol` ids the language adapters
/// emitted: a Python entry whose argument dispatch is proven enters the
/// selected function when it is among them, and the file otherwise.
pub fn extract_dir(
    root: &Path,
    id: &SourceId,
    symbols: &BTreeSet<String>,
) -> std::io::Result<ScheduleExtraction> {
    let walked = crate::captured_walk::walk(root)?;
    let text = |rel: &str| std::fs::read_to_string(root.join(rel)).ok();
    Ok(extract_walked(&walked, &text, id, symbols))
}

/// True when the schedule reader may open `rel` and a capture retains it: a
/// YAML file (a yacron file, a workflow or a `CronJob` manifest) other than
/// an env file, a crontab, a Python script a job runs or imports, or the
/// root `Dockerfile`.
pub fn is_schedule_file(rel: &str) -> bool {
    // An env file holds secrets: no capture retains it (AC-0097).
    !crate::toolchain::is_env_name(rel.rsplit('/').next().unwrap_or(rel)) && is_read_file(rel)
}

fn is_read_file(rel: &str) -> bool {
    rel.ends_with(".yml")
        || rel.ends_with(".yaml")
        || crontab_kind(rel).is_some()
        || rel.ends_with(".py")
        || rel == "Dockerfile"
}

/// The files [`extract_dir`] may open or cite under `root`, in path order: a
/// source capture retains them (AC-1061).
pub fn schedule_paths(root: &Path) -> std::io::Result<Vec<String>> {
    crate::captured_walk::selected(root, is_schedule_file, || {
        let id = SourceId {
            repo: "selection",
            commit: "",
        };
        let out = extract_dir(root, &id, &BTreeSet::new())?;
        Ok((out.nodes, out.edges))
    })
}

/// [`extract_dir`] over a source capture (AC-1061, #385): `captured` is
/// every path the capture holds and `read` its bytes. The checkout is
/// neither listed nor read.
pub fn extract_from_capture(
    root: &Path,
    captured: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    id: &SourceId,
    symbols: &BTreeSet<String>,
) -> std::io::Result<ScheduleExtraction> {
    let walked = crate::captured_walk::listing(root, captured);
    let text = |rel: &str| {
        read(rel)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };
    Ok(extract_walked(&walked, &text, id, symbols))
}

fn extract_walked(
    walked: &[source_walk::WalkedFile],
    text: &dyn Fn(&str) -> Option<String>,
    id: &SourceId,
    symbols: &BTreeSet<String>,
) -> ScheduleExtraction {
    let files: BTreeSet<String> = walked.iter().map(|file| file.rel.clone()).collect();
    let container = text("Dockerfile").and_then(|text| container_root(&text));
    let mut out = ScheduleExtraction::default();
    let mut emitted_files = BTreeSet::new();
    let mut sources: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut dispatch_of = |rel: &str, command: &str| -> Option<Dispatched> {
        if !rel.ends_with(".py") {
            return None;
        }
        let args = script_args(command)?;
        let mut read = |rel: &str| {
            sources
                .entry(rel.to_string())
                .or_insert_with(|| text(rel))
                .clone()
        };
        let Some(source) = read(rel) else {
            return args
                .iter()
                .any(|arg| arg.starts_with('-'))
                .then_some(Err(dispatch::UNPROVEN));
        };
        Some(dispatch::dispatch(&source, &args)?.and_then(|found| {
            // `import argparse` / `asyncio` must load the standard library,
            // not a repository module beside the script (#804 review).
            let stdlib = ["argparse"]
                .into_iter()
                .chain(found.awaited.then_some("asyncio"));
            if stdlib
                .into_iter()
                .any(|module| shadows(rel, module, &files))
            {
                return Err("standard-library module shadowed by a repository file");
            }
            // An imported function is the one its module file defines.
            let file = match &found.import {
                None => rel.to_string(),
                Some((module, _)) => {
                    let file = module_file(rel, module, &files)
                        .ok_or("imported module is not one repository file")?;
                    read(&file)
                        .is_some_and(|text| {
                            dispatch::defines(&text, &found.function, found.awaited)
                        })
                        .then_some(file)
                        .ok_or("imported function is not one top-level def the call runs")?
                }
            };
            symbols
                .contains(&format!("sym:{}@{file}#{}", id.repo, found.function))
                .then_some((found, file))
                .ok_or("dispatched function not emitted by the Python adapter")
        }))
    };
    for file in walked {
        let jobs = if file.name.ends_with(".yml") || file.name.ends_with(".yaml") {
            let Some(text) = text(&file.rel) else {
                continue;
            };
            if workflow_file(&file.rel) {
                actions_jobs(&text)
            } else {
                let jobs = yacron_jobs(&text);
                if jobs.is_empty() {
                    cronjob_jobs(&text)
                } else {
                    jobs
                }
            }
        } else if let Some(system) = crontab_kind(&file.rel) {
            let Some(text) = text(&file.rel) else {
                continue;
            };
            crontab_jobs(&text, system)
        } else {
            continue;
        };
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
                &mut dispatch_of,
                &mut emitted_files,
                &mut out,
            );
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn emit(
    id: &SourceId,
    rel: &str,
    index: usize,
    job: &Job,
    files: &BTreeSet<String>,
    container: Option<&ContainerRoot>,
    dispatch_of: &mut dyn FnMut(&str, &str) -> Option<Dispatched>,
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
    // The content hash covers the fact's properties as well as its identity,
    // so an edited cron, command or entry changes the graph fingerprint.
    let seal =
        |mut props: serde_json::Value, confidence, spans: Vec<EvidenceRef>, identity: &str| {
            let input =
                serde_json::to_vec(&serde_json::json!([identity, &props])).expect("serializes");
            props["prov"] = serde_json::to_value(
                Provenance::new(
                    Tier::Deterministic,
                    confidence,
                    spans,
                    SCHEDULE_EXTRACTOR_ID,
                    &input,
                )
                .expect("within ceiling"),
            )
            .expect("serializes");
            props
        };
    let schedule_id = format!("sched:{}@{rel}#{index}", id.repo);
    let display = job
        .name
        .clone()
        .unwrap_or_else(|| format!("{rel} job {}", index + 1));
    out.nodes.push(Node {
        id: schedule_id.clone(),
        label: "Schedule".into(),
        props: seal(
            serde_json::json!({
                "name": display,
                "schedule": job.schedule,
                "command": job.command,
                "file": rel,
                "format": job.format,
            }),
            ConfidenceTier::Confirmed,
            vec![evidence(rel, job.span)],
            &schedule_id,
        ),
    });
    let entry = match (job.unreadable, job.command.as_deref()) {
        (Some(reason), _) => Entry::Gap(reason),
        // A scheduled workflow runs the workflow itself; its steps are not
        // traced through here.
        (None, _) if job.format == "github-actions" => Entry::File {
            rel: rel.to_string(),
            confidence: ConfidenceTier::Confirmed,
            marker: None,
            dockerfile: Vec::new(),
            container_dir: None,
        },
        (None, None) => Entry::Gap("schedule job without a literal command"),
        (None, Some(command)) => resolve_entry(command, files, container),
    };
    match entry {
        Entry::File {
            rel: entry_rel,
            confidence,
            marker,
            dockerfile,
            container_dir,
        } => {
            let file_id = format!("file:{}@{entry_rel}", id.repo);
            let mut spans = vec![evidence(rel, job.span)];
            spans.extend(dockerfile.iter().map(|span| evidence("Dockerfile", *span)));
            if emitted_files.insert(file_id.clone()) {
                // An adapter that owns this file supersedes this node.
                out.nodes.push(Node {
                    id: file_id.clone(),
                    label: "File".into(),
                    props: seal(
                        serde_json::json!({ "path": entry_rel }),
                        ConfidenceTier::Confirmed,
                        spans.clone(),
                        &file_id,
                    ),
                });
            }
            // A proven argument dispatch enters the selected function, as a
            // static reading of the branch (InferredStrong); an unproven one
            // keeps the file and names why.
            let dispatched = job
                .command
                .as_deref()
                .and_then(|command| dispatch_of(&entry_rel, command));
            let dst = match &dispatched {
                Some(Ok((found, file))) => {
                    spans.push(evidence(&entry_rel, found.span));
                    if let Some((_, span)) = &found.import {
                        spans.push(evidence(&entry_rel, *span));
                    }
                    format!("sym:{}@{file}#{}", id.repo, found.function)
                }
                _ => file_id,
            };
            let identity = format!("{schedule_id} ENTRY {dst}");
            let mut props = serde_json::json!({ "command": job.command });
            let confidence = match dispatched {
                Some(Ok((found, file))) => {
                    props["dispatch"] = serde_json::json!({
                        "option": found.option,
                        "value": found.value,
                        "function": found.function,
                        "file": file,
                    });
                    if let Some((module, _)) = found.import {
                        props["dispatch"]["module"] = module.into();
                    }
                    props["entry_argument_dispatch"] = true.into();
                    ConfidenceTier::InferredStrong
                }
                Some(Err(reason)) => {
                    props["dispatch_unresolved"] = reason.into();
                    confidence
                }
                None => confidence,
            };
            if let Some(marker) = marker {
                props[marker] = true.into();
            }
            // A CronJob names a registry image: that it was built from the
            // root `Dockerfile` is an assumption, not a proof.
            let confidence = if job.format == "kubernetes" && container_dir.is_some() {
                props["entry_image_assumed_from_dockerfile"] = true.into();
                ConfidenceTier::InferredStrong
            } else {
                confidence
            };
            if let Some(dir) = container_dir {
                props["container_dir"] = dir.into();
            }
            out.edges.push(Edge {
                src: schedule_id,
                dst,
                label: "ENTRY".into(),
                props: seal(props, confidence, spans, &identity),
            });
        }
        Entry::Gap(reason) => {
            let gap_id = format!("gap:schedule:{}@{rel}#{index}", id.repo);
            let spans = vec![evidence(rel, job.span)];
            out.nodes.push(Node {
                id: gap_id.clone(),
                label: "Gap".into(),
                props: seal(
                    serde_json::json!({
                        "reason": reason,
                        "command": job.command,
                        "attempted_tiers": ["T0"],
                    }),
                    ConfidenceTier::Gap,
                    spans.clone(),
                    &gap_id,
                ),
            });
            let identity = format!("{schedule_id} ENTRY {gap_id}");
            out.edges.push(Edge {
                src: schedule_id,
                dst: gap_id,
                label: "ENTRY".into(),
                props: seal(
                    serde_json::json!({ "reason": reason }),
                    ConfidenceTier::Gap,
                    spans,
                    &identity,
                ),
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
    let script = match script_of(&words) {
        Ok(script) => script,
        Err(reason) => return Entry::Gap(reason),
    };
    let candidates: Vec<String> = match script {
        Script::Path(path) => vec![path.to_string()],
        Script::Module(module) => {
            let base = module.replace('.', "/");
            vec![format!("{base}.py"), format!("{base}/__main__.py")]
        }
    };
    let exists = |rel: &str| files.contains(rel).then(|| rel.to_string());
    // A relative path runs from the final working directory, when proven.
    let cwd = container.and_then(|container| container.cwd.as_ref());
    let resolve = |candidate: &String| -> Option<Entry> {
        let relative = candidate.trim_start_matches("./");
        let (absolute, cwd_span) = match (candidate.starts_with('/'), cwd) {
            (true, _) => (normalize(candidate), None),
            (false, Some((dir, span))) => (normalize(&format!("{dir}/{relative}")), Some(*span)),
            (false, None) => {
                let rel = exists(relative)?;
                return Some(Entry::File {
                    rel,
                    confidence: ConfidenceTier::InferredStrong,
                    marker: Some("entry_relative_to_repo_root"),
                    dockerfile: Vec::new(),
                    container_dir: None,
                });
            }
        };
        let container = container?;
        let rel = absolute
            .strip_prefix(&format!("{}/", container.dir))
            .and_then(exists)?;
        let mut dockerfile = container.spans.clone();
        dockerfile.extend(cwd_span.filter(|span| !container.spans.contains(span)));
        let replaced: Vec<(u64, u64)> = container
            .writes
            .iter()
            .filter(|(dest, _)| {
                dest.as_deref().is_none_or(|dest| {
                    absolute == dest || absolute.starts_with(&format!("{dest}/"))
                })
            })
            .map(|(_, span)| *span)
            .collect();
        let (confidence, marker) = if replaced.is_empty() {
            (ConfidenceTier::Confirmed, None)
        } else {
            dockerfile.extend(replaced);
            (
                ConfidenceTier::InferredStrong,
                Some("entry_may_be_replaced_in_image"),
            )
        };
        Some(Entry::File {
            rel,
            confidence,
            marker,
            dockerfile,
            container_dir: Some(container.dir.clone()),
        })
    };
    // `python -m a.b` may name `a/b.py` or `a/b/__main__.py`: both is ambiguous.
    let mut found = candidates.iter().filter_map(resolve);
    match (found.next(), found.next()) {
        (Some(entry), None) => return entry,
        (Some(_), Some(_)) => return Entry::Gap("schedule module matches several files"),
        (None, _) => {}
    }
    // A path no container mapping proves: a unique suffix only.
    let unmapped = match script {
        Script::Path(path) if path.starts_with('/') => Some(normalize(path)),
        Script::Path(path) => cwd.map(|(dir, _)| normalize(&format!("{dir}/{path}"))),
        Script::Module(_) => None,
    };
    if let Some(path) = unmapped {
        let matches: Vec<&String> = files
            .iter()
            .filter(|rel| path.ends_with(&format!("/{rel}")))
            .collect();
        return match matches.as_slice() {
            [rel] => Entry::File {
                rel: (*rel).clone(),
                confidence: ConfidenceTier::InferredStrong,
                marker: Some("entry_path_suffix"),
                dockerfile: Vec::new(),
                container_dir: None,
            },
            [] => Entry::Gap("schedule entry not found in source tree"),
            _ => Entry::Gap("schedule entry matches several files"),
        };
    }
    Entry::Gap("schedule entry not found in source tree")
}

/// The one repository file `import <module>` loads from a script at `rel`,
/// searched from the script's directory as Python's `sys.path[0]` is:
/// `<module>.py` or `<module>/__init__.py`, with no package on the way
/// shadowed by a same-named module file.
pub(crate) fn module_file(rel: &str, module: &str, files: &BTreeSet<String>) -> Option<String> {
    let dir = rel.rsplit_once('/').map_or("", |(dir, _)| dir);
    let join = |path: &str| {
        if dir.is_empty() {
            path.to_string()
        } else {
            format!("{dir}/{path}")
        }
    };
    let parts: Vec<&str> = module.split('.').collect();
    for depth in 1..parts.len() {
        if files.contains(&join(&format!("{}.py", parts[..depth].join("/")))) {
            return None;
        }
    }
    let path = parts.join("/");
    let found: Vec<String> = [format!("{path}.py"), format!("{path}/__init__.py")]
        .iter()
        .map(|candidate| join(candidate))
        .filter(|candidate| files.contains(candidate))
        .collect();
    match <[String; 1]>::try_from(found) {
        Ok([file]) => Some(file),
        Err(_) => None,
    }
}

/// Whether a repository module beside the script at `rel` takes precedence
/// over the top-level module `module`, as `sys.path[0]` does.
fn shadows(rel: &str, module: &str, files: &BTreeSet<String>) -> bool {
    let dir = rel
        .rsplit_once('/')
        .map_or(String::new(), |(dir, _)| format!("{dir}/"));
    [format!("{module}.py"), format!("{module}/__init__.py")]
        .iter()
        .any(|candidate| files.contains(&format!("{dir}{candidate}")))
}

/// The command's words after the script it runs.
fn script_args(command: &str) -> Option<Vec<&str>> {
    let words: Vec<&str> = command.split_whitespace().collect();
    let script = match script_of(&words).ok()? {
        Script::Path(word) | Script::Module(word) => word,
    };
    let at = words
        .iter()
        .position(|word| word.as_ptr() == script.as_ptr())?;
    Some(words[at + 1..].to_vec())
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Script<'a> {
    Path(&'a str),
    Module(&'a str),
}

const NO_SCRIPT: &str = "schedule command runs no script file";
const INLINE_CODE: &str = "schedule command evaluates inline code";
const UNMODELED_OPTION: &str = "schedule command uses an unmodeled interpreter option";

/// The closed argument grammar of an interpreter: anything outside it is
/// unmodeled rather than skipped, since an option may stop file execution.
struct Grammar {
    /// Single-letter options without an operand; they may be combined.
    flags: &'static str,
    /// Single-letter options that evaluate their operand as code.
    eval: &'static str,
    /// Options taking exactly one operand word.
    operand: &'static [&'static str],
    /// Long options without an operand; a trailing `=` matches any value.
    long: &'static [&'static str],
    /// Long options that evaluate their operand as code.
    eval_long: &'static [&'static str],
}

const PYTHON: Grammar = Grammar {
    flags: "uBOsSEIqbdv",
    eval: "c",
    operand: &["-W", "-X"],
    long: &[],
    eval_long: &[],
};
const NODE: Grammar = Grammar {
    flags: "",
    eval: "ep",
    operand: &[],
    long: &[
        "--enable-source-maps",
        "--no-warnings",
        "--no-deprecation",
        "--trace-warnings",
        "--max-old-space-size=",
    ],
    eval_long: &["--eval", "--print"],
};
const SHELL: Grammar = Grammar {
    flags: "euxv",
    eval: "c",
    operand: &[],
    long: &[],
    eval_long: &[],
};
const RUBY: Grammar = Grammar {
    flags: "w",
    eval: "e",
    operand: &[],
    long: &[],
    eval_long: &[],
};
const PERL: Grammar = Grammar {
    flags: "w",
    eval: "eE",
    operand: &[],
    long: &[],
    eval_long: &[],
};

/// The script a command runs: `python[3[.x]] [options] path`,
/// `python -m mod`, `node|bash|sh|ruby|perl [options] path`, or a path
/// executed directly; otherwise why no script file is proven.
fn script_of<'a>(words: &[&'a str]) -> Result<Script<'a>, &'static str> {
    let (first, rest) = words.split_first().ok_or(NO_SCRIPT)?;
    let program = first.rsplit('/').next().unwrap_or(first);
    let python = program
        .strip_prefix("python")
        .is_some_and(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'));
    let grammar = match program {
        _ if python => &PYTHON,
        "node" => &NODE,
        "bash" | "sh" => &SHELL,
        "ruby" => &RUBY,
        "perl" => &PERL,
        // A bare program name is looked up on PATH, not in the repository.
        _ if first.contains('/') => return Ok(Script::Path(first)),
        _ => return Err(NO_SCRIPT),
    };
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match *arg {
            "--" => return args.next().map(|path| Script::Path(path)).ok_or(NO_SCRIPT),
            // `-` reads the program from standard input.
            "-" => return Err(NO_SCRIPT),
            "-m" if python => {
                return args
                    .next()
                    .map(|module| Script::Module(module))
                    .ok_or(NO_SCRIPT);
            }
            _ if arg.starts_with('+') => return Err(UNMODELED_OPTION),
            _ if !arg.starts_with('-') => return Ok(Script::Path(arg)),
            _ => {}
        }
        if grammar
            .eval_long
            .iter()
            .any(|long| arg == long || arg.starts_with(&format!("{long}=")))
        {
            return Err(INLINE_CODE);
        }
        if grammar
            .long
            .iter()
            .any(|long| arg == long || (long.ends_with('=') && arg.starts_with(long)))
        {
            continue;
        }
        if grammar.operand.contains(arg) {
            args.next().ok_or(NO_SCRIPT)?;
            continue;
        }
        let letters = arg
            .strip_prefix('-')
            .filter(|letters| !letters.starts_with('-'));
        for letter in letters.ok_or(UNMODELED_OPTION)?.chars() {
            if grammar.eval.contains(letter) {
                return Err(INLINE_CODE);
            }
            if !grammar.flags.contains(letter) {
                return Err(UNMODELED_OPTION);
            }
        }
    }
    Err(NO_SCRIPT)
}

/// A `Dockerfile` path as an absolute container path, relative ones joined
/// to the working directory; `None` when it isn't a plain literal path.
fn container_path(path: &str, workdir: Option<&(String, (u64, u64))>) -> Option<String> {
    if path.contains(['"', '[', ']', '$', '*', '?']) {
        return None;
    }
    if path.starts_with('/') {
        return Some(normalize(path));
    }
    let (base, _) = workdir?;
    Some(normalize(&format!("{base}/{path}")))
}

/// An absolute container path with `.`, `..` and repeated or trailing
/// slashes resolved, so containment is a plain prefix test; `/` is `""`.
fn normalize(absolute: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in absolute.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    segments
        .iter()
        .map(|segment| format!("/{segment}"))
        .collect()
}

/// Logical `Dockerfile` instructions with their spans: continuation lines
/// joined, comment and blank lines dropped.
pub(crate) fn instructions(text: &str) -> Vec<((u64, u64), String)> {
    let mut out = Vec::new();
    let mut current: Option<(u64, String)> = None;
    let mut offset = 0u64;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len() as u64;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let end = start + line.trim_end().len() as u64;
        let (begin, mut body) = current.take().unwrap_or((start, String::new()));
        match trimmed.strip_suffix('\\') {
            Some(part) => {
                body.push_str(part);
                body.push(' ');
                current = Some((begin, body));
            }
            None => {
                body.push_str(trimmed);
                out.push(((begin, end), body));
            }
        }
    }
    if let Some((begin, body)) = current {
        out.push(((begin, offset), body));
    }
    out
}

/// The container directory the final stage copies the repository into, its
/// final working directory and the later writes that may replace files.
fn container_root(text: &str) -> Option<ContainerRoot> {
    // A changed escape character or a heredoc changes how lines parse.
    if text
        .lines()
        .any(|line| line.trim().to_ascii_lowercase().starts_with("# escape="))
    {
        return None;
    }
    let mut workdir: Option<(String, (u64, u64))> = None;
    let mut found: Vec<ContainerRoot> = Vec::new();
    let mut writes: Vec<(Option<String>, (u64, u64))> = Vec::new();
    for (span, instruction) in instructions(text) {
        if instruction.contains("<<") {
            return None;
        }
        let mut words = instruction.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        let args: Vec<&str> = words.collect();
        match keyword.to_ascii_uppercase().as_str() {
            "FROM" => {
                workdir = None;
                found.clear();
                writes.clear();
            }
            "WORKDIR" => {
                workdir = match args.as_slice() {
                    [dir] => container_path(dir, workdir.as_ref()).map(|dir| (dir, span)),
                    _ => None,
                };
            }
            "RUN" if !found.is_empty() => writes.push((None, span)),
            "COPY" | "ADD" => {
                let from = args.iter().any(|arg| arg.starts_with("--from"));
                let paths: Vec<&str> = args
                    .iter()
                    .copied()
                    .filter(|a| !a.starts_with("--"))
                    .collect();
                let destination = paths
                    .last()
                    .and_then(|dest| container_path(dest, workdir.as_ref()));
                match (paths.as_slice(), destination) {
                    ([".", _] | ["./", _], Some(dir)) if !from => {
                        let mut spans: Vec<(u64, u64)> =
                            workdir.iter().map(|(_, span)| *span).collect();
                        spans.push(span);
                        found.push(ContainerRoot {
                            dir,
                            spans,
                            cwd: None,
                            writes: Vec::new(),
                        });
                    }
                    (_, destination) if !found.is_empty() => writes.push((destination, span)),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    // Two whole-repository copies in the final stage prove no single root.
    match found.as_slice() {
        [root] if !root.dir.is_empty() => Some(ContainerRoot {
            cwd: workdir,
            writes,
            ..root.clone()
        }),
        _ => None,
    }
}

/// Jobs of a yacron-format file: top-level `jobs` is a block sequence of
/// mappings, at least one with a `schedule`. Anything else yields none.
/// Whether `rel` names a crontab(5) file, and whether its lines carry a
/// user field (`Some(true)` for a file directly in a `cron.d` directory).
fn crontab_kind(rel: &str) -> Option<bool> {
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    if dir == "cron.d" || dir.ends_with("/cron.d") {
        return (!name.starts_with('.')).then_some(true);
    }
    (name == "crontab" || name.ends_with(".crontab") || name.ends_with(".cron")).then_some(false)
}

/// The `@` schedule macros crontab(5) accepts in place of five fields.
const CRON_MACROS: &[&str] = &[
    "@reboot",
    "@yearly",
    "@annually",
    "@monthly",
    "@weekly",
    "@daily",
    "@midnight",
    "@hourly",
];

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// Crontab field `position` (minute, hour, day of month, month, day of
/// week): `*` or values and ranges within that field's bounds, `,` lists,
/// and a `/` step of at least 1 after `*` or a range, or with `start_step`
/// after a single value. Names are accepted only in the month and weekday
/// fields.
fn cron_field(field: &str, position: usize, start_step: bool) -> bool {
    let (low, high, names): (u32, u32, &[&str]) = match position {
        0 => (0, 59, &[]),
        1 => (0, 23, &[]),
        2 => (1, 31, &[]),
        3 => (1, 12, &MONTHS),
        _ => (0, 7, &WEEKDAYS),
    };
    let value = |part: &str| -> Option<u32> {
        let named = names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(part))
            .map(|index| index as u32 + low);
        named
            .or_else(|| {
                (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| part.parse().ok())
                    .flatten()
            })
            .filter(|value| (low..=high).contains(value))
    };
    field.split(',').all(|item| {
        let (base, step) = match item.split_once('/') {
            Some((base, step)) => (base, Some(step)),
            None => (item, None),
        };
        let stepped = step.is_none_or(|step| {
            !step.is_empty()
                && step.bytes().all(|b| b.is_ascii_digit())
                && step.parse::<u32>().is_ok_and(|step| step >= 1)
        });
        let ranged = match base.split_once('-') {
            _ if base == "*" => true,
            Some((from, to)) => matches!((value(from), value(to)), (Some(a), Some(b)) if a <= b),
            // `20/15`: from a start to the field's end. GitHub documents it;
            // crontab(5) does not (#799 review).
            None => (step.is_none() || start_step) && value(base).is_some(),
        };
        stepped && ranged
    })
}

/// Byte ranges of the whitespace-separated words of `text`.
fn word_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(from)) => {
                spans.push((from, at));
                start = None;
            }
            (false, None) => start = Some(at),
            _ => {}
        }
    }
    if let Some(from) = start {
        spans.push((from, text.len()));
    }
    spans
}

/// Jobs in a crontab(5) file. Blank, comment and environment
/// (`NAME = value`) lines are not jobs. A job is five time fields or an `@`
/// macro, then (with `system`) a user, then the command, kept verbatim. The
/// file declares jobs only when some line parses; then any other line is a
/// Gap rather than skipped, and a command with an unescaped `%` (stdin and
/// newline in cron) is too.
fn crontab_jobs(text: &str, system: bool) -> Vec<Job> {
    let mut jobs = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let lead = body.len() - body.trim_start().len();
        let span = ((start + lead) as u64, (start + lead + trimmed.len()) as u64);
        if let Some((name, _)) = trimmed.split_once('=')
            && !name.trim_end().is_empty()
            && name
                .trim_end()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }
        let mut job = Job {
            name: None,
            schedule: None,
            command: None,
            span,
            unreadable: Some("crontab line is not a schedule entry"),
            format: "crontab",
        };
        let words = word_spans(trimmed);
        let word = |index: usize| &trimmed[words[index].0..words[index].1];
        let fields = if CRON_MACROS.contains(&word(0)) {
            1
        } else if words.len() > 5 && (0..5).all(|index| cron_field(word(index), index, false)) {
            5
        } else {
            jobs.push(job);
            continue;
        };
        let skip = fields + usize::from(system);
        if words.len() > skip {
            job.schedule = Some(trimmed[..words[fields - 1].1].to_string());
            let command = &trimmed[words[skip].0..];
            job.unreadable = command
                .split("\\%")
                .any(|part| part.contains('%'))
                .then_some("crontab command uses % (stdin and newlines)");
            job.command = Some(command.to_string());
        }
        jobs.push(job);
    }
    if jobs.iter().any(|job| job.schedule.is_some()) {
        jobs
    } else {
        Vec::new()
    }
}

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
    // A mapping without `schedule` is no job, unless a merge key or computed
    // key could be hiding one: that stays a Gap rather than vanishing.
    if jobs.iter().any(|job| job.schedule.is_some()) {
        jobs.into_iter()
            .filter(|job| job.schedule.is_some() || job.unreadable.is_some())
            .collect()
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
        format: "yacron",
    })
}

/// Kubernetes `CronJob` documents (`apiVersion: batch/…`): one job per
/// container of `spec.jobTemplate.spec.template.spec.containers`, its
/// command the container's `command` then `args`.
fn cronjob_jobs(text: &str) -> Vec<Job> {
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
    let mut walk = root.walk();
    let documents: Vec<TsNode> = root
        .named_children(&mut walk)
        .filter(|node| node.kind() == "document")
        .collect();
    let mut jobs = Vec::new();
    for document in documents {
        let Some(top) = first_mapping(document) else {
            continue;
        };
        let named = |mapping: TsNode, name: &str| {
            value_of(text, mapping, name).and_then(|value| scalar(text, value))
        };
        if named(top, "kind").as_deref() != Some("CronJob")
            || !named(top, "apiVersion").is_some_and(|api| api.starts_with("batch/"))
        {
            continue;
        }
        let path = |names: &[&str]| {
            names.iter().try_fold(top, |mapping, name| {
                value_of(text, mapping, name).and_then(|value| descend(value, "block_mapping"))
            })
        };
        let name = path(&["metadata"]).and_then(|metadata| named(metadata, "name"));
        let spec = path(&["spec"]);
        let schedule = spec.and_then(|spec| named(spec, "schedule"));
        let span = (document.start_byte() as u64, document.end_byte() as u64);
        // A templated manifest (Helm) or a YAML alias, anchor or tag is not
        // the text the cluster runs.
        let document_unreadable = if text[document.byte_range()].contains("{{") {
            Some("CronJob manifest is templated")
        } else if contains_kind(document, &["alias", "anchor", "tag"]) {
            Some("CronJob uses a YAML alias, anchor or tag")
        } else {
            None
        };
        let containers: Vec<TsNode> = path(&["spec", "jobTemplate", "spec", "template", "spec"])
            .and_then(|pod| value_of(text, pod, "containers"))
            .and_then(|value| descend(value, "block_sequence"))
            .map(|sequence| {
                let mut walk = sequence.walk();
                sequence
                    .named_children(&mut walk)
                    .filter(|item| item.kind() == "block_sequence_item")
                    .collect()
            })
            .unwrap_or_default();
        if containers.is_empty() {
            jobs.push(Job {
                name,
                schedule,
                command: None,
                span,
                unreadable: Some("CronJob containers are not a block sequence"),
                format: "kubernetes",
            });
            continue;
        }
        let several = containers.len() > 1;
        for item in containers {
            let container = descend(item, "block_mapping");
            let field =
                |field: &str| container.and_then(|container| value_of(text, container, field));
            let (command, unreadable) = match (field("command"), field("args")) {
                (None, _) => (None, Some("CronJob container runs its image entrypoint")),
                (Some(command), args) => {
                    match [Some(command), args]
                        .into_iter()
                        .flatten()
                        .map(|list| words(text, list))
                        .collect::<Option<Vec<Vec<String>>>>()
                    {
                        // An empty `command` list leaves the image
                        // entrypoint in place, as an absent one does; an
                        // empty or spaced argument has no word-split form.
                        Some(lists) if lists[0].is_empty() => {
                            (None, Some("CronJob container runs its image entrypoint"))
                        }
                        Some(lists) => {
                            let words = lists.concat();
                            let unsplittable = words
                                .iter()
                                .any(|word| word.is_empty() || word.contains(char::is_whitespace));
                            (
                                Some(words.join(" ")),
                                unsplittable
                                    .then_some("CronJob argument is empty or contains whitespace"),
                            )
                        }
                        None => (
                            None,
                            Some("CronJob command is not a list of literal strings"),
                        ),
                    }
                }
            };
            let unreadable = document_unreadable
                .or(unreadable)
                .or(field("workingDir").map(|_| "CronJob container sets workingDir"));
            let container_name = container.and_then(|container| named(container, "name"));
            jobs.push(Job {
                name: match (&name, container_name) {
                    (Some(job), Some(container)) if several => Some(format!("{job}/{container}")),
                    (name, _) => name.clone(),
                },
                schedule: schedule.clone(),
                command,
                span,
                unreadable,
                format: "kubernetes",
            });
        }
    }
    jobs
}

/// The value of the `name` key in a block mapping.
fn value_of<'t>(text: &str, mapping: TsNode<'t>, name: &str) -> Option<TsNode<'t>> {
    pairs(mapping)
        .into_iter()
        .find(|(key, _)| scalar(text, *key).as_deref() == Some(name))
        .map(|(_, value)| value)
}

/// A GitHub Actions workflow: a YAML file directly in `.github/workflows`.
pub(crate) fn workflow_file(rel: &str) -> bool {
    rel.strip_prefix(".github/workflows/")
        .is_some_and(|name| !name.contains('/'))
}

/// The `on.schedule` entries of a GitHub Actions workflow: one job per
/// `cron`, its five fields kept verbatim and checked like crontab(5).
fn actions_jobs(text: &str) -> Vec<Job> {
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
    let name = value_of(text, top, "name").and_then(|value| scalar(text, value));
    let Some(on) = value_of(text, top, "on") else {
        return Vec::new();
    };
    let Some(schedule) = descend(on, "block_mapping").and_then(|on| value_of(text, on, "schedule"))
    else {
        return Vec::new();
    };
    // An anchor or tag on a container the crons sit in (#799 review).
    let decorated = [on, schedule].into_iter().any(|node| {
        let mut walk = node.walk();
        let wrappers: Vec<TsNode> = node.named_children(&mut walk).collect();
        node.kind() == "alias"
            || wrappers
                .iter()
                .any(|child| matches!(child.kind(), "alias" | "anchor" | "tag"))
    });
    let job = |span: (u64, u64), cron: Option<String>, unreadable| Job {
        name: name.clone(),
        schedule: cron,
        command: None,
        span,
        unreadable,
        format: "github-actions",
    };
    let Some(sequence) = descend(schedule, "block_sequence") else {
        let span = (schedule.start_byte() as u64, schedule.end_byte() as u64);
        return vec![job(
            span,
            None,
            Some("workflow schedule is not a block sequence"),
        )];
    };
    let mut walk = sequence.walk();
    let items: Vec<TsNode> = sequence
        .named_children(&mut walk)
        .filter(|item| item.kind() == "block_sequence_item")
        .collect();
    items
        .into_iter()
        .map(|item| {
            let span = (item.start_byte() as u64, item.end_byte() as u64);
            let value =
                descend(item, "block_mapping").and_then(|entry| value_of(text, entry, "cron"));
            if decorated || contains_kind(item, &["alias", "anchor", "tag"]) {
                return job(
                    span,
                    None,
                    Some("workflow cron uses a YAML alias, anchor or tag"),
                );
            }
            match value.and_then(|value| scalar(text, value)) {
                Some(cron) => {
                    let fields: Vec<&str> = cron.split_whitespace().collect();
                    let valid = fields.len() == 5
                        && fields
                            .iter()
                            .enumerate()
                            .all(|(index, field)| cron_field(field, index, true));
                    let unreadable =
                        (!valid).then_some("workflow cron is not five schedule fields");
                    job(span, Some(cron), unreadable)
                }
                None => job(
                    span,
                    None,
                    Some("workflow schedule entry without a literal cron"),
                ),
            }
        })
        .collect()
}

/// A YAML sequence of literal scalars, block or flow; `None` otherwise.
fn words(text: &str, node: TsNode) -> Option<Vec<String>> {
    let sequence = descend(node, "block_sequence").or_else(|| descend(node, "flow_sequence"))?;
    let mut walk = sequence.walk();
    let items: Vec<TsNode> = sequence.named_children(&mut walk).collect();
    items
        .into_iter()
        .filter(|item| item.kind() != "comment")
        .map(|item| {
            let value = match item.kind() {
                "block_sequence_item" => {
                    let mut walk = item.walk();
                    let children: Vec<TsNode> = item.named_children(&mut walk).collect();
                    *children.first()?
                }
                _ => item,
            };
            scalar(text, value)
        })
        .collect()
}

pub(crate) fn first_mapping(node: TsNode) -> Option<TsNode> {
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

pub(crate) fn contains_kind(node: TsNode, kinds: &[&str]) -> bool {
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
pub(crate) fn scalar(text: &str, node: TsNode) -> Option<String> {
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
