use super::*;
use core_prov::content_hash;
use serde_json::json;

fn reservation(task: &str, step: &str) -> Reservation {
    let mut source: InvestigationInspectionTarget = serde_json::from_value(json!({
        "schema_version":1,"target_id":"","graph_snapshot_id":format!("context-v1:{}","a".repeat(64)),
        "fact":{"kind":"node","id":"node:checkout"},"fact_digest":format!("node-v1:{}","b".repeat(64)),
        "registered_source_id":format!("src_{}","d".repeat(32)),"receipt_id":format!("ts-primary-v2:{}","c".repeat(64)),
        "receipt_inventory_index":3,"role":"provenance","index":0,
        "source":{"repo":"acme/api","path":"guard.ts","byte_start":0,"byte_end":12,"commit_sha":"a".repeat(40)},
        "captured":{"file":{"source_id":format!("src_{}","d".repeat(32)),"capture_id":format!("capture-v1:{}","e".repeat(64)),
        "path":"guard.ts","digest":content_hash(b"private text"),"byte_len":12},"byte_start":0,"byte_end":12}
    })).unwrap();
    source.target_id = source.content_id().unwrap();
    let read = ReadIntent::Source {
        fact: source.fact.clone(),
        role: source.role,
        index: source.index,
    };
    let r = Reservation {
        schema_version: 2,
        binding: ReadBinding {
            task_id: task.into(),
            step_id: step.into(),
            request_hash: content_hash(b"request"),
            specialist_id: SpecialistId::DomainAnalystV10,
            policy_profile: "investigation-v2".into(),
            effective_limits: None,
            execution_job: 1,
            execution_generation: 1,
            owner_basis_hash: content_hash(b"private-owner-basis"),
            original_response_hash: content_hash(b"saved-response"),
            original_action_hash: staged::read_action_hash(&read).unwrap(),
            resolved_read_hash: read.fingerprint().unwrap(),
            original_read: read,
            base_ledger_hash: content_hash(b"ledger"),
            base_ledger_revision: 0,
            context_id: format!("context-v2:{}", "f".repeat(64)),
            archive_hash: content_hash(b"archive"),
            base_usage: InvestigationUsage {
                model_invocations: 1,
                generated_token_reservations: 2048,
                ..InvestigationUsage::default()
            },
            prior_completion_hash: content_hash(b"completion"),
            certification_basis_hash: content_hash(b"future-certification-basis"),
        },
        source,
        storage_claim: read_reservations::StorageClaim {
            validation_bytes: 8 * 1024,
            settlement_bytes: 24 * 1024,
            validation_events: 1,
            settlement_events: 2,
            settlement_attempts: 1,
        },
    };
    r.validate().unwrap();
    r
}
fn connection() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE investigation_tasks (id TEXT PRIMARY KEY NOT NULL, record BLOB NOT NULL) STRICT; INSERT INTO investigation_tasks VALUES ('task:1',X'00'),('task:2',X'ff');").unwrap();
    conn
}
fn init(conn: &mut Connection) {
    let tx = conn.transaction().unwrap();
    initialize(&tx).unwrap();
    tx.commit().unwrap();
}
fn replace_body(conn: &Connection, table: &str, task: &str, step: &str, body: &[u8], hash: &str) {
    let trigger = format!("{table}_no_update");
    conn.execute_batch(&format!("DROP TRIGGER {trigger}"))
        .unwrap();
    conn.execute(
        &format!("UPDATE {table} SET body=?3,body_hash=?4 WHERE task_id=?1 AND step_id=?2"),
        params![task, step, body, hash],
    )
    .unwrap();
    let (_, _, sql) = schema()
        .into_iter()
        .find(|(name, _, _)| name == &trigger)
        .unwrap();
    conn.execute_batch(&sql).unwrap();
}

