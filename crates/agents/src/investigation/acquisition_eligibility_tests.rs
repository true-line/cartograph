use super::*;
use crate::investigation::response_schema;
use staged_refusal::CompletionCallMode::{Choice, FinishOnly};

fn selected(
    f: &Fixture,
    choices: &AcquisitionChoices,
    ledger: &InvestigationInputLedger,
) -> serde_json::Value {
    let ordinary =
        response_schema::select_acquisition(Choice, choices, ledger, &f.contract.request_hash)
            .unwrap();
    serde_json::from_str(
        &response_schema::select_eligible_acquisition(ordinary, Choice, choices, ledger)
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}
// AC-0566: transport choices cannot offer a guaranteed premature finish.
#[test]
fn analyst20_schema_pending_and_partial_attempts_offer_acquisition_only() {
    let f = Fixture::new();
    let pending = f.derive(&f.ledger, &[]).unwrap();
    let partial = f
        .derive(
            &f.ledger,
            &[f.source_attempt(AttemptOutcome::Unavailable {
                reason: UnavailableReason::PinnedTargetAbsent,
            })],
        )
        .unwrap();
    for choices in [&pending, &partial] {
        let schema = selected(&f, choices, &f.ledger);
        assert_eq!(schema["anyOf"].as_array().unwrap().len(), 1);
        assert_eq!(
            schema["anyOf"][0]["enum"].as_array().unwrap().len(),
            choices.entries().len()
        );
        let old = response_schema::select_acquisition(
            Choice,
            choices,
            &f.ledger,
            &f.contract.request_hash,
        )
        .unwrap()
        .unwrap();
        assert!(old.contains("\"$ref\":\"#/$defs/supported_wrapper\""));
    }
}
// AC-0566: an authenticated completed attempt enables honest insufficiency;
// acquired citations constrain supported output without replacing host guards.
#[test]
fn analyst20_schema_completed_attempts_constrain_citations() {
    let mut f = Fixture::new();
    f.contract
        .obligations
        .obligations
        .retain(|o| matches!(o, Obligation::Source { .. }));
    f.contract.obligations_hash = f
        .contract
        .obligations
        .fingerprint_with_policy(POLICY)
        .unwrap();
    f.metadata.obligations_hash = f.contract.obligations_hash.clone();
    let attempt = f.source_attempt(AttemptOutcome::Unavailable {
        reason: UnavailableReason::PinnedTargetAbsent,
    });
    let mut ledger = f.ledger.clone();
    let complete = f.derive(&ledger, std::slice::from_ref(&attempt)).unwrap();
    assert!(complete.attempts_complete());
    let ordinary =
        response_schema::select_acquisition(Choice, &complete, &ledger, &f.contract.request_hash)
            .unwrap();
    let finish_schema = |ledger: &InvestigationInputLedger| -> serde_json::Value {
        serde_json::from_str(
            &response_schema::select_eligible_acquisition(
                ordinary.clone(),
                Choice,
                &complete,
                ledger,
            )
            .unwrap()
            .unwrap(),
        )
        .unwrap()
    };
    let schema = finish_schema(&ledger);
    assert!(
        schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|branch| { branch["properties"]["type"]["const"] == "query_context" }),
        "attempted but unavailable must retain exploration"
    );
    let expected: Vec<_> = ledger
        .citations
        .iter()
        .map(|c| c.citation_id.clone())
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(
        schema["$defs"]["finding"]["properties"]["citation_ids"]["items"]["enum"],
        serde_json::json!(expected)
    );
    ledger.citations.clear();
    let schema = finish_schema(&ledger);
    assert!(
        !schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["$ref"] == "#/$defs/supported_wrapper")
    );
    assert!(
        schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["$ref"] == "#/$defs/insufficient_wrapper")
    );
}
// AC-0566: only fully acquired obligations close exploration.
#[test]
fn analyst20_schema_all_acquired_removes_exploration() {
    let mut f = Fixture::new();
    f.contract
        .obligations
        .obligations
        .retain(|o| matches!(o, Obligation::Source { .. }));
    f.contract.obligations_hash = f
        .contract
        .obligations
        .fingerprint_with_policy(POLICY)
        .unwrap();
    f.metadata.obligations_hash = f.contract.obligations_hash.clone();
    let attempt = f.source_attempt(AttemptOutcome::SourceAcquired {
        citation_id: "evidence-1".into(),
    });
    let mut ledger = f.ledger.clone();
    let complete = f.derive(&ledger, std::slice::from_ref(&attempt)).unwrap();
    assert!(complete.attempts_complete());
    let ordinary =
        response_schema::select_acquisition(Choice, &complete, &ledger, &f.contract.request_hash)
            .unwrap();
    let finish_schema = |ledger: &InvestigationInputLedger| -> serde_json::Value {
        serde_json::from_str(
            &response_schema::select_eligible_acquisition(
                ordinary.clone(),
                Choice,
                &complete,
                ledger,
            )
            .unwrap()
            .unwrap(),
        )
        .unwrap()
    };
    let schema = finish_schema(&ledger);
    assert!(
        schema["anyOf"].as_array().unwrap().iter().all(|branch| {
            matches!(
                branch["$ref"].as_str(),
                Some("#/$defs/supported_wrapper" | "#/$defs/insufficient_wrapper")
            )
        }),
        "completed attempts must not reopen exploration"
    );
    let expected: Vec<_> = ledger
        .citations
        .iter()
        .map(|c| c.citation_id.clone())
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(
        schema["$defs"]["finding"]["properties"]["citation_ids"]["items"]["enum"],
        serde_json::json!(expected)
    );
    ledger.citations.clear();
    let schema = finish_schema(&ledger);
    assert!(
        !schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["$ref"] == "#/$defs/supported_wrapper")
    );
    assert!(
        schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["$ref"] == "#/$defs/insufficient_wrapper")
    );
}
// AC-0566: blocked/deferred no-action states and FinishOnly retain fallback,
// never an empty grammar or invented completed attempt.
#[test]
fn analyst20_schema_preserves_no_action_and_finish_only_fallbacks() {
    let f = Fixture::new();
    let mut choices = f.derive(&f.ledger, &[]).unwrap();
    let old = response_schema::select_acquisition(
        FinishOnly,
        &choices,
        &f.ledger,
        &f.contract.request_hash,
    )
    .unwrap();
    assert_eq!(
        response_schema::select_eligible_acquisition(old.clone(), FinishOnly, &choices, &f.ledger)
            .unwrap(),
        old
    );
    choices.entries.clear();
    for deferred in [false, true] {
        choices.blocked.clear();
        choices.deferred = usize::from(deferred);
        if !deferred {
            choices.blocked.push("blocked".into());
        }
        assert!(!choices.attempts_complete());
        let old = response_schema::select_acquisition(
            Choice,
            &choices,
            &f.ledger,
            &f.contract.request_hash,
        )
        .unwrap();
        assert_eq!(
            response_schema::select_eligible_acquisition(old.clone(), Choice, &choices, &f.ledger)
                .unwrap(),
            old
        );
    }
}

