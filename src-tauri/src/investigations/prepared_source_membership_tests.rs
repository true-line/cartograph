//! AC-0488: actual response coordinator and owner writer, no provider dispatch.
use super::*;
use crate::investigations::worker::{handle_gated_response, with_response_app};
use agents::investigation::{
    CapacityRefusalReason, InvestigationEventKind, InvestigationQueryBudget,
};
use llm::bounded::{BoundedCompletion, CompletionLimits, ReportedUsage};
use tauri::Manager;

fn completion(
    saved: &CapturedSourceInvocationFixture,
    action: &serde_json::Value,
) -> BoundedCompletion {
    BoundedCompletion {
        text: format!("\n{action}\n"),
        requested_model: saved.prepared().profile().requested_model.clone(),
        response_model: None,
        provider_request_id: None,
        usage: Some(ReportedUsage {
            input_tokens: Some(7),
            output_tokens: Some(3),
            cache_read_input_tokens: Some(2),
            cache_creation_input_tokens: None,
        }),
        request_bytes: 17,
        response_bytes: 23,
    }
}
fn rows(path: &std::path::Path, id: &str, table: &str) -> Vec<Vec<u8>> {
    rusqlite::Connection::open_with_flags(
        path.join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .prepare(&format!(
        "SELECT body FROM {table} WHERE task_id=?1 ORDER BY body"
    ))
    .unwrap()
    .query_map([id], |row| row.get(0))
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn worker_membership_refuses_before_acquisition_and_reopens_feedback() {
    for kind in ["query", "source", "inventory"] {
        let (saved, state, context, target, path) = fixture_for(true);
        with_response_app(state, |app| {
            let state = app.state::<AppState>();
            let detail = || {
                state
                    .jobs
                    .lock()
                    .unwrap()
                    .investigation(saved.id())
                    .unwrap()
            };
            let before = detail();
            let inputs = rows(&path, saved.id(), "investigation_inputs");
            let events = rows(&path, saved.id(), "investigation_events");
            let offered = completion(&saved, saved.admission().original_action);
            assert!(!handle_gated_response(app, &saved, &offered, || {}).unwrap());
            assert_eq!(detail(), before);
            assert_eq!(rows(&path, saved.id(), "investigation_events"), events);
            let context_id = saved.request().expected_context_id.clone();
            let action = match kind {
                "query" => json!({"type":"query_context","context_id":context_id,
                    "query":{"scope":{"type":"node","id":"absent-query-anchor"},
                    "kind":"node","labels":[],"max_facts":1,"max_bytes":32768,"cursor":null}}),
                "source" => {
                    json!({"type":"read_evidence","fact":{"kind":"node","id":"not-admitted"},
                    "role":target.role,"index":target.index})
                }
                _ => json!({"type":"read_coverage_inventory","context_id":context_id,
                    "repo":"absent/repo","inventory_hash":"a".repeat(64),"max_bytes":32768}),
            };
            let response = completion(&saved, &action);
            assert!(handle_gated_response(app, &saved, &response, || {}).unwrap());
            let after = detail();
            let mut admitted = after.usage.clone();
            admitted.reported_input_tokens = before.usage.reported_input_tokens;
            admitted.reported_output_tokens = before.usage.reported_output_tokens;
            admitted.reported_cache_read_tokens = before.usage.reported_cache_read_tokens;
            admitted.active_milliseconds = before.usage.active_milliseconds;
            assert_eq!(admitted, before.usage); // invocation already charged; no acquisition debit
            assert_eq!(saved.counts(), (0, 0, 0));
            assert_eq!(rows(&path, saved.id(), "investigation_inputs"), inputs);
            let page = state
                .jobs
                .lock()
                .unwrap()
                .investigation_events(saved.id(), before.summary.last_event_sequence)
                .unwrap();
            assert_eq!(
                page.items.iter().map(|item| item.kind).collect::<Vec<_>>(),
                [
                    InvestigationEventKind::ModelCompleted,
                    InvestigationEventKind::ActionRefused
                ]
            );
            let reopened = crate::jobs::JobStore::open(path.join("state.db")).unwrap();
            let snapshot = reopened
                .investigation_next_prompt_snapshot(saved.id(), saved.request())
                .unwrap();
            assert!(snapshot.attempts().is_empty());
            assert_eq!(snapshot.capacity_refusal_count(), 0);
            assert_eq!(
                InvestigationQueryBudget::from_ledger(snapshot.ledger()).unwrap(),
                InvestigationQueryBudget::from_ledger(saved.current().ledger()).unwrap()
            );
            let next = InvestigationInput::prepare_v9_with_policy(
                &saved.request().question,
                before.summary.specialist_id,
                snapshot.ledger().clone(),
                &context
                    .query_pages
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                &[],
                &[],
                &snapshot.detail().usage,
                snapshot.basis(),
                saved.request(),
                snapshot.metadata(),
                &[],
                snapshot.mode(),
                snapshot.capacity_refusal_count(),
                snapshot.capacity_feedback(),
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
            let next = snapshot.attach_acquisition(next).unwrap();
            let action_hash = content_hash(&serde_json::to_vec(&action).unwrap());
            let next_action = next.action("membership-feedback".into()).unwrap();
            let prompt: serde_json::Value =
                serde_json::from_str(&next_action.payload.prompt).unwrap();
            assert_eq!(
                prompt["turn_progress"]["action_feedback"]["action_hash"],
                action_hash
            );
            assert_eq!(
                prompt["turn_progress"]["action_feedback"]["invocation_ordinal"],
                before.usage.model_invocations
            );
            let prepared = llm::EgressFirewall::new(llm::EgressPolicy::default())
                .prepare_bounded(
                    &super::publication_tests::provider(),
                    &next_action,
                    &CompletionLimits::default(),
                )
                .unwrap();
            assert!(
                next.seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
                    .unwrap()
                    .unwrap()
                    .admission
                    .unwrap()
                    .acquisition
                    .unwrap()
                    .action_feedback_hash
                    .is_some()
            );
            let connection = rusqlite::Connection::open(path.join("state.db")).unwrap();
            let bytes: Vec<u8> = connection
                .query_row(
                    "SELECT body FROM investigation_responses WHERE task_id=?1 AND step_id=?2",
                    rusqlite::params![saved.id(), saved.admission().response.step_id],
                    |row| row.get(0),
                )
                .unwrap();
            let recorded: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                recorded["response_hash"],
                content_hash(response.text.as_bytes())
            );
            assert_eq!(recorded["action_hash"], action_hash);
            assert_ne!(recorded["response_hash"], recorded["action_hash"]);
            assert_eq!(recorded["reported_input_tokens"], 7);
            assert_eq!(recorded["reported_output_tokens"], 3);
            assert_eq!(recorded["reported_cache_read_tokens"], 2);
        });
    }
}

#[test]
fn worker_membership_cancellation_and_stale_revision_do_not_publish() {
    for cancel in [true, false] {
        let (saved, state, _, _, path) = fixture_for(true);
        with_response_app(state, |app| {
            let state = app.state::<AppState>();
            let action = json!({"type":"read_evidence","fact":{"kind":"node","id":"not-admitted"},"role":"provenance","index":0});
            let response = completion(&saved, &action);
            let after_race = std::cell::RefCell::new(vec![]);
            let result = handle_gated_response(app, &saved, &response, || {
                let mut jobs = state.jobs.lock().unwrap();
                if cancel {
                    jobs.cancel_next_prompt_investigation(saved.id()).unwrap();
                } else {
                    jobs.refuse_next_prompt_capacity(
                        saved.execution(),
                        saved.revision(),
                        saved.admission().response,
                        saved.admission().original_action,
                        CapacityRefusalReason::PromptCapacity,
                    )
                    .unwrap();
                }
                *after_race.borrow_mut() = rows(&path, saved.id(), "investigation_events");
            });
            let error = result.unwrap_err();
            assert!(
                error.contains(if cancel { "Cancelled" } else { "Stale" }),
                "{error}"
            );
            assert_eq!(
                rows(&path, saved.id(), "investigation_events"),
                *after_race.borrow()
            );
            assert!(
                state
                    .jobs
                    .lock()
                    .unwrap()
                    .investigation_events(saved.id(), 0)
                    .unwrap()
                    .items
                    .iter()
                    .all(|item| item.kind != InvestigationEventKind::ActionRefused)
            );
            assert_eq!(saved.counts(), (0, 0, 0));
        });
    }
}
