//! Entry points beyond HTTP (#1997): every way a process of the system
//! starts becomes an `EntryPoint` flow trigger with an `ENTRY` into the
//! file (or the declared function) it runs, or a `Gap` that names it.
//!
//! Five readers share one node shape:
//!
//! - a Python module with a top-level `if __name__ == "__main__":` block,
//!   or a script under an operator directory (`commands/`, `scripts/`,
//!   `bin/`, `tools/`, `management/commands/`) or behind a `python` shebang
//!   that runs statements at module level, is a process start (`Confirmed`);
//! - the final stage's `CMD` / `ENTRYPOINT` of the root `Dockerfile`, and
//!   each `Procfile` process, is a container or platform process start;
//! - `[project.scripts]` and `[tool.poetry.scripts]` in `pyproject.toml`
//!   are declared console scripts;
//! - `bin` in a `package.json` is a declared command.
//! - a `.js`/`.mjs`/`.cjs` script behind a `node` shebang is a process
//!   start, and each subcommand it dispatches on `process.argv` a command
//!   (`node.rs`).
//! - a Go `func main()` in `package main` is a process start (`go.rs`).
//! - a Java `public static void main`, a message-listener, `@Scheduled` or
//!   runner method is a process start, queue consumer, scheduled job or
//!   startup task (`java.rs`).
//! - a Kotlin `fun main`, a message-listener, `@Scheduled` or runner
//!   function is a process start (an HTTP server when it starts Ktor or
//!   Spring Boot with web endpoints), queue consumer, scheduled job or
//!   startup task (`kotlin.rs`).
//! - an Android `<activity>`, `<service>`, `<receiver>` or `<provider>`
//!   an `AndroidManifest.xml` declares is a screen, service, broadcast
//!   receiver or content provider start entering its class's lifecycle
//!   function (`android.rs`), and a Kotlin WorkManager worker's `doWork`
//!   a background job (`kotlin.rs`).
//! - a Rust `fn main()` in a binary root is a process start, a
//!   `#[tauri::command]` function an IPC command and a tonic service method
//!   a gRPC method (`rust.rs`).
//!
//! What starts a Python process is read from the module and the repository
//! modules it imports, never guessed from its name, and recorded as `kind`
//! with the span that shows it (`kind_evidence`):
//!
//! | kind | read from |
//! |---|---|
//! | `smtp-listener` | an import of `aiosmtpd` or `smtpd` |
//! | `database-listener` | a SQL `LISTEN` string in the module or a repository module it imports |
//! | `queue-consumer` | an import of a queue client (`pika`, `kombu`, Kafka, NATS, Pub/Sub) |
//! | `operator-shell` | `embed()` from IPython, or `code.interact()`, in the started code |
//! | `command` | a script under an operator directory, or a declared console script or `bin` |
//! | `polling-loop` | a `while True:` loop in the started code |
//! | `http-server` | a `.run(...)` call on a Flask app in the started code, or a WSGI/ASGI server command |
//! | `process` | none of the above |
//!
//! A file a schedule job already enters is that job's entry, not another.
//! A container, Procfile, console-script or `bin` command this reader
//! cannot resolve to one repository file is a `Gap` naming the command and
//! why (R-INT-4). Test modules, `setup.py` and `conftest.py` are not
//! entry points of the system. This tier never calls an LLM.

use crate::schedules::SourceId;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

/// Extractor id recorded on every entry-point fact.
pub const ENTRY_POINT_EXTRACTOR_ID: &str = "t0.entry-point";

/// Facts from one directory walk.
#[derive(Debug, Clone, Default)]
pub struct EntryPointExtraction {
    /// `EntryPoint` triggers, the `File`s they enter and their `Gap`s.
    pub nodes: Vec<Node>,
    /// `ENTRY` edges (entry point → file, symbol or gap).
    pub edges: Vec<Edge>,
    /// Files that declared at least one entry point.
    pub files: u64,
}

/// What a process start was read as, and the span that shows it.
#[derive(Debug, Clone, PartialEq)]
struct Kind {
    kind: &'static str,
    /// The file and span that show the kind, when not the start itself.
    evidence: Option<(String, (u64, u64))>,
}

