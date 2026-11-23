//! AC-0653: `curated.md`. Both export modes, the DR-4 fields, the citations
//! through the fact index, a compile with no projection, and the Gap
//! predicate this crate shares with the projection core.

use super::*;
use crate::{
    ExportMode, FactIndex, SelectedArtifactLimit, SelectedArtifactOutcome, SelectedCompileLimits,
    SpecArtifact, SpecBundle, Supplied, check_citations, compile_spec, compile_spec_selected,
    compile_spec_selected_supplied, try_compile_spec_supplied,
};
use context_hub::curated::{
    AssertionDecisionEntry, BasisAssessment, BasisDimensions, CURATED_POLICY_VERSION,
    EvidenceAvailability, EvidenceStatus, ProjectionInput, RecordEvidence, ReviewedProposal,
    project,
};
use core_graph::Edge;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::json;

const GRAPH_HASH: &str = "graph-hash-1";

fn prov(tier: Tier, confidence: ConfidenceTier, id: &str) -> Provenance {
    Provenance::new(
        tier,
        confidence,
        vec![EvidenceRef {
            repo: "local/shop".into(),
            path: "src/app.ts".into(),
            byte_start: 1,
            byte_end: 9,
            commit_sha: "abc123".into(),
        }],
        "spec.curated.test",
        id.as_bytes(),
    )
    .unwrap()
}

fn node(id: &str, label: &str, tier: Tier, confidence: ConfidenceTier) -> Node {
    Node {
        id: id.into(),
        label: label.into(),
        props: json!({ "name": id, "prov": prov(tier, confidence, id) }),
    }
}

/// `gap:1` with `sym:a` and `sym:b`; `sym:b` is a T2 inference, so the
/// Workbench can reject it.
fn nodes() -> Vec<Node> {
    vec![
        node("gap:1", "Gap", Tier::Deterministic, ConfidenceTier::Gap),
        node(
            "sym:a",
            "Symbol",
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
        ),
        node(
            "sym:b",
            "Symbol",
            Tier::Semantic,
            ConfidenceTier::InferredStrong,
        ),
    ]
}

/// An accepted version 2 record for `gap:1`: `sym:a -CALLS-> sym:b`.
fn record(id: &str, captured: bool) -> ReviewedProposal {
    ReviewedProposal {
        record_id: id.into(),
        schema_version: 2,
        gap_id: "gap:1".into(),
        source_id: "sym:a".into(),
        target_id: "sym:b".into(),
        edge_label: "CALLS".into(),
        tier: Tier::Agentic,
        confidence: ConfidenceTier::InferredWeak,
        decision: Some(context_hub::curated::ReviewDecision::Accepted),
        evidence: vec![RecordEvidence {
            evidence_id: "E1".into(),
            captured,
        }],
    }
}

fn unchanged() -> BasisAssessment {
    BasisAssessment::Assessed(BasisDimensions {
        graph: BasisComparison::Unchanged,
        association: BasisComparison::Unchanged,
        evidence: vec![EvidenceStatus {
            evidence_id: "E1".into(),
            status: EvidenceAvailability::Available,
        }],
        unverified_evidence: 0,
        cited_files: BasisComparison::LegacyUnverified,
    })
}

/// The projection of `records` over [`nodes`], each assessed as unchanged.
fn projection(records: &[ReviewedProposal]) -> CuratedProjection {
    let assessments = records
        .iter()
        .map(|record| (record.record_id.clone(), unchanged()))
        .collect();
    let stale = [AssertionDecisionEntry {
        content_hash: "hash-of-a-statement-no-longer-made".into(),
        decision: AssertionReview::Rejected,
    }];
    project(&ProjectionInput {
        nodes: &nodes(),
        edges: &[],
        graph_content_hash: GRAPH_HASH,
        proposals: records,
        assessments: &assessments,
        assertion_decisions: if records.is_empty() { &[] } else { &stale },
        current_assertion_hashes: &BTreeSet::new(),
        drafts: &[],
        cited_fact_hashes: &std::collections::BTreeMap::new(),
        findings: &[],
        cited_fact_digests: &std::collections::BTreeMap::new(),
        attested: &[],
        policy_version: CURATED_POLICY_VERSION,
    })
    .unwrap()
}

