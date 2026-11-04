//! Content-limited audit decision receipt, never raw model prose or source text.
//! Insert only inside the task's authenticated response settlement transaction.
use super::*;
use agents::investigation::semantic_audit::{AuditDisposition, SemanticAuditReply};

pub(super) const TABLE: &str = "candidate_audit_responses";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuditReceipt {
    pub schema_version: u32,
    pub task_id: String,
    pub invocation_hash: String,
    pub reply: SemanticAuditReply,
    pub auditor_usage: InvestigationUsage,
}

impl AuditReceipt {
    /// Re-derive the disposition on every read; no stored boolean grants pass.
    pub(super) fn validate(
        &self,
        invocation: &super::semantic_audit_journal::AuditInvocation,
        candidate: &InvestigationResult,
        acquired_ids: &std::collections::BTreeSet<String>,
    ) -> Result<AuditDisposition, InvestigationStoreError> {
        if self.schema_version != 1
            || self.task_id != invocation.task_id
            || self.task_id != candidate.investigation_id
            || self.invocation_hash
                != core_prov::content_hash(&encode(invocation, MAX_MANIFEST_BYTES)?)
            || self.auditor_usage.model_invocations != 1
            || self.auditor_usage.generated_token_reservations
                != invocation.limits.max_output_tokens
        {
            return Err(InvestigationStoreError::Invalid);
        }
        encode(self, MAX_RECORD_BYTES)?;
        Ok(self.reply.validate_bound(
            candidate,
            &invocation.candidate_hash,
            &invocation.acquired_ledger_hash,
            &invocation.contract_hash,
            acquired_ids,
        )?)
    }
}

pub(super) fn insert(
    tx: &rusqlite::Transaction<'_>,
    receipt: &AuditReceipt,
    candidate: &InvestigationResult,
    acquired_ids: &std::collections::BTreeSet<String>,
) -> Result<AuditDisposition, InvestigationStoreError> {
    let invocation = super::semantic_audit_journal::load(tx, &receipt.task_id)?
        .ok_or(InvestigationStoreError::Invalid)?;
    let disposition = receipt.validate(&invocation, candidate, acquired_ids)?;
    let body = encode(receipt, MAX_RECORD_BYTES)?;
    one(tx.execute(
        &format!("INSERT INTO {TABLE} VALUES (?1,?2,?3)"),
        params![receipt.task_id, body, core_prov::content_hash(&body)],
    )?)?;
    Ok(disposition)
}

