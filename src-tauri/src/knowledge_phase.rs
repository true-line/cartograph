//! Knowledge phase after ingest (ADR-0051 decision 3, #1441).
//!
//! Once an ingest has committed the deterministic base, the knowledge units
//! of that base are embedded through the local provider and the knowledge
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
use semantic::knowledge_ingest::{self, PhaseOutcome};
use serde::Serialize;
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

/// The local embedding provider when its pinned model is installed.
pub(crate) fn local_provider() -> Option<llm::OllamaProvider> {
    let provider = llm::OllamaProvider::local_default().ok()?;
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
    run_within(graph, cache, overlay, provider, PHASE_LIMITS)
}

/// Run the phase as the T2 setting allows (#1782). With T2 off no provider is
/// used and nothing is embedded, which the report names; the deterministic
/// producers need no provider and publish either way.
pub(crate) fn run_for_setting(
    graph: &Mutex<SqliteGraphStore>,
    cache: &Arc<Mutex<EmbeddingCache>>,
    overlay: &Arc<Mutex<OverlayStore>>,
    semantic: bool,
    provider: impl FnOnce() -> Option<Box<dyn LlmProvider>>,
) -> Result<KnowledgeReport, String> {
    if semantic {
        let provider = provider();
        return run(graph, cache, overlay, provider.as_deref());
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

fn run_within(
    graph: &Mutex<SqliteGraphStore>,
    cache: &Arc<Mutex<EmbeddingCache>>,
    overlay: &Arc<Mutex<OverlayStore>>,
    provider: Option<&dyn LlmProvider>,
    limits: SnapshotReadLimits,
) -> Result<KnowledgeReport, String> {
    let _phase = PHASE.lock().map_err(|e| e.to_string())?;
    let (snapshot, lease) = {
        let graph = graph.lock().map_err(|e| e.to_string())?;
        let snapshot = match graph.read_snapshot_with_revision_bounded(limits) {
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
    // Provider work runs without the graph lock; the revision check at
    // publication keeps a moved base from receiving this overlay.
    let (recipe, mut report) =
        match knowledge_ingest::run(&snapshot.graph.0, provider, Arc::clone(cache), lease) {
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
    let derived = derive(&snapshot);
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
    derived
}

/// The phase recipe with each producer rule, so a rule version change
/// changes the overlay identity.
pub(crate) fn recipe_name() -> String {
    format!(
        "{RECIPE};{}@{};{}@{};{}@{};{}@{}",
        spec::CAPABILITY_GRAPHQL_RULE,
        spec::CAPABILITY_GRAPHQL_RULE_VERSION,
        spec::BUSINESS_FLOW_RULE,
        spec::BUSINESS_FLOW_RULE_VERSION,
        spec::DOMAIN_ENTITY_ORM_RULE,
        spec::DOMAIN_ENTITY_ORM_RULE_VERSION,
        spec::ACTOR_ALLOW_RULE,
        spec::ACTOR_ALLOW_RULE_VERSION
    )
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
                let status = KnowledgeStatus::from(run_for_setting(
                    &state.graph,
                    cache,
                    overlay,
                    semantic_enabled(&state.settings),
                    || local_provider().map(|p| Box::new(p) as Box<dyn LlmProvider>),
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