/// One eligible record and one listed as unverified.
fn mixed() -> CuratedProjection {
    let projection = projection(&[
        record("rec-eligible", true),
        record("rec-unverified", false),
    ]);
    assert_eq!(projection.items.len(), 1);
    assert_eq!(projection.listed.len(), 1);
    projection
}

fn compile(
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    curated: Option<&CuratedProjection>,
) -> SpecBundle {
    try_compile_spec_supplied(
        &nodes(),
        &[] as &[Edge],
        &[],
        mode,
        rejected,
        Supplied {
            repo_names: &[],
            knowledge: None,
            coverage: &[],
            stale_coverage: &[],
            curated: curated.map_or(Curated::NotSupplied, Curated::Computed),
            graph_content_hash: None,
            source_revisions: None,
        },
    )
    .unwrap()
}

fn artifact<'a>(bundle: &'a SpecBundle, id: &str) -> &'a SpecArtifact {
    bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.id == id)
        .unwrap()
}

fn index(bundle: &SpecBundle) -> FactIndex {
    serde_json::from_str(&artifact(bundle, "fact-index-json").content).unwrap()
}

/// The part of `curated.md` under one `## ` heading.
fn section<'a>(content: &'a str, heading: &str) -> &'a str {
    let start = content.find(heading).unwrap() + heading.len();
    let rest = &content[start..];
    &rest[..rest.find("\n## ").unwrap_or(rest.len())]
}

/// Every citation id the text prints.
fn printed_citations(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|word| word.len() == 18 && word.starts_with("F-"))
        .collect()
}

const MODES: [ExportMode; 2] = [ExportMode::VerifiedOnly, ExportMode::BestEffort];

// AC-0653: a compile with no projection says that none was supplied, lists
// nothing and cites nothing, in both modes and through the bare compiler.
#[test]
fn a_compile_with_no_projection_says_that_none_was_supplied() {
    for mode in MODES {
        let supplied = compile(mode, &BTreeSet::new(), None);
        let curated = artifact(&supplied, "curated");
        assert_eq!(curated.file_name, "curated.md");
        assert_eq!(curated.title, "Curated items");
        assert!(curated.content.contains(NO_PROJECTION));
        assert!(!curated.content.contains("## Curated items"));
        assert!(!curated.content.contains("Curated revision"));
        assert!(curated.assertions.is_empty());
        assert!(curated.verify_identity());
        assert_eq!(supplied.curated_basis, None);
        assert_eq!(
            compile_spec(&nodes(), &[], &[], mode, &BTreeSet::new()),
            supplied
        );
    }
}

