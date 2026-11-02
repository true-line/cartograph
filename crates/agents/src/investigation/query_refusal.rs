//! Bounded query-refusal identity. The host must separately verify the actual
//! frozen-context error and admitted step before persisting this observation.
use super::*;
use serde::{Deserialize, Serialize};

/// Closed reason codes never echo model text, graph facts or source content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryRejection {
    UnknownAnchor,
    UnknownNode,
    CursorSnapshotMismatch,
    CursorSelectionMismatch,
    CursorContextMismatch,
    InvalidCursorOffset,
    InvalidLimit,
}

/// A refused selection is not a successful page or an obligation acquisition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRefusalRecord {
    pub schema_version: u32,
    pub record_id: String,
    pub task_id: String,
    pub step_id: String,
    pub action_hash: String,
    pub input_ledger_hash: String,
    pub input_revision: u64,
    pub context_id: Option<String>,
    pub query: InvestigationQuery,
    pub reason: QueryRejection,
}
impl QueryRefusalRecord {
    pub fn content_id(&self) -> Result<String, InvestigationError> {
        let mut record = self.clone();
        record.record_id.clear();
        Ok(crate::source_basis::domain_hash(
            b"cartograph:query-refusal:v1\0",
            &bounded_bytes(&record, MAX_ACTION_BYTES)?,
        ))
    }

    /// Match the canonical model action hash used by the host admission journal.
    pub fn query_action_hash(&self) -> Result<String, InvestigationError> {
        let mut action = serde_json::json!({"type":"query_context", "query":self.query});
        if let Some(context) = &self.context_id {
            action["context_id"] = serde_json::Value::String(context.clone());
        }
        Ok(core_prov::content_hash(&bounded_bytes(
            &action,
            MAX_ACTION_BYTES,
        )?))
    }

