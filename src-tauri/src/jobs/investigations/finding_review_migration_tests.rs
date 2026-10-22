use super::*;

fn layout(connection: &Connection) -> Vec<(String, String, String)> {
    connection
        .prepare(&format!(
            "SELECT name,type,sql FROM sqlite_schema WHERE {OWNED} ORDER BY name"
        ))
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

// AC-0294: production migration rolls back metadata DDL and review objects together.
#[test]
fn finding_review_migration_faults_restore_complete_v1_layout() {
    for stage in [1, 2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = JobStore::open(&path).unwrap();
        version_one_fixture(&store.conn);
        let old = layout(&store.conn);
        assert_eq!(
            initialize_with_hook(&mut store.conn, &store.namespace, |point| {
                if point == stage {
                    Err(InvestigationStoreError::Storage)
                } else {
                    Ok(())
                }
            }),
            Err(InvestigationStoreError::Storage)
        );
        let independent = Connection::open(&path).unwrap();
        assert_eq!(layout(&independent), old);
        let version: i64 = independent
            .query_row("SELECT version FROM investigation_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        initialize(&mut store.conn, &store.namespace).unwrap();
        validate(&store.conn, &store.namespace).unwrap();
        let upgraded = layout(&store.conn);
        drop(store);
        let reopened = JobStore::open(&path).unwrap();
        assert_eq!(layout(&reopened.conn), upgraded);
        let count: i64 = reopened
            .conn
            .query_row(
                "SELECT count(*) FROM investigation_finding_reviews",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}

// AC-0294: never repair unknown versions or partial/foreign review namespaces.
#[test]
fn finding_review_migration_refuses_partial_and_unknown_versions() {
    for sql in [
        "DROP TRIGGER investigation_finding_reviews_no_delete",
        "CREATE TABLE investigation_finding_review_extra (x TEXT) STRICT",
        "PRAGMA ignore_check_constraints=ON; UPDATE investigation_meta SET version=3",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = JobStore::open(&path).unwrap();
        store.conn.execute_batch(sql).unwrap();
        let before = layout(&store.conn);
        drop(store);
        assert!(JobStore::open(&path).is_err());
        assert_eq!(layout(&Connection::open(&path).unwrap()), before);
    }
}

// AC-0294: independently versioned extensions remain exact through core upgrade.
#[test]
fn finding_review_migration_preserves_supported_optional_extensions() {
    for extension in 0..4 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = JobStore::open(&path).unwrap();
        {
            let tx = store.conn.transaction().unwrap();
            if extension & 1 != 0 {
                super::super::read_reservations::initialize(&tx).unwrap();
            }
            if extension & 2 != 0 {
                super::super::admission_journal::initialize(&tx).unwrap();
            }
            tx.commit().unwrap();
        }
        version_one_fixture(&store.conn);
        let before = layout(&store.conn);
        drop(store);
        let reopened = JobStore::open(&path).unwrap();
        let after = layout(&reopened.conn);
        for entry in before
            .iter()
            .filter(|entry| entry.0 != "investigation_meta")
        {
            assert!(after.contains(entry));
        }
        let tx = reopened.conn.unchecked_transaction().unwrap();
        validate(&tx, &reopened.namespace).unwrap();
        tx.commit().unwrap();
    }
}
