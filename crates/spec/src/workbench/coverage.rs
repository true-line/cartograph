//! Coverage artifacts (#1786, AC-0592): `coverage.md` and `coverage.json`.
//!
//! Both are projections of the coverage inventories stored with each repo's
//! facts (`core_graph::coverage`, AC-0239), the same records a specialist
//! reads through `read_coverage_inventory`. Nothing is derived from the
//! graph and nothing is filtered by export mode: coverage is a fact about the
//! run, not an inferred statement.
//!
//! Size (#1582): the Markdown carries one row per repository plus estate
//! totals by producer and by reason, and per-repository producer detail only
//! up to [`COVERAGE_DETAIL_REPO_LIMIT`] repositories. The JSON index carries
//! every stored inventory body verbatim, so no site, input path or stopping
//! reason is reachable only through a document that grows with the estate.
//!
//! Stale inventories (#1814, AC-0657): a host that compares each stored
//! inventory with the repository's current registration names the ones
//! recorded under another. Such a repository keeps its row and its inventory
//! hash and is stated to be stale with what was compared. Nothing is read
//! from its inventory, in either artifact, and it enters no total. A host
//! that makes no comparison names none, and its artifacts are unchanged.
//!
//! Plugin refusals (#1810, AC-0658): a gated plugin's row may record the
//! files the host refused its extraction of, by reason class. The Markdown
//! lists those files under the repository's producers and totals them under
//! their own reason. The row's file count never includes them. An inventory
//! that records no refusal renders the bytes it did before.

use super::producers::Output;
use super::render::{Budget, RenderLimit, Safe, Text};
use super::selected::StaleCoverage;
use core_graph::coverage::StoredCoverage;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Most repositories whose per-producer detail `coverage.md` renders. Past
/// it the Markdown keeps its per-repository row and the estate totals, and
/// names `coverage.json` as the place the detail is.
pub const COVERAGE_DETAIL_REPO_LIMIT: usize = 12;

/// Schema of the `coverage.json` body.
const INDEX_SCHEMA: &str = "cartograph.coverage-index/v1";

const NO_INVENTORY: &str = "No stored coverage inventory was supplied to this compilation, so this \
document states nothing about what the trace read. Without one, an absent finding cannot be told \
from an unread file.\n";

/// The recorded inventory body, read leniently: unknown fields are ignored
/// here and still reach the reader through the verbatim body in the index.
#[derive(Deserialize)]
struct Inventory {
    inventory: String,
    commit: String,
    #[serde(default)]
    commit_note: Option<String>,
    layer_hints: Vec<String>,
    producers: Vec<Producer>,
    missing_inputs: Vec<MissingInput>,
    unsupported: Vec<Unsupported>,
    limited: bool,
    /// Documentation files found, read and skipped; absent when the
    /// repository has none or the inventory predates the reader.
    #[serde(default)]
    documents: Option<Documents>,
}

/// What the documentation reader recorded (ADR-0065, AC-0677).
#[derive(Deserialize)]
struct Documents {
    found: u64,
    read: u64,
    #[serde(default)]
    skipped: BTreeMap<String, u64>,
    sections: u64,
    #[serde(default)]
    sections_truncated: u64,
}

/// A documentation skip reason in words; an unknown code is shown as
/// recorded.
fn skip_text(reason: &str) -> &str {
    match reason {
        "too_large" => "larger than the file limit",
        "not_utf8" => "not UTF-8 text",
        "over_file_limit" => "past the per-repository file limit",
        other => other,
    }
}

#[derive(Deserialize)]
struct Producer {
    id: String,
    kind: String,
    covers: String,
    state: String,
    files: u64,
    nodes: u64,
    edges: u64,
    inputs: Vec<String>,
    stopping_reason: String,
    /// Files a gated plugin was refused on; absent when there are none.
    #[serde(default)]
    refusals: Option<Refusals>,
}

/// The bounded list of files one plugin was refused on. The counts by class
/// are read from the stopping reason and the not-covered entries.
#[derive(Deserialize)]
struct Refusals {
    #[serde(default)]
    files: Vec<RefusedFile>,
    /// Refused files the inventory counted but did not list.
    #[serde(default)]
    omitted: u64,
}

#[derive(Deserialize)]
struct RefusedFile {
    path: String,
    reason: String,
}

#[derive(Deserialize)]
struct MissingInput {
    input: String,
    producer: String,
    reason: String,
}

#[derive(Deserialize)]
struct Unsupported {
    capability: String,
    occurrences: u64,
    first_site: String,
    reason: String,
}

const RAN: &str = "ran";
const NOT_SELECTED: &str = "not_selected";
const NOT_APPLICABLE: &str = "not_applicable";
const MISSING_INPUT: &str = "missing_input";
const UNSUPPORTED: &str = "unsupported";
const PLUGIN_REFUSED: &str = "plugin_refused";

/// The not-covered capabilities `ingest::coverage` records for files a
/// gated plugin was refused on, one per reason class.
const REFUSAL_CAPABILITIES: [&str; 3] = [
    "plugin-malformed-fact",
    "plugin-guest-error",
    "plugin-bound-exceeded",
];

/// One stored record with its body read, in repo-key order.
struct Repo<'a> {
    key: &'a str,
    hash: &'a str,
    body: &'a str,
    /// `None` when this version cannot read the body, or when the inventory
    /// is stale and is not read. Such a repository is listed as unread,
    /// never counted as covered or as unlimited.
    inventory: Option<Inventory>,
    /// What the host compared when it found this inventory stale.
    stale: Option<&'a [String]>,
}

