use super::*;
use crate::{AssertionDecision, CuratableAssertion};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier, content_hash};
use rusqlite::params;

fn assertion(seed: &str) -> CuratableAssertion {
    CuratableAssertion {
        subject_id: format!("symbol:{seed}"),
        summary: "Observed interpretation".into(),
        provenance: Provenance::new(
            Tier::Agentic,
            ConfidenceTier::InferredWeak,
            vec![EvidenceRef {
                repo: "fixture/repo".into(),
                path: "source.ts".into(),
                byte_start: 0,
                byte_end: 4,
                commit_sha: "bound-revision".into(),
            }],
            "fixture.model",
            seed.as_bytes(),
        )
        .unwrap(),
    }
}

fn record(log: &mut DecisionLog, seed: &str, note: Option<&str>) -> String {
    let assertion = assertion(seed);
    log.record_assertion(&assertion, AssertionDecision::Accepted, note)
        .unwrap();
    assertion.provenance.content_hash
}

#[test]
fn selected_decisions_normalize_keys_keep_missing_and_skip_irrelevant_rows() {
    // AC-0249: annotations come only from the exact selected historical hashes.
    let mut log = DecisionLog::open(":memory:").unwrap();
    let a = record(&mut log, "a", None);
    let b = record(&mut log, "b", Some("reviewed"));
    let missing = content_hash(b"not recorded");
    log.conn
        .execute(
            "INSERT INTO assertion_decisions(content_hash,assertion_json,decision,note)
             VALUES(?1,?2,'accepted',?3)",
            params![
                content_hash(b"irrelevant"),
                "{".repeat(256 * 1024),
                "x".repeat(256 * 1024)
            ],
        )
        .unwrap();
    let selected = log
        .read_selected_assertions(&[b.clone(), missing.clone(), a.clone(), b.clone()])
        .unwrap();
    let mut expected = vec![a.clone(), b.clone(), missing.clone()];
    expected.sort();
    assert_eq!(
        selected
            .iter()
            .map(|entry| entry.content_hash.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        selected
            .iter()
            .find(|entry| entry.content_hash == missing)
            .unwrap()
            .record
            .is_none()
    );
    assert_eq!(
        selected
            .iter()
            .find(|entry| entry.content_hash == a)
            .unwrap()
            .record,
        log.get_assertion(&a).unwrap()
    );
    assert!(log.read_selected_assertions(&[]).unwrap().is_empty());
}

#[test]
fn selected_decisions_reject_invalid_keys_and_raw_count_before_storage() {
    let log = DecisionLog::open(":memory:").unwrap();
    log.conn
        .execute_batch("DROP TABLE assertion_decisions")
        .unwrap();
    for key in ["a".repeat(63), "A".repeat(64), "g".repeat(64)] {
        assert_eq!(
            log.read_selected_assertions(&[key]),
            Err(DecisionSelectionError::InvalidSelection)
        );
    }
    assert_eq!(
        log.read_selected_assertions(&vec!["a".repeat(64); 65]),
        Err(DecisionSelectionError::LimitExceeded)
    );
    assert_eq!(
        log.read_selected_assertions(&["a".repeat(64)]),
        Err(DecisionSelectionError::Storage)
    );
}

#[test]
fn selected_decisions_admit_every_stored_text_length_before_parsing() {
    let mut log = DecisionLog::open(":memory:").unwrap();
    let key = record(&mut log, "a", None);
    // A malformed oversized assertion must be rejected by the byte admission,
    // rather than reaching JSON parsing. Unicode notes count UTF-8 bytes.
    for (field, value) in [
        (
            "assertion_json",
            "{".repeat(MAX_SELECTED_ASSERTION_ROW_BYTES + 1),
        ),
        ("note", "é".repeat(MAX_SELECTED_ASSERTION_ROW_BYTES / 2 + 1)),
        (
            "updated_at",
            "x".repeat(MAX_SELECTED_ASSERTION_ROW_BYTES + 1),
        ),
        ("decision", "x".repeat(MAX_SELECTED_ASSERTION_ROW_BYTES + 1)),
    ] {
        record(&mut log, "a", None);
        log.conn
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        log.conn
            .execute(
                &format!("UPDATE assertion_decisions SET {field}=?1 WHERE content_hash=?2"),
                params![value, key],
            )
            .unwrap();
        assert_eq!(
            log.read_selected_assertions(std::slice::from_ref(&key)),
            Err(DecisionSelectionError::LimitExceeded)
        );
    }
    record(&mut log, "a", None);
    log.conn
        .execute(
            "UPDATE assertion_decisions SET assertion_json='{' WHERE content_hash=?1",
            [&key],
        )
        .unwrap();
    assert_eq!(
        log.read_selected_assertions(&[key]),
        Err(DecisionSelectionError::InvalidRecord)
    );
}

#[test]
fn selected_decisions_bound_cumulative_storage_and_escaped_serialization() {
    for (note, expected_raw_limit) in [("x".repeat(15_000), true), ("\u{1}".repeat(13_000), false)]
    {
        let mut log = DecisionLog::open(":memory:").unwrap();
        let keys: Vec<_> = (0..9)
            .map(|index| record(&mut log, &index.to_string(), Some(&note)))
            .collect();
        let (largest, total): (i64, i64) = log.conn.query_row(
            "SELECT max(length(CAST(assertion_json AS BLOB))+length(CAST(decision AS BLOB))+length(CAST(note AS BLOB))+length(CAST(updated_at AS BLOB))),
                    sum(length(CAST(assertion_json AS BLOB))+length(CAST(decision AS BLOB))+length(CAST(note AS BLOB))+length(CAST(updated_at AS BLOB)))
             FROM assertion_decisions", [], |row| Ok((row.get(0)?,row.get(1)?))
        ).unwrap();
        assert!(largest <= MAX_SELECTED_ASSERTION_ROW_BYTES as i64);
        assert_eq!(
            total > MAX_SELECTED_ASSERTION_BYTES as i64,
            expected_raw_limit
        );
        assert_eq!(
            log.read_selected_assertions(&keys),
            Err(DecisionSelectionError::LimitExceeded)
        );
    }
}

#[test]
fn selected_decisions_reject_stored_key_mismatch_and_observe_later_reviews() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("decisions.sqlite");
    let mut log = DecisionLog::open(&path).unwrap();
    let selected = assertion("selected");
    let key = selected.provenance.content_hash.clone();
    let absent = log
        .read_selected_assertions(std::slice::from_ref(&key))
        .unwrap();
    assert!(absent[0].record.is_none());
    log.record_assertion(&selected, AssertionDecision::Accepted, None)
        .unwrap();
    let saved = log
        .read_selected_assertions(std::slice::from_ref(&key))
        .unwrap();
    drop(log);
    let mut reopened = DecisionLog::open(&path).unwrap();
    assert_eq!(
        saved,
        reopened
            .read_selected_assertions(std::slice::from_ref(&key))
            .unwrap()
    );
    reopened
        .record_assertion(
            &selected,
            AssertionDecision::Rejected,
            Some("changed review"),
        )
        .unwrap();
    let reviewed = reopened
        .read_selected_assertions(std::slice::from_ref(&key))
        .unwrap();
    assert_ne!(saved, reviewed);
    assert_eq!(
        reviewed[0].record.as_ref().unwrap().decision,
        AssertionDecision::Rejected
    );
    let wrong_key = content_hash(b"different database key");
    reopened
        .conn
        .execute(
            "UPDATE assertion_decisions SET content_hash=?1 WHERE content_hash=?2",
            params![wrong_key, key],
        )
        .unwrap();
    assert_eq!(
        reopened.read_selected_assertions(&[wrong_key]),
        Err(DecisionSelectionError::InvalidRecord)
    );
}

