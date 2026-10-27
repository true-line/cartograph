use super::*;
use crate::jobs::investigations::finding_reviews::FindingReviewError;

/// Exercise production execution admission and result publication, rather than
/// inserting a result DTO directly into SQL. The fixture makes no provider call.
fn completed_review_fixture(fixture: &mut Fixture, nonce: &str) -> (String, InvestigationResult) {
    let (id, execution) = fixture.start(nonce, InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, true);
    let pending = begin(fixture, &id, &execution, step(&ledger, 1));
    let result = InvestigationResult::admit(
        &id,
        &ledger,
        &InvestigationAction::Finish {
            findings: vec![
                ProposedInvestigationFinding {
                    claim_kind: InvestigationClaimKind::InferredInterpretation,
                    title: "Historical responsibility".into(),
                    statement: "The selected metadata suggests a responsibility worth examining."
                        .into(),
                    citation_ids: vec!["citation:graph".into()],
                    limitations: vec!["Implementation behavior remains unverified.".into()],
                },
                ProposedInvestigationFinding {
                    claim_kind: InvestigationClaimKind::ProposedDesign,
                    title: "Possible follow-up".into(),
                    statement:
                        "A future investigation could compare this responsibility with the design."
                            .into(),
                    citation_ids: vec!["citation:graph".into()],
                    limitations: vec![
                        "This is a proposed next step, not an implemented fact.".into(),
                    ],
                },
            ],
            knowledge_completeness: KnowledgeCompleteness::Partial,
            limitations: vec!["Only the saved input was inspected.".into()],
        },
        std::iter::empty(),
        Some("fixture-model".into()),
        "2026-10-22T00:00:00Z".into(),
    )
    .unwrap();
    fixture
        .store
        .advance_investigation(
            &execution,
            pending.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response("step:1", 10),
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        )
        .unwrap();
    (id, result)
}

fn review_request(
    observation: &FindingReviewObservation,
    decision: FindingReviewDecision,
) -> ReviewInvestigationFindingRequest {
    ReviewInvestigationFindingRequest {
        schema_version: 1,
        subject: observation.subject.clone(),
        expected_revision: observation.revision,
        decision,
    }
}

/// Snapshot every non-review investigation table plus Jobs as typed SQLite
/// values. This catches accidental lifecycle/counter/body/ref/event writes.
fn non_review_rows(store: &JobStore) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
    let names = store
        .conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND (name GLOB 'investigation_*' OR name='jobs') AND name != 'investigation_finding_reviews' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let mut statement = store
                .conn
                .prepare(&format!("SELECT * FROM {name} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (name, rows)
        })
        .collect()
}

