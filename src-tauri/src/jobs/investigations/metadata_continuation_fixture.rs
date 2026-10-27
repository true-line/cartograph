//! Genuine production start/call/metadata fixtures; no converted task authority.
use super::*;
use completion::FinishFixture;
use context_hub::coverage::{CoverageContext, QualifiedRequest};
use next_prompt_invocation::tests::{firewall, limits, provider};
use std::sync::Arc;

pub(super) struct Proposal {
    pub f: FinishFixture,
    pub ledger: InvestigationInputLedger,
    pub raw: String,
    pub original: Value,
    pub response: InvestigationResponseMeta,
    pub kind: ContextPreviewKind,
    pages: Vec<String>,
}
impl Proposal {
    pub fn new(kind: ContextPreviewKind, refusal: bool) -> Self {
        let mut f = FinishFixture::new(true);
        if refusal {
            f.capacity_refusal();
        }
        f.begin();
        let mut result = Self {
            ledger: f.base.f.ledger.clone(),
            raw: String::new(),
            original: Value::Null,
            response: f.response.clone(),
            f,
            kind,
            pages: vec![],
        };
        result.set_candidate(false);
        result
    }
    pub fn read(&self) -> MetadataContinuationRead<'_> {
        MetadataContinuationRead {
            request: &self.f.base.f.request,
            response: &self.response,
            original_action: &self.original,
            current: &self.f.input,
            current_prepared: &self.f.prepared,
            candidate_ledger: &self.ledger,
            raw_response: &self.raw,
            kind: self.kind,
            active_milliseconds: self.response.active_milliseconds + 1,
        }
    }
    pub fn snapshot(&self) -> Result<MetadataContinuationSnapshot, InvestigationStoreError> {
        self.f
            .base
            .f
            .store
            .investigation_metadata_continuation_snapshot(
                self.f.base.id(),
                self.f.base.f.task.detail.summary.revision,
                &self.read(),
            )
    }
    pub fn candidate(&self, snapshot: &MetadataContinuationSnapshot) -> InvestigationInput {
        let mut pages = self.pages.clone();
        if self.kind == ContextPreviewKind::Query {
            pages.push(self.raw.clone());
        }
        let details = self
            .ledger
            .coverage_reads
            .iter()
            .map(|manifest| {
                String::from_utf8(
                    encode(
                        &self.f.base.f.archive.read(&manifest.request).unwrap(),
                        manifest.request.max_bytes,
                    )
                    .unwrap(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        InvestigationInput::prepare_v9_with_policy(
            &self.f.base.f.request.question,
            self.f.base.f.task.detail.summary.specialist_id,
            self.ledger.clone(),
            &pages.iter().map(String::as_str).collect::<Vec<_>>(),
            &[],
            &[],
            snapshot.usage(),
            snapshot.basis(),
            &self.f.base.f.request,
            snapshot.metadata(),
            &details.iter().map(String::as_str).collect::<Vec<_>>(),
            snapshot.mode(),
            snapshot.capacity_refusal_count(),
            None,
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap()
    }
    pub fn publish(&mut self, snapshot: &MetadataContinuationSnapshot) -> InvestigationDetail {
        let candidate = self.candidate(snapshot);
        let action = candidate.action(snapshot.next_action_id().into()).unwrap();
        let prepared = firewall()
            .prepare_bounded(&provider(false), &action, &limits())
            .unwrap();
        let proof = self
            .f
            .input
            .certify_context_preview_v9_with_policy(
                &candidate,
                self.kind,
                &self.raw,
                snapshot.admitted_usage(),
                &provider(false).0,
                &limits(),
                snapshot.next_action_id(),
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
        let active_milliseconds = self.read().active_milliseconds;
        self.f
            .base
            .f
            .store
            .admit_next_prompt_metadata(
                &self.f.base.f.execution,
                snapshot.revision(),
                &MetadataAdmission {
                    response: &self.response,
                    original_action: &self.original,
                    current: &self.f.input,
                    candidate: &candidate,
                    current_prepared: &self.f.prepared,
                    candidate_prepared: &prepared,
                    proof: &proof,
                    kind: self.kind,
                    active_milliseconds,
                },
                &self.raw,
                |_| Ok(()),
            )
            .unwrap()
    }
    pub fn advance_query(&mut self, replay: bool) {
        assert_eq!(self.kind, ContextPreviewKind::Query);
        self.pages.push(self.raw.clone());
        self.f.base.f.ledger = self.ledger.clone();
        self.f.base.refresh();
        let saved = self.f.base.snapshot();
        let details = saved
            .ledger()
            .coverage_reads
            .iter()
            .map(|manifest| {
                String::from_utf8(
                    encode(
                        &self.f.base.f.archive.read(&manifest.request).unwrap(),
                        manifest.request.max_bytes,
                    )
                    .unwrap(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let mut usage = saved.detail().usage.clone();
        usage.active_milliseconds += 1;
        self.f.input = InvestigationInput::prepare_v9_with_policy(
            &self.f.base.f.request.question,
            saved.detail().summary.specialist_id,
            saved.ledger().clone(),
            &self.pages.iter().map(String::as_str).collect::<Vec<_>>(),
            &[],
            &[],
            &usage,
            saved.basis(),
            &self.f.base.f.request,
            saved.metadata(),
            &details.iter().map(String::as_str).collect::<Vec<_>>(),
            saved.mode(),
            saved.capacity_refusal_count(),
            saved.capacity_feedback(),
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
        self.f.prepared = firewall()
            .prepare_bounded(
                &provider(false),
                &self
                    .f
                    .input
                    .action(format!(
                        "{}:step-{}",
                        self.f.base.id(),
                        usage.model_invocations + 1
                    ))
                    .unwrap(),
                &limits(),
            )
            .unwrap();
        self.response = InvestigationResponseMeta {
            step_id: format!("step-{}", usage.model_invocations + 1),
            response_hash: core_prov::content_hash(b"complete next metadata response"),
            action_hash: "a".repeat(64),
            observed_model: None,
            reported_input_tokens: Some(17),
            reported_output_tokens: Some(3),
            reported_cache_read_tokens: None,
            active_milliseconds: usage.active_milliseconds + 1,
        };
        self.f.begin();
        self.set_candidate(replay);
    }
    fn set_candidate(&mut self, replay: bool) {
        let prior = &self.f.base.f.ledger;
        self.ledger = prior.clone();
        self.ledger.revision += 1;
        match self.kind {
            ContextPreviewKind::Query => {
                let query = InvestigationQuery {
                    scope: InvestigationScope::All,
                    kind: None,
                    labels: vec![],
                    max_facts: 12,
                    max_bytes: MAX_QUERY_BYTES,
                    cursor: None,
                };
                let snapshot = Arc::new(context_hub::ContextSnapshot::new(vec![], vec![]).unwrap());
                assert_eq!(snapshot.id(), prior.scope_snapshot_id);
                let coverage = CoverageContext::from_shared_snapshot(
                    snapshot,
                    prior.coverage.as_ref().unwrap().qualification.clone(),
                )
                .unwrap();
                let page = coverage
                    .query(QualifiedRequest {
                        context_id: Some(self.f.base.f.request.expected_context_id.clone()),
                        request: context_hub::QueryRequest {
                            scope: context_hub::QueryScope::All,
                            kind: None,
                            labels: vec![],
                            max_facts: query.max_facts,
                            max_bytes: query.max_bytes,
                            cursor: None,
                        },
                    })
                    .unwrap();
                self.raw = serde_json::json!({"context":page,"evidence_options":[]}).to_string();
                self.ledger.queries.push(InvestigationQueryManifest {
                    query: query.clone(),
                    response_hash: core_prov::content_hash(self.raw.as_bytes()),
                    response_bytes: self.raw.len(),
                    returned_facts: 0,
                    total_selected: 0,
                    has_more: false,
                });
                self.original = if replay {
                    let hash = self
                        .f
                        .base
                        .f
                        .request
                        .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
                        .unwrap();
                    let reference = QueryHistoryReference::from_ledger(
                        &hash,
                        prior,
                        0,
                        &INVESTIGATION_V2_POLICY,
                    )
                    .unwrap();
                    serde_json::json!({"type":"replay_query","context_id":self.f.base.f.request.expected_context_id,"query_index":reference.query_index,"query_id":reference.query_id})
                } else {
                    serde_json::json!({"type":"query_context","context_id":self.f.base.f.request.expected_context_id,"query":query})
                };
            }
            ContextPreviewKind::Inventory => {
                let request = prior.coverage_reads[0].request.clone();
                self.raw = String::from_utf8(
                    encode(
                        &self.f.base.f.archive.read(&request).unwrap(),
                        request.max_bytes,
                    )
                    .unwrap(),
                )
                .unwrap();
                self.ledger
                    .coverage_reads
                    .push(InvestigationCoverageReadManifest {
                        request: request.clone(),
                        response_hash: core_prov::content_hash(self.raw.as_bytes()),
                        response_bytes: self.raw.len(),
                    });
                self.original = serde_json::json!({"type":"read_coverage_inventory","context_id":request.context_id,"repo":request.repo,"inventory_hash":request.inventory_hash,"max_bytes":request.max_bytes});
            }
        }
        self.response.action_hash =
            core_prov::content_hash(&encode(&self.original, MAX_ACTION_BYTES).unwrap());
    }
}
