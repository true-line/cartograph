use super::super::{
    prepare::{self, PreparationBinding},
    records::{ControlClassification, SemanticGate, evaluate},
};
use super::*;
use crate::investigations::worker::tests::{Mode, Scripted};
use std::sync::atomic::Ordering;
fn prepared(directory: &Path, provider: &dyn LlmProvider) -> PreparedState {
    prepared_family(directory, provider, prepare::Family::V7)
}
fn prepared_family(
    directory: &Path,
    provider: &dyn LlmProvider,
    family: prepare::Family,
) -> PreparedState {
    prepare::prepare_family(
        directory,
        &descriptor(provider, InvestigationProviderMode::Local).unwrap(),
        PreparationBinding {
            run_id: "execution-test-v7".into(),
            protocol_hash: "a".repeat(64),
            source_commit: "b".repeat(40),
            source_tree: "c".repeat(40),
        },
        family,
    )
    .unwrap()
}
fn tasks(prepared: &PreparedState) -> i64 {
    rusqlite::Connection::open(prepared.app_data.join("state.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM investigation_tasks", [], |row| {
            row.get(0)
        })
        .unwrap()
}

// AC-0267: the actual worker chooses its reads and finishes. First-shot and
// one-refusal supported controls remain separate; neither starts an auditor.
#[test]
fn completion_execution_supported_categories_use_actual_worker_and_never_auto_auditor() {
    for (mode, classification, refusals, calls) in [
        (
            Mode::CompletionSupported,
            ControlClassification::SupportedFirstShot,
            0,
            4,
        ),
        (
            Mode::CompletionRepair,
            ControlClassification::SupportedAfterOneRefusal,
            1,
            5,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(Scripted::new(mode));
        let prepared = prepared(directory.path(), provider.as_ref());
        let evidence = run_role(&prepared, Role::Analyst, provider.clone()).unwrap();
        assert_eq!(evidence.receipt.classification, classification);
        assert_eq!(evidence.receipt.durable_refusal_count, refusals);
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
        assert_eq!(tasks(&prepared), 1);
        assert!(prepared.auditor_id.is_none());
        assert!(
            evidence
                .mechanical_checks
                .iter()
                .all(|check| check.status == CheckStatus::Passed),
            "{:?}",
            evidence.mechanical_checks
        );
        assert!(evidence.representation_reviewable);
        assert!(
            !evaluate(&evidence.receipt, SemanticGate::Pending)
                .unwrap()
                .control_pass
        );
        assert!(run_role(&prepared, Role::Analyst, provider.clone()).is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
    }
}
struct InsufficientAfterReads {
    inner: Scripted,
    malformed: bool,
}
impl LlmProvider for InsufficientAfterReads {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.inner.embed(texts)
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.inner.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let mut response = self.inner.complete_bounded(request, control)?;
        let action: Value = serde_json::from_str(&response.text).unwrap();
        if action["outcome"] == "supported" && self.malformed {
            response.text = "not an admissible action".into();
        } else if action["outcome"] == "supported" {
            response.text=serde_json::json!({"schema_version":1,"outcome":"insufficient_evidence","action":{"type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence","limitations":["The completed bounded inspections do not establish the requested behavior."]}}).to_string();
        }
        Ok(response)
    }
}
// AC-0267: completed mixed attempts may honestly admit empty insufficient
// findings; this is retained as a distinct nonpassing control, never a rerun.
#[test]
fn completion_execution_honest_insufficient_is_recorded_without_control_pass() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Arc::new(InsufficientAfterReads {
        inner: Scripted::new(Mode::CompletionSupported),
        malformed: false,
    });
    let prepared = prepared(directory.path(), provider.as_ref());
    let evidence = run_role(&prepared, Role::Analyst, provider.clone()).unwrap();
    assert_eq!(
        evidence.receipt.classification,
        ControlClassification::Insufficient
    );
    assert_eq!(evidence.receipt.refusals_before_finish, Some(0));
    assert!(evidence.result.as_ref().unwrap().findings.is_empty());
    assert!(
        !evaluate(&evidence.receipt, SemanticGate::Passed)
            .unwrap()
            .control_pass
    );
    assert_eq!(provider.inner.calls.load(Ordering::SeqCst), 4);
    assert_eq!(tasks(&prepared), 1);
}
// AC-0267: actual capped/unknown worker outcomes return immutable runtime
// evidence without a synthetic finish, hidden retry, result or auditor dispatch.
#[test]
fn completion_execution_caps_and_unknown_are_honest_durable_outcomes() {
    for (mode, classification) in [
        (Mode::CompletionCap, ControlClassification::ObligationsUnmet),
        (Mode::Unknown, ControlClassification::Unknown),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(Scripted::new(mode));
        let prepared = prepared(directory.path(), provider.as_ref());
        let evidence = run_role(&prepared, Role::Analyst, provider.clone()).unwrap();
        assert_eq!(evidence.receipt.classification, classification);
        assert!(evidence.result.is_none());
        assert_eq!(evidence.receipt.refusals_before_finish, None);
        assert!(
            !evaluate(&evidence.receipt, SemanticGate::Passed)
                .unwrap()
                .control_pass
        );
        let calls = provider.calls.load(Ordering::SeqCst);
        assert_eq!(calls, if matches!(mode, Mode::Unknown) { 1 } else { 3 });
        assert!(run_role(&prepared, Role::Analyst, provider.clone()).is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
        assert_eq!(tasks(&prepared), 1);
    }
}
// AC-0267: analyst and auditor are separate process-stage invocations. Fresh
// preparation reopens the exact saved parent; an already-started role is refused.
#[test]
fn completion_execution_reopens_stage_and_auditor_independently_inspects_parent() {
    let directory = tempfile::tempdir().unwrap();
    let analyst = Arc::new(Scripted::new(Mode::CompletionSupported));
    let initial = prepared(directory.path(), analyst.as_ref());
    assert!(run_role(&initial, Role::Auditor, analyst.clone()).is_err());
    assert_eq!(analyst.calls.load(Ordering::SeqCst), 0);
    let evidence = run_role(&initial, Role::Analyst, analyst).unwrap();
    assert_eq!(tasks(&initial), 1);
    let parent = evidence.result.unwrap().result_id;
    drop(initial);
    let reopened = prepare::reopen(directory.path()).unwrap();
    assert!(reopened.auditor_id.is_none());
    let auditor = Arc::new(IndependentAuditor {
        inner: Scripted::new(Mode::CompletionSupported),
    });
    let evidence = run_role(&reopened, Role::Auditor, auditor.clone()).unwrap();
    assert_eq!(
        evidence.detail.summary.parent_id.as_ref(),
        Some(&reopened.analyst_id)
    );
    assert_eq!(tasks(&reopened), 2);
    assert!(
        evidence
            .mechanical_checks
            .iter()
            .all(|check| check.status == CheckStatus::Passed),
        "checks={:?} detail={:?} calls={}",
        evidence.mechanical_checks,
        evidence.detail,
        auditor.inner.calls.load(Ordering::SeqCst)
    );
    assert!(
        evidence
            .observations
            .iter()
            .all(|observation| observation.parent_result_hashes.contains_key(&parent))
    );
    assert_eq!(auditor.inner.calls.load(Ordering::SeqCst), 4);
    drop(reopened);
    let reopened = prepare::reopen(directory.path()).unwrap();
    assert!(reopened.auditor_id.is_some());
    assert!(run_role(&reopened, Role::Auditor, auditor.clone()).is_err());
    assert_eq!(auditor.inner.calls.load(Ordering::SeqCst), 4);
}

struct RepeatedSourceRead {
    inner: Scripted,
}
impl LlmProvider for RepeatedSourceRead {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.inner.embed(texts)
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.inner.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let mut response = self.inner.complete_bounded(request, control)?;
        let call = self.inner.calls.load(Ordering::SeqCst) - 1;
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        let obligations = prompt["completion_contract"]["obligations"]["obligations"]
            .as_array()
            .unwrap();
        if call == 2 {
            let target = &obligations
                .iter()
                .find(|item| item["kind"] == "source")
                .unwrap()["target"];
            response.text=serde_json::json!({"type":"read_evidence","fact":target["fact"],"role":target["role"],"index":target["index"]}).to_string();
        } else if call == 3 {
            let target = &obligations
                .iter()
                .find(|item| item["kind"] == "inventory")
                .unwrap()["target"];
            response.text=serde_json::json!({"type":"read_coverage_inventory","context_id":target["context_id"],"repo":target["repo"],"inventory_hash":target["inventory_hash"],"max_bytes":32768}).to_string();
        } else if call == 4 {
            let mut finish: Value = serde_json::from_str(&response.text).unwrap();
            let citation = prompt["citations"]
                .as_array()
                .unwrap()
                .iter()
                .rev()
                .find(|citation| citation["origin"]["kind"] == "captured_primary_source")
                .unwrap();
            finish["action"]["findings"][0]["citation_ids"] =
                serde_json::json!([citation["citation_id"]]);
            response.text = finish.to_string();
        }
        Ok(response)
    }
}
// AC-0267: repeated exact source reads have distinct citation IDs. Mechanical
// success follows any matching supplied/cited occurrence, including the second.
#[test]
fn completion_execution_repeated_source_reads_accept_later_cited_occurrence() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Arc::new(RepeatedSourceRead {
        inner: Scripted::new(Mode::CompletionSupported),
    });
    let prepared = prepared(directory.path(), provider.as_ref());
    let evidence = run_role(&prepared, Role::Analyst, provider.clone()).unwrap();
    assert_eq!(
        evidence.receipt.classification,
        ControlClassification::SupportedFirstShot
    );
    assert!(
        evidence
            .mechanical_checks
            .iter()
            .all(|check| check.status == CheckStatus::Passed),
        "{:?}",
        evidence.mechanical_checks
    );
    let ledger = evidence.ledger.unwrap();
    let source = prepared
        .analyst_request
        .obligations
        .sources()
        .unwrap()
        .remove(0);
    let matching: Vec<_> = ledger
        .citations
        .iter()
        .filter(|citation| source.matches(citation))
        .collect();
    assert_eq!(matching.len(), 2);
    let result = evidence.result.unwrap();
    assert!(
        !result.findings[0]
            .citation_ids
            .contains(&matching[0].citation_id)
    );
    assert!(
        result.findings[0]
            .citation_ids
            .contains(&matching[1].citation_id)
    );
    assert_eq!(provider.inner.calls.load(Ordering::SeqCst), 5);
}

