//! Genuine production start/call/metadata fixtures; no converted task authority.
use super::*;
use completion::FinishFixture;
use context_hub::coverage::{CoverageContext, QualifiedRequest};
use next_prompt_invocation::tests::{firewall, limits, provider};
use std::sync::Arc;

pub(in crate::jobs::investigations) struct Proposal {
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
        Self::from_fixture(FinishFixture::new(true), kind, refusal)
    }
    pub fn gated(kind: ContextPreviewKind) -> Self {
        use completion::next_prompt_snapshot::tests::fixture::Fixture;
        let (_, target) = source_validation::internal_source_invocation_fixture();
        let base = Fixture::customized(false, |f| {
            f.request.specialist_id = "domain-analyst@18".into();
            assert_eq!(target.graph_snapshot_id, f.ledger.graph_snapshot_id);
            f.request.expected_graph_revision = Some(target.graph_snapshot_id.clone());
            f.task.expected_graph_revision = f.request.expected_graph_revision.clone();
            let mut source = Obligation::Source {
                obligation_id: String::new(),
                target: Box::new(target),
            };
            let id = source.content_id().unwrap();
            if let Obligation::Source { obligation_id, .. } = &mut source {
                *obligation_id = id;
            }
            f.request.obligations.obligations.push(source);
            f.request = f
                .request
                .normalized_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap();
            let intent = completion::host_fixture_intent(&f.request).unwrap();
            f.task.intent_hash = intent.request_hash.clone();
            f.task.detail.completion = Some(completion::initial_detail(&intent));
            f.task.completion_intent = Some(intent.clone());
            f.task.detail.summary.specialist_id = SpecialistId::DomainAnalystV18;
            f.task.detail.specialist = SpecialistId::DomainAnalystV18.definition().unwrap();
            f.metadata = completion::derive_protected_metadata(&f.task, vec![], &[]).unwrap();
            let tx = f.store.conn.unchecked_transaction().unwrap();
            admission_journal::tests::save_task(&tx, &f.task);
            tx.commit().unwrap();
        });
        Self::from_fixture(FinishFixture::from_base(base, false), kind, false)
    }
    pub(in crate::jobs::investigations) fn from_fixture(
        mut f: FinishFixture,
        kind: ContextPreviewKind,
        refusal: bool,
    ) -> Self {
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
        let input = InvestigationInput::prepare_v9_with_policy(
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
        .unwrap();
        snapshot.attach_acquisition(input).unwrap()
    }
    pub fn publish(&mut self, snapshot: &MetadataContinuationSnapshot) -> InvestigationDetail {
        self.try_publish(snapshot, false, |_| Ok(())).unwrap()
    }
    pub fn try_publish(
        &mut self,
        snapshot: &MetadataContinuationSnapshot,
        stale: bool,
        retained: impl FnOnce(
            &crate::sources::RetainedSourceView<'_>,
        ) -> Result<(), InvestigationStoreError>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        let actual = self.candidate(snapshot);
        let candidate = &actual;
        let proof = self
            .f
            .input
            .certify_context_preview_v9_with_policy(
                candidate,
                self.kind,
                &self.raw,
                snapshot.admitted_usage(),
                &provider(false).0,
                &limits(),
                snapshot.next_action_id(),
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
        let candidate = if stale { &self.f.input } else { candidate };
        let prepared = firewall()
            .prepare_bounded(
                &provider(false),
                &candidate.action(snapshot.next_action_id().into()).unwrap(),
                &limits(),
            )
            .unwrap();
        let active_milliseconds = self.read().active_milliseconds;
        self.f.base.f.store.admit_next_prompt_metadata(
            &self.f.base.f.execution,
            snapshot.revision(),
            &MetadataAdmission {
                response: &self.response,
                original_action: &self.original,
                current: &self.f.input,
                candidate,
                current_prepared: &self.f.prepared,
                candidate_prepared: &prepared,
                proof: &proof,
                kind: self.kind,
                active_milliseconds,
            },
            &self.raw,
            retained,
        )
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
            finish_feedback_witness: None,
            finish_telemetry: None,
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
            ContextPreviewKind::Artifact => {
                unreachable!("artifact continuations are tested separately")
            }
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
                let target = if self
                    .f
                    .base
                    .f
                    .task
                    .detail
                    .summary
                    .specialist_id
                    .uses_acquisition_choices()
                {
                    self.f
                        .base
                        .f
                        .request
                        .obligations
                        .sources_with_policy(&INVESTIGATION_V2_POLICY)
                        .unwrap()
                        .into_iter()
                        .next()
                } else {
                    None
                };
                let mut query = query;
                if let Some(target) = target {
                    let agents::TaskFactKey::Node { id } = &target.fact else {
                        panic!("node fixture");
                    };
                    query.scope = InvestigationScope::Node { id: id.clone() };
                    query.kind = Some(InvestigationFactKind::Node);
                    query.max_facts = 1; // Same exact-target selector offered by @18.
                    self.raw = serde_json::json!({"context":{"context_id":self.f.base.f.request.expected_context_id,
                        "coverage":prior.coverage.as_ref().unwrap().qualification,
                        "response":{"snapshot_id":prior.scope_snapshot_id,"view":"recovered_graph","total_selected":1,
                        "facts":[{"reference":target.fact,"properties":{"note":"bounded native fixture"}}],"next_cursor":null}},
                        "evidence_options":[{"fact":target.fact,"origin":"captured_primary_source_metadata","ranges":[]}]}).to_string();
                    self.ledger.citations.push(InvestigationCitation {
                        citation_id: "fact:checkout".into(),
                        fact: target.fact.clone(),
                        fact_digest: target.fact_digest.clone(),
                        source: None,
                        role: None,
                        index: None,
                        text_hash: None,
                        origin: InvestigationEvidenceOrigin::GraphMetadata,
                    });
                    self.ledger.selected_facts.push(agents::TaskFactSelection {
                        fact: target.fact,
                        fact_digest: target.fact_digest.clone(),
                        binding: Some(agents::TaskSourceAssociation {
                            repo_key: target.source.repo.clone(),
                            receipt_id: target.receipt_id.clone(),
                            emitted_fact_digest: target.fact_digest,
                        }),
                    });
                    self.ledger
                        .receipt_references
                        .push(InvestigationReceiptReference {
                            source_id: target.registered_source_id,
                            repo_key: target.source.repo,
                            receipt_id: target.receipt_id,
                        });
                }

                self.ledger.queries.push(InvestigationQueryManifest {
                    query: query.clone(),
                    response_hash: core_prov::content_hash(self.raw.as_bytes()),
                    response_bytes: self.raw.len(),
                    returned_facts: self.ledger.selected_facts.len(),
                    total_selected: self.ledger.selected_facts.len(),
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
                let request = prior
                    .coverage_reads
                    .first()
                    .map(|read| read.request.clone())
                    .unwrap_or_else(|| {
                        let target = self
                            .f
                            .base
                            .f
                            .request
                            .obligations
                            .obligations
                            .iter()
                            .find_map(|item| {
                                if let Obligation::Inventory { target, .. } = item {
                                    Some(target)
                                } else {
                                    None
                                }
                            })
                            .unwrap();
                        InvestigationCoverageRead {
                            context_id: target.context_id.clone(),
                            repo: target.repo.clone(),
                            inventory_hash: target.inventory_hash.clone(),
                            max_bytes: MAX_QUERY_BYTES,
                        }
                    });
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
