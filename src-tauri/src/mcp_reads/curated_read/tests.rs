//! AC-0654 (SPEC-12 slice 4): the curated read of an MCP session against the
//! desktop export, without a staging store. The reviewed-record cases are in
//! `proposals` and `task_evidence::completion_tests`, which own the staging
//! fixtures.
use super::*;
use crate::curated::{CuratedReview, NoReviews, ReviewSource, ReviewsUnreadable};
use crate::mcp_reads::artifacts::ArtifactCompilation;
use crate::mcp_reads::{ReadError, ReadSession};
use agents::{AssertionDecision, CuratableAssertion, DecisionLog};
use core_graph::coverage::CoverageReadLimits;
use core_graph::{GraphStore, Node, SnapshotReadLimits, SqliteGraphStore};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::{Value, json};
use std::sync::Mutex;

const D1_ID: &str = "d1-product-specification";

fn evidence() -> Vec<EvidenceRef> {
    vec![EvidenceRef {
        repo: "fixture/repo".into(),
        path: "main.tf".into(),
        byte_start: 0,
        byte_end: 8,
        commit_sha: "fixed".into(),
    }]
}

fn inferred(content: &str) -> Provenance {
    Provenance::new(
        Tier::Semantic,
        ConfidenceTier::InferredStrong,
        evidence(),
        "fixture.semantic",
        content.as_bytes(),
    )
    .unwrap()
}

fn decide(decisions: &Mutex<DecisionLog>, provenance: Provenance, decision: AssertionDecision) {
    decisions
        .lock()
        .unwrap()
        .record_assertion(
            &CuratableAssertion {
                subject_id: "module:fixture".into(),
                summary: "fixture".into(),
                provenance,
            },
            decision,
            None,
        )
        .unwrap();
}

/// A graph with one confirmed fact and two stored inferred facts, and two
/// Workbench decisions, each on one of the stored inferred facts: one
/// rejected, one accepted.
fn stores() -> (Mutex<SqliteGraphStore>, Mutex<DecisionLog>) {
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        evidence(),
        "fixture",
        b"resource",
    )
    .unwrap();
    graph
        .put_node(&Node {
            id: "resource:orders".into(),
            label: "Resource".into(),
            props: json!({"logical_id":"orders","type":"queue","prov":provenance}),
        })
        .unwrap();
    for id in ["module:rejected", "module:accepted"] {
        graph
            .put_node(&Node {
                id: id.into(),
                label: "Module".into(),
                props: json!({"name":id,"prov":inferred(id)}),
            })
            .unwrap();
    }
    let decisions = Mutex::new(DecisionLog::open(":memory:").unwrap());
    decide(
        &decisions,
        inferred("module:rejected"),
        AssertionDecision::Rejected,
    );
    decide(
        &decisions,
        inferred("module:accepted"),
        AssertionDecision::Accepted,
    );
    (Mutex::new(graph), decisions)
}

fn acquire(
    stores: &(Mutex<SqliteGraphStore>, Mutex<DecisionLog>),
    reviews: &dyn ReviewSource,
    mode: ExportMode,
) -> Result<ReadSession, ReadError> {
    ReadSession::acquire_with_overlay(
        &stores.0,
        None,
        &stores.1,
        reviews,
        || Ok(vec![]),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        mode,
    )
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

fn export(
    stores: &(Mutex<SqliteGraphStore>, Mutex<DecisionLog>),
    review: &CuratedReview,
    mode: ExportMode,
) -> spec::SpecBundle {
    crate::build_spec_bundle(
        &stores.0.lock().unwrap(),
        &stores.1.lock().unwrap(),
        review,
        mode,
    )
    .unwrap()
}

fn curated_of(bundle: &spec::SpecBundle) -> &spec::SpecArtifact {
    bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == ARTIFACT_ID)
        .unwrap()
}

