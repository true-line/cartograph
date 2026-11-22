use super::*;
use context_hub::{
    ContextSnapshot,
    coverage::{CoverageQualification, INVENTORY_VERSION, ScopedRepo},
};
use core_graph::coverage::{CoverageRecord, StoredCoverage};
use core_prov::content_hash;
use serde_json::json;

fn initial() -> InvestigationInputLedger {
    let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
    let record = CoverageRecord {
        repo_key: "acme/api".into(),
        registered_source_id: Some("src:api".into()),
        ingest_id: "private-ingest-basis".into(),
        config_hash: "private-plugin-basis".into(),
        inventory_json:
            json!({"inventory":INVENTORY_VERSION,"repo":"acme/api", "commit":"a".repeat(40),
            "layer_hints":["application"], "producers":[{"state":"not_selected"}],
            "unsupported":[], "limited":true})
            .to_string(),
    };
    let stored = StoredCoverage {
        inventory_hash: record.inventory_hash(),
        record,
    };
    let qualification = CoverageQualification::new(
        &[
            ScopedRepo {
                repo_key: "acme/api".into(),
                registered_source_id: Some("src:api".into()),
                config_hash: "private-plugin-basis".into(),
                layer_hints: None,
            },
            ScopedRepo {
                repo_key: "acme/unknown".into(),
                registered_source_id: None,
                config_hash: "plugins:unknown".into(),
                layer_hints: None,
            },
        ],
        &[stored],
    )
    .unwrap();
    InvestigationInputLedger {
        coverage_reads: vec![],
        artifact_reads: vec![],
        schema_version: 2,
        coverage: Some(qualification.durable_proof(snapshot.id()).unwrap()),
        graph_snapshot_id: snapshot.id().into(),
        scope_snapshot_id: snapshot.id().into(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    }
}
fn basis(
    ledger: &InvestigationInputLedger,
    previous: Option<InvestigationInputLedger>,
) -> InvestigationTurnBasis {
    InvestigationTurnBasis {
        input_ledger_hash: ledger.fingerprint().unwrap(),
        completed_tool: previous.as_ref().map(|_| InvestigationTool::QueryContext),
        previous_ledger: previous,
    }
}
fn prepare(
    ledger: InvestigationInputLedger,
    pages: &[String],
    basis: &InvestigationTurnBasis,
    targets: Option<&[InvestigationInspectionTarget]>,
) -> Result<InvestigationInput, InvestigationError> {
    InvestigationInput::prepare_v6(
        "Explain recovered behavior",
        SpecialistId::DomainAnalystV6,
        ledger,
        pages,
        &[],
        &[],
        &InvestigationUsage::default(),
        basis,
        targets,
        &[],
    )
}
fn prompt(input: &InvestigationInput) -> Value {
    serde_json::from_str(&input.action("inv:qualified".into()).unwrap().payload.prompt).unwrap()
}

// AC-0239: every invocation includes the full public block, never the host basis.
#[test]
fn v6_input_keeps_mandatory_public_qualification_initial_and_after_query() {
    let initial = initial();
    let first = prompt(&prepare(initial.clone(), &[], &basis(&initial, None), None).unwrap());
    assert_eq!(first["schema_version"], 5);
    assert_eq!(
        first["coverage_qualification"],
        serde_json::to_value(&initial.coverage.as_ref().unwrap().qualification).unwrap()
    );
    assert_eq!(
        first["context_id"],
        initial.coverage.as_ref().unwrap().context_id
    );
    assert_eq!(
        first["coverage_qualification"]["repos"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(first["coverage_qualification"]["exhaustive"], false);
    assert_eq!(
        first["coverage_qualification"]["repos"][0]["counts"]["not_selected"],
        1
    );
    assert!(first.get("inspection_requirements").is_none());
    assert!(!first.to_string().contains("private-ingest-basis"));
    assert!(!first.to_string().contains("private-plugin-basis"));
    let mut current = initial.clone();
    current.revision = 1;
    let raw = json!({"context":{"context_id":current.coverage.as_ref().unwrap().context_id,
        "coverage":current.coverage.as_ref().unwrap().qualification,
        "response":{"snapshot_id":current.scope_snapshot_id,"view":"recovered_context",
        "total_selected":0,"facts":[],"next_cursor":null}},"evidence_options":[]})
    .to_string();
    current.queries.push(InvestigationQueryManifest {
        query: InvestigationQuery {
            scope: InvestigationScope::All,
            kind: None,
            labels: vec![],
            max_facts: 12,
            max_bytes: 16384,
            cursor: None,
        },
        response_hash: content_hash(raw.as_bytes()),
        response_bytes: raw.len(),
        returned_facts: 0,
        total_selected: 0,
        has_more: false,
    });
    for field in ["legacy", "context_id", "coverage"] {
        let mut changed: Value = serde_json::from_str(&raw).unwrap();
        match field {
            "legacy" => changed["context"] = changed["context"]["response"].clone(),
            "context_id" => changed["context"]["context_id"] = "context-v2:wrong".into(),
            _ => changed["context"]["coverage"]["exhaustive"] = true.into(),
        }
        let changed = changed.to_string();
        let mut wrong = current.clone();
        wrong.queries[0].response_hash = content_hash(changed.as_bytes());
        wrong.queries[0].response_bytes = changed.len();
        assert!(
            prepare(
                wrong.clone(),
                &[changed],
                &basis(&wrong, Some(initial.clone())),
                None
            )
            .is_err()
        );
    }
    let next = prompt(
        &prepare(
            current.clone(),
            &[raw],
            &basis(&current, Some(initial)),
            None,
        )
        .unwrap(),
    );
    assert_eq!(
        next["coverage_qualification"],
        first["coverage_qualification"]
    );
    assert_eq!(next["context_id"], first["context_id"]);
    assert_eq!(next["turn_progress"]["phase"], "after_query");
    assert_eq!(
        next["turn_progress"]["raw_query_budget"]["consumed_bytes"],
        current.queries[0].response_bytes
    );
}

#[test]
fn v6_input_rejects_missing_mutated_proof_snapshot_and_legacy_paths() {
    let fixture = initial();
    let turn = basis(&fixture, None);
    for mut wrong in [
        fixture.clone(),
        fixture.clone(),
        fixture.clone(),
        fixture.clone(),
    ]
    .into_iter()
    .enumerate()
    {
        match wrong.0 {
            0 => wrong.1.coverage = None,
            1 => wrong.1.coverage.as_mut().unwrap().context_id.push('0'),
            2 => wrong.1.scope_snapshot_id = format!("context-v1:{}", "f".repeat(64)),
            _ => wrong.1.coverage.as_mut().unwrap().qualification.exhaustive = true,
        }
        assert!(prepare(wrong.1, &[], &turn, None).is_err());
    }
    assert!(prepare(fixture.clone(), &[], &turn, Some(&[])).is_err());
    let mut legacy = fixture.clone();
    legacy.schema_version = 1;
    legacy.coverage = None;
    assert!(prepare(legacy.clone(), &[], &basis(&legacy, None), None).is_err());
    for ledger in [fixture, legacy] {
        for specialist in [
            SpecialistId::DomainAnalystV6,
            SpecialistId::EvidenceAuditorV6,
        ] {
            assert!(
                InvestigationInput::prepare(
                    "Question",
                    specialist,
                    ledger.clone(),
                    &[],
                    &[],
                    &[],
                    &InvestigationUsage::default()
                )
                .is_err()
            );
            assert!(
                InvestigationInput::prepare_v4(
                    "Question",
                    specialist,
                    ledger.clone(),
                    &[],
                    &[],
                    &[],
                    &InvestigationUsage::default(),
                    &turn
                )
                .is_err()
            );
            assert!(
                InvestigationInput::prepare_v5(
                    "Question",
                    specialist,
                    ledger.clone(),
                    &[],
                    &[],
                    &[],
                    &InvestigationUsage::default(),
                    &turn,
                    &[]
                )
                .is_err()
            );
        }
    }
    for specialist in [
        SpecialistId::DomainAnalyst,
        SpecialistId::DomainAnalystV3,
        SpecialistId::DomainAnalystV4,
        SpecialistId::DomainAnalystV5,
    ] {
        assert!(
            InvestigationInput::prepare_base(
                "Question",
                specialist,
                initial(),
                &[],
                &[],
                &[],
                &InvestigationUsage::default()
            )
            .is_err()
        );
    }
}

#[test]
fn v6_input_specialist_protocol_is_explicit_and_context_qualified() {
    for specialist in [
        SpecialistId::DomainAnalystV6,
        SpecialistId::EvidenceAuditorV6,
    ] {
        let definition = specialist.definition().unwrap();
        assert_eq!(definition.version, 6);
        assert!(
            definition
                .operations
                .iter()
                .any(|op| op == "read_coverage_inventory")
        );
        assert!(specialist.uses_compact_disclosure() && specialist.uses_turn_progress());
        assert!(!specialist.uses_inspection_requirements());
        let system = specialist.prompt();
        assert!(!system.contains(r#"{"type":"query_context","query""#));
        assert!(system.contains(r#"{"type":"query_context","context_id""#));
        assert!(system.contains(r#"{"type":"read_coverage_inventory","context_id""#));
    }
}

// Frozen from the pre-@6 definitions, independently compared byte-for-byte
// against that saved implementation before recording these identity fixtures.
#[test]
fn v6_input_legacy_fingerprints_are_frozen() {
    for (id, expected) in [
        (
            "domain-analyst@1",
            "ab952c9fe055e1fde7aa5ca3f1d7f60851b402021460fdbd9011a765e20967c3",
        ),
        (
            "evidence-auditor@1",
            "b98abf4564bbccbf31e159fa3ecc759d8049f7f00a3d7cec998c9f5b0793d595",
        ),
        (
            "domain-analyst@2",
            "9eb83715f67f8b022ec5a144de89c4a4a86e0c3ee86ab71e545677e0fe3e2b37",
        ),
        (
            "evidence-auditor@2",
            "11f2d6ddbf7792a788a645f6f2e3827d8f0ff0ca4ff8d83356ea17e50216b1d0",
        ),
        (
            "domain-analyst@3",
            "6525d4d18f258b3bbb12963e5e1fc5b26fbedb8bb30d25590a67bb9a539bd6e6",
        ),
        (
            "evidence-auditor@3",
            "2c8cd70e4adae125869d7be57a595ef5524eada4c126e03e52ef201e95be0d0f",
        ),
        (
            "domain-analyst@4",
            "503b9b28dd1b98cd660b17cdbd74016574cf445a33643b20ec71617956f17077",
        ),
        (
            "evidence-auditor@4",
            "97086e3b88035057e6954dca878ea089428e1faf613dd5a0dcb8a726bf36c804",
        ),
        (
            "domain-analyst@5",
            "ae67454605a84cc6af1cfd69b1ca7ccd8944ba3df46e02329583bc98412c3836",
        ),
        (
            "evidence-auditor@5",
            "9fd4bb01d104d98b848796a172e5001da8ecb90732768cc1473f6b72b19dfe19",
        ),
    ] {
        let specialist: SpecialistId = serde_json::from_value(json!(id)).unwrap();
        assert_eq!(
            specialist.definition().unwrap().prompt_fingerprint,
            expected
        );
    }
}

// AC-0239: supplied targets remain an exact nonempty selection, never inherited.
#[test]
fn v6_input_accepts_optional_exact_targets_and_rejects_other_snapshot() {
    let ledger = initial();
    let mut target: InvestigationInspectionTarget=serde_json::from_value(json!({
        "schema_version":1,"target_id":"","graph_snapshot_id":ledger.graph_snapshot_id,
        "fact":{"kind":"node","id":"node:checkout"},"fact_digest":format!("node-v1:{}","a".repeat(64)),
        "registered_source_id":format!("src_{}","d".repeat(32)),"receipt_id":format!("ts-primary-v2:{}","c".repeat(64)),
        "receipt_inventory_index":3,"role":"provenance","index":0,
        "source":{"repo":"acme/api","path":"guard.ts","byte_start":0,"byte_end":12,"commit_sha":"a".repeat(40)},
        "captured":{"file":{"source_id":format!("src_{}","d".repeat(32)),"capture_id":format!("capture-v1:{}","e".repeat(64)),
        "path":"guard.ts","digest":content_hash(b"private text"),"byte_len":12},"byte_start":0,"byte_end":12}
    })).unwrap();
    target.target_id = target.content_id().unwrap();
    target.validate().unwrap();
    let input = prepare(
        ledger.clone(),
        &[],
        &basis(&ledger, None),
        Some(&[target.clone(), target.clone()]),
    )
    .unwrap();
    let supplied = prompt(&input);
    assert_eq!(supplied["inspection_requirements"], json!([target]));
    assert_eq!(
        supplied["inspection_requirements_hash"],
        inspection_requirements_hash(std::slice::from_ref(&target)).unwrap()
    );
    assert_eq!(
        supplied["inspection_progress"]["requirements"][0]["acquired"],
        false
    );
    assert!(supplied.get("coverage_qualification").is_some());
    target.graph_snapshot_id = format!("context-v1:{}", "f".repeat(64));
    target.target_id = target.content_id().unwrap();
    assert!(prepare(ledger.clone(), &[], &basis(&ledger, None), Some(&[target])).is_err());
}

// AC-0239: mandatory qualification is refused whole if it cannot be supplied.
#[test]
fn v6_input_never_truncates_oversized_qualification() {
    let mut ledger = initial();
    let repos = (0..600)
        .map(|i| ScopedRepo {
            repo_key: format!("repo/{i:04}/{}", "x".repeat(200)),
            registered_source_id: None,
            config_hash: "plugins:unknown".into(),
            layer_hints: None,
        })
        .collect::<Vec<_>>();
    let qualification = CoverageQualification::new(&repos, &[]).unwrap();
    ledger.coverage = Some(
        qualification
            .durable_proof(&ledger.scope_snapshot_id)
            .unwrap(),
    );
    ledger.coverage.as_ref().unwrap().validate().unwrap();
    let turn = InvestigationTurnBasis {
        input_ledger_hash: "unused-before-admission".into(),
        previous_ledger: None,
        completed_tool: None,
    };
    assert!(matches!(
        prepare(ledger, &[], &turn, None),
        Err(InvestigationError::LimitExceeded)
    ));
}

// AC-0239: real firewall preparation must preserve the entire mandatory block.
// This performs local preparation only; no provider dispatch occurs.
#[test]
fn v6_input_seals_actual_firewall_payload_and_rejects_coverage_mutation() {
    use llm::bounded::CompletionLimits;
    use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
    use std::time::Duration;
    let ledger = initial();
    let input = prepare(ledger.clone(), &[], &basis(&ledger, None), None).unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let original = input.action("inv:qualified".into()).unwrap();
    let prepared = firewall
        .prepare_bounded(&provider, &original, &CompletionLimits::default())
        .unwrap();
    let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
    assert_eq!(disclosure.schema_version, 3);
    let attestation = disclosure.coverage.as_ref().unwrap();
    assert_eq!(
        attestation.context_id,
        ledger.coverage.as_ref().unwrap().context_id
    );
    assert_eq!(
        attestation.coverage_id,
        ledger.coverage.as_ref().unwrap().qualification.coverage_id
    );
    disclosure.validate(&ledger).unwrap();
    for change in [
        "drop_coverage",
        "null_coverage",
        "change_coverage",
        "drop_context",
        "change_context",
    ] {
        let mut action = original.clone();
        let mut fields: Value = serde_json::from_str(&action.payload.prompt).unwrap();
        match change {
            "drop_coverage" => {
                fields
                    .as_object_mut()
                    .unwrap()
                    .remove("coverage_qualification");
            }
            "null_coverage" => fields["coverage_qualification"] = Value::Null,
            "change_coverage" => fields["coverage_qualification"]["exhaustive"] = true.into(),
            "drop_context" => {
                fields.as_object_mut().unwrap().remove("context_id");
            }
            _ => fields["context_id"] = "context-v2:wrong".into(),
        }
        action.payload.prompt = fields.to_string();
        let changed = firewall
            .prepare_bounded(&provider, &action, &CompletionLimits::default())
            .unwrap();
        // Check mandatory admission independently of action identity comparison.
        assert!(
            crate::investigation::sealing::seal_with_policy(
                &ledger,
                &action.payload,
                &changed,
                &INVESTIGATION_V1_POLICY,
                None
            )
            .is_err()
        );
        // A changed post-firewall representation is refused independently too.
        assert!(
            crate::investigation::sealing::seal_with_policy(
                &ledger,
                &original.payload,
                &changed,
                &INVESTIGATION_V1_POLICY,
                None
            )
            .is_err()
        );
    }
    let mut secret_ledger = initial();
    let qualification = CoverageQualification::new(
        &[ScopedRepo {
            repo_key: "acme/password=supersecret123".into(),
            registered_source_id: None,
            config_hash: "unknown".into(),
            layer_hints: None,
        }],
        &[],
    )
    .unwrap();
    secret_ledger.coverage = Some(
        qualification
            .durable_proof(&secret_ledger.scope_snapshot_id)
            .unwrap(),
    );
    let input = prepare(
        secret_ledger.clone(),
        &[],
        &basis(&secret_ledger, None),
        None,
    )
    .unwrap();
    let action = input.action("inv:secret-repo".into()).unwrap();
    let prepared = firewall
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    let after: Value = serde_json::from_str(&prepared.preview().egress.payload.prompt).unwrap();
    assert_ne!(
        after["coverage_qualification"],
        prompt(&input)["coverage_qualification"]
    );
    assert!(input.seal_disclosure(&prepared).is_err());
}

#[test]
fn selected_policy_input_binds_opt_in_role_contract_and_limits() {
    // AC-0281: the selected policy is present in the actual bounded prompt.
    use super::super::obligations::{
        InventoryTarget, Obligation, ObligationEnvelope, StagedObligationRequest,
    };
    use super::super::staged_refusal::ProtectedCompletionMetadata;
    let policy = &super::super::policy::INVESTIGATION_V2_POLICY;
    let ledger = initial();
    let proof = ledger.coverage.as_ref().unwrap();
    let repo = &proof.qualification.repos[0];
    let mut obligation = Obligation::Inventory {
        obligation_id: String::new(),
        target: InventoryTarget {
            context_id: proof.context_id.clone(),
            repo: repo.repo.clone(),
            inventory_hash: repo.inventory_hash.clone().unwrap(),
        },
    };
    let id = obligation.content_id().unwrap();
    if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
        *obligation_id = id;
    }
    let request = StagedObligationRequest {
        schema_version: 5,
        request_nonce: "selected-policy".into(),
        specialist_id: "domain-analyst@8".into(),
        obligations: ObligationEnvelope {
            schema_version: 2,
            contract: "investigation-completion-obligations@2".into(),
            obligations: vec![obligation],
        },
        inspection_requirements: None,
        question: "Explain recovered behavior".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: policy.profile().into(),
        expected_graph_revision: None,
        expected_context_id: proof.context_id.clone(),
        conversation_id: None,
        parent_id: None,
    };
    let metadata = ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: request.intent_hash_with_policy(policy).unwrap(),
        obligations_hash: request.obligations.fingerprint_with_policy(policy).unwrap(),
        completed_attempt_ids: vec![],
        refusal_count: 0,
        last_refusal_id: None,
        feedback: None,
    };
    let turn = basis(&ledger, None);
    let input = InvestigationInput::prepare_v7_with_policy(
        &request.question,
        SpecialistId::DomainAnalystV8,
        ledger.clone(),
        &[],
        &[],
        &[],
        &InvestigationUsage::default(),
        &turn,
        &request,
        &metadata,
        &[],
        policy,
    )
    .unwrap();
    let prompt: Value = serde_json::from_str(&input.payload.prompt).unwrap();
    assert_eq!(
        prompt["limits"],
        serde_json::to_value(policy.limits()).unwrap()
    );
    assert_eq!(
        prompt["completion_contract"]["request_hash"],
        metadata.request_hash
    );
    assert_eq!(
        prompt["completion_contract"]["obligations"]["schema_version"],
        2
    );
    assert!(
        InvestigationInput::prepare_v7(
            &request.question,
            SpecialistId::DomainAnalystV8,
            ledger,
            &[],
            &[],
            &[],
            &InvestigationUsage::default(),
            &turn,
            &request,
            &metadata,
            &[]
        )
        .is_err()
    );
}

/// A schema-4 section-writer@1 input under the v1 policy (#1989).
fn section_writer_input(specialist: SpecialistId) -> InvestigationInput {
    use super::super::obligations::{
        InventoryTarget, Obligation, ObligationEnvelope, StagedObligationRequest,
    };
    use super::super::staged_refusal::ProtectedCompletionMetadata;
    let policy = &INVESTIGATION_V1_POLICY;
    let ledger = initial();
    let proof = ledger.coverage.as_ref().unwrap();
    let repo = &proof.qualification.repos[0];
    let mut obligation = Obligation::Inventory {
        obligation_id: String::new(),
        target: InventoryTarget {
            context_id: proof.context_id.clone(),
            repo: repo.repo.clone(),
            inventory_hash: repo.inventory_hash.clone().unwrap(),
        },
    };
    let id = obligation.content_id().unwrap();
    if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
        *obligation_id = id;
    }
    let request = StagedObligationRequest {
        schema_version: 4,
        request_nonce: "section-schema".into(),
        specialist_id: serde_json::to_value(specialist)
            .unwrap()
            .as_str()
            .unwrap()
            .into(),
        obligations: ObligationEnvelope {
            schema_version: 1,
            contract: "investigation-completion-obligations@1".into(),
            obligations: vec![obligation],
        },
        inspection_requirements: None,
        question: "Draft D1 section 1.1".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: policy.profile().into(),
        expected_graph_revision: None,
        expected_context_id: proof.context_id.clone(),
        conversation_id: None,
        parent_id: None,
    };
    let metadata = ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: request.intent_hash_with_policy(policy).unwrap(),
        obligations_hash: request.obligations.fingerprint_with_policy(policy).unwrap(),
        completed_attempt_ids: vec![],
        refusal_count: 0,
        last_refusal_id: None,
        feedback: None,
    };
    let turn = basis(&ledger, None);
    InvestigationInput::prepare_v7_with_policy(
        &request.question,
        specialist,
        ledger,
        &[],
        &[],
        &[],
        &InvestigationUsage::default(),
        &turn,
        &request,
        &metadata,
        &[],
        policy,
    )
    .unwrap()
}

// AC-0861: a section draft request carries a transport schema that admits
// exactly the actions section-writer@1 offers, holds a provider to one bare
// JSON object, travels on every bounded wire and seals unchanged; other
// schema-4 roles still send none.
#[test]
fn section_writer_requests_carry_their_action_schema() {
    let input = section_writer_input(SpecialistId::SectionWriterV1);
    let raw = input
        .payload
        .response_schema
        .clone()
        .expect("section schema");
    let schema: Value = serde_json::from_str(&raw).unwrap();
    let actions: Vec<_> = schema["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|action| {
            action["properties"]["type"]["const"]
                .as_str()
                .or_else(|| action["$ref"].as_str())
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        actions,
        [
            "query_context",
            "read_evidence",
            "read_coverage_inventory",
            "#/$defs/supported_wrapper",
            "#/$defs/insufficient_wrapper",
        ]
    );
    assert_eq!(
        schema["$defs"]["finding"]["properties"]["claim_kind"]["enum"],
        json!([
            "implemented_behavior",
            "documented_intent",
            "inferred_interpretation"
        ])
    );
    assert!(llm::bounded::anthropic_wire_schema(&raw).is_some());
    assert!(llm::bounded::openai_cloud_wire_schema(&raw).is_some());
    let provider = llm::OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let prepared = llm::EgressFirewall::new(llm::EgressPolicy::default())
        .prepare_bounded(
            &provider,
            &input.action("task:draft".into()).unwrap(),
            &llm::bounded::CompletionLimits::default(),
        )
        .unwrap();
    assert_eq!(
        prepared.preview().egress.payload.response_schema.as_deref(),
        Some(raw.as_str())
    );
    input.seal_disclosure(&prepared).unwrap();
    assert!(
        section_writer_input(SpecialistId::DomainAnalystV7)
            .payload
            .response_schema
            .is_none()
    );
}

#[path = "input_capacity_tests.rs"]
mod capacity_tests;