// AC-0293/AC-0296: actual production transition path, original SQLite bytes and
// counters survive human review, reopen and supported finished-job cleanup.
#[test]
fn finding_review_persists_exact_finding_without_rewriting_history() {
    let mut fixture = Fixture::new();
    let (id, result) = completed_review_fixture(&mut fixture, "review-reopen");
    let original_result_bytes: (Vec<u8>, String) = fixture
        .store
        .conn
        .query_row(
            "SELECT body,body_hash FROM investigation_results WHERE task_id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let original_ledger = fixture.store.investigation_ledger(&id).unwrap();
    let original_detail = fixture.store.investigation(&id).unwrap();
    // Preserve an existing independently versioned optional read namespace.
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    read_reservations::initialize(&tx).unwrap();
    tx.commit().unwrap();
    let read_schema_before: Vec<(String, String)> = fixture.store.conn.prepare(
        "SELECT name,sql FROM sqlite_schema WHERE name GLOB 'investigation_read_*' ORDER BY name"
    ).unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?))).unwrap().collect::<Result<_,_>>().unwrap();
    // AC-0294: migrate a real completed result, not merely an empty store.
    storage::version_one_fixture(&fixture.store.conn);
    fixture.store = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        fixture.store.investigation_ledger(&id).unwrap(),
        original_ledger
    );
    assert_eq!(fixture.store.investigation(&id).unwrap(), original_detail);
    let migrated_result_bytes: (Vec<u8>, String) = fixture
        .store
        .conn
        .query_row(
            "SELECT body,body_hash FROM investigation_results WHERE task_id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(migrated_result_bytes, original_result_bytes);
    let read_schema_after: Vec<(String, String)> = fixture.store.conn.prepare(
        "SELECT name,sql FROM sqlite_schema WHERE name GLOB 'investigation_read_*' ORDER BY name"
    ).unwrap().query_map([], |row| Ok((row.get(0)?,row.get(1)?))).unwrap().collect::<Result<_,_>>().unwrap();
    assert_eq!(read_schema_after, read_schema_before);
    let before = non_review_rows(&fixture.store);
    let pending = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap();
    assert_eq!(pending.reviews.len(), 2);
    assert!(
        pending.reviews.iter().all(|row| {
            row.revision == 0 && row.decision.is_none() && row.reviewed_at.is_none()
        })
    );
    assert_eq!(before, non_review_rows(&fixture.store));
    let chosen = fixture
        .store
        .review_investigation_finding(&review_request(
            &pending.reviews[0],
            FindingReviewDecision::AcceptedForFollowUp,
        ))
        .unwrap();
    assert_eq!(chosen.revision, 1);
    assert!(chosen.reviewed_at.is_some());
    assert_eq!(before, non_review_rows(&fixture.store));
    let path = fixture.directory.path().join("state.db");
    let mut reopened = JobStore::open(&path).unwrap();
    assert_eq!(before, non_review_rows(&reopened));
    let reviews = reopened
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap();
    assert_eq!(reviews.reviews[0], chosen);
    assert_eq!(reviews.reviews[1], pending.reviews[1]);
    assert_eq!(
        reopened.investigation_result(&id).unwrap(),
        Some(result.clone())
    );
    let finished_job = reopened.investigation(&id).unwrap().summary.job_id;
    assert_eq!(reopened.clear_finished().unwrap(), 1);
    assert!(reopened.get(finished_job).is_err());
    assert_eq!(
        reopened
            .investigation_finding_reviews(&id, &result.result_id)
            .unwrap()
            .reviews[0],
        chosen
    );
    let after_cleanup = non_review_rows(&reopened);
    let mut reopened = JobStore::open(&path).unwrap();
    let changed = reopened
        .review_investigation_finding(&review_request(
            &chosen,
            FindingReviewDecision::RejectedForFollowUp,
        ))
        .unwrap();
    assert_eq!(changed.revision, 2);
    assert_eq!(
        changed.decision,
        Some(FindingReviewDecision::RejectedForFollowUp)
    );
    assert_eq!(after_cleanup, non_review_rows(&reopened));
    assert_eq!(reopened.investigation_result(&id).unwrap(), Some(result));
}

// AC-0294: independent file-backed connections race on one human revision;
// duplicate/lost-response retry cannot write, even for the winning decision.
#[test]
fn finding_review_two_connections_cas_returns_one_durable_winner() {
    let mut fixture = Fixture::new();
    let (id, result) = completed_review_fixture(&mut fixture, "review-cas");
    let initial = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap()
        .reviews
        .remove(0);
    let first_request = review_request(&initial, FindingReviewDecision::AcceptedForFollowUp);
    let second_request = review_request(&initial, FindingReviewDecision::RejectedForFollowUp);
    let mut other = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    // The contract is the CAS outcome, not lock-wait timing: a loaded runner
    // may hold the winner's write lock past the store's 1 s busy timeout
    // (#1246), so the race waits for the lock instead of failing on it.
    for store in [&fixture.store, &other] {
        store
            .conn
            .busy_timeout(std::time::Duration::from_secs(60))
            .unwrap();
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let second_barrier = barrier.clone();
    let thread = std::thread::spawn(move || {
        second_barrier.wait();
        other.review_investigation_finding(&second_request)
    });
    barrier.wait();
    let first = fixture.store.review_investigation_finding(&first_request);
    let second = thread.join().unwrap();
    let (winner, loser) = match (first, second) {
        (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner, loser),
        outcomes => panic!("expected exactly one committed review: {outcomes:?}"),
    };
    assert_eq!(winner.revision, 1);
    assert_eq!(
        loser,
        FindingReviewError::Conflict(Box::new(winner.clone()))
    );
    let third = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        third
            .investigation_finding_reviews(&id, &result.result_id)
            .unwrap()
            .reviews[0],
        winner
    );
    let mut retry = review_request(&initial, winner.decision.unwrap());
    assert_eq!(
        fixture.store.review_investigation_finding(&retry),
        Err(FindingReviewError::Conflict(Box::new(winner.clone())))
    );
    retry.expected_revision = 1;
    retry.decision = FindingReviewDecision::RejectedForFollowUp;
    assert_eq!(
        fixture
            .store
            .review_investigation_finding(&retry)
            .unwrap()
            .revision,
        2
    );
    assert_eq!(
        third
            .investigation_finding_reviews(&id, &result.result_id)
            .unwrap()
            .reviews[0]
            .revision,
        2
    );
}

