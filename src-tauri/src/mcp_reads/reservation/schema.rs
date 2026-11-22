//! Fixed metadata owners admitted before bounded graph/source/coverage reads.
//! Counts follow the exact private v2 source schema and v1 generation registry;
//! they are not new policy ceilings or allocator/RSS bounds.
use context_hub::logical::{map, slots, sum};

const SOURCE_OBJECTS: usize = 32;
const SOURCE_GENERATION_TRIGGERS: usize = 12;
const COVERAGE_OBJECTS: usize = 2;
const COVERAGE_GENERATION_TRIGGERS: usize = 3;
const GENERATION_OBJECTS: usize = 16;
const NAME_BYTES: usize = 256;
const TYPE_BYTES: usize = 16;
const SQL_BYTES: usize = 4096;

pub(crate) fn workspace_bytes() -> Option<usize> {
    let objects = (SOURCE_OBJECTS + SOURCE_GENERATION_TRIGGERS)
        .max(COVERAGE_OBJECTS + COVERAGE_GENERATION_TRIGGERS)
        .max(GENERATION_OBJECTS);
    let cell = sum(&[NAME_BYTES, TYPE_BYTES, SQL_BYTES])?;
    // Source/coverage owned-schema maps overlap generated generation-schema Vec
    // during stripping. Generation validation instead overlaps its own map and
    // that same Vec; its temporary type/SQL comparison clones are also charged.
    sum(&[
        objects.checked_mul(cell)?,
        map::<String, (String, String)>(objects)?,
        GENERATION_OBJECTS.checked_mul(cell)?,
        slots::<(String, String, String)>(GENERATION_OBJECTS)?,
        TYPE_BYTES,
        SQL_BYTES,
        slots::<(i64, i64, i64)>(2)?,
        // One currently formatted fixed owner/preflight query (<4096 bytes).
        SQL_BYTES,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    // AC-0288: schema inventories cannot silently outgrow worker admission.
    #[test]
    fn selection_schema_workspace_tracks_exact_private_inventory_and_cell_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.db");
        let _store = core_graph::SqliteGraphStore::open(&path).unwrap();
        let db = rusqlite::Connection::open(path).unwrap();
        let owners = [
            (
                "name GLOB 'source_*' OR (type='trigger' AND tbl_name IN ('nodes','edges','source_node_bindings','source_edge_bindings'))",
                SOURCE_OBJECTS + SOURCE_GENERATION_TRIGGERS,
            ),
            (
                "name GLOB 'coverage_*' OR (type='trigger' AND tbl_name='coverage_inventories')",
                COVERAGE_OBJECTS + COVERAGE_GENERATION_TRIGGERS,
            ),
            ("name GLOB 'graph_generation*'", GENERATION_OBJECTS),
        ];
        for (predicate, expected) in owners {
            let sql = format!(
                "SELECT count(*),min(length(CAST(name AS BLOB))<=?1 AND length(CAST(type AS BLOB))<=?2 AND length(CAST(sql AS BLOB))<=?3) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' AND ({predicate})"
            );
            let (count, bounded): (i64, bool) = db
                .query_row(
                    &sql,
                    rusqlite::params![NAME_BYTES as i64, TYPE_BYTES as i64, SQL_BYTES as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(count, expected as i64);
            assert!(bounded);
        }
        assert!(
            workspace_bytes().unwrap() > (SOURCE_OBJECTS + SOURCE_GENERATION_TRIGGERS) * SQL_BYTES
        );
    }
}
