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
use agents::gated_compose::{self, GatedInput, GatedStats, StageModel, Workflow};
use agents::section_compose::{
    self, ComposeFact, ComposeGroup, ComposeInput, Composed, SECTION_COMPOSER_ROLE,
};
use agents::section_draft::{Claim, SectionAddress, SectionDraftFinish, admit};
use core_graph::{Edge, Node};
use llm::bounded::{
    BoundedCompletion, CallDirective, CompletionControl, CompletionLimitValues, CompletionLimits,
    HOSTED_OUTPUT_TOKENS, MAX_REQUEST_TIMEOUT_MS,
};
use llm::{CompletionAction, ConsentGrant, EgressFirewall, EgressPreview, LlmProvider, Locality};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tauri::Manager;

const D1_ARTIFACT_ID: &str = "d1-product-specification";

/// Everything one composition is made from, read once.
pub(crate) struct Prepared {
    address: SectionAddress,
    action: CompletionAction,
    citable: BTreeSet<String>,
    nodes: Arc<Vec<Node>>,
    edges: Arc<Vec<Edge>>,
    /// The overlay's derived Capabilities and Actors, citable beside the
    /// base facts (#2040).
    overlay: Arc<Vec<Node>>,
    /// The area an area-scoped composition is of (#2083), by name.
    area: Option<String>,
    /// The section's catalogue title, the compiled text and the citable
    /// facts, for a gated composition (#2083).
    title: &'static str,
    text: String,
    facts: Vec<ComposeFact>,
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
    /// How many claims each check left out, by reason (#2349).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) left_out_reasons: BTreeMap<&'static str, usize>,
    /// What a gated composition did (#2083, AC-0980).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gated: Option<GatedStats>,
}

impl ComposeOutcome {
    fn none(reason: &'static str) -> Self {
        Self {
            outcome: DraftingOutcome::NoDraft { reason },
            claims: 0,
            left_out: 0,
            left_out_reasons: BTreeMap::new(),
            gated: None,
        }
    }
}

/// Leave out the claims of `finish` that `keep` refuses, counting them
/// under `reason`. Whether every claim is now left out.
fn leave_out(
    finish: &mut SectionDraftFinish,
    (left_out, reasons): (&mut usize, &mut BTreeMap<&'static str, usize>),
    reason: &'static str,
    keep: impl Fn(&Claim) -> bool,
) -> bool {
    let before = finish.claims.len();
    finish.claims.retain(|claim| keep(claim));
    let dropped = before - finish.claims.len();
    if dropped > 0 {
        *left_out += dropped;
        *reasons.entry(reason).or_default() += dropped;
    }
    finish.claims.is_empty()
}

/// The address to compose, checked like a drafting run's. [`prepare`]
/// composes the system scope, or an area's scope on a section arranged by
/// area (#2083).
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
    let compiled = compile(state, catalogue_version, document, section, scope)?;
    if compiled.address.scope == spec::SYSTEM_SCOPE {
        let text = compiled.text.clone();
        return prepared(&compiled, compiled.address.clone(), None, &text);
    }
    // An area scope (#2083): the area's block of a section arranged by area.
    if !spec::AREA_SECTIONS.contains(&compiled.address.section.as_str()) {
        return Err("composition_system_scope_only".into());
    }
    let name = compiled
        .areas
        .iter()
        .find(|(id, _)| id == scope)
        .map(|(_, name)| name.as_str())
        .ok_or("area_not_found")?;
    let (blocks, _) = spec::area_blocks(&compiled.text, &[name]);
    let (_, block) = blocks.first().ok_or("area_not_in_section")?;
    let text = area_text(&compiled.text, block);
    prepared(
        &compiled,
        compiled.address.clone(),
        Some(name.to_owned()),
        &text,
    )
}

/// One read of the graph, the decisions and the knowledge overlay, and the
/// section compiled from it.
struct Compiled {
    address: SectionAddress,
    title: &'static str,
    /// The section as a composition reads it.
    text: String,
    nodes: Arc<Vec<Node>>,
    edges: Arc<Vec<Edge>>,
    /// The overlay's Capabilities and Actors with the base facts each
    /// rests on; citable as themselves (#2040).
    derived: Vec<core_graph::overlay::OverlayFact>,
    overlay: Arc<Vec<Node>>,
    /// Each named Subsystem as (scope, name): its citation id and its name,
    /// in id order, each name once.
    areas: Vec<(String, String)>,
}

