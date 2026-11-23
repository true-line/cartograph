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
use agents::claim_review;
use agents::draft_store::{DraftOrigin, DraftStoreError};
use agents::gated_compose::{self, GatedInput, GatedStats, StageModel, Workflow};
use agents::rule_scope;
use agents::section_compose::{
    self, ComposeAttested, ComposeFact, ComposeGroup, ComposeInput, Composed, SECTION_COMPOSER_ROLE,
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
    /// The attested records the composer was handed, by `A-` id (ADR-0090
    /// decision 6).
    attested: BTreeMap<String, HandedAttested>,
}

/// An attested record a composition was handed.
#[derive(Debug, Clone)]
struct HandedAttested {
    /// Its identity: the hash a draft citing it records.
    identity: String,
    /// `attested`, or `agent-attested, unconfirmed`.
    label: String,
    /// Its statement: what a claim citing it is checked against.
    statement: String,
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
    /// Whether the claims were reviewed before staging (#2419):
    /// `reviewed`, `review_unavailable` (the reply was not the schema's),
    /// `review_too_large` (one claim alone was over the input ceiling; more
    /// claims than one request holds are reviewed in batches, #1748) or
    /// `not_reviewed_cloud` (a cloud consent covers one payload).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) review: Option<&'static str>,
    /// Whether the composer was asked once more (#2449): `kept_first` or
    /// `kept_retry`, the screened reply that kept more claims.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) retry: Option<&'static str>,
    /// Whether the claims citing rules had their rules' scope checked
    /// (#2482): `checked`, `none_cite_rules`, `unavailable` (fewer than two
    /// readable replies) or `too_large` (over the input ceiling).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rule_scope: Option<&'static str>,
}

impl ComposeOutcome {
    fn none(reason: &'static str) -> Self {
        Self {
            outcome: DraftingOutcome::NoDraft { reason },
            claims: 0,
            left_out: 0,
            left_out_reasons: BTreeMap::new(),
            gated: None,
            review: None,
            retry: None,
            rule_scope: None,
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
        // The longest rule excerpt whose request stays under the input
        // ceiling, or none (#2386).
        let ceiling = limits()?
            .values()
            .input_bytes
            .saturating_sub(INPUT_MARGIN_BYTES);
        // Each excerpt with the citation schema listing the citable ids,
        // then with one that does not (#2399): the prompt prints every id
        // already, so the rules come before the list.
        for (rules, listed) in attempts(compiled.excerpts.len()) {
            let text = match rules {
                Some(at) => format!("{}{}", compiled.text, compiled.excerpts[at]),
                None => compiled.text.clone(),
            };
            let candidate = prepared(&compiled, compiled.address.clone(), None, &text, listed)?;
            if rules.is_none() || payload_bytes(&candidate.action) <= ceiling {
                return Ok(candidate);
            }
        }
        unreachable!("the attempts end with no excerpt");
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
        true,
    )
}

/// The order a system composition tries its `excerpts` rule excerpts in,
/// as (excerpt index, citation ids listed): each excerpt, longest first,
/// with the ids listed and then not (#2399), and last no excerpt with the
/// ids listed, which is sent whatever its size.
fn attempts(excerpts: usize) -> Vec<(Option<usize>, bool)> {
    (0..excerpts)
        .flat_map(|at| [(Some(at), true), (Some(at), false)])
        .chain([(None, true)])
        .collect()
}

/// The citation ids of the citable Capability facts, by name (#2416).
fn capability_ids(facts: &[ComposeFact]) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut ids: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for fact in facts.iter().filter(|fact| fact.label == "Capability") {
        if let Some(name) = fact.name.as_deref() {
            ids.entry(name)
                .or_default()
                .insert(fact.citation_id.as_str());
        }
    }
    ids
}

/// Whether `citations` cite, for each excerpt rule among them, a Capability
/// fact named like one of the capabilities the rule is listed under (#2416).
/// A rule none of whose capabilities a citable fact is named is not checked.
fn rule_capability_cited(
    citations: &[String],
    rule_capability: &BTreeMap<String, BTreeSet<String>>,
    capabilities: &BTreeMap<&str, BTreeSet<&str>>,
) -> bool {
    citations.iter().all(|id| {
        let ids: BTreeSet<&str> = rule_capability
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|capability| capabilities.get(capability.as_str()))
            .flatten()
            .copied()
            .collect();
        ids.is_empty() || citations.iter().any(|cited| ids.contains(cited.as_str()))
    })
}

/// Room kept under the input ceiling for what the call adds around the
/// payload (#2386).
const INPUT_MARGIN_BYTES: u64 = 4 * 1024;

/// The bytes of `action`'s payload as JSON, which the input ceiling bounds.
fn payload_bytes(action: &CompletionAction) -> u64 {
    serde_json::to_vec(&action.payload).map_or(u64::MAX, |bytes| bytes.len() as u64)
}