#[test]
fn mcp_curated_read_is_the_export_document_and_revision_in_both_modes() {
    // AC-0654: with no reviewed record the projection is still computed,
    // over the graph and the Workbench decisions, which here are all on
    // stored facts, so no document is compiled for it. The `curated` artifact an
    // MCP session serves is the one `build_spec_bundle` holds, and the
    // session's basis carries the bundle's curated revision.
    let stores = stores();
    let mut revisions = Vec::new();
    for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
        let bundle = export(&stores, &CuratedReview::default(), mode);
        let basis = bundle.curated_basis.as_ref().expect("computed");
        let session = acquire(&stores, &NoReviews, mode).unwrap();
        assert_eq!(
            &read(&session, ARTIFACT_ID),
            curated_of(&bundle),
            "{mode:?}"
        );
        assert!(session.basis().h3_curated_available);
        assert_eq!(
            session.basis().curated,
            CuratedBasis::Computed {
                revision: basis.revision.clone()
            }
        );
        let wire = serde_json::to_value(session.basis()).unwrap();
        assert_eq!(wire["h3_curated_available"], true);
        assert_eq!(
            wire["curated"],
            json!({"status":"computed","revision":basis.revision})
        );

        let structured = read(&session, JSON_ARTIFACT_ID);
        assert_eq!(structured.schema, JSON_SCHEMA);
        assert_eq!(structured.content_id, structured.compute_content_id());
        let data: Value = serde_json::from_str(&structured.content).unwrap();
        assert_eq!(data["schema"], JSON_SCHEMA);
        assert_eq!(data["status"], "computed");
        assert_eq!(data["view"], "curated");
        assert_eq!(data["revision"], basis.revision.as_str());
        assert_eq!(
            data["graph_basis"],
            bundle.graph_basis.as_ref().unwrap().content_hash.as_str()
        );
        assert_eq!(data["items"], json!([]));
        assert_eq!(data["items_omitted"], 0);
        assert_eq!(data["listed"], json!([]));
        assert_eq!(data["counts"]["accepted"], 0);
        // Both decisions match a stored fact: one rejection applies and
        // none is stale.
        let decisions = &data["assertion_decisions"];
        assert_eq!(decisions["rejections_applied"], 1);
        assert_eq!(decisions["stale"], json!([]));
        revisions.push(basis.revision.clone());
    }
    // One revision in both modes: the mode filters the document.
    assert_eq!(revisions[0], revisions[1]);
}

#[test]
fn uncomputed_curated_projection_fails_no_mcp_read_and_states_its_reason() {
    // AC-0654 (ADR-0061 decision 11): when the reviews cannot be read the
    // session is still acquired and every other artifact reads as before.
    // The flag is false, the basis carries the reason, and the curated read
    // returns the statement the export makes for the same failure.
    let stores = stores();
    for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
        let computed = acquire(&stores, &NoReviews, mode).unwrap();
        let session = acquire(&stores, &ReviewsUnreadable, mode).unwrap();
        assert!(!session.basis().h3_curated_available);
        assert_eq!(
            session.basis().curated,
            CuratedBasis::NotComputed(CuratedFailure::ReviewsUnavailable)
        );
        let wire = serde_json::to_value(session.basis()).unwrap();
        assert_eq!(wire["h3_curated_available"], false);
        assert_eq!(
            wire["curated"],
            json!({"status":"not_computed","reason":"reviews_unavailable"})
        );
        // The failure is part of the basis identity.
        assert_ne!(session.basis_id().unwrap(), computed.basis_id().unwrap());
        for descriptor in crate::mcp_reads::artifacts::readable_catalog() {
            let id = descriptor.id;
            // The D1 header and the bundle manifest give the reason in
            // place of the revision.
            assert_eq!(
                read(&session, id) == read(&computed, id),
                !matches!(
                    id,
                    ARTIFACT_ID | JSON_ARTIFACT_ID | D1_ID | "bundle-manifest"
                ),
                "{id}"
            );
        }

        let failed = CuratedReview::failed(CuratedFailure::ReviewsUnavailable);
        let bundle = export(&stores, &failed, mode);
        assert!(bundle.curated_basis.is_none());
        let document = read(&session, ARTIFACT_ID);
        assert_eq!(&document, curated_of(&bundle), "{mode:?}");
        // The D1 read is the export's too, with the graph hash stated.
        let d1 = read(&session, D1_ID);
        assert_eq!(
            Some(&d1),
            bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.id == D1_ID)
        );
        assert!(d1.content.contains(&format!(
            "- Graph content hash: {}\n- Curated revision: not computed (`reviews_unavailable`)",
            bundle.graph_basis.as_ref().unwrap().content_hash
        )));
        assert!(document.content.contains("reviews_unavailable"));
        let data: Value = serde_json::from_str(&read(&session, JSON_ARTIFACT_ID).content).unwrap();
        assert_eq!(data["status"], "not_computed");
        assert_eq!(data["failure"], json!({"reason":"reviews_unavailable"}));
        assert!(data.get("items").is_none() && data.get("revision").is_none());
    }
}

