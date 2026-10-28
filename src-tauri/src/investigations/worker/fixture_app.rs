//! Private synchronous ownership boundary around Tauri 2.12.5's AppHandle cycles.
use super::*;
use std::cell::Cell;
use tauri::Manager;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lifetime {
    Retain,
    SynchronousReturn,
    ExternalUse,
}
pub(super) struct FixtureApp {
    app: tauri::App<tauri::test::MockRuntime>,
    lifetime: Cell<Lifetime>,
}
impl FixtureApp {
    pub(super) fn new(state: AppState) -> Self {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(state);
        Self {
            app,
            lifetime: Cell::new(Lifetime::Retain),
        }
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
        self.lifetime.set(Lifetime::ExternalUse);
    }
    fn begin(&self) {
        if self.lifetime.get() != Lifetime::ExternalUse {
            self.lifetime.set(Lifetime::Retain);
        }
    }
    fn end(&self) {
        if self.lifetime.get() != Lifetime::ExternalUse {
            self.lifetime.set(Lifetime::SynchronousReturn);
        }
    }
    // These concrete inline calls cannot return a handle or accept an arbitrary
    // callback. All external handle access opts out permanently, including setup.
    pub(super) fn run_legacy(
        &self,
        id: String,
        request: StartInvestigationRequest,
        execution: JobExecution,
        provider: Arc<dyn LlmProvider>,
        live: Arc<LiveTask>,
        started: Instant,
    ) {
        self.begin();
        super::run(
            self.app.handle().clone(),
            id,
            request,
            execution,
            provider,
            live,
            started,
        );
        self.end();
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
        self.begin();
        super::run_completion(
            self.app.handle().clone(),
            id,
            request,
            execution,
            provider,
            live,
            started,
        );
        self.end();
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
impl Drop for FixtureApp {
    fn drop(&mut self) {
        if self.lifetime.get() != Lifetime::SynchronousReturn {
            return;
        }
        // SAFETY: the only eligibility setters wrap concrete synchronous workers;
        // their normal return drops worker State and runs Cleanup. Forwarded
        // State borrows are tied to this owner. Raw App is private and no Deref
        // exposes it: every handle-out permanently disables release, so detached
        // workers/callbacks cannot coexist with unmanage. Panic keeps Retain.
        #[allow(deprecated)]
        drop(self.app.unmanage::<AppState>());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synchronous_worker_fixture_releases_stores_only_after_normal_return() {
        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = Fixture::new(provider.as_ref());
        let sources = Arc::downgrade(&fixture.app.try_state::<AppState>().unwrap().sources);
        fixture.run(provider);
        drop(fixture);
        assert!(
            sources.upgrade().is_none(),
            "synchronous AppState was retained"
        );

        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = Fixture::new(provider.as_ref());
        let sources = Arc::downgrade(&fixture.app.state::<AppState>().sources);
        fixture.run(provider);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            fixture.app.begin();
            panic!("unfinished second inline run");
        }));
        assert!(unwind.is_err());
        assert!(
            sources.upgrade().is_some(),
            "unfinished run must retain state"
        );
    }

    #[test]
    fn escaped_handle_permanently_prevents_release_after_inline_worker() {
        let provider = Arc::new(Scripted::new(Mode::Finish));
        let fixture = Fixture::new(provider.as_ref());
        let escaped = fixture.app.handle().clone();
        fixture.run(provider);
        drop(fixture);
        assert!(escaped.try_state::<AppState>().is_some());
    }
}
