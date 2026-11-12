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

use super::producers::Output;
use super::render::{Budget, RenderLimit, Safe, Text};
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

/// One stored record with its body read, in repo-key order.
struct Repo<'a> {
    key: &'a str,
    hash: &'a str,
    body: &'a str,
    /// `None` when this version cannot read the body. Such a repository is
    /// listed as unread, never counted as covered or as unlimited.
    inventory: Option<Inventory>,
}

fn repos(records: &[StoredCoverage]) -> Vec<Repo<'_>> {
    let mut repos: Vec<_> = records
        .iter()
        .map(|stored| Repo {
            key: &stored.record.repo_key,
            hash: &stored.inventory_hash,
            body: &stored.record.inventory_json,
            inventory: serde_json::from_str(&stored.record.inventory_json).ok(),
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
    /// `not_selected`, `missing_input` or `unsupported`.
    reason: &'static str,
    /// The producer coverage, manifest input or capability concerned.
    what: String,
    repositories: u64,
    occurrences: u64,
}

struct Totals {
    limited: usize,
    unreadable: usize,
    producers: Vec<ProducerTotal>,
    reasons: Vec<ReasonTotal>,
}

fn totals(repos: &[Repo<'_>]) -> Totals {
    let mut producers: BTreeMap<(&str, &str), ProducerTotal> = BTreeMap::new();
    let mut reasons: BTreeMap<(&'static str, &str), (u64, u64)> = BTreeMap::new();
    let mut limited = 0;
    let mut unreadable = 0;
    for repo in repos {
        let Some(inventory) = &repo.inventory else {
            unreadable += 1;
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
            let occurrences = seen
                .entry(("unsupported", &unsupported.capability))
                .or_default();
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
}

#[derive(Serialize)]
struct IndexRepo<'a> {
    repo: &'a str,
    /// Content hash of the stored inventory body, as recorded by the store.
    inventory_hash: &'a str,
    /// Whether this version could read the body for the totals.
    readable: bool,
    inventory: Body<'a>,
}

#[derive(Serialize)]
struct Index<'a> {
    schema: &'static str,
    inventories: usize,
    limited: usize,
    unreadable: usize,
    producers: Vec<ProducerTotal>,
    reasons: Vec<ReasonTotal>,
    repositories: Vec<IndexRepo<'a>>,
}

fn index(repos: &[Repo<'_>], out: &mut Text) -> Result<(), RenderLimit> {
    let totals = totals(repos);
    let index = Index {
        schema: INDEX_SCHEMA,
        inventories: repos.len(),
        limited: totals.limited,
        unreadable: totals.unreadable,
        producers: totals.producers,
        reasons: totals.reasons,
        repositories: repos
            .iter()
            .map(|repo| IndexRepo {
                repo: repo.key,
                inventory_hash: repo.hash,
                readable: repo.inventory.is_some(),
                inventory: serde_json::from_str::<&RawValue>(repo.body)
                    .map_or(Body::Text(repo.body), Body::Json),
            })
            .collect(),
    };
    serde_json::to_writer(out, &index).map_err(|_| RenderLimit)
}

/// `coverage.md` (`markdown`) or `coverage.json` over the stored inventories,
/// refused whole when it would exceed `max`. Neither carries assertions:
/// an inventory entry is a statement about the run, not a graph fact.
pub(super) fn output(
    records: &[StoredCoverage],
    as_markdown: bool,
    max: Option<usize>,
) -> Result<Output, RenderLimit> {
    let budget = Budget::new(max)?;
    let repos = repos(records);
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
    })
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
}
