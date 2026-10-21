use super::*;

fn snapshot(fixture: &Fixture) -> CompletionPromptSnapshot {
    fixture
        .store
        .investigation_completion_prompt_snapshot(&fixture.id)
        .unwrap()
}

fn assert_matches_old_reads(fixture: &Fixture) -> CompletionPromptSnapshot {
    let snapshot = snapshot(fixture);
    assert_eq!(
        snapshot.detail(),
        &fixture.store.investigation(&fixture.id).unwrap()
    );
    assert_eq!(
        snapshot.ledger(),
        &fixture
            .store
            .investigation_ledger(&fixture.id)
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        snapshot.attempts(),
        fixture
            .store
            .investigation_completion_attempts(&fixture.id)
            .unwrap()
    );
    assert_eq!(
        snapshot.attempt_ledgers(),
        fixture
            .store
            .investigation_completion_attempt_ledgers(&fixture.id)
            .unwrap()
    );
    assert_eq!(
        snapshot.metadata(),
        &fixture
            .store
            .investigation_completion_metadata(&fixture.id)
            .unwrap()
    );
    assert_eq!(
        snapshot.basis(),
        &fixture.store.investigation_turn_basis(&fixture.id).unwrap()
    );
    assert_eq!(snapshot.attempts().len(), snapshot.attempt_ledgers().len());
    for (record, ledger) in snapshot.attempts().iter().zip(snapshot.attempt_ledgers()) {
        let policy = InvestigationPolicy::for_limits(&snapshot.detail().limits).unwrap();
        assert_eq!(record.input_revision, ledger.revision);
        assert_eq!(
            record.input_ledger_hash,
            ledger.fingerprint_with_policy(policy).unwrap()
        );
    }
    snapshot
}

fn complete_inventory_read(fixture: &mut Fixture, unavailable: bool) {
    let (_, step) = fixture.step();
    let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0] else {
        unreachable!()
    };
    let request = InvestigationCoverageRead {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: if unavailable { 1 } else { 32768 },
    };
    let read = ReadIntent::Inventory {
        context_id: request.context_id.clone(),
        repo: request.repo.clone(),
        inventory_hash: request.inventory_hash.clone(),
        max_bytes: request.max_bytes,
    };
    fixture
        .advance(InvestigationTransition::ActionAdmitted {
            response: Fixture::response(&step, staged::read_action_hash(&read).unwrap()),
            tool: Some(InvestigationTool::ReadCoverageInventory),
            result: None,
        })
        .unwrap();
    fixture
        .advance(InvestigationTransition::BeginTool {
            tool: InvestigationTool::ReadCoverageInventory,
        })
        .unwrap();
    if unavailable {
        fixture
            .advance(InvestigationTransition::ReadUnavailable {
                read,
                reason: UnavailableReason::InventoryExceedsMaxBytes,
            })
            .unwrap();
    } else {
        let raw = serde_json::to_vec(&fixture.archive.read(&request).unwrap()).unwrap();
        let mut ledger = fixture
            .store
            .investigation_ledger(&fixture.id)
            .unwrap()
            .unwrap();
        ledger.revision += 1;
        ledger
            .coverage_reads
            .push(InvestigationCoverageReadManifest {
                request,
                response_hash: core_prov::content_hash(&raw),
                response_bytes: raw.len(),
            });
        let usage = fixture.store.investigation(&fixture.id).unwrap().usage;
        fixture
            .advance(InvestigationTransition::InputAdmitted {
                ledger: Box::new(ledger),
                usage,
            })
            .unwrap();
        fixture
            .inventory_details
            .push(String::from_utf8(raw).unwrap());
    }
}

#[test]
fn completion_prompt_snapshot_matches_historical_reads_and_preserves_attempt_ledger_order() {
    // AC-0287: every published completion family retains its exact read semantics,
    // including successful acquisition and unavailable reads without a revision.
    for (policy, progressive) in [
        (&INVESTIGATION_V1_POLICY, false),
        (&INVESTIGATION_V2_POLICY, false),
        (&INVESTIGATION_V2_POLICY, true),
    ] {
        let mut fixture =
            Fixture::with_family(InvestigationProviderMode::Local, policy, progressive);
        let initial = assert_matches_old_reads(&fixture);
        assert_eq!(initial.ledger().revision, 0);
        assert!(initial.attempts().is_empty());
        fixture.refuse();
        assert_eq!(
            assert_matches_old_reads(&fixture).metadata().refusal_count,
            1
        );
        complete_inventory_read(&mut fixture, false);
        let acquired = assert_matches_old_reads(&fixture);
        assert_eq!(acquired.ledger().revision, 1);
        assert_eq!(acquired.attempts().len(), 1);
        assert_eq!(
            acquired.basis().previous_ledger.as_ref().unwrap().revision,
            0
        );
        assert_eq!(
            acquired.basis().completed_tool,
            Some(InvestigationTool::ReadCoverageInventory)
        );
        complete_inventory_read(&mut fixture, true);
        let unavailable = assert_matches_old_reads(&fixture);
        assert_eq!(unavailable.ledger().revision, 1);
        assert_eq!(unavailable.attempts().len(), 2);
        assert!(matches!(
            unavailable.attempts()[1].outcome,
            AttemptOutcome::Unavailable { .. }
        ));
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        let reopened = reopened
            .investigation_completion_prompt_snapshot(&fixture.id)
            .unwrap();
        assert_eq!(reopened.detail(), unavailable.detail());
        assert_eq!(reopened.attempts(), unavailable.attempts());
        assert_eq!(reopened.metadata(), unavailable.metadata());
        assert_eq!(reopened.basis(), unavailable.basis());
    }
}

