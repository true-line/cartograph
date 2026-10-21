use super::*;

fn assert_same_metadata(
    expected: &ProtectedCompletionMetadata,
    actual: &ProtectedCompletionMetadata,
    policy: &InvestigationPolicy,
) {
    assert_eq!(expected, actual);
    assert_eq!(
        serde_json::to_vec(expected).unwrap(),
        serde_json::to_vec(actual).unwrap()
    );
    assert_eq!(
        expected.fingerprint_with_policy(policy).unwrap(),
        actual.fingerprint_with_policy(policy).unwrap()
    );
}

#[test]
fn prospective_metadata_matches_saved_projection_and_invocation_cutoff() {
    // AC-0287 groundwork: extraction preserves initial, latest-refusal and
    // historical strict-before-invocation semantics in every existing family.
    for (policy, progressive) in [
        (&INVESTIGATION_V1_POLICY, false),
        (&INVESTIGATION_V2_POLICY, false),
        (&INVESTIGATION_V2_POLICY, true),
    ] {
        let mut fixture =
            Fixture::with_family(InvestigationProviderMode::Local, policy, progressive);
        let initial = {
            let tx = fixture.store.conn.unchecked_transaction().unwrap();
            let task = load_task(&tx, &fixture.id).unwrap();
            let projected = derive_protected_metadata(&task, vec![], &[]).unwrap();
            assert_same_metadata(&projected, &metadata(&tx, &task, None).unwrap(), policy);
            tx.commit().unwrap();
            projected
        };
        fixture.refuse();
        let first_step = {
            let tx = fixture.store.conn.unchecked_transaction().unwrap();
            let task = load_task(&tx, &fixture.id).unwrap();
            let refusals = raw_refusals(&tx, &task).unwrap();
            let step = latest_step(&tx, &fixture.id).unwrap();
            let projected =
                derive_protected_metadata(&task, vec![], &[&refusals[0].record]).unwrap();
            assert_same_metadata(&projected, &metadata(&tx, &task, None).unwrap(), policy);
            assert_same_metadata(
                &initial,
                &metadata(&tx, &task, Some(&step)).unwrap(),
                policy,
            );
            // Historical unknown-step behavior is unchanged: no resolved cutoff
            // means the complete current journal, rather than a synthetic cut.
            assert_same_metadata(
                &projected,
                &metadata(&tx, &task, Some("not-invoked")).unwrap(),
                policy,
            );
            tx.commit().unwrap();
            step
        };
        fixture.refuse();
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let task = load_task(&tx, &fixture.id).unwrap();
        let refusals = raw_refusals(&tx, &task).unwrap();
        let second_step = latest_step(&tx, &fixture.id).unwrap();
        let projected = derive_protected_metadata(&task, vec![], &[&refusals[0].record]).unwrap();
        assert_same_metadata(
            &projected,
            &metadata(&tx, &task, Some(&second_step)).unwrap(),
            policy,
        );
        assert_same_metadata(
            &initial,
            &metadata(&tx, &task, Some(&first_step)).unwrap(),
            policy,
        );
        assert!(metadata(&tx, &task, None).is_err());
        assert!(
            derive_protected_metadata(&task, vec![], &[&refusals[0].record, &refusals[1].record])
                .is_err()
        );
        tx.commit().unwrap();
    }
}