struct IndependentAuditor {
    inner: Scripted,
}
impl LlmProvider for IndependentAuditor {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.inner.embed(texts)
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.inner.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let mut response = self.inner.complete_bounded(request, control)?;
        let mut finish: Value = serde_json::from_str(&response.text).unwrap();
        if finish["outcome"] == "supported" {
            finish["action"]["findings"][0]["title"] = "Independent bounded comparison".into();
            finish["action"]["findings"][0]["statement"]="The inspected initializer compares an input to a boolean literal; this observation alone leaves surrounding control flow undecided.".into();
            finish["action"]["findings"][0]["limitations"] = serde_json::json!([
                "No surrounding function semantics are established by this single occurrence."
            ]);
            finish["action"]["limitations"] = serde_json::json!([
                "Only one selected captured occurrence and one inventory were examined."
            ]);
            response.text = finish.to_string();
        }
        Ok(response)
    }
}

pub(super) struct RecordingFailureProvider {
    pub(super) inner: Scripted,
    pub(super) database: std::path::PathBuf,
    pub(super) id: String,
}
impl LlmProvider for RecordingFailureProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.inner.embed(texts)
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.inner.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let response = self.inner.complete_bounded(request, control)?;
        if serde_json::from_str::<Value>(&response.text).unwrap()["outcome"] == "supported" {
            let connection = rusqlite::Connection::open(&self.database).unwrap();
            let body: Vec<u8> = connection
                .query_row(
                    "SELECT record FROM investigation_tasks WHERE id=?1",
                    [&self.id],
                    |row| row.get(0),
                )
                .unwrap();
            // Preserve the production struct's canonical ordering after this
            // explicit test-only corruption by patching the original error token.
            let original = String::from_utf8(body).unwrap();
            let replacement =
                serde_json::to_string("synthetic recording defect\nmultiline stop cause").unwrap();
            let canonical = original
                .replacen("\"error\":null", &format!("\"error\":{replacement}"), 1)
                .into_bytes();
            connection
                .execute(
                    "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
                    rusqlite::params![canonical, content_hash(&canonical), self.id],
                )
                .unwrap();
        }
        Ok(response)
    }
}
// AC-0267: a post-worker record validation failure still returns the captured
// actual invocation hashes; it never turns that run into a resumable attempt.
#[test]
fn completion_execution_recording_error_preserves_actual_observation_hashes() {
    let directory = tempfile::tempdir().unwrap();
    let baseline = Scripted::new(Mode::CompletionSupported);
    let prepared = prepared(directory.path(), &baseline);
    let provider = Arc::new(RecordingFailureProvider {
        inner: Scripted::new(Mode::CompletionSupported),
        database: prepared.app_data.join("state.db"),
        id: prepared.analyst_id.clone(),
    });
    let error = run_role(&prepared, Role::Analyst, provider.clone()).unwrap_err();
    assert!(error.post_dispatch);
    assert_eq!(error.reason, "Invalid acceptance record");
    assert_eq!(error.observations.len(), 4);
    assert!(
        error
            .observations
            .iter()
            .all(|observed| !observed.payload_hash.is_empty())
    );
    assert_eq!(provider.inner.calls.load(Ordering::SeqCst), 4);
    assert!(run_role(&prepared, Role::Analyst, provider.clone()).is_err());
    assert_eq!(provider.inner.calls.load(Ordering::SeqCst), 4);
}