#[test]
fn completion_prompt_snapshot_is_read_only_and_owns_its_saved_revision() {
    // AC-0287: taking a snapshot performs no writes and does not make a later
    // stale snapshot into permission to continue work.
    let mut fixture = Fixture::new();
    let changes = fixture.store.conn.total_changes();
    fixture
        .store
        .conn
        .pragma_update(None, "query_only", true)
        .unwrap();
    let saved = snapshot(&fixture);
    assert_eq!(fixture.store.conn.total_changes(), changes);
    fixture
        .store
        .conn
        .pragma_update(None, "query_only", false)
        .unwrap();
    fixture.refuse();
    let next = snapshot(&fixture);
    assert!(next.detail().summary.revision > saved.detail().summary.revision);
    assert_eq!(saved.detail().usage.model_invocations, 0);
    assert_eq!(saved.metadata().refusal_count, 0);
    assert_eq!(next.detail().usage.model_invocations, 1);
    assert_eq!(next.metadata().refusal_count, 1);
}

#[test]
fn completion_prompt_snapshot_rejects_rehashed_saved_authority_corruption() {
    // AC-0287: an internally consistent body checksum cannot replace request,
    // ledger, policy/revision or namespace authority.
    for case in ["request", "ledger", "usage", "revision", "namespace"] {
        let fixture = Fixture::new();
        if case == "namespace" {
            fixture
                .store
                .conn
                .execute(
                    "UPDATE investigation_meta SET namespace=?1 WHERE singleton=1",
                    ["a".repeat(32)],
                )
                .unwrap();
        } else {
            let tx = fixture.store.conn.unchecked_transaction().unwrap();
            let mut task = load_task(&tx, &fixture.id).unwrap();
            tx.commit().unwrap();
            match case {
                "request" => task.completion_intent.as_mut().unwrap().request_hash = "a".repeat(64),
                "ledger" => task.ledger_hash = Some("b".repeat(64)),
                "usage" => {
                    task.detail.usage.model_invocations = task.detail.limits.model_invocations + 1
                }
                "revision" => task.detail.summary.revision += 1,
                _ => unreachable!(),
            }
            let bytes = serde_json::to_vec(&task).unwrap();
            fixture
                .store
                .conn
                .execute(
                    "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
                    params![bytes, core_prov::content_hash(&bytes), fixture.id],
                )
                .unwrap();
        }
        let changes = fixture.store.conn.total_changes();
        assert!(
            fixture
                .store
                .investigation_completion_prompt_snapshot(&fixture.id)
                .is_err(),
            "{case}"
        );
        assert_eq!(fixture.store.conn.total_changes(), changes);
    }
}

#[test]
fn completion_prompt_snapshot_rejects_rehashed_foreign_attempt_authority() {
    // AC-0287: returned attempts are authenticated against the saved intent and
    // exact ledger, rather than merely returned alongside a valid task.
    let mut fixture = Fixture::new();
    complete_inventory_read(&mut fixture, true);
    let connection = &fixture.store.conn;
    let mut entry: AttemptEntry = read_body(
        connection,
        "completion_attempts",
        "sequence",
        &fixture.id,
        &1i64,
    )
    .unwrap()
    .unwrap();
    entry.record.request_hash = "c".repeat(64);
    entry.record.attempt_id = entry.record.content_id().unwrap();
    let bytes = serde_json::to_vec(&entry).unwrap();
    connection
        .execute_batch("DROP TRIGGER completion_attempts_no_update")
        .unwrap();
    connection
        .execute(
            "UPDATE completion_attempts SET body=?1,body_hash=?2 WHERE task_id=?3 AND sequence=1",
            params![bytes, core_prov::content_hash(&bytes), fixture.id],
        )
        .unwrap();
    connection.execute_batch(ATTEMPT_SCHEMA[1]).unwrap();
    assert!(
        fixture
            .store
            .investigation_completion_prompt_snapshot(&fixture.id)
            .is_err()
    );
}

#[test]
fn completion_prompt_snapshot_keeps_one_sqlite_view_across_another_committed_transition() {
    // AC-0287: a concurrent commit cannot mix an old task/revision/usage with
    // new refusal metadata. This pins the same transaction/helper used by the API.
    let mut fixture = Fixture::new();
    let (input, step) = fixture.step();
    let mut writer = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let before = snapshot(&fixture);
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    validate(&tx, &fixture.store.namespace).unwrap();
    let task = load_task(&tx, &fixture.id).unwrap();
    let finish = Fixture::finish();
    let response = Fixture::response(
        &step,
        core_prov::content_hash(
            &serde_json::to_vec(&serde_json::to_value(&finish).unwrap()).unwrap(),
        ),
    );
    writer
        .advance_investigation(
            &fixture.execution,
            before.detail().summary.revision,
            InvestigationTransition::FinishRefused {
                request: Box::new(fixture.request.clone()),
                response,
                finish: Box::new(finish),
                input: Box::new(input),
            },
        )
        .unwrap();
    let pinned = prompt_snapshot(&tx, task).unwrap();
    assert_eq!(pinned.detail(), before.detail());
    assert_eq!(pinned.metadata(), before.metadata());
    assert_eq!(pinned.basis(), before.basis());
    tx.commit().unwrap();
    let after = writer
        .investigation_completion_prompt_snapshot(&fixture.id)
        .unwrap();
    assert!(after.detail().summary.revision > pinned.detail().summary.revision);
    assert_eq!(pinned.metadata().refusal_count, 0);
    assert_eq!(after.metadata().refusal_count, 1);
}
