//! Configuration keys declared and read (#1821, AC-0651, catalogue D7 §7.3
//! and D6 §6.4): `configuration.md`.
//!
//! A deterministic (T0) projection of two fact kinds: the environment-file
//! `Tool` facts, which hold the key names a file declares (AC-0097), and the
//! `ConfigRead` facts with their `READS_CONFIG` relations, which hold the
//! standard-library environment reads the language adapters recover
//! (ADR-0059). No model is called and no fact is recovered here.
//!
//! **Names only.** The facts hold key names and spans, never a string value,
//! default or fallback, and nothing else is read here (ADR-0059 decisions 5
//! and 7). A Python read's numeric or boolean `defaults` (ADR-0059 decision
//! 5 as amended) are stated in rule rows (#1998), not in this artifact. An environment-file fact has no span by design: a span would be
//! a readable window into a file that holds secrets.
//!
//! **One row per key.** A key is listed once per repository, with every
//! environment file of that repository that declares it and every file that
//! reads it. Names are compared exactly. Repositories are not joined: a file
//! in one says nothing about a read in another.
//!
//! **Cost.** One index over the admitted facts and one pass over the key
//! names the environment files hold: linear in both. Nothing is walked.
//!
//! **Mode.** The producer receives the facts the export mode admitted, like
//! every other graph artifact, so a `verified-only` export holds no
//! InferredWeak row. The caller counts what the mode and rejections withheld
//! ([`withheld`]) and the document states it (R-INT-5).
//!
//! **Coverage.** Which languages are read is a stated list
//! ([`CONFIG_READ_COVERAGE`]), because the stored coverage inventories do
//! not carry the adapter coverage cells and this crate cannot see them. A
//! test in the app crate, which sees both, fails when the list and the
//! `config_reads` cells differ.

use super::producers::Output;
use super::render::{Assertions, Budget, Literal, RenderLimit, Text};
use super::{ExportMode, included, provenance};
use core_graph::{Edge, Node};
use core_prov::ConfidenceTier;
use serde::Deserialize;
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

const CONFIG_READ: &str = "ConfigRead";
const READS_CONFIG: &str = "READS_CONFIG";
const TOOL: &str = "Tool";
/// `props.category` of an environment-file `Tool` fact.
const ENVIRONMENT: &str = "environment";
/// `props.source` of an environment-variable read, and its id segment.
const ENV: &str = "env";

/// The languages whose standard-library environment reads are recovered,
/// each with what its adapter reads and does not read. Equal, cell for cell
/// and in order, to the `config_reads` cells of `ingest::matrix`
/// (ADR-0059 decision 9); `configuration_coverage_matches_the_matrix_cells`
/// in the app crate keeps the two from drifting.
pub const CONFIG_READ_COVERAGE: &[(&str, &str)] = &[
    ("TypeScript", TS),
    ("JavaScript", TS),
    (
        "Python",
        "`os.environ[\"X\"]`, `os.environ.get(\"X\")` and `os.getenv(\"X\")` through a \
         module-level import of `os` nothing rebinds; a non-literal key is a read without a \
         name; a keyword `key=` argument, `\"X\" in os.environ`, subscript reads in a file that \
         also writes `os.environ[...]`, every read when the repository has a module named `os`, \
         and settings frameworks are not read",
    ),
    (
        "Go",
        "`os.Getenv` and `os.LookupEnv` through the file's import of `os`; a constant or \
         computed key is a read without a name; a dot-imported `Getenv` and configuration \
         frameworks (Viper) are not read",
    ),
    (
        "Rust",
        "`std::env::var` and `std::env::var_os` through the qualified path or a `use` of \
         `std::env`; a constant or computed key is a read without a name; the compile-time \
         `env!`/`option_env!` macros, `std::env::vars()` and configuration crates are not read",
    ),
    (
        "Java",
        "`System.getenv(\"X\")` when no Java source in the repository declares or imports \
         anything named `System` and the file has no static wildcard import; a non-literal key \
         is a read without a name; a member named `System` inherited from a type outside the \
         repository is not ruled out; `System.getenv()` as a map, `java.lang.System.getenv` and \
         Spring `@Value`/`Environment` are not read",
    ),
    (
        "Kotlin",
        "`System.getenv(\"X\")` when nothing in the repository's Kotlin sources or product Java \
         top-level types is named `System` and every star import of the file names a repository \
         package; a non-literal key is a read without a name; a member named `System` reached \
         through an implicit receiver or a supertype outside the repository, or inherited from \
         a repository Java class, is not ruled out; files with other star imports, \
         `System.getenv()` as a map and Spring `@Value` are not read",
    ),
];

