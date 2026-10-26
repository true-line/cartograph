//! Adapter work packet (#1072, epic #998): for a coverage cell whose adapter
//! lookup (#1071) is `none`, hand the gap off as a normal W6 work packet.
//!
//! ADR-0036: the packet is built from facts and fixtures alone. Every claim
//! in it is either a deterministic observation (the cell, the lookup result,
//! a sampled source span) or an instruction taken verbatim from the adapter
//! contract docs; nothing is a model guess. The packet is the existing
//! `work_packets::PacketBody`, so it is exportable (`export_markdown`) and
//! reviewable like any other. Generating it touches no graph: the cell and
//! sample "facts" live only inside the packet body as evidence.
//!
//! One packet per uncovered language, whose target cells are that language's
//! `none` cells. Sample files are chosen by position in the sorted,
//! `.gitignore`-honouring walk (evenly spaced), so the choice depends only on
//! the file listing; a file is read once, bounded, to compute its byte span.

use crate::adapter_lookup::{CellLookup, LookupStatus};
use crate::matrix::{CORE, Capability, ReportedCell, Support};
use crate::preflight::{LanguageDetection, extensions_for_language, skip_dir};
use core_graph::Node;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::json;
use source_walk::WalkedFile;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use work_packets::{
    AcceptanceProcedure, Component, Dependency, EvidenceLimitation, FactEvidence, Known, Origin,
    PacketBody, PacketDetail, PacketFact, PacketFreshness, PacketLineage, PacketResult,
    PacketReview, SourceIdentity, evidence_id, export_markdown, missing_omission,
};

/// Producer id recorded in provenance and in the packet's generator version.
pub const GENERATOR: &str = "adapter-work-packet@1";
/// Most sample files cited by one packet.
pub const MAX_SAMPLE_FILES: usize = 5;
/// Largest file considered representative; bigger files are skipped unread.
pub const MAX_SAMPLE_FILE_BYTES: u64 = 256 * 1024;
/// Longest byte span cited from one sample file.
pub const MAX_SPAN_BYTES: usize = 4096;
/// Files probed (read) while looking for eligible samples, per language.
const MAX_PROBES: usize = 4 * MAX_SAMPLE_FILES;

/// Identity of the ingested repo and the recovered snapshot a packet is
/// generated against.
#[derive(Debug, Clone, Copy)]
pub struct PacketContext<'a> {
    /// Recovered-graph snapshot id, `context-v1:<64 hex>`.
    pub snapshot_id: &'a str,
    /// Source registration the repo was ingested under.
    pub registration_id: &'a str,
    /// Repo identity (`owner/name`, or `local/<source_id>`).
    pub repo_key: &'a str,
    /// Commit the sample spans are read at.
    pub commit_sha: &'a str,
}

/// The core cells an uncovered language lacks. Preflight reports no cells for
/// a language no adapter covers, so these are the matrix cells a first
/// adapter for it would claim.
pub fn uncovered_cells(languages: &[LanguageDetection]) -> Vec<ReportedCell> {
    languages
        .iter()
        .filter(|language| language.adapter.is_none() && language.coverage.is_empty())
        .flat_map(|language| {
            [Capability::Imports, Capability::Calls, Capability::Types].map(|capability| {
                ReportedCell {
                    language: language.language.clone(),
                    framework: CORE.to_string(),
                    capability: capability.as_str().to_string(),
                    support: Support::Unsupported,
                    note: "no installed adapter covers this language".to_string(),
                }
            })
        })
        .collect()
}

/// Every file the extractors would read under `root`: sorted, symlinks
/// skipped, `.gitignore` honoured, with preflight's directory skips.
pub fn list_repo_files(root: &std::path::Path) -> std::io::Result<Vec<WalkedFile>> {
    source_walk::files(root, &skip_dir, source_walk::Gitignores::Honor)
}

/// A language's display name and its target cells by (framework, capability).
type Target<'a> = (&'a str, BTreeMap<(&'a str, &'a str), &'a ReportedCell>);

