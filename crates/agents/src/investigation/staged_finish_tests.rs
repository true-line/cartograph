use super::super::{staged_attempt::*, staged_finish::*, staged_refusal::*};
use super::*;
use llm::bounded::CompletionLimits;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
use std::time::Duration;

struct PreparedFixture {
    request: StagedObligationRequest,
    input: InvestigationInput,
    disclosure: InvestigationDisclosure,
}
fn identified(mut item: Obligation) -> Obligation {
    let id = item.content_id().unwrap();
    match &mut item {
        Obligation::Source { obligation_id, .. } | Obligation::Inventory { obligation_id, .. } => {
            *obligation_id = id
        }
    };
    item
}
fn fixture(source: bool, inventory: bool, secret: bool) -> PreparedFixture {
    fixture_version(source, inventory, secret, false)
}
fn fixture_version(source: bool, inventory: bool, secret: bool, v7: bool) -> PreparedFixture {
    fixture_policy(
        source,
        inventory,
        secret,
        v7.then_some(&INVESTIGATION_V1_POLICY),
    )
}
/// `completion` selects the v7 completion input under that exact policy.
fn fixture_policy(
    source: bool,
    inventory: bool,
    secret: bool,
    completion: Option<&InvestigationPolicy>,
) -> PreparedFixture {
    let legacy = completion.is_none_or(|policy| policy.profile() == "investigation-v1");
    let (target, mut ledger, _) = super::inspection_tests::fixture();
    let repo = target.source.repo.clone();
    let inventory_json=json!({"inventory":context_hub::coverage::INVENTORY_VERSION,"repo":repo,"commit":"pinned-revision","layer_hints":[],"producers":[{"state":"not_selected","stopping_reason":if secret {"limit password=syntheticsecret123 bounded "}else{"optional input not selected"}}],"unsupported":[],"limited":true}).to_string();
    let record = core_graph::coverage::CoverageRecord {
        repo_key: repo.clone(),
        registered_source_id: Some(target.registered_source_id.clone()),
        ingest_id: "fixture-ingest".into(),
        config_hash: "fixture-config".into(),
        inventory_json,
    };
    let stored = core_graph::coverage::StoredCoverage {
        inventory_hash: record.inventory_hash(),
        record,
    };
    let qualification = context_hub::coverage::CoverageQualification::new(
        &[context_hub::coverage::ScopedRepo {
            repo_key: repo.clone(),
            registered_source_id: Some(target.registered_source_id.clone()),
            config_hash: "fixture-config".into(),
            layer_hints: None,
        }],
        std::slice::from_ref(&stored),
    )
    .unwrap();
    ledger.schema_version = 2;
    ledger.coverage = Some(
        qualification
            .durable_proof(&ledger.scope_snapshot_id)
            .unwrap(),
    );
    if !source {
        ledger.citations.pop();
    }
    let mut page: serde_json::Value = serde_json::from_str(&super::projection_tests::page(
        &ledger,
        "Recovered behavior",
    ))
    .unwrap();
    page["context"] = json!({"context_id":ledger.coverage.as_ref().unwrap().context_id,"coverage":ledger.coverage.as_ref().unwrap().qualification,"response":page["context"].clone()});
    let pages = vec![page.to_string()];
    super::projection_tests::bind_pages(&mut ledger, &pages);
    let mut previous = ledger.clone();
    let mut details = Vec::new();
    let tool = if inventory {
        let archive = InvestigationCoverageArchive::new(
            ledger.coverage.clone().unwrap(),
            vec![stored.clone()],
        )
        .unwrap();
        let read = InvestigationCoverageRead {
            context_id: archive.proof().context_id.clone(),
            repo: repo.clone(),
            inventory_hash: stored.inventory_hash.clone(),
            max_bytes: MAX_QUERY_BYTES,
        };
        let raw = serde_json::to_string(&archive.read(&read).unwrap()).unwrap();
        ledger
            .coverage_reads
            .push(InvestigationCoverageReadManifest {
                request: read,
                response_hash: content_hash(raw.as_bytes()),
                response_bytes: raw.len(),
            });
        details.push(raw);
        InvestigationTool::ReadCoverageInventory
    } else if source {
        previous.citations.pop();
        InvestigationTool::ReadEvidence
    } else {
        previous.queries.clear();
        previous.citations.clear();
        previous.selected_facts.clear();
        previous.receipt_references.clear();
        InvestigationTool::QueryContext
    };
    previous.revision = 0;
    ledger.revision = 1;
    let evidence = if source {
        vec![crate::AgentEvidence {
            id: ledger.citations[1].citation_id.clone(),
            source: target.source.clone(),
            text: "private text".into(),
        }]
    } else {
        vec![]
    };
    let mut input = InvestigationInput::prepare_v6(
        "Inspect selected evidence",
        SpecialistId::DomainAnalystV6,
        ledger.clone(),
        &pages,
        &evidence,
        &[],
        &InvestigationUsage::default(),
        &InvestigationTurnBasis {
            input_ledger_hash: ledger.fingerprint().unwrap(),
            previous_ledger: Some(previous.clone()),
            completed_tool: Some(tool),
        },
        Some(std::slice::from_ref(&target)),
        &details,
    )
    .unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let prepared = EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(
            &provider,
            &input.action("task:finish".into()).unwrap(),
            &CompletionLimits::default(),
        )
        .unwrap();
    let mut disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
    let policy = completion.unwrap_or(&INVESTIGATION_V1_POLICY);
    let request = StagedObligationRequest {
        schema_version: if legacy { 4 } else { 5 },
        request_nonce: "staged-nonce".into(),
        specialist_id: if legacy {
            "domain-analyst@7"
        } else {
            "domain-analyst@8"
        }
        .into(),
        inspection_requirements: None,
        question: "Inspect selected evidence".into(),
        scope: InvestigationScope::All,
        provider_mode: InvestigationProviderMode::Local,
        limit_profile: policy.profile().into(),
        expected_graph_revision: Some(target.graph_snapshot_id.clone()),
        expected_context_id: ledger.coverage.as_ref().unwrap().context_id.clone(),
        conversation_id: None,
        parent_id: None,
        obligations: ObligationEnvelope {
            schema_version: if legacy { 1 } else { 2 },
            contract: if legacy {
                "investigation-completion-obligations@1"
            } else {
                "investigation-completion-obligations@2"
            }
            .into(),
            obligations: vec![
                identified(Obligation::Source {
                    obligation_id: String::new(),
                    target: Box::new(target),
                }),
                identified(Obligation::Inventory {
                    obligation_id: String::new(),
                    target: InventoryTarget {
                        context_id: ledger.coverage.as_ref().unwrap().context_id.clone(),
                        repo,
                        inventory_hash: stored.inventory_hash,
                    },
                }),
            ],
        }
        .normalized_with_policy(policy)
        .unwrap(),
    };
    if completion.is_some() {
        let metadata = ProtectedCompletionMetadata {
            schema_version: 1,
            request_hash: request.intent_hash_with_policy(policy).unwrap(),
            obligations_hash: request.obligations.fingerprint_with_policy(policy).unwrap(),
            completed_attempt_ids: vec![],
            refusal_count: 0,
            last_refusal_id: None,
            feedback: None,
        };
        input = InvestigationInput::prepare_v7_with_policy(
            &request.question,
            if legacy {
                SpecialistId::DomainAnalystV7
            } else {
                SpecialistId::DomainAnalystV8
            },
            ledger.clone(),
            &pages,
            &evidence,
            &[],
            &InvestigationUsage::default(),
            &InvestigationTurnBasis {
                input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
                previous_ledger: Some(previous),
                completed_tool: Some(tool),
            },
            &request,
            &metadata,
            &details,
            policy,
        )
        .unwrap();
        let prepared = EgressFirewall::new(EgressPolicy::default())
            .prepare_bounded(
                &provider,
                &input.action("task:finish".into()).unwrap(),
                &CompletionLimits::default(),
            )
            .unwrap();
        disclosure = input
            .seal_disclosure_with_policy(&prepared, policy)
            .unwrap()
            .unwrap();
    }
    PreparedFixture {
        request,
        input,
        disclosure,
    }
}
fn finish(insufficient: bool) -> StagedFinish {
    let action = if insufficient {
        InvestigationAction::Finish {
            findings: vec![],
            knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
            limitations: vec!["Selected evidence is not sufficient.".into()],
        }
    } else {
        action()
    };
    StagedFinish {
        schema_version: 1,
        outcome: if insufficient {
            CompletionOutcome::InsufficientEvidence
        } else {
            CompletionOutcome::Supported
        },
        action,
    }
}
fn attempt(fixture: &PreparedFixture, item: &Obligation, outcome: AttemptOutcome) -> AttemptRecord {
    let read = match item {
        Obligation::Source { target, .. } => ReadIntent::Source {
            fact: target.fact.clone(),
            role: target.role,
            index: target.index,
        },
        Obligation::Inventory { target, .. } => ReadIntent::Inventory {
            context_id: target.context_id.clone(),
            repo: target.repo.clone(),
            inventory_hash: target.inventory_hash.clone(),
            max_bytes: MAX_QUERY_BYTES,
        },
    };
    let ledger = fixture.input.ledger();
    let mut record = AttemptRecord {
        schema_version: 1,
        attempt_id: String::new(),
        task_id: "task".into(),
        request_hash: fixture.request.intent_hash().unwrap(),
        obligations_hash: fixture.request.obligations.fingerprint().unwrap(),
        obligation_id: item.key().1.into(),
        step_id: "read-step".into(),
        read,
        input_ledger_hash: ledger.fingerprint().unwrap(),
        input_revision: ledger.revision,
        outcome,
    };
    record.attempt_id = record.content_id().unwrap();
    record
}
fn decide(
    fixture: &PreparedFixture,
    finish: &StagedFinish,
    records: &[AttemptRecord],
) -> Result<FinishDecision, InvestigationError> {
    let evidence = records
        .iter()
        .map(|record| AttemptEvidence {
            record,
            ledger: fixture.input.ledger(),
        })
        .collect::<Vec<_>>();
    admit_finish(
        &fixture.request,
        "task",
        "finish-step",
        &fixture.input,
        &fixture.disclosure,
        finish,
        &evidence,
        None,
        "now".into(),
    )
}
// AC-0265: actual production input preparation/firewall/finish validators gate decisions.
#[test]
fn staged_finish_requires_exact_mixed_finishing_disclosure_and_security_first() {
    let complete = fixture(true, true, false);
    assert!(matches!(
        decide(&complete, &finish(false), &[]),
        Ok(FinishDecision::Accepted(_))
    ));
    let mut forged = finish(false);
    if let InvestigationAction::Finish { findings, .. } = &mut forged.action {
        findings[0].citation_ids = vec!["invented".into()];
    }
    assert!(matches!(
        decide(&complete, &forged, &[]),
        Err(InvestigationError::InvalidCitation)
    ));
    for (source, inventory) in [(true, false), (false, true), (false, false)] {
        let partial = fixture(source, inventory, false);
        assert!(matches!(
            decide(&partial, &finish(false), &[]),
            Ok(FinishDecision::Refused { .. })
        ));
    }
    let mut invalid = finish(true);
    invalid.outcome = CompletionOutcome::Supported;
    assert!(matches!(
        decide(&complete, &invalid, &[]),
        Err(InvestigationError::InvalidAction)
    ));
}
#[test]
fn staged_finish_insufficient_requires_every_completed_attempt_and_binds_identity() {
    let fixture = fixture(false, false, false);
    assert!(matches!(
        decide(&fixture, &finish(true), &[]),
        Ok(FinishDecision::Refused { .. })
    ));
    let records = fixture
        .request
        .obligations
        .obligations
        .iter()
        .map(|item| {
            attempt(
                &fixture,
                item,
                AttemptOutcome::Unavailable {
                    reason: match item {
                        Obligation::Source { .. } => UnavailableReason::PinnedTargetAbsent,
                        Obligation::Inventory { .. } => UnavailableReason::InventoryExceedsMaxBytes,
                    },
                },
            )
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        decide(&fixture, &finish(true), &records[..1]),
        Ok(FinishDecision::Refused { .. })
    ));
    assert!(matches!(
        decide(&fixture, &finish(true), &records),
        Ok(FinishDecision::Accepted(_))
    ));
    let mut forged = records.clone();
    forged[0].step_id.clear();
    forged[0].attempt_id = forged[0].content_id().unwrap();
    assert!(decide(&fixture, &finish(true), &forged).is_err());
    forged = records;
    forged[0].obligation_id = "0".repeat(64);
    forged[0].attempt_id = forged[0].content_id().unwrap();
    assert!(decide(&fixture, &finish(true), &forged).is_err());
}
#[test]
fn staged_finish_inventory_redaction_and_omitted_source_do_not_discharge() {
    let mut omitted = fixture(true, true, false);
    omitted.input = omitted
        .input
        .select_sources(
            &std::collections::BTreeSet::new(),
            DisclosureOmissionReason::WorkingSetBudget,
        )
        .unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let prepared = EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(
            &provider,
            &omitted.input.action("task:omitted".into()).unwrap(),
            &CompletionLimits::default(),
        )
        .unwrap();
    omitted.disclosure = omitted.input.seal_disclosure(&prepared).unwrap().unwrap();
    let acquired = omitted
        .request
        .obligations
        .obligations
        .iter()
        .map(|item| {
            attempt(
                &omitted,
                item,
                match item {
                    Obligation::Source { .. } => AttemptOutcome::SourceAcquired {
                        citation_id: "evidence-1".into(),
                    },
                    Obligation::Inventory { .. } => {
                        AttemptOutcome::InventoryAcquired { read_index: 0 }
                    }
                },
            )
        })
        .collect::<Vec<_>>();
    assert!(
        matches!(decide(&omitted,&finish(false),&acquired),Ok(FinishDecision::Refused {feedback,..}) if feedback.unmet.iter().any(|entry|entry.reason==RefusalReason::AcquiredNotSupplied))
    );
    assert!(matches!(
        decide(&omitted, &finish(true), &acquired),
        Ok(FinishDecision::Accepted(_))
    ));

    let redacted = fixture(true, true, true);
    let records = redacted
        .request
        .obligations
        .obligations
        .iter()
        .map(|item| {
            attempt(
                &redacted,
                item,
                match item {
                    Obligation::Source { .. } => AttemptOutcome::SourceAcquired {
                        citation_id: "evidence-1".into(),
                    },
                    Obligation::Inventory { .. } => {
                        AttemptOutcome::InventoryAcquired { read_index: 0 }
                    }
                },
            )
        })
        .collect::<Vec<_>>();
    let decision = decide(&redacted, &finish(false), &records).unwrap();
    assert!(
        matches!(decision,FinishDecision::Refused {feedback,..} if feedback.unmet.iter().any(|entry|entry.reason==RefusalReason::Redacted))
    );
    assert!(matches!(
        decide(&redacted, &finish(true), &records),
        Ok(FinishDecision::Accepted(_))
    ));
}
#[test]
fn staged_finish_feedback_is_closed_bounded_and_protected_by_actual_firewall() {
    let feedback = RefusalFeedback {
        schema_version: 1,
        unmet: (0..12)
            .map(|index| FeedbackEntry {
                obligation_id: format!("{index:064x}"),
                kind: "inventory".into(),
                reason: RefusalReason::AcquiredNotSupplied,
            })
            .collect(),
    };
    assert!(feedback.bytes().unwrap().len() <= 2048);
    let metadata = ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: "a".repeat(64),
        obligations_hash: "b".repeat(64),
        completed_attempt_ids: vec![],
        refusal_count: 1,
        last_refusal_id: Some("c".repeat(64)),
        feedback: Some(feedback),
    };
    let mut action = super::sealing_tests::input(SpecialistId::DomainAnalystV3)
        .action("task:metadata".into())
        .unwrap();
    let mut prompt: serde_json::Value = serde_json::from_str(&action.payload.prompt).unwrap();
    prompt["completion_metadata"] = serde_json::to_value(&metadata).unwrap();
    action.payload.prompt = prompt.to_string();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let prepared = EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    metadata.seal(&action, &prepared).unwrap();
    let mut altered = metadata.clone();
    altered.refusal_count = 2;
    assert!(altered.seal(&action, &prepared).is_err());
    altered = metadata;
    altered.completed_attempt_ids.push("d".repeat(64));
    assert!(altered.seal(&action, &prepared).is_err());
}

