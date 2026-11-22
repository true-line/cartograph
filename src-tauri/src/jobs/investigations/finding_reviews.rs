//! Human follow-up associations over exact immutable historical results.
//! No provider, graph or source acquisition authority is available here.
use super::*;

const TABLE: &str = "investigation_finding_reviews";

pub(super) fn schema() -> Vec<(String, String, String)> {
    [
        (TABLE, "table", "CREATE TABLE investigation_finding_reviews (task_id TEXT NOT NULL CHECK(length(CAST(task_id AS BLOB)) BETWEEN 1 AND 128), result_id TEXT NOT NULL CHECK(length(CAST(result_id AS BLOB)) BETWEEN 1 AND 128), finding_id TEXT NOT NULL CHECK(length(CAST(finding_id AS BLOB)) BETWEEN 1 AND 128), subject_id TEXT NOT NULL UNIQUE CHECK(length(CAST(subject_id AS BLOB)) BETWEEN 1 AND 128), result_body_hash TEXT NOT NULL CHECK(length(CAST(result_body_hash AS BLOB))=64 AND result_body_hash NOT GLOB '*[^0-9a-f]*'), review_revision INTEGER NOT NULL CHECK(review_revision>0), decision TEXT NOT NULL CHECK(decision IN ('accepted_for_follow_up','rejected_for_follow_up')), reviewed_at TEXT NOT NULL CHECK(length(CAST(reviewed_at AS BLOB)) BETWEEN 1 AND 32), PRIMARY KEY(task_id,result_id,finding_id)) STRICT"),
        ("investigation_finding_reviews_no_delete", "trigger", "CREATE TRIGGER investigation_finding_reviews_no_delete BEFORE DELETE ON investigation_finding_reviews BEGIN SELECT RAISE(ABORT, 'durable finding review'); END"),
        ("investigation_finding_reviews_guard_update", "trigger", "CREATE TRIGGER investigation_finding_reviews_guard_update BEFORE UPDATE ON investigation_finding_reviews WHEN NEW.task_id IS NOT OLD.task_id OR NEW.result_id IS NOT OLD.result_id OR NEW.finding_id IS NOT OLD.finding_id OR NEW.subject_id IS NOT OLD.subject_id OR NEW.result_body_hash IS NOT OLD.result_body_hash OR NEW.review_revision != OLD.review_revision + 1 BEGIN SELECT RAISE(ABORT, 'finding review identity or revision'); END"),
    ].into_iter().map(|(name,kind,sql)| (name.into(),kind.into(),sql.into())).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingReviewError {
    Store(InvestigationStoreError),
    SubjectChanged,
    Conflict(Box<FindingReviewObservation>),
    /// Another connection held the write lock for the whole busy timeout.
    /// Raised only while taking the lock, so nothing was written (#1246).
    Busy,
}
impl FindingReviewError {
    /// Classify a failure to take the write lock: lock contention is a
    /// retryable outcome, anything else is a storage failure.
    fn write_lock(error: rusqlite::Error) -> Self {
        match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::DatabaseBusy) => Self::Busy,
            _ => error.into(),
        }
    }
}
impl From<InvestigationStoreError> for FindingReviewError {
    fn from(value: InvestigationStoreError) -> Self {
        Self::Store(value)
    }
}
impl From<rusqlite::Error> for FindingReviewError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Store(InvestigationStoreError::Storage)
    }
}
impl From<InvestigationError> for FindingReviewError {
    fn from(_: InvestigationError) -> Self {
        Self::Store(InvestigationStoreError::Invalid)
    }
}

#[derive(Serialize)]
struct FindingReviewBasisV1<'a> {
    schema_version: u32,
    namespace: &'a str,
    investigation_id: &'a str,
    intent_hash: &'a str,
    specialist: &'a InvestigationSpecialist,
    configured_provider: &'a InvestigationProvider,
    scope: &'a InvestigationScope,
    limits: &'a InvestigationLimits,
    result_id: &'a str,
    result_body_hash: &'a str,
    finding_id: &'a str,
}

fn subject(
    task: &StoredTask,
    result: &InvestigationResult,
    namespace: &ExecutionNamespace,
    body_hash: &str,
    finding_id: &str,
) -> Result<FindingReviewSubjectV1, InvestigationStoreError> {
    let basis = FindingReviewBasisV1 {
        schema_version: 1,
        namespace: &namespace.value,
        investigation_id: &task.detail.summary.investigation_id,
        intent_hash: &task.intent_hash,
        specialist: &task.detail.specialist,
        configured_provider: &task.detail.provider,
        scope: &task.detail.summary.scope,
        limits: &task.detail.limits,
        result_id: &result.result_id,
        result_body_hash: body_hash,
        finding_id,
    };
    let subject = FindingReviewSubjectV1 {
        investigation_id: basis.investigation_id.into(),
        result_id: result.result_id.clone(),
        finding_id: finding_id.into(),
        subject_id: subject_id(&basis)?,
    };
    subject.validate()?;
    Ok(subject)
}

