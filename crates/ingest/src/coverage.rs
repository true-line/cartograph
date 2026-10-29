//! Per-repo coverage inventory (ADR-0036 §1, AC-0238): which deterministic
//! and observational producers ran, which inputs were missing and which
//! capabilities are unsupported, each with a scoped stopping reason.
//!
//! The inventory is built only from facts the ingest already recorded; it
//! runs no producer. "Completed" always means the declared repo, revision,
//! layer hints and inputs, never global completeness.

use crate::preflight::PreflightReport;
use serde::{Deserialize, Serialize};

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
                 receipt store, which cannot yet reclaim space"
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
    /// True when any producer was excluded or lacked its input, or any
    /// capability is unsupported. A limited inventory never reads as
    /// exhaustive.
    pub limited: bool,
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
    /// Gated plugins that ran, by id, with the facts each published.
    pub plugins: Vec<(String, ProducerCount)>,
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
    // Stitching consumes every layer's event and client-call sites.
    ("t0.events", "event and client-call site", None),
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
            });
        }
        for (plugin, count) in &facts.plugins {
            producers.push(ProducerCoverage {
                id: plugin.clone(),
                kind: ProducerKind::Deterministic,
                covers: "gated plugin".into(),
                state: ProducerState::Ran,
                files: count.files,
                nodes: count.nodes,
                edges: count.edges,
                inputs: Vec::new(),
                stopping_reason: format!(
                    "completed over the files its extensions claim in {scope}"
                ),
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
                (
                    ProducerState::Ran,
                    format!(
                        "applied {} declared {} file(s) to {} {} facts in {scope}",
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
        };
        coverage.refresh_limited();
        coverage
    }

    /// Add the constructs no installed producer covers, grouped by uncovered
    /// language or finding kind in first-seen order.
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
                reason: format!(
                    "no installed producer covers {capability}: a tool limitation, \
                     not a system gap"
                ),
                capability,
                occurrences: 1,
                first_site: format!("{}:{}", finding.path, finding.line),
            });
        }
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

    fn refresh_limited(&mut self) {
        self.limited = !self.missing_inputs.is_empty()
            || !self.unsupported.is_empty()
            || self
                .producers
                .iter()
                .any(|producer| producer.state == ProducerState::NotSelected);
    }
}

#[cfg(test)]
mod tests;