#[test]
fn staged_refusal_cap_requires_terminal_and_cannot_prepare_next_input() {
    let fixture = fixture(false, false, false);
    let finish = finish(true);
    let FinishDecision::Refused {
        assessment,
        feedback,
    } = decide(&fixture, &finish, &[]).unwrap()
    else {
        panic!("expected refusal")
    };
    let ledger = fixture.input.ledger();
    let mut first = RefusalRecord {
        schema_version: 1,
        refusal_id: String::new(),
        task_id: "task".into(),
        request_hash: fixture.request.intent_hash().unwrap(),
        obligations_hash: fixture.request.obligations.fingerprint().unwrap(),
        step_id: "finish-step".into(),
        input_ledger_hash: ledger.fingerprint().unwrap(),
        disclosure_hash: fixture.disclosure.fingerprint(ledger).unwrap(),
        finish_hash: finish.fingerprint().unwrap(),
        outcome: finish.outcome,
        response_hash: content_hash(b"response"),
        previous_refusal: None,
        refusal_count: 1,
        assessment,
        feedback,
    };
    first.refusal_id = first.content_id().unwrap();
    assert_eq!(
        first
            .validate(&fixture.request, ledger, &fixture.disclosure, &finish, None)
            .unwrap(),
        RefusalDisposition::Continue
    );
    let mut second = first.clone();
    second.step_id = "second-step".into();
    second.refusal_count = 2;
    second.previous_refusal = Some(first.refusal_id.clone());
    second.refusal_id = second.content_id().unwrap();
    assert_eq!(
        second
            .validate(
                &fixture.request,
                ledger,
                &fixture.disclosure,
                &finish,
                Some(&first)
            )
            .unwrap(),
        RefusalDisposition::TerminalRequired
    );
    let mut third = second.clone();
    third.refusal_count = 3;
    third.previous_refusal = Some(second.refusal_id.clone());
    third.refusal_id = third.content_id().unwrap();
    assert!(
        third
            .validate(
                &fixture.request,
                ledger,
                &fixture.disclosure,
                &finish,
                Some(&second)
            )
            .is_err()
    );
    assert!(
        ProtectedCompletionMetadata {
            schema_version: 1,
            request_hash: first.request_hash,
            obligations_hash: first.obligations_hash,
            completed_attempt_ids: vec![],
            refusal_count: 2,
            last_refusal_id: Some(second.refusal_id),
            feedback: Some(second.feedback)
        }
        .validate()
        .is_err()
    );
}

