//! Knowledge phase after ingest (ADR-0051 decision 3, #1441).
//!
//! Once an ingest has committed the deterministic base, the knowledge units
//! of that base are embedded through the local provider and the knowledge
//! overlay is republished for the same base revision, recording in its
//! recipe whether the provider phase ran and which embedding namespace it
//! used. The phase never fails or delays the ingest: every outcome,
//! including no local provider, is reported as a named result.
use core_graph::overlay::{OverlayRecipe, OverlayStore};
use core_graph::{RevisionedSnapshot, SnapshotReadLimits, SqliteGraphStore};
use llm::LlmProvider;
use semantic::embedding_cache::EmbeddingCache;
use semantic::knowledge_ingest::{self, PhaseOutcome};
use serde::Serialize;
use std::sync::{Arc, Mutex};

/// Versioned producer recipe recorded in the overlay basis.
pub(crate) const RECIPE: &str = "knowledge@1";
/// Omission when the base is past the bounded snapshot read.
pub(crate) const BASE_BOUNDS: &str = "knowledge-base-bounds";
/// Omission when the local provider failed during the phase.
pub(crate) const PROVIDER_FAILED: &str = "embedding-provider-failed";

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
    /// Whether the overlay was republished for the embedded base revision;
    /// false when the base moved on before publication.
    pub published: bool,
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
    run_within(
        graph,
        cache,
        overlay,
        provider,
        SnapshotReadLimits::default(),
    )
}

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
                return Ok(KnowledgeReport {
                    embedded: 0,
                    omitted: 0,
                    omission: Some(BASE_BOUNDS),
                    published: false,
                });
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
    report.published = publish(graph, overlay, &snapshot, recipe)?;
    Ok(report)
}

fn recipe(embedding_namespace: Option<String>, provider_ran: bool) -> OverlayRecipe {
    OverlayRecipe {
        recipe: RECIPE.into(),
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
    }
}

/// Publish only while `snapshot` is still the current base. Lock order is
/// graph, then overlay, as in the graph reset hook.
fn publish(
    graph: &Mutex<SqliteGraphStore>,
    overlay: &Arc<Mutex<OverlayStore>>,
    snapshot: &RevisionedSnapshot,
    recipe: OverlayRecipe,
) -> Result<bool, String> {
    let graph = graph.lock().map_err(|e| e.to_string())?;
    if graph.read_revision().map_err(|e| e.to_string())? != snapshot.revision {
        return Ok(false);
    }
    // Knowledge producers add their facts here as they land (#1443).
    overlay
        .lock()
        .map_err(|e| e.to_string())?
        .publish(snapshot, recipe, vec![])
        .map_err(|e| e.to_string())?;
    Ok(true)
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

/// The worker's loop: one pass while T2 is enabled, then any owed pass.
fn drain(coalescer: &Coalescer, enabled: impl Fn() -> bool, mut pass: impl FnMut()) {
    loop {
        if enabled() {
            pass();
        }
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
/// report as `knowledge-phase`. Skipped without a cache or overlay, or while
/// T2 is disabled; a failed thread start never affects the ingest.
pub(crate) fn spawn_after_ingest<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::{Emitter, Manager};
    {
        let state = app.state::<crate::AppState>();
        if state.embedding_cache.is_none()
            || state.knowledge_overlay.is_none()
            || !semantic_enabled(&state.settings)
        {
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
            drain(
                &WORKER,
                || semantic_enabled(&state.settings),
                || {
                    let provider = local_provider();
                    let result = run(
                        &state.graph,
                        cache,
                        overlay,
                        provider.as_ref().map(|p| p as &dyn LlmProvider),
                    );
                    let _ = app.emit("knowledge-phase", result);
                },
            );
        });
    if started.is_err() {
        WORKER.abandon();
    }
}

#[cfg(test)]
#[path = "knowledge_phase_tests.rs"]
mod tests;
