use super::*;
use core_graph::coverage::{CoverageRecord, StoredCoverage};

const POLICY: &InvestigationPolicy = &INVESTIGATION_V2_POLICY;
struct Fixture {
    target: InvestigationInspectionTarget,
    ledger: InvestigationInputLedger,
    contract: InvestigationCompletionContract,
    metadata: ProtectedCompletionMetadata,
}
fn identified(mut item: Obligation) -> Obligation {
    let id = item.content_id().unwrap();
    match &mut item {
        Obligation::Source { obligation_id, .. } | Obligation::Inventory { obligation_id, .. } => {
            *obligation_id = id
        }
    }
    item
}
impl Fixture {
    fn new() -> Self {
        let (target, mut ledger, _) = crate::investigation::tests::inspection_tests::fixture();
        let record = CoverageRecord {
            repo_key: target.source.repo.clone(),
            registered_source_id: Some(target.registered_source_id.clone()),
            ingest_id: "fixture-ingest".into(),
            config_hash: "fixture-config".into(),
            inventory_json: format!(
                r#"{{"inventory":"{}","repo":"{}","commit":"pinned-revision","layer_hints":[],"producers":[{{"state":"not_selected","stopping_reason":"optional input not selected"}}],"unsupported":[],"limited":true}}"#,
                context_hub::coverage::INVENTORY_VERSION,
                target.source.repo
            ),
        };
        let stored = StoredCoverage {
            inventory_hash: record.inventory_hash(),
            record,
        };
        let qualification = context_hub::coverage::CoverageQualification::new(
            &[context_hub::coverage::ScopedRepo {
                repo_key: target.source.repo.clone(),
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
        let obligations = ObligationEnvelope {
            schema_version: 2,
            contract: "investigation-completion-obligations@2".into(),
            obligations: vec![
                identified(Obligation::Source {
                    obligation_id: String::new(),
                    target: Box::new(target.clone()),
                }),
                identified(Obligation::Inventory {
                    obligation_id: String::new(),
                    target: InventoryTarget {
                        context_id: ledger.coverage.as_ref().unwrap().context_id.clone(),
                        repo: target.source.repo.clone(),
                        inventory_hash: stored.inventory_hash,
                    },
                }),
            ],
        }
        .normalized_with_policy(POLICY)
        .unwrap();
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: "a".repeat(64),
            obligations_hash: obligations.fingerprint_with_policy(POLICY).unwrap(),
            obligations,
        };
        let metadata = ProtectedCompletionMetadata {
            schema_version: 1,
            request_hash: contract.request_hash.clone(),
            obligations_hash: contract.obligations_hash.clone(),
            completed_attempt_ids: vec![],
            refusal_count: 0,
            last_refusal_id: None,
            feedback: None,
        };
        Self {
            target,
            ledger,
            contract,
            metadata,
        }
    }
    fn empty(&self) -> InvestigationInputLedger {
        let mut ledger = self.ledger.clone();
        ledger.selected_facts.clear();
        ledger.citations.clear();
        ledger.receipt_references.clear();
        ledger.queries.clear();
        ledger
    }
    fn derive(
        &self,
        current: &InvestigationInputLedger,
        records: &[AttemptRecord],
    ) -> Result<AcquisitionChoices, InvestigationError> {
        let mut metadata = self.metadata.clone();
        metadata.completed_attempt_ids = records
            .iter()
            .map(|record| record.attempt_id.clone())
            .collect();
        let evidence = records
            .iter()
            .map(|record| AttemptEvidence {
                record,
                ledger: &self.ledger,
            })
            .collect::<Vec<_>>();
        AcquisitionChoices::derive(
            &self.contract,
            &metadata,
            "task",
            current,
            &evidence,
            POLICY,
        )
    }
    fn source_attempt(&self, outcome: AttemptOutcome) -> AttemptRecord {
        let obligation = self
            .contract
            .obligations
            .obligations
            .iter()
            .find(|item| matches!(item, Obligation::Source { .. }))
            .unwrap();
        let mut record = AttemptRecord {
            schema_version: 1,
            attempt_id: String::new(),
            task_id: "task".into(),
            request_hash: self.contract.request_hash.clone(),
            obligations_hash: self.contract.obligations_hash.clone(),
            obligation_id: obligation.key().1.into(),
            step_id: "read-step".into(),
            read: ReadIntent::Source {
                fact: self.target.fact.clone(),
                role: self.target.role,
                index: self.target.index,
            },
            input_ledger_hash: self.ledger.fingerprint_with_policy(POLICY).unwrap(),
            input_revision: self.ledger.revision,
            outcome,
        };
        record.attempt_id = record.content_id().unwrap();
        record
    }
}

// AC-0472: real target metadata becomes discovery, not source authority.
#[test]
fn obligation_acquisition_choices_follow_admission_and_attempts() {
    let mut f = Fixture::new();
    let initial = f.derive(&f.empty(), &[]).unwrap();
    assert_eq!(initial.entries().len(), 2);
    let query = initial
        .entries()
        .iter()
        .find_map(|entry| match &entry.action {
            AcquisitionAction::QueryContext { query, .. } => Some(query),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        query.scope,
        InvestigationScope::Node {
            id: match &f.target.fact {
                TaskFactKey::Node { id } => id.clone(),
                _ => panic!("node fixture"),
            }
        }
    );
    assert_eq!(
        (
            query.kind,
            query.max_facts,
            query.max_bytes,
            &query.labels,
            &query.cursor
        ),
        (
            Some(InvestigationFactKind::Node),
            1,
            MAX_QUERY_BYTES,
            &vec![],
            &None
        )
    );
    let admitted = f.derive(&f.ledger, &[]).unwrap();
    assert!(admitted.entries().iter().any(|entry| entry.action
        == AcquisitionAction::ReadEvidence {
            fact: f.target.fact.clone(),
            role: f.target.role,
            index: f.target.index
        }));
    let acquired = f.source_attempt(AttemptOutcome::SourceAcquired {
        citation_id: "evidence-1".into(),
    });
    assert_eq!(
        f.derive(&f.ledger, std::slice::from_ref(&acquired))
            .unwrap()
            .entries()
            .len(),
        1
    );
    let unavailable = f.source_attempt(AttemptOutcome::Unavailable {
        reason: UnavailableReason::PinnedTargetAbsent,
    });
    assert_eq!(
        f.derive(&f.ledger, &[unavailable]).unwrap().entries().len(),
        1
    );
    let refused = f.source_attempt(AttemptOutcome::FirewallRefused {
        acquired_attempt_id: acquired.attempt_id.clone(),
        observation_hash: "c".repeat(64),
    });
    assert!(
        f.derive(&f.ledger, &[refused.clone(), acquired.clone()])
            .is_err()
    );
    assert_eq!(
        f.derive(&f.ledger, &[acquired, refused])
            .unwrap()
            .entries()
            .len(),
        1
    );
    // A finish refusal has no completed read: it leaves both choices intact.
    f.metadata.refusal_count = 1;
    f.metadata.last_refusal_id = Some("d".repeat(64));
    f.metadata.feedback = Some(staged_finish::RefusalFeedback {
        prose_correction: None,
        schema_version: 1,
        citation_correction: None,
        unmet: f
            .contract
            .obligations
            .obligations
            .iter()
            .map(|item| staged_finish::FeedbackEntry {
                obligation_id: item.key().1.into(),
                kind: item.key().0.into(),
                reason: staged_finish::RefusalReason::NotAttempted,
            })
            .collect(),
    });
    assert_eq!(
        f.derive(&f.ledger, &[]).unwrap().entries(),
        admitted.entries()
    );
    let AcquisitionAction::ReadCoverageInventory {
        context_id,
        repo,
        inventory_hash,
        max_bytes,
    } = &admitted.entries()[0].action
    else {
        panic!("normalized inventory first")
    };
    assert_eq!(
        (context_id, repo, inventory_hash, *max_bytes),
        (
            &f.ledger.coverage.as_ref().unwrap().context_id,
            &f.target.source.repo,
            &f.contract
                .obligations
                .obligations
                .iter()
                .find_map(|item| if let Obligation::Inventory { target, .. } = item {
                    Some(target.inventory_hash.clone())
                } else {
                    None
                })
                .unwrap(),
            MAX_QUERY_BYTES
        )
    );
}

// AC-0472: failed discovery is blocked, not a fabricated source attempt.
#[test]
fn obligation_acquisition_choices_reject_foreign_state_and_suppress_empty_discovery() {
    let mut f = Fixture::new();
    let mut empty = f.empty();
    let plan = f.derive(&empty, &[]).unwrap();
    let query = plan
        .entries()
        .iter()
        .find_map(|entry| {
            if let AcquisitionAction::QueryContext { query, .. } = &entry.action {
                Some(query.clone())
            } else {
                None
            }
        })
        .unwrap();
    empty.queries.push(InvestigationQueryManifest {
        query,
        response_hash: "b".repeat(64),
        response_bytes: 100,
        returned_facts: 0,
        total_selected: 0,
        has_more: false,
    });
    let blocked = f.derive(&empty, &[]).unwrap();
    assert_eq!(blocked.entries().len(), 1);
    assert_eq!(blocked.blocked().len(), 1);
    assert!(matches!(
        blocked.entries()[0].action,
        AcquisitionAction::ReadCoverageInventory { .. }
    ));
    let mut wrong = f.ledger.clone();
    wrong
        .citations
        .retain(|item| matches!(item.origin, InvestigationEvidenceOrigin::GraphMetadata));
    wrong.selected_facts[0].binding.as_mut().unwrap().receipt_id =
        format!("ts-primary-v2:{}", "0".repeat(64));
    wrong.receipt_references[0].receipt_id = wrong.selected_facts[0]
        .binding
        .as_ref()
        .unwrap()
        .receipt_id
        .clone();
    wrong.validate_with_policy(POLICY).unwrap();
    assert!(f.derive(&wrong, &[]).is_err());
    f.metadata.obligations_hash = "e".repeat(64);
    assert!(f.derive(&empty, &[]).is_err());
    f.metadata.obligations_hash = f.contract.obligations_hash.clone();
    let mut evidence = f.source_attempt(AttemptOutcome::Unavailable {
        reason: UnavailableReason::PinnedTargetAbsent,
    });
    evidence.task_id = "foreign-task".into();
    evidence.attempt_id = evidence.content_id().unwrap();
    assert!(f.derive(&f.ledger, &[evidence]).is_err());
    f.contract.obligations.obligations.reverse();
    assert!(f.derive(&empty, &[]).is_err());
}

// AC-0472: the same validated state reconstructs the same bounded prefix.
#[test]
fn obligation_acquisition_choices_are_a_stable_bounded_prefix() {
    let mut f = Fixture::new();
    let mut sources = Vec::new();
    for index in 0..POLICY.max_completion_obligations() {
        let mut target = f.target.clone();
        target.fact = TaskFactKey::Node {
            id: format!("{index:02}{}", "\"".repeat(1000)),
        };
        target.target_id = target.content_id().unwrap();
        sources.push(identified(Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(target),
        }));
        let envelope = ObligationEnvelope {
            schema_version: 2,
            contract: "investigation-completion-obligations@2".into(),
            obligations: sources.clone(),
        };
        if envelope.normalized_with_policy(POLICY).is_err() {
            sources.pop();
            break;
        }
    }
    f.contract.obligations.obligations = sources;
    f.contract.obligations = f
        .contract
        .obligations
        .normalized_with_policy(POLICY)
        .unwrap();
    f.contract.obligations_hash = f
        .contract
        .obligations
        .fingerprint_with_policy(POLICY)
        .unwrap();
    f.metadata.obligations_hash = f.contract.obligations_hash.clone();
    let empty = f.empty();
    let plan = f.derive(&empty, &[]).unwrap();
    assert!(plan.deferred() > 0);
    assert!(!plan.entries().is_empty());
    assert_eq!(
        plan.entries().len() + plan.deferred(),
        f.contract.obligations.obligations.len()
    );
    assert!(bounded_bytes(&plan, MAX_ACTION_BYTES).is_ok());
    assert_eq!(
        serde_json::to_vec(&plan).unwrap(),
        serde_json::to_vec(&f.derive(&empty, &[]).unwrap()).unwrap()
    );
    assert_eq!(
        plan.entries()
            .iter()
            .map(|entry| entry.obligation_id.as_str())
            .collect::<Vec<_>>(),
        f.contract
            .obligations
            .obligations
            .iter()
            .take(plan.entries().len())
            .map(|item| item.key().1)
            .collect::<Vec<_>>()
    );
    // A structurally valid foreign binding only in the deferred suffix must
    // still be rejected, rather than hidden by the byte-prefix cut.
    let Obligation::Source { target, .. } = f.contract.obligations.obligations.last().unwrap()
    else {
        unreachable!()
    };
    let mut wrong = empty;
    let mut selected = f.ledger.selected_facts[0].clone();
    selected.fact = target.fact.clone();
    selected.binding.as_mut().unwrap().receipt_id = format!("ts-primary-v2:{}", "0".repeat(64));
    let mut citation = f
        .ledger
        .citations
        .iter()
        .find(|item| matches!(item.origin, InvestigationEvidenceOrigin::GraphMetadata))
        .unwrap()
        .clone();
    citation.fact = selected.fact.clone();
    let mut reference = f.ledger.receipt_references[0].clone();
    reference.receipt_id = selected.binding.as_ref().unwrap().receipt_id.clone();
    wrong.queries = f.ledger.queries.clone();
    wrong.selected_facts.push(selected);
    wrong.citations.push(citation);
    wrong.receipt_references.push(reference);
    wrong.validate_with_policy(POLICY).unwrap();
    assert!(f.derive(&wrong, &[]).is_err());
}

// AC-0472: source-fact validity does not imply exact-query-scope eligibility.
#[test]
fn obligation_acquisition_choices_block_only_unqueryable_source_ids() {
    for length in [1024, 1025, 4000] {
        let mut f = Fixture::new();
        f.target.fact = TaskFactKey::Node {
            id: "n".repeat(length),
        };
        f.target.target_id = f.target.content_id().unwrap();
        for item in &mut f.contract.obligations.obligations {
            if matches!(item, Obligation::Source { .. }) {
                *item = identified(Obligation::Source {
                    obligation_id: String::new(),
                    target: Box::new(f.target.clone()),
                });
            }
        }
        f.contract.obligations = f
            .contract
            .obligations
            .normalized_with_policy(POLICY)
            .unwrap_or_else(|error| panic!("contract for {length}-byte source ID: {error:?}"));
        f.contract.obligations_hash = f
            .contract
            .obligations
            .fingerprint_with_policy(POLICY)
            .unwrap();
        f.metadata.obligations_hash = f.contract.obligations_hash.clone();
        f.contract.validate_with_policy(POLICY).unwrap();
        let discovery = f
            .derive(&f.empty(), &[])
            .unwrap_or_else(|error| panic!("discovery for {length}-byte source ID: {error:?}"));
        assert!(discovery.entries().iter().any(|choice| matches!(
            choice.action,
            AcquisitionAction::ReadCoverageInventory { .. }
        )));
        if length == 1024 {
            assert!(discovery.blocked().is_empty());
            assert!(
                discovery
                    .entries()
                    .iter()
                    .any(|choice| matches!(choice.action, AcquisitionAction::QueryContext { .. }))
            );
        } else {
            assert_eq!(discovery.entries().len(), 1);
            assert_eq!(
                discovery.blocked(),
                &[f.contract
                    .obligations
                    .obligations
                    .iter()
                    .find(|item| matches!(item, Obligation::Source { .. }))
                    .unwrap()
                    .key()
                    .1]
            );
        }
        // An existing frozen binding remains readable even when its ID cannot
        // be expressed in a new exact-query scope. No source guard is bypassed.
        f.ledger.selected_facts[0].fact = f.target.fact.clone();
        for citation in &mut f.ledger.citations {
            citation.fact = f.target.fact.clone();
        }
        f.ledger.validate_with_policy(POLICY).unwrap();
        let admitted = f.derive(&f.ledger, &[]).unwrap();
        assert!(admitted.blocked().is_empty());
        assert!(admitted.entries().iter().any(|choice| choice.action
            == AcquisitionAction::ReadEvidence {
                fact: f.target.fact.clone(),
                role: f.target.role,
                index: f.target.index,
            }));
    }
}

// AC-0480: schema-only synthetic pending states never become input authority.
#[test]
fn gated_acquisition_schema_never_broadens_empty_pending_choices() {
    let f = Fixture::new();
    let mut sizing_only = f.derive(&f.empty(), &[]).unwrap();
    sizing_only.entries.clear();
    sizing_only.blocked.clear();
    sizing_only.deferred = 1;
    for blocked in [false, true] {
        if blocked {
            sizing_only.deferred = 0;
            sizing_only.blocked.push("a".repeat(64));
        }
        for mode in [
            staged_refusal::CompletionCallMode::Choice,
            staged_refusal::CompletionCallMode::FinishOnly,
        ] {
            assert_eq!(
                super::super::response_schema::select_acquisition(
                    mode,
                    &sizing_only,
                    &f.ledger,
                    &f.contract.request_hash
                )
                .unwrap()
                .as_deref(),
                Some(super::super::response_schema::FINISH)
            );
        }
    }
}

// AC-0480: completed inventory/source obligations do not invent query history.
#[test]
fn completed_acquisitions_without_queries_do_not_offer_replay() {
    let f = Fixture::new();
    let mut complete = f.derive(&f.empty(), &[]).unwrap();
    complete.entries.clear();
    complete.blocked.clear();
    complete.deferred = 0;
    assert!(complete.attempts_complete());
    let ledger = f.empty();
    assert!(ledger.queries.is_empty());
    let wire = super::super::response_schema::select_acquisition(
        staged_refusal::CompletionCallMode::Choice,
        &complete,
        &ledger,
        &f.contract.request_hash,
    )
    .unwrap()
    .unwrap();
    let schema: serde_json::Value = serde_json::from_str(&wire).unwrap();
    let actions = schema["anyOf"].as_array().unwrap();
    assert!(
        !actions
            .iter()
            .any(|a| a["properties"]["type"]["const"] == "replay_query")
    );
    assert!(
        actions
            .iter()
            .any(|a| a["properties"]["type"]["const"] == "query_context")
    );
}

// AC-0480: pair IDs with positions, including the latest recorded query.
#[test]
fn completed_acquisitions_offer_only_resolvable_replay_pairs() {
    let mut f = Fixture::new();
    assert!(!f.ledger.queries.is_empty());
    f.ledger.queries.push(f.ledger.queries[0].clone());
    f.ledger.validate_with_policy(POLICY).unwrap();
    let mut complete = f.derive(&f.ledger, &[]).unwrap();
    complete.entries.clear();
    complete.blocked.clear();
    complete.deferred = 0;
    let wire = super::super::response_schema::select_acquisition(
        staged_refusal::CompletionCallMode::Choice,
        &complete,
        &f.ledger,
        &f.contract.request_hash,
    )
    .unwrap()
    .unwrap();
    let schema: serde_json::Value = serde_json::from_str(&wire).unwrap();
    let pairs: Vec<_> = schema["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["properties"]["type"]["const"] == "replay_query")
        .collect();
    fn grammar_compatible(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                assert!(
                    !(map.contains_key("properties")
                        && (map.contains_key("anyOf") || map.contains_key("oneOf")))
                );
                for value in map.values() {
                    grammar_compatible(value);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    grammar_compatible(value);
                }
            }
            _ => {}
        }
    }
    grammar_compatible(&schema);
    assert_eq!(pairs.len(), f.ledger.queries.len());
    let first =
        QueryHistoryReference::from_ledger(&f.contract.request_hash, &f.ledger, 0, POLICY).unwrap();
    let swapped = QueryHistoryReference {
        query_index: 1,
        query_id: first.query_id.clone(),
    };
    assert!(
        swapped
            .resolve(&f.contract.request_hash, &f.ledger, POLICY)
            .is_err()
    );
    assert!(!pairs.iter().any(
        |pair| pair["properties"]["query_id"]["const"] == swapped.query_id
            && pair["properties"]["query_index"]["const"] == swapped.query_index
    ));

    for (index, pair) in pairs.iter().enumerate() {
        let reference =
            QueryHistoryReference::from_ledger(&f.contract.request_hash, &f.ledger, index, POLICY)
                .unwrap();
        assert_eq!(pair["properties"]["query_id"]["const"], reference.query_id);
        assert_eq!(pair["properties"]["query_index"]["const"], index);
        assert_eq!(pair["properties"]["type"]["const"], "replay_query");
        assert_eq!(
            pair["properties"]["context_id"]["const"],
            f.ledger.coverage.as_ref().unwrap().context_id
        );
        assert_eq!(
            pair["required"],
            serde_json::json!(["type", "context_id", "query_id", "query_index"])
        );
        assert_eq!(pair["additionalProperties"], false);
        assert!(bounded_bytes(pair, 1024).is_ok());
        assert!(
            reference
                .resolve(&f.contract.request_hash, &f.ledger, POLICY)
                .is_ok()
        );
    }
    assert_eq!(
        super::super::response_schema::select_acquisition(
            staged_refusal::CompletionCallMode::FinishOnly,
            &complete,
            &f.ledger,
            &f.contract.request_hash,
        )
        .unwrap()
        .as_deref(),
        Some(super::super::response_schema::FINISH)
    );
}

// AC-0260: an unknown-ID refusal offers fresh discovery, not another guessed ID.
#[test]
fn unknown_id_recovery_schema_is_bounded_and_preserves_legacy_feedback() {
    let f = Fixture::new();
    let ledger = f.empty();
    let mut complete = f.derive(&ledger, &[]).unwrap();
    complete.entries.clear();
    complete.blocked.clear();
    complete.deferred = 0;
    let mode = staged_refusal::CompletionCallMode::Choice;
    let ordinary =
        response_schema::select_acquisition(mode, &complete, &ledger, &f.contract.request_hash)
            .unwrap();
    let mut feedback = ActionRefusalFeedback {
        reason: ActionRefusalReason::QueryRejected,
        invocation_ordinal: 1,
        action_hash: "a".repeat(64),
        choices_hash: complete.stamp_hash().unwrap(),
    };
    let select = |feedback: &ActionRefusalFeedback| {
        response_schema::select_acquisition_recovery(
            mode,
            &complete,
            &ledger,
            &f.contract.request_hash,
            Some(feedback),
        )
        .unwrap()
    };
    assert_eq!(select(&feedback), ordinary);
    for reason in [
        ActionRefusalReason::UnknownAnchor,
        ActionRefusalReason::UnknownNode,
    ] {
        feedback.reason = reason;
        let recovered = select(&feedback).unwrap();
        assert!(recovered.len() < ordinary.as_ref().unwrap().len());
        let schema: serde_json::Value = serde_json::from_str(&recovered).unwrap();
        assert_eq!(
            schema["$defs"]["query_scope"]["anyOf"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            schema["$defs"]["query_scope"]["anyOf"][0]["properties"]["type"]["const"],
            "all"
        );
        assert_eq!(
            schema["$defs"]["query_cursor"],
            serde_json::json!({"type":"null"})
        );
        assert_eq!(
            response_schema::select_acquisition_recovery(
                staged_refusal::CompletionCallMode::FinishOnly,
                &complete,
                &ledger,
                &f.contract.request_hash,
                Some(&feedback)
            )
            .unwrap()
            .as_deref(),
            Some(response_schema::FINISH)
        );
    }
}

// AC-0565: classification cannot turn unknown or already-attempted targets into reads.
#[test]
fn unacquired_citation_feedback_rejects_foreign_mixed_and_attempted_targets() {
    let f = Fixture::new();
    let ledger = f.empty();
    let choices = f.derive(&ledger, &[]).unwrap();
    let classify = |ids: &[String]| {
        choices.unacquired_citation_targets(ids, &f.contract, &f.metadata, &ledger, POLICY)
    };
    let id = f.target.target_id.clone();
    assert_eq!(
        classify(&[id.clone(), id.clone()]).unwrap(),
        Some(vec![id.clone()])
    );
    assert_eq!(classify(&[]).unwrap(), None);
    assert_eq!(classify(&["foreign-target".into()]).unwrap(), None);
    assert_eq!(
        classify(&[id.clone(), "foreign-target".into()]).unwrap(),
        None
    );
    let mut foreign = f.metadata.clone();
    foreign.request_hash = "b".repeat(64);
    assert!(
        choices
            .unacquired_citation_targets(
                std::slice::from_ref(&id),
                &f.contract,
                &foreign,
                &ledger,
                POLICY
            )
            .is_err()
    );
    let attempt = f.source_attempt(AttemptOutcome::Unavailable {
        reason: UnavailableReason::PinnedTargetAbsent,
    });
    let mut metadata = f.metadata.clone();
    metadata
        .completed_attempt_ids
        .push(attempt.attempt_id.clone());
    let attempted = f.derive(&f.ledger, &[attempt]).unwrap();
    assert_eq!(
        attempted
            .unacquired_citation_targets(&[id], &f.contract, &metadata, &f.ledger, POLICY)
            .unwrap(),
        None
    );
}

// #1650: correction maps a mistaken target ID to the exact current action,
// never to an invented citation or an action from another acquisition phase.
#[test]
fn citation_correction_projects_exact_query_then_source_read() {
    let f = Fixture::new();
    let empty = f.empty();
    let ids = vec![f.target.target_id.clone(), f.target.target_id.clone()];
    for (ledger, expected_kind) in [(&empty, "query_context"), (&f.ledger, "read_evidence")] {
        let choices = f.derive(ledger, &[]).unwrap();
        let correction = choices
            .citation_correction(&ids, &f.contract, &f.metadata, ledger, POLICY)
            .unwrap()
            .unwrap();
        let value = serde_json::to_value(correction).unwrap();
        assert_eq!(value["entries"].as_array().unwrap().len(), 1);
        assert_eq!(value["entries"][0]["target_id"], f.target.target_id);
        assert_eq!(value["choices_hash"], choices.stamp_hash().unwrap());
        assert_eq!(value["entries"][0]["action"]["type"], expected_kind);
        let offered = choices
            .entries()
            .iter()
            .find(|entry| value["entries"][0]["obligation_id"] == entry.obligation_id)
            .unwrap();
        assert_eq!(
            value["entries"][0]["action"],
            serde_json::to_value(&offered.action).unwrap()
        );
        let mut mixed = ids.clone();
        mixed.push("foreign-target".into());
        assert!(
            choices
                .citation_correction(&mixed, &f.contract, &f.metadata, ledger, POLICY)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn citation_correction_decoding_does_not_authorize_forged_mapping() {
    let f = Fixture::new();
    let ledger = f.empty();
    let choices = f.derive(&ledger, &[]).unwrap();
    let ids = vec![f.target.target_id.clone()];
    let correction = choices
        .citation_correction(&ids, &f.contract, &f.metadata, &ledger, POLICY)
        .unwrap()
        .unwrap();
    correction
        .validate(&ids, &choices, &f.contract, &f.metadata, &ledger, POLICY)
        .unwrap();
    for change in 0..4 {
        let mut wire = serde_json::to_value(&correction).unwrap();
        match change {
            0 => wire["choices_hash"] = "a".repeat(64).into(),
            1 => wire["entries"][0]["target_id"] = "b".repeat(64).into(),
            2 => wire["entries"][0]["obligation_id"] = "c".repeat(64).into(),
            _ => wire["entries"][0]["action"]["query"]["max_facts"] = 2.into(),
        }
        let forged: acquisition_choices::CitationCorrection = serde_json::from_value(wire).unwrap();
        assert!(
            forged
                .validate(&ids, &choices, &f.contract, &f.metadata, &ledger, POLICY)
                .is_err()
        );
    }
    let mut wire = serde_json::to_value(&correction).unwrap();
    wire["candidate_prose"] = "must not survive decoding".into();
    assert!(serde_json::from_value::<acquisition_choices::CitationCorrection>(wire).is_err());
}

// AC-0566: decoded storage is authenticated at the original cutoff, while
// continuation guidance follows current acquisition state, not stale actions.
#[test]
fn citation_correction_reopen_reprojects_and_drops_attempted_targets() {
    let f = Fixture::new();
    let empty = f.empty();
    let original = f.derive(&empty, &[]).unwrap();
    let ids = vec![f.target.target_id.clone()];
    let saved = original
        .citation_correction(&ids, &f.contract, &f.metadata, &empty, POLICY)
        .unwrap()
        .unwrap();
    let wire = serde_json::to_vec(&saved).unwrap();
    let reopened: acquisition_choices::CitationCorrection = serde_json::from_slice(&wire).unwrap();
    let authenticated = reopened
        .authenticate(&ids, &original, &f.contract, &f.metadata, &empty, POLICY)
        .unwrap();
    let current = f.derive(&f.ledger, &[]).unwrap();
    let projected = authenticated
        .project(&current, &f.contract, &f.metadata, &f.ledger, POLICY)
        .unwrap()
        .unwrap();
    let value = serde_json::to_value(&projected).unwrap();
    assert_eq!(value["entries"][0]["action"]["type"], "read_evidence");
    assert_eq!(value["choices_hash"], current.stamp_hash().unwrap());
    assert_ne!(projected, reopened);
    assert_eq!(serde_json::to_vec(&reopened).unwrap(), wire);
    // Historical guidance is never valid at the new cutoff.
    assert!(
        reopened
            .authenticate(&ids, &current, &f.contract, &f.metadata, &f.ledger, POLICY)
            .is_err()
    );
    let attempt = f.source_attempt(AttemptOutcome::Unavailable {
        reason: UnavailableReason::PinnedTargetAbsent,
    });
    let mut metadata = f.metadata.clone();
    metadata
        .completed_attempt_ids
        .push(attempt.attempt_id.clone());
    let attempted = f.derive(&f.ledger, &[attempt]).unwrap();
    assert!(
        authenticated
            .project(&attempted, &f.contract, &metadata, &f.ledger, POLICY)
            .unwrap()
            .is_none()
    );
    let mut foreign = f.contract.clone();
    foreign.request_hash = "b".repeat(64);
    assert!(
        authenticated
            .project(&current, &foreign, &f.metadata, &f.ledger, POLICY)
            .is_err()
    );
}

// AC-0566: additive wire field preserves legacy bytes and shares, rather than
// increases, the existing feedback limit. This does not activate stored v2.
#[test]
fn citation_correction_feedback_version_and_shared_budget() {
    use staged_finish::{FeedbackEntry, RefusalFeedback, RefusalReason};
    let f = Fixture::new();
    let empty = f.empty();
    let choices = f.derive(&empty, &[]).unwrap();
    let correction = choices
        .citation_correction(
            std::slice::from_ref(&f.target.target_id),
            &f.contract,
            &f.metadata,
            &empty,
            POLICY,
        )
        .unwrap()
        .unwrap();
    let id = correction.entries[0].obligation_id.clone();
    let legacy = serde_json::json!({
        "schema_version": 1,
        "unmet": [{"obligation_id": id, "kind": "source", "reason": "not_attempted"}]
    });
    let mut feedback: RefusalFeedback = serde_json::from_value(legacy.clone()).unwrap();
    let old_bytes = feedback.bytes_with_policy(POLICY).unwrap();
    assert_eq!(serde_json::to_value(&feedback).unwrap(), legacy);
    feedback.citation_correction = Some(correction);
    assert!(feedback.bytes_with_policy(POLICY).is_err());
    feedback.schema_version = 2;
    let bytes = feedback.bytes_with_policy(POLICY).unwrap();
    let reopened: RefusalFeedback = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reopened, feedback);
    assert_eq!(reopened.bytes_with_policy(POLICY).unwrap(), bytes);
    feedback.unmet[0].reason = RefusalReason::Unavailable;
    assert!(feedback.bytes_with_policy(POLICY).is_err());
    feedback.unmet[0].reason = RefusalReason::NotAttempted;
    // Fill the original v1 ceiling with valid generic entries; adding the
    // separately bounded mapping must fail the complete feedback admission.
    for i in 0..11 {
        feedback.unmet.push(FeedbackEntry {
            obligation_id: format!("{i:064x}"),
            kind: "inventory".into(),
            reason: RefusalReason::AcquiredNotSupplied,
        });
    }
    feedback
        .unmet
        .sort_by(|a, b| (&a.kind, &a.obligation_id).cmp(&(&b.kind, &b.obligation_id)));
    assert!(
        feedback
            .citation_correction
            .as_ref()
            .unwrap()
            .bytes_with_policy(&INVESTIGATION_V1_POLICY)
            .is_ok()
    );
    assert!(matches!(
        feedback.bytes_with_policy(&INVESTIGATION_V1_POLICY),
        Err(InvestigationError::LimitExceeded)
    ));
    feedback.citation_correction = None;
    feedback.schema_version = 1;
    assert!(feedback.bytes_with_policy(&INVESTIGATION_V1_POLICY).is_ok());
    feedback.unmet.retain(|entry| entry.kind == "source");
    assert_eq!(feedback.bytes_with_policy(POLICY).unwrap(), old_bytes);
}

#[test]
fn citation_correction_analyst20_prompt_qualifies_finish_without_rewriting_19() {
    let old = SpecialistId::DomainAnalystV19;
    assert_eq!(
        old.prompt()
            .matches("Completion obligations are mandatory. Return only")
            .count(),
        1
    );
    let new = SpecialistId::DomainAnalystV20;
    assert_eq!(
        old.definition().unwrap().prompt_fingerprint,
        "6bce88ebb8d1a745172d4faf79294d29f919f140775b5d0794ac35c8e7c9214f"
    );
    assert!(
        old.prompt()
            .contains("Completion obligations are mandatory. Return only")
    );
    assert!(
        !new.prompt()
            .contains("Completion obligations are mandatory. Return only")
    );
    assert!(
        new.prompt()
            .contains("When finishing, return only the versioned finish wrapper")
    );
    assert!(
        new.prompt()
            .contains("These IDs are source targets, not citations")
    );
    let paraphrase =
        "Describe cited source in your own words; never copy a complete source excerpt";
    assert!(new.prompt().contains(paraphrase));
    assert!(!old.prompt().contains(paraphrase));
    assert!(!SpecialistId::DomainAnalystV18.prompt().contains(paraphrase));
    assert_eq!(
        new.definition().unwrap().prompt_fingerprint,
        "25ae99fe3e2a6611db20cc673b27190a59dfe1061aa5a786cbe351b29d9e65e4"
    );
    assert_eq!(new.definition().unwrap().version, 20);
    assert_ne!(
        new.definition().unwrap().prompt_fingerprint,
        old.definition().unwrap().prompt_fingerprint
    );
}

// AC-0566: receipt handles map to offered targets, never to citations.
#[test]
fn citation_correction_receipt_handles_preserve_historical_and_foreign_boundaries() {
    let f = Fixture::new();
    let ledger = f.empty();
    let choices = f.derive(&ledger, &[]).unwrap();
    let receipt = f.target.receipt_id.clone();
    let classify = |ids: &[String]| {
        choices.unacquired_citation_handles(ids, &f.contract, &f.metadata, &ledger, POLICY)
    };
    assert_eq!(
        classify(std::slice::from_ref(&receipt)).unwrap(),
        Some(vec![f.target.target_id.clone()])
    );
    assert_eq!(
        classify(&[receipt.clone(), "foreign".into()]).unwrap(),
        None
    );
    assert_eq!(
        choices
            .unacquired_citation_targets(&[receipt], &f.contract, &f.metadata, &ledger, POLICY)
            .unwrap(),
        None
    );
}

// AC-0566: every target sharing a receipt must remain pending and offered.
#[test]
fn citation_correction_shared_receipt_maps_all_targets_and_rejects_attempted_member() {
    let mut f = Fixture::new();
    let mut second = f.target.clone();
    second.fact = TaskFactKey::Node {
        id: "second-source-fact".into(),
    };
    second.target_id = second.content_id().unwrap();
    f.contract
        .obligations
        .obligations
        .push(identified(Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(second.clone()),
        }));
    f.contract.obligations = f
        .contract
        .obligations
        .normalized_with_policy(POLICY)
        .unwrap();
    f.contract.obligations_hash = f
        .contract
        .obligations
        .fingerprint_with_policy(POLICY)
        .unwrap();
    f.metadata.obligations_hash = f.contract.obligations_hash.clone();
    let ledger = f.empty();
    let choices = f.derive(&ledger, &[]).unwrap();
    let mut expected = vec![f.target.target_id.clone(), second.target_id];
    expected.sort();
    let ids = vec![f.target.receipt_id.clone()];
    assert_eq!(
        choices
            .unacquired_citation_handles(&ids, &f.contract, &f.metadata, &ledger, POLICY)
            .unwrap(),
        Some(expected)
    );
    let attempt = f.source_attempt(AttemptOutcome::Unavailable {
        reason: UnavailableReason::PinnedTargetAbsent,
    });
    let mut metadata = f.metadata.clone();
    metadata
        .completed_attempt_ids
        .push(attempt.attempt_id.clone());
    let attempted = f.derive(&f.ledger, &[attempt]).unwrap();
    assert_eq!(
        attempted
            .unacquired_citation_handles(&ids, &f.contract, &metadata, &f.ledger, POLICY)
            .unwrap(),
        None
    );
}

#[path = "acquisition_eligibility_tests.rs"]
mod eligibility;
