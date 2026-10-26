//! AC-0287: mixed-family retention/history reads use genuine production starts.
use super::*;
use completion::FinishFixture;

fn pinned() -> (
    tempfile::TempDir,
    JobStore,
    String,
    InvestigationReceiptReference,
) {
    // Independent target/request data only; this store has no converted tasks.
    let (seed, target) = source_validation::internal_source_invocation_fixture();
    let request = seed.request.clone();
    let provider = seed.task.detail.provider.clone();
    drop(seed);
    let directory = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
    let started = store
        .start_next_prompt_investigation(&request, Some(&provider))
        .unwrap();
    (
        directory,
        store,
        started.detail.summary.investigation_id,
        InvestigationReceiptReference {
            source_id: target.registered_source_id,
            repo_key: target.source.repo,
            receipt_id: target.receipt_id,
        },
    )
}

#[test]
fn investigation_reads_next_pinned_receipt_is_retained_before_any_input_exists() {
    let (directory, store, id, source) = pinned();
    let refs = store
        .investigation_receipt_references(&source.source_id)
        .unwrap();
    assert_eq!(
        refs,
        vec![InvestigationReceiptUse {
            investigation_id: id.clone(),
            source_id: source.source_id.clone(),
            repo_key: source.repo_key.clone(),
            receipt_id: source.receipt_id.clone()
        }]
    );
    assert!(
        store
            .investigation_receipt_references("unrelated-source")
            .unwrap()
            .is_empty()
    );
    let inputs: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM investigation_inputs", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(inputs, 0);
    let indexed: i64 = store
        .conn
        .query_row(
            "SELECT input_revision FROM investigation_refs WHERE task_id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexed, 0);
    let history = store.investigation_history(None, None).unwrap();
    assert_eq!(history.items.len(), 1);
    assert_eq!(history.items[0].schema_version, 7);
    assert_eq!(history.items[0].investigation_id, id);
    let events = store.investigation_events(&id, 0).unwrap();
    assert_eq!(
        events.items.last().unwrap().sequence,
        history.items[0].last_event_sequence
    );
    let reopened = JobStore::open(directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_receipt_references(&source.source_id)
            .unwrap(),
        refs
    );
}

#[test]
fn investigation_reads_next_ready_and_invocation_coexist_with_legacy_history() {
    let mut f = FinishFixture::new(true);
    let id = f.base.id().to_owned();
    let legacy = f
        .base
        .f
        .store
        .start_investigation(
            &StartInvestigationRequest {
                schema_version: 1,
                request_nonce: "receipt-sweep-legacy".into(),
                specialist_id: SpecialistId::DomainAnalyst,
                inspection_requirements: None,
                question: "Which evidence is available?".into(),
                scope: InvestigationScope::All,
                provider_mode: InvestigationProviderMode::Local,
                limit_profile: "investigation-v1".into(),
                expected_graph_revision: None,
                expected_context_id: None,
                conversation_id: None,
                parent_id: None,
            },
            Some(&f.base.f.task.detail.provider),
        )
        .unwrap()
        .detail
        .summary
        .investigation_id;
    for invocation in [false, true] {
        if invocation {
            f.begin();
        }
        let history = f.base.f.store.investigation_history(None, None).unwrap();
        assert_eq!(history.items.len(), 2);
        assert!(
            history
                .items
                .iter()
                .any(|row| row.investigation_id == legacy && row.schema_version == 1)
        );
        assert!(
            history
                .items
                .iter()
                .any(|row| row.investigation_id == id && row.schema_version == 7)
        );
        let legacy_search = f
            .base
            .f
            .store
            .search_investigation_history("which evidence is available", None)
            .unwrap();
        assert!(
            legacy_search
                .items
                .iter()
                .any(|row| row.investigation_id == legacy && row.schema_version == 1)
        );
        let current_question = f.base.f.task.detail.summary.question.clone();
        let current_search = f
            .base
            .f
            .store
            .search_investigation_history(&current_question, None)
            .unwrap();
        assert!(
            current_search
                .items
                .iter()
                .any(|row| row.investigation_id == id && row.schema_version == 7)
        );
        assert!(
            f.base
                .f
                .store
                .investigation_receipt_references("unrelated-source")
                .unwrap()
                .is_empty()
        );
        let mut after = 0;
        let mut all = vec![];
        loop {
            let page = f.base.f.store.investigation_events(&id, after).unwrap();
            after = page.next_sequence;
            all.extend(page.items);
            if !page.has_more {
                break;
            }
        }
        assert_eq!(after, f.base.f.task.detail.summary.last_event_sequence);
        assert_eq!(
            all.iter()
                .filter(|event| event.kind == InvestigationEventKind::ModelStarted)
                .count(),
            1 + usize::from(invocation)
        );
    }
}

#[test]
fn investigation_reads_pinned_index_corruption_fails_before_source_filtering() {
    for defect in ["delete", "relabel", "revision"] {
        let (_directory, store, id, source) = pinned();
        let trigger = if defect == "delete" {
            "investigation_refs_no_delete"
        } else {
            "investigation_refs_no_update"
        };
        let canonical: String = store
            .conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name=?1",
                [trigger],
                |r| r.get(0),
            )
            .unwrap();
        store
            .conn
            .execute_batch(&format!("DROP TRIGGER {trigger}"))
            .unwrap();
        match defect {
            "delete" => {
                store
                    .conn
                    .execute("DELETE FROM investigation_refs WHERE task_id=?1", [&id])
                    .unwrap();
            }
            "relabel" => {
                store.conn.execute("UPDATE investigation_refs SET source_id='different-source' WHERE task_id=?1",[&id]).unwrap();
            }
            "revision" => {
                store
                    .conn
                    .execute(
                        "UPDATE investigation_refs SET input_revision=1 WHERE task_id=?1",
                        [&id],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        store.conn.execute_batch(&canonical).unwrap();
        for filter in [&source.source_id, "unrelated-source"] {
            assert_eq!(
                store.investigation_receipt_references(filter),
                Err(InvestigationStoreError::Invalid),
                "{defect}"
            );
        }
    }
}
