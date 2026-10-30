//! AC-0489: genuine initial writer and real private loop; no public activation.
use super::*;

fn fixture(flow: GatedFlow) -> (NextFixture, Arc<NextProvider>) {
    let mut scripted = NextProvider::new(false);
    scripted.gated_flow = Some(flow);
    let provider = Arc::new(scripted);
    (NextFixture::gated(provider.as_ref()), provider)
}
fn detail(f: &NextFixture) -> InvestigationDetail {
    f.base
        .base
        .app
        .state::<AppState>()
        .jobs
        .lock()
        .unwrap()
        .investigation(&f.id)
        .unwrap()
}
fn count(f: &NextFixture, table: &str) -> u32 {
    rusqlite::Connection::open(f.base.base.app_data.join("state.db"))
        .unwrap()
        .query_row(
            &format!("SELECT count(*) FROM {table} WHERE task_id=?1"),
            [&f.id],
            |r| r.get(0),
        )
        .unwrap()
}
fn charges(f: &NextFixture, provider: &NextProvider, expected: u32) {
    assert_eq!(
        provider.observed.calls.load(Ordering::SeqCst),
        expected as usize,
        "actual provider calls"
    );
    let detail = detail(f);
    assert_eq!(detail.usage.model_invocations, expected);
    assert_eq!(
        detail.usage.generated_token_reservations,
        expected * detail.limits.generated_tokens_per_invocation
    );
    assert_eq!(count(f, "investigation_invocations"), expected);
    for (index, prompt) in provider.observed.prompts.lock().unwrap().iter().enumerate() {
        assert_eq!(prompt["consumed_budgets"]["model_invocations"], index);
    }
}
fn budget(prompt: &Value) -> InvestigationQueryBudget {
    serde_json::from_value(prompt["turn_progress"]["raw_query_budget"].clone()).unwrap()
}

#[test]
fn gated_initial_worker_refusal_offered_finish_and_reopen() {
    let (f, provider) = fixture(GatedFlow::Repair);
    f.run_gated_inner(provider.as_ref());
    charges(&f, &provider, 3);
    let result = detail(&f);
    assert_eq!(result.usage.tool_actions, 1);
    assert_eq!(result.usage.evidence_requests, 0);
    assert_eq!(count(&f, "completion_attempts"), 1);
    let prompts = provider.observed.prompts.lock().unwrap();
    assert!(prompts[0]["turn_progress"]["action_feedback"].is_null());
    let offered = prompts[0]["turn_progress"]["acquisition_choices"]["entries"]
        .as_array()
        .unwrap();
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0]["action"]["type"], "read_coverage_inventory");
    let payloads = provider.received_payloads.lock().unwrap();
    let schema: Value =
        serde_json::from_str(payloads[0].response_schema.as_deref().unwrap()).unwrap();
    let finite = schema["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|item| item.get("enum"))
        .unwrap();
    assert_eq!(
        *finite,
        json!(
            offered
                .iter()
                .map(|item| &item["action"])
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(
        prompts[1]["turn_progress"]["action_feedback"]["reason"],
        "action_not_offered"
    );
    assert_eq!(
        prompts[1]["turn_progress"]["action_feedback"]["invocation_ordinal"],
        1
    );
    assert_eq!(
        prompts[0]["turn_progress"]["acquisition_choices"],
        prompts[1]["turn_progress"]["acquisition_choices"]
    );
    assert_eq!(budget(&prompts[0]), budget(&prompts[1]));
    assert_eq!(prompts[1]["consumed_budgets"]["tool_actions"], 0);
    assert!(prompts[2]["turn_progress"]["action_feedback"].is_null());
    assert_eq!(
        prompts[2]["completion_metadata"]["completed_attempt_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(budget(&prompts[2]).consumed_bytes > budget(&prompts[1]).consumed_bytes);
    assert_eq!(
        result.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        result.error
    );
    assert_eq!(
        result.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::InsufficientEvidence)
    );
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), result);
    assert!(reopened.investigation_result(&f.id).unwrap().is_some());
}

#[test]
fn gated_initial_worker_refusals_stop_at_existing_finish_reserve() {
    let (f, provider) = fixture(GatedFlow::Repeat);
    f.run_gated_inner(provider.as_ref());
    let result = detail(&f);
    let prompts = provider.observed.prompts.lock().unwrap();
    let metadata: staged_refusal::ProtectedCompletionMetadata =
        serde_json::from_value(prompts[0]["completion_metadata"].clone()).unwrap();
    let reserve = metadata
        .finish_call_reserve_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let expected = result.limits.model_invocations - reserve.model_invocations + 1;
    drop(prompts);
    charges(&f, &provider, expected);
    assert_eq!(result.summary.status, InvestigationStatus::Failed);
    assert!(result.usage.model_invocations < result.limits.model_invocations);
    assert_eq!(result.usage.tool_actions, 0);
    assert_eq!(result.usage.evidence_requests, 0);
    assert_eq!(result.usage.evidence_items, 0);
    assert_eq!(count(&f, "completion_attempts"), 0);
    assert_eq!(count(&f, "investigation_inputs"), 1);
    let prompts = provider.observed.prompts.lock().unwrap();
    assert_eq!(
        prompts.last().unwrap()["turn_progress"]["admission_mode"],
        "finish_only"
    );
    for (index, prompt) in prompts.iter().enumerate() {
        assert_eq!(budget(prompt), budget(&prompts[0]));
        assert_eq!(prompt["consumed_budgets"]["tool_actions"], 0);
        if index > 0 {
            assert_eq!(
                prompt["turn_progress"]["action_feedback"]["invocation_ordinal"],
                index
            );
        }
    }
    let state = f.base.base.app.state::<AppState>();
    let page = state
        .jobs
        .lock()
        .unwrap()
        .investigation_events(&f.id, 0)
        .unwrap();
    // Fetch the whole paginated event stream rather than inferring refusals from calls.
    let mut next = page;
    let mut refused = 0;
    loop {
        refused += next
            .items
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::ActionRefused)
            .count();
        if !next.has_more {
            break;
        }
        next = state
            .jobs
            .lock()
            .unwrap()
            .investigation_events(&f.id, next.next_sequence)
            .unwrap();
    }
    assert_eq!(refused, expected as usize - 1); // final FinishOnly read is terminal, not repair
}

#[test]
fn gated_initial_worker_cancelled_unoffered_does_not_publish() {
    let (f, provider) = fixture(GatedFlow::Cancel);
    let app = f.base.base.app.handle().clone();
    let id = f.id.clone();
    *provider.observed.cancel.lock().unwrap() = Some(Box::new(move || {
        app.state::<AppState>()
            .jobs
            .lock()
            .unwrap()
            .cancel_next_prompt_investigation(&id)
            .unwrap();
    }));
    f.run_gated_inner(provider.as_ref());
    charges(&f, &provider, 1);
    let result = detail(&f);
    assert_eq!(result.summary.status, InvestigationStatus::Cancelled);
    assert_eq!(result.usage.tool_actions, 0);
    assert_eq!(result.usage.evidence_requests, 0);
    assert_eq!(count(&f, "investigation_responses"), 0);
    assert_eq!(count(&f, "completion_attempts"), 0);
    let state = f.base.base.app.state::<AppState>();
    assert!(
        state
            .jobs
            .lock()
            .unwrap()
            .investigation_events(&f.id, 0)
            .unwrap()
            .items
            .iter()
            .all(|event| event.kind != InvestigationEventKind::ActionRefused)
    );
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), result);
}