fn subject_id(basis: &FindingReviewBasisV1<'_>) -> Result<String, InvestigationStoreError> {
    let bytes = encode(basis, MAX_RECORD_BYTES)?;
    let mut domain = b"cartograph:investigation-finding-review:v1\0".to_vec();
    domain.extend_from_slice(&bytes);
    Ok(format!(
        "investigation-finding-review-v1:{}",
        core_prov::content_hash(&domain)
    ))
}

/// The SQL writer emits exactly YYYY-MM-DDTHH:MM:SSZ. Validate the
/// calendar as well as separators; arbitrary bounded text is not a timestamp.
fn canonical_review_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| !matches!(i, 4 | 7 | 10 | 13 | 16 | 19) && !b.is_ascii_digit())
    {
        return false;
    }
    let number = |start: usize, end: usize| {
        bytes[start..end]
            .iter()
            .fold(0u32, |n, b| n * 10 + u32::from(*b - b'0'))
    };
    let year = number(0, 4);
    let month = number(5, 7);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&number(8, 10))
        && number(11, 13) < 24
        && number(14, 16) < 60
        && number(17, 19) < 60
}

fn load(
    connection: &Connection,
    namespace: &ExecutionNamespace,
    id: &str,
    expected_result: &str,
) -> Result<(FindingReviews, String), FindingReviewError> {
    if !id_valid(id) || !id_valid(expected_result) {
        return Err(InvestigationStoreError::Invalid.into());
    }
    validate(connection, namespace)?;
    let task = load_execution_task(connection, id)?;
    let result = load_result(connection, &task)?.ok_or(InvestigationStoreError::Missing)?;
    if result.result_id != expected_result {
        return Err(FindingReviewError::SubjectChanged);
    }
    // load_result verifies exact canonical round-trip bytes and every binding.
    let body_hash = core_prov::content_hash(&encode(&result, MAX_RESULT_BYTES)?);
    let mut reviews = result
        .findings
        .iter()
        .map(|finding| {
            Ok(FindingReviewObservation {
                subject: subject(&task, &result, namespace, &body_hash, &finding.finding_id)?,
                revision: 0,
                decision: None,
                reviewed_at: None,
            })
        })
        .collect::<Result<Vec<_>, InvestigationStoreError>>()?;
    let (count, valid): (i64, bool) = connection.query_row(
        "SELECT count(*), coalesce(min(typeof(task_id)='text' AND length(CAST(task_id AS BLOB)) BETWEEN 1 AND 128 AND typeof(result_id)='text' AND length(CAST(result_id AS BLOB)) BETWEEN 1 AND 128 AND typeof(finding_id)='text' AND length(CAST(finding_id AS BLOB)) BETWEEN 1 AND 128 AND typeof(subject_id)='text' AND length(CAST(subject_id AS BLOB)) BETWEEN 1 AND 128 AND typeof(result_body_hash)='text' AND length(CAST(result_body_hash AS BLOB))=64 AND typeof(review_revision)='integer' AND review_revision>0 AND typeof(decision)='text' AND decision IN ('accepted_for_follow_up','rejected_for_follow_up') AND typeof(reviewed_at)='text' AND length(CAST(reviewed_at AS BLOB)) BETWEEN 1 AND 32),1) FROM investigation_finding_reviews WHERE task_id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?)))?;
    if !valid
        || usize::try_from(count)
            .ok()
            .is_none_or(|n| n > reviews.len())
    {
        return Err(InvestigationStoreError::Invalid.into());
    }
    let mut statement = connection.prepare("SELECT result_id,finding_id,subject_id,result_body_hash,review_revision,decision,reviewed_at FROM investigation_finding_reviews WHERE task_id=?1")?;
    let mut rows = statement.query([id])?;
    while let Some(row) = rows.next()? {
        let result_id: String = row.get(0)?;
        let finding_id: String = row.get(1)?;
        let subject_id: String = row.get(2)?;
        let stored_hash: String = row.get(3)?;
        let revision: i64 = row.get(4)?;
        let decision: String = row.get(5)?;
        let reviewed_at: String = row.get(6)?;
        let review = reviews
            .iter_mut()
            .find(|r| r.subject.finding_id == finding_id)
            .ok_or(InvestigationStoreError::Invalid)?;
        if review.revision != 0
            || result_id != result.result_id
            || stored_hash != body_hash
            || subject_id != review.subject.subject_id
            || !canonical_review_timestamp(&reviewed_at)
        {
            return Err(InvestigationStoreError::Invalid.into());
        }
        review.revision = u64::try_from(revision).map_err(|_| InvestigationStoreError::Invalid)?;
        review.decision = Some(match decision.as_str() {
            "accepted_for_follow_up" => FindingReviewDecision::AcceptedForFollowUp,
            "rejected_for_follow_up" => FindingReviewDecision::RejectedForFollowUp,
            _ => return Err(InvestigationStoreError::Invalid.into()),
        });
        review.reviewed_at = Some(reviewed_at);
    }
    let response = FindingReviews {
        schema_version: 1,
        investigation_id: id.into(),
        result_id: result.result_id,
        reviews,
    };
    encode(&response, MAX_RECORD_BYTES)?;
    Ok((response, body_hash))
}

/// One saved finding as the host resolved it for a curation review
/// (ADR-0074 decision 2): the finding, its producing specialist and the
/// saved citation behind each of its citation ids. Nothing in it comes from
/// the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFinding {
    pub subject: FindingReviewSubjectV1,
    /// The saved specialist id, such as `domain-analyst@2`.
    pub role: String,
    pub finding: InvestigationFinding,
    /// One citation per citation id of the finding, in its order.
    pub citations: Vec<InvestigationCitation>,
}