/// One process start read from a file.
#[derive(Debug, Clone, PartialEq)]
struct Start {
    /// Where it is declared: the file and the span of the declaration.
    declared_in: String,
    span: (u64, u64),
    /// The identity suffix after `@<declared_in>#`.
    key: String,
    /// How it is started, as written (`__main__`, a command, a script).
    how: String,
    /// The command line, when the start is a command.
    command: Option<String>,
    kind: Kind,
    /// Default listening port, when the start declares one.
    port: Option<u64>,
    /// Further facts the declaration states, set on the entry point as is.
    extra: Vec<(&'static str, serde_json::Value)>,
    /// Further spans that show what `extra` states.
    extra_spans: Vec<(String, (u64, u64))>,
    target: Target,
}

#[derive(Debug, Clone, PartialEq)]
enum Target {
    /// A repository file, optionally a function in it the adapter emitted.
    File {
        rel: String,
        function: Option<String>,
        confidence: ConfidenceTier,
        marker: Option<&'static str>,
    },
    Gap(String),
}

/// Directories whose scripts a person runs by hand.
const OPERATOR_DIRS: &[&str] = &["commands", "scripts", "bin", "tools"];

/// Queue clients whose import makes a started module a consumer.
const QUEUE_MODULES: &[&str] = &[
    "pika",
    "kombu",
    "kafka",
    "aiokafka",
    "confluent_kafka",
    "nats",
    "aio_pika",
    "google.cloud.pubsub",
    "google.cloud.pubsub_v1",
];

/// Walk `root` and turn every process start into facts. `symbols` holds the
/// `Symbol` ids the language adapters emitted, so a declared console script
/// enters its function when the adapter proved it; `scheduled` holds the
/// files schedule jobs already enter.
pub fn extract_dir(
    root: &Path,
    id: &SourceId,
    symbols: &BTreeSet<String>,
    scheduled: &BTreeSet<String>,
) -> std::io::Result<EntryPointExtraction> {
    let walked = crate::captured_walk::walk(root)?;
    let text = |rel: &str| std::fs::read_to_string(root.join(rel)).ok();
    Ok(extract_walked(&walked, &text, id, symbols, scheduled))
}

/// The JavaScript and TypeScript extensions a Node start or a worker's
/// `main` module may have.
const SCRIPT_EXTENSIONS: &[&str] = &[".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx"];

/// The other file names the reader opens.
const START_FILE_NAMES: &[&str] = &[
    "Dockerfile",
    "Procfile",
    "pyproject.toml",
    "package.json",
    "Cargo.toml",
    "build.gradle",
    "build.gradle.kts",
];

/// True when the entry-point reader may open `rel`: a Python, Go, Rust,
/// Java, Kotlin, JavaScript or TypeScript source, a Worker config, an
/// Android manifest, or a `Dockerfile`, `Procfile`, `pyproject.toml`,
/// `package.json`, `Cargo.toml` or Gradle build file.
pub fn is_entry_point_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    [".py", ".go", ".rs", ".java", ".kt"]
        .iter()
        .chain(SCRIPT_EXTENSIONS)
        .any(|ext| name.ends_with(ext))
        || START_FILE_NAMES.contains(&name)
        || wrangler::config(name)
        || android::candidate(rel)
}

