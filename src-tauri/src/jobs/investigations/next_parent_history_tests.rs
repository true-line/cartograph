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
fn parent_history_borrowed_encoding_preserves_canonical_wire_and_refuses_large_envelope() {
    // AC-0327: compare bytes, including nested key order, with the prior shape.
    let f = completed();
    let parent = f.saved();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let result = load_result(&tx, &parent).unwrap();
    let summary = &parent.detail.summary;
    let prior = serde_json::json!({
        "parent_investigation_id": f.base.id(),
        "original_graph_snapshot_id": summary.graph_snapshot_id,
        "original_scope": summary.scope,
        "saved_execution_status": summary.status,
        "cancellation_requested": summary.cancel_requested,
        "invocation_outcome_pending": summary.invocation_pending,
        "authority": "Prior specialist findings remain T3/InferredWeak; execution status does not establish complete knowledge.",
        "saved_result": result,
    });
    assert_eq!(
        encode_parent_history(f.base.id(), summary, &result).unwrap(),
        encode(&prior, 16 * 1024).unwrap()
    );
    let mut oversized = summary.clone();
    oversized.graph_snapshot_id = Some("x".repeat(16 * 1024));
    assert!(matches!(
        encode_parent_history(f.base.id(), &oversized, &result),
        Err(InvestigationStoreError::Capacity)
    ));
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
    // AC-0327: transfer the genuine returned history without duplicating its
    // retained vector/string payload at the worker boundary.
    let pointer = history.history()[0].as_ptr();
    let vector = history.history().as_ptr();
    #[cfg(unix)]
    {
        let (retained, projection) = parent_history_projection_workspace().unwrap();
        let actual = std::mem::size_of::<NextParentHistory>()
            + std::mem::size_of::<String>()
            + history.history()[0].len();
        assert!(retained >= actual);
        assert!(projection > retained);
    }
    let moved = history.into_history();
    assert_eq!(moved[0].as_ptr(), pointer);
    assert_eq!(moved.as_ptr(), vector);
    let history = NextParentHistory { history: moved };
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
    // AC-0578: actual fresh child start persists the version, reopen recovers it.
    assert_eq!(
        started.detail.limits.history_policy,
        Some(agents::investigation::HistoryPolicy::ParentHistory32768V1)
    );
    assert_eq!(
        policy_for_detail(&started.detail)
            .unwrap()
            .supplied_history_bytes(),
        32768
    );
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
            .investigation(&started.detail.summary.investigation_id)
            .unwrap()
            .limits,
        started.detail.limits
    );
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