/// One read of the graph, the decisions and the knowledge overlay, and the
/// section compiled from it.
struct Compiled {
    address: SectionAddress,
    title: &'static str,
    /// The section as a composition reads it.
    text: String,
    /// The rule excerpts a system composition may append, longest first
    /// (#2375, #2386).
    excerpts: Vec<String>,
    nodes: Arc<Vec<Node>>,
    edges: Arc<Vec<Edge>>,
    /// The overlay's Capabilities and Actors with the base facts each
    /// rests on; citable as themselves (#2040).
    derived: Vec<core_graph::overlay::OverlayFact>,
    overlay: Arc<Vec<Node>>,
    /// Each named Subsystem as (scope, name): its citation id and its name,
    /// in id order, each name once.
    areas: Vec<(String, String)>,
    /// The attested records the projection may print: `eligible` or
    /// `agent-attested, unconfirmed` (ADR-0090 decision 6).
    attested: Vec<context_hub::curated::CuratedAttested>,
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
    let inputs = read.inputs(Some(&review));
    let attested = match &inputs.curated {
        crate::CuratedInput::Computed(projection) => projection
            .attested
            .iter()
            .filter(|record| record.printed_label().is_some())
            .cloned()
            .collect(),
        crate::CuratedInput::NotRead | crate::CuratedInput::NotComputed(_) => Vec::new(),
    };
    let artifact =
        crate::compile_spec_artifact(inputs, spec::ExportMode::BestEffort, D1_ARTIFACT_ID)?;
    // An overview section is read up to its per-capability detail (#1991);
    // a summary section reads the sections it summarises (#2319).
    let text = spec::composition_source(&artifact.content, &address.section)
        .ok_or("section_not_compiled")?;
    // A system overview also reads the rules its capabilities enforce, so
    // what it says a capability does can cite them (#2375): the whole
    // excerpt, then shorter ones for a request near its input ceiling
    // (#2386).
    let mut excerpts: Vec<String> = Vec::new();
    if address.scope == spec::SYSTEM_SCOPE
        && let Some(budget) = spec::rule_excerpt_budget(&address.section)
    {
        for share in [1, 2, 4, 8] {
            let bytes = budget / share;
            if let Some(rules) =
                spec::rule_excerpt_within(&artifact.content, &address.section, bytes)
                && excerpts.last() != Some(&rules)
            {
                excerpts.push(rules);
            }
        }
    }
    Ok(Compiled {
        address,
        title,
        text,
        excerpts,
        nodes,
        edges,
        derived,
        overlay,
        areas,
        attested,
    })
}

