//! Metadata-only human review IPC; no execution or evidence acquisition path.
use crate::{
    AppState,
    jobs::investigations::{FindingReviewError, InvestigationStoreError},
};
use agents::investigation::{
    FindingReviewObservation, FindingReviews, ReviewInvestigationFindingRequest,
};
use serde::Serialize;
use tauri::{Emitter, Manager};

/// Advisory invalidation wire: observations remain a separate command response.
#[derive(Debug, Clone, Serialize)]
struct FindingReviewChanged {
    investigation_id: String,
    result_id: String,
    subject_id: String,
    review_revision: u64,
}
impl From<&FindingReviewObservation> for FindingReviewChanged {
    fn from(observation: &FindingReviewObservation) -> Self {
        Self {
            investigation_id: observation.subject.investigation_id.clone(),
            result_id: observation.subject.result_id.clone(),
            subject_id: observation.subject.subject_id.clone(),
            review_revision: observation.revision,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub(crate) enum ReviewCommandError {
    InvalidRequest,
    InvalidRecord,
    NotFound,
    SubjectChanged,
    Conflict {
        current: Box<FindingReviewObservation>,
    },
    Capacity,
    StorageUnavailable,
}
impl From<FindingReviewError> for ReviewCommandError {
    fn from(error: FindingReviewError) -> Self {
        match error {
            FindingReviewError::SubjectChanged => Self::SubjectChanged,
            FindingReviewError::Conflict(current) => Self::Conflict { current },
            FindingReviewError::Store(error) => match error {
                InvestigationStoreError::Invalid => Self::InvalidRecord,
                InvestigationStoreError::Missing => Self::NotFound,
                InvestigationStoreError::Stale | InvestigationStoreError::Conflict => {
                    Self::SubjectChanged
                }
                InvestigationStoreError::Capacity => Self::Capacity,
                _ => Self::StorageUnavailable,
            },
        }
    }
}

#[tauri::command]
pub(crate) async fn get_investigation_finding_reviews(
    investigation_id: String,
    expected_result_id: String,
    app: tauri::AppHandle,
) -> Result<FindingReviews, ReviewCommandError> {
    if investigation_id.is_empty()
        || investigation_id.len() > 128
        || expected_result_id.is_empty()
        || expected_result_id.len() > 128
    {
        return Err(ReviewCommandError::InvalidRequest);
    }
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .map_err(|_| ReviewCommandError::StorageUnavailable)?
            .investigation_finding_reviews(&investigation_id, &expected_result_id)
            .map_err(ReviewCommandError::from)
    })
    .await
    .map_err(|_| ReviewCommandError::StorageUnavailable)?
}

#[tauri::command]
pub(crate) async fn review_investigation_finding(
    request_json: String,
    app: tauri::AppHandle,
) -> Result<FindingReviewObservation, ReviewCommandError> {
    tauri::async_runtime::spawn_blocking(move || {
        let request = ReviewInvestigationFindingRequest::decode(&request_json)
            .map_err(|_| ReviewCommandError::InvalidRequest)?;
        let observation = app
            .state::<AppState>()
            .jobs
            .lock()
            .map_err(|_| ReviewCommandError::StorageUnavailable)?
            .review_investigation_finding(&request)
            .map_err(ReviewCommandError::from)?;
        // Storage has committed and released its lock. Delivery is advisory:
        // lost acknowledgement is recovered by a metadata refresh, never replay.
        let _ = app.emit(
            "investigation-review://changed",
            FindingReviewChanged::from(&observation),
        );
        Ok(observation)
    })
    .await
    .map_err(|_| ReviewCommandError::StorageUnavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::investigation::{FindingReviewDecision, FindingReviewSubjectV1};

    #[test]
    fn review_invalidation_matches_declared_top_level_wire() {
        let observation = FindingReviewObservation {
            subject: FindingReviewSubjectV1 {
                investigation_id: "investigation-1".into(),
                result_id: "result-1".into(),
                finding_id: "finding-1".into(),
                subject_id: format!("investigation-finding-review-v1:{}", "0".repeat(64)),
            },
            revision: 2,
            decision: Some(FindingReviewDecision::AcceptedForFollowUp),
            reviewed_at: Some("2026-10-21T00:00:00Z".into()),
        };
        let invalidation = serde_json::to_value(FindingReviewChanged::from(&observation)).unwrap();
        assert_eq!(
            invalidation,
            serde_json::json!({
                "investigation_id": observation.subject.investigation_id,
                "result_id": observation.subject.result_id,
                "subject_id": observation.subject.subject_id,
                "review_revision": 2,
            })
        );
    }
}
