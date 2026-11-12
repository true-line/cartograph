//! AC-0591: synthetic providers only; these are not hosted or semantic evidence.
use super::*;
use llm::bounded::*;
use llm::{Embedding, Locality, ProviderCaps, ProviderError};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
pub(super) enum Rejection {
    Once,
    Repeated,
    ShortTitle,
    Sensitive,
}
struct RejectFinish {
    inner: Arc<dyn LlmProvider>,
    text: String,
    rejection: Rejection,
    finishes: AtomicUsize,
}
impl LlmProvider for RejectFinish {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("scripted only")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
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
        let mut value: Value = serde_json::from_str(&response.text).unwrap();
        if value["action"]["type"] == "finish" {
            let finish = self.finishes.fetch_add(1, Ordering::SeqCst);
            if finish == 0 || matches!(self.rejection, Rejection::Repeated | Rejection::Sensitive) {
                value["action"]["limitations"] = json!([self.text]);
                response.text = value.to_string();
            }
        }
        response.usage = Some(ReportedUsage {
            input_tokens: Some(17),
            output_tokens: Some(3),
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        });
        Ok(response)
    }
}
pub(super) fn provider(
    inner: Arc<dyn LlmProvider>,
    parent: &InvestigationResult,
    rejection: Rejection,
) -> Arc<dyn LlmProvider> {
    let text = match rejection {
        Rejection::ShortTitle => {
            let title = parent
                .findings
                .iter()
                .map(|f| &f.title)
                .find(|t| t.chars().filter(|c| !c.is_whitespace()).count() < 48)
                .expect("short saved parent title");
            title.clone()
        }
        Rejection::Sensitive => "password=syntheticsecret123".into(),
        _ => {
            let text: String = parent.findings[0].statement.chars().take(160).collect();
            assert!(text.chars().filter(|c| !c.is_whitespace()).count() >= 48);
            text
        }
    };
    Arc::new(RejectFinish {
        inner,
        text,
        rejection,
        finishes: AtomicUsize::new(0),
    })
}
pub(super) fn verify(
    actual: &Value,
    provider: &execution::ObservedProvider,
    role: SpecialistId,
    rejection: Rejection,
) {
    let corrected = matches!(
        role,
        SpecialistId::EvidenceAuditorV13 | SpecialistId::EvidenceAuditorV14
    ) && matches!(rejection, Rejection::Once | Rejection::ShortTitle);
    assert_eq!(
        actual["detail"]["status"],
        if corrected { "completed" } else { "failed" },
        "{}",
        actual["detail"]["error"]
    );
    assert_eq!(actual["detail"]["usage"]["tool_actions"], 3);
    let observations = provider.observations().unwrap();
    assert_eq!(
        observations.len(),
        if matches!(
            role,
            SpecialistId::EvidenceAuditorV13 | SpecialistId::EvidenceAuditorV14
        ) && !matches!(rejection, Rejection::Sensitive)
        {
            5
        } else {
            4
        }
    );
    assert_eq!(
        actual["detail"]["usage"]["reported_output_tokens"],
        observations.len() as u64 * 3
    );
    assert_eq!(
        actual["detail"]["usage"]["reported_input_tokens"],
        observations.len() as u64 * 17
    );
    let last = serde_json::to_value(observations.last().unwrap()).unwrap();
    assert_eq!(
        last["response"]["schema"],
        "cartograph-response-observation@1"
    );
    assert_eq!(last["response"]["response_class"], "finish");
    assert_eq!(last["response"]["reported_output_tokens"], 3);
    assert!(
        last["response"]["response_hash"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    if corrected {
        assert!(actual["detail"]["terminal_diagnostic"].is_null());
        assert_eq!(actual["finish_refusal_count"], 1);
    } else {
        assert!(actual["result"].is_null());
        assert_eq!(
            actual["detail"]["terminal_diagnostic"]["schema"],
            "investigation-terminal-diagnostic@3"
        );
        assert_eq!(
            actual["detail"]["terminal_diagnostic"]["reason"],
            if matches!(rejection, Rejection::Sensitive) {
                "sensitive_output"
            } else {
                "source_replay"
            }
        );
    }
}
#[test]
fn auditor12_rejected_finish_preserves_reason_usage_and_reopens() {
    for rejection in [Rejection::Once, Rejection::Sensitive] {
        exercise_auditor_configured(
            Scenario::CloudComplete,
            SpecialistId::EvidenceAuditorV12,
            Some(rejection),
        );
    }
}
#[test]
fn auditor13_replay_correction_succeeds_or_exhausts_and_reopens() {
    for rejection in [
        Rejection::Once,
        Rejection::Repeated,
        Rejection::ShortTitle,
        Rejection::Sensitive,
    ] {
        exercise_auditor_configured(
            Scenario::CloudComplete,
            SpecialistId::EvidenceAuditorV13,
            Some(rejection),
        );
    }
}
#[test]
fn auditor13_withheld_parent_cloud_shaped_receipt_reopens() {
    exercise_auditor_role(Scenario::WithheldCloud, SpecialistId::EvidenceAuditorV13);
}

#[test]
fn auditor14_replay_correction_and_withheld_parent_receipt_reopen() {
    for rejection in [Rejection::Once, Rejection::Repeated, Rejection::ShortTitle] {
        exercise_auditor_configured(
            Scenario::CloudComplete,
            SpecialistId::EvidenceAuditorV14,
            Some(rejection),
        );
    }
    exercise_auditor_role(Scenario::WithheldCloud, SpecialistId::EvidenceAuditorV14);
}
