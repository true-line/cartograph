use super::*;

fn row_counts(connection: &Connection, id: &str) -> (i64, i64, i64) {
    connection.query_row(
        "SELECT (SELECT count(*) FROM investigation_inputs WHERE task_id=?1), (SELECT count(*) FROM investigation_refs WHERE task_id=?1), (SELECT count(*) FROM investigation_events WHERE task_id=?1)",
        [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap()
}

#[test]
fn input_storage_helpers_preserve_bytes_receipt_order_and_caller_rollback() {
    // AC-0287 groundwork: split validation/persistence never commits or changes
    // the existing input wire; the normal transition keeps its finalization.
    for with_receipt in [false, true] {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start("input-helper", InvestigationProviderMode::Local);
        let candidate = ledger(with_receipt);
        let usage = InvestigationUsage {
            selected_facts: candidate.selected_facts.len(),
            ..fixture.store.investigation(&id).unwrap().usage
        };
        let expected_body = encode(&candidate, MAX_MANIFEST_BYTES).unwrap();
        let before = row_counts(&fixture.store.conn, &id);
        {
            let tx = fixture.store.conn.transaction().unwrap();
            let task = load_task(&tx, &id).unwrap();
            let task_before: Vec<u8> = tx
                .query_row(
                    "SELECT record FROM investigation_tasks WHERE id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .unwrap();
            let policy =
                transitions::validate_input_candidate(&tx, &task, &candidate, &usage).unwrap();
            assert_eq!(policy.profile(), INVESTIGATION_V1_POLICY.profile());
            assert_eq!(row_counts(&tx, &id), before);
            transitions::persist_input_rows(&tx, &task, &candidate).unwrap();
            let written = row_counts(&tx, &id);
            assert_eq!(written.0, before.0 + 1);
            assert_eq!(written.1, before.1 + i64::from(with_receipt));
            assert_eq!(written.2, before.2);
            let (body, body_hash):(Vec<u8>,String) = tx.query_row(
                "SELECT body,body_hash FROM investigation_inputs WHERE task_id=?1 AND revision=?2",
                params![id, signed(candidate.revision).unwrap()], |row| Ok((row.get(0)?,row.get(1)?)),
            ).unwrap();
            assert_eq!(body, expected_body);
            assert_eq!(body_hash, core_prov::content_hash(&expected_body));
            assert_eq!(
                tx.query_row(
                    "SELECT record FROM investigation_tasks WHERE id=?1",
                    [&id],
                    |row| row.get::<_, Vec<u8>>(0)
                )
                .unwrap(),
                task_before
            );
            assert!(task.ledger_revision.is_none());
            tx.rollback().unwrap();
        }
        assert_eq!(row_counts(&fixture.store.conn, &id), before);
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert!(reopened.investigation_ledger(&id).unwrap().is_none());
        drop(reopened);
        let revision = fixture.store.investigation(&id).unwrap().summary.revision;
        fixture
            .store
            .advance_investigation(
                &execution,
                revision,
                InvestigationTransition::InputPrepared {
                    coverage_archive: None,
                    ledger: Box::new(candidate.clone()),
                    usage,
                },
            )
            .unwrap();
        let committed: Vec<u8> = fixture
            .store
            .conn
            .query_row(
                "SELECT body FROM investigation_inputs WHERE task_id=?1 AND revision=?2",
                params![id, signed(candidate.revision).unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(committed, expected_body);
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation_ledger(&id).unwrap(), Some(candidate));
        assert_eq!(row_counts(&reopened.conn, &id).1, i64::from(with_receipt));
    }
}

#[test]
fn input_storage_validation_preserves_counter_and_prefix_rejections_without_writes() {
    // AC-0287 groundwork: preview cannot invent charged tool work or erase the
    // original acquired query prefix; all existing checks run before writes.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("input-helper-invalid", InvestigationProviderMode::Local);
    let previous = fixture.prepare(&id, &execution, true);
    let tx = fixture.store.conn.transaction().unwrap();
    let task = load_task(&tx, &id).unwrap();
    let before = row_counts(&tx, &id);
    let mut candidate = previous.clone();
    candidate.revision += 1;
    let mut usage = task.detail.usage.clone();
    usage.tool_actions += 1;
    assert!(transitions::validate_input_candidate(&tx, &task, &candidate, &usage).is_err());
    usage = task.detail.usage.clone();
    candidate.queries.clear();
    assert!(transitions::validate_input_candidate(&tx, &task, &candidate, &usage).is_err());
    assert_eq!(row_counts(&tx, &id), before);
    tx.rollback().unwrap();
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation_ledger(&id).unwrap(), Some(previous));
}