// AC-0653: a projection that could not be computed does not fail the
// export. The document states that and the reason, shows no accepted
// proposal and cites nothing; every other artifact is byte-identical to a
// compile with no projection, in both modes and compiled alone.
#[test]
fn a_projection_that_was_not_computed_is_stated_and_changes_no_other_artifact() {
    let failures = [
        (CuratedFailure::ReviewsUnavailable, "reviews_unavailable"),
        (
            CuratedFailure::TooManyRecords { limit: 4096 },
            "too_many_records",
        ),
        (
            CuratedFailure::TooManyAccepted { limit: 256 },
            "too_many_accepted",
        ),
        (
            CuratedFailure::AcceptedTooLarge {
                limit_bytes: 67_108_864,
            },
            "accepted_too_large",
        ),
        (CuratedFailure::DuplicateRecord, "duplicate_record"),
        (
            CuratedFailure::DuplicateAssertionDecision,
            "duplicate_assertion_decision",
        ),
        (
            CuratedFailure::AssertionsNotCompiled,
            "assertions_not_compiled",
        ),
        (CuratedFailure::RevisionNotEncoded, "revision_not_encoded"),
        (
            CuratedFailure::DecisionsUnavailable,
            "decisions_unavailable",
        ),
        (
            CuratedFailure::DerivedDecisionsNeedCompile,
            "derived_decisions_need_compile",
        ),
    ];
    let mut sentences = BTreeSet::new();
    for (failure, code) in failures {
        assert_eq!(json!(failure)["reason"], code);
        let supplied = Supplied {
            repo_names: &[],
            knowledge: None,
            coverage: &[],
            stale_coverage: &[],
            curated: Curated::NotComputed(failure),
            graph_content_hash: None,
            source_revisions: None,
        };
        for mode in MODES {
            let bare = compile(mode, &BTreeSet::new(), None);
            let failed =
                try_compile_spec_supplied(&nodes(), &[], &[], mode, &BTreeSet::new(), supplied)
                    .unwrap();
            assert_eq!(failed.curated_basis, None);
            assert_eq!(bare.artifacts.len(), failed.artifacts.len());
            for (before, after) in bare.artifacts.iter().zip(&failed.artifacts) {
                assert_eq!(
                    before == after,
                    // The D1 header gives the reason code too (AC-0663), and
                    // so does the bundle manifest (AC-0682).
                    !matches!(
                        before.id.as_str(),
                        "curated" | "d1-product-specification" | "bundle-manifest"
                    ),
                    "{} in {mode:?}",
                    before.id
                );
            }
            let curated = artifact(&failed, "curated");
            assert!(curated.content.contains(NOT_COMPUTED));
            assert!(curated.content.contains(&format!("(`{code}`)")));
            assert!(!curated.content.contains(NO_PROJECTION));
            for heading in ["## Curated items", "## Accepted records", "## Counts"] {
                assert!(!curated.content.contains(heading), "{heading}");
            }
            assert!(!curated.content.contains("Curated revision"));
            assert!(curated.assertions.is_empty());
            assert!(index_rows_cited_by(&failed, "curated").is_empty());
            let reason = curated
                .content
                .lines()
                .find(|line| line.starts_with("Reason: "))
                .unwrap();
            assert!(reason.ends_with(&format!(". (`{code}`)")), "{reason}");
            sentences.insert(reason.to_string());
            let alone = compile_spec_selected_supplied(
                &nodes(),
                &[],
                &[],
                mode,
                &BTreeSet::new(),
                &["curated"],
                SelectedCompileLimits {
                    max_bytes: usize::MAX,
                    adr: crate::AdrLimits {
                        max_rows: usize::MAX,
                        max_output_bytes: usize::MAX,
                    },
                },
                supplied,
            )
            .unwrap();
            assert_eq!(alone, [SelectedArtifactOutcome::Produced(curated.clone())]);
        }
    }
    assert_eq!(sentences.len(), failures.len());
    let limited = artifact(
        &try_compile_spec_supplied(
            &nodes(),
            &[],
            &[],
            ExportMode::BestEffort,
            &BTreeSet::new(),
            Supplied {
                repo_names: &[],
                knowledge: None,
                coverage: &[],
                stale_coverage: &[],
                curated: Curated::NotComputed(CuratedFailure::TooManyAccepted { limit: 256 }),
                graph_content_hash: None,
                source_revisions: None,
            },
        )
        .unwrap(),
        "curated",
    )
    .content
    .clone();
    assert!(limited.contains("More than 256 proposals are accepted"));
}

