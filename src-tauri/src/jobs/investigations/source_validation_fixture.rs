use super::*;
use read_reservations::{ReadBinding, Reservation};
use serde_json::json;
#[path = "source_validation_query_fixture.rs"]
mod query_fixture;
use query_fixture::*;

/// Explicit internal records only: real saved invocation, disclosure, journal,
/// owner lock and charge transaction; no actual parser/source-read assertion.
pub(in crate::jobs::investigations) fn fixture() -> (completion::InternalFixture, Reservation) {
    fixture_with_obligation(false)
}
pub(in crate::jobs::investigations) fn fixture_with_source_obligation()
-> (completion::InternalFixture, Reservation) {
    fixture_with_obligation(true)
}
pub(in crate::jobs::investigations) fn gated_source_obligation_fixture()
-> (completion::InternalFixture, Reservation) {
    let (f, source) = invocation_fixture(true, true);
    reserve_fixture(f, source)
}
fn fixture_with_obligation(source_obligation: bool) -> (completion::InternalFixture, Reservation) {
    let (f, source) = invocation_fixture(source_obligation, false);
    reserve_fixture(f, source)
}
pub(in crate::jobs::investigations) fn source_invocation_fixture()
-> (completion::InternalFixture, InvestigationInspectionTarget) {
    invocation_fixture(true, false)
}
fn invocation_fixture(
    source_obligation: bool,
    gated: bool,
) -> (completion::InternalFixture, InvestigationInspectionTarget) {
    let mut target = None;
    let mut f = completion::internal_fixture_with_initial_ledger(|store, task, ledger, request| {
        if gated {
            request.specialist_id = "domain-analyst@18".into();
            task.detail.summary.specialist_id = SpecialistId::DomainAnalystV18;
            task.detail.specialist = SpecialistId::DomainAnalystV18.definition().unwrap();
        }
        let mut source:InvestigationInspectionTarget=serde_json::from_value(json!({
            "schema_version":1,"target_id":"","graph_snapshot_id":ledger.graph_snapshot_id,
            "fact":{"kind":"node","id":"node:checkout"},"fact_digest":format!("node-v1:{}","b".repeat(64)),
            "registered_source_id":format!("src_{}","d".repeat(32)),"receipt_id":format!("ts-primary-v2:{}","c".repeat(64)),
            "receipt_inventory_index":0,"role":"provenance","index":0,
            "source":{"repo":"fixture/repo","path":"guard.ts","byte_start":0,"byte_end":12,"commit_sha":"pinned-revision"},
            "captured":{"file":{"source_id":format!("src_{}","d".repeat(32)),"capture_id":format!("capture-v1:{}","e".repeat(64)),
            "path":"guard.ts","digest":core_prov::content_hash(b"private text"),"byte_len":12},"byte_start":0,"byte_end":12}
        })).unwrap();
        source.target_id = source.content_id().unwrap();
        source.validate().unwrap();
        if source_obligation {
            let provider = llm::OllamaProvider::new(
                "http://127.0.0.1:11434",
                "test-model",
                std::time::Duration::from_secs(1),
            )
            .unwrap();
            let profile = llm::LlmProvider::bounded_profile(&provider).unwrap();
            task.detail.provider.provider_id = profile.provider_id;
            task.detail.provider.endpoint = profile.endpoint_id;
            task.detail.provider.model = profile.requested_model;
            task.detail.provider.protocol_version = Some(profile.protocol_version);
            request.expected_graph_revision = Some(source.graph_snapshot_id.clone());
            task.expected_graph_revision = request.expected_graph_revision.clone();
            let mut obligation = Obligation::Source {
                obligation_id: String::new(),
                target: Box::new(source.clone()),
            };
            let id = obligation.content_id().unwrap();
            if let Obligation::Source { obligation_id, .. } = &mut obligation {
                *obligation_id = id;
            }
            if gated {
                request.obligations.obligations.push(obligation);
            } else {
                request.obligations.obligations = vec![obligation];
            }
        }
        ledger.selected_facts.push(agents::TaskFactSelection {
            fact: source.fact.clone(),
            fact_digest: source.fact_digest.clone(),
            binding: Some(agents::TaskSourceAssociation {
                repo_key: source.source.repo.clone(),
                receipt_id: source.receipt_id.clone(),
                emitted_fact_digest: source.fact_digest.clone(),
            }),
        });
        ledger
            .receipt_references
            .push(InvestigationReceiptReference {
                source_id: source.registered_source_id.clone(),
                repo_key: source.source.repo.clone(),
                receipt_id: source.receipt_id.clone(),
            });
        ledger.citations.push(InvestigationCitation {
            citation_id: "fact:checkout".into(),
            fact: source.fact.clone(),
            fact_digest: source.fact_digest.clone(),
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        });
        let raw=json!({"context":{"context_id":ledger.coverage.as_ref().unwrap().context_id,
            "coverage":ledger.coverage.as_ref().unwrap().qualification,
            "response":{"snapshot_id":ledger.scope_snapshot_id,"view":"recovered_graph","total_selected":1,
            "facts":[{"reference":source.fact,"properties":{"note":"bounded native fixture"}}],"next_cursor":null}},
            "evidence_options":[{"fact":source.fact,"origin":"captured_primary_source_metadata","ranges":[]}]}).to_string();
        ledger.queries.push(InvestigationQueryManifest {
            query: InvestigationQuery {
                scope: InvestigationScope::All,
                kind: Some(InvestigationFactKind::Node),
                labels: vec![],
                max_facts: 1,
                max_bytes: 32768,
                cursor: None,
            },
            response_hash: core_prov::content_hash(raw.as_bytes()),
            response_bytes: raw.len(),
            returned_facts: 1,
            total_selected: 1,
            has_more: false,
        });
        ledger
            .validate_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        let acquired = ledger.clone();
        *ledger = initial_ledger_for_query(&acquired);
        task.ledger_hash = Some(
            ledger
                .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap(),
        );
        task.detail.citations = ledger.citations.clone();
        task.detail.usage.selected_facts = 0;
        task.detail.usage.tool_actions = 0;
        let tx = store.conn.transaction().unwrap();
        tx.execute_batch("DROP TRIGGER investigation_inputs_no_update")
            .unwrap();
        let bytes = encode(ledger, MAX_MANIFEST_BYTES).unwrap();
        tx.execute(
            "UPDATE investigation_inputs SET body=?2,body_hash=?3 WHERE task_id=?1 AND revision=0",
            params![
                task.detail.summary.investigation_id,
                bytes,
                core_prov::content_hash(&bytes)
            ],
        )
        .unwrap();
        tx.execute_batch("CREATE TRIGGER investigation_inputs_no_update BEFORE UPDATE ON investigation_inputs BEGIN SELECT RAISE(ABORT, 'immutable investigation record'); END").unwrap();
        tx.commit().unwrap();
        target = Some((source, acquired, raw));
        vec![]
    });
    let (source, acquired, raw) = target.unwrap();
    publish_query_and_start_source_call(&mut f, acquired, raw);
    (f, source)
}