/// One packet body per language that has a `none` lookup, ordered by
/// language. A cell that is covered, declared-unverified or supported yields
/// no packet. `files` is the repo's file listing; only the sampled files are
/// opened, and only to compute their byte spans.
pub fn adapter_work_packets(
    lookups: &[CellLookup],
    files: &[WalkedFile],
    context: &PacketContext<'_>,
) -> PacketResult<Vec<PacketBody>> {
    // language (lowercase) -> (display name, (framework, capability) -> cell)
    let mut targets: BTreeMap<String, Target<'_>> = BTreeMap::new();
    for lookup in lookups
        .iter()
        .filter(|lookup| lookup.status == LookupStatus::None)
    {
        let cell = &lookup.cell;
        targets
            .entry(cell.language.to_ascii_lowercase())
            .or_insert_with(|| (cell.language.as_str(), BTreeMap::new()))
            .1
            .insert((cell.framework.as_str(), cell.capability.as_str()), cell);
    }
    targets
        .into_values()
        .map(|(language, cells)| {
            let cells: Vec<&ReportedCell> = cells.into_values().collect();
            build_packet(language, &cells, files, context)
        })
        .collect()
}

/// Render a packet as the standard inert Markdown export, with its content
/// identity. The packet is a draft: unreviewed, execution unoffered.
pub fn export_adapter_packet(body: &PacketBody) -> PacketResult<(String, String)> {
    let packet_id = body.content_id()?;
    let detail = PacketDetail {
        packet_id: packet_id.clone(),
        body: body.clone(),
        review: PacketReview::default(),
        lineage: PacketLineage::default(),
        freshness: PacketFreshness::default(),
        execution: work_packets::ExecutionState::Unoffered,
    };
    Ok((packet_id, export_markdown(&detail)?))
}

fn cell_id(cell: &ReportedCell) -> String {
    format!(
        "{}.{}.{}",
        cell.language.to_ascii_lowercase(),
        cell.framework,
        cell.capability
    )
}

/// A sampled file and the span cited from it.
struct Sample<'a> {
    file: &'a WalkedFile,
    byte_len: u64,
    span_end: u64,
}

fn fact_evidence(node: Node) -> PacketResult<FactEvidence> {
    let fact = PacketFact::Node(node);
    let reference = fact.reference();
    let expected_digest = fact.digest()?;
    let provenance = fact
        .props()
        .get("prov")
        .cloned()
        .and_then(|value| serde_json::from_value::<Provenance>(value).ok());
    Ok(FactEvidence {
        id: evidence_id("fact", &(&reference, &expected_digest))?,
        reference,
        expected_digest,
        fact,
        provenance,
        provenance_problem: None,
        source_association: Known::missing("generated from a file listing, not a captured source"),
        review_annotations: vec![],
    })
}

fn provenance(evidence: Vec<EvidenceRef>, bytes: &[u8]) -> PacketResult<Provenance> {
    Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        evidence,
        GENERATOR,
        bytes,
    )
    .map_err(|_| work_packets::PacketError::InvalidInput("provenance"))
}