/// The files [`extract_dir`] may open or cite under `root`, in path order: a
/// source capture retains them (AC-1061).
pub fn entry_point_paths(root: &Path) -> std::io::Result<Vec<String>> {
    crate::captured_walk::selected(root, is_entry_point_file, || {
        let id = SourceId {
            repo: "selection",
            commit: "",
        };
        let none = BTreeSet::new();
        let out = extract_dir(root, &id, &none, &none)?;
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
    scheduled: &BTreeSet<String>,
) -> std::io::Result<EntryPointExtraction> {
    let walked = crate::captured_walk::listing(root, captured);
    let text = |rel: &str| {
        read(rel)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };
    Ok(extract_walked(&walked, &text, id, symbols, scheduled))
}

fn extract_walked(
    walked: &[source_walk::WalkedFile],
    text: &dyn Fn(&str) -> Option<String>,
    id: &SourceId,
    symbols: &BTreeSet<String>,
    scheduled: &BTreeSet<String>,
) -> EntryPointExtraction {
    let files: BTreeSet<String> = walked.iter().map(|file| file.rel.clone()).collect();
    let mut sources: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut read = |rel: &str| {
        sources
            .entry(rel.to_string())
            .or_insert_with(|| text(rel))
            .clone()
    };
    let mut starts: Vec<Start> = Vec::new();
    let mut java_files: Vec<(String, String)> = Vec::new();
    let mut kotlin_files: Vec<(String, String)> = Vec::new();
    let mut manifests: Vec<(String, String)> = Vec::new();
    for file in walked {
        let rel = file.rel.as_str();
        if rel.ends_with(".py") {
            if scheduled.contains(rel) || !system_module(rel) {
                continue;
            }
            let Some(text) = read(rel) else { continue };
            if let Some(start) = python_start(rel, &text, &files, &mut read) {
                starts.push(start);
            }
        } else if rel == "Dockerfile" {
            let Some(text) = read(rel) else { continue };
            starts.extend(dockerfile_start(&text, &files));
        } else if file.name == "Procfile" {
            let Some(text) = read(rel) else { continue };
            starts.extend(procfile_starts(rel, &text, &files));
        } else if wrangler::config(&file.name) && !rel.contains("node_modules/") {
            let Some(text) = read(rel) else { continue };
            if let Some(mut start) = wrangler::worker_start(rel, &file.name, &text, &files) {
                if let Target::File { rel: module, .. } = start.target.clone()
                    && let Some(code) = read(&module)
                {
                    wrangler::webhook_sender(&mut start, &module, &code);
                }
                starts.push(start);
            }
        } else if android::candidate(rel) {
            let Some(text) = read(rel) else { continue };
            manifests.push((rel.to_string(), text));
        } else if file.name == "pyproject.toml" {
            let Some(text) = read(rel) else { continue };
            starts.extend(console_scripts(rel, &text, &files, id, symbols));
        } else if go::candidate(rel) {
            let Some(text) = read(rel) else { continue };
            starts.extend(go::go_start(id.repo, rel, &text, symbols));
        } else if rust::candidate(rel) {
            let Some(text) = read(rel) else { continue };
            starts.extend(rust::rust_starts(id.repo, rel, &text, symbols));
        } else if java::candidate(rel) {
            let Some(text) = read(rel) else { continue };
            java_files.push((rel.to_string(), text));
        } else if kotlin::candidate(rel) {
            let Some(text) = read(rel) else { continue };
            kotlin_files.push((rel.to_string(), text));
        } else if node::candidate(rel) && !rel.contains("node_modules/") {
            if scheduled.contains(rel) {
                continue;
            }
            let Some(text) = read(rel) else { continue };
            starts.extend(node::node_starts(rel, &text));
        } else if file.name == "package.json" && !rel.contains("node_modules/") {
            let Some(text) = read(rel) else { continue };
            starts.extend(package_bins(rel, &text, &files));
        }
    }
    // A Kotlin Spring Boot main serves HTTP when Kotlin or Java sources
    // declare Spring web endpoints (AC-1104).
    let web = kotlin::spring_web(&kotlin_files, &java_files);
    starts.extend(kotlin::starts(id.repo, &kotlin_files, web, symbols));
    starts.extend(java::starts(id.repo, &java_files, symbols));
    // Android components enter the class their manifest names (AC-1110).
    if !manifests.is_empty() {
        let classes = android::declared_classes(&kotlin_files, &java_files);
        for (rel, text) in &manifests {
            let namespace = android::build_files(rel)
                .iter()
                .filter(|build| files.contains(build.as_str()))
                .find_map(|build| read(build).and_then(|t| android::gradle_namespace(&t)));
            starts.extend(android::starts(
                id.repo,
                rel,
                text,
                namespace.as_deref(),
                &classes,
                symbols,
            ));
        }
    }
    // A Rust binary that only calls its own package's library is a desktop
    // app when the library function it calls builds a `tauri::Builder`
    // (AC-1009, #1227).
    for start in &mut starts {
        if !matches!(start.kind.kind, "command" | "process") || start.how != "fn main" {
            continue;
        }
        let rel = start.declared_in.clone();
        if !rust::candidate(&rel) {
            continue;
        }
        let Some(dir) = rust::package_dir(&rel) else {
            continue;
        };
        let Some((krate, name)) =
            read(&rel).and_then(|text| rust::delegated_call(&text, start.span))
        else {
            continue;
        };
        let manifest = format!("{dir}Cargo.toml");
        if !files.contains(&manifest) {
            continue;
        }
        let Some((lib_name, lib_path)) = read(&manifest).and_then(|text| rust::library(&text))
        else {
            continue;
        };
        let lib_rel = format!("{dir}{lib_path}");
        if lib_name != krate
            || lib_rel == rel
            || !files.contains(&lib_rel)
            || !rust::candidate(&lib_rel)
        {
            continue;
        }
        if let Some(at) = read(&lib_rel).and_then(|text| rust::desktop_function(&text, &name)) {
            start.kind = Kind {
                kind: "desktop-app",
                evidence: Some((lib_rel, at)),
            };
        }
    }
    let mut out = EntryPointExtraction::default();
    let declaring: BTreeSet<&str> = starts.iter().map(|s| s.declared_in.as_str()).collect();
    out.files = declaring.len() as u64;
    let mut emitted_files = BTreeSet::new();
    for start in &starts {
        emit(id, start, &mut emitted_files, &mut out);
    }
    out
}

/// Whether a Python file is part of the running system rather than its
/// tests or packaging.
fn system_module(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let in_tests = rel
        .split('/')
        .rev()
        .skip(1)
        .any(|dir| matches!(dir, "tests" | "test" | "testing"));
    !(in_tests
        || name.starts_with("test_")
        || name.ends_with("_test.py")
        || matches!(name, "conftest.py" | "setup.py" | "noxfile.py"))
}

/// Whether a file sits under a directory of operator scripts.
fn operator_script(rel: &str) -> bool {
    let dirs: Vec<&str> = rel.split('/').collect();
    let dirs = &dirs[..dirs.len().saturating_sub(1)];
    dirs.iter().any(|dir| OPERATOR_DIRS.contains(dir))
        || dirs.windows(2).any(|w| w == ["management", "commands"])
}

fn python_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("tree-sitter-python grammar");
    parser
}

fn span(node: TsNode) -> (u64, u64) {
    (node.start_byte() as u64, node.end_byte() as u64)
}

/// The process start a Python module declares, if any.
fn python_start(
    rel: &str,
    text: &str,
    files: &BTreeSet<String>,
    read: &mut dyn FnMut(&str) -> Option<String>,
) -> Option<Start> {
    let tree = python_parser().parse(text, None)?;
    let module = tree.root_node();
    let mut walk = module.walk();
    let top: Vec<TsNode> = module.named_children(&mut walk).collect();
    let guard = top
        .iter()
        .copied()
        .find(|node| node.kind() == "if_statement" && main_guard(text, *node));
    let shebang =
        text.starts_with("#!") && text.lines().next().is_some_and(|l| l.contains("python"));
    let operator = operator_script(rel);
    // The code that runs when the process starts: the guard's body, or the
    // module-level statements of a script.
    let (started, how, start_span): (Vec<TsNode>, &str, (u64, u64)) = match guard {
        Some(guard) => (vec![guard], "__main__", span(guard)),
        None if shebang || operator => {
            let run: Vec<TsNode> = top.iter().copied().filter(|n| executes(text, *n)).collect();
            let (first, last) = (run.first()?, run.last()?);
            let span = (first.start_byte() as u64, last.end_byte() as u64);
            (run, "script", span)
        }
        None => return None,
    };
    let imports = imports(text, &top);
    let kind = python_kind(rel, text, &imports, &started, operator, files, read);
    let port = started.iter().find_map(|node| port_default(text, *node));
    Some(Start {
        declared_in: rel.to_string(),
        span: start_span,
        key: "main".into(),
        how: how.into(),
        command: None,
        kind,
        port,
        extra: Vec::new(),
        extra_spans: Vec::new(),
        target: Target::File {
            rel: rel.to_string(),
            function: None,
            confidence: ConfidenceTier::Confirmed,
            marker: None,
        },
    })
}

/// `if __name__ == "__main__":`, either way round.
fn main_guard(text: &str, statement: TsNode) -> bool {
    let Some(condition) = statement.child_by_field_name("condition") else {
        return false;
    };
    if condition.kind() != "comparison_operator" {
        return false;
    }
    let mut walk = condition.walk();
    let operands: Vec<TsNode> = condition.named_children(&mut walk).collect();
    let [left, right] = operands.as_slice() else {
        return false;
    };
    let mut walk = condition.walk();
    let operators: Vec<&str> = condition
        .children(&mut walk)
        .filter(|child| !child.is_named())
        .map(|child| child.kind())
        .collect();
    let name = |n: TsNode| n.kind() == "identifier" && &text[n.byte_range()] == "__name__";
    let main = |n: TsNode| {
        n.kind() == "string" && matches!(&text[n.byte_range()], "\"__main__\"" | "'__main__'")
    };
    operators == ["=="] && ((name(*left) && main(*right)) || (main(*left) && name(*right)))
}

/// Whether a module-level statement does work when the module runs: a call,
/// a loop or a block, not a definition, an import, an assignment or a
/// docstring.
fn executes(text: &str, node: TsNode) -> bool {
    match node.kind() {
        "for_statement" | "while_statement" | "with_statement" | "try_statement"
        | "if_statement" => true,
        "expression_statement" => {
            node.named_child(0)
                .is_some_and(|child| matches!(child.kind(), "call" | "await"))
                || contains(node, &|n| {
                    n.kind() == "call"
                        && call_name(text, n).is_some_and(|c| c.ends_with("parse_args"))
                })
        }
        _ => false,
    }
}

fn contains(node: TsNode, pred: &dyn Fn(TsNode) -> bool) -> bool {
    if pred(node) {
        return true;
    }
    let mut walk = node.walk();
    let children: Vec<TsNode> = node.named_children(&mut walk).collect();
    children.into_iter().any(|child| contains(child, pred))
}

fn find<'t>(node: TsNode<'t>, pred: &dyn Fn(TsNode) -> bool) -> Option<TsNode<'t>> {
    if pred(node) {
        return Some(node);
    }
    let mut walk = node.walk();
    let children: Vec<TsNode> = node.named_children(&mut walk).collect();
    children.into_iter().find_map(|child| find(child, pred))
}