fn repos<'a>(records: &'a [StoredCoverage], stale: &[StaleCoverage<'a>]) -> Vec<Repo<'a>> {
    let stale: BTreeMap<&str, &'a [String]> = stale
        .iter()
        .map(|entry| (entry.repo_key, entry.compared))
        .collect();
    let mut repos: Vec<_> = records
        .iter()
        .map(|stored| {
            let stale = stale.get(stored.record.repo_key.as_str()).copied();
            Repo {
                key: &stored.record.repo_key,
                hash: &stored.inventory_hash,
                body: &stored.record.inventory_json,
                // A stale body is not read at all.
                inventory: match stale {
                    Some(_) => None,
                    None => serde_json::from_str(&stored.record.inventory_json).ok(),
                },
                stale,
            }
        })
        .collect();
    // The store reads by repo key already; sorting keeps the artifact
    // independent of the order a caller supplies.
    repos.sort_by(|left, right| (left.key, left.hash).cmp(&(right.key, right.hash)));
    repos
}

/// Estate totals for one producer.
#[derive(Default, Serialize)]
struct ProducerTotal {
    covers: String,
    producer: String,
    kind: String,
    /// Repositories in which it ran.
    ran: u64,
    /// Files it read (deterministic) or declared input files (observational).
    files: u64,
    nodes: u64,
    edges: u64,
    not_selected: u64,
    not_applicable: u64,
    missing_input: u64,
    /// Repositories recording a state this version does not name.
    other: u64,
}

/// Estate totals for one reason something was not read or not covered.
#[derive(Serialize)]
struct ReasonTotal {
    /// `not_selected`, `missing_input`, `unsupported` or, for files a gated
    /// plugin was refused on, `plugin_refused`.
    reason: &'static str,
    /// The producer coverage, manifest input or capability concerned.
    what: String,
    repositories: u64,
    occurrences: u64,
}

struct Totals {
    limited: usize,
    unreadable: usize,
    stale: usize,
    producers: Vec<ProducerTotal>,
    reasons: Vec<ReasonTotal>,
}

fn totals(repos: &[Repo<'_>]) -> Totals {
    let mut producers: BTreeMap<(&str, &str), ProducerTotal> = BTreeMap::new();
    let mut reasons: BTreeMap<(&'static str, &str), (u64, u64)> = BTreeMap::new();
    let mut limited = 0;
    let mut unreadable = 0;
    let mut stale = 0;
    for repo in repos {
        let Some(inventory) = &repo.inventory else {
            if repo.stale.is_some() {
                stale += 1;
            } else {
                unreadable += 1;
            }
            continue;
        };
        limited += usize::from(inventory.limited);
        // Reasons are gathered per repository first, so each counts its repository once.
        let mut seen: BTreeMap<(&'static str, &str), u64> = BTreeMap::new();
        for producer in &inventory.producers {
            let total = producers
                .entry((&producer.covers, &producer.id))
                .or_insert_with(|| ProducerTotal {
                    covers: producer.covers.clone(),
                    producer: producer.id.clone(),
                    kind: producer.kind.clone(),
                    ..ProducerTotal::default()
                });
            match producer.state.as_str() {
                RAN => {
                    total.ran += 1;
                    total.files = total.files.saturating_add(producer.files);
                    total.nodes = total.nodes.saturating_add(producer.nodes);
                    total.edges = total.edges.saturating_add(producer.edges);
                }
                NOT_SELECTED => {
                    total.not_selected += 1;
                    *seen.entry((NOT_SELECTED, &producer.covers)).or_default() += 1;
                }
                NOT_APPLICABLE => total.not_applicable += 1,
                MISSING_INPUT => total.missing_input += 1,
                _ => total.other += 1,
            }
        }
        for missing in &inventory.missing_inputs {
            *seen.entry((MISSING_INPUT, &missing.input)).or_default() += 1;
        }
        for unsupported in &inventory.unsupported {
            let reason = if REFUSAL_CAPABILITIES.contains(&unsupported.capability.as_str()) {
                PLUGIN_REFUSED
            } else {
                UNSUPPORTED
            };
            let occurrences = seen.entry((reason, &unsupported.capability)).or_default();
            *occurrences = occurrences.saturating_add(unsupported.occurrences);
        }
        for (key, occurrences) in seen {
            let total = reasons.entry(key).or_default();
            total.0 += 1;
            total.1 = total.1.saturating_add(occurrences);
        }
    }
    Totals {
        limited,
        unreadable,
        stale,
        producers: producers.into_values().collect(),
        reasons: reasons
            .into_iter()
            .map(
                |((reason, what), (repositories, occurrences))| ReasonTotal {
                    reason,
                    what: what.to_string(),
                    repositories,
                    occurrences,
                },
            )
            .collect(),
    }
}

fn reason_text(reason: &str) -> &'static str {
    match reason {
        NOT_SELECTED => "Producer excluded by the layer hints",
        MISSING_INPUT => "Input not declared",
        PLUGIN_REFUSED => "A gated plugin's extraction of the file was refused",
        _ => "No installed producer covers it, or a tool limit was reached",
    }
}

fn state_text(state: &str) -> &str {
    match state {
        RAN => "ran",
        NOT_SELECTED => "not selected",
        NOT_APPLICABLE => "nothing to read",
        MISSING_INPUT => "missing input",
        other => other,
    }
}

/// What the host compared for a stale inventory, as the artifacts print it.
fn compared_text(compared: &[String]) -> String {
    if compared.is_empty() {
        "nothing recorded".into()
    } else {
        compared.join(", ")
    }
}

/// A refusal reason class as the Markdown prints it. A class this version
/// does not name is printed as recorded.
fn refusal_text(reason: &str) -> &str {
    match reason {
        "malformed_fact" => "the plugin emitted a fact the host does not accept",
        "guest_error" => "the plugin returned an error or trapped",
        "bound_exceeded" => "the plugin exceeded its fuel, memory or table bound",
        other => other,
    }
}

fn list(values: &[String]) -> String {
    if values.is_empty() {
        "—".into()
    } else {
        values.join(", ")
    }
}

fn markdown(repos: &[Repo<'_>], out: &mut Text) -> std::fmt::Result {
    out.push_str("# Coverage\n\n")?;
    if repos.is_empty() {
        return out.push_str(NO_INVENTORY);
    }
    let totals = totals(repos);
    let detailed = repos.len() <= COVERAGE_DETAIL_REPO_LIMIT;
    out.push_str(
        "What the trace read, what it did not, and why, per repository. This is a projection of \
         the coverage inventory stored with each repository's facts, and it is the same in \
         `verified-only` and `best-effort` exports: coverage describes the run and is not an \
         inferred statement. Each inventory hash identifies the stored inventory this document \
         was projected from. A stopping reason is scoped to the repository, revision, layer \
         hints and inputs it names, never to the whole system. An entry that was not covered is \
         a tool limitation, not a gap in the system under study.\n\n",
    )?;
    writeln!(
        out,
        "- Repositories with a stored inventory: {}",
        repos.len()
    )?;
    writeln!(
        out,
        "- Limited (something excluded, missing or not covered): {}",
        totals.limited
    )?;
    writeln!(
        out,
        "- Inventories this version cannot read: {}",
        totals.unreadable
    )?;
    // Stated only by a host that compares inventories with the current
    // registration, so every other export keeps its bytes.
    if totals.stale != 0 {
        writeln!(
            out,
            "- Stale inventories (recorded under a registration or plugin basis that is not the \
             repository's current one; nothing is read from them): {}",
            totals.stale
        )?;
    }
    if detailed {
        out.push_str(
            "- Every stored inventory, with each site, declared input and stopping reason, is in \
             `coverage.json`.\n",
        )?;
    } else {
        writeln!(
            out,
            "- Per-repository producer detail is omitted here past {COVERAGE_DETAIL_REPO_LIMIT} \
             repositories. Every stored inventory, with each site, declared input and stopping \
             reason, is in `coverage.json`."
        )?;
    }

    out.push_str(
        "\n## Repositories\n\nFile reads are summed over the producers that ran, so a file two \
         producers read counts twice.\n\n| Repository | Commit | Inventory hash | Limited | \
         Producers ran | File reads | Not selected | Missing inputs | Not covered (kinds / sites) \
         |\n|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for repo in repos {
        if let Some(compared) = repo.stale {
            writeln!(
                out,
                "| {} | unknown | `{}` | unknown: the stored inventory is stale (compared: {}) | \
                 — | — | — | — | — |",
                Safe(repo.key),
                Safe(repo.hash),
                Safe(compared_text(compared))
            )?;
            continue;
        }
        let Some(inventory) = &repo.inventory else {
            writeln!(
                out,
                "| {} | unknown | `{}` | unknown: this version cannot read the inventory | — | — \
                 | — | — | — |",
                Safe(repo.key),
                Safe(repo.hash)
            )?;
            continue;
        };
        let state = |state: &str| {
            inventory
                .producers
                .iter()
                .filter(|producer| producer.state == state)
                .count()
        };
        let reads = inventory
            .producers
            .iter()
            .filter(|producer| producer.state == RAN)
            .fold(0u64, |sum, producer| sum.saturating_add(producer.files));
        let sites = inventory
            .unsupported
            .iter()
            .fold(0u64, |sum, entry| sum.saturating_add(entry.occurrences));
        writeln!(
            out,
            "| {} | {} | `{}` | {} | {} | {} | {} | {} | {} / {} |",
            Safe(repo.key),
            Safe(&inventory.commit),
            Safe(repo.hash),
            if inventory.limited { "yes" } else { "no" },
            state(RAN),
            reads,
            state(NOT_SELECTED),
            inventory.missing_inputs.len(),
            inventory.unsupported.len(),
            sites
        )?;
    }

    out.push_str(
        "\n## Totals by language and file kind\n\nOne row per producer across every readable \
         inventory. Files are files read by a deterministic producer, or declared input files \
         for an observational one.\n\n| Covers | Producer | Kind | Repositories ran | Files | \
         Nodes | Edges | Not selected | Nothing to read | Missing input | Other state \
         |\n|---|---|---|---|---|---|---|---|---|---|---|\n",
    )?;
    for total in &totals.producers {
        writeln!(
            out,
            "| {} | `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            Safe(&total.covers),
            Safe(&total.producer),
            Safe(&total.kind),
            total.ran,
            total.files,
            total.nodes,
            total.edges,
            total.not_selected,
            total.not_applicable,
            total.missing_input,
            total.other
        )?;
    }

    out.push_str("\n## Totals by reason\n\n")?;
    if totals.reasons.is_empty() {
        out.push_str("No readable inventory records anything excluded, missing or not covered.\n")?;
    } else {
        out.push_str("| Why | What | Repositories | Occurrences |\n|---|---|---|---|\n")?;
        for total in &totals.reasons {
            writeln!(
                out,
                "| {} | {} | {} | {} |",
                reason_text(total.reason),
                Safe(&total.what),
                total.repositories,
                total.occurrences
            )?;
        }
    }

    if !detailed {
        return Ok(());
    }
    out.push_str("\n## Detail by repository\n")?;
    for repo in repos {
        writeln!(out, "\n### {}\n", Safe(repo.key))?;
        writeln!(out, "- Inventory hash: `{}`", Safe(repo.hash))?;
        if let Some(compared) = repo.stale {
            writeln!(
                out,
                "- The stored inventory is stale: it was recorded under a registration or plugin \
                 basis that is not this repository's current one (compared when this document \
                 was read: {}). Nothing is read from it, so nothing is stated about what the \
                 trace read in this repository.",
                Safe(compared_text(compared))
            )?;
            continue;
        }
        let Some(inventory) = &repo.inventory else {
            out.push_str(
                "- This version cannot read the stored inventory, so nothing is stated about \
                 what was read. The stored body is in `coverage.json`.\n",
            )?;
            continue;
        };
        writeln!(out, "- Inventory rules: `{}`", Safe(&inventory.inventory))?;
        match &inventory.commit_note {
            Some(note) => writeln!(
                out,
                "- Commit: {} ({})",
                Safe(&inventory.commit),
                Safe(note)
            )?,
            None => writeln!(out, "- Commit: {}", Safe(&inventory.commit))?,
        }
        if inventory.layer_hints.is_empty() {
            out.push_str("- Layer hints: none, every layer selected\n")?;
        } else {
            writeln!(
                out,
                "- Layer hints: {}",
                Safe(inventory.layer_hints.join(", "))
            )?;
        }
        out.push_str(
            "\n| Covers | Producer | Kind | State | Files | Nodes | Edges | Declared inputs | Why \
             recovery stopped |\n|---|---|---|---|---|---|---|---|---|\n",
        )?;
        for producer in &inventory.producers {
            writeln!(
                out,
                "| {} | `{}` | {} | {} | {} | {} | {} | {} | {} |",
                Safe(&producer.covers),
                Safe(&producer.id),
                Safe(&producer.kind),
                Safe(state_text(&producer.state)),
                producer.files,
                producer.nodes,
                producer.edges,
                Safe(list(&producer.inputs)),
                Safe(&producer.stopping_reason)
            )?;
        }
        // Written only when the inventory records documentation files, so
        // every other inventory keeps its bytes.
        if let Some(documents) = &inventory.documents {
            writeln!(
                out,
                "\nDocumentation files (Markdown and plain text): {} found, {} read into \
                 {} sections, {} not read. These count this repository at this commit; they \
                 do not state that its documentation is complete.",
                documents.found,
                documents.read,
                documents.sections,
                documents.found.saturating_sub(documents.read)
            )?;
            if !documents.skipped.is_empty() || documents.sections_truncated != 0 {
                out.push_str("\n| Documentation not read | Files |\n|---|---|\n")?;
                for (reason, files) in &documents.skipped {
                    writeln!(out, "| {} | {} |", Safe(skip_text(reason)), files)?;
                }
                if documents.sections_truncated != 0 {
                    writeln!(
                        out,
                        "| read to the section limit, later sections left out | {} |",
                        documents.sections_truncated
                    )?;
                }
            }
        }
        // Written only when a refusal is recorded, so every other
        // inventory keeps its bytes.
        let refused = || {
            inventory
                .producers
                .iter()
                .filter_map(|producer| Some((producer, producer.refusals.as_ref()?)))
        };
        if refused().next().is_some() {
            out.push_str(
                "\nFiles a gated plugin's extraction was refused on. No fact from these files by \
                 that plugin joined the graph, and they are not in its file count.\n\n| Producer \
                 | File | Why refused |\n|---|---|---|\n",
            )?;
            for (producer, refusals) in refused() {
                for file in &refusals.files {
                    writeln!(
                        out,
                        "| `{}` | `{}` | {} |",
                        Safe(&producer.id),
                        Safe(&file.path),
                        Safe(refusal_text(&file.reason))
                    )?;
                }
                if refusals.omitted != 0 {
                    writeln!(
                        out,
                        "| `{}` | {} more refused files are counted but not listed | — |",
                        Safe(&producer.id),
                        refusals.omitted
                    )?;
                }
            }
        }
        if inventory.missing_inputs.is_empty() && inventory.unsupported.is_empty() {
            out.push_str("\nNo missing input and nothing not covered is recorded.\n")?;
            continue;
        }
        out.push_str(
            "\n| Not read or not covered | Sites | First site | Why |\n|---|---|---|---|\n",
        )?;
        for missing in &inventory.missing_inputs {
            writeln!(
                out,
                "| Input `{}` for `{}` | — | — | {} |",
                Safe(&missing.input),
                Safe(&missing.producer),
                Safe(&missing.reason)
            )?;
        }
        for entry in &inventory.unsupported {
            writeln!(
                out,
                "| {} | {} | `{}` | {} |",
                Safe(&entry.capability),
                entry.occurrences,
                Safe(&entry.first_site),
                Safe(&entry.reason)
            )?;
        }
    }
    Ok(())
}

/// The stored body exactly as recorded. The store admits only JSON objects;
/// a body that is not JSON is carried as text rather than dropped.
#[derive(Serialize)]
#[serde(untagged)]
enum Body<'a> {
    Json(&'a RawValue),
    Text(&'a str),
    /// A stale inventory's body is not carried: `null`.
    Withheld,
}

/// Why a repository's stored inventory is not read.
#[derive(Serialize)]
struct Stale<'a> {
    /// What the host compared with the repository's current state.
    compared: &'a [String],
}

#[derive(Serialize)]
struct IndexRepo<'a> {
    repo: &'a str,
    /// Content hash of the stored inventory body, as recorded by the store.
    inventory_hash: &'a str,
    /// Whether this version could read the body for the totals.
    readable: bool,
    /// Present only for a stale inventory, whose body is then `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    stale: Option<Stale<'a>>,
    inventory: Body<'a>,
}

#[derive(Serialize)]
struct Index<'a> {
    schema: &'static str,
    inventories: usize,
    limited: usize,
    unreadable: usize,
    /// Present only when a host named a stale inventory.
    #[serde(skip_serializing_if = "is_zero")]
    stale: usize,
    producers: Vec<ProducerTotal>,
    reasons: Vec<ReasonTotal>,
    repositories: Vec<IndexRepo<'a>>,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

fn index(repos: &[Repo<'_>], out: &mut Text) -> Result<(), RenderLimit> {
    let totals = totals(repos);
    let index = Index {
        schema: INDEX_SCHEMA,
        inventories: repos.len(),
        limited: totals.limited,
        unreadable: totals.unreadable,
        stale: totals.stale,
        producers: totals.producers,
        reasons: totals.reasons,
        repositories: repos
            .iter()
            .map(|repo| IndexRepo {
                repo: repo.key,
                inventory_hash: repo.hash,
                readable: repo.inventory.is_some(),
                stale: repo.stale.map(|compared| Stale { compared }),
                inventory: match repo.stale {
                    Some(_) => Body::Withheld,
                    None => serde_json::from_str::<&RawValue>(repo.body)
                        .map_or(Body::Text(repo.body), Body::Json),
                },
            })
            .collect(),
    };
    serde_json::to_writer(out, &index).map_err(|_| RenderLimit)
}

/// `coverage.md` (`markdown`) or `coverage.json` over the stored inventories,
/// refused whole when it would exceed `max`. Neither carries assertions:
/// an inventory entry is a statement about the run, not a graph fact.
/// A repository named in `stale` is listed as stale and its inventory is
/// not read.
pub(super) fn output(
    records: &[StoredCoverage],
    stale: &[StaleCoverage<'_>],
    as_markdown: bool,
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let repos = repos(records, stale);
    let mut content = Text::new(&budget, "")?;
    if as_markdown {
        markdown(&repos, &mut content)?;
    } else {
        index(&repos, &mut content)?;
    }
    Ok(Output {
        content: content.finish(),
        assertions: Vec::new(),
        subjects: Vec::new(),
        attested: Vec::new(),
    })
}

/// Logical workspace of the `coverage` and `coverage-index` producers over
/// stored inventories admitted under `records` rows and `bytes` body bytes,
/// in the currency of [`super::accounting`]. It is added to
/// [`crate::logical_compile_bound`], which models neither the supplied
/// inventories nor these producers. The inventories themselves stay with
/// the caller and are borrowed, never copied. Not an allocator or RSS claim.
pub fn logical_coverage_render_bound(records: usize, bytes: usize) -> Option<usize> {
    use super::accounting::{json_bytes, map_slots, slots, sum};
    sum(&[
        // One row per stored record and the stale-repository lookup.
        slots::<Repo<'_>>(records)?,
        map_slots::<&str, &[String]>(records)?,
        // Every inventory is parsed to its typed body and all are held
        // together: their strings are at most the stored body bytes.
        json_bytes(bytes)?,
        // Totals clone producer, coverage and capability names, at most the
        // stored body bytes again, into rows keyed by borrowed names.
        json_bytes(bytes)?,
        // The index validates one borrowed body at a time.
        json_bytes(bytes.min(core_graph::coverage::MAX_INVENTORY_BYTES))?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ExportMode, SelectedArtifactLimit, SelectedArtifactOutcome, SelectedCompileLimits,
        SpecArtifact, compile_spec, compile_spec_selected_with_coverage,
        compile_spec_with_coverage,
    };
    use core_graph::coverage::CoverageRecord;
    use serde_json::json;
    use std::collections::BTreeSet;

    const RUBY_REASON: &str =
        "no installed producer covers Ruby: a tool limitation, not a system gap";
    const LINK_REASON: &str = "1 symlink(s) in the selected tree were not followed or read; \
         sources they point to are not covered unless also present in the tree";

    /// An inventory body in the shape `ingest::coverage::RepoCoverage`
    /// serializes: one TypeScript file read, one Ruby file no producer covers
    /// and one symlink left unread.
    fn inventory(repo: &str) -> String {
        json!({
            "inventory": "coverage@1",
            "repo": repo,
            "commit": "0123abc",
            "layer_hints": ["server"],
            "producers": [
                {"id": "t0.adapter-ts", "kind": "deterministic", "covers": "TypeScript and JavaScript",
                 "state": "ran", "files": 1, "nodes": 4, "edges": 3, "inputs": [],
                 "stopping_reason": format!("completed over 1 files of {repo}@0123abc for layers server")},
                {"id": "t0.adapter-python", "kind": "deterministic", "covers": "Python",
                 "state": "not_applicable", "files": 0, "nodes": 0, "edges": 0, "inputs": [],
                 "stopping_reason": format!("no Python files in {repo}@0123abc")},
                {"id": "t0.iac-terraform", "kind": "deterministic", "covers": "Terraform",
                 "state": "not_selected", "files": 0, "nodes": 0, "edges": 0, "inputs": [],
                 "stopping_reason": "not selected: layer hints [server] exclude infra and cloud"},
                {"id": "t1.terraform-state", "kind": "observational", "covers": "resource",
                 "state": "missing_input", "files": 0, "nodes": 0, "edges": 0, "inputs": [],
                 "stopping_reason": format!("no state_json declared for {repo}: 2 resource facts stay T0 without observed values")}
            ],
            "missing_inputs": [
                {"input": "state_json", "producer": "t1.terraform-state",
                 "reason": "declare state_json for this repo in cartograph.system.toml"}
            ],
            "unsupported": [
                {"capability": "Ruby", "occurrences": 1, "first_site": "legacy/report.rb:1",
                 "reason": RUBY_REASON},
                {"capability": "symlink-not-followed", "occurrences": 1, "first_site": "CLAUDE.md",
                 "reason": LINK_REASON}
            ],
            "limited": true
        })
        .to_string()
    }

    fn stored(repo: &str, inventory_json: String) -> StoredCoverage {
        let record = CoverageRecord {
            repo_key: repo.into(),
            registered_source_id: Some(format!("source:{repo}")),
            ingest_id: "job:7".into(),
            config_hash: "config".into(),
            inventory_json,
        };
        StoredCoverage {
            inventory_hash: record.inventory_hash(),
            record,
        }
    }

    fn estate(count: usize) -> Vec<StoredCoverage> {
        (0..count)
            .map(|index| {
                let repo = format!("acme/repo-{index:02}");
                stored(&repo, inventory(&repo))
            })
            .collect()
    }

    fn export(records: &[StoredCoverage], mode: ExportMode) -> (SpecArtifact, SpecArtifact) {
        let bundle = compile_spec_with_coverage(&[], &[], &[], mode, &BTreeSet::new(), records);
        let find = |id: &str| {
            bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == id)
                .cloned()
                .expect("official coverage artifact")
        };
        (find("coverage"), find("coverage-index"))
    }

    #[derive(Deserialize)]
    struct ReadRepo {
        repo: String,
        inventory_hash: String,
        readable: bool,
        inventory: Box<RawValue>,
    }

    #[derive(Deserialize)]
    struct ReadIndex {
        schema: String,
        inventories: usize,
        limited: usize,
        unreadable: usize,
        reasons: Vec<serde_json::Value>,
        repositories: Vec<ReadRepo>,
    }

    // AC-0592: on a repo with one unread file and one unsupported language,
    // both artifacts list both with the reasons the inventory recorded.
    #[test]
    fn coverage_artifacts_list_the_unread_file_and_unsupported_language_with_reasons() {
        let records = estate(1);
        let (markdown, index) = export(&records, ExportMode::VerifiedOnly);
        assert_eq!(
            (markdown.file_name.as_str(), markdown.format.as_str()),
            ("coverage.md", "markdown")
        );
        assert_eq!(
            (index.file_name.as_str(), index.format.as_str()),
            ("coverage.json", "json")
        );
        assert!(markdown.verify_identity() && index.verify_identity());
        assert!(
            markdown.content.contains(&format!(
                "| Ruby | 1 | `legacy/report.rb:1` | {RUBY_REASON} |"
            )),
            "{}",
            markdown.content
        );
        assert!(markdown.content.contains(&format!(
            "| symlink-not-followed | 1 | `CLAUDE.md` | {LINK_REASON} |"
        )));
        // Read, excluded and missing are each stated with the recorded reason.
        assert!(markdown.content.contains(
            "| TypeScript and JavaScript | `t0.adapter-ts` | deterministic | ran | 1 | 4 | 3 | — \
             | completed over 1 files of acme/repo-00@0123abc for layers server |"
        ));
        assert!(markdown.content.contains(
            "| Terraform | `t0.iac-terraform` | deterministic | not selected | 0 | 0 | 0 | — | \
             not selected: layer hints [server] exclude infra and cloud |"
        ));
        assert!(markdown.content.contains(
            "| Input `state_json` for `t1.terraform-state` | — | — | declare state_json for this \
             repo in cartograph.system.toml |"
        ));
        // Totals per repository and per reason.
        assert!(markdown.content.contains(&format!(
            "| acme/repo-00 | 0123abc | `{}` | yes | 1 | 1 | 1 | 1 | 2 / 2 |",
            records[0].inventory_hash
        )));
        assert!(markdown.content.contains(
            "| No installed producer covers it, or a tool limit was reached | Ruby | 1 | 1 |"
        ));

        let read: ReadIndex = serde_json::from_str(&index.content).unwrap();
        assert_eq!(read.schema, "cartograph.coverage-index/v1");
        assert_eq!((read.inventories, read.limited, read.unreadable), (1, 1, 0));
        let body: serde_json::Value =
            serde_json::from_str(read.repositories[0].inventory.get()).unwrap();
        assert_eq!(
            body["unsupported"],
            json!([
                {"capability": "Ruby", "occurrences": 1, "first_site": "legacy/report.rb:1",
                 "reason": RUBY_REASON},
                {"capability": "symlink-not-followed", "occurrences": 1,
                 "first_site": "CLAUDE.md", "reason": LINK_REASON}
            ])
        );
        assert!(read.reasons.contains(&json!({
            "reason": "unsupported", "what": "symlink-not-followed",
            "repositories": 1, "occurrences": 1
        })));
    }

    const PLUGIN: &str = "plugin.zig@0123456789ab";
    const MALFORMED_REASON: &str = "2 file(s) were refused for a gated plugin because the plugin \
         emitted a fact the host does not accept; no fact from them by that plugin joined the \
         graph, so that plugin does not cover them: a plugin failure, not a system gap";

    /// [`inventory`] with a gated plugin row. With `refused`, the row is in
    /// the shape `ingest::coverage` records for a plugin refused on three of
    /// five claimed files, one of them past the listing bound.
    fn plugin_inventory(repo: &str, refused: bool) -> String {
        let mut body: serde_json::Value = serde_json::from_str(&inventory(repo)).unwrap();
        let mut row = json!({
            "id": PLUGIN, "kind": "deterministic", "covers": "gated plugin", "state": "ran",
            "files": 2, "nodes": 5, "edges": 3, "inputs": [],
            "stopping_reason": format!("completed over the files its extensions claim in {repo}@0123abc")
        });
        if refused {
            row["stopping_reason"] = json!(format!(
                "read 2 of the 5 files its extensions claim in {repo}@0123abc; the host refused \
                 its extraction of 3 (2 malformed fact, 1 bound exceeded), and no fact from those \
                 files by this plugin joined the graph"
            ));
            row["refusals"] = json!({
                "malformed_fact": 2, "guest_error": 0, "bound_exceeded": 1,
                "files": [
                    {"path": "lib/m.zig", "reason": "malformed_fact"},
                    {"path": "src/z.zig", "reason": "bound_exceeded"}
                ],
                "omitted": 1
            });
            let unsupported = body["unsupported"].as_array_mut().unwrap();
            unsupported.push(json!({
                "capability": "plugin-malformed-fact", "occurrences": 2,
                "first_site": "lib/m.zig", "reason": MALFORMED_REASON
            }));
            unsupported.push(json!({
                "capability": "plugin-bound-exceeded", "occurrences": 1,
                "first_site": "src/z.zig", "reason": "recorded reason"
            }));
        }
        body["producers"].as_array_mut().unwrap().push(row);
        body.to_string()
    }

    // AC-0658: a plugin refused on some files is listed with the refused
    // count by class, the refused files and their class, and a file count
    // that leaves them out. The index carries the recorded refusals verbatim.
    #[test]
    fn coverage_artifacts_list_the_files_a_plugin_was_refused_on() {
        let records = [stored("acme/api", plugin_inventory("acme/api", true))];
        let (markdown, index) = export(&records, ExportMode::VerifiedOnly);
        assert!(
            markdown.content.contains(&format!(
                "| gated plugin | `{PLUGIN}` | deterministic | ran | 2 | 5 | 3 | — | read 2 of \
                 the 5 files its extensions claim in acme/api@0123abc; the host refused its \
                 extraction of 3 (2 malformed fact, 1 bound exceeded), and no fact from those \
                 files by this plugin joined the graph |"
            )),
            "{}",
            markdown.content
        );
        assert!(markdown.content.contains(&format!(
            "\nFiles a gated plugin's extraction was refused on. No fact from these files by that \
             plugin joined the graph, and they are not in its file count.\n\n\
             | Producer | File | Why refused |\n|---|---|---|\n\
             | `{PLUGIN}` | `lib/m.zig` | the plugin emitted a fact the host does not accept |\n\
             | `{PLUGIN}` | `src/z.zig` | the plugin exceeded its fuel, memory or table bound |\n\
             | `{PLUGIN}` | 1 more refused files are counted but not listed | — |\n"
        )));
        assert!(markdown.content.contains(&format!(
            "| plugin-malformed-fact | 2 | `lib/m.zig` | {MALFORMED_REASON} |"
        )));
        // File reads are the TypeScript file and the plugin's two read
        // files: the three refused files are not read files.
        assert!(markdown.content.contains(&format!(
            "| acme/api | 0123abc | `{}` | yes | 2 | 3 | 1 | 1 | 4 / 5 |",
            records[0].inventory_hash
        )));
        assert!(markdown.content.contains(
            "| A gated plugin's extraction of the file was refused | plugin-malformed-fact | 1 \
             | 2 |"
        ));
        assert!(markdown.content.contains(
            "| A gated plugin's extraction of the file was refused | plugin-bound-exceeded | 1 \
             | 1 |"
        ));

        let read: ReadIndex = serde_json::from_str(&index.content).unwrap();
        assert!(read.repositories[0].readable);
        assert_eq!(
            read.repositories[0].inventory.get(),
            records[0].record.inventory_json
        );
        assert!(read.reasons.contains(&json!({
            "reason": "plugin_refused", "what": "plugin-malformed-fact",
            "repositories": 1, "occurrences": 2
        })));
        // The same stored inventory gives the same bytes.
        assert_eq!(
            (markdown, index),
            export(&records, ExportMode::VerifiedOnly)
        );
    }

    // AC-0658: an inventory that records no refusal states none. A plugin
    // row without the field, as stored before the field existed, reads as
    // before and adds nothing to either artifact.
    #[test]
    fn coverage_artifacts_state_no_refusal_when_none_is_recorded() {
        let records = [stored("acme/api", plugin_inventory("acme/api", false))];
        let (markdown, index) = export(&records, ExportMode::VerifiedOnly);
        assert!(markdown.content.contains(&format!(
            "| gated plugin | `{PLUGIN}` | deterministic | ran | 2 | 5 | 3 | — | completed over \
             the files its extensions claim in acme/api@0123abc |"
        )));
        for content in [&markdown.content, &index.content] {
            assert!(!content.contains("refused"), "{content}");
        }
        let read: ReadIndex = serde_json::from_str(&index.content).unwrap();
        assert!(read.repositories[0].readable);
        assert_eq!((read.limited, read.unreadable), (1, 0));

        // The plugin row is the only difference from the same inventory
        // without a plugin.
        let (plain, _) = export(
            &[stored("acme/api", inventory("acme/api"))],
            ExportMode::VerifiedOnly,
        );
        let without_plugin_rows: String = markdown
            .content
            .lines()
            .filter(|line| !line.contains(PLUGIN))
            .map(|line| format!("{line}\n"))
            .collect();
        let hash_line = |content: &str| {
            content
                .lines()
                .filter(|line| !line.contains("Inventory hash") && !line.contains("| acme/api |"))
                .map(|line| format!("{line}\n"))
                .collect::<String>()
        };
        assert_eq!(hash_line(&without_plugin_rows), hash_line(&plain.content));
    }

    // AC-0677 (T-0677): the documentation files the reader found, read and
    // skipped are printed as counts of the repository at its commit; an
    // inventory that records none renders the bytes it did before.
    #[test]
    fn coverage_artifacts_count_the_documentation_files_read_and_skipped() {
        let with = |documents: serde_json::Value| {
            let mut body: serde_json::Value = serde_json::from_str(&inventory("acme/api")).unwrap();
            body["documents"] = documents;
            stored("acme/api", body.to_string())
        };
        let records = [with(json!({
            "found": 7, "read": 4,
            "skipped": {"not_utf8": 1, "over_file_limit": 1, "too_large": 1},
            "sections": 31, "sections_truncated": 2
        }))];
        let (markdown, index) = export(&records, ExportMode::VerifiedOnly);
        assert!(
            markdown.content.contains(
                "\nDocumentation files (Markdown and plain text): 7 found, 4 read into 31 \
                 sections, 3 not read. These count this repository at this commit; they do not \
                 state that its documentation is complete.\n\n\
                 | Documentation not read | Files |\n|---|---|\n\
                 | not UTF-8 text | 1 |\n\
                 | past the per-repository file limit | 1 |\n\
                 | larger than the file limit | 1 |\n\
                 | read to the section limit, later sections left out | 2 |\n"
            ),
            "{}",
            markdown.content
        );
        // The index carries the recorded counts verbatim.
        assert!(index.content.contains("\"sections_truncated\":2"));
        for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
            assert_eq!(export(&records, mode).0.content, markdown.content);
        }

        // Every file read: the counts and no table.
        let all = [with(json!({
            "found": 2, "read": 2, "skipped": {}, "sections": 5, "sections_truncated": 0
        }))];
        let (read, _) = export(&all, ExportMode::VerifiedOnly);
        assert!(
            read.content
                .contains("2 found, 2 read into 5 sections, 0 not read.")
        );
        assert!(!read.content.contains("Documentation not read"));

        // No documentation recorded: nothing is said about it.
        let (plain, _) = export(
            &[stored("acme/api", inventory("acme/api"))],
            ExportMode::VerifiedOnly,
        );
        assert!(!plain.content.contains("Documentation"));
    }

    // AC-0592: both artifacts carry the stored inventory hash, and the index
    // carries the stored body those bytes hash to.
    #[test]
    fn coverage_artifacts_carry_the_stored_inventory_hash() {
        let records = estate(2);
        let (markdown, index) = export(&records, ExportMode::BestEffort);
        let read: ReadIndex = serde_json::from_str(&index.content).unwrap();
        assert_eq!(read.repositories.len(), 2);
        for (stored, listed) in records.iter().zip(&read.repositories) {
            assert_eq!(stored.inventory_hash, stored.record.inventory_hash());
            assert_eq!(listed.repo, stored.record.repo_key);
            assert_eq!(listed.inventory_hash, stored.inventory_hash);
            assert!(listed.readable);
            assert_eq!(listed.inventory.get(), stored.record.inventory_json);
            assert_eq!(
                core_prov::content_hash(listed.inventory.get().as_bytes()),
                stored.inventory_hash
            );
            assert!(
                markdown
                    .content
                    .contains(&format!("- Inventory hash: `{}`", stored.inventory_hash))
            );
        }
        // The job that recorded an inventory is local run state, not part of
        // the projection: re-ingesting the same commit exports the same bytes.
        assert!(!index.content.contains("job:7") && !markdown.content.contains("job:7"));
    }

    // AC-0592: a second export is byte-identical, in either mode, whatever
    // order the records arrive in, and whether compiled alone or in the bundle.
    #[test]
    fn coverage_export_is_byte_identical_across_exports_and_modes() {
        let records = estate(3);
        let first = export(&records, ExportMode::VerifiedOnly);
        assert_eq!(first, export(&records, ExportMode::VerifiedOnly));
        // Nothing is filtered by mode.
        assert_eq!(first, export(&records, ExportMode::BestEffort));
        let reversed: Vec<_> = records.iter().rev().cloned().collect();
        assert_eq!(first, export(&reversed, ExportMode::VerifiedOnly));

        let unbounded = SelectedCompileLimits {
            max_bytes: usize::MAX,
            adr: crate::AdrLimits {
                max_rows: usize::MAX,
                max_output_bytes: usize::MAX,
            },
        };
        let selected = compile_spec_selected_with_coverage(
            &[],
            &[],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
            &["coverage-index", "coverage"],
            unbounded,
            &records,
        )
        .unwrap();
        assert_eq!(
            selected,
            [
                SelectedArtifactOutcome::Produced(first.0.clone()),
                SelectedArtifactOutcome::Produced(first.1.clone())
            ]
        );
        // A selection past its output budget is refused whole, never cut.
        let refused = compile_spec_selected_with_coverage(
            &[],
            &[],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
            &["coverage"],
            SelectedCompileLimits {
                max_bytes: first.0.content.len() + 1,
                ..unbounded
            },
            &records,
        )
        .unwrap();
        assert!(matches!(
            refused[0],
            SelectedArtifactOutcome::Limited {
                limit: SelectedArtifactLimit::OutputBytes { .. },
                ..
            }
        ));
    }

    // AC-0592: past the detail limit the Markdown keeps one row per
    // repository and the totals; every inventory stays in the index.
    #[test]
    fn coverage_markdown_is_bounded_on_a_large_estate() {
        let within = export(
            &estate(COVERAGE_DETAIL_REPO_LIMIT),
            ExportMode::VerifiedOnly,
        )
        .0;
        assert!(within.content.contains("## Detail by repository"));
        assert!(within.content.contains(RUBY_REASON));

        let count = COVERAGE_DETAIL_REPO_LIMIT + 1;
        let records = estate(count);
        let (markdown, index) = export(&records, ExportMode::VerifiedOnly);
        assert!(!markdown.content.contains("## Detail by repository"));
        assert!(!markdown.content.contains("legacy/report.rb"));
        assert!(markdown.content.contains(&format!(
            "Per-repository producer detail is omitted here past {COVERAGE_DETAIL_REPO_LIMIT} \
             repositories."
        )));
        for stored in &records {
            assert!(markdown.content.contains(&format!(
                "| {} | 0123abc | `{}` | yes |",
                stored.record.repo_key, stored.inventory_hash
            )));
        }
        assert!(markdown.content.contains(&format!(
            "| TypeScript and JavaScript | `t0.adapter-ts` | deterministic | {count} | {count} | \
             {} | {} | 0 | 0 | 0 | 0 |",
            count * 4,
            count * 3
        )));
        assert!(markdown.content.contains(&format!(
            "| No installed producer covers it, or a tool limit was reached | Ruby | {count} | \
             {count} |"
        )));
        assert!(markdown.content.contains(&format!(
            "| Producer excluded by the layer hints | Terraform | {count} | {count} |"
        )));
        assert!(markdown.content.contains(&format!(
            "| Input not declared | state_json | {count} | {count} |"
        )));
        // Each further repository adds its one row; only the digits of the
        // totals widen besides.
        let larger = export(&estate(count + 50), ExportMode::VerifiedOnly).0;
        let row = "| acme/repo-00 | 0123abc | `` | yes | 1 | 1 | 1 | 1 | 2 / 2 |\n".len() + 64;
        let growth = larger.content.len() - markdown.content.len();
        assert!((50 * row..50 * row + 16).contains(&growth), "{growth}");

        let read: ReadIndex = serde_json::from_str(&index.content).unwrap();
        assert_eq!(read.repositories.len(), count);
        assert!(
            read.repositories
                .iter()
                .all(|repo| repo.inventory.get().contains("legacy/report.rb:1"))
        );
    }

    // AC-0592: with no inventory supplied the artifacts claim nothing, and an
    // inventory this version cannot read is listed as unread, never dropped
    // or counted as covered.
    #[test]
    fn coverage_artifacts_never_read_a_missing_or_unreadable_inventory_as_covered() {
        let empty = compile_spec(&[], &[], &[], ExportMode::VerifiedOnly, &BTreeSet::new());
        let coverage = empty
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "coverage")
            .unwrap();
        assert_eq!(coverage.content, format!("# Coverage\n\n{NO_INVENTORY}"));
        assert!(coverage.assertions.is_empty());
        assert_eq!(empty, {
            let none: &[StoredCoverage] = &[];
            compile_spec_with_coverage(
                &[],
                &[],
                &[],
                ExportMode::VerifiedOnly,
                &BTreeSet::new(),
                none,
            )
        });

        let future = stored(
            "acme/future",
            json!({"inventory": "coverage@9"}).to_string(),
        );
        let records = [
            future.clone(),
            stored("acme/known", inventory("acme/known")),
        ];
        let (markdown, index) = export(&records, ExportMode::VerifiedOnly);
        assert!(markdown.content.contains(&format!(
            "| acme/future | unknown | `{}` | unknown: this version cannot read the inventory |",
            future.inventory_hash
        )));
        assert!(
            markdown
                .content
                .contains("- Inventories this version cannot read: 1")
        );
        let read: ReadIndex = serde_json::from_str(&index.content).unwrap();
        assert_eq!((read.inventories, read.limited, read.unreadable), (2, 1, 1));
        assert!(!read.repositories[0].readable);
        assert_eq!(
            read.repositories[0].inventory.get(),
            future.record.inventory_json
        );
    }

    // AC-0657 (#1814): a repository whose stored inventory the host names as
    // stale keeps its row and hash and is stated to be stale. Nothing of its
    // inventory is read or carried, it enters no total, and the other
    // repository is unchanged. A host that names none gets the same bytes
    // as before.
    #[test]
    fn coverage_artifacts_state_a_stale_inventory_and_read_nothing_from_it() {
        let records = [
            stored("acme/known", inventory("acme/known")),
            stored("acme/moved", inventory("acme/moved")),
        ];
        let compared = ["registration".to_string(), "plugins".to_string()];
        let compile = |stale: &[crate::StaleCoverage<'_>]| {
            let mut outcomes = crate::compile_spec_selected_supplied(
                &[],
                &[],
                &[],
                ExportMode::VerifiedOnly,
                &BTreeSet::new(),
                &["coverage", "coverage-index"],
                SelectedCompileLimits {
                    max_bytes: usize::MAX,
                    adr: crate::AdrLimits {
                        max_rows: usize::MAX,
                        max_output_bytes: usize::MAX,
                    },
                },
                crate::Supplied {
                    repo_names: &[],
                    knowledge: None,
                    coverage: &records,
                    stale_coverage: stale,
                    curated: crate::Curated::NotSupplied,
                    graph_content_hash: None,
                    source_revisions: None,
                },
            )
            .unwrap()
            .into_iter()
            .map(|outcome| match outcome {
                SelectedArtifactOutcome::Produced(artifact) => artifact,
                SelectedArtifactOutcome::Limited { .. } => panic!("unbounded"),
            });
            (outcomes.next().unwrap(), outcomes.next().unwrap())
        };
        assert_eq!(compile(&[]), export(&records, ExportMode::VerifiedOnly));

        let (markdown, index) = compile(&[crate::StaleCoverage {
            repo_key: "acme/moved",
            compared: &compared,
        }]);
        let hash = &records[1].inventory_hash;
        assert!(
            markdown.content.contains(&format!(
                "| acme/moved | unknown | `{hash}` | unknown: the stored inventory is stale \
                 (compared: registration, plugins) | — | — | — | — | — |"
            )),
            "{}",
            markdown.content
        );
        assert!(markdown.content.contains("- Stale inventories ("));
        assert!(markdown.content.contains(
            "- The stored inventory is stale: it was recorded under a registration or plugin \
             basis that is not this repository's current one"
        ));
        // The current repository is still read; the stale one adds nothing.
        assert!(markdown.content.contains("| acme/known | 0123abc |"));
        assert!(!markdown.content.contains("acme/moved@0123abc"));
        assert!(markdown.content.contains(
            "| No installed producer covers it, or a tool limit was reached | Ruby | 1 | 1 |"
        ));
        assert!(
            markdown
                .content
                .contains("- Inventories this version cannot read: 0")
        );

        let read: serde_json::Value = serde_json::from_str(&index.content).unwrap();
        assert_eq!(read["inventories"], 2);
        assert_eq!(read["stale"], 1);
        assert_eq!(
            (&read["limited"], &read["unreadable"]),
            (&json!(1), &json!(0))
        );
        assert_eq!(read["repositories"][0]["repo"], "acme/known");
        assert!(read["repositories"][0].get("stale").is_none());
        assert_eq!(
            read["repositories"][1],
            json!({
                "repo": "acme/moved", "inventory_hash": hash, "readable": false,
                "stale": {"compared": ["registration", "plugins"]}, "inventory": null
            })
        );
        assert!(!index.content.contains("acme/moved@0123abc"));
    }
}