fn build_packet(
    language: &str,
    cells: &[&ReportedCell],
    files: &[WalkedFile],
    context: &PacketContext<'_>,
) -> PacketResult<PacketBody> {
    let samples = sample_files(language, files);

    let mut body = PacketBody::draft(context.snapshot_id.to_string());
    body.identity.generator_version = GENERATOR.into();

    // Evidence: one fact per target cell, one per sampled source span.
    let mut cell_facts = Vec::new();
    for cell in cells {
        let id = format!("coverage-cell:{}", cell_id(cell));
        let props = json!({
            "language": cell.language,
            "framework": cell.framework,
            "capability": cell.capability,
            "support": cell.support,
            "note": cell.note,
            "adapter_lookup": "none",
            "prov": provenance(vec![], id.as_bytes())?,
        });
        cell_facts.push(fact_evidence(Node {
            id,
            label: "CoverageCell".into(),
            props,
        })?);
    }
    let mut sample_facts = Vec::new();
    for sample in &samples {
        let path = &sample.file.rel;
        let id = format!("source-sample:{path}");
        let prov = provenance(
            vec![EvidenceRef {
                repo: context.repo_key.into(),
                path: path.clone(),
                byte_start: 0,
                byte_end: sample.span_end,
                commit_sha: context.commit_sha.into(),
            }],
            id.as_bytes(),
        )?;
        sample_facts.push(fact_evidence(Node {
            id,
            label: "SourceSample".into(),
            props: json!({
                "language": language,
                "path": path,
                "byte_len": sample.byte_len,
                "span": {"start": 0, "end": sample.span_end},
                "prov": prov,
            }),
        })?);
    }
    let cell_ids: Vec<String> = cell_facts.iter().map(|f| f.id.clone()).collect();
    let from_cells = || Origin::Evidence {
        evidence_ids: cell_ids.clone(),
    };
    let supplied = |value: String| Known::Supplied {
        value,
        origin: from_cells(),
    };

    let fixtures: Vec<String> = cells
        .iter()
        .map(|cell| format!("crates/ingest/conformance/{}", cell_id(cell)))
        .collect();
    let claims: Vec<String> = cells
        .iter()
        .map(|cell| {
            format!(
                "{}: CoverageCell {{ language: \"{}\", framework: \"{}\", capability: \"{}\", support: Supported }}",
                cell_id(cell),
                cell.language,
                cell.framework,
                cell.capability
            )
        })
        .collect();

    body.identity.sources = vec![SourceIdentity {
        registration_id: context.registration_id.into(),
        repo_key: context.repo_key.into(),
        revision: supplied(context.commit_sha.into()),
    }];

    body.intent.objective = supplied(format!(
        "Make {language} a covered language: write an adapter that claims the {} target cell(s) below, with each conformance fixture written first.",
        cells.len()
    ));
    body.intent.expected_outcome = supplied(
        "Each claimed cell passes its conformance fixture and reads Supported in the coverage matrix; a re-ingest reports the cell as covered."
            .into(),
    );
    body.intent.current_behavior = supplied(format!(
        "No compiled-in adapter or installed plugin declares coverage of these {language} cells (adapter lookup: none), so Preflight reports them as an unsupported tool limitation."
    ));
    body.intent.intended_behavior = supplied(format!("Claim: {}", claims.join("; ")));
    body.intent.scope = supplied(format!(
        "Adapter contract: ADR-0017 plugin host (WASM) or ADR-0003 tree-sitter SPI (compiled-in), per docs/adapters/AUTHORING_GUIDE.md. Target cells: {}.",
        cells
            .iter()
            .map(|c| cell_id(c))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    body.work.affected_components = if samples.is_empty() {
        Known::missing(format!(
            "no {language} source file in the listing is eligible as a representative sample"
        ))
    } else {
        Known::Supplied {
            value: samples
                .iter()
                .zip(&sample_facts)
                .map(|(sample, fact)| Component {
                    repo_key: context.repo_key.into(),
                    path: Some(sample.file.rel.clone()),
                    fact_references: vec![fact.reference.clone()],
                })
                .collect(),
            origin: Origin::Evidence {
                evidence_ids: sample_facts.iter().map(|f| f.id.clone()).collect(),
            },
        }
    };
    body.work.proposed_approach = supplied_list(
        vec![
            format!(
                "Write the conformance fixtures first, each a src/ tree plus expect.json: {}.",
                fixtures.join(", ")
            ),
            "Implement extraction under the chosen adapter contract until every fixture passes."
                .into(),
            "Claim each target cell in the coverage matrix as Supported, naming its fixture."
                .into(),
            "Re-ingest the repo and confirm Preflight reports each target cell as covered.".into(),
        ],
        &cell_ids,
    );
    body.work.dependencies = Known::Supplied {
        value: vec![
            Dependency {
                description: "ADR-0003 tree-sitter adapter SPI (compiled-in adapters)".into(),
                evidence_ids: vec![],
            },
            Dependency {
                description: "ADR-0017 runtime WASM adapter plugin host".into(),
                evidence_ids: vec![],
            },
            Dependency {
                description: "docs/adapters/AUTHORING_GUIDE.md".into(),
                evidence_ids: vec![],
            },
        ],
        origin: from_cells(),
    };
    body.work.open_decisions = supplied_list(
        vec![
            "Adapter contract: ADR-0017 WASM plugin (default; no core PR) or ADR-0003 compiled-in SPI (maintainer path)."
                .into(),
        ],
        &cell_ids,
    );

    let positive: Vec<AcceptanceProcedure> = cells
        .iter()
        .zip(&cell_facts)
        .map(|(cell, fact)| AcceptanceProcedure {
            id: format!("fixture-{}", cell_id(cell)),
            description: format!("Cell {} passes its conformance fixture.", cell_id(cell)),
            steps: vec![
                format!(
                    "Write crates/ingest/conformance/{}/src with representative {language} sources.",
                    cell_id(cell)
                ),
                format!(
                    "Write crates/ingest/conformance/{}/expect.json listing the facts it must yield.",
                    cell_id(cell)
                ),
                "Run the conformance test; the cell's fixture must pass.".into(),
            ],
            expected_evidence: vec![format!("conformance fixture passes for {}", cell_id(cell))],
            evidence_ids: vec![fact.id.clone()],
        })
        .collect();
    body.acceptance.positive_cases = Known::Supplied {
        value: positive,
        origin: from_cells(),
    };
    body.acceptance.negative_cases = Known::Supplied {
        value: vec![AcceptanceProcedure {
            id: "claim-without-fixture".into(),
            description: "A claimed cell with no conformance fixture is rejected.".into(),
            steps: vec!["Run the ingest tests with a cell claimed and its fixture absent.".into()],
            expected_evidence: vec!["the ingest tests fail on the claim with no fixture".into()],
            evidence_ids: cell_ids.clone(),
        }],
        origin: from_cells(),
    };
    body.acceptance.proposed_verification = Known::Supplied {
        value: vec![AcceptanceProcedure {
            id: "matrix-updated".into(),
            description: "The coverage matrix states every claimed cell.".into(),
            steps: vec![
                "Compare the matrix against the target cells.".into(),
                "Re-run Preflight on the repo.".into(),
            ],
            expected_evidence: vec![
                "each target cell reads Supported in the coverage matrix".into(),
                "adapter lookup reports each target cell as covered".into(),
            ],
            evidence_ids: cell_ids.clone(),
        }],
        origin: from_cells(),
    };

    if samples.is_empty() {
        body.evidence.limitations.push(EvidenceLimitation {
            id: "limitation-no-sample-files".into(),
            pointers: vec![language.to_string()],
            omission: missing_omission(format!(
                "no eligible {language} sample file; the fixtures must be authored without a repo excerpt"
            )),
        });
    }
    body.evidence.facts = cell_facts.into_iter().chain(sample_facts).collect();
    body.normalize()?;
    Ok(body)
}

fn supplied_list(value: Vec<String>, evidence_ids: &[String]) -> Known<Vec<String>> {
    Known::Supplied {
        value,
        origin: Origin::Evidence {
            evidence_ids: evidence_ids.to_vec(),
        },
    }
}

/// Up to [`MAX_SAMPLE_FILES`] eligible files of `language`, evenly spaced
/// through the sorted listing and returned in listing order.
fn sample_files<'a>(language: &str, files: &'a [WalkedFile]) -> Vec<Sample<'a>> {
    let extensions: BTreeSet<String> = extensions_for_language(language).into_iter().collect();
    let candidates: Vec<&WalkedFile> = files
        .iter()
        .filter(|file| {
            std::path::Path::new(&file.name)
                .extension()
                .is_some_and(|ext| extensions.contains(&ext.to_string_lossy().to_ascii_lowercase()))
        })
        .collect();
    let wanted = candidates.len().min(MAX_SAMPLE_FILES);
    // Even spread first, then every remaining position, as fallbacks for
    // files that turn out to be ineligible.
    let probe_order: Vec<usize> = (0..wanted)
        .map(|i| i * candidates.len() / wanted)
        .chain(0..candidates.len())
        .collect();
    let mut chosen: BTreeMap<usize, Sample<'a>> = BTreeMap::new();
    let mut probed = BTreeSet::new();
    for index in probe_order {
        if chosen.len() == wanted || probed.len() == MAX_PROBES {
            break;
        }
        if !probed.insert(index) {
            continue;
        }
        if let Some(sample) = read_sample(candidates[index]) {
            chosen.insert(index, sample);
        }
    }
    chosen.into_values().collect()
}

