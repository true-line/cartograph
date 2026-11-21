//! Captured-preparation fixture over a test app. Its AppState stores are freed
//! when the owner drops (#1621); a handle a callback cloned out keeps them alive.
use super::super::expanded::vendure_prepare::{self, BoundDefinition};
use super::*;
use agents::investigation::InvestigationDetail;

pub(in super::super) struct PreparationApp {
    app: crate::test_app::TestApp,
}
pub(super) struct CaptureSnapshot {
    pub(super) task: InvestigationDetail,
    pub(super) analyst_jobs: usize,
    pub(super) invocations: i64,
    pub(super) attempts: i64,
}
impl PreparationApp {
    pub(super) fn new(profile: &profile::NativeProfile) -> Result<Self, String> {
        Ok(Self {
            app: profile.app()?,
        })
    }
    pub(in super::super) fn for_reopen(
        basis: &measurement::ReopenPreparation<'_>,
    ) -> Result<Self, String> {
        Self::new(&basis.profile)
    }
    pub(super) fn capture(self, basis: CapturePreparation<'_>) -> Result<Preparation, String> {
        complete_capture(basis, &self.app)
    }
    pub(in super::super) fn bound(
        self,
        basis: &measurement::ReopenPreparation<'_>,
        paths: [&Path; 4],
        expected: &vendure_prepare::Definition,
        validated: BoundDefinition<'_>,
    ) -> Result<serde_json::Value, String> {
        vendure_prepare::prepare_fixture_on_app(&self.app, basis, paths, expected, validated)
    }
    pub(super) fn snapshot(self, id: &str) -> Result<CaptureSnapshot, String> {
        self.read_snapshot(id)
    }
    fn read_snapshot(&self, id: &str) -> Result<CaptureSnapshot, String> {
        let state = self.app.state::<AppState>();
        let jobs = state.jobs.lock().map_err(|_| "jobs unavailable")?;
        let task = jobs.investigation(id).map_err(|e| e.to_string())?;
        let analyst_jobs = jobs
            .list()
            .map_err(|e| e.to_string())?
            .iter()
            .filter(|job| job.investigation_id.is_some())
            .count();
        let db = rusqlite::Connection::open(
            self.app
                .path()
                .app_data_dir()
                .map_err(|_| "profile data unavailable")?
                .join("state.db"),
        )
        .map_err(|e| e.to_string())?;
        let count = |table| {
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|e| e.to_string())
        };
        Ok(CaptureSnapshot {
            task,
            analyst_jobs,
            invocations: count("investigation_invocations")?,
            attempts: count("completion_attempts")?,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use agents::investigation::*;
    use std::sync::Arc;

    // AC-0450: actual store ownership, not descriptor-count assumptions.
    #[test]
    fn captured_fixture_preparation_releases_stores_after_normal_results() {
        let case = super::super::tests::Case::new();
        let prepared = case.prepare_fixture().unwrap();
        for success in [true, false] {
            let owner = PreparationApp::new(&prepared.profile).unwrap();
            let weak = Arc::downgrade(&owner.app.state::<AppState>().sources);
            let id = if success {
                let state = owner.app.state::<AppState>();
                let inventory = crate::investigations::completion_choices::control_inventory(
                    owner.app.handle(),
                    &state,
                    &InvestigationScope::All,
                )
                .unwrap();
                let context = match &inventory {
                    obligations::Obligation::Inventory { target, .. } => target.context_id.clone(),
                    _ => unreachable!(),
                };
                let request = obligations::StagedObligationRequest {
                    schema_version: 7,
                    request_nonce: "scoped-lifetime".into(),
                    specialist_id: "domain-analyst@10".into(),
                    question: "Inspect the saved fixture lifetime.".into(),
                    scope: InvestigationScope::All,
                    provider_mode: InvestigationProviderMode::Local,
                    limit_profile: "investigation-v2".into(),
                    inspection_requirements: None,
                    expected_graph_revision: Some(prepared.graph.graph_snapshot_id.clone()),
                    expected_context_id: context,
                    conversation_id: None,
                    parent_id: None,
                    obligations: obligations::ObligationEnvelope {
                        schema_version: 2,
                        contract: "investigation-completion-obligations@2".into(),
                        obligations: vec![inventory],
                    },
                };
                let descriptor = crate::investigations::runtime::descriptor(
                    crate::investigations::worker::tests::completion_tests::expanded_scripted_provider(false).as_ref(),
                    InvestigationProviderMode::Local).unwrap();
                owner
                    .app
                    .state::<AppState>()
                    .jobs
                    .lock()
                    .unwrap()
                    .start_next_prompt_investigation(&request, Some(&descriptor))
                    .unwrap()
                    .detail
                    .summary
                    .investigation_id
            } else {
                "missing-fixture-task".into()
            };
            let result = owner.snapshot(&id);
            assert_eq!(result.is_ok(), success, "{:?}", result.as_ref().map(|_| ()));
            assert!(
                weak.upgrade().is_none(),
                "normal return retained store ownership"
            );
        }
    }

    // AC-0450: a panicked owner releases too; a handle a generic callback cloned keeps state.
    #[test]
    fn captured_fixture_preparation_releases_on_panic_and_retains_escaped_handles() {
        let case = super::super::tests::Case::new();
        let prepared = case.prepare_fixture().unwrap();
        let owner = PreparationApp::new(&prepared.profile).unwrap();
        let weak = Arc::downgrade(&owner.app.state::<AppState>().sources);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let _owner = owner;
                panic!("unfinished fixture operation");
            }))
            .is_err()
        );
        assert!(weak.upgrade().is_none());

        let base = case.root.parent().unwrap();
        let receipt = base.join("lifetime-preparation.json");
        let proposal = base.join("lifetime-proposal.json");
        std::fs::write(&receipt, serde_json::to_vec(&prepared).unwrap()).unwrap();
        std::fs::write(&proposal, b"[]").unwrap();
        let escaped = reopen_preparation(
            &case.config,
            &receipt,
            &proposal,
            &base.join("lifetime-output"),
            InputPolicy::OfflineFixture,
            |app, _, _, _| Ok(app.handle().clone()),
        )
        .unwrap();
        let sources = Arc::downgrade(&escaped.state::<AppState>().sources);
        // The generic callback's handle keeps the app alive until it drops.
        assert!(escaped.try_state::<AppState>().is_some());
        drop(escaped);
        assert!(sources.upgrade().is_none());
    }
}