/// The dotted name a call calls (`embed`, `app.run`, `code.interact`).
fn call_name<'s>(text: &'s str, call: TsNode) -> Option<&'s str> {
    let function = call.child_by_field_name("function")?;
    matches!(function.kind(), "identifier" | "attribute").then(|| &text[function.byte_range()])
}

/// A module-level import: the module, the names it binds and its span.
struct Import {
    module: String,
    names: Vec<String>,
    span: (u64, u64),
}

fn imports(text: &str, top: &[TsNode]) -> Vec<Import> {
    let mut out = Vec::new();
    for node in top {
        match node.kind() {
            "import_statement" => {
                let mut walk = node.walk();
                for child in node.named_children(&mut walk) {
                    let dotted = match child.kind() {
                        "dotted_name" => Some(child),
                        "aliased_import" => child.child_by_field_name("name"),
                        _ => None,
                    };
                    if let Some(dotted) = dotted {
                        let module = text[dotted.byte_range()].to_string();
                        out.push(Import {
                            names: vec![module.clone()],
                            module,
                            span: span(*node),
                        });
                    }
                }
            }
            "import_from_statement" => {
                let Some(module) = node.child_by_field_name("module_name") else {
                    continue;
                };
                let mut names = Vec::new();
                let mut walk = node.walk();
                for name in node.children_by_field_name("name", &mut walk) {
                    let bound = match name.kind() {
                        "aliased_import" => name.child_by_field_name("name"),
                        _ => Some(name),
                    };
                    if let Some(bound) = bound {
                        names.push(text[bound.byte_range()].to_string());
                    }
                }
                out.push(Import {
                    module: text[module.byte_range()].to_string(),
                    names,
                    span: span(*node),
                });
            }
            _ => {}
        }
    }
    out
}

fn imports_root<'i>(imports: &'i [Import], roots: &[&str]) -> Option<&'i Import> {
    imports.iter().find(|import| {
        roots
            .iter()
            .any(|root| import.module == *root || import.module.starts_with(&format!("{root}.")))
    })
}

fn python_kind(
    rel: &str,
    text: &str,
    imports: &[Import],
    started: &[TsNode],
    operator: bool,
    files: &BTreeSet<String>,
    read: &mut dyn FnMut(&str) -> Option<String>,
) -> Kind {
    let here = |span: (u64, u64)| Some((rel.to_string(), span));
    if let Some(import) = imports_root(imports, &["aiosmtpd", "smtpd"]) {
        return Kind {
            kind: "smtp-listener",
            evidence: here(import.span),
        };
    }
    if let Some(found) = listen_statement(rel, text, imports, files, read) {
        return Kind {
            kind: "database-listener",
            evidence: Some(found),
        };
    }
    if let Some(import) = imports_root(imports, QUEUE_MODULES) {
        return Kind {
            kind: "queue-consumer",
            evidence: here(import.span),
        };
    }
    let ipython = imports
        .iter()
        .any(|i| i.module == "IPython" && i.names.iter().any(|n| n == "embed"));
    let shell = started.iter().find_map(|node| {
        find(*node, &|n| {
            n.kind() == "call"
                && match call_name(text, n) {
                    Some("embed") => ipython,
                    Some("IPython.embed" | "code.interact") => true,
                    _ => false,
                }
        })
    });
    if let Some(call) = shell {
        return Kind {
            kind: "operator-shell",
            evidence: here(span(call)),
        };
    }
    // A person runs an operator script, however it loops.
    if operator {
        return Kind {
            kind: "command",
            evidence: None,
        };
    }
    let forever = started.iter().find_map(|node| {
        find(*node, &|n| {
            n.kind() == "while_statement"
                && n.child_by_field_name("condition")
                    .is_some_and(|c| c.kind() == "true")
        })
    });
    if let Some(found) = forever {
        let head = found
            .child_by_field_name("condition")
            .map_or(span(found), |c| {
                (found.start_byte() as u64, c.end_byte() as u64)
            });
        return Kind {
            kind: "polling-loop",
            evidence: here(head),
        };
    }
    if imports_root(imports, &["flask"]).is_some()
        && let Some(call) = started.iter().find_map(|node| {
            find(*node, &|n| {
                n.kind() == "call" && call_name(text, n).is_some_and(|c| c.ends_with(".run"))
            })
        })
    {
        return Kind {
            kind: "http-server",
            evidence: here(span(call)),
        };
    }
    Kind {
        kind: "process",
        evidence: None,
    }
}

