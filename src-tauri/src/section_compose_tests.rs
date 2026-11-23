//! AC-0862 (T-0862): a section is composed in one bounded call from the
//! section D1 compiles and the facts it cites, and staged for review.
use super::*;
use agents::section_draft::ClaimKind;
use core_graph::{FactKey, GraphStore};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use llm::bounded::{
    AuthorizedBoundedCompletion, BoundedCallError, BoundedCompletion, ProviderProfile,
};
use llm::{Embedding, ProviderCaps, ProviderError};
use serde_json::json;
use std::sync::Mutex;

fn capability(id: &str, name: &str) -> Node {
    let prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        vec![EvidenceRef {
            repo: "fixture/repo".into(),
            path: "src/orders/app.ts".into(),
            byte_start: 0,
            byte_end: 8,
            commit_sha: "fixed".into(),
        }],
        "fixture",
        id.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: "Capability".into(),
        props: json!({ "name": name, "prov": prov }),
    }
}

fn id(node: &str) -> String {
    spec::citation_id(&FactKey::Node { id: node.into() })
}

/// A local provider that records the prompt and answers with `reply`.
pub(crate) struct Scripted {
    pub(crate) reply: String,
    pub(crate) prompts: Mutex<Vec<String>>,
}

impl LlmProvider for Scripted {
    fn id(&self) -> &str {
        "local-scripted"
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        Err(ProviderError::Unsupported("fixture embeddings"))
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        Ok(ProviderProfile {
            protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
            provider_id: self.id().into(),
            locality: Locality::Local,
            endpoint_id: "http://127.0.0.1:11434/api/chat".into(),
            requested_model: "scripted-model".into(),
        })
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        self.prompts
            .lock()
            .unwrap()
            .push(request.payload().prompt.clone());
        Ok(BoundedCompletion {
            text: self.reply.clone(),
            requested_model: "scripted-model".into(),
            response_model: None,
            provider_request_id: None,
            usage: None,
            request_bytes: 1,
            response_bytes: 1,
        })
    }
}

fn state() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::registered_source_tests::app_state(dir.path());
    {
        let mut graph = state.graph.lock().unwrap();
        graph
            .put_node(&capability("cap:checkout", "Checkout"))
            .unwrap();
        graph.put_node(&capability("cap:refund", "Refund")).unwrap();
    }
    (dir, state)
}

fn reply() -> String {
    json!({"claims": [
        {"statement": "Checkout takes an order and confirms it.",
         "claim_kind": "implemented_behavior", "citation_ids": [id("cap:checkout")]},
        {"statement": "Refunds are served by src/orders/app.ts.",
         "claim_kind": "implemented_behavior", "citation_ids": [id("cap:refund")]},
        {"statement": "Cites a fact the section does not print.",
         "claim_kind": "inferred_interpretation", "citation_ids": [id("cap:other")]},
    ]})
    .to_string()
}