// AC-0566: prospective source citation bytes must be reserved, including the
// schema's outer JSON string escaping. Sizing data is not acquired evidence.
#[test]
fn analyst20_schema_reserve_includes_prospective_citation_enum() {
    let f = Fixture::new();
    let mut choices = f.derive(&f.ledger, &[]).unwrap();
    choices.entries.clear();
    choices.blocked.clear();
    choices.deferred = 0;
    let mut ledger = f.ledger.clone();
    let mut future = ledger.citations[0].clone();
    future.citation_id = "evidence-999".into();
    ledger.citations.push(future);
    let prompt = serde_json::json!({"citations":ledger.citations});
    let reserve = response_schema::reserve_eligible_acquisition(
        Some(
            serde_json::to_string(
                &serde_json::from_str::<serde_json::Value>(response_schema::FINISH).unwrap(),
            )
            .unwrap(),
        ),
        Choice,
        &prompt,
    )
    .unwrap()
    .unwrap();
    let actual = response_schema::select_eligible_acquisition(
        Some(
            serde_json::to_string(
                &serde_json::from_str::<serde_json::Value>(response_schema::FINISH).unwrap(),
            )
            .unwrap(),
        ),
        Choice,
        &choices,
        &ledger,
    )
    .unwrap()
    .unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&reserve).unwrap();
    assert!(
        decoded["$defs"]["finding"]["properties"]["citation_ids"]["items"]["anyOf"][1]["enum"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("evidence-999"))
    );
    let mut actual = actual;
    let mut reserve = reserve;
    for _ in 0..4 {
        assert!(reserve.len() >= actual.len());
        actual = serde_json::to_string(&actual).unwrap();
        reserve = serde_json::to_string(&reserve).unwrap();
    }
}

