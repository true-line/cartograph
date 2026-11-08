//! Owner-authorized hosted acceptance runs (#1709, owner decision 2026-11-07).
//!
//! The frozen definition names a Cloud provider at a pinned production
//! endpoint. The owner's go for one run is an environment variable naming that
//! exact provider id, plus a hard step cap. Under that authority only:
//!
//! - T3 cloud consent is granted in the reopened profile's settings. It is
//!   refused if T3 already uses the cloud, so nothing pre-existing is
//!   overwritten. When the run ends, T3 is reverted to local (which revokes
//!   the consent) and its prior enabled flag is restored.
//! - Each pending step is approved through the app's own consent path
//!   ([`consent_decision`]), with the same exact step, revision and payload-hash
//!   check the UI uses. When the cap is reached, the next step is declined.
//! - A durable, append-only consent log records each event before it takes
//!   effect: the grant, every approval (step, revision, payload hash, payload
//!   bytes, redaction count), the cap decline and the revert.
//!
//! API keys are read from the environment of the owner-run process and are
//! never logged.
use crate::investigations::{
    commands::{consent_decision, pending},
    runtime::descriptor,
};
use agents::investigation::{InvestigationProvider, InvestigationProviderMode};
use llm::LlmProvider;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::Manager;

/// The tier whose egress the investigation analyst uses.
const TIER: &str = "T3";
/// The highest step cap an authorization may name.
const MAX_STEP_CAP: usize = 64;
pub(super) const AUTHORIZE: &str = "CARTOGRAPH_ACCEPTANCE_CLOUD_AUTHORIZE";
pub(super) const STEP_CAP: &str = "CARTOGRAPH_ACCEPTANCE_CLOUD_STEP_CAP";

/// The owner's go for one hosted run of one exact frozen provider.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Authorization {
    step_cap: usize,
}

/// Lanes with a bounded profile, by exact model id.
fn claude_lane(model: &str) -> Option<llm::anthropic::ClaudeLane> {
    use llm::anthropic::ClaudeLane;
    [ClaudeLane::Haiku, ClaudeLane::Opus]
        .into_iter()
        .find(|lane| lane.model_id() == model)
}

/// Only pinned production endpoints qualify as frozen cloud identities.
pub(super) fn pinned(expected: &InvestigationProvider) -> Result<(), String> {
    let ok = expected.mode == InvestigationProviderMode::Cloud
        && expected.available
        && match expected.provider_id.split_once(':') {
            Some(("anthropic", model)) => {
                expected.endpoint == llm::anthropic::API_URL
                    && expected.model == model
                    && claude_lane(model).is_some()
            }
            Some(("openai", model)) => {
                expected.endpoint == llm::openai::API_URL
                    && expected.model == model
                    && !model.is_empty()
            }
            _ => false,
        };
    ok.then_some(())
        .ok_or_else(|| "Cloud definition must name a pinned hosted provider.".into())
}

/// Reads the authorization for `expected`. `None` values mean unset.
pub(super) fn authorization(
    expected: &InvestigationProvider,
    authorized: Option<&str>,
    step_cap: Option<&str>,
) -> Result<Authorization, String> {
    pinned(expected)?;
    if authorized != Some(expected.provider_id.as_str()) {
        return Err(format!(
            "Hosted run not authorized: set {AUTHORIZE} to the exact frozen provider id."
        ));
    }
    let step_cap = step_cap
        .and_then(|cap| cap.parse::<usize>().ok())
        .filter(|cap| (1..=MAX_STEP_CAP).contains(cap))
        .ok_or_else(|| format!("Set {STEP_CAP} to a step cap from 1 to {MAX_STEP_CAP}."))?;
    Ok(Authorization { step_cap })
}

pub(super) fn authorization_from_env(
    expected: &InvestigationProvider,
) -> Result<Authorization, String> {
    authorization(
        expected,
        std::env::var(AUTHORIZE).ok().as_deref(),
        std::env::var(STEP_CAP).ok().as_deref(),
    )
}