#[test]
fn a_section_is_composed_from_its_compiled_text_and_staged_for_review() {
    let (_dir, state) = state();
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let provider = Scripted {
        reply: reply(),
        prompts: Mutex::new(Vec::new()),
    };
    let composed = compose(
        &state,
        &provider,
        &prepared,
        None,
        &|| CallDirective::Continue,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(composed.left_out, 1);

    // The composer read the compiled section and its facts, never a path.
    let prompt = provider.prompts.lock().unwrap().remove(0);
    assert!(prompt.contains("<section>\n## 1.3 Capabilities"));
    assert!(prompt.contains(&format!("{} | Capability | Checkout |", id("cap:checkout"))));
    assert!(!prompt.contains("src/orders/app.ts"));

    let (_, job) = crate::start_job(&state, "section-compose:D1:1.3:system:local").unwrap();
    let outcome = stage(&state, &prepared, composed, job.id()).unwrap();
    // The claim naming its cited fact's file is not staged; the other is.
    assert_eq!((outcome.claims, outcome.left_out), (1, 2));
    let DraftingOutcome::Staged { draft_id } = outcome.outcome else {
        panic!("staged: {outcome:?}");
    };
    let store = state.drafts.lock().unwrap();
    let stored = store.for_job(job.id()).unwrap().unwrap();
    assert_eq!(stored.draft_id, draft_id);
    assert_eq!(stored.origin.role, SECTION_COMPOSER_ROLE);
    assert_eq!(
        stored.origin.role_definition,
        section_compose::definition_fingerprint()
    );
    assert_eq!(
        stored.draft.claims[0].statement,
        "Checkout takes an order and confirms it."
    );
}

/// AC-1313 (T-1313): §1.1 Purpose and scope prints no fact of its own; it
/// is composed from the facts §1.2 and the §1.3 overview print, and staged
/// (#2319).
#[test]
fn purpose_and_scope_is_composed_from_the_sections_it_summarises() {
    let (_dir, state) = state();
    let prepared = prepare(&state, "1.0", "D1", "1.1", "system").unwrap();
    let provider = Scripted {
        reply: reply(),
        prompts: Mutex::new(Vec::new()),
    };
    let composed = compose(
        &state,
        &provider,
        &prepared,
        None,
        &|| CallDirective::Continue,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(composed.left_out, 1);
    let prompt = provider.prompts.lock().unwrap().remove(0);
    assert!(prompt.contains("<section>\n## 1.1 Purpose and scope"));
    assert!(prompt.contains("## 1.3 Capabilities"));
    assert!(prompt.contains(&format!("{} | Capability | Checkout |", id("cap:checkout"))));
    assert!(prompt.contains("not a capability-by-capability listing"));
    let (_, job) = crate::start_job(&state, "section-compose:D1:1.1:system:local").unwrap();
    let outcome = stage(&state, &prepared, composed, job.id()).unwrap();
    assert_eq!((outcome.claims, outcome.left_out), (1, 2));
    assert!(matches!(outcome.outcome, DraftingOutcome::Staged { .. }));
}

/// AC-1313: the capability areas §1.3 prints are citable facts of the
/// composition, so §1.1 can cite its main areas (#2319).
#[test]
fn the_capability_areas_are_citable_beside_capabilities_and_actors() {
    let (_dir, state) = crate::area_briefing::tests::state();
    let compiled = compile(&state, "1.0", "D1", "1.1", "system").unwrap();
    let areas: Vec<&str> = compiled
        .derived
        .iter()
        .filter_map(|fact| match &fact.item {
            core_graph::overlay::OverlayItem::Node(node) if node.label == "Subsystem" => {
                Some(node.id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        areas,
        ["subsystem:cluster:alias", "subsystem:cluster:mailbox"]
    );
    assert_eq!(compiled.overlay.len(), 2);
}

#[test]
fn a_composition_is_refused_off_the_system_or_an_area_scope_or_after_the_graph_changed() {
    let (_dir, state) = state();
    // A section not arranged by area is composed for the whole system only.
    let scoped = prepare(&state, "1.0", "D1", "1.2", &id("cap:checkout"));
    assert_eq!(
        scoped.err().as_deref(),
        Some("composition_system_scope_only")
    );
    // One arranged by area takes an area's scope, never another subject's
    // (#2083).
    let scoped = prepare(&state, "1.0", "D1", "1.3", &id("cap:checkout"));
    assert_eq!(scoped.err().as_deref(), Some("area_not_found"));

    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let provider = Scripted {
        reply: reply(),
        prompts: Mutex::new(Vec::new()),
    };
    let composed = compose(
        &state,
        &provider,
        &prepared,
        None,
        &|| CallDirective::Continue,
        None,
    )
    .unwrap()
    .unwrap();
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&capability("cap:late", "Late"))
        .unwrap();
    let (_, job) = crate::start_job(&state, "section-compose:D1:1.3:system:local").unwrap();
    let outcome = stage(&state, &prepared, composed, job.id()).unwrap();
    assert_eq!(outcome, ComposeOutcome::none("graph_changed"));
}

#[test]
fn a_reply_that_is_not_the_schema_stages_nothing() {
    let (_dir, state) = state();
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let provider = Scripted {
        reply: "I drafted the section: Checkout takes orders.".into(),
        prompts: Mutex::new(Vec::new()),
    };
    let decoded = compose(
        &state,
        &provider,
        &prepared,
        None,
        &|| CallDirective::Continue,
        None,
    )
    .unwrap();
    assert_eq!(decoded.err(), Some("reply_not_the_schema"));
}

/// AC-0980 (T-0980): the gated workflow drafts §1.5 only, and only on a
/// local provider.
#[test]
fn a_gated_composition_runs_on_section_1_5_and_a_local_provider_only() {
    let (_dir, state) = state();
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let provider = Scripted {
        reply: reply(),
        prompts: Mutex::new(Vec::new()),
    };
    let continue_ = || CallDirective::Continue;
    assert_eq!(
        compose_gated(&state, &provider, &prepared, &continue_, 1),
        Err("gated_composition_section_unsupported".into())
    );
    assert!(provider.prompts.lock().unwrap().is_empty());

    struct Cloud;
    impl LlmProvider for Cloud {
        fn id(&self) -> &str {
            "cloud"
        }
        fn locality(&self) -> Locality {
            Locality::Cloud
        }
        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }
        fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Err(ProviderError::Unsupported("fixture"))
        }
    }
    let mut model = HostModel {
        state: &state,
        provider: &Cloud,
        check: &continue_,
        job_id: 1,
    };
    assert_eq!(
        model.complete(&prepared.action),
        Err("gated_composition_local_only".into())
    );
}

/// AC-1314 (T-1314): a claim naming a route that no fact it cites shows is
/// left out at staging (#2328).
#[test]
fn a_claim_naming_a_route_its_citations_do_not_show_is_not_staged() {
    let (_dir, state) = state();
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&capability("cap:orders", "Place orders at /api/orders"))
        .unwrap();
    let mut endpoint = capability("endpoint:POST /api/orders", "POST /api/orders");
    endpoint.label = "Endpoint".into();
    state.graph.lock().unwrap().put_node(&endpoint).unwrap();
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let provider = Scripted {
        reply: json!({"claims": [
            {"statement": "Shoppers place orders through POST /api/orders.",
             "claim_kind": "implemented_behavior",
             "citation_ids": [id("cap:orders"), id("endpoint:POST /api/orders")]},
            {"statement": "Shoppers confirm a checkout through POST /api/checkout.",
             "claim_kind": "implemented_behavior", "citation_ids": [id("cap:checkout")]},
        ]})
        .to_string(),
        prompts: Mutex::new(Vec::new()),
    };
    let mut composed = compose(
        &state,
        &provider,
        &prepared,
        None,
        &|| CallDirective::Continue,
        None,
    )
    .unwrap()
    .unwrap();
    // §1.3 prints no Endpoint, so the reply could not cite one; a claim
    // that cites it, as a section printing it would, is staged (#2349).
    composed.finish.claims.insert(
        0,
        Claim {
            statement: "Shoppers place orders through POST /api/orders.".into(),
            kind: agents::section_draft::ClaimKind::Implemented,
            citations: vec![id("cap:orders"), id("endpoint:POST /api/orders")],
        },
    );
    assert_eq!(composed.reasons["citation_not_citable"], 1);
    let (_, job) = crate::start_job(&state, "section-compose:D1:1.3:system:local").unwrap();
    let outcome = stage(&state, &prepared, composed, job.id()).unwrap();
    assert_eq!((outcome.claims, outcome.left_out), (1, 2), "{outcome:?}");
    assert_eq!(outcome.left_out_reasons["route_not_cited"], 1);
    let store = state.drafts.lock().unwrap();
    let stored = store.for_job(job.id()).unwrap().unwrap();
    assert_eq!(
        stored.draft.claims[0].statement,
        "Shoppers place orders through POST /api/orders."
    );
}

/// AC-1317 (T-1317): a figure no cited fact shows, or a claim about
/// endpoints citing none, is left out, and the job records why (#2349).
#[test]
fn an_uncited_figure_or_endpoint_claim_is_left_out_with_its_reason() {
    let (_dir, state) = state();
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&capability("cap:orders", "Place orders"))
        .unwrap();
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let cites = [id("cap:orders")];
    let provider = Scripted {
        reply: json!({"claims": [
            {"statement": "Shoppers place up to 5 orders a day.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
            {"statement": "Orders are placed through dedicated endpoints.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
            {"statement": "Shoppers place orders for themselves.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
        ]})
        .to_string(),
        prompts: Mutex::new(Vec::new()),
    };
    let composed = compose(
        &state,
        &provider,
        &prepared,
        None,
        &|| CallDirective::Continue,
        None,
    )
    .unwrap()
    .unwrap();
    let (_, job) = crate::start_job(&state, "section-compose:D1:1.3:system:local").unwrap();
    let outcome = stage(&state, &prepared, composed, job.id()).unwrap();
    assert_eq!((outcome.claims, outcome.left_out), (1, 2), "{outcome:?}");
    assert_eq!(
        outcome.left_out_reasons,
        BTreeMap::from([("endpoint_not_cited", 1), ("number_not_cited", 1)])
    );
    assert_eq!(
        left_out_stage(&[&outcome]),
        "composed: 1 staged, 2 left out (endpoint_not_cited 1, number_not_cited 1)"
    );
    // A gated composition counts its gate drops, staged or not.
    assert_eq!(gated_reasons(3), BTreeMap::from([("gated_check", 3)]));
    assert!(gated_reasons(0).is_empty());
    // The reasons are part of what the command returns.
    let returned = serde_json::to_value(&outcome).unwrap();
    assert_eq!(returned["left_out_reasons"]["number_not_cited"], 1);
}

/// AC-1319 (T-1319): a claim asserting more than its cited facts show is
/// left out as `claim_not_supported`, and a paraphrase of them is staged
/// (#2371).
#[test]
fn a_claim_asserting_more_than_its_facts_show_is_left_out() {
    let (_dir, state) = state();
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&capability("cap:orders", "Place orders"))
        .unwrap();
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let cites = [id("cap:orders")];
    let provider = Scripted {
        reply: json!({"claims": [
            {"statement": "The mobile checkout screen syncs carts with the warehouse ledger.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
            {"statement": "Shoppers place orders for themselves.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
        ]})
        .to_string(),
        prompts: Mutex::new(Vec::new()),
    };
    let composed = compose(
        &state,
        &provider,
        &prepared,
        None,
        &|| CallDirective::Continue,
        None,
    )
    .unwrap()
    .unwrap();
    let (_, job) = crate::start_job(&state, "section-compose:D1:1.3:system:local").unwrap();
    let outcome = stage(&state, &prepared, composed, job.id()).unwrap();
    assert_eq!((outcome.claims, outcome.left_out), (1, 1), "{outcome:?}");
    assert_eq!(
        outcome.left_out_reasons,
        BTreeMap::from([("claim_not_supported", 1)])
    );
    let store = state.drafts.lock().unwrap();
    let stored = store.for_job(job.id()).unwrap().unwrap();
    assert_eq!(
        stored.draft.claims[0].statement,
        "Shoppers place orders for themselves."
    );
}

#[test]
fn a_system_composition_tries_each_excerpt_listed_then_unlisted() {
    // AC-1322 (T-1322, #2399): rules come before the listed ids.
    assert_eq!(
        attempts(2),
        vec![
            (Some(0), true),
            (Some(0), false),
            (Some(1), true),
            (Some(1), false),
            (None, true),
        ]
    );
    assert_eq!(attempts(0), vec![(None, true)]);
}

#[test]
fn a_rule_is_cited_with_the_capability_it_is_listed_under() {
    // AC-1324 (T-1324, #2416).
    let rules = BTreeMap::from([(
        "F-rule".to_owned(),
        BTreeSet::from(["Manage user (API)".to_owned()]),
    )]);
    let capabilities: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::from([
        ("Manage user (API)", BTreeSet::from(["F-user"])),
        ("Enter sudo (dashboard)", BTreeSet::from(["F-sudo"])),
    ]);
    let cites = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
    // The rule with its own capability, alone or beside others.
    assert!(rule_capability_cited(
        &cites(&["F-rule", "F-user"]),
        &rules,
        &capabilities
    ));
    assert!(rule_capability_cited(
        &cites(&["F-sudo", "F-rule", "F-user"]),
        &rules,
        &capabilities
    ));
    // The rule stated under another capability.
    assert!(!rule_capability_cited(
        &cites(&["F-sudo", "F-rule"]),
        &rules,
        &capabilities
    ));
    // No excerpt rule cited, or a rule whose capability is not citable.
    assert!(rule_capability_cited(
        &cites(&["F-sudo"]),
        &rules,
        &capabilities
    ));
    let orphan = BTreeMap::from([("F-rule".to_owned(), BTreeSet::from(["Gone".to_owned()]))]);
    // A rule listed under two capabilities is cited with either.
    let shared = BTreeMap::from([(
        "F-rule".to_owned(),
        BTreeSet::from([
            "Manage user (API)".to_owned(),
            "Enter sudo (dashboard)".to_owned(),
        ]),
    )]);
    assert!(rule_capability_cited(
        &cites(&["F-sudo", "F-rule"]),
        &shared,
        &capabilities
    ));
    assert!(rule_capability_cited(
        &cites(&["F-rule"]),
        &orphan,
        &capabilities
    ));
    // Capabilities are found by name among the citable facts.
    let facts = vec![ComposeFact {
        citation_id: "F-user".into(),
        label: "Capability".into(),
        name: Some("Manage user (API)".into()),
        tier: "Derived".into(),
        confidence: "Verified".into(),
    }];
    assert_eq!(
        capability_ids(&facts),
        BTreeMap::from([("Manage user (API)", BTreeSet::from(["F-user"]))])
    );
}

/// AC-1605 (#1748): `review_batches` halves a batch that does not fit, keeps
/// the batches in order, and gives up only on one claim that does not fit.
#[test]
fn review_batches_halve_until_each_fits() {
    let small = |batch: std::ops::Range<usize>| batch.len() <= 2;
    assert_eq!(review_batches(5, &small), Some(vec![0..2, 2..3, 3..5]));
    assert_eq!(
        review_batches(2, &small),
        Some(std::iter::once(0..2).collect())
    );
    assert_eq!(review_batches(3, &|_| false), None);
}

/// AC-1605 (#1748): claims too many for one review request are reviewed in
/// batches that each fit, and a claim a batch's majority judges an
/// overreach is left out as before.
#[test]
fn claims_over_the_review_ceiling_are_reviewed_in_batches() {
    let (_dir, state) = state();
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&capability("cap:orders", "Place orders"))
        .unwrap();
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let cites = [id("cap:orders")];
    let composer = Scripted {
        reply: json!({"claims": [
            {"statement": "Shoppers place orders for themselves.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
            {"statement": "Shoppers place orders from every channel there is.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
        ]})
        .to_string(),
        prompts: Mutex::new(Vec::new()),
    };
    let mut composed = compose(
        &state,
        &composer,
        &prepared,
        None,
        &|| CallDirective::Continue,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(screen(&prepared, &mut composed), None);
    assert_eq!(composed.finish.claims.len(), 2);
    // A ceiling that holds one claim's review but not both.
    let one = payload_bytes(&review_action(&prepared, &composed.finish.claims[..1]));
    let both = payload_bytes(&review_action(&prepared, &composed.finish.claims));
    assert!(one + 64 < both);
    let reviewer = Scripted {
        reply: json!({"verdicts": [{"claim": 1, "verdict": "overreach"}]}).to_string(),
        prompts: Mutex::new(Vec::new()),
    };
    let calls = Calls {
        state: &state,
        provider: &reviewer,
        check: &|| CallDirective::Continue,
        job_id: None,
        ceiling: one + 64,
    };
    // Under a ceiling no single claim fits, nothing is reviewed.
    let tiny = Calls {
        state: &state,
        provider: &reviewer,
        check: &|| CallDirective::Continue,
        job_id: None,
        ceiling: 1,
    };
    let mut untouched = composed.clone();
    assert_eq!(
        review_claims(&tiny, &prepared, &mut untouched).unwrap(),
        "review_too_large"
    );
    assert_eq!(untouched, composed);
    assert!(reviewer.prompts.lock().unwrap().is_empty());
    assert_eq!(
        review_claims(&calls, &prepared, &mut composed).unwrap(),
        "reviewed"
    );
    assert!(composed.finish.claims.is_empty());
    assert_eq!(composed.reasons["claim_overreaches"], 2);
    // Each batch asked the reviewer the usual number of times, about its own
    // claim alone.
    let prompts = reviewer.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 2 * agents::claim_review::REVIEW_SAMPLES);
    assert!(prompts[0].contains("1. Shoppers place orders for themselves."));
    assert!(!prompts[0].contains("every channel"));
    let last = &prompts[2 * agents::claim_review::REVIEW_SAMPLES - 1];
    assert!(last.contains("1. Shoppers place orders from every channel there is."));
}

/// AC-1325 (T-1325, #2419): on a local lane each claim the reviewer judges
/// an overreach is left out before staging; an unreadable review keeps the
/// claims.
#[test]
fn a_claim_the_reviewer_judges_an_overreach_is_left_out() {
    let (_dir, state) = state();
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&capability("cap:orders", "Place orders"))
        .unwrap();
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let cites = [id("cap:orders")];
    let composer = Scripted {
        reply: json!({"claims": [
            {"statement": "Shoppers place orders for themselves.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
            {"statement": "Shoppers place orders from every channel there is.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
            {"statement": "Shoppers place up to 17 orders a day.",
             "claim_kind": "implemented_behavior", "citation_ids": cites},
        ]})
        .to_string(),
        prompts: Mutex::new(Vec::new()),
    };
    // The staging checks run first: the claim stating an uncited number
    // keeps its own reason and is never reviewed.
    let composed = || {
        let mut composed = compose(
            &state,
            &composer,
            &prepared,
            None,
            &|| CallDirective::Continue,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(screen(&prepared, &mut composed), None);
        assert_eq!(composed.reasons["number_not_cited"], 1);
        composed
    };
    let reviewer = Scripted {
        reply: json!({"verdicts": [
            {"claim": 1, "verdict": "shown"}, {"claim": 2, "verdict": "overreach"},
        ]})
        .to_string(),
        prompts: Mutex::new(Vec::new()),
    };
    let (kept, how, _) = review(
        &state,
        &reviewer,
        &prepared,
        composed(),
        &|| CallDirective::Continue,
        None,
    )
    .unwrap();
    assert_eq!(how, "reviewed");
    assert_eq!(kept.finish.claims.len(), 1);
    assert_eq!(
        kept.finish.claims[0].statement,
        "Shoppers place orders for themselves."
    );
    assert_eq!((kept.left_out, kept.reasons["claim_overreaches"]), (2, 1));
    // The reviewer read both claims and the cited capability's text.
    // AC-1329 (T-1329): the same review is asked three times (#2458).
    let prompts = reviewer.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), agents::claim_review::REVIEW_SAMPLES);
    assert!(prompts.iter().all(|prompt| *prompt == prompts[0]));
    let prompt = prompts[0].clone();
    assert!(prompt.contains("2. Shoppers place orders from every channel there is."));
    assert!(!prompt.contains("17 orders"));
    assert!(prompt.contains(&format!("{}: ", id("cap:orders"))) && prompt.contains("Place orders"));
    // A reply that is not the schema's keeps every claim.
    let unreadable = Scripted {
        reply: "not a review".into(),
        prompts: Mutex::new(Vec::new()),
    };
    let (kept, how, _) = review(
        &state,
        &unreadable,
        &prepared,
        composed(),
        &|| CallDirective::Continue,
        None,
    )
    .unwrap();
    assert_eq!((how, kept.finish.claims.len()), ("review_unavailable", 2));
    // A failed review call is an unreadable sample, so no reply keeps every
    // claim; a cancelled job still fails (#2458).
    struct Failing;
    impl LlmProvider for Failing {
        fn id(&self) -> &str {
            "local-failing"
        }
        fn locality(&self) -> Locality {
            Locality::Local
        }
        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }
        fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Err(ProviderError::Unsupported("fixture"))
        }
    }
    let (kept, how, _) = review(
        &state,
        &Failing,
        &prepared,
        composed(),
        &|| CallDirective::Continue,
        None,
    )
    .unwrap();
    assert_eq!((how, kept.finish.claims.len()), ("review_unavailable", 2));
    assert!(
        review(
            &state,
            &Failing,
            &prepared,
            composed(),
            &|| CallDirective::Cancelled,
            None
        )
        .is_err()
    );
}

/// AC-1330 (T-1330, #2482), AC-1331 (T-1331, #2495): on a local lane a
/// claim most samples read as
/// stating a cited rule beyond the capabilities it is listed under (or
/// under which an identical rule is listed) is left out before staging;
/// a claim citing no rule, or only rules with no citable capability, is not
/// checked, and no readable reply keeps all.
#[test]
fn a_claim_stating_a_rule_wider_than_its_capabilities_is_left_out() {
    let (_dir, state) = state();
    let mut prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let (checkout, refund) = (id("cap:checkout"), id("cap:refund"));
    let (valid, valid_too, window) = (id("rule:valid"), id("rule:valid-too"), id("rule:window"));
    let orphan = id("rule:orphan");
    let valid_archive = id("rule:valid-archive");
    prepared.text.push_str(&format!(
        "\n### Rules the capabilities enforce (from 1.5)\n\n\
         | Capability | Rule | Citation |\n|---|---|---|\n\
         | Checkout | A card must be valid | {valid} |\n\
         | Refund | A card must be valid | {valid_too} |\n\
         | Refund | Refunds within 30 days | {window} |\n\
         | Gone | Gift cards expire | {orphan} |\n\
         | Archive | A card must be valid | {valid_archive} |\n"
    ));
    let claim = |statement: &str, citations: &[&String]| Claim {
        statement: statement.into(),
        kind: ClaimKind::Implemented,
        citations: citations.iter().map(|id| (*id).clone()).collect(),
    };
    let composed = || Composed {
        finish: SectionDraftFinish {
            address: prepared.address.clone(),
            claims: vec![
                claim("Checkout needs a valid card.", &[&valid, &checkout]),
                claim("Every payment needs a valid card.", &[&valid, &checkout]),
                claim(
                    "Refunds need a valid card too.",
                    &[&valid, &checkout, &refund],
                ),
                claim(
                    "Checkout refunds within 30 days.",
                    &[&window, &refund, &checkout],
                ),
                claim("Shoppers check out.", &[&checkout]),
                claim("Gift cards expire at checkout.", &[&orphan, &checkout]),
            ],
        },
        left_out: 0,
        reasons: BTreeMap::new(),
    };
    // One provider answers both checks: the review cannot read this reply
    // and keeps every claim; the scope check reads it.
    let checker = Scripted {
        reply: json!({"answers": [
            {"pair": "1.1", "answer": "within"},
            {"pair": "2.1", "answer": "widened"},
            {"pair": "3.1", "answer": "within"},
            {"pair": "4.1", "answer": "widened"},
        ]})
        .to_string(),
        prompts: Mutex::new(Vec::new()),
    };
    let (kept, how, scope) = review(
        &state,
        &checker,
        &prepared,
        composed(),
        &|| CallDirective::Continue,
        None,
    )
    .unwrap();
    assert_eq!((how, scope), ("review_unavailable", Some("checked")));
    let statements: Vec<&str> = kept
        .finish
        .claims
        .iter()
        .map(|claim| claim.statement.as_str())
        .collect();
    assert_eq!(
        statements,
        [
            "Checkout needs a valid card.",
            "Refunds need a valid card too.",
            "Shoppers check out.",
            "Gift cards expire at checkout."
        ]
    );
    assert_eq!((kept.left_out, kept.reasons["rule_scope_widened"]), (2, 2));
    // The scope check is asked three times, with the rule under its
    // capability and only the claims citing a rule.
    let prompts = checker.prompts.lock().unwrap().clone();
    let scoped: Vec<&String> = prompts
        .iter()
        .filter(|prompt| prompt.contains("<pairs>"))
        .collect();
    assert_eq!(scoped.len(), agents::rule_scope::SCOPE_SAMPLES);
    assert!(scoped[0].contains(&format!(
        "{valid}: Rule of capability \"Checkout\": A card must be valid"
    )));
    // AC-1331 (T-1331, #2495): one line per claim and rule pair, the rule
    // holding for its capability and every identical rule's, citable or not.
    assert!(scoped[0].contains(&format!(
        "2.1  Claim 2: \"Every payment needs a valid card.\"  |  Rule {valid} of \"Archive\", \"Checkout\", \"Refund\": A card must be valid\n"
    )));
    assert!(!scoped[0].contains("Shoppers check out."));
    // A rule whose capability is not citable is not checked (#2483).
    assert!(!scoped[0].contains("Gift cards expire at checkout."));
    // A reply that is not the schema's keeps every claim.
    let unreadable = Scripted {
        reply: "not a scope".into(),
        prompts: Mutex::new(Vec::new()),
    };
    let (kept, _, scope) = review(
        &state,
        &unreadable,
        &prepared,
        composed(),
        &|| CallDirective::Continue,
        None,
    )
    .unwrap();
    assert_eq!((scope, kept.finish.claims.len()), (Some("unavailable"), 6));
}

/// AC-1328 (T-1328, #2449): a local composition whose reply lost two claims
/// to uncited routes, numbers or endpoints is asked once more with the
/// correction, and the screened reply keeping more claims goes on; one such
/// claim, a cloud provider or an unreadable retry keeps the first.
#[test]
fn a_reply_losing_claims_to_uncited_figures_is_asked_once_more() {
    let (_dir, state) = state();
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&capability("cap:orders", "Place orders"))
        .unwrap();
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let cites = [id("cap:orders")];
    let claim = |statement: &str| json!({"statement": statement, "claim_kind": "implemented_behavior", "citation_ids": cites});
    let first = |uncited: usize| {
        let mut claims = vec![claim("Shoppers place orders for themselves.")];
        for n in 0..uncited {
            claims.push(claim(&format!(
                "Shoppers place up to {} orders a day.",
                17 + n
            )));
        }
        let composer = Scripted {
            reply: json!({ "claims": claims }).to_string(),
            prompts: Mutex::new(Vec::new()),
        };
        compose(
            &state,
            &composer,
            &prepared,
            None,
            &|| CallDirective::Continue,
            None,
        )
        .unwrap()
        .unwrap()
    };
    let retry = |reply: String| Scripted {
        reply,
        prompts: Mutex::new(Vec::new()),
    };
    let continue_ = || CallDirective::Continue;
    let better = json!({"claims": [
        claim("Shoppers place orders for themselves."),
        claim("Shoppers place orders for the people they shop for."),
    ]})
    .to_string();
    // Two claims left out for uncited numbers: asked once more, and the
    // retry keeps two claims to the first's one.
    let provider = retry(better.clone());
    let (kept, reason, how) =
        screen_or_retry(&state, &provider, &prepared, first(2), &continue_, None).unwrap();
    assert_eq!((reason, how), (None, Some("kept_retry")));
    assert_eq!(kept.finish.claims.len(), 2);
    let prompts = provider.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].ends_with(&format!(
        "<correction>\n{}\n</correction>\n",
        agents::section_compose::CORRECTION
    )));
    drop(prompts);
    // An unreadable retry keeps the first reply as screened.
    let provider = retry("not a reply".into());
    let (kept, _, how) =
        screen_or_retry(&state, &provider, &prepared, first(2), &continue_, None).unwrap();
    assert_eq!((how, kept.finish.claims.len()), (Some("kept_first"), 1));
    assert_eq!(kept.reasons["number_not_cited"], 2);
    // One such claim is not worth a second call.
    let provider = retry(better.clone());
    let (kept, _, how) =
        screen_or_retry(&state, &provider, &prepared, first(1), &continue_, None).unwrap();
    assert_eq!((how, kept.finish.claims.len()), (None, 1));
    assert!(provider.prompts.lock().unwrap().is_empty());
    // A cloud consent covers one payload: no second call.
    struct Cloud;
    impl LlmProvider for Cloud {
        fn id(&self) -> &str {
            "cloud"
        }
        fn locality(&self) -> Locality {
            Locality::Cloud
        }
        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }
        fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Err(ProviderError::Unsupported("fixture"))
        }
    }
    let (kept, _, how) =
        screen_or_retry(&state, &Cloud, &prepared, first(2), &continue_, None).unwrap();
    assert_eq!((how, kept.finish.claims.len()), (None, 1));
    // A local retry whose call fails keeps the first reply; a cancelled job
    // still fails.
    struct Failing;
    impl LlmProvider for Failing {
        fn id(&self) -> &str {
            "local-failing"
        }
        fn locality(&self) -> Locality {
            Locality::Local
        }
        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }
        fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Err(ProviderError::Unsupported("fixture"))
        }
    }
    let (kept, _, how) =
        screen_or_retry(&state, &Failing, &prepared, first(2), &continue_, None).unwrap();
    assert_eq!((how, kept.finish.claims.len()), (Some("kept_first"), 1));
    let cancelled = || CallDirective::Cancelled;
    assert!(screen_or_retry(&state, &Failing, &prepared, first(2), &cancelled, None).is_err());
}