// AC-0287: choosing Standard for a follow-up preserves the parent's immutable
// family while admitting only the child's exact historical request4 tuple.
#[test]
fn next_parent_history_completed_task3_allows_standard_child_without_cross_conversation() {
    let mut fixture = completed();
    let parent = fixture.saved();
    assert_eq!(parent.detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(
        parent.detail.summary.specialist_id,
        SpecialistId::DomainAnalystV10
    );
    let parent_id = fixture.base.id().to_owned();
    let parent_result = fixture
        .base
        .f
        .store
        .investigation_result(&parent_id)
        .unwrap()
        .unwrap();
    assert_eq!(parent_result.schema_version, 7);
    let mut child = fixture.base.f.request.clone();
    child.schema_version = 4;
    child.specialist_id = "domain-analyst@7".into();
    child.limit_profile = INVESTIGATION_V1_POLICY.profile().into();
    child.obligations.schema_version = 1;
    child.obligations.contract = "investigation-completion-obligations@1".into();
    child.request_nonce = "standard-other-conversation".into();
    child.parent_id = None;
    child.conversation_id = None;
    let child = child
        .normalized_with_policy(&INVESTIGATION_V1_POLICY)
        .unwrap();
    let other = fixture
        .base
        .f
        .store
        .start_completion_investigation(&child, Some(&parent.detail.provider))
        .unwrap();
    assert_ne!(
        other.detail.summary.conversation_id,
        parent.detail.summary.conversation_id
    );
    let mut child = child;
    child.request_nonce = "standard-task3-parent-followup".into();
    child.parent_id = Some(parent_id.clone());
    child.conversation_id = Some(other.detail.summary.conversation_id);
    let counts = |store: &JobStore| -> (i64, i64, i64) {
        store.conn.query_row(
            "SELECT (SELECT count(*) FROM investigation_tasks),(SELECT count(*) FROM jobs),(SELECT count(*) FROM investigation_conversations)",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).unwrap()
    };
    let before = counts(&fixture.base.f.store);
    assert!(matches!(
        fixture
            .base
            .f
            .store
            .start_completion_investigation(&child, Some(&parent.detail.provider),),
        Err(InvestigationStoreError::Invalid)
    ));
    assert_eq!(counts(&fixture.base.f.store), before);
    child.conversation_id = Some(parent.detail.summary.conversation_id.clone());
    let expected_hash = child
        .intent_hash_with_policy(&INVESTIGATION_V1_POLICY)
        .unwrap();
    let started = fixture
        .base
        .f
        .store
        .start_completion_investigation(&child, Some(&parent.detail.provider))
        .unwrap();
    assert!(started.created);
    assert_eq!(
        started.detail.summary.parent_id.as_deref(),
        Some(parent_id.as_str())
    );
    assert_eq!(
        started.detail.summary.conversation_id,
        parent.detail.summary.conversation_id
    );
    assert_eq!(
        started.detail.summary.specialist_id,
        SpecialistId::DomainAnalystV7
    );
    assert_eq!(started.detail.limits, INVESTIGATION_V1_POLICY.limits());
    let tx = fixture.base.f.store.conn.unchecked_transaction().unwrap();
    let saved = load_task(&tx, &started.detail.summary.investigation_id).unwrap();
    assert_eq!(saved.schema_version, 2);
    assert!(saved.admission_state.is_none());
    assert_eq!(saved.completion_intent.as_ref().unwrap().schema_version, 1);
    assert_eq!(saved.intent_hash, expected_hash);
    assert_eq!(
        saved.completion_intent.as_ref().unwrap().request_hash,
        expected_hash
    );
    tx.commit().unwrap();
    assert_eq!(
        fixture.base.f.store.investigation(&parent_id).unwrap(),
        parent.detail
    );
    assert_eq!(
        fixture
            .base
            .f
            .store
            .investigation_result(&parent_id)
            .unwrap()
            .unwrap(),
        parent_result
    );
    let reopened = JobStore::open(fixture.base.f.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation(&started.detail.summary.investigation_id)
            .unwrap(),
        started.detail
    );
    assert_eq!(reopened.investigation(&parent_id).unwrap(), parent.detail);
    assert_eq!(
        reopened.investigation_result(&parent_id).unwrap().unwrap(),
        parent_result
    );
}

// AC-0327: inspect the legacy component before the larger worker maximum can
// mask an omitted overlapping ledger/metadata or second invocation row.
#[cfg(unix)]
#[test]
fn parent_history_legacy_pending_accounts_for_overlapping_owners() {
    let (basis, invoked) = legacy_parent_pending_workspace().unwrap();
    let (ledger, _) = InvestigationInputLedger::decode_workspace_bytes().unwrap();
    let metadata = next_prompt_metadata_workspace().unwrap().2;
    let disclosure = agents::investigation::disclosure_validation_workspace(
        &INVESTIGATION_V2_POLICY,
        MAX_RECORD_BYTES,
    )
    .unwrap();
    let other = next_prompt_ledger_workspace().unwrap()
        + disclosure
        + context_hub::logical::json_bytes(INVESTIGATION_V2_POLICY.envelope_bytes()).unwrap() * 3
        + legacy_parent_contract_workspace().unwrap();
    assert_eq!(basis.checked_sub(other), Some(ledger + metadata));
    let decoding = record_decode_scratch_bytes()
        .unwrap()
        .max(context_hub::logical::json_bytes(MAX_RECORD_BYTES).unwrap() * (RECORD_MAX_DEPTH + 1));
    let acquisition = decoding
        + MAX_RECORD_BYTES
        + std::mem::size_of::<Vec<u8>>()
        + 128
        + 3 * std::mem::size_of::<String>()
        + 4096;
    assert_eq!(
        invoked.checked_sub(acquisition),
        Some(metadata_history_workspace::row_workspace().unwrap())
    );
    assert!(
        legacy_parent_contract_workspace().unwrap()
            >= next_prompt_contract_workspace().unwrap()
                + context_hub::logical::json_bytes(MAX_RECORD_BYTES).unwrap() * 3
    );
}

#[test]
fn versioned_parent_history_bound_and_canonical_replay() {
    // AC-0578: envelope and escaped-array bounds both apply; old canonical bytes survive.
    let f = completed();
    let parent = f.saved();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let result = load_result(&tx, &parent).unwrap();
    let old = encode_parent_history(f.base.id(), &parent.detail.summary, &result).unwrap();
    let cap = agents::investigation::PARENT_HISTORY_32768_V1_BYTES;
    assert_eq!(
        old,
        encode_parent_history_with_limit(f.base.id(), &parent.detail.summary, &result, cap)
            .unwrap()
    );
    let mut summary = parent.detail.summary.clone();
    summary.graph_snapshot_id = Some("x".repeat(20000));
    assert!(encode_parent_history(f.base.id(), &summary, &result).is_err());
    let raw = encode_parent_history_with_limit(f.base.id(), &summary, &result, cap).unwrap();
    let extra = cap - raw.len();
    summary
        .graph_snapshot_id
        .as_mut()
        .unwrap()
        .push_str(&"x".repeat(extra));
    let raw = encode_parent_history_with_limit(f.base.id(), &summary, &result, cap).unwrap();
    assert_eq!(raw.len(), cap);
    assert!(encode(&vec![String::from_utf8(raw).unwrap()], cap).is_err());
    summary.graph_snapshot_id.as_mut().unwrap().push('x');
    assert!(encode_parent_history_with_limit(f.base.id(), &summary, &result, cap).is_err());
    for bytes in [23240, cap, cap + 1] {
        let history = vec!["x".repeat(bytes - 4)];
        assert_eq!(encode(&history, cap).is_ok(), bytes <= cap);
        assert!(
            encode(
                &history,
                agents::investigation::LEGACY_SUPPLIED_HISTORY_BYTES
            )
            .is_err()
        );
    }
}

#[test]
fn a_child_resumes_with_the_history_bytes_its_ledger_hashed() {
    // AC-1336 (#598, Codex P2 on #2501): a child saved before the reading
    // keeps its parent's earlier bytes; any other child gets the current ones.
    let policy = &INVESTIGATION_V2_POLICY;
    let history = |text: &str| NextParentHistory {
        history: vec![text.to_owned()],
    };
    let hash = |text: &str| {
        core_prov::content_hash(
            &encode(&vec![text.to_owned()], policy.supplied_history_bytes()).unwrap(),
        )
    };
    let pick = |saved: Option<String>, before: Option<&str>| {
        resume_history(history("current"), saved.as_deref(), policy, || {
            Ok(before.map(history))
        })
        .unwrap()
        .into_history()
    };
    assert_eq!(pick(None, Some("before")), ["current"]);
    assert_eq!(pick(Some(hash("current")), Some("before")), ["current"]);
    assert_eq!(pick(Some(hash("before")), Some("before")), ["before"]);
    // Neither matches: the current history, which preparation then refuses.
    assert_eq!(pick(Some(hash("other")), Some("before")), ["current"]);
    assert_eq!(pick(Some(hash("before")), None), ["current"]);
}
