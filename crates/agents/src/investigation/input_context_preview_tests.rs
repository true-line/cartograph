use super::*;
use context_hub::{
    ContextSnapshot,
    coverage::{CoverageQualification, ScopedRepo},
};
use core_graph::coverage::{CoverageRecord, StoredCoverage};
use llm::{EgressFirewall, EgressPolicy, LlmProvider, OllamaProvider};
use obligations::{InventoryTarget, Obligation, ObligationEnvelope, StagedObligationRequest};
use serde_json::json;

struct Fixture {
    ledger: InvestigationInputLedger,
    archive: InvestigationCoverageArchive,
    read: InvestigationCoverageRead,
    request: StagedObligationRequest,
    metadata: ProtectedCompletionMetadata,
}
impl Fixture {
    fn new(role: SpecialistId, inventory: &str) -> Self {
        let record = CoverageRecord {
            repo_key: "acme/api".into(),
            registered_source_id: Some("source:api".into()),
            ingest_id: "ingest:fixture".into(),
            config_hash: "config:fixture".into(),
            inventory_json: json!({
                "inventory": context_hub::coverage::INVENTORY_VERSION,
                "repo": "acme/api", "commit": "a".repeat(40), "layer_hints": [],
                "producers": [], "unsupported": [], "limited": false,
                "fixture_metadata": serde_json::from_str::<Value>(inventory).unwrap(),
            })
            .to_string(),
        };
        let stored = StoredCoverage {
            inventory_hash: record.inventory_hash(),
            record,
        };
        let qualification = CoverageQualification::new(
            &[ScopedRepo {
                repo_key: "acme/api".into(),
                registered_source_id: Some("source:api".into()),
                config_hash: "config:fixture".into(),
                layer_hints: None,
            }],
            std::slice::from_ref(&stored),
        )
        .unwrap();
        let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
        let proof = qualification.durable_proof(snapshot.id()).unwrap();
        let read = InvestigationCoverageRead {
            context_id: proof.context_id.clone(),
            repo: "acme/api".into(),
            inventory_hash: stored.inventory_hash.clone(),
            max_bytes: MAX_QUERY_BYTES,
        };
        let ledger = InvestigationInputLedger {
            schema_version: 2,
            coverage: Some(proof.clone()),
            coverage_reads: vec![],
            artifact_reads: vec![],
            graph_snapshot_id: snapshot.id().into(),
            scope_snapshot_id: snapshot.id().into(),
            revision: 0,
            selected_facts: vec![],
            receipt_references: vec![],
            citations: vec![],
            queries: vec![],
            supplied_history_hash: None,
            supplied_history_bytes: 0,
        };
        let mut obligation = Obligation::Inventory {
            obligation_id: String::new(),
            target: InventoryTarget {
                context_id: read.context_id.clone(),
                repo: read.repo.clone(),
                inventory_hash: read.inventory_hash.clone(),
            },
        };
        let id = obligation.content_id().unwrap();
        if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
            *obligation_id = id;
        }
        let request = StagedObligationRequest {
            schema_version: 7,
            request_nonce: "context-preview".into(),
            specialist_id: serde_json::to_value(role).unwrap().as_str().unwrap().into(),
            obligations: ObligationEnvelope {
                schema_version: 2,
                contract: "investigation-completion-obligations@2".into(),
                obligations: vec![obligation],
            },
            inspection_requirements: None,
            question: "Describe the retained metadata.".into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: INVESTIGATION_V2_POLICY.profile().into(),
            expected_graph_revision: Some(snapshot.id().into()),
            expected_context_id: read.context_id.clone(),
            conversation_id: None,
            parent_id: None,
        };
        let metadata = ProtectedCompletionMetadata {
            schema_version: 1,
            request_hash: request
                .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap(),
            obligations_hash: request
                .obligations
                .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap(),
            completed_attempt_ids: vec![],
            refusal_count: 0,
            last_refusal_id: None,
            feedback: None,
        };
        Self {
            ledger,
            archive: InvestigationCoverageArchive::new(proof, vec![stored]).unwrap(),
            read,
            request,
            metadata,
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn build(
        &self,
        role: SpecialistId,
        ledger: InvestigationInputLedger,
        previous: Option<InvestigationInputLedger>,
        pages: &[&str],
        details: &[&str],
        usage: &InvestigationUsage,
        metadata: &ProtectedCompletionMetadata,
        tool: Option<InvestigationTool>,
        count: u32,
        feedback: Option<&AdmissionCapacityFeedback>,
    ) -> InvestigationInput {
        let basis = InvestigationTurnBasis {
            input_ledger_hash: ledger
                .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap(),
            previous_ledger: previous,
            completed_tool: tool,
        };
        let mode = metadata
            .next_call_mode_with_policy(usage, &INVESTIGATION_V2_POLICY)
            .unwrap();
        InvestigationInput::prepare_v9_with_policy(
            &self.request.question,
            role,
            ledger,
            pages,
            &[],
            &[],
            usage,
            &basis,
            &self.request,
            metadata,
            details,
            mode,
            count,
            feedback,
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap()
    }
    fn candidate(
        &self,
        role: SpecialistId,
        kind: ContextPreviewKind,
        base: &InvestigationInput,
        before: &InvestigationUsage,
        previous_pages: &[&str],
        previous_details: &[&str],
    ) -> (InvestigationInput, String, InvestigationUsage) {
        let mut admitted = before.clone();
        admitted.model_invocations += 1;
        admitted.generated_token_reservations +=
            CompletionLimits::default().values().max_output_tokens;
        admitted.active_milliseconds += 1;
        admitted.reported_input_tokens = Some(admitted.reported_input_tokens.unwrap_or(0) + 30);
        admitted.reported_output_tokens = Some(admitted.reported_output_tokens.unwrap_or(0) + 10);
        let mut after = admitted.clone();
        after.tool_actions += 1;
        after.active_milliseconds += 1;
        let mut ledger = base.ledger.clone();
        ledger.revision += 1;
        let base_prompt: Value = serde_json::from_str(&base.payload.prompt).unwrap();
        let mut metadata: ProtectedCompletionMetadata =
            serde_json::from_value(base_prompt["completion_metadata"].clone()).unwrap();
        let count = base_prompt["turn_progress"]["capacity_refusal_count"]
            .as_u64()
            .unwrap() as u32;
        let mut pages = previous_pages.to_vec();
        let mut details = previous_details.to_vec();
        let raw = match kind {
            ContextPreviewKind::Query => {
                let key = crate::TaskFactKey::Node {
                    id: "preview:fact".into(),
                };
                if ledger.selected_facts.is_empty() {
                    let digest = format!("node-v1:{}", "a".repeat(64));
                    ledger.selected_facts.push(crate::TaskFactSelection {
                        fact: key.clone(),
                        fact_digest: digest.clone(),
                        binding: None,
                    });
                    ledger.citations.push(InvestigationCitation {
                        citation_id: "fact-1".into(),
                        fact: key.clone(),
                        fact_digest: digest,
                        source: None,
                        role: None,
                        index: None,
                        text_hash: None,
                        origin: InvestigationEvidenceOrigin::GraphMetadata,
                    });
                }
                let raw = json!({"context":{"context_id":self.read.context_id,
                    "coverage":ledger.coverage.as_ref().unwrap().qualification,
                    "response":{"snapshot_id":ledger.scope_snapshot_id,"view":"recovered_graph","total_selected":1,
                        "facts":[{"reference":key,"properties":{"untrusted":"λ / \u{1} / password=secret"}}],"next_cursor":null}},
                    "evidence_options":[{"fact":key,"origin":"recovered_graph_metadata","ranges":[]}]}).to_string();
                ledger.queries.push(InvestigationQueryManifest {
                    query: InvestigationQuery {
                        scope: InvestigationScope::All,
                        kind: None,
                        labels: vec![],
                        max_facts: MAX_QUERY_FACTS,
                        max_bytes: raw.len(),
                        cursor: None,
                    },
                    response_hash: content_hash(raw.as_bytes()),
                    response_bytes: raw.len(),
                    returned_facts: 1,
                    total_selected: 1,
                    has_more: false,
                });
                raw
            }
            ContextPreviewKind::Inventory => {
                let raw = serde_json::to_string(&self.archive.read(&self.read).unwrap()).unwrap();
                let mut request = self.read.clone();
                request.max_bytes = raw.len();
                ledger
                    .coverage_reads
                    .push(InvestigationCoverageReadManifest {
                        request,
                        response_hash: content_hash(raw.as_bytes()),
                        response_bytes: raw.len(),
                    });
                metadata.completed_attempt_ids = (0..ledger.coverage_reads.len())
                    .map(|index| format!("{index:064x}"))
                    .collect();
                raw
            }
        };
        match kind {
            ContextPreviewKind::Query => pages.push(&raw),
            ContextPreviewKind::Inventory => details.push(&raw),
        }
        after.selected_facts = ledger.selected_facts.len();
        let candidate = self.build(
            role,
            ledger,
            Some(base.ledger.clone()),
            &pages,
            &details,
            &after,
            &metadata,
            Some(kind.tool()),
            count,
            None,
        );
        (candidate, raw, admitted)
    }
}
fn provider() -> OllamaProvider {
    OllamaProvider::new(
        "http://127.0.0.1:11434",
        "preview-fixture",
        std::time::Duration::from_secs(1),
    )
    .unwrap()
}
fn certify(
    base: &InvestigationInput,
    candidate: &InvestigationInput,
    kind: ContextPreviewKind,
    raw: &str,
    usage: &InvestigationUsage,
) -> Result<ContextPreviewBound, InvestigationError> {
    base.certify_context_preview_v9_with_policy(
        candidate,
        kind,
        raw,
        usage,
        &provider().bounded_profile().unwrap(),
        &CompletionLimits::default(),
        "step:preview",
        &INVESTIGATION_V2_POLICY,
    )
}

#[test]
fn context_preview_binds_actual_query_and_inventory_serialization_and_continuation() {
    // AC-0287, AC-0439: deterministic retained metadata, no source/provider dispatch.
    for role in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
        SpecialistId::DomainAnalystV12,
        SpecialistId::DomainAnalystV13,
    ] {
        let fixture = Fixture::new(
            role,
            &json!({"metadata":"Unicode λ / control \u{1} / untrusted description"}).to_string(),
        );
        for kind in [ContextPreviewKind::Query, ContextPreviewKind::Inventory] {
            let usage = InvestigationUsage::default();
            let base = fixture.build(
                role,
                fixture.ledger.clone(),
                None,
                &[],
                &[],
                &usage,
                &fixture.metadata,
                None,
                0,
                None,
            );
            let (candidate, raw, admitted) = fixture.candidate(role, kind, &base, &usage, &[], &[]);
            let proof = certify(&base, &candidate, kind, &raw, &admitted).unwrap();
            let profile = provider().bounded_profile().unwrap();
            let limits = CompletionLimits::default();
            assert_eq!(
                proof.measurements().candidate,
                llm::bounded::measure_bounded_action(
                    &profile,
                    &candidate.action("step:preview".into()).unwrap(),
                    &limits
                )
                .unwrap()
            );
            assert_eq!(
                proof.measurements().continuation,
                candidate
                    .measure_v9_reserve_with_policy(
                        &profile,
                        &limits,
                        "step:preview",
                        &INVESTIGATION_V2_POLICY
                    )
                    .unwrap()
            );
            proof
                .verify_with_policy(
                    &base,
                    &candidate,
                    kind,
                    &raw,
                    &admitted,
                    &profile,
                    &limits,
                    "step:preview",
                    &INVESTIGATION_V2_POLICY,
                )
                .unwrap();
            let firewall = EgressFirewall::new(EgressPolicy::default());
            let prepared = firewall
                .prepare_bounded(
                    &provider(),
                    &candidate.action("step:preview".into()).unwrap(),
                    &limits,
                )
                .unwrap();
            candidate
                .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
                .unwrap_or_else(|error| panic!("actual {role:?}/{kind:?} seal failed: {error:?}"));
            assert!(
                proof
                    .verify_with_policy(
                        &base,
                        &candidate,
                        kind,
                        &raw,
                        &admitted,
                        &profile,
                        &limits,
                        "step:other",
                        &INVESTIGATION_V2_POLICY
                    )
                    .is_err()
            );
            let mut rebound_profile = profile.clone();
            rebound_profile.requested_model = "another-fixture".into();
            assert!(
                proof
                    .verify_with_policy(
                        &base,
                        &candidate,
                        kind,
                        &raw,
                        &admitted,
                        &rebound_profile,
                        &limits,
                        "step:preview",
                        &INVESTIGATION_V2_POLICY
                    )
                    .is_err()
            );
            assert!(certify(&base, &candidate, kind, &format!("{raw} "), &admitted).is_err());
        }
    }
}

#[test]
fn context_preview_size_does_not_override_nested_redaction_seal_failure() {
    // Wire fit alone cannot admit an actual representation rejected by sealing.
    // This calibrates the existing nested-string redaction behavior; it does
    // not weaken the redactor or protected-field/JSON admission policy.
    let role = SpecialistId::DomainAnalystV10;
    let fixture = Fixture::new(role, &json!({"metadata":"password=secret"}).to_string());
    let usage = InvestigationUsage::default();
    let base = fixture.build(
        role,
        fixture.ledger.clone(),
        None,
        &[],
        &[],
        &usage,
        &fixture.metadata,
        None,
        0,
        None,
    );
    let (candidate, raw, admitted) =
        fixture.candidate(role, ContextPreviewKind::Inventory, &base, &usage, &[], &[]);
    certify(
        &base,
        &candidate,
        ContextPreviewKind::Inventory,
        &raw,
        &admitted,
    )
    .unwrap();
    let action = candidate.action("step:preview".into()).unwrap();
    let prepared = EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(&provider(), &action, &CompletionLimits::default())
        .unwrap();
    assert_eq!(
        candidate.seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY),
        Err(InvestigationError::InvalidInput)
    );
}