#[test]
fn selected_decisions_keep_one_revision_during_concurrent_atomic_reviews() {
    // AC-0249: independent connections commit all selected annotations together;
    // every owned selection must contain one whole database revision.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coherent.sqlite");
    let mut log = DecisionLog::open(&path).unwrap();
    let keys: Vec<_> = (0..64)
        .map(|index| record(&mut log, &index.to_string(), Some("initial")))
        .collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_barrier = barrier.clone();
    let writer = std::thread::spawn(move || {
        let log = DecisionLog::open(&path).unwrap();
        writer_barrier.wait();
        for revision in 0..128 {
            log.conn
                .execute(
                    "UPDATE assertion_decisions SET note=?1",
                    [format!("revision-{revision}")],
                )
                .unwrap();
        }
    });
    barrier.wait();
    for _ in 0..32 {
        let selection = log.read_selected_assertions(&keys).unwrap();
        let notes: BTreeSet<_> = selection
            .iter()
            .map(|entry| entry.record.as_ref().unwrap().note.as_deref())
            .collect();
        assert_eq!(notes.len(), 1);
    }
    writer.join().unwrap();
}

#[test]
fn curation_observation_matches_compiler_policy_and_preserves_missing_selections() {
    // AC-0249: complete policy and selected historical annotations are separate.
    let mut log = DecisionLog::open(":memory:").unwrap();
    let accepted = record(&mut log, "accepted", None);
    let rejected = assertion("rejected");
    log.record_assertion(&rejected, AssertionDecision::Rejected, Some("rejected"))
        .unwrap();
    let annotated = assertion("annotated");
    log.record_assertion(&annotated, AssertionDecision::Annotated, Some("note"))
        .unwrap();
    let missing = content_hash(b"missing");
    let keys = vec![
        missing.clone(),
        rejected.provenance.content_hash.clone(),
        missing.clone(),
    ];
    let observed = log.read_curation_observation(&keys).unwrap();
    let existing_policy = log
        .list_assertions()
        .unwrap()
        .into_iter()
        .filter(|record| record.decision == AssertionDecision::Rejected)
        .map(|record| record.assertion.provenance.content_hash)
        .collect();
    assert_eq!(observed.rejected_hashes, existing_policy);
    assert_eq!(
        observed.selected,
        log.read_selected_assertions(&keys).unwrap()
    );
    assert!(
        observed
            .selected
            .iter()
            .find(|entry| entry.content_hash == missing)
            .unwrap()
            .record
            .is_none()
    );
    // An unrelated accepted-note update does not change compiler policy or the
    // requested historical records; selected changes are compared in full.
    log.record_assertion(
        &assertion("accepted"),
        AssertionDecision::Accepted,
        Some("unrelated note"),
    )
    .unwrap();
    assert_eq!(observed, log.read_curation_observation(&keys).unwrap());
    log.conn
        .execute(
            "UPDATE assertion_decisions SET note='changed' WHERE content_hash=?1",
            [&rejected.provenance.content_hash],
        )
        .unwrap();
    assert_ne!(observed, log.read_curation_observation(&keys).unwrap());
    assert!(!observed.rejected_hashes.contains(&accepted));
    assert!(
        log.read_curation_observation(&[])
            .unwrap()
            .selected
            .is_empty()
    );
}

