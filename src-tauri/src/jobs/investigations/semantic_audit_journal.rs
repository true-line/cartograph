//! Optional immutable audit invocation journal. No raw prompt/source bytes.
//! The caller authenticates the task and commits the charge in the same transaction.
use super::*;
use llm::bounded::{CompletionLimitValues, CompletionLimits, ProviderProfile};
use rusqlite::Transaction;

const TABLE: &str = "candidate_audit_invocations";
const OWNED: &str = "name GLOB 'candidate_audit_*' OR (tbl_name GLOB 'candidate_audit_*' AND name NOT GLOB 'sqlite_*')";

/// Configured identity reported by the caller, not an attestation of executed weights.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeparateAuditor {
    pub profile: ProviderProfile,
    pub reported_model_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyst_reported_model_digest: Option<String>,
}
impl SeparateAuditor {
    pub fn validate(&self) -> Result<(), InvestigationStoreError> {
        if self.profile.locality != llm::Locality::Local
            || [
                &self.reported_model_digest,
                &self.analyst_reported_model_digest,
            ]
            .into_iter()
            .flatten()
            .any(|digest| digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(InvestigationStoreError::Invalid);
        }
        if let (Some(auditor), Some(analyst)) = (
            &self.reported_model_digest,
            &self.analyst_reported_model_digest,
        ) && auditor.eq_ignore_ascii_case(analyst)
        {
            return Err(InvestigationStoreError::Invalid);
        }
        encode(self, MAX_MANIFEST_BYTES)?;
        Ok(())
    }
    pub fn validate_against_model(
        &self,
        analyst_model: &str,
    ) -> Result<(), InvestigationStoreError> {
        self.validate()?;
        if self.profile.requested_model == analyst_model {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuditInvocation {
    pub schema_version: u32,
    pub task_id: String,
    pub finishing_step_id: String,
    pub candidate_hash: String,
    pub acquired_ledger_hash: String,
    pub contract_hash: String,
    pub action_id: String,
    pub payload_hash: String,
    pub provider: ProviderProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separate_auditor: Option<SeparateAuditor>,
    pub limits: CompletionLimitValues,
    /// Original aggregate usage before charging the single reserved audit call.
    pub pre_usage: InvestigationUsage,
}

impl AuditInvocation {
    fn validate(&self) -> Result<(), InvestigationStoreError> {
        if let Some(selected) = &self.separate_auditor {
            selected.validate()?;
            if selected.profile != self.provider {
                return Err(InvestigationStoreError::Invalid);
            }
        }
        if !matches!(self.schema_version, 1 | 2)
            || (self.schema_version == 2)
                != (self.contract_hash
                    == agents::investigation::semantic_audit::advisory_contract_hash()?)
            || !valid_id(&self.task_id)
            || !valid_id(&self.finishing_step_id)
            || self.action_id != format!("{}:audit-1", self.task_id)
            || self.provider.locality != llm::Locality::Local
            || [
                &self.candidate_hash,
                &self.acquired_ledger_hash,
                &self.contract_hash,
                &self.payload_hash,
            ]
            .iter()
            .any(|hash| {
                hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || CompletionLimits::from_audit_values(self.limits).is_err()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        encode(self, MAX_MANIFEST_BYTES)?;
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 1024 && !id.chars().any(char::is_control)
}
fn schema() -> Vec<String> {
    let mut result = Vec::new();
    for table in [TABLE, super::semantic_audit_receipt::TABLE] {
        result.push(format!("CREATE TABLE {table} (task_id TEXT PRIMARY KEY NOT NULL, body BLOB NOT NULL, body_hash TEXT NOT NULL) STRICT"));
        for verb in ["UPDATE", "DELETE"] {
            result.push(format!("CREATE TRIGGER {table}_no_{} BEFORE {verb} ON {table} BEGIN SELECT RAISE(ABORT, 'immutable audit record'); END", verb.to_lowercase()));
        }
    }
    result
}
fn exists(connection: &Connection) -> Result<bool, InvestigationStoreError> {
    Ok(connection.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE {OWNED})"),
        [],
        |r| r.get(0),
    )?)
}
fn validate_schema(connection: &Connection) -> Result<(), InvestigationStoreError> {
    let mut expected = schema();
    let max = expected.iter().map(String::len).max().unwrap();
    let mut actual = connection.prepare(&format!("SELECT CASE WHEN typeof(sql)='text' AND length(CAST(sql AS BLOB))<=?1 THEN sql END FROM sqlite_schema WHERE {OWNED} LIMIT ?2"))?
        .query_map(params![max as i64, expected.len() as i64 + 1], |r| r.get::<_, Option<String>>(0))?
        .collect::<Result<Vec<_>, _>>()?.into_iter().collect::<Option<Vec<_>>>()
        .ok_or(InvestigationStoreError::Invalid)?;
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}
pub(super) fn insert(
    tx: &Transaction<'_>,
    invocation: &AuditInvocation,
) -> Result<(), InvestigationStoreError> {
    invocation.validate()?;
    if !exists(tx)? {
        for sql in schema() {
            tx.execute_batch(&sql)?;
        }
    }
    validate_schema(tx)?;
    let body = encode(invocation, MAX_MANIFEST_BYTES)?;
    one(tx.execute(
        &format!("INSERT INTO {TABLE} VALUES (?1,?2,?3)"),
        params![invocation.task_id, body, core_prov::content_hash(&body)],
    )?)
}
pub(super) fn load(
    connection: &Connection,
    id: &str,
) -> Result<Option<AuditInvocation>, InvestigationStoreError> {
    if !exists(connection)? {
        return Ok(None);
    }
    validate_schema(connection)?;
    let record: Option<AuditInvocation> = read_body(connection, TABLE, "task_id", id, &id)?;
    if let Some(record) = &record {
        record.validate()?;
        if record.task_id != id {
            return Err(InvestigationStoreError::Invalid);
        }
    } else {
        let orphan: bool = connection.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {} WHERE task_id=?1)",
                super::semantic_audit_receipt::TABLE
            ),
            [id],
            |row| row.get(0),
        )?;
        if orphan {
            return Err(InvestigationStoreError::Invalid);
        }
    }
    Ok(record)
}
pub(super) fn stored_bytes(
    connection: &Connection,
    id: Option<&str>,
) -> Result<usize, InvestigationStoreError> {
    if !exists(connection)? {
        return Ok(0);
    }
    validate_schema(connection)?;
    let mut bytes = 0usize;
    for table in [TABLE, super::semantic_audit_receipt::TABLE] {
        let occupied: i64 = connection.query_row(&format!("SELECT coalesce(sum(length(CAST(body AS BLOB))+length(CAST(body_hash AS BLOB))+length(CAST(task_id AS BLOB))+256),0) FROM {table} WHERE (?1 IS NULL OR task_id=?1)"), [id], |r| r.get(0))?;
        bytes = bytes
            .checked_add(usize::try_from(occupied).map_err(|_| InvestigationStoreError::Capacity)?)
            .ok_or(InvestigationStoreError::Capacity)?;
    }
    Ok(bytes)
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;
    fn invocation() -> AuditInvocation {
        AuditInvocation {
            separate_auditor: None,
            schema_version: 1,
            task_id: "task:fixture".into(),
            finishing_step_id: "step-4".into(),
            candidate_hash: "a".repeat(64),
            acquired_ledger_hash: "b".repeat(64),
            contract_hash: "c".repeat(64),
            action_id: "task:fixture:audit-1".into(),
            payload_hash: "d".repeat(64),
            provider: ProviderProfile {
                protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
                provider_id: "local:fixture".into(),
                locality: llm::Locality::Local,
                endpoint_id: "http://127.0.0.1:11434".into(),
                requested_model: "fixture".into(),
            },
            limits: CompletionLimits::default().values(),
            pre_usage: InvestigationUsage::default(),
        }
    }
    // AC-0566: committed invocation identity is single-use and survives reopening.
    #[test]
    fn audit_journal_is_immutable_single_use_and_rollback_safe() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audit.db");
        let mut connection = Connection::open(&path).unwrap();
        let record = invocation();
        assert_eq!(stored_bytes(&connection, None).unwrap(), 0);
        assert!(load(&connection, &record.task_id).unwrap().is_none());
        {
            let tx = connection.transaction().unwrap();
            insert(&tx, &record).unwrap();
            tx.rollback().unwrap();
        }
        assert!(!exists(&connection).unwrap());
        {
            let tx = connection.transaction().unwrap();
            insert(&tx, &record).unwrap();
            tx.commit().unwrap();
        }
        drop(connection);
        let mut connection = Connection::open(&path).unwrap();
        assert_eq!(
            load(&connection, &record.task_id).unwrap(),
            Some(record.clone())
        );
        assert!(
            stored_bytes(&connection, Some(&record.task_id)).unwrap()
                > encode(&record, MAX_MANIFEST_BYTES).unwrap().len()
        );
        assert_eq!(stored_bytes(&connection, Some("other-task")).unwrap(), 0);
        {
            let tx = connection.transaction().unwrap();
            assert!(insert(&tx, &record).is_err());
            tx.rollback().unwrap();
        }
        assert!(
            connection
                .execute(&format!("UPDATE {TABLE} SET body=x'00'"), [])
                .is_err()
        );
        assert!(
            connection
                .execute(&format!("DELETE FROM {TABLE}"), [])
                .is_err()
        );
        assert_eq!(load(&connection, &record.task_id).unwrap(), Some(record));
        connection
            .execute_batch(&format!("DROP TRIGGER {TABLE}_no_update"))
            .unwrap();
        assert!(load(&connection, "task:fixture").is_err());
        assert!(stored_bytes(&connection, None).is_err());
    }
    // AC-0566: optional identity preserves historical bytes and rejects mismatches.
    #[test]
    fn audit_separate_identity_preserves_legacy_and_rejects_mismatched_profile() {
        let mut record = invocation();
        let historical = encode(&record, MAX_MANIFEST_BYTES).unwrap();
        assert!(
            !String::from_utf8(historical.clone())
                .unwrap()
                .contains("separate_auditor")
        );
        let reopened: AuditInvocation = serde_json::from_slice(&historical).unwrap();
        assert_eq!(encode(&reopened, MAX_MANIFEST_BYTES).unwrap(), historical);
        record.separate_auditor = Some(SeparateAuditor {
            profile: record.provider.clone(),
            reported_model_digest: Some("a".repeat(64)),
            analyst_reported_model_digest: Some("b".repeat(64)),
        });
        record.validate().unwrap();
        let selected = record.separate_auditor.as_mut().unwrap();
        assert!(
            selected
                .validate_against_model(&selected.profile.requested_model)
                .is_err()
        );
        selected.validate_against_model("different-model").unwrap();
        selected.analyst_reported_model_digest = Some("A".repeat(64));
        assert!(selected.validate_against_model("different-model").is_err());
        selected.analyst_reported_model_digest = Some("b".repeat(64));
        record
            .separate_auditor
            .as_mut()
            .unwrap()
            .profile
            .requested_model = "other".into();
        assert!(record.validate().is_err());
        record.separate_auditor.as_mut().unwrap().profile = record.provider.clone();
        record
            .separate_auditor
            .as_mut()
            .unwrap()
            .reported_model_digest = Some("not-a-digest".into());
        assert!(record.validate().is_err());
    }
}
