//! Per-repo coverage inventory (ADR-0036 §1, AC-0238): which deterministic
//! and observational producers ran, which inputs were missing and which
//! capabilities are unsupported, each with a scoped stopping reason.
//!
//! The inventory is built only from facts the ingest already recorded; it
//! runs no producer. "Completed" always means the declared repo, revision,
//! layer hints and inputs, never global completeness.
//!
//! A language present in the run whose adapter has no producer for an
//! extractor family (target tests, persistence, rule evidence, endpoint
//! auth) is a not-covered capability too (#2164, [`families`]).

use crate::preflight::PreflightReport;
pub use families::{Family, MISSING_FAMILY_CAPABILITY, PRODUCERS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Versioned identity of the inventory rules, recorded on every inventory.
pub const INVENTORY_ID: &str = "coverage@1";

/// Maximum UTF-8 byte length of [`RepoCoverage::commit_note`].
pub const MAX_COMMIT_NOTE_BYTES: usize = 256;

/// Capability naming declarations left without a primary receipt.
pub const RECEIPT_OMISSION_CAPABILITY: &str = "ts-declaration-receipts";
/// Capability naming rule facts left without a primary receipt (#722).
pub const RULE_RECEIPT_OMISSION_CAPABILITY: &str = "ts-rule-receipts";
/// Capability naming receipts the app's receipt store had no room to keep.
pub const STORED_RECEIPT_OMISSION_CAPABILITY: &str = "receipt-store-capacity";
/// Capability naming symlinks the captured TypeScript selection did not follow (#1229).
pub const UNFOLLOWED_LINK_CAPABILITY: &str = "symlink-not-followed";

/// Capability naming found ADR/RFC files over the reader's size bound (#1847).
pub const ADR_TOO_LARGE_CAPABILITY: &str = "adr-file-too-large";
/// Capability naming found ADR/RFC files that are not valid UTF-8 (#1847).
pub const ADR_NOT_UTF8_CAPABILITY: &str = "adr-file-not-utf8";
/// Extractor id of the found ADR/RFC reader (`spec::FOUND_ADR_EXTRACTOR_ID`).
pub const FOUND_ADR_PRODUCER_ID: &str = "t0.adr-markdown";
/// Extractor id of the declared API contract reader (`spec::CONTRACT_EXTRACTOR_ID`).
pub const API_CONTRACT_PRODUCER_ID: &str = "t0.api-contract";

/// Most refused file paths one plugin's [`PluginRefusals::files`] lists
/// (#1810). The rest are counted in [`PluginRefusals::omitted`].
pub const MAX_REFUSED_FILES_LISTED: usize = 256;
/// Most UTF-8 bytes of refused file paths one plugin's
/// [`PluginRefusals::files`] lists, so long paths cannot grow the inventory
/// past its stored bound.
pub const MAX_REFUSED_PATH_BYTES: usize = 32 * 1024;

/// Why the plugin host refused one file's extraction by one plugin (#1810,
/// AC-0658). The class only: the host's error text is guest-controlled and
/// is never recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// The plugin emitted a fact the host does not accept.
    MalformedFact,
    /// The plugin returned an extraction error or trapped.
    GuestError,
    /// The plugin exceeded its fuel, memory or table bound.
    BoundExceeded,
}

impl RefusalReason {
    /// Every class, in report order.
    pub const ALL: [Self; 3] = [Self::MalformedFact, Self::GuestError, Self::BoundExceeded];

    /// The capability naming files refused for this reason in
    /// [`RepoCoverage::unsupported`].
    pub fn capability(self) -> &'static str {
        match self {
            Self::MalformedFact => "plugin-malformed-fact",
            Self::GuestError => "plugin-guest-error",
            Self::BoundExceeded => "plugin-bound-exceeded",
        }
    }

    /// The class as a reader sees it in a stopping reason.
    fn label(self) -> &'static str {
        match self {
            Self::MalformedFact => "malformed fact",
            Self::GuestError => "guest error",
            Self::BoundExceeded => "bound exceeded",
        }
    }

    fn cause(self) -> &'static str {
        match self {
            Self::MalformedFact => "the plugin emitted a fact the host does not accept",
            Self::GuestError => "the plugin returned an error or trapped",
            Self::BoundExceeded => "the plugin exceeded its fuel, memory or table bound",
        }
    }
}