/// The attested records of `compiled` a composition at `address` may cite:
/// those answering this section for this scope, and those answering a Gap
/// among `citable`, the facts the composition is handed.
fn attested_in_scope<'a>(
    compiled: &'a Compiled,
    address: &SectionAddress,
    citable: &BTreeSet<String>,
) -> Vec<&'a context_hub::curated::CuratedAttested> {
    use context_hub::curated::AttestedAnswer;
    compiled
        .attested
        .iter()
        .filter(|record| match &record.answer {
            AttestedAnswer::Section {
                catalogue_version,
                document,
                section,
                scope,
            } => {
                (
                    catalogue_version.as_str(),
                    document.as_str(),
                    section.as_str(),
                    scope.as_str(),
                ) == (
                    address.catalogue_version.as_str(),
                    address.document.as_str(),
                    address.section.as_str(),
                    address.scope.as_str(),
                )
            }
            AttestedAnswer::Gap { gap_id, .. } => {
                citable.contains(&spec::citation_id(&core_graph::FactKey::Node {
                    id: gap_id.clone(),
                }))
            }
        })
        .collect()
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
    listed: bool,
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
    let mut citable = picked.citable();
    let in_scope = attested_in_scope(compiled, &address, &citable);
    let attested: Vec<ComposeAttested> = in_scope
        .iter()
        .filter_map(|record| {
            Some(ComposeAttested {
                citation_id: record.citation_id.clone(),
                label: record.printed_label()?.to_owned(),
                statement: record.statement.clone(),
            })
        })
        .collect();
    let input = ComposeInput {
        address: &address,
        title: compiled.title,
        section: text,
        facts: &facts,
        groups: &groups,
        attested: &attested,
    };
    let action_id = format!(
        "section-compose:{}:{}:{}",
        address.document, address.section, address.scope
    );
    let action = if listed {
        section_compose::action(&input, action_id)
    } else {
        section_compose::action_unlisted(&input, action_id)
    };
    let attested: BTreeMap<String, HandedAttested> = in_scope
        .iter()
        .map(|record| {
            let handed = HandedAttested {
                identity: record.record_id.clone(),
                label: record.printed_label().unwrap_or_default().to_owned(),
                statement: record.statement.clone(),
            };
            (record.citation_id.clone(), handed)
        })
        .collect();
    citable.extend(attested.keys().cloned());
    Ok(Prepared {
        address,
        action,
        citable,
        nodes: Arc::clone(nodes),
        edges: Arc::clone(edges),
        overlay: Arc::clone(&compiled.overlay),
        area,
        title: compiled.title,
        text: text.to_owned(),
        facts,
        attested,
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

/// Leave out each claim of `composed` a staging check refuses, counting it
/// under the check's reason (SPEC-16 rule 3, #2328, #2349, #2371, #2416).
/// The reason that left out the last claim, if every claim is left out.
/// Running it again on what it kept leaves out nothing more.
fn screen(prepared: &Prepared, composed: &mut Composed) -> Option<&'static str> {
    let (nodes, edges) = (&prepared.nodes, &prepared.edges);
    // SPEC-16 rule 3 (AC-0683): a statement naming a source identifier it
    // cites is not staged.
    let cited: BTreeSet<String> = composed
        .finish
        .claims
        .iter()
        .flat_map(|claim| claim.citations.iter().cloned())
        .collect();
    let source = spec::SourceIdentifiers::of(nodes, edges, &cited);
    // A route a statement spells out must be shown by a fact it cites
    // (#2328), not taken from the section text around it; so must a figure
    // or version, and a claim about endpoints cites one (#2349).
    let shown = cited_text(prepared, &cited);
    // A rule from the excerpt is stated for the capability it is listed
    // under, so a claim citing it cites that capability too (#2416).
    let rule_capability = spec::excerpt_rule_capabilities(&prepared.text);
    let capabilities = capability_ids(&prepared.facts);
    type Check<'a> = (&'static str, &'a dyn Fn(&Claim) -> bool);
    let checks: [Check<'_>; 7] = [
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
        // A platform a claim names is named by a fact it cites (AC-1342).
        ("platform_not_cited", &|claim| {
            shown
                .uncited_platform(&claim.statement, &claim.citations)
                .is_none()
        }),
        // A claim asserts no more than its cited facts show (#2371).
        ("claim_not_supported", &|claim| {
            !shown
                .support(&claim.statement, &claim.citations)
                .overreaches()
        }),
        ("rule_capability_not_cited", &|claim| {
            rule_capability_cited(&claim.citations, &rule_capability, &capabilities)
        }),
    ];
    for (reason, keep) in checks {
        let counts = (&mut composed.left_out, &mut composed.reasons);
        if leave_out(&mut composed.finish, counts, reason, keep) {
            return Some(reason);
        }
    }
    None
}

/// The staging checks whose misses a second sample, told of them, can avoid
/// (#2449): the request already holds the facts such a claim needs.
const RETRY_REASONS: [&str; 3] = ["route_not_cited", "number_not_cited", "endpoint_not_cited"];

/// How many claims [`RETRY_REASONS`] must leave out before the composer is
/// asked once more.
const RETRY_MIN_LEFT_OUT: usize = 2;

/// [`screen`] `composed`, and on a local provider whose reply lost at least
/// [`RETRY_MIN_LEFT_OUT`] claims to [`RETRY_REASONS`], ask once more with
/// [`section_compose::corrected`] and keep the screened reply with more
/// claims, the first on a tie, an unreadable or failed retry, or one over
/// the input ceiling (#2449). A cloud consent covers one payload, so a
/// cloud composition is not asked again.
/// Returns the reply kept, why it has no claim left, and whether a retry
/// ran.
fn screen_or_retry(
    state: &AppState,
    provider: &dyn LlmProvider,
    prepared: &Prepared,
    mut composed: Composed,
    check: &dyn Fn() -> CallDirective,
    job_id: Option<i64>,
) -> Result<(Composed, Option<&'static str>, Option<&'static str>), String> {
    let before: usize = RETRY_REASONS
        .iter()
        .filter_map(|reason| composed.reasons.get(reason))
        .sum();
    let reason = screen(prepared, &mut composed);
    let avoidable = RETRY_REASONS
        .iter()
        .filter_map(|reason| composed.reasons.get(reason))
        .sum::<usize>()
        - before;
    if provider.locality() != Locality::Local || avoidable < RETRY_MIN_LEFT_OUT {
        return Ok((composed, reason, None));
    }
    let action = section_compose::corrected(&prepared.action);
    // The retry is optional: a request the correction puts over the input
    // ceiling is not sent, and a failed call keeps the first reply unless
    // the job was cancelled or lost.
    let ceiling = limits()?
        .values()
        .input_bytes
        .saturating_sub(INPUT_MARGIN_BYTES);
    if payload_bytes(&action) > ceiling {
        return Ok((composed, reason, Some("kept_first")));
    }
    let text = match complete(state, provider, &action, None, check, job_id) {
        Ok(completion) => completion.text,
        Err(error) if check() != CallDirective::Continue => return Err(error),
        Err(_) => return Ok((composed, reason, Some("kept_first"))),
    };
    let Ok(mut second) = section_compose::decode(&prepared.address, &text, &prepared.citable)
    else {
        return Ok((composed, reason, Some("kept_first")));
    };
    let second_reason = screen(prepared, &mut second);
    if second.finish.claims.len() > composed.finish.claims.len() {
        Ok((second, second_reason, Some("kept_retry")))
    } else {
        Ok((composed, reason, Some("kept_first")))
    }
}

/// [`stage`] recording `role`, its definition fingerprint, as the author.
fn stage_as(
    state: &AppState,
    prepared: &Prepared,
    composed: Composed,
    job_id: i64,
    role: (&str, String),
) -> Result<ComposeOutcome, String> {
    let mut composed = composed;
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
    if let Some(reason) = screen(prepared, &mut composed) {
        return Ok(ComposeOutcome {
            left_out: composed.left_out,
            left_out_reasons: composed.reasons,
            ..ComposeOutcome::none(reason)
        });
    }
    let Composed {
        finish,
        left_out,
        reasons,
    } = composed;
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
    // An attested record the composer was handed is cited by its `A-` id,
    // with its identity as the hash the projection compares (ADR-0090
    // decision 6).
    for id in &cited {
        if let Some(handed) = prepared.attested.get(id) {
            citation_set.insert(id.clone(), handed.identity.clone());
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
            review: None,
            retry: None,
            rule_scope: None,
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

pub(crate) fn run_blocking(
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
        Ok(composed) => {
            // The deterministic checks run first, so each claim they refuse
            // keeps its own reason; only what passes them is reviewed.
            let (composed, reason, retry) = screen_or_retry(
                state,
                provider,
                &prepared,
                composed,
                &check,
                Some(execution.id()),
            )?;
            if let Some(reason) = reason {
                return Ok(ComposeOutcome {
                    left_out: composed.left_out,
                    left_out_reasons: composed.reasons,
                    retry,
                    ..ComposeOutcome::none(reason)
                });
            }
            let (composed, review, rule_scope) = review(
                state,
                provider,
                &prepared,
                composed,
                &check,
                Some(execution.id()),
            )?;
            let outcome = if composed.finish.claims.is_empty() {
                ComposeOutcome {
                    left_out: composed.left_out,
                    left_out_reasons: composed.reasons,
                    ..ComposeOutcome::none("claim_overreaches")
                }
            } else {
                stage(state, &prepared, composed, execution.id())?
            };
            Ok(ComposeOutcome {
                review: Some(review),
                retry,
                rule_scope,
                ..outcome
            })
        }
        Err(reason) => Ok(ComposeOutcome::none(reason)),
    }
}

/// Review the composed claims against the facts they cite (#2419): the same
/// bounded call of `claim-reviewer@1` on a local provider, asked
/// [`claim_review::REVIEW_SAMPLES`] times, and each claim a majority of the
/// readable replies judges an overreach left out as `claim_overreaches`
/// (#2458). A cloud provider is not called again, since its consent covers
/// the composition's payload alone; fewer than two readable replies, or a
/// request over the input ceiling, leave the claims as they are. Returns
/// the claims kept, how the review went and, on a local provider, how the
/// rule-scope check went ([`check_rule_scope`]).
pub(crate) fn review(
    state: &AppState,
    provider: &dyn LlmProvider,
    prepared: &Prepared,
    mut composed: Composed,
    check: &dyn Fn() -> CallDirective,
    job_id: Option<i64>,
) -> Result<(Composed, &'static str, Option<&'static str>), String> {
    if provider.locality() != Locality::Local {
        return Ok((composed, "not_reviewed_cloud", None));
    }
    if composed.finish.claims.is_empty() {
        return Ok((composed, "reviewed", None));
    }
    let ceiling = limits()?
        .values()
        .input_bytes
        .saturating_sub(INPUT_MARGIN_BYTES);
    let calls = Calls {
        state,
        provider,
        check,
        job_id,
        ceiling,
    };
    let status = review_claims(&calls, prepared, &mut composed)?;
    let scope = if composed.finish.claims.is_empty() {
        None
    } else {
        Some(check_rule_scope(&calls, prepared, &mut composed)?)
    };
    Ok((composed, status, scope))
}

/// What a check after composing calls the provider with.
struct Calls<'a> {
    state: &'a AppState,
    provider: &'a dyn LlmProvider,
    check: &'a dyn Fn() -> CallDirective,
    job_id: Option<i64>,
    /// The input ceiling a request must keep under to be sent.
    ceiling: u64,
}

impl Calls<'_> {
    /// Ask `action` `times` times, decoding each reply; a failed call is an
    /// unreadable sample, unless the job was cancelled or lost.
    fn samples<T>(
        &self,
        action: &CompletionAction,
        times: usize,
        decode: impl Fn(&str) -> Option<T>,
    ) -> Result<Vec<Option<T>>, String> {
        let mut samples = Vec::with_capacity(times);
        for _ in 0..times {
            samples.push(
                match complete(
                    self.state,
                    self.provider,
                    action,
                    None,
                    self.check,
                    self.job_id,
                ) {
                    Ok(completion) => decode(&completion.text),
                    Err(error) if (self.check)() != CallDirective::Continue => return Err(error),
                    Err(_) => None,
                },
            );
        }
        Ok(samples)
    }
}

/// Cut `0..claims` into contiguous batches that each `fit`, halving a batch
/// that does not (#1748). `None` when one claim alone does not fit.
fn review_batches(
    claims: usize,
    fit: &dyn Fn(std::ops::Range<usize>) -> bool,
) -> Option<Vec<std::ops::Range<usize>>> {
    let mut pending: Vec<std::ops::Range<usize>> = std::iter::once(0..claims).collect();
    let mut batches = Vec::new();
    while let Some(batch) = pending.pop() {
        if fit(batch.clone()) {
            batches.push(batch);
        } else if batch.len() <= 1 {
            return None;
        } else {
            let middle = batch.start + batch.len() / 2;
            // Popped last first, so the batches come out in order.
            pending.push(middle..batch.end);
            pending.push(batch.start..middle);
        }
    }
    Some(batches)
}

/// Ask the reviewer [`claim_review::REVIEW_SAMPLES`] times and leave out
/// each claim a majority judges an overreach. Claims too many for one
/// request under the input ceiling are reviewed in contiguous batches that
/// each fit, every batch asked the same way (#1748); only a claim that does
/// not fit alone leaves the review undone (`review_too_large`), and a batch
/// with fewer than two readable replies leaves every claim as it is.
fn review_claims(
    calls: &Calls<'_>,
    prepared: &Prepared,
    composed: &mut Composed,
) -> Result<&'static str, String> {
    let claims = &composed.finish.claims;
    let action_of = |batch: &std::ops::Range<usize>, batched: bool| {
        let mut action = review_action(prepared, &claims[batch.clone()]);
        if batched {
            action.action_id = format!(
                "{}:claims-{}-{}",
                action.action_id,
                batch.start + 1,
                batch.end
            );
        }
        action
    };
    let whole = 0..claims.len();
    let batches = if payload_bytes(&action_of(&whole, false)) <= calls.ceiling {
        vec![whole]
    } else {
        // A request over the input ceiling is not sent.
        let fit = |batch: std::ops::Range<usize>| {
            payload_bytes(&action_of(&batch, true)) <= calls.ceiling
        };
        match review_batches(claims.len(), &fit) {
            Some(batches) => batches,
            None => return Ok("review_too_large"),
        }
    };
    let batched = batches.len() > 1;
    let mut overreaching = Vec::with_capacity(claims.len());
    for batch in &batches {
        let action = action_of(batch, batched);
        // One sample's verdicts vary between identical calls, so the review
        // is asked several times and a claim left out on a majority (#2458).
        let size = batch.len();
        let samples: Vec<_> = calls
            .samples(&action, claim_review::REVIEW_SAMPLES, |text| {
                claim_review::decode(text, size)
            })?
            .into_iter()
            .map(Option::unwrap_or_default)
            .collect();
        let Some(verdicts) = claim_review::overreaching(&samples, size) else {
            return Ok("review_unavailable");
        };
        overreaching.extend(verdicts);
    }
    let mut overreaching = overreaching.into_iter();
    let before = composed.finish.claims.len();
    composed
        .finish
        .claims
        .retain(|_| overreaching.next() != Some(true));
    let dropped = before - composed.finish.claims.len();
    if dropped > 0 {
        composed.left_out += dropped;
        *composed.reasons.entry("claim_overreaches").or_default() += dropped;
    }
    Ok("reviewed")
}