// AC-0267: a final unadmitted call has unknown totals, never a passing failed run.
#[test]
fn completion_execution_trailing_unadmitted_usage_is_unknown_not_success() {
    let directory = tempfile::tempdir().unwrap();
    let provider = Arc::new(InsufficientAfterReads {
        inner: Scripted::new(Mode::CompletionSupported),
        malformed: true,
    });
    let prepared = prepared(directory.path(), provider.as_ref());
    let evidence = run_role(&prepared, Role::Analyst, provider.clone()).unwrap();
    assert_eq!(
        evidence.receipt.classification,
        ControlClassification::Failed
    );
    assert!(evidence.result.is_none());
    assert_eq!(provider.inner.calls.load(Ordering::SeqCst), 4);
    assert_eq!(evidence.detail.usage.reported_input_tokens, None);
    assert_eq!(evidence.detail.usage.reported_output_tokens, None);
    assert_eq!(
        evidence
            .mechanical_checks
            .iter()
            .find(|check| check.name == "known_response_usage_charged_once")
            .unwrap()
            .status,
        CheckStatus::Unknown
    );
    assert!(
        !evaluate(&evidence.receipt, SemanticGate::Passed)
            .unwrap()
            .control_pass
    );
    assert!(run_role(&prepared, Role::Auditor, provider.clone()).is_err());
    assert_eq!(provider.inner.calls.load(Ordering::SeqCst), 4);
    let database = prepared.app_data.join("state.db");
    let steps = rows::<PendingInvestigationStep>(
        &database,
        &prepared.analyst_id,
        "investigation_invocations",
        8,
        256 * 1024,
    )
    .unwrap();
    let responses = rows::<InvestigationResponseMeta>(
        &database,
        &prepared.analyst_id,
        "investigation_responses",
        8,
        4096,
    )
    .unwrap();
    assert_eq!(responses.len(), 3);
    assert_eq!(evidence.detail.usage.reported_cache_read_tokens, None);
    let mut detail = evidence.detail.clone();
    detail.usage.reported_cache_read_tokens = Some(0);
    assert_eq!(
        response_usage_status(&steps, &responses, &detail, &evidence.events),
        CheckStatus::Failed
    );
    let mut complete = responses.clone();
    let mut tail = complete[0].clone();
    tail.step_id = steps.last().unwrap().step_id.clone();
    complete.push(tail);
    for response in &mut complete {
        response.reported_input_tokens = Some(2);
        response.reported_output_tokens = Some(3);
        response.reported_cache_read_tokens = Some(5);
    }
    detail.usage.reported_input_tokens = Some(8);
    detail.usage.reported_output_tokens = Some(12);
    detail.usage.reported_cache_read_tokens = Some(20);
    assert_eq!(
        response_usage_status(&steps, &complete, &detail, &evidence.events),
        CheckStatus::Passed
    );
    detail.usage.reported_cache_read_tokens = Some(19);
    assert_eq!(
        response_usage_status(&steps, &complete, &detail, &evidence.events),
        CheckStatus::Failed
    );
    complete[0].reported_cache_read_tokens = Some(u64::MAX);
    assert_eq!(
        response_usage_status(&steps, &complete, &detail, &evidence.events),
        CheckStatus::Failed
    );
    for kind in 0..8 {
        let mut changed = responses.clone();
        let mut events = evidence.events.clone();
        match kind {
            0 => {
                changed.remove(0);
            }
            1 => changed.push(changed[0].clone()),
            2 => changed[0].step_id = "foreign".into(),
            3 => {
                changed[0].reported_input_tokens = Some(u64::MAX);
                changed[1].reported_input_tokens = Some(1);
            }
            4 => {
                events.pop();
            }
            5 => events.last_mut().unwrap().investigation_id = "foreign".into(),
            6 => events.last_mut().unwrap().kind = InvestigationEventKind::Completed,
            _ => {
                events
                    .iter_mut()
                    .rev()
                    .find(|event| event.kind == InvestigationEventKind::ModelStarted)
                    .unwrap()
                    .step_id = Some("foreign".into())
            }
        }
        assert_eq!(
            response_usage_status(&steps, &changed, &evidence.detail, &events),
            CheckStatus::Failed,
            "case {kind}"
        );
    }
}