/// A SQL `LISTEN` string in the module or in a repository module it imports
/// with an absolute `from <module> import …` or `import <module>`.
fn listen_statement(
    rel: &str,
    text: &str,
    imports: &[Import],
    files: &BTreeSet<String>,
    read: &mut dyn FnMut(&str) -> Option<String>,
) -> Option<(String, (u64, u64))> {
    if let Some(found) = listen_in(text) {
        return Some((rel.to_string(), found));
    }
    let modules: BTreeSet<String> = imports
        .iter()
        .filter_map(|import| crate::schedules::module_file(rel, &import.module, files))
        .collect();
    modules.into_iter().find_map(|module| {
        let source = read(&module)?;
        listen_in(&source).map(|found| (module, found))
    })
}

fn listen_in(text: &str) -> Option<(u64, u64)> {
    if !text.contains("LISTEN") && !text.contains("listen ") {
        return None;
    }
    let tree = python_parser().parse(text, None)?;
    let found = find(tree.root_node(), &|n| {
        n.kind() == "string_content" && {
            let value = text[n.byte_range()].trim_start();
            value
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("listen "))
        }
    })?;
    let string = found.parent().unwrap_or(found);
    Some(span(string))
}

/// The integer default of a `--port` argument the started code declares.
fn port_default(text: &str, node: TsNode) -> Option<u64> {
    let call = find(node, &|n| {
        n.kind() == "call"
            && call_name(text, n).is_some_and(|c| c.ends_with("add_argument"))
            && n.child_by_field_name("arguments").is_some_and(|args| {
                let mut walk = args.walk();
                let named: Vec<TsNode> = args.named_children(&mut walk).collect();
                named.iter().any(|a| {
                    a.kind() == "string" && {
                        let raw = &text[a.byte_range()];
                        raw == "\"--port\"" || raw == "'--port'"
                    }
                })
            })
    })?;
    let args = call.child_by_field_name("arguments")?;
    let mut walk = args.walk();
    let named: Vec<TsNode> = args.named_children(&mut walk).collect();
    named.into_iter().find_map(|arg| {
        (arg.kind() == "keyword_argument"
            && arg
                .child_by_field_name("name")
                .is_some_and(|n| &text[n.byte_range()] == "default"))
        .then(|| arg.child_by_field_name("value"))
        .flatten()
        .filter(|v| v.kind() == "integer")
        .and_then(|v| text[v.byte_range()].parse().ok())
    })
}

/// The final stage's process command of the root `Dockerfile`.
fn dockerfile_start(text: &str, files: &BTreeSet<String>) -> Option<Start> {
    let mut entrypoint: Option<((u64, u64), Exec)> = None;
    let mut cmd: Option<((u64, u64), Exec)> = None;
    for (span, instruction) in crate::schedules::instructions(text) {
        let (keyword, rest) = instruction
            .split_once(char::is_whitespace)
            .unwrap_or((instruction.as_str(), ""));
        match keyword.to_ascii_uppercase().as_str() {
            "FROM" => {
                entrypoint = None;
                cmd = None;
            }
            "ENTRYPOINT" => entrypoint = Some((span, exec(rest.trim()))),
            "CMD" => cmd = Some((span, exec(rest.trim()))),
            _ => {}
        }
    }
    let (span, words) = match (entrypoint, cmd) {
        (None, None) => return None,
        (Some((span, Exec::Words(mut e))), Some((cspan, Exec::Words(c)))) => {
            e.extend(c);
            ((span.0.min(cspan.0), span.1.max(cspan.1)), Exec::Words(e))
        }
        (Some(entry), _) => entry,
        (None, Some(cmd)) => cmd,
    };
    Some(command_start(
        "Dockerfile",
        span,
        "cmd",
        "container command",
        words,
        files,
    ))
}

/// Each `<name>: <command>` process of a `Procfile`.
fn procfile_starts(rel: &str, text: &str, files: &BTreeSet<String>) -> Vec<Start> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len() as u64;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, command)) = trimmed.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            continue;
        }
        let end = start + line.trim_end().len() as u64;
        out.push(command_start(
            rel,
            (start, end),
            name,
            "Procfile process",
            exec(command.trim()),
            files,
        ));
    }
    out
}

/// A command as words, or a shell form this reader does not split.
#[derive(Debug, Clone, PartialEq)]
enum Exec {
    Words(Vec<String>),
    Shell(String),
}

/// JSON exec form, or a shell form without shell syntax, as words.
fn exec(rest: &str) -> Exec {
    if rest.starts_with('[') {
        return match serde_json::from_str::<Vec<String>>(rest) {
            Ok(words) => Exec::Words(words),
            Err(_) => Exec::Shell(rest.to_string()),
        };
    }
    if rest.chars().any(|c| {
        matches!(
            c,
            '|' | '&' | ';' | '<' | '>' | '$' | '`' | '(' | ')' | '\'' | '"' | '\\' | '*' | '?'
        )
    }) {
        return Exec::Shell(rest.to_string());
    }
    Exec::Words(rest.split_whitespace().map(str::to_string).collect())
}