fn compile(
    state: &AppState,
    catalogue_version: &str,
    document: &str,
    section: &str,
    scope: &str,
) -> Result<Compiled, String> {
    let (address, title) = address_of(catalogue_version, document, section, scope)?;
    let review = crate::curated::gather(state);
    let read = crate::read_spec_locked(state)?;
    let mut named: Vec<(&str, &str)> = read
        .knowledge
        .subsystems
        .iter()
        .filter_map(|fact| match &fact.item {
            core_graph::overlay::OverlayItem::Node(node) if node.label == "Subsystem" => node.props
                ["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .map(|name| (node.id.as_str(), name)),
            _ => None,
        })
        .collect();
    named.sort();
    let mut seen = BTreeSet::new();
    let areas = named
        .into_iter()
        .filter(|(_, name)| seen.insert(*name))
        .map(|(id, name)| {
            (
                spec::citation_id(&core_graph::FactKey::Node { id: id.to_owned() }),
                name.to_owned(),
            )
        })
        .collect();
    let nodes = Arc::new(read.nodes.clone());
    let edges = Arc::new(read.edges.clone());
    let derived: Vec<_> = read
        .knowledge
        .capabilities
        .iter()
        .chain(&read.knowledge.actors)
        // The areas 1.3 prints, so a summary of them can cite them (#2319).
        .chain(read.knowledge.subsystems.iter().filter(|fact| {
            matches!(&fact.item, core_graph::overlay::OverlayItem::Node(node) if node.label == "Subsystem")
        }))
        .cloned()
        .collect();
    let overlay = Arc::new(
        derived
            .iter()
            .filter_map(|fact| match &fact.item {
                core_graph::overlay::OverlayItem::Node(node) => Some(node.clone()),
                core_graph::overlay::OverlayItem::Edge(_) => None,
            })
            .collect(),
    );
    let artifact = crate::compile_spec_artifact(
        read.inputs(Some(&review)),
        spec::ExportMode::BestEffort,
        D1_ARTIFACT_ID,
    )?;
    // An overview section is read up to its per-capability detail (#1991);
    // a summary section reads the sections it summarises (#2319).
    let text = spec::composition_source(&artifact.content, &address.section)
        .ok_or("section_not_compiled")?;
    Ok(Compiled {
        address,
        title,
        text,
        nodes,
        edges,
        derived,
        overlay,
        areas,
    })
}

/// An area's part of a section as a composition reads it: the section's
/// heading line, then the area's block.
fn area_text(section: &str, block: &str) -> String {
    let heading = section.lines().next().unwrap_or_default();
    format!("{heading}\n\n{block}")
}

/// The composition of `text`, a part of the compiled section, at `address`.
fn prepared(
    compiled: &Compiled,
    address: SectionAddress,
    area: Option<String>,
    text: &str,
) -> Result<Prepared, String> {
    let (nodes, edges) = (&compiled.nodes, &compiled.edges);
    let derived: Vec<_> = compiled
        .derived
        .iter()
        .filter_map(|fact| match &fact.item {
            core_graph::overlay::OverlayItem::Node(node) => Some((node, &fact.cites)),
            core_graph::overlay::OverlayItem::Edge(_) => None,
        })
        .collect();
    let picked = spec::composition_facts(text, nodes, edges, &derived);
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
            title: compiled.title,
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
        nodes: Arc::clone(nodes),
        edges: Arc::clone(edges),
        overlay: Arc::clone(&compiled.overlay),
        area,
        title: compiled.title,
        text: text.to_owned(),
        facts,
    })
}

/// One call's limits: the general ceilings, with the hosted lane's output
/// allowance and the longest permitted timeout.
pub(crate) fn limits() -> Result<CompletionLimits, String> {
    CompletionLimits::for_investigation(CompletionLimitValues {
        max_output_tokens: HOSTED_OUTPUT_TOKENS,
        request_timeout_ms: MAX_REQUEST_TIMEOUT_MS,
        ..CompletionLimitValues::default()
    })
    .map_err(|error| error.to_string())
}

