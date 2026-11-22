//! Knowledge phase after ingest (ADR-0051 decision 3, #1441).
//!
//! Once an ingest has committed the deterministic base, the knowledge units
//! of that base, and of the flows traced from it, are embedded through the
//! local provider and the knowledge
//! overlay is republished for the same base revision, recording in its
//! recipe whether the provider phase ran and which embedding namespace it
//! used. The phase never fails or delays the ingest: every outcome,
//! including no local provider, is reported as a named result.
//!
//! The deterministic knowledge producers (#1443) derive their facts from the
//! same snapshot and are published with it; they need no provider.
use core_graph::overlay::{
    MAX_BYTES, MAX_FACTS, OverlayError, OverlayFact, OverlayRecipe, OverlayStore,
};
use core_graph::{RevisionedSnapshot, SnapshotReadLimits, SqliteGraphStore};
use llm::LlmProvider;
use semantic::embedding_cache::EmbeddingCache;
use semantic::knowledge_ingest::{self, CommitText, PhaseOutcome, SectionText};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Versioned phase recipe recorded in the overlay basis; [`recipe_name`]
/// appends every deterministic producer rule and version to it.
pub(crate) const RECIPE: &str = "knowledge@2";
/// Omission when the base is past the bounded snapshot read.
pub(crate) const BASE_BOUNDS: &str = "knowledge-base-bounds";
/// Omission when the T2 semantic tier is off in Settings: nothing is embedded,
/// and the deterministic producers still publish (#1782).
pub(crate) const SEMANTIC_DISABLED: &str = "semantic-tier-disabled";
/// Omission when the local provider failed during the phase.
pub(crate) const PROVIDER_FAILED: &str = "embedding-provider-failed";
/// Omission when the derived facts were refused as one publication; the
/// overlay is then published without them and its recipe says so.
pub(crate) const DERIVED_REJECTED: &str = "knowledge-derived-rejected";
/// BusinessFlow trace limits, numerically aligned with the overlay caps so no
/// new number is introduced. They are independent budgets: hops count the
/// whole trace and bytes measure serialized flows, not overlay facts, so the
/// combined publication can still be refused under DERIVED_REJECTED.
const FLOW_LIMITS: flowtracer::TraceLimits = flowtracer::TraceLimits {
    max_flows: MAX_FACTS,
    max_hops: MAX_FACTS,
    max_output_bytes: MAX_BYTES,
};

/// What one phase did, reported to the UI; never an ingest failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KnowledgeReport {
    /// Units embedded (cache hits included).
    pub embedded: usize,
    /// Units past the unit cap.
    pub omitted: usize,
    /// Named reason the provider phase did not run, if it did not.
    pub omission: Option<&'static str>,
    /// Deterministic producer facts published with the overlay.
    pub derived: usize,
    /// Producer candidates dropped by their per-fact size cap.
    pub oversized: usize,
    /// Named reason the derived facts were not published, if they were not.
    pub derived_omission: Option<&'static str>,
    /// Named producers that yielded nothing for this base, in producer order.
    pub producer_omissions: Vec<&'static str>,
    /// Whether the overlay was republished for the embedded base revision;
    /// false when the base moved on before publication.
    pub published: bool,
    /// Admitted documentation sections whose text was read from the source
    /// and matched the section's provenance hash (AC-1505).
    pub sections_read: usize,
    /// Admitted sections embedded by heading alone: the source was busy,
    /// moved or unreadable, or the span no longer matches the graph.
    pub sections_unread: usize,
    /// Commits read from the ingested repositories' first-parent history
    /// and embedded as units (AC-1516).
    pub commits_read: usize,
    /// Named sources the provider phase could not read: issue text always,
    /// since no tracker is read (AC-1516).
    pub source_omissions: Vec<&'static str>,
    /// Test-only census of producer nodes in a successfully published full set.
    /// Zero for a moved base, refusal fallback or a phase that never derived.
    #[cfg(test)]
    #[serde(skip)]
    pub published_producer_nodes: ProducerNodeCounts,
}

/// The outcome of the most recent phase as the UI receives it: the report, or
/// the failure that kept the phase from producing one (#1782).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KnowledgeStatus {
    pub report: Option<KnowledgeReport>,
    pub error: Option<String>,
}

