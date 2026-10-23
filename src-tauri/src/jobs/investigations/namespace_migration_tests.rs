use super::*;

fn task_row(store: &JobStore, id: &str) -> Vec<rusqlite::types::Value> {
    store.conn.query_row(
        "SELECT ordinal,id,nonce,conversation_id,job_id,revision,live,record,record_hash FROM investigation_tasks WHERE id=?1",
        [id], |row| (0..9).map(|column| row.get(column)).collect(),
    ).unwrap()
}

// AC-0323: migrate real admitted/prepared App work without rewriting its bytes,
// losing its nonce interpretation, or resetting deleted-row ordinal high-water.
#[test]
fn mcp_namespace_migration_preserves_app_history_nonce_and_ordinal_high_water() {
    let mut fixture = Fixture::new();
    let mode = InvestigationProviderMode::Local;
    let (id, execution) = fixture.start("old-app-nonce", mode);
    let ledger = fixture.prepare(&id, &execution, false);
    let detail = fixture.store.investigation(&id).unwrap();
    let original = task_row(&fixture.store, &id);
    storage::version_two_fixture(&fixture.store.conn);
    fixture
        .store
        .conn
        .execute(
            "UPDATE sqlite_sequence SET seq=137 WHERE name='investigation_tasks'",
            [],
        )
        .unwrap();
    fixture.store = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(task_row(&fixture.store, &id), original);
    assert_eq!(fixture.store.investigation(&id).unwrap(), detail);
    assert_eq!(
        fixture.store.investigation_ledger(&id).unwrap(),
        Some(ledger)
    );
    let namespace: String = fixture
        .store
        .conn
        .query_row(
            "SELECT request_namespace FROM investigation_tasks WHERE id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(namespace, "app");
    let retry = fixture
        .store
        .start_investigation(&request("old-app-nonce", mode), Some(&provider(mode)))
        .unwrap();
    assert_eq!(retry.detail.summary.investigation_id, id);
    assert_eq!(task_row(&fixture.store, &id), original);
    let next = fixture
        .store
        .start_investigation(&request("new-app-nonce", mode), Some(&provider(mode)))
        .unwrap();
    let ordinal: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT ordinal FROM investigation_tasks WHERE id=?1",
            [&next.detail.summary.investigation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ordinal, 138);
}

// AC-0323: an empty legacy table still carries its persisted ordinal sequence.
#[test]
fn mcp_namespace_migration_preserves_empty_table_ordinal_high_water() {
    let mut fixture = Fixture::new();
    storage::version_two_fixture(&fixture.store.conn);
    fixture
        .store
        .conn
        .execute(
            "UPDATE sqlite_sequence SET seq=137 WHERE name='investigation_tasks'",
            [],
        )
        .unwrap();
    fixture.store = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let next = fixture
        .store
        .start_investigation(
            &request("after-empty-migration", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap();
    let ordinal: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT ordinal FROM investigation_tasks WHERE id=?1",
            [&next.detail.summary.investigation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ordinal, 138);
}