pub(crate) fn firewall(state: &AppState) -> Result<EgressFirewall, String> {
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
    job_id: Option<i64>,
) -> Result<Result<Composed, &'static str>, String> {
    let text = complete(
        state,
        provider,
        &prepared.action,
        approved_payload_hash,
        check,
        job_id,
    )?
    .text;
    Ok(
        section_compose::decode(&prepared.address, &text, &prepared.citable)
            .map_err(|refusal| refusal.code()),
    )
}

/// One bounded call of `action` through the egress firewall, and its reply
/// text. A cloud provider needs `approved_payload_hash` to be the hash of
/// this exact payload; the payload is recorded when it is handed to the
/// transport. `check` stops the call. Shared with the topic expert (#2057),
/// which also reports the provider's usage.
pub(crate) fn complete(
    state: &AppState,
    provider: &dyn LlmProvider,
    action: &CompletionAction,
    approved_payload_hash: Option<String>,
    check: &dyn Fn() -> CallDirective,
    job_id: Option<i64>,
) -> Result<BoundedCompletion, String> {
    let limits = limits()?;
    let firewall = firewall(state)?;
    let bounded = firewall
        .prepare_bounded(provider, action, &limits)
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
    if let Some(job_id) = job_id {
        record_usage(state, job_id, &completion);
    }
    Ok(completion)
}

/// Add a returned call's provider-reported usage to its job (AC-1703).
/// The answer is already valid, so a usage row that cannot be written is
/// left out rather than failing the call; the job then shows no usage.
fn record_usage(state: &AppState, job_id: i64, completion: &BoundedCompletion) {
    let reported = completion.usage.as_ref();
    let call = crate::jobs::CallUsage {
        model: &completion.requested_model,
        input_tokens: reported.and_then(|usage| usage.input_tokens),
        output_tokens: reported.and_then(|usage| usage.output_tokens),
        request_bytes: completion.request_bytes,
    };
    if let Ok(mut jobs) = state.jobs.lock() {
        let _ = jobs.add_usage(job_id, &call);
    }
}

/// Stage a decoded composition as a draft of job `job_id`.
pub(crate) fn stage(
    state: &AppState,
    prepared: &Prepared,
    composed: Composed,
    job_id: i64,
) -> Result<ComposeOutcome, String> {
    stage_as(
        state,
        prepared,
        composed,
        job_id,
        (
            SECTION_COMPOSER_ROLE,
            section_compose::definition_fingerprint(),
        ),
    )
}

