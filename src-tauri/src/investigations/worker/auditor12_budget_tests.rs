//! AC-0585: offline real-worker refusal accounting, cap termination and replay.
use super::*;

struct BudgetProvider {
    base: NextProvider,
    repeat: bool,
    calls: AtomicUsize,
    prompts: std::sync::Mutex<Vec<Value>>,
}
impl LlmProvider for BudgetProvider {
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
        panic!("offline")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.base.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            call < 64,
            "existing invocation cap must stop repeated refusals"
        );
        let p: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        let action = if call == 0 {
            p["turn_progress"]["acquisition_choices"]["entries"][0]["action"].clone()
        } else if self.repeat || call == 1 {
            json!({"type":"query_context","context_id":p["context_id"],"query":{
                "scope":{"type":"all"},"kind":null,"labels":[],"max_facts":1,"max_bytes":1,"cursor":null}})
        } else {
            json!({"schema_version":1,"outcome":"insufficient_evidence","action":{
                "type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence",
                "limitations":["The metadata page did not fit the chosen budget."]}})
        };
        self.prompts.lock().unwrap().push(p);
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
fn auditor12_refusal_recovers_without_credit_and_caps_without_retry() {
    for repeat in [false, true] {
        let provider = Arc::new(BudgetProvider {
            base: NextProvider::new(false),
            repeat,
            calls: AtomicUsize::new(0),
            prompts: std::sync::Mutex::new(vec![]),
        });
        let f = NextFixture::new(provider.as_ref(), SpecialistId::EvidenceAuditorV12);
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&f.id).unwrap();
        let prompts = provider.prompts.lock().unwrap();
        assert!(prompts.len() >= 3, "{:?}", detail.error);
        let before = &prompts[1];
        for after in &prompts[2..] {
            assert_eq!(
                after["turn_progress"]["action_feedback"]["reason"],
                "query_page_budget"
            );
            assert_eq!(after["compact_context"], before["compact_context"]);
            assert_eq!(
                after["completion_metadata"]["completed_attempt_ids"],
                before["completion_metadata"]["completed_attempt_ids"]
            );
            assert_eq!(after["citations"], before["citations"]);
        }
        let events = jobs.investigation_events(&f.id, 0).unwrap();
        let refused = events
            .items
            .iter()
            .filter(|e| e.kind == InvestigationEventKind::QueryRefused)
            .count();
        assert!(refused > 0);
        assert_eq!(detail.usage.evidence_items, 0);
        if repeat {
            assert_eq!(detail.summary.status, InvestigationStatus::Failed);
            assert_eq!(detail.usage.tool_actions, detail.limits.tool_actions);
            assert!(matches!(
                detail.terminal_diagnostic,
                Some(
                    agents::investigation::InvestigationTerminalDiagnostic::QueryBudget {
                        termination: agents::investigation::QueryBudgetTermination::ToolActions,
                        ..
                    }
                )
            ));
        } else {
            assert_eq!(
                detail.summary.status,
                InvestigationStatus::Completed,
                "{:?}",
                detail.error
            );
            assert_eq!(detail.usage.model_invocations, 3);
            assert_eq!(detail.usage.tool_actions, 2);
            assert_eq!(refused, 1);
        }
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
        assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);
    }
}

struct ToolCapProvider {
    base: NextProvider,
    offered: bool,
    calls: AtomicUsize,
}
impl LlmProvider for ToolCapProvider {
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
        panic!("offline")
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.base.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(call < 3);
        let p: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        let action = if call == 0 {
            assert_eq!(p["turn_progress"]["admission_mode"], "choice");
            let offered = p["turn_progress"]["acquisition_choices"]["entries"][0]["action"].clone();
            assert_eq!(offered["type"], "read_coverage_inventory");
            if self.offered {
                offered
            } else {
                // Valid decoded metadata read, but inventory is the only offered choice.
                json!({"type":"query_context","context_id":p["context_id"],"query":{
                    "scope":{"type":"all"},"kind":null,"labels":[],"max_facts":1,"max_bytes":6000,"cursor":null}})
            }
        } else {
            assert!(!self.offered);
            if call == 1 {
                assert_eq!(
                    p["turn_progress"]["action_feedback"]["reason"],
                    "action_not_offered"
                );
            }
            // The tool cap must not prevent a finishing response. This fixture's
            // inventory obligation remains unmet, so normal finish validation applies.
            json!({"schema_version":1,"outcome":"insufficient_evidence","action":{
                "type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence",
                "limitations":["The required inventory was not read."]}})
        };
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
fn auditor12_tool_cap_refuses_unoffered_read_without_cap_diagnostic() {
    for offered in [false, true] {
        let provider = Arc::new(ToolCapProvider {
            base: NextProvider::new(false),
            offered,
            calls: AtomicUsize::new(0),
        });
        let f = NextFixture::new(provider.as_ref(), SpecialistId::EvidenceAuditorV12);
        // Seed the existing tool counter at its boundary before any invocation.
        // Preserve typed field order and its integrity hash; do not alter limits.
        let connection = rusqlite::Connection::open(f.base.base.app_data.join("state.db")).unwrap();
        let bytes: Vec<u8> = connection
            .query_row(
                "SELECT record FROM investigation_tasks WHERE id=?1",
                [&f.id],
                |r| r.get(0),
            )
            .unwrap();
        let task: Value = serde_json::from_slice(&bytes).unwrap();
        let cap = task["detail"]["limits"]["tool_actions"].as_u64().unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let old = serde_json::from_value::<agents::investigation::InvestigationUsage>(
            task["detail"]["usage"].clone(),
        )
        .unwrap();
        let mut usage = old.clone();
        usage.tool_actions = cap as u32;
        let bytes = text
            .replace(
                &serde_json::to_string(&old).unwrap(),
                &serde_json::to_string(&usage).unwrap(),
            )
            .into_bytes();
        connection
            .execute(
                "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
                rusqlite::params![bytes, core_prov::content_hash(&bytes), f.id],
            )
            .unwrap();
        drop(connection);
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&f.id).unwrap();
        assert_eq!(detail.usage.tool_actions, u32::try_from(cap).unwrap());
        assert_eq!(detail.terminal_diagnostic, None);
        let events = jobs.investigation_events(&f.id, 0).unwrap();
        let refused = events
            .items
            .iter()
            .filter(|e| e.kind == InvestigationEventKind::ActionRefused)
            .count();
        if offered {
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            assert_eq!(refused, 0);
            assert!(
                detail.error.as_deref().unwrap().contains("store_capacity"),
                "{:?}",
                detail.error
            );
        } else {
            assert_eq!(
                provider.calls.load(Ordering::SeqCst),
                3,
                "{:?}",
                detail.error
            );
            assert_eq!(refused, 1);
            assert_eq!(
                detail.summary.completion_outcome,
                Some(InvestigationCompletionOutcome::ObligationsUnmet)
            );
            assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 2);
        }
    }
}