// AC-0430/0267: genuine host and provider limits keep the existing control class.
#[test]
fn completion_execution_new_limit_messages_preserve_exhausted_and_unknown() {
    for (mode, classification) in [
        (Mode::CompletionHostLimit, ControlClassification::Exhausted),
        (Mode::ResponseTooLarge, ControlClassification::Exhausted),
        (
            Mode::UnknownResponseTooLarge,
            ControlClassification::Unknown,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(Scripted::new(mode));
        let prepared = prepared(directory.path(), provider.as_ref());
        let evidence = run_role(&prepared, Role::Analyst, provider.clone()).unwrap();
        assert_eq!(evidence.receipt.classification, classification);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(evidence.detail.usage.model_invocations, 1);
        assert_eq!(
            evidence.detail.usage.tool_actions,
            u32::from(matches!(mode, Mode::CompletionHostLimit))
        );
        assert!(evidence.result.is_none());
        evidence.receipt.validate().unwrap();
    }
}

fn drive_observed(prepared: &PreparedState, provider: Arc<dyn LlmProvider>) {
    let state = prepared.app.state::<crate::AppState>();
    let mut jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&prepared.analyst_id).unwrap();
    let plan = jobs
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let lock = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .unwrap();
    let (_, execution) = jobs.claim_execution(&plan, lock).unwrap();
    drop(jobs);
    let live = state.investigations.insert(&prepared.analyst_id).unwrap();
    run_completion(
        prepared.app.handle().clone(),
        prepared.analyst_id.clone(),
        prepared.analyst_request.clone(),
        execution,
        provider,
        live,
        Instant::now(),
    );
}
// AC-0465: genuine supplied V2 source/inventory bodies bind to durable disclosure6.
#[test]
fn next_acceptance_observer_matches_disclosure_and_rejects_tampering() {
    let directory = tempfile::tempdir().unwrap();
    let provider =
        crate::investigations::worker::tests::completion_tests::expanded_scripted_provider(true);
    let prepared = prepared_family(
        directory.path(),
        provider.as_ref(),
        prepare::Family::Expanded,
    );
    let request = &prepared.analyst_request;
    let observed = Arc::new(ObservedProvider::next(
        provider,
        request.expected_context_id.clone(),
    ));
    drive_observed(&prepared, observed.clone());
    let jobs = JobStore::open(prepared.app_data.join("state.db")).unwrap();
    let (detail, result, _, _) = read_expanded_state(&jobs, &prepared.analyst_id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::Supported)
    );
    assert_eq!(result.unwrap().schema_version, 7);
    let db = prepared.app_data.join("state.db");
    let policy = &INVESTIGATION_V2_POLICY;
    let steps = rows(
        &db,
        &prepared.analyst_id,
        "investigation_invocations",
        policy.limits().model_invocations as usize,
        2 * 1024 * 1024,
    )
    .unwrap();
    let ledgers = rows(
        &db,
        &prepared.analyst_id,
        "investigation_inputs",
        policy.journal_events() as usize,
        2 * 1024 * 1024,
    )
    .unwrap();
    let observations = observed.observations().unwrap();
    assert_eq!(observations.len(), 4);
    let check = |observations: &[_]| {
        sealed_checks_with_policy(request, &steps, &ledgers, observations, policy).unwrap()
    };
    assert_eq!(check(&observations), (true, true, true));
    for field in [
        "source",
        "inventory",
        "payload",
        "context",
        "ledger",
        "history",
    ] {
        let mut tampered = observations.clone();
        let last = tampered.last_mut().unwrap();
        match field {
            "source" => *last.source_hashes.values_mut().next().unwrap() = "f".repeat(64),
            "inventory" => *last.inventory_hashes.values_mut().next().unwrap() = "f".repeat(64),
            "payload" => last.payload_hash = "f".repeat(64),
            "context" => last.context_id = "foreign-context".into(),
            "ledger" => last.input_ledger_hash = "f".repeat(64),
            "history" => last.history_hash = Some("f".repeat(64)),
            _ => unreachable!(),
        }
        let (matching, bound, _) = check(&tampered);
        assert!(!matching || !bound, "{field}");
    }
}
// AC-0465: each schema-family mismatch is refused before the inner provider call.
#[test]
fn acceptance_observer_rejects_other_input_family_before_delegation() {
    for next in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let inner = Arc::new(Scripted::new(Mode::CompletionSupported));
        let family = if next {
            prepare::Family::V7
        } else {
            prepare::Family::Expanded
        };
        let prepared = prepared_family(directory.path(), inner.as_ref(), family);
        let observed = Arc::new(ObservedProvider {
            inner: inner.clone(),
            context_id: prepared.analyst_request.expected_context_id.clone(),
            observations: Mutex::new(vec![]),
            next,
        });
        drive_observed(&prepared, observed.clone());
        let jobs = JobStore::open(prepared.app_data.join("state.db")).unwrap();
        let detail = jobs.investigation(&prepared.analyst_id).unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        assert_eq!(detail.usage.model_invocations, 1);
        assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        assert!(observed.observations().unwrap().is_empty());
    }
}