/// One file a plugin was refused on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedFile {
    /// Repo-relative path.
    pub path: String,
    /// Why the host refused it.
    pub reason: RefusalReason,
}

/// The files one plugin was refused on (#1810, AC-0658). No fact from these
/// files by this plugin joined the graph, and they are not counted in the
/// producer's `files`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRefusals {
    /// Files refused for a malformed fact.
    pub malformed_fact: u64,
    /// Files refused because the plugin returned an error or trapped.
    pub guest_error: u64,
    /// Files refused because the plugin exceeded a fuel, memory or table bound.
    pub bound_exceeded: u64,
    /// Refused files in path order, at most [`MAX_REFUSED_FILES_LISTED`]
    /// paths and [`MAX_REFUSED_PATH_BYTES`] path bytes.
    pub files: Vec<RefusedFile>,
    /// Refused files past those bounds, counted above but not listed.
    pub omitted: u64,
}

impl PluginRefusals {
    /// Every refused file, listed or not.
    pub fn total(&self) -> u64 {
        self.malformed_fact
            .saturating_add(self.guest_error)
            .saturating_add(self.bound_exceeded)
    }

    /// Files refused for `reason`.
    pub fn count(&self, reason: RefusalReason) -> u64 {
        match reason {
            RefusalReason::MalformedFact => self.malformed_fact,
            RefusalReason::GuestError => self.guest_error,
            RefusalReason::BoundExceeded => self.bound_exceeded,
        }
    }

    /// The bounded record of `refused`, or `None` when nothing was refused.
    /// Sorted by path, so the record does not depend on the order given.
    fn record(refused: &[(String, RefusalReason)]) -> Option<Self> {
        if refused.is_empty() {
            return None;
        }
        let mut sorted: Vec<&(String, RefusalReason)> = refused.iter().collect();
        sorted.sort();
        let mut refusals = Self::default();
        for (_, reason) in &sorted {
            match reason {
                RefusalReason::MalformedFact => refusals.malformed_fact += 1,
                RefusalReason::GuestError => refusals.guest_error += 1,
                RefusalReason::BoundExceeded => refusals.bound_exceeded += 1,
            }
        }
        let mut bytes = 0usize;
        for (path, reason) in sorted {
            bytes = bytes.saturating_add(path.len());
            if refusals.files.len() == MAX_REFUSED_FILES_LISTED || bytes > MAX_REFUSED_PATH_BYTES {
                break;
            }
            refusals.files.push(RefusedFile {
                path: path.clone(),
                reason: *reason,
            });
        }
        refusals.omitted = refusals.total() - refusals.files.len() as u64;
        Some(refusals)
    }

