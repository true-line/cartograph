//! AC-0287: real internal factory/claim/initial publication and sealed calls;
//! no SQL family conversion, provider dispatch or public-route activation.
use super::*;
use completion::{InitialFixture, initial_fixture};
use llm::bounded::PreparedBoundedCompletion;
use next_prompt_invocation::tests::{firewall, limits, provider};
use serde_json::json;

pub(in crate::jobs::investigations) struct Fixture {
    pub(in crate::jobs::investigations) f: InitialFixture,
    pub(in crate::jobs::investigations) input: InvestigationInput,
    pub(in crate::jobs::investigations) prepared: PreparedBoundedCompletion,
    cloud: bool,
}
impl Fixture {
    pub(in crate::jobs::investigations) fn new(cloud: bool) -> Self {
        let profile = provider(cloud).0;
        let mode = if cloud {
            InvestigationProviderMode::Cloud
        } else {
            InvestigationProviderMode::Local
        };
        let mut f = initial_fixture(
            mode,
            InvestigationProvider {
                mode,
                provider_id: profile.provider_id,
                model: profile.requested_model,
                endpoint: profile.endpoint_id,
                deployment: None,
                protocol_version: Some(profile.protocol_version),
                available: true,
                unavailable_reason: None,
            },
        );
        let policy = &INVESTIGATION_V2_POLICY;
        let mut usage = f.task.detail.usage.clone();
        usage.active_milliseconds += 5;
        let basis = InvestigationTurnBasis {
            input_ledger_hash: f.ledger.fingerprint_with_policy(policy).unwrap(),
            previous_ledger: None,
            completed_tool: None,
        };
        let input = InvestigationInput::prepare_v9_with_policy(
            &f.request.question,
            f.task.detail.summary.specialist_id,
            f.ledger.clone(),
            &[],
            &[],
            &[],
            &usage,
            &basis,
            &f.request,
            &f.metadata,
            &[],
            CompletionCallMode::Choice,
            0,
            None,
            policy,
        )
        .unwrap();
        let action = input
            .action(format!("{}:step-1", f.task.detail.summary.investigation_id))
            .unwrap();
        let prepared = firewall()
            .prepare_bounded(&provider(cloud), &action, &limits())
            .unwrap();
        f.store
            .admit_next_prompt_initial_context(
                &f.execution,
                f.task.detail.summary.revision,
                &NextPromptInvocation {
                    input: &input,
                    prepared: &prepared,
                    active_milliseconds: usage.active_milliseconds,
                },
                &f.archive,
                |_| Ok(()),
            )
            .unwrap();
        let mut result = Self {
            f,
            input,
            prepared,
            cloud,
        };
        result.refresh();
        result
    }
    pub(in crate::jobs::investigations) fn id(&self) -> &str {
        &self.f.task.detail.summary.investigation_id
    }
    pub(in crate::jobs::investigations) fn refresh(&mut self) {
        let tx = self.f.store.conn.unchecked_transaction().unwrap();
        self.f.task = load_next_prompt_task_base(&tx, self.id()).unwrap();
    }
    pub(in crate::jobs::investigations) fn snapshot(&self) -> NextPromptSnapshot {
        self.f
            .store
            .investigation_next_prompt_snapshot(self.id(), &self.f.request)
            .unwrap()
    }
    pub(in crate::jobs::investigations) fn begin(&mut self) {
        assert!(!self.cloud);
        let authorized = firewall()
            .authorize_bounded(&provider(false), &self.prepared, None)
            .unwrap();
        self.f
            .store
            .begin_next_prompt_invocation(
                &self.f.execution,
                self.f.task.detail.summary.revision,
                &NextPromptInvocation {
                    input: &self.input,
                    prepared: &self.prepared,
                    active_milliseconds: self.f.task.detail.usage.active_milliseconds,
                },
                &authorized,
                |_| Ok(()),
            )
            .unwrap();
        self.refresh();
    }
    fn response(&self, original: &serde_json::Value) -> InvestigationResponseMeta {
        InvestigationResponseMeta {
            step_id: "step-1".into(),
            response_hash: core_prov::content_hash(b"complete bounded response"),
            action_hash: core_prov::content_hash(&encode(original, MAX_ACTION_BYTES).unwrap()),
            observed_model: None,
            reported_input_tokens: Some(10),
            reported_output_tokens: Some(5),
            active_milliseconds: self.f.task.detail.usage.active_milliseconds + 1,
        }
    }
    pub(in crate::jobs::investigations) fn refuse(&mut self) {
        self.begin();
        let query = InvestigationQuery {
            scope: InvestigationScope::All,
            kind: None,
            labels: vec![],
            max_facts: 12,
            max_bytes: 16384,
            cursor: None,
        };
        let original = json!({"type":"query_context","context_id":self.f.request.expected_context_id,"query":query});
        let response = self.response(&original);
        self.f
            .store
            .refuse_next_prompt_capacity(
                &self.f.execution,
                self.f.task.detail.summary.revision,
                &response,
                &original,
                CapacityRefusalReason::PromptCapacity,
            )
            .unwrap();
        self.refresh();
    }
    pub(in crate::jobs::investigations) fn acquire_inventory(&mut self) {
        self.begin();
        let policy = &INVESTIGATION_V2_POLICY;
        let Obligation::Inventory { target, .. } = &self.f.request.obligations.obligations[0]
        else {
            unreachable!()
        };
        let read = InvestigationCoverageRead {
            context_id: target.context_id.clone(),
            repo: target.repo.clone(),
            inventory_hash: target.inventory_hash.clone(),
            max_bytes: MAX_QUERY_BYTES,
        };
        let original = json!({"type":"read_coverage_inventory","context_id":read.context_id,"repo":read.repo,"inventory_hash":read.inventory_hash,"max_bytes":read.max_bytes});
        let raw = String::from_utf8(
            encode(&self.f.archive.read(&read).unwrap(), read.max_bytes).unwrap(),
        )
        .unwrap();
        let response = self.response(&original);
        let mut ledger = self.f.ledger.clone();
        ledger.revision += 1;
        ledger
            .coverage_reads
            .push(InvestigationCoverageReadManifest {
                request: read.clone(),
                response_hash: core_prov::content_hash(raw.as_bytes()),
                response_bytes: raw.len(),
            });
        let intent = ReadIntent::Inventory {
            context_id: read.context_id,
            repo: read.repo,
            inventory_hash: read.inventory_hash,
            max_bytes: read.max_bytes,
        };
        let tx = self.f.store.conn.unchecked_transaction().unwrap();
        let (_, admitted) = transitions::validate_response(&tx, &self.f.task, &response).unwrap();
        let records = derive_attempt_records(
            &self.f.task,
            &response.step_id,
            &response,
            &intent,
            &AttemptOutcome::InventoryAcquired { read_index: 0 },
            Some(&ledger),
            0,
        )
        .unwrap();
        let metadata = derive_protected_metadata(
            &self.f.task,
            records
                .into_iter()
                .map(|record| record.attempt_id)
                .collect(),
            &[],
        )
        .unwrap();
        tx.commit().unwrap();
        let mut usage = admitted.clone();
        usage.tool_actions += 1;
        usage.active_milliseconds += 1;
        let basis = InvestigationTurnBasis {
            input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
            previous_ledger: Some(self.f.ledger.clone()),
            completed_tool: Some(InvestigationTool::ReadCoverageInventory),
        };
        let candidate = InvestigationInput::prepare_v9_with_policy(
            &self.f.request.question,
            self.f.task.detail.summary.specialist_id,
            ledger.clone(),
            &[],
            &[],
            &[],
            &usage,
            &basis,
            &self.f.request,
            &metadata,
            &[raw.as_str()],
            CompletionCallMode::Choice,
            0,
            None,
            policy,
        )
        .unwrap();
        let action_id = format!("{}:step-2", self.id());
        let prepared = firewall()
            .prepare_bounded(
                &provider(false),
                &candidate.action(action_id.clone()).unwrap(),
                &limits(),
            )
            .unwrap();
        let proof = self
            .input
            .certify_context_preview_v9_with_policy(
                &candidate,
                ContextPreviewKind::Inventory,
                &raw,
                &admitted,
                &provider(false).0,
                &limits(),
                &action_id,
                policy,
            )
            .unwrap();
        self.f
            .store
            .admit_next_prompt_metadata(
                &self.f.execution,
                self.f.task.detail.summary.revision,
                &MetadataAdmission {
                    response: &response,
                    original_action: &original,
                    current: &self.input,
                    candidate: &candidate,
                    current_prepared: &self.prepared,
                    candidate_prepared: &prepared,
                    proof: &proof,
                    kind: ContextPreviewKind::Inventory,
                    active_milliseconds: usage.active_milliseconds,
                },
                &raw,
                |_| Ok(()),
            )
            .unwrap();
        self.f.ledger = ledger;
        self.refresh();
    }
}
