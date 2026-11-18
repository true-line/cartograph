//! Curation review IPC for saved findings (#1746 requirement 2, ADR-0074,
//! AC-0844). It is separate from the follow-up review commands: its own
//! request, its own store and its own event.
//!
//! The caller names a finding by its review subject and nothing else. The
//! host resolves the finding, its producing specialist and its citations
//! from the immutable saved result, and that is what the first review
//! records. No graph or source is read, no model is called and no fact or
//! tier changes: what a curated read shows is decided by the next
//! projection (R-INT-1, R-INT-3).
//!
//! **Locks.** The investigation store is read and released before the
//! section draft store, which holds the curation table, is taken. The two
//! are never held together.
use crate::{
    AppState,
    jobs::investigations::{FindingReviewError, InvestigationStoreError, ResolvedFinding},
};
use agents::finding_curation::{
    CitedFact, CuratedFindingRecord, CurationObservation, FindingCurationError,
    StoredCuratedFinding,
};
use agents::investigation::{CurateInvestigationFindingRequest, InvestigationEvidenceOrigin};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tauri::{Emitter, Manager};

/// Most findings one saved result holds: a finish admits twelve.
const MAX_RESULT_FINDINGS: usize = 12;

/// The curation reviews of one result's findings. A finding with no
/// curation review has no entry and is at revision zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FindingCurations {
    schema_version: u32,
    investigation_id: String,
    result_id: String,
    reviews: Vec<CurationObservation>,
}

/// Advisory invalidation wire: the observation is the command response.
#[derive(Debug, Clone, Serialize)]
struct FindingCurationChanged {
    investigation_id: String,
    result_id: String,
    subject_id: String,
    review_revision: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub(crate) enum CurationCommandError {
    InvalidRequest,
    /// The saved finding, or a stored curation row, failed its checks.
    InvalidRecord,
    NotFound,
    SubjectChanged,
    /// Another review changed the finding first.
    Conflict {
        current_revision: u64,
    },
    /// The store stayed locked by another writer; nothing was saved.
    Busy,
    StorageUnavailable,
}

impl From<FindingReviewError> for CurationCommandError {
    fn from(error: FindingReviewError) -> Self {
        match error {
            FindingReviewError::SubjectChanged => Self::SubjectChanged,
            // Resolving a finding writes nothing, so it meets no revision.
            FindingReviewError::Conflict(_) => Self::StorageUnavailable,
            FindingReviewError::Busy => Self::Busy,
            FindingReviewError::Store(error) => match error {
                InvestigationStoreError::Invalid => Self::InvalidRecord,
                InvestigationStoreError::Missing => Self::NotFound,
                InvestigationStoreError::Stale | InvestigationStoreError::Conflict => {
                    Self::SubjectChanged
                }
                _ => Self::StorageUnavailable,
            },
        }
    }
}

impl From<FindingCurationError> for CurationCommandError {
    fn from(error: FindingCurationError) -> Self {
        match error {
            FindingCurationError::StaleReviewRevision { actual, .. } => Self::Conflict {
                current_revision: actual,
            },
            // The subject binds one immutable result, so other content
            // under it is a damaged record, not a newer one.
            FindingCurationError::InvalidRecord(_)
            | FindingCurationError::RecordTooLarge { .. }
            | FindingCurationError::ContentChanged
            | FindingCurationError::InvalidStored => Self::InvalidRecord,
            FindingCurationError::Busy => Self::Busy,
            FindingCurationError::InvalidLimit
            | FindingCurationError::Storage(_)
            | FindingCurationError::Json(_) => Self::StorageUnavailable,
        }
    }
}

/// The record a curation review stores for `resolved` (ADR-0074 decision
/// 2): one entry per cited fact, ordered by its ADR-0058 citation id, with
/// the digest the producing run recorded. Two citations of one finding may
/// name the same fact through different spans; they must agree on its
/// digest, and the fact counts as captured only when every one of them
/// rests on captured primary-source evidence.
pub(crate) fn record_of(
    resolved: ResolvedFinding,
) -> Result<CuratedFindingRecord, CurationCommandError> {
    let mut cited: BTreeMap<String, CitedFact> = BTreeMap::new();
    for citation in &resolved.citations {
        let id = spec::citation_id(&super::section_drafts::fact_key(&citation.fact));
        let captured = matches!(
            citation.origin,
            InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
        );
        match cited.get_mut(&id) {
            Some(known) => {
                if known.fact != citation.fact || known.digest != citation.fact_digest {
                    return Err(CurationCommandError::InvalidRecord);
                }
                known.captured &= captured;
            }
            None => {
                cited.insert(
                    id.clone(),
                    CitedFact {
                        citation: id,
                        fact: citation.fact.clone(),
                        digest: citation.fact_digest.clone(),
                        captured,
                    },
                );
            }
        }
    }
    Ok(CuratedFindingRecord {
        subject: resolved.subject,
        role: resolved.role,
        claim_kind: resolved.finding.claim_kind,
        title: resolved.finding.title,
        statement: resolved.finding.statement,
        limitations: resolved.finding.limitations,
        cited: cited.into_values().collect(),
    })
}

fn curate(
    state: &AppState,
    request: &CurateInvestigationFindingRequest,
) -> Result<StoredCuratedFinding, CurationCommandError> {
    // Released before the draft store is taken.
    let resolved = state
        .jobs
        .lock()
        .map_err(|_| CurationCommandError::StorageUnavailable)?
        .resolve_investigation_finding(&request.subject)?;
    let record = record_of(resolved)?;
    let drafts = state
        .drafts
        .lock()
        .map_err(|_| CurationCommandError::StorageUnavailable)?;
    Ok(drafts
        .finding_curation()
        .review(&record, request.expected_revision, request.decision)?)
}

/// The curation reviews whose subject the saved result still computes. A
/// row stored under another subject of the same result id is not a review
/// of a finding the caller can see.
fn current_curations(
    stored: Vec<CurationObservation>,
    subjects: &BTreeSet<String>,
) -> Vec<CurationObservation> {
    stored
        .into_iter()
        .filter(|review| subjects.contains(&review.subject_id))
        .collect()
}

fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128
}