/// [`stage`] recording `role`, its definition fingerprint, as the author.
fn stage_as(
    state: &AppState,
    prepared: &Prepared,
    composed: Composed,
    job_id: i64,
    role: (&str, String),
) -> Result<ComposeOutcome, String> {
    let Composed {
        mut finish,
        mut left_out,
        reasons,
    } = composed;
    let mut reasons = reasons;
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
    // A route a statement spells out must be shown by a fact it cites
    // (#2328), not taken from the section text around it; so must a figure
    // or version, and a claim about endpoints cites one (#2349).
    let shown = spec::CitedText::of(nodes, edges, &prepared.overlay, &cited);
    type Check<'a> = (&'static str, &'a dyn Fn(&Claim) -> bool);
    let checks: [Check<'_>; 4] = [
        ("statement_names_source", &|claim| {
            !source.named_in(&claim.statement, &claim.citations)
        }),
        ("route_not_cited", &|claim| {
            shown
                .uncited_route(&claim.statement, &claim.citations)
                .is_none()
        }),
        ("number_not_cited", &|claim| {
            shown
                .uncited_number(&claim.statement, &claim.citations)
                .is_none()
        }),
        ("endpoint_not_cited", &|claim| {
            !shown.endpoint_claim_uncited(&claim.statement, &claim.citations)
        }),
    ];
    for (reason, keep) in checks {
        if leave_out(&mut finish, (&mut left_out, &mut reasons), reason, keep) {
            return Ok(ComposeOutcome {
                left_out,
                left_out_reasons: reasons,
                ..ComposeOutcome::none(reason)
            });
        }
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
    // An area's draft rests on what the section prints for the area
    // (#2083): the facts its block lets a claim cite.
    let mut citation_set = if prepared.area.is_some() {
        let in_area: BTreeSet<String> = cited.intersection(&prepared.citable).cloned().collect();
        spec::cited_fact_hashes(nodes, edges, &in_area)
    } else {
        match spec::section_citation_set(section, &address.scope, nodes, edges, &cited) {
            Ok(set) => set,
            Err(refusal) => return Ok(ComposeOutcome::none(refusal.code())),
        }
    };
    // A derived Capability or Actor the composer was handed is citable too
    // (#2040): its hash is the one the curated projection compares.
    let derived: BTreeSet<String> = cited.intersection(&prepared.citable).cloned().collect();
    if prepared.area.is_some() || address.scope == spec::SYSTEM_SCOPE {
        for (id, hash) in spec::cited_node_hashes(prepared.overlay.iter(), &derived) {
            citation_set.entry(id).or_insert(hash);
        }
    }
    let draft = match admit(&address, &citation_set, finish) {
        Ok(draft) => draft,
        Err(refusal) => {
            return Ok(ComposeOutcome {
                left_out,
                left_out_reasons: reasons,
                ..ComposeOutcome::none(refusal.code())
            });
        }
    };
    let origin = DraftOrigin {
        role: role.0.to_owned(),
        role_definition: role.1,
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
            left_out_reasons: reasons,
            gated: None,
        }),
        Err(DraftStoreError::Storage(error)) => Err(error.to_string()),
        Err(_) => Ok(ComposeOutcome {
            left_out,
            left_out_reasons: reasons,
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
        let provider =
            crate::egress::escalation_provider("cloud", llm::OllamaProvider::DEFAULT_PORT)?;
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
    match compose(
        state,
        provider,
        &prepared,
        approved_payload_hash,
        &check,
        Some(execution.id()),
    )? {
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
    let (running, execution) = crate::agent_pool::start_job(&app, &kind).await?;
    crate::emit_job(&app, &running);
    let worker = app.clone();
    let result = crate::off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = worker.state::<AppState>();
        let provider = crate::egress::escalation_provider(
            &mode,
            crate::settings::saved_ollama_port(&state.settings)?,
        )?;
        run_blocking(
            &state,
            execution,
            provider.as_ref(),
            (&catalogue_version, &document, &section, &scope),
            approved_payload_hash,
        )
    })
    .await;
    let outcomes = result
        .as_ref()
        .map(|outcome| vec![outcome])
        .map_err(Clone::clone);
    finish_job(&app, &state, &execution, &outcomes)?;
    result
}

/// The local provider a gated composition runs on: the configured local
/// completion model (#1643) on the loopback endpoint.
fn gated_provider(state: &AppState) -> Result<Box<dyn LlmProvider>, String> {
    let model = state
        .settings
        .lock()
        .map_err(|e| e.to_string())?
        .local_model()
        .map_err(|e| e.to_string())?
        .model;
    Ok(Box::new(
        llm::OllamaProvider::with_models(
            &llm::OllamaProvider::loopback_url(crate::settings::saved_ollama_port(
                &state.settings,
            )?),
            llm::OllamaProvider::DEFAULT_EMBEDDING_MODEL,
            model,
            std::time::Duration::from_secs(MAX_REQUEST_TIMEOUT_MS / 1000),
        )
        .map_err(|e| e.to_string())?,
    ))
}

/// The host's [`StageModel`]: each stage call is one bounded local call
/// through the egress firewall, stopped by `check`.
struct HostModel<'a> {
    state: &'a AppState,
    provider: &'a dyn LlmProvider,
    check: &'a dyn Fn() -> CallDirective,
    job_id: i64,
}

impl StageModel for HostModel<'_> {
    fn complete(&mut self, action: &CompletionAction) -> Result<String, String> {
        if self.provider.locality() != Locality::Local {
            // A gated run makes many calls; one-action cloud consent covers
            // one exact payload, so this slice runs locally only.
            return Err("gated_composition_local_only".into());
        }
        Ok(complete(
            self.state,
            self.provider,
            action,
            None,
            self.check,
            Some(self.job_id),
        )?
        .text)
    }
}

