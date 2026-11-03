use super::{HostError, context::bounded_json};
use agents::investigation::*;
use llm::LlmProvider;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// App-lifetime clients and transient worker payloads. Durable task state and
/// deduplication belong exclusively to JobStore's coordinator tables.
#[derive(Default)]
pub(crate) struct InvestigationRuntime {
    providers: Mutex<BTreeMap<InvestigationProviderMode, Arc<dyn LlmProvider>>>,
    /// Local providers for a configured completion model (#1643), by model.
    local_models: Mutex<BTreeMap<String, Arc<dyn LlmProvider>>>,
    live: Mutex<BTreeMap<String, Arc<LiveTask>>>,
}

impl InvestigationRuntime {
    #[cfg(test)]
    pub(super) fn set_test_provider(
        &self,
        mode: InvestigationProviderMode,
        provider: Arc<dyn LlmProvider>,
    ) {
        self.providers.lock().unwrap().insert(mode, provider);
    }

    pub(super) fn provider(
        &self,
        mode: InvestigationProviderMode,
    ) -> Result<Arc<dyn LlmProvider>, HostError> {
        let mut providers = self.providers.lock().map_err(|_| HostError::Operational)?;
        if let Some(provider) = providers.get(&mode) {
            return Ok(provider.clone());
        }
        // Constructors make no HTTP/model calls. Reuse their blocking runtimes
        // for the application lifetime, including after a bounded request ends.
        let provider: Arc<dyn LlmProvider> = Arc::from(
            crate::egress::escalation_provider(match mode {
                InvestigationProviderMode::Local => "local",
                InvestigationProviderMode::Cloud => "cloud",
            })
            .map_err(|_| HostError::Operational)?,
        );
        provider
            .bounded_profile()
            .map_err(|_| HostError::InvalidInput)?;
        providers.insert(mode, provider.clone());
        Ok(provider)
    }

    /// The provider a new desktop start uses: the configured local completion
    /// model (#1643) when it differs from the shipped default, otherwise the
    /// mode's default provider. Construction makes no network call; a model
    /// that is not installed fails at its first request and is reported.
    pub(super) fn provider_for_start(
        &self,
        mode: InvestigationProviderMode,
        local_model: Option<&str>,
    ) -> Result<Arc<dyn LlmProvider>, HostError> {
        let model = match (mode, local_model) {
            (InvestigationProviderMode::Local, Some(model))
                if model != llm::OllamaProvider::DEFAULT_COMPLETION_MODEL =>
            {
                model
            }
            _ => return self.provider(mode),
        };
        let mut providers = self
            .local_models
            .lock()
            .map_err(|_| HostError::Operational)?;
        if let Some(provider) = providers.get(model) {
            return Ok(provider.clone());
        }
        let provider: Arc<dyn LlmProvider> = Arc::new(
            llm::OllamaProvider::with_models(
                llm::OllamaProvider::DEFAULT_URL,
                llm::OllamaProvider::DEFAULT_EMBEDDING_MODEL,
                model,
                Duration::from_secs(120),
            )
            .map_err(|_| HostError::Operational)?,
        );
        provider
            .bounded_profile()
            .map_err(|_| HostError::InvalidInput)?;
        providers.insert(model.to_owned(), provider.clone());
        Ok(provider)
    }

    /// The provider a new desktop start would use, as the catalog shows it:
    /// the configured local model (#1643) when one is saved.
    pub(super) fn descriptor_for_start(
        &self,
        mode: InvestigationProviderMode,
        local_model: Option<&str>,
    ) -> InvestigationProvider {
        self.provider_for_start(mode, local_model)
            .and_then(|p| descriptor(p.as_ref(), mode))
            .unwrap_or_else(|_| InvestigationProvider {
                mode,
                provider_id: match mode {
                    InvestigationProviderMode::Local => "ollama",
                    InvestigationProviderMode::Cloud => "anthropic",
                }
                .into(),
                model: String::new(),
                endpoint: String::new(),
                deployment: None,
                protocol_version: None,
                available: false,
                unavailable_reason: Some(
                    "A compatible provider is not configured. No model request has been made."
                        .into(),
                ),
            })
    }