// AC-0266: explicit V7 input6/disclosure4/result5, unchanged read protocol,
// and generation-discriminated transport with no legacy finish fallback.
#[test]
fn completion_v7_generations_bind_actual_firewall_and_reject_legacy_authority() {
    let fixture = fixture_version(true, true, false, true);
    let raw = serde_json::to_string(&fixture.request).unwrap();
    assert!(matches!(
        InvestigationStartRequest::decode(&raw).unwrap(),
        InvestigationStartRequest::Completion(_)
    ));
    assert!(serde_json::from_str::<InvestigationStartRequest>(&raw).is_ok());
    assert!(
        serde_json::from_value::<InvestigationStartRequest>(serde_json::from_str(&raw).unwrap())
            .is_ok()
    );
    let mut legacy = request();
    legacy.schema_version = 3;
    legacy.specialist_id = SpecialistId::DomainAnalystV7;
    legacy.expected_context_id = Some(fixture.request.expected_context_id.clone());
    assert!(legacy.validate().is_err());
    assert!(InvestigationStartRequest::decode(&serde_json::to_string(&legacy).unwrap()).is_err());
    // IPC shape decoding defers semantic generation/specialist validation to
    // start(), while the explicit raw-protocol decoder remains strict.
    assert!(matches!(
        serde_json::from_value::<InvestigationStartRequest>(serde_json::to_value(&legacy).unwrap()),
        Ok(InvestigationStartRequest::Legacy(_))
    ));
    let duplicate = raw.replacen(
        "\"schema_version\":4",
        "\"schema_version\":4,\"schema_version\":4",
        1,
    );
    assert!(InvestigationStartRequest::decode(&duplicate).is_err());
    assert!(serde_json::from_str::<InvestigationStartRequest>(&duplicate).is_err());
    let finish = finish(false);
    let finish_raw = serde_json::to_string(&finish).unwrap();
    assert!(matches!(
        CompletionInvestigationAction::decode(&finish_raw, &fixture.request.expected_context_id)
            .unwrap(),
        CompletionInvestigationAction::Finish(_)
    ));
    assert!(
        CompletionInvestigationAction::decode(
            &serde_json::to_string(&finish.action).unwrap(),
            &fixture.request.expected_context_id
        )
        .is_err()
    );
    let prompt: serde_json::Value = serde_json::from_str(
        &fixture
            .input
            .action("task:prompt".into())
            .unwrap()
            .payload
            .prompt,
    )
    .unwrap();
    assert_eq!(prompt["schema_version"], 6);
    assert_eq!(fixture.disclosure.schema_version, 4);
    assert!(
        fixture
            .disclosure
            .fingerprint(fixture.input.ledger())
            .unwrap()
            .starts_with("investigation-disclosure-v4:")
    );
    let result = fixture
        .input
        .admit_completion_finish(
            &fixture.request,
            "task:finish",
            "step:finish",
            &fixture.disclosure,
            &finish,
            &[],
            None,
            "2026-10-20T12:00:00Z".into(),
        )
        .unwrap()
        .unwrap_or_else(|_| panic!("exact source and inventory must satisfy supported"));
    assert_eq!(result.schema_version, 5);
    assert!(result.result_id.starts_with("investigation-result-v5:"));
    result
        .validate_completion(
            &fixture.request,
            "task:finish",
            "step:finish",
            fixture.input.ledger(),
            &fixture.disclosure,
            &[],
        )
        .unwrap();
    let actual = json!({"analyst":SpecialistId::DomainAnalystV7.definition().unwrap().prompt_fingerprint,"auditor":SpecialistId::EvidenceAuditorV7.definition().unwrap().prompt_fingerprint,"disclosure":fixture.disclosure.fingerprint(fixture.input.ledger()).unwrap(),"result":result.result_id,"request":fixture.request.intent_hash().unwrap()});
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/completion-v7-goldens.json")).unwrap();
    assert_eq!(actual, expected);
    let mut tampered = result.clone();
    tampered.completion.as_mut().unwrap().outcome = CompletionOutcome::InsufficientEvidence;
    assert!(
        tampered
            .validate_with_disclosure(fixture.input.ledger(), "step:finish", &fixture.disclosure)
            .is_err()
    );
    assert!(
        fixture
            .input
            .admit_result_for_step(
                "task:finish",
                "step:finish",
                Some(&fixture.disclosure),
                &finish.action,
                None,
                "2026-10-20T12:00:00Z".into()
            )
            .is_err()
    );
}