// AC-0566: a short exact enum must not under-reserve the unchanged fallback.
#[test]
fn analyst20_schema_reserve_dominates_short_id_fallback() {
    let prompt = serde_json::json!({"citations":[{"citation_id":"x"}]});
    let mut reserve = response_schema::reserve_eligible_acquisition(
        Some(response_schema::FINISH.into()),
        Choice,
        &prompt,
    )
    .unwrap()
    .unwrap();
    let mut fallback = response_schema::FINISH.to_owned();
    for _ in 0..4 {
        assert!(reserve.len() >= fallback.len());
        reserve = serde_json::to_string(&reserve).unwrap();
        fallback = serde_json::to_string(&fallback).unwrap();
    }
}

// AC-0566: exact contract action bounds dominate both query and read mappings,
// including complete feedback and current correction at repeated escape layers.
#[test]
fn analyst20_contract_feedback_bound_covers_query_and_read_corrections() {
    let f = Fixture::new();
    for ledger in [f.empty(), f.ledger.clone()] {
        let choices = f.derive(&ledger, &[]).unwrap();
        let correction = choices
            .citation_correction(
                std::slice::from_ref(&f.target.target_id),
                &f.contract,
                &f.metadata,
                &ledger,
                POLICY,
            )
            .unwrap()
            .unwrap();
        let feedback = staged_finish::RefusalFeedback {
            citation_drop: None,
            prose_correction: None,
            schema_version: 2,
            citation_correction: Some(correction.clone()),
            unmet: f
                .contract
                .obligations
                .obligations
                .iter()
                .map(|o| {
                    let (kind, id) = o.key();
                    staged_finish::FeedbackEntry {
                        obligation_id: id.into(),
                        kind: kind.into(),
                        reason: staged_finish::RefusalReason::NotAttempted,
                    }
                })
                .collect(),
        };
        let bound = feedback_encoded_bound(&f.contract, &ledger, POLICY).unwrap();
        assert!(bound < staged_finish::RefusalFeedback::max_bytes_with_policy(POLICY).unwrap());
        assert!(feedback.bytes_with_policy(POLICY).unwrap().len() <= bound);
        let upper = staged_finish::RefusalFeedback::sizing_typed_contract_envelope(
            &f.contract,
            &ledger,
            POLICY,
        )
        .unwrap();
        for value in [
            serde_json::to_value(feedback).unwrap(),
            serde_json::to_value(correction).unwrap(),
        ] {
            let mut actual = serde_json::to_string(&value).unwrap();
            let mut maximum = serde_json::to_string(&upper).unwrap();
            for _ in 0..4 {
                assert!(actual.len() <= maximum.len());
                actual = serde_json::to_string(&actual).unwrap();
                maximum = serde_json::to_string(&maximum).unwrap();
            }
        }
    }
}

