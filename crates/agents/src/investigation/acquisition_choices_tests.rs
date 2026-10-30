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
        schema_version: 1,
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
                super::super::response_schema::select_acquisition(mode, &sizing_only, &f.ledger)
                    .unwrap()
                    .as_deref(),
                Some(super::super::response_schema::FINISH)
            );
        }
    }
}
