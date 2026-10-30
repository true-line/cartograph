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

// AC-0485: scripted read outcomes, actual candidates/certificates/writer; no provider.
#[test]
fn metadata_choices_use_projected_attempts_and_reopen_after_publication() {
    use next_prompt_invocation::tests::{firewall, limits, provider};
    for kind in [ContextPreviewKind::Query, ContextPreviewKind::Inventory] {
        let mut proposal = Proposal::gated(kind);
        let snapshot = assert_read_only(&proposal);
        let candidate = proposal.candidate(&snapshot);
        let action = candidate.action(snapshot.next_action_id().into()).unwrap();
        let prompt: Value = serde_json::from_str(&action.payload.prompt).unwrap();
        let entries = prompt["turn_progress"]["acquisition_choices"]["entries"]
            .as_array()
            .unwrap();
        if kind == ContextPreviewKind::Query {
            assert_eq!(snapshot.metadata().completed_attempt_ids.len(), 0);
            assert!(
                entries
                    .iter()
                    .any(|item| item["action"]["type"] == "read_evidence")
            );
        } else {
            assert_eq!(snapshot.metadata().completed_attempt_ids.len(), 1);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0]["action"]["type"], "query_context");
            assert!(
                !entries
                    .iter()
                    .any(|item| item["action"]["type"] == "read_coverage_inventory")
            );
        }
        let prepared = firewall()
            .prepare_bounded(&provider(false), &action, &limits())
            .unwrap();
        let disclosure = candidate
            .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
            .unwrap()
            .unwrap();
        let stamp = disclosure.admission.as_ref().unwrap();
        snapshot.validate_acquisition(stamp).unwrap();
        assert!(
            stamp
                .acquisition
                .as_ref()
                .unwrap()
                .action_feedback_hash
                .is_none()
        );
        #[cfg(unix)]
        {
            let (retained, _) =
                metadata_history_workspace::next_prompt_metadata_continuation_workspace().unwrap();
            assert!(
                retained
                    >= context_hub::logical::count(&prompt["turn_progress"]["acquisition_choices"])
                        .unwrap()
            );
        }
        let expected = stamp.acquisition.as_ref().unwrap().choices_hash.clone();
        proposal.publish(&snapshot);
        let reopened = JobStore::open(proposal.f.base.f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task(&tx, proposal.f.base.id()).unwrap();
        let ledger = load_ledger(&tx, &task).unwrap().unwrap();
        let metadata = completion::metadata_current_next_prompt(&tx, &task).unwrap();
        assert_eq!(&metadata, snapshot.metadata());
        let choices = completion::SavedMetadataHistory::load(&tx, &task)
            .unwrap()
            .current_choices(&ledger, &metadata)
            .unwrap()
            .unwrap();
        assert_eq!(choices.stamp_hash().unwrap(), expected);
        if kind == ContextPreviewKind::Inventory {
            // Substitute a well-formed, checksummed older ledger at the exact
            // revision now named by the acquired inventory attempt.
            completion::rewrite_test_body(
                &tx,
                "investigation_inputs",
                proposal.f.base.id(),
                "revision",
                &signed(ledger.revision).unwrap(),
                &encode(&proposal.f.base.f.ledger, MAX_MANIFEST_BYTES).unwrap(),
            );
            assert!(completion::SavedMetadataHistory::load(&tx, &task).is_err());
        }
    }
}

#[test]
fn metadata_choices_reject_stale_authority_before_publication() {
    use next_prompt_invocation::tests::{firewall, limits, provider};
    for kind in [ContextPreviewKind::Query, ContextPreviewKind::Inventory] {
        let mut proposal = Proposal::gated(kind);
        let snapshot = proposal.snapshot().unwrap();
        let before = proposal.f.rows();
        let tx = proposal
            .f
            .base
            .f
            .store
            .conn
            .unchecked_transaction()
            .unwrap();
        let task = load_next_prompt_task(&tx, proposal.f.base.id()).unwrap();
        let call = task
            .admission_state
            .as_ref()
            .unwrap()
            .current_call
            .as_ref()
            .unwrap();
        let stale = completion::SavedMetadataHistory::load(&tx, &task)
            .unwrap()
            .acquisition_choices_at(call, &proposal.f.base.f.ledger)
            .unwrap()
            .unwrap();
        drop(tx);
        // A genuine stale authority cannot pass Input's fingerprint binding.
        assert!(
            proposal
                .candidate(&snapshot)
                .with_acquisition_choices(std::sync::Arc::new(stale))
                .is_err()
        );
        let candidate = proposal.candidate(&snapshot);
        let prepared = firewall()
            .prepare_bounded(
                &provider(false),
                &candidate.action(snapshot.next_action_id().into()).unwrap(),
                &limits(),
            )
            .unwrap();
        let good = candidate
            .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
            .unwrap()
            .unwrap();
        snapshot
            .validate_acquisition(good.admission.as_ref().unwrap())
            .unwrap();
        let old = proposal
            .f
            .input
            .seal_disclosure_with_policy(&proposal.f.prepared, &INVESTIGATION_V2_POLICY)
            .unwrap()
            .unwrap();
        // Isolate the exact stamp comparator independently used by the writer,
        // using genuine current/candidate seals, without a forgeable cache API.
        assert!(
            snapshot
                .validate_acquisition(old.admission.as_ref().unwrap())
                .is_err()
        );
        let callbacks = std::cell::Cell::new(0);
        // The real writer rejects this valid stale Input earlier on its old
        // ledger; this is atomicity proof, not isolation of its stamp check.
        assert!(
            proposal
                .try_publish(&snapshot, true, |_| {
                    callbacks.set(callbacks.get() + 1);
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(callbacks.get(), 0);
        assert_eq!(proposal.f.rows(), before);
    }
}
