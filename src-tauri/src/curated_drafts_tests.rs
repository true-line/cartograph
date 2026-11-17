//! AC-0667 (T-0667): the host reads the section draft store, the curated
//! projection gives each accepted draft its state against the graph the
//! export read, the export prints the eligible ones in `best-effort`, and an
//! MCP session reads the same bytes.

use super::*;
use crate::mcp_reads::artifacts::ArtifactCompilation;
use crate::mcp_reads::{ReadError, ReadSession};
use agents::DecisionLog;
use agents::draft_store::{DraftDecision, DraftOrigin, DraftStore, StoredDraft};
use agents::section_draft::{Claim, ClaimKind, SYSTEM_SCOPE, SectionAddress, SectionDraft};
use core_graph::coverage::CoverageReadLimits;
use core_graph::{FactKey, GraphStore, SnapshotReadLimits};
use core_prov::EvidenceRef;
use serde_json::json;
use spec::ExportMode;

const D1: &str = "d1-product-specification";
const MODES: [ExportMode; 2] = [ExportMode::BestEffort, ExportMode::VerifiedOnly];

struct Fixture {
    _dir: tempfile::TempDir,
    graph: Mutex<SqliteGraphStore>,
    decisions: Mutex<DecisionLog>,
    reviews: StoredReviews,
}

fn capability(id: &str, content: &str) -> Node {
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
        content.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: "Capability".into(),
        props: json!({ "name": id, "prov": prov }),
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    for id in ["cap:checkout", "cap:refund"] {
        graph.put_node(&capability(id, id)).unwrap();
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

fn id(node: &str) -> String {
    spec::citation_id(&FactKey::Node { id: node.into() })
}

impl Fixture {
    /// Store a draft for §`section` whose one claim cites `cited` as the
    /// graph now holds them.
    fn draft(&self, section: &str, statement: &str, cited: &[&str]) -> StoredDraft {
        let (nodes, edges) = self.graph.lock().unwrap().read_snapshot().unwrap();
        let citations: Vec<String> = cited.iter().map(|node| id(node)).collect();
        let draft = SectionDraft {
            address: SectionAddress {
                catalogue_version: "1.0".into(),
                document: "D1".into(),
                section: section.into(),
                scope: SYSTEM_SCOPE.into(),
            },
            claims: vec![Claim {
                statement: statement.into(),
                kind: ClaimKind::Implemented,
                citations: citations.clone(),
            }],
            cited_hashes: spec::cited_fact_hashes(&nodes, &edges, &citations.into_iter().collect()),
        };
        assert_eq!(draft.cited_hashes.len(), cited.len());
        let origin = DraftOrigin {
            role: "section-writer@1".into(),
            role_definition: "definition-1".into(),
            job_id: 7,
            graph_content_hash: spec::graph_content_hash(&nodes, &edges),
        };
        self.reviews
            .drafts
            .lock()
            .unwrap()
            .store(&draft, &origin, None)
            .unwrap()
    }

    fn review(&self, draft: &StoredDraft, decision: DraftDecision) {
        self.reviews
            .drafts
            .lock()
            .unwrap()
            .review(&draft.draft_id, draft.review_revision, decision, None)
            .unwrap();
    }

    fn export(&self, mode: ExportMode) -> spec::SpecBundle {
        crate::build_spec_bundle(
            &self.graph.lock().unwrap(),
            &self.decisions.lock().unwrap(),
            &self.reviews.gather(),
            mode,
        )
        .unwrap()
    }

    fn acquire(&self, mode: ExportMode) -> Result<ReadSession, ReadError> {
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
    }
}

fn artifact<'a>(bundle: &'a spec::SpecBundle, id: &str) -> &'a spec::SpecArtifact {
    bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == id)
        .unwrap()
}

fn read(session: &ReadSession) -> spec::SpecArtifact {
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
    match session.compile_artifact(D1, trace, compile).unwrap() {
        ArtifactCompilation::Produced(artifact) => artifact.artifact().clone(),
        limited => panic!("{limited:?}"),
    }
}

