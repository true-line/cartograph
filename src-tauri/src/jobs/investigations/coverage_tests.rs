use super::*;
use context_hub::{
    ContextSnapshot,
    coverage::{CoverageQualification, ScopedRepo},
};
use core_graph::coverage::{CoverageRecord, StoredCoverage};
use serde_json::json;

fn archive(padding: usize, count: usize) -> InvestigationCoverageArchive {
    let records: Vec<_> = (0..count)
        .map(|index| {
            let repo = format!("acme/repo{index}");
            let record = CoverageRecord {
                repo_key: repo.clone(),
                registered_source_id: Some(format!("src:{index}")),
                ingest_id: "job:1".into(),
                config_hash: "config:one".into(),
                inventory_json: json!({"inventory":"coverage@1", "repo":repo,"commit":"workdir",
                "layer_hints":[],"producers":[],"unsupported":[],"limited":false,
                "padding":"x".repeat(padding)})
                .to_string(),
            };
            StoredCoverage {
                inventory_hash: record.inventory_hash(),
                record,
            }
        })
        .collect();
    let scope: Vec<_> = records
        .iter()
        .map(|stored| ScopedRepo {
            repo_key: stored.record.repo_key.clone(),
            registered_source_id: stored.record.registered_source_id.clone(),
            config_hash: stored.record.config_hash.clone(),
            layer_hints: None,
        })
        .collect();
    let qualification = CoverageQualification::new(&scope, &records).unwrap();
    let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
    InvestigationCoverageArchive::new(qualification.durable_proof(snapshot.id()).unwrap(), records)
        .unwrap()
}

fn start(store: &mut JobStore, nonce: &str) -> String {
    start_version(store, nonce, false)
}

fn start_version(store: &mut JobStore, nonce: &str, qualified: bool) -> String {
    let request: StartInvestigationRequest = serde_json::from_value(json!({
        "schema_version":if qualified {3} else {1},"request_nonce":nonce,
        "specialist_id":if qualified {"domain-analyst@6"} else {"domain-analyst@3"},
        "question":"Inspect the saved basis.","scope":{"type":"all"},"provider_mode":"local",
        "limit_profile":"investigation-v1"}))
    .unwrap();
    let provider: InvestigationProvider = serde_json::from_value(json!({
        "mode":"local","provider_id":"fixture","model":"fixture","endpoint":"http://localhost",
        "deployment":null,"available":true,"unavailable_reason":null}))
    .unwrap();
    store
        .start_investigation(&request, Some(&provider))
        .unwrap()
        .detail
        .summary
        .investigation_id
}

fn extension_drop(connection: &Connection) {
    storage::version_one_fixture(connection);
    connection
        .execute_batch(
            "DROP TRIGGER investigation_coverage_archives_no_update;
        DROP TRIGGER investigation_coverage_archives_no_delete;
        DROP TABLE investigation_coverage_archives; DROP TABLE investigation_coverage_meta;",
        )
        .unwrap();
}

fn legacy_layout(connection: &Connection) -> Vec<(String, String)> {
    connection.prepare("SELECT name,sql FROM sqlite_schema WHERE (name GLOB 'investigation_*' OR tbl_name GLOB 'investigation_*') AND name NOT GLOB 'investigation_coverage_*' AND name NOT GLOB 'investigation_finding_*' AND name NOT GLOB 'investigation_chat_*' AND name NOT GLOB 'investigation_chats*' AND tbl_name != 'investigation_chats' AND name != 'investigation_meta' AND name != 'investigation_tasks' AND name != 'investigation_mcp_clients' AND name NOT GLOB 'sqlite_*' ORDER BY name").unwrap()
        .query_map([],|row| Ok((row.get(0)?,row.get(1)?))).unwrap()
        .collect::<Result<_,_>>().unwrap()
}