/// Compose one section through `draft-rules@1` (#2083, AC-0980) on
/// `provider` and stage the assembled draft.
pub(crate) fn compose_gated(
    state: &AppState,
    provider: &dyn LlmProvider,
    prepared: &Prepared,
    check: &dyn Fn() -> CallDirective,
    job_id: i64,
) -> Result<ComposeOutcome, String> {
    let workflow = Workflow::shipped()?;
    if prepared.address.section != workflow.section {
        return Err("gated_composition_section_unsupported".into());
    }
    let names_source = |claim: &Claim| {
        let cited: BTreeSet<String> = claim.citations.iter().cloned().collect();
        spec::SourceIdentifiers::of(&prepared.nodes, &prepared.edges, &cited)
            .named_in(&claim.statement, &claim.citations)
    };
    let mut model = HostModel {
        state,
        provider,
        check,
        job_id,
    };
    let outcome = gated_compose::run(
        &workflow,
        &GatedInput {
            address: &prepared.address,
            title: prepared.title,
            section: &prepared.text,
            facts: &prepared.facts,
        },
        &mut model,
        &names_source,
    )?;
    let stats = outcome.stats;
    let Some(finish) = outcome.finish else {
        return Ok(ComposeOutcome {
            left_out: outcome.left_out,
            gated: Some(stats),
            ..ComposeOutcome::none("no_claim_admitted")
        });
    };
    let staged = stage_as(
        state,
        prepared,
        Composed {
            finish,
            left_out: outcome.left_out,
            reasons: BTreeMap::from([("gated_check", outcome.left_out)])
                .into_iter()
                .filter(|(_, count)| *count > 0)
                .collect(),
        },
        job_id,
        (
            gated_compose::DRAFT_RULES_ROLE,
            gated_compose::definition_fingerprint(),
        ),
    )?;
    Ok(ComposeOutcome {
        gated: Some(stats),
        ..staged
    })
}

/// One draft of a gated run: the area it is of, if any, and its outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GatedDraft {
    /// The draft's scope.
    pub(crate) scope: String,
    /// The area's name; none for the system draft.
    pub(crate) area: Option<String>,
    #[serde(flatten)]
    pub(crate) outcome: ComposeOutcome,
}

/// Compose one section through `draft-rules@1` at the system scope, area by
/// area (#2083): one draft per area the section holds a block for, scoped
/// to that area so each fits the draft bound, and one system draft of the
/// rest (the introduction and blocks that are no area's, such as shared
/// rejections) when it prints a fact. Without areas, one system draft.
pub(crate) fn compose_gated_by_area(
    state: &AppState,
    provider: &dyn LlmProvider,
    address: (&str, &str, &str),
    check: &dyn Fn() -> CallDirective,
    job_id: i64,
) -> Result<Vec<GatedDraft>, String> {
    let (catalogue_version, document, section) = address;
    let compiled = compile(
        state,
        catalogue_version,
        document,
        section,
        spec::SYSTEM_SCOPE,
    )?;
    let names: Vec<&str> = compiled
        .areas
        .iter()
        .map(|(_, name)| name.as_str())
        .collect();
    let (blocks, rest) = spec::area_blocks(&compiled.text, &names);
    let mut parts: Vec<(String, Option<String>, String)> = Vec::new();
    for (name, block) in &blocks {
        let (scope, _) = compiled
            .areas
            .iter()
            .find(|(_, area)| area == name)
            .expect("a block is cut for a named area");
        parts.push((
            scope.clone(),
            Some((*name).to_owned()),
            area_text(&compiled.text, block),
        ));
    }
    let whole = blocks.is_empty();
    parts.push((
        spec::SYSTEM_SCOPE.to_owned(),
        None,
        if whole { compiled.text.clone() } else { rest },
    ));
    let mut drafts = Vec::new();
    for (scope, area, text) in parts {
        let address = SectionAddress {
            scope: scope.clone(),
            ..compiled.address.clone()
        };
        let outcome = match prepared(&compiled, address, area.clone(), &text) {
            Ok(prepared) => compose_gated(state, provider, &prepared, check, job_id),
            // The rest of an arranged section may print no fact at all.
            Err(reason) if reason == "no_citable_fact" && !whole => continue,
            Err(reason) => Err(reason),
        };
        match outcome {
            Ok(outcome) => drafts.push(GatedDraft {
                scope,
                area,
                outcome,
            }),
            // Nothing was staged yet: the run failed as a whole.
            Err(error) if !drafts.iter().any(staged) => return Err(error),
            // Drafts of earlier areas are already staged under this job, so
            // the run ends here and reports them, with this part named as
            // where it stopped (a cancel, a provider failure).
            Err(_) => {
                drafts.push(GatedDraft {
                    scope,
                    area,
                    outcome: ComposeOutcome::none("run_stopped"),
                });
                break;
            }
        }
    }
    Ok(drafts)
}

