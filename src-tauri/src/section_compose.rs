//! Section composition (#1991, AC-0862): one catalogue section drafted in
//! one bounded call from the section as the export compiles it.
//!
//! A drafting run (`section-writer@1`) has to find its context with a tool
//! budget, and the knowledge layer's Capabilities are not in the graph it
//! queries. A composition is handed that context instead:
//!
//! 1. [`prepare`] compiles D1 from one read of the graph, the decisions and
//!    the knowledge overlay, takes the section out of it, and lists the base
//!    facts it may cite (`spec::composition_facts`). It writes the one
//!    completion action (`agents::section_compose::action`).
//! 2. [`section_compose_preview`] shows the exact redacted payload. A cloud
//!    composition runs only for the payload hash the user approved, through
//!    the same firewall, policy, consent and egress record as an escalation.
//! 3. [`compose`] makes the call and [`stage`] stages the reply as a draft
//!    for review, through the same citation set and admission rule as a
//!    drafting run. A claim that names a source identifier it cites
//!    (SPEC-16 rule 3, AC-0683) is not staged; the others are.
//!
//! The draft records the role `section-composer@1` and the durable job that
//! produced it. Nothing here writes the graph (R-INT-1, R-INT-3).

use crate::investigations::section_drafts::{DraftingOutcome, drafting_address, predecessor};
use crate::{AppState, JobExecution};
use agents::draft_store::{DraftOrigin, DraftStoreError};
use agents::section_compose::{
    self, ComposeFact, ComposeGroup, ComposeInput, Composed, SECTION_COMPOSER_ROLE,
};
use agents::section_draft::{SectionAddress, admit};
use core_graph::{Edge, Node};
use llm::bounded::{
    CallDirective, CompletionControl, CompletionLimitValues, CompletionLimits,
    HOSTED_OUTPUT_TOKENS, MAX_REQUEST_TIMEOUT_MS,
};
use llm::{CompletionAction, ConsentGrant, EgressFirewall, EgressPreview, LlmProvider, Locality};
use serde::Serialize;
use std::collections::BTreeSet;
use tauri::Manager;

const D1_ARTIFACT_ID: &str = "d1-product-specification";

/// Everything one composition is made from, read once.
pub(crate) struct Prepared {
    address: SectionAddress,
    action: CompletionAction,
    citable: BTreeSet<String>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

/// What a composition produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ComposeOutcome {
    /// The staged draft, or why there is none.
    #[serde(flatten)]
    pub(crate) outcome: DraftingOutcome,
    /// Claims staged.
    pub(crate) claims: usize,
    /// Claims left out: a per-claim bound, or a statement naming a source
    /// identifier it cites.
    pub(crate) left_out: usize,
}

impl ComposeOutcome {
    fn none(reason: &'static str) -> Self {
        Self {
            outcome: DraftingOutcome::NoDraft { reason },
            claims: 0,
            left_out: 0,
        }
    }
}

/// The address to compose, checked like a drafting run's. Only the system
/// scope is composed: the compiled section is the whole system's.
fn address_of(
    catalogue_version: &str,
    document: &str,
    section: &str,
    scope: &str,
) -> Result<(SectionAddress, &'static str), String> {
    let question = crate::investigations::section_drafts::drafting_question(
        catalogue_version,
        document,
        section,
        scope,
    )
    .map_err(str::to_owned)?;
    let address = drafting_address(&question).map_err(str::to_owned)?;
    if address.scope != spec::SYSTEM_SCOPE {
        return Err("composition_system_scope_only".into());
    }
    if address.document != "D1" {
        return Err("composition_d1_only".into());
    }
    let title = spec::drafted_section(catalogue_version, document, section)
        .map_err(|error| error.code().to_owned())?
        .title;
    Ok((address, title))
}

/// Compile D1 and write the composition of one section.
pub(crate) fn prepare(
    state: &AppState,
    catalogue_version: &str,
    document: &str,
    section: &str,
    scope: &str,
) -> Result<Prepared, String> {
    let (address, title) = address_of(catalogue_version, document, section, scope)?;
    let review = crate::curated::gather(state);
    let read = crate::read_spec_locked(state)?;
    let nodes = read.nodes.clone();
    let edges = read.edges.clone();
    let derived_facts = read.knowledge.capabilities.clone();
    let artifact = crate::compile_spec_artifact(
        read.inputs(Some(&review)),
        spec::ExportMode::BestEffort,
        D1_ARTIFACT_ID,
    )?;
    let text =
        spec::section_text(&artifact.content, &address.section).ok_or("section_not_compiled")?;
    let derived: Vec<_> = derived_facts
        .iter()
        .filter_map(|fact| match &fact.item {
            core_graph::overlay::OverlayItem::Node(node) => Some((node, &fact.cites)),
            core_graph::overlay::OverlayItem::Edge(_) => None,
        })
        .collect();
    let picked = spec::composition_facts(text, &nodes, &edges, &derived);
    if picked.facts.is_empty() {
        return Err("no_citable_fact".into());
    }
    let facts: Vec<ComposeFact> = picked
        .facts
        .iter()
        .map(|fact| ComposeFact {
            citation_id: fact.citation_id.clone(),
            label: fact.label.clone(),
            name: fact.name.clone(),
            tier: fact.tier.clone(),
            confidence: fact.confidence.clone(),
        })
        .collect();
    let groups: Vec<ComposeGroup> = picked
        .derived
        .iter()
        .map(|group| ComposeGroup {
            citation_id: group.citation_id.clone(),
            label: group.label.clone(),
            name: group.name.clone(),
            rests_on: group.rests_on.clone(),
        })
        .collect();
    let action = section_compose::action(
        &ComposeInput {
            address: &address,
            title,
            section: text,
            facts: &facts,
            groups: &groups,
        },
        format!(
            "section-compose:{}:{}:{}",
            address.document, address.section, address.scope
        ),
    );
    Ok(Prepared {
        address,
        action,
        citable: picked.citable(),
        nodes,
        edges,
    })
}