fn reserve_fixture(
    mut f: completion::InternalFixture,
    source: InvestigationInspectionTarget,
) -> (completion::InternalFixture, Reservation) {
    let read = ReadIntent::Source {
        fact: source.fact.clone(),
        role: source.role,
        index: source.index,
    };
    let response = InvestigationResponseMeta {
        finish_feedback_witness: None,
        finish_telemetry: None,
        step_id: f.step.step_id.clone(),
        response_hash: core_prov::content_hash(b"actual saved scripted response"),
        action_hash: staged::read_action_hash(&read).unwrap(),
        observed_model: Some("scripted-model".into()),
        reported_input_tokens: Some(10),
        reported_output_tokens: Some(5),
        reported_cache_read_tokens: None,
        active_milliseconds: f.task.detail.usage.active_milliseconds + 2,
    };
    let tx = f.store.conn.transaction().unwrap();
    let (_, usage) = transitions::validate_response(&tx, &f.task, &response).unwrap();
    let archive_hash: String = tx
        .query_row(
            "SELECT body_hash FROM investigation_coverage_archives WHERE task_id=?1",
            [&f.basis.task_id],
            |row| row.get(0),
        )
        .unwrap();
    let r = Reservation {
        schema_version: 2,
        source,
        storage_claim: read_reservations::StorageClaim {
            validation_bytes: 8 * 1024,
            settlement_bytes: 24 * 1024,
            validation_events: 1,
            settlement_events: 2,
            settlement_attempts: 1,
        },
        binding: ReadBinding {
            task_id: f.basis.task_id.clone(),
            step_id: f.step.step_id.clone(),
            request_hash: f.task.intent_hash.clone(),
            specialist_id: f.task.detail.summary.specialist_id,
            policy_profile: f.task.detail.limits.profile.clone(),
            effective_limits: None,
            execution_job: f.task.detail.summary.job_id,
            execution_generation: f.task.execution.as_ref().unwrap().generation,
            owner_basis_hash: owner_basis_hash(&f.task).unwrap(),
            original_response_hash: response.response_hash.clone(),
            original_action_hash: response.action_hash.clone(),
            resolved_read_hash: read.fingerprint().unwrap(),
            original_read: read,
            base_ledger_hash: f.basis.input_ledger_hash.clone(),
            base_ledger_revision: f.ledger.revision,
            context_id: f.ledger.coverage.as_ref().unwrap().context_id.clone(),
            archive_hash,
            base_usage: usage.clone(),
            prior_completion_hash: f
                .metadata
                .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap(),
            certification_basis_hash: core_prov::content_hash(
                b"test-only existing reservation certificate identity",
            ),
        },
    };
    r.validate().unwrap();
    insert_body(
        &tx,
        "investigation_responses",
        &f.basis.task_id,
        &f.step.step_id,
        &response,
        4096,
    )
    .unwrap();
    read_reservations::initialize(&tx).unwrap();
    read_reservations::insert_reservation(&tx, &r).unwrap();
    let expected = f.task.detail.summary.revision;
    f.task.phase = Phase::Reservation;
    f.task.pending = None;
    f.task.detail.usage = usage;
    f.task.pending_source_read = Some(PendingSourceRead {
        step_id: f.step.step_id.clone(),
        reservation_hash: r.fingerprint().unwrap(),
    });
    bump(&tx, &mut f.task).unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ModelCompleted,
        Some(&f.step.step_id),
        None,
        "Test saved source response; no provider ran.",
    )
    .unwrap();
    event(
        &tx,
        &mut f.task,
        InvestigationEventKind::ActionAdmitted,
        Some(&f.step.step_id),
        Some(InvestigationTool::ReadEvidence),
        "Test internal source reservation; no source access.",
    )
    .unwrap();
    save_next_prompt_task(&tx, &f.task, expected, false).unwrap();
    validate_saved_pending(&tx, &f.task).unwrap();
    tx.commit().unwrap();
    (f, r)
}
pub(super) fn basis<'a>(
    f: &'a completion::InternalFixture,
    hash: &'a str,
) -> SourceValidationBasis<'a> {
    SourceValidationBasis {
        task_id: &f.basis.task_id,
        step_id: &f.step.step_id,
        reservation_hash: hash,
        revision: f.task.detail.summary.revision,
    }
}
pub(super) fn charge(
    f: &mut completion::InternalFixture,
    r: &Reservation,
) -> Result<CommittedSourceValidation, InvestigationStoreError> {
    let hash = r.fingerprint()?;
    f.store.charge_prepared_source_validation(
        &f._execution_guard,
        &SourceValidationBasis {
            task_id: &f.basis.task_id,
            step_id: &f.step.step_id,
            reservation_hash: &hash,
            revision: f.task.detail.summary.revision,
        },
        &r.source,
        &r.binding.base_ledger_hash,
    )
}
pub(super) fn saved(f: &completion::InternalFixture) -> SavedRead {
    let tx = f.store.conn.unchecked_transaction().unwrap();
    read_reservations::load(&tx, &f.basis.task_id, &f.step.step_id)
        .unwrap()
        .unwrap()
}