/// Check the scope of each rule a claim cites (#2482, #2495): `rule-scope@3`
/// judges, for each claim and cited rule, whether the claim states the
/// rule beyond the capabilities it is listed under (or under which an
/// identical rule is listed), asked [`rule_scope::SCOPE_SAMPLES`] times,
/// and a claim a majority reads as widening any of its rules is left out
/// as `rule_scope_widened`. A rule none of whose capabilities is a citable
/// fact is not checked, nor a claim citing no other rule. Identical rules a
/// claim cites are one pair, and a rule [`identical_rule_settled`] for the
/// claim is not asked (#1752 re-check 12).
fn check_rule_scope(
    calls: &Calls<'_>,
    prepared: &Prepared,
    composed: &mut Composed,
) -> Result<&'static str, String> {
    let rules = spec::excerpt_rules(&prepared.text);
    let by_name = capability_ids(&prepared.facts);
    // The capabilities a rule holds for, by name: those it is listed under,
    // and those an identical rule is listed under. A rule none of whose
    // capabilities is a citable fact has nothing to hold for.
    let holds_for = |text: &str| -> Vec<&str> {
        let names: BTreeSet<&str> = rules
            .values()
            .filter(|(_, other)| other == text)
            .flat_map(|(names, _)| names)
            .map(String::as_str)
            .collect();
        if names.iter().any(|name| by_name.contains_key(name)) {
            names.into_iter().collect()
        } else {
            Vec::new()
        }
    };
    let capabilities: BTreeSet<&str> = by_name.values().flatten().copied().collect();
    let mut checked = Vec::new();
    let mut claims = Vec::new();
    let mut settled = false;
    for (at, claim) in composed.finish.claims.iter().enumerate() {
        let cites: BTreeSet<&str> = claim.citations.iter().map(String::as_str).collect();
        let mut cited: Vec<rule_scope::ScopeRule<'_>> = Vec::new();
        for id in &claim.citations {
            let Some((_, text)) = rules.get(id) else {
                continue;
            };
            // Identical rules the claim cites are one pair (#1752 re-check 12).
            if cited.iter().any(|rule| rule.text == text.as_str()) {
                continue;
            }
            let holds_for = holds_for(text);
            if holds_for.is_empty() {
                continue;
            }
            if identical_rule_settled(&rules, &by_name, &capabilities, &cites, text, &holds_for) {
                settled = true;
                continue;
            }
            cited.push(rule_scope::ScopeRule {
                id,
                text,
                holds_for,
            });
        }
        if cited.is_empty() {
            continue;
        }
        claims.push(rule_scope::ScopeClaim {
            statement: &claim.statement,
            citations: &claim.citations,
            rules: cited,
        });
        checked.push(at);
    }
    if claims.is_empty() {
        return Ok(if settled {
            "settled"
        } else {
            "none_cite_rules"
        });
    }
    let cited: BTreeSet<String> = claims
        .iter()
        .flat_map(|claim| claim.citations.iter().cloned())
        .collect();
    let action = rule_scope::action(
        &claims,
        &cited_facts(prepared, &cited),
        format!(
            "rule-scope:{}:{}:{}",
            prepared.address.document, prepared.address.section, prepared.address.scope
        ),
    );
    if payload_bytes(&action) > calls.ceiling {
        return Ok("too_large");
    }
    let samples = calls.samples(&action, rule_scope::SCOPE_SAMPLES, |text| {
        rule_scope::decode(text, &claims)
    })?;
    let Some(widened) = rule_scope::widened_claims(&samples) else {
        return Ok("unavailable");
    };
    let dropped: BTreeSet<usize> = checked
        .into_iter()
        .zip(widened)
        .filter_map(|(at, widened)| widened.then_some(at))
        .collect();
    if !dropped.is_empty() {
        let mut at = 0;
        composed.finish.claims.retain(|_| {
            at += 1;
            !dropped.contains(&(at - 1))
        });
        composed.left_out += dropped.len();
        *composed.reasons.entry("rule_scope_widened").or_default() += dropped.len();
    }
    Ok("checked")
}