impl JobStore {
    /// Resolve the exact historical finding `subject` names. The subject
    /// must be the one this store computes for that finding of that result;
    /// any other fails as [`FindingReviewError::SubjectChanged`]. Reads
    /// only: no review row and no task state is written.
    pub fn resolve_investigation_finding(
        &self,
        subject: &FindingReviewSubjectV1,
    ) -> Result<ResolvedFinding, FindingReviewError> {
        subject.validate()?;
        let tx = self.conn.unchecked_transaction()?;
        let id = &subject.investigation_id;
        let (reviews, _) = load(&tx, &self.namespace, id, &subject.result_id)?;
        let current = reviews
            .reviews
            .iter()
            .find(|review| review.subject.finding_id == subject.finding_id)
            .ok_or(InvestigationStoreError::Missing)?;
        if current.subject != *subject {
            return Err(FindingReviewError::SubjectChanged);
        }
        let task = load_execution_task(&tx, id)?;
        let result = load_result(&tx, &task)?.ok_or(InvestigationStoreError::Missing)?;
        let finding = result
            .findings
            .iter()
            .find(|finding| finding.finding_id == subject.finding_id)
            .ok_or(InvestigationStoreError::Missing)?
            .clone();
        // The task's citations are the saved ledger's (`load_ledger`).
        let citations = finding
            .citation_ids
            .iter()
            .map(|cited| {
                task.detail
                    .citations
                    .iter()
                    .find(|citation| citation.citation_id == *cited)
                    .cloned()
                    .ok_or(InvestigationStoreError::Invalid)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let role = serde_json::to_value(task.detail.specialist.id)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or(InvestigationStoreError::Invalid)?;
        tx.commit()?;
        Ok(ResolvedFinding {
            subject: subject.clone(),
            role,
            finding,
            citations,
        })
    }

    pub fn investigation_finding_reviews(
        &self,
        id: &str,
        expected_result: &str,
    ) -> Result<FindingReviews, FindingReviewError> {
        let tx = self.conn.unchecked_transaction()?;
        let (response, _) = load(&tx, &self.namespace, id, expected_result)?;
        tx.commit()?;
        Ok(response)
    }

    pub fn review_investigation_finding(
        &mut self,
        request: &ReviewInvestigationFindingRequest,
    ) -> Result<FindingReviewObservation, FindingReviewError> {
        self.review_investigation_finding_with_hook(request, || Ok(()))
    }

    pub(super) fn review_investigation_finding_with_hook(
        &mut self,
        request: &ReviewInvestigationFindingRequest,
        before_commit: impl FnOnce() -> Result<(), InvestigationStoreError>,
    ) -> Result<FindingReviewObservation, FindingReviewError> {
        request.validate()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(FindingReviewError::write_lock)?;
        let id = &request.subject.investigation_id;
        let (response, hash) = load(&tx, &self.namespace, id, &request.subject.result_id)?;
        let current = response
            .reviews
            .into_iter()
            .find(|r| r.subject.finding_id == request.subject.finding_id)
            .ok_or(InvestigationStoreError::Missing)?;
        if current.subject != request.subject {
            return Err(FindingReviewError::SubjectChanged);
        }
        if current.revision != request.expected_revision {
            return Err(FindingReviewError::Conflict(Box::new(current)));
        }
        let revision = signed(current.revision)?
            .checked_add(1)
            .ok_or(InvestigationStoreError::Invalid)?;
        let time = timestamp(&tx)?;
        if !canonical_review_timestamp(&time) {
            return Err(InvestigationStoreError::Invalid.into());
        }
        let decision = match request.decision {
            FindingReviewDecision::AcceptedForFollowUp => "accepted_for_follow_up",
            FindingReviewDecision::RejectedForFollowUp => "rejected_for_follow_up",
        };
        if current.revision == 0 {
            one(tx.execute(
                "INSERT INTO investigation_finding_reviews VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    id,
                    request.subject.result_id,
                    request.subject.finding_id,
                    request.subject.subject_id,
                    hash,
                    revision,
                    decision,
                    time
                ],
            )?)?;
        } else {
            one(tx.execute("UPDATE investigation_finding_reviews SET review_revision=?1,decision=?2,reviewed_at=?3 WHERE task_id=?4 AND result_id=?5 AND finding_id=?6 AND subject_id=?7 AND review_revision=?8", params![revision,decision,time,id,request.subject.result_id,request.subject.finding_id,request.subject.subject_id,signed(current.revision)?])?)?;
        }
        before_commit()?;
        // Human review may use free terminal capacity, never another task's reserve.
        check_task_capacity(&tx, id, true)?;
        let updated = FindingReviewObservation {
            subject: current.subject,
            revision: u64::try_from(revision).map_err(|_| InvestigationStoreError::Invalid)?,
            decision: Some(request.decision),
            reviewed_at: Some(time),
        };
        encode(&updated, MAX_RECORD_BYTES)?;
        tx.commit()?;
        Ok(updated)
    }
}

pub(super) fn stored_bytes(
    connection: &Connection,
    id: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    let bytes: i64 = connection.query_row("SELECT coalesce(sum(length(CAST(task_id AS BLOB))+length(CAST(result_id AS BLOB))+length(CAST(finding_id AS BLOB))+length(CAST(subject_id AS BLOB))+length(CAST(result_body_hash AS BLOB))+length(CAST(decision AS BLOB))+length(CAST(reviewed_at AS BLOB))+8+256),0) FROM investigation_finding_reviews WHERE (?1 IS NULL OR task_id=?1)", [id], |r| r.get(0))?;
    usize::try_from(bytes).map_err(|_| InvestigationStoreError::Invalid)
}

#[cfg(any(test, feature = "test-support"))]
mod identity_tests {
    use super::*;
    // AC-0293: literal JSON was assembled outside the production serializer;
    // its domain-separated hash was computed from those literal bytes.
    const BASIS: &str = r#"{"schema_version":1,"namespace":"0123456789abcdef0123456789abcdef","investigation_id":"investigation:fixture","intent_hash":"0000000000000000000000000000000000000000000000000000000000000000","specialist":{"id":"domain-analyst@2","name":"Fixture","version":2,"prompt_fingerprint":"1111111111111111111111111111111111111111111111111111111111111111","purpose":"Fixture identity only","operations":["query_context"],"tier":"Agentic","confidence_tier":"InferredWeak"},"configured_provider":{"mode":"local","provider_id":"local:fixture","model":"requested-model","endpoint":"http://127.0.0.1:11434","deployment":null,"available":true,"unavailable_reason":null},"scope":{"type":"all"},"limits":{"profile":"fixture","model_invocations":1,"tool_actions":1,"selected_facts":1,"evidence_requests":1,"evidence_items":1,"evidence_bytes":1,"captured_validation_bytes":1,"generated_tokens_per_invocation":1,"generated_token_reservations":1,"active_seconds":1,"consent_wait_seconds":1,"wall_seconds":1},"result_id":"result:fixture","result_body_hash":"2222222222222222222222222222222222222222222222222222222222222222","finding_id":"finding-1"}"#;
    #[test]
    fn finding_review_subject_literal_fixes_domain_order_and_saved_configuration() {
        let value: serde_json::Value = serde_json::from_str(BASIS).unwrap();
        let specialist = serde_json::from_value(value["specialist"].clone()).unwrap();
        let provider = serde_json::from_value(value["configured_provider"].clone()).unwrap();
        let scope = serde_json::from_value(value["scope"].clone()).unwrap();
        let limits = serde_json::from_value(value["limits"].clone()).unwrap();
        let basis = FindingReviewBasisV1 {
            schema_version: 1,
            namespace: value["namespace"].as_str().unwrap(),
            investigation_id: value["investigation_id"].as_str().unwrap(),
            intent_hash: value["intent_hash"].as_str().unwrap(),
            specialist: &specialist,
            configured_provider: &provider,
            scope: &scope,
            limits: &limits,
            result_id: value["result_id"].as_str().unwrap(),
            result_body_hash: value["result_body_hash"].as_str().unwrap(),
            finding_id: value["finding_id"].as_str().unwrap(),
        };
        assert_eq!(encode(&basis, MAX_RECORD_BYTES).unwrap(), BASIS.as_bytes());
        assert_eq!(
            subject_id(&basis).unwrap(),
            "investigation-finding-review-v1:64fd6b88fccc47f0c4a8b0fa8262633af40bd87f374f939a576f44ac648dd3ab"
        );
    }
}