fn staged(draft: &GatedDraft) -> bool {
    matches!(draft.outcome.outcome, DraftingOutcome::Staged { .. })
}

/// Compose §1.5 through the gated `draft-rules@1` workflow on the
/// configured local model, as a durable job, and stage the drafts (#2083,
/// AC-0980, AC-0981): at the system scope one per area and one of the rest
/// ([`compose_gated_by_area`]), at an area's scope that area's alone.
/// Nothing leaves the machine, so no consent is asked.
#[tauri::command]
pub(crate) async fn run_section_compose_gated(
    catalogue_version: String,
    document: String,
    section: String,
    scope: String,
    app: tauri::AppHandle,
) -> Result<Vec<GatedDraft>, String> {
    let state = app.state::<AppState>();
    let kind = format!("section-compose-gated:{document}:{section}:{scope}:local");
    let (running, execution) = crate::agent_pool::start_job(&app, &kind).await?;
    crate::emit_job(&app, &running);
    let worker = app.clone();
    let result = crate::off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = worker.state::<AppState>();
        let provider = gated_provider(&state)?;
        let check = || match crate::job_cancelled(&state, execution) {
            Ok(false) => CallDirective::Continue,
            Ok(true) => CallDirective::Cancelled,
            Err(_) => CallDirective::OwnershipLost,
        };
        if scope == spec::SYSTEM_SCOPE {
            return compose_gated_by_area(
                &state,
                provider.as_ref(),
                (&catalogue_version, &document, &section),
                &check,
                execution.id(),
            );
        }
        let prepared = prepare(&state, &catalogue_version, &document, &section, &scope)?;
        let outcome = compose_gated(&state, provider.as_ref(), &prepared, &check, execution.id())?;
        Ok(vec![GatedDraft {
            scope,
            area: prepared.area.clone(),
            outcome,
        }])
    })
    .await;
    let outcomes: Result<Vec<&ComposeOutcome>, String> = match &result {
        Ok(drafts) => Ok(drafts.iter().map(|draft| &draft.outcome).collect()),
        Err(error) => Err(error.clone()),
    };
    finish_job(&app, &state, &execution, &outcomes)?;
    result
}

fn finish_job(
    app: &tauri::AppHandle,
    state: &AppState,
    execution: &JobExecution,
    result: &Result<Vec<&ComposeOutcome>, String>,
) -> Result<(), String> {
    match result {
        Ok(outcomes) => {
            let artifacts: Vec<String> = outcomes
                .iter()
                .filter_map(|outcome| match &outcome.outcome {
                    DraftingOutcome::Staged { draft_id } => Some(draft_id.clone()),
                    DraftingOutcome::NoDraft { .. } => None,
                })
                .collect();
            let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
            // Why claims were left out stays on the job (#2349); a stage
            // that cannot be written does not fail the finished job.
            let _ = jobs.progress_execution(execution, &left_out_stage(outcomes), 100.0);
            let update = jobs
                .finish_execution(execution, &artifacts)
                .map_err(|e| e.to_string())?;
            drop(jobs);
            crate::emit_job(app, &crate::updated_job(update));
        }
        Err(error) => crate::report_failure(app, state, execution, error),
    }
    Ok(())
}

/// The job stage a finished composition records: claims staged and left
/// out, with the reasons (#2349), e.g. `composed: 12 staged, 4 left out
/// (citation_not_citable 3, number_not_cited 1)`.
fn left_out_stage(outcomes: &[&ComposeOutcome]) -> String {
    let staged: usize = outcomes.iter().map(|outcome| outcome.claims).sum();
    let left_out: usize = outcomes.iter().map(|outcome| outcome.left_out).sum();
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for (reason, count) in outcomes.iter().flat_map(|o| &o.left_out_reasons) {
        *reasons.entry(reason).or_default() += count;
    }
    let mut stage = format!("composed: {staged} staged, {left_out} left out");
    if !reasons.is_empty() {
        let listed: Vec<String> = reasons
            .iter()
            .map(|(reason, count)| format!("{reason} {count}"))
            .collect();
        stage.push_str(&format!(" ({})", listed.join(", ")));
    }
    stage
}

#[cfg(test)]
#[path = "section_compose_tests.rs"]
pub(crate) mod tests;
