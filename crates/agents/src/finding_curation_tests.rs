//! AC-0844 (T-0844): the finding curation store and its review.

use super::*;

const SUBJECT_HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde";

fn subject(finding: &str, digit: char) -> FindingReviewSubjectV1 {
    FindingReviewSubjectV1 {
        investigation_id: "investigation-1".into(),
        result_id: "result-1".into(),
        finding_id: finding.into(),
        subject_id: format!("investigation-finding-review-v1:{SUBJECT_HASH}{digit}"),
    }
}

fn record(finding: &str, digit: char) -> CuratedFindingRecord {
    CuratedFindingRecord {
        subject: subject(finding, digit),
        role: "analyst@27".into(),
        claim_kind: InvestigationClaimKind::ImplementedBehavior,
        title: "Refunds need an approved return".into(),
        statement: "A refund is issued only for a return in the approved state.".into(),
        limitations: vec!["Partial refunds were not read.".into()],
        cited: vec![
            CitedFact {
                citation: "F-0000000000000001".into(),
                fact: TaskFactKey::Node {
                    id: "rule:refund".into(),
                },
                digest: "node-v1:1".into(),
                captured: true,
            },
            CitedFact {
                citation: "F-0000000000000002".into(),
                fact: TaskFactKey::Edge {
                    source: "rule:refund".into(),
                    label: "GUARDS".into(),
                    destination: "ep:refund".into(),
                },
                digest: "edge-v1:2".into(),
                captured: false,
            },
        ],
    }
}

fn store() -> (tempfile::TempDir, DraftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = DraftStore::open(dir.path().join("review.sqlite")).unwrap();
    (dir, store)
}

