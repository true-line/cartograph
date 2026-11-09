use super::*;

fn inventory_read(fixture: &Fixture, max_bytes: usize) -> ReadIntent {
    let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0] else {
        unreachable!()
    };
    ReadIntent::Inventory {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes,
    }
}

fn begin_read(fixture: &mut Fixture, read: &ReadIntent) -> (String, InvestigationResponseMeta) {
    let (_, step) = fixture.step();
    let response = Fixture::response(&step, staged::read_action_hash(read).unwrap());
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
    (step.step_id, response)
}

#[test]
fn prospective_attempt_records_equal_committed_records_for_acquired_and_unavailable_reads() {
    // AC-0287 groundwork: pure preview and actual publication share exact IDs
    // and bytes in all existing completion families; preview publishes nothing.
    for (policy, progressive) in [
        (&INVESTIGATION_V1_POLICY, false),
        (&INVESTIGATION_V2_POLICY, false),
        (&INVESTIGATION_V2_POLICY, true),
    ] {
        for acquired in [false, true] {
            let mut fixture =
                Fixture::with_family(InvestigationProviderMode::Local, policy, progressive);
            let read = inventory_read(&fixture, if acquired { MAX_QUERY_BYTES } else { 1 });
            let (step, response) = begin_read(&mut fixture, &read);
            let mut ledger = fixture
                .store
                .investigation_ledger(&fixture.id)
                .unwrap()
                .unwrap();
            let outcome = if acquired {
                let ReadIntent::Inventory {
                    context_id,
                    repo,
                    inventory_hash,
                    max_bytes,
                } = &read
                else {
                    unreachable!()
                };
                let request = InvestigationCoverageRead {
                    context_id: context_id.clone(),
                    repo: repo.clone(),
                    inventory_hash: inventory_hash.clone(),
                    max_bytes: *max_bytes,
                };
                let raw = serde_json::to_vec(&fixture.archive.read(&request).unwrap()).unwrap();
                ledger.revision += 1;
                ledger
                    .coverage_reads
                    .push(InvestigationCoverageReadManifest {
                        request,
                        response_hash: core_prov::content_hash(&raw),
                        response_bytes: raw.len(),
                    });
                AttemptOutcome::InventoryAcquired { read_index: 0 }
            } else {
                AttemptOutcome::Unavailable {
                    reason: UnavailableReason::InventoryExceedsMaxBytes,
                }
            };
            let prospective = {
                let tx = fixture.store.conn.unchecked_transaction().unwrap();
                let task = load_task(&tx, &fixture.id).unwrap();
                let prior = attempts(&tx, &task).unwrap().len();
                let prospective = derive_attempt_records(
                    &task,
                    &step,
                    &response,
                    &read,
                    &outcome,
                    Some(&ledger),
                    prior,
                )
                .unwrap();
                assert_eq!(prospective.len(), 1);
                let rows: i64 = tx
                    .query_row(
                        "SELECT count(*) FROM completion_attempts WHERE task_id=?1",
                        [&fixture.id],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(rows, 0);
                // Validate the same original intent binding used by reopen.
                prospective[0]
                    .validate_with_policy(&fixture.request, &fixture.id, &ledger, policy)
                    .unwrap();
                tx.commit().unwrap();
                prospective
            };
            let expected_revision = ledger.revision;
            if acquired {
                let usage = fixture.store.investigation(&fixture.id).unwrap().usage;
                fixture
                    .advance(InvestigationTransition::InputAdmitted {
                        ledger: Box::new(ledger),
                        usage,
                    })
                    .unwrap();
            } else {
                fixture
                    .advance(InvestigationTransition::ReadUnavailable {
                        read,
                        reason: UnavailableReason::InventoryExceedsMaxBytes,
                    })
                    .unwrap();
            }
            let committed = fixture
                .store
                .investigation_completion_attempts(&fixture.id)
                .unwrap();
            assert_eq!(committed, prospective);
            assert_eq!(
                serde_json::to_vec(&committed).unwrap(),
                serde_json::to_vec(&prospective).unwrap()
            );
            assert_eq!(committed[0].input_revision, expected_revision);
            let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
            assert_eq!(
                reopened
                    .investigation_completion_attempts(&fixture.id)
                    .unwrap(),
                prospective
            );
        }
    }
}

#[test]
fn prospective_attempt_derivation_rejects_mismatched_authority_outcome_and_capacity() {
    // AC-0287 groundwork: preview is not permission to invent an action, intent,
    // ledger acquisition or extra completed-attempt slot.
    let mut fixture = Fixture::registered_v2();
    let read = inventory_read(&fixture, 1);
    let (step, response) = begin_read(&mut fixture, &read);
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    let mut task = load_task(&tx, &fixture.id).unwrap();
    let ledger = load_ledger(&tx, &task).unwrap().unwrap();
    let outcome = AttemptOutcome::Unavailable {
        reason: UnavailableReason::InventoryExceedsMaxBytes,
    };
    let derive = |task: &StoredTask,
                  step: &str,
                  response: &InvestigationResponseMeta,
                  outcome: &AttemptOutcome,
                  ledger,
                  count| {
        derive_attempt_records(task, step, response, &read, outcome, ledger, count)
    };
    assert_eq!(
        derive(&task, &step, &response, &outcome, Some(&ledger), 0)
            .unwrap()
            .len(),
        1
    );
    let mut foreign = response.clone();
    foreign.step_id.push_str("-foreign");
    assert!(derive(&task, &step, &foreign, &outcome, Some(&ledger), 0).is_err());
    foreign = response.clone();
    foreign.action_hash = "a".repeat(64);
    assert!(derive(&task, &step, &foreign, &outcome, Some(&ledger), 0).is_err());
    let original_request = task
        .completion_intent
        .as_ref()
        .unwrap()
        .request_hash
        .clone();
    task.completion_intent.as_mut().unwrap().request_hash = "b".repeat(64);
    assert!(derive(&task, &step, &response, &outcome, Some(&ledger), 0).is_err());
    task.completion_intent.as_mut().unwrap().request_hash = original_request;
    assert!(derive(&task, &step, &response, &outcome, None, 0).is_err());
    assert!(
        derive(
            &task,
            &step,
            &response,
            &AttemptOutcome::InventoryAcquired { read_index: 0 },
            Some(&ledger),
            0
        )
        .is_err()
    );
    assert!(
        derive(
            &task,
            &step,
            &response,
            &AttemptOutcome::Unavailable {
                reason: UnavailableReason::PinnedTargetAbsent
            },
            Some(&ledger),
            0
        )
        .is_err()
    );
    assert!(
        derive(
            &task,
            &step,
            &response,
            &AttemptOutcome::FirewallRefused {
                acquired_attempt_id: "a".repeat(64),
                observation_hash: "b".repeat(64)
            },
            Some(&ledger),
            0
        )
        .is_err()
    );
    let limit = saved_policy(&task).unwrap().max_completed_attempts();
    assert_eq!(
        derive(&task, &step, &response, &outcome, Some(&ledger), limit - 1)
            .unwrap()
            .len(),
        1
    );
    for count in [limit, limit + 1, usize::MAX] {
        assert!(matches!(
            derive(&task, &step, &response, &outcome, Some(&ledger), count),
            Err(InvestigationStoreError::Capacity)
        ));
    }
    assert!(attempts(&tx, &task).unwrap().is_empty());
    tx.commit().unwrap();
}

#[test]
fn prospective_attempt_zero_matches_preserve_no_ledger_incidental_reads() {
    // AC-0287 groundwork: no obligated observation means no ledger lookup or
    // record validation, preserving the legacy incidental-read path.
    let fixture = Fixture::new();
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    let task = load_task(&tx, &fixture.id).unwrap();
    let mut read = inventory_read(&fixture, 1);
    if let ReadIntent::Inventory { repo, .. } = &mut read {
        repo.push_str("-incidental");
    }
    let response = InvestigationResponseMeta {
        finish_feedback_witness: None,
        finish_telemetry: None,
        step_id: "incidental-step".into(),
        response_hash: "a".repeat(64),
        action_hash: staged::read_action_hash(&read).unwrap(),
        observed_model: None,
        reported_input_tokens: None,
        reported_output_tokens: None,
        reported_cache_read_tokens: None,
        active_milliseconds: 0,
    };
    let outcome = AttemptOutcome::Unavailable {
        reason: UnavailableReason::InventoryExceedsMaxBytes,
    };
    assert!(
        derive_attempt_records(
            &task,
            &response.step_id,
            &response,
            &read,
            &outcome,
            None,
            0
        )
        .unwrap()
        .is_empty()
    );
    let mut invalid_ledger = load_ledger(&tx, &task).unwrap().unwrap();
    invalid_ledger.schema_version = u32::MAX;
    assert!(
        derive_attempt_records(
            &task,
            &response.step_id,
            &response,
            &read,
            &outcome,
            Some(&invalid_ledger),
            0
        )
        .unwrap()
        .is_empty()
    );
    let mut mismatched = response.clone();
    mismatched.action_hash = "b".repeat(64);
    assert!(
        derive_attempt_records(
            &task,
            &response.step_id,
            &mismatched,
            &read,
            &outcome,
            None,
            0
        )
        .is_err()
    );
    assert!(attempts(&tx, &task).unwrap().is_empty());
    tx.commit().unwrap();
}
