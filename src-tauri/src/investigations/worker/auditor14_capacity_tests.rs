//! AC-0596: forced real capacity failure with a synthetic captured source.
//! No model, acceptance evidence, limit override or private corpus is used.
use super::*;
struct CapacityProvider {
    base: NextProvider,
    prompts: std::sync::Mutex<Vec<Value>>,
}
impl LlmProvider for CapacityProvider {
    fn id(&self) -> &str {
        self.base.id()
    }
    fn locality(&self) -> Locality {
        self.base.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.base.capabilities()
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        panic!("scripted")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.base.bounded_profile()
    }
    fn complete_bounded(
        &self,
        r: &AuthorizedBoundedCompletion,
        c: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((c.check)(), CallDirective::Continue);
        let prompt: Value = serde_json::from_str(&r.payload().prompt).unwrap();
        let mut prompts = self.prompts.lock().unwrap();
        assert!(
            prompts.len() < 32,
            "capacity exclusion must not repeat until the call cap"
        );
        let action=prompt["turn_progress"]["acquisition_choices"]["entries"].as_array().and_then(|a|a.first()).map(|e|e["action"].clone()).unwrap_or_else(||json!({"schema_version":1,"outcome":"insufficient_evidence","action":{"type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence","limitations":["A mandatory source could not be acquired within capacity; its claims remain unresolved."]}}));
        prompts.push(prompt);
        Ok(BoundedCompletion {
            text: action.to_string(),
            requested_model: "scripted-model".into(),
            response_model: Some("scripted-model-observed".into()),
            provider_request_id: None,
            usage: None,
            request_bytes: 100,
            response_bytes: 100,
        })
    }
}
#[test]
fn auditor14_unaffordable_source_is_excluded_and_finishes_honestly() {
    let provider = Arc::new(CapacityProvider {
        base: NextProvider::new(false),
        prompts: std::sync::Mutex::new(vec![]),
    });
    let original=(0..7).map(|i|format!("export function enabled{i}(ok: boolean) {{ const limited = ok{}=== false; if (limited) return false; }}\n", " ".repeat(7500))).collect::<String>();
    let base = CompletionFixture::with_original(
        provider.as_ref(),
        SpecialistId::DomainAnalystV9,
        false,
        true,
        &original,
    );
    let mut request = base.request.clone();
    {
        let state = base.base.app.state::<AppState>();
        let nodes = state
            .graph
            .lock()
            .unwrap()
            .nodes_with_label("BusinessRule")
            .unwrap();
        assert_eq!(nodes.len(), 7);
        for node in nodes {
            let fact = agents::TaskFactKey::Node { id: node.id };
            let description =
                crate::primary_source::describe(&state, &crate::task_evidence::graph_fact(&fact))
                    .unwrap();
            let target = description
                .ranges
                .iter()
                .filter_map(|range| {
                    crate::investigations::inspection::resolve(
                        &state,
                        &fact,
                        &description.graph_snapshot_id,
                        &description.receipt_id,
                        range.index,
                    )
                    .ok()
                    .map(|(target, _)| target)
                })
                .find(|target| target.role == agents::TaskRangeRole::DefinitionInitializer)
                .expect("valid bounded captured initializer");
            assert!(target.source.byte_end - target.source.byte_start <= 8192);
            request.expected_graph_revision = Some(target.graph_snapshot_id.clone());
            let mut obligation = agents::investigation::Obligation::Source {
                obligation_id: String::new(),
                target: Box::new(target),
            };
            let hash = obligation.content_id().unwrap();
            if let agents::investigation::Obligation::Source { obligation_id, .. } = &mut obligation
            {
                *obligation_id = hash;
            }
            request.obligations.obligations.push(obligation);
        }
        request.obligations = request
            .obligations
            .normalized_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
    }
    let f = NextFixture::from_completion_base(
        provider.as_ref(),
        SpecialistId::EvidenceAuditorV14,
        base,
        request,
    );
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    let events = jobs.investigation_events(&f.id, 0).unwrap();
    let refused = events
        .items
        .iter()
        .filter(|e| e.kind == InvestigationEventKind::CapacityRefused)
        .count();
    assert!(refused > 0, "{:?}", detail.error);
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(detail.terminal_diagnostic, Some(agents::investigation::InvestigationTerminalDiagnostic::CapacityCompletion { reason: agents::investigation::CapacityCompletionReason::UnmetObligationsAfterCapacityExclusion }));
    assert!(detail.usage.evidence_items < 7);
    assert!(detail.usage.model_invocations < 32);
    let prompts = provider.prompts.lock().unwrap();
    let refusal = prompts
        .iter()
        .position(|p| p["turn_progress"]["capacity_feedback"]["reason"] == "prompt_capacity")
        .expect("closed capacity feedback");
    let before = &prompts[refusal - 1];
    assert_eq!(
        before["turn_progress"]["acquisition_choices"]["entries"][0]["action"]["type"],
        "read_evidence"
    );
    let after = &prompts[refusal];
    assert_eq!(after["compact_context"], before["compact_context"]);
    assert_eq!(after["citations"], before["citations"]);
    let refused_action = &before["turn_progress"]["acquisition_choices"]["entries"][0]["action"];
    assert!(
        !after["turn_progress"]["acquisition_choices"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| &e["action"] == refused_action)
    );
    assert!(
        prompts.last().unwrap()["turn_progress"]["acquisition_choices"]["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap(),
        detail
    );
}