/// Server options whose value is the next argument.
const SERVER_VALUE_OPTIONS: &[&str] = &[
    "-b",
    "--bind",
    "-c",
    "--config",
    "-k",
    "--worker-class",
    "--host",
    "--port",
    "--chdir",
    "--env",
    "-e",
    "--name",
    "-n",
    "--app-dir",
];

/// WSGI and ASGI servers that take the application as `module:object`.
const APP_SERVERS: &[&str] = &[
    "gunicorn",
    "uvicorn",
    "hypercorn",
    "daphne",
    "waitress-serve",
];

fn command_start(
    rel: &str,
    span: (u64, u64),
    key: &str,
    how: &str,
    exec: Exec,
    files: &BTreeSet<String>,
) -> Start {
    let (command, kind, target) = match exec {
        Exec::Shell(command) => (
            command,
            "process",
            Target::Gap("shell-composed command".into()),
        ),
        Exec::Words(words) => {
            let command = words.join(" ");
            let (kind, target) = resolve_command(&words, files);
            (command, kind, target)
        }
    };
    Start {
        declared_in: rel.to_string(),
        span,
        key: key.to_string(),
        how: how.to_string(),
        command: Some(command),
        kind: Kind {
            kind,
            evidence: None,
        },
        port: None,
        extra: Vec::new(),
        extra_spans: Vec::new(),
        target,
    }
}

/// The one repository file a command runs, or why it is not proven.
fn resolve_command(words: &[String], files: &BTreeSet<String>) -> (&'static str, Target) {
    let Some(program) = words.first() else {
        return ("process", Target::Gap("empty command".into()));
    };
    let base = program.rsplit('/').next().unwrap_or(program);
    let args = &words[1..];
    if APP_SERVERS.contains(&base) {
        // The application is the last positional `module:object` whose
        // parts are identifiers: a bind address (`0.0.0.0:8000`,
        // `localhost:8000`) is not one, nor is the value of an option.
        let identifier = |part: &str| {
            part.chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
        };
        let app = args
            .iter()
            .enumerate()
            .filter(|(i, arg)| {
                !arg.starts_with('-')
                    && !(*i > 0 && SERVER_VALUE_OPTIONS.contains(&args[i - 1].as_str()))
                    && arg.split_once(':').is_some_and(|(module, object)| {
                        identifier(module)
                            && identifier(object)
                            && module
                                .chars()
                                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
                    })
            })
            .map(|(_, arg)| arg)
            .next_back();
        let Some(app) = app else {
            return (
                "http-server",
                Target::Gap(format!("`{base}` names no application as module:object")),
            );
        };
        let module = app.split_once(':').map_or(app.as_str(), |(m, _)| m);
        return ("http-server", module_target(module, None, files));
    }
    let python = base == "python"
        || base
            .strip_prefix("python")
            .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'));
    if python {
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            if arg == "-m" {
                return match rest.next() {
                    Some(module) => ("process", module_target(module, None, files)),
                    None => ("process", Target::Gap("`-m` without a module".into())),
                };
            }
            if arg == "-c" {
                return ("process", Target::Gap("inline code (`-c`)".into()));
            }
            // `-W ignore`, `-X dev`: the option's operand is not the script.
            if matches!(arg.as_str(), "-W" | "-X") {
                rest.next();
                continue;
            }
            if arg.starts_with('-') {
                continue;
            }
            return ("process", path_target(arg, files));
        }
        return (
            "process",
            Target::Gap("interpreter without a script".into()),
        );
    }
    if matches!(base, "node" | "bun" | "deno") {
        return match args.iter().find(|arg| !arg.starts_with('-')) {
            Some(script) => ("process", path_target(script, files)),
            None => ("process", Target::Gap("runtime without a script".into())),
        };
    }
    if program.contains('/') || program.contains('.') {
        return ("process", path_target(program, files));
    }
    (
        "process",
        Target::Gap(format!("runs `{base}`, which no reader interprets")),
    )
}

/// A script path: relative to the repository root, or an absolute path whose
/// unique suffix is a repository file.
fn path_target(path: &str, files: &BTreeSet<String>) -> Target {
    let relative = path.trim_start_matches("./");
    if !path.starts_with('/') && files.contains(relative) {
        return Target::File {
            rel: relative.to_string(),
            function: None,
            confidence: ConfidenceTier::InferredStrong,
            marker: Some("entry_relative_to_repo_root"),
        };
    }
    let suffixes: Vec<&String> = files
        .iter()
        .filter(|file| path.ends_with(&format!("/{file}")))
        .collect();
    match suffixes.as_slice() {
        [file] => Target::File {
            rel: (*file).clone(),
            function: None,
            confidence: ConfidenceTier::InferredStrong,
            marker: Some("entry_path_suffix"),
        },
        [] => Target::Gap(format!("`{path}` is not a repository file")),
        _ => Target::Gap(format!("`{path}` matches several repository files")),
    }
}

/// A Python module path as one repository file, from the root or `src/`.
fn module_target(module: &str, function: Option<String>, files: &BTreeSet<String>) -> Target {
    if module.is_empty()
        || !module
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return Target::Gap(format!("`{module}` is not a module name"));
    }
    let path = module.replace('.', "/");
    let found: Vec<String> = ["", "src/"]
        .iter()
        .flat_map(|prefix| {
            [
                format!("{prefix}{path}.py"),
                format!("{prefix}{path}/__init__.py"),
                format!("{prefix}{path}/__main__.py"),
            ]
        })
        .filter(|candidate| files.contains(candidate))
        .collect();
    match found.as_slice() {
        [rel] => Target::File {
            rel: rel.clone(),
            function,
            confidence: ConfidenceTier::InferredStrong,
            marker: Some("entry_module_relative_to_repo_root"),
        },
        [] => Target::Gap(format!("module `{module}` is not a repository file")),
        _ => Target::Gap(format!(
            "module `{module}` matches several repository files"
        )),
    }
}