// AC-0653: with no accepted record the document says so, and every other
// artifact is byte-identical to a compile with no projection.
#[test]
fn no_accepted_record_is_stated_and_changes_no_other_artifact() {
    let empty = projection(&[]);
    assert_eq!(empty.counts.accepted, 0);
    for mode in MODES {
        let bare = compile(mode, &BTreeSet::new(), None);
        let with = compile(mode, &BTreeSet::new(), Some(&empty));
        assert_eq!(bare.artifacts.len(), with.artifacts.len());
        for (before, after) in bare.artifacts.iter().zip(&with.artifacts) {
            assert_eq!(
                before == after,
                // D1 prints the supplied revision in its header (AC-0663),
                // and the bundle manifest states it (AC-0682).
                !matches!(
                    before.id.as_str(),
                    "curated" | "d1-product-specification" | "bundle-manifest"
                ),
                "{} in {mode:?}",
                before.id
            );
        }
        assert_eq!(bare.assertion_count, with.assertion_count);
        let curated = artifact(&with, "curated");
        assert!(curated.content.contains(NO_ACCEPTED));
        assert!(curated.content.contains(&empty.revision));
        assert!(curated.content.contains(GRAPH_HASH));
        assert!(!curated.content.contains(NO_PROJECTION));
        assert!(curated.assertions.is_empty());
    }
}

// AC-0653, R-INT-5: a best-effort export shows the eligible record as an
// item with every DR-4 field, marked InferredWeak; a verified-only export
// shows no item and states how many it left out. The listed record, the
// assertion decisions and the counts are the same in both modes.
#[test]
fn best_effort_marks_items_and_verified_only_counts_them() {
    let projection = mixed();
    let best = compile(ExportMode::BestEffort, &BTreeSet::new(), Some(&projection));
    let verified = compile(
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
        Some(&projection),
    );
    let best = &artifact(&best, "curated").content;
    let verified = &artifact(&verified, "curated").content;

    for content in [best, verified] {
        assert!(content.contains("- View: `curated`"));
        assert!(content.contains(&format!("- Curated revision: {}", projection.revision)));
        assert!(content.contains(&format!("- Graph basis: {GRAPH_HASH}")));
        assert!(
            content.contains("- Accepted records: 2. Curated items: 1. Listed with a reason: 1.")
        );
    }

    let items = section(best, "\n## Curated items\n");
    let row = items
        .lines()
        .find(|line| line.starts_with("| rec-eligible |"))
        .unwrap();
    assert!(row.contains(
        "| rec-eligible | gap:1 | sym:a CALLS sym:b | Agentic | InferredWeak | accepted | \
         captured 1, unverified 0 | graph unchanged; associations unchanged; evidence available \
         1, not available 0, unverified 0 | eligible |"
    ));
    assert!(row.ends_with(&format!("| {ITEM_NOTE} |")));
    assert!(!items.contains("rec-unverified"));
    assert!(
        best.contains("Curated items are included, each marked InferredWeak in the Note column.")
    );

    let withheld = section(verified, "\n## Curated items\n");
    assert_eq!(
        withheld.trim(),
        "None is shown: this is a `verified-only` export. Curated items left out: 1."
    );
    assert!(verified.contains(
        "Curated items left out of this export because every curated item is InferredWeak: 1."
    ));
    assert!(!verified.contains("rec-eligible"));
    assert!(!verified.contains(ITEM_NOTE));

    let listed = "\n## Accepted records that are not curated items\n";
    let decisions = "\n## Workbench assertion decisions\n";
    let counts = "\n## Counts\n";
    for heading in [listed, decisions, counts] {
        assert_eq!(section(best, heading), section(verified, heading));
    }
    let listed = section(best, listed);
    assert!(listed.contains(
        "| rec-unverified | gap:1 | sym:a CALLS sym:b | Agentic | InferredWeak | accepted | \
         captured 0, unverified 1 |"
    ));
    assert!(listed.contains("| unverified | At least one evidence item was read from the working"));
    assert!(listed.contains("(`evidence_not_captured`) |"));
    assert!(!listed.contains("rec-eligible"));

    let decisions = section(best, decisions);
    assert!(decisions.contains("- Rejections applied: 0."));
    assert!(decisions.contains("- Decisions that match no current assertion: 1."));
    assert!(decisions.contains(&projection.assertion_decisions.digest));
    assert!(decisions.contains("| hash-of-a-statement-no-longer-made | rejected | stale |"));

    let counts = section(best, counts);
    for row in [
        "| Accepted | 2 |",
        "| `eligible` (curated items) | 1 |",
        "| `unverified` | 1 |",
        "| `stale` | 0 |",
    ] {
        assert!(counts.contains(row), "{row}");
    }
}