/// Read one file, bounded, to compute the byte span cited from it: the head
/// of the file up to [`MAX_SPAN_BYTES`], cut back to a line boundary.
fn read_sample(file: &WalkedFile) -> Option<Sample<'_>> {
    if !std::fs::symlink_metadata(&file.path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&file.path)
        .ok()?
        .take(MAX_SAMPLE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let byte_len = bytes.len() as u64;
    if bytes.is_empty() || byte_len > MAX_SAMPLE_FILE_BYTES || bytes.contains(&0) {
        return None;
    }
    let mut end = bytes.len().min(MAX_SPAN_BYTES);
    if end < bytes.len()
        && let Some(newline) = bytes[..end].iter().rposition(|b| *b == b'\n')
    {
        end = newline + 1;
    }
    Some(Sample {
        file,
        byte_len,
        span_end: end as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter_lookup::{BuiltinClaim, PluginDeclaration, lookup_coverage};
    use core_graph::source::FactKey;

    const SNAPSHOT: &str =
        "context-v1:0000000000000000000000000000000000000000000000000000000000000001";

    fn context() -> PacketContext<'static> {
        PacketContext {
            snapshot_id: SNAPSHOT,
            registration_id: "src-ruby-fixture",
            repo_key: "local/ruby-fixture",
            commit_sha: "0123456789abcdef0123456789abcdef01234567",
        }
    }

    fn write(root: &std::path::Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// A small Ruby repo with a gitignored file and a non-Ruby file.
    fn ruby_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, ".gitignore", "vendor/\n");
        write(root, "vendor/ignored.rb", "puts 'vendored'\n");
        write(root, "README.md", "# fixture\n");
        write(root, "Gemfile", "source 'https://rubygems.org'\n");
        write(
            root,
            "app/models/user.rb",
            "require 'json'\n\nclass User\n  def name\n    'u'\n  end\nend\n",
        );
        write(
            root,
            "app/models/order.rb",
            "class Order\n  def total\n    1\n  end\nend\n",
        );
        write(
            root,
            "app/controllers/users_controller.rb",
            "class UsersController\n  def index\n    User.new.name\n  end\nend\n",
        );
        write(
            root,
            "lib/util.rb",
            "module Util\n  def self.id(x) = x\nend\n",
        );
        write(root, "empty.rb", "");
        dir
    }

    fn lookups_for(languages: &[LanguageDetection]) -> Vec<CellLookup> {
        lookup_coverage(&uncovered_cells(languages), &[], &[])
    }

    fn ruby_detection() -> Vec<LanguageDetection> {
        vec![LanguageDetection {
            language: "Ruby".into(),
            files: 5,
            adapter: None,
            coverage: vec![],
        }]
    }

    fn generate(root: &std::path::Path) -> Vec<PacketBody> {
        let files = list_repo_files(root).unwrap();
        adapter_work_packets(&lookups_for(&ruby_detection()), &files, &context()).unwrap()
    }

    #[test]
    fn golden_exported_packet_for_an_uncovered_ruby_repo() {
        let repo = ruby_repo();
        let packets = generate(repo.path());
        assert_eq!(packets.len(), 1);
        let (id, markdown) = export_adapter_packet(&packets[0]).unwrap();
        assert!(id.starts_with("work-packet-v1:"));
        let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/adapter_work_packet_ruby.md");
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &markdown).unwrap();
        }
        let expected = std::fs::read_to_string(&golden).expect("golden file is checked in");
        assert_eq!(markdown, expected);
    }

    #[test]
    fn the_packet_is_byte_identical_across_runs_and_roots() {
        let first = ruby_repo();
        let second = ruby_repo();
        let a = export_adapter_packet(&generate(first.path())[0]).unwrap();
        let b = export_adapter_packet(&generate(first.path())[0]).unwrap();
        let c = export_adapter_packet(&generate(second.path())[0]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn the_packet_validates_and_round_trips_as_a_normal_w6_packet() {
        let repo = ruby_repo();
        let body = generate(repo.path()).remove(0);
        body.validate().unwrap();
        let bytes = body.canonical_bytes().unwrap();
        assert_eq!(
            PacketBody::from_json(&serde_json::to_vec(&body).unwrap()).unwrap(),
            body
        );
        assert!(!bytes.is_empty());
    }

    #[test]
    fn samples_cite_repo_relative_paths_with_in_file_byte_spans_and_skip_ignored_files() {
        let repo = ruby_repo();
        let body = generate(repo.path()).remove(0);
        let cited: Vec<_> = body
            .evidence
            .facts
            .iter()
            .filter(|fact| {
                fact.provenance
                    .as_ref()
                    .is_some_and(|p| !p.evidence.is_empty())
            })
            .flat_map(|fact| fact.provenance.as_ref().unwrap().evidence.clone())
            .collect();
        let paths: Vec<_> = cited.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "app/controllers/users_controller.rb",
                "app/models/order.rb",
                "app/models/user.rb",
                "lib/util.rb"
            ]
        );
        for evidence in &cited {
            let len = std::fs::metadata(repo.path().join(&evidence.path))
                .unwrap()
                .len();
            assert!(evidence.byte_end > evidence.byte_start && evidence.byte_end <= len);
            assert!(!evidence.path.starts_with('/'));
        }
    }

    #[test]
    fn the_sample_is_bounded_and_spread_in_stable_order() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..40 {
            write(dir.path(), &format!("src/f{i:02}.rb"), "x = 1\n");
        }
        let files = list_repo_files(dir.path()).unwrap();
        let samples = sample_files("Ruby", &files);
        assert_eq!(samples.len(), MAX_SAMPLE_FILES);
        let names: Vec<_> = samples.iter().map(|s| s.file.rel.as_str()).collect();
        assert_eq!(
            names,
            [
                "src/f00.rb",
                "src/f08.rb",
                "src/f16.rb",
                "src/f24.rb",
                "src/f32.rb"
            ]
        );
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn a_long_file_is_cited_up_to_a_line_boundary_within_the_span_bound() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "big.rb", &"puts 'line'\n".repeat(1000));
        let files = list_repo_files(dir.path()).unwrap();
        let samples = sample_files("Ruby", &files);
        assert_eq!(samples.len(), 1);
        assert!(samples[0].span_end as usize <= MAX_SPAN_BYTES);
        assert_eq!(samples[0].span_end % 12, 0);
        // Oversized files are not representative samples.
        write(
            dir.path(),
            "huge.rb",
            &"a\n".repeat(MAX_SAMPLE_FILE_BYTES as usize),
        );
        let files = list_repo_files(dir.path()).unwrap();
        let names: Vec<_> = sample_files("Ruby", &files)
            .iter()
            .map(|s| s.file.rel.clone())
            .collect();
        assert_eq!(names, ["big.rb"]);
    }

    #[test]
    fn a_covered_or_supported_cell_produces_no_packet() {
        let repo = ruby_repo();
        let files = list_repo_files(repo.path()).unwrap();
        let cells = uncovered_cells(&ruby_detection());
        let claim = |cell: &ReportedCell| ReportedCell {
            support: Support::Supported,
            note: String::new(),
            ..cell.clone()
        };
        // Every cell covered by a gated plugin, then by an ungated one.
        for gate_passed in [true, false] {
            let plugins = [PluginDeclaration {
                id: "t0.plugin-ruby".into(),
                version: "hash".into(),
                gate_passed,
                cells: cells.iter().map(claim).collect(),
            }];
            let lookups = lookup_coverage(&cells, &[], &plugins);
            assert!(
                adapter_work_packets(&lookups, &files, &context())
                    .unwrap()
                    .is_empty()
            );
        }
        let builtins: Vec<_> = cells
            .iter()
            .map(|cell| BuiltinClaim {
                adapter: "t0.adapter-ruby".into(),
                cell: claim(cell),
            })
            .collect();
        let lookups = lookup_coverage(&cells, &builtins, &[]);
        assert!(
            adapter_work_packets(&lookups, &files, &context())
                .unwrap()
                .is_empty()
        );
        // Already-supported cells are not even looked up.
        let supported: Vec<_> = cells.iter().map(claim).collect();
        assert!(lookup_coverage(&supported, &[], &[]).is_empty());
    }

    #[test]
    fn only_the_uncovered_cells_become_target_cells() {
        let repo = ruby_repo();
        let files = list_repo_files(repo.path()).unwrap();
        let cells = uncovered_cells(&ruby_detection());
        let plugins = [PluginDeclaration {
            id: "t0.plugin-ruby".into(),
            version: "hash".into(),
            gate_passed: true,
            cells: vec![ReportedCell {
                support: Support::Supported,
                note: String::new(),
                ..cells[0].clone()
            }],
        }];
        let lookups = lookup_coverage(&cells, &[], &plugins);
        let body = adapter_work_packets(&lookups, &files, &context())
            .unwrap()
            .remove(0);
        let ids: Vec<_> = body
            .evidence
            .facts
            .iter()
            .filter_map(|f| match &f.reference {
                FactKey::Node { id } => id.strip_prefix("coverage-cell:").map(str::to_string),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["ruby.core.calls", "ruby.core.types"]);
    }

    #[test]
    fn packets_are_ordered_by_language_whatever_the_lookup_order() {
        let repo = ruby_repo();
        write(repo.path(), "main.php", "<?php echo 1;\n");
        let files = list_repo_files(repo.path()).unwrap();
        let languages = vec![
            LanguageDetection {
                language: "Ruby".into(),
                files: 4,
                adapter: None,
                coverage: vec![],
            },
            LanguageDetection {
                language: "PHP".into(),
                files: 1,
                adapter: None,
                coverage: vec![],
            },
        ];
        let mut lookups = lookups_for(&languages);
        let forward = adapter_work_packets(&lookups, &files, &context()).unwrap();
        lookups.reverse();
        let reversed = adapter_work_packets(&lookups, &files, &context()).unwrap();
        assert_eq!(forward, reversed);
        let objectives: Vec<_> = forward
            .iter()
            .map(|b| b.intent.objective.value().unwrap().clone())
            .collect();
        assert!(objectives[0].contains("PHP") && objectives[1].contains("Ruby"));
    }

    #[test]
    fn a_language_without_sample_files_still_yields_a_packet_with_a_limitation() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "README.md", "# nothing\n");
        let files = list_repo_files(dir.path()).unwrap();
        let body = adapter_work_packets(&lookups_for(&ruby_detection()), &files, &context())
            .unwrap()
            .remove(0);
        assert_eq!(body.evidence.limitations.len(), 1);
        assert!(body.work.affected_components.value().is_none());
        body.validate().unwrap();
    }

    #[test]
    fn generation_leaves_the_repo_untouched() {
        let repo = ruby_repo();
        let before = list_repo_files(repo.path()).unwrap();
        let contents: Vec<_> = before
            .iter()
            .map(|f| std::fs::read(&f.path).unwrap())
            .collect();
        generate(repo.path());
        let after = list_repo_files(repo.path()).unwrap();
        assert_eq!(before, after);
        assert_eq!(
            contents,
            after
                .iter()
                .map(|f| std::fs::read(&f.path).unwrap())
                .collect::<Vec<_>>()
        );
    }
}