    /// Pure binding checks do not attest execution or authorize a retry. The
    /// caller must load task, step, admitted action and input in one transaction,
    /// rederive the error, and preserve the existing charged tool-attempt count.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_binding(
        &self,
        task_id: &str,
        step_id: &str,
        admitted_action_hash: &str,
        ledger: &InvestigationInputLedger,
        specialist: SpecialistId,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        bounded_bytes(self, MAX_ACTION_BYTES)?;
        ledger.validate_with_policy(policy)?;
        self.query.validate_for_specialist(specialist)?;
        let expected_context = ledger
            .coverage
            .as_ref()
            .map(|proof| proof.context_id.as_str());
        if self.schema_version != 1
            || !records::opaque_id(task_id)
            || !records::opaque_id(step_id)
            || self.task_id != task_id
            || self.step_id != step_id
            || self.input_revision != ledger.revision
            || self.input_ledger_hash != ledger.fingerprint_with_policy(policy)?
            || self.context_id.as_deref() != expected_context
            || self.action_hash != admitted_action_hash
            || self.action_hash != self.query_action_hash()?
            || self.record_id != self.content_id()?
            || !self.reason.applies_to(&self.query)
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}
impl InvestigationEvent {
    /// Structural event binding; the host additionally checks admission and the
    /// exact saved ledger in the same read transaction.
    pub fn validate_query_refusal_envelope(&self) -> Result<(), InvestigationError> {
        match (&self.kind, &self.query_refusal) {
            (InvestigationEventKind::QueryRefused, Some(record))
                if self.investigation_id == record.task_id
                    && self.step_id.as_deref() == Some(record.step_id.as_str())
                    && self.tool == Some(InvestigationTool::QueryContext)
                    && record.schema_version == 1
                    && record.record_id == record.content_id()? =>
            {
                Ok(())
            }
            (InvestigationEventKind::QueryRefused, _) | (_, Some(_)) => {
                Err(InvestigationError::InvalidInput)
            }
            (_, None) => Ok(()),
        }
    }
}
impl QueryRejection {
    fn applies_to(self, query: &InvestigationQuery) -> bool {
        match self {
            Self::UnknownAnchor => matches!(query.scope, InvestigationScope::Neighborhood { .. }),
            Self::UnknownNode => matches!(query.scope, InvestigationScope::Node { .. }),
            Self::CursorSnapshotMismatch
            | Self::CursorSelectionMismatch
            | Self::CursorContextMismatch
            | Self::InvalidCursorOffset => query.cursor.is_some(),
            Self::InvalidLimit => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (QueryRefusalRecord, InvestigationInputLedger) {
        let ledger = super::super::tests::ledger();
        let mut record = QueryRefusalRecord {
            schema_version: 1,
            record_id: String::new(),
            task_id: "task-1".into(),
            step_id: "step-1".into(),
            action_hash: String::new(),
            input_ledger_hash: ledger.fingerprint().unwrap(),
            input_revision: ledger.revision,
            context_id: None,
            query: InvestigationQuery {
                scope: InvestigationScope::Neighborhood {
                    anchor: "absent".into(),
                    hops: 1,
                },
                kind: None,
                labels: vec![],
                max_facts: 1,
                max_bytes: 16384,
                cursor: None,
            },
            reason: QueryRejection::UnknownAnchor,
        };
        record.action_hash = record.query_action_hash().unwrap();
        record.record_id = record.content_id().unwrap();
        (record, ledger)
    }
    fn validate(
        record: &QueryRefusalRecord,
        ledger: &InvestigationInputLedger,
        action: &str,
    ) -> Result<(), InvestigationError> {
        record.validate_binding(
            "task-1",
            "step-1",
            action,
            ledger,
            SpecialistId::DomainAnalystV4,
            &INVESTIGATION_V1_POLICY,
        )
    }
    #[test]
    fn query_refusal_record_binds_action_step_and_unchanged_ledger() {
        let (record, ledger) = fixture();
        assert!(validate(&record, &ledger, &record.action_hash).is_ok());
        let bytes = serde_json::to_vec(&record).unwrap();
        let reopened: QueryRefusalRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record, reopened);
        assert!(validate(&reopened, &ledger, &record.action_hash).is_ok());
        for field in [
            "task", "step", "revision", "ledger", "query", "reason", "context",
        ] {
            let mut changed = record.clone();
            match field {
                "task" => changed.task_id = "another-task".into(),
                "step" => changed.step_id = "another-step".into(),
                "revision" => changed.input_revision += 1,
                "ledger" => changed.input_ledger_hash = "0".repeat(64),
                "query" => changed.query.max_facts += 1,
                "reason" => changed.reason = QueryRejection::UnknownNode,
                "context" => changed.context_id = Some("different-context".into()),
                _ => unreachable!(),
            }
            // Rehashing an edited record must not bypass authoritative bindings.
            changed.action_hash = changed.query_action_hash().unwrap();
            changed.record_id = changed.content_id().unwrap();
            assert!(
                validate(&changed, &ledger, &record.action_hash).is_err(),
                "{field}"
            );
        }
    }
    #[test]
    fn query_refusal_event_requires_exact_envelope_and_preserves_legacy_bytes() {
        let (record, _) = fixture();
        let mut event = InvestigationEvent {
            query_refusal: None,
            investigation_id: record.task_id.clone(),
            sequence: 1,
            revision: 1,
            kind: InvestigationEventKind::Created,
            step_id: None,
            tool: None,
            summary: "Created".into(),
            created_at: "2026-11-02T00:00:00Z".into(),
        };
        let legacy = serde_json::to_value(&event).unwrap();
        assert!(legacy.get("query_refusal").is_none());
        let reopened: InvestigationEvent = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(reopened).unwrap(), legacy);
        event.kind = InvestigationEventKind::QueryRefused;
        assert!(event.validate_query_refusal_envelope().is_err());
        event.step_id = Some(record.step_id.clone());
        event.tool = Some(InvestigationTool::QueryContext);
        event.query_refusal = Some(Box::new(record));
        assert!(event.validate_query_refusal_envelope().is_ok());
        let bytes = serde_json::to_vec(&event).unwrap();
        let reopened: InvestigationEvent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(event, reopened);
        for mutation in ["kind", "step", "tool", "task"] {
            let mut changed = event.clone();
            match mutation {
                "kind" => changed.kind = InvestigationEventKind::ToolCompleted,
                "step" => changed.step_id = Some("another-step".into()),
                "tool" => changed.tool = Some(InvestigationTool::ReadEvidence),
                "task" => changed.investigation_id = "another-task".into(),
                _ => unreachable!(),
            }
            assert!(
                changed.validate_query_refusal_envelope().is_err(),
                "{mutation}"
            );
        }
    }
    #[test]
    fn query_refusal_record_rejects_unbounded_and_unknown_fields() {
        let (mut record, ledger) = fixture();
        let mut value = serde_json::to_value(&record).unwrap();
        value["invented"] = true.into();
        assert!(serde_json::from_value::<QueryRefusalRecord>(value).is_err());
        record.query.labels.push("x".repeat(MAX_ACTION_BYTES));
        assert!(record.content_id().is_err());
        assert!(validate(&record, &ledger, &record.action_hash).is_err());
    }
}

#[cfg(test)]
mod feedback_budget_tests {
    use super::*;

    // AC-0266: both production reserve serializers use ActionNotOffered. The
    // query variant has identical fields and a shorter fixed ASCII reason.
    #[test]
    fn query_refusal_feedback_fits_existing_action_feedback_reservation() {
        let mut reserved = ActionRefusalFeedback {
            reason: ActionRefusalReason::ActionNotOffered,
            invocation_ordinal: INVESTIGATION_V2_POLICY.limits().model_invocations,
            action_hash: "f".repeat(64),
            choices_hash: "f".repeat(64),
        };
        let maximum = serde_json::to_vec(&reserved).unwrap();
        let original_stamp = reserved.stamp_hash().unwrap();
        reserved.reason = ActionRefusalReason::QueryRejected;
        let actual = serde_json::to_vec(&reserved).unwrap();
        assert!(actual.len() < maximum.len());
        assert!(actual.is_ascii());
        assert_ne!(reserved.stamp_hash().unwrap(), original_stamp);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&actual).unwrap()["reason"],
            "query_rejected"
        );
    }
}