impl From<Result<KnowledgeReport, String>> for KnowledgeStatus {
    fn from(result: Result<KnowledgeReport, String>) -> Self {
        match result {
            Ok(report) => Self {
                report: Some(report),
                error: None,
            },
            Err(error) => Self {
                report: None,
                error: Some(error),
            },
        }
    }
}

/// The last phase outcome, kept so a window opened after the phase finished
/// still shows it. Process memory only; a restart starts with none.
static LAST: Mutex<Option<KnowledgeStatus>> = Mutex::new(None);

fn remember(status: &KnowledgeStatus) {
    *LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(status.clone());
}

/// The outcome of the most recent knowledge phase in this process, if one ran.
#[tauri::command]
pub(crate) fn knowledge_phase_status() -> Option<KnowledgeStatus> {
    LAST.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Serializes phases so an older base can never be published after a newer one.
static PHASE: Mutex<()> = Mutex::new(());

/// The local embedding provider on the loopback Ollama at `port` (#1653),
/// when its pinned model is installed there.
pub(crate) fn local_provider(port: u16) -> Option<llm::OllamaProvider> {
    let provider = llm::OllamaProvider::local_on_port(port).ok()?;
    let health = llm::catalog::classify_health(
        llm::catalog::ModelAction::Embedding,
        &provider.list_local_models(),
    );
    matches!(health, llm::catalog::ModelHealth::Available { .. }).then_some(provider)
}

/// Embed the current base and republish the overlay for its revision.
pub(crate) fn run(
    graph: &Mutex<SqliteGraphStore>,
    cache: &Arc<Mutex<EmbeddingCache>>,
    overlay: &Arc<Mutex<OverlayStore>>,
    provider: Option<&dyn LlmProvider>,
) -> Result<KnowledgeReport, String> {
    run_with_sections(graph, cache, overlay, provider, &no_sections)
}

/// Reads the text of the admitted documentation sections from their
/// sources. ADR-0065 keeps document text out of the graph, so the phase asks
/// the host; a section missing from the answer is embedded by its heading.
pub(crate) type SectionReader<'a> = dyn Fn(&[&core_graph::Node]) -> SectionText + 'a;

/// No source access: every section is embedded by its heading.
pub(crate) fn no_sections(_: &[&core_graph::Node]) -> SectionText {
    SectionText::new()
}

/// [`run`] with the host's section reader (AC-1505).
pub(crate) fn run_with_sections(
    graph: &Mutex<SqliteGraphStore>,
    cache: &Arc<Mutex<EmbeddingCache>>,
    overlay: &Arc<Mutex<OverlayStore>>,
    provider: Option<&dyn LlmProvider>,
    sections: &SectionReader<'_>,
) -> Result<KnowledgeReport, String> {
    run_with_sources(graph, cache, overlay, provider, sections, &no_history)
}

/// Reads the first-parent history of each ingested repository at the
/// commit its base was ingested from, given as repository key to commit
/// (AC-1516). Commits never enter the graph, so the phase asks the host.
pub(crate) type HistoryReader<'a> = dyn Fn(&BTreeMap<String, String>) -> Vec<CommitText> + 'a;

/// No source access: no commit is a unit.
pub(crate) fn no_history(_: &BTreeMap<String, String>) -> Vec<CommitText> {
    Vec::new()
}

/// [`run_with_sections`] with the host's history reader (AC-1516).
pub(crate) fn run_with_sources(
    graph: &Mutex<SqliteGraphStore>,
    cache: &Arc<Mutex<EmbeddingCache>>,
    overlay: &Arc<Mutex<OverlayStore>>,
    provider: Option<&dyn LlmProvider>,
    sections: &SectionReader<'_>,
    history: &HistoryReader<'_>,
) -> Result<KnowledgeReport, String> {
    run_within(
        graph,
        cache,
        overlay,
        provider,
        PHASE_LIMITS,
        sections,
        history,
    )
}

/// Run the phase as the T2 setting allows (#1782). With T2 off no provider is
/// used and nothing is embedded, which the report names; the deterministic
/// producers need no provider and publish either way.
#[cfg(test)]
pub(crate) fn run_for_setting(
    graph: &Mutex<SqliteGraphStore>,
    cache: &Arc<Mutex<EmbeddingCache>>,
    overlay: &Arc<Mutex<OverlayStore>>,
    semantic: bool,
    provider: impl FnOnce() -> Option<Box<dyn LlmProvider>>,
) -> Result<KnowledgeReport, String> {
    run_for_setting_with_sources(
        graph,
        cache,
        overlay,
        semantic,
        provider,
        &no_sections,
        &no_history,
    )
}

/// [`run_for_setting`] with the host's section and history readers, which
/// are only asked when the provider phase runs (AC-1505, AC-1516).
pub(crate) fn run_for_setting_with_sources(
    graph: &Mutex<SqliteGraphStore>,
    cache: &Arc<Mutex<EmbeddingCache>>,
    overlay: &Arc<Mutex<OverlayStore>>,
    semantic: bool,
    provider: impl FnOnce() -> Option<Box<dyn LlmProvider>>,
    sections: &SectionReader<'_>,
    history: &HistoryReader<'_>,
) -> Result<KnowledgeReport, String> {
    if semantic {
        let provider = provider();
        return run_with_sources(
            graph,
            cache,
            overlay,
            provider.as_deref(),
            sections,
            history,
        );
    }
    let mut report = run(graph, cache, overlay, None)?;
    if report.omission == Some(knowledge_ingest::NO_PROVIDER) {
        report.omission = Some(SEMANTIC_DISABLED);
    }
    Ok(report)
}

/// The base this phase reads (#1771). It is a desktop background phase on
/// the owner's machine with no memory reserved in advance, the same class
/// of read as a desktop-started investigation (#1602), so it takes that
/// ceiling (ADR-0052, amended). Under the interactive default of 50,000 rows a real repository
/// was skipped whole and no knowledge node was produced for it.
pub(crate) const PHASE_LIMITS: SnapshotReadLimits = SnapshotReadLimits::investigation();

/// Labels whose props no producer, unit builder or overlay admission reads.
/// Their nodes stay in the base with id and label, so citations, id lookups
/// and shadowing checks see them, but their props are read as `{}` (AC-1202).
/// A recovered `BusinessRule` holds its whole rule text and evidence: on REA
/// those props are a third of the graph's JSON.
pub(crate) const PROPS_UNREAD: [&str; 1] = ["BusinessRule"];

/// The bounded base the phase reads, streamed under the same admission as
/// [`SqliteGraphStore::read_snapshot_with_revision_bounded`] and in its row
/// order, without holding the props of [`PROPS_UNREAD`] labels (AC-1202),
/// and with each fact's `prov` held as text (`spec::compact_node`, AC-1209).
fn read_base(
    graph: &SqliteGraphStore,
    limits: SnapshotReadLimits,
) -> Result<RevisionedSnapshot, core_graph::GraphError> {
    let (mut nodes, mut edges) = (Vec::new(), Vec::new());
    let (revision, _) = graph.visit_snapshot_with_revision_bounded(limits, |row| {
        match row {
            core_graph::SnapshotRow::Node(mut node) => {
                if PROPS_UNREAD.contains(&node.label.as_str()) {
                    node.props = serde_json::Value::Object(serde_json::Map::new());
                }
                // Each fact's `prov` is held as its text as the row is read,
                // so the whole graph is never decoded at once (#2215).
                spec::compact_node(&mut node);
                nodes.push(node);
            }
            core_graph::SnapshotRow::Edge(mut edge) => {
                spec::compact_edge(&mut edge);
                edges.push(edge);
            }
        }
        Ok::<_, core_graph::GraphError>(())
    })?;
    // The stream delivers edges by (src, label, dst); the materialized read
    // and every producer's tie-breaking see them by (src, dst, label).
    edges.sort_unstable_by(|a, b| {
        (a.src.as_str(), a.dst.as_str(), a.label.as_str()).cmp(&(
            b.src.as_str(),
            b.dst.as_str(),
            b.label.as_str(),
        ))
    });
    Ok(RevisionedSnapshot {
        revision,
        graph: (nodes, edges),
    })
}

fn run_within(
    graph: &Mutex<SqliteGraphStore>,
    cache: &Arc<Mutex<EmbeddingCache>>,
    overlay: &Arc<Mutex<OverlayStore>>,
    provider: Option<&dyn LlmProvider>,
    limits: SnapshotReadLimits,
    sections: &SectionReader<'_>,
    history: &HistoryReader<'_>,
) -> Result<KnowledgeReport, String> {
    let _phase = PHASE.lock().map_err(|e| e.to_string())?;
    let (snapshot, lease) = {
        let graph = graph.lock().map_err(|e| e.to_string())?;
        let snapshot = match read_base(&graph, limits) {
            Ok(snapshot) => snapshot,
            Err(core_graph::GraphError::SnapshotBounds(_)) => {
                return Ok(report(0, 0, Some(BASE_BOUNDS)));
            }
            Err(error) => return Err(error.to_string()),
        };
        let lease = cache
            .lock()
            .map_err(|e| e.to_string())?
            .bind_instance(&snapshot.revision.graph_instance)
            .map_err(|e| e.to_string())?;
        (snapshot, lease)
    };
    // Producers and provider work run without the graph lock; the revision
    // check at publication keeps a moved base from receiving this overlay.
    let derived = derive(&snapshot);
    // The traced flows are units too. Their vectors are cache rows keyed by
    // text, so a derived set the overlay later refuses leaves nothing behind
    // that anything reads as a fact.
    let flows: Vec<&core_graph::Node> = derived
        .facts
        .iter()
        .filter_map(|fact| match &fact.item {
            core_graph::overlay::OverlayItem::Node(node) if node.label == "BusinessFlow" => {
                Some(node)
            }
            _ => None,
        })
        .collect();
    // Section text is read only when it can be embedded, and outside the
    // graph lock (AC-1505).
    let admitted = knowledge_ingest::admitted_sections(&snapshot.graph.0, &flows);
    let embeds = provider.is_some_and(|p| p.capabilities().embeddings);
    let section_text = if embeds {
        sections(&admitted)
    } else {
        SectionText::new()
    };
    // History is read the same way: only when it can be embedded, outside
    // the graph lock, at the commit each repository's base came from.
    let commits = if embeds {
        history(&ingested_heads(&snapshot.graph.0))
    } else {
        Vec::new()
    };
    let (recipe, mut report) = match knowledge_ingest::run(
        &snapshot.graph.0,
        &flows,
        &section_text,
        &commits,
        provider,
        Arc::clone(cache),
        lease,
    ) {
        Ok(PhaseOutcome::Embedded {
            binding,
            units,
            omitted,
            ..
        }) => (
            recipe(binding.map(|b| b.namespace().to_owned()), true),
            report(units.len(), omitted, None),
        ),
        Ok(PhaseOutcome::Skipped { omission }) => {
            (recipe(None, false), report(0, 0, Some(omission)))
        }
        Err(_) => (recipe(None, false), report(0, 0, Some(PROVIDER_FAILED))),
    };
    if report.omission.is_none() {
        report.sections_read = admitted
            .iter()
            .filter(|node| section_text.contains_key(&node.id))
            .count();
        report.sections_unread = admitted.len() - report.sections_read;
        report.commits_read = knowledge_ingest::cited_commits(&snapshot.graph.0, &commits).count();
        report.source_omissions = vec![knowledge_ingest::NO_ISSUE_SOURCE];
    }
    report.oversized = derived.oversized;
    report.producer_omissions = derived.omissions;
    // Count the already-held vector before publication consumes it; expose the
    // census only after the complete derived set has committed successfully.
    #[cfg(test)]
    let producer_nodes = producer_node_counts(&derived.facts);
    match publish(graph, overlay, &snapshot, recipe, derived.facts)? {
        Published::Moved => {}
        Published::Facts(derived) => {
            report.published = true;
            report.derived = derived;
            #[cfg(test)]
            {
                report.published_producer_nodes = producer_nodes;
            }
        }
        Published::WithoutDerived => {
            report.published = true;
            report.derived_omission = Some(DERIVED_REJECTED);
        }
    }
    Ok(report)
}

/// Constant-size, test-only census; edges such as MAPS_TO are not node yields.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ProducerNodeCounts {
    pub capabilities: usize,
    pub business_flows: usize,
    pub domain_entities: usize,
    pub actors: usize,
}
#[cfg(test)]
fn producer_node_counts(facts: &[OverlayFact]) -> ProducerNodeCounts {
    let mut counts = ProducerNodeCounts::default();
    for fact in facts {
        let core_graph::overlay::OverlayItem::Node(node) = &fact.item else {
            continue;
        };
        match node.label.as_str() {
            "Capability" => counts.capabilities += 1,
            "BusinessFlow" => counts.business_flows += 1,
            "DomainEntity" => counts.domain_entities += 1,
            "Actor" => counts.actors += 1,
            _ => {}
        }
    }
    counts
}