// AC-0653, ADR-0058: an item cites its Gap, source and target nodes, each id
// it prints has a row in the fact index, and the item itself has none. A
// verified-only export cites nothing from here.
#[test]
fn item_citations_resolve_in_the_fact_index() {
    let projection = mixed();
    let best = compile(ExportMode::BestEffort, &BTreeSet::new(), Some(&projection));
    let curated = artifact(&best, "curated");
    let subjects: Vec<&str> = curated
        .assertions
        .iter()
        .map(|assertion| assertion.subject_id.as_str())
        .collect();
    assert_eq!(subjects, ["gap:1", "sym:a", "sym:b"]);

    let index = index(&best);
    let printed = printed_citations(section(&curated.content, "\n## Curated items\n"));
    assert_eq!(printed.len(), 3);
    check_citations(&index, printed.iter().copied()).unwrap();
    for id in ["gap:1", "sym:a", "sym:b"] {
        let citation = citation_id(&FactKey::Node { id: id.into() });
        assert!(printed.contains(&citation.as_str()), "{id}");
        let row = index
            .rows
            .iter()
            .find(|row| row.citation_id == citation)
            .unwrap();
        assert!(row.cited_by.iter().any(|by| by == "curated"), "{id}");
    }
    // A curated item is not a fact: no row is keyed by a record.
    assert!(index.rows.iter().all(|row| {
        !serde_json::to_string(&row.fact)
            .unwrap()
            .contains("rec-eligible")
    }));

    let verified = compile(
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
        Some(&projection),
    );
    assert!(artifact(&verified, "curated").assertions.is_empty());
    assert!(
        index_rows_cited_by(&verified, "curated").is_empty(),
        "a verified-only export shows no item and cites nothing for one"
    );
}

fn index_rows_cited_by(bundle: &SpecBundle, artifact: &str) -> Vec<String> {
    index(bundle)
        .rows
        .into_iter()
        .filter(|row| row.cited_by.iter().any(|by| by == artifact))
        .map(|row| row.citation_id)
        .collect()
}

// AC-0653: a Workbench rejection withholds the item's target node from the
// export. The item is still shown, the node is named as not cited, no
// citation id is printed for it and the export still compiles.
#[test]
fn a_node_the_export_withholds_is_not_cited() {
    let projection = mixed();
    let target = &nodes()[2];
    let rejected: BTreeSet<String> = [crate::provenance(&target.props, &target.id).content_hash]
        .into_iter()
        .collect();
    let best = compile(ExportMode::BestEffort, &rejected, Some(&projection));
    let curated = artifact(&best, "curated");
    let subjects: Vec<&str> = curated
        .assertions
        .iter()
        .map(|assertion| assertion.subject_id.as_str())
        .collect();
    assert_eq!(subjects, ["gap:1", "sym:a"]);

    let items = section(&curated.content, "\n## Curated items\n");
    let row = items
        .lines()
        .find(|line| line.starts_with("| rec-eligible |"))
        .unwrap();
    assert!(row.contains(&format!("target {NOT_CITED}")));
    let printed = printed_citations(items);
    assert_eq!(printed.len(), 2);
    let index = index(&best);
    check_citations(&index, printed.iter().copied()).unwrap();
    let withheld = citation_id(&FactKey::Node { id: "sym:b".into() });
    assert!(!curated.content.contains(&withheld));
    assert!(index.rows.iter().all(|row| row.citation_id != withheld));
    assert!(
        curated
            .content
            .contains("1 Gap, source or target nodes of the curated items are not cited")
    );
}

