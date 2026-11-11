//! Scripted mechanics only, NOT model-negative-case or frontier evidence.
use super::super::super::vendure::negative_cases::tests::Fixture;
use super::super::vendure_prepare::Selection;
use super::*;
use llm::{Embedding, LlmProvider, Locality, ProviderCaps, ProviderError, bounded::*};
use std::sync::atomic::{AtomicUsize, Ordering};
struct ScriptedFailure {
    inner: llm::openai::OpenAiProvider,
    calls: AtomicUsize,
}
impl ScriptedFailure {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: llm::openai::OpenAiProvider::new("gpt-6.1-sol", "synthetic-never-used").unwrap(),
            calls: AtomicUsize::new(0),
        })
    }
}
impl LlmProvider for ScriptedFailure {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn completion_model_id(&self) -> String {
        self.inner.completion_model_id()
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("no embedding")
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.inner.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        assert!(request.payload().system.contains("analyst"));
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(BoundedCallError {
            code: FailureCode::Transport,
            outcome: InvocationOutcome::Unknown,
        })
    }
}
fn definition(case: NegativeCase598, prepared: &Value, provider: &dyn LlmProvider) -> Definition {
    let mut selections = Vec::new();
    for file in case.spec().unwrap().inputs {
        for candidate in prepared["graph"]["candidates"].as_array().unwrap() {
            let receipt: adapters_lang_ts::captured::Receipt =
                serde_json::from_value(candidate["receipt"].clone()).unwrap();
            if let Some(ordinal) = receipt.ranges().iter().position(|r| {
                r.captured.file.path == file.path
                    && r.captured.byte_start <= file.evidence_byte_range[0]
                    && r.captured.byte_end >= file.evidence_byte_range[1]
            }) {
                selections.push(Selection {
                    fact: serde_json::from_value(candidate["binding"]["fact"].clone()).unwrap(),
                    graph_snapshot_id: prepared["graph"]["graph_snapshot_id"]
                        .as_str()
                        .unwrap()
                        .into(),
                    receipt_id: candidate["binding"]["receipt_id"].as_str().unwrap().into(),
                    ordinal,
                });
                break;
            }
        }
    }
    assert_eq!(selections.len(), case.spec().unwrap().inputs.len());
    Definition {
        binding: super::super::super::completion::prepare::PreparationBinding {
            run_id: format!("non-evidence-negative-{}", case.id()),
            protocol_hash: "a".repeat(64),
            source_commit: "a".repeat(40),
            source_tree: "b".repeat(40),
        },
        start_sha256: None,
        preparation_sha256: sha(&serde_json::to_vec(prepared).unwrap()),
        provider: crate::investigations::runtime::descriptor(
            provider,
            InvestigationProviderMode::Cloud,
        )
        .unwrap(),
        selections,
        question: case.spec().unwrap().question,
        request_nonce: format!("non-evidence-{}", case.id()),
        scope: InvestigationScope::All,
        specialist_id: Some("domain-analyst@27".into()),
        run_limits: None,
        generation_policy: None,
    }
}
// AC-0589: closed fixture transport only, never semantic acceptance evidence.
#[test]
fn negative_route_scripted_cases_prepare_execute_once_and_reopen_non_evidence() {
    for id in [
        "ambiguity",
        "missing_input",
        "conflicting_evidence",
        "stale_evidence",
        "injection",
    ] {
        let case = NegativeCase598::parse(id).unwrap();
        let fixture = Fixture::new(case);
        let prepared = fixture.prepare(case).unwrap();
        let provider = ScriptedFailure::new();
        let mut expected = definition(case, &prepared, provider.as_ref());
        let paths = [
            fixture.config.clone(),
            fixture.base.join("preparation.json"),
            fixture.base.join("definition.json"),
            fixture.base.join("output"),
        ];
        std::fs::write(&paths[1], serde_json::to_vec(&prepared).unwrap()).unwrap();
        std::fs::write(&paths[2], serde_json::to_vec(&expected).unwrap()).unwrap();
        let paths = paths.each_ref().map(|p| p.as_path());
        let mut wrong = expected.clone();
        wrong.question.push('!');
        assert!(prepare(case, paths, &wrong).is_err());
        assert!(!paths[3].exists());
        for mutation in 0..4 {
            let mut wrong = expected.clone();
            match mutation {
                0 => wrong.specialist_id = Some("domain-analyst@26".into()),
                1 => {
                    wrong.scope = InvestigationScope::Node {
                        id: "foreign".into(),
                    }
                }
                2 => wrong.provider.model = "different".into(),
                _ => {
                    wrong.run_limits = Some(super::super::vendure_prepare::RunLimits {
                        active_seconds: 600,
                        request_timeout_ms: 180000,
                    })
                }
            }
            assert!(prepare(case, paths, &wrong).is_err());
            assert!(!paths[3].exists());
        }
        let mut wrong = expected.clone();
        wrong.selections[0].ordinal = usize::MAX;
        std::fs::write(paths[2], serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(prepare(case, paths, &wrong).is_err());
        assert!(!paths[3].exists());
        std::fs::write(paths[2], serde_json::to_vec(&expected).unwrap()).unwrap();
        prepare(case, paths, &expected).unwrap();
        expected.start_sha256 = Some(sha(&std::fs::read(paths[3].join("start.json")).unwrap()));
        assert!(
            run(case, paths, &expected, true, &|_| None, || panic!(
                "gate precedes provider"
            ))
            .is_err()
        );
        assert!(!paths[3].join("analyst-attempt.json").exists());
        let env = |name: &str| match name {
            cloud_run::AUTHORIZE => Some("openai:gpt-6.1-sol".into()),
            cloud_run::STEP_CAP => Some("32".into()),
            _ => panic!("scripted run must not read credentials"),
        };
        let receipt = run(case, paths, &expected, true, &env, || Ok(provider.clone())).unwrap();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "terminal={}",
            receipt["analyst"]["detail"]["error"]
        );
        assert_eq!(receipt["fixture"], case.binding().unwrap());
        let detail = &receipt["analyst"]["detail"];
        assert_eq!(detail["parent_id"], Value::Null);
        assert_eq!(detail["limits"]["generated_tokens_per_invocation"], 16384);
        assert_eq!(detail["limits"]["generated_token_reservations"], 1048576);
        assert_eq!(detail["limits"]["active_seconds"], 600);
        assert_eq!(detail["usage"]["model_invocations"], 1);
        assert_eq!(receipt["analyst"]["result"], Value::Null);
        let consent = std::fs::read_to_string(paths[3].join("analyst-consent-log.jsonl")).unwrap();
        assert!(consent.contains("step_approved") && consent.contains("tier_reverted_local"));
        assert!(paths[3].join("analyst-attempt.json").is_file());
        let again = run(
            case,
            paths,
            &expected,
            false,
            &|_| panic!("reopen reads no env"),
            || panic!("reopen dispatch"),
        )
        .unwrap();
        assert_eq!(receipt, again);
        assert!(
            run(case, paths, &expected, true, &env, || panic!(
                "retry provider"
            ))
            .is_err()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
}