#[test]
fn prospective_metadata_appended_attempt_ids_equal_committed_protected_metadata() {
    // AC-0287 groundwork: a pure attempt can be projected into the next metadata
    // without rows, then exactly equals the metadata after the real transition.
    let policy = &INVESTIGATION_V2_POLICY;
    let mut fixture = Fixture::with_family(InvestigationProviderMode::Local, policy, true);
    fixture.refuse();
    let (_, step) = fixture.step();
    let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0] else {
        unreachable!()
    };
    let read = ReadIntent::Inventory {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: 1,
    };
    let response = Fixture::response(&step, staged::read_action_hash(&read).unwrap());
    fixture
        .advance(InvestigationTransition::ActionAdmitted {
            response: response.clone(),
            tool: Some(InvestigationTool::ReadCoverageInventory),
            result: None,
        })
        .unwrap();
    fixture
        .advance(InvestigationTransition::BeginTool {
            tool: InvestigationTool::ReadCoverageInventory,
        })
        .unwrap();
    let prospective = {
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let task = load_task(&tx, &fixture.id).unwrap();
        let ledger = load_ledger(&tx, &task).unwrap().unwrap();
        let existing = metadata(&tx, &task, None).unwrap();
        let records = derive_attempt_records(
            &task,
            &step.step_id,
            &response,
            &read,
            &AttemptOutcome::Unavailable {
                reason: UnavailableReason::InventoryExceedsMaxBytes,
            },
            Some(&ledger),
            existing.completed_attempt_ids.len(),
        )
        .unwrap();
        let mut ids = existing.completed_attempt_ids;
        ids.extend(records.into_iter().map(|record| record.attempt_id));
        let refusals = raw_refusals(&tx, &task).unwrap();
        let projected = derive_protected_metadata(&task, ids, &[&refusals[0].record]).unwrap();
        assert_eq!(projected.completed_attempt_ids.len(), 1);
        assert!(attempts(&tx, &task).unwrap().is_empty());
        tx.commit().unwrap();
        projected
    };
    fixture
        .advance(InvestigationTransition::ReadUnavailable {
            read,
            reason: UnavailableReason::InventoryExceedsMaxBytes,
        })
        .unwrap();
    let committed = fixture
        .store
        .investigation_completion_metadata(&fixture.id)
        .unwrap();
    assert_same_metadata(&prospective, &committed, policy);
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_same_metadata(
        &prospective,
        &reopened
            .investigation_completion_metadata(&fixture.id)
            .unwrap(),
        policy,
    );
}

#[test]
fn prospective_metadata_preserves_existing_shape_limits_and_intent_binding() {
    // AC-0287 groundwork: use existing metadata validation, without upgrading
    // this pure projection into authentication of unavailable journal evidence.
    let policy = &INVESTIGATION_V2_POLICY;
    let mut fixture = Fixture::registered_v2();
    fixture.refuse();
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    let mut task = load_task(&tx, &fixture.id).unwrap();
    let refusals = raw_refusals(&tx, &task).unwrap();
    let refusal = &refusals[0].record;
    let limit = policy.max_completed_attempts();
    let ids = (0..limit)
        .map(|index| format!("{index:064x}"))
        .collect::<Vec<_>>();
    derive_protected_metadata(&task, ids.clone(), &[refusal]).unwrap();
    let mut over = ids;
    over.push(format!("{limit:064x}"));
    assert!(derive_protected_metadata(&task, over, &[refusal]).is_err());
    assert!(derive_protected_metadata(&task, vec!["a".repeat(64); 2], &[refusal]).is_err());
    assert!(derive_protected_metadata(&task, vec!["not-a-hash".into()], &[refusal]).is_err());
    assert!(derive_protected_metadata(&task, vec![], &[refusal, refusal]).is_err());
    let mut malformed = refusal.clone();
    malformed.refusal_id = "not-a-hash".into();
    assert!(derive_protected_metadata(&task, vec![], &[&malformed]).is_err());
    malformed = refusal.clone();
    malformed.feedback.schema_version = 2;
    assert!(derive_protected_metadata(&task, vec![], &[&malformed]).is_err());
    let actual = derive_protected_metadata(&task, vec![], &[refusal]).unwrap();
    let mut unprojected = refusal.clone();
    unprojected.request_hash = "b".repeat(64);
    unprojected.refusal_count = 99;
    // Journal authentication belongs to the loader. Old metadata construction
    // only copied latest ID/feedback and derived count from the ordered slice.
    assert_same_metadata(
        &actual,
        &derive_protected_metadata(&task, vec![], &[&unprojected]).unwrap(),
        policy,
    );
    let request_hash = task
        .completion_intent
        .as_ref()
        .unwrap()
        .request_hash
        .clone();
    task.completion_intent.as_mut().unwrap().request_hash = "c".repeat(64);
    assert!(derive_protected_metadata(&task, vec![], &[refusal]).is_err());
    task.completion_intent.as_mut().unwrap().request_hash = request_hash;
    task.completion_intent.as_mut().unwrap().obligations_hash = "d".repeat(64);
    assert!(derive_protected_metadata(&task, vec![], &[refusal]).is_err());
    tx.commit().unwrap();
}
