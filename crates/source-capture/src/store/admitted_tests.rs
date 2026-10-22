use super::*;
use crate::capture_working_tree;

fn capture(source: &str, bytes: &[u8]) -> Capture {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("source"), bytes).unwrap();
    capture_working_tree(
        root.path(),
        &SourceId::new(source).unwrap(),
        &["source".into()],
        CaptureLimits::default(),
    )
    .unwrap()
}

#[test]
fn selected_span_host_bounds_preserve_exact_bytes_and_legacy_policy() {
    // AC-0288: selected bytes cannot exceed admitted file/span materialization.
    let capture = capture("bounded-source", "café".as_bytes());
    let reference = capture.file("source").unwrap().span(0, 5).unwrap();
    let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
    store.persist(&capture).unwrap();
    assert_eq!(
        store.read_span_bounded(&reference, 5, 5).unwrap(),
        "café".as_bytes()
    );
    assert_eq!(
        store.read_text_span_bounded(&reference, 5, 5).unwrap(),
        "café"
    );
    assert!(matches!(
        store.read_span_bounded(&reference, 4, 5),
        Err(CaptureError::Limit("selected file bytes"))
    ));
    assert!(matches!(
        store.read_span_bounded(&reference, 5, 4),
        Err(CaptureError::Limit(_))
    ));
    assert_eq!(store.read_span(&reference).unwrap(), "café".as_bytes());
}

#[test]
fn selected_span_checks_object_metadata_before_copy_or_digest() {
    let capture = capture("bounded-source", b"abcd");
    let reference = capture.file("source").unwrap().span(0, 4).unwrap();
    let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
    store.persist(&capture).unwrap();
    // This corrupt oversized blob also has the wrong digest. The selected
    // reader rejects its metadata before the blob/digest stage; legacy APIs
    // keep their original validation order and global object policy.
    store
        .connection
        .execute("UPDATE objects SET bytes=zeroblob(8192)", [])
        .unwrap();
    assert!(matches!(
        store.read_span_bounded(&reference, 4, 4),
        Err(CaptureError::Corrupt("manifest and object length disagree"))
    ));
    assert!(matches!(
        store.read_span(&reference),
        Err(CaptureError::Corrupt(
            "stored object digest or length mismatch"
        ))
    ));
    assert!(matches!(
        read_object_admitted(&store.connection, &reference.file.digest, Some(8192), 4),
        Err(CaptureError::Limit("selected file bytes"))
    ));
}

#[test]
fn selected_capture_presence_validates_only_its_manifest_and_source() {
    let selected = capture("selected-source", b"selected");
    let unrelated = capture("unrelated-source", b"unrelated");
    let mut store = CaptureStore::in_memory(StoreLimits::default()).unwrap();
    store.persist(&selected).unwrap();
    store.persist(&unrelated).unwrap();
    let source = &selected.manifest().source_id;
    assert!(
        store
            .capture_belongs_to_source(source, selected.id())
            .unwrap()
    );
    assert!(
        !store
            .capture_belongs_to_source(source, unrelated.id())
            .unwrap()
    );
    store
        .connection
        .execute(
            "UPDATE captures SET manifest=x'FF' WHERE id=?1",
            [unrelated.id()],
        )
        .unwrap();
    assert!(store.source_inventory(source).is_err());
    assert!(
        store
            .capture_belongs_to_source(source, selected.id())
            .unwrap()
    );
    assert!(
        store
            .capture_belongs_to_source(source, unrelated.id())
            .is_err()
    );
    store
        .connection
        .execute("DELETE FROM captures WHERE id=?1", [selected.id()])
        .unwrap();
    assert!(
        !store
            .capture_belongs_to_source(source, selected.id())
            .unwrap()
    );
}
