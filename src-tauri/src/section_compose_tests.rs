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