    pub(super) fn insert(&self, id: &str) -> Result<Arc<LiveTask>, HostError> {
        let mut live = self.live.lock().map_err(|_| HostError::Operational)?;
        if live.contains_key(id) || live.len() >= crate::jobs::investigations::MAX_LIVE_TASKS {
            return Err(HostError::Operational);
        }
        let task = Arc::new(LiveTask::default());
        live.insert(id.into(), task.clone());
        Ok(task)
    }

    pub(super) fn get(&self, id: &str) -> Result<Option<Arc<LiveTask>>, HostError> {
        Ok(self
            .live
            .lock()
            .map_err(|_| HostError::Operational)?
            .get(id)
            .cloned())
    }

    pub(super) fn remove(&self, id: &str) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(id);
        }
    }

    pub(crate) fn wake(&self, id: &str) {
        if let Ok(Some(task)) = self.get(id) {
            task.wake();
        }
    }
}

pub(super) fn descriptor(
    provider: &dyn LlmProvider,
    mode: InvestigationProviderMode,
) -> Result<InvestigationProvider, HostError> {
    let profile = provider
        .bounded_profile()
        .map_err(|_| HostError::InvalidInput)?;
    if (profile.locality == llm::Locality::Local) != (mode == InvestigationProviderMode::Local) {
        return Err(HostError::InvalidInput);
    }
    let descriptor = InvestigationProvider {
        mode,
        provider_id: profile.provider_id,
        model: profile.requested_model,
        endpoint: profile.endpoint_id,
        deployment: None,
        protocol_version: Some(profile.protocol_version),
        available: true,
        unavailable_reason: None,
    };
    bounded_json(&descriptor, 8192)?;
    Ok(descriptor)
}

#[derive(Default)]
pub(super) struct LiveTask {
    pub pending: Mutex<Option<InvestigationConsent>>,
    generation: Mutex<u64>,
    changed: Condvar,
}

impl LiveTask {
    pub(super) fn wake(&self) {
        if let Ok(mut generation) = self.generation.lock() {
            *generation = generation.wrapping_add(1);
            self.changed.notify_all();
        }
    }
    /// The durable state is rechecked at most one second later even if a
    /// notification races entry or originated in another app process.
    pub(super) fn wait(&self, remaining: Duration) -> Result<(), HostError> {
        let generation = self.generation.lock().map_err(|_| HostError::Operational)?;
        let _guard = self
            .changed
            .wait_timeout(generation, remaining.min(Duration::from_secs(1)))
            .map_err(|_| HostError::Operational)?;
        Ok(())
    }
}

const WALL_LIMIT: Duration = Duration::from_secs(3600);

/// The default active allowance; saved task limits replace it at run start.
const DEFAULT_ACTIVE_LIMIT: Duration = Duration::from_secs(600);

pub(super) struct RunClock {
    started: Instant,
    consent_waiting: Duration,
    active_limit: Duration,
    #[cfg(test)]
    active_elapsed_override: Option<Duration>,
}

