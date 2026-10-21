//! AC-0287: genuine completed/internal parent, full saved authority and exact
//! mandatory history through public reads. No provider dispatch or migration.
use super::*;
use completion::FinishFixture;

fn completed() -> FinishFixture {
    let mut f = FinishFixture::new(true);
    f.begin();
    f.publish().unwrap();
    f
}
#[test]
fn next_parent_history_completed_parent_starts_internal_child_with_exact_saved_context() {
    let mut f = completed();
    let parent = f.saved();
    let history = f
        .base
        .f
        .store
        .investigation_next_parent_history(f.base.id(), &parent.detail.summary.conversation_id)
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&history.history()[0]).unwrap();
    assert_eq!(
        value["original_graph_snapshot_id"],
        serde_json::to_value(&parent.detail.summary.graph_snapshot_id).unwrap()
    );
    assert_eq!(
        value["original_scope"],
        serde_json::to_value(&parent.detail.summary.scope).unwrap()
    );
    assert_eq!(
        value["saved_result"],
        serde_json::to_value(
            load_result(
                &f.base.f.store.conn.unchecked_transaction().unwrap(),
                &parent
            )
            .unwrap()
        )
        .unwrap()
    );
    let mut child = f.base.f.request.clone();
    child.request_nonce = "internal-parent-child".into();
    child.parent_id = Some(f.base.id().into());
    child.conversation_id = Some(parent.detail.summary.conversation_id.clone());
    child.specialist_id = "evidence-auditor@10".into();
    let started = f
        .base
        .f
        .store
        .start_next_prompt_investigation(&child, Some(&parent.detail.provider))
        .unwrap();
    assert_eq!(started.detail.summary.parent_id, child.parent_id);
    assert_eq!(
        started.detail.summary.conversation_id,
        parent.detail.summary.conversation_id
    );
    assert_eq!(
        f.base.f.store.investigation(f.base.id()).unwrap(),
        parent.detail
    );
    assert_eq!(
        serde_json::to_value(f.base.f.store.investigation_result(f.base.id()).unwrap()).unwrap(),
        value["saved_result"]
    );
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_next_parent_history(f.base.id(), &parent.detail.summary.conversation_id)
            .unwrap()
            .history(),
        history.history()
    );
}
#[test]
fn next_parent_history_preserves_live_and_failed_optional_result_semantics() {
    let mut f = FinishFixture::new(false);
    let id = f.base.id().to_owned();
    let conversation = f.base.f.task.detail.summary.conversation_id.clone();
    let live = f
        .base
        .f
        .store
        .investigation_next_parent_history(&id, &conversation)
        .unwrap();
    let live: serde_json::Value = serde_json::from_str(&live.history()[0]).unwrap();
    assert!(live["saved_result"].is_null());
    assert_eq!(
        live["saved_execution_status"],
        serde_json::to_value(f.base.f.task.detail.summary.status).unwrap()
    );
    f.base
        .f
        .store
        .stop_next_prompt_investigation(
            &f.base.f.execution,
            f.base.f.task.detail.summary.revision,
            InvestigationStopReason::Operational,
            f.base.f.task.detail.usage.active_milliseconds,
        )
        .unwrap();
    let failed = f
        .base
        .f
        .store
        .investigation_next_parent_history(&id, &conversation)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&failed.history()[0]).unwrap()["saved_execution_status"],
        serde_json::to_value(InvestigationStatus::Failed).unwrap()
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&failed.history()[0]).unwrap()["saved_result"]
            .is_null()
    );
}
#[test]
fn next_parent_history_rejects_foreign_conversation_namespace_and_rehashed_wrong_family() {
    let f = completed();
    let task = f.saved();
    let id = f.base.id();
    let conversation = &task.detail.summary.conversation_id;
    assert!(matches!(
        f.base
            .f
            .store
            .investigation_next_parent_history(id, "other-conversation"),
        Err(InvestigationStoreError::Invalid)
    ));
    let other = tempfile::tempdir().unwrap();
    let store = JobStore::open(other.path().join("state.db")).unwrap();
    assert!(matches!(
        store.investigation_next_parent_history(id, conversation),
        Err(InvestigationStoreError::Missing)
    ));
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let mut result = load_result(&tx, &task).unwrap().unwrap();
    result.schema_version = 6;
    let trigger: String = tx
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='investigation_results_no_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    tx.execute_batch("DROP TRIGGER investigation_results_no_update")
        .unwrap();
    let body = encode(&result, MAX_RESULT_BYTES).unwrap();
    tx.execute(
        "UPDATE investigation_results SET body=?1,body_hash=?2 WHERE task_id=?3",
        params![body, core_prov::content_hash(&body), id],
    )
    .unwrap();
    tx.execute_batch(&trigger).unwrap();
    tx.commit().unwrap();
    assert!(
        f.base
            .f
            .store
            .investigation_next_parent_history(id, conversation)
            .is_err()
    );
}