// AC-0653: the same inputs give the same bytes, the artifact compiled alone
// equals the bundle's, and the output bound refuses it whole.
#[test]
fn curated_is_deterministic_selected_alone_and_bounded() {
    let projection = mixed();
    let limits = |max_bytes| SelectedCompileLimits {
        max_bytes,
        adr: crate::AdrLimits {
            max_rows: usize::MAX,
            max_output_bytes: usize::MAX,
        },
    };
    for mode in MODES {
        let bundle = compile(mode, &BTreeSet::new(), Some(&projection));
        assert_eq!(bundle, compile(mode, &BTreeSet::new(), Some(&projection)));
        let expected = artifact(&bundle, "curated");
        let select = |ids: &[&str], max_bytes| {
            compile_spec_selected_supplied(
                &nodes(),
                &[],
                &[],
                mode,
                &BTreeSet::new(),
                ids,
                limits(max_bytes),
                Supplied {
                    repo_names: &[],
                    knowledge: None,
                    coverage: &[],
                    stale_coverage: &[],
                    curated: Curated::Computed(&projection),
                    graph_content_hash: None,
                    source_revisions: None,
                },
            )
            .unwrap()
        };
        assert_eq!(
            select(&["curated"], usize::MAX),
            [SelectedArtifactOutcome::Produced(expected.clone())]
        );
        // The index compiled alone still lists what `curated` cites.
        assert_eq!(
            select(&["fact-index-json"], usize::MAX),
            [SelectedArtifactOutcome::Produced(
                artifact(&bundle, "fact-index-json").clone()
            )]
        );
        let size = expected.content.len() + serde_json::to_vec(&expected.assertions).unwrap().len();
        assert_eq!(
            select(&["curated"], size),
            [SelectedArtifactOutcome::Produced(expected.clone())]
        );
        assert_eq!(
            select(&["curated"], size - 1),
            [SelectedArtifactOutcome::Limited {
                artifact_id: "curated".into(),
                limit: SelectedArtifactLimit::OutputBytes {
                    remaining: size - 1
                },
            }]
        );
        // Without a projection the selected compiler says none was supplied.
        let bare = compile_spec_selected(
            &nodes(),
            &[],
            &[],
            mode,
            &BTreeSet::new(),
            &["curated"],
            limits(usize::MAX),
        )
        .unwrap();
        let [SelectedArtifactOutcome::Produced(bare)] = bare.as_slice() else {
            panic!("curated compiles without a projection")
        };
        assert!(bare.content.contains(NO_PROJECTION));
    }
}

// AC-0653: every state and reason is named as the projection serializes it,
// and every reason has a sentence of its own.
#[test]
fn states_and_reasons_are_named_as_the_projection_serializes_them() {
    use RecordState as S;
    use StateReason as R;
    for state in [
        S::Unverified,
        S::AlreadyCovered,
        S::Unresolvable,
        S::Stale,
        S::Conflicted,
        S::Eligible,
    ] {
        assert_eq!(json!(state), state_name(state));
    }
    let reasons = [
        R::LegacyRecord,
        R::UnsupportedRecordVersion,
        R::NoEvidence,
        R::EvidenceNotCaptured,
        R::BaseFactFillsSlot,
        R::GapMissing,
        R::GapNoLongerGap,
        R::SourceMissing,
        R::TargetMissing,
        R::AssessmentMissing,
        R::AssessmentFailed,
        R::GraphComparisonFailed,
        R::GraphChanged,
        R::AssociationComparisonFailed,
        R::AssociationChanged,
        R::CitedFilesUnchanged,
        R::EvidenceNotAssessed,
        R::EvidenceReadFailed,
        R::EvidenceUnavailable,
        R::AssessmentMismatch,
        R::DifferentTargetAccepted,
        R::AllConditionsHold,
    ];
    let mut sentences = BTreeSet::new();
    for reason in reasons {
        let (code, sentence) = reason_text(reason);
        assert_eq!(json!(reason), code);
        assert!(sentence.ends_with('.'), "{code}");
        assert!(sentences.insert(sentence), "{code} repeats a sentence");
    }
    assert_eq!(json!(CuratedView::Curated), view_name(CuratedView::Curated));
    assert_eq!(
        json!(CuratedView::Recovered),
        view_name(CuratedView::Recovered)
    );
}

