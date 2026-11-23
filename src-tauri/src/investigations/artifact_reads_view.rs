//! Saved `read_artifact` manifests for the investigation view (ADR-0089).
//! Read from the task's admitted ledger, never recompiled: the view names
//! what the specialist was shown, not what the artifact says now.
use agents::investigation::InvestigationArtifactReadManifest;
use serde::Serialize;
use tauri::Manager;

use crate::AppState;

/// One page the specialist read, as its ledger manifest records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InvestigationArtifactReadView {
    pub artifact_id: String,
    /// The catalogue title, or the id when the catalogue no longer lists it.
    pub title: String,
    pub content_id: String,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub response_bytes: usize,
}

pub(crate) fn artifact_read_views(
    reads: &[InvestigationArtifactReadManifest],
) -> Vec<InvestigationArtifactReadView> {
    let catalog = crate::mcp_reads::artifacts::readable_catalog();
    reads
        .iter()
        .map(|read| InvestigationArtifactReadView {
            artifact_id: read.request.artifact_id.clone(),
            title: catalog
                .iter()
                .find(|artifact| artifact.id == read.request.artifact_id)
                .map_or_else(
                    || read.request.artifact_id.clone(),
                    |artifact| artifact.title.to_owned(),
                ),
            content_id: read.content_id.clone(),
            offset: read.request.offset,
            next_offset: read.next_offset,
            response_bytes: read.response_bytes,
        })
        .collect()
}

/// The saved artifact pages of one investigation, in ledger order; empty
/// when it has no ledger yet or read none.
#[tauri::command]
pub(crate) async fn investigation_artifact_reads(
    investigation_id: String,
    app: tauri::AppHandle,
) -> Result<Vec<InvestigationArtifactReadView>, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let ledger = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation_ledger(&investigation_id)
            .map_err(|e| e.to_string())?;
        Ok(ledger.map_or_else(Vec::new, |ledger| {
            artifact_read_views(&ledger.artifact_reads)
        }))
    })
    .await
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod tests {
    use super::*;
    use agents::investigation::InvestigationArtifactRead;

    // AC-1816: the view lists each saved page in ledger order with its
    // catalogue title and exact span, and falls back to the id for an
    // artifact the catalogue no longer lists.
    #[test]
    fn artifact_read_views_name_saved_pages_in_ledger_order() {
        let manifest = |artifact_id: &str, offset, next_offset| InvestigationArtifactReadManifest {
            request: InvestigationArtifactRead {
                context_id: format!("context-v2:{}", "a".repeat(64)),
                artifact_id: artifact_id.into(),
                expected_content_id: None,
                offset,
                max_bytes: 4096,
            },
            content_id: format!("spec-artifact-v1:{}", "b".repeat(64)),
            response_hash: "c".repeat(64),
            response_bytes: 4000,
            next_offset,
        };
        let reads = vec![
            manifest("topology", 0, Some(3500)),
            manifest("topology", 3500, None),
            manifest("retired-artifact", 0, None),
        ];
        let views = artifact_read_views(&reads);
        let title = crate::mcp_reads::artifacts::readable_catalog()
            .into_iter()
            .find(|artifact| artifact.id == "topology")
            .unwrap()
            .title;
        assert_eq!(views.len(), 3);
        assert_eq!(views[0].title, title);
        assert_eq!((views[0].offset, views[0].next_offset), (0, Some(3500)));
        assert_eq!((views[1].offset, views[1].next_offset), (3500, None));
        assert_eq!(views[2].title, "retired-artifact");
        assert!(artifact_read_views(&[]).is_empty());
    }
}