/// Facts from every deterministic producer over one snapshot.
struct Derived {
    facts: Vec<OverlayFact>,
    /// Candidates dropped under a producer's per-fact size cap.
    oversized: usize,
    /// Producers that failed as a whole, by their named omission.
    omissions: Vec<&'static str>,
}

/// Every deterministic producer over one snapshot, in a fixed order. A
/// producer that fails is a named omission; the others still publish.
fn derive(snapshot: &RevisionedSnapshot) -> Derived {
    derive_within(snapshot, FLOW_LIMITS)
}

fn derive_within(snapshot: &RevisionedSnapshot, flow_limits: flowtracer::TraceLimits) -> Derived {
    let capabilities = spec::derive_graphql_resolver_capabilities(snapshot);
    let mut derived = Derived {
        facts: capabilities.facts,
        oversized: capabilities.oversized_groups,
        omissions: Vec::new(),
    };
    let controllers = spec::derive_controller_capabilities(snapshot);
    derived.facts.extend(controllers.facts);
    derived.oversized += controllers.oversized_groups;
    let entry_points = spec::derive_entry_point_capabilities(snapshot);
    derived.facts.extend(entry_points.facts);
    derived.oversized += entry_points.oversized_groups;
    match spec::derive_business_flows(snapshot, flow_limits) {
        Ok(flows) => {
            derived.facts.extend(flows.facts);
            derived.oversized += flows.oversized_flows;
        }
        Err(omission) => derived.omissions.push(omission.code()),
    }
    let entities = spec::derive_orm_domain_entities(snapshot);
    derived.facts.extend(entities.facts);
    derived.oversized += entities.oversized_models;
    let actors = spec::derive_permission_actors(snapshot);
    derived.facts.extend(actors.facts);
    derived.oversized += actors.oversized_endpoints;
    let subsystems = spec::derive_subsystems(snapshot);
    derived.facts.extend(subsystems.facts);
    derived.oversized += subsystems.oversized_groups;
    derived.omissions.extend(subsystems.omission);
    derived
}