const TS: &str = "`process.env.X` and `process.env[\"X\"]` while `process` is the global or an \
import of `process`/`node:process`; a non-literal index is a read without a name; \
`import.meta.env`, destructuring of `process.env`, `globalThis.process` and configuration \
frameworks (NestJS `ConfigService`) are not read";

/// What the projection does not cover, stated in every export.
const NOT_COVERED: &[&str] = &[
    "`import.meta.env` and a destructured `process.env` (`const { X } = process.env`) in \
     TypeScript and JavaScript are not read.",
    "A membership test, `\"X\" in os.environ`, in Python is not read.",
    "The compile-time `env!` and `option_env!` macros in Rust are not read.",
    "Configuration frameworks are not read: Spring `@Value` and `Environment`, NestJS \
     `ConfigService`, Viper, and settings libraries in general. A key reached only through one \
     of them is listed as declared and never read, or is not listed at all.",
    "A language with no row in the table above has no configuration read recovered.",
    "Only environment files count as declarations. A key set in a container or compose file, a \
     deployment manifest, an infrastructure definition, a workflow `env:` map or a shell script \
     is not listed as declared.",
    "Whether a language adapter ran for a repository is not stated here. The coverage document \
     states it.",
    "No value is read. A key that is declared empty, or read with a default, looks the same as \
     any other.",
];

/// The confidence a fact records, without decoding its whole provenance. A
/// fact with none is unresolved, as [`provenance`] makes it.
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

fn is_env_file(node: &Node) -> bool {
    node.label == TOOL && node.props["category"] == ENVIRONMENT
}

/// How many configuration facts the export mode and the rejections
/// withhold, counted over the facts before that filter: the environment-file
/// facts, the `ConfigRead` facts and the `READS_CONFIG` relations.
pub(super) fn withheld<N: Borrow<Node>, E: Borrow<Edge>>(
    nodes: &[N],
    edges: &[E],
    mode: ExportMode,
    rejected: &BTreeSet<String>,
) -> usize {
    let nodes = nodes
        .iter()
        .map(Borrow::borrow)
        .filter(|node| node.label == CONFIG_READ || is_env_file(node))
        .filter(|node| !included(&provenance(&node.props, &node.id), mode, rejected))
        .count();
    let edges = edges
        .iter()
        .map(Borrow::borrow)
        .filter(|edge| edge.label == READS_CONFIG)
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

/// `{repo}` and the rest of an id shaped `{scheme}{repo}@{rest}`.
fn scoped<'a>(id: &'a str, scheme: &str) -> Option<(&'a str, &'a str)> {
    id.strip_prefix(scheme)?.split_once('@')
}

/// One `ConfigRead` fact: the reads of one key, or the unnamed reads, in one
/// file.
struct Read<'a> {
    node: &'a Node,
    repo: &'a str,
    path: &'a str,
    /// Its `READS_CONFIG` relations, by reading symbol or file id.
    sources: Vec<&'a Edge>,
}

impl<'a> Read<'a> {
    /// The repository, file and key the fact's id and properties name. The
    /// key is `None` for the unnamed reads of a file. A fact whose id does
    /// not say where it is, is not placed.
    fn place(node: &'a Node) -> Option<(Self, Option<&'a str>)> {
        let (repo, rest) = scoped(&node.id, "cfgread:")?;
        let key = node.props["key"]
            .as_str()
            .filter(|key| !key.is_empty() && node.props["dynamic"] != true);
        let path = match key {
            Some(key) => rest.strip_suffix(key)?.strip_suffix(":env:")?,
            None => rest.strip_suffix(":env:?")?,
        };
        let read = Self {
            node,
            repo,
            path,
            sources: Vec::new(),
        };
        Some((read, key))
    }

    fn weak(&self) -> usize {
        usize::from(weak(&self.node.props))
            + self.sources.iter().filter(|edge| weak(&edge.props)).count()
    }
}

/// One environment file that declares a key.
#[derive(Clone, Copy)]
struct Declaration<'a> {
    tool: &'a Node,
    path: &'a str,
}