#[tauri::command]
pub(crate) async fn get_investigation_finding_curations(
    investigation_id: String,
    expected_result_id: String,
    app: tauri::AppHandle,
) -> Result<FindingCurations, CurationCommandError> {
    if !id_ok(&investigation_id) || !id_ok(&expected_result_id) {
        return Err(CurationCommandError::InvalidRequest);
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // The saved result decides whether the pair exists: a missing or
        // replaced result is an error, never an empty list. Released before
        // the draft store is taken.
        let subjects: BTreeSet<String> = state
            .jobs
            .lock()
            .map_err(|_| CurationCommandError::StorageUnavailable)?
            .investigation_finding_reviews(&investigation_id, &expected_result_id)?
            .reviews
            .into_iter()
            .map(|review| review.subject.subject_id)
            .collect();
        let reviews = current_curations(
            state
                .drafts
                .lock()
                .map_err(|_| CurationCommandError::StorageUnavailable)?
                .finding_curation()
                .for_result(&investigation_id, &expected_result_id, MAX_RESULT_FINDINGS)?,
            &subjects,
        );
        Ok(FindingCurations {
            schema_version: 1,
            investigation_id,
            result_id: expected_result_id,
            reviews,
        })
    })
    .await
    .map_err(|_| CurationCommandError::StorageUnavailable)?
}