#[test]
fn read_reservation_schema_is_additive_transactional_and_fails_closed() {
    // AC-0287 foundation: explicit namespace migration never rewrites legacy rows.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE investigation_tasks (id TEXT PRIMARY KEY NOT NULL, record BLOB NOT NULL) STRICT; INSERT INTO investigation_tasks VALUES ('task:1',X'0001ff');").unwrap();
    let legacy: (String, Vec<u8>) = conn.query_row("SELECT sql,record FROM sqlite_schema,investigation_tasks WHERE name='investigation_tasks'", [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    {
        let tx = conn.transaction().unwrap();
        initialize(&tx).unwrap();
        tx.rollback().unwrap();
    }
    assert_eq!(namespace_count(&conn).unwrap(), 0);
    init(&mut conn);
    drop(conn);
    let mut conn = Connection::open(&path).unwrap();
    let tx = conn.transaction().unwrap();
    initialize(&tx).unwrap(); // repeat initialization is read-only
    let after: (String, Vec<u8>) = tx.query_row("SELECT sql,record FROM sqlite_schema,investigation_tasks WHERE name='investigation_tasks'", [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(legacy, after);
    assert_eq!(stored_bytes(&tx, None).unwrap(), 256);
    assert_eq!(pending_storage_claims(&tx, None).unwrap(), 0);
    tx.execute_batch(
        "PRAGMA ignore_check_constraints=ON; UPDATE investigation_read_meta SET version=2",
    )
    .unwrap();
    assert_eq!(initialize(&tx), Err(InvestigationStoreError::Invalid));
    tx.rollback().unwrap();
    assert_eq!(initialize(&conn), Err(InvestigationStoreError::Invalid)); // no caller transaction
    let tx = conn.transaction().unwrap();
    tx.execute_batch("CREATE TABLE investigation_read_foreign (body BLOB) STRICT")
        .unwrap();
    assert_eq!(initialize(&tx), Err(InvestigationStoreError::Invalid));
    tx.rollback().unwrap();
    let mut partial = connection();
    let tx = partial.transaction().unwrap();
    tx.execute_batch(&schema()[0].2).unwrap();
    assert_eq!(initialize(&tx), Err(InvestigationStoreError::Invalid));
}

#[test]
fn read_reservation_rows_rollback_reopen_and_reject_duplicate_replacement() {
    // AC-0287: these helpers do not own COMMIT or acknowledge partial state.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE investigation_tasks (id TEXT PRIMARY KEY NOT NULL, record BLOB NOT NULL) STRICT; INSERT INTO investigation_tasks VALUES ('task:1',X'00');").unwrap();
    init(&mut conn);
    let r = reservation("task:1", "step:1");
    let charge = ValidationCharge::for_reservation(&r).unwrap();
    let acquired =
        Disposition::for_reservation(&r, Some(&charge), DispositionKind::Acquired).unwrap();
    {
        let tx = conn.transaction().unwrap();
        insert_reservation(&tx, &r).unwrap();
        insert_charge(&tx, "task:1", "step:1", &charge).unwrap();
        insert_disposition(&tx, "task:1", "step:1", &acquired).unwrap();
        tx.rollback().unwrap();
    }
    {
        let tx = conn.transaction().unwrap();
        assert!(load(&tx, "task:1", "step:1").unwrap().is_none());
        tx.commit().unwrap();
    }
    #[cfg(unix)]
    {
        // AC-0327: all three real immutable records can be retained together.
        let bytes = serde_json::to_vec(&(&r, &charge, &acquired)).unwrap().len();
        let (retained, peak) = workspace::next_prompt_read_reservation_workspace().unwrap();
        assert!(retained >= bytes && peak >= retained);
    }
    let body = encode(&r, MAX_MANIFEST_BYTES).unwrap();
    {
        let tx = conn.transaction().unwrap();
        insert_reservation(&tx, &r).unwrap();
        assert_eq!(
            insert_reservation(&tx, &r),
            Err(InvestigationStoreError::Conflict)
        );
        assert_eq!(
            insert_reservation(&tx, &reservation("task:1", "step:2")),
            Err(InvestigationStoreError::Conflict)
        );
        insert_charge(&tx, "task:1", "step:1", &charge).unwrap();
        assert_eq!(
            insert_charge(&tx, "task:1", "step:1", &charge),
            Err(InvestigationStoreError::Conflict)
        );
        tx.commit().unwrap();
    }
    drop(conn);
    let mut conn = Connection::open(&path).unwrap();
    let tx = conn.transaction().unwrap();
    let saved = load(&tx, "task:1", "step:1").unwrap().unwrap();
    assert_eq!(saved.reservation, r);
    assert_eq!(saved.charge, Some(charge.clone()));
    assert!(load(&tx, "task:1", "step:foreign").unwrap().is_none());
    let raw: (Vec<u8>, String) = tx
        .query_row(
            "SELECT body,body_hash FROM investigation_read_reservations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(raw, (body.clone(), content_hash(&body)));
    assert_eq!(
        pending_storage_claims(&tx, Some("task:1")).unwrap(),
        r.storage_claim.bytes(true).unwrap()
    );
    insert_disposition(&tx, "task:1", "step:1", &acquired).unwrap();
    assert_eq!(
        insert_disposition(&tx, "task:1", "step:1", &acquired),
        Err(InvestigationStoreError::Conflict)
    );
    assert_eq!(
        insert_charge(&tx, "task:1", "step:1", &charge),
        Err(InvestigationStoreError::Conflict)
    );
    assert!(
        tx.execute("UPDATE investigation_read_reservations SET body=body", [])
            .is_err()
    );
    assert!(
        tx.execute("DELETE FROM investigation_read_dispositions", [])
            .is_err()
    );
    assert_eq!(pending_storage_claims(&tx, None).unwrap(), 0);
    let accounting = task_accounting(&tx, "task:1", false).unwrap();
    assert_eq!(accounting.executed_tool_actions, 1);
    assert_eq!(accounting.validation_bytes, 12);
    tx.commit().unwrap();
}

#[test]
fn read_reservation_unknown_settlement_retains_identical_unknown_debits() {
    // AC-0287 / Claude175500: settling an orphan frees only storage claim/live
    // slot authority (the latter is not implemented here), never tool slots.
    for charged in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unknown.db");
        let mut conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE investigation_tasks (id TEXT PRIMARY KEY NOT NULL, record BLOB NOT NULL) STRICT; INSERT INTO investigation_tasks VALUES ('task:1',X'00');").unwrap();
        init(&mut conn);
        let r = reservation("task:1", "step:1");
        let charge = charged.then(|| ValidationCharge::for_reservation(&r).unwrap());
        let projected;
        {
            let tx = conn.transaction().unwrap();
            insert_reservation(&tx, &r).unwrap();
            if let Some(c) = &charge {
                insert_charge(&tx, "task:1", "step:1", c).unwrap();
            }
            projected = task_accounting(&tx, "task:1", true).unwrap();
            assert_eq!(projected.reserved_unknown_tool_actions, 1);
            assert_eq!(projected.reserved_unknown_evidence_requests, 1);
            assert_eq!(projected.executed_tool_actions, 0);
            assert_eq!(projected.reserved_tool_actions, 0);
            assert_eq!(projected.validation_bytes, if charged { 12 } else { 0 });
            assert_eq!(
                pending_storage_claims(&tx, None).unwrap(),
                r.storage_claim.bytes(charged).unwrap()
            );
            let before = stored_bytes(&tx, None).unwrap();
            let unknown =
                Disposition::for_reservation(&r, charge.as_ref(), DispositionKind::OutcomeUnknown)
                    .unwrap();
            assert_eq!(unknown.accounting, projected);
            insert_disposition(&tx, "task:1", "step:1", &unknown).unwrap();
            assert_eq!(task_accounting(&tx, "task:1", false).unwrap(), projected);
            assert_eq!(pending_storage_claims(&tx, None).unwrap(), 0);
            assert!(stored_bytes(&tx, None).unwrap() > before); // audit rows remain
            tx.commit().unwrap();
        }
        drop(conn);
        let mut conn = Connection::open(&path).unwrap();
        let tx = conn.transaction().unwrap();
        assert_eq!(task_accounting(&tx, "task:1", false).unwrap(), projected);
        assert_eq!(pending_storage_claims(&tx, None).unwrap(), 0);
        let saved = load(&tx, "task:1", "step:1").unwrap().unwrap();
        assert_eq!(saved.charge, charge);
    }
}

#[test]
fn read_reservation_actual_read_defects_charge_but_released_before_read_does_not() {
    // AC-0287: preserve real validation charges independently of tool outcome.
    let r = reservation("task:1", "step:1");
    let c = ValidationCharge::for_reservation(&r).unwrap();
    for kind in [
        DispositionKind::Acquired,
        DispositionKind::ReadUnavailable,
        DispositionKind::KnownReadDiscarded,
        DispositionKind::BoundInvariantDefect,
    ] {
        assert_eq!(
            Disposition::for_reservation(&r, None, kind),
            Err(InvestigationStoreError::Invalid)
        );
        let d = Disposition::for_reservation(&r, Some(&c), kind).unwrap();
        let a = project_accounting(&r, Some(&c), Some(&d), true).unwrap();
        assert_eq!(a.executed_tool_actions, 1);
        assert_eq!(a.executed_evidence_requests, 1);
        assert_eq!(a.reserved_unknown_tool_actions, 0);
        assert_eq!(a.validation_bytes, 12);
    }
    for charge in [None, Some(&c)] {
        let d =
            Disposition::for_reservation(&r, charge, DispositionKind::ReleasedBeforeRead).unwrap();
        let a = project_accounting(&r, charge, Some(&d), true).unwrap();
        assert_eq!(a.executed_tool_actions, 0);
        assert_eq!(a.reserved_tool_actions, 0);
        assert_eq!(a.reserved_unknown_evidence_requests, 0);
        assert_eq!(a.validation_bytes, charge.map_or(0, |_| 12));
    }
}

#[test]
fn read_reservation_identity_shape_capacity_and_charge_binding_reject_tampering() {
    let r = reservation("task:1", "step:1");
    let fingerprint = r.fingerprint().unwrap();
    let mut changed = r.clone();
    changed.binding.original_response_hash = content_hash(b"other-response");
    assert_ne!(changed.fingerprint().unwrap(), fingerprint); // saved authority checked by future caller
    for change in [
        |r: &mut Reservation| r.schema_version = 1,
        |r: &mut Reservation| r.binding.specialist_id = SpecialistId::DomainAnalystV9,
        |r: &mut Reservation| r.binding.policy_profile = "investigation-v1".into(),
        |r: &mut Reservation| r.binding.original_action_hash = content_hash(b"wrong-action"),
        |r: &mut Reservation| r.binding.resolved_read_hash = content_hash(b"wrong-read"),
        |r: &mut Reservation| r.binding.archive_hash = "X".repeat(64),
        |r: &mut Reservation| r.binding.context_id = "context-v2:invalid".into(),
        |r: &mut Reservation| r.binding.execution_job = 0,
        |r: &mut Reservation| r.storage_claim.validation_bytes = 0,
        |r: &mut Reservation| r.storage_claim.validation_bytes = TASK_CAPACITY + 1,
        |r: &mut Reservation| r.source.captured.file.byte_len += 1,
    ] {
        let mut bad = r.clone();
        change(&mut bad);
        assert_eq!(bad.validate(), Err(InvestigationStoreError::Invalid));
    }
    let mut capacity = r.clone();
    capacity.binding.base_usage.tool_actions = INVESTIGATION_V2_POLICY.limits().tool_actions;
    assert_eq!(capacity.validate(), Err(InvestigationStoreError::Capacity));
    capacity = r.clone();
    capacity.binding.base_usage.evidence_requests =
        INVESTIGATION_V2_POLICY.limits().evidence_requests;
    assert_eq!(capacity.validate(), Err(InvestigationStoreError::Capacity));
    capacity = r.clone();
    capacity.binding.base_usage.captured_validation_bytes =
        INVESTIGATION_V2_POLICY.limits().captured_validation_bytes - 11;
    assert_eq!(capacity.validate(), Err(InvestigationStoreError::Capacity));
    let c = ValidationCharge::for_reservation(&r).unwrap();
    let mut bad = c.clone();
    bad.validation_bytes -= 1;
    assert_eq!(bad.validate(&r), Err(InvestigationStoreError::Invalid));
    bad = c.clone();
    bad.reservation_hash = content_hash(b"other");
    assert_eq!(bad.validate(&r), Err(InvestigationStoreError::Invalid));
    bad = c.clone();
    bad.source_descriptor_hash = content_hash(b"other");
    assert_eq!(bad.validate(&r), Err(InvestigationStoreError::Invalid));
    let mut d =
        Disposition::for_reservation(&r, Some(&c), DispositionKind::OutcomeUnknown).unwrap();
    d.accounting.reserved_unknown_tool_actions = 0;
    assert_eq!(
        d.validate(&r, Some(&c)),
        Err(InvestigationStoreError::Invalid)
    );
}

#[test]
fn read_reservation_corrupt_rows_are_bounded_and_never_release_unvalidated_claims() {
    // AC-0287: even a present settlement row cannot release capacity until its
    // body, hash, exact reservation/charge binding and accounting all validate.
    let mut conn = connection();
    init(&mut conn);
    let r = reservation("task:1", "step:1");
    let tx = conn.transaction().unwrap();
    insert_reservation(&tx, &r).unwrap();
    let body = encode(&r, MAX_MANIFEST_BYTES).unwrap();
    replace_body(
        &tx,
        TABLES[0],
        "task:1",
        "step:1",
        &body,
        &content_hash(b"wrong"),
    );
    assert_eq!(
        load(&tx, "task:1", "step:1"),
        Err(InvestigationStoreError::Invalid)
    );
    replace_body(
        &tx,
        TABLES[0],
        "task:1",
        "step:1",
        &body,
        &content_hash(&body),
    );
    let mut noncanonical = body.clone();
    noncanonical.push(b' ');
    replace_body(
        &tx,
        TABLES[0],
        "task:1",
        "step:1",
        &noncanonical,
        &content_hash(&noncanonical),
    );
    assert_eq!(
        pending_storage_claims(&tx, None),
        Err(InvestigationStoreError::Invalid)
    );
    // AC-0327: nested malformed Content in either tagged path still fails
    // closed; JSON nesting does not introduce recursive typed enum layers.
    for pointer in ["/binding/original_read/fact/id", "/source/fact/id"] {
        let mut value = serde_json::to_value(&r).unwrap();
        let mut nested = json!("not an id");
        for _ in 0..48 {
            nested = json!([nested]);
        }
        *value.pointer_mut(pointer).unwrap() = nested;
        let malformed = serde_json::to_vec(&value).unwrap();
        assert!(malformed.len() < MAX_MANIFEST_BYTES);
        replace_body(
            &tx,
            TABLES[0],
            "task:1",
            "step:1",
            &malformed,
            &content_hash(&malformed),
        );
        assert_eq!(
            load(&tx, "task:1", "step:1"),
            Err(InvestigationStoreError::Invalid)
        );
    }
    let mut value = serde_json::to_value(&r).unwrap();
    value["foreign"] = json!(true);
    let foreign = serde_json::to_vec(&value).unwrap();
    replace_body(
        &tx,
        TABLES[0],
        "task:1",
        "step:1",
        &foreign,
        &content_hash(&foreign),
    );
    assert_eq!(
        load(&tx, "task:1", "step:1"),
        Err(InvestigationStoreError::Invalid)
    );
    replace_body(
        &tx,
        TABLES[0],
        "task:1",
        "step:1",
        &body,
        &content_hash(&body),
    );
    let c = ValidationCharge::for_reservation(&r).unwrap();
    insert_charge(&tx, "task:1", "step:1", &c).unwrap();
    let mut d =
        Disposition::for_reservation(&r, Some(&c), DispositionKind::OutcomeUnknown).unwrap();
    insert_disposition(&tx, "task:1", "step:1", &d).unwrap();
    d.accounting.reserved_unknown_evidence_requests = 0;
    let corrupt = encode(&d, MAX_MANIFEST_BYTES).unwrap();
    replace_body(
        &tx,
        TABLES[2],
        "task:1",
        "step:1",
        &corrupt,
        &content_hash(&corrupt),
    );
    assert_eq!(
        pending_storage_claims(&tx, None),
        Err(InvestigationStoreError::Invalid)
    );
    assert_eq!(
        task_accounting(&tx, "task:1", false),
        Err(InvestigationStoreError::Invalid)
    );
    tx.rollback().unwrap();
    let tx = conn.transaction().unwrap();
    insert_reservation(&tx, &r).unwrap();
    tx.execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    let huge = vec![b'x'; MAX_MANIFEST_BYTES + 1];
    replace_body(
        &tx,
        TABLES[0],
        "task:1",
        "step:1",
        &huge,
        &content_hash(&huge),
    );
    assert_eq!(
        load(&tx, "task:1", "step:1"),
        Err(InvestigationStoreError::Invalid)
    );
}

#[test]
fn read_reservation_accounting_charges_all_rows_and_bounds_record_counts() {
    let mut conn = connection();
    init(&mut conn);
    let tx = conn.transaction().unwrap();
    let r = reservation("task:1", "step:0");
    insert_reservation(&tx, &r).unwrap();
    let bytes = encode(&r, MAX_MANIFEST_BYTES).unwrap().len()
        + 64
        + r.binding.task_id.len()
        + r.binding.step_id.len()
        + 256;
    assert_eq!(stored_bytes(&tx, Some("task:1")).unwrap(), bytes);
    assert_eq!(stored_bytes(&tx, None).unwrap(), bytes + 256);
    assert_eq!(
        pending_storage_claims(&tx, None).unwrap(),
        r.storage_claim.bytes(false).unwrap()
    );
    let d = Disposition::for_reservation(&r, None, DispositionKind::ReleasedBeforeRead).unwrap();
    insert_disposition(&tx, "task:1", "step:0", &d).unwrap();
    for n in 1..MAX_RESERVATIONS_PER_TASK {
        let r = reservation("task:1", &format!("step:{n}"));
        insert_reservation(&tx, &r).unwrap();
        let d =
            Disposition::for_reservation(&r, None, DispositionKind::ReleasedBeforeRead).unwrap();
        insert_disposition(&tx, "task:1", &r.binding.step_id, &d).unwrap();
    }
    assert_eq!(
        insert_reservation(&tx, &reservation("task:1", "step:65")),
        Err(InvestigationStoreError::Capacity)
    );
    assert_eq!(
        task_accounting(&tx, "task:1", false).unwrap(),
        ReadAccounting::default()
    );
    assert_eq!(pending_storage_claims(&tx, None).unwrap(), 0);
    let r = reservation("task:2", "step:1");
    insert_reservation(&tx, &r).unwrap();
    assert_eq!(
        pending_storage_claims(&tx, None).unwrap(),
        r.storage_claim.bytes(false).unwrap()
    );
    assert_eq!(pending_storage_claims(&tx, Some("task:1")).unwrap(), 0);
}

#[test]
fn read_reservation_storage_ceiling_covers_every_accepted_policy_budget() {
    // AC-0287 / review P3: Reservation::validate currently accepts only V2 for
    // both @10 specialists. Policy increases must not silently exceed the
    // immutable storage record ceiling.
    for specialist in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
    ] {
        let mut r = reservation("task:1", "step:budget");
        r.binding.specialist_id = specialist;
        r.validate().unwrap();
        let limits = InvestigationPolicy::for_profile(&r.binding.policy_profile)
            .unwrap()
            .limits();
        assert!(MAX_RESERVATIONS_PER_TASK >= limits.tool_actions as usize);
        assert!(MAX_RESERVATIONS_PER_TASK >= limits.evidence_requests as usize);
        assert_eq!(r.binding.policy_profile, INVESTIGATION_V2_POLICY.profile());
    }
}

#[path = "read_reservation_storage_tests.rs"]
mod storage_tests;

#[path = "read_reservation_preflight_tests.rs"]
mod preflight_tests;

#[test]
fn read_reservation_stage_claims_release_only_the_committed_stage() {
    let mut conn = connection();
    init(&mut conn);
    let tx = conn.transaction().unwrap();
    let r = reservation("task:1", "step:1");
    insert_reservation(&tx, &r).unwrap();
    assert_eq!(pending_event_claims(&tx, "task:1").unwrap(), 3);
    let before = pending_storage_claims(&tx, Some("task:1")).unwrap();
    let charge = ValidationCharge::for_reservation(&r).unwrap();
    insert_charge(&tx, "task:1", "step:1", &charge).unwrap();
    assert_eq!(pending_event_claims(&tx, "task:1").unwrap(), 2);
    assert_eq!(
        pending_storage_claims(&tx, Some("task:1")).unwrap(),
        before - r.storage_claim.validation_bytes
    );
    let disposition =
        Disposition::for_reservation(&r, Some(&charge), DispositionKind::OutcomeUnknown).unwrap();
    insert_disposition(&tx, "task:1", "step:1", &disposition).unwrap();
    assert_eq!(pending_event_claims(&tx, "task:1").unwrap(), 0);
    assert_eq!(pending_storage_claims(&tx, Some("task:1")).unwrap(), 0);
    tx.rollback().unwrap();
}

#[test]
fn hosted_source_reservation_accepts_ninth_call_within_frozen_budget() {
    // AC-0570: source admission must honor the task's frozen hosted allowance.
    let hosted = INVESTIGATION_V2_POLICY.with_hosted_generation().unwrap();
    let mut r = reservation("task:1", "step-9");
    r.binding.specialist_id = SpecialistId::DomainAnalystV25;
    r.binding.effective_limits = Some(hosted.limits());
    r.binding.base_usage.model_invocations = 9;
    r.binding.base_usage.generated_token_reservations =
        9 * hosted.limits().generated_tokens_per_invocation;
    validate_usage(&r.binding.base_usage, &hosted.limits()).unwrap();
    assert_eq!(r.validate(), Ok(()));
}

#[test]
fn legacy_source_reservation_keeps_encoding_and_static_ceiling() {
    // AC-0570: absent fields preserve historical hashes and the Local/V2 cap.
    let mut r = reservation("task:1", "step-9");
    let bytes = serde_json::to_vec(&r).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("effective_limits"));
    let reopened: Reservation = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(serde_json::to_vec(&reopened).unwrap(), bytes);
    assert_eq!(reopened.fingerprint(), r.fingerprint());
    r.binding.base_usage.model_invocations = 9;
    r.binding.base_usage.generated_token_reservations = 9 * llm::bounded::HOSTED_OUTPUT_TOKENS;
    assert_eq!(r.validate(), Err(InvestigationStoreError::Capacity));
}

#[test]
fn hosted_source_binding_requires_saved_task_policy() {
    // AC-0570: registered limits in a row are not provider authorization.
    let fixture = completion::internal_next_prompt_fixture();
    let mut detail = fixture.task.detail.clone();
    let mut r = reservation("task:1", "step-9");
    let hosted = INVESTIGATION_V2_POLICY.with_hosted_generation().unwrap();
    r.binding.effective_limits = Some(hosted.limits());
    assert!(r.validate().is_ok());
    assert_eq!(
        r.validate_task_policy(&detail),
        Err(InvestigationStoreError::Invalid)
    );
    detail.limits = hosted.limits();
    assert_eq!(
        r.validate_task_policy(&detail),
        Err(InvestigationStoreError::Invalid)
    );
    detail.origin = InvestigationOrigin::App;
    detail.summary.specialist_id = SpecialistId::DomainAnalystV25;
    detail.provider.mode = InvestigationProviderMode::Cloud;
    detail.summary.provider_mode = InvestigationProviderMode::Cloud;
    detail.provider.provider_id = "anthropic:claude-haiku-4-5-20251001".into();
    assert_eq!(
        r.validate_task_policy(&detail),
        Err(InvestigationStoreError::Invalid)
    );
    detail.provider.provider_id = "anthropic:claude-opus-5-5".into();
    assert!(r.validate_task_policy(&detail).is_ok());
    r.binding
        .effective_limits
        .as_mut()
        .unwrap()
        .generated_token_reservations += 1;
    assert_eq!(r.validate(), Err(InvestigationStoreError::Invalid));
    assert_eq!(
        r.validate_task_policy(&detail),
        Err(InvestigationStoreError::Invalid)
    );
}