/// Constructs the hosted provider for `expected`. Construction makes no
/// network call, and the provider must reproduce the frozen identity exactly.
pub(super) fn provider(
    expected: &InvestigationProvider,
    key: impl Fn(&str) -> Option<String>,
) -> Result<Arc<dyn LlmProvider>, String> {
    pinned(expected)?;
    let provider: Arc<dyn LlmProvider> = match expected.provider_id.split_once(':') {
        Some(("anthropic", model)) => Arc::new(
            llm::anthropic::AnthropicProvider::new(
                claude_lane(model).ok_or("Unsupported Claude lane.")?,
                key("ANTHROPIC_API_KEY").ok_or("ANTHROPIC_API_KEY is not set.")?,
            )
            .map_err(|e| e.to_string())?,
        ),
        Some(("openai", model)) => Arc::new(
            llm::openai::OpenAiProvider::new(
                model,
                key("OPENAI_API_KEY").ok_or("OPENAI_API_KEY is not set.")?,
            )
            .map_err(|e| e.to_string())?,
        ),
        _ => return Err("Unsupported hosted provider.".into()),
    };
    if descriptor(provider.as_ref(), InvestigationProviderMode::Cloud).map_err(|e| e.to_string())?
        != *expected
    {
        return Err("Hosted configuration differs from frozen identity.".into());
    }
    Ok(provider)
}

struct Log(File);
impl Log {
    fn create(path: &Path) -> Result<Self, String> {
        let mut options = OpenOptions::new();
        options.create_new(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(|e| e.to_string())?;
        super::vendure_prepare::sync_directory(path.parent().ok_or("Missing log parent.")?)?;
        Ok(Self(file))
    }
    /// Durable before the recorded event takes effect.
    fn record(&mut self, event: Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(&event).map_err(|e| e.to_string())?;
        line.push(b'\n');
        self.0
            .write_all(&line)
            .and_then(|_| self.0.sync_data())
            .map_err(|e| e.to_string())
    }
}

/// The consent log shared by the grant, the approver and the revert. A panic
/// elsewhere never stops the revert from being recorded.
fn locked(log: &Mutex<Log>) -> MutexGuard<'_, Log> {
    log.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Scoped T3 cloud consent for one hosted run. Dropping it, including after a
/// partial grant, records and then reverts T3 to local, restoring its prior
/// enabled flag.
pub(super) struct Consent {
    handle: tauri::AppHandle<tauri::test::MockRuntime>,
    log: Arc<Mutex<Log>>,
    prior_enabled: bool,
    step_cap: usize,
    reverted: bool,
}

impl Consent {
    /// Grants T3 cloud consent for `expected`, creating the consent log
    /// exclusively at `log`. Refuses when T3 is not currently local.
    pub(super) fn grant(
        handle: &tauri::AppHandle<tauri::test::MockRuntime>,
        expected: &InvestigationProvider,
        authorization: &Authorization,
        log: &Path,
    ) -> Result<Self, String> {
        let mut log = Log::create(log)?;
        let state = handle.state::<crate::AppState>();
        let mut settings = state.settings.lock().map_err(|_| "Settings unavailable.")?;
        let prior = settings
            .all()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|tier| tier.tier == TIER)
            .ok_or("T3 settings missing.")?;
        if prior.provider != "local" {
            return Err("T3 already uses the cloud; refusing to overwrite its settings.".into());
        }
        log.record(json!({"event": "tier_consent_granted", "tier": TIER,
            "provider_id": expected.provider_id, "endpoint": expected.endpoint,
            "step_cap": authorization.step_cap}))?;
        let consent = Self {
            handle: handle.clone(),
            log: Arc::new(Mutex::new(log)),
            prior_enabled: prior.enabled,
            step_cap: authorization.step_cap,
            reverted: false,
        };
        let disclosure = json!({"provider_id": expected.provider_id, "model": expected.model,
            "endpoint": expected.endpoint, "authority": AUTHORIZE,
            "step_cap": authorization.step_cap})
        .to_string();
        // From here on, a failure drops `consent`, which reverts the tier. The
        // settings lock is released first, so that revert cannot deadlock.
        let applied = settings
            .set_enabled(TIER, true)
            .and_then(|_| settings.set_provider(TIER, "cloud"))
            .and_then(|_| settings.grant_consent(TIER, &disclosure));
        drop(settings);
        applied.map_err(|e| e.to_string())?;
        Ok(consent)
    }

