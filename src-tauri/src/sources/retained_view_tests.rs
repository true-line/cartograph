use super::*;

fn fixture() -> (
    tempfile::TempDir,
    SourceRegistry,
    RegisteredSource,
    Connection,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("source");
    std::fs::create_dir(&root).unwrap();
    let state = directory.path().join("state.db");
    let mut registry = SourceRegistry::open(&state, directory.path()).unwrap();
    let source = registry.register_local(&root).unwrap();
    let writer = Connection::open(state).unwrap();
    (directory, registry, source, writer)
}

#[test]
fn retained_source_view_uses_active_writer_identity_without_checkout_access() {
    // AC-0287: the registry connection cannot see the writer's uncommitted edit.
    let (directory, registry, source, mut writer) = fixture();
    std::fs::remove_dir(source.root()).unwrap();
    let tx = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let view = RetainedSourceView::from_transaction(&tx);
    view.verify(&source).unwrap();
    let alternate = directory.path().join("replacement");
    tx.execute(
        "UPDATE registered_sources SET root=?1 WHERE source_id=?2",
        params![alternate.to_str().unwrap(), source.source_id],
    )
    .unwrap();
    assert_eq!(view.verify(&source), Err(RetainedSourceError::Changed));
    assert_eq!(
        registry
            .get_by_repo(&source.repo_key)
            .unwrap()
            .unwrap()
            .root(),
        source.root()
    );
    drop(tx);
    let tx = writer.transaction().unwrap();
    RetainedSourceView::from_transaction(&tx)
        .verify(&source)
        .unwrap();
}

#[test]
fn retained_source_view_rejects_deleted_reassigned_foreign_and_corrupt_registry() {
    // AC-0287: bounded real registry rows, no fallback registration or live graph.
    let (_directory, _registry, source, mut writer) = fixture();
    for sql in [
        "DELETE FROM registered_sources",
        "UPDATE registered_sources SET source_id='src_00000000000000000000000000000000',repo_key='local/src_00000000000000000000000000000000'",
        "UPDATE source_registry_meta SET registry_id='reg_00000000000000000000000000000000'",
        "UPDATE registered_sources SET display_name=printf('%0100000d',0)",
        "CREATE TRIGGER foreign_source_trigger AFTER UPDATE ON registered_sources BEGIN SELECT 1; END",
    ] {
        let tx = writer.transaction().unwrap();
        let view = RetainedSourceView::from_transaction(&tx);
        view.verify(&source).unwrap();
        tx.execute_batch(sql).unwrap();
        assert!(view.verify(&source).is_err(), "{sql}");
        // Even an already-used view must reject same-transaction mutation.
    }
}

#[test]
fn retained_source_view_requires_real_registry_for_nonempty_identity_set() {
    let (_directory, _registry, source, _) = fixture();
    let mut unrelated = Connection::open_in_memory().unwrap();
    let tx = unrelated.transaction().unwrap();
    let view = RetainedSourceView::from_transaction(&tx);
    assert_eq!(view.verify(&source), Err(RetainedSourceError::Operational));
}