// AC-0653: the curated basis is stamped by the host, is absent from a bare
// compile and round-trips; a bundle written before the field existed reads.
#[test]
fn bundle_curated_basis_is_additive_and_round_trips() {
    let projection = mixed();
    let mut bundle = compile(ExportMode::BestEffort, &BTreeSet::new(), Some(&projection));
    assert_eq!(bundle.curated_basis, None);
    let plain = serde_json::to_value(&bundle).unwrap();
    assert!(plain.get("curated_basis").is_none());
    assert_eq!(serde_json::from_value::<SpecBundle>(plain).unwrap(), bundle);

    bundle.curated_basis = Some(CuratedBasis::of(&projection));
    let stamped = serde_json::to_value(&bundle).unwrap();
    assert_eq!(stamped["curated_basis"]["revision"], projection.revision);
    assert_eq!(stamped["curated_basis"]["counts"]["accepted"], 2);
    assert_eq!(stamped["curated_basis"]["counts"]["eligible"], 1);
    assert_eq!(stamped["curated_basis"]["counts"]["unverified"], 1);
    assert_eq!(
        serde_json::from_value::<SpecBundle>(stamped).unwrap(),
        bundle
    );
}

// AC-0653 (SPEC-12 open item): the projection core and this crate each hold
// the Gap predicate. They must agree on every shape of node, or a record
// could be eligible for a Gap the registers do not list.
#[test]
fn gap_predicate_matches_the_projection_core() {
    let mut shapes = Vec::new();
    for label in ["Gap", "Symbol", "Finding"] {
        for (tier, confidence) in [
            (Tier::Deterministic, ConfidenceTier::Confirmed),
            (Tier::Deterministic, ConfidenceTier::Gap),
            (Tier::Dynamic, ConfidenceTier::Confirmed),
            (Tier::Semantic, ConfidenceTier::InferredStrong),
            (Tier::Semantic, ConfidenceTier::Gap),
            (Tier::Agentic, ConfidenceTier::InferredWeak),
            (Tier::Agentic, ConfidenceTier::Gap),
        ] {
            shapes.push(node("n", label, tier, confidence));
        }
        // No provenance, provenance of the wrong shape, provenance that
        // parses and does not validate, and properties that are no object.
        let mut invalid =
            serde_json::to_value(prov(Tier::Deterministic, ConfidenceTier::Confirmed, "n"))
                .unwrap();
        invalid["confidence_tier"] = json!("InferredWeak");
        invalid["tier"] = json!("Deterministic");
        for props in [
            json!({}),
            json!({ "prov": null }),
            json!({ "prov": "text" }),
            json!({ "prov": { "tier": "Deterministic" } }),
            json!({ "prov": invalid }),
            json!(null),
            json!([1, 2]),
            json!("text"),
        ] {
            shapes.push(Node {
                id: "n".into(),
                label: label.into(),
                props,
            });
        }
    }
    assert_eq!(shapes.len(), 45);
    let mut gaps = 0;
    for shape in &shapes {
        let here = crate::is_gap_node(shape);
        assert_eq!(
            here,
            context_hub::curated::is_gap(shape),
            "{} {}",
            shape.label,
            shape.props
        );
        gaps += usize::from(here);
    }
    // Both answers occur, so the comparison is not vacuous.
    assert!(gaps > 0 && gaps < shapes.len());
}