/// One call's limits: the general ceilings, with the hosted lane's output
/// allowance and the longest permitted timeout.
fn limits() -> Result<CompletionLimits, String> {
    CompletionLimits::for_investigation(CompletionLimitValues {
        max_output_tokens: HOSTED_OUTPUT_TOKENS,
        request_timeout_ms: MAX_REQUEST_TIMEOUT_MS,
        ..CompletionLimitValues::default()
    })
    .map_err(|error| error.to_string())
}

fn firewall(state: &AppState) -> Result<EgressFirewall, String> {
    let policy = state
        .settings
        .lock()
        .map_err(|e| e.to_string())?
        .egress_policy()
        .map_err(|e| e.to_string())?;
    Ok(EgressFirewall::new(policy))
}

/// Make the call and decode its reply. `consent` is required for a cloud
/// provider and must match this exact payload. `check` stops the call.
pub(crate) fn compose(
    state: &AppState,
    provider: &dyn LlmProvider,
    prepared: &Prepared,
    approved_payload_hash: Option<String>,
    check: &dyn Fn() -> CallDirective,
) -> Result<Result<Composed, &'static str>, String> {
    let limits = limits()?;
    let firewall = firewall(state)?;
    let bounded = firewall
        .prepare_bounded(provider, &prepared.action, &limits)
        .map_err(|error| error.to_string())?;
    let consent: Option<ConsentGrant> = if provider.locality() == Locality::Cloud {
        Some(crate::egress::consent_grant_for_approved_payload(
            &bounded.preview().egress,
            approved_payload_hash,
        )?)
    } else {
        None
    };
    let authorized = firewall
        .authorize_bounded(provider, &bounded, consent.as_ref())
        .map_err(|error| error.to_string())?;
    // #1767: a cloud payload is recorded when it is handed to the transport;
    // a call whose record cannot be written is not made.
    let provider_label = format!(
        "{}:{}",
        bounded.profile().provider_id,
        bounded.profile().requested_model
    );
    let record = |bytes: u64| {
        state
            .settings
            .lock()
            .is_ok_and(|mut settings| settings.record_egress(&provider_label, bytes).is_ok())
    };
    let cloud = provider.locality() == Locality::Cloud;
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(limits.values().request_timeout_ms);
    let completion = provider
        .complete_bounded(
            &authorized,
            &CompletionControl {
                deadline,
                check,
                dispatching: cloud.then_some(&record),
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(
        section_compose::decode(&prepared.address, &completion.text, &prepared.citable)
            .map_err(|refusal| refusal.code()),
    )
}

/// Stage a decoded composition as a draft of job `job_id`.
pub(crate) fn stage(
    state: &AppState,
    prepared: &Prepared,
    composed: Composed,
    job_id: i64,
) -> Result<ComposeOutcome, String> {
    let Composed {
        mut finish,
        mut left_out,
    } = composed;
    let (nodes, edges) = (&prepared.nodes, &prepared.edges);
    // The draft is recorded against the graph the section was compiled
    // from, so that graph must still be the current one.
    let current = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        let (_, ((nodes, edges), _)) = crate::curated::read_graph(&graph)?;
        spec::graph_content_hash(&nodes, &edges)
    };
    let graph_content_hash = spec::graph_content_hash(nodes, edges);
    if current != graph_content_hash {
        return Ok(ComposeOutcome::none("graph_changed"));
    }
    // SPEC-16 rule 3 (AC-0683): a statement naming a source identifier it
    // cites is not staged.
    let cited: BTreeSet<String> = finish
        .claims
        .iter()
        .flat_map(|claim| claim.citations.iter().cloned())
        .collect();
    let source = spec::SourceIdentifiers::of(nodes, edges, &cited);
    let before = finish.claims.len();
    finish
        .claims
        .retain(|claim| !source.named_in(&claim.statement, &claim.citations));
    left_out += before - finish.claims.len();
    if finish.claims.is_empty() {
        return Ok(ComposeOutcome {
            left_out,
            ..ComposeOutcome::none("statement_names_source")
        });
    }
    let cited: BTreeSet<String> = finish
        .claims
        .iter()
        .flat_map(|claim| claim.citations.iter().cloned())
        .collect();
    let address = prepared.address.clone();
    let section = spec::drafted_section(
        &address.catalogue_version,
        &address.document,
        &address.section,
    )
    .map_err(|error| error.code().to_owned())?;
    let citation_set =
        match spec::section_citation_set(section, &address.scope, nodes, edges, &cited) {
            Ok(set) => set,
            Err(refusal) => return Ok(ComposeOutcome::none(refusal.code())),
        };
    let draft = match admit(&address, &citation_set, finish) {
        Ok(draft) => draft,
        Err(refusal) => {
            return Ok(ComposeOutcome {
                left_out,
                ..ComposeOutcome::none(refusal.code())
            });
        }
    };
    let origin = DraftOrigin {
        role: SECTION_COMPOSER_ROLE.to_owned(),
        role_definition: section_compose::definition_fingerprint(),
        job_id,
        graph_content_hash,
    };
    let claims = draft.claims.len();
    let mut store = state.drafts.lock().map_err(|e| e.to_string())?;
    let replaces = predecessor(&store, &address, (nodes, edges)).map_err(|e| e.to_string())?;
    match store.store(&draft, &origin, replaces.as_deref()) {
        Ok(stored) => Ok(ComposeOutcome {
            outcome: DraftingOutcome::Staged {
                draft_id: stored.draft_id,
            },
            claims,
            left_out,
        }),
        Err(DraftStoreError::Storage(error)) => Err(error.to_string()),
        Err(_) => Ok(ComposeOutcome {
            left_out,
            ..ComposeOutcome::none("draft_not_storable")
        }),
    }
}