// AC-0293: two genuine runs share result-local ordinals, never review identity.
#[test]
fn finding_review_cross_run_subject_mixes_never_create_rows() {
    let mut fixture = Fixture::new();
    let (first_id, first_result) = completed_review_fixture(&mut fixture, "review-cross-first");
    let (second_id, second_result) = completed_review_fixture(&mut fixture, "review-cross-second");
    let first = fixture
        .store
        .investigation_finding_reviews(&first_id, &first_result.result_id)
        .unwrap();
    let second = fixture
        .store
        .investigation_finding_reviews(&second_id, &second_result.result_id)
        .unwrap();
    assert_eq!(
        first.reviews[0].subject.finding_id,
        second.reviews[0].subject.finding_id
    );
    assert_ne!(
        first.reviews[0].subject.subject_id,
        second.reviews[0].subject.subject_id
    );
    let original = review_request(
        &first.reviews[0],
        FindingReviewDecision::AcceptedForFollowUp,
    );
    let mut mixed = original.clone();
    mixed.subject.investigation_id = second_id;
    assert!(fixture.store.review_investigation_finding(&mixed).is_err());
    mixed = original.clone();
    mixed.subject.result_id = second_result.result_id;
    assert!(fixture.store.review_investigation_finding(&mixed).is_err());
    mixed = original.clone();
    mixed.subject.finding_id = first.reviews[1].subject.finding_id.clone();
    assert!(fixture.store.review_investigation_finding(&mixed).is_err());
    mixed = original;
    mixed.subject.subject_id = second.reviews[0].subject.subject_id.clone();
    assert_eq!(
        fixture.store.review_investigation_finding(&mixed),
        Err(FindingReviewError::SubjectChanged)
    );
    let count: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM investigation_finding_reviews",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

// AC-0293/AC-0295: corrupt/orphan reviews fail closed without erasing the
// independently readable immutable result through the unchanged result API.
#[test]
fn finding_review_orphan_row_does_not_hide_legacy_result() {
    let mut fixture = Fixture::new();
    let (id, result) = completed_review_fixture(&mut fixture, "review-orphan");
    let reviews = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap();
    fixture.store.conn.execute(
        "INSERT INTO investigation_finding_reviews VALUES (?1,?2,'finding-missing',?3,?4,1,'accepted_for_follow_up','2026-10-22T00:00:00Z')",
        params![id,result.result_id,reviews.reviews[0].subject.subject_id,hash("unrelated-result")],
    ).unwrap();
    assert_eq!(
        fixture
            .store
            .investigation_finding_reviews(&id, &result.result_id),
        Err(FindingReviewError::Store(InvestigationStoreError::Invalid))
    );
    assert_eq!(
        fixture.store.investigation_result(&id).unwrap(),
        Some(result)
    );
}

// AC-0294: production write transaction rolls back first insert and later update
// if a failure occurs after SQL writes but before capacity checks/commit.
#[test]
fn finding_review_write_failure_rolls_back_insert_and_decision_change() {
    let mut fixture = Fixture::new();
    let (id, result) = completed_review_fixture(&mut fixture, "review-write-rollback");
    let pending = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap()
        .reviews
        .remove(0);
    let request = review_request(&pending, FindingReviewDecision::AcceptedForFollowUp);
    let before = non_review_rows(&fixture.store);
    assert_eq!(
        fixture
            .store
            .review_investigation_finding_with_hook(&request, || Err(
                InvestigationStoreError::Storage
            )),
        Err(FindingReviewError::Store(InvestigationStoreError::Storage))
    );
    let mut reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(non_review_rows(&reopened), before);
    assert_eq!(
        reopened
            .investigation_finding_reviews(&id, &result.result_id)
            .unwrap()
            .reviews[0],
        pending
    );
    let first = reopened.review_investigation_finding(&request).unwrap();
    let change = review_request(&first, FindingReviewDecision::RejectedForFollowUp);
    assert_eq!(
        reopened.review_investigation_finding_with_hook(&change, || Err(
            InvestigationStoreError::Storage
        )),
        Err(FindingReviewError::Store(InvestigationStoreError::Storage))
    );
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_finding_reviews(&id, &result.result_id)
            .unwrap()
            .reviews[0],
        first
    );
    assert_eq!(non_review_rows(&reopened), before);
}