/// Whether a claim citing `cites` states the rule `text` only for the
/// capabilities it `holds_for`, without asking (#1752 re-check 12): the rule is listed
/// identically under two or more citable capabilities, the claim cites every
/// such copy, and every Capability fact the claim cites is one of those the
/// rule holds for. On re-check 12 the model read such a claim (the empty-body
/// 400 of both API sign-in capabilities, stated for API sign-in) as widened
/// on two of three samples.
fn identical_rule_settled(
    rules: &BTreeMap<String, (BTreeSet<String>, String)>,
    by_name: &BTreeMap<&str, BTreeSet<&str>>,
    capabilities: &BTreeSet<&str>,
    cites: &BTreeSet<&str>,
    text: &str,
    holds_for: &[&str],
) -> bool {
    let copies: Vec<&str> = rules
        .iter()
        .filter(|(_, (names, other))| {
            other == text && names.iter().any(|name| by_name.contains_key(name.as_str()))
        })
        .map(|(id, _)| id.as_str())
        .collect();
    let allowed: BTreeSet<&str> = holds_for
        .iter()
        .filter_map(|name| by_name.get(name))
        .flatten()
        .copied()
        .collect();
    let named: Vec<&&str> = cites
        .iter()
        .filter(|id| capabilities.contains(*id))
        .collect();
    copies.len() >= 2
        && copies.iter().all(|id| cites.contains(id))
        && !named.is_empty()
        && named.iter().all(|id| allowed.contains(**id))
}

