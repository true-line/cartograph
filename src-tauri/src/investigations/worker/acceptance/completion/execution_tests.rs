use super::super::{
    prepare::{self, PreparationBinding},
    records::{ControlClassification, SemanticGate, evaluate},
};
use super::*;
use crate::investigations::worker::tests::{Mode, Scripted};
use std::sync::atomic::Ordering;
fn prepared(directory: &Path, provider: &dyn LlmProvider) -> PreparedState {
    prepare::prepare(
        directory,
        &descriptor(provider, InvestigationProviderMode::Local).unwrap(),
        PreparationBinding {
            run_id: "execution-test-v7".into(),
            protocol_hash: "a".repeat(64),
            source_commit: "b".repeat(40),
            source_tree: "c".repeat(40),
        },
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
        if action["outcome"] == "supported" {
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