/// `[project.scripts]` and `[tool.poetry.scripts]` console scripts.
fn console_scripts(
    rel: &str,
    text: &str,
    files: &BTreeSet<String>,
    id: &SourceId,
    symbols: &BTreeSet<String>,
) -> Vec<Start> {
    let Ok(doc) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let tables = [
        doc.get("project").and_then(|p| p.get("scripts")),
        doc.get("tool")
            .and_then(|t| t.get("poetry"))
            .and_then(|p| p.get("scripts")),
    ];
    let dir = rel.rsplit_once('/').map_or("", |(dir, _)| dir);
    let scoped: BTreeSet<String> = files
        .iter()
        .filter_map(|file| {
            if dir.is_empty() {
                Some(file.clone())
            } else {
                file.strip_prefix(&format!("{dir}/")).map(str::to_string)
            }
        })
        .collect();
    let mut out = Vec::new();
    for table in tables.into_iter().flatten() {
        let Some(table) = table.as_table() else {
            continue;
        };
        for (name, value) in table {
            let span = key_span(text, name);
            let Some(spec) = value.as_str() else {
                out.push(declared(
                    rel,
                    span,
                    name,
                    None,
                    Target::Gap("console script is not a `module:function` string".into()),
                ));
                continue;
            };
            let target = match spec.split_once(':') {
                Some((module, function)) => {
                    let function = function.split('.').next().unwrap_or(function).trim();
                    match module_target(module.trim(), None, &scoped) {
                        Target::File {
                            rel: file,
                            confidence,
                            marker,
                            ..
                        } => {
                            let file = if dir.is_empty() {
                                file
                            } else {
                                format!("{dir}/{file}")
                            };
                            let proven =
                                symbols.contains(&format!("sym:{}@{file}#{function}", id.repo));
                            Target::File {
                                rel: file,
                                function: proven.then(|| function.to_string()),
                                confidence,
                                marker,
                            }
                        }
                        gap => gap,
                    }
                }
                None => Target::Gap(format!("console script `{spec}` names no function")),
            };
            out.push(declared(rel, span, name, Some(spec.to_string()), target));
        }
    }
    out
}

/// `bin` of a `package.json`: one command name, or a map of them.
fn package_bins(rel: &str, text: &str, files: &BTreeSet<String>) -> Vec<Start> {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let dir = rel
        .rsplit_once('/')
        .map_or(String::new(), |(dir, _)| format!("{dir}/"));
    let bins: Vec<(String, String)> = match &doc["bin"] {
        serde_json::Value::String(path) => vec![(
            doc["name"].as_str().unwrap_or("bin").to_string(),
            path.clone(),
        )],
        serde_json::Value::Object(map) => map
            .iter()
            .filter_map(|(name, path)| Some((name.clone(), path.as_str()?.to_string())))
            .collect(),
        _ => return Vec::new(),
    };
    bins.into_iter()
        .map(|(name, path)| {
            let joined = format!("{dir}{}", path.trim_start_matches("./"));
            let target = if files.contains(&joined) {
                Target::File {
                    rel: joined,
                    function: None,
                    confidence: ConfidenceTier::Confirmed,
                    marker: None,
                }
            } else {
                Target::Gap(format!("`bin` path `{path}` is not a repository file"))
            };
            let span = key_span(text, "bin");
            declared(rel, span, &name, Some(path), target)
        })
        .collect()
}

fn declared(
    rel: &str,
    span: (u64, u64),
    name: &str,
    command: Option<String>,
    target: Target,
) -> Start {
    Start {
        declared_in: rel.to_string(),
        span,
        key: name.to_string(),
        how: "declared command".into(),
        command: command.map(|c| format!("{name} = {c}")),
        kind: Kind {
            kind: "command",
            evidence: None,
        },
        port: None,
        extra: Vec::new(),
        extra_spans: Vec::new(),
        target,
    }
}

/// The span of the first line declaring `key`, or the whole file.
fn key_span(text: &str, key: &str) -> (u64, u64) {
    let mut offset = 0u64;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let quoted = format!("\"{key}\"");
        if trimmed.starts_with(key) || trimmed.starts_with(&quoted) {
            let start = offset + (line.len() - trimmed.len()) as u64;
            return (start, offset + line.trim_end().len() as u64);
        }
        offset += line.len() as u64;
    }
    (0, text.len().max(1) as u64)
}

/// What starts the process, in words.
fn started_by(kind: &str) -> &'static str {
    match kind {
        "smtp-listener" => "a message delivered over SMTP",
        "database-listener" => "a database notification (LISTEN)",
        "queue-consumer" => "a message on a queue",
        "operator-shell" => "a person opens an interactive shell",
        "polling-loop" => "its own loop, which runs until the process stops",
        "http-server" => "an HTTP request",
        "command" => "a person runs the command",
        "edge-worker" => "an HTTP request at the edge",
        "scheduled-job" => "its schedule",
        "startup-task" => "the application starting",
        "desktop-app" => "a person opens the application",
        "grpc-server" | "grpc-method" => "a gRPC call",
        "ipc-command" => "the desktop front end invoking the command",
        "android-activity" => "a person opening the screen, or an intent starting it",
        "android-service" => "an intent starting or binding the service",
        "android-receiver" => "a broadcast intent",
        "android-provider" => "a content query, or the app process starting",
        "background-job" => "WorkManager running enqueued work",
        _ => "the process is started",
    }
}