// AC-0266: a well-formed partial finish is recoverable, while forged protected
// contract/metadata and malformed findings stay hard errors before refusal.
#[test]
fn completion_v7_partial_and_metadata_tampering_are_distinct() {
    let fixture = fixture_version(true, false, false, true);
    let finish = finish(false);
    assert!(matches!(
        fixture
            .input
            .admit_completion_finish(
                &fixture.request,
                "task:finish",
                "step:finish",
                &fixture.disclosure,
                &finish,
                &[],
                None,
                "2026-10-20T12:00:00Z".into()
            )
            .unwrap(),
        Err(FinishDecision::Refused { .. })
    ));
    let mut forged = fixture.disclosure.clone();
    forged.completion_metadata_hash = Some("a".repeat(64));
    assert!(
        fixture
            .input
            .admit_completion_finish(
                &fixture.request,
                "task:finish",
                "step:finish",
                &forged,
                &finish,
                &[],
                None,
                "2026-10-20T12:00:00Z".into()
            )
            .is_err()
    );
    let mut wrong = fixture.request.clone();
    wrong.request_nonce = "another-nonce".into();
    assert!(
        fixture
            .input
            .admit_completion_finish(
                &wrong,
                "task:finish",
                "step:finish",
                &fixture.disclosure,
                &finish,
                &[],
                None,
                "2026-10-20T12:00:00Z".into()
            )
            .is_err()
    );
    let mut forged = finish.clone();
    if let InvestigationAction::Finish { findings, .. } = &mut forged.action {
        findings[0].citation_ids = vec!["not-supplied".into()];
    }
    assert!(
        fixture
            .input
            .admit_completion_finish(
                &fixture.request,
                "task:finish",
                "step:finish",
                &fixture.disclosure,
                &forged,
                &[],
                None,
                "2026-10-20T12:00:00Z".into()
            )
            .is_err()
    );
}