/// Every producer rule and its version, in phase order.
pub(crate) const PRODUCER_RULES: [(&str, &str); 7] = [
    (
        spec::CAPABILITY_GRAPHQL_RULE,
        spec::CAPABILITY_GRAPHQL_RULE_VERSION,
    ),
    (
        spec::CAPABILITY_CONTROLLER_RULE,
        spec::CAPABILITY_CONTROLLER_RULE_VERSION,
    ),
    (
        spec::CAPABILITY_ENTRY_POINT_RULE,
        spec::CAPABILITY_ENTRY_POINT_RULE_VERSION,
    ),
    (spec::BUSINESS_FLOW_RULE, spec::BUSINESS_FLOW_RULE_VERSION),
    (
        spec::DOMAIN_ENTITY_ORM_RULE,
        spec::DOMAIN_ENTITY_ORM_RULE_VERSION,
    ),
    (spec::ACTOR_ALLOW_RULE, spec::ACTOR_ALLOW_RULE_VERSION),
    (spec::SUBSYSTEM_RULE, spec::SUBSYSTEM_RULE_VERSION),
];

/// The phase recipe with a digest of every producer rule and version, so a
/// rule version change changes the overlay identity. The name has one
/// length however many producers land: `knowledge@2;producers@blake3:`
/// and 64 hex digits, within the overlay's name bound (#2040).
pub(crate) fn recipe_name() -> String {
    recipe_over(&PRODUCER_RULES)
}