/// The reviewer's action for `claims` of `prepared`: each cited fact's
/// indexed text, and for a rule of the excerpt the capability it is listed
/// under and the rule in domain terms (#2419).
fn review_action(prepared: &Prepared, claims: &[Claim]) -> CompletionAction {
    let cited: BTreeSet<String> = claims
        .iter()
        .flat_map(|claim| claim.citations.iter().cloned())
        .collect();
    let facts = cited_facts(prepared, &cited);
    let review: Vec<claim_review::ReviewClaim<'_>> = claims
        .iter()
        .map(|claim| claim_review::ReviewClaim {
            statement: &claim.statement,
            citations: &claim.citations,
        })
        .collect();
    claim_review::action(
        &review,
        &facts,
        format!(
            "claim-review:{}:{}:{}",
            prepared.address.document, prepared.address.section, prepared.address.scope
        ),
    )
}

/// What each `cited` fact of `prepared` shows, and each cited attested
/// record its statement (ADR-0090 decision 6).
fn cited_text(prepared: &Prepared, cited: &BTreeSet<String>) -> spec::CitedText {
    let mut shown = spec::CitedText::of(&prepared.nodes, &prepared.edges, &prepared.overlay, cited);
    for (id, handed) in &prepared.attested {
        if cited.contains(id) {
            shown.insert_attested(id, &handed.statement);
        }
    }
    shown
}