    /// "2 malformed fact, 1 bound exceeded": the classes that occurred.
    fn by_class(&self) -> String {
        RefusalReason::ALL
            .into_iter()
            .filter(|reason| self.count(*reason) != 0)
            .map(|reason| format!("{} {}", self.count(reason), reason.label()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Why a recovered fact has no retained primary receipt. Each kind is a tool
/// limitation named in coverage, never a gap in the system under study.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptOmission {
    /// Declarations that did not fit once rule receipts were admitted.
    Declarations,
    /// Rule facts whose receipts did not fit the extraction budget.
    Rules,
    /// Receipts the retained receipt store could not keep.
    StoreCapacity,
}

impl From<ReceiptOmission> for core_graph::source::ReceiptOmissionKind {
    fn from(omission: ReceiptOmission) -> Self {
        match omission {
            ReceiptOmission::Declarations => Self::Declarations,
            ReceiptOmission::Rules => Self::Rules,
            ReceiptOmission::StoreCapacity => Self::StoreCapacity,
        }
    }
}

impl From<core_graph::source::ReceiptOmissionKind> for ReceiptOmission {
    fn from(kind: core_graph::source::ReceiptOmissionKind) -> Self {
        match kind {
            core_graph::source::ReceiptOmissionKind::Declarations => Self::Declarations,
            core_graph::source::ReceiptOmissionKind::Rules => Self::Rules,
            core_graph::source::ReceiptOmissionKind::StoreCapacity => Self::StoreCapacity,
        }
    }
}

impl ReceiptOmission {
    fn capability(self) -> &'static str {
        match self {
            Self::Declarations => RECEIPT_OMISSION_CAPABILITY,
            Self::Rules => RULE_RECEIPT_OMISSION_CAPABILITY,
            Self::StoreCapacity => STORED_RECEIPT_OMISSION_CAPABILITY,
        }
    }

    fn reason(self, occurrences: u64, files: usize) -> String {
        let what = match self {
            Self::Declarations => format!(
                "{occurrences} TypeScript declarations in {files} files exceeded the \
                 primary receipt bounds"
            ),
            Self::Rules => format!(
                "{occurrences} TypeScript rule facts in {files} files exceeded the \
                 extraction receipt budget"
            ),
            Self::StoreCapacity => format!(
                "{occurrences} receipts in {files} files did not fit the retained \
                 receipt store (forgetting a retained source frees its receipts, and \
                 a recovery after a source changed frees space once its superseded \
                 receipts are reclaimed, in both cases only receipts that nothing pins \
                 and the current graph no longer binds; a recovery of an unchanged source frees \
                 nothing, and neither frees room under a source's own share limit)"
            ),
        };
        format!(
            "{what} and cannot be cited from captured source: a tool limitation, not a system gap"
        )
    }
}

/// Layer hints that select application producers (SPEC-00 §3).
pub const APPLICATION_LAYERS: &[&str] = &["server", "events", "client"];
/// Layer hints that select infrastructure producers.
pub const INFRA_LAYERS: &[&str] = &["infra", "cloud"];

/// True when `hints` select any of `layers`; no hints select everything.
/// The ingest gates its producers with this, so the inventory and the run
/// can never disagree about what was selected.
pub fn wants(hints: &[String], layers: &[&str]) -> bool {
    hints.is_empty() || layers.iter().any(|layer| hints.iter().any(|h| h == layer))
}

/// Whether a producer reads source deterministically or applies observed
/// runtime or deployment evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerKind {
    /// Reads source or configuration at T0.
    Deterministic,
    /// Applies observed deployment or runtime evidence at T1.
    Observational,
}

/// What happened to one producer in this ingest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerState {
    /// It ran over this repo's matching input.
    Ran,
    /// Layer hints excluded it.
    NotSelected,
    /// Nothing in this repo for it to read or enrich.
    NotApplicable,
    /// It could have enriched facts here, but its input was not declared.
    MissingInput,
}

/// One producer's outcome with its scoped stopping reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerCoverage {
    /// Extractor id as recorded in provenance.
    pub id: String,
    /// Deterministic or observational.
    pub kind: ProducerKind,
    /// Language, format or evidence the producer covers.
    pub covers: String,
    /// Whether it ran, and if not, why.
    pub state: ProducerState,
    /// Files read (deterministic) or declared input files (observational).
    pub files: u64,
    /// Nodes it published.
    pub nodes: u64,
    /// Edges it published.
    pub edges: u64,
    /// Declared input paths, as written in the manifest.
    pub inputs: Vec<String>,
    /// Why recovery stopped here, scoped to the repo, revision, layer hints and inputs.
    pub stopping_reason: String,
    /// Files a gated plugin was refused on (#1810, AC-0658). Absent for
    /// every other producer and for a plugin with no refusal, so such an
    /// inventory keeps the bytes it had before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusals: Option<PluginRefusals>,
}

/// An input whose absence leaves facts at a lower tier than they could reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingInput {
    /// Manifest field that would declare it.
    pub input: String,
    /// Producer that would have applied it.
    pub producer: String,
    /// Why it matters, and how to supply or cover it.
    pub reason: String,
}