// AC-0294: real retained SQL bytes consume existing task capacity, and a first
// review failing that capacity check leaves neither its row nor other changes.
#[test]
fn finding_review_first_insert_at_task_capacity_rolls_back() {
    let mut fixture = Fixture::new();
    let (id, result) = completed_review_fixture(&mut fixture, "review-capacity");
    let pending = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap()
        .reviews
        .remove(0);
    let request = review_request(&pending, FindingReviewDecision::AcceptedForFollowUp);
    let occupied = stored_bytes(&fixture.store.conn, Some(&id)).unwrap();
    let mut remaining = TASK_CAPACITY - occupied;
    let mut revision = 1;
    // Accounting-only archive fixtures, like existing SQL capacity regressions:
    // these unused bodies are not claimed as production-issued ledger history.
    // Each BLOB respects the existing input byte ceiling; stored checksums and
    // actual SQLite lengths (not fake counters) determine occupied capacity.
    while remaining >= 321 {
        let body_bytes = remaining.saturating_sub(320).min(MAX_INPUT_BYTES);
        let body = vec![b' '; body_bytes];
        fixture
            .store
            .conn
            .execute(
                "INSERT INTO investigation_inputs VALUES (?1,?2,?3,?4)",
                params![id, revision, body, core_prov::content_hash(&body)],
            )
            .unwrap();
        remaining -= body_bytes + 320;
        revision += 1;
    }
    assert!(remaining < 321);
    assert_eq!(
        stored_bytes(&fixture.store.conn, Some(&id)).unwrap(),
        TASK_CAPACITY - remaining
    );
    // The saved result still resolves its original valid ledger, never an
    // unused accounting fixture; review reaches its actual insert/capacity path.
    assert_eq!(
        fixture.store.investigation_result(&id).unwrap(),
        Some(result.clone())
    );
    let before = non_review_rows(&fixture.store);
    assert_eq!(
        fixture.store.review_investigation_finding(&request),
        Err(FindingReviewError::Store(InvestigationStoreError::Capacity))
    );
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(before, non_review_rows(&reopened));
    assert_eq!(
        reopened
            .investigation_finding_reviews(&id, &result.result_id)
            .unwrap()
            .reviews[0],
        pending
    );
    assert_eq!(reopened.investigation_result(&id).unwrap(), Some(result));
}