pub(super) fn load(
    connection: &Connection,
    id: &str,
    candidate: &InvestigationResult,
    acquired_ids: &std::collections::BTreeSet<String>,
) -> Result<Option<(AuditReceipt, AuditDisposition)>, InvestigationStoreError> {
    let Some(invocation) = super::semantic_audit_journal::load(connection, id)? else {
        return Ok(None);
    };
    let receipt: Option<AuditReceipt> = read_body(connection, TABLE, "task_id", id, &id)?;
    receipt
        .map(|receipt| {
            if receipt.task_id != id {
                return Err(InvestigationStoreError::Invalid);
            }
            let disposition = receipt.validate(&invocation, candidate, acquired_ids)?;
            Ok((receipt, disposition))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::super::semantic_audit_journal::{self as journal, AuditInvocation};
    use super::*;
    use agents::investigation::semantic_audit::{AuditSubject, SubjectReview, SubjectVerdict};

    #[test]
    fn audit_receipt_binds_invocation_and_reopens_without_raw_prose() {
        let candidate: InvestigationResult = serde_json::from_value(serde_json::json!({
            "schema_version":1,"investigation_id":"task:receipt","result_id":"result:receipt",
            "input_ledger_hash":"a".repeat(64),"graph_snapshot_id":"snapshot:receipt",
            "findings":[],"knowledge_completeness":"partial",
            "limitations":["Server behavior was not inspected."],
            "observed_response_model":null,"created_at":"2026-11-03T00:00:00Z"
        }))
        .unwrap();
        let invocation = AuditInvocation {
            separate_auditor: None,
            schema_version: 1,
            task_id: candidate.investigation_id.clone(),
            finishing_step_id: "step-4".into(),
            candidate_hash: core_prov::content_hash(&encode(&candidate, MAX_RESULT_BYTES).unwrap()),
            acquired_ledger_hash: candidate.input_ledger_hash.clone(),
            contract_hash: "b".repeat(64),
            action_id: "task:receipt:audit-1".into(),
            payload_hash: "c".repeat(64),
            provider: llm::bounded::ProviderProfile {
                protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
                provider_id: "local:fixture".into(),
                locality: llm::Locality::Local,
                endpoint_id: "http://127.0.0.1:11434".into(),
                requested_model: "fixture".into(),
            },
            limits: llm::bounded::CompletionLimits::default().values(),
            pre_usage: InvestigationUsage::default(),
        };
        let receipt = AuditReceipt {
            schema_version: 1,
            task_id: invocation.task_id.clone(),
            invocation_hash: core_prov::content_hash(
                &encode(&invocation, MAX_MANIFEST_BYTES).unwrap(),
            ),
            reply: SemanticAuditReply {
                schema_version: 1,
                candidate_hash: invocation.candidate_hash.clone(),
                acquired_ledger_hash: invocation.acquired_ledger_hash.clone(),
                contract_hash: invocation.contract_hash.clone(),
                reviews: vec![SubjectReview {
                    subject: AuditSubject::Limitation {
                        limitation_index: 0,
                    },
                    assessment: SubjectVerdict::Pass {},
                }],
            },
            auditor_usage: InvestigationUsage {
                model_invocations: 1,
                generated_token_reservations: invocation.limits.max_output_tokens,
                ..Default::default()
            },
        };
        let mut rejected = receipt.clone();
        rejected.reply.reviews[0].assessment = SubjectVerdict::UnsupportedScope {};
        assert_eq!(
            rejected
                .validate(&invocation, &candidate, &std::collections::BTreeSet::new())
                .unwrap(),
            AuditDisposition::Rejected
        );
        rejected.reply.reviews[0].assessment = SubjectVerdict::Indeterminate {};
        assert_eq!(
            rejected
                .validate(&invocation, &candidate, &std::collections::BTreeSet::new())
                .unwrap(),
            AuditDisposition::Indeterminate
        );
        // AC-0566: the receipt authenticates the separately reported auditor
        // identity through the complete immutable invocation hash.
        let mut separate = invocation.clone();
        separate.separate_auditor = Some(journal::SeparateAuditor {
            profile: invocation.provider.clone(),
            reported_model_digest: Some("a".repeat(64)),
            analyst_reported_model_digest: Some("b".repeat(64)),
        });
        let mut bound = receipt.clone();
        bound.invocation_hash =
            core_prov::content_hash(&encode(&separate, MAX_MANIFEST_BYTES).unwrap());
        assert!(
            bound
                .validate(&separate, &candidate, &std::collections::BTreeSet::new())
                .is_ok()
        );
        let mut changed_analyst = separate.clone();
        changed_analyst
            .separate_auditor
            .as_mut()
            .unwrap()
            .analyst_reported_model_digest = Some("c".repeat(64));
        assert!(
            bound
                .validate(
                    &changed_analyst,
                    &candidate,
                    &std::collections::BTreeSet::new()
                )
                .is_err()
        );
        separate
            .separate_auditor
            .as_mut()
            .unwrap()
            .reported_model_digest = Some("c".repeat(64));
        assert!(
            bound
                .validate(&separate, &candidate, &std::collections::BTreeSet::new())
                .is_err()
        );
        assert!(
            receipt
                .validate(&separate, &candidate, &std::collections::BTreeSet::new())
                .is_err()
        );
        let ids = std::collections::BTreeSet::new();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipts.db");
        let mut connection = Connection::open(&path).unwrap();
        {
            let tx = connection.transaction().unwrap();
            assert!(insert(&tx, &receipt, &candidate, &ids).is_err());
            tx.rollback().unwrap();
        }
        {
            let tx = connection.transaction().unwrap();
            journal::insert(&tx, &invocation).unwrap();
            tx.commit().unwrap();
        }
        {
            let tx = connection.transaction().unwrap();
            let bytes = encode(&receipt, MAX_RECORD_BYTES).unwrap();
            tx.execute(
                &format!("INSERT INTO {TABLE} VALUES (?1,?2,?3)"),
                params!["orphan", bytes, core_prov::content_hash(&bytes)],
            )
            .unwrap();
            assert!(load(&tx, "orphan", &candidate, &ids).is_err());
            tx.rollback().unwrap();
        }
        let before = journal::stored_bytes(&connection, None).unwrap();
        {
            let tx = connection.transaction().unwrap();
            assert_eq!(
                insert(&tx, &receipt, &candidate, &ids).unwrap(),
                AuditDisposition::Pass
            );
            tx.rollback().unwrap();
        }
        assert_eq!(journal::stored_bytes(&connection, None).unwrap(), before);
        assert!(
            load(&connection, &receipt.task_id, &candidate, &ids)
                .unwrap()
                .is_none()
        );
        let mut forged = receipt.clone();
        forged.invocation_hash = "d".repeat(64);
        {
            let tx = connection.transaction().unwrap();
            assert!(insert(&tx, &forged, &candidate, &ids).is_err());
            tx.rollback().unwrap();
        }
        {
            let tx = connection.transaction().unwrap();
            insert(&tx, &receipt, &candidate, &ids).unwrap();
            tx.commit().unwrap();
        }
        drop(connection);
        let mut connection = Connection::open(&path).unwrap();
        assert_eq!(
            load(&connection, &receipt.task_id, &candidate, &ids).unwrap(),
            Some((receipt.clone(), AuditDisposition::Pass))
        );
        assert!(journal::stored_bytes(&connection, None).unwrap() > before);
        {
            let tx = connection.transaction().unwrap();
            assert!(insert(&tx, &receipt, &candidate, &ids).is_err());
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
        let bytes = encode(&receipt, MAX_RECORD_BYTES).unwrap();
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("Server behavior")
        );
        let mut changed = candidate.clone();
        changed.limitations[0] = "different claim".into();
        assert!(load(&connection, &receipt.task_id, &changed, &ids).is_err());
    }
}