/// [`recipe_name`] over `rules`: the content hash (BLAKE3) of each
/// `rule@version`, one per line in phase order, so the order and every
/// version are in it.
pub(crate) fn recipe_over(rules: &[(&str, &str)]) -> String {
    let listed: String = rules
        .iter()
        .map(|(rule, version)| format!("{rule}@{version}\n"))
        .collect();
    let digest = core_prov::content_hash(listed.as_bytes());
    format!("{RECIPE};producers@blake3:{digest}")
}

fn recipe(embedding_namespace: Option<String>, provider_ran: bool) -> OverlayRecipe {
    OverlayRecipe {
        recipe: recipe_name(),
        embedding_namespace,
        provider_ran,
    }
}

fn report(embedded: usize, omitted: usize, omission: Option<&'static str>) -> KnowledgeReport {
    KnowledgeReport {
        embedded,
        omitted,
        omission,
        published: false,
        sections_read: 0,
        sections_unread: 0,
        commits_read: 0,
        source_omissions: Vec::new(),
        derived: 0,
        oversized: 0,
        derived_omission: None,
        producer_omissions: Vec::new(),
        #[cfg(test)]
        published_producer_nodes: ProducerNodeCounts::default(),
    }
}

enum Published {
    /// The base moved on; nothing was published.
    Moved,
    /// Published with this many derived facts.
    Facts(usize),
    /// The derived facts were refused; published without them.
    WithoutDerived,
}