/// One key of one repository.
#[derive(Default)]
struct Key<'a> {
    /// By environment-file fact id.
    declared: Vec<Declaration<'a>>,
    /// By `ConfigRead` fact id.
    reads: Vec<Read<'a>>,
}

impl Key<'_> {
    fn status(&self) -> &'static str {
        match (self.declared.is_empty(), self.reads.is_empty()) {
            (false, false) => "declared and read",
            (true, _) => "read, never declared",
            (false, true) => "declared, never read",
        }
    }

    fn weak(&self) -> usize {
        self.declared
            .iter()
            .filter(|declaration| weak(&declaration.tool.props))
            .count()
            + self.reads.iter().map(Read::weak).sum::<usize>()
    }
}

/// The admitted facts as the document lists them.
struct Model<'a> {
    /// By repository, then key name.
    keys: BTreeMap<(&'a str, &'a str), Key<'a>>,
    /// The unnamed reads, by `ConfigRead` fact id.
    dynamic: Vec<Read<'a>>,
    /// Environment-file facts admitted.
    files: usize,
    /// `ConfigRead` facts whose source is not the environment.
    other_sources: usize,
    /// Facts whose id does not say where they are.
    unplaced: usize,
    /// `READS_CONFIG` relations whose `ConfigRead` fact is not listed.
    loose: usize,
}

impl<'a> Model<'a> {
    fn new(nodes: &[&'a Node], edges: &[&'a Edge]) -> Self {
        // Never the store's or the caller's order, and each fact once.
        let by_id: BTreeMap<&str, &Node> = nodes
            .iter()
            .filter(|node| node.label == CONFIG_READ || is_env_file(node))
            .map(|node| (node.id.as_str(), *node))
            .collect();
        let mut sources: BTreeMap<&str, Vec<&Edge>> = BTreeMap::new();
        for edge in edges.iter().filter(|edge| edge.label == READS_CONFIG) {
            sources.entry(edge.dst.as_str()).or_default().push(edge);
        }
        let mut model = Self {
            keys: BTreeMap::new(),
            dynamic: Vec::new(),
            files: 0,
            other_sources: 0,
            unplaced: 0,
            loose: 0,
        };
        for node in by_id.values().copied() {
            if node.label == TOOL {
                model.declare(node);
                continue;
            }
            if node.props["source"] != ENV {
                model.other_sources += 1;
                continue;
            }
            let Some((mut read, key)) = Read::place(node) else {
                model.unplaced += 1;
                continue;
            };
            read.sources = sources.remove(node.id.as_str()).unwrap_or_default();
            read.sources.sort_by(|left, right| left.src.cmp(&right.src));
            read.sources.dedup_by(|left, right| left.src == right.src);
            match key {
                Some(key) => model
                    .keys
                    .entry((read.repo, key))
                    .or_default()
                    .reads
                    .push(read),
                None => model.dynamic.push(read),
            }
        }
        model.loose = sources.values().map(Vec::len).sum();
        model
    }