/// Reviews that read once and then change, or stop reading.
struct Later(Result<bool, CuratedFailure>);

impl ReviewSource for Later {
    fn gather(&self) -> CuratedReview {
        CuratedReview::default()
    }

    fn unchanged(&self, _: &CuratedReview) -> Result<bool, CuratedFailure> {
        self.0
    }
}

#[test]
fn mcp_read_is_stale_when_the_reviews_change_while_it_is_acquired() {
    // AC-0654 (SPEC-12 "Surfaces"): reviewed records that differ at the end
    // of the acquisition from the ones gathered at its start would give
    // another curated revision, so the read fails with `StaleBasis`. A
    // record read that no longer finishes is not a changed revision: the
    // session is acquired with the projection not computed.
    let stores = stores();
    assert!(matches!(
        acquire(&stores, &Later(Ok(false)), ExportMode::BestEffort),
        Err(ReadError::StaleBasis)
    ));
    let limit = crate::curated::MAX_CURATED_ACCEPTED;
    let failure = CuratedFailure::TooManyAccepted { limit };
    let session = acquire(&stores, &Later(Err(failure)), ExportMode::BestEffort).unwrap();
    assert_eq!(session.basis().curated, CuratedBasis::NotComputed(failure));
    assert_eq!(
        serde_json::to_value(&session.basis().curated).unwrap(),
        json!({"status":"not_computed","reason":"too_many_accepted","limit":limit})
    );
    assert!(acquire(&stores, &Later(Ok(true)), ExportMode::BestEffort).is_ok());
}

#[test]
fn workbench_decision_gives_the_mcp_basis_another_curated_revision() {
    // AC-0654 (ADR-0061 decision 8): the Workbench decisions join the
    // revision, so a new rejection of a stored fact gives a later session
    // another curated revision and another basis identity.
    let stores = stores();
    let before = acquire(&stores, &NoReviews, ExportMode::BestEffort).unwrap();
    decide(
        &stores.1,
        inferred("module:accepted"),
        AssertionDecision::Rejected,
    );
    let after = acquire(&stores, &NoReviews, ExportMode::BestEffort).unwrap();
    assert!(before.basis().h3_curated_available && after.basis().h3_curated_available);
    assert_ne!(before.basis().curated, after.basis().curated);
    assert_ne!(before.basis_id().unwrap(), after.basis_id().unwrap());
}