/// AC-1606 (#1748): in a gated run by area, a failed model call on one
/// part is that part's, and the next is drafted; a host failure, a cancel or
/// lost ownership still ends the run, which fails whole when nothing was
/// staged.
#[test]
fn one_part_failing_does_not_end_a_gated_run() {
    for staged_any in [false, true] {
        assert_eq!(
            after_failure(staged_any, true, CallDirective::Continue),
            AfterFailure::Next
        );
    }
    assert_eq!(
        after_failure(false, false, CallDirective::Continue),
        AfterFailure::Fail
    );
    assert_eq!(
        after_failure(true, false, CallDirective::Continue),
        AfterFailure::Stop
    );
    for directive in [CallDirective::Cancelled, CallDirective::OwnershipLost] {
        for provider_failed in [false, true] {
            assert_eq!(
                after_failure(false, provider_failed, directive),
                AfterFailure::Fail
            );
            assert_eq!(
                after_failure(true, provider_failed, directive),
                AfterFailure::Stop
            );
        }
    }
    // The watch follows the last model call.
    struct Quiet;
    impl LlmProvider for Quiet {
        fn id(&self) -> &str {
            "local-quiet"
        }
        fn locality(&self) -> Locality {
            Locality::Local
        }
        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }
        fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Err(ProviderError::Unsupported("fixture"))
        }
    }
    let watched = Watched::new(&Quiet);
    assert!(!watched.last_call_failed());
    assert!(watched.noted::<(), ()>(Err(())).is_err());
    assert!(watched.last_call_failed());
    assert!(watched.noted::<(), ()>(Ok(())).is_ok());
    assert!(!watched.last_call_failed());
}

