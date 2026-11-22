use super::{HostError, context::bounded_json};
use agents::investigation::*;
use llm::LlmProvider;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A desktop local provider: its completion model (#1643), context window and
/// loopback Ollama port (#1653).
type LocalProviderKey = (String, Option<u32>, u16);

/// App-lifetime clients and transient worker payloads. Durable task state and
/// deduplication belong exclusively to JobStore's coordinator tables.
#[derive(Default)]
pub(crate) struct InvestigationRuntime {
    providers: Mutex<BTreeMap<InvestigationProviderMode, Arc<dyn LlmProvider>>>,
    /// Local providers for a configured completion model (#1643) and context
    /// window (#1653), by both.
    local_models: Mutex<BTreeMap<LocalProviderKey, Arc<dyn LlmProvider>>>,
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

    /// Drop the cached cloud client so the next start builds one with the
    /// key that is current now (#1763). Running tasks keep the client they hold.
    pub(crate) fn forget_cloud_provider(&self) {
        if let Ok(mut providers) = self.providers.lock() {
            providers.remove(&InvestigationProviderMode::Cloud);
        }
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
            // The default port: a saved port gets its own provider in
            // `provider_for_start` (#1653).
            crate::egress::escalation_provider(
                match mode {
                    InvestigationProviderMode::Local => "local",
                    InvestigationProviderMode::Cloud => "cloud",
                },
                llm::OllamaProvider::DEFAULT_PORT,
            )
            .map_err(|_| HostError::Operational)?,
        );
        provider
            .bounded_profile()
            .map_err(|_| HostError::InvalidInput)?;
        providers.insert(mode, provider.clone());
        Ok(provider)
    }

    /// The provider a new desktop start uses: the configured local completion
    /// model (#1643), context window and Ollama port (#1653) when any differs
    /// from the shipped default, otherwise the mode's default provider.
    /// Construction makes no network call; a model that is not installed, or
    /// a port with no Ollama, fails at its first request and is reported.
    pub(super) fn provider_for_start(
        &self,
        mode: InvestigationProviderMode,
        local_model: Option<&str>,
        context_window: Option<u32>,
        port: Option<u16>,
    ) -> Result<Arc<dyn LlmProvider>, HostError> {
        if mode != InvestigationProviderMode::Local {
            return self.provider(mode);
        }
        let model = local_model.unwrap_or(llm::OllamaProvider::DEFAULT_COMPLETION_MODEL);
        let port = port.unwrap_or(llm::OllamaProvider::DEFAULT_PORT);
        if model == llm::OllamaProvider::DEFAULT_COMPLETION_MODEL
            && context_window.is_none()
            && port == llm::OllamaProvider::DEFAULT_PORT
        {
            return self.provider(mode);
        }
        let mut providers = self
            .local_models
            .lock()
            .map_err(|_| HostError::Operational)?;
        let key = (model.to_owned(), context_window, port);
        if let Some(provider) = providers.get(&key) {
            return Ok(provider.clone());
        }
        let mut local = llm::OllamaProvider::with_models(
            &llm::OllamaProvider::loopback_url(port),
            llm::OllamaProvider::DEFAULT_EMBEDDING_MODEL,
            model,
            Duration::from_secs(120),
        )
        .map_err(|_| HostError::Operational)?;
        if let Some(tokens) = context_window {
            local = local
                .with_context_window(tokens)
                .map_err(|_| HostError::InvalidInput)?;
        }
        let provider: Arc<dyn LlmProvider> = Arc::new(local);
        provider
            .bounded_profile()
            .map_err(|_| HostError::InvalidInput)?;
        providers.insert(key, provider.clone());
        Ok(provider)
    }

    /// The provider a new desktop start would use, as the catalog shows it:
    /// the configured local model (#1643) when one is saved.
    pub(super) fn descriptor_for_start(
        &self,
        mode: InvestigationProviderMode,
        local_model: Option<&str>,
        context_window: Option<u32>,
        port: Option<u16>,
    ) -> InvestigationProvider {
        self.provider_for_start(mode, local_model, context_window, port)
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

/// Registry defaults (`InvestigationLimits::default()`) used only until the
/// task's saved limits are applied at run start (AC-0562, AC-0710).
const DEFAULT_WALL_LIMIT: Duration = Duration::from_secs(3600);
const DEFAULT_ACTIVE_LIMIT: Duration = Duration::from_secs(600);
const DEFAULT_CONSENT_WAIT_LIMIT: Duration = Duration::from_secs(900);

pub(super) struct RunClock {
    started: Instant,
    consent_waiting: Duration,
    active_limit: Duration,
    wall_limit: Duration,
    consent_wait_limit: Duration,
    #[cfg(test)]
    active_elapsed_override: Option<Duration>,
    #[cfg(test)]
    expire_at_invocation: Option<u32>,
}

impl RunClock {
    pub(super) fn new(started: Instant) -> Self {
        Self {
            started,
            consent_waiting: Duration::ZERO,
            active_limit: DEFAULT_ACTIVE_LIMIT,
            wall_limit: DEFAULT_WALL_LIMIT,
            consent_wait_limit: DEFAULT_CONSENT_WAIT_LIMIT,
            #[cfg(test)]
            active_elapsed_override: None,
            #[cfg(test)]
            expire_at_invocation: None,
        }
    }
    /// Apply the task's saved time limits, so the stored `limits` are the one
    /// source of truth for when a run ends (AC-0562, AC-0710).
    pub(super) fn apply_saved_limits(&mut self, limits: &InvestigationLimits) {
        self.active_limit = Duration::from_secs(limits.active_seconds);
        self.wall_limit = Duration::from_secs(limits.wall_seconds);
        self.consent_wait_limit = Duration::from_secs(limits.consent_wait_seconds);
    }
    /// The longest one consent wait may last: the saved consent budget,
    /// never past the wall-clock cap.
    pub(super) fn consent_wait_remaining(&self) -> Duration {
        self.remaining_wall().min(self.consent_wait_limit)
    }
    #[cfg(test)]
    pub(super) fn frozen_active(started: Instant) -> Self {
        Self {
            started,
            consent_waiting: Duration::ZERO,
            active_limit: DEFAULT_ACTIVE_LIMIT,
            wall_limit: DEFAULT_WALL_LIMIT,
            consent_wait_limit: DEFAULT_CONSENT_WAIT_LIMIT,
            active_elapsed_override: Some(Duration::ZERO),
            expire_at_invocation: None,
        }
    }
    #[cfg(test)]
    pub(super) fn expiring_at_invocation(invocation: u32) -> Self {
        let mut clock = Self::frozen_active(Instant::now());
        clock.expire_at_invocation = Some(invocation);
        clock
    }
    #[cfg(test)]
    pub(super) fn invocation_committed(&mut self, invocation: u32) {
        if self.expire_at_invocation == Some(invocation) {
            self.active_elapsed_override = Some(self.active_limit + Duration::from_millis(1));
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
        self.wall_limit
            .saturating_sub(now.saturating_duration_since(self.started))
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
            .provider_for_start(local, Some("qwen3:30b-a3b"), None, None)
            .unwrap();
        assert_eq!(
            descriptor(chosen.as_ref(), local).unwrap().model,
            "qwen3:30b-a3b"
        );
        let again = runtime
            .provider_for_start(local, Some("qwen3:30b-a3b"), None, None)
            .unwrap();
        assert!(Arc::ptr_eq(&chosen, &again));
        let default = runtime
            .provider_for_start(
                local,
                Some(llm::OllamaProvider::DEFAULT_COMPLETION_MODEL),
                None,
                None,
            )
            .unwrap();
        assert!(Arc::ptr_eq(&default, &runtime.provider(local).unwrap()));
        assert_eq!(
            descriptor(default.as_ref(), local).unwrap().model,
            llm::OllamaProvider::DEFAULT_COMPLETION_MODEL
        );
        assert!(Arc::ptr_eq(
            &runtime.provider_for_start(local, None, None, None).unwrap(),
            &default
        ));
        // The catalog shows what a desktop start would use.
        assert_eq!(
            runtime
                .descriptor_for_start(local, Some("qwen3:30b-a3b"), None, None)
                .model,
            "qwen3:30b-a3b"
        );
        assert_eq!(
            runtime.descriptor_for_start(local, None, None, None).model,
            llm::OllamaProvider::DEFAULT_COMPLETION_MODEL
        );
    }
    // #1653: a chosen context window gets its own provider, named by it,
    // with the default or a chosen model; none keeps the mode provider.
    #[test]
    fn configured_context_window_selects_its_own_provider() {
        let runtime = InvestigationRuntime::default();
        let local = InvestigationProviderMode::Local;
        let windowed = runtime
            .provider_for_start(local, None, Some(32768), None)
            .unwrap();
        assert!(!Arc::ptr_eq(&windowed, &runtime.provider(local).unwrap()));
        assert_eq!(
            descriptor(windowed.as_ref(), local).unwrap().provider_id,
            format!(
                "ollama:{}+{}:num-ctx-32768",
                llm::OllamaProvider::DEFAULT_EMBEDDING_MODEL,
                llm::OllamaProvider::DEFAULT_COMPLETION_MODEL
            )
        );
        assert!(Arc::ptr_eq(
            &windowed,
            &runtime
                .provider_for_start(local, None, Some(32768), None)
                .unwrap()
        ));
        let both = runtime
            .provider_for_start(local, Some("qwen3:30b-a3b"), Some(8192), None)
            .unwrap();
        assert!(
            descriptor(both.as_ref(), local)
                .unwrap()
                .provider_id
                .ends_with("qwen3:30b-a3b:num-ctx-8192")
        );
        assert!(
            runtime
                .provider_for_start(local, None, Some(5000), None)
                .is_err()
        );
        let cloud = InvestigationProviderMode::Cloud;
        // The cloud provider ignores the window: same provider, or the same
        // error when no cloud key is configured.
        match (
            runtime.provider_for_start(cloud, None, Some(32768), None),
            runtime.provider(cloud),
        ) {
            (Ok(windowed), Ok(plain)) => assert!(Arc::ptr_eq(&windowed, &plain)),
            (Err(windowed), Err(plain)) => {
                assert_eq!(format!("{windowed:?}"), format!("{plain:?}"))
            }
            _ => panic!("the context window changed the cloud provider"),
        }
    }
    // #1653: a saved Ollama port gets its own loopback provider, recorded in
    // the descriptor's endpoint; the default port keeps the mode provider.
    #[test]
    fn configured_ollama_port_selects_its_own_loopback_provider() {
        let runtime = InvestigationRuntime::default();
        let local = InvestigationProviderMode::Local;
        let moved = runtime
            .provider_for_start(local, None, None, Some(11500))
            .unwrap();
        assert!(!Arc::ptr_eq(&moved, &runtime.provider(local).unwrap()));
        assert_eq!(
            descriptor(moved.as_ref(), local).unwrap().endpoint,
            "http://127.0.0.1:11500/api/chat"
        );
        assert!(Arc::ptr_eq(
            &runtime
                .provider_for_start(local, None, None, Some(llm::OllamaProvider::DEFAULT_PORT))
                .unwrap(),
            &runtime.provider(local).unwrap()
        ));
        let windowed = runtime
            .provider_for_start(local, None, Some(16384), Some(11500))
            .unwrap();
        assert!(!Arc::ptr_eq(&windowed, &moved));
        assert_eq!(
            runtime
                .descriptor_for_start(local, None, None, Some(11500))
                .endpoint,
            "http://127.0.0.1:11500/api/chat"
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
        clock.apply_saved_limits(&InvestigationLimits {
            active_seconds: 3600,
            ..InvestigationLimits::default()
        });
        assert_eq!(
            clock.remaining_deadline_at(started).unwrap(),
            Duration::from_secs(2900)
        );
    }
    // AC-0287: synthetic active time preserves the real wall deadline.
    #[test]
    fn frozen_active_time_keeps_real_wall_deadline() {
        let started = Instant::now();
        let expired = started.checked_add(DEFAULT_WALL_LIMIT).unwrap();
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
            DEFAULT_WALL_LIMIT.as_millis() as u64
        );
    }
    // AC-0710: the pre-start clock matches the registry defaults, and the
    // saved wall-clock and consent-wait budgets, not constants, bound the run.
    #[test]
    fn saved_wall_and_consent_limits_replace_constants() {
        let defaults = InvestigationLimits::default();
        assert_eq!(DEFAULT_ACTIVE_LIMIT.as_secs(), defaults.active_seconds);
        assert_eq!(DEFAULT_WALL_LIMIT.as_secs(), defaults.wall_seconds);
        assert_eq!(
            DEFAULT_CONSENT_WAIT_LIMIT.as_secs(),
            defaults.consent_wait_seconds
        );
        assert_eq!(defaults.active_seconds, 600);
        for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
            let started = Instant::now();
            let mut applied = RunClock::frozen_active(started);
            applied.apply_saved_limits(&policy.limits());
            let unapplied = RunClock::frozen_active(started);
            assert_eq!(
                applied.remaining_wall_at(started),
                unapplied.remaining_wall_at(started)
            );
            assert_eq!(applied.consent_wait_limit, unapplied.consent_wait_limit);
            assert_eq!(
                applied.remaining_deadline_at(started),
                unapplied.remaining_deadline_at(started)
            );
        }

        let started = Instant::now();
        let mut clock = RunClock::frozen_active(started);
        let mut limits = InvestigationLimits {
            wall_seconds: 120,
            consent_wait_seconds: 30,
            ..InvestigationLimits::default()
        };
        clock.apply_saved_limits(&limits);
        assert_eq!(clock.remaining_wall_at(started), Duration::from_secs(120));
        assert!(clock.consent_wait_remaining() <= Duration::from_secs(30));
        assert!(clock.consent_wait_remaining() > Duration::from_secs(29));
        // The wall cap ends a run even with active time left.
        let later = started.checked_add(Duration::from_secs(120)).unwrap();
        assert_eq!(
            clock.remaining_deadline_at(later),
            Err(HostError::LimitExceeded)
        );
        // A consent budget longer than the wall cap is cut at the wall cap.
        limits.consent_wait_seconds = 900;
        clock.apply_saved_limits(&limits);
        assert!(clock.consent_wait_remaining() <= Duration::from_secs(120));
    }
}
