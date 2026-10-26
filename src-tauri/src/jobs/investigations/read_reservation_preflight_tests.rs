use super::*;

#[test]
fn read_reservation_preflight_handle_reuses_one_coherent_snapshot() {
    // AC-0287: preflight once and hash each historical read once per unchanged
    // snapshot; task/global queries reuse scalar maps, not fresh body scans.
    let mut conn = connection();
    init(&mut conn);
    let first = reservation("task:1", "step:1");
    let second = reservation("task:2", "step:2");
    let tx = conn.transaction().unwrap();
    insert_reservation(&tx, &first).unwrap();
    insert_reservation(&tx, &second).unwrap();
    let charge = ValidationCharge::for_reservation(&second).unwrap();
    insert_charge(&tx, "task:2", "step:2", &charge).unwrap();
    let d = Disposition::for_reservation(&second, Some(&charge), DispositionKind::OutcomeUnknown)
        .unwrap();
    insert_disposition(&tx, "task:2", "step:2", &d).unwrap();
    let mut reads = ReadTransaction::new(&tx).unwrap();
    assert_eq!(reads.preflight_runs, 1);
    assert_eq!(reads.snapshot_runs, 0);
    assert_eq!(
        reads.load("task:1", "step:1").unwrap().unwrap().reservation,
        first
    );
    reads.load("task:2", "step:2").unwrap();
    let a = reads.capacity_charge(Some("task:1")).unwrap();
    let b = reads.capacity_charge(Some("task:2")).unwrap();
    let global = reads.capacity_charge(None).unwrap();
    assert_eq!(
        global.rows,
        a.rows + b.rows + READ_NAMESPACE_ALLOWANCE_BYTES
    );
    assert_eq!(global.claims, first.storage_claim.bytes(false).unwrap());
    assert_eq!(b.claims, 0);
    assert_eq!(reads.preflight_runs, 1);
    assert_eq!(reads.snapshot_runs, 1);
    assert_eq!(reads.validated_reads, 2);
    #[cfg(unix)]
    {
        // AC-0327: inspect the actual pending + settled snapshot, not row wire
        // alone. Map node currency is logical; this is not an allocator peak.
        let snapshot = reads.capacity.as_ref().unwrap();
        let actual = std::mem::size_of_val(snapshot)
            + context_hub::logical::map::<String, CapacityCharge>(snapshot.tasks.len()).unwrap()
            + snapshot.tasks.keys().map(String::len).sum::<usize>();
        let (retained, peak) = workspace::next_prompt_source_capacity_workspace().unwrap();
        assert!(retained >= actual);
        assert_eq!(snapshot.tasks.len(), 2);
        assert!(
            peak >= retained
                + workspace::next_prompt_read_reservation_workspace()
                    .unwrap()
                    .1
        );
    }

    for _ in 0..3 {
        assert_eq!(reads.capacity_charge(None).unwrap(), global);
    }
    assert_eq!(reads.snapshot_runs, 1);
    assert_eq!(reads.validated_reads, 2);
}

#[test]
fn read_reservation_preflight_handle_invalidates_after_each_controlled_namespace_write() {
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    let mut reads = ReadTransaction::new(&tx).unwrap();
    assert_eq!(
        reads.capacity_charge(None).unwrap(),
        CapacityCharge::default()
    );
    reads.initialize().unwrap();
    let empty = reads.capacity_charge(None).unwrap();
    assert_eq!(empty.rows, READ_NAMESPACE_ALLOWANCE_BYTES);
    let r = reservation("task:1", "step:1");
    reads.insert_reservation(&r).unwrap();
    let reserved = reads.capacity_charge(None).unwrap();
    assert_eq!(reserved.claims, r.storage_claim.bytes(false).unwrap());
    assert!(reserved.rows > empty.rows);
    let charge = ValidationCharge::for_reservation(&r).unwrap();
    reads.insert_charge("task:1", "step:1", &charge).unwrap();
    let charged = reads.capacity_charge(None).unwrap();
    assert_eq!(
        charged.claims,
        reserved.claims - r.storage_claim.validation_bytes
    );
    assert!(charged.rows > reserved.rows);
    let d =
        Disposition::for_reservation(&r, Some(&charge), DispositionKind::OutcomeUnknown).unwrap();
    reads.insert_disposition("task:1", "step:1", &d).unwrap();
    let settled = reads.capacity_charge(None).unwrap();
    assert_eq!(settled.claims, 0);
    assert!(settled.rows > charged.rows);
    assert_eq!(reads.preflight_runs, 4); // initialize, reservation, charge, disposition
    assert_eq!(reads.snapshot_runs, 5); // includes initial absent snapshot
    assert_eq!(
        reads
            .load("task:1", "step:1")
            .unwrap()
            .unwrap()
            .disposition
            .unwrap()
            .accounting,
        d.accounting
    );
}

