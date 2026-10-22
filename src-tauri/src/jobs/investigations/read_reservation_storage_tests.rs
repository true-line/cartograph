//! Production namespace/capacity integration only; direct fixture rows below
//! are not runtime source admission, saved-action authority or owner-loss proof.
use super::*;
use crate::jobs::investigations::storage;

fn start(store: &mut JobStore, nonce: &str) -> String {
    let request = StartInvestigationRequest {
        schema_version: 1,
        request_nonce: nonce.into(),
        specialist_id: SpecialistId::DomainAnalyst,
        question: "Which retained evidence supports this design?".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: "investigation-v1".into(),
        expected_graph_revision: None,
        expected_context_id: None,
        conversation_id: None,
        parent_id: None,
        inspection_requirements: None,
    };
    let provider = InvestigationProvider {
        mode: InvestigationProviderMode::Local,
        provider_id: "local:fixture".into(),
        model: "fixture-model".into(),
        endpoint: "http://127.0.0.1:11434".into(),
        deployment: None,
        protocol_version: None,
        available: true,
        unavailable_reason: None,
    };
    store
        .start_investigation(&request, Some(&provider))
        .unwrap()
        .detail
        .summary
        .investigation_id
}
fn layout(conn: &Connection) -> Vec<(String, String, String)> {
    conn.prepare("SELECT name,type,sql FROM sqlite_schema WHERE (name GLOB 'investigation_*' OR (tbl_name GLOB 'investigation_*' AND name NOT GLOB 'sqlite_*')) AND name NOT GLOB 'investigation_read_*' AND tbl_name NOT GLOB 'investigation_read_*' ORDER BY name").unwrap()
        .query_map([],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap()
}
type SavedTaskRows = (Vec<u8>, String, Vec<(Vec<u8>, String)>);
fn saved(conn: &Connection, id: &str) -> SavedTaskRows {
    let task: (Vec<u8>, String) = conn
        .query_row(
            "SELECT record,record_hash FROM investigation_tasks WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let events = conn
        .prepare(
            "SELECT body,body_hash FROM investigation_events WHERE task_id=?1 ORDER BY sequence",
        )
        .unwrap()
        .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (task.0, task.1, events)
}

#[test]
fn read_reservation_production_schema_preserves_legacy_open_and_explicit_additive_reopen() {
    // AC-0287: old open has no read namespace, rewritten records or new charges.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut store = JobStore::open(&path).unwrap();
    let id = start(&mut store, "schema");
    let original_layout = layout(&store.conn);
    let original = saved(&store.conn, &id);
    let original_bytes = storage::stored_bytes(&store.conn, Some(&id)).unwrap();
    drop(store);
    let mut store = JobStore::open(&path).unwrap();
    assert_eq!(namespace_count(&store.conn).unwrap(), 0);
    assert_eq!(layout(&store.conn), original_layout);
    assert_eq!(saved(&store.conn, &id), original);
    assert_eq!(
        storage::stored_bytes(&store.conn, Some(&id)).unwrap(),
        original_bytes
    );
    // Registration is explicit and additive in the existing writer transaction.
    {
        let tx = store.conn.transaction().unwrap();
        storage::validate(&tx, &store.namespace).unwrap();
        initialize(&tx).unwrap();
        storage::validate(&tx, &store.namespace).unwrap();
        assert_eq!(
            storage::stored_bytes(&tx, Some(&id)).unwrap(),
            original_bytes
        );
        tx.commit().unwrap();
    }
    drop(store);
    let store = JobStore::open(&path).unwrap();
    assert_eq!(layout(&store.conn), original_layout);
    assert_eq!(saved(&store.conn, &id), original);
    assert!(namespace_count(&store.conn).unwrap() > 0);
    // Present schema/body/accounting reads require a coherent caller transaction.
    assert_eq!(
        storage::stored_bytes(&store.conn, Some(&id)),
        Err(InvestigationStoreError::Invalid)
    );
    let tx = store.conn.unchecked_transaction().unwrap();
    storage::validate(&tx, &store.namespace).unwrap();
    assert_eq!(
        storage::stored_bytes(&tx, Some(&id)).unwrap(),
        original_bytes
    );
    tx.commit().unwrap();
    assert_eq!(
        store.investigation(&id).unwrap().summary.specialist_id,
        SpecialistId::DomainAnalyst
    );
}

#[test]
fn read_reservation_production_preserves_original_coverage_migration_and_rejects_bad_extensions() {
    // AC-0287: preserve the old exact legacy-to-coverage upgrade; optional read
    // tables do not become an alternate route around complete layout validation.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let mut store = JobStore::open(&path).unwrap();
    let id = start(&mut store, "legacy");
    let original = saved(&store.conn, &id);
    storage::version_one_fixture(&store.conn);
    for (name, kind, _) in crate::jobs::investigations::coverage::schema()
        .into_iter()
        .rev()
    {
        store
            .conn
            .execute_batch(&format!("DROP {} {name}", kind.to_uppercase()))
            .unwrap();
    }
    let legacy = layout(&store.conn);
    drop(store);
    let mut store = JobStore::open(&path).unwrap();
    assert_eq!(saved(&store.conn, &id), original);
    assert_eq!(namespace_count(&store.conn).unwrap(), 0);
    let migrated = layout(&store.conn);
    assert!(
        legacy
            .iter()
            .filter(|entry| entry.0 != "investigation_meta")
            .all(|entry| migrated.contains(entry))
    );
    {
        let tx = store.conn.transaction().unwrap();
        initialize(&tx).unwrap();
        tx.commit().unwrap();
    }
    drop(store);
    // Unknown version with otherwise canonical DDL fails closed on real reopen.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA ignore_check_constraints=ON; UPDATE investigation_read_meta SET version=2",
    )
    .unwrap();
    drop(conn);
    assert!(JobStore::open(&path).is_err());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.db");
    let store = JobStore::open(&path).unwrap();
    store.conn.execute_batch(&schema()[0].2).unwrap();
    drop(store);
    assert!(JobStore::open(&path).is_err());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("foreign.db");
    let store = JobStore::open(&path).unwrap();
    store
        .conn
        .execute_batch("CREATE TABLE investigation_read_foreign (body BLOB) STRICT")
        .unwrap();
    drop(store);
    assert!(JobStore::open(&path).is_err());
}

#[test]
fn read_reservation_production_capacity_charges_claims_once_and_retains_unknown_accounting() {
    // AC-0287: all existing capacity callers see immutable rows and claims, not
    // only the private reservation insertion path. No public start is enabled.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capacity.db");
    let mut store = JobStore::open(&path).unwrap();
    let id = start(&mut store, "capacity");
    let original_task = storage::stored_bytes(&store.conn, Some(&id)).unwrap();
    let original_global = storage::stored_bytes(&store.conn, None).unwrap();
    let original_remaining = storage::remaining_task_capacity(&store.conn, &id).unwrap();
    let original_global_remaining = storage::remaining_store_capacity(&store.conn).unwrap();
    let r = reservation(&id, "step:capacity");
    {
        let tx = store.conn.transaction().unwrap();
        initialize(&tx).unwrap();
        insert_reservation(&tx, &r).unwrap();
        let row_bytes = stored_bytes(&tx, Some(&id)).unwrap();
        assert_eq!(
            storage::stored_bytes(&tx, Some(&id)).unwrap(),
            original_task + row_bytes + r.storage_claim.bytes(false).unwrap()
        );
        assert_eq!(
            storage::stored_bytes(&tx, None).unwrap(),
            original_global + row_bytes + 256 + r.storage_claim.bytes(false).unwrap()
        );
        assert_eq!(
            storage::remaining_task_capacity(&tx, &id).unwrap(),
            original_remaining - row_bytes - r.storage_claim.bytes(false).unwrap()
        );
        assert_eq!(
            storage::remaining_store_capacity(&tx).unwrap(),
            original_global_remaining - row_bytes - 256 - r.storage_claim.bytes(false).unwrap()
        );
        storage::check_task_capacity(&tx, &id, false).unwrap();
        tx.commit().unwrap();
    }
    // A separate ordinary start remains historical family1 and accounts for the
    // same pending claim, without eager source routing or source execution.
    let other = start(&mut store, "ordinary-writer");
    let tx = store.conn.transaction().unwrap();
    assert_eq!(pending_storage_claims(&tx, Some(&other)).unwrap(), 0);
    let before = storage::stored_bytes(&tx, None).unwrap();
    let projection = task_accounting(&tx, &id, true).unwrap();
    let d = Disposition::for_reservation(&r, None, DispositionKind::OutcomeUnknown).unwrap();
    insert_disposition(&tx, &id, "step:capacity", &d).unwrap();
    let disposition_bytes =
        encode(&d, MAX_MANIFEST_BYTES).unwrap().len() + 64 + id.len() + "step:capacity".len() + 256;
    assert_eq!(
        storage::stored_bytes(&tx, None).unwrap(),
        before + disposition_bytes - r.storage_claim.bytes(false).unwrap()
    );
    assert_eq!(pending_storage_claims(&tx, None).unwrap(), 0);
    assert_eq!(task_accounting(&tx, &id, false).unwrap(), projection);
    tx.commit().unwrap();
    drop(store);
    let store = JobStore::open(&path).unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    assert_eq!(pending_storage_claims(&tx, None).unwrap(), 0);
    assert_eq!(task_accounting(&tx, &id, false).unwrap(), projection);
}

#[test]
fn read_reservation_production_capacity_refusal_rolls_back_schema_rows_and_existing_writes() {
    // AC-0287: the caller's failed admission rolls back the additive migration,
    // reservation and unrelated rows together; no helper commits on its behalf.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollback.db");
    let mut store = JobStore::open(&path).unwrap();
    let id = start(&mut store, "rollback");
    let original = saved(&store.conn, &id);
    let original_layout = layout(&store.conn);
    let r = reservation(&id, "step:rollback");
    {
        let tx = store.conn.transaction().unwrap();
        initialize(&tx).unwrap();
        insert_reservation(&tx, &r).unwrap();
        let remaining = storage::remaining_task_capacity(&tx, &id).unwrap();
        tx.execute(
            "INSERT INTO investigation_inputs VALUES (?1,99,zeroblob(?2),?3)",
            params![id, i64::try_from(remaining).unwrap(), "0".repeat(64)],
        )
        .unwrap();
        assert_eq!(
            storage::check_task_capacity(&tx, &id, false),
            Err(InvestigationStoreError::Capacity)
        );
        tx.rollback().unwrap();
    }
    drop(store);
    let mut store = JobStore::open(&path).unwrap();
    assert_eq!(namespace_count(&store.conn).unwrap(), 0);
    assert_eq!(layout(&store.conn), original_layout);
    assert_eq!(saved(&store.conn, &id), original);
    // Once a reservation is committed, a later writer cannot spend its claim.
    {
        let tx = store.conn.transaction().unwrap();
        initialize(&tx).unwrap();
        insert_reservation(&tx, &r).unwrap();
        tx.commit().unwrap();
    }
    {
        let tx = store.conn.transaction().unwrap();
        let remaining = storage::remaining_task_capacity(&tx, &id).unwrap();
        tx.execute(
            "INSERT INTO investigation_inputs VALUES (?1,99,zeroblob(?2),?3)",
            params![id, i64::try_from(remaining).unwrap(), "0".repeat(64)],
        )
        .unwrap();
        assert_eq!(
            storage::check_task_capacity(&tx, &id, false),
            Err(InvestigationStoreError::Capacity)
        );
        tx.rollback().unwrap();
    }
    drop(store);
    let store = JobStore::open(&path).unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    assert_eq!(
        pending_storage_claims(&tx, Some(&id)).unwrap(),
        r.storage_claim.bytes(false).unwrap()
    );
    assert!(
        load(&tx, &id, "step:rollback")
            .unwrap()
            .unwrap()
            .disposition
            .is_none()
    );
    assert_eq!(saved(&tx, &id), original);
    let inputs: i64 = tx
        .query_row(
            "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(inputs, 0);
}

#[test]
fn read_reservation_production_additional_claim_does_not_count_terminal_reserve_twice() {
    // AC-0287: claim is additional to the unchanged terminal reserve. Occupancy
    // adds the claim once; the terminal capacity check retains exactly the old
    // TERMINAL_RESERVE difference, even with a pending source reservation.
    let dir = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(dir.path().join("state.db")).unwrap();
    let id = start(&mut store, "terminal-headroom");
    let r = reservation(&id, "step:reserve");
    let tx = store.conn.transaction().unwrap();
    initialize(&tx).unwrap();
    insert_reservation(&tx, &r).unwrap();
    let remaining = storage::remaining_task_capacity(&tx, &id).unwrap();
    tx.execute(
        "INSERT INTO investigation_inputs VALUES (?1,90,zeroblob(?2),?3)",
        params![id, i64::try_from(remaining - 320).unwrap(), "0".repeat(64)],
    )
    .unwrap();
    assert_eq!(storage::remaining_task_capacity(&tx, &id).unwrap(), 0);
    storage::check_task_capacity(&tx, &id, false).unwrap();
    tx.execute(
        "INSERT INTO investigation_inputs VALUES (?1,91,zeroblob(?2),?3)",
        params![
            id,
            i64::try_from(TERMINAL_RESERVE - 320).unwrap(),
            "0".repeat(64)
        ],
    )
    .unwrap();
    assert_eq!(
        storage::stored_bytes(&tx, Some(&id)).unwrap(),
        TASK_CAPACITY
    );
    storage::check_task_capacity(&tx, &id, true).unwrap();
    assert_eq!(
        storage::check_task_capacity(&tx, &id, false),
        Err(InvestigationStoreError::Capacity)
    );
    assert_eq!(
        pending_storage_claims(&tx, Some(&id)).unwrap(),
        r.storage_claim.bytes(false).unwrap()
    );
    tx.rollback().unwrap();
}

#[test]
fn read_reservation_production_task_global_capacity_share_preflight_snapshot() {
    // AC-0287: central capacity admission reuses the caller's coherent read
    // snapshot for both bounds and detects subsequent namespace writes.
    let dir = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(dir.path().join("state.db")).unwrap();
    let id = start(&mut store, "coherent-capacity");
    let tx = store.conn.transaction().unwrap();
    initialize(&tx).unwrap();
    let r = reservation(&id, "step:capacity");
    insert_reservation(&tx, &r).unwrap();
    let mut reads = ReadTransaction::new(&tx).unwrap();
    storage::check_task_capacity_with_reads(&tx, &id, false, &mut reads).unwrap();
    storage::check_task_capacity_with_reads(&tx, &id, true, &mut reads).unwrap();
    assert_eq!(reads.preflight_runs, 1);
    assert_eq!(reads.snapshot_runs, 1);
    assert_eq!(reads.validated_reads, 1);
    let old = storage::stored_bytes_with_reads(&tx, Some(&id), &mut reads).unwrap();
    let charge = ValidationCharge::for_reservation(&r).unwrap();
    reads.insert_charge(&id, "step:capacity", &charge).unwrap();
    let new = storage::stored_bytes_with_reads(&tx, Some(&id), &mut reads).unwrap();
    assert_eq!(
        new,
        old - r.storage_claim.validation_bytes
            + logical_row_bytes(&id, "step:capacity", &charge).unwrap()
    );
    assert_eq!(reads.preflight_runs, 2);
    assert_eq!(reads.snapshot_runs, 2);
    let other = Connection::open_in_memory().unwrap();
    assert_eq!(
        storage::stored_bytes_with_reads(&other, Some(&id), &mut reads),
        Err(InvestigationStoreError::Invalid)
    );
}

#[test]
fn read_reservation_event_writer_preserves_claims_at_normal_and_cancel_boundaries() {
    // AC-0287: exercise the actual event persistence guard independently of
    // task admission. Sparse event sequences are test-only boundary fixtures.
    for charged in [false, true] {
        for cancel in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = JobStore::open(dir.path().join("state.db")).unwrap();
            let id = start(&mut store, "event-claim-boundary");
            let tx = store.conn.transaction().unwrap();
            initialize(&tx).unwrap();
            let reservation = reservation(&id, "step:events");
            insert_reservation(&tx, &reservation).unwrap();
            if charged {
                insert_charge(
                    &tx,
                    &id,
                    "step:events",
                    &ValidationCharge::for_reservation(&reservation).unwrap(),
                )
                .unwrap();
            }
            let reserved = reservation.storage_claim.events(charged) as u64;
            let maximum = INVESTIGATION_V2_POLICY.journal_events() - if cancel { 2 } else { 3 };
            let mut event = InvestigationEvent {
                investigation_id: id.clone(),
                sequence: maximum - reserved + 1,
                revision: 1,
                kind: if cancel {
                    InvestigationEventKind::CancelRequested
                } else {
                    InvestigationEventKind::ActionAdmitted
                },
                step_id: Some("step:events".into()),
                tool: None,
                summary: "Reserved event headroom boundary.".into(),
                created_at: "2026-10-21T00:00:00Z".into(),
            };
            let before: i64 = tx
                .query_row(
                    "SELECT count(*) FROM investigation_events WHERE task_id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                storage::persist_event(&tx, &event),
                Err(InvestigationStoreError::Capacity)
            );
            let after: i64 = tx
                .query_row(
                    "SELECT count(*) FROM investigation_events WHERE task_id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(after, before);
            event.sequence -= 1;
            storage::persist_event(&tx, &event).unwrap();
            assert_eq!(pending_event_claims(&tx, &id).unwrap() as u64, reserved);
            tx.rollback().unwrap();
        }
    }
}