fn count(store: &DraftStore) -> i64 {
    store
        .connection()
        .query_row("SELECT COUNT(*) FROM curated_findings", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn the_first_curation_review_stores_the_finding_as_resolved() {
    let (_dir, drafts) = store();
    let store = drafts.finding_curation();
    let finding = record("finding-1", '0');
    assert!(store.get(&finding.subject.subject_id).unwrap().is_none());
    let stored = store
        .review(&finding, 0, CurationDecision::Accepted)
        .unwrap();
    assert_eq!(stored.schema_version, CURATED_FINDING_SCHEMA_VERSION);
    assert_eq!(stored.record, finding);
    assert_eq!(stored.review_revision, 1);
    assert_eq!(stored.review_decision, CurationDecision::Accepted);
    assert_eq!(stored.reviewed_at.len(), 20);
    assert_eq!(
        store.get(&finding.subject.subject_id).unwrap(),
        Some(stored)
    );
    assert_eq!(count(&drafts), 1);
}

#[test]
fn a_curation_review_is_a_compare_and_set_and_rejection_withdraws() {
    let (_dir, drafts) = store();
    let store = drafts.finding_curation();
    let finding = record("finding-1", '0');
    // No row: only revision zero is current.
    assert!(matches!(
        store.review(&finding, 1, CurationDecision::Accepted),
        Err(FindingCurationError::StaleReviewRevision {
            expected: 1,
            actual: 0
        })
    ));
    assert_eq!(count(&drafts), 0);

    store
        .review(&finding, 0, CurationDecision::Accepted)
        .unwrap();
    // A second writer that also read revision zero loses.
    assert!(matches!(
        store.review(&finding, 0, CurationDecision::Rejected),
        Err(FindingCurationError::StaleReviewRevision {
            expected: 0,
            actual: 1
        })
    ));
    let withdrawn = store
        .review(&finding, 1, CurationDecision::Rejected)
        .unwrap();
    assert_eq!(withdrawn.review_revision, 2);
    assert_eq!(withdrawn.review_decision, CurationDecision::Rejected);
    assert_eq!(withdrawn.record, finding);
    let again = store
        .review(&finding, 2, CurationDecision::Accepted)
        .unwrap();
    assert_eq!(
        (again.review_revision, again.review_decision),
        (3, CurationDecision::Accepted)
    );
    assert_eq!(count(&drafts), 1);
}

#[test]
fn stored_finding_content_never_changes() {
    let (_dir, drafts) = store();
    let store = drafts.finding_curation();
    let finding = record("finding-1", '0');
    store
        .review(&finding, 0, CurationDecision::Accepted)
        .unwrap();

    // The same subject with other content is refused at any revision.
    let mut reworded = finding.clone();
    reworded.statement = "A refund is always issued.".into();
    let mut redigested = finding.clone();
    redigested.cited[0].digest = "node-v1:other".into();
    for changed in [reworded, redigested] {
        for revision in [0, 1] {
            assert!(matches!(
                store.review(&changed, revision, CurationDecision::Accepted),
                Err(FindingCurationError::ContentChanged)
            ));
        }
    }
    assert_eq!(
        store
            .get(&finding.subject.subject_id)
            .unwrap()
            .unwrap()
            .record,
        finding
    );

    // No caller can rewrite the content or delete the row.
    for sql in [
        "UPDATE curated_findings SET immutable_json = '{}'",
        "UPDATE curated_findings SET content_digest = 'x'",
        "UPDATE curated_findings SET subject_id = 'x'",
        "UPDATE curated_findings SET finding_id = 'x'",
        "DELETE FROM curated_findings",
    ] {
        assert!(store.conn.execute(sql, []).is_err(), "{sql}");
    }
    assert_eq!(count(&drafts), 1);
}

#[test]
fn a_record_the_host_could_not_have_resolved_is_refused_before_any_write() {
    let (_dir, drafts) = store();
    let store = drafts.finding_curation();
    let base = record("finding-1", '0');
    let mut cases: Vec<(&str, CuratedFindingRecord)> = Vec::new();
    let mut case = |name: &'static str, change: &dyn Fn(&mut CuratedFindingRecord)| {
        let mut changed = base.clone();
        change(&mut changed);
        cases.push((name, changed));
    };
    case("subject", &|r| r.subject.subject_id = "finding-1".into());
    case("role", &|r| r.role = " ".into());
    case("title", &|r| r.title = "x".repeat(161));
    case("statement", &|r| r.statement = String::new());
    case("statement", &|r| r.statement = "x".repeat(2049));
    case("limitations", &|r| r.limitations = vec![String::new()]);
    case("cited facts", &|r| r.cited.clear());
    case("cited facts", &|r| {
        let fact = r.cited[0].clone();
        r.cited = (0..9)
            .map(|index| CitedFact {
                citation: format!("F-{index:016}"),
                fact: TaskFactKey::Node {
                    id: format!("node:{index}"),
                },
                ..fact.clone()
            })
            .collect();
    });
    case("cited fact", &|r| r.cited[0].digest = String::new());
    case("cited fact", &|r| r.cited[1].citation = "F\n2".into());
    case("cited fact order", &|r| r.cited.reverse());
    case("cited fact order", &|r| {
        r.cited[1].citation = r.cited[0].citation.clone();
    });
    case("cited fact order", &|r| {
        r.cited[1].fact = r.cited[0].fact.clone()
    });
    for (name, changed) in cases {
        match store.review(&changed, 0, CurationDecision::Accepted) {
            Err(FindingCurationError::InvalidRecord(field)) => assert_eq!(field, name),
            other => panic!("{name}: {other:?}"),
        }
    }
    assert_eq!(count(&drafts), 0);
}

#[test]
fn reads_are_bounded_and_say_when_more_exist() {
    let (_dir, drafts) = store();
    let store = drafts.finding_curation();
    let digits = ['0', '1', '2'];
    for (index, digit) in digits.into_iter().enumerate() {
        store
            .review(
                &record(&format!("finding-{}", 3 - index), digit),
                0,
                CurationDecision::Accepted,
            )
            .unwrap();
    }
    let mut other = record("finding-9", '9');
    other.subject.result_id = "result-2".into();
    store.review(&other, 0, CurationDecision::Rejected).unwrap();

    // Oldest first, and never a partial read passed off as complete.
    let all = store.reviewed(8).unwrap();
    assert!(!all.more);
    assert_eq!(
        all.items
            .iter()
            .map(|stored| stored.record.subject.finding_id.as_str())
            .collect::<Vec<_>>(),
        ["finding-3", "finding-2", "finding-1", "finding-9"]
    );
    let exact = store.reviewed(4).unwrap();
    assert_eq!((exact.items.len(), exact.more), (4, false));
    let short = store.reviewed(3).unwrap();
    assert_eq!((short.items.len(), short.more), (3, true));
    assert!(matches!(
        store.reviewed(0),
        Err(FindingCurationError::InvalidLimit)
    ));

    // One result's reviews, in finding id order, without the other result's.
    let observed = store.for_result("investigation-1", "result-1", 12).unwrap();
    assert_eq!(
        observed
            .iter()
            .map(|review| (review.finding_id.as_str(), review.review_revision))
            .collect::<Vec<_>>(),
        [("finding-1", 1), ("finding-2", 1), ("finding-3", 1)]
    );
    assert!(
        observed
            .iter()
            .all(|review| review.review_decision == CurationDecision::Accepted)
    );
    assert!(
        store
            .for_result("investigation-1", "result-3", 12)
            .unwrap()
            .is_empty()
    );
    // More reviews than a result can have findings fails closed.
    assert!(matches!(
        store.for_result("investigation-1", "result-1", 2),
        Err(FindingCurationError::InvalidStored)
    ));
}

#[test]
fn a_tampered_row_fails_closed_and_reviews_survive_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("review.sqlite");
    let finding = record("finding-1", '0');
    {
        let drafts = DraftStore::open(&path).unwrap();
        drafts
            .finding_curation()
            .review(&finding, 0, CurationDecision::Accepted)
            .unwrap();
    }
    let drafts = DraftStore::open(&path).unwrap();
    let store = drafts.finding_curation();
    let reopened = store.get(&finding.subject.subject_id).unwrap().unwrap();
    assert_eq!(
        (reopened.review_revision, reopened.review_decision),
        (1, CurationDecision::Accepted)
    );
    assert_eq!(reopened.record, finding);

    // A row written around the store, with a digest that does not match
    // its content, is never served or reviewed.
    store
        .conn
        .execute_batch("DROP TRIGGER curated_findings_immutable")
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE curated_findings SET immutable_json = replace(immutable_json, 'approved', 'any')",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.get(&finding.subject.subject_id),
        Err(FindingCurationError::InvalidStored)
    ));
    assert!(matches!(
        store.reviewed(8),
        Err(FindingCurationError::InvalidStored)
    ));
    assert!(matches!(
        store.review(&finding, 1, CurationDecision::Rejected),
        Err(FindingCurationError::InvalidStored)
    ));
}