#[test]
fn read_reservation_preflight_handle_detects_same_row_and_rolled_back_sql_mutations() {
    let mut conn = connection();
    init(&mut conn);
    let tx = conn.transaction().unwrap();
    let mut r = reservation("task:1", "step:1");
    insert_reservation(&tx, &r).unwrap();
    let mut reads = ReadTransaction::new(&tx).unwrap();
    let original = reads.capacity_charge(None).unwrap();
    r.storage_claim.settlement_bytes += 1;
    let changed = encode(&r, MAX_MANIFEST_BYTES).unwrap();
    replace_body(
        &tx,
        TABLES[0],
        "task:1",
        "step:1",
        &changed,
        &content_hash(&changed),
    );
    assert_eq!(
        reads.capacity_charge(None).unwrap().claims,
        original.claims + 1
    );
    assert_eq!(reads.preflight_runs, 2);
    assert_eq!(reads.snapshot_runs, 2);
    // No row-count or value change: total_changes still invalidates the cache.
    tx.execute("UPDATE investigation_read_meta SET version=version", [])
        .unwrap();
    reads.capacity_charge(None).unwrap();
    assert_eq!(reads.snapshot_runs, 3);
    // Even rolled-back writes may invalidate unnecessarily, which is safe.
    tx.execute_batch("SAVEPOINT bookkeeping; UPDATE investigation_read_meta SET version=version; ROLLBACK TO bookkeeping; RELEASE bookkeeping").unwrap();
    reads.capacity_charge(None).unwrap();
    assert_eq!(reads.snapshot_runs, 4);
    // DDL alone also invalidates, even though no SQL row counter changes.
    tx.execute_batch("CREATE TABLE unrelated_temporary_fixture (id INTEGER) STRICT")
        .unwrap();
    reads.capacity_charge(None).unwrap();
    assert_eq!(reads.snapshot_runs, 5);
    let mut malformed = r.clone();
    malformed.binding.original_action_hash = content_hash(b"wrong");
    let body = encode(&malformed, MAX_MANIFEST_BYTES).unwrap();
    replace_body(
        &tx,
        TABLES[0],
        "task:1",
        "step:1",
        &body,
        &content_hash(&body),
    );
    assert_eq!(
        reads.capacity_charge(None),
        Err(InvestigationStoreError::Invalid)
    );
    assert!(reads.capacity.is_none()); // no old successful snapshot survives
}

#[test]
fn read_reservation_preflight_fresh_transaction_revalidates_malformed_settlement_body() {
    // AC-0287: no cache crosses a transaction boundary, including equal row
    // count/key layouts and recomputed hashes of inconsistent settlement bodies.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh.db");
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE investigation_tasks (id TEXT PRIMARY KEY NOT NULL, record BLOB NOT NULL) STRICT; INSERT INTO investigation_tasks VALUES ('task:1',X'00');").unwrap();
    init(&mut conn);
    let r = reservation("task:1", "step:1");
    let d = Disposition::for_reservation(&r, None, DispositionKind::OutcomeUnknown).unwrap();
    {
        let tx = conn.transaction().unwrap();
        insert_reservation(&tx, &r).unwrap();
        insert_disposition(&tx, "task:1", "step:1", &d).unwrap();
        let mut reads = ReadTransaction::new(&tx).unwrap();
        assert_eq!(reads.capacity_charge(None).unwrap().claims, 0);
        drop(reads);
        tx.commit().unwrap();
    }
    let mut malformed = d.clone();
    malformed.accounting.reserved_unknown_tool_actions = 0;
    let body = encode(&malformed, MAX_MANIFEST_BYTES).unwrap();
    {
        let tx = conn.transaction().unwrap();
        replace_body(
            &tx,
            TABLES[2],
            "task:1",
            "step:1",
            &body,
            &content_hash(&body),
        );
        tx.commit().unwrap();
    }
    let tx = conn.transaction().unwrap();
    let mut reads = ReadTransaction::new(&tx).unwrap();
    assert_eq!(
        reads.capacity_charge(None),
        Err(InvestigationStoreError::Invalid)
    );
    assert_eq!(reads.preflight_runs, 1);
    assert_eq!(reads.snapshot_runs, 1);
}

#[test]
fn read_reservation_preflight_bounds_history_before_reading_any_body() {
    // Body count/type/length are legal individually, but aggregate task storage
    // already exceeds its fixed cap. Reject before deserializing invalid blobs.
    let mut conn = connection();
    init(&mut conn);
    let tx = conn.transaction().unwrap();
    for n in 0..17 {
        tx.execute(
            "INSERT INTO investigation_read_reservations VALUES ('task:1',?1,zeroblob(131072),?2)",
            params![format!("step:{n}"), "0".repeat(64)],
        )
        .unwrap();
    }
    assert!(matches!(
        ReadTransaction::new(&tx),
        Err(InvestigationStoreError::Capacity)
    ));
}