#[test]
fn derived_or_orphaned_workbench_decision_is_not_compiled_for_an_mcp_read() {
    // AC-0654: a Workbench decision that matches no stored fact was made on
    // a derived statement or matches nothing, and only a compile of every
    // document can tell. An MCP acquisition does not make that compile: the
    // projection is not computed, with that reason, in the basis and in
    // both documents, and no other read fails. The export for the same
    // state compiles and computes.
    let stores = stores();
    let computed = acquire(&stores, &NoReviews, ExportMode::BestEffort).unwrap();
    assert!(computed.basis().h3_curated_available);
    decide(
        &stores.1,
        inferred("no such statement"),
        AssertionDecision::Accepted,
    );
    for mode in [ExportMode::BestEffort, ExportMode::VerifiedOnly] {
        let session = acquire(&stores, &NoReviews, mode).unwrap();
        assert!(!session.basis().h3_curated_available);
        assert_eq!(
            session.basis().curated,
            CuratedBasis::NotComputed(CuratedFailure::DerivedDecisionsNeedCompile)
        );
        assert_eq!(
            serde_json::to_value(&session.basis().curated).unwrap(),
            json!({"status":"not_computed","reason":"derived_decisions_need_compile"})
        );
        let document = read(&session, ARTIFACT_ID).content;
        assert!(
            document.contains("(`derived_decisions_need_compile`)"),
            "{document}"
        );
        assert!(document.contains("which an agent read does not do"));
        let data: Value = serde_json::from_str(&read(&session, JSON_ARTIFACT_ID).content).unwrap();
        assert_eq!(data["status"], "not_computed");
        assert_eq!(
            data["failure"],
            json!({"reason":"derived_decisions_need_compile"})
        );
        // Every other artifact reads.
        for descriptor in crate::mcp_reads::artifacts::readable_catalog() {
            read(&session, descriptor.id);
        }
        // The export compiles, computes and lists the decision as stale.
        let bundle = export(&stores, &CuratedReview::default(), mode);
        assert!(bundle.curated_basis.is_some(), "{mode:?}");
        assert!(
            curated_of(&bundle)
                .content
                .contains("match no current assertion: 1")
        );
    }
    // The accepted decision changed no rejection, so the other artifacts
    // are the ones the computed session reads.
    let session = acquire(&stores, &NoReviews, ExportMode::BestEffort).unwrap();
    assert_eq!(read(&session, "topology"), read(&computed, "topology"));
}

/// Counts the reviews gathered.
struct Counting(std::cell::Cell<usize>);

impl ReviewSource for Counting {
    fn gather(&self) -> CuratedReview {
        self.0.set(self.0.get() + 1);
        CuratedReview::default()
    }

    fn unchanged(&self, _: &CuratedReview) -> Result<bool, CuratedFailure> {
        Ok(true)
    }
}

#[test]
fn graph_over_the_session_limits_fails_before_any_review_is_gathered() {
    // AC-0654: the reviews are gathered, and so the assessments are made,
    // only after the bounded graph read admitted the graph. A graph over
    // the session's limits fails the acquisition with nothing gathered.
    let stores = stores();
    let counting = Counting(std::cell::Cell::new(0));
    let limited = ReadSession::acquire_with_overlay(
        &stores.0,
        None,
        &stores.1,
        &counting,
        || Ok(vec![]),
        SnapshotReadLimits {
            max_rows: 1,
            ..SnapshotReadLimits::default()
        },
        CoverageReadLimits::default(),
        ExportMode::BestEffort,
    );
    assert!(matches!(limited, Err(ReadError::Limit)));
    assert_eq!(counting.0.get(), 0);
    assert!(acquire(&stores, &counting, ExportMode::BestEffort).is_ok());
    assert_eq!(counting.0.get(), 1);
}

#[test]
fn curated_basis_reservation_fixture_is_the_longest_wire_form() {
    // AC-0654: the reservation ledger counts the basis from its fixture.
    let longest = context_hub::logical::count(&CuratedBasis::reservation_fixture()).unwrap();
    for failure in [
        CuratedFailure::ReviewsUnavailable,
        CuratedFailure::TooManyRecords { limit: usize::MAX },
        CuratedFailure::TooManyAccepted { limit: usize::MAX },
        CuratedFailure::DuplicateRecord,
        CuratedFailure::DuplicateAssertionDecision,
        CuratedFailure::AssertionsNotCompiled,
        CuratedFailure::RevisionNotEncoded,
        CuratedFailure::DecisionsUnavailable,
        CuratedFailure::DerivedDecisionsNeedCompile,
    ] {
        let wire = context_hub::logical::count(&CuratedBasis::NotComputed(failure)).unwrap();
        assert!(longest >= wire, "{failure:?}");
    }
}
