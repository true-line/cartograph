use super::*;

fn corrupt_store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    // Deliberately permissive corrupt fixture; production schema validation
    // remains the caller's responsibility and is not relaxed by this test.
    conn.execute_batch(
        "CREATE TABLE receipt_headers(id TEXT PRIMARY KEY, source_id, repo_key, header);
        CREATE TABLE receipts(id TEXT PRIMARY KEY, source_id, repo_key, payload, header_id);
        INSERT INTO receipts VALUES ('r','src','repo','{}',NULL);",
    )
    .unwrap();
    conn
}

#[test]
fn receipt_projection_rejects_oversized_invalid_utf8_before_string_decode() {
    // AC-0288: oversized retained cells fail admission before materialization.
    for (column, limit) in [
        ("source_id", 256),
        ("repo_key", 256),
        ("payload", MAX_RECEIPT_BYTES as usize),
        ("header_id", 256),
    ] {
        let conn = corrupt_store();
        let bytes = vec![0xffu8; limit + 1];
        conn.execute(
            &format!("UPDATE receipts SET {column}=CAST(?1 AS TEXT)"),
            [&bytes],
        )
        .unwrap();
        // An unguarded row.get<String> would fail UTF-8 decoding and surface as
        // Operational. Invalid proves the bounded projection withheld the cell.
        assert_eq!(
            stored_receipt(&conn, "r").unwrap_err(),
            PinnedReadError::Invalid,
            "{column}"
        );
    }
    for (column, limit) in [
        ("source_id", 256),
        ("repo_key", 256),
        ("header", MAX_RECEIPT_HEADER_BYTES as usize),
    ] {
        let conn = corrupt_store();
        conn.execute_batch(
            "INSERT INTO receipt_headers VALUES ('h','src','repo','{}');
             UPDATE receipts SET header_id='h';",
        )
        .unwrap();
        let bytes = vec![0xffu8; limit + 1];
        conn.execute(
            &format!("UPDATE receipt_headers SET {column}=CAST(?1 AS TEXT)"),
            [&bytes],
        )
        .unwrap();
        assert_eq!(
            stored_receipt(&conn, "r").unwrap_err(),
            PinnedReadError::Invalid,
            "receipt_headers.{column}"
        );
    }
}

#[test]
fn receipt_projection_counts_utf8_bytes_and_rejects_nontext_cells() {
    for column in ["source_id", "repo_key", "payload"] {
        let conn = corrupt_store();
        for sql_value in ["NULL", "42", "X'7B7D'", "CAST(X'FFFF' AS TEXT)"] {
            conn.execute_batch(&format!("UPDATE receipts SET {column}={sql_value}"))
                .unwrap();
            assert_eq!(
                stored_receipt(&conn, "r").unwrap_err(),
                PinnedReadError::Invalid
            );
        }
    }
    for sql_value in ["42", "X'7B7D'", "CAST(X'FFFF' AS TEXT)"] {
        let conn = corrupt_store();
        conn.execute_batch(&format!("UPDATE receipts SET header_id={sql_value}"))
            .unwrap();
        assert_eq!(
            stored_receipt(&conn, "r").unwrap_err(),
            PinnedReadError::Invalid
        );
    }
    for column in ["source_id", "repo_key", "header"] {
        let conn = corrupt_store();
        conn.execute_batch(
            "INSERT INTO receipt_headers VALUES ('h','src','repo','{}');
             UPDATE receipts SET header_id='h';",
        )
        .unwrap();
        for sql_value in ["NULL", "42", "X'7B7D'", "CAST(X'FFFF' AS TEXT)"] {
            conn.execute_batch(&format!("UPDATE receipt_headers SET {column}={sql_value}"))
                .unwrap();
            assert_eq!(
                stored_receipt(&conn, "r").unwrap_err(),
                PinnedReadError::Invalid
            );
        }
    }
    assert_eq!(
        stored_receipt(&Connection::open_in_memory().unwrap(), "r").unwrap_err(),
        PinnedReadError::Operational
    );
    let conn = corrupt_store();
    let mut utf8_boundary = "é".repeat(128).into_bytes();
    utf8_boundary.push(0xff);
    conn.execute(
        "UPDATE receipts SET repo_key=CAST(?1 AS TEXT)",
        [&utf8_boundary],
    )
    .unwrap();
    assert_eq!(
        stored_receipt(&conn, "r").unwrap_err(),
        PinnedReadError::Invalid
    );
    assert_eq!(
        stored_receipt(&conn, "missing").unwrap_err(),
        PinnedReadError::Unavailable
    );
}