#[test]
fn a_stored_finding_becomes_the_projection_input_without_an_upgrade() {
    let (_dir, drafts) = store();
    let store = drafts.finding_curation();
    let finding = record("finding-1", '0');
    let accepted = store
        .review(&finding, 0, CurationDecision::Accepted)
        .unwrap();
    let input = reviewed_finding(&accepted);
    assert_eq!(input.finding_ref, finding.subject.subject_id);
    assert_eq!(
        (
            input.investigation_id.as_str(),
            input.result_id.as_str(),
            input.finding_id.as_str()
        ),
        ("investigation-1", "result-1", "finding-1")
    );
    assert_eq!(input.role, "analyst@27");
    assert_eq!(input.kind, "implemented_behavior");
    assert_eq!(input.title, finding.title);
    assert_eq!(input.statement, finding.statement);
    assert_eq!(input.limitations, finding.limitations);
    assert_eq!(
        input.cited_digests,
        std::collections::BTreeMap::from([
            ("F-0000000000000001".to_owned(), "node-v1:1".to_owned()),
            ("F-0000000000000002".to_owned(), "edge-v1:2".to_owned()),
        ])
    );
    assert_eq!(
        input.support,
        SourceSupport {
            corroborated: 0,
            captured: 1,
            unverified: 1
        }
    );
    assert_eq!(input.decision, Some(FindingCuration::Accepted));

    let rejected = store
        .review(&finding, 1, CurationDecision::Rejected)
        .unwrap();
    assert_eq!(
        reviewed_finding(&rejected).decision,
        Some(FindingCuration::Rejected)
    );
    // Every claim kind has one spelling, the one the saved result uses.
    for kind in [
        InvestigationClaimKind::ImplementedBehavior,
        InvestigationClaimKind::DocumentedIntent,
        InvestigationClaimKind::InferredInterpretation,
        InvestigationClaimKind::ProposedDesign,
    ] {
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            serde_json::Value::from(claim_kind(kind))
        );
    }
}

// A writer that holds the file for the whole busy timeout is contention,
// not a storage failure, and nothing is written.
#[test]
fn a_locked_store_reports_busy_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("review.sqlite");
    let drafts = DraftStore::open(&path).unwrap();
    let store = drafts.finding_curation();
    store.conn.busy_timeout(std::time::Duration::ZERO).unwrap();
    let finding = record("finding-1", '0');

    let other = rusqlite::Connection::open(&path).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(matches!(
        store.review(&finding, 0, CurationDecision::Accepted),
        Err(FindingCurationError::Busy)
    ));
    other.execute_batch("ROLLBACK").unwrap();
    assert_eq!(count(&drafts), 0);
    // The same review succeeds once the lock is free.
    store
        .review(&finding, 0, CurationDecision::Accepted)
        .unwrap();
}