/// Publish only while `snapshot` is still the current base. Lock order is
/// graph, then overlay, as in the graph reset hook.
fn publish(
    graph: &Mutex<SqliteGraphStore>,
    overlay: &Arc<Mutex<OverlayStore>>,
    snapshot: &RevisionedSnapshot,
    recipe: OverlayRecipe,
    facts: Vec<OverlayFact>,
) -> Result<Published, String> {
    let graph = graph.lock().map_err(|e| e.to_string())?;
    if graph.read_revision().map_err(|e| e.to_string())? != snapshot.revision {
        return Ok(Published::Moved);
    }
    let mut overlay = overlay.lock().map_err(|e| e.to_string())?;
    let derived = facts.len();
    match overlay.publish(snapshot, recipe.clone(), facts) {
        Ok(_) => Ok(Published::Facts(derived)),
        // A refused derived set must not cost the embedding outcome: publish
        // the phase alone, marked in the recipe so readers can tell.
        Err(OverlayError::Rejected(_)) if derived > 0 => {
            let recipe = OverlayRecipe {
                recipe: format!("{};{DERIVED_REJECTED}", recipe.recipe),
                ..recipe
            };
            overlay
                .publish(snapshot, recipe, vec![])
                .map_err(|e| e.to_string())?;
            Ok(Published::WithoutDerived)
        }
        Err(error) => Err(error.to_string()),
    }
}

/// At most one background worker. Ingests that finish while it runs are
/// coalesced into one follow-up pass over the then-current base, so no
/// thread, provider probe or embedding pass is queued per ingest.
struct Coalescer {
    /// `(running, pending)`.
    state: Mutex<(bool, bool)>,
}
impl Coalescer {
    const fn new() -> Self {
        Self {
            state: Mutex::new((false, false)),
        }
    }
    fn state(&self) -> std::sync::MutexGuard<'_, (bool, bool)> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    /// True when the caller must start the worker; otherwise the running
    /// worker owes one more pass.
    fn admit(&self) -> bool {
        let mut state = self.state();
        if state.0 {
            state.1 = true;
            false
        } else {
            *state = (true, false);
            true
        }
    }
    /// True when another pass is owed; otherwise the worker stops.
    fn next(&self) -> bool {
        let mut state = self.state();
        if state.1 {
            state.1 = false;
            true
        } else {
            *state = (false, false);
            false
        }
    }
    /// The worker could not start; nothing is running.
    fn abandon(&self) {
        *self.state() = (false, false);
    }
}

static WORKER: Coalescer = Coalescer::new();

/// The worker's loop: one pass, then any owed pass.
fn drain(coalescer: &Coalescer, mut pass: impl FnMut()) {
    loop {
        pass();
        if !coalescer.next() {
            return;
        }
    }
}

/// T2 participation as set in Settings; unreadable settings fail closed.
fn semantic_enabled(settings: &Mutex<crate::settings::SettingsStore>) -> bool {
    settings.lock().ok().is_some_and(|settings| {
        settings
            .all()
            .is_ok_and(|tiers| tiers.iter().any(|t| t.tier == "T2" && t.enabled))
    })
}

