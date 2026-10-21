//! AC-0287: the worker's read snapshot and publication share exact derivation.
use super::*;
#[path = "metadata_continuation_fixture.rs"]
mod fixture;
use fixture::Proposal;

fn assert_read_only(proposal: &Proposal) -> MetadataContinuationSnapshot {
    let changes = proposal.f.base.f.store.conn.total_changes();
    let schema: i64 = proposal
        .f
        .base
        .f
        .store
        .conn
        .pragma_query_value(None, "schema_version", |row| row.get(0))
        .unwrap();
    let before = proposal.f.rows();
    let snapshot = proposal.snapshot().unwrap();
    assert_eq!(proposal.f.base.f.store.conn.total_changes(), changes);
    assert_eq!(
        proposal
            .f
            .base
            .f
            .store
            .conn
            .pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        schema
    );
    assert_eq!(proposal.f.rows(), before);
    assert!(proposal.f.base.f.store.conn.is_autocommit());
    snapshot
}
fn assert_agreement(proposal: &mut Proposal) {
    let snapshot = assert_read_only(proposal);
    assert_eq!(
        snapshot.revision(),
        proposal.f.base.f.task.detail.summary.revision
    );
    assert_eq!(
        snapshot.admitted_usage().model_invocations,
        proposal.f.base.f.task.detail.usage.model_invocations
    );
    let usage = snapshot.usage().clone();
    let metadata = snapshot.metadata().clone();
    let basis = snapshot.basis().clone();
    let mode = snapshot.mode();
    let count = snapshot.capacity_refusal_count();
    assert_eq!(
        snapshot.next_action_id(),
        format!(
            "{}:step-{}",
            proposal.f.base.id(),
            usage.model_invocations + 1
        )
    );
    let detail = proposal.publish(&snapshot);
    assert_eq!(detail.usage, usage);
    let reopened = JobStore::open(proposal.f.base.f.directory.path().join("state.db")).unwrap();
    let saved = reopened
        .investigation_next_prompt_snapshot(proposal.f.base.id(), &proposal.f.base.f.request)
        .unwrap();
    assert_eq!(saved.ledger(), &proposal.ledger);
    assert_eq!(saved.metadata(), &metadata);
    assert_eq!(saved.basis().input_ledger_hash, basis.input_ledger_hash);
    assert_eq!(saved.basis().previous_ledger, basis.previous_ledger);
    assert_eq!(saved.basis().completed_tool, basis.completed_tool);
    assert_eq!(saved.mode(), mode);
    assert_eq!(saved.capacity_refusal_count(), count);
    assert!(saved.capacity_feedback().is_none());
}

#[test]
fn metadata_continuation_snapshot_query_and_replay_keep_original_identity_and_attempt_prefix() {
    let mut proposal = Proposal::new(ContextPreviewKind::Query, true);
    let first = assert_read_only(&proposal);
    assert_eq!(first.metadata().completed_attempt_ids.len(), 1);
    assert_eq!(first.capacity_refusal_count(), 1);
    assert_agreement(&mut proposal);
    proposal.advance_query(true);
    assert_eq!(proposal.original["type"], "replay_query");
    let replay = assert_read_only(&proposal);
    assert_eq!(
        replay.metadata().completed_attempt_ids,
        first.metadata().completed_attempt_ids
    );
    assert_eq!(replay.capacity_refusal_count(), 1);
    assert_agreement(&mut proposal);
    let tx = proposal
        .f
        .base
        .f
        .store
        .conn
        .unchecked_transaction()
        .unwrap();
    let response: InvestigationResponseMeta = read_body(
        &tx,
        "investigation_responses",
        "step_id",
        proposal.f.base.id(),
        &proposal.response.step_id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        response.action_hash,
        core_prov::content_hash(&encode(&proposal.original, MAX_ACTION_BYTES).unwrap())
    );
    let resolved = ProgressiveInvestigationAction::decode_resolved(
        &serde_json::to_string(&proposal.original).unwrap(),
        &proposal.f.base.f.request.expected_context_id,
        &proposal
            .f
            .base
            .f
            .request
            .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
        replay.basis().previous_ledger.as_ref().unwrap(),
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    let CompletionInvestigationAction::Read(QualifiedInvestigationAction::QueryContext {
        context_id,
        query,
    }) = resolved
    else {
        unreachable!()
    };
    let projected =
        serde_json::json!({"type":"query_context","context_id":context_id,"query":query});
    assert_ne!(
        response.action_hash,
        core_prov::content_hash(&encode(&projected, MAX_ACTION_BYTES).unwrap())
    );
}

#[test]
fn metadata_continuation_snapshot_inventory_appends_exact_attempt_to_saved_history() {
    let mut proposal = Proposal::new(ContextPreviewKind::Inventory, false);
    let before = proposal.f.base.f.task.detail.usage.tool_actions;
    let snapshot = assert_read_only(&proposal);
    assert_eq!(snapshot.usage().tool_actions, before + 1);
    assert_eq!(snapshot.metadata().completed_attempt_ids.len(), 2);
    assert_agreement(&mut proposal);
    let reopened = JobStore::open(proposal.f.base.f.directory.path().join("state.db")).unwrap();
    let saved = reopened
        .investigation_next_prompt_snapshot(proposal.f.base.id(), &proposal.f.base.f.request)
        .unwrap();
    assert_eq!(saved.attempts().len(), 2);
    assert_eq!(
        saved.attempt_ledgers()[0].revision + 1,
        saved.attempt_ledgers()[1].revision
    );
    assert_ne!(
        saved.attempts()[0].attempt_id,
        saved.attempts()[1].attempt_id
    );
}

#[test]
fn metadata_continuation_snapshot_rejects_epoch_request_raw_and_action_substitution_without_writes()
{
    for kind in [
        "revision",
        "request",
        "raw",
        "action",
        "mixed_kind",
        "cancel",
    ] {
        let mut proposal = Proposal::new(ContextPreviewKind::Query, false);
        let mut expected = proposal.f.base.f.task.detail.summary.revision;
        match kind {
            "revision" => expected += 1,
            "request" => proposal.f.base.f.request.question.push_str(" changed"),
            "raw" => proposal.raw.push(' '),
            "action" => {
                proposal.original["query"]["max_facts"] = 1.into();
            }
            "mixed_kind" => proposal
                .ledger
                .coverage_reads
                .push(proposal.ledger.coverage_reads[0].clone()),
            "cancel" => {
                let id = proposal.f.base.id().to_owned();
                let detail = proposal
                    .f
                    .base
                    .f
                    .store
                    .cancel_next_prompt_investigation(&id)
                    .unwrap();
                expected = detail.summary.revision;
            }
            _ => unreachable!(),
        }
        let changes = proposal.f.base.f.store.conn.total_changes();
        let result = proposal
            .f
            .base
            .f
            .store
            .investigation_metadata_continuation_snapshot(
                proposal.f.base.id(),
                expected,
                &proposal.read(),
            );
        assert!(result.is_err(), "{kind}");
        if kind == "revision" {
            assert!(matches!(result, Err(InvestigationStoreError::Stale)));
        }
        if kind == "cancel" {
            assert!(matches!(result, Err(InvestigationStoreError::Cancelled)));
        }
        assert_eq!(
            proposal.f.base.f.store.conn.total_changes(),
            changes,
            "{kind}"
        );
    }
}