    fn declare(&mut self, tool: &'a Node) {
        let Some((repo, rest)) = scoped(&tool.id, "tool:") else {
            self.unplaced += 1;
            return;
        };
        self.files += 1;
        let path = tool.props["name"].as_str().unwrap_or(rest);
        // A name assigned twice in one file is one declaration.
        let names: BTreeSet<&str> = tool.props["settings"]["keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|name| name.as_str())
            .filter(|name| !name.is_empty())
            .collect();
        for name in names {
            self.keys
                .entry((repo, name))
                .or_default()
                .declared
                .push(Declaration { tool, path });
        }
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
            return f.write_str(if is_env_file(self.0) {
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

/// The symbols and files a `ConfigRead` fact is read from.
struct ReadFrom<'p, 'a>(&'p Read<'a>);
impl fmt::Display for ReadFrom<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.sources.is_empty() {
            return f.write_str("no reading symbol or file among the facts this export admits");
        }
        for (index, edge) in self.0.sources.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            if edge.src.starts_with("file:") {
                f.write_str("file level")?;
            } else {
                // The symbol's name in its file; the whole id when the id
                // does not carry one. The fact index has the id either way.
                let name = edge
                    .src
                    .rsplit_once('#')
                    .map_or(&*edge.src, |(_, name)| name);
                write!(f, "{}", Literal(name))?;
            }
            if weak(&edge.props) {
                f.write_str(" [InferredWeak; a verified-only export leaves this out]")?;
            }
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

fn language(node: &Node) -> &str {
    node.props["language"].as_str().unwrap_or("not recorded")
}

const INTRO: &str = "# Configuration keys\n\n\
A deterministic projection of the configuration keys the ingested sources declare and read: \
each key once, with the environment files that declare it and the files and symbols that read \
it. No model is involved. Only key names are listed: no value, default or fallback is \
shown.\n\n\
## How to read this document\n\n\
- **A key** is the name of an environment variable. Names are compared exactly, including \
case. A key is listed once for each repository: an environment file in one repository says \
nothing about a read in another.\n\
- **Declared** means an environment file among the ingested sources assigns the key. \
Environment files are `.env`, `.env.*`, `*.env`, `env.example`, `env.sample` and \
`env.template`.\n\
- **Read** means a standard-library environment read with that literal name was recovered from \
source code.\n\
- **Read, never declared** means that no environment file in the ingested sources declares the \
key. It does not mean that the key is unset at run time: a deployment platform, a pipeline \
setting, a container definition, a secret store or a shell can set it, and none of those is \
read here.\n\
- **Declared, never read** means that no recovered read names the key. A read whose key is not \
a literal, a read this version does not cover and a consumer outside the ingested sources can \
still use it.\n\
- **Files with reads that cannot be named** read at least one key whose name the source does \
not state as a literal. Those keys exist and are not in the tables.\n\
- Every row carries the tier, confidence and source span of its fact. The fact index lists \
every span. An environment file has no span: these files hold secrets, so no byte range of \
one is stored, except the line of a value its author allowlists (ADR-0075).\n\
- The last sections state which languages are read and what is not covered.\n\n";

const DYNAMIC: &str = "\n## Files with reads that cannot be named\n\n\
Each file below reads at least one key whose name is not recorded: the key is not a string \
literal in the source, or the literal is empty, too long, or shaped like a secret and so is \
not stored. Keys exist in these files that this document cannot name, and any key marked \
declared and never read may be one of them.\n\n\
| Repository | File | Language | Read from | Tier | Confidence | Span | Note |\n\
|---|---|---|---|---|---|---|---|\n";

/// `configuration.md` over the facts the export mode admitted and the count
/// of configuration facts it withheld.
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
    let mut cited: BTreeSet<&str> = BTreeSet::new();
    let reads = model
        .keys
        .values()
        .flat_map(|key| &key.reads)
        .chain(&model.dynamic);
    for key in model.keys.values() {
        for declaration in &key.declared {
            if cited.insert(declaration.tool.id.as_str()) {
                assertions.node(declaration.tool, None)?;
            }
        }
    }
    for read in reads {
        assertions.node(read.node, None)?;
        for edge in &read.sources {
            assertions.edge(edge)?;
        }
    }

    let count = |status: &str| {
        model
            .keys
            .values()
            .filter(|key| key.status() == status)
            .count()
    };
    let mut content = Text::new(&budget, INTRO)?;
    writeln!(
        content,
        "Export mode: `{}`. Keys: {}. Declared and read: {}. Read, never declared: {}. \
         Declared, never read: {}. Environment files: {}. Files with reads that cannot be \
         named: {}.\n",
        match mode {
            ExportMode::VerifiedOnly => "verified-only",
            ExportMode::BestEffort => "best-effort",
        },
        model.keys.len(),
        count("declared and read"),
        count("read, never declared"),
        count("declared, never read"),
        model.files,
        model.dynamic.len(),
    )?;
    match mode {
        ExportMode::VerifiedOnly => writeln!(
            content,
            "Configuration facts left out of this export because they are InferredWeak or \
             rejected: {withheld}. A `best-effort` export includes the InferredWeak ones, \
             marked. A fact left out can change whether a key is listed as declared or read.\n"
        )?,
        ExportMode::BestEffort => writeln!(
            content,
            "InferredWeak rows are included and marked in the Note column. Configuration facts \
             left out of this export because they are rejected: {withheld}.\n"
        )?,
    }
    for (count, what) in [
        (
            model.unplaced,
            "admitted configuration facts are not listed: the fact's id does not name its \
             repository and file",
        ),
        (
            model.other_sources,
            "admitted configuration-read facts are not listed: their source is not the \
             environment, and this version lists environment reads only",
        ),
        (
            model.loose,
            "admitted reading relations are not listed: the read each one points to is not \
             among the facts this export lists",
        ),
    ] {
        if count > 0 {
            writeln!(content, "{count} {what}.\n")?;
        }
    }

    content.push_str(
        "## Keys\n\n\
         | Repository | Key | Status | Declaring files | Reading files | Note |\n\
         |---|---|---|---|---|---|\n",
    )?;
    for ((repo, name), key) in &model.keys {
        write!(
            content,
            "| {} | {} | {} | {} | {} | ",
            Literal(repo),
            Literal(name),
            key.status(),
            key.declared.len(),
            key.reads.len(),
        )?;
        match key.weak() {
            0 => content.push_str("— |\n")?,
            weak => writeln!(
                content,
                "InferredWeak facts among these: {weak}; a verified-only export omits them, \
                 which can change this status |"
            )?,
        }
    }
    if model.keys.is_empty() {
        content.push_str("| — | None in this export | — | 0 | 0 | — |\n")?;
    }

    content.push_str(
        "\n## Where each key is declared\n\n\
         | Repository | Key | Environment file | Tier | Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|\n",
    )?;
    let mut declared = 0usize;
    for ((repo, name), key) in &model.keys {
        for declaration in &key.declared {
            declared += 1;
            writeln!(
                content,
                "| {} | {} | {} | {} | {} |",
                Literal(repo),
                Literal(name),
                Literal(declaration.path),
                Evidence(declaration.tool),
                note(weak(&declaration.tool.props))
            )?;
        }
    }
    if declared == 0 {
        content.push_str("| — | None in this export | — | — | — | — | — |\n")?;
    }

    content.push_str(
        "\n## Where each key is read\n\n\
         One row for each file that reads the key. The span is the first read in the file.\n\n\
         | Repository | Key | File | Language | Read from | Tier | Confidence | Span | Note |\n\
         |---|---|---|---|---|---|---|---|---|\n",
    )?;
    let mut read_rows = 0usize;
    for ((repo, name), key) in &model.keys {
        for read in &key.reads {
            read_rows += 1;
            writeln!(
                content,
                "| {} | {} | {} | {} | {} | {} | {} |",
                Literal(repo),
                Literal(name),
                Literal(read.path),
                Literal(language(read.node)),
                ReadFrom(read),
                Evidence(read.node),
                note(weak(&read.node.props))
            )?;
        }
    }
    if read_rows == 0 {
        content.push_str("| — | None in this export | — | — | — | — | — | — | — |\n")?;
    }

    content.push_str(DYNAMIC)?;
    for read in &model.dynamic {
        writeln!(
            content,
            "| {} | {} | {} | {} | {} | {} |",
            Literal(read.repo),
            Literal(read.path),
            Literal(language(read.node)),
            ReadFrom(read),
            Evidence(read.node),
            note(weak(&read.node.props))
        )?;
    }
    if model.dynamic.is_empty() {
        content.push_str("| — | None recovered | — | — | — | — | — | — |\n")?;
    }

    content.push_str(
        "\n## Languages read\n\n\
         Reads are recovered from the languages below, by the language's own environment read \
         only. Each row states what is read and what is not. The list is the same in every \
         export; it does not say that a repository holds that language.\n\n\
         | Language | What is read, and what is not |\n|---|---|\n",
    )?;
    for (language, cell) in CONFIG_READ_COVERAGE {
        writeln!(content, "| {language} | {cell} |")?;
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
/// Rows hold references only; provenance is decoded one row at a time. A key
/// name an environment file declares is a JSON value of its fact, so the key
/// index is charged by values as well as by rows.
pub(super) fn logical_index_bytes(rows: usize, values: usize) -> Option<usize> {
    use super::accounting::{map_slots, set_slots, slots, sum};
    sum(&[
        map_slots::<&str, &Node>(rows)?,                  // by_id
        map_slots::<&str, Vec<&Edge>>(rows)?,             // READS_CONFIG by read
        slots::<&Edge>(rows)?,                            // their elements
        slots::<Read<'static>>(rows)?,                    // one per ConfigRead
        map_slots::<(&str, &str), Key<'static>>(rows)?,   // keys that are read
        map_slots::<(&str, &str), Key<'static>>(values)?, // keys that are declared
        slots::<Declaration<'static>>(values)?,           // one per declared name
        set_slots::<&str>(values)?,                       // names of one file
        set_slots::<&str>(rows)?,                         // cited environment files
    ])
}

#[cfg(test)]
#[path = "configuration_tests.rs"]
mod tests;