#[tauri::command]
pub(crate) async fn curate_investigation_finding(
    request_json: String,
    app: tauri::AppHandle,
) -> Result<CurationObservation, CurationCommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        let request = CurateInvestigationFindingRequest::decode(&request_json)
            .map_err(|_| CurationCommandError::InvalidRequest)?;
        let stored = curate(&app.state::<AppState>(), &request)?;
        let subject = stored.record.subject;
        // Storage has committed and released its lock. Delivery is
        // advisory: a lost event is recovered by the next read.
        let _ = app.emit(
            "investigation-curation://changed",
            FindingCurationChanged {
                investigation_id: subject.investigation_id,
                result_id: subject.result_id,
                subject_id: subject.subject_id.clone(),
                review_revision: stored.review_revision,
            },
        );
        Ok(CurationObservation {
            subject_id: subject.subject_id,
            finding_id: subject.finding_id,
            review_revision: stored.review_revision,
            review_decision: stored.review_decision,
            reviewed_at: stored.reviewed_at,
        })
    })
    .await
    .map_err(|_| CurationCommandError::StorageUnavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::TaskFactKey;
    use agents::investigation::{
        FindingReviewSubjectV1, InvestigationCitation, InvestigationClaimKind, InvestigationFinding,
    };
    use core_prov::{ConfidenceTier, Tier};

    fn citation(id: &str, node: &str, digest: &str, working_tree: bool) -> InvestigationCitation {
        InvestigationCitation {
            citation_id: id.into(),
            fact: TaskFactKey::Node { id: node.into() },
            fact_digest: digest.into(),
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: if working_tree {
                InvestigationEvidenceOrigin::WorkingTreeUnverified
            } else {
                InvestigationEvidenceOrigin::GraphMetadata
            },
        }
    }

    fn resolved(citations: Vec<InvestigationCitation>) -> ResolvedFinding {
        ResolvedFinding {
            subject: FindingReviewSubjectV1 {
                investigation_id: "investigation-1".into(),
                result_id: "result-1".into(),
                finding_id: "finding-1".into(),
                subject_id: format!("investigation-finding-review-v1:{}", "0".repeat(64)),
            },
            role: "domain-analyst@2".into(),
            finding: InvestigationFinding {
                finding_id: "finding-1".into(),
                claim_kind: InvestigationClaimKind::ImplementedBehavior,
                title: "Refunds need an approved return".into(),
                statement: "A refund is issued only for an approved return.".into(),
                citation_ids: citations.iter().map(|c| c.citation_id.clone()).collect(),
                limitations: vec!["Partial refunds were not read.".into()],
                tier: Tier::Agentic,
                confidence_tier: ConfidenceTier::InferredWeak,
            },
            citations,
        }
    }

    fn id(node: &str) -> String {
        spec::citation_id(&core_graph::FactKey::Node { id: node.into() })
    }

    // AC-0844: the record is the saved finding, one entry per cited fact.
    #[test]
    fn curation_record_is_the_saved_finding_with_one_entry_per_cited_fact() {
        let source = resolved(vec![
            citation("citation:b", "rule:refund", "node-v1:1", false),
            citation("citation:a", "ep:refund", "node-v1:2", true),
            // The same fact through another span.
            citation("citation:c", "rule:refund", "node-v1:1", true),
        ]);
        let record = record_of(source.clone()).unwrap();
        assert_eq!(record.subject, source.subject);
        assert_eq!(record.role, "domain-analyst@2");
        assert_eq!(record.claim_kind, source.finding.claim_kind);
        assert_eq!(record.title, source.finding.title);
        assert_eq!(record.statement, source.finding.statement);
        assert_eq!(record.limitations, source.finding.limitations);
        let mut expected = vec![
            (id("rule:refund"), "rule:refund", "node-v1:1"),
            (id("ep:refund"), "ep:refund", "node-v1:2"),
        ];
        expected.sort();
        assert_eq!(record.cited.len(), 2);
        for (cited, (citation, node, digest)) in record.cited.iter().zip(expected) {
            assert_eq!(cited.citation, citation);
            assert_eq!(cited.fact, TaskFactKey::Node { id: node.into() });
            assert_eq!(cited.digest, digest);
        }
        // Nothing here is captured primary-source evidence.
        assert!(record.cited.iter().all(|cited| !cited.captured));
    }

    // AC-0844: one fact with two recorded digests is not a record.
    #[test]
    fn curation_record_refuses_one_fact_with_two_digests() {
        let source = resolved(vec![
            citation("citation:a", "rule:refund", "node-v1:1", false),
            citation("citation:b", "rule:refund", "node-v1:other", false),
        ]);
        assert_eq!(record_of(source), Err(CurationCommandError::InvalidRecord));
    }

    // AC-0844: every store failure has one closed code.
    #[test]
    fn curation_errors_map_to_closed_codes() {
        assert_eq!(
            CurationCommandError::from(FindingCurationError::StaleReviewRevision {
                expected: 1,
                actual: 3
            }),
            CurationCommandError::Conflict {
                current_revision: 3
            }
        );
        assert_eq!(
            serde_json::to_value(CurationCommandError::Conflict {
                current_revision: 3
            })
            .unwrap(),
            serde_json::json!({"code": "conflict", "current_revision": 3})
        );
        assert_eq!(
            CurationCommandError::from(FindingCurationError::ContentChanged),
            CurationCommandError::InvalidRecord
        );
        assert_eq!(
            CurationCommandError::from(FindingReviewError::SubjectChanged),
            CurationCommandError::SubjectChanged
        );
        assert_eq!(
            CurationCommandError::from(FindingReviewError::Store(InvestigationStoreError::Missing)),
            CurationCommandError::NotFound
        );
        assert_eq!(
            CurationCommandError::from(FindingReviewError::Busy),
            CurationCommandError::Busy
        );
        assert_eq!(
            CurationCommandError::from(FindingCurationError::Busy),
            CurationCommandError::Busy
        );
    }

    // AC-0844: the read returns only reviews of subjects the saved result
    // computes.
    #[test]
    fn curation_read_keeps_only_the_subjects_of_the_saved_result() {
        let review = |subject: &str| CurationObservation {
            subject_id: subject.into(),
            finding_id: "finding-1".into(),
            review_revision: 1,
            review_decision: agents::finding_curation::CurationDecision::Accepted,
            reviewed_at: "2026-01-01T00:00:00Z".into(),
        };
        let subjects = BTreeSet::from(["subject-a".to_owned()]);
        assert_eq!(
            current_curations(vec![review("subject-a"), review("subject-b")], &subjects),
            vec![review("subject-a")]
        );
        assert!(current_curations(vec![review("subject-b")], &subjects).is_empty());
    }
}
