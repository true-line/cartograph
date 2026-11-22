//! Additive descriptive evaluation metadata; never turns stored JSON into gate authority.
use super::*;
use semantic::evaluation::report::{EvaluationSummary, MAX_SUMMARY_BYTES};

const TABLE: &str = "eval_evaluation_summaries";
const SCHEMA: &str = "CREATE TABLE eval_evaluation_summaries (eval_id INTEGER PRIMARY KEY REFERENCES eval_results(id) ON DELETE CASCADE, payload BLOB NOT NULL CHECK(length(payload) BETWEEN 1 AND 65536)) STRICT";
fn invalid() -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}

pub(super) fn initialize(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
        [TABLE],
        |r| r.get(0),
    )?;
    if !exists {
        tx.execute_batch(SCHEMA)?;
    }
    validate(&tx)?;
    tx.commit()
}
pub(super) fn validate(conn: &Connection) -> rusqlite::Result<()> {
    // Compare schema in SQLite, never allocate untrusted SQL or trigger bodies.
    let exact: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 AND sql=?2)",
        params![TABLE, SCHEMA],
        |r| r.get(0),
    )?;
    let extras:i64=conn.query_row("SELECT count(*) FROM sqlite_master WHERE tbl_name=?1 AND name!=?1 AND NOT(type='index' AND name GLOB 'sqlite_autoindex_*' AND sql IS NULL)",[TABLE],|r|r.get(0))?;
    if !exact || extras != 0 {
        return Err(invalid());
    }
    let invalid_rows:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM eval_evaluation_summaries s LEFT JOIN eval_results e ON e.id=s.eval_id WHERE typeof(s.eval_id)!='integer' OR e.id IS NULL OR typeof(s.payload)!='blob' OR length(s.payload) NOT BETWEEN 1 AND ?1)",[MAX_SUMMARY_BYTES as i64],|r|r.get(0))?;
    if invalid_rows {
        return Err(invalid());
    }
    Ok(())
}
fn matching_counts(
    summary: &EvaluationSummary,
    proposals: u64,
    approved: u64,
) -> rusqlite::Result<()> {
    summary.validate().map_err(|_| invalid())?;
    let staged = summary
        .families
        .iter()
        .try_fold(0u64, |sum, family| sum.checked_add(family.staged))
        .ok_or_else(invalid)?;
    let admitted = summary
        .families
        .iter()
        .try_fold(0u64, |sum, family| sum.checked_add(family.admitted))
        .ok_or_else(invalid)?;
    if staged != proposals || admitted != approved {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn insert(
    conn: &Connection,
    id: i64,
    summary: &EvaluationSummary,
    proposals: u64,
    approved: u64,
) -> rusqlite::Result<()> {
    matching_counts(summary, proposals, approved)?;
    let bytes = serde_json::to_vec(summary).map_err(|_| invalid())?;
    if bytes.is_empty() || bytes.len() > MAX_SUMMARY_BYTES {
        return Err(invalid());
    }
    conn.execute(
        "INSERT INTO eval_evaluation_summaries(eval_id,payload) VALUES(?1,?2)",
        params![id, bytes],
    )?;
    Ok(())
}
pub(super) fn read(
    conn: &Connection,
    id: i64,
    proposals: u64,
    approved: u64,
) -> rusqlite::Result<Option<EvaluationSummary>> {
    use rusqlite::OptionalExtension;
    // Admission and BLOB retrieval are one statement in the caller's snapshot.
    // A present-but-invalid payload is distinct from an absent legacy companion.
    let bytes:Option<Option<Vec<u8>>>=conn.query_row("SELECT CASE WHEN typeof(payload)='blob' AND length(payload) BETWEEN 1 AND ?2 THEN payload ELSE NULL END FROM eval_evaluation_summaries WHERE eval_id=?1",params![id,MAX_SUMMARY_BYTES as i64],|r|r.get(0)).optional()?;
    let Some(bytes) = bytes else { return Ok(None) };
    let summary = EvaluationSummary::decode(&bytes.ok_or_else(invalid)?).map_err(|_| invalid())?;
    matching_counts(&summary, proposals, approved)?;
    Ok(Some(summary))
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;
    use semantic::evaluation::report::{
        DecisionReason, FamilyDecision, FamilyStatus, QualityMetrics, SummaryMode,
    };
    fn calibration() -> semantic::EvalReport {
        semantic::EvalReport {
            precision_floor: 0.9,
            similarity_threshold: 0.7,
            precision: 1.,
            recall: 1.,
            passed: true,
            true_positives: 1,
            false_positives: 0,
            false_negatives: 0,
        }
    }
    fn summary() -> EvaluationSummary {
        EvaluationSummary {
            schema_version: 1,
            mode: SummaryMode::VisibleMechanismEvaluation,
            families: vec![FamilyDecision {
                family: semantic::FamilyKey::new(semantic::HopKind::Call, None),
                status: FamilyStatus::IndependentlyEvaluated,
                reason: DecisionReason::IndependentlyEvaluated,
                staged: 3,
                admitted: 1,
                ambiguous: 0,
                receipt_digest: Some("a".repeat(64)),
                metrics: Some(QualityMetrics {
                    precision: 1.,
                    recall: 1.,
                    true_positives: 1,
                    false_positives: 0,
                    false_negatives: 0,
                    unknown: 0,
                    no_match_false_admissions: 0,
                    retrieval_recall: Some(1.),
                }),
            }],
        }
    }
    #[test]
    fn evaluation_summary_reopens_without_reinterpreting_legacy_calibration() {
        // AC-0276: additive metadata preserves historical scalars and explicit absence.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let expected = summary();
        {
            let mut store = JobStore::open(&path).unwrap();
            let legacy = store.record_eval("legacy", &calibration(), 3, 1).unwrap();
            assert!(legacy.evaluation.is_none());
            let actual = store
                .record_eval_with_summary("scripted", &calibration(), 3, 1, &expected)
                .unwrap();
            assert_eq!(actual.evaluation.as_ref(), Some(&expected));
        }
        let store = JobStore::open(&path).unwrap();
        let rows = store.list_evals().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].evaluation.as_ref(), Some(&expected));
        assert!(rows[1].evaluation.is_none());
        assert!(rows[1].passed);
        assert_eq!(rows[1].approved, 1);
        let wire = serde_json::to_value(&rows[1]).unwrap();
        assert!(wire.get("evaluation").is_none());
    }
    #[test]
    fn evaluation_summary_insert_rejects_count_mismatch_atomically() {
        // AC-0276: a descriptive report cannot contradict the exact persisted counts.
        let dir = tempfile::tempdir().unwrap();
        let mut store = JobStore::open(dir.path().join("state.db")).unwrap();
        assert!(
            store
                .record_eval_with_summary("scripted", &calibration(), 2, 1, &summary())
                .is_err()
        );
        assert!(store.list_evals().unwrap().is_empty());
        assert_eq!(
            store
                .conn
                .query_row("SELECT count(*) FROM eval_evaluation_summaries", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        let mut invalid_summary = summary();
        invalid_summary.families[0].admitted = 2;
        assert!(
            store
                .record_eval_with_summary("scripted", &calibration(), 3, 1, &invalid_summary)
                .is_err()
        );
        assert!(store.list_evals().unwrap().is_empty());
    }
    #[test]
    fn evaluation_summary_reads_reject_malformed_closed_json_and_oversized_payloads() {
        // AC-0276: stored passed=true does not rescue malformed or oversized metadata.
        for bytes in [b"{not-json}".to_vec(),b"{\"schema_version\":1,\"mode\":\"calibration_only\",\"families\":[],\"unexpected\":true}".to_vec(),vec![b' ';MAX_SUMMARY_BYTES+1]] {
            let dir=tempfile::tempdir().unwrap();let path=dir.path().join("state.db");let mut store=JobStore::open(&path).unwrap();let row=store.record_eval_with_summary("scripted",&calibration(),3,1,&summary()).unwrap();store.conn.pragma_update(None,"ignore_check_constraints",true).unwrap();store.conn.execute("UPDATE eval_evaluation_summaries SET payload=?1 WHERE eval_id=?2",params![bytes,row.id]).unwrap();assert!(store.list_evals().is_err());drop(store);
            if let Ok(reopened)=JobStore::open(&path) {assert!(reopened.list_evals().is_err());}
        }
    }
    #[test]
    fn evaluation_summary_schema_and_companion_count_tampering_fail_closed() {
        // AC-0276: validate owned schema before any write or payload allocation.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = JobStore::open(&path).unwrap();
        store
            .record_eval_with_summary("scripted", &calibration(), 3, 1, &summary())
            .unwrap();
        store
            .conn
            .execute("UPDATE eval_results SET approved=2", [])
            .unwrap();
        assert!(store.list_evals().is_err());
        store
            .conn
            .execute("UPDATE eval_results SET approved=1", [])
            .unwrap();
        store.conn.execute_batch("CREATE TRIGGER unexpected_eval_trigger AFTER INSERT ON eval_evaluation_summaries BEGIN DELETE FROM eval_results; END;").unwrap();
        assert!(
            store
                .record_eval_with_summary("scripted", &calibration(), 3, 1, &summary())
                .is_err()
        );
        assert_eq!(
            store
                .conn
                .query_row("SELECT count(*) FROM eval_results", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(store);
        assert!(JobStore::open(path).is_err());
    }
}
