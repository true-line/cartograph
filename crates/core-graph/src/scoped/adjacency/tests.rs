use super::*;
fn limits(rows: usize) -> SnapshotReadLimits {
    SnapshotReadLimits {
        max_rows: rows,
        ..Default::default()
    }
}
fn fixture() -> SqliteGraphStore {
    let s = SqliteGraphStore::open_in_memory().unwrap();
    s.conn.execute_batch("INSERT INTO nodes VALUES('a','DataEntity','{}'),('b','DataEntity','{}'),('c','Gap','{}'); INSERT INTO edges(rowid,src,dst,label) VALUES(-9,'a','b','REFERENCES'),(7,'c','b','UNRELATED'),(20,'b','c','REFERENCES');").unwrap();
    s
}
#[test]
fn indexed_pages_charge_all_labels_and_signed_positions() {
    let s = fixture();
    let rev = s.read_revision().unwrap();
    s.with_scoped_reader::<_, GraphError>(&rev, limits(12), |r| {
        assert_eq!(r.node("absent")?, None);
        let first = r.incoming("b", None, 1)?;
        assert_eq!(first.next_incoming, Some(-9));
        let second = r.incoming("b", first.next_incoming, 1)?;
        assert_eq!(second.edges[0].label, "UNRELATED");
        assert_eq!(second.remaining, Remaining::Exhausted);
        let out = r.outgoing("b", None, 1)?;
        assert_eq!(out.edges[0].dst, "c");
        assert_eq!(r.usage().visits, 4);
        assert_eq!(r.usage().adjacency_probes, 3);
        Ok(())
    })
    .unwrap();
}
#[test]
fn cumulative_visits_include_missing_and_duplicates() {
    let s = fixture();
    let rev = s.read_revision().unwrap();
    let e = s.with_scoped_reader::<_, GraphError>(&rev, limits(2), |r| {
        r.node("missing")?;
        r.incoming("b", None, 1)?;
        r.node("a")
    });
    assert!(matches!(
        e,
        Err(GraphError::SnapshotBounds(SnapshotBoundsError::RowLimit))
    ));
}
#[test]
fn incoming_index_must_be_exact_and_present() {
    for sql in [
        "DROP INDEX idx_edges_dst",
        "DROP INDEX idx_edges_dst; CREATE INDEX idx_edges_dst ON edges(src)",
        "DROP INDEX idx_edges_dst; CREATE INDEX idx_edges_dst ON edges(dst) WHERE label='REFERENCES'",
        "DROP INDEX idx_edges_dst; CREATE INDEX idx_edges_dst ON edges(dst COLLATE NOCASE)",
    ] {
        let s = fixture();
        s.conn.execute_batch(sql).unwrap();
        let rev = s.read_revision().unwrap();
        let result = s.with_scoped_reader::<_, GraphError>(&rev, limits(10), |_| Ok(()));
        assert!(matches!(
            result,
            Err(GraphError::SnapshotBounds(
                SnapshotBoundsError::InvalidSchema
            ))
        ));
    }
}
#[test]
fn unrelated_estate_does_not_consume_closure_visits() {
    let mut s = fixture();
    let tx = s.conn.transaction().unwrap();
    tx.execute_batch("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<60000) INSERT INTO nodes SELECT printf('z%06d',i),'Symbol','{}' FROM n; INSERT INTO edges SELECT id,id,'CALLS','{}' FROM nodes WHERE id GLOB 'z*';").unwrap();
    tx.commit().unwrap();
    let rev = s.read_revision().unwrap();
    s.with_scoped_reader::<_, GraphError>(&rev, limits(10), |r| {
        assert_eq!(r.incoming("b", None, 2)?.edges.len(), 2);
        assert_eq!(r.outgoing("a", None, 2)?.edges.len(), 1);
        assert_eq!(r.usage().visits, 3);
        Ok(())
    })
    .unwrap();
}
#[test]
fn replace_changes_rowid_and_old_revision_is_rejected() {
    let s = fixture();
    let rev = s.read_revision().unwrap();
    s.conn
        .execute_batch("INSERT OR REPLACE INTO edges(src,dst,label) VALUES('a','b','REFERENCES')")
        .unwrap();
    let id: i64 = s
        .conn
        .query_row("SELECT rowid FROM edges WHERE src='a'", [], |r| r.get(0))
        .unwrap();
    assert_ne!(id, -9);
    assert!(matches!(
        s.with_scoped_reader::<_, GraphError>(&rev, limits(10), |_| Ok(())),
        Err(GraphError::Generation(GenerationError::Changed))
    ));
}
#[test]
fn whole_callback_pins_one_snapshot_while_writer_commits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    let s = SqliteGraphStore::open(&path).unwrap();
    s.conn.execute_batch("INSERT INTO nodes VALUES('a','DataEntity','{}'),('b','DataEntity','{}'); INSERT INTO edges(src,dst,label) VALUES('a','b','REFERENCES');").unwrap();
    let writer = SqliteGraphStore::open(&path).unwrap();
    let rev = s.read_revision().unwrap();
    s.with_scoped_reader::<_, GraphError>(&rev, limits(10), |r| {
        assert_eq!(r.outgoing("a", None, 2)?.edges.len(), 1);
        writer
            .conn
            .execute_batch("DELETE FROM edges; UPDATE nodes SET label='Changed' WHERE id='b';")?;
        assert_eq!(r.node("b")?.unwrap().label, "DataEntity");
        assert_eq!(r.incoming("b", None, 2)?.edges.len(), 1);
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        s.with_scoped_reader::<_, GraphError>(&rev, limits(10), |_| Ok(())),
        Err(GraphError::Generation(GenerationError::Changed))
    ));
}
#[test]
fn raw_preflight_rejects_before_corrupt_property_decode() {
    let s = fixture();
    s.conn
        .execute(
            "UPDATE edges SET props=?1 WHERE src='a'",
            ["x".repeat(1000)],
        )
        .unwrap();
    let rev = s.read_revision().unwrap();
    let mut cap = limits(10);
    cap.max_bytes = 100;
    assert!(matches!(
        s.with_scoped_reader::<_, GraphError>(&rev, cap, |r| r.outgoing("a", None, 1)),
        Err(GraphError::SnapshotBounds(
            SnapshotBoundsError::RawByteLimit
        ))
    ));
}
#[test]
fn incoming_plan_uses_index_without_sort() {
    let s = fixture();
    let mut q=s.conn.prepare("EXPLAIN QUERY PLAN SELECT src,dst,label,props FROM edges INDEXED BY idx_edges_dst WHERE dst=?1 AND rowid>?2 ORDER BY rowid LIMIT 1").unwrap();
    let rows = q
        .query_map(rusqlite::params!["b", -10], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(rows.iter().any(|s| s.contains("idx_edges_dst")));
    assert!(
        rows.iter()
            .all(|s| !s.contains("SCAN ") && !s.contains("TEMP B-TREE"))
    );
}
#[test]
fn binary_adjacency_overrides_nocase_defaults_and_still_seeks() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE nodes(id TEXT COLLATE NOCASE,label TEXT NOT NULL,props TEXT NOT NULL DEFAULT '{}',PRIMARY KEY(id COLLATE BINARY)) STRICT;
    CREATE TABLE edges(src TEXT COLLATE NOCASE NOT NULL,dst TEXT COLLATE NOCASE NOT NULL,label TEXT COLLATE NOCASE NOT NULL,props TEXT NOT NULL DEFAULT '{}',PRIMARY KEY(src COLLATE BINARY,dst COLLATE BINARY,label COLLATE BINARY)) STRICT;
    CREATE INDEX idx_edges_dst ON edges(dst COLLATE BINARY);").unwrap();
    c.pragma_update(None, "user_version", crate::GRAPH_SCHEMA_VERSION)
        .unwrap();
    let s = SqliteGraphStore::init(c).unwrap();
    // Match the existing NOCASE fixture: its nonstandard parent collation
    // cannot satisfy source-binding foreign-key collation rules.
    s.conn.pragma_update(None, "foreign_keys", false).unwrap();
    s.conn.execute_batch("INSERT INTO nodes VALUES('a','Entity','{}'),('A','Entity','{}'),('b','Entity','{}'); INSERT INTO edges VALUES('a','A','a','{}'),('a','a','A','{}'),('a','a','a','{}'),('A','a','a','{}');").unwrap();
    let rev = s.read_revision().unwrap();
    s.with_scoped_reader::<_, GraphError>(&rev, limits(20), |r| {
        let mut after = None;
        for (dst, label) in [("A", "a"), ("a", "A"), ("a", "a")] {
            let page = r.outgoing("a", after.as_ref(), 1)?;
            assert_eq!(
                (
                    &*page.edges[0].src,
                    &*page.edges[0].dst,
                    &*page.edges[0].label
                ),
                ("a", dst, label)
            );
            after = page.next_outgoing;
        }
        let page = r.incoming("A", None, 10)?;
        assert_eq!(page.edges.len(), 1);
        assert_eq!(page.edges[0].dst, "A");
        Ok(())
    })
    .unwrap();
    for sql in [
        "SELECT src,dst,label,props FROM edges INDEXED BY sqlite_autoindex_edges_1 WHERE src COLLATE BINARY=?1 AND (dst COLLATE BINARY,label COLLATE BINARY)>(?2,?3) ORDER BY dst COLLATE BINARY,label COLLATE BINARY LIMIT 1",
        "SELECT src,dst,label,props FROM edges INDEXED BY idx_edges_dst WHERE dst COLLATE BINARY=?1 AND rowid>?2 ORDER BY rowid LIMIT ?3",
    ] {
        let mut q = s
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let plans = q
            .query_map(rusqlite::params!["a", "", 1], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(plans.iter().any(|p| p.contains("SEARCH ")));
        assert!(
            plans
                .iter()
                .all(|p| !p.contains("SCAN ") && !p.contains("TEMP B-TREE"))
        );
    }
}
