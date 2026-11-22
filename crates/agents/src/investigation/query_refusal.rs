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
    PageBudget,
}

/// Text-free measurement of the specific page envelope that could not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryBudgetStage {
    QualificationEnvelope,
    CorePage,
    EvidenceOptionsEnvelope,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredBytesKind {
    Exact,
    AtLeast,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryBudgetDiagnostic {
    pub stage: QueryBudgetStage,
    pub requested_bytes: usize,
    pub effective_bytes: usize,
    pub required_bytes: usize,
    pub required_bytes_kind: RequiredBytesKind,
}
impl QueryBudgetDiagnostic {
    /// Sizing-only maximum wire fields; never a genuine observation.
    pub(crate) fn reservation() -> Self {
        Self {
            stage: QueryBudgetStage::EvidenceOptionsEnvelope,
            requested_bytes: usize::MAX,
            effective_bytes: usize::MAX,
            required_bytes: usize::MAX,
            required_bytes_kind: RequiredBytesKind::AtLeast,
        }
    }

    pub fn validate(&self) -> Result<(), InvestigationError> {
        if self.requested_bytes == 0
            || self.requested_bytes > MAX_QUERY_BYTES
            || self.effective_bytes == 0
            || self.effective_bytes > self.requested_bytes
            || self.required_bytes <= self.effective_bytes
            || (self.stage != QueryBudgetStage::CorePage
                && self.effective_bytes != self.requested_bytes)
            || (self.stage == QueryBudgetStage::EvidenceOptionsEnvelope
                && (self.required_bytes_kind != RequiredBytesKind::AtLeast
                    || self.effective_bytes.checked_add(1) != Some(self.required_bytes)))
            || (self.stage == QueryBudgetStage::CorePage
                && self.required_bytes_kind == RequiredBytesKind::AtLeast
                && self.effective_bytes.checked_add(1) != Some(self.required_bytes))
            || (self.stage == QueryBudgetStage::QualificationEnvelope
                && self.required_bytes_kind != RequiredBytesKind::AtLeast)
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}

/// A refused selection is not a successful page or an obligation acquisition.
/// Schema 3 versions this query-refusal wire record only; it is unrelated to
/// source reservation schema 3.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<QueryBudgetDiagnostic>,
}
impl QueryRefusalRecord {
    fn valid_budget_shape(&self) -> bool {
        match (self.schema_version, self.reason, self.budget) {
            (1 | 2, QueryRejection::PageBudget, _) => false,
            (1 | 2, _, None) => true,
            (3, QueryRejection::PageBudget, Some(budget)) => {
                budget.requested_bytes == self.query.max_bytes && budget.validate().is_ok()
            }
            _ => false,
        }
    }

    /// V1 preserves its historical feedback bytes. V2 discloses the closed
    /// diagnostic already authenticated in the event, without model text or IDs.
    pub fn feedback_reason(&self) -> ActionRefusalReason {
        if self.schema_version == 1 {
            return ActionRefusalReason::QueryRejected;
        }
        match self.reason {
            QueryRejection::UnknownAnchor => ActionRefusalReason::UnknownAnchor,
            QueryRejection::UnknownNode => ActionRefusalReason::UnknownNode,
            QueryRejection::CursorSnapshotMismatch => ActionRefusalReason::CursorSnapshot,
            QueryRejection::CursorSelectionMismatch => ActionRefusalReason::CursorSelection,
            QueryRejection::CursorContextMismatch => ActionRefusalReason::CursorContext,
            QueryRejection::InvalidCursorOffset => ActionRefusalReason::CursorOffset,
            QueryRejection::InvalidLimit => ActionRefusalReason::QueryLimit,
            QueryRejection::PageBudget => ActionRefusalReason::QueryPageBudget,
        }
    }

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
        if !self.valid_budget_shape()
            || (self.schema_version == 3 && !specialist.uses_query_budget_refusals())
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
                    && record.valid_budget_shape()
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
            Self::InvalidLimit | Self::PageBudget => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (QueryRefusalRecord, InvestigationInputLedger) {
        let ledger = super::super::tests::ledger();
        let mut record = QueryRefusalRecord {
            budget: None,
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
    // AC-0585: new numeric feedback cannot enter a historical role/version.
    #[test]
    fn auditor12_query_budget_record_is_closed_bound_and_legacy_safe() {
        let (mut record, ledger) = fixture();
        let legacy = serde_json::to_value(&record).unwrap();
        assert!(legacy.get("budget").is_none());
        record.schema_version = 3;
        record.reason = QueryRejection::PageBudget;
        record.budget = Some(QueryBudgetDiagnostic {
            stage: QueryBudgetStage::CorePage,
            requested_bytes: record.query.max_bytes,
            effective_bytes: 5386,
            required_bytes: 7061,
            required_bytes_kind: RequiredBytesKind::Exact,
        });
        record.record_id = record.content_id().unwrap();
        let check = |r: &QueryRefusalRecord, role| {
            r.validate_binding(
                "task-1",
                "step-1",
                &record.action_hash,
                &ledger,
                role,
                &INVESTIGATION_V1_POLICY,
            )
        };
        assert!(check(&record, SpecialistId::EvidenceAuditorV12).is_ok());
        assert!(check(&record, SpecialistId::EvidenceAuditorV11).is_err());
        for mutation in [
            "version",
            "missing_budget",
            "over_maximum",
            "reason",
            "requested",
            "effective",
            "required",
            "kind",
        ] {
            let mut changed = record.clone();
            match mutation {
                "version" => changed.schema_version = 2,
                "missing_budget" => changed.budget = None,
                "over_maximum" => {
                    changed.query.max_bytes = MAX_QUERY_BYTES + 1;
                    changed.budget.as_mut().unwrap().requested_bytes = MAX_QUERY_BYTES + 1;
                    assert!(changed.budget.unwrap().validate().is_err());
                }
                "reason" => changed.reason = QueryRejection::InvalidLimit,
                "requested" => changed.budget.as_mut().unwrap().requested_bytes -= 1,
                "effective" => changed.budget.as_mut().unwrap().effective_bytes = usize::MAX,
                "required" => changed.budget.as_mut().unwrap().required_bytes = 1,
                "kind" => {
                    changed.budget.as_mut().unwrap().required_bytes_kind =
                        RequiredBytesKind::AtLeast
                }
                _ => unreachable!(),
            }
            changed.record_id = changed.content_id().unwrap();
            assert!(
                check(&changed, SpecialistId::EvidenceAuditorV12).is_err(),
                "{mutation}"
            );
        }
        let wire = serde_json::to_value(record.budget.unwrap()).unwrap();
        assert_eq!(wire.as_object().unwrap().len(), 5);
        let mut unknown = wire;
        unknown["text"] = "forbidden".into();
        assert!(serde_json::from_value::<QueryBudgetDiagnostic>(unknown).is_err());
        let feedback = ActionRefusalFeedback {
            query_budget: record.budget,
            reason: ActionRefusalReason::QueryPageBudget,
            patch_rejection: None,
            invocation_ordinal: 1,
            action_hash: record.action_hash.clone(),
            choices_hash: "f".repeat(64),
        };
        assert!(feedback.stamp_hash().is_ok());
        let mut changed = feedback.clone();
        changed.query_budget.as_mut().unwrap().required_bytes += 1;
        assert_ne!(
            feedback.stamp_hash().unwrap(),
            changed.stamp_hash().unwrap()
        );
        assert!(
            serde_json::to_vec(&feedback.query_budget).unwrap().len()
                <= serde_json::to_vec(&Some(QueryBudgetDiagnostic::reservation()))
                    .unwrap()
                    .len()
        );
    }

    #[test]
    fn auditor12_terminal_query_budget_wire_is_versioned_and_text_free() {
        let budget = QueryBudgetDiagnostic {
            stage: QueryBudgetStage::CorePage,
            requested_bytes: 6000,
            effective_bytes: 5386,
            required_bytes: 7061,
            required_bytes_kind: RequiredBytesKind::Exact,
        };
        for termination in [
            QueryBudgetTermination::PageBudget,
            QueryBudgetTermination::ToolActions,
            QueryBudgetTermination::ModelInvocations,
            QueryBudgetTermination::ActiveTime,
        ] {
            let diagnostic = InvestigationTerminalDiagnostic::QueryBudget {
                termination,
                budget,
            };
            let wire = serde_json::to_value(diagnostic).unwrap();
            assert_eq!(wire["schema"], "investigation-terminal-diagnostic@2");
            assert_eq!(wire.as_object().unwrap().len(), 3);
            assert_eq!(
                serde_json::from_value::<InvestigationTerminalDiagnostic>(wire.clone()).unwrap(),
                diagnostic
            );
            let mut unknown = wire;
            unknown["response"] = "forbidden".into();
            assert!(serde_json::from_value::<InvestigationTerminalDiagnostic>(unknown).is_err());
        }
    }

    #[test]
    fn query_refusal_v2_reason_preserves_v1_projection_and_binds_version() {
        let (mut record, ledger) = fixture();
        let legacy_id = record.record_id.clone();
        assert_eq!(record.feedback_reason(), ActionRefusalReason::QueryRejected);
        record.schema_version = 2;
        assert!(validate(&record, &ledger, &record.action_hash).is_err());
        record.record_id = record.content_id().unwrap();
        assert_ne!(record.record_id, legacy_id);
        assert!(validate(&record, &ledger, &record.action_hash).is_ok());
        assert_eq!(record.feedback_reason(), ActionRefusalReason::UnknownAnchor);
        record.schema_version = 3;
        record.record_id = record.content_id().unwrap();
        assert!(validate(&record, &ledger, &record.action_hash).is_err());
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
            query_budget: None,
            reason: ActionRefusalReason::ActionNotOffered,
            patch_rejection: None,
            invocation_ordinal: INVESTIGATION_V2_POLICY.limits().model_invocations,
            action_hash: "f".repeat(64),
            choices_hash: "f".repeat(64),
        };
        let maximum = serde_json::to_vec(&reserved).unwrap();
        let original_stamp = reserved.stamp_hash().unwrap();
        for reason in [
            ActionRefusalReason::RepeatedRead,
            ActionRefusalReason::QueryRejected,
            ActionRefusalReason::UnknownAnchor,
            ActionRefusalReason::UnknownNode,
            ActionRefusalReason::CursorSnapshot,
            ActionRefusalReason::CursorSelection,
            ActionRefusalReason::CursorContext,
            ActionRefusalReason::CursorOffset,
            ActionRefusalReason::QueryLimit,
        ] {
            reserved.reason = reason;
            assert!(serde_json::to_vec(&reserved).unwrap().len() <= maximum.len());
        }
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
