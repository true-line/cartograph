//! AC-0287 / #1695: admission creates correctness; reopen checks independent integrity.
use super::*;

#[test]
fn analyst22_latest_refusal_requires_independent_feedback_witness() {
    assert_latest_feedback_witness("domain-analyst@22");
}

#[test]
fn analyst23_latest_refusal_requires_independent_feedback_witness() {
    assert_latest_feedback_witness("domain-analyst@23");
}

#[test]
fn analyst24_latest_refusal_requires_independent_feedback_witness() {
    assert_latest_feedback_witness("domain-analyst@24");
}

fn assert_latest_feedback_witness(role: &str) {
    let mut f = correction_fixture_role(role);
    f.begin();
    f.publish().unwrap();
    let task = f.saved();
    for mutation in [
        "missing_digest",
        "changed_digest",
        "changed_feedback",
        "missing_response",
    ] {
        let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
        let mut entry = completion::raw_refusals(&tx, &task).unwrap().remove(0);
        assert!(entry.response.finish_feedback_witness.is_some());
        for table in ["investigation_responses", "staged_completion_refusals"] {
            let trigger = format!(
                "{table}_no_{}",
                if mutation == "missing_response" && table == "investigation_responses" {
                    "delete"
                } else {
                    "update"
                }
            );
            let sql: String = tx
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE name=?1",
                    [&trigger],
                    |r| r.get(0),
                )
                .unwrap();
            tx.execute_batch(&format!("DROP TRIGGER {trigger}"))
                .unwrap();
            if table == "investigation_responses" {
                match mutation {
                    "missing_digest" => entry.response.finish_feedback_witness = None,
                    "changed_digest" => {
                        entry.response.finish_feedback_witness = Some("f".repeat(64))
                    }
                    _ => (),
                }
                if mutation == "missing_response" {
                    tx.execute(
                        "DELETE FROM investigation_responses WHERE task_id=?1 AND step_id=?2",
                        params![entry.record.task_id, entry.record.step_id],
                    )
                    .unwrap();
                } else {
                    let body = encode(&entry.response, 4096).unwrap();
                    tx.execute("UPDATE investigation_responses SET body=?3,body_hash=?4 WHERE task_id=?1 AND step_id=?2", params![entry.record.task_id,entry.record.step_id,body,core_prov::content_hash(&body)]).unwrap();
                }
            } else {
                if mutation == "changed_feedback" {
                    entry.record.feedback.unmet[0].reason =
                        agents::investigation::staged_finish::RefusalReason::Unavailable;
                    entry.record.refusal_id = entry.record.content_id().unwrap();
                }
                entry.rehash_for_test();
                let body = encode(&entry, MAX_RECORD_BYTES).unwrap();
                tx.execute("UPDATE staged_completion_refusals SET body=?2,body_hash=?3 WHERE task_id=?1 AND sequence=1", params![entry.record.task_id,body,core_prov::content_hash(&body)]).unwrap();
            }
            tx.execute_batch(&sql).unwrap();
        }
        assert!(staged::load_for_task(&tx, &task).is_err(), "{mutation}");
        // Drop rolls back mutation and restored triggers before the next case.
    }
}

#[test]
fn analyst22_replay_locator_is_bound_and_survives_reopen_without_text() {
    assert_replay_locator_reopen("domain-analyst@22");
}
#[test]
fn analyst24_combined_replay_is_bound_and_survives_reopen_without_text() {
    assert_replay_locator_reopen("domain-analyst@24");
}
fn assert_replay_locator_reopen(role: &str) {
    let mut f = correction_fixture_role(role);
    let replay = f
        .input
        .ledger()
        .coverage
        .as_ref()
        .unwrap()
        .qualification
        .coverage_id
        .clone();
    f.original["outcome"] = "insufficient_evidence".into();
    f.original["action"]["knowledge_completeness"] = "insufficient_evidence".into();
    f.original["action"]["findings"] = serde_json::json!([]);
    f.original["action"]["limitations"] = serde_json::json!([replay.clone()]);
    f.response.action_hash =
        core_prov::content_hash(&encode(&f.original, MAX_ACTION_BYTES).unwrap());
    f.begin();
    let detail = f.publish().unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Running);
    let task = f.saved();
    let tx = f.base.f.store.conn.unchecked_transaction().unwrap();
    let entry = completion::raw_refusals(&tx, &task).unwrap().remove(0);
    let location = if role == "domain-analyst@24" {
        assert_eq!(entry.record.feedback.schema_version, 6);
        let Some(agents::investigation::staged_finish::ProseCorrection::Combined(combined)) =
            &entry.record.feedback.prose_correction
        else {
            panic!("expected combined feedback")
        };
        let agents::investigation::staged_finish::CombinedCorrection::ReplayAt(location) =
            &combined.entries[0]
        else {
            panic!("expected replay")
        };
        location
    } else {
        assert_eq!(entry.record.feedback.schema_version, 4);
        let Some(agents::investigation::staged_finish::ProseCorrection::ParaphraseReplayAt(
            location,
        )) = &entry.record.feedback.prose_correction
        else {
            panic!("expected replay locator")
        };
        location
    };
    assert_eq!(
        location.start.field,
        agents::investigation::ProseField::Limitation { limitation: 0 }
    );
    assert_eq!(location.start.sentence, 1);
    assert!(
        !String::from_utf8(entry.record.feedback.bytes().unwrap())
            .unwrap()
            .contains(&replay)
    );
    assert!(entry.response.finish_feedback_witness.is_some());
    assert!(staged::load_for_task(&tx, &task).is_ok());
    if role == "domain-analyst@24" {
        let mut entry = entry;
        let Some(agents::investigation::staged_finish::ProseCorrection::Combined(combined)) =
            &mut entry.record.feedback.prose_correction
        else {
            unreachable!()
        };
        combined.omitted += 1;
        entry.record.refusal_id = entry.record.content_id().unwrap();
        entry.rehash_for_test();
        let trigger_sql: String = tx
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='staged_completion_refusals_no_update'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        tx.execute_batch("DROP TRIGGER staged_completion_refusals_no_update")
            .unwrap();
        let body = encode(&entry, MAX_RECORD_BYTES).unwrap();
        tx.execute("UPDATE staged_completion_refusals SET body=?2,body_hash=?3 WHERE task_id=?1 AND sequence=1",
            params![entry.record.task_id, body, core_prov::content_hash(&body)]).unwrap();
        tx.execute_batch(&trigger_sql).unwrap();
        assert!(
            staged::load_for_task(&tx, &task).is_err(),
            "rehashed omitted count must fail independent witness"
        );
    }
    drop(tx);
    let reopened = JobStore::open(f.base.f.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(f.base.id()).unwrap(), detail);
}
