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
        let _ = app.emit("investigation-review://changed", &observation);
        Ok(observation)
    })
    .await
    .map_err(|_| ReviewCommandError::StorageUnavailable)?
}