// AC-0266: policy-aware finish/refusal paths keep v1's literal bounds and
// identities; the saved policy, not a static ceiling, sets every boundary.
#[test]
fn staged_policy_paths_preserve_v1_bounds_and_identities() {
    let policy = &INVESTIGATION_V1_POLICY;
    assert_eq!(
        RefusalFeedback::max_bytes_with_policy(policy).unwrap(),
        2048
    );
    let entries = |count: usize| RefusalFeedback {
        schema_version: 1,
        unmet: (0..count)
            .map(|index| FeedbackEntry {
                obligation_id: format!("{index:064x}"),
                kind: "inventory".into(),
                reason: RefusalReason::AcquiredNotSupplied,
            })
            .collect(),
    };
    let widest = entries(policy.max_completion_obligations());
    assert_eq!(
        widest.bytes().unwrap(),
        widest.bytes_with_policy(policy).unwrap()
    );
    assert!(widest.bytes().unwrap().len() <= 2048);
    assert_eq!(
        widest.fingerprint().unwrap(),
        widest.fingerprint_with_policy(policy).unwrap()
    );
    let over = entries(policy.max_completion_obligations() + 1);
    assert_eq!(over.bytes(), Err(InvestigationError::InvalidInput));
    assert_eq!(
        over.bytes_with_policy(policy),
        Err(InvestigationError::InvalidInput)
    );

    let metadata = |ids: usize| ProtectedCompletionMetadata {
        schema_version: 1,
        request_hash: "a".repeat(64),
        obligations_hash: "b".repeat(64),
        completed_attempt_ids: (0..ids).map(|index| format!("{index:064x}")).collect(),
        refusal_count: 1,
        last_refusal_id: Some("c".repeat(64)),
        feedback: Some(widest.clone()),
    };
    let full = metadata(policy.max_completed_attempts());
    assert_eq!(
        full.fingerprint().unwrap(),
        full.fingerprint_with_policy(policy).unwrap()
    );
    let excess = metadata(policy.max_completed_attempts() + 1);
    assert_eq!(excess.validate(), Err(InvestigationError::InvalidInput));
    assert_eq!(
        excess.validate_with_policy(policy),
        Err(InvestigationError::InvalidInput)
    );

    for (source, inventory) in [(true, true), (true, false), (false, false)] {
        let fixture = fixture(source, inventory, false);
        let legacy = decide(&fixture, &finish(false), &[]);
        let ledger = fixture.input.ledger();
        let scoped = admit_finish_with_policy(
            &fixture.request,
            "task",
            "finish-step",
            &fixture.input,
            &fixture.disclosure,
            &finish(false),
            &[],
            None,
            "now".into(),
            policy,
        );
        match (legacy, scoped) {
            (Ok(FinishDecision::Accepted(a)), Ok(FinishDecision::Accepted(b))) => {
                assert_eq!(a.assessment.len(), b.assessment.len());
            }
            (
                Ok(FinishDecision::Refused { feedback: a, .. }),
                Ok(FinishDecision::Refused { feedback: b, .. }),
            ) => assert_eq!(a, b),
            _ => panic!("policy-aware finish diverged from v1"),
        }
        let request = fixture.request.normalized().unwrap();
        assert_eq!(
            derive_assessment_for_binding(
                &request.intent_hash().unwrap(),
                &request.obligations,
                "task",
                "finish-step",
                ledger,
                &fixture.disclosure,
                CompletionOutcome::Supported,
                &[],
            )
            .unwrap(),
            derive_assessment_for_binding_with_policy(
                &request.intent_hash().unwrap(),
                &request.obligations,
                "task",
                "finish-step",
                ledger,
                &fixture.disclosure,
                CompletionOutcome::Supported,
                &[],
                policy,
            )
            .unwrap()
        );
    }
}