pub struct SourceValidationInitialData {
    pub ledger: InvestigationInputLedger,
    pub query_pages: Vec<String>,
    pub archive: InvestigationCoverageArchive,
    pub target: InvestigationInspectionTarget,
}
pub struct SourceValidationTestFixture {
    inner: completion::InternalFixture,
    reservation: Reservation,
    hash: String,
}
impl SourceValidationTestFixture {
    pub fn lose_charge_ack(&self) {
        CHARGE_FAILURE.with(|slot| slot.set(Some(ChargeFailure::AfterCommit)));
    }
    pub fn basis(&self) -> SourceValidationBasis<'_> {
        basis(&self.inner, &self.hash)
    }
    pub fn execution(&self) -> &JobExecution {
        &self.inner._execution_guard
    }
    pub fn charged_bytes_after_reopen(&self) -> u64 {
        let reopened = JobStore::open(self.inner.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let task = load_next_prompt_task_base(&tx, &self.inner.basis.task_id).unwrap();
        let row = read_reservations::load(&tx, &self.inner.basis.task_id, &self.inner.step.step_id)
            .unwrap()
            .unwrap();
        let bytes = row.charge.as_ref().unwrap().validation_bytes;
        assert_eq!(bytes, self.reservation.source.captured.file.byte_len);
        assert_eq!(
            task.detail.usage.captured_validation_bytes,
            self.reservation
                .binding
                .base_usage
                .captured_validation_bytes
                + bytes
        );
        bytes
    }
}

/// App adapter tests supply real parser/receipt/query data in this exact private
/// directory/state.db. Only task3/reservation construction is explicit test data;
/// prompt9 disclosure and saved call stamps use production builders/validators.
pub fn captured_source_fixture(
    initialize: impl FnOnce(&std::path::Path) -> SourceValidationInitialData,
) -> SourceValidationTestFixture {
    let (f, source, _) = captured_source_invocation(initialize);
    let (inner, reservation) = reserve_fixture(f, source);
    let hash = reservation.fingerprint().unwrap();
    SourceValidationTestFixture {
        inner,
        reservation,
        hash,
    }
}