    /// Runs `run` while auto-approving `investigation_id`'s pending steps up to
    /// the cap, then reverts the tier.
    pub(super) fn approve_while<R>(
        mut self,
        investigation_id: &str,
        run: impl FnOnce() -> R,
    ) -> Result<R, String> {
        let stop = Arc::new(AtomicBool::new(false));
        // If `run` panics, `approver` is dropped (stopped and joined) before
        // `self`, whose Drop then records and applies the revert.
        let mut approver = {
            let (handle, id, thread_stop, cap) = (
                self.handle.clone(),
                investigation_id.to_owned(),
                stop.clone(),
                self.step_cap,
            );
            let log = self.log.clone();
            Approver {
                stop,
                thread: Some(std::thread::spawn(move || {
                    approve(&handle, &id, cap, &thread_stop, &log)
                })),
            }
        };
        let result = run();
        let approvals = approver.finish();
        // Revert even when approval failed, so no standing consent survives.
        self.revert(approvals.as_ref().ok().copied())?;
        approvals?;
        Ok(result)
    }

    /// Durably records the revert request, applies it, then records the
    /// outcome. The revert is applied even when recording fails, so a log
    /// failure never leaves standing consent; any recording error is returned.
    fn revert(&mut self, approvals: Option<usize>) -> Result<(), String> {
        let requested = locked(&self.log).record(
            json!({"event": "tier_revert_requested", "tier": TIER, "approvals": approvals}),
        );
        let applied = self.apply_revert();
        let outcome = locked(&self.log).record(match &applied {
            Ok(()) => json!({"event": "tier_reverted_local", "tier": TIER}),
            Err(error) => json!({"event": "tier_revert_failed", "tier": TIER, "error": error}),
        });
        if applied.is_ok() {
            self.reverted = true;
        }
        applied.and(requested).and(outcome)
    }

    fn apply_revert(&self) -> Result<(), String> {
        let state = self.handle.state::<crate::AppState>();
        let mut settings = state.settings.lock().map_err(|_| "Settings unavailable.")?;
        // Switching to local also revokes the standing consent.
        settings
            .set_provider(TIER, "local")
            .and_then(|_| settings.set_enabled(TIER, self.prior_enabled))
            .map_err(|e| e.to_string())
    }
}

impl Drop for Consent {
    fn drop(&mut self) {
        if !self.reverted {
            let _ = self.revert(None);
        }
    }
}

/// The approver thread. It is stopped and joined on every exit, including
/// unwinding from a panic in the run it accompanies.
struct Approver {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<usize, String>>>,
}

impl Approver {
    fn finish(&mut self) -> Result<usize, String> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread
            .take()
            .ok_or("Consent approver already finished.")?
            .join()
            .map_err(|_| "Consent approver panicked.".to_string())?
    }
}

impl Drop for Approver {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.finish();
        }
    }
}