#[test]
fn context_preview_rejects_wrong_kind_usage_source_and_metadata_binding() {
    let role = SpecialistId::DomainAnalystV10;
    let fixture = Fixture::new(role, "{}");
    let usage = InvestigationUsage::default();
    let base = fixture.build(
        role,
        fixture.ledger.clone(),
        None,
        &[],
        &[],
        &usage,
        &fixture.metadata,
        None,
        0,
        None,
    );
    let (candidate, raw, admitted) =
        fixture.candidate(role, ContextPreviewKind::Query, &base, &usage, &[], &[]);
    assert!(
        certify(
            &base,
            &candidate,
            ContextPreviewKind::Inventory,
            &raw,
            &admitted
        )
        .is_err()
    );
    let mut wrong = admitted.clone();
    wrong.evidence_requests += 1;
    assert!(certify(&base, &candidate, ContextPreviewKind::Query, &raw, &wrong).is_err());
    wrong = admitted.clone();
    wrong.generated_token_reservations -= 1;
    assert!(certify(&base, &candidate, ContextPreviewKind::Query, &raw, &wrong).is_err());
    for mutation in [
        "question",
        "history",
        "proof",
        "metadata",
        "progress",
        "usage",
        "span",
        "added_field",
        "removed_field",
    ] {
        let mut forged = candidate.clone();
        let mut prompt: Value = serde_json::from_str(&forged.payload.prompt).unwrap();
        match mutation {
            "question" => prompt["question"] = "Another question".into(),
            // AC-0327: borrowed comparisons must preserve exact enclosing keys.
            "added_field" => prompt["unexpected_field"] = true.into(),
            "removed_field" => {
                prompt.as_object_mut().unwrap().remove("question");
            }
            "history" => prompt["prior_findings_as_t3_history"] = json!(["injected"]),
            "proof" => prompt["context_id"] = "context-v2:wrong".into(),
            "metadata" => {
                prompt["completion_metadata"]["completed_attempt_ids"] = json!(["a".repeat(64)])
            }
            "progress" => prompt["turn_progress"]["input_revision"] = 99.into(),
            "usage" => prompt["consumed_budgets"]["evidence_bytes"] = 1.into(),
            "span" => forged.payload.spans.push(llm::PayloadSpan {
                id: "injected".into(),
                repo: "acme/api".into(),
                path: "file.ts".into(),
                byte_start: 0,
                byte_end: 1,
                commit_sha: "a".repeat(40),
                text: "x".into(),
            }),
            _ => unreachable!(),
        }
        forged.payload.prompt = serde_json::to_string(&prompt).unwrap();
        assert!(
            certify(&base, &forged, ContextPreviewKind::Query, &raw, &admitted).is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn context_preview_reserves_finish_only_and_rejects_tool_from_finish_only() {
    let role = SpecialistId::DomainAnalystV10;
    let fixture = Fixture::new(role, "{}");
    let limits = INVESTIGATION_V2_POLICY.limits();
    let usage = InvestigationUsage {
        model_invocations: limits.model_invocations - 3,
        generated_token_reservations: limits.generated_token_reservations
            - 3 * limits.generated_tokens_per_invocation,
        ..Default::default()
    };
    let base = fixture.build(
        role,
        fixture.ledger.clone(),
        None,
        &[],
        &[],
        &usage,
        &fixture.metadata,
        None,
        0,
        None,
    );
    let (candidate, raw, admitted) =
        fixture.candidate(role, ContextPreviewKind::Query, &base, &usage, &[], &[]);
    assert_eq!(
        serde_json::from_str::<Value>(&candidate.payload.prompt).unwrap()["turn_progress"]["admission_mode"],
        "finish_only"
    );
    let proof = certify(
        &base,
        &candidate,
        ContextPreviewKind::Query,
        &raw,
        &admitted,
    )
    .unwrap();
    assert!(
        proof
            .measurements()
            .continuation
            .choice
            .original_payload_bytes
            > 0
    );
    assert!(
        certify(
            &candidate,
            &candidate,
            ContextPreviewKind::Query,
            &raw,
            &admitted
        )
        .is_err()
    );
}

#[test]
fn context_preview_exact_wire_boundary_includes_simultaneous_feedback_reserve() {
    let role = SpecialistId::DomainAnalystV10;
    let fixture = Fixture::new(
        role,
        &json!({"control": "\u{1}".repeat(1000), "unicode": "λ\"\\".repeat(50)}).to_string(),
    );
    let usage = InvestigationUsage::default();
    let base = fixture.build(
        role,
        fixture.ledger.clone(),
        None,
        &[],
        &[],
        &usage,
        &fixture.metadata,
        None,
        0,
        None,
    );
    let (candidate, raw, admitted) =
        fixture.candidate(role, ContextPreviewKind::Inventory, &base, &usage, &[], &[]);
    let measurements = certify(
        &base,
        &candidate,
        ContextPreviewKind::Inventory,
        &raw,
        &admitted,
    )
    .unwrap()
    .measurements();
    let needed = [
        measurements.current,
        measurements.candidate,
        measurements.continuation.choice,
        measurements.continuation.finish_only,
    ]
    .into_iter()
    .map(|sizes| {
        sizes
            .raw_string_bytes
            .max(sizes.original_payload_bytes)
            .max(sizes.redacted_payload_bytes)
            .max(sizes.original_action_bytes.saturating_sub(4096))
    })
    .max()
    .unwrap();
    let mut values = CompletionLimits::default().values();
    values.input_bytes = needed;
    let exact = CompletionLimits::new(values).unwrap();
    let profile = provider().bounded_profile().unwrap();
    base.certify_context_preview_v9_with_policy(
        &candidate,
        ContextPreviewKind::Inventory,
        &raw,
        &admitted,
        &profile,
        &exact,
        "step:preview",
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    values.input_bytes -= 1;
    assert_eq!(
        base.certify_context_preview_v9_with_policy(
            &candidate,
            ContextPreviewKind::Inventory,
            &raw,
            &admitted,
            &profile,
            &CompletionLimits::new(values).unwrap(),
            "step:preview",
            &INVESTIGATION_V2_POLICY
        ),
        Err(InvestigationError::LimitExceeded)
    );
}

#[test]
fn context_preview_preserves_mixed_history_and_clears_only_latest_capacity_slot() {
    let role = SpecialistId::DomainAnalystV10;
    let fixture = Fixture::new(role, "{}");
    let usage = InvestigationUsage {
        model_invocations: 1,
        generated_token_reservations: 2048,
        ..Default::default()
    };
    let feedback = AdmissionCapacityFeedback {
        schema_version: 1,
        action_hash: "a".repeat(64),
        reason: CapacityRefusalReason::PromptCapacity,
    };
    let base = fixture.build(
        role,
        fixture.ledger.clone(),
        None,
        &[],
        &[],
        &usage,
        &fixture.metadata,
        None,
        1,
        Some(&feedback),
    );
    let (inventory, inventory_raw, admitted) =
        fixture.candidate(role, ContextPreviewKind::Inventory, &base, &usage, &[], &[]);
    certify(
        &base,
        &inventory,
        ContextPreviewKind::Inventory,
        &inventory_raw,
        &admitted,
    )
    .unwrap();
    let body: Value = serde_json::from_str(&inventory.payload.prompt).unwrap();
    assert_eq!(body["turn_progress"]["capacity_refusal_count"], 1);
    assert!(body["turn_progress"]["capacity_feedback"].is_null());
    let after: InvestigationUsage =
        serde_json::from_value(body["consumed_budgets"].clone()).unwrap();
    let (query, query_raw, admitted) = fixture.candidate(
        role,
        ContextPreviewKind::Query,
        &inventory,
        &after,
        &[],
        &[&inventory_raw],
    );
    certify(
        &inventory,
        &query,
        ContextPreviewKind::Query,
        &query_raw,
        &admitted,
    )
    .unwrap();
    let body: Value = serde_json::from_str(&query.payload.prompt).unwrap();
    assert_eq!(body["coverage_details"].as_array().unwrap().len(), 1);
    assert_eq!(
        body["completion_metadata"]["completed_attempt_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(body["turn_progress"]["capacity_refusal_count"], 1);
    let after: InvestigationUsage =
        serde_json::from_value(body["consumed_budgets"].clone()).unwrap();
    let (repeated, repeated_raw, admitted) = fixture.candidate(
        role,
        ContextPreviewKind::Inventory,
        &query,
        &after,
        &[&query_raw],
        &[&inventory_raw],
    );
    assert_eq!(repeated_raw, inventory_raw);
    assert_eq!(repeated.ledger.coverage_reads.len(), 2);
    assert_eq!(
        repeated.ledger.coverage_reads[1],
        inventory.ledger.coverage_reads[0]
    );
    certify(
        &query,
        &repeated,
        ContextPreviewKind::Inventory,
        &repeated_raw,
        &admitted,
    )
    .unwrap();
}

// AC-0260: repeated metadata identity is exact, never fuzzy evidence equality.
#[test]
fn repeated_metadata_identity_preserves_context_and_all_read_parameters() {
    let f = Fixture::new(SpecialistId::DomainAnalystV18, "{}");
    let mut ledger = f.ledger.clone();
    let read = f.read.clone();
    let action = QualifiedInvestigationAction::ReadCoverageInventory(read.clone());
    assert!(!action.repeats_acquired_metadata(&ledger));
    ledger
        .coverage_reads
        .push(InvestigationCoverageReadManifest {
            request: read.clone(),
            response_hash: "a".repeat(64),
            response_bytes: 1,
        });
    assert!(action.repeats_acquired_metadata(&ledger));
    let mut changed = read.clone();
    changed.max_bytes += 1;
    assert!(
        !QualifiedInvestigationAction::ReadCoverageInventory(changed)
            .repeats_acquired_metadata(&ledger)
    );
    let mut changed = read;
    changed.context_id.push('x');
    assert!(
        !QualifiedInvestigationAction::ReadCoverageInventory(changed)
            .repeats_acquired_metadata(&ledger)
    );
    let query: InvestigationQuery = serde_json::from_value(json!({
        "scope":{"type":"all"}, "kind":null, "labels":[],
        "max_facts":12, "max_bytes":16384, "cursor":null
    }))
    .unwrap();
    let context_id = ledger.coverage.as_ref().unwrap().context_id.clone();
    let action = QualifiedInvestigationAction::QueryContext {
        context_id: context_id.clone(),
        query: query.clone(),
    };
    assert!(!action.repeats_acquired_metadata(&ledger));
    ledger.queries.push(InvestigationQueryManifest {
        query: query.clone(),
        response_hash: "b".repeat(64),
        response_bytes: 1,
        returned_facts: 0,
        total_selected: 0,
        has_more: false,
    });
    assert!(action.repeats_acquired_metadata(&ledger));
    let mut changed = query.clone();
    changed.max_bytes += 1;
    assert!(
        !QualifiedInvestigationAction::QueryContext {
            context_id: context_id.clone(),
            query: changed
        }
        .repeats_acquired_metadata(&ledger)
    );
    assert!(
        !QualifiedInvestigationAction::QueryContext {
            context_id: format!("{context_id}x"),
            query
        }
        .repeats_acquired_metadata(&ledger)
    );
}

// AC-0768 / #1335: an @28 metadata preview accepts the candidate schema only
// as the validated per-call selection, shown in the prompt and charged by reserves.
#[test]
fn analyst28_context_preview_admits_only_the_derived_visible_schema_growth() {
    fn choose(f: &Fixture, input: InvestigationInput) -> InvestigationInput {
        let prompt: Value = serde_json::from_str(&input.payload.prompt).unwrap();
        let metadata: ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone()).unwrap();
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: metadata.request_hash.clone(),
            obligations_hash: metadata.obligations_hash.clone(),
            obligations: f.request.obligations.clone(),
        };
        let choices = acquisition_choices::AcquisitionChoices::derive(
            &contract,
            &metadata,
            "context-preview-task",
            &input.ledger,
            &[],
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
        input
            .with_acquisition_choices(std::sync::Arc::new(choices))
            .unwrap()
    }
    let kind = ContextPreviewKind::Query;
    for role in [
        SpecialistId::DomainAnalystV27,
        SpecialistId::DomainAnalystV28,
    ] {
        let fixture = Fixture::new(role, "{}");
        let usage = InvestigationUsage::default();
        let base = fixture.build(
            role,
            fixture.ledger.clone(),
            None,
            &[],
            &[],
            &usage,
            &fixture.metadata,
            None,
            0,
            None,
        );
        let base = choose(&fixture, base);
        let (candidate, raw, admitted) = fixture.candidate(role, kind, &base, &usage, &[], &[]);
        let candidate = choose(&fixture, candidate);
        let proof = certify(&base, &candidate, kind, &raw, &admitted).unwrap();
        let profile = provider().bounded_profile().unwrap();
        let limits = CompletionLimits::default();
        assert_eq!(
            proof.measurements().continuation,
            candidate
                .measure_v9_reserve_with_policy(
                    &profile,
                    &limits,
                    "step:preview",
                    &INVESTIGATION_V2_POLICY
                )
                .unwrap()
        );
        let shown = |input: &InvestigationInput| {
            serde_json::from_str::<Value>(&input.payload.prompt).unwrap()["turn_progress"]
                .get("response_schema")
                .cloned()
        };
        if role == SpecialistId::DomainAnalystV27 {
            assert!(shown(&candidate).is_none());
            continue;
        }
        let transport = candidate.payload.response_schema.clone().unwrap();
        assert_eq!(
            serde_json::to_string(&shown(&candidate).unwrap()).unwrap(),
            transport
        );
        assert_eq!(
            serde_json::to_string(&shown(&base).unwrap()).unwrap(),
            base.payload.response_schema.clone().unwrap()
        );
        // A candidate showing any other schema is not a certifiable preview.
        let mut forged: Value = serde_json::from_str(&candidate.payload.prompt).unwrap();
        forged["turn_progress"]["response_schema"] =
            serde_json::from_str(response_schema::FINISH).unwrap();
        let mut tampered = candidate.clone();
        tampered.payload.prompt = serde_json::to_string(&forged).unwrap();
        assert!(certify(&base, &tampered, kind, &raw, &admitted).is_err());
        let firewall = EgressFirewall::new(EgressPolicy::default());
        let prepared = firewall
            .prepare_bounded(
                &provider(),
                &candidate.action("step:preview".into()).unwrap(),
                &limits,
            )
            .unwrap();
        candidate
            .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
            .unwrap()
            .unwrap();
    }
}