#[test]
fn curation_observation_fails_on_invalid_unselected_policy_rows() {
    let mut log = DecisionLog::open(":memory:").unwrap();
    let selected = record(&mut log, "selected", None);
    let other = record(&mut log, "other", None);
    log.conn
        .execute(
            "UPDATE assertion_decisions SET assertion_json='{' WHERE content_hash=?1",
            [&other],
        )
        .unwrap();
    assert!(
        log.read_selected_assertions(std::slice::from_ref(&selected))
            .is_ok()
    );
    assert_eq!(
        log.read_curation_observation(std::slice::from_ref(&selected)),
        Err(DecisionSelectionError::InvalidRecord)
    );
    record(&mut log, "other", None);
    log.conn
        .execute(
            "UPDATE assertion_decisions SET note=?1 WHERE content_hash=?2",
            params!["x".repeat(MAX_SELECTED_ASSERTION_ROW_BYTES), other],
        )
        .unwrap();
    assert!(
        log.read_selected_assertions(std::slice::from_ref(&selected))
            .is_ok()
    );
    assert_eq!(
        log.read_curation_observation(std::slice::from_ref(&selected)),
        Err(DecisionSelectionError::LimitExceeded)
    );
    record(&mut log, "other", None);
    log.conn
        .execute(
            "UPDATE assertion_decisions SET content_hash=?1 WHERE content_hash=?2",
            params![content_hash(b"mismatched"), other],
        )
        .unwrap();
    assert_eq!(
        log.read_curation_observation(std::slice::from_ref(&selected)),
        Err(DecisionSelectionError::InvalidRecord)
    );
    log.conn
        .execute(
            "UPDATE assertion_decisions SET content_hash=?1 WHERE content_hash=?2",
            params!["z".repeat(64), content_hash(b"mismatched")],
        )
        .unwrap();
    assert_eq!(
        log.read_curation_observation(&[]),
        Err(DecisionSelectionError::InvalidRecord)
    );
}

