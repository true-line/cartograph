//! AC-0845 (T-0845): the export, the Workbench read and an MCP session list
//! the findings accepted as curated from one projection, under one
//! revision, and follow the export mode the same way.

use super::*;
use crate::mcp_reads::ReadSession;
use crate::mcp_reads::artifacts::ArtifactCompilation;
use agents::draft_store::DraftStore;
use agents::finding_curation::{CitedFact, CuratedFindingRecord, CurationDecision};
use agents::investigation::{FindingReviewSubjectV1, InvestigationClaimKind};
use agents::{DecisionLog, TaskFactKey};
use core_graph::coverage::CoverageReadLimits;
use core_graph::{FactKey, GraphStore, SnapshotReadLimits};
use core_prov::EvidenceRef;
use serde_json::{Value, json};
use spec::ExportMode;

const HEADING: &str = "\n## Findings accepted as curated\n";

struct Fixture {
    _dir: tempfile::TempDir,
    graph: Mutex<SqliteGraphStore>,
    decisions: Mutex<DecisionLog>,
    reviews: StoredReviews,
}

fn rule(id: &str, note: &str) -> Node {
    let prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: "fixture/repo".into(),
            path: "src/app.ts".into(),
            byte_start: 0,
            byte_end: 8,
            commit_sha: "fixed".into(),
        }],
        "fixture",
        id.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: "Rule".into(),
        props: json!({ "name": id, "note": note, "prov": prov }),
    }
}

fn citation(node: &str) -> String {
    spec::citation_id(&FactKey::Node { id: node.into() })
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    for id in ["rule:refund", "rule:return"] {
        graph.put_node(&rule(id, "as read")).unwrap();
    }
    Fixture {
        graph: Mutex::new(graph),
        decisions: Mutex::new(DecisionLog::open(":memory:").unwrap()),
        reviews: StoredReviews {
            proposals: Mutex::new(
                agents::ProposalStore::open(dir.path().join("proposals.sqlite")).unwrap(),
            ),
            drafts: Mutex::new(DraftStore::open(dir.path().join("section-drafts.sqlite")).unwrap()),
        },
        _dir: dir,
    }
}

impl Fixture {
    /// Accept as curated a finding that cites `node` as the graph holds it
    /// now. `digit` makes its subject.
    fn accept(&self, finding: &str, digit: char, node: &str) -> CuratedFindingRecord {
        let (nodes, _) = self.graph.lock().unwrap().read_snapshot().unwrap();
        let cited = nodes.iter().find(|have| have.id == node).unwrap();
        let record = CuratedFindingRecord {
            subject: FindingReviewSubjectV1 {
                investigation_id: "investigation-1".into(),
                result_id: "result-1".into(),
                finding_id: finding.into(),
                subject_id: format!("investigation-finding-review-v1:{}{digit}", "0".repeat(63)),
            },
            role: "domain-analyst@2".into(),
            claim_kind: InvestigationClaimKind::ImplementedBehavior,
            title: format!("Title of {finding}"),
            statement: format!("Statement of {finding}."),
            limitations: vec!["Partial refunds were not read.".into()],
            cited: vec![CitedFact {
                citation: citation(node),
                fact: TaskFactKey::Node { id: node.into() },
                digest: core_graph::source::node_digest(cited).unwrap(),
                captured: false,
            }],
        };
        self.reviews
            .drafts
            .lock()
            .unwrap()
            .finding_curation()
            .review(&record, 0, CurationDecision::Accepted)
            .unwrap();
        record
    }

    fn session(&self, mode: ExportMode) -> ReadSession {
        ReadSession::acquire_with_overlay(
            &self.graph,
            None,
            &self.decisions,
            &self.reviews,
            || Ok(vec![]),
            SnapshotReadLimits::default(),
            CoverageReadLimits::default(),
            mode,
        )
        .unwrap()
    }
}

fn read(session: &ReadSession, id: &str) -> spec::SpecArtifact {
    let trace = flowtracer::TraceLimits {
        max_flows: 64,
        max_hops: 512,
        max_output_bytes: 1024 * 1024,
    };
    let compile = spec::SelectedCompileLimits {
        max_bytes: 1024 * 1024,
        adr: spec::AdrLimits {
            max_rows: 1024,
            max_output_bytes: 1024 * 1024,
        },
    };
    match session.compile_artifact(id, trace, compile).unwrap() {
        ArtifactCompilation::Produced(artifact) => artifact.artifact().clone(),
        limited => panic!("{id}: {limited:?}"),
    }
}

