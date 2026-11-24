//! Input closures retained beside the contract-v4 receipts that name them
//! (ADR-0072 slice 5, AC-1082).

use super::tests::{fixture, schema_objects};
use super::*;
use captured::{ClosureInput, ClosureParameters, InputClosureV1, ReceiptClosure};

fn closure_for(source: &RegisteredSource, capture: &Capture) -> InputClosureV1 {
    InputClosureV1::new(
        "t0.adapter-ts/direct-lexical-v4",
        vec![
            "tree-sitter@0.27.2".into(),
            "tree-sitter-typescript@0.23.2".into(),
        ],
        ClosureParameters {
            repo_key: source.repo_key.clone(),
            walk_policy: "walk-v1".into(),
            layer_hints: vec![],
            grammar_rule: "grammar-by-path-v1".into(),
        },
        vec![ClosureInput::CapturedFile {
            file: capture.file("rule.ts").unwrap().reference().clone(),
        }],
    )
    .unwrap()
}

fn established(receipts: &[Receipt], closure: &InputClosureV1) -> Vec<Receipt> {
    receipts
        .iter()
        .map(|receipt| {
            receipt
                .clone()
                .with_closure(ReceiptClosure::Established {
                    closure_id: closure.closure_id.clone(),
                })
                .unwrap()
        })
        .collect()
}