/// A construct no installed producer covers: a tool limitation, never a
/// system Gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedCapability {
    /// Uncovered language, or the preflight finding kind.
    pub capability: String,
    /// Sites preflight found.
    pub occurrences: u64,
    /// First occurrence as `path:line`, or the path alone for a file-scoped
    /// limit.
    pub first_site: String,
    /// Why it matters, and how to supply or cover it.
    pub reason: String,
}

/// The coverage inventory of one repo at one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCoverage {
    /// Inventory rules version, [`INVENTORY_ID`].
    pub inventory: String,
    /// Repo identity.
    pub repo: String,
    /// Commit SHA, or `workdir` for a local tree.
    pub commit: String,
    /// Why `commit` is `workdir`, e.g. "3 modified or untracked files"; absent for a commit SHA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_note: Option<String>,
    /// Layer hints the ingest applied; empty selects every layer.
    pub layer_hints: Vec<String>,
    /// Every catalogued producer and gated plugin, in report order.
    pub producers: Vec<ProducerCoverage>,
    /// Inputs that would have enriched facts here.
    pub missing_inputs: Vec<MissingInput>,
    /// Constructs no installed producer covers.
    pub unsupported: Vec<UnsupportedCapability>,
    /// True when any producer was excluded or lacked its input, any
    /// capability is unsupported, or a documentation file was not read in
    /// full. A limited inventory never reads as exhaustive.
    pub limited: bool,
    /// Documentation files found, read and skipped (ADR-0065, AC-0677).
    /// Absent when the repository has no documentation file, so such an
    /// inventory keeps the bytes it had before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<DocumentCoverage>,
}

/// What the documentation reader found in one repository: counts of the
/// declared repository at its commit, never a statement that the
/// documentation is complete.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentCoverage {
    /// Files with a documentation extension.
    pub found: u64,
    /// Files read into a `Document` fact.
    pub read: u64,
    /// Files found and not read, by closed reason code.
    pub skipped: BTreeMap<String, u64>,
    /// `DocSection` facts.
    pub sections: u64,
    /// Files read to the section bound, with later sections left out.
    pub sections_truncated: u64,
}

impl DocumentCoverage {
    /// Whether any file was skipped or read only in part.
    pub fn partial(&self) -> bool {
        self.read != self.found || self.sections_truncated != 0
    }
}

/// Facts one producer published.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProducerCount {
    /// Files it read.
    pub files: u64,
    /// Nodes it published.
    pub nodes: u64,
    /// Edges it published.
    pub edges: u64,
}

/// What one gated plugin read and was refused on in this ingest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginRead {
    /// Pinned extractor id, as recorded in provenance.
    pub id: String,
    /// Files whose extraction succeeded, and the facts they published.
    pub count: ProducerCount,
    /// Files the host refused for this plugin, each with its reason class
    /// (#1810). No fact from them by this plugin joined the graph.
    pub refused: Vec<(String, RefusalReason)>,
}

/// One observational producer's declared input and what it could enrich.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// Extractor id, e.g. `t1.terraform-state`.
    pub producer: &'static str,
    /// Manifest field, e.g. `state_json`.
    pub input: &'static str,
    /// The kind of fact it enriches, for the stopping reason.
    pub enriches: &'static str,
    /// Declared input paths, as written in the manifest.
    pub declared: Vec<String>,
    /// Facts in this repo it could enrich.
    pub targets: u64,
    /// False when the layer hints exclude the layers that read this input,
    /// so a declared input was never read.
    pub selected: bool,
    /// Declared paths, among every observation input of the repo, read
    /// without a capture, so their evidence is unverified (ADR-0080). Only
    /// those this producer declared are named.
    pub uncaptured: Vec<String>,
}

/// Everything the inventory reads, all recorded by the ingest itself.
#[derive(Debug, Clone, Default)]
pub struct CoverageFacts<'a> {
    /// Repo identity.
    pub repo: &'a str,
    /// Commit SHA, or `workdir` for a local tree.
    pub commit: &'a str,
    /// Why `commit` is `workdir`, e.g. "3 modified or untracked files"; absent for a commit SHA.
    pub commit_note: Option<&'a str>,
    /// Layer hints the ingest applied.
    pub layer_hints: &'a [String],
    /// Facts each deterministic producer published, by extractor id. A
    /// producer the catalog names but this map lacks published nothing.
    pub deterministic: Vec<(&'static str, ProducerCount)>,
    /// Gated plugins that ran, with the facts each published and the files
    /// each was refused on.
    pub plugins: Vec<PluginRead>,
    /// Observational producers with their declared inputs.
    pub observations: Vec<Observation>,
}

