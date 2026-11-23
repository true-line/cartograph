//! Staged records as the curated projection reads them (SPEC-12 slice 1,
//! ADR-0061, AC-0652).
//!
//! The projection core lives in `context_hub::curated` and knows nothing of
//! the staging store. These conversions are the only place a staged record
//! becomes projection input. They copy the identity, the proposed edge, the
//! producing tier and confidence, the review decision and each evidence
//! item's origin. The review note, the timestamps and the review revision
//! counter are not copied, so they cannot reach the curated revision.

use crate::{
    AssertionDecision, AssertionDecisionRecord, ProposalDecision, StagedProposal, TaskEvidenceKind,
};
use context_hub::curated::{
    AssertionDecisionEntry, AssertionReview, DraftAddress, DraftClaim, DraftReview, RecordEvidence,
    ReviewDecision, ReviewedDraft, ReviewedProposal,
};
use std::collections::BTreeMap;

/// One staged proposal as projection input.
///
/// Evidence is every span the producing task was supplied, by citation id. An
/// item is captured only when the version 2 source basis records every origin
/// of that id as captured primary source; an item with no recorded origin,
/// and every item of a version 1 record, is unverified.
pub fn curated_record(staged: &StagedProposal) -> ReviewedProposal {
    let mut evidence: BTreeMap<&str, Option<bool>> = staged
        .basis
        .evidence
        .iter()
        .map(|item| (item.id.as_str(), None))
        .collect();
    for origin in staged.source_basis.iter().flat_map(|basis| &basis.evidence) {
        let captured = matches!(
            origin.origin,
            TaskEvidenceKind::CapturedPrimarySource { .. }
        );
        let entry = evidence.entry(&origin.evidence_id).or_insert(None);
        *entry = Some(entry.unwrap_or(true) && captured);
    }
    ReviewedProposal {
        record_id: staged.proposal_id.clone(),
        schema_version: staged.schema_version,
        gap_id: staged.proposal.gap_id.clone(),
        source_id: staged.proposal.source_id.clone(),
        target_id: staged.proposal.target_id.clone(),
        edge_label: staged.proposal.edge_label.clone(),
        tier: staged.proposal.provenance.tier,
        confidence: staged.proposal.provenance.confidence_tier,
        decision: staged.review_decision.map(|decision| match decision {
            ProposalDecision::Accepted => ReviewDecision::Accepted,
            ProposalDecision::Rejected => ReviewDecision::Rejected,
        }),
        evidence: evidence
            .into_iter()
            .map(|(id, captured)| RecordEvidence {
                evidence_id: id.to_string(),
                captured: captured.unwrap_or(false),
            })
            .collect(),
    }
}

/// One stored section draft as projection input (SPEC-15 slice 5, AC-0667).
/// Nothing is derived: the address, role, claims, recorded hashes and review
/// are the stored ones.
pub fn reviewed_draft(stored: &crate::draft_store::StoredDraft) -> ReviewedDraft {
    use crate::draft_store::DraftDecision;
    use crate::section_draft::ClaimKind;
    let address = &stored.draft.address;
    ReviewedDraft {
        draft_id: stored.draft_id.clone(),
        address: DraftAddress {
            catalogue_version: address.catalogue_version.clone(),
            document: address.document.clone(),
            section: address.section.clone(),
            scope: address.scope.clone(),
        },
        role: stored.origin.role.clone(),
        claims: stored
            .draft
            .claims
            .iter()
            .map(|claim| DraftClaim {
                statement: claim.statement.clone(),
                kind: match claim.kind {
                    ClaimKind::Implemented => "implemented",
                    ClaimKind::DocumentedIntent => "documented_intent",
                    ClaimKind::Interpretation => "interpretation",
                }
                .into(),
                citations: claim.citations.clone(),
            })
            .collect(),
        cited_hashes: stored.draft.cited_hashes.clone(),
        replaces: stored.replaces.clone(),
        decision: stored.review_decision.map(|decision| match decision {
            DraftDecision::Accepted => DraftReview::Accepted,
            DraftDecision::Rejected => DraftReview::Rejected,
            DraftDecision::Returned => DraftReview::Returned,
        }),
    }
}