/// Populate valid synthetic assertion rows without materializing their history.
fn fill_policy(log: &DecisionLog, count: usize, note: Option<&str>) -> usize {
    let base = assertion("bulk");
    let raw = serde_json::to_string(&base).unwrap();
    log.conn
        .execute(
            "WITH RECURSIVE i(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM i WHERE n+1<?1)
         INSERT INTO assertion_decisions(content_hash,assertion_json,decision,note,updated_at)
         SELECT printf('%064x',n),replace(?2,?3,printf('%064x',n)),'accepted',?4,'u' FROM i",
            params![count as i64, raw, base.provenance.content_hash, note],
        )
        .unwrap();
    raw.len() + "accepted".len() + "u".len()
}

#[test]
fn curation_observation_enforces_complete_scan_cardinality_boundary() {
    let log = DecisionLog::open(":memory:").unwrap();
    fill_policy(&log, MAX_CURATION_ROWS, None);
    let observed = log.read_curation_observation(&[]).unwrap();
    assert!(observed.rejected_hashes.is_empty());
    log.conn
        .execute(
            "INSERT INTO assertion_decisions(content_hash,assertion_json,decision)
         VALUES(?1,'{','accepted')",
            [content_hash(b"one over cardinality")],
        )
        .unwrap();
    // Cardinality admission precedes parsing the malformed extra record.
    assert_eq!(
        log.read_curation_observation(&[]),
        Err(DecisionSelectionError::LimitExceeded)
    );
}

#[test]
fn curation_observation_enforces_exact_complete_scan_byte_boundary() {
    let log = DecisionLog::open(":memory:").unwrap();
    let base = assertion("bulk");
    let raw = serde_json::to_string(&base).unwrap();
    let base_bytes = raw.len() + "accepted".len() + "u".len();
    let note_bytes = MAX_SELECTED_ASSERTION_ROW_BYTES - base_bytes;
    fill_policy(
        &log,
        MAX_CURATION_BYTES / MAX_SELECTED_ASSERTION_ROW_BYTES,
        Some(&"x".repeat(note_bytes)),
    );
    assert!(log.read_curation_observation(&[]).is_ok());
    // Every existing row is exactly 16 KiB. Trim one note, then append one
    // normal row so the complete scan is exactly one byte above 32 MiB.
    log.conn
        .execute(
            "UPDATE assertion_decisions SET note=substr(note,1,length(note)-?1)
         WHERE content_hash=printf('%064x',0)",
            [(base_bytes - 1) as i64],
        )
        .unwrap();
    let extra_hash = content_hash(b"extra-byte-boundary");
    log.conn
        .execute(
            "INSERT INTO assertion_decisions(content_hash,assertion_json,decision,note,updated_at)
         VALUES(?1,?2,'accepted',NULL,'u')",
            params![
                extra_hash,
                raw.replace(&base.provenance.content_hash, &extra_hash)
            ],
        )
        .unwrap();
    let total: i64 = log.conn.query_row(
        "SELECT sum(length(CAST(assertion_json AS BLOB))+length(CAST(decision AS BLOB))+
         coalesce(length(CAST(note AS BLOB)),0)+length(CAST(updated_at AS BLOB))) FROM assertion_decisions",
        [], |row| row.get(0)
    ).unwrap();
    assert_eq!(total, (MAX_CURATION_BYTES + 1) as i64);
    assert_eq!(
        log.read_curation_observation(&[]),
        Err(DecisionSelectionError::LimitExceeded)
    );
}