// AC-0566: all targets choose their largest finite action, including escaped
// identities; subsets and policy-ceiling fallback cannot enlarge the bound.
#[test]
fn analyst20_contract_bound_covers_largest_actions_for_all_targets() {
    for count in [3, 7, 20] {
        let mut f = Fixture::new();
        f.contract
            .obligations
            .obligations
            .retain(|o| matches!(o, Obligation::Inventory { .. }));
        for index in 0..count {
            let mut target = f.target.clone();
            target.fact = TaskFactKey::Node {
                id: format!(
                    "node-{index}-{}",
                    "quoted-\"-\\-é".repeat(if count == 20 { 15 } else { 1 })
                ),
            };
            target.target_id = target.content_id().unwrap();
            f.contract
                .obligations
                .obligations
                .push(identified(Obligation::Source {
                    obligation_id: String::new(),
                    target: Box::new(target),
                }));
        }
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
        let bound = feedback_encoded_bound(&f.contract, &ledger, POLICY).unwrap();
        let ids = f
            .contract
            .obligations
            .obligations
            .iter()
            .filter_map(|o| match o {
                Obligation::Source { target, .. } => Some(target.target_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if count < 20 {
            assert!(bound < staged_finish::RefusalFeedback::max_bytes_with_policy(POLICY).unwrap());
        }
        for size in [1, ids.len()] {
            let correction = choices.citation_correction(
                &ids[..size],
                &f.contract,
                &f.metadata,
                &ledger,
                POLICY,
            );
            if let Ok(Some(correction)) = correction {
                assert_eq!(correction.entries.len(), size);
                for entry in &correction.entries {
                    let target = f
                        .contract
                        .obligations
                        .obligations
                        .iter()
                        .find_map(|o| match o {
                            Obligation::Source { target, .. }
                                if target.target_id == entry.target_id =>
                            {
                                Some(target)
                            }
                            _ => None,
                        })
                        .unwrap();
                    let read = AcquisitionAction::ReadEvidence {
                        fact: target.fact.clone(),
                        role: target.role,
                        index: target.index,
                    };
                    assert!(
                        serde_json::to_vec(&entry.action).unwrap().len()
                            >= serde_json::to_vec(&read).unwrap().len()
                    );
                }
                let feedback = staged_finish::RefusalFeedback {
                    citation_drop: None,
                    prose_correction: None,
                    schema_version: 2,
                    citation_correction: Some(correction),
                    unmet: f
                        .contract
                        .obligations
                        .obligations
                        .iter()
                        .map(|o| {
                            let (kind, id) = o.key();
                            staged_finish::FeedbackEntry {
                                kind: kind.into(),
                                obligation_id: id.into(),
                                reason: staged_finish::RefusalReason::NotAttempted,
                            }
                        })
                        .collect(),
                };
                if let Ok(bytes) = feedback.bytes_with_policy(POLICY) {
                    assert!(bytes.len() <= bound);
                    let upper = staged_finish::RefusalFeedback::sizing_typed_contract_envelope(
                        &f.contract,
                        &ledger,
                        POLICY,
                    )
                    .unwrap();
                    let mut actual = String::from_utf8(bytes).unwrap();
                    let mut maximum = serde_json::to_string(&upper).unwrap();
                    for _ in 0..4 {
                        assert!(actual.len() <= maximum.len());
                        actual = serde_json::to_string(&actual).unwrap();
                        maximum = serde_json::to_string(&maximum).unwrap();
                    }
                } else {
                    assert_eq!(
                        bound,
                        staged_finish::RefusalFeedback::max_bytes_with_policy(POLICY).unwrap()
                    );
                }
            } else {
                assert_eq!(
                    bound,
                    staged_finish::RefusalFeedback::max_bytes_with_policy(POLICY).unwrap()
                );
            }
        }
        if count == 20 {
            assert_eq!(
                bound,
                staged_finish::RefusalFeedback::max_bytes_with_policy(POLICY).unwrap()
            );
        }
    }
}

// AC-0566: acquired source plus unavailable inventory is not all-acquired.
#[test]
fn analyst20_schema_mixed_acquisition_retains_exploration() {
    let f = Fixture::new();
    let source = f.source_attempt(AttemptOutcome::SourceAcquired {
        citation_id: "evidence-1".into(),
    });
    let (obligation_id, target) = f
        .contract
        .obligations
        .obligations
        .iter()
        .find_map(|o| match o {
            Obligation::Inventory {
                obligation_id,
                target,
            } => Some((obligation_id, target)),
            _ => None,
        })
        .unwrap();
    let mut inventory = source.clone();
    inventory.obligation_id = obligation_id.clone();
    inventory.step_id = "inventory-step".into();
    inventory.read = ReadIntent::Inventory {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: MAX_QUERY_BYTES,
    };
    inventory.outcome = AttemptOutcome::Unavailable {
        reason: UnavailableReason::InventoryExceedsMaxBytes,
    };
    inventory.attempt_id = inventory.content_id().unwrap();
    let choices = f.derive(&f.ledger, &[source, inventory]).unwrap();
    assert!(choices.attempts_complete());
    assert!(!choices.all_acquired());
    let schema = selected(&f, &choices, &f.ledger);
    assert!(
        schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|branch| { branch["properties"]["type"]["const"] == "query_context" })
    );
}

// AC-0566: retained acquisition survives revision growth and a disclosure
// refusal, but not loss of its exact citation. The witness is never wire data.
#[test]
fn analyst20_schema_acquired_witness_tracks_current_ledger() {
    let mut f = Fixture::new();
    f.contract
        .obligations
        .obligations
        .retain(|o| matches!(o, Obligation::Source { .. }));
    f.contract.obligations_hash = f
        .contract
        .obligations
        .fingerprint_with_policy(POLICY)
        .unwrap();
    f.metadata.obligations_hash = f.contract.obligations_hash.clone();
    let source = f.source_attempt(AttemptOutcome::SourceAcquired {
        citation_id: "evidence-1".into(),
    });
    let refused = f.source_attempt(AttemptOutcome::FirewallRefused {
        acquired_attempt_id: source.attempt_id.clone(),
        observation_hash: "c".repeat(64),
    });
    let mut current = f.ledger.clone();
    current.revision += 1;
    let records = [source, refused];
    let choices = f.derive(&current, &records).unwrap();
    assert!(choices.all_acquired());
    let wire = serde_json::to_value(&choices).unwrap();
    assert!(wire.get("all_acquired").is_none());
    let mut conservative = choices.clone();
    conservative.all_acquired = false;
    assert_eq!(serde_json::to_value(&conservative).unwrap(), wire);
    let ordinary =
        response_schema::select_acquisition(Choice, &choices, &current, &f.contract.request_hash)
            .unwrap();
    let reserve = response_schema::reserve_eligible_acquisition(
        ordinary,
        Choice,
        &serde_json::json!({"citations":current.citations}),
    )
    .unwrap()
    .unwrap();
    let actual = serde_json::to_string(&selected(&f, &choices, &current)).unwrap();
    assert!(
        serde_json::to_string(&reserve).unwrap().len()
            >= serde_json::to_string(&actual).unwrap().len()
    );
    current.citations.retain(|c| c.citation_id != "evidence-1");
    let missing = f.derive(&current, &records).unwrap();
    assert!(missing.attempts_complete());
    assert!(!missing.all_acquired());
}

// AC-0566: inconsistent readiness must not erase acquisition branches;
// inventory-only completion without citations still permits insufficiency.
#[test]
fn analyst20_schema_never_empties_on_inconsistent_or_inventory_only_state() {
    let mut f = Fixture::new();
    let mut inconsistent = f.derive(&f.ledger, &[]).unwrap();
    inconsistent.all_acquired = true;
    assert!(!inconsistent.attempts_complete());
    let schema = selected(&f, &inconsistent, &f.ledger);
    assert!(!schema["anyOf"].as_array().unwrap().is_empty());
    assert!(schema["anyOf"][0].get("enum").is_some());

    let mut record = f.source_attempt(AttemptOutcome::InventoryAcquired { read_index: 0 });
    f.contract
        .obligations
        .obligations
        .retain(|o| matches!(o, Obligation::Inventory { .. }));
    f.contract.obligations_hash = f
        .contract
        .obligations
        .fingerprint_with_policy(POLICY)
        .unwrap();
    f.metadata.obligations_hash = f.contract.obligations_hash.clone();
    f.ledger = f.empty();
    let Obligation::Inventory {
        obligation_id,
        target,
    } = &f.contract.obligations.obligations[0]
    else {
        unreachable!()
    };
    f.ledger
        .coverage_reads
        .push(InvestigationCoverageReadManifest {
            request: InvestigationCoverageRead {
                context_id: target.context_id.clone(),
                repo: target.repo.clone(),
                inventory_hash: target.inventory_hash.clone(),
                max_bytes: MAX_QUERY_BYTES,
            },
            response_hash: "a".repeat(64),
            response_bytes: 1,
        });
    record.obligation_id = obligation_id.clone();
    record.obligations_hash = f.contract.obligations_hash.clone();
    record.read = ReadIntent::Inventory {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: MAX_QUERY_BYTES,
    };
    record.input_ledger_hash = f.ledger.fingerprint_with_policy(POLICY).unwrap();
    record.attempt_id = record.content_id().unwrap();
    let choices = f.derive(&f.ledger, &[record]).unwrap();
    assert!(choices.attempts_complete() && choices.all_acquired());
    assert!(f.ledger.citations.is_empty());
    let schema = selected(&f, &choices, &f.ledger);
    assert_eq!(
        schema["anyOf"],
        serde_json::json!([{ "$ref": "#/$defs/insufficient_wrapper" }])
    );
}

/// Inventory attempt for `f`'s Inventory obligation, built like `source`.
fn inventory_attempt(
    f: &Fixture,
    source: &AttemptRecord,
    outcome: AttemptOutcome,
) -> AttemptRecord {
    let (obligation_id, target) = f
        .contract
        .obligations
        .obligations
        .iter()
        .find_map(|o| match o {
            Obligation::Inventory {
                obligation_id,
                target,
            } => Some((obligation_id, target)),
            _ => None,
        })
        .unwrap();
    let mut inventory = source.clone();
    inventory.obligation_id = obligation_id.clone();
    inventory.step_id = "inventory-step".into();
    inventory.read = ReadIntent::Inventory {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: MAX_QUERY_BYTES,
    };
    inventory.outcome = outcome;
    inventory.attempt_id = inventory.content_id().unwrap();
    inventory
}

/// Checks the eligibility shape a dumped schema must have for its state.
fn assert_eligible_shape(
    schema: &serde_json::Value,
    choices: &AcquisitionChoices,
    ledger: &InvestigationInputLedger,
) {
    let branches = schema["anyOf"].as_array().unwrap();
    let has = |target: &str| branches.iter().any(|b| b["$ref"] == target);
    let complete = choices.attempts_complete();
    assert_eq!(
        has("#/$defs/supported_wrapper"),
        complete && !ledger.citations.is_empty()
    );
    assert_eq!(has("#/$defs/insufficient_wrapper"), complete);
    let exploration = branches
        .iter()
        .any(|b| b["properties"]["type"]["const"] == "query_context");
    if complete {
        assert_eq!(exploration, !choices.all_acquired());
    } else {
        assert_eq!(branches.len(), 1);
        assert!(branches[0].get("enum").is_some());
    }
}

// #1709 / #1710: writes the analyst response-schema variants for the
// owner-run hosted schema preflight (`llm` example `anthropic_schema_preflight`)
// from genuine states, each asserted before it is written. Ignored by default
// and makes no network call.
// Run with CARTOGRAPH_SCHEMA_DUMP=<empty dir> cargo test -p agents -- --ignored dump_analyst_schema_variants
#[test]
#[ignore]
fn dump_analyst_schema_variants() {
    let Some(dir) = std::env::var_os("CARTOGRAPH_SCHEMA_DUMP") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let mut written = std::collections::BTreeSet::new();
    let mut write = |name: &str, schema: &str| {
        let value: serde_json::Value = serde_json::from_str(schema).unwrap();
        assert!(written.insert(name.to_string()), "duplicate dump {name}");
        std::fs::write(dir.join(format!("{name}.json")), value.to_string()).unwrap();
    };
    write("finish-only", response_schema::FINISH);
    write("choice-v13", response_schema::CHOICE);

    // Not complete: acquisition only.
    let f = Fixture::new();
    let pending = f.derive(&f.ledger, &[]).unwrap();
    let partial = f
        .derive(
            &f.ledger,
            &[f.source_attempt(AttemptOutcome::Unavailable {
                reason: UnavailableReason::PinnedTargetAbsent,
            })],
        )
        .unwrap();
    for (name, choices) in [("pending", &pending), ("partial", &partial)] {
        assert!(!choices.attempts_complete() && !choices.all_acquired());
        let schema = selected(&f, choices, &f.ledger);
        assert_eligible_shape(&schema, choices, &f.ledger);
        write(name, &schema.to_string());
    }

    // Complete, both obligations attempted. Each state is dumped as the
    // ordinary schema, the eligible schema with citations, the eligible schema
    // after the current ledger loses its citations, and (where exploration
    // remains) the unknown-ID recovery chain production uses.
    let unknown = |choices: &AcquisitionChoices| ActionRefusalFeedback {
        query_budget: None,
        reason: ActionRefusalReason::UnknownAnchor,
        patch_rejection: None,
        invocation_ordinal: 1,
        action_hash: "a".repeat(64),
        choices_hash: choices.stamp_hash().unwrap(),
    };
    let mut dump_complete = |name: &str, f: &Fixture, records: &[AttemptRecord], all: bool| {
        let choices = f.derive(&f.ledger, records).unwrap();
        assert!(choices.attempts_complete());
        assert_eq!(choices.all_acquired(), all);
        assert!(!f.ledger.citations.is_empty());
        let request = &f.contract.request_hash;
        let ordinary =
            response_schema::select_acquisition(Choice, &choices, &f.ledger, request).unwrap();
        write(&format!("{name}-ordinary"), ordinary.as_deref().unwrap());
        let schema = selected(f, &choices, &f.ledger);
        assert_eligible_shape(&schema, &choices, &f.ledger);
        write(&format!("{name}-cited"), &schema.to_string());
        if !all {
            let feedback = unknown(&choices);
            let recovery = response_schema::select_acquisition_recovery(
                Choice,
                &choices,
                &f.ledger,
                request,
                Some(&feedback),
            )
            .unwrap();
            assert_ne!(
                recovery, ordinary,
                "unknown-ID feedback must narrow selection"
            );
            let schema: serde_json::Value = serde_json::from_str(
                &response_schema::select_eligible_acquisition(
                    recovery, Choice, &choices, &f.ledger,
                )
                .unwrap()
                .unwrap(),
            )
            .unwrap();
            assert_eligible_shape(&schema, &choices, &f.ledger);
            write(&format!("{name}-recovery"), &schema.to_string());
        }
        // The current ledger without evidence (citations and the facts and
        // receipts they back); the attempts keep their historical ledger.
        let mut uncited = f.ledger.clone();
        uncited.citations.clear();
        uncited.selected_facts.clear();
        uncited.receipt_references.clear();
        let choices = f.derive(&uncited, records).unwrap();
        assert!(choices.attempts_complete() && !choices.all_acquired());
        let schema = selected(f, &choices, &uncited);
        assert_eligible_shape(&schema, &choices, &uncited);
        write(&format!("{name}-citations-lost"), &schema.to_string());
        ordinary
    };
    let source = f.source_attempt(AttemptOutcome::Unavailable {
        reason: UnavailableReason::PinnedTargetAbsent,
    });
    let unavailable = AttemptOutcome::Unavailable {
        reason: UnavailableReason::InventoryExceedsMaxBytes,
    };
    let inventory = inventory_attempt(&f, &source, unavailable.clone());
    dump_complete("completed-unavailable", &f, &[source, inventory], false);
    let source = f.source_attempt(AttemptOutcome::SourceAcquired {
        citation_id: "evidence-1".into(),
    });
    let inventory = inventory_attempt(&f, &source, unavailable);
    let mixed_ordinary = dump_complete("completed-mixed", &f, &[source, inventory], false);

    let mut acquired = Fixture::new();
    let Some(Obligation::Inventory { target, .. }) = acquired
        .contract
        .obligations
        .obligations
        .iter()
        .find(|o| matches!(o, Obligation::Inventory { .. }))
        .cloned()
    else {
        unreachable!()
    };
    acquired
        .ledger
        .coverage_reads
        .push(InvestigationCoverageReadManifest {
            request: InvestigationCoverageRead {
                context_id: target.context_id.clone(),
                repo: target.repo.clone(),
                inventory_hash: target.inventory_hash.clone(),
                max_bytes: MAX_QUERY_BYTES,
            },
            response_hash: "a".repeat(64),
            response_bytes: 1,
        });
    let source = acquired.source_attempt(AttemptOutcome::SourceAcquired {
        citation_id: "evidence-1".into(),
    });
    let inventory = inventory_attempt(
        &acquired,
        &source,
        AttemptOutcome::InventoryAcquired { read_index: 0 },
    );
    dump_complete(
        "completed-all-acquired",
        &acquired,
        &[source, inventory],
        true,
    );

    // Genuinely all acquired without citations: the inventory-only state.
    let mut f = Fixture::new();
    let mut record = f.source_attempt(AttemptOutcome::InventoryAcquired { read_index: 0 });
    f.contract
        .obligations
        .obligations
        .retain(|o| matches!(o, Obligation::Inventory { .. }));
    f.contract.obligations_hash = f
        .contract
        .obligations
        .fingerprint_with_policy(POLICY)
        .unwrap();
    f.metadata.obligations_hash = f.contract.obligations_hash.clone();
    f.ledger = f.empty();
    let Obligation::Inventory {
        obligation_id,
        target,
    } = &f.contract.obligations.obligations[0]
    else {
        unreachable!()
    };
    f.ledger
        .coverage_reads
        .push(InvestigationCoverageReadManifest {
            request: InvestigationCoverageRead {
                context_id: target.context_id.clone(),
                repo: target.repo.clone(),
                inventory_hash: target.inventory_hash.clone(),
                max_bytes: MAX_QUERY_BYTES,
            },
            response_hash: "a".repeat(64),
            response_bytes: 1,
        });
    record.obligation_id = obligation_id.clone();
    record.obligations_hash = f.contract.obligations_hash.clone();
    record.read = ReadIntent::Inventory {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: MAX_QUERY_BYTES,
    };
    record.input_ledger_hash = f.ledger.fingerprint_with_policy(POLICY).unwrap();
    record.attempt_id = record.content_id().unwrap();
    let choices = f.derive(&f.ledger, &[record]).unwrap();
    assert!(choices.attempts_complete() && choices.all_acquired());
    assert!(f.ledger.citations.is_empty());
    let schema = selected(&f, &choices, &f.ledger);
    assert_eligible_shape(&schema, &choices, &f.ledger);
    write("completed-all-acquired-uncited", &schema.to_string());

    // Sizing only, never an actual call schema: the reservation for the
    // genuine completed (mixed) ordinary schema plus the fixture citations.
    let fixture = Fixture::new();
    let prompt = serde_json::json!({"citations": fixture.ledger.citations});
    write(
        "reserve-sizing-completed-mixed",
        &response_schema::reserve_eligible_acquisition(mixed_ordinary, Choice, &prompt)
            .unwrap()
            .unwrap(),
    );

    // Largest action menu exercised by the contract-bound test.
    let mut f = Fixture::new();
    f.contract
        .obligations
        .obligations
        .retain(|o| matches!(o, Obligation::Inventory { .. }));
    for index in 0..20 {
        let mut target = f.target.clone();
        target.fact = TaskFactKey::Node {
            id: format!("node-{index}-{}", "quoted-\"-\\-é".repeat(15)),
        };
        target.target_id = target.content_id().unwrap();
        f.contract
            .obligations
            .obligations
            .push(identified(Obligation::Source {
                obligation_id: String::new(),
                target: Box::new(target),
            }));
    }
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
    let largest = f.derive(&ledger, &[]).unwrap();
    assert!(!largest.attempts_complete());
    let schema = selected(&f, &largest, &ledger);
    assert_eligible_shape(&schema, &largest, &ledger);
    write("largest-menu", &schema.to_string());
    assert_eq!(written.len(), 18);
}