/// Run the phase in the background after a committed ingest and emit its
/// outcome as `knowledge-phase`. Skipped without a cache or overlay. With T2
/// disabled it still runs for the deterministic producers and embeds nothing
/// (#1782). A failed thread start never affects the ingest.
/// Section text bytes kept per section: the unit is cut to
/// [`knowledge_ingest::MAX_UNIT_BYTES`] after its heading anyway.
const SECTION_TEXT_BYTES: usize = 4096;

/// Read each admitted section's span from its registered source through the
/// confined evidence read, under one read-only source operation released
/// before anything is embedded (AC-1505). A section's text is kept only when
/// its bytes still hash to the section's provenance, so a working tree that
/// moved since ingest never puts other text under a section. A source that
/// is busy, unregistered or unreadable leaves its sections unread.
pub(crate) fn source_sections(
    registry: &Mutex<crate::sources::SourceRegistry>,
    sections: &[&core_graph::Node],
) -> SectionText {
    let mut by_repo: std::collections::BTreeMap<
        String,
        Vec<(&str, core_prov::EvidenceRef, String)>,
    > = std::collections::BTreeMap::new();
    for node in sections {
        let Ok(prov) = <core_prov::Provenance as serde::Deserialize>::deserialize(
            &*core_graph::stored::stored_prov(&node.props),
        ) else {
            continue;
        };
        let [evidence] = prov.evidence.as_slice() else {
            continue;
        };
        if evidence.byte_end <= evidence.byte_start
            || evidence.byte_end - evidence.byte_start > crate::evidence::MAX_EVIDENCE_BYTES
        {
            continue;
        }
        by_repo.entry(evidence.repo.clone()).or_default().push((
            node.id.as_str(),
            evidence.clone(),
            prov.content_hash.clone(),
        ));
    }
    let mut texts = SectionText::new();
    if by_repo.is_empty() {
        return texts;
    }
    let Ok(listed) = registry
        .lock()
        .map_err(|e| e.to_string())
        .and_then(|r| r.list())
    else {
        return texts;
    };
    let wanted = listed
        .into_iter()
        .filter(|source| by_repo.contains_key(&source.repo_key))
        .map(|source| (source, false));
    let Ok(operation) = crate::source_access::SourceOperation::acquire(registry, wanted) else {
        return texts;
    };
    for (repo, items) in &by_repo {
        let Ok(root) = operation.root(repo) else {
            continue;
        };
        for (id, evidence, hash) in items {
            let Ok(text) = crate::evidence::read_span_exact(
                root,
                &evidence.path,
                &(evidence.byte_start..evidence.byte_end),
            ) else {
                continue;
            };
            if spec::section_content_hash(id, &text) != *hash {
                continue;
            }
            // Bound the collapsed text, as the unit renders it, so a run of
            // whitespace never crowds out the prose after it.
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut end = text.len().min(SECTION_TEXT_BYTES);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            texts.insert((*id).to_string(), text[..end].to_string());
        }
    }
    drop(operation);
    texts
}

/// The commit each repository's base was ingested from: the one commit its
/// `File` facts cite, when they all cite the same full object id. A
/// repository ingested from a working tree, or whose files cite several
/// commits, has none.
pub(crate) fn ingested_heads(nodes: &[core_graph::Node]) -> BTreeMap<String, String> {
    let mut seen: BTreeMap<String, Option<String>> = BTreeMap::new();
    for node in nodes.iter().filter(|node| node.label == "File") {
        let Ok(prov) = <core_prov::Provenance as serde::Deserialize>::deserialize(
            &*core_graph::stored::stored_prov(&node.props),
        ) else {
            continue;
        };
        for evidence in &prov.evidence {
            let full = matches!(evidence.commit_sha.len(), 40 | 64)
                && evidence.commit_sha.bytes().all(|b| b.is_ascii_hexdigit());
            let slot = seen
                .entry(evidence.repo.clone())
                .or_insert_with(|| full.then(|| evidence.commit_sha.clone()));
            if !full || slot.as_deref() != Some(evidence.commit_sha.as_str()) {
                *slot = None;
            }
        }
    }
    seen.into_iter()
        .filter_map(|(repo, commit)| Some((repo, commit?)))
        .collect()
}

