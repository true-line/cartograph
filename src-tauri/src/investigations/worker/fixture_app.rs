//! Synchronous worker fixture over a test app. The app's state is freed when
//! the fixture drops (#1621), unless a handle was handed out: then the app is
//! retained, because that handle may still be in use.
use super::*;
use tauri::Manager;
pub(super) struct FixtureApp {
    app: crate::test_app::TestApp,
}
impl FixtureApp {
    pub(super) fn new(state: AppState) -> Self {
        let app = crate::test_app::mock_app();
        app.manage(state);
        Self { app }
    }
    pub(super) fn state<T: Send + Sync + 'static>(&self) -> tauri::State<'_, T> {
        self.app.state()
    }
    pub(super) fn try_state<T: Send + Sync + 'static>(&self) -> Option<tauri::State<'_, T>> {
        self.app.try_state()
    }
    pub(super) fn handle(&self) -> &tauri::AppHandle<tauri::test::MockRuntime> {
        self.retain_for_external_use();
        self.app.handle()
    }
    pub(super) fn retain_for_external_use(&self) {
        self.app.retain_with_escaped_handles();
    }
    pub(super) fn run_legacy(
        &self,
        id: String,
        request: StartInvestigationRequest,
        execution: JobExecution,
        provider: Arc<dyn LlmProvider>,
        live: Arc<LiveTask>,
        started: Instant,
    ) {
        super::run(
            self.app.handle().clone(),
            id,
            request,
            execution,
            provider,
            live,
            started,
        );
    }
    pub(super) fn run_completion(
        &self,
        id: String,
        request: StagedObligationRequest,
        execution: JobExecution,
        provider: Arc<dyn LlmProvider>,
        live: Arc<LiveTask>,
        started: Instant,
    ) {
        self.run_completion_with_audit(
            id, request, execution, provider, live, started, false, None,
        );
    }
    // Test/development only: exercise the real entry and dispatch, never audit_probe.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_completion_with_audit(
        &self,
        id: String,
        request: StagedObligationRequest,
        execution: JobExecution,
        provider: Arc<dyn LlmProvider>,
        live: Arc<LiveTask>,
        started: Instant,
        dispatch_audit: bool,
        auditor: Option<ConfiguredAuditor>,
    ) {
        super::run_completion_with_clock(
            self.app.handle().clone(),
            id,
            request,
            execution,
            provider,
            live,
            RunClock::new(started),
            None,
            dispatch_audit,
            auditor,
        );
    }
    // AuditProbe receives execution/DTO references only.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_gated_completion_inner(
        &self,
        id: &str,
        staged: &StagedObligationRequest,
        execution: &JobExecution,
        live: &Arc<LiveTask>,
        provider: &dyn LlmProvider,
        mut clock: RunClock,
        audit_probe: Option<&AuditProbe<'_>>,
        dispatch_audit: bool,
    ) {
        {
            let app = self.app.handle();
            let state = app.state::<AppState>();
            let _cleanup = Cleanup {
                state: &state,
                id,
                live,
            };
            let detail = read_detail(&state, id, execution).unwrap();
            let request = FrozenContext::completion_acquisition_request(staged).unwrap();
            let mut worker = Worker {
                pending_patch: None,
                last_completion: None,
                app,
                state: &state,
                id,
                execution,
                live,
                detail,
                clock: &mut clock,
                task_limits: None,
                request_timeout_ms: CompletionLimits::default().values().request_timeout_ms,
                audit_probe,
                dispatch_audit,
                auditor: None,
            };
            let outcome = worker
                .execute_inner(&request, Some(staged), provider)
                .map_err(|failure| {
                    failure.preparation_after_call(
                        request.specialist_id,
                        worker.detail.usage.model_invocations,
                    )
                });
            if audit_probe.is_some() {
                assert!(outcome.is_ok(), "audit probe worker failed: {outcome:?}");
            }
            if let Err(failure) = outcome
                && let Some(reason) = failure.reason()
            {
                stop(
                    app,
                    &state,
                    id,
                    execution,
                    reason,
                    clock.active_milliseconds(),
                );
            }
        }
    }
    // FrozenContext owns snapshots/DTOs/archive/static policy, never App/State.
    pub(super) fn prepare_context(
        &self,
        request: &StartInvestigationRequest,
        capacity: usize,
    ) -> Result<FrozenContext, HostError> {
        FrozenContext::prepare_with_app(
            self.app.handle(),
            &self.state::<AppState>(),
            request,
            &[],
            Some(capacity),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_fixture_releases_stores_after_return_and_after_panic() {
        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = Fixture::new(provider.as_ref());
        let sources = Arc::downgrade(&fixture.app.try_state::<AppState>().unwrap().sources);
        fixture.run(provider);
        drop(fixture);
        assert!(
            sources.upgrade().is_none(),
            "returned fixture retained AppState"
        );

        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = Fixture::new(provider.as_ref());
        let sources = Arc::downgrade(&fixture.app.state::<AppState>().sources);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _fixture = fixture;
            panic!("unfinished inline run");
        }));
        assert!(unwind.is_err());
        assert!(
            sources.upgrade().is_none(),
            "panicked fixture retained AppState"
        );
    }

    #[test]
    fn escaped_handle_permanently_retains_state() {
        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = Fixture::new(provider.as_ref());
        let sources = Arc::downgrade(&fixture.app.state::<AppState>().sources);
        let escaped = fixture.app.handle().clone();
        fixture.run(provider);
        drop(fixture);
        assert!(escaped.try_state::<AppState>().is_some());
        drop(escaped);
        assert!(sources.upgrade().is_some());
    }
}
