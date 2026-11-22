//! AC-0862 (T-0862): a section is composed in one bounded call from the
//! section D1 compiles and the facts it cites, and staged for review.
use super::*;
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
    let rules = BTreeMap::from([("F-rule".to_owned(), "Manage user (API)".to_owned())]);
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
    let orphan = BTreeMap::from([("F-rule".to_owned(), "Gone".to_owned())]);
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