// AC-0239: migration validates the exact legacy layout before adding an
// independent versioned extension; old SQL, namespace and record bytes survive.
#[test]
fn coverage_archive_migration_preserves_legacy_schema_and_records() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let mut store = JobStore::open(&path).unwrap();
    let id = start(&mut store, "legacy");
    extension_drop(&store.conn);
    let before_layout = legacy_layout(&store.conn);
    let before: (Vec<u8>, String) = store
        .conn
        .query_row(
            "SELECT record,record_hash FROM investigation_tasks WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let namespace = store.namespace.value.clone();
    drop(store);
    let reopened = JobStore::open(&path).unwrap();
    assert_eq!(legacy_layout(&reopened.conn), before_layout);
    let clients: i64 = reopened
        .conn
        .query_row("SELECT count(*) FROM investigation_mcp_clients", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(clients, 0, "migration must not invent client identities");
    let after: (Vec<u8>, String) = reopened
        .conn
        .query_row(
            "SELECT record,record_hash FROM investigation_tasks WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(reopened.namespace.value, namespace);
    assert_eq!(
        reopened.investigation(&id).unwrap().summary.schema_version,
        1
    );
    let versions: (i64,i64) = reopened.conn.query_row("SELECT (SELECT version FROM investigation_meta),(SELECT version FROM investigation_coverage_meta)",[],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(versions, (4, 1));
}

// AC-0239: partial, future or foreign layouts never get CREATE-repaired, and
// malformed legacy state prevents creation of any extension objects.
#[test]
fn coverage_archive_migration_refuses_partial_foreign_and_future_schemas() {
    for sql in [
        "DROP TRIGGER investigation_coverage_archives_no_update",
        "CREATE TRIGGER foreign_archive_write AFTER INSERT ON investigation_coverage_archives BEGIN SELECT 1; END",
        "PRAGMA ignore_check_constraints=ON; UPDATE investigation_coverage_meta SET version=2",
        "DROP TABLE investigation_coverage_meta; CREATE TABLE investigation_coverage_meta(future TEXT)",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.db");
        let store = JobStore::open(&path).unwrap();
        store.conn.execute_batch(sql).unwrap();
        drop(store);
        assert!(JobStore::open(&path).is_err(), "{sql}");
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let store = JobStore::open(&path).unwrap();
    extension_drop(&store.conn);
    store
        .conn
        .execute_batch("DROP TRIGGER investigation_inputs_no_update")
        .unwrap();
    drop(store);
    assert!(JobStore::open(&path).is_err());
    let connection = Connection::open(&path).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'investigation_coverage_*'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

// AC-0239: archive and first input share their caller's transaction, including
// rollback and reopen. Reads are keyed only by exact saved task identity.
#[test]
fn coverage_archive_is_atomic_immutable_and_task_keyed_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let mut store = JobStore::open(&path).unwrap();
    let id = start(&mut store, "atomic");
    let other = start(&mut store, "other");
    let original = archive(64, 1);
    assert!(coverage::insert(&store.conn, &id, &original).is_err());
    {
        let tx = store.conn.unchecked_transaction().unwrap();
        coverage::insert(&tx, &id, &original).unwrap();
        tx.execute(
            "INSERT INTO investigation_inputs VALUES (?1,0,?2,?3)",
            params![id, b"rollback-input".as_slice(), "0".repeat(64)],
        )
        .unwrap();
        assert!(coverage::load(&tx, &id).unwrap().is_some());
        // No commit: both archive and input roll back.
    }
    {
        let tx = store.conn.unchecked_transaction().unwrap();
        assert!(coverage::load(&tx, &id).unwrap().is_none());
        let inputs: i64 = tx
            .query_row(
                "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(inputs, 0);
        coverage::insert(&tx, &id, &original).unwrap();
        tx.commit().unwrap();
    }
    {
        let tx = store.conn.unchecked_transaction().unwrap();
        assert_eq!(
            coverage::insert(&tx, &id, &archive(0, 1)),
            Err(InvestigationStoreError::Conflict)
        );
        assert!(
            tx.execute(
                "DELETE FROM investigation_coverage_archives WHERE task_id=?1",
                [&id]
            )
            .is_err()
        );
        assert!(
            tx.execute(
                "UPDATE investigation_coverage_archives SET body=body WHERE task_id=?1",
                [&id]
            )
            .is_err()
        );
        tx.commit().unwrap();
    }
    drop(store);
    let reopened = JobStore::open(&path).unwrap();
    let tx = reopened.conn.unchecked_transaction().unwrap();
    let restored = coverage::load(&tx, &id).unwrap().unwrap();
    assert_eq!(restored, original);
    assert!(coverage::load(&tx, &other).unwrap().is_none());
    assert_eq!(
        coverage::load(&tx, "investigation:missing"),
        Err(InvestigationStoreError::Missing)
    );
    assert!(coverage::load(&tx, "invalid/path").is_err());
}

fn corrupt(connection: &Connection, id: &str, body: &[u8], hash: &str) {
    connection
        .execute_batch("DROP TRIGGER investigation_coverage_archives_no_update")
        .unwrap();
    connection
        .execute(
            "UPDATE investigation_coverage_archives SET body=?2,body_hash=?3 WHERE task_id=?1",
            params![id, body, hash],
        )
        .unwrap();
    let (_, _, sql) = coverage::schema()
        .into_iter()
        .find(|(name, _, _)| name == "investigation_coverage_archives_no_update")
        .unwrap();
    connection.execute_batch(&sql).unwrap();
}

// AC-0239: preflight rejects oversized storage before copying it; hashes and
// strict typed archive decoding reject corruption even under a valid SQL schema.
#[test]
fn coverage_archive_read_rejects_corruption_and_preflights_lengths() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
    let id = start(&mut store, "corrupt");
    let original = archive(0, 1);
    let tx = store.conn.unchecked_transaction().unwrap();
    coverage::insert(&tx, &id, &original).unwrap();
    tx.commit().unwrap();
    for body in [b"{}".as_slice(), b"not json", b"\xff"] {
        corrupt(&store.conn, &id, body, &core_prov::content_hash(body));
        let tx = store.conn.unchecked_transaction().unwrap();
        assert!(coverage::load(&tx, &id).is_err());
    }
    let body = serde_json::to_vec(&original).unwrap();
    corrupt(&store.conn, &id, &body, &"0".repeat(64));
    let tx = store.conn.unchecked_transaction().unwrap();
    assert_eq!(
        coverage::load(&tx, &id),
        Err(InvestigationStoreError::Invalid)
    );
    drop(tx);
    corrupt(&store.conn, &id, &body, &core_prov::content_hash(&body));
    store.conn.execute_batch("DROP TRIGGER investigation_coverage_archives_no_update; PRAGMA ignore_check_constraints=ON;").unwrap();
    store
        .conn
        .execute(
            "UPDATE investigation_coverage_archives SET body=zeroblob(?2) WHERE task_id=?1",
            params![
                id,
                i64::try_from(MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES + 1).unwrap()
            ],
        )
        .unwrap();
    let (_, _, sql) = coverage::schema()
        .into_iter()
        .find(|(name, _, _)| name == "investigation_coverage_archives_no_update")
        .unwrap();
    store.conn.execute_batch(&sql).unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    assert_eq!(
        coverage::load(&tx, &id),
        Err(InvestigationStoreError::Capacity)
    );
}

// AC-0239: archives consume both existing capacities and the terminal reserve;
// every later admission includes them. No 16 MiB archive allowance widens a task.
#[test]
fn coverage_archive_charges_task_store_and_subsequent_admissions() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
    let id = start(&mut store, "capacity");
    let original = archive(64, 1);
    let tx = store.conn.unchecked_transaction().unwrap();
    let task_before = remaining_task_capacity(&tx, &id).unwrap();
    let global_before = remaining_store_capacity(&tx).unwrap();
    coverage::insert(&tx, &id, &original).unwrap();
    let charged = serde_json::to_vec(&original).unwrap().len() + 64 + 256;
    assert_eq!(
        task_before - remaining_task_capacity(&tx, &id).unwrap(),
        charged
    );
    assert_eq!(
        global_before - remaining_store_capacity(&tx).unwrap(),
        charged
    );
    let remaining = remaining_task_capacity(&tx, &id).unwrap();
    tx.execute(
        "INSERT INTO investigation_inputs VALUES (?1,0,zeroblob(?2),?3)",
        params![id, i64::try_from(remaining).unwrap(), "0".repeat(64)],
    )
    .unwrap();
    assert_eq!(
        check_task_capacity(&tx, &id, false),
        Err(InvestigationStoreError::Capacity)
    );
    // Later failed admission and archive roll back together.
    drop(tx);
    let tx = store.conn.unchecked_transaction().unwrap();
    assert!(coverage::load(&tx, &id).unwrap().is_none());
    let too_large = archive(950 * 1024, 2);
    assert!(
        serde_json::to_vec(&too_large).unwrap().len() < MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES
    );
    assert_eq!(
        coverage::insert(&tx, &id, &too_large),
        Err(InvestigationStoreError::Capacity)
    );
    tx.commit().unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    assert!(coverage::load(&tx, &id).unwrap().is_none());
}

// AC-0239: the production first-input transition commits archive and ledger as
// one unit. A failure after archive insertion leaves neither behind; historical
// reads independently reject a valid archive substituted for the saved basis.
#[test]
fn coverage_archive_first_input_transition_is_atomic_and_history_bound() {
    use crate::job_execution::JobExecutionLocks;
    use crate::jobs::ClaimMode;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let mut store = JobStore::open(&path).unwrap();
    let id = start_version(&mut store, "first-input", true);
    let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
    let queued = store.investigation(&id).unwrap();
    let plan = store
        .claim_plan(queued.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
    let original = archive(64, 1);
    let ledger = InvestigationInputLedger {
        coverage_reads: vec![],
        artifact_reads: vec![],
        schema_version: 2,
        coverage: Some(original.proof().clone()),
        graph_snapshot_id: original.proof().snapshot_id.clone(),
        scope_snapshot_id: original.proof().snapshot_id.clone(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    let preparing = store.investigation(&id).unwrap();
    // This usage is rejected inside admit_input, after archive::insert succeeds.
    let mut invalid_usage = preparing.usage.clone();
    invalid_usage.selected_facts = 1;
    assert_eq!(
        store.advance_investigation(
            &execution,
            preparing.summary.revision,
            InvestigationTransition::InputPrepared {
                ledger: Box::new(ledger.clone()),
                usage: invalid_usage,
                coverage_archive: Some(Box::new(original.clone()))
            }
        ),
        Err(InvestigationStoreError::Invalid)
    );
    {
        let tx = store.conn.unchecked_transaction().unwrap();
        assert!(coverage::load(&tx, &id).unwrap().is_none());
        let rows: i64 = tx
            .query_row(
                "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
    }
    assert_eq!(store.investigation(&id).unwrap(), preparing);
    store
        .advance_investigation(
            &execution,
            preparing.summary.revision,
            InvestigationTransition::InputPrepared {
                ledger: Box::new(ledger.clone()),
                usage: preparing.usage,
                coverage_archive: Some(Box::new(original.clone())),
            },
        )
        .unwrap();
    assert_eq!(
        store.investigation_ledger(&id).unwrap(),
        Some(ledger.clone())
    );
    drop(store);
    let reopened = JobStore::open(&path).unwrap();
    assert_eq!(
        reopened.investigation_ledger(&id).unwrap(),
        Some(ledger.clone())
    );
    let substitute = archive(128, 1);
    let replacement = serde_json::to_vec(&substitute).unwrap();
    corrupt(
        &reopened.conn,
        &id,
        &replacement,
        &core_prov::content_hash(&replacement),
    );
    {
        let tx = reopened.conn.unchecked_transaction().unwrap();
        assert_eq!(coverage::load(&tx, &id).unwrap(), Some(substitute));
    }
    assert_eq!(
        reopened.investigation_ledger(&id),
        Err(InvestigationStoreError::Invalid)
    );
    let bytes = serde_json::to_vec(&original).unwrap();
    corrupt(
        &reopened.conn,
        &id,
        &bytes,
        &core_prov::content_hash(&bytes),
    );
    assert_eq!(reopened.investigation_ledger(&id).unwrap(), Some(ledger));
    reopened
        .conn
        .execute_batch("DROP TRIGGER investigation_coverage_archives_no_delete")
        .unwrap();
    reopened
        .conn
        .execute(
            "DELETE FROM investigation_coverage_archives WHERE task_id=?1",
            [&id],
        )
        .unwrap();
    let (_, _, sql) = coverage::schema()
        .into_iter()
        .find(|(name, _, _)| name == "investigation_coverage_archives_no_delete")
        .unwrap();
    reopened.conn.execute_batch(&sql).unwrap();
    assert_eq!(
        reopened.investigation_ledger(&id),
        Err(InvestigationStoreError::Invalid)
    );
}

// AC-0327: exercise actual optional schema validation and durable archive load.
#[cfg(unix)]
#[test]
fn coverage_archive_workspace_covers_optional_layout_and_restore() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
    let id = start(&mut store, "workspace");
    let original = archive(4096, 2);
    let tx = store.conn.unchecked_transaction().unwrap();
    read_reservations::initialize(&tx).unwrap();
    admission_journal::initialize(&tx).unwrap();
    coverage::insert(&tx, &id, &original).unwrap();
    assert_eq!(coverage::load(&tx, &id).unwrap(), Some(original.clone()));
    let rows = tx
        .prepare("SELECT name,type,sql FROM sqlite_schema WHERE name GLOB 'investigation_*'")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let actual = std::mem::size_of_val(&rows)
        + rows
            .iter()
            .map(|(n, k, s)| {
                std::mem::size_of::<(String, String, String)>() + n.len() + k.len() + s.len()
            })
            .sum::<usize>();
    assert!(storage::archive_schema_workspace().unwrap() >= actual * 2);
    let wire = serde_json::to_vec(&original).unwrap();
    let (_, decode) = InvestigationCoverageArchive::decode_workspace_bytes().unwrap();
    let (caller, authentication) = next_prompt_record_workspace(MAX_RECORD_BYTES).unwrap();
    let bound = coverage::archive_acquisition_workspace().unwrap();
    assert!(bound >= caller + decode + wire.len());
    assert!(bound >= caller + authentication);
    assert!(bound >= caller * 2 + next_prompt_contract_workspace().unwrap());
}