pub(in crate::jobs::investigations) fn captured_source_invocation(
    initialize: impl FnOnce(&std::path::Path) -> SourceValidationInitialData,
) -> (
    completion::InternalFixture,
    InvestigationInspectionTarget,
    String,
) {
    captured_source_invocation_for(false, initialize)
}
pub(in crate::jobs::investigations) fn captured_source_invocation_for(
    gated: bool,
    initialize: impl FnOnce(&std::path::Path) -> SourceValidationInitialData,
) -> (
    completion::InternalFixture,
    InvestigationInspectionTarget,
    String,
) {
    captured_source_invocation_for_role(gated.then_some(SpecialistId::DomainAnalystV18), initialize)
}
pub(in crate::jobs::investigations) fn captured_source_invocation_for_role(
    role: Option<SpecialistId>,
    initialize: impl FnOnce(&std::path::Path) -> SourceValidationInitialData,
) -> (
    completion::InternalFixture,
    InvestigationInspectionTarget,
    String,
) {
    let mut target = None;
    let mut f = completion::internal_fixture_with_initial_ledger(|store, task, ledger, request| {
        if let Some(role) = role {
            assert!(role.uses_acquisition_choices());
            request.specialist_id = serde_json::to_value(role).unwrap().as_str().unwrap().into();
            task.detail.summary.specialist_id = role;
            task.detail.specialist = role.definition().unwrap();
        }
        let provider = llm::OllamaProvider::new(
            "http://127.0.0.1:11434",
            "test-model",
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        let profile = llm::LlmProvider::bounded_profile(&provider).unwrap();
        task.detail.provider.provider_id = profile.provider_id;
        task.detail.provider.endpoint = profile.endpoint_id;
        task.detail.provider.model = profile.requested_model;
        task.detail.provider.protocol_version = Some(profile.protocol_version);
        let path = store.namespace.store_path.parent().unwrap();
        let data = initialize(path);
        let acquired = data.ledger;
        *ledger = initial_ledger_for_query(&acquired);
        let proof = ledger.coverage.as_ref().unwrap();
        request.expected_graph_revision = Some(ledger.graph_snapshot_id.clone());
        request.expected_context_id = proof.context_id.clone();
        let mut obligation = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(data.target.clone()),
        };
        let id = obligation.content_id().unwrap();
        if let Obligation::Source { obligation_id, .. } = &mut obligation {
            *obligation_id = id;
        }
        request.obligations.obligations = vec![obligation];
        task.expected_graph_revision = request.expected_graph_revision.clone();
        task.detail.summary.graph_snapshot_id = Some(ledger.graph_snapshot_id.clone());
        task.detail.scope_snapshot_id = Some(ledger.scope_snapshot_id.clone());
        task.detail.coverage_context = Some(InvestigationCoverageContext {
            context_id: proof.context_id.clone(),
            coverage: proof.qualification.clone(),
        });
        task.ledger_revision = Some(ledger.revision);
        task.ledger_hash = Some(
            ledger
                .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap(),
        );
        task.detail.citations = ledger.citations.clone();
        task.detail.usage.selected_facts = ledger.selected_facts.len();
        task.detail.usage.tool_actions = 0;
        let tx = store.conn.transaction().unwrap();
        for table in ["investigation_inputs", "investigation_coverage_archives"] {
            tx.execute_batch(&format!("DROP TRIGGER {table}_no_update"))
                .unwrap();
        }
        let bytes = encode(ledger, MAX_MANIFEST_BYTES).unwrap();
        tx.execute(
            "UPDATE investigation_inputs SET body=?2,body_hash=?3 WHERE task_id=?1 AND revision=0",
            params![
                task.detail.summary.investigation_id,
                bytes,
                core_prov::content_hash(&bytes)
            ],
        )
        .unwrap();
        let bytes = encode(&data.archive, MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES).unwrap();
        tx.execute(
            "UPDATE investigation_coverage_archives SET body=?2,body_hash=?3 WHERE task_id=?1",
            params![
                task.detail.summary.investigation_id,
                bytes,
                core_prov::content_hash(&bytes)
            ],
        )
        .unwrap();
        tx.execute_batch("CREATE TRIGGER investigation_inputs_no_update BEFORE UPDATE ON investigation_inputs BEGIN SELECT RAISE(ABORT, 'immutable investigation record'); END").unwrap();
        tx.execute_batch("CREATE TRIGGER investigation_coverage_archives_no_update BEFORE UPDATE ON investigation_coverage_archives BEGIN SELECT RAISE(ABORT, 'immutable investigation coverage archive'); END").unwrap();
        tx.commit().unwrap();
        target = Some((data.target, acquired, data.query_pages));
        vec![]
    });
    let (source, acquired, mut pages) = target.unwrap();
    assert_eq!(pages.len(), 1);
    let raw = pages.remove(0);
    publish_query_and_start_source_call(&mut f, acquired, raw.clone());
    (f, source, raw)
}