#[test]
fn sealing_reconstructs_contract_sourced_targets_for_the_selected_policy() {
    // AC-0281: a non-v1 completion prompt carries its inspection targets once,
    // in the completion contract; sealing rebuilds and binds them.
    let policy = &INVESTIGATION_V2_POLICY;
    let fixture = fixture_policy(true, true, false, Some(policy));
    let prompt: serde_json::Value = serde_json::from_str(
        &fixture
            .input
            .action("task:finish".into())
            .unwrap()
            .payload
            .prompt,
    )
    .unwrap();
    assert_eq!(prompt["schema_version"], 7);
    assert!(prompt.get("inspection_requirements").is_none());
    assert_eq!(fixture.disclosure.schema_version, 4);
    assert!(fixture.disclosure.inspection_requirements_hash.is_some());
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let seal = |input: &InvestigationInput,
                change: &dyn Fn(&mut serde_json::Value),
                policy: &InvestigationPolicy,
                private: Option<&[InvestigationInspectionTarget]>| {
        let mut action = input.action("task:finish".into()).unwrap();
        let mut prompt: serde_json::Value = serde_json::from_str(&action.payload.prompt).unwrap();
        change(&mut prompt);
        action.payload.prompt = prompt.to_string();
        let prepared = EgressFirewall::new(EgressPolicy::default())
            .prepare_bounded(&provider, &action, &CompletionLimits::default())
            .unwrap();
        crate::investigation::sealing::seal_with_policy(
            input.ledger(),
            &action.payload,
            &prepared,
            policy,
            private,
        )
    };
    let private = Some(
        fixture
            .request
            .obligations
            .sources_with_policy(policy)
            .unwrap(),
    );
    let unchanged = |_: &mut serde_json::Value| {};
    assert_eq!(
        serde_json::to_value(seal(&fixture.input, &unchanged, policy, private.as_deref()).unwrap())
            .unwrap(),
        serde_json::to_value(&fixture.disclosure).unwrap()
    );
    let refused = |change: &dyn Fn(&mut serde_json::Value)| {
        seal(&fixture.input, change, policy, private.as_deref())
            == Err(InvestigationError::InvalidInput)
    };
    // A second descriptor copy, a schema-6 relabel, or a changed binding fails.
    assert!(refused(&|p| {
        p["inspection_requirements"] = serde_json::to_value(&private).unwrap();
    }));
    assert!(refused(&|p| p["schema_version"] = 6.into()));
    assert!(refused(&|p| {
        p["inspection_requirements_hash"] = "0".repeat(64).into();
    }));
    assert!(refused(&|p| {
        p["inspection_progress"]["requirements"][0]["target_id"] = "0".repeat(64).into();
    }));
    assert!(refused(&|p| {
        p["inspection_progress"]["requirements"]
            .as_array_mut()
            .unwrap()
            .clear();
    }));
    assert!(refused(&|p| {
        for item in p["completion_contract"]["obligations"]["obligations"]
            .as_array_mut()
            .unwrap()
        {
            if item
                .get("target")
                .is_some_and(|t| t.get("receipt_id").is_some())
            {
                item["target"]["receipt_inventory_index"] = 9.into();
            }
        }
    }));
    // The contract targets must also be the host's private admitted targets.
    assert_eq!(
        seal(&fixture.input, &unchanged, policy, None),
        Err(InvestigationError::InvalidInput)
    );
    // v1 never accepts schema 7, even for an otherwise valid v1 prompt.
    let legacy = fixture_version(true, true, false, true);
    let legacy_private = Some(legacy.request.obligations.sources().unwrap());
    assert_eq!(
        seal(
            &legacy.input,
            &|p| {
                p.as_object_mut().unwrap().remove("inspection_requirements");
                p["schema_version"] = 7.into();
            },
            &INVESTIGATION_V1_POLICY,
            legacy_private.as_deref(),
        ),
        Err(InvestigationError::InvalidInput)
    );
}