/// Deterministic producers in report order: extractor id, what it covers and
/// the layers that select it. `None` runs for every layer selection.
const DETERMINISTIC: &[(&str, &str, Option<&[&str]>)] = &[
    ("t0.adapter-ts", "TypeScript and JavaScript", None),
    (
        "t0.webextension",
        "WebExtension manifests",
        Some(APPLICATION_LAYERS),
    ),
    ("t0.adapter-python", "Python", Some(APPLICATION_LAYERS)),
    // Go, Rust and JVM sources carry client calls (net/http, reqwest,
    // Retrofit) as well as server routes.
    ("t0.adapter-go", "Go", Some(APPLICATION_LAYERS)),
    ("t0.adapter-rust", "Rust", Some(APPLICATION_LAYERS)),
    ("t0.adapter-java", "Java", Some(APPLICATION_LAYERS)),
    ("t0.adapter-kotlin", "Kotlin", Some(APPLICATION_LAYERS)),
    // Compiled classes shipped without their source (AC-1215).
    (
        "t0.artifact-jvm",
        "compiled JVM class",
        Some(APPLICATION_LAYERS),
    ),
    ("t0.iac-terraform", "Terraform", Some(INFRA_LAYERS)),
    (
        crate::toolchain::TOOLCHAIN_EXTRACTOR_ID,
        "toolchain configuration",
        None,
    ),
    (
        crate::schedules::SCHEDULE_EXTRACTOR_ID,
        "scheduled jobs",
        None,
    ),
    (
        crate::entry_points::ENTRY_POINT_EXTRACTOR_ID,
        "process entry points",
        None,
    ),
    (
        crate::pipelines::PIPELINE_EXTRACTOR_ID,
        "build and deployment pipeline",
        None,
    ),
    (
        crate::environments::ENVIRONMENT_EXTRACTOR_ID,
        "environments and workloads",
        None,
    ),
    // Stitching consumes every layer's event and client-call sites.
    ("t0.events", "event and client-call site", None),
    // Found decisions and declared API references are read for every layer
    // selection (#1847).
    (FOUND_ADR_PRODUCER_ID, "ADR and RFC Markdown", None),
    (API_CONTRACT_PRODUCER_ID, "declared API reference", None),
];

fn selected(hints: &[String], id: &str, layers: Option<&[&str]>) -> bool {
    match layers {
        Some(layers) => wants(hints, layers),
        // TypeScript runs for application code, or for Pulumi bindings
        // alone when only infrastructure is selected.
        None if id == "t0.adapter-ts" => {
            wants(hints, APPLICATION_LAYERS) || wants(hints, INFRA_LAYERS)
        }
        None => true,
    }
}

