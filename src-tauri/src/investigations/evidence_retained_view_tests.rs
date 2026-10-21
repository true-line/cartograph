use super::*;

#[test]
fn retained_evidence_view_revalidates_held_guards_without_registry_or_jobs_mutex() {
    // AC-0287: reuse guards under the writer transaction, without fresh leases.
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("source");
    std::fs::create_dir(&root).unwrap();
    let state = crate::registered_source_tests::app_state(directory.path());
    let source = state.sources.lock().unwrap().register_local(&root).unwrap();
    let held = state.primary_sources.task_guard(&source.source_id).unwrap();
    let guards = EvidenceGuards {
        sources: BTreeMap::from([(source.repo_key.clone(), source.clone())]),
        guards: BTreeMap::from([(source.source_id.clone(), held)]),
        references: vec![],
    };
    // Captured metadata remains valid without a working checkout.
    std::fs::remove_dir(&root).unwrap();
    let _registry_mutex = state.sources.lock().unwrap();
    let _jobs_mutex = state.jobs.lock().unwrap();
    let mut writer = rusqlite::Connection::open(directory.path().join("state.db")).unwrap();
    let tx = writer
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let view = crate::sources::RetainedSourceView::from_transaction(&tx);
    guards
        .revalidate_in_view(&state.primary_sources, &view)
        .unwrap();
    tx.execute("DELETE FROM registered_sources", []).unwrap();
    assert!(matches!(
        guards.revalidate_in_view(&state.primary_sources, &view),
        Err(HostError::SourceUnavailable)
    ));
}

#[test]
fn retained_evidence_view_rejects_guard_from_different_retained_store() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("source");
    std::fs::create_dir(&root).unwrap();
    let state = crate::registered_source_tests::app_state(directory.path());
    let source = state.sources.lock().unwrap().register_local(&root).unwrap();
    let guard = state.primary_sources.task_guard(&source.source_id).unwrap();
    let guards = EvidenceGuards {
        sources: BTreeMap::from([(source.repo_key.clone(), source.clone())]),
        guards: BTreeMap::from([(source.source_id, guard)]),
        references: vec![],
    };
    let other = tempfile::tempdir().unwrap();
    let other_store = crate::primary_source::PrimarySourceStore::open(other.path()).unwrap();
    let mut writer = rusqlite::Connection::open(directory.path().join("state.db")).unwrap();
    let tx = writer.transaction().unwrap();
    let view = crate::sources::RetainedSourceView::from_transaction(&tx);
    assert!(guards.revalidate_in_view(&other_store, &view).is_err());
}