/// Approves each newly pending step until stopped. Returns the number of
/// approvals.
fn approve(
    handle: &tauri::AppHandle<tauri::test::MockRuntime>,
    id: &str,
    cap: usize,
    stop: &AtomicBool,
    log: &Mutex<Log>,
) -> Result<usize, String> {
    let mut seen: Vec<(String, u64, String)> = Vec::new();
    let mut approvals = 0;
    while !stop.load(Ordering::SeqCst) {
        let state = handle.state::<crate::AppState>();
        if let Some(step) = pending(&state, id)? {
            let key = (
                step.step_id.clone(),
                step.revision,
                step.preview.payload_hash.clone(),
            );
            if !seen.contains(&key) {
                seen.push(key);
                let approve = approvals < cap;
                let payload_bytes = serde_json::to_vec(&step.preview.payload)
                    .map_err(|e| e.to_string())?
                    .len();
                locked(log).record(json!({
                    "event": if approve { "step_approved" } else { "step_declined_cap" },
                    "n": approvals + usize::from(approve),
                    "step_id": step.step_id,
                    "revision": step.revision,
                    "payload_hash": step.preview.payload_hash,
                    "payload_bytes": payload_bytes,
                    "redaction_count": step.preview.redaction_count,
                    "provider_id": step.preview.provider_id,
                }))?;
                // The worker re-checks this exact step, revision and hash.
                let decided = consent_decision(
                    handle,
                    id,
                    &step.step_id,
                    step.revision,
                    &step.preview.payload_hash,
                    approve,
                );
                if let Err(error) = decided {
                    locked(log).record(json!({"event": "decision_failed",
                        "step_id": step.step_id, "revision": step.revision, "error": error}))?;
                } else if approve {
                    approvals += 1;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(approvals)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloud(provider_id: &str, model: &str, endpoint: &str) -> InvestigationProvider {
        InvestigationProvider {
            mode: InvestigationProviderMode::Cloud,
            provider_id: provider_id.into(),
            model: model.into(),
            endpoint: endpoint.into(),
            deployment: None,
            protocol_version: Some(llm::bounded::PROTOCOL_VERSION.into()),
            available: true,
            unavailable_reason: None,
        }
    }

    // AC-0569 / #1709: only a pinned hosted identity and an exact, capped
    // authorization admit a hosted run; provider construction reproduces the
    // frozen identity without any network call.
    #[test]
    fn hosted_run_requires_pinned_identity_and_exact_authorization() {
        let opus = cloud(
            "anthropic:claude-opus-5-5",
            "claude-opus-5-5",
            llm::anthropic::API_URL,
        );
        let sol = cloud("openai:gpt-6.1-sol", "gpt-6.1-sol", llm::openai::API_URL);
        for expected in [&opus, &sol] {
            assert_eq!(
                authorization(expected, Some(&expected.provider_id), Some("24")),
                Ok(Authorization { step_cap: 24 })
            );
            for (authorized, cap) in [
                (None, Some("24")),
                (Some("anthropic:claude-haiku-4-5-20251001"), Some("24")),
                (Some(expected.provider_id.as_str()), None),
                (Some(expected.provider_id.as_str()), Some("0")),
                (Some(expected.provider_id.as_str()), Some("65")),
                (Some(expected.provider_id.as_str()), Some("x")),
            ] {
                assert!(authorization(expected, authorized, cap).is_err());
            }
            let key = |_: &str| Some("fixture-key".to_string());
            assert!(provider(expected, key).is_ok());
            assert!(provider(expected, |_| None).is_err());
        }
        for unpinned in [
            cloud(
                "anthropic:claude-opus-5-5",
                "claude-opus-5-5",
                "https://example.com/v1/messages",
            ),
            cloud(
                "openai:gpt-6.1-sol",
                "gpt-6.1-sol",
                "https://example.com/v1/chat/completions",
            ),
            cloud(
                "anthropic:claude-fable-5",
                "claude-fable-5",
                llm::anthropic::API_URL,
            ),
            cloud("openai:gpt-6.1-sol", "gpt-6.1-terra", llm::openai::API_URL),
            cloud(
                "ollama:qwen3:8b",
                "qwen3:8b",
                "http://127.0.0.1:11434/api/chat",
            ),
            InvestigationProvider {
                mode: InvestigationProviderMode::Local,
                ..opus.clone()
            },
            InvestigationProvider {
                available: false,
                ..sol.clone()
            },
        ] {
            assert!(pinned(&unpinned).is_err(), "{}", unpinned.provider_id);
            assert!(authorization(&unpinned, Some(&unpinned.provider_id), Some("8")).is_err());
        }
    }

    /// A scripted provider claiming the pinned Opus identity (or a loopback
    /// identity for the fixture seed). It never opens a connection.
    struct ScriptedOpus {
        cloud: bool,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl LlmProvider for ScriptedOpus {
        fn id(&self) -> &str {
            if self.cloud {
                "anthropic:claude-opus-5-5"
            } else {
                "scripted-seed"
            }
        }
        fn locality(&self) -> llm::Locality {
            if self.cloud {
                llm::Locality::Cloud
            } else {
                llm::Locality::Local
            }
        }
        fn capabilities(&self) -> llm::ProviderCaps {
            llm::ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }
        fn supports_bounded_response_schema(&self) -> bool {
            true
        }
        fn embed(&self, _: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
            Err(llm::ProviderError::Unsupported("embeddings"))
        }
        fn bounded_profile(
            &self,
        ) -> Result<llm::bounded::ProviderProfile, llm::bounded::BoundedCallError> {
            Ok(llm::bounded::ProviderProfile {
                protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
                provider_id: self.id().into(),
                locality: self.locality(),
                endpoint_id: if self.cloud {
                    llm::anthropic::API_URL
                } else {
                    "http://127.0.0.1:11434/api/chat"
                }
                .into(),
                requested_model: "claude-opus-5-5".into(),
            })
        }
        fn complete_bounded(
            &self,
            request: &llm::bounded::AuthorizedBoundedCompletion,
            _: &llm::bounded::CompletionControl<'_>,
        ) -> Result<llm::bounded::BoundedCompletion, llm::bounded::BoundedCallError> {
            let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let text = match call {
                0 => json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                    "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],"max_facts":1,"max_bytes":32768,"cursor":null}}),
                1 => {
                    let target = &prompt["completion_contract"]["obligations"]["obligations"][0]["target"];
                    json!({"type":"read_evidence","fact":target["fact"],"role":target["role"],"index":target["index"]})
                }
                2 => {
                    let citation = prompt["citations"].as_array().unwrap().iter()
                        .find(|citation| citation["origin"]["kind"] == "captured_primary_source").unwrap();
                    json!({"schema_version":1,"outcome":"supported","action":{"type":"finish","findings":[{
                        "claim_kind":"inferred_interpretation","title":"Bounded source observation",
                        "statement":"The captured callable contains a conditional exit.","citation_ids":[citation["citation_id"]],
                        "limitations":["Runtime inputs remain unknown."]}],"knowledge_completeness":"partial",
                        "limitations":["This result establishes no complete business policy."]}})
                }
                _ => panic!("no fourth call"),
            }
            .to_string();
            Ok(llm::bounded::BoundedCompletion {
                text,
                requested_model: "claude-opus-5-5".into(),
                response_model: Some("claude-opus-5-5".into()),
                provider_request_id: None,
                usage: None,
                request_bytes: 100,
                response_bytes: 100,
            })
        }
    }

    fn terminal(
        handle: &tauri::AppHandle<tauri::test::MockRuntime>,
        id: &str,
    ) -> agents::investigation::InvestigationDetail {
        use agents::investigation::InvestigationStatus as S;
        let until = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let detail = handle
                .state::<crate::AppState>()
                .jobs
                .lock()
                .unwrap()
                .investigation(id)
                .unwrap();
            if matches!(
                detail.summary.status,
                S::Completed | S::Cancelled | S::Failed | S::OutcomeUnknown | S::Interrupted
            ) {
                return detail;
            }
            assert!(std::time::Instant::now() < until, "worker did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn t3(handle: &tauri::AppHandle<tauri::test::MockRuntime>) -> crate::settings::TierSettings {
        let state = handle.state::<crate::AppState>();
        let settings = state.settings.lock().unwrap();
        settings
            .all()
            .unwrap()
            .into_iter()
            .find(|tier| tier.tier == TIER)
            .unwrap()
    }

    // AC-0569 / #1709: the scoped approver drives a real cloud investigation
    // through the app's own consent path, logs every approval before it takes
    // effect, declines past its cap, and always reverts T3 to local.
    #[test]
    fn scoped_approver_runs_consents_and_reverts() {
        for (cap, approvals) in [(3, 3), (2, 2)] {
            let seed = Arc::new(ScriptedOpus {
                cloud: false,
                calls: Default::default(),
            });
            crate::investigations::worker::with_public_start_fixture(
                seed,
                false,
                |handle, mut request, app_data| {
                    let state = handle.state::<crate::AppState>();
                    let provider = Arc::new(ScriptedOpus {
                        cloud: true,
                        calls: Default::default(),
                    });
                    state
                        .investigations
                        .set_test_provider(InvestigationProviderMode::Cloud, provider.clone());
                    let expected =
                        descriptor(provider.as_ref(), InvestigationProviderMode::Cloud).unwrap();
                    assert!(pinned(&expected).is_ok());
                    let before = t3(handle);
                    assert_eq!(before.provider, "local");
                    let log = app_data.join(format!("consent-{cap}.jsonl"));
                    let consent =
                        Consent::grant(handle, &expected, &Authorization { step_cap: cap }, &log)
                            .unwrap();
                    assert!(t3(handle).consented);
                    // Only one scoped grant at a time.
                    assert!(
                        Consent::grant(
                            handle,
                            &expected,
                            &Authorization { step_cap: cap },
                            &app_data.join("second.jsonl")
                        )
                        .is_err()
                    );
                    request.specialist_id = "domain-analyst@10".into();
                    request.provider_mode = InvestigationProviderMode::Cloud;
                    let id = crate::investigations::commands::start_completion_for_test(
                        request,
                        handle.clone(),
                    )
                    .unwrap()
                    .investigation_id;
                    let detail = consent
                        .approve_while(&id, || terminal(handle, &id))
                        .unwrap();
                    assert_eq!(provider.calls.load(Ordering::SeqCst), approvals);
                    assert_eq!(
                        detail.summary.status,
                        if cap == 3 {
                            agents::investigation::InvestigationStatus::Completed
                        } else {
                            agents::investigation::InvestigationStatus::Cancelled
                        },
                        "{:?}",
                        detail.error
                    );
                    let after = t3(handle);
                    assert_eq!(
                        (after.provider.as_str(), after.consented, after.enabled),
                        ("local", false, before.enabled)
                    );
                    let events: Vec<Value> = std::fs::read_to_string(&log)
                        .unwrap()
                        .lines()
                        .map(|line| serde_json::from_str(line).unwrap())
                        .collect();
                    let kinds: Vec<&str> = events
                        .iter()
                        .map(|e| e["event"].as_str().unwrap())
                        .collect();
                    let mut want = vec!["tier_consent_granted"];
                    want.extend(std::iter::repeat_n("step_approved", approvals));
                    if cap < 3 {
                        want.push("step_declined_cap");
                    }
                    want.extend(["tier_revert_requested", "tier_reverted_local"]);
                    assert_eq!(kinds, want);
                    for event in events.iter().filter(|e| e["event"] == "step_approved") {
                        assert_eq!(event["payload_hash"].as_str().unwrap().len(), 64);
                        assert!(event["payload_bytes"].as_u64().unwrap() > 0);
                        assert_eq!(event["provider_id"], "anthropic:claude-opus-5-5");
                    }
                    assert_eq!(events[events.len() - 2]["approvals"], approvals);
                    if cap == 2 {
                        cleanup_paths_record_and_restore(handle, &expected, app_data);
                    }
                },
            );
        }
    }

    fn logged(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Every exceptional exit records the revert request before applying it,
    /// its outcome after, and leaves T3 local with its prior enabled flag.
    fn assert_cleaned_up(
        handle: &tauri::AppHandle<tauri::test::MockRuntime>,
        log: &Path,
        enabled: bool,
    ) {
        let after = t3(handle);
        assert_eq!(
            (after.provider.as_str(), after.consented, after.enabled),
            ("local", false, enabled)
        );
        let events = logged(log);
        assert_eq!(
            events
                .iter()
                .map(|e| e["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "tier_consent_granted",
                "tier_revert_requested",
                "tier_reverted_local"
            ]
        );
        assert!(events[1]["approvals"].is_null());
    }

    // AC-0569 / #1709 review: cleanup through Drop, a panicking run and a
    // settings write failing at each grant step all record and restore,
    // without deadlocking on the settings lock.
    fn cleanup_paths_record_and_restore(
        handle: &tauri::AppHandle<tauri::test::MockRuntime>,
        expected: &InvestigationProvider,
        app_data: &Path,
    ) {
        let auth = Authorization { step_cap: 1 };
        let enabled = t3(handle).enabled;
        let dropped = app_data.join("dropped.jsonl");
        drop(Consent::grant(handle, expected, &auth, &dropped).unwrap());
        assert_cleaned_up(handle, &dropped, enabled);

        let panicked = app_data.join("panicked.jsonl");
        let consent = Consent::grant(handle, expected, &auth, &panicked).unwrap();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            consent.approve_while("no-such-investigation", || panic!("injected run panic"))
        }));
        assert!(unwound.is_err());
        assert_cleaned_up(handle, &panicked, enabled);

        let settings = |sql: &str| {
            let state = handle.state::<crate::AppState>();
            state
                .settings
                .lock()
                .unwrap()
                .execute_batch_for_test(sql)
                .unwrap();
        };
        // The enabled step only writes when T3 starts disabled.
        handle
            .state::<crate::AppState>()
            .settings
            .lock()
            .unwrap()
            .set_enabled(TIER, false)
            .unwrap();
        for (step, when) in [
            ("enabled", "NEW.enabled <> OLD.enabled"),
            ("provider", "NEW.provider = 'cloud'"),
            ("consent", "NEW.consented = 1"),
        ] {
            settings(&format!(
                "CREATE TEMP TRIGGER fail_grant BEFORE UPDATE ON tier_settings \
                 WHEN NEW.tier = '{TIER}' AND {when} \
                 BEGIN SELECT RAISE(ABORT, 'injected'); END;"
            ));
            let log = app_data.join(format!("failed-{step}.jsonl"));
            let (sent, received) = std::sync::mpsc::channel();
            let (thread_handle, thread_expected, thread_log) =
                (handle.clone(), expected.clone(), log.clone());
            std::thread::spawn(move || {
                let granted = Consent::grant(
                    &thread_handle,
                    &thread_expected,
                    &Authorization { step_cap: 1 },
                    &thread_log,
                );
                let _ = sent.send(granted.is_err());
            });
            assert!(
                received
                    .recv_timeout(Duration::from_secs(30))
                    .unwrap_or_else(|_| panic!("grant cleanup hung at {step}")),
                "{step}"
            );
            settings("DROP TRIGGER fail_grant;");
            assert_cleaned_up(handle, &log, false);
        }
    }
}
