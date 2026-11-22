use super::*;
use context_hub::{
    ContextSnapshot, QueryRequest, QueryScope,
    coverage::{CoverageContext, CoverageQualification, QualifiedRequest, ScopedRepo},
};
use serde_json::json;
use std::sync::Arc;

const REQUEST_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn fixture(repositories: usize) -> (CoverageContext, InvestigationInputLedger) {
    let snapshot = Arc::new(ContextSnapshot::new(vec![], vec![]).unwrap());
    let scopes = (0..repositories)
        .map(|index| ScopedRepo {
            repo_key: format!("fixture/{index:03}-{}", "r".repeat(100)),
            registered_source_id: None,
            config_hash: "query-history-fixture".into(),
            layer_hints: None,
        })
        .collect::<Vec<_>>();
    let qualification = CoverageQualification::new(&scopes, &[]).unwrap();
    let context = CoverageContext::from_shared_snapshot(snapshot, qualification).unwrap();
    let ledger = InvestigationInputLedger {
        schema_version: 2,
        coverage: Some(context.durable_proof().unwrap()),
        coverage_reads: vec![],
        artifact_reads: vec![],
        graph_snapshot_id: context.snapshot_id().into(),
        scope_snapshot_id: context.snapshot_id().into(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    ledger
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    (context, ledger)
}

fn query(labels: Vec<String>) -> InvestigationQuery {
    InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        labels,
        max_facts: MAX_QUERY_FACTS,
        max_bytes: MAX_QUERY_BYTES,
        cursor: None,
    }
}

fn acquire(
    context: &CoverageContext,
    ledger: &mut InvestigationInputLedger,
    pages: &mut Vec<String>,
    query: InvestigationQuery,
) {
    query.validate().unwrap();
    let response = context
        .query(QualifiedRequest {
            request: QueryRequest {
                scope: QueryScope::All,
                kind: None,
                labels: query.labels.clone(),
                max_facts: query.max_facts,
                max_bytes: query.max_bytes,
                cursor: None,
            },
            context_id: Some(context.context_id().into()),
        })
        .unwrap();
    assert!(response.response.facts.is_empty());
    let raw = json!({"context":response,"evidence_options":[]}).to_string();
    append(ledger, pages, query, raw, 0);
}

fn append(
    ledger: &mut InvestigationInputLedger,
    pages: &mut Vec<String>,
    query: InvestigationQuery,
    raw: String,
    returned_facts: usize,
) {
    ledger.revision += 1;
    ledger.queries.push(InvestigationQueryManifest {
        query,
        response_hash: content_hash(raw.as_bytes()),
        response_bytes: raw.len(),
        returned_facts,
        total_selected: returned_facts,
        has_more: false,
    });
    pages.push(raw);
    ledger
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
}

fn project(ledger: &InvestigationInputLedger, pages: &[String]) -> ProgressiveInvestigationContext {
    ProgressiveInvestigationContext::prepare(REQUEST_HASH, ledger, pages, &INVESTIGATION_V2_POLICY)
        .unwrap()
}

#[test]
fn query_history_initial_and_repeated_or_distinct_selectors_have_one_full_view() {
    // AC-0286: history size does not depend on exact-selector deduplication.
    for distinct in [false, true] {
        let (context, mut ledger) = fixture(1);
        let mut pages = vec![];
        let initial = project(&ledger, &pages);
        assert!(initial.query_history.prior_queries.is_empty());
        assert!(initial.query_history.latest_query.is_none());
        for index in 0..INVESTIGATION_V2_POLICY.limits().tool_actions {
            let suffix = if distinct {
                format!("{index:02}")
            } else {
                "xx".into()
            };
            let labels = vec![format!("{}{}", "\\".repeat(126), suffix); 4];
            acquire(&context, &mut ledger, &mut pages, query(labels));
            let projected = project(&ledger, &pages);
            let history = &projected.query_history;
            assert_eq!(history.prior_queries.len(), index as usize);
            assert_eq!(
                history.latest_query.as_ref().unwrap().selection,
                ledger.queries.last().unwrap().query
            );
            history
                .validate_registry(REQUEST_HASH, &ledger, &INVESTIGATION_V2_POLICY)
                .unwrap();
            assert!(
                history.transport_bytes().unwrap()
                    <= InvestigationQueryHistory::transport_upper_bound_with_policy(
                        &INVESTIGATION_V2_POLICY
                    )
                    .unwrap()
            );
            let value = serde_json::to_value(history).unwrap();
            for reference in value["prior_queries"].as_array().unwrap() {
                assert_eq!(reference.as_object().unwrap().len(), 2);
                assert!(reference.get("selection").is_none());
                assert_eq!(reference["query_id"].as_str().unwrap().len(), 64);
            }
        }
        assert!(
            InvestigationQueryBudget::from_ledger(&ledger)
                .unwrap()
                .consumed_bytes
                < MAX_TOTAL_QUERY_BYTES
        );
    }
}

#[test]
fn query_history_does_not_pass_through_oversized_legacy_compact_intermediate() {
    // AC-0286: repeated public qualification and selectors may overflow the old
    // intermediate even though the exact new representation is small.
    let (context, mut ledger) = (1..256)
        .find_map(|repositories| {
            let (context, ledger) = fixture(repositories);
            let bytes = serde_json::to_vec(context.coverage()).unwrap().len();
            (21_000..23_000)
                .contains(&bytes)
                .then_some((context, ledger))
        })
        .expect("qualification size fixture");
    let mut pages = vec![];
    for _ in 0..4 {
        acquire(
            &context,
            &mut ledger,
            &mut pages,
            query(vec!["\u{1}".repeat(128); 16]),
        );
    }
    assert!(matches!(
        CompactInvestigationContext::prepare_with_policy(&ledger, &pages, &INVESTIGATION_V2_POLICY),
        Err(InvestigationError::LimitExceeded)
    ));
    let projected = project(&ledger, &pages);
    assert!(serde_json::to_vec(&projected).unwrap().len() < MAX_INPUT_BYTES);
    assert_eq!(projected.query_history.prior_queries.len(), 3);
    assert!(projected.facts.is_empty());
}

#[test]
fn query_history_replay_identity_binds_request_context_index_selector_and_response() {
    // AC-0286: a provider cannot resolve a handle against another task basis.
    let (context, mut ledger) = fixture(1);
    let mut pages = vec![];
    for _ in 0..2 {
        acquire(
            &context,
            &mut ledger,
            &mut pages,
            query(vec!["missing".into()]),
        );
    }
    let reference =
        QueryHistoryReference::from_ledger(REQUEST_HASH, &ledger, 0, &INVESTIGATION_V2_POLICY)
            .unwrap();
    assert_eq!(
        reference
            .resolve(REQUEST_HASH, &ledger, &INVESTIGATION_V2_POLICY)
            .unwrap(),
        ledger.queries[0].query
    );
    assert!(
        reference
            .resolve(&"b".repeat(64), &ledger, &INVESTIGATION_V2_POLICY)
            .is_err()
    );
    assert!(
        reference
            .resolve("not-a-request-hash", &ledger, &INVESTIGATION_V2_POLICY)
            .is_err()
    );
    let mut changed = reference.clone();
    changed.query_index = 1;
    assert!(
        changed
            .resolve(REQUEST_HASH, &ledger, &INVESTIGATION_V2_POLICY)
            .is_err()
    );
    changed.query_index = usize::MAX;
    assert!(
        changed
            .resolve(REQUEST_HASH, &ledger, &INVESTIGATION_V2_POLICY)
            .is_err()
    );
    let mut changed = ledger.clone();
    changed.queries[0].query.max_facts = 1;
    assert!(
        reference
            .resolve(REQUEST_HASH, &changed, &INVESTIGATION_V2_POLICY)
            .is_err()
    );
    let mut changed = ledger.clone();
    changed.queries[0].response_hash = "b".repeat(64);
    assert!(
        reference
            .resolve(REQUEST_HASH, &changed, &INVESTIGATION_V2_POLICY)
            .is_err()
    );
    let (_, other_context) = fixture(2);
    let mut changed = ledger.clone();
    changed.coverage = other_context.coverage;
    changed
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    assert!(
        reference
            .resolve(REQUEST_HASH, &changed, &INVESTIGATION_V2_POLICY)
            .is_err()
    );
    assert!(
        reference
            .resolve(REQUEST_HASH, &ledger, &INVESTIGATION_V1_POLICY)
            .is_err()
    );
    // Full selector bytes survive saved-ledger round trips; no page, filesystem
    // or graph reconstruction is needed merely to resolve a replay selection.
    let saved: InvestigationInputLedger =
        decode_record(&serde_json::to_string(&ledger).unwrap()).unwrap();
    assert_eq!(
        reference
            .resolve(REQUEST_HASH, &saved, &INVESTIGATION_V2_POLICY)
            .unwrap(),
        ledger.queries[0].query
    );
}

#[test]
fn query_history_registry_rejects_changed_missing_reordered_and_extra_references() {
    // AC-0286: supplied history order and latest selection bind the full ledger.
    let (context, mut ledger) = fixture(1);
    let mut pages = vec![];
    for index in 0..3 {
        acquire(
            &context,
            &mut ledger,
            &mut pages,
            query(vec![format!("missing-{index}")]),
        );
    }
    let original = project(&ledger, &pages).query_history;
    let mut cases = vec![];
    let mut changed = original.clone();
    changed.prior_queries.swap(0, 1);
    cases.push(changed);
    let mut changed = original.clone();
    changed.prior_queries.pop();
    cases.push(changed);
    let mut changed = original.clone();
    changed.prior_queries.push(changed.prior_queries[0].clone());
    cases.push(changed);
    let mut changed = original.clone();
    changed.registry_hash = "b".repeat(64);
    cases.push(changed);
    let mut changed = original.clone();
    changed.latest_query.as_mut().unwrap().selection.max_bytes = 1;
    cases.push(changed);
    let mut changed = original.clone();
    changed.latest_query = None;
    cases.push(changed);
    for changed in cases {
        assert!(
            changed
                .validate_registry(REQUEST_HASH, &ledger, &INVESTIGATION_V2_POLICY)
                .is_err()
        );
    }
}

#[test]
fn query_history_preserves_all_complete_fact_and_option_representations() {
    // AC-0286: query-history omission cannot narrow acquired graph inventories.
    let (_, mut ledger) = fixture(1);
    let selected = super::super::tests::ledger();
    ledger.selected_facts = selected.selected_facts;
    ledger.citations = selected.citations;
    let fact = ledger.selected_facts[0].fact.clone();
    let mut pages = vec![];
    for note in ["first", "different", "first"] {
        let raw = json!({"context":{
            "context_id":ledger.coverage.as_ref().unwrap().context_id,
            "coverage":ledger.coverage.as_ref().unwrap().qualification,
            "response":{"snapshot_id":ledger.scope_snapshot_id,"view":"recovered_graph","total_selected":1,
                "facts":[{"reference":fact,"properties":{"note":note,"unknown_fact_property":[1,2,3]}}],"next_cursor":null}},
            "evidence_options":[{"fact":fact,"origin":"working_tree_unverified","ranges":[],"unknown_option_property":note}]}).to_string();
        append(&mut ledger, &mut pages, query(vec![]), raw, 1);
    }
    let projected = project(&ledger, &pages);
    assert_eq!(projected.facts.len(), 2);
    assert_eq!(projected.evidence_options.len(), 2);
    assert_eq!(
        projected.facts[0]["properties"]["unknown_fact_property"],
        json!([1, 2, 3])
    );
    assert_eq!(
        projected
            .query_history
            .latest_query
            .as_ref()
            .unwrap()
            .context
            .fact_indexes,
        vec![0]
    );
    assert_eq!(
        projected
            .query_history
            .latest_query
            .as_ref()
            .unwrap()
            .evidence_option_indexes,
        vec![0]
    );
}

#[test]
fn query_history_rejects_unknown_dropped_metadata_and_rebound_response_bytes() {
    // AC-0286: fields that the new closed latest view cannot preserve fail closed.
    let (context, mut ledger) = fixture(1);
    let mut pages = vec![];
    acquire(&context, &mut ledger, &mut pages, query(vec![]));
    for location in [
        "envelope",
        "qualification",
        "response",
        "view",
        "missing_cursor",
    ] {
        let mut page: Value = serde_json::from_str(&pages[0]).unwrap();
        match location {
            "envelope" => page["future_metadata"] = true.into(),
            "qualification" => page["context"]["future_metadata"] = true.into(),
            "response" => page["context"]["response"]["future_metadata"] = true.into(),
            "view" => page["context"]["response"]["view"] = "future_view".into(),
            _ => {
                page["context"]["response"]
                    .as_object_mut()
                    .unwrap()
                    .remove("next_cursor");
            }
        }
        let raw = page.to_string();
        let mut rebound = ledger.clone();
        rebound.queries[0].response_hash = content_hash(raw.as_bytes());
        rebound.queries[0].response_bytes = raw.len();
        assert!(
            ProgressiveInvestigationContext::prepare(
                REQUEST_HASH,
                &rebound,
                &[raw],
                &INVESTIGATION_V2_POLICY
            )
            .is_err()
        );
    }
    let raw = format!("{} ", pages[0]);
    assert!(
        ProgressiveInvestigationContext::prepare(
            REQUEST_HASH,
            &ledger,
            &[raw],
            &INVESTIGATION_V2_POLICY
        )
        .is_err()
    );
}

#[test]
fn query_history_transport_bound_covers_existing_selector_field_limits() {
    // AC-0286: this is a conservative component bound, never a new quota or a
    // claim that source, inventory, parent history and the whole payload fit.
    let upper =
        InvestigationQueryHistory::transport_upper_bound_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
    eprintln!("query history derived nested transport upper bound: {upper}");
    assert!(upper < MAX_INPUT_BYTES);
    let (context, mut ledger) = fixture(1);
    let mut pages = vec![];
    for index in 0..56 {
        let mut selected = query(vec![]);
        if index == 55 {
            selected = InvestigationQuery {
                scope: InvestigationScope::Neighborhood {
                    anchor: "\u{1}".repeat(1024),
                    hops: 3,
                },
                kind: Some(InvestigationFactKind::Edge),
                labels: vec!["\u{1}".repeat(128); 16],
                max_facts: MAX_QUERY_FACTS,
                max_bytes: MAX_QUERY_BYTES,
                cursor: Some(InvestigationQueryCursor {
                    snapshot_id: ledger.scope_snapshot_id.clone(),
                    selection_id: "\u{1}".repeat(128),
                    offset: 50_000,
                }),
            };
        }
        // The raw service result remains an empty-page fixture; this test
        // bounds the admitted selector's typed fields, not graph reachability.
        let response = context
            .query(QualifiedRequest {
                request: QueryRequest::default(),
                context_id: Some(context.context_id().into()),
            })
            .unwrap();
        append(
            &mut ledger,
            &mut pages,
            selected,
            json!({"context":response,"evidence_options":[]}).to_string(),
            0,
        );
    }
    let history = project(&ledger, &pages).query_history;
    assert!(history.transport_bytes().unwrap() <= upper);
    assert_eq!(history.prior_queries.len(), 55);
    assert!(
        InvestigationQueryHistory::transport_upper_bound_with_policy(&INVESTIGATION_V1_POLICY)
            .is_err()
    );
}

// AC-0286: batching preserves saved history bytes and fail-closed errors,
// including the historical zero-query legacy-ledger path.
#[test]
fn disclosed_history_batch_matches_per_reference_validation() {
    fn prior(
        hash: &str,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<DisclosedQueryHistory, InvestigationError> {
        let refs = (0..ledger.queries.len())
            .map(|index| QueryHistoryReference::from_ledger(hash, ledger, index, policy))
            .collect::<Result<Vec<_>, _>>()?;
        if !crate::source_basis::hash(hash) || policy.profile() != INVESTIGATION_V2_POLICY.profile()
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(DisclosedQueryHistory {
            request_hash: hash.into(),
            registry_hash: super::query_history::registry_hash(hash, ledger, policy, &refs)?,
            query_ids: refs.into_iter().map(|item| item.query_id).collect(),
        })
    }
    for count in [0, 1, 7] {
        let (context, mut basis) = fixture(1);
        let mut pages = vec![];
        for _ in 0..count {
            acquire(
                &context,
                &mut basis,
                &mut pages,
                query(vec!["missing".into()]),
            );
        }
        let mut variants = vec![basis.clone()];
        let mut legacy = basis.clone();
        legacy.schema_version = 1;
        legacy.coverage = None;
        variants.push(legacy);
        let mut corrupt = basis.clone();
        corrupt.scope_snapshot_id = "corrupt".into();
        variants.push(corrupt);
        let mut missing = basis.clone();
        missing.coverage = None;
        variants.push(missing);
        let mut oversized = basis.clone();
        oversized.supplied_history_bytes = 16 * 1024 + 1;
        variants.push(oversized);
        if count != 0 {
            let mut malformed = basis.clone();
            malformed.queries[0].response_hash = "not-a-hash".into();
            variants.push(malformed);
        }
        for ledger in variants {
            for hash in [REQUEST_HASH, "invalid"] {
                for policy in [&INVESTIGATION_V1_POLICY, &INVESTIGATION_V2_POLICY] {
                    let before = prior(hash, &ledger, policy)
                        .map(|value| serde_json::to_vec(&value).unwrap())
                        .map_err(|error| format!("{error:?}"));
                    let after = DisclosedQueryHistory::from_ledger(hash, &ledger, policy)
                        .map(|value| serde_json::to_vec(&value).unwrap())
                        .map_err(|error| format!("{error:?}"));
                    assert_eq!(after, before, "queries={count}, hash={hash}");
                }
            }
        }
    }
}