#[test]
fn the_export_the_workbench_and_an_mcp_session_list_the_same_findings_under_one_revision() {
    let fixture = fixture();
    let eligible = fixture.accept("finding-ok", '1', "rule:refund");
    let stale = fixture.accept("finding-stale", '2', "rule:return");
    fixture
        .graph
        .lock()
        .unwrap()
        .put_node(&rule("rule:return", "changed since"))
        .unwrap();

    let mut revisions = Vec::new();
    for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
        let shown = mode == ExportMode::BestEffort;
        let (bundle, _, workbench) = crate::build_spec_with_views_and_curated(
            &fixture.graph.lock().unwrap(),
            &fixture.decisions.lock().unwrap(),
            &fixture.reviews.gather(),
            mode,
        )
        .unwrap();
        let revision = bundle.curated_basis.as_ref().unwrap().revision.clone();
        let document = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "curated")
            .unwrap();

        // `curated.md`: the eligible finding in best-effort only, the stale
        // one listed without its statement in both.
        let start = document.content.find(HEADING).unwrap();
        let section = &document.content[start..];
        assert!(
            section.contains("- Accepted findings: 2. Eligible: 1. Stale: 1. Unresolvable: 0.")
        );
        assert_eq!(section.contains(&eligible.statement), shown);
        assert_eq!(section.contains(&citation("rule:refund")), shown);
        assert!(!document.content.contains(&stale.statement));
        assert!(section.contains(&format!(
            "| {} | investigation-1 | result-1 | stale |",
            stale.subject.subject_id.replace('_', "\\_")
        )));

        // The Workbench read, from the projection that compile computed.
        let workbench = serde_json::to_value(&workbench).unwrap();
        assert_eq!(workbench["status"], "computed");
        assert_eq!(workbench["revision"], revision.as_str());
        assert_eq!(
            workbench["finding_counts"],
            json!({"accepted": 2, "eligible": 1, "unresolvable": 0, "stale": 1})
        );
        assert_eq!(workbench["findings_omitted"], usize::from(!shown));
        let rows = workbench["findings"].as_array().unwrap();
        assert_eq!(rows.len(), usize::from(shown));
        if let [row] = rows.as_slice() {
            assert_eq!(row["finding_ref"], eligible.subject.subject_id.as_str());
            assert_eq!(row["finding_id"], "finding-ok");
            assert_eq!(row["statement"], eligible.statement.as_str());
            assert_eq!(row["title"], eligible.title.as_str());
            assert_eq!(row["kind"], "implemented_behavior");
            assert_eq!(row["role"], "domain-analyst@2");
            assert_eq!(row["limitations"], json!(eligible.limitations));
            assert_eq!(row["citations"], json!([citation("rule:refund")]));
            assert_eq!(row["tier"], json!(Tier::Agentic));
            assert_eq!(row["confidence"], json!(ConfidenceTier::InferredWeak));
            assert_eq!(row["review_status"], "accepted");
            assert_eq!(
                row["source_support"],
                json!({"captured": 0, "unverified": 1})
            );
            assert_eq!(row["state"], "eligible");
            assert_eq!(row["reason"], "all_conditions_hold");
            assert_eq!(
                row["reason_sentence"],
                "Every fact the finding cites is as the investigation read it."
            );
        }
        // The stale finding: identity, state and the changed id, and none
        // of its content.
        assert_eq!(
            workbench["findings_listed"],
            json!([{
                "finding_ref": stale.subject.subject_id,
                "investigation_id": "investigation-1",
                "result_id": "result-1",
                "finding_id": "finding-stale",
                "role": "domain-analyst@2",
                "cited_facts": 1,
                "state": "stale",
                "reason": "cited_fact_changed",
                "reason_sentence":
                    "A fact the finding cites changed after the investigation read it.",
                "changed": [citation("rule:return")],
                "missing": [],
            }])
        );
        let wire = workbench.to_string();
        assert!(!wire.contains(&stale.statement) && !wire.contains(&stale.title));
        assert_eq!(wire.contains(&eligible.statement), shown);

        // An MCP session: the export's `curated.md` bytes, and the same
        // split in `curated-json`.
        let session = fixture.session(mode);
        assert_eq!(&read(&session, "curated"), document);
        let structured = read(&session, "curated-json");
        assert_eq!(structured.content_id, structured.compute_content_id());
        let data: Value = serde_json::from_str(&structured.content).unwrap();
        assert_eq!(data["status"], "computed");
        assert_eq!(data["revision"], revision.as_str());
        for field in [
            "findings",
            "findings_omitted",
            "findings_listed",
            "finding_counts",
        ] {
            assert_eq!(data[field], workbench[field], "{field}");
        }
        assert_eq!(structured.content.contains(&eligible.statement), shown);
        assert!(!structured.content.contains(&stale.statement));
        revisions.push(revision);
    }
    // One revision in both modes: the mode filters what is shown.
    assert_eq!(revisions[0], revisions[1]);
}

#[test]
fn a_read_with_no_accepted_finding_carries_empty_finding_fields() {
    let fixture = fixture();
    for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
        let session = fixture.session(mode);
        assert!(!read(&session, "curated").content.contains(HEADING));
        let data: Value = serde_json::from_str(&read(&session, "curated-json").content).unwrap();
        assert_eq!(data["findings"], json!([]));
        assert_eq!(data["findings_omitted"], 0);
        assert_eq!(data["findings_listed"], json!([]));
        assert_eq!(
            data["finding_counts"],
            json!({"accepted": 0, "eligible": 0, "unresolvable": 0, "stale": 0})
        );
    }
}
