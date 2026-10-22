use super::*;
use crate::sources::{SourceRegistry, read_sources};
use std::path::Path;

fn directory(parent: &Path, name: &str) -> std::path::PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).unwrap();
    crate::paths::canonicalize(path).unwrap()
}

fn bytes(conn: &Connection) -> usize {
    conn.query_row(
        "SELECT coalesce(sum(length(CAST(source_id AS BLOB)) + length(CAST(repo_key AS BLOB)) + length(CAST(root AS BLOB)) + length(CAST(display_name AS BLOB)) + length(CAST(kind AS BLOB)) + coalesce(length(CAST(origin_key AS BLOB)),0) + coalesce(length(CAST(clone_url AS BLOB)),0) + coalesce(length(CAST(github_repo AS BLOB)),0)),0) FROM registered_sources",
        [], |row| row.get::<_, i64>(0),
    ).unwrap().try_into().unwrap()
}

#[test]
fn bounded_registry_empty_boundaries_and_complete_inventory_parity() {
    // AC-0288: bound registration materialization before constructing MCP scope.
    let temp = tempfile::tempdir().unwrap();
    let private = directory(temp.path(), "private");
    let mut registry = SourceRegistry::open(private.join("state.db"), &private).unwrap();
    assert!(
        registry
            .list_bounded(SourceListLimits {
                max_rows: 0,
                max_bytes: 0
            })
            .unwrap()
            .is_empty()
    );
    let local = directory(temp.path(), "café");
    registry.register_local(&local).unwrap();
    registry
        .reserve_managed("https://github.com/example/project.git")
        .unwrap();
    let size = bytes(&registry.conn);
    let admitted = registry
        .list_bounded(SourceListLimits {
            max_rows: 2,
            max_bytes: size,
        })
        .unwrap();
    let legacy = registry.list().unwrap();
    assert_eq!(admitted.len(), 2);
    for (bounded, original) in admitted.iter().zip(&legacy) {
        assert_eq!(bounded.source_id, original.source_id);
        assert_eq!(bounded.repo_key, original.repo_key);
        assert_eq!(bounded.root, original.root);
        assert_eq!(bounded.display_name, original.display_name);
        assert_eq!(bounded.clone_url(), original.clone_url());
        assert_eq!(bounded.ready, original.ready);
    }
    assert_eq!(
        registry
            .list_bounded(SourceListLimits {
                max_rows: 1,
                max_bytes: size
            })
            .unwrap_err(),
        SourceListError::RowLimit
    );
    assert_eq!(
        registry
            .list_bounded(SourceListLimits {
                max_rows: 2,
                max_bytes: size - 1
            })
            .unwrap_err(),
        SourceListError::ByteLimit
    );
}

#[test]
fn bounded_registry_rejects_corrupt_oversized_text_before_utf8_decode() {
    let temp = tempfile::tempdir().unwrap();
    let private = directory(temp.path(), "private");
    let root = directory(temp.path(), "project");
    let mut registry = SourceRegistry::open(private.join("state.db"), &private).unwrap();
    registry.register_local(&root).unwrap();
    // SQLite TEXT may contain invalid UTF-8. The byte preflight cannot decode it.
    registry
        .conn
        .execute(
            "UPDATE registered_sources SET root=CAST(?1 AS TEXT)",
            [vec![0xff_u8; 8192]],
        )
        .unwrap();
    assert_eq!(
        registry
            .list_bounded(SourceListLimits {
                max_rows: 0,
                max_bytes: 1
            })
            .unwrap_err(),
        SourceListError::RowLimit
    );
    assert_eq!(
        registry
            .list_bounded(SourceListLimits {
                max_rows: 1,
                max_bytes: 8191
            })
            .unwrap_err(),
        SourceListError::ByteLimit
    );
    // With enough bytes admitted, ordinary decode fails as storage, proving the
    // earlier byte error takes precedence over attempting to decode this cell.
    assert_eq!(
        registry
            .list_bounded(SourceListLimits {
                max_rows: 1,
                max_bytes: 65536
            })
            .unwrap_err(),
        SourceListError::Storage
    );
}

#[test]
fn bounded_registry_invalid_state_and_operational_failure_remain_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let private = directory(temp.path(), "private");
    let root = directory(temp.path(), "project");
    let mut registry = SourceRegistry::open(private.join("state.db"), &private).unwrap();
    registry.register_local(&root).unwrap();
    registry
        .conn
        .execute(
            "UPDATE registered_sources SET repo_key='invalid-association'",
            [],
        )
        .unwrap();
    let limits = SourceListLimits {
        max_rows: 1,
        max_bytes: 65536,
    };
    assert_eq!(
        registry.list_bounded(limits).unwrap_err(),
        SourceListError::InvalidState
    );
    registry
        .conn
        .execute("DROP TABLE registered_sources", [])
        .unwrap();
    assert_eq!(
        registry.list_bounded(limits).unwrap_err(),
        SourceListError::Storage
    );
}

#[test]
fn bounded_registry_preflight_checks_nullable_and_integer_sql_types() {
    let conn = Connection::open_in_memory().unwrap();
    // Deliberately non-STRICT fixture exercises metadata type checks directly;
    // production check_state separately requires the exact STRICT table DDL.
    conn.execute_batch("CREATE TABLE registered_sources(source_id,repo_key,root,display_name,kind,origin_key,clone_url,github_repo,ready); INSERT INTO registered_sources VALUES ('id','repo','root','display','local',NULL,NULL,NULL,1)").unwrap();
    let limits = SourceListLimits {
        max_rows: 1,
        max_bytes: 1024,
    };
    preflight(&conn, limits).unwrap();
    for column in [
        "source_id",
        "repo_key",
        "root",
        "display_name",
        "kind",
        "origin_key",
        "clone_url",
        "github_repo",
        "ready",
    ] {
        let tx = conn.unchecked_transaction().unwrap();
        tx.execute(
            &format!("UPDATE registered_sources SET {column}=?1"),
            [vec![1_u8]],
        )
        .unwrap();
        assert_eq!(
            preflight(&tx, limits),
            Err(SourceListError::InvalidState),
            "{column}"
        );
        tx.rollback().unwrap();
    }
    conn.execute("UPDATE registered_sources SET ready=2", [])
        .unwrap();
    assert_eq!(preflight(&conn, limits), Err(SourceListError::InvalidState));
}

#[test]
fn bounded_registry_preflight_and_materialization_share_one_wal_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let private = directory(temp.path(), "private");
    let first = directory(temp.path(), "first");
    let second = directory(temp.path(), "second");
    let state = private.join("state.db");
    let mut registry = SourceRegistry::open(&state, &private).unwrap();
    registry.register_local(&first).unwrap();
    let mut writer = SourceRegistry::open(&state, &private).unwrap();
    let limits = SourceListLimits {
        max_rows: 1,
        max_bytes: bytes(&registry.conn),
    };
    let tx = registry.conn.unchecked_transaction().unwrap();
    preflight(&tx, limits).unwrap();
    writer.register_local(&second).unwrap();
    // Same transaction sequence used by list_bounded: a later committed row
    // cannot slip between successful preflight and registry materialization.
    crate::sources::check_state(&tx, Some(&registry.registry_id)).unwrap();
    assert_eq!(
        read_sources(&tx, &registry.app_data_dir, &registry.registry_id, None)
            .unwrap()
            .len(),
        1
    );
    tx.commit().unwrap();
    assert_eq!(
        registry.list_bounded(limits).unwrap_err(),
        SourceListError::RowLimit
    );
    assert_eq!(registry.list().unwrap().len(), 2);
}