/// The text of each `cited` fact of `prepared` by citation id: its indexed
/// text, and for a rule of the excerpt the capability it is listed under
/// and the rule in domain terms (#2419).
fn cited_facts(prepared: &Prepared, cited: &BTreeSet<String>) -> BTreeMap<String, String> {
    let shown = cited_text(prepared, cited);
    let rules = spec::excerpt_rules(&prepared.text);
    cited
        .iter()
        .map(|id| {
            let text = match rules.get(id) {
                Some((capabilities, rule)) => {
                    let capabilities: Vec<String> = capabilities
                        .iter()
                        .map(|name| format!("\"{name}\""))
                        .collect();
                    format!("Rule of capability {}: {rule}", capabilities.join(", "))
                }
                None => shown.text(id).unwrap_or_default().to_owned(),
            };
            (id.clone(), text)
        })
        .collect()
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
    // The attested records the section was handed are drafted after its
    // rule rows (ADR-0090 decision 6).
    let attested: Vec<ComposeAttested> = prepared
        .attested
        .iter()
        .map(|(id, handed)| ComposeAttested {
            citation_id: id.clone(),
            label: handed.label.clone(),
            statement: handed.statement.clone(),
        })
        .collect();
    let outcome = gated_compose::run(
        &workflow,
        &GatedInput {
            address: &prepared.address,
            title: prepared.title,
            section: &prepared.text,
            facts: &prepared.facts,
            attested: &attested,
        },
        &mut model,
        &names_source,
    )?;
    let stats = outcome.stats;
    let Some(finish) = outcome.finish else {
        return Ok(ComposeOutcome {
            left_out: outcome.left_out,
            left_out_reasons: gated_reasons(outcome.left_out),
            gated: Some(stats),
            ..ComposeOutcome::none("no_claim_admitted")
        });
    };
    let mut composed = Composed {
        finish,
        left_out: outcome.left_out,
        reasons: gated_reasons(outcome.left_out),
    };
    // The staging checks, then the claim review, as for a one-call
    // composition (#2419).
    if let Some(reason) = screen(prepared, &mut composed) {
        return Ok(ComposeOutcome {
            left_out: composed.left_out,
            left_out_reasons: composed.reasons,
            gated: Some(stats),
            ..ComposeOutcome::none(reason)
        });
    }
    let (composed, review, rule_scope) =
        review(state, provider, prepared, composed, check, Some(job_id))?;
    let staged = if composed.finish.claims.is_empty() {
        ComposeOutcome {
            left_out: composed.left_out,
            left_out_reasons: composed.reasons,
            ..ComposeOutcome::none("claim_overreaches")
        }
    } else {
        stage_as(
            state,
            prepared,
            composed,
            job_id,
            (
                gated_compose::DRAFT_RULES_ROLE,
                gated_compose::definition_fingerprint(),
            ),
        )?
    };
    Ok(ComposeOutcome {
        gated: Some(stats),
        review: Some(review),
        rule_scope,
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
    let mut first_failure = None;
    for (scope, area, text) in parts {
        let address = SectionAddress {
            scope: scope.clone(),
            ..compiled.address.clone()
        };
        let watched = Watched::new(provider);
        let outcome = match prepared(&compiled, address, area.clone(), &text, true) {
            Ok(prepared) => compose_gated(state, &watched, &prepared, check, job_id),
            // The rest of an arranged section may print no fact at all.
            Err(reason) if reason == "no_citable_fact" && !whole => continue,
            Err(reason) => Err(reason),
        };
        let error = match outcome {
            Ok(outcome) => {
                drafts.push(GatedDraft {
                    scope,
                    area,
                    outcome,
                });
                continue;
            }
            Err(error) => error,
        };
        let provider_failed = watched.last_call_failed();
        match after_failure(drafts.iter().any(staged), provider_failed, check()) {
            // Nothing was staged yet: the run failed as a whole.
            AfterFailure::Fail => return Err(error),
            // Drafts of earlier areas are already staged under this job, so
            // the run ends here and reports them, with this part named as
            // where it stopped.
            AfterFailure::Stop => {
                drafts.push(GatedDraft {
                    scope,
                    area,
                    outcome: ComposeOutcome::none("run_stopped"),
                });
                break;
            }
            // One area's failure is that area's: the next is drafted (#1748).
            AfterFailure::Next => {
                first_failure.get_or_insert(error);
                drafts.push(GatedDraft {
                    scope,
                    area,
                    outcome: ComposeOutcome::none("part_failed"),
                });
            }
        }
    }
    match first_failure {
        // Every part failed or staged nothing: the run failed as a whole.
        Some(error) if !drafts.iter().any(staged) => Err(error),
        _ => Ok(drafts),
    }
}

/// What a gated run by area does when one part's composition fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterFailure {
    /// Fail the run: nothing is staged under its job.
    Fail,
    /// End the run, reporting the drafts staged so far.
    Stop,
    /// Record the part as failed and draft the next.
    Next,
}

/// A failure whose last model call failed (`provider_failed`) while the
/// job goes on (`directive`, read after the failure) is the failing part's
/// alone (#1748). A cancel, lost ownership or host failure (the graph, the
/// draft store) ends the run, which fails as a whole when nothing was
/// `staged_any`.
fn after_failure(
    staged_any: bool,
    provider_failed: bool,
    directive: CallDirective,
) -> AfterFailure {
    match (directive, provider_failed, staged_any) {
        (CallDirective::Continue, true, _) => AfterFailure::Next,
        (_, _, false) => AfterFailure::Fail,
        (_, _, true) => AfterFailure::Stop,
    }
}

/// A provider that remembers whether its last model call failed, so a gated
/// run by area tells a model failure from a host one (#1748).
struct Watched<'a> {
    inner: &'a dyn LlmProvider,
    last_failed: std::sync::atomic::AtomicBool,
}

