//! Real factory/claim/initial input, invocation and optional inventory admission.
use super::*;
use crate::jobs::investigations::completion::next_prompt_snapshot::tests::fixture as acquisition;
use next_prompt_invocation::tests::{firewall, limits, provider};

pub(in crate::jobs::investigations) struct Fixture {
    pub(in crate::jobs::investigations) base: acquisition::Fixture,
    pub(in crate::jobs::investigations) input: InvestigationInput,
    pub(in crate::jobs::investigations) prepared: PreparedBoundedCompletion,
    pub(in crate::jobs::investigations) original: Value,
    pub(in crate::jobs::investigations) response: InvestigationResponseMeta,
}
impl Fixture {
    pub(in crate::jobs::investigations) fn new(acquired: bool) -> Self {
        Self::from_base(acquisition::Fixture::new(false), acquired)
    }
    pub(in crate::jobs::investigations) fn from_base(
        mut base: acquisition::Fixture,
        acquired: bool,
    ) -> Self {
        if acquired {
            base.acquire_inventory();
        }
        let (input, prepared) = Self::prepare(&base);
        let original = serde_json::to_value(StagedFinish {
            narrative: None,
            schema_version: 1,
            outcome: CompletionOutcome::InsufficientEvidence,
            action: InvestigationAction::Finish {
                findings: vec![],
                knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
                limitations: vec![
                    "The inspected inventory supplies no direct source evidence.".into(),
                ],
            },
        })
        .unwrap();
        let response = Self::response(&base, &original);
        Self {
            base,
            input,
            prepared,
            original,
            response,
        }
    }
    fn prepare(base: &acquisition::Fixture) -> (InvestigationInput, PreparedBoundedCompletion) {
        let saved = base.snapshot();
        let policy = &INVESTIGATION_V2_POLICY;
        let details = base
            .f
            .ledger
            .coverage_reads
            .iter()
            .map(|manifest| {
                String::from_utf8(
                    encode(
                        &base.f.archive.read(&manifest.request).unwrap(),
                        manifest.request.max_bytes,
                    )
                    .unwrap(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let mut usage = saved.detail().usage.clone();
        usage.active_milliseconds += 1;
        let input = InvestigationInput::prepare_v9_with_policy(
            &base.f.request.question,
            saved.detail().summary.specialist_id,
            saved.ledger().clone(),
            &[],
            &[],
            &[],
            &usage,
            saved.basis(),
            &base.f.request,
            saved.metadata(),
            &details.iter().map(String::as_str).collect::<Vec<_>>(),
            saved.mode(),
            saved.capacity_refusal_count(),
            saved.capacity_feedback(),
            policy,
        )
        .unwrap();
        let input = saved.attach_acquisition(input).unwrap();
        let action = input
            .action(format!(
                "{}:step-{}",
                base.id(),
                usage.model_invocations + 1
            ))
            .unwrap();
        let prepared = firewall()
            .prepare_bounded(&provider(false), &action, &limits())
            .unwrap();
        (input, prepared)
    }
    fn response(base: &acquisition::Fixture, original: &Value) -> InvestigationResponseMeta {
        InvestigationResponseMeta {
            step_id: format!("step-{}", base.f.task.detail.usage.model_invocations + 1),
            response_hash: core_prov::content_hash(b"complete finish response"),
            action_hash: core_prov::content_hash(&encode(original, MAX_ACTION_BYTES).unwrap()),
            observed_model: None,
            reported_input_tokens: Some(17),
            reported_output_tokens: Some(3),
            reported_cache_read_tokens: None,
            active_milliseconds: base.f.task.detail.usage.active_milliseconds + 2,
        }
    }
    pub(in crate::jobs::investigations) fn begin(&mut self) {
        let authorized = firewall()
            .authorize_bounded(&provider(false), &self.prepared, None)
            .unwrap();
        self.base
            .f
            .store
            .begin_next_prompt_invocation(
                &self.base.f.execution,
                self.base.f.task.detail.summary.revision,
                &NextPromptInvocation {
                    input: &self.input,
                    prepared: &self.prepared,
                    active_milliseconds: self.base.f.task.detail.usage.active_milliseconds + 1,
                },
                &authorized,
                |_| Ok(()),
            )
            .unwrap();
        self.base.refresh();
    }
    pub(in crate::jobs::investigations) fn publish(
        &mut self,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.base.f.store.admit_next_prompt_finish(
            &self.base.f.execution,
            self.base.f.task.detail.summary.revision,
            &NextPromptFinish {
                request: &self.base.f.request,
                response: &self.response,
                original_action: &self.original,
                current: &self.input,
                current_prepared: &self.prepared,
            },
        )
    }
    pub(in crate::jobs::investigations) fn next(&mut self) {
        self.base.refresh();
        (self.input, self.prepared) = Self::prepare(&self.base);
        self.response = Self::response(&self.base, &self.original);
    }
    pub(in crate::jobs::investigations) fn capacity_refusal(&mut self) {
        self.begin();
        let original = self.query_action();
        let mut response = self.response.clone();
        response.action_hash =
            core_prov::content_hash(&encode(&original, MAX_ACTION_BYTES).unwrap());
        self.base
            .f
            .store
            .refuse_next_prompt_capacity(
                &self.base.f.execution,
                self.base.f.task.detail.summary.revision,
                &response,
                &original,
                CapacityRefusalReason::PromptCapacity,
            )
            .unwrap();
        self.next();
    }
    pub(in crate::jobs::investigations) fn query_action(&self) -> Value {
        let query = InvestigationQuery {
            scope: InvestigationScope::All,
            kind: None,
            labels: vec![],
            max_facts: 1,
            max_bytes: 16384,
            cursor: None,
        };
        serde_json::json!({"type":"query_context","context_id":self.base.f.request.expected_context_id,"query":query})
    }
    pub(in crate::jobs::investigations) fn saved(&self) -> StoredTask {
        let reopened = JobStore::open(self.base.f.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        load_next_prompt_task(&tx, self.base.id()).unwrap()
    }
    pub(in crate::jobs::investigations) fn rows(&self) -> Vec<Vec<u8>> {
        let mut rows = vec![encode(&self.saved(), MAX_RECORD_BYTES).unwrap()];
        for table in [
            "investigation_responses",
            "investigation_results",
            "investigation_events",
            "investigation_inputs",
        ] {
            let mut statement = self
                .base
                .f
                .store
                .conn
                .prepare(&format!(
                    "SELECT body FROM {table} WHERE task_id=?1 ORDER BY body"
                ))
                .unwrap();
            rows.extend(
                statement
                    .query_map([self.base.id()], |row| row.get::<_, Vec<u8>>(0))
                    .unwrap()
                    .map(Result::unwrap),
            );
        }
        rows
    }
}