#[test]
fn curation_observation_bounds_keys_and_escaped_selected_output() {
    let mut log = DecisionLog::open(":memory:").unwrap();
    assert_eq!(
        log.read_curation_observation(&vec!["a".repeat(64); 65]),
        Err(DecisionSelectionError::LimitExceeded)
    );
    assert_eq!(
        log.read_curation_observation(&["A".repeat(64)]),
        Err(DecisionSelectionError::InvalidSelection)
    );
    let keys: Vec<_> = (0..9)
        .map(|index| record(&mut log, &index.to_string(), Some(&"\u{1}".repeat(13_000))))
        .collect();
    assert_eq!(
        log.read_curation_observation(&keys),
        Err(DecisionSelectionError::LimitExceeded)
    );
    // The complete history is valid and admitted when those large annotations
    // are not selected for return; it is never retained as a whole.
    assert!(log.read_curation_observation(&[]).is_ok());
}

#[test]
fn curation_observation_keeps_policy_and_selected_records_in_one_revision() {
    // AC-0249: atomic concurrent policy changes cannot mix rejected hashes from
    // one revision with selected annotations from another, even in one second.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("curation-coherent.sqlite");
    let mut log = DecisionLog::open(&path).unwrap();
    let keys: Vec<_> = (0..64)
        .map(|index| record(&mut log, &index.to_string(), Some("accepted")))
        .collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writer_barrier = barrier.clone();
    let writer = std::thread::spawn(move || {
        let log = DecisionLog::open(&path).unwrap();
        writer_barrier.wait();
        for revision in 0..128 {
            let decision = if revision % 2 == 0 {
                "rejected"
            } else {
                "accepted"
            };
            log.conn
                .execute(
                    "UPDATE assertion_decisions SET decision=?1,note=?1",
                    [decision],
                )
                .unwrap();
        }
    });
    barrier.wait();
    for _ in 0..32 {
        let observed = log.read_curation_observation(&keys).unwrap();
        let decision = observed.selected[0].record.as_ref().unwrap().decision;
        let rejected: BTreeSet<_> = if decision == AssertionDecision::Rejected {
            keys.iter().cloned().collect()
        } else {
            BTreeSet::new()
        };
        assert_eq!(observed.rejected_hashes, rejected);
        for entry in &observed.selected {
            let record = entry.record.as_ref().unwrap();
            assert_eq!(record.decision, decision);
            assert_eq!(record.note.as_deref(), Some(decision.as_str()));
        }
    }
    writer.join().unwrap();
}

#[test]
fn curation_observation_preserves_selected_raw_byte_limit_after_json_normalization() {
    // AC-0249: padding is stored input too, even when typed JSON serializes small.
    let mut log = DecisionLog::open(":memory:").unwrap();
    let keys: Vec<_> = (0..9)
        .map(|index| record(&mut log, &index.to_string(), None))
        .collect();
    for key in &keys {
        let raw =
            serde_json::to_string(&log.get_assertion(key).unwrap().unwrap().assertion).unwrap();
        let padded = format!("{raw}{}", " ".repeat(15_000 - raw.len()));
        log.conn
            .execute(
                "UPDATE assertion_decisions SET assertion_json=?1 WHERE content_hash=?2",
                params![padded, key],
            )
            .unwrap();
    }
    let normalized: Vec<_> = keys
        .iter()
        .map(|key| SelectedAssertionDecision {
            content_hash: key.clone(),
            record: log.get_assertion(key).unwrap(),
        })
        .collect();
    assert!(serde_json::to_vec(&normalized).unwrap().len() < MAX_SELECTED_ASSERTION_BYTES);
    let raw_bytes: i64 = log.conn.query_row(
        "SELECT sum(length(CAST(assertion_json AS BLOB))+length(CAST(decision AS BLOB))+
         coalesce(length(CAST(note AS BLOB)),0)+length(CAST(updated_at AS BLOB))) FROM assertion_decisions",
        [], |row| row.get(0),
    ).unwrap();
    assert!(raw_bytes > MAX_SELECTED_ASSERTION_BYTES as i64);
    assert!(raw_bytes < MAX_CURATION_BYTES as i64);
    assert_eq!(
        log.read_selected_assertions(&keys),
        Err(DecisionSelectionError::LimitExceeded)
    );
    assert_eq!(
        log.read_curation_observation(&keys),
        Err(DecisionSelectionError::LimitExceeded)
    );
    assert!(log.read_curation_observation(&[]).is_ok());
}
