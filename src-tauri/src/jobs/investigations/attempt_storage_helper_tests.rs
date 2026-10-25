use super::*;

fn begin_read(fixture: &mut Fixture) -> (String, ReadIntent) {
    let (_, step) = fixture.step();
    let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0] else {
        unreachable!()
    };
    let read = ReadIntent::Inventory {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: 1,
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
    (step.step_id, read)
}
fn attempt_bytes(connection: &Connection, id: &str) -> (Vec<u8>, String) {
    connection
        .query_row(
            "SELECT body,body_hash FROM completion_attempts WHERE task_id=?1 AND sequence=1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn exact_step_attempt_helper_preserves_committed_ids_bytes_and_caller_rollback() {
    // AC-0287 groundwork: explicit-step writes reuse saved response/read authority
    // and exactly match the historical latest-step wrapper after real commit.
    for (policy, progressive) in [
        (&INVESTIGATION_V1_POLICY, false),
        (&INVESTIGATION_V2_POLICY, false),
        (&INVESTIGATION_V2_POLICY, true),
    ] {
        let mut fixture =
            Fixture::with_family(InvestigationProviderMode::Local, policy, progressive);
        let (step, read) = begin_read(&mut fixture);
        let reason = UnavailableReason::InventoryExceedsMaxBytes;
        let expected = {
            let tx = fixture.store.conn.transaction().unwrap();
            let mut task = load_task(&tx, &fixture.id).unwrap();
            // Mirror the existing caller's genuine unavailable event before its
            // append. These partial rows remain private to this rolled-back tx.
            task.phase = Phase::Ready;
            task.action_tool = None;
            bump(&tx, &mut task).unwrap();
            event(
                &tx,
                &mut task,
                InvestigationEventKind::ReadUnavailable,
                Some(&step),
                Some(InvestigationTool::ReadCoverageInventory),
                unavailable_message(&reason),
            )
            .unwrap();
            let revision = task.ledger_revision.unwrap();
            append_attempt_at_step(
                &tx,
                &task,
                &step,
                &read,
                AttemptOutcome::Unavailable {
                    reason: reason.clone(),
                },
                revision,
            )
            .unwrap();
            let entries = attempts(&tx, &task).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].record.step_id, step);
            #[cfg(unix)]
            {
                // AC-0327: replay retains the attempt set while restoring the
                // real archive, even though the one-byte inventory read refuses.
                use crate::jobs::investigations::coverage;
                let archive = coverage::load_for_task(&tx, &task).unwrap().unwrap();
                let peak = next_prompt_attempt_replay_workspace().unwrap();
                let (retained, _) = InvestigationCoverageArchive::decode_workspace_bytes().unwrap();
                let (_, read_peak) = InvestigationCoverageArchive::read_workspace_bytes().unwrap();
                assert!(
                    peak >= next_prompt_attempt_acquisition_workspace().unwrap()
                        + coverage::archive_acquisition_workspace().unwrap()
                        + retained
                        + read_peak
                );
                assert!(retained >= serde_json::to_vec(&archive).unwrap().len());
                let schema_bytes: i64 = tx.query_row(
                    "SELECT sum(length(CAST(sql AS BLOB))) FROM sqlite_schema WHERE name GLOB 'completion_*'",
                    [], |row| row.get(0),
                ).unwrap();
                assert!(
                    attempt_schema_workspace().unwrap() >= usize::try_from(schema_bytes).unwrap()
                );
            }
            // AC-0327 (#969): exercise the actual bounded record decoder at
            // both buffered tags, without changing immutable production rows.
            tx.execute_batch("CREATE TEMP TABLE decoder_fixture (task_id TEXT, sequence INTEGER, body BLOB, body_hash TEXT)").unwrap();
            let original = serde_json::to_value(&entries[0]).unwrap();
            for pointer in ["/record/read/context_id", "/record/read/fact/id"] {
                let mut value = original.clone();
                if pointer.ends_with("fact/id") {
                    value["record"]["read"] = serde_json::json!({
                        "kind": "source", "fact": {"kind": "node", "id": "node:1"},
                        "role": "provenance", "index": 0
                    });
                }
                // Decode-only control: the source variant need not match the
                // stored inventory binding; attempts() validates that separately.
                let control: AttemptEntry = serde_json::from_value(value.clone()).unwrap();
                for malformed in [false, true] {
                    let mut candidate = value.clone();
                    if malformed {
                        let mut nested = serde_json::json!("not a string");
                        for _ in 0..48 {
                            nested = serde_json::json!([nested]);
                        }
                        *candidate.pointer_mut(pointer).unwrap() = nested;
                    }
                    let body = if malformed {
                        serde_json::to_vec(&candidate).unwrap()
                    } else {
                        serde_json::to_vec(&control).unwrap()
                    };
                    tx.execute("DELETE FROM decoder_fixture", []).unwrap();
                    tx.execute(
                        "INSERT INTO decoder_fixture VALUES (?1,1,?2,?3)",
                        params![fixture.id, body, core_prov::content_hash(&body)],
                    )
                    .unwrap();
                    let decoded = read_body::<AttemptEntry>(
                        &tx,
                        "decoder_fixture",
                        "sequence",
                        &fixture.id,
                        &1,
                    );
                    if malformed {
                        assert!(matches!(decoded, Err(InvestigationStoreError::Invalid)));
                    } else {
                        assert_eq!(
                            serde_json::to_value(decoded.unwrap().unwrap()).unwrap(),
                            serde_json::to_value(&control).unwrap()
                        );
                    }
                }
            }
            let bytes = attempt_bytes(&tx, &fixture.id);
            assert_eq!(bytes.1, core_prov::content_hash(&bytes.0));
            let record = entries[0].record.clone();
            tx.rollback().unwrap();
            (bytes, record)
        };
        assert!(
            fixture
                .store
                .investigation_completion_attempts(&fixture.id)
                .unwrap()
                .is_empty()
        );
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert!(
            reopened
                .investigation_completion_attempts(&fixture.id)
                .unwrap()
                .is_empty()
        );
        drop(reopened);
        fixture
            .advance(InvestigationTransition::ReadUnavailable { read, reason })
            .unwrap();
        assert_eq!(attempt_bytes(&fixture.store.conn, &fixture.id), expected.0);
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(
            reopened
                .investigation_completion_attempts(&fixture.id)
                .unwrap(),
            vec![expected.1]
        );
    }
}