#[test]
fn export_prints_an_accepted_draft_and_the_mcp_read_returns_the_same_bytes() {
    let fixture = fixture();
    let draft = fixture.draft(
        "1.1",
        "The shop takes a cart to an order.",
        &["cap:checkout"],
    );
    // A pending draft is not shown and does not change the revision.
    let pending = fixture.export(ExportMode::BestEffort);
    assert!(!artifact(&pending, D1).content.contains("takes a cart"));
    fixture.review(&draft, DraftDecision::Accepted);

    for mode in MODES {
        let bundle = fixture.export(mode);
        let document = artifact(&bundle, D1);
        let basis = bundle.curated_basis.as_ref().expect("computed");
        assert_ne!(
            basis.revision,
            pending.curated_basis.as_ref().unwrap().revision
        );
        assert!(document.content.contains(&format!(
            "- Graph content hash: {}\n- Curated revision: {}\n",
            bundle.graph_basis.as_ref().unwrap().content_hash,
            basis.revision
        )));
        match mode {
            ExportMode::BestEffort => {
                assert!(document.content.contains(&format!(
                    "1. The shop takes a cart to an order. (implemented; T3 / InferredWeak; \
                     role `section-writer@1`; human-reviewed: accepted; cites {})\n",
                    id("cap:checkout")
                )));
                assert!(document.content.contains("Drafted claims printed: 1."));
            }
            ExportMode::VerifiedOnly => {
                assert!(!document.content.contains("takes a cart"));
                assert!(
                    document
                        .content
                        .contains("Drafted claims left out of this `verified-only` export: 1.\n")
                );
            }
        }
        // Every id the document prints is a row of the export's index.
        let index: spec::FactIndex =
            serde_json::from_str(&artifact(&bundle, "fact-index-json").content).unwrap();
        let printed = document
            .content
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .filter(|word| word.len() == 18 && word.starts_with("F-"));
        spec::check_citations(&index, printed).unwrap();

        // The MCP read is the export's artifact for the same inputs and mode.
        let session = fixture.acquire(mode).unwrap();
        assert_eq!(&read(&session), document, "{mode:?}");
    }
}

#[test]
fn a_draft_is_stale_once_a_fact_it_cites_changes_and_says_so_in_the_document() {
    let fixture = fixture();
    let draft = fixture.draft(
        "1.1",
        "The shop takes a cart to an order.",
        &["cap:checkout"],
    );
    fixture.review(&draft, DraftDecision::Accepted);
    // Another fact changing leaves it eligible.
    fixture
        .graph
        .lock()
        .unwrap()
        .put_node(&capability("cap:refund", "changed"))
        .unwrap();
    assert!(
        artifact(&fixture.export(ExportMode::BestEffort), D1)
            .content
            .contains("takes a cart")
    );
    // The cited fact changing makes it stale.
    fixture
        .graph
        .lock()
        .unwrap()
        .put_node(&capability("cap:checkout", "changed"))
        .unwrap();
    for mode in MODES {
        let bundle = fixture.export(mode);
        let content = &artifact(&bundle, D1).content;
        // SPEC-17 slice 2 (AC-0672): best-effort keeps the stale draft in
        // place, marked; verified-only prints no claim and names it.
        assert!(content.contains("- Stale sections: 1.1 (scope `system`, `stale`)\n"));
        match mode {
            ExportMode::BestEffort => {
                assert!(content.contains("takes a cart"));
                assert!(
                    content.contains("Stale: cited facts changed since this text was accepted: F-")
                );
                assert!(content.contains("; cites a changed fact)\n"));
            }
            ExportMode::VerifiedOnly => {
                assert!(!content.contains("takes a cart"));
                assert!(content.contains(&format!(
                    "- Draft `{}` for scope `system` is listed as `stale` \
                     (`cited_fact_changed`).\n",
                    draft.draft_id
                )));
            }
        }
        assert_eq!(
            &read(&fixture.acquire(mode).unwrap()),
            artifact(&bundle, D1)
        );
    }
}

#[test]
fn a_rejected_or_returned_draft_is_not_shown_and_a_review_stales_an_open_read() {
    let fixture = fixture();
    let rejected = fixture.draft("1.1", "Rejected claim.", &["cap:checkout"]);
    let returned = fixture.draft("1.3", "Returned claim.", &["cap:refund"]);
    let before = fixture.export(ExportMode::BestEffort);
    fixture.review(&rejected, DraftDecision::Rejected);
    let gathered = fixture.reviews.gather();
    assert_eq!(fixture.reviews.unchanged(&gathered), Ok(true));
    fixture.review(&returned, DraftDecision::Returned);
    // A review made after the gather is seen: the read is refused as stale.
    assert_eq!(fixture.reviews.unchanged(&gathered), Ok(false));

    let bundle = fixture.export(ExportMode::BestEffort);
    let content = &artifact(&bundle, D1).content;
    assert!(!content.contains("Rejected claim.") && !content.contains("Returned claim."));
    assert!(content.contains("Drafted claims printed: 0."));
    // The rejection enters the revision; the document's sections do not change.
    assert_ne!(
        bundle.curated_basis.as_ref().unwrap().revision,
        before.curated_basis.as_ref().unwrap().revision
    );
}

#[test]
fn a_draft_store_that_cannot_be_read_leaves_the_projection_not_computed() {
    let fixture = fixture();
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = fixture.reviews.drafts.lock().unwrap();
        panic!("poison the draft store lock");
    }));
    assert!(poisoned.is_err());
    let bundle = fixture.export(ExportMode::BestEffort);
    assert!(bundle.curated_basis.is_none());
    assert!(
        artifact(&bundle, D1)
            .content
            .contains("- Curated revision: not computed (`reviews_unavailable`)")
    );
}