/// One Workbench assertion decision as projection input, keyed by the
/// content hash of the reviewed assertion.
pub fn curated_assertion_decision(record: &AssertionDecisionRecord) -> AssertionDecisionEntry {
    AssertionDecisionEntry {
        content_hash: record.assertion.provenance.content_hash.clone(),
        decision: match record.decision {
            AssertionDecision::Accepted => AssertionReview::Accepted,
            AssertionDecision::Rejected => AssertionReview::Rejected,
            AssertionDecision::Annotated => AssertionReview::Annotated,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepared::tests::{fixture, result};
    use crate::{CuratableAssertion, ProposalStore, TaskSourceBasisV2};
    use context_hub::curated::{
        BasisAssessment, CURATED_POLICY_VERSION, CuratedProjection, ProjectionInput, RecordState,
        StateReason, project,
    };
    use core_graph::{Edge, Node};
    use core_prov::{ConfidenceTier, Provenance, Tier};
    use std::collections::BTreeSet;

    /// A version 2 record staged and accepted through the real store.
    fn accepted(note: Option<&str>) -> (ProposalStore, StagedProposal) {
        let task = fixture();
        let mut store = ProposalStore::open(":memory:").unwrap();
        let staged = store
            .stage_prepared(
                &task,
                &result(&task, "Possible target link", &["source", "target"]),
                1,
                &task.source_basis().graph_snapshot_id,
            )
            .unwrap();
        let reviewed = store
            .review(&staged.proposal_id, 0, ProposalDecision::Accepted, note)
            .unwrap();
        (store, reviewed)
    }

    fn run(staged: &[&StagedProposal]) -> CuratedProjection {
        let proposals: Vec<_> = staged.iter().map(|record| curated_record(record)).collect();
        project(&ProjectionInput {
            nodes: &[] as &[Node],
            edges: &[] as &[Edge],
            graph_content_hash: "graph",
            proposals: &proposals,
            assessments: &BTreeMap::<String, BasisAssessment>::new(),
            assertion_decisions: &[],
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

    // AC-0652: the conversion copies identity, slot, decision and the producing
    // tier and confidence, and marks each evidence item by its real origin.
    #[test]
    fn staged_record_converts_with_per_item_origin() {
        let (_store, staged) = accepted(None);
        let record = curated_record(&staged);
        assert_eq!(record.record_id, staged.proposal_id);
        assert_eq!(record.schema_version, 2);
        assert_eq!(record.gap_id, staged.proposal.gap_id);
        assert_eq!(record.source_id, staged.proposal.source_id);
        assert_eq!(record.target_id, staged.proposal.target_id);
        assert_eq!(record.edge_label, staged.proposal.edge_label);
        assert_eq!(record.tier, staged.proposal.provenance.tier);
        assert_eq!(
            record.confidence,
            staged.proposal.provenance.confidence_tier
        );
        assert_eq!(
            (record.tier, record.confidence),
            (Tier::Agentic, ConfidenceTier::InferredWeak)
        );
        assert_eq!(record.decision, Some(ReviewDecision::Accepted));
        let origins: Vec<(&str, bool)> = record
            .evidence
            .iter()
            .map(|item| (item.evidence_id.as_str(), item.captured))
            .collect();
        // The fixture captured only the source span.
        assert_eq!(
            origins,
            [("gap", false), ("source", true), ("target", false)]
        );

        // With today's mixed evidence the accepted record is listed unverified.
        let projection = run(&[&staged]);
        assert!(projection.items.is_empty());
        assert_eq!(projection.listed[0].state, RecordState::Unverified);
        assert_eq!(
            projection.listed[0].reason,
            StateReason::EvidenceNotCaptured
        );
        assert_eq!(projection.listed[0].tier, staged.proposal.provenance.tier);
        assert_eq!(
            projection.listed[0].confidence,
            staged.proposal.provenance.confidence_tier
        );
    }

    // AC-0652: an item whose origin is missing from the source basis, or that
    // has one uncaptured origin among several, is not captured.
    #[test]
    fn missing_or_mixed_origin_is_not_captured() {
        let (_store, mut staged) = accepted(None);
        let basis: &mut TaskSourceBasisV2 = staged.source_basis.as_mut().unwrap();
        let mut second = basis.evidence[0].clone();
        second.origin = TaskEvidenceKind::WorkingTreeUnverified;
        basis.evidence.push(second);
        basis
            .evidence
            .retain(|origin| origin.evidence_id != "target");
        let record = curated_record(&staged);
        assert!(record.evidence.iter().all(|item| !item.captured));
        assert_eq!(record.evidence.len(), 3);
    }

    // AC-0652: a version 1 record has no source basis; every item is unverified.
    #[test]
    fn legacy_record_is_unverified() {
        let (_store, mut staged) = accepted(None);
        staged.schema_version = 1;
        staged.source_basis = None;
        let record = curated_record(&staged);
        assert!(record.evidence.iter().all(|item| !item.captured));
        let projection = run(&[&staged]);
        assert_eq!(projection.listed[0].reason, StateReason::LegacyRecord);
    }

    // AC-0652: a review note, a timestamp and the review revision counter do
    // not change the revision; flipping the decision does.
    #[test]
    fn note_timestamp_and_review_counter_do_not_change_the_revision() {
        let (mut store, staged) = accepted(None);
        let base = run(&[&staged]);

        // The same decision recorded again with a note, through the store.
        let noted = store
            .review(
                &staged.proposal_id,
                staged.review_revision,
                ProposalDecision::Accepted,
                Some("checked against the handler"),
            )
            .unwrap();
        assert_ne!(noted.review_revision, staged.review_revision);
        assert_ne!(noted.review_note, staged.review_note);
        assert_eq!(run(&[&noted]).revision, base.revision);
        assert_eq!(run(&[&noted]), base);

        let mut later = noted.clone();
        later.created_at = "2031-01-01 00:00:00".into();
        later.reviewed_at = Some("2031-01-02 00:00:00".into());
        later.review_revision += 40;
        later.review_note = None;
        assert_eq!(run(&[&later]).revision, base.revision);

        let rejected = store
            .review(
                &noted.proposal_id,
                noted.review_revision,
                ProposalDecision::Rejected,
                None,
            )
            .unwrap();
        let withdrawn = run(&[&rejected]);
        assert_ne!(withdrawn.revision, base.revision);
        assert_eq!(withdrawn.counts.accepted, 0);
        assert!(withdrawn.listed.is_empty());
    }

    // AC-0652: a Workbench decision is keyed by the assertion content hash; its
    // note and timestamp are not carried.
    #[test]
    fn assertion_decision_converts_by_content_hash() {
        let provenance = Provenance::new(
            Tier::Semantic,
            ConfidenceTier::InferredStrong,
            vec![],
            "t2.fixture",
            b"assertion",
        )
        .unwrap();
        for (decision, expected) in [
            (AssertionDecision::Accepted, AssertionReview::Accepted),
            (AssertionDecision::Rejected, AssertionReview::Rejected),
            (AssertionDecision::Annotated, AssertionReview::Annotated),
        ] {
            let record = AssertionDecisionRecord {
                assertion: CuratableAssertion {
                    subject_id: "symbol:a".into(),
                    summary: "a calls b".into(),
                    provenance: provenance.clone(),
                },
                decision,
                note: Some("note".into()),
                updated_at: "2030-01-01 00:00:00".into(),
            };
            assert_eq!(
                curated_assertion_decision(&record),
                AssertionDecisionEntry {
                    content_hash: provenance.content_hash.clone(),
                    decision: expected,
                }
            );
        }
    }
}