#[test]
fn exact_step_attempt_helper_rejects_orphan_step_and_changed_read() {
    // AC-0287 groundwork: an explicit mismatched step cannot be replaced with
    // latest, and even a rehashed response row cannot change the admitted read.
    let mut fixture = Fixture::new();
    let (step, read) = begin_read(&mut fixture);
    let tx = fixture.store.conn.transaction().unwrap();
    let task = load_task(&tx, &fixture.id).unwrap();
    let mut response: InvestigationResponseMeta = read_body(
        &tx,
        "investigation_responses",
        "step_id",
        &fixture.id,
        &step,
    )
    .unwrap()
    .unwrap();
    response.step_id = "orphan-step".into();
    insert_body(
        &tx,
        "investigation_responses",
        &fixture.id,
        &response.step_id,
        &response,
        4096,
    )
    .unwrap();
    let outcome = AttemptOutcome::Unavailable {
        reason: UnavailableReason::InventoryExceedsMaxBytes,
    };
    assert_eq!(
        append_attempt_at_step(&tx, &task, "orphan-step", &read, outcome.clone(), 0),
        Err(InvestigationStoreError::Invalid)
    );
    response.step_id = step.clone();
    response.action_hash = "a".repeat(64);
    let body = encode(&response, 4096).unwrap();
    assert!(tx.execute(
        "UPDATE investigation_responses SET body=?1,body_hash=?2 WHERE task_id=?3 AND step_id=?4",
        params![body, core_prov::content_hash(&body), fixture.id, step],
    ).is_err());
    let mut changed_read = read.clone();
    if let ReadIntent::Inventory { max_bytes, .. } = &mut changed_read {
        *max_bytes += 1;
    }
    assert_eq!(
        append_attempt_at_step(&tx, &task, &step, &changed_read, outcome, 0),
        Err(InvestigationStoreError::Invalid)
    );
    let rows: i64 = tx
        .query_row(
            "SELECT count(*) FROM completion_attempts WHERE task_id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    tx.rollback().unwrap();
    assert!(
        fixture
            .store
            .investigation_completion_attempts(&fixture.id)
            .unwrap()
            .is_empty()
    );
}