fn count(store: &ReceiptStore, table: &str) -> i64 {
    store
        .conn
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn persist(
    store: &mut ReceiptStore,
    capture: &Capture,
    source: &RegisteredSource,
    receipts: &[Receipt],
    closures: &[InputClosureV1],
) -> Result<RetainedReceipts, String> {
    let share = store.capacity;
    store.persist_within(capture, source, receipts, closures, share)
}

// AC-1082: a v4 receipt naming an established closure is retained only with
// that closure, in the same transaction, and compactly; it reads back byte
// for byte and the closure decodes to the record that was bound.
#[test]
fn a_v4_receipt_is_retained_only_with_its_closure() {
    let (_directory, source, capture, _, receipts) = fixture();
    let closure = closure_for(&source, &capture);
    let v4 = established(&receipts, &closure);
    let directory = tempfile::tempdir().unwrap();
    let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();

    assert!(persist(&mut store, &capture, &source, &v4, &[]).is_err());
    assert_eq!(
        (count(&store, "receipts"), count(&store, "input_closures")),
        (0, 0)
    );

    let mut other = source.clone();
    other.repo_key = "other".into();
    let foreign = closure_for(&other, &capture);
    assert!(persist(&mut store, &capture, &source, &v4, &[foreign]).is_err());
    assert_eq!(count(&store, "receipts"), 0);

    let retained = persist(
        &mut store,
        &capture,
        &source,
        &v4,
        std::slice::from_ref(&closure),
    )
    .unwrap();
    assert_eq!(retained.ids.len(), v4.len());
    assert_eq!(count(&store, "input_closures"), 1);
    for receipt in &v4 {
        let (payload, header_id, closure_id): (String, Option<String>, Option<String>) = store
            .conn
            .query_row(
                "SELECT payload, header_id, closure_id FROM receipts WHERE id = ?1",
                [receipt.id()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(header_id.is_some(), "a v4 row is stored compactly");
        assert!(payload.len() < receipt.to_json().unwrap().len());
        assert_eq!(closure_id.as_deref(), Some(closure.closure_id.as_str()));
        let loaded = store.get(receipt.id()).unwrap();
        assert_eq!(&loaded, receipt);
        assert_eq!(loaded.to_json().unwrap(), receipt.to_json().unwrap());
    }
    assert_eq!(store.closure(&closure.closure_id).unwrap(), closure);

    // Already stored, the closure need not be supplied again; v3 receipts
    // name none.
    persist(&mut store, &capture, &source, &v4, &[]).unwrap();
    persist(&mut store, &capture, &source, &receipts, &[]).unwrap();
    let unnamed: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM receipts WHERE closure_id IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unnamed, receipts.len() as i64);
    assert_eq!(count(&store, "input_closures"), 1);
}

// AC-1082: a closure is charged once against the store and the source's
// share, and a receipt without room is omitted without storing it.
#[test]
fn a_closure_is_charged_once_and_only_when_a_receipt_is_retained() {
    let (_directory, source, capture, _, receipts) = fixture();
    let closure = closure_for(&source, &capture);
    let v4 = established(&receipts, &closure);
    let closure_bytes = closure.canonical_bytes().unwrap().len() as u64;
    let directory = tempfile::tempdir().unwrap();
    let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();

    let first = normalize_v3_receipt(&v4[0], &v4[0].to_json().unwrap()).unwrap();
    let first_cost = (first.header_json.len() + first.body_json.len()) as u64 + closure_bytes;
    store.capacity.1 = first_cost - 1;
    let retained = persist(
        &mut store,
        &capture,
        &source,
        &v4[..1],
        std::slice::from_ref(&closure),
    )
    .unwrap();
    assert!(retained.ids.is_empty());
    assert_eq!(count(&store, "input_closures"), 0);

    store.capacity.1 = first_cost;
    let retained = persist(
        &mut store,
        &capture,
        &source,
        &v4[..1],
        std::slice::from_ref(&closure),
    )
    .unwrap();
    assert_eq!(retained.charged, (1, first_cost));
    assert_eq!(receipt_storage(&store.conn).unwrap().1, first_cost);

    store.capacity.1 = MAX_RECEIPT_STORE_BYTES;
    let retained = persist(
        &mut store,
        &capture,
        &source,
        &v4,
        std::slice::from_ref(&closure),
    )
    .unwrap();
    let bodies: u64 = v4
        .iter()
        .map(|receipt| {
            normalize_v3_receipt(receipt, &receipt.to_json().unwrap())
                .unwrap()
                .body_json
                .len() as u64
        })
        .sum();
    let expected = bodies + first.header_json.len() as u64 + closure_bytes;
    assert_eq!(retained.charged, (v4.len() as u64, expected));
    assert_eq!(
        receipt_storage(&store.conn).unwrap(),
        (v4.len() as u64, expected)
    );
}

// AC-1082: forgetting a source keeps a closure while a pinned receipt still
// names it, and reclaims it with the last receipt that does.
#[test]
fn a_closure_lives_exactly_as_long_as_a_receipt_names_it() {
    let (_directory, source, capture, _, receipts) = fixture();
    let closure = closure_for(&source, &capture);
    let v4 = established(&receipts, &closure);
    let directory = tempfile::tempdir().unwrap();
    let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
    persist(
        &mut store,
        &capture,
        &source,
        &v4,
        std::slice::from_ref(&closure),
    )
    .unwrap();

    let pins = std::collections::BTreeSet::from([v4[0].id().to_owned()]);
    store.reclaim_forgotten(&source, &[], &pins).unwrap();
    assert_eq!(count(&store, "receipts"), 1);
    assert_eq!(store.closure(&closure.closure_id).unwrap(), closure);
    assert_eq!(&store.get(v4[0].id()).unwrap(), &v4[0]);

    store
        .reclaim_forgotten(&source, &[], &Default::default())
        .unwrap();
    assert_eq!(count(&store, "receipts"), 0);
    assert_eq!(count(&store, "input_closures"), 0);
    assert!(store.closure(&closure.closure_id).is_err());
}

// AC-1082: a closure row no receipt names, one owned by another source, one
// that is not canonical, or a receipt row naming another closure than its
// receipt records, fails closed.
#[test]
fn a_misbound_closure_fails_closed() {
    let (_directory, source, capture, _, receipts) = fixture();
    let closure = closure_for(&source, &capture);
    let v4 = established(&receipts, &closure);
    let stray = format!("closure-v1:{}", "0".repeat(64));
    for (mutation, store_fails) in [
        (
            format!(
                "INSERT INTO input_closures VALUES ('{stray}', '{}', '{}', '{{}}')",
                source.source_id, source.repo_key
            ),
            true,
        ),
        (
            "UPDATE input_closures SET repo_key = 'other'".to_owned(),
            true,
        ),
        (
            "UPDATE input_closures SET id = 'closure-v1:x'".to_owned(),
            true,
        ),
        (
            "UPDATE input_closures SET payload = replace(payload, 'walk-v1', 'walk-v2')".to_owned(),
            false,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        persist(
            &mut store,
            &capture,
            &source,
            &v4,
            std::slice::from_ref(&closure),
        )
        .unwrap();
        store
            .conn
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        store.conn.execute_batch(&mutation).unwrap();
        assert!(store.closure(&closure.closure_id).is_err(), "{mutation}");
        assert_eq!(store.get(v4[0].id()).is_err(), store_fails, "{mutation}");
    }

    // One receipt row stops naming the closure its receipt records.
    let directory = tempfile::tempdir().unwrap();
    let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
    persist(
        &mut store,
        &capture,
        &source,
        &v4,
        std::slice::from_ref(&closure),
    )
    .unwrap();
    store
        .conn
        .execute(
            "UPDATE receipts SET closure_id = NULL WHERE id = ?1",
            [v4[0].id()],
        )
        .unwrap();
    assert!(store.get(v4[0].id()).is_err());
    assert!(store.ids(&source.source_id).is_err());
    assert_eq!(&store.get(v4[1].id()).unwrap(), &v4[1]);
}

// AC-1082: an exact v4 store migrates to v5 on open, keeping its rows, and
// matches a fresh store's schema.
#[test]
fn a_v4_store_migrates_to_v5() {
    let (_directory, source, capture, _, receipts) = fixture();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("v4-receipts.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        for ddl in [
            RECEIPTS_SCHEMA_V4,
            HEADERS_SCHEMA,
            TOMBSTONES_SCHEMA,
            RECLAIMS_SCHEMA,
        ] {
            conn.execute_batch(ddl).unwrap();
        }
        for (_, ddl) in &RECEIPT_INDEXES[..4] {
            conn.execute_batch(ddl).unwrap();
        }
        conn.execute_batch(META_SCHEMA_V4).unwrap();
        conn.execute("INSERT INTO receipt_meta VALUES (4)", [])
            .unwrap();
        for receipt in &receipts {
            conn.execute(
                "INSERT INTO receipts (id, source_id, repo_key, payload) VALUES (?1, ?2, ?3, ?4)",
                params![
                    receipt.id(),
                    source.source_id,
                    source.repo_key,
                    receipt.to_json().unwrap()
                ],
            )
            .unwrap();
        }
    }
    let mut store = ReceiptStore::open(&path).unwrap();
    let fresh = ReceiptStore::open(&directory.path().join("fresh.sqlite")).unwrap();
    assert_eq!(schema_objects(&store.conn), schema_objects(&fresh.conn));
    let version: i64 = store
        .conn
        .query_row("SELECT version FROM receipt_meta", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);
    for receipt in &receipts {
        assert_eq!(&store.get(receipt.id()).unwrap(), receipt);
    }
    let closure = closure_for(&source, &capture);
    let v4 = established(&receipts, &closure);
    persist(&mut store, &capture, &source, &v4, &[closure]).unwrap();
    drop(store);
    assert_eq!(
        ReceiptStore::open(&path)
            .unwrap()
            .ids(&source.source_id)
            .unwrap()
            .len(),
        2 * receipts.len()
    );
}