/// An attested record for §1.3 of the system resting on `cap:checkout`, as
/// the graph holds it, stated by the (placeholder) engineer. Returns its
/// `A-` id and identity; `withdrawn` withdraws it afterwards.
fn attested_for_capabilities(state: &AppState, withdrawn: bool) -> (String, String) {
    use agents::attested::{
        Attestation, AttestationSource, AttestedRecord, AttestedTarget, EngineerVerb,
    };
    let cited = BTreeSet::from([id("cap:checkout")]);
    let recorded_hashes = {
        let graph = state.graph.lock().unwrap();
        let (nodes, edges) = graph.read_snapshot().unwrap();
        spec::cited_fact_hashes(&nodes, &edges, &cited)
    };
    let drafts = state.drafts.lock().unwrap();
    let store = drafts.attested();
    // Placeholder contributor only (ADR-0090 decision 4).
    let engineer = store.set_engineer("Placeholder Engineer", None).unwrap();
    let says = |verb| Attestation::Engineer {
        contributor: engineer.id.clone(),
        verb,
        at_ms: 1_700_000_000_000,
        source: AttestationSource::Workbench,
        reference: None,
    };
    let staged = store
        .stage(AttestedRecord {
            target: AttestedTarget::Section {
                address: SectionAddress {
                    catalogue_version: "1.0".into(),
                    document: "D1".into(),
                    section: "1.3".into(),
                    scope: "system".into(),
                },
            },
            statement: "Checkout is used by the shop staff only.".into(),
            supporting: cited.into_iter().collect(),
            recorded_hashes,
            attestations: vec![says(EngineerVerb::Stated)],
        })
        .unwrap();
    if withdrawn {
        store
            .attest(&staged.record_id, 1, says(EngineerVerb::Withdrew))
            .unwrap();
    }
    (staged.citation_id, staged.record_id)
}