/// The exact redacted payload a cloud composition of one section sends.
#[tauri::command]
pub(crate) async fn section_compose_preview(
    catalogue_version: String,
    document: String,
    section: String,
    scope: String,
    app: tauri::AppHandle,
) -> Result<EgressPreview, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let prepared = prepare(&state, &catalogue_version, &document, &section, &scope)?;
        let provider = crate::egress::escalation_provider("cloud")?;
        let bounded = firewall(&state)?
            .prepare_bounded(provider.as_ref(), &prepared.action, &limits()?)
            .map_err(|error| error.to_string())?;
        Ok(bounded.preview().egress.clone())
    })
    .await
}

fn run_blocking(
    state: &AppState,
    execution: &JobExecution,
    provider: &dyn LlmProvider,
    address: (&str, &str, &str, &str),
    approved_payload_hash: Option<String>,
) -> Result<ComposeOutcome, String> {
    let (catalogue_version, document, section, scope) = address;
    let prepared = prepare(state, catalogue_version, document, section, scope)?;
    let check = || match crate::job_cancelled(state, execution) {
        Ok(false) => CallDirective::Continue,
        Ok(true) => CallDirective::Cancelled,
        Err(_) => CallDirective::OwnershipLost,
    };
    match compose(state, provider, &prepared, approved_payload_hash, &check)? {
        Ok(composed) => stage(state, &prepared, composed, execution.id()),
        Err(reason) => Ok(ComposeOutcome::none(reason)),
    }
}

/// Compose one section as a durable job and stage the draft for review. A
/// cloud composition runs only when `approved_payload_hash` is the hash of
/// the payload this exact read produces (one-action consent, AC-0063).
#[tauri::command]
pub(crate) async fn run_section_compose(
    catalogue_version: String,
    document: String,
    section: String,
    scope: String,
    mode: String,
    approved_payload_hash: Option<String>,
    app: tauri::AppHandle,
) -> Result<ComposeOutcome, String> {
    let state = app.state::<AppState>();
    let kind = format!("section-compose:{document}:{section}:{scope}:{mode}");
    let (running, execution) = crate::start_job(&state, &kind)?;
    crate::emit_job(&app, &running);
    let worker = app.clone();
    let result = crate::off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = worker.state::<AppState>();
        let provider = crate::egress::escalation_provider(&mode)?;
        run_blocking(
            &state,
            execution,
            provider.as_ref(),
            (&catalogue_version, &document, &section, &scope),
            approved_payload_hash,
        )
    })
    .await;
    match &result {
        Ok(outcome) => {
            let artifacts = match &outcome.outcome {
                DraftingOutcome::Staged { draft_id } => vec![draft_id.clone()],
                DraftingOutcome::NoDraft { .. } => Vec::new(),
            };
            let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
            let update = jobs
                .finish_execution(&execution, &artifacts)
                .map_err(|e| e.to_string())?;
            drop(jobs);
            crate::emit_job(&app, &crate::updated_job(update));
        }
        Err(error) => crate::report_failure(&app, &state, &execution, error),
    }
    result
}

#[cfg(test)]
#[path = "section_compose_tests.rs"]
mod tests;
