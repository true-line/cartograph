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
    read_id(session, D1)
}

fn read_id(session: &ReadSession, id: &str) -> spec::SpecArtifact {
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
fn the_mcp_read_of_the_bundle_manifest_is_the_exports_manifest() {
    let fixture = fixture();
    let draft = fixture.draft(
        "1.1",
        "The shop takes a cart to an order.",
        &["cap:checkout"],
    );
    fixture.review(&draft, DraftDecision::Accepted);
    for mode in MODES {
        let bundle = fixture.export(mode);
        let manifest = artifact(&bundle, "bundle-manifest");
        assert!(manifest.content.contains(&format!(
            "- Graph content hash: {}\n",
            bundle.graph_basis.as_ref().unwrap().content_hash
        )));
        assert!(manifest.content.contains(&format!(
            "- Curated revision: {}",
            bundle.curated_basis.as_ref().unwrap().revision
        )));
        assert!(
            !manifest
                .content
                .contains("not supplied to this compilation"),
            "{}",
            manifest.content
        );
        let session = fixture.acquire(mode).unwrap();
        assert_eq!(&read_id(&session, "bundle-manifest"), manifest, "{mode:?}");
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

// AC-1010 (T-1010, #2040): accepted drafts are bounded by their bytes, not
// by a count. More than 256 small accepted drafts are all read; accepted
// drafts past the byte budget fail the read whole, with the reason.
#[test]
fn accepted_drafts_are_bounded_by_bytes_not_by_count() {
    let fixture = fixture();
    let count = MAX_CURATED_ACCEPTED + 44;
    for index in 0..count {
        let draft = fixture.draft(
            "1.1",
            &format!("Checkout {index:03} is offered."),
            &["cap:checkout"],
        );
        fixture.review(&draft, DraftDecision::Accepted);
    }
    let review = fixture.reviews.gather();
    assert_eq!(review.failure, None);
    assert_eq!(review.drafts.len(), count);

    let store = fixture.reviews.drafts.lock().unwrap();
    let one = payload_bytes(
        &store.list(1, None).unwrap().items[0].draft,
        MAX_CURATED_ACCEPTED_DRAFT_BYTES,
    );
    // Every accepted draft has the same encoded size here.
    assert!(reviewed_drafts_within(&store, one * count).is_ok());
    assert_eq!(
        reviewed_drafts_within(&store, one * count - 1),
        Err(CuratedFailure::AcceptedTooLarge {
            limit_bytes: one * count - 1
        })
    );
    assert_eq!(
        MAX_CURATED_ACCEPTED_DRAFT_BYTES,
        MAX_CURATED_ACCEPTED * agents::draft_store::MAX_DRAFT_RECORD_BYTES
    );
}

// AC-1011 (T-1011): an accepted draft citing a Capability that lives only in
// the knowledge overlay is eligible when the overlay is current for the
// export, and `unresolvable` when the export reads no overlay (#2040).
#[test]
fn a_draft_citing_an_overlay_capability_resolves_against_the_overlay() {
    use context_hub::curated::{DraftAddress, DraftClaim, DraftReview, ReviewedDraft};
    let fixture = fixture();
    let derived = capability("capability:returns", "derived returns");
    let cited = vec![id("capability:returns"), id("cap:checkout")];
    let (revision, ((nodes, edges), coverage)) =
        read_graph(&fixture.graph.lock().unwrap()).unwrap();
    let ids: BTreeSet<String> = cited.iter().cloned().collect();
    let mut cited_hashes = spec::cited_fact_hashes(&nodes, &edges, &ids);
    cited_hashes.extend(spec::cited_node_hashes([&derived], &ids));
    assert_eq!(cited_hashes.len(), 2);
    let review = CuratedReview {
        failure: None,
        proposals: Vec::new(),
        drafts: vec![ReviewedDraft {
            draft_id: "draft-overlay".into(),
            address: DraftAddress {
                catalogue_version: "1.0".into(),
                document: "D1".into(),
                section: "1.3".into(),
                scope: SYSTEM_SCOPE.into(),
            },
            role: "section-composer@1".into(),
            claims: vec![DraftClaim {
                statement: "Returns are taken at checkout.".into(),
                kind: "implemented".into(),
                citations: cited,
            }],
            cited_hashes,
            replaces: None,
            decision: Some(DraftReview::Accepted),
        }],
        findings: Vec::new(),
        attested: Vec::new(),
        assessments: BTreeMap::new(),
        settled: Arc::default(),
    };
    let content_hash = spec::graph_content_hash(&nodes, &edges);
    let counts = |overlay: &[&Node]| {
        project(
            &review,
            &Snapshot {
                nodes: &nodes,
                edges: &edges,
                overlay,
                derived_decisions: DerivedDecisions::Compile(&[]),
                coverage: &coverage,
                content_hash: &content_hash,
                revision: &revision,
            },
            &[],
        )
        .unwrap()
        .draft_counts
    };
    let with = counts(&[&derived]);
    assert_eq!((with.eligible, with.unresolvable), (1, 0));
    let without = counts(&[]);
    assert_eq!((without.eligible, without.unresolvable), (0, 1));
}

// AC-1011 (T-1011): the overlay a draft resolves against is the one D1
// prints. A derived Capability the export withholds (here, rejected by a
// reviewer) is withheld from both, so a draft citing it is not eligible.
#[test]
fn a_withheld_overlay_capability_resolves_no_draft() {
    let fixture = fixture();
    let (revision, ((nodes, edges), _)) = read_graph(&fixture.graph.lock().unwrap()).unwrap();
    let base = core_graph::RevisionedSnapshot {
        revision,
        graph: (nodes, edges),
    };
    let prov = Provenance::new(
        Tier::Semantic,
        ConfidenceTier::InferredWeak,
        vec![EvidenceRef {
            repo: "fixture/repo".into(),
            path: "src/app.ts".into(),
            byte_start: 0,
            byte_end: 8,
            commit_sha: "fixed".into(),
        }],
        "t2.knowledge.capabilities",
        b"returns",
    )
    .unwrap()
    .with_method(core_prov::ProducingMethod::Rule {
        rule: "capabilities".into(),
        version: "1".into(),
    });
    let rejected = BTreeSet::from([prov.content_hash.clone()]);
    let derived = [core_graph::overlay::OverlayFact {
        item: core_graph::overlay::OverlayItem::Node(Node {
            id: "capability:returns".into(),
            label: "Capability".into(),
            props: json!({ "name": "Returns", "prov": prov }),
        }),
        cites: [core_graph::overlay::BaseRef::Node("cap:checkout".into())].into(),
    }];
    let printed = overlay_nodes(&base, &derived, &[], &BTreeSet::new());
    assert_eq!(
        printed
            .iter()
            .map(|node| node.id.as_str())
            .collect::<Vec<_>>(),
        ["capability:returns"]
    );
    assert!(overlay_nodes(&base, &derived, &[], &rejected).is_empty());
}