// AC-1077 (T-1077, ADR-0090 decision 6): a composition is handed the
// attested records of its section and scope, and a claim citing one is
// staged with the record's identity as the id's hash.
#[test]
fn a_composition_may_cite_an_attested_record_of_its_section() {
    let (_dir, state) = state();
    let (attested, identity) = attested_for_capabilities(&state, false);
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    let provider = Scripted {
        reply: json!({"claims": [
            {"statement": "Checkout takes an order for the shop staff.",
             "claim_kind": "implemented_behavior",
             "citation_ids": [id("cap:checkout"), attested.clone()]},
        ]})
        .to_string(),
        prompts: Mutex::new(Vec::new()),
    };
    let composed = compose(
        &state,
        &provider,
        &prepared,
        None,
        &|| CallDirective::Continue,
        None,
    )
    .unwrap()
    .unwrap();
    let prompt = provider.prompts.lock().unwrap().remove(0);
    assert!(prompt.contains(&format!(
        "{attested} | attested | Checkout is used by the shop staff only.\n"
    )));
    // Contributors are project data: the composer is not told them.
    assert!(!prompt.contains("Placeholder Engineer"));

    let (_, job) = crate::start_job(&state, "section-compose:D1:1.3:system:local").unwrap();
    let outcome = stage(&state, &prepared, composed, job.id()).unwrap();
    assert_eq!(outcome.claims, 1, "{outcome:?}");
    let store = state.drafts.lock().unwrap();
    let stored = store.for_job(job.id()).unwrap().unwrap();
    assert_eq!(stored.draft.cited_hashes.get(&attested), Some(&identity));
    assert!(stored.draft.cited_hashes.contains_key(&id("cap:checkout")));
}

#[test]
fn a_withdrawn_attested_record_is_not_offered_to_a_composition() {
    let (_dir, state) = state();
    let (attested, _) = attested_for_capabilities(&state, true);
    let prepared = prepare(&state, "1.0", "D1", "1.3", "system").unwrap();
    assert!(!prepared.citable.contains(&attested));
    assert!(!prepared.action.payload.prompt.contains("attested_records"));
}