impl RepoCoverage {
    /// Build the inventory from recorded facts. Unsupported capabilities are
    /// added from the reconciled preflight report with
    /// [`RepoCoverage::with_unsupported`].
    pub fn from_facts(facts: &CoverageFacts<'_>) -> Self {
        let scope = format!("{}@{}", facts.repo, facts.commit);
        let hints = facts.layer_hints.join(", ");
        let layer_scope = if facts.layer_hints.is_empty() {
            String::new()
        } else {
            format!(" for layers {hints}")
        };
        let mut producers = Vec::new();
        for (id, covers, layers) in DETERMINISTIC {
            let count = facts
                .deterministic
                .iter()
                .find(|(producer, _)| producer == id)
                .map(|(_, count)| *count)
                .unwrap_or_default();
            let (state, stopping_reason) = if !selected(facts.layer_hints, id, *layers) {
                (
                    ProducerState::NotSelected,
                    format!(
                        "not selected: layer hints [{hints}] exclude {}",
                        layers.unwrap_or(INFRA_LAYERS).join(" and ")
                    ),
                )
            } else if count.files == 0 {
                (
                    ProducerState::NotApplicable,
                    format!("no {covers} files in {scope}"),
                )
            } else if *id == "t0.adapter-ts" && !wants(facts.layer_hints, APPLICATION_LAYERS) {
                (
                    ProducerState::Ran,
                    format!(
                        "completed over {} files of {scope} for Pulumi infrastructure \
                         bindings only: application layers were not selected",
                        count.files
                    ),
                )
            } else {
                (
                    ProducerState::Ran,
                    format!(
                        "completed over {} files of {scope}{layer_scope}",
                        count.files
                    ),
                )
            };
            producers.push(ProducerCoverage {
                id: id.to_string(),
                kind: ProducerKind::Deterministic,
                covers: covers.to_string(),
                state,
                files: count.files,
                nodes: count.nodes,
                edges: count.edges,
                inputs: Vec::new(),
                stopping_reason,
                refusals: None,
            });
        }
        for plugin in &facts.plugins {
            let refusals = PluginRefusals::record(&plugin.refused);
            let stopping_reason = match &refusals {
                None => format!("completed over the files its extensions claim in {scope}"),
                // A refused file is not covered by this plugin, and the row
                // says so: `files` counts only the files it read.
                Some(refusals) => format!(
                    "read {} of the {} files its extensions claim in {scope}; the host refused \
                     its extraction of {} ({}), and no fact from those files by this plugin \
                     joined the graph",
                    plugin.count.files,
                    plugin.count.files.saturating_add(refusals.total()),
                    refusals.total(),
                    refusals.by_class()
                ),
            };
            producers.push(ProducerCoverage {
                id: plugin.id.clone(),
                kind: ProducerKind::Deterministic,
                covers: "gated plugin".into(),
                state: ProducerState::Ran,
                files: plugin.count.files,
                nodes: plugin.count.nodes,
                edges: plugin.count.edges,
                inputs: Vec::new(),
                stopping_reason,
                refusals,
            });
        }
        let mut missing_inputs = Vec::new();
        for observation in &facts.observations {
            let (state, stopping_reason) = if !observation.selected {
                let unread = if observation.declared.is_empty() {
                    String::new()
                } else {
                    format!("; declared {} was not read", observation.input)
                };
                (
                    ProducerState::NotSelected,
                    format!(
                        "not selected: layer hints [{hints}] exclude {}{unread}",
                        INFRA_LAYERS.join(" and ")
                    ),
                )
            } else if !observation.declared.is_empty() {
                let uncaptured: Vec<&str> = observation
                    .declared
                    .iter()
                    .filter(|path| observation.uncaptured.contains(path))
                    .map(String::as_str)
                    .collect();
                let unverified = if uncaptured.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; not captured, so unverified (outside the manifest folder or past the capture limits): {}",
                        uncaptured.join(", ")
                    )
                };
                (
                    ProducerState::Ran,
                    format!(
                        "applied {} declared {} file(s) to {} {} facts in {scope}{unverified}",
                        observation.declared.len(),
                        observation.input,
                        observation.targets,
                        observation.enriches
                    ),
                )
            } else if observation.targets == 0 {
                (
                    ProducerState::NotApplicable,
                    format!("no {} facts in {scope} to enrich", observation.enriches),
                )
            } else {
                let reason = format!(
                    "no {} declared for {}: {} {} facts stay T0 without observed values",
                    observation.input, facts.repo, observation.targets, observation.enriches
                );
                missing_inputs.push(MissingInput {
                    input: observation.input.to_string(),
                    producer: observation.producer.to_string(),
                    reason: format!(
                        "{reason}; declare {} for this repo in cartograph.system.toml",
                        observation.input
                    ),
                });
                (ProducerState::MissingInput, reason)
            };
            producers.push(ProducerCoverage {
                id: observation.producer.to_string(),
                kind: ProducerKind::Observational,
                covers: observation.enriches.to_string(),
                state,
                files: observation.declared.len() as u64,
                nodes: 0,
                edges: 0,
                inputs: observation.declared.clone(),
                stopping_reason,
                refusals: None,
            });
        }
        let commit_note = facts
            .commit_note
            .map(|note| note[..note.floor_char_boundary(MAX_COMMIT_NOTE_BYTES)].to_string());
        let mut coverage = RepoCoverage {
            inventory: INVENTORY_ID.to_string(),
            repo: facts.repo.to_string(),
            commit: facts.commit.to_string(),
            commit_note,
            layer_hints: facts.layer_hints.to_vec(),
            producers,
            missing_inputs,
            unsupported: Vec::new(),
            limited: false,
            documents: None,
        };
        coverage.unsupported = coverage.refused_capabilities();
        coverage.refresh_limited();
        coverage
    }

    /// Files gated plugins were refused on, as one not-covered entry per
    /// reason class (#1810, AC-0658). The entry is what makes the inventory
    /// limited: a refused file is never read as covered. Derived from the
    /// producer rows, so it survives [`RepoCoverage::with_unsupported`].
    fn refused_capabilities(&self) -> Vec<UnsupportedCapability> {
        RefusalReason::ALL
            .into_iter()
            .filter_map(|reason| {
                let refused = || self.producers.iter().filter_map(|p| p.refusals.as_ref());
                let occurrences = refused().fold(0u64, |sum, refusals| {
                    sum.saturating_add(refusals.count(reason))
                });
                // A class whose files all fell past the listing bound names
                // the plugin instead of a path.
                let first_site = refused()
                    .flat_map(|refusals| &refusals.files)
                    .filter(|file| file.reason == reason)
                    .map(|file| file.path.as_str())
                    .min()
                    .or_else(|| {
                        self.producers
                            .iter()
                            .find(|p| p.refusals.as_ref().is_some_and(|r| r.count(reason) != 0))
                            .map(|producer| producer.id.as_str())
                    })?;
                Some(UnsupportedCapability {
                    capability: reason.capability().into(),
                    occurrences,
                    first_site: first_site.to_string(),
                    reason: format!(
                        "{occurrences} file(s) were refused for a gated plugin because {}; no \
                         fact from them by that plugin joined the graph, so that plugin does \
                         not cover them: a plugin failure, not a system gap",
                        reason.cause()
                    ),
                })
            })
            .collect()
    }

    /// Add the constructs no installed producer covers, grouped by uncovered
    /// language or finding kind in first-seen order, then the extractor
    /// families no producer covers for a detected language, then the files
    /// gated plugins were refused on.
    pub fn with_unsupported(mut self, report: &PreflightReport) -> Self {
        let mut unsupported: Vec<UnsupportedCapability> = Vec::new();
        for finding in &report.unsupported {
            let capability = finding
                .request_adapter
                .clone()
                .unwrap_or_else(|| finding.kind.clone());
            if let Some(known) = unsupported
                .iter_mut()
                .find(|known| known.capability == capability)
            {
                known.occurrences += 1;
                continue;
            }
            unsupported.push(UnsupportedCapability {
                // A pipeline limit (#1790) and an environment limit (#1791)
                // say what is present and why it was not read; every other
                // kind keeps the general reason.
                reason: if crate::pipelines::is_limit_kind(&finding.kind)
                    || crate::environments::is_limit_kind(&finding.kind)
                {
                    format!("{}: a tool limitation, not a system gap", finding.message)
                } else {
                    format!(
                        "no installed producer covers {capability}: a tool limitation, \
                         not a system gap"
                    )
                },
                capability,
                occurrences: 1,
                first_site: format!("{}:{}", finding.path, finding.line),
            });
        }
        let ran: Vec<&str> = self
            .producers
            .iter()
            .filter(|producer| producer.state == ProducerState::Ran)
            .map(|producer| producer.id.as_str())
            .collect();
        unsupported.extend(families::missing(&report.languages, &ran));
        unsupported.extend(self.refused_capabilities());
        self.unsupported = unsupported;
        self.refresh_limited();
        self
    }

    /// Add facts left without a primary receipt because a receipt bound was
    /// reached (#691, #693, #722), so an uncitable fact is a named limitation
    /// of `kind`, never a silent omission. Apply after
    /// [`RepoCoverage::with_unsupported`], which replaces the list.
    pub fn with_receipt_omissions(
        mut self,
        kind: ReceiptOmission,
        omitted: &std::collections::BTreeMap<String, u64>,
    ) -> Self {
        let occurrences: u64 = omitted.values().sum();
        if let Some(first) = omitted.keys().next()
            && occurrences > 0
        {
            self.unsupported.push(UnsupportedCapability {
                capability: kind.capability().into(),
                occurrences,
                first_site: first.clone(),
                reason: kind.reason(occurrences, omitted.len()),
            });
            self.refresh_limited();
        }
        self
    }

    /// Add the symlinks the captured TypeScript producer did not follow
    /// (#1229), so whatever they point at is a named limitation, never
    /// silently treated as covered. `unfollowed` is sorted. Apply after
    /// [`RepoCoverage::with_unsupported`], which replaces the list.
    pub fn with_unfollowed_links(mut self, unfollowed: &[String]) -> Self {
        if let Some(first) = unfollowed.first() {
            let count = unfollowed.len();
            self.unsupported.push(UnsupportedCapability {
                capability: UNFOLLOWED_LINK_CAPABILITY.into(),
                occurrences: count as u64,
                first_site: first.clone(),
                reason: format!(
                    "{count} symlink(s) in the selected tree were not followed or read; \
                     sources they point to are not covered unless also present in the tree"
                ),
            });
            self.refresh_limited();
        }
        self
    }

    /// Record what the documentation reader found. A repository with no
    /// documentation file records nothing. A skipped or partly read file
    /// makes the inventory limited.
    pub fn with_documents(mut self, documents: DocumentCoverage) -> Self {
        if documents.found != 0 {
            self.documents = Some(documents);
            self.refresh_limited();
        }
        self
    }

    /// Add the found ADR/RFC files the reader did not read (#1847): one
    /// not-covered entry per reason, and the reader's row says how many it
    /// skipped, so a skipped decision is never read as covered. Each list is
    /// sorted. Apply after [`RepoCoverage::with_unsupported`], which replaces
    /// the list.
    pub fn with_unread_adrs(mut self, too_large: &[String], not_utf8: &[String]) -> Self {
        let skipped = too_large.len() + not_utf8.len();
        if skipped == 0 {
            return self;
        }
        for (capability, files, why) in [
            (
                ADR_TOO_LARGE_CAPABILITY,
                too_large,
                "were larger than the 1 MiB ADR bound",
            ),
            (ADR_NOT_UTF8_CAPABILITY, not_utf8, "were not valid UTF-8"),
        ] {
            if let Some(first) = files.first() {
                let count = files.len();
                self.unsupported.push(UnsupportedCapability {
                    capability: capability.into(),
                    occurrences: count as u64,
                    first_site: first.clone(),
                    reason: format!(
                        "{count} ADR or RFC file(s) {why} and were not read; no decision \
                         or governed link from them joined the graph: a tool limitation, \
                         not a system gap"
                    ),
                });
            }
        }
        if let Some(row) = self
            .producers
            .iter_mut()
            .find(|row| row.id == FOUND_ADR_PRODUCER_ID)
            && row.state != ProducerState::NotSelected
        {
            let scope = format!("{}@{}", self.repo, self.commit);
            row.state = ProducerState::Ran;
            row.stopping_reason = format!(
                "read {} of the {} ADR and RFC files in {scope}; {} too large and {} not \
                 UTF-8 were not read",
                row.files,
                row.files.saturating_add(skipped as u64),
                too_large.len(),
                not_utf8.len()
            );
        }
        self.refresh_limited();
        self
    }

    fn refresh_limited(&mut self) {
        self.limited = !self.missing_inputs.is_empty()
            || !self.unsupported.is_empty()
            || self
                .documents
                .as_ref()
                .is_some_and(DocumentCoverage::partial)
            || self
                .producers
                .iter()
                .any(|producer| producer.state == ProducerState::NotSelected);
    }
}

mod families;
#[cfg(test)]
mod tests;