// AC-0293: saved configured identity is part of the subject independently of
// unchanged prose/result bytes and the separately reported finishing model.
#[test]
fn finding_review_subject_binds_saved_requested_provider_fields() {
    let mut fixture = Fixture::new();
    let (id, result) = completed_review_fixture(&mut fixture, "review-configured-provider");
    let original = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap()
        .reviews
        .remove(0);
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    let saved = load_execution_task(&tx, &id).unwrap();
    tx.commit().unwrap();
    let original_record = encode(&saved, MAX_RECORD_BYTES).unwrap();
    for field in ["model", "endpoint", "protocol"] {
        let mut changed = saved.clone();
        match field {
            "model" => changed.detail.provider.model = "different-saved-requested-model".into(),
            "endpoint" => changed.detail.provider.endpoint = "http://127.0.0.1:11435".into(),
            "protocol" => changed.detail.provider.protocol_version = Some("saved-wire-v2".into()),
            _ => unreachable!(),
        }
        let bytes = encode(&changed, MAX_RECORD_BYTES).unwrap();
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
                params![id, bytes, core_prov::content_hash(&bytes)],
            )
            .unwrap();
        let changed_review = fixture
            .store
            .investigation_finding_reviews(&id, &result.result_id)
            .unwrap()
            .reviews
            .remove(0);
        assert_ne!(
            changed_review.subject.subject_id, original.subject.subject_id,
            "saved {field}"
        );
        assert_eq!(
            fixture.store.investigation_result(&id).unwrap(),
            Some(result.clone())
        );
        assert_eq!(
            fixture.store.review_investigation_finding(&review_request(
                &original,
                FindingReviewDecision::AcceptedForFollowUp
            )),
            Err(FindingReviewError::SubjectChanged)
        );
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
                params![
                    id,
                    original_record,
                    core_prov::content_hash(&original_record)
                ],
            )
            .unwrap();
        assert_eq!(
            fixture
                .store
                .investigation_finding_reviews(&id, &result.result_id)
                .unwrap()
                .reviews[0],
            original
        );
    }
}

// AC-0293/0294/0295: bounded scalar text is not sufficient timestamp integrity.
// Exercise corruption through the real update guard (revision increments), and
// preserve the independent legacy result while review read/write fail closed.
#[test]
fn finding_review_rejects_noncanonical_and_invalid_calendar_timestamps() {
    let mut fixture = Fixture::new();
    let (id, result) = completed_review_fixture(&mut fixture, "review-timestamp");
    let pending = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap()
        .reviews
        .remove(0);
    let request = review_request(&pending, FindingReviewDecision::AcceptedForFollowUp);
    let saved = fixture
        .store
        .review_investigation_finding(&request)
        .unwrap();
    assert_eq!(saved.reviewed_at.as_ref().unwrap().len(), 20);
    let next = review_request(&saved, FindingReviewDecision::RejectedForFollowUp);
    for invalid in [
        "x",
        "2026-10-22",
        "2026-10-22T00:00:00+00:00",
        "2026-10-22T00:00:00.000Z",
        "2026-10-22t00:00:00Z",
        "2026-00-22T00:00:00Z",
        "2026-13-22T00:00:00Z",
        "2026-10-00T00:00:00Z",
        "2026-04-31T00:00:00Z",
        "2025-02-29T00:00:00Z",
        "1900-02-29T00:00:00Z",
        "2026-10-22T24:00:00Z",
        "2026-10-22T00:60:00Z",
        "2026-10-22T00:00:60Z",
        "2026-10-22T0x:00:00Z",
    ] {
        fixture.store.conn.execute("UPDATE investigation_finding_reviews SET review_revision=review_revision+1,reviewed_at=?1 WHERE task_id=?2", params![invalid,id]).unwrap();
        assert_eq!(
            fixture
                .store
                .investigation_finding_reviews(&id, &result.result_id),
            Err(FindingReviewError::Store(InvestigationStoreError::Invalid)),
            "{invalid}"
        );
        assert_eq!(
            fixture.store.review_investigation_finding(&next),
            Err(FindingReviewError::Store(InvestigationStoreError::Invalid)),
            "{invalid}"
        );
        assert_eq!(
            fixture.store.investigation_result(&id).unwrap(),
            Some(result.clone())
        );
    }
    for valid in [
        "2000-02-29T23:59:59Z",
        "2024-02-29T00:00:00Z",
        "2026-04-30T12:34:56Z",
    ] {
        fixture.store.conn.execute("UPDATE investigation_finding_reviews SET review_revision=review_revision+1,reviewed_at=?1 WHERE task_id=?2", params![valid,id]).unwrap();
        let observed = fixture
            .store
            .investigation_finding_reviews(&id, &result.result_id)
            .unwrap();
        assert_eq!(observed.reviews[0].reviewed_at.as_deref(), Some(valid));
    }
}