// AC-0465: inventory details are bounded by tool actions, not completed attempts.
#[test]
fn acceptance_observer_inventory_count_matches_tool_action_bounds() {
    use llm::bounded::{CallDirective, CompletionLimits};
    use llm::{AnalysisTier, CompletionAction, CompletionPayload, EgressFirewall, EgressPolicy};
    for (next, limit) in [(false, 8), (true, 56)] {
        let inner = Arc::new(Scripted::new(Mode::CompletionSupported));
        let observed = ObservedProvider {
            inner: inner.clone(),
            context_id: "inventory-boundary".into(),
            observations: Mutex::new(vec![]),
            next,
        };
        for count in [limit, limit + 1] {
            let details = (0..count).map(|index| {
                let body = format!("{{\"entry\":{index}}}");
                serde_json::json!({"inventory_hash": content_hash(body.as_bytes()), "inventory_json": body})
            }).collect::<Vec<_>>();
            let action = CompletionAction {
                action_id: "inventory-boundary".into(),
                tier: AnalysisTier::Agentic,
                payload: CompletionPayload {
                    system: "Return a JSON action.".into(),
                    prompt: serde_json::json!({
                        "schema_version": if next { 9 } else { 6 },
                        "input_ledger_hash": "a".repeat(64),
                        "context_id": "inventory-boundary",
                        "coverage_details": details,
                        "prior_findings_as_t3_history": []
                    })
                    .to_string(),
                    spans: vec![],
                    response_schema: None,
                },
            };
            let firewall = EgressFirewall::new(EgressPolicy::default());
            let prepared = firewall
                .prepare_bounded(inner.as_ref(), &action, &CompletionLimits::default())
                .unwrap();
            let authorized = firewall
                .authorize_bounded(inner.as_ref(), &prepared, None)
                .unwrap();
            if count == limit {
                assert_eq!(
                    observation(&authorized, next)
                        .unwrap()
                        .inventory_hashes
                        .len(),
                    count
                );
            } else {
                assert_eq!(
                    observation(&authorized, next).unwrap_err(),
                    "Too many actual inventories."
                );
                let control = CompletionControl {
                    deadline: Instant::now() + std::time::Duration::from_secs(10),
                    check: &|| CallDirective::Continue,
                    dispatching: None,
                };
                let error = observed
                    .complete_bounded(&authorized, &control)
                    .unwrap_err();
                assert_eq!(error.code, FailureCode::InvalidInput);
                assert_eq!(error.outcome, InvocationOutcome::NotDispatched);
                assert!(observed.observations().unwrap().is_empty());
            }
            assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
        }
    }
}