/// Read the first-parent history of each repository from its commit, under
/// one read-only source operation released before anything is embedded
/// (AC-1516): at most [`knowledge_ingest::MAX_COMMIT_UNITS`] commits per
/// repository, newest first, each with its message and up to
/// [`knowledge_ingest::MAX_COMMIT_PATHS`] paths its tree changed from its
/// first parent. Author names and emails are never read. A source that is
/// busy, unregistered, not a repository root or missing the commit gives
/// no commits.
pub(crate) fn source_history(
    registry: &Mutex<crate::sources::SourceRegistry>,
    heads: &BTreeMap<String, String>,
) -> Vec<CommitText> {
    let mut commits = Vec::new();
    if heads.is_empty() {
        return commits;
    }
    let Ok(listed) = registry
        .lock()
        .map_err(|e| e.to_string())
        .and_then(|r| r.list())
    else {
        return commits;
    };
    let wanted = listed
        .into_iter()
        .filter(|source| heads.contains_key(&source.repo_key))
        .map(|source| (source, false));
    let Ok(operation) = crate::source_access::SourceOperation::acquire(registry, wanted) else {
        return commits;
    };
    for (repo, head) in heads {
        let Ok(root) = operation.root(repo) else {
            continue;
        };
        if let Ok(read) = repository_history(root, repo, head) {
            commits.extend(read);
        }
    }
    drop(operation);
    commits
}

fn repository_history(
    root: &std::path::Path,
    repo: &str,
    head: &str,
) -> Result<Vec<CommitText>, git2::Error> {
    // The source root itself must be the repository: a discovered parent
    // repository's paths would not be the base's paths.
    let git = git2::Repository::open_ext(
        root,
        git2::RepositoryOpenFlags::NO_SEARCH,
        std::iter::empty::<&std::ffi::OsStr>(),
    )?;
    let mut walk = git.revwalk()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL)?;
    walk.simplify_first_parent()?;
    walk.push(git2::Oid::from_str(head)?)?;
    let mut out = Vec::new();
    for oid in walk.take(knowledge_ingest::MAX_COMMIT_UNITS) {
        let commit = git.find_commit(oid?)?;
        let parent = commit
            .parents()
            .next()
            .map(|parent| parent.tree())
            .transpose()?;
        let diff = git.diff_tree_to_tree(parent.as_ref(), Some(&commit.tree()?), None)?;
        let mut paths = std::collections::BTreeSet::new();
        for delta in diff.deltas() {
            for file in [delta.old_file(), delta.new_file()] {
                if let Some(path) = file.path().and_then(|path| path.to_str()) {
                    paths.insert(path.replace('\\', "/"));
                }
            }
        }
        out.push(CommitText {
            repo: repo.to_string(),
            sha: commit.id().to_string(),
            message: String::from_utf8_lossy(commit.message_bytes()).into_owned(),
            paths: paths
                .into_iter()
                .take(knowledge_ingest::MAX_COMMIT_PATHS)
                .collect(),
        });
    }
    Ok(out)
}

pub(crate) fn spawn_after_ingest<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::{Emitter, Manager};
    {
        let state = app.state::<crate::AppState>();
        if state.embedding_cache.is_none() || state.knowledge_overlay.is_none() {
            return;
        }
    }
    if !WORKER.admit() {
        return;
    }
    let app = app.clone();
    let started = std::thread::Builder::new()
        .name("knowledge-phase".into())
        .spawn(move || {
            let state = app.state::<crate::AppState>();
            let (Some(cache), Some(overlay)) = (&state.embedding_cache, &state.knowledge_overlay)
            else {
                WORKER.abandon();
                return;
            };
            drain(&WORKER, || {
                let status = KnowledgeStatus::from(run_for_setting_with_sources(
                    &state.graph,
                    cache,
                    overlay,
                    semantic_enabled(&state.settings),
                    || {
                        let port = crate::settings::saved_ollama_port(&state.settings).ok()?;
                        local_provider(port).map(|p| Box::new(p) as Box<dyn LlmProvider>)
                    },
                    &|sections| source_sections(&state.sources, sections),
                    &|heads| source_history(&state.sources, heads),
                ));
                remember(&status);
                let _ = app.emit("knowledge-phase", status);
            });
        });
    if started.is_err() {
        WORKER.abandon();
    }
}

#[cfg(test)]
#[path = "knowledge_phase_tests.rs"]
mod tests;