impl RunClock {
    pub(super) fn new(started: Instant) -> Self {
        Self {
            started,
            consent_waiting: Duration::ZERO,
            active_limit: DEFAULT_ACTIVE_LIMIT,
            #[cfg(test)]
            active_elapsed_override: None,
        }
    }
    /// Apply the task's saved active-time allowance (AC-0562).
    pub(super) fn set_active_limit(&mut self, active_seconds: u64) {
        self.active_limit = Duration::from_secs(active_seconds);
    }
    #[cfg(test)]
    pub(super) fn frozen_active(started: Instant) -> Self {
        Self {
            started,
            consent_waiting: Duration::ZERO,
            active_limit: DEFAULT_ACTIVE_LIMIT,
            active_elapsed_override: Some(Duration::ZERO),
        }
    }
    fn active_milliseconds_at(&self, now: Instant) -> u64 {
        #[cfg(test)]
        let elapsed = self
            .active_elapsed_override
            .unwrap_or_else(|| now.saturating_duration_since(self.started));
        #[cfg(not(test))]
        let elapsed = now.saturating_duration_since(self.started);
        elapsed
            .saturating_sub(self.consent_waiting)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    pub(super) fn active_milliseconds(&self) -> u64 {
        self.active_milliseconds_at(Instant::now())
    }
    pub(super) fn record_wait(&mut self, elapsed: Duration) {
        self.consent_waiting = self.consent_waiting.saturating_add(elapsed);
    }
    fn remaining_wall_at(&self, now: Instant) -> Duration {
        WALL_LIMIT.saturating_sub(now.saturating_duration_since(self.started))
    }
    pub(super) fn remaining_wall(&self) -> Duration {
        self.remaining_wall_at(Instant::now())
    }
    pub(super) fn deadline(&self) -> Result<Instant, HostError> {
        let remaining = self.remaining_deadline_at(Instant::now())?;
        Instant::now()
            .checked_add(remaining)
            .ok_or(HostError::LimitExceeded)
    }
    fn remaining_deadline_at(&self, now: Instant) -> Result<Duration, HostError> {
        let active = Duration::from_millis(self.active_milliseconds_at(now));
        let remaining = self
            .active_limit
            .saturating_sub(active)
            .min(self.remaining_wall_at(now));
        if remaining.is_zero() {
            Err(HostError::LimitExceeded)
        } else {
            Ok(remaining)
        }
    }
}

#[cfg(test)]
mod run_clock_tests {
    use super::*;
    // AC-0564: a desktop start with a configured local model gets a provider
    // for that model; the default model and cloud keep the mode provider.
    #[test]
    fn configured_local_model_selects_its_own_provider() {
        let runtime = InvestigationRuntime::default();
        let local = InvestigationProviderMode::Local;
        let chosen = runtime
            .provider_for_start(local, Some("qwen3:30b-a3b"))
            .unwrap();
        assert_eq!(
            descriptor(chosen.as_ref(), local).unwrap().model,
            "qwen3:30b-a3b"
        );
        let again = runtime
            .provider_for_start(local, Some("qwen3:30b-a3b"))
            .unwrap();
        assert!(Arc::ptr_eq(&chosen, &again));
        let default = runtime
            .provider_for_start(local, Some(llm::OllamaProvider::DEFAULT_COMPLETION_MODEL))
            .unwrap();
        assert!(Arc::ptr_eq(&default, &runtime.provider(local).unwrap()));
        assert_eq!(
            descriptor(default.as_ref(), local).unwrap().model,
            llm::OllamaProvider::DEFAULT_COMPLETION_MODEL
        );
        assert!(Arc::ptr_eq(
            &runtime.provider_for_start(local, None).unwrap(),
            &default
        ));
        // The catalog shows what a desktop start would use.
        assert_eq!(
            runtime
                .descriptor_for_start(local, Some("qwen3:30b-a3b"))
                .model,
            "qwen3:30b-a3b"
        );
        assert_eq!(
            runtime.descriptor_for_start(local, None).model,
            llm::OllamaProvider::DEFAULT_COMPLETION_MODEL
        );
    }
    // AC-0562: a saved active allowance above the default extends the deadline.
    #[test]
    fn saved_active_limit_replaces_default_deadline() {
        let started = Instant::now();
        let mut clock = RunClock::new(started);
        clock.active_elapsed_override = Some(Duration::from_secs(700));
        assert!(matches!(
            clock.remaining_deadline_at(started),
            Err(HostError::LimitExceeded)
        ));
        clock.set_active_limit(3600);
        assert_eq!(
            clock.remaining_deadline_at(started).unwrap(),
            Duration::from_secs(2900)
        );
    }
    // AC-0287: synthetic active time preserves the real wall deadline.
    #[test]
    fn frozen_active_time_keeps_real_wall_deadline() {
        let started = Instant::now();
        let expired = started.checked_add(WALL_LIMIT).unwrap();
        let frozen = RunClock::frozen_active(started);
        assert_eq!(frozen.active_milliseconds_at(expired), 0);
        assert_eq!(frozen.remaining_wall_at(expired), Duration::ZERO);
        assert_eq!(
            frozen.remaining_deadline_at(expired),
            Err(HostError::LimitExceeded)
        );

        let real = RunClock::new(started);
        assert_eq!(
            real.active_milliseconds_at(expired),
            WALL_LIMIT.as_millis() as u64
        );
    }
}