// AC-0327: real source/inventory binding validation, including forged identity
// refusal, exercises the pure component used by the host reservation.
#[test]
fn attempt_binding_workspace_covers_real_policy_validation() {
    for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
        let f = fixture_policy(true, true, false, Some(policy));
        let ledger = f.input.ledger();
        let bytes = serde_json::to_vec(ledger).unwrap().len();
        let bound = attempt_binding_validation_workspace(policy, bytes).unwrap();
        let envelope = &f.request.obligations;
        let request = f.request.intent_hash_with_policy(policy).unwrap();
        for item in &envelope.obligations {
            let (read, outcome) = match item {
                Obligation::Source { target, .. } => (
                    ReadIntent::Source {
                        fact: target.fact.clone(),
                        role: target.role,
                        index: target.index,
                    },
                    AttemptOutcome::SourceAcquired {
                        citation_id: "evidence-1".into(),
                    },
                ),
                Obligation::Inventory { target, .. } => (
                    ReadIntent::Inventory {
                        context_id: target.context_id.clone(),
                        repo: target.repo.clone(),
                        inventory_hash: target.inventory_hash.clone(),
                        max_bytes: MAX_QUERY_BYTES,
                    },
                    AttemptOutcome::InventoryAcquired { read_index: 0 },
                ),
            };
            let mut record = AttemptRecord {
                schema_version: 1,
                attempt_id: String::new(),
                task_id: "task".into(),
                request_hash: request.clone(),
                obligations_hash: envelope.fingerprint_with_policy(policy).unwrap(),
                obligation_id: item.key().1.into(),
                step_id: "read-step".into(),
                read,
                input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
                input_revision: ledger.revision,
                outcome,
            };
            record.attempt_id = record.content_id().unwrap();
            record
                .validate_binding_with_policy(&request, envelope, "task", ledger, policy)
                .unwrap();
            let normalized = envelope.normalized_with_policy(policy).unwrap();
            let concrete = std::mem::size_of_val(&record)
                + serde_json::to_vec(&record).unwrap().len() * 3
                + serde_json::to_vec(&normalized).unwrap().len() * 2;
            assert!(bound >= concrete);
            record.attempt_id = "0".repeat(64);
            assert!(
                record
                    .validate_binding_with_policy(&request, envelope, "task", ledger, policy)
                    .is_err()
            );
        }
        assert!(attempt_binding_validation_workspace(policy, MAX_RECORD_BYTES).unwrap() >= bound);
        assert!(attempt_binding_validation_workspace(policy, MAX_RECORD_BYTES + 1).is_none());
        assert!(attempt_binding_validation_workspace(policy, usize::MAX).is_none());
    }
}