// AC-0585: observation adds no provider capability and must not hide one either.
#[test]
fn observed_provider_preserves_bounded_schema_capability() {
    let unsupported: Arc<dyn LlmProvider> =
        Arc::new(Scripted::without_schema(Mode::CompletionNoAttempt));
    assert!(!unsupported.supports_bounded_response_schema());
    let wrapped = ObservedProvider::next(unsupported, "fixture-context".into());
    assert!(!wrapped.supports_bounded_response_schema());
    let supported =
        crate::investigations::worker::tests::completion_tests::full_hosted_auditor_provider(true);
    assert!(supported.supports_bounded_response_schema());
    let wrapped = ObservedProvider::next(supported, "fixture-context".into());
    assert!(wrapped.supports_bounded_response_schema());
    assert!(wrapped.observations().unwrap().is_empty());
}

// AC-0591: untrusted transport observation is content-free and optional for old receipts.
#[test]
fn response_observation_is_text_free_and_historical_wire_stays_readable() {
    let text = r#"{"schema_version":1,"action":{"type":"finish","limitations":["private response sentinel"]}}"#;
    let response = BoundedCompletion {
        text: text.into(),
        requested_model: "scripted".into(),
        response_model: None,
        provider_request_id: None,
        usage: Some(llm::bounded::ReportedUsage {
            input_tokens: None,
            output_tokens: Some(7),
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }),
        request_bytes: 1,
        response_bytes: text.len() as u64,
    };
    let observation = ResponseObservation::from_result(&Ok(response));
    let raw = serde_json::to_string(&observation).unwrap();
    assert!(!raw.contains("private response sentinel"));
    assert_eq!(
        serde_json::from_str::<ResponseObservation>(&raw).unwrap(),
        observation
    );
    let value: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["response_class"], "finish");
    assert_eq!(value["reported_output_tokens"], 7);
    assert_eq!(value["response_hash"], content_hash(text.as_bytes()));
    for (key, value) in [
        ("response_class", "private prose"),
        ("response_text", "private prose"),
    ] {
        let mut changed: Value = serde_json::from_str(&raw).unwrap();
        changed[key] = serde_json::json!(value);
        assert!(serde_json::from_value::<ResponseObservation>(changed).is_err());
    }
    let old = InvocationObservation {
        payload_hash: "p".into(),
        input_ledger_hash: "l".into(),
        context_id: "c".into(),
        source_hashes: BTreeMap::new(),
        inventory_hashes: BTreeMap::new(),
        history_hash: None,
        parent_result_hashes: BTreeMap::new(),
        response: None,
    };
    let wire = serde_json::to_string(&old).unwrap();
    assert!(!wire.contains("response"));
    assert_eq!(
        serde_json::from_str::<InvocationObservation>(&wire).unwrap(),
        old
    );
    assert_eq!(
        serde_json::to_string(&serde_json::from_str::<InvocationObservation>(&wire).unwrap())
            .unwrap(),
        wire
    );
}