impl<'a> Watched<'a> {
    fn new(inner: &'a dyn LlmProvider) -> Self {
        Self {
            inner,
            last_failed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn last_call_failed(&self) -> bool {
        self.last_failed.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn noted<T, E>(&self, result: Result<T, E>) -> Result<T, E> {
        self.last_failed
            .store(result.is_err(), std::sync::atomic::Ordering::SeqCst);
        result
    }
}

impl LlmProvider for Watched<'_> {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn locality(&self) -> Locality {
        self.inner.locality()
    }
    fn capabilities(&self) -> llm::ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, batch: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
        self.inner.embed(batch)
    }
    fn embedding_cache_identity(
        &self,
    ) -> Result<Option<llm::EmbeddingIdentity>, llm::ProviderError> {
        self.inner.embedding_cache_identity()
    }
    fn embedding_model_id(&self) -> String {
        self.inner.embedding_model_id()
    }
    fn completion_model_id(&self) -> String {
        self.inner.completion_model_id()
    }
    fn complete(
        &self,
        request: &llm::ProviderCompletionRequest,
    ) -> Result<llm::Completion, llm::ProviderError> {
        self.noted(self.inner.complete(request))
    }
    fn supports_bounded_response_schema(&self) -> bool {
        self.inner.supports_bounded_response_schema()
    }
    fn bounded_profile(
        &self,
    ) -> Result<llm::bounded::ProviderProfile, llm::bounded::BoundedCallError> {
        self.inner.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &llm::bounded::AuthorizedBoundedCompletion,
        control: &llm::bounded::CompletionControl<'_>,
    ) -> Result<llm::bounded::BoundedCompletion, llm::bounded::BoundedCallError> {
        self.noted(self.inner.complete_bounded(request, control))
    }
    fn proxy_credential(&self) -> Option<llm::UpstreamCredential> {
        self.inner.proxy_credential()
    }
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

/// The reason a gated composition's `left_out` claims are counted under:
/// they failed one of its gates (#2349).
fn gated_reasons(left_out: usize) -> BTreeMap<&'static str, usize> {
    BTreeMap::from([("gated_check", left_out)])
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .collect()
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

#[cfg(all(test, not(app_test_group = "acceptance")))]
#[path = "section_compose_tests.rs"]
pub(crate) mod tests;