fn display(start: &Start) -> String {
    let what = match start.kind.kind {
        "smtp-listener" => "Mail received over SMTP",
        "database-listener" => "Database notifications",
        "queue-consumer" => "Queue messages",
        "operator-shell" => "Operator shell",
        "polling-loop" => "Polling loop",
        "http-server" => "HTTP server",
        "command" => "Command",
        "edge-worker" => "Edge worker",
        "scheduled-job" => "Scheduled job",
        "startup-task" => "Startup task",
        "desktop-app" => "Desktop application",
        "grpc-server" => "gRPC server",
        "grpc-method" => "gRPC method",
        "ipc-command" => "IPC command",
        "android-activity" if start.extra.iter().any(|(k, _)| *k == "launcher") => {
            "App launch screen"
        }
        "android-activity" => "Screen",
        "android-service" => "Android service",
        "android-receiver" => "Broadcast receiver",
        "android-provider" => "Content provider",
        "background-job" => "Background job",
        _ => "Process",
    };
    let handler = start
        .extra
        .iter()
        .find_map(|(key, value)| (*key == "handler").then(|| value.as_str()).flatten())
        .or_else(|| {
            start
                .extra
                .iter()
                .find_map(|(key, value)| (*key == "component").then(|| value.as_str()).flatten())
        });
    let at = match (&start.command, handler) {
        (Some(command), _) => format!("{}: {command}", start.declared_in),
        (None, Some(handler)) => format!("{}: {handler}", start.declared_in),
        (None, None) => start.declared_in.clone(),
    };
    match start.port {
        Some(port) => format!("{what} ({at}, port {port})"),
        None => format!("{what} ({at})"),
    }
}

fn emit(
    id: &SourceId,
    start: &Start,
    emitted_files: &mut BTreeSet<String>,
    out: &mut EntryPointExtraction,
) {
    let evidence = |path: &str, (s, e): (u64, u64)| EvidenceRef {
        repo: id.repo.to_string(),
        path: path.to_string(),
        byte_start: s,
        byte_end: e.max(s + 1),
        commit_sha: id.commit.to_string(),
    };
    let seal =
        |mut props: serde_json::Value, confidence, spans: Vec<EvidenceRef>, identity: &str| {
            let input =
                serde_json::to_vec(&serde_json::json!([identity, &props])).expect("serializes");
            props["prov"] = serde_json::to_value(
                Provenance::new(
                    Tier::Deterministic,
                    confidence,
                    spans,
                    ENTRY_POINT_EXTRACTOR_ID,
                    &input,
                )
                .expect("within ceiling"),
            )
            .expect("serializes");
            props
        };
    let entry_id = format!("entry:{}@{}#{}", id.repo, start.declared_in, start.key);
    let mut spans = vec![evidence(&start.declared_in, start.span)];
    for (path, span) in start.kind.evidence.iter().chain(&start.extra_spans) {
        let extra = evidence(path, *span);
        if !spans.contains(&extra) {
            spans.push(extra);
        }
    }
    let mut props = serde_json::json!({
        "name": display(start),
        "kind": start.kind.kind,
        "started_by": started_by(start.kind.kind),
        "how": start.how,
        "file": start.declared_in,
    });
    if let Some(command) = &start.command {
        props["command"] = command.clone().into();
    }
    if let Some(port) = start.port {
        props["port"] = port.into();
    }
    for (key, value) in &start.extra {
        props[*key] = value.clone();
    }
    if let Some((path, _)) = &start.kind.evidence {
        props["kind_evidence"] = path.clone().into();
    }
    out.nodes.push(Node {
        id: entry_id.clone(),
        label: "EntryPoint".into(),
        props: seal(props, ConfidenceTier::Confirmed, spans.clone(), &entry_id),
    });
    match &start.target {
        Target::File {
            rel,
            function,
            confidence,
            marker,
        } => {
            let file_id = format!("file:{}@{rel}", id.repo);
            if emitted_files.insert(file_id.clone()) {
                // An adapter that owns this file supersedes this node.
                out.nodes.push(Node {
                    id: file_id.clone(),
                    label: "File".into(),
                    props: seal(
                        serde_json::json!({ "path": rel }),
                        ConfidenceTier::Confirmed,
                        vec![evidence(&start.declared_in, start.span)],
                        &file_id,
                    ),
                });
            }
            let dst = match function {
                Some(function) => format!("sym:{}@{rel}#{function}", id.repo),
                None => file_id,
            };
            let identity = format!("{entry_id} ENTRY {dst}");
            let mut props = serde_json::json!({});
            if let Some(marker) = marker {
                props[*marker] = true.into();
            }
            out.edges.push(Edge {
                src: entry_id,
                dst,
                label: "ENTRY".into(),
                props: seal(props, *confidence, spans, &identity),
            });
        }
        Target::Gap(reason) => {
            let gap_id = format!(
                "gap:entry-point:{}@{}#{}",
                id.repo, start.declared_in, start.key
            );
            out.nodes.push(Node {
                id: gap_id.clone(),
                label: "Gap".into(),
                props: seal(
                    serde_json::json!({
                        "reason": format!("entry point not interpreted: {reason}"),
                        "command": start.command,
                        "attempted_tiers": ["T0"],
                    }),
                    ConfidenceTier::Gap,
                    spans.clone(),
                    &gap_id,
                ),
            });
            let identity = format!("{entry_id} ENTRY {gap_id}");
            out.edges.push(Edge {
                src: entry_id,
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

mod android;
mod go;
mod java;
mod kotlin;
mod node;
mod rust;
mod wrangler;

#[cfg(test)]
mod tests;