// AC-0294/0295: a valid near-MAX_RESULT_BYTES immutable result is independent
// of the review metadata envelope. Drive domain admission and real publication;
// this is a historical-format readability fixture, not a provider-output run.
#[test]
fn finding_review_preserves_near_limit_result_and_maximum_finding_metadata() {
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("review-largest-result", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, true);
    let pending = begin(&mut fixture, &id, &execution, step(&ledger, 1));
    let candidate = |limitation_bytes: usize| {
        InvestigationResult::admit(
            &id,
            &ledger,
            &InvestigationAction::Finish {
                findings: (0..12)
                    .map(|index| ProposedInvestigationFinding {
                        claim_kind: InvestigationClaimKind::InferredInterpretation,
                        title: format!("Historical finding {index}"),
                        statement: "Unverified historical interpretation. ".repeat(50),
                        citation_ids: vec!["citation:graph".into()],
                        limitations: vec!["x".repeat(limitation_bytes); 8],
                    })
                    .collect(),
                knowledge_completeness: KnowledgeCompleteness::Partial,
                limitations: vec![
                    "Historical format boundary fixture; not provider acceptance.".into(),
                ],
            },
            std::iter::empty(),
            Some("fixture-model".into()),
            "2026-10-22T00:00:00Z".into(),
        )
    };
    let (mut low, mut high) = (1usize, 512usize);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if candidate(middle).is_ok() {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let result = candidate(low).unwrap();
    let bytes = encode(&result, MAX_RESULT_BYTES).unwrap();
    assert!(
        bytes.len() > MAX_RESULT_BYTES - 128,
        "fixture must approach existing result ceiling: {}",
        bytes.len()
    );
    assert!(candidate(low + 1).is_err());
    fixture
        .store
        .advance_investigation(
            &execution,
            pending.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response("step:1", 10),
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        )
        .unwrap();
    let before: (Vec<u8>, String) = fixture
        .store
        .conn
        .query_row(
            "SELECT body,body_hash FROM investigation_results WHERE task_id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(before.0, bytes);
    assert_eq!(
        fixture.store.investigation_result(&id).unwrap(),
        Some(result.clone())
    );
    let metadata = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap();
    assert_eq!(metadata.reviews.len(), 12);
    let pending_json = serde_json::to_value(&metadata).unwrap();
    assert!(pending_json.get("result").is_none());
    assert!(serde_json::to_vec(&pending_json).unwrap().len() <= MAX_RECORD_BYTES);
    for review in metadata.reviews {
        let committed = fixture
            .store
            .review_investigation_finding(&review_request(
                &review,
                FindingReviewDecision::AcceptedForFollowUp,
            ))
            .unwrap();
        let json = serde_json::to_value(&committed).unwrap();
        assert!(json.get("result").is_none());
        assert!(json.get("findings").is_none());
        assert!(serde_json::to_vec(&json).unwrap().len() <= MAX_RECORD_BYTES);
    }
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let after: (Vec<u8>, String) = reopened
        .conn
        .query_row(
            "SELECT body,body_hash FROM investigation_results WHERE task_id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        reopened.investigation_result(&id).unwrap(),
        Some(result.clone())
    );
    let metadata = reopened
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap();
    assert!(
        metadata.reviews.iter().all(|row| row.revision == 1
            && row.decision == Some(FindingReviewDecision::AcceptedForFollowUp))
    );
    assert!(serde_json::to_vec(&metadata).unwrap().len() <= MAX_RECORD_BYTES);
}

// AC-0294: original pre-coverage stores can contain completed history. The
// production open path must apply both upgrades without rewriting any old row.
#[test]
fn finding_review_precoverage_upgrade_preserves_completed_history() {
    let mut fixture = Fixture::new();
    let (id, result) = completed_review_fixture(&mut fixture, "review-original-layout");
    storage::version_one_fixture(&fixture.store.conn);
    for (name, kind, _) in coverage::schema().into_iter().rev() {
        fixture
            .store
            .conn
            .execute_batch(&format!("DROP {} {name}", kind.to_uppercase()))
            .unwrap();
    }
    let originals: Vec<_> = non_review_rows(&fixture.store)
        .into_iter()
        .filter(|(name, _)| name != "investigation_meta")
        .collect();
    let path = fixture.directory.path().join("state.db");
    fixture.store = JobStore::open(&path).unwrap();
    let mut upgraded = non_review_rows(&fixture.store);
    // The namespace migration adds one indexed scalar; all original task cells
    // (including immutable record bytes and hash) must remain byte-for-byte.
    for (name, rows) in &mut upgraded {
        if name == "investigation_tasks" {
            for row in rows {
                assert_eq!(row.remove(3), rusqlite::types::Value::Text("app".into()));
            }
        }
    }
    for old in originals {
        assert!(upgraded.contains(&old), "old table {} changed", old.0);
    }
    assert_eq!(
        fixture.store.investigation_result(&id).unwrap(),
        Some(result.clone())
    );
    let versions: (i64,i64) = fixture.store.conn.query_row("SELECT (SELECT version FROM investigation_meta),(SELECT version FROM investigation_coverage_meta)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(versions, (4, 1));
    let pending = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap();
    assert!(pending.reviews.iter().all(|r| r.revision == 0));
    let mut reopened = JobStore::open(&path).unwrap();
    reopened
        .review_investigation_finding(&review_request(
            &pending.reviews[0],
            FindingReviewDecision::AcceptedForFollowUp,
        ))
        .unwrap();
    assert_eq!(reopened.investigation_result(&id).unwrap(), Some(result));
}

// AC-0294: exercise the defensive global check while the reviewed task itself
// has space. Fully admitted 128-task stores cannot independently hit this path
// (STORE_CAPACITY == MAX_TASKS * TASK_CAPACITY), so the other task's excess
// archive rows are explicitly accounting-only corruption fixtures, not a
// claimed production-admitted history. Actual SQLite lengths drive the check.
#[test]
fn finding_review_defensive_global_capacity_failure_rolls_back() {
    let mut fixture = Fixture::new();
    let (id, result) = completed_review_fixture(&mut fixture, "review-global-target");
    let (other, _) = completed_review_fixture(&mut fixture, "review-global-accounting");
    let pending = fixture
        .store
        .investigation_finding_reviews(&id, &result.result_id)
        .unwrap()
        .reviews
        .remove(0);
    assert!(stored_bytes(&fixture.store.conn, Some(&id)).unwrap() < TASK_CAPACITY / 2);
    let mut remaining = STORE_CAPACITY - stored_bytes(&fixture.store.conn, None).unwrap();
    let tx = fixture.store.conn.unchecked_transaction().unwrap();
    let mut ordinal = 1;
    while remaining >= 321 {
        let size = remaining.saturating_sub(320).min(MAX_INPUT_BYTES);
        let body = vec![b' '; size];
        tx.execute(
            "INSERT INTO investigation_inputs VALUES (?1,?2,?3,?4)",
            params![other, ordinal, body, core_prov::content_hash(&body)],
        )
        .unwrap();
        remaining -= size + 320;
        ordinal += 1;
    }
    tx.commit().unwrap();
    let occupied = stored_bytes(&fixture.store.conn, None).unwrap();
    assert_eq!(occupied, STORE_CAPACITY - remaining);
    let original = fixture.store.investigation(&id).unwrap();
    assert_eq!(
        fixture.store.review_investigation_finding(&review_request(
            &pending,
            FindingReviewDecision::AcceptedForFollowUp
        )),
        Err(FindingReviewError::Store(InvestigationStoreError::Capacity))
    );
    assert_eq!(stored_bytes(&fixture.store.conn, None).unwrap(), occupied);
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&id).unwrap(), original);
    assert_eq!(
        reopened
            .investigation_finding_reviews(&id, &result.result_id)
            .unwrap()
            .reviews[0],
        pending
    );
    assert_eq!(reopened.investigation_result(&id).unwrap(), Some(result));
}
