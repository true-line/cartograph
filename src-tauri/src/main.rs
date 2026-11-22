//! Cartograph desktop shell (M0): boots the webview, owns the graph store and
//! the durable job spine, and exposes the first Tauri commands.

// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent_pool;
mod area_briefing;
mod area_expert;
#[cfg(test)]
mod conformance_tests;
mod context;
mod curated;
mod curated_workbench;
mod drop_intake;
mod pipeline_activity;
use app_store::egress;
use app_store::escalation;
use app_store::evidence;
mod export_save;
use app_store::findings;
#[cfg(test)]
mod graph_projection_tests;
mod investigations;
use app_store::job_execution;
#[cfg(test)]
mod job_execution_host_tests;
use app_store::jobs;
mod knowledge_phase;
mod local_commit;
mod manifest_draft;
mod mcp_reads;
use app_store::metrics;
mod model_debug;
#[cfg(test)]
mod name_unique_tests;
use app_store::ingest_basis::{
    ActivePlugin, SystemRepo, coverage_config_hash, plugin_settings_root, source_revisions_of,
    system_contents_of,
};
use app_store::observation_inputs;
use app_store::operator_limits;
use app_store::paths;
#[path = "primary_source_commands.rs"]
mod primary_source;
#[cfg(test)]
mod primary_source_tests;
mod projects;
mod proposals;
mod provider_checks;
use app_store::provider_credentials;
#[cfg(test)]
mod registered_source_tests;
#[cfg(test)]
mod registry_tests;
mod section_compose;
mod section_review;
#[cfg(test)]
mod semantic_cache_tests;
mod semantic_evaluation;
use app_store::settings;
#[cfg(test)]
mod showcase_tests;
use app_store::source_access;
use app_store::sources;
mod spec_bundle_file;
mod system_preflight;
mod task_evidence;
#[cfg(test)]
mod test_app;
mod work_packets;

use adapters_fw::events::Registries;
use core_graph::facts::FactStore;
use core_graph::packed::{PackedEdge, PackedNode};
use core_graph::{Edge, GraphStore, Node, SqliteGraphStore};
use findings::{Finding, FindingStore, NewFinding};
use job_execution::{JobExecution, JobExecutionLocks};
use jobs::{
    ClaimMode, EvalResult, ExecutionCheck, ExecutionUpdate, Job, JobStore, JobTransitionError,
};
use llm::LlmProvider;
use serde::Serialize;
use source_access::SourceOperation;
use sources::{RegisteredSource, SourceRegistry};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};

use app_store::app_state::AppState;
use app_store::extraction_cache::{ExtractionCaches, RepoExtractionCache};

#[derive(Serialize)]
struct GraphStats {
    nodes: u64,
    edges: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct LayerSummary {
    files: u64,
    nodes: u64,
    edges: u64,
}

impl LayerSummary {
    fn add(&mut self, other: Self) {
        self.files += other.files;
        self.nodes += other.nodes;
        self.edges += other.edges;
    }
}

/// The store keeps one fact per node id and per `(src, dst, label)` relation
/// (SPEC-00 §4.4), so counts quote distinct keys, never raw occurrences —
/// otherwise the ingest summary and the Workspace graph counts disagree
/// (AC-0195, #242).
fn pack_err(error: core_graph::GraphError) -> String {
    error.to_string()
}

/// Ids of `label` nodes without a `placeholder` prop, in order with repeats.
/// Only nodes whose packed props mention the key are decoded (#1574).
fn unplaceholdered<'a>(facts: &'a FactStore, label: &str) -> Result<Vec<&'a str>, String> {
    let mut ids = Vec::new();
    for node in facts.nodes().iter().filter(|node| node.label == label) {
        if node.props_json().contains("\"placeholder\"")
            && node
                .unpack()
                .map_err(pack_err)?
                .props
                .get("placeholder")
                .is_some()
        {
            continue;
        }
        ids.push(node.id.as_str());
    }
    Ok(ids)
}

/// [`unplaceholdered`] as an owned set.
fn unplaceholdered_set(
    facts: &FactStore,
    label: &str,
) -> Result<std::collections::BTreeSet<String>, String> {
    Ok(unplaceholdered(facts, label)?
        .into_iter()
        .map(str::to_owned)
        .collect())
}

/// The nodes [`repo_coverage`] counts, decoded from packed facts: Resource,
/// Endpoint and Channel nodes and any node whose packed props mention
/// `pulumi`, a superset of every node its predicates select (#1574).
fn coverage_view(facts: &FactStore) -> Result<adapters_lang_ts::Extraction, String> {
    let mut view = adapters_lang_ts::Extraction::default();
    view.nodes = facts
        .nodes_where(|node| {
            matches!(node.label.as_str(), "Resource" | "Endpoint" | "Channel")
                || node.props_json().contains("pulumi")
        })
        .map_err(pack_err)?;
    Ok(view)
}

/// [`distinct_node_count`] over packed nodes.
fn distinct_packed_node_count(nodes: &[PackedNode]) -> u64 {
    nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64
}

/// [`distinct_edge_count`] over packed edges.
fn distinct_packed_edge_count(edges: &[PackedEdge]) -> u64 {
    edges
        .iter()
        .map(|edge| (edge.src.as_str(), edge.dst.as_str(), edge.label.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64
}

fn distinct_node_count(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64
}

fn distinct_edge_count(edges: &[Edge]) -> u64 {
    edges
        .iter()
        .map(|edge| (edge.src.as_str(), edge.dst.as_str(), edge.label.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64
}

/// Occurrences an extraction emitted beyond the one fact the store keeps per
/// key. Stated in the summary so collapsing is never silent (AC-0195).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct MergedFacts {
    /// Repeated node ids beyond the first occurrence.
    nodes: u64,
    /// Repeated relations beyond the first — typically one call or import
    /// relation cited from several sites, whose evidence the stored edge
    /// unions (AC-0203).
    edges: u64,
    /// Node ids emitted with differing facts: distinct declarations sharing
    /// one identity, of which only the last occurrence is stored.
    node_collisions: u64,
}

impl MergedFacts {
    fn add(&mut self, other: Self) {
        self.nodes += other.nodes;
        self.edges += other.edges;
        self.node_collisions += other.node_collisions;
    }
}

#[cfg(test)]
use app_store::edge_merge::MAX_MERGED_EDGE_EVIDENCE;
use app_store::edge_merge::{
    EdgeKey, edge_key, merge_edge_occurrences, merge_packed_edge_occurrences, merged_edge_view,
};

/// The distinct fact keys one load wrote (its Repo node included) and what
/// it collapsed to get there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PublishedFacts {
    nodes: std::collections::BTreeSet<String>,
    edges: std::collections::BTreeSet<EdgeKey>,
    merged: MergedFacts,
}

/// Every fact key one recovery operation published, across each repo it
/// loaded and each whole-graph stage it ran afterwards (BACKS stitching,
/// found-ADR relinking). It is the single source every ingest summary quotes
/// (AC-0195): a key published by two repos is one stored fact, so the
/// cross-repo repeat is reported as merged rather than counted twice.
#[derive(Debug, Default)]
struct OperationFacts {
    nodes: std::collections::BTreeSet<String>,
    edges: std::collections::BTreeSet<EdgeKey>,
    merged: MergedFacts,
}

impl OperationFacts {
    fn record_load(&mut self, loaded: PublishedFacts) {
        self.merged.add(loaded.merged);
        for id in loaded.nodes {
            if !self.nodes.insert(id) {
                self.merged.nodes += 1;
            }
        }
        for key in loaded.edges {
            if !self.edges.insert(key) {
                self.merged.edges += 1;
            }
        }
    }

    /// A later whole-graph stage re-publishing or retracting facts, in the
    /// store's own patch order: edge deletes, node deletes (with their
    /// incident edges), then upserts. Re-publishing a key is not a merge.
    fn record_patch(&mut self, patch: &core_graph::GraphPatch) {
        for key in &patch.delete_edges {
            self.edges.remove(key);
        }
        for id in &patch.delete_node_ids {
            self.nodes.remove(id);
            self.edges.retain(|(src, dst, _)| src != id && dst != id);
        }
        self.nodes
            .extend(patch.upsert_nodes.iter().map(|node| node.id.clone()));
        self.edges.extend(patch.upsert_edges.iter().map(edge_key));
        self.nodes
            .extend(patch.upsert_packed_nodes.iter().map(|node| node.id.clone()));
        self.edges.extend(
            patch
                .upsert_packed_edges
                .iter()
                .map(|edge| (edge.src.clone(), edge.dst.clone(), edge.label.clone())),
        );
    }

    fn nodes(&self) -> u64 {
        self.nodes.len() as u64
    }

    fn edges(&self) -> u64 {
        self.edges.len() as u64
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct LayerBreakdown {
    ts: LayerSummary,
    python: LayerSummary,
    go: LayerSummary,
    rust: LayerSummary,
    java: LayerSummary,
    kotlin: LayerSummary,
    tf: LayerSummary,
    webext: LayerSummary,
    tools: LayerSummary,
    schedules: LayerSummary,
    /// Process starts beyond HTTP: listeners, workers, commands (#1997).
    entry_points: LayerSummary,
    /// Ordered workflow and Dockerfile steps (#1790).
    pipelines: LayerSummary,
    /// Environments and workloads from Compose files (#1791).
    environments: LayerSummary,
    /// Channel and client-call stitching over every layer's sites (#874).
    events: LayerSummary,
}

impl LayerBreakdown {
    fn add(&mut self, other: Self) {
        self.ts.add(other.ts);
        self.python.add(other.python);
        self.go.add(other.go);
        self.rust.add(other.rust);
        self.java.add(other.java);
        self.kotlin.add(other.kotlin);
        self.tf.add(other.tf);
        self.webext.add(other.webext);
        self.tools.add(other.tools);
        self.schedules.add(other.schedules);
        self.entry_points.add(other.entry_points);
        self.pipelines.add(other.pipelines);
        self.environments.add(other.environments);
        self.events.add(other.events);
    }

    fn files(self) -> u64 {
        self.ts.files
            + self.python.files
            + self.go.files
            + self.rust.files
            + self.java.files
            + self.kotlin.files
            + self.tf.files
    }
}

#[derive(Serialize)]
struct PingReply {
    app: &'static str,
    version: &'static str,
}

#[tauri::command]
fn ping() -> PingReply {
    PingReply {
        app: "cartograph",
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
fn graph_stats(state: State<'_, AppState>) -> Result<GraphStats, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    let (nodes, edges) = graph.fact_counts().map_err(|e| e.to_string())?;
    Ok(GraphStats { nodes, edges })
}

/// One discovered plugin with its per-project lifecycle state (#198) and
/// its conformance-gate verdict for these exact bytes (#200).
#[derive(Serialize)]
struct PluginStatus {
    #[serde(flatten)]
    plugin: adapters_plugin_host::discovery::DiscoveredPlugin,
    /// Explicit per-project opt-in; absent rows are disabled (fail closed).
    enabled: bool,
    /// `passed`, `failed`, or `ungated` — the proposed state every artifact
    /// starts in. Keyed by artifact and corpus bytes: replacing either one
    /// leaves the plugin `ungated` again.
    gate: &'static str,
    /// First failing check as `name: detail` when the gate failed.
    gate_detail: Option<String>,
    /// Coverage cells the gated corpus declares; empty unless `passed`.
    coverage: Vec<ingest::matrix::ReportedCell>,
}

/// The corpus next to a plugin artifact, read once: its exact bytes key the
/// gate verdict, and a parsed copy supplies the claims that verdict covers.
struct PluginCorpus {
    gate_key: String,
    corpus: Option<adapters_plugin_host::gate::GoldenCorpus>,
}

impl PluginCorpus {
    fn read(plugin: &adapters_plugin_host::discovery::DiscoveredPlugin) -> Self {
        let bytes = std::fs::read(plugin.path.with_extension("golden.json")).ok();
        Self::from_bytes(plugin, bytes)
    }

    fn from_bytes(
        plugin: &adapters_plugin_host::discovery::DiscoveredPlugin,
        bytes: Option<Vec<u8>>,
    ) -> Self {
        Self {
            gate_key: adapters_plugin_host::gate::gate_key(&plugin.content_hash, bytes.as_deref()),
            corpus: bytes.and_then(|bytes| serde_json::from_slice(&bytes).ok()),
        }
    }

    /// Declared cells in the reported shape (AC-0263).
    fn cells(&self) -> Vec<ingest::matrix::ReportedCell> {
        use adapters_plugin_host::gate::DeclaredSupport;
        use ingest::matrix::Support;
        self.corpus
            .iter()
            .flat_map(|corpus| &corpus.coverage)
            .map(|cell| ingest::matrix::ReportedCell {
                language: cell.language.clone(),
                framework: cell.framework.clone(),
                capability: cell.capability.clone(),
                support: match cell.support {
                    DeclaredSupport::Supported => Support::Supported,
                    DeclaredSupport::Partial => Support::Partial,
                    DeclaredSupport::Unsupported => Support::Unsupported,
                },
                note: cell.note.clone(),
            })
            .collect()
    }
}

/// Discovery metadata stays coupled to the managed roots that supplied it.
/// Keep this value alive through every dependent source read and publication;
/// a discovered path alone cannot prevent participating clone replacement.
struct SessionPluginDiscovery {
    plugins: Vec<adapters_plugin_host::discovery::DiscoveredPlugin>,
    _source_operation: SourceOperation,
}

/// Discover plugin artifacts: `.cartograph/adapters/` inside every resolved
/// registered ingest root (never the raw Connect input — a GitHub URL or
/// manifest path is not a directory, #203 review), then the user-level
/// adapters directory. Project wins on id conflict. Enablement joins on the
/// exact artifact hash, so replaced bytes are disabled again. Discovery
/// never runs guest code.
fn discover_session_plugins<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
) -> Result<SessionPluginDiscovery, String> {
    let user_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("adapters");
    let registered = state.sources.lock().map_err(|e| e.to_string())?.list()?;
    let available = registered
        .into_iter()
        .filter(RegisteredSource::is_ready)
        .collect::<Vec<_>>();
    let operation = SourceOperation::acquire(
        &state.sources,
        available.iter().cloned().map(|source| (source, false)),
    )?;
    let roots = available
        .iter()
        .map(|source| {
            operation
                .root(&source.repo_key)
                .map(std::path::Path::to_path_buf)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SessionPluginDiscovery {
        plugins: adapters_plugin_host::discovery::discover(&roots, &user_dir),
        _source_operation: operation,
    })
}

/// The plugins allowed to extract for `root` right now. Everything about
/// this is fail-closed: no enablement row, a different artifact hash, a
/// missing/failed gate, or an unreadable/empty corpus each drop the plugin
/// from the active set silently — extraction never guesses.
fn active_plugins_for_root<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    root: &std::path::Path,
) -> Result<Vec<ActivePlugin>, String> {
    let user_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("adapters");
    active_plugins_in(state, root, &user_dir)
}

/// Every plugin installed for `root`, gated or not, as coverage
/// declarations for the adapter lookup (#1071 review): an installed plugin
/// whose gate has not passed for its exact bytes, or that is not enabled,
/// declares its cells unverified rather than disappearing. Declarations
/// only: nothing is loaded or executed.
fn plugin_declarations_for_root<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    root: &std::path::Path,
) -> Result<Vec<ingest::adapter_lookup::PluginDeclaration>, String> {
    let user_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("adapters");
    plugin_declarations_in(state, root, &user_dir)
}

fn plugin_declarations_in(
    state: &AppState,
    root: &std::path::Path,
    user_dir: &std::path::Path,
) -> Result<Vec<ingest::adapter_lookup::PluginDeclaration>, String> {
    let root_key = root.display().to_string();
    let discovered = adapters_plugin_host::discovery::discover(&[root.to_path_buf()], user_dir);
    let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
    let mut declarations = Vec::new();
    for plugin in discovered {
        let settings_root = plugin_settings_root(&plugin);
        if settings_root != root_key && settings_root != "user" {
            continue;
        }
        let corpus = PluginCorpus::read(&plugin);
        // Verified means exactly what extraction trusts (`active_plugins_in`).
        let gate_passed = settings_store
            .plugin_enabled_exact(&settings_root, &plugin.id, &plugin.content_hash)
            .map_err(|e| e.to_string())?
            && settings_store
                .plugin_gate_passed_exact(&plugin.id, &corpus.gate_key)
                .map_err(|e| e.to_string())?;
        declarations.push(ingest::adapter_lookup::PluginDeclaration {
            id: plugin.id,
            version: plugin.content_hash,
            gate_passed,
            cells: corpus.cells(),
        });
    }
    Ok(declarations)
}

/// The effective event registries for one ingest (#594): the built-in
/// files plus `<app_data>/registries/*.json`. Read once per ingest and
/// shared by every repository, extraction lane, backing join, and the
/// coverage identity, so no ingest sees two registry sets.
fn event_registries_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<Registries, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("registries");
    Ok(event_registries_in(&dir))
}

/// The scan behind [`event_registries_for`]. Only regular `.json` files
/// count, in name order (other `.json` entries are rejected without taking
/// a slot); each is read through a bound of one byte past
/// the registry limit, so an oversized or still-growing file is rejected
/// without being read whole, and files past the count bound are rejected
/// unread. A file that cannot be read is rejected like an invalid one.
fn event_registries_in(dir: &std::path::Path) -> Registries {
    use adapters_fw::events::{MAX_EXTENSION_FILES, MAX_REGISTRY_BYTES, RegistryRejection};
    use std::io::Read;
    let mut names = Vec::new();
    let mut unreadable = Vec::new();
    let directory_error = |error: std::io::Error| RegistryRejection {
        name: "extension:<directory>".into(),
        reason: format!("unreadable: {error}"),
    };
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => {
                        let name = match entry.file_name().into_string() {
                            Ok(name) => name,
                            Err(raw) => {
                                // A lossy conversion could alias two files to
                                // one source name; refuse rather than guess.
                                let name = raw.to_string_lossy().into_owned();
                                if name.ends_with(".json") {
                                    unreadable.push(RegistryRejection {
                                        name: format!("extension:{name}"),
                                        reason: "unreadable: file name is not valid UTF-8".into(),
                                    });
                                }
                                continue;
                            }
                        };
                        if !name.ends_with(".json") {
                            continue;
                        }
                        // Classify before counting: only regular files take
                        // one of the bounded extension slots.
                        match std::fs::symlink_metadata(entry.path()) {
                            Ok(metadata) if metadata.file_type().is_file() => {
                                names.push((name, entry.path()))
                            }
                            Ok(_) => unreadable.push(RegistryRejection {
                                name: format!("extension:{name}"),
                                reason: "unreadable: not a regular file".into(),
                            }),
                            Err(error) => unreadable.push(RegistryRejection {
                                name: format!("extension:{name}"),
                                reason: format!("unreadable: {error}"),
                            }),
                        }
                    }
                    Err(error) => unreadable.push(directory_error(error)),
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => unreadable.push(directory_error(error)),
    }
    names.sort();
    let mut files = Vec::new();
    for (index, (name, path)) in names.into_iter().enumerate() {
        if index >= MAX_EXTENSION_FILES {
            // Rejected by `layered` on count alone; never read.
            files.push((name, Vec::new()));
            continue;
        }
        let read = (|| {
            if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
                return Err(std::io::Error::other("not a regular file"));
            }
            let mut bytes = Vec::new();
            std::fs::File::open(&path)?
                .take(MAX_REGISTRY_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        match read {
            Ok(bytes) => files.push((name, bytes)),
            Err(error) => unreadable.push(RegistryRejection {
                name: format!("extension:{name}"),
                reason: format!("unreadable: {error}"),
            }),
        }
    }
    let mut registries = Registries::layered(&files);
    registries.rejected.extend(unreadable);
    registries.rejected.sort_by(|a, b| a.name.cmp(&b.name));
    registries
}

/// What the registry snapshot of an ingest was: the files that applied
/// and the files rejected whole, with why.
#[derive(Debug, Clone, Serialize)]
struct RegistryReport {
    sources: Vec<adapters_fw::events::RegistryIdentity>,
    rejected: Vec<adapters_fw::events::RegistryRejection>,
}

impl From<&Registries> for RegistryReport {
    fn from(registries: &Registries) -> Self {
        Self {
            sources: registries.sources.clone(),
            rejected: registries.rejected.clone(),
        }
    }
}

/// The scan behind [`active_plugins_for_root`], with the user directory
/// injected. Discovery runs for *this root only* (#208 review): a project
/// copy in some other session root must not shadow the user-level copy
/// this root's coverage relies on.
fn active_plugins_in(
    state: &AppState,
    root: &std::path::Path,
    user_dir: &std::path::Path,
) -> Result<Vec<ActivePlugin>, String> {
    let root_key = root.display().to_string();
    let discovered = adapters_plugin_host::discovery::discover(&[root.to_path_buf()], user_dir);
    let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
    let limits = plugin_limits(&settings_store)?;
    let mut active = Vec::new();
    for plugin in discovered {
        let settings_root = plugin_settings_root(&plugin);
        // A project copy only ever extracts for its own project; user-level
        // copies apply wherever their (user-scoped) enablement says so.
        if settings_root != root_key && settings_root != "user" {
            continue;
        }
        let enabled = settings_store
            .plugin_enabled_exact(&settings_root, &plugin.id, &plugin.content_hash)
            .map_err(|e| e.to_string())?;
        if !enabled {
            continue;
        }
        // The verdict covers these exact corpus bytes: an edited corpus
        // (new extension or coverage claims) is ungated, never trusted.
        let corpus = PluginCorpus::read(&plugin);
        let gate_passed = settings_store
            .plugin_gate_passed_exact(&plugin.id, &corpus.gate_key)
            .map_err(|e| e.to_string())?;
        if !gate_passed {
            continue;
        }
        let cells = corpus.cells();
        let gate_key = corpus.gate_key;
        let Some((extensions, languages)) = corpus
            .corpus
            .map(|corpus| (corpus.extensions, corpus.languages))
            .filter(|(extensions, _)| !extensions.is_empty())
        else {
            continue;
        };
        active.push(ActivePlugin {
            plugin_id: plugin.id,
            path: plugin.path,
            content_hash: plugin.content_hash,
            extensions,
            cells,
            languages,
            gate_key,
            limits,
        });
    }
    Ok(active)
}

/// The project's plugin sandbox bounds (#1653, AC-1803): fuel, linear
/// memory and per-file deadline from the saved operator limits. The table
/// bound stays fixed. The conformance gate keeps the defaults, so a verdict
/// never depends on a project's settings.
fn plugin_limits(
    settings: &settings::SettingsStore,
) -> Result<adapters_plugin_host::PluginLimits, String> {
    let limit = |key| {
        settings
            .operator_limit(key)
            .map_err(|error| error.to_string())
    };
    Ok(adapters_plugin_host::PluginLimits {
        max_fuel: limit(operator_limits::PLUGIN_MAX_FUEL_BILLIONS)?
            .checked_mul(1_000_000_000)
            .ok_or("Plugin fuel overflows.")?,
        max_memory_bytes: usize::try_from(limit(operator_limits::PLUGIN_MAX_MEMORY_MIB)?)
            .ok()
            .and_then(|mib| mib.checked_mul(1024 * 1024))
            .ok_or("Plugin memory overflows.")?,
        deadline: std::time::Duration::from_secs(limit(operator_limits::PLUGIN_DEADLINE_SECONDS)?),
        ..adapters_plugin_host::PluginLimits::default()
    })
}

#[tauri::command]
fn list_plugins(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<PluginStatus>, String> {
    let discovered = discover_session_plugins(&app, &state)?;
    let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
    let mut statuses = Vec::with_capacity(discovered.plugins.len());
    for plugin in discovered.plugins {
        let enabled = settings_store
            .enabled_plugins(&plugin_settings_root(&plugin))
            .map_err(|e| e.to_string())?
            .iter()
            .any(|(id, hash)| *id == plugin.id && *hash == plugin.content_hash);
        let corpus = PluginCorpus::read(&plugin);
        let (gate, gate_detail) = match settings_store
            .plugin_gate(&plugin.id, &corpus.gate_key)
            .map_err(|e| e.to_string())?
        {
            Some((true, _)) => ("passed", None),
            Some((false, report_json)) => ("failed", first_failing_check(&report_json)),
            None => ("ungated", None),
        };
        let coverage = if gate == "passed" {
            corpus.cells()
        } else {
            Vec::new()
        };
        statuses.push(PluginStatus {
            enabled,
            gate,
            gate_detail,
            coverage,
            plugin,
        });
    }
    Ok(statuses)
}

/// `name: detail` of the first failing check in a stored gate report — the
/// one line a user needs to see next to a `failed` chip.
fn first_failing_check(report_json: &str) -> Option<String> {
    let report: serde_json::Value = serde_json::from_str(report_json).ok()?;
    report["checks"].as_array()?.iter().find_map(|check| {
        if check["passed"].as_bool() == Some(false) {
            Some(format!(
                "{}: {}",
                check["name"].as_str().unwrap_or("check"),
                check["detail"].as_str().unwrap_or("failed")
            ))
        } else {
            None
        }
    })
}

/// The fixed source identity conformance corpora are authored against:
/// golden node/edge ids that embed the repo must use `golden` (the host
/// hands the plugin this exact repo/commit during the gate, and only then).
fn gate_source_id() -> adapters_plugin_host::SourceId {
    adapters_plugin_host::SourceId {
        repo: "golden".to_string(),
        commit: "golden".to_string(),
    }
}

/// Run the conformance gate for one discovered plugin as a durable job
/// (#200, AC-0068): SPI contract under the standard bounds, the
/// generator-supplied golden corpus (`{plugin-id}.golden.json` next to the
/// artifact), and a double-run determinism check. The verdict persists per
/// (plugin id, artifact and corpus bytes) — a missing or unreadable corpus records a
/// failed gate, never a skipped one, and the plugin stays proposed until a
/// recorded pass for these exact bytes.
#[tauri::command]
async fn run_plugin_gate(
    plugin_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let (running, execution) = start_job(&state, &format!("plugin-gate:{plugin_id}"))?;
    emit_job(&app, &running);
    off_ui_thread_for_job(execution, move |execution| {
        plugin_gate_blocking(&plugin_id, execution, &app)
    })
    .await
}

/// The gate pipeline behind one already-running `plugin-gate:{id}` job —
/// shared by the command above and the Jobs retry path, so an interrupted
/// or failed gate reruns instead of dead-ending. Cancellation is honored
/// before the verdict persists: a cancelled job never changes the trusted
/// artifact state.
fn plugin_gate_blocking<R: tauri::Runtime>(
    plugin_id: &str,
    execution: &JobExecution,
    app: &tauri::AppHandle<R>,
) -> Result<serde_json::Value, String> {
    let state = app.state::<AppState>();
    let fail = |error: String| -> String {
        report_failure(app, &state, execution, &error);
        error
    };

    if job_cancelled(&state, execution)? {
        return Err("cancelled".into());
    }

    report_progress(app, &state, execution, "discover", 10.0).map_err(&fail)?;
    // Retain the discovery's shared source guards through both filesystem
    // reads, gate execution and verdict publication. This coordinates managed
    // checkout replacement, not external edits or user-level plugin writes.
    let discovery = discover_session_plugins(app, &state).map_err(&fail)?;
    let plugin = discovery
        .plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .ok_or_else(|| fail(format!("no discovered plugin with id {plugin_id}")))?;
    // Hash the bytes actually gated, not the discovery-time snapshot: the
    // verdict must bind to what ran even if the file changed in between.
    ensure_running_job(&state, execution)?;
    let wasm_bytes = std::fs::read(&plugin.path).map_err(|e| fail(e.to_string()))?;
    let content_hash = core_prov::content_hash(&wasm_bytes);

    report_progress(app, &state, execution, "gate", 30.0).map_err(&fail)?;
    let corpus_path = plugin.path.with_extension("golden.json");
    // The verdict binds to the artifact and corpus bytes actually gated.
    let corpus_bytes = std::fs::read(&corpus_path);
    let gate_key =
        adapters_plugin_host::gate::gate_key(&content_hash, corpus_bytes.as_deref().ok());
    let report = match corpus_bytes.map_err(|e| e.to_string()).and_then(|bytes| {
        serde_json::from_slice::<adapters_plugin_host::gate::GoldenCorpus>(&bytes)
            .map_err(|e| e.to_string())
    }) {
        Ok(corpus) => {
            ensure_running_job(&state, execution)?;
            let host = adapters_plugin_host::PluginHost::new().map_err(|e| fail(e.to_string()))?;
            adapters_plugin_host::gate::run_gate(
                &host,
                plugin_id,
                &wasm_bytes,
                &corpus,
                adapters_plugin_host::PluginLimits::default(),
                &gate_source_id(),
            )
        }
        // No corpus, no proof: record the failure instead of erroring, so
        // the artifact is provably `failed`, not indefinitely `ungated`.
        Err(error) => adapters_plugin_host::gate::GateReport {
            passed: false,
            checks: vec![adapters_plugin_host::gate::GateCheck {
                name: "corpus-present".to_string(),
                passed: false,
                detail: format!("{}: {error}", corpus_path.display()),
            }],
        },
    };

    // A cancel that landed while the gate ran wins outright: the job row
    // stays cancelled and the verdict is discarded, so the visible job
    // outcome and the trusted artifact state never diverge (#206 review).
    if job_cancelled(&state, execution)? {
        return Err("cancelled".to_string());
    }

    report_progress(app, &state, execution, "record", 90.0).map_err(&fail)?;
    let report_json = serde_json::to_value(&report).map_err(|e| fail(e.to_string()))?;
    {
        let mut settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        ensure_running_job(&state, execution)?;
        settings_store
            .record_plugin_gate(
                plugin_id,
                &gate_key,
                report.passed,
                &report_json.to_string(),
            )
            .map_err(|e| fail(e.to_string()))?;
    }

    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let job = updated_job(
        jobs.finish_execution(execution, &[format!("gate:{plugin_id}@{content_hash}")])
            .map_err(|e| e.to_string())?,
    );
    emit_job(app, &job);
    completed_job(&job)?;
    Ok(report_json)
}

/// Persist a per-project plugin opt-in/out (#198), bound to the exact
/// artifact hash (#203 review). Enabling never runs the plugin here —
/// extraction happens only behind the conformance gate.
#[tauri::command]
fn set_plugin_enabled(
    project_root: String,
    plugin_id: String,
    content_hash: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut settings_store = state.settings.lock().map_err(|e| e.to_string())?;
    settings_store
        .set_plugin_enabled(&project_root, &plugin_id, &content_hash, enabled)
        .map_err(|e| e.to_string())
}

/// The adapter inventory (#163): the same registry Preflight consults, so
/// Settings and coverage can never disagree. Static per build.
#[derive(Serialize)]
struct AdapterInventory {
    installed: &'static [ingest::preflight::AdapterInfo],
    planned: &'static [ingest::preflight::PlannedAdapter],
    /// Language × framework × capability cells (AC-0253).
    matrix: &'static [ingest::matrix::CoverageCell],
    detector: &'static str,
}

#[tauri::command]
fn adapter_inventory() -> AdapterInventory {
    AdapterInventory {
        installed: ingest::preflight::INSTALLED_ADAPTERS,
        planned: ingest::preflight::PLANNED_ADAPTERS,
        matrix: ingest::matrix::COVERAGE_MATRIX,
        detector: ingest::preflight::DETECTOR_ID,
    }
}

/// What the current system contains, derived from the graph's own facts
/// (never from history logs, which survive a clear): each distinct repo
/// any node's evidence cites, with its recorded commit identity. Evidence
/// is the source so infra-only (Resource) and manifest-only (Extension)
/// repos count too, not just ones with File nodes (#187 review).
/// Deterministic: sorted by repo.
#[tauri::command]
fn system_contents(state: State<'_, AppState>) -> Result<Vec<SystemRepo>, String> {
    system_contents_for(&state)
}

fn system_contents_for(state: &AppState) -> Result<Vec<SystemRepo>, String> {
    let nodes = state
        .graph
        .lock()
        .map_err(|e| e.to_string())?
        .all_nodes()
        .map_err(|e| e.to_string())?;
    let mut contents = system_contents_of(&nodes);
    {
        // The recorded hash is read back, never recomputed here: showing it
        // costs one ingest-history lookup per registration, not a graph read.
        let metrics = state.metrics.lock().map_err(|e| e.to_string())?;
        for entry in &mut contents {
            entry.graph_hash = metrics
                .recorded_content_hash(&entry.repo, &entry.commit)
                .map_err(|e| e.to_string())?;
        }
    }
    let registry = state.sources.lock().map_err(|e| e.to_string())?;
    for entry in &mut contents {
        entry.display_name = registry
            .get_by_repo(&entry.repo)?
            .map(|source| source.display_name);
    }
    Ok(contents)
}

/// What an export was compiled from (#1765, AC-0593): the content hash of
/// the whole base graph and the source revisions it holds, both taken from
/// the snapshot the export itself read. Curation decisions and overlay facts
/// are not graph facts and never enter the hash.
fn graph_basis_of(nodes: &[Node], edges: &[Edge]) -> spec::GraphBasis {
    spec::GraphBasis {
        content_hash: spec::graph_content_hash(nodes, edges),
        sources: source_revisions_of(nodes),
    }
}

fn clear_graph_store(graph: &mut SqliteGraphStore) -> Result<GraphStats, String> {
    graph.clear().map_err(|e| e.to_string())?;
    Ok(GraphStats { nodes: 0, edges: 0 })
}

#[tauri::command]
fn clear_graph(state: State<'_, AppState>) -> Result<GraphStats, String> {
    let mut graph = state.graph.lock().map_err(|e| e.to_string())?;
    let stats = clear_graph_store(&mut graph)?;
    state
        .extraction_caches
        .lock()
        .map_err(|e| e.to_string())?
        .repos
        .clear();
    Ok(stats)
}

/// Refuse while a job is queued or running, then ask the next launch to
/// make `change` and restart (#1994). The jobs lock is held through the
/// restart, so no job can be enqueued between the check and the marker.
fn change_project(
    app: &tauri::AppHandle,
    state: &AppState,
    change: projects::Change,
) -> Result<(), String> {
    let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let busy = jobs
        .list()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|job| matches!(job.status.as_str(), "queued" | "running"));
    if busy {
        return Err(
            "Work is still running. Cancel it or let it finish, then change project.".into(),
        );
    }
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    projects::request(&data_dir, change)?;
    let _held = jobs;
    app.restart()
}

/// Set the loaded project aside and start an empty one (AC-1013).
#[tauri::command]
fn start_new_project(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    change_project(&app, &state, projects::Change::New)
}

/// Set the loaded project aside and load the parked project `id` (AC-1014).
#[tauri::command]
fn switch_project(
    id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    change_project(&app, &state, projects::Change::Switch { to: id })
}

/// Every project, the loaded one first (AC-1014).
#[tauri::command]
fn list_projects(app: tauri::AppHandle) -> Result<Vec<projects::ProjectEntry>, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    projects::list(&data_dir)
}

/// Name a project; an empty name clears it (AC-1014).
#[tauri::command]
fn rename_project(
    id: String,
    name: String,
    app: tauri::AppHandle,
) -> Result<projects::ProjectMeta, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    projects::rename(&data_dir, &id, &name)
}

/// Delete a project that is not loaded (AC-1014).
#[tauri::command]
fn delete_project(id: String, app: tauri::AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    projects::delete(&data_dir, &id)
}

#[tauri::command]
fn clear_finished_jobs(state: State<'_, AppState>) -> Result<usize, String> {
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.clear_finished().map_err(|e| e.to_string())
}

#[tauri::command]
fn list_jobs(state: State<'_, AppState>) -> Result<Vec<Job>, String> {
    let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.list().map_err(|e| e.to_string())
}

/// The provider-reported usage of every job's model calls (AC-1703).
#[tauri::command]
fn list_job_usage(state: State<'_, AppState>) -> Result<Vec<jobs::JobUsage>, String> {
    let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    jobs.list_usage().map_err(|e| e.to_string())
}

#[tauri::command]
fn list_evals(state: State<'_, AppState>) -> Result<Vec<EvalResult>, String> {
    let jobs = state.jobs.lock().map_err(|error| error.to_string())?;
    jobs.list_evals().map_err(|error| error.to_string())
}

/// Legacy decision history; these caller-body records are not staged proposals.
#[tauri::command]
fn list_agent_decisions(state: State<'_, AppState>) -> Result<Vec<agents::DecisionRecord>, String> {
    let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions.list().map_err(|error| error.to_string())
}

/// Historical decisions matching a legacy basis digest. This lookup cannot
/// establish source freshness or activate a proposal in curated context.
#[tauri::command]
fn reapply_agent_decisions(
    basis_hash: String,
    state: State<'_, AppState>,
) -> Result<Vec<agents::DecisionRecord>, String> {
    let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions
        .reapply(&basis_hash)
        .map_err(|error| error.to_string())
}

/// Persist accept/reject/annotate for one cited T2/T3 Workbench assertion.
#[tauri::command]
fn record_assertion_decision(
    assertion: agents::CuratableAssertion,
    decision: agents::AssertionDecision,
    note: Option<String>,
    state: State<'_, AppState>,
) -> Result<agents::AssertionDecisionRecord, String> {
    let mut decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions
        .record_assertion(&assertion, decision, note.as_deref())
        .map_err(|error| error.to_string())
}

/// All content-addressed Workbench decisions, newest first.
#[tauri::command]
fn list_assertion_decisions(
    state: State<'_, AppState>,
) -> Result<Vec<agents::AssertionDecisionRecord>, String> {
    let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    decisions
        .list_assertions()
        .map_err(|error| error.to_string())
}

#[derive(Serialize)]
struct IngestSummary {
    job_id: i64,
    /// The recovered repo's identity; a local tree's commit is always
    /// `workdir` (AC-0237: a dropped folder is listed with both).
    repo: String,
    files: u64,
    nodes: u64,
    edges: u64,
    merged: MergedFacts,
    layers: LayerBreakdown,
    delta: DeltaSummary,
    /// The preflight report reconciled with this recovery's AST proof
    /// (AC-0200), so the Preflight surface can replace its pending view.
    preflight: ingest::preflight::PreflightReport,
    /// Where that report stands in the register order (#458); `None` when a
    /// newer preflight's findings were already there, so it wrote nothing.
    preflight_register: Option<RegisterStamp>,
    /// What this recovery's producers covered and where they stopped
    /// (AC-0238), shown before any interpretation.
    coverage: Vec<ingest::coverage::RepoCoverage>,
    /// The event registry files this recovery applied, and any rejected
    /// whole with why (#594).
    registries: RegistryReport,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct DeltaSummary {
    recomputed_files: u64,
    reused_files: u64,
    deleted_files: u64,
}

impl DeltaSummary {
    fn add(&mut self, recomputed_files: u64, reused_files: u64, deleted_files: u64) {
        self.recomputed_files += recomputed_files;
        self.reused_files += reused_files;
        self.deleted_files += deleted_files;
    }
}

/// Observational inputs a manifest declared for one repo, as written there.
#[derive(Debug, Default)]
struct DeclaredInputs {
    state_json: Option<String>,
    pulumi_json: Option<String>,
    otel_jsonl: Vec<String>,
    /// Declared paths read without a capture: outside the manifest folder or
    /// past the capture limits, so their evidence stays unverified (AC-1041).
    uncaptured: Vec<String>,
}

/// One repo's coverage inventory from what its extraction recorded
/// (AC-0238); unsupported capabilities come from the reconciled preflight.
/// What producers read during one extraction, for the coverage inventory
/// (AC-0238): inputs that the retained output alone cannot show.
#[derive(Debug)]
struct ProducerReads {
    /// TypeScript files the adapter read, before any infra-only projection.
    ts_files: u64,
    /// Gated plugins that ran: pinned extractor id, what each read and
    /// published, and the files each was refused on (#1810).
    plugins: Vec<ingest::coverage::PluginRead>,
    /// Captured TypeScript declarations left without a receipt, by file.
    omitted_declaration_receipts: std::collections::BTreeMap<String, u64>,
    /// Captured TypeScript rule facts left without a receipt, by file (#722).
    omitted_rule_receipts: std::collections::BTreeMap<String, u64>,
    /// Receipts the retained store had no room for, by file (#722).
    omitted_stored_receipts: std::collections::BTreeMap<String, u64>,
    /// Fact identities omitted during captured TypeScript extraction or store
    /// persistence (#725).
    omitted_receipt_facts: Vec<core_graph::source::SourceOmission>,
    /// Whether every omitted receipt had a fact key (#725).
    omitted_receipt_facts_complete: bool,
    /// Whether the captured TypeScript producer ran (#725).
    captured_ts_ran: bool,
    /// Symlinks the captured TypeScript selection did not follow (#1229).
    unfollowed_links: Vec<String>,
    /// What the found ADR/RFC reader read and published (#1847).
    adrs: ingest::coverage::ProducerCount,
    /// Found ADR/RFC files over the size bound, in path order (#1847).
    adr_too_large: Vec<String>,
    /// Found ADR/RFC files that are not UTF-8, in path order (#1847).
    adr_not_utf8: Vec<String>,
    /// What the declared API contract reader read and published (#1847).
    contracts: ingest::coverage::ProducerCount,
    /// What the documentation reader found, from the walk that also read
    /// the found ADRs (AC-0677, AC-1600).
    documents: ingest::coverage::DocumentCoverage,
}

impl Default for ProducerReads {
    fn default() -> Self {
        Self {
            ts_files: 0,
            plugins: Vec::new(),
            omitted_declaration_receipts: std::collections::BTreeMap::new(),
            omitted_rule_receipts: std::collections::BTreeMap::new(),
            omitted_stored_receipts: std::collections::BTreeMap::new(),
            omitted_receipt_facts: Vec::new(),
            omitted_receipt_facts_complete: true,
            captured_ts_ran: false,
            unfollowed_links: Vec::new(),
            adrs: ingest::coverage::ProducerCount::default(),
            adr_too_large: Vec::new(),
            adr_not_utf8: Vec::new(),
            contracts: ingest::coverage::ProducerCount::default(),
            documents: ingest::coverage::DocumentCoverage::default(),
        }
    }
}

impl ProducerReads {
    fn record_stored_omissions(
        &mut self,
        omitted_facts: Vec<(core_graph::source::FactKey, String)>,
    ) {
        self.omitted_receipt_facts
            .extend(
                omitted_facts
                    .into_iter()
                    .map(
                        |(fact, emitted_fact_digest)| core_graph::source::SourceOmission {
                            fact,
                            emitted_fact_digest,
                            kind: core_graph::source::ReceiptOmissionKind::StoreCapacity,
                        },
                    ),
            );
    }

    fn source_omissions(&self) -> Vec<core_graph::source::SourceOmission> {
        self.omitted_receipt_facts.clone()
    }
}

/// The coverage class of a plugin-host refusal (#1810, AC-0658).
fn refusal_reason(reason: adapters_plugin_host::RefusalReason) -> ingest::coverage::RefusalReason {
    use adapters_plugin_host::RefusalReason as Host;
    use ingest::coverage::RefusalReason as Recorded;
    match reason {
        Host::MalformedFact => Recorded::MalformedFact,
        Host::GuestError => Recorded::GuestError,
        Host::BoundExceeded => Recorded::BoundExceeded,
    }
}

/// The inventory a publication records with its repo's facts (AC-0239).
fn coverage_record(
    coverage: &ingest::coverage::RepoCoverage,
    source: &RegisteredSource,
    job_id: i64,
    plugins: &[ActivePlugin],
    registries: &Registries,
) -> Result<core_graph::coverage::CoverageRecord, String> {
    Ok(core_graph::coverage::CoverageRecord {
        repo_key: coverage.repo.clone(),
        registered_source_id: Some(source.source_id.clone()),
        ingest_id: format!("job:{job_id}"),
        config_hash: coverage_config_hash(plugins, registries),
        inventory_json: serde_json::to_string(coverage).map_err(|error| error.to_string())?,
    })
}

#[allow(clippy::too_many_arguments)]
fn repo_coverage(
    repo: &str,
    commit: &str,
    commit_note: Option<&str>,
    hints: &[String],
    layers: &LayerBreakdown,
    extraction: &adapters_lang_ts::Extraction,
    declared: &DeclaredInputs,
    reads: &ProducerReads,
) -> ingest::coverage::RepoCoverage {
    use ingest::coverage::{CoverageFacts, Observation, ProducerCount, RepoCoverage};
    let count = |layer: &LayerSummary| ProducerCount {
        files: layer.files,
        nodes: layer.nodes,
        edges: layer.edges,
    };
    let counted = |predicate: &dyn Fn(&core_graph::Node) -> bool| {
        extraction
            .nodes
            .iter()
            .filter(|node| predicate(node))
            .count() as u64
    };
    // State and Pulumi inputs are read only inside the infrastructure layer.
    let infra = ingest::coverage::wants(hints, ingest::coverage::INFRA_LAYERS);
    RepoCoverage::from_facts(&CoverageFacts {
        repo,
        commit,
        commit_note,
        layer_hints: hints,
        deterministic: vec![
            (
                "t0.adapter-ts",
                ProducerCount {
                    files: reads.ts_files,
                    ..count(&layers.ts)
                },
            ),
            ("t0.webextension", count(&layers.webext)),
            ("t0.adapter-python", count(&layers.python)),
            ("t0.adapter-go", count(&layers.go)),
            ("t0.adapter-rust", count(&layers.rust)),
            ("t0.adapter-java", count(&layers.java)),
            ("t0.adapter-kotlin", count(&layers.kotlin)),
            ("t0.iac-terraform", count(&layers.tf)),
            (
                ingest::toolchain::TOOLCHAIN_EXTRACTOR_ID,
                count(&layers.tools),
            ),
            (
                ingest::schedules::SCHEDULE_EXTRACTOR_ID,
                count(&layers.schedules),
            ),
            (
                ingest::entry_points::ENTRY_POINT_EXTRACTOR_ID,
                count(&layers.entry_points),
            ),
            (
                ingest::pipelines::PIPELINE_EXTRACTOR_ID,
                count(&layers.pipelines),
            ),
            (
                ingest::environments::ENVIRONMENT_EXTRACTOR_ID,
                count(&layers.environments),
            ),
            (events::STITCH_EXTRACTOR_ID, count(&layers.events)),
            (spec::FOUND_ADR_EXTRACTOR_ID, reads.adrs),
            (spec::CONTRACT_EXTRACTOR_ID, reads.contracts),
        ],
        plugins: reads.plugins.clone(),
        observations: vec![
            Observation {
                producer: dynamic::EXTRACTOR_ID,
                input: "state_json",
                enriches: "Terraform",
                declared: declared.state_json.iter().cloned().collect(),
                uncaptured: declared.uncaptured.clone(),
                // Only Terraform-extracted resources take observed state;
                // Pulumi resources share the `res:` id space.
                targets: counted(&|node| {
                    node.label == "Resource"
                        && node.props["prov"]["extractor_id"] == "t0.iac-terraform"
                }),
                selected: infra,
            },
            Observation {
                producer: dynamic::PULUMI_EXTRACTOR_ID,
                input: "pulumi_json",
                enriches: "Pulumi resource",
                declared: declared.pulumi_json.iter().cloned().collect(),
                uncaptured: declared.uncaptured.clone(),
                targets: counted(&|node| node.props["source"].as_str() == Some("pulumi")),
                selected: infra,
            },
            Observation {
                producer: dynamic::OTEL_EXTRACTOR_ID,
                input: "otel_jsonl",
                enriches: "endpoint and channel",
                declared: declared.otel_jsonl.clone(),
                uncaptured: declared.uncaptured.clone(),
                targets: counted(&|node| node.label == "Endpoint" || node.label == "Channel"),
                selected: true,
            },
        ],
    })
}

/// The cross-layer T0 pipeline over one tree: TypeScript, Python, Go, Terraform,
/// channel stitching, client fetch resolution — closed over so the
/// FK-enforcing store never sees a dangling endpoint.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn extract_tree_with_summary(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    state_json: Option<&std::path::Path>,
    pulumi_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
) -> Result<(adapters_lang_ts::Extraction, LayerBreakdown), String> {
    let mut cache = RepoExtractionCache::default();
    extract_tree_incremental(
        root,
        repo,
        commit,
        layers,
        manifest_env,
        state_json,
        pulumi_json,
        otel_jsonl,
        &mut cache,
        &[],
        &mut |_| {},
    )
    .map(|(extraction, layers, _)| (extraction, layers))
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn extract_tree_incremental(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    state_json: Option<&std::path::Path>,
    pulumi_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
    cache: &mut RepoExtractionCache,
    plugins: &[ActivePlugin],
    on_file: &mut dyn FnMut(&str),
) -> Result<(adapters_lang_ts::Extraction, LayerBreakdown, DeltaSummary), String> {
    extract_tree_with_primary(
        root,
        repo,
        commit,
        layers,
        manifest_env,
        &std::collections::BTreeSet::new(),
        state_json,
        pulumi_json,
        otel_jsonl,
        cache,
        plugins,
        on_file,
        None,
        &mut Vec::new(),
        &mut ProducerReads::default(),
    )
}

/// [`extract_tree_with_registries`] under the built-in event registries.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn extract_tree_with_primary(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    deferred_origins: &std::collections::BTreeSet<String>,
    state_json: Option<&std::path::Path>,
    pulumi_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
    cache: &mut RepoExtractionCache,
    plugins: &[ActivePlugin],
    on_file: &mut dyn FnMut(&str),
    primary: Option<&source_capture::Capture>,
    receipts: &mut Vec<adapters_lang_ts::captured::MintedReceipt>,
    reads: &mut ProducerReads,
) -> Result<(adapters_lang_ts::Extraction, LayerBreakdown, DeltaSummary), String> {
    extract_tree_with_registries(
        root,
        repo,
        commit,
        layers,
        manifest_env,
        deferred_origins,
        state_json,
        pulumi_json,
        otel_jsonl,
        &[],
        cache,
        plugins,
        Registries::builtin(),
        on_file,
        primary,
        receipts,
        reads,
    )
}

/// Extract one tree. Event sites are recognized under `registries`, the
/// one registry snapshot of the whole ingest (#594).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn extract_tree_with_registries(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    // Origins the system manifest binds (#589); empty outside a system add.
    deferred_origins: &std::collections::BTreeSet<String>,
    state_json: Option<&std::path::Path>,
    pulumi_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
    api_docs: &[ingest::api_docs::ApiDocDeclaration],
    cache: &mut RepoExtractionCache,
    plugins: &[ActivePlugin],
    registries: &Registries,
    on_file: &mut dyn FnMut(&str),
    primary: Option<&source_capture::Capture>,
    receipts: &mut Vec<adapters_lang_ts::captured::MintedReceipt>,
    reads: &mut ProducerReads,
) -> Result<(adapters_lang_ts::Extraction, LayerBreakdown, DeltaSummary), String> {
    let (mut extraction, facts, layers, delta) = extract_tree_packed(
        root,
        repo,
        commit,
        layers,
        manifest_env,
        &std::collections::BTreeSet::new(),
        deferred_origins,
        &observation_inputs::ObservationInputs::uncaptured(state_json, pulumi_json, otel_jsonl),
        api_docs,
        RecoveryLimits::default(),
        cache,
        plugins,
        registries,
        on_file,
        primary,
        receipts,
        reads,
    )?;
    (extraction.nodes, extraction.edges) = facts.into_facts().map_err(pack_err)?;
    Ok((extraction, layers, delta))
}

/// Recovery's packed result: the extraction's sites and pending state (its
/// `nodes` and `edges` empty), the facts packed in recovery order, and the
/// layer and delta summaries. Publication takes the facts as they are
/// (#1574, #725).
type PackedRecovery = (
    adapters_lang_ts::Extraction,
    FactStore,
    LayerBreakdown,
    DeltaSummary,
);

/// The captured files a non-TS lane parses: the capture's paths ending in
/// `suffix`, in manifest order (#385).
fn captured_lane_paths(primary: Option<&source_capture::Capture>, suffix: &str) -> Vec<String> {
    primary
        .map(|capture| {
            capture
                .manifest()
                .files
                .iter()
                .filter(|entry| entry.path.ends_with(suffix))
                .map(|entry| entry.path.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Keep a lane's staged receipts only for facts graph publication will
/// materialize as minted (last-value node winners, merged edges), then admit
/// them after those already held. A receipt past the
/// budget is left out and its fact stays uncaptured, as for a TS completion
/// receipt (AC-1019).
fn admit_surviving_lane_receipts(
    nodes: &[core_graph::Node],
    edges: &[core_graph::Edge],
    stage: adapters_lang_ts::captured::ReceiptStage,
    receipts: &mut Vec<adapters_lang_ts::captured::MintedReceipt>,
) {
    let mut minted = stage.into_receipts();
    let survivors = primary_source::matching_bindings_in(nodes, edges, &minted)
        .into_iter()
        .map(|binding| (binding.fact, binding.emitted_fact_digest))
        .collect::<std::collections::BTreeSet<_>>();
    minted.retain(|receipt| {
        survivors.contains(&(receipt.fact_key().clone(), receipt.fact_digest().to_owned()))
    });
    adapters_lang_ts::captured::admit_after(receipts, minted);
}

/// Mint a lane's receipts from the facts graph publication materializes
/// (last-value node winners, merged edges, AC-1037), so a repeated node or
/// relation is cited in its published form, then admit the survivors.
fn stage_lane_publication_receipts(
    grammar: adapters_lang_ts::captured::Grammar,
    capture: &source_capture::Capture,
    files: &[String],
    id: &adapters_lang_ts::SourceId<'_>,
    nodes: &[core_graph::Node],
    edges: &[core_graph::Edge],
    receipts: &mut Vec<adapters_lang_ts::captured::MintedReceipt>,
) {
    let (published_nodes, published_edges) = publication_view(nodes, edges);
    let stage = adapters_lang_ts::captured::stage_lane_receipts(
        grammar,
        capture,
        files,
        id,
        &published_nodes,
        &published_edges,
    );
    admit_surviving_lane_receipts(nodes, edges, stage, receipts);
}

/// The facts graph publication keeps: each node id's last occurrence, in
/// extraction order, and each relation merged across its occurrences.
fn publication_view(
    nodes: &[core_graph::Node],
    edges: &[core_graph::Edge],
) -> (Vec<core_graph::Node>, Vec<core_graph::Edge>) {
    let mut last = std::collections::HashMap::<&str, usize>::new();
    for (index, node) in nodes.iter().enumerate() {
        last.insert(node.id.as_str(), index);
    }
    let nodes = nodes
        .iter()
        .enumerate()
        .filter(|(index, node)| last[node.id.as_str()] == *index)
        .map(|(_, node)| node.clone())
        .collect();
    let edges = merged_edge_view(edges)
        .into_values()
        .map(std::borrow::Cow::into_owned)
        .collect();
    (nodes, edges)
}

/// Observed receipts for the published view of `facts` whose observation
/// cites the input `capture` (ADR-0079, AC-1045): each node's last
/// occurrence and each edge's merged occurrences, as publication keeps
/// them. Only facts with an occurrence citing the input are decoded.
fn observed_receipts_packed(
    capture: &source_capture::Capture,
    facts: &FactStore,
) -> Result<Vec<adapters_lang_ts::captured::MintedReceipt>, String> {
    let cited = format!(
        "\"repo\":{}",
        serde_json::to_string(capture.manifest().source_id.as_str()).map_err(|e| e.to_string())?
    );
    let error = |e: core_graph::GraphError| e.to_string();
    let mut winners = std::collections::BTreeMap::new();
    for (index, node) in facts.nodes().iter().enumerate() {
        winners.insert(node.id.as_str(), index);
    }
    let nodes = winners
        .into_values()
        .filter(|&index| facts.nodes()[index].props_json().contains(&cited))
        .map(|index| facts.nodes()[index].unpack())
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let observed_edges: std::collections::BTreeSet<_> = facts
        .edges()
        .iter()
        .filter(|edge| edge.props_json().contains(&cited))
        .map(|edge| (edge.src.as_str(), edge.dst.as_str(), edge.label.as_str()))
        .collect();
    let occurrences = facts
        .edges_where(|edge| {
            observed_edges.contains(&(edge.src.as_str(), edge.dst.as_str(), edge.label.as_str()))
        })
        .map_err(error)?;
    let edges: Vec<Edge> = merged_edge_view(&occurrences)
        .into_values()
        .map(std::borrow::Cow::into_owned)
        .collect();
    Ok(adapters_lang_ts::captured::mint_observed_receipts(
        capture, &nodes, &edges,
    ))
}

/// [`extract_tree_with_registries`] without the final decode.
#[allow(clippy::too_many_arguments)]
fn extract_tree_packed(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    // Env keys the system manifest lets the resolver read (ADR-0075);
    // unioned with the repository's `.cartograph/env.toml`.
    env_read: &std::collections::BTreeSet<String>,
    // Origins the system manifest binds (#589); empty outside a system add.
    deferred_origins: &std::collections::BTreeSet<String>,
    // Declared Terraform state, Pulumi and trace inputs, captured under their
    // own input identity when they can be (ADR-0080).
    observed: &observation_inputs::ObservationInputs,
    api_docs: &[ingest::api_docs::ApiDocDeclaration],
    limits: RecoveryLimits,
    cache: &mut RepoExtractionCache,
    plugins: &[ActivePlugin],
    registries: &Registries,
    on_file: &mut dyn FnMut(&str),
    primary: Option<&source_capture::Capture>,
    receipts: &mut Vec<adapters_lang_ts::captured::MintedReceipt>,
    reads: &mut ProducerReads,
) -> Result<PackedRecovery, String> {
    // Layer hints gate extractors (AC-0002): empty means everything; the
    // The TS pass covers server/events/client plus Pulumi infra/cloud; the HCL
    // pass covers Terraform infra/cloud.
    // The coverage inventory reads the same gates (AC-0238).
    use ingest::coverage::{APPLICATION_LAYERS, INFRA_LAYERS, wants};
    let wants_application = wants(layers, APPLICATION_LAYERS);
    let wants_infra = wants(layers, INFRA_LAYERS);
    let ts_id = adapters_lang_ts::SourceId { repo, commit };
    let mut layers = LayerBreakdown::default();
    let mut delta = DeltaSummary::default();
    // Facts stay packed from the TS walk until the end of recovery: each
    // later pass decodes only what it reads (#1574, #725).
    let (mut extraction, mut facts) = if wants_application || wants_infra {
        // #209 live detail: the TS pass covers application code when that
        // layer is wanted, otherwise it's only running for Pulumi infra
        // bindings — say which one is actually happening.
        let ts_phase = if wants_application {
            "Reading application code"
        } else {
            "Reading infrastructure (Pulumi)"
        };
        let mut ts_progress = |path: &str| on_file(&format!("{ts_phase} — {path}"));
        let (mut extraction, mut facts, stats) = if let Some(capture) = primary {
            // The captured walk hands its facts back packed (#1237).
            let (extraction, facts, produced, stats) =
                adapters_lang_ts::captured::extract_captured_dir_packed_cached(
                    root,
                    &ts_id,
                    capture,
                    registries,
                    &mut cache.ts_captured,
                    &mut ts_progress,
                )
                .map_err(|e| e.to_string())?;
            receipts.extend(produced);
            reads.omitted_declaration_receipts = stats.omitted_declaration_receipts.clone();
            reads.omitted_rule_receipts = stats.omitted_rule_receipts.clone();
            reads.omitted_receipt_facts = stats.omitted_receipt_facts.clone();
            reads.omitted_receipt_facts_complete = stats.omitted_receipt_facts_complete;
            reads.captured_ts_ran = true;
            (extraction, facts, stats)
        } else {
            adapters_lang_ts::extract_dir_incremental_packed(
                root,
                &ts_id,
                &mut cache.ts,
                registries,
                &mut ts_progress,
            )
            .map_err(|e| e.to_string())?
        };
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        reads.ts_files = unplaceholdered(&facts, "File")?.len() as u64;
        if !wants_application {
            (extraction.nodes, extraction.edges) = facts.into_facts().map_err(pack_err)?;
            extraction.retain_only_pulumi();
            facts = FactStore::from_facts(
                std::mem::take(&mut extraction.nodes),
                std::mem::take(&mut extraction.edges),
            )
            .map_err(pack_err)?;
        }
        (extraction, facts)
    } else {
        (
            adapters_lang_ts::Extraction::default(),
            FactStore::default(),
        )
    };
    layers.ts = LayerSummary {
        files: unplaceholdered(&facts, "File")?.len() as u64,
        nodes: distinct_packed_node_count(facts.nodes()),
        edges: distinct_packed_edge_count(facts.edges()),
    };
    if wants_application {
        // WebExtension manifests (US-0016): topology + permission facts.
        // Runs after the TS pass so entry bindings reuse its File nodes.
        let known_files: std::collections::BTreeSet<String> = facts
            .nodes()
            .iter()
            .filter(|node| node.label == "File")
            .map(|node| node.id.clone())
            .collect();
        let (webext, manifests) =
            adapters_lang_ts::webextension::extract_manifests(root, &ts_id, &known_files)
                .map_err(|e| e.to_string())?;
        layers.webext = LayerSummary {
            files: manifests,
            nodes: distinct_node_count(&webext.nodes),
            edges: distinct_edge_count(&webext.edges),
        };
        facts.extend(webext.nodes, webext.edges).map_err(pack_err)?;
        // Chrome runtime messaging sites join the same channel stitch as
        // SDK event sites: literals confirm, computed identities gap.
        // A capture's bytes, never the checkout, when one exists (AC-1022).
        extraction.event_sites.extend(match primary {
            Some(capture) => adapters_lang_ts::chrome_messaging::extract_captured(capture, &ts_id)
                .map_err(|e| e.to_string())?,
            None => adapters_lang_ts::chrome_messaging::extract_dir(root, &ts_id)
                .map_err(|e| e.to_string())?,
        });
        // Table-driven HTTP clients (#737): a joined call replaces the
        // generic computed-URL site the TS pass recorded at the same call.
        let joined = match primary {
            Some(capture) => adapters_lang_ts::client_table::extract_captured(capture, &ts_id)
                .map_err(|e| e.to_string())?,
            None => adapters_lang_ts::client_table::extract_dir(root, &ts_id)
                .map_err(|e| e.to_string())?,
        };
        let joined_at: std::collections::BTreeSet<(&str, u64)> = joined
            .iter()
            .map(|site| (site.path.as_str(), site.byte_start))
            .collect();
        extraction
            .fetch_sites
            .retain(|site| !joined_at.contains(&(site.path.as_str(), site.byte_start)));
        extraction.fetch_sites.extend(joined);
        // IndexedDB schema/store declarations and repository operations
        // become the cited data model (DataEntity + READS/WRITES).
        let idb = match primary {
            Some(capture) => adapters_lang_ts::indexeddb::extract_captured(capture, &ts_id)
                .map_err(|e| e.to_string())?,
            None => {
                adapters_lang_ts::indexeddb::extract_dir(root, &ts_id).map_err(|e| e.to_string())?
            }
        };
        layers.webext.nodes += distinct_node_count(&idb.nodes);
        layers.webext.edges += distinct_edge_count(&idb.edges);
        facts.extend(idb.nodes, idb.edges).map_err(pack_err)?;
        // Observed infrastructure (#734): storage, SDK and auth clients the
        // code proves it talks to, beside any IaC-declared resources.
        let observed = match primary {
            Some(capture) => {
                adapters_lang_ts::observed_infra::extract_captured(capture, &ts_id, registries)
                    .map_err(|e| e.to_string())?
            }
            None => adapters_lang_ts::observed_infra::extract_dir(root, &ts_id, registries)
                .map_err(|e| e.to_string())?,
        };
        layers.ts.nodes += distinct_node_count(&observed.nodes);
        layers.ts.edges += distinct_edge_count(&observed.edges);
        facts
            .extend(observed.nodes, observed.edges)
            .map_err(pack_err)?;
    }
    // Python carries server routes, `requests`/`httpx`/`aiohttp` client calls and
    // boto3/kafka-python event sites, so a client- or events-only Python
    // repository runs it too.
    if wants_application {
        let python_id = adapters_lang_python::SourceId { repo, commit };
        let mut python_progress = |path: &str| on_file(&format!("Reading Python sources — {path}"));
        // A capture holding the Python selection is parsed from its retained
        // bytes, and each file's own facts are bound to provenance-only
        // receipts (AC-1024); otherwise Python reads the checkout as before.
        let captured_python = captured_lane_paths(primary, ".py");
        let (python, stats) = if let (Some(capture), false) = (primary, captured_python.is_empty())
        {
            let python = adapters_lang_python::extract_dir_from_sources_within(
                root,
                &python_id,
                &captured_python,
                &|path| {
                    capture
                        .file(path)
                        .map(|file| file.bytes().to_vec())
                        .map_err(|_| std::io::Error::other("captured Python file is unavailable"))
                },
                &mut python_progress,
                &mut |_, _| {},
                limits.python,
            )
            .map_err(|error| error.to_string())?;
            // Facts are bound in their final form, after directory resolution,
            // so those the join builds are covered too (AC-1037).
            stage_lane_publication_receipts(
                adapters_lang_ts::captured::Grammar::Python,
                capture,
                &captured_python,
                &ts_id,
                &python.nodes,
                &python.edges,
                receipts,
            );
            let stats = adapters_lang_python::IncrementalStats {
                recomputed_files: captured_python.len() as u64,
                ..Default::default()
            };
            (python, stats)
        } else {
            adapters_lang_python::extract_dir_incremental_within(
                root,
                &python_id,
                &mut cache.python,
                &mut python_progress,
                limits.python,
            )
            .map_err(|error| error.to_string())?
        };
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.python = LayerSummary {
            files: python
                .nodes
                .iter()
                .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
                .count() as u64,
            nodes: distinct_node_count(&python.nodes),
            edges: distinct_edge_count(&python.edges),
        };
        facts.extend(python.nodes, python.edges).map_err(pack_err)?;
        // Observed infrastructure from Python (#734): registry clients the
        // code proves it talks to (boto3, redis, smtplib, SQLAlchemy), and
        // `requests`/`httpx`/`aiohttp` calls as fetch sites (AC-0424, AC-0496), and boto3 /
        // kafka-python calls as event sites (AC-0458).
        let observed = adapters_lang_python::observed_infra::extract_dir_with_sites(
            root, &python_id, registries,
        )
        .map_err(|error| error.to_string())?;
        extraction.fetch_sites.extend(observed.fetch_sites);
        extraction.event_sites.extend(observed.event_sites);
        layers.python.nodes += distinct_node_count(&observed.nodes);
        layers.python.edges += distinct_edge_count(&observed.edges);
        facts
            .extend(observed.nodes, observed.edges)
            .map_err(pack_err)?;
    }
    // Go carries server routes and `net/http` client calls (AC-0434), so a
    // client-only Go repository runs it too.
    if wants_application {
        let go_id = adapters_lang_go::SourceId { repo, commit };
        let mut go_progress = |path: &str| on_file(&format!("Reading Go sources — {path}"));
        // As for Python, a capture holding the Go selection is parsed from its
        // retained bytes with provenance-only Go receipts (AC-1026); `go.mod`,
        // module layout and Vercel configuration are still read from `root`.
        let captured_go = captured_lane_paths(primary, ".go");
        let (go, stats) = if let (Some(capture), false) = (primary, captured_go.is_empty()) {
            let go = adapters_lang_go::extract_dir_from_sources(
                root,
                &go_id,
                &captured_go,
                &|path| {
                    capture
                        .file(path)
                        .map(|file| file.bytes().to_vec())
                        .map_err(|_| std::io::Error::other("captured Go file is unavailable"))
                },
                &mut go_progress,
                &mut |_, _| {},
            )
            .map_err(|error| error.to_string())?;
            // Facts are bound in their final form, after directory resolution,
            // so those the join builds are covered too (AC-1037).
            stage_lane_publication_receipts(
                adapters_lang_ts::captured::Grammar::Go,
                capture,
                &captured_go,
                &ts_id,
                &go.nodes,
                &go.edges,
                receipts,
            );
            let stats = adapters_lang_go::IncrementalStats {
                recomputed_files: captured_go.len() as u64,
                ..Default::default()
            };
            (go, stats)
        } else {
            adapters_lang_go::extract_dir_incremental_with_progress(
                root,
                &go_id,
                &mut cache.go,
                &mut go_progress,
            )
            .map_err(|error| error.to_string())?
        };
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.go = LayerSummary {
            files: go
                .nodes
                .iter()
                .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
                .count() as u64,
            nodes: distinct_node_count(&go.nodes),
            edges: distinct_edge_count(&go.edges),
        };
        facts.extend(go.nodes, go.edges).map_err(pack_err)?;
        extraction.fetch_sites.extend(go.fetch_sites);
        extraction.event_sites.extend(go.event_sites);
    }
    // Rust carries server routes (axum, actix-web) and client calls
    // (reqwest), so a client-only Rust repository runs it too, as Java does.
    if wants_application {
        let rust_id = adapters_lang_rust::SourceId { repo, commit };
        let mut rust_progress = |path: &str| on_file(&format!("Reading Rust sources — {path}"));
        // As for the other lanes, a capture holding the Rust selection is
        // parsed from its retained bytes with provenance-only Rust receipts
        // (AC-1033); crate discovery still reads `Cargo.toml` from `root`.
        let captured_rust = captured_lane_paths(primary, ".rs");
        let (rust, stats) = if let (Some(capture), false) = (primary, captured_rust.is_empty()) {
            let rust = adapters_lang_rust::extract_dir_from_sources(
                root,
                &rust_id,
                &captured_rust,
                &|path| {
                    capture
                        .file(path)
                        .map(|file| file.bytes().to_vec())
                        .map_err(|_| std::io::Error::other("captured Rust file is unavailable"))
                },
                &mut rust_progress,
                &mut |_, _| {},
            )
            .map_err(|error| error.to_string())?;
            // Facts are bound in their final form, after directory resolution,
            // so those the join builds are covered too (AC-1037).
            stage_lane_publication_receipts(
                adapters_lang_ts::captured::Grammar::Rust,
                capture,
                &captured_rust,
                &ts_id,
                &rust.nodes,
                &rust.edges,
                receipts,
            );
            let stats = adapters_lang_rust::IncrementalStats {
                recomputed_files: captured_rust.len() as u64,
                ..Default::default()
            };
            (rust, stats)
        } else {
            adapters_lang_rust::extract_dir_incremental_with_progress(
                root,
                &rust_id,
                &mut cache.rust,
                &mut rust_progress,
            )
            .map_err(|error| error.to_string())?
        };
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.rust = LayerSummary {
            files: rust
                .nodes
                .iter()
                .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
                .count() as u64,
            nodes: distinct_node_count(&rust.nodes),
            edges: distinct_edge_count(&rust.edges),
        };
        facts.extend(rust.nodes, rust.edges).map_err(pack_err)?;
        extraction.fetch_sites.extend(rust.fetch_sites);
        extraction.event_sites.extend(rust.event_sites);
    }
    // Java and Kotlin carry server routes and client calls (Retrofit), so a
    // client-only Android or JVM repository runs them too (#757 review).
    if wants_application {
        let java_id = adapters_lang_java::SourceId { repo, commit };
        let mut java_progress = |path: &str| on_file(&format!("Reading Java sources — {path}"));
        // As for Python and Go, a capture holding the Java selection is parsed
        // from its retained bytes with provenance-only Java receipts
        // (AC-1031); build and configuration files are still read from `root`.
        let captured_java = captured_lane_paths(primary, ".java");
        let (java, stats) = if let (Some(capture), false) = (primary, captured_java.is_empty()) {
            let java = adapters_lang_java::extract_dir_from_sources(
                root,
                &java_id,
                &captured_java,
                &|path| {
                    capture
                        .file(path)
                        .map(|file| file.bytes().to_vec())
                        .map_err(|_| std::io::Error::other("captured Java file is unavailable"))
                },
                &mut java_progress,
                &mut |_, _| {},
            )
            .map_err(|error| error.to_string())?;
            // Facts are bound in their final form, after directory resolution,
            // so those the join builds are covered too (AC-1037).
            stage_lane_publication_receipts(
                adapters_lang_ts::captured::Grammar::Java,
                capture,
                &captured_java,
                &ts_id,
                &java.nodes,
                &java.edges,
                receipts,
            );
            let stats = adapters_lang_java::IncrementalStats {
                recomputed_files: captured_java.len() as u64,
                ..Default::default()
            };
            (java, stats)
        } else {
            adapters_lang_java::extract_dir_incremental_with_progress(
                root,
                &java_id,
                &mut cache.java,
                &mut java_progress,
            )
            .map_err(|error| error.to_string())?
        };
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.java = LayerSummary {
            files: java
                .nodes
                .iter()
                .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
                .count() as u64,
            nodes: distinct_node_count(&java.nodes),
            edges: distinct_edge_count(&java.edges),
        };
        facts.extend(java.nodes, java.edges).map_err(pack_err)?;
        extraction.fetch_sites.extend(java.fetch_sites);
        extraction.event_sites.extend(java.event_sites);

        let kotlin_id = adapters_lang_kotlin::SourceId { repo, commit };
        let mut kotlin_progress = |path: &str| on_file(&format!("Reading Kotlin sources — {path}"));
        // As for the other lanes, a capture holding the Kotlin selection is
        // parsed from its retained bytes with provenance-only Kotlin receipts
        // (AC-1035); Java type discovery for interop still reads `root`.
        let mut captured_kotlin = captured_lane_paths(primary, ".kt");
        captured_kotlin.extend(captured_lane_paths(primary, ".kts"));
        captured_kotlin.sort();
        let (kotlin, stats) = if let (Some(capture), false) = (primary, captured_kotlin.is_empty())
        {
            let kotlin = adapters_lang_kotlin::extract_dir_from_sources(
                root,
                &kotlin_id,
                &captured_kotlin,
                &|path| {
                    capture
                        .file(path)
                        .map(|file| file.bytes().to_vec())
                        .map_err(|_| std::io::Error::other("captured Kotlin file is unavailable"))
                },
                &mut kotlin_progress,
                &mut |_, _| {},
            )
            .map_err(|error| error.to_string())?;
            // Facts are bound in their final form, after directory resolution,
            // so those the join builds are covered too (AC-1037).
            stage_lane_publication_receipts(
                adapters_lang_ts::captured::Grammar::Kotlin,
                capture,
                &captured_kotlin,
                &ts_id,
                &kotlin.nodes,
                &kotlin.edges,
                receipts,
            );
            let stats = adapters_lang_kotlin::IncrementalStats {
                recomputed_files: captured_kotlin.len() as u64,
                ..Default::default()
            };
            (kotlin, stats)
        } else {
            adapters_lang_kotlin::extract_dir_incremental_with_progress(
                root,
                &kotlin_id,
                &mut cache.kotlin,
                &mut kotlin_progress,
            )
            .map_err(|error| error.to_string())?
        };
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.kotlin = LayerSummary {
            files: kotlin
                .nodes
                .iter()
                .filter(|node| node.label == "File" && node.props.get("placeholder").is_none())
                .count() as u64,
            nodes: distinct_node_count(&kotlin.nodes),
            edges: distinct_edge_count(&kotlin.edges),
        };
        facts.extend(kotlin.nodes, kotlin.edges).map_err(pack_err)?;
        extraction.fetch_sites.extend(kotlin.fetch_sites);
        extraction.event_sites.extend(kotlin.event_sites);
    }
    if wants_infra {
        let tf_id = iac::SourceId { repo, commit };
        let mut tf_progress =
            |path: &str| on_file(&format!("Reading infrastructure (Terraform) — {path}"));
        let (tf, stats) = iac::extract_dir_incremental_with_progress(
            root,
            &tf_id,
            &mut cache.tf,
            &mut tf_progress,
        )
        .map_err(|e| e.to_string())?;
        delta.add(
            stats.recomputed_files,
            stats.reused_files,
            stats.deleted_files,
        );
        layers.tf = LayerSummary {
            // Distinct physical files this run actually parsed or reused,
            // which already includes every local-module instantiation's
            // files (an explicit `source` reference overrides `.gitignore`,
            // #468, ADR-0034) — matching extraction file-for-file, unlike a
            // separate filesystem walk that doesn't know which directories
            // module declarations reference. Deduplicated across module
            // contexts (`distinct_files`, not `recomputed_files +
            // reused_files`), so a local directory instantiated by two
            // module blocks doesn't inflate this denominator relative to
            // `metrics::compute`'s `files_with_facts`, which dedups the same
            // way by provenance `repo:path`.
            files: stats.distinct_files,
            nodes: distinct_node_count(&tf.nodes),
            edges: distinct_edge_count(&tf.edges),
        };
        let tf_root_modules = tf.root_modules;
        facts.extend(tf.nodes, tf.edges).map_err(pack_err)?;
        // T1: observed state supersedes ambiguous T0 refs (AC-0009).
        if let Some(input) = &observed.state_json {
            let read = observed.read(input, "state_json")?;
            let state = dynamic::parse_state(&read.raw).map_err(|e| e.to_string())?;
            // ADR-0077: a state that cannot be tied to one root module is
            // not applied; its Gap joins the graph instead.
            facts
                .update_nodes(&["Resource"], |nodes, added| {
                    let mut enrichment = dynamic::enrich_resources(
                        nodes,
                        repo,
                        &state,
                        &read.cited_path,
                        &read.raw,
                        &tf_root_modules,
                    );
                    observed.cite(
                        nodes
                            .iter_mut()
                            .chain(enrichment.unapplied.as_mut())
                            .map(|node| &mut node.props),
                    );
                    added.extend(enrichment.unapplied);
                })
                .map_err(pack_err)?;
        }
        if let Some(input) = &observed.pulumi_json {
            let read = observed.read(input, "pulumi_json")?;
            let deployment =
                dynamic::parse_pulumi_json(&read.raw).map_err(|error| error.to_string())?;
            facts
                .update_nodes(&["Resource"], |nodes, _| {
                    dynamic::enrich_pulumi_resources(nodes, &deployment, &read.cited_path);
                    observed.cite(nodes.iter_mut().map(|node| &mut node.props));
                })
                .map_err(pack_err)?;
        }
    }
    let mut read = ingest::manifest::repo_env_read(root).map_err(|e| e.to_string())?;
    read.extend(env_read.iter().cloned());
    let mut cfg = events::ConfigIndex::from_dir(root, &read).map_err(|e| e.to_string())?;
    cfg.apply_manifest(manifest_env, ingest::manifest::MANIFEST_NAME);
    cfg.defer_origins(deferred_origins);
    let ev_id = events::SourceId { repo, commit };
    // Stitching reads every layer's sites, so its facts are its own row,
    // never the TypeScript adapter's (#874).
    layers.events.files = extraction
        .event_sites
        .iter()
        .map(|site| site.path.as_str())
        .chain(extraction.fetch_sites.iter().map(|site| site.path.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64;
    let stitched = events::stitch(&extraction.event_sites, &cfg, &ev_id);
    layers.events.nodes += distinct_node_count(&stitched.nodes);
    layers.events.edges += distinct_edge_count(&stitched.edges);
    facts
        .extend(stitched.nodes, stitched.edges)
        .map_err(pack_err)?;
    let endpoints = facts.nodes_labeled(&["Endpoint"]).map_err(pack_err)?;
    let endpoint_ids: Vec<String> = endpoints.iter().map(|n| n.id.clone()).collect();
    // Go's `ServeMux` serves HEAD from a GET pattern (AC-0434).
    let get_serves_head: std::collections::BTreeSet<String> = endpoints
        .iter()
        .filter(|n| {
            n.label == "Endpoint"
                && n.props["framework"] == "net/http"
                && n.props["language"] == "go"
                && n.props["method"] == "GET"
        })
        .map(|n| n.id.clone())
        .collect();
    let prefix_kinds = events::prefix_kinds(&endpoints);
    drop(endpoints);
    let fetched = events::stitch_fetches_with(
        &extraction.fetch_sites,
        &endpoint_ids,
        &get_serves_head,
        &prefix_kinds,
        &cfg,
        &ev_id,
    );
    layers.events.nodes += distinct_node_count(&fetched.nodes);
    layers.events.edges += distinct_edge_count(&fetched.edges);
    facts
        .extend(fetched.nodes, fetched.edges)
        .map_err(pack_err)?;
    // T1: observed messaging identities fill only explicit channel Gaps;
    // observed HTTP attributes enrich T0 endpoints beside their provenance.
    // All trace files are applied together so an EventBridge identity
    // conflict split across files is still seen (#573).
    let mut traces = Vec::new();
    for input in &observed.otel_jsonl {
        let read = observed.read(input, "otel_jsonl")?;
        let trace = dynamic::parse_otlp_jsonl(&read.raw).map_err(|e| e.to_string())?;
        traces.push((trace, read.cited_path));
    }
    let traces: Vec<(&dynamic::ObservedTrace, &str)> = traces
        .iter()
        .map(|(trace, path)| (trace, path.as_str()))
        .collect();
    let gaps = facts.nodes_labeled(&["Gap"]).map_err(pack_err)?;
    if dynamic::traces_apply(&gaps, &traces) {
        // The pass reads and edits across labels, so it runs decoded, and
        // only when it can change anything.
        let (mut nodes, mut edges) = std::mem::take(&mut facts).into_facts().map_err(pack_err)?;
        dynamic::apply_traces(&mut nodes, &mut edges, &traces);
        observed.cite(
            nodes
                .iter_mut()
                .map(|node| &mut node.props)
                .chain(edges.iter_mut().map(|edge| &mut edge.props)),
        );
        facts = FactStore::from_facts(nodes, edges).map_err(pack_err)?;
    }
    drop(gaps);
    // Found ADR/RFC files are T0 facts. They are parsed after all code/infra
    // layers so explicit `Governs:` ids can link to existing graph targets.
    // A file too large or not UTF-8 is skipped and named in coverage, never
    // fatal to the other decisions (#1847).
    //
    // The same walk reads the documentation files for the coverage inventory
    // and the declared API references as documents, so each file is read
    // once (ADR-0065 decision 6, AC-1600).
    let api_docs = ingest::api_docs::load_api_docs_within(root, api_docs, limits.api_docs);
    let declared_api_docs = declared_api_docs(&api_docs);
    let adr_read = {
        let ids: std::collections::BTreeSet<&str> =
            facts.nodes().iter().map(|node| node.id.as_str()).collect();
        let is_candidate = |id: &str| ids.contains(id);
        let read = spec::read_documentation(
            root,
            repo,
            commit,
            &spec::DocumentationCases {
                documents: true,
                api_references: &declared_api_docs,
                decisions: Some(&is_candidate),
            },
        )
        .map_err(|error| error.to_string())?;
        reads.documents = document_coverage(read.documents.counts);
        read.decisions
    };
    reads.adrs = ingest::coverage::ProducerCount {
        files: adr_read.files,
        nodes: distinct_node_count(&adr_read.facts.nodes),
        edges: distinct_edge_count(&adr_read.facts.edges),
    };
    for unread in adr_read.unread {
        match unread.reason {
            spec::UnreadAdrReason::TooLarge => reads.adr_too_large.push(unread.path),
            spec::UnreadAdrReason::NotUtf8 => reads.adr_not_utf8.push(unread.path),
        }
    }
    facts
        .extend(adr_read.facts.nodes, adr_read.facts.edges)
        .map_err(pack_err)?;
    let (contract_facts, contract_files) =
        extract_repo_contracts(repo, commit, api_docs, declared_api_docs, &facts)?;
    reads.contracts = ingest::coverage::ProducerCount {
        files: contract_files,
        nodes: distinct_node_count(&contract_facts.nodes),
        edges: distinct_edge_count(&contract_facts.edges),
    };
    facts
        .extend(contract_facts.nodes, contract_facts.edges)
        .map_err(pack_err)?;
    let test_drift = spec::derive_test_drift(
        &facts
            .nodes_labeled(&["TestCase", "Endpoint"])
            .map_err(pack_err)?,
        &facts
            .edges_where(|edge| edge.label == "REQUESTS")
            .map_err(pack_err)?,
    );
    facts
        .extend(test_drift.nodes, test_drift.edges)
        .map_err(pack_err)?;
    // Toolchain facts (#215): config files become Tool nodes with cited
    // settings, DEFINED_IN the config File that proves them. Runs for every
    // layer selection — the toolchain is cross-cutting evidence.
    {
        let tool_id = ingest::toolchain::SourceId { repo, commit };
        let mut tool_progress = |path: &str| on_file(&format!("Reading configuration — {path}"));
        let tool_facts =
            ingest::toolchain::extract_dir_reading(root, &tool_id, &read, &mut tool_progress)
                .map_err(|e| e.to_string())?;
        layers.tools = LayerSummary {
            files: tool_facts.files,
            nodes: distinct_node_count(&tool_facts.nodes),
            edges: distinct_edge_count(&tool_facts.edges),
        };
        // Config files an adapter already owns (a `.ts`-authored vite
        // config, a webext manifest) keep the adapter's richer File node;
        // the DEFINED_IN edge targets the same id either way. An import
        // placeholder (an imported `package.json`, #237) owns nothing: the
        // parsed config's File node supersedes it (last occurrence wins).
        let known_files = unplaceholdered_set(&facts, "File")?;
        facts
            .extend(
                tool_facts
                    .nodes
                    .into_iter()
                    .filter(|node| node.label != "File" || !known_files.contains(&node.id)),
                tool_facts.edges,
            )
            .map_err(pack_err)?;
        // An env-referenced observed resource (#734) names the env files
        // that declare its key; key names only, never values (AC-0097).
        let env_links = ingest::toolchain::link_env_resources(
            &facts
                .nodes_labeled(&["Tool", "Resource"])
                .map_err(pack_err)?,
            &tool_id,
        );
        layers.tools.edges += distinct_edge_count(&env_links);
        facts.extend([], env_links).map_err(pack_err)?;
    }
    // Scheduled jobs (#770): declared schedule files become Schedule flow
    // triggers with an ENTRY into the file each job runs, or into the
    // adapter's function a proven argument dispatch selects (#779). An
    // adapter's File node for that file supersedes the schedule reader's.
    {
        let schedule_id = ingest::schedules::SourceId { repo, commit };
        let symbols = unplaceholdered_set(&facts, "Symbol")?;
        let schedule_facts = ingest::schedules::extract_dir(root, &schedule_id, &symbols)
            .map_err(|e| e.to_string())?;
        layers.schedules = LayerSummary {
            files: schedule_facts.files,
            nodes: distinct_node_count(&schedule_facts.nodes),
            edges: distinct_edge_count(&schedule_facts.edges),
        };
        // Files a job enters are that job's entry, not another (#1997).
        let scheduled: std::collections::BTreeSet<String> = schedule_facts
            .nodes
            .iter()
            .filter(|node| node.label == "File")
            .filter_map(|node| node.props["path"].as_str().map(str::to_owned))
            .collect();
        let known_files = unplaceholdered_set(&facts, "File")?;
        facts
            .extend(
                schedule_facts
                    .nodes
                    .into_iter()
                    .filter(|node| node.label != "File" || !known_files.contains(&node.id)),
                schedule_facts.edges,
            )
            .map_err(pack_err)?;
        // Entry points beyond HTTP (#1997): each process start (a Python
        // `__main__` or operator script, the container command, a Procfile
        // process, a declared console script or `bin`) becomes an
        // EntryPoint flow trigger with an ENTRY into the file it runs, or a
        // Gap naming it. An adapter's File node supersedes the reader's.
        let entry_facts =
            ingest::entry_points::extract_dir(root, &schedule_id, &symbols, &scheduled)
                .map_err(|e| e.to_string())?;
        layers.entry_points = LayerSummary {
            files: entry_facts.files,
            nodes: distinct_node_count(&entry_facts.nodes),
            edges: distinct_edge_count(&entry_facts.edges),
        };
        let known_files = unplaceholdered_set(&facts, "File")?;
        facts
            .extend(
                entry_facts
                    .nodes
                    .into_iter()
                    .filter(|node| node.label != "File" || !known_files.contains(&node.id)),
                entry_facts.edges,
            )
            .map_err(pack_err)?;
    }
    // Build and deployment order (#1790, AC-0598): workflows and Dockerfiles
    // become ordered Pipeline facts. Runs for every layer selection, as the
    // toolchain reader does. The reader emits no File node: a file it could
    // not read exactly yields no facts and is named by Preflight's
    // `unreadable-pipeline-file` finding, which the coverage inventory lists.
    {
        let pipeline_id = ingest::pipelines::SourceId { repo, commit };
        let pipeline_facts =
            ingest::pipelines::extract_dir(root, &pipeline_id).map_err(|e| e.to_string())?;
        layers.pipelines = LayerSummary {
            files: pipeline_facts.files,
            nodes: distinct_node_count(&pipeline_facts.nodes),
            edges: distinct_edge_count(&pipeline_facts.edges),
        };
        facts
            .extend(pipeline_facts.nodes, pipeline_facts.edges)
            .map_err(pack_err)?;
    }
    // Environments (#1791, AC-0661, AC-0662, ADR-0070): each Compose file
    // is an Environment with a Workload per service; each kustomization is
    // an Environment that DECLARES the pod-bearing Kubernetes manifest
    // workloads its `resources` chain reaches and PATCHES those its patch
    // files name; an environment file that names an environment only by its
    // file name is a Gap. Runs for every layer selection, as the toolchain
    // reader does. The reader emits no File node and leaves the toolchain
    // reader's Compose `Tool` fact and the schedule reader's `CronJob`
    // facts as they are. Helm charts are workloads and helmfile releases
    // are environments (AC-1502); manifests of other kinds are named by
    // Preflight's findings, which the coverage inventory lists.
    {
        let environment_id = ingest::environments::SourceId { repo, commit };
        let environment_facts =
            ingest::environments::extract_dir(root, &environment_id).map_err(|e| e.to_string())?;
        layers.environments = LayerSummary {
            files: environment_facts.files,
            nodes: distinct_node_count(&environment_facts.nodes),
            edges: distinct_edge_count(&environment_facts.edges),
        };
        facts
            .extend(environment_facts.nodes, environment_facts.edges)
            .map_err(pack_err)?;
    }
    // Gated plugin adapters (#201): route the files each active plugin
    // claims via its golden-corpus extensions. Every fact arrives pinned to
    // the exact artifact by the host, and a failed call yields zero partial
    // facts (AC-0070). A failure that is a function of the plugin and the
    // file alone refuses that file: no fact from it by that plugin joins the
    // graph, coverage names it, and the other files and plugins continue
    // (#1810, AC-0658). Any other failure (the wall-clock deadline, the
    // native stack, instantiation, IO) still fails the ingest, because
    // skipping the file would make the graph depend on the machine.
    if !plugins.is_empty() {
        let host = adapters_plugin_host::PluginHost::new().map_err(|e| e.to_string())?;
        let plugin_source = adapters_plugin_host::SourceId {
            repo: repo.to_string(),
            commit: commit.to_string(),
        };
        for plugin in plugins {
            let wasm_bytes = std::fs::read(&plugin.path).map_err(|e| e.to_string())?;
            // The gate verdict binds to exact bytes: a swap since gating
            // fails closed rather than running ungated code.
            if core_prov::content_hash(&wasm_bytes) != plugin.content_hash {
                return Err(format!(
                    "plugin {} changed on disk since its gate passed — re-run the \
                     conformance gate",
                    plugin.plugin_id
                ));
            }
            let loaded = host
                .load(&wasm_bytes, &plugin.plugin_id)
                .map_err(|e| e.to_string())?;
            let claimed = adapters_plugin_host::route::extract_claimed_counted(
                &host,
                &loaded,
                root,
                &plugin.extensions,
                &plugin_source,
                plugin.limits,
            )
            .map_err(|e| e.to_string())?;
            let plugin_facts = claimed.facts;
            reads.plugins.push(ingest::coverage::PluginRead {
                id: adapters_plugin_host::pinned_extractor_id(
                    &plugin.plugin_id,
                    &plugin.content_hash,
                ),
                count: ingest::coverage::ProducerCount {
                    files: claimed.files_read,
                    nodes: distinct_node_count(&plugin_facts.nodes),
                    edges: distinct_edge_count(&plugin_facts.edges),
                },
                refused: claimed
                    .refused
                    .into_iter()
                    .map(|file| (file.path, refusal_reason(file.reason)))
                    .collect(),
            });
            facts
                .extend(plugin_facts.nodes, plugin_facts.edges)
                .map_err(pack_err)?;
        }
    }
    adapters_lang_ts::close_over_fact_endpoints(&mut facts).map_err(pack_err)?;
    // Each language adapter closed over its own extraction; a shared id
    // (`mod:foo` imported from TS and Go) can now carry one adapter's Gap
    // and another's Confirmed boundary. The store keeps the last duplicate,
    // so reconcile first: a Gap is never shown as Confirmed (#237 review).
    facts.reconcile_placeholders().map_err(pack_err)?;
    Ok((extraction, facts, layers, delta))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn extract_tree(
    root: &std::path::Path,
    repo: &str,
    commit: &str,
    layers: &[String],
    manifest_env: &std::collections::BTreeMap<String, String>,
    state_json: Option<&std::path::Path>,
    pulumi_json: Option<&std::path::Path>,
    otel_jsonl: &[std::path::PathBuf],
) -> Result<adapters_lang_ts::Extraction, String> {
    extract_tree_with_summary(
        root,
        repo,
        commit,
        layers,
        manifest_env,
        state_json,
        pulumi_json,
        otel_jsonl,
    )
    .map(|(extraction, _)| extraction)
}

/// Load an extraction plus its `Repo` node (`repo:{identity}`, carrying the
/// tree root and commit so evidence reads resolve per repo).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ReconcileStats {
    inserted_or_updated: u64,
    unchanged: u64,
    deleted: u64,
    published: PublishedFacts,
}

fn fact_owned_by_repo(props: &serde_json::Value, repo: &str) -> bool {
    serde_json::from_value::<core_prov::Provenance>(props.get("prov").cloned().unwrap_or_default())
        .ok()
        .is_some_and(|provenance| {
            !provenance.evidence.is_empty()
                && provenance
                    .evidence
                    .iter()
                    .all(|evidence| evidence.repo == repo)
        })
}

/// The validated API reference documents of a load, as the spec crate's
/// declared documents.
fn declared_api_docs(loaded: &ingest::api_docs::LoadedApiDocs) -> Vec<spec::DeclaredApiDoc> {
    loaded
        .docs
        .iter()
        .map(|doc| spec::DeclaredApiDoc {
            path: doc.path.clone(),
            prefix: doc.prefix.clone(),
            contents: doc.contents.clone(),
        })
        .collect()
}

/// Contract facts of the API references `loaded` declared; `docs` are its
/// validated documents, read once for these and for the documentation walk.
fn extract_repo_contracts(
    repo: &str,
    commit: &str,
    loaded: ingest::api_docs::LoadedApiDocs,
    docs: Vec<spec::DeclaredApiDoc>,
    facts: &FactStore,
) -> Result<(spec::ContractFacts, u64), String> {
    if !loaded.declared {
        return Ok((spec::ContractFacts::default(), 0));
    }
    // Declared API docs match recovered facts by id and Endpoint attributes:
    // only Endpoint nodes are decoded, and only for a repository that
    // declares docs (#1574).
    let endpoints = facts.nodes_labeled(&["Endpoint"]).map_err(pack_err)?;
    let ids: std::collections::BTreeSet<&str> =
        facts.nodes().iter().map(|node| node.id.as_str()).collect();
    let gaps: Vec<spec::ContractValidationGap> = loaded
        .gaps
        .into_iter()
        .map(|gap| spec::ContractValidationGap {
            doc_path: gap.doc_path,
            reason: gap.reason,
        })
        .collect();
    let files = docs.len() as u64;
    Ok((
        spec::extract_contract_facts_among(repo, commit, &docs, &gaps, &endpoints, |id| {
            ids.contains(id)
        }),
        files,
    ))
}

fn id_explicitly_owned_by_repo(id: &str, repo: &str) -> bool {
    id == format!("repo:{repo}")
        || id.starts_with(&format!("file:{repo}@"))
        || id.starts_with(&format!("sym:{repo}@"))
        || id.starts_with(&format!("ep:{repo}@"))
        || id.starts_with(&format!("res:{repo}@"))
        || id.starts_with(&format!("screen:{repo}@"))
        || id.starts_with(&format!("adr:{repo}@"))
        || id.starts_with(&format!("doc-route:{repo}@"))
        || id.starts_with(&format!("drift:contract:{repo}@"))
        || id.starts_with(&format!("gap:contract:{repo}@"))
        || id.starts_with(&format!("tool:{repo}@"))
        || id.starts_with(&format!("gap:call:{repo}@"))
        || id.starts_with(&format!("gap:chan:{repo}@"))
        || id.starts_with(&format!("gap:fetch:{repo}@"))
        // A name-only environment Gap has no evidence span to own it by
        // (AC-0097), like the environment-file `Tool` fact (#1791).
        || id.starts_with(&format!("gap:environment:{repo}@"))
        // The unapplied-state Gap cites only the state file, an input
        // artifact with no repository evidence (AC-1501, ADR-0077).
        || id == format!("gap:tf-state-root:{repo}@")
}

#[cfg(test)]
fn load_into_graph(
    graph: &mut SqliteGraphStore,
    extraction: &adapters_lang_ts::Extraction,
    repo: &str,
    _root: &std::path::Path,
    commit: &str,
) -> Result<ReconcileStats, String> {
    load_into_graph_with_bindings(graph, extraction, repo, _root, commit, &[], None)
}

/// Publish `repo`'s facts, its source bindings and its coverage inventory in
/// one write transaction; a publication without an inventory clears the
/// repo's previous one rather than leaving it to describe other facts.
#[cfg(test)]
fn load_into_graph_with_bindings(
    graph: &mut SqliteGraphStore,
    extraction: &adapters_lang_ts::Extraction,
    repo: &str,
    root: &std::path::Path,
    commit: &str,
    bindings: &[core_graph::source::SourceBinding],
    coverage: Option<&core_graph::coverage::CoverageRecord>,
) -> Result<ReconcileStats, String> {
    load_into_graph_with_bindings_and_omissions(
        graph,
        extraction.clone(),
        repo,
        root,
        commit,
        bindings,
        coverage,
        core_graph::source::SourceOmissions::NONE,
    )
}

/// ADR-0046 decision 5: once the graph holds a publish, release its prepared
/// input and reclaim the receipts it superseded. Best effort: a failure keeps
/// the old receipts retained and never undoes the publish.
fn reclaim_superseded_receipts(
    state: &AppState,
    source: &RegisteredSource,
    primary: primary_source::PrimaryInput,
    input_capture: Option<&str>,
) {
    // The code capture supersedes the source's own receipts and the input
    // capture its observed ones; a source with no captured code of its own
    // may still have captured observation inputs (ADR-0079).
    let capture_id = primary
        .capture
        .as_ref()
        .map(|capture| capture.id().to_owned());
    if capture_id.is_none() && input_capture.is_none() {
        return;
    }
    // The prepared input holds the shared retention guard; the reclaim
    // needs the exclusive one.
    drop(primary);
    if primary_source::reclaim_superseded_by(state, source, capture_id.as_deref(), input_capture)
        .is_err()
    {
        eprintln!("cartograph: superseded receipts were kept; they settle on the next recovery");
    }
}

/// `Node` equality on packed facts: equal bytes are equal facts, and only
/// differing bytes are decoded, so a `-0.0` or other equal-value spelling is
/// still judged by value exactly as an unpacked comparison would (#725).
fn same_node(a: &PackedNode, b: &PackedNode) -> Result<bool, core_graph::GraphError> {
    Ok(a.id == b.id
        && a.label == b.label
        && (a.props_json() == b.props_json() || a.unpack()?.props == b.unpack()?.props))
}

/// [`same_node`] for edges.
fn same_edge(a: &PackedEdge, b: &PackedEdge) -> Result<bool, core_graph::GraphError> {
    Ok(a.src == b.src
        && a.dst == b.dst
        && a.label == b.label
        && (a.props_json() == b.props_json() || a.unpack()?.props == b.unpack()?.props))
}

/// Publish `repo`'s facts, its source bindings, its coverage inventory and its
/// per-fact receipt omission records in one write transaction.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn load_into_graph_with_bindings_and_omissions(
    graph: &mut SqliteGraphStore,
    extraction: adapters_lang_ts::Extraction,
    repo: &str,
    root: &std::path::Path,
    commit: &str,
    bindings: &[core_graph::source::SourceBinding],
    coverage: Option<&core_graph::coverage::CoverageRecord>,
    omissions: core_graph::source::SourceOmissions<'_>,
) -> Result<ReconcileStats, String> {
    let adapters_lang_ts::Extraction { nodes, edges, .. } = extraction;
    let facts = FactStore::from_facts(nodes, edges).map_err(pack_err)?;
    load_packed_into_graph(
        graph,
        facts,
        repo,
        root,
        commit,
        bindings,
        &[],
        coverage,
        omissions,
    )
}

/// [`load_into_graph_with_bindings_and_omissions`] for facts already packed
/// in recovery order: nothing is decoded but repeated edges, whose evidence
/// is merged (#1574, #725). `observations` replaces the repo's observed
/// bindings (ADR-0079).
#[allow(clippy::too_many_arguments)]
fn load_packed_into_graph(
    graph: &mut SqliteGraphStore,
    facts: FactStore,
    repo: &str,
    _root: &std::path::Path,
    commit: &str,
    bindings: &[core_graph::source::SourceBinding],
    observations: &[core_graph::source::SourceBinding],
    coverage: Option<&core_graph::coverage::CoverageRecord>,
    omissions: core_graph::source::SourceOmissions<'_>,
) -> Result<ReconcileStats, String> {
    let repo_prov = core_prov::Provenance::new(
        core_prov::Tier::Deterministic,
        core_prov::ConfidenceTier::Confirmed,
        vec![],
        "app.ingest",
        &serde_json::to_vec(&("registered-repo-v1", repo, commit)).expect("serializes"),
    )
    .expect("within ceiling");
    let repo_node = Node {
        id: format!("repo:{repo}"),
        label: "Repo".into(),
        props: serde_json::json!({
            "commit": commit,
            "prov": serde_json::to_value(repo_prov).expect("serializes"),
        }),
    };
    // Last occurrence wins, in deterministic extraction order (SPEC-00 §4.4).
    // Facts move out of the extraction and are packed as they move, so the
    // publish holds compact JSON rather than `Value` trees (#1235, #725).
    let (nodes, edges) = facts.into_packed();
    let extracted_nodes = nodes.len();
    let pack = |error: core_graph::GraphError| error.to_string();
    let mut current_nodes = std::collections::BTreeMap::<String, PackedNode>::new();
    let mut colliding_ids = std::collections::BTreeSet::new();
    for node in nodes {
        let id = node.id.clone();
        if let Some(previous) = current_nodes.insert(id.clone(), node)
            && !same_node(&previous, &current_nodes[&id]).map_err(pack)?
        {
            colliding_ids.insert(id);
        }
    }
    let merged = MergedFacts {
        nodes: (extracted_nodes - current_nodes.len()) as u64,
        edges: edges.len() as u64 - distinct_packed_edge_count(&edges),
        node_collisions: colliding_ids.len() as u64,
    };
    current_nodes.insert(
        repo_node.id.clone(),
        PackedNode::pack(repo_node).map_err(pack)?,
    );
    let current_edges = merge_packed_edge_occurrences(edges).map_err(pack)?;
    // Packed, never decoded whole: the snapshot is the whole graph across
    // every repo, so a `Value` copy would double the publish peak (#725).
    let expected = graph.read_packed_snapshot().map_err(pack)?;
    let existing_nodes = expected
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<std::collections::BTreeMap<_, _>>();
    let existing_edges = expected
        .edges
        .iter()
        .map(|edge| {
            (
                (edge.src.clone(), edge.dst.clone(), edge.label.clone()),
                edge,
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut patch = core_graph::GraphPatch::default();
    let mut stats = ReconcileStats {
        published: PublishedFacts {
            nodes: current_nodes.keys().cloned().collect(),
            edges: current_edges.keys().cloned().collect(),
            merged,
        },
        ..ReconcileStats::default()
    };
    let mut remaining_edge_keys = std::collections::BTreeSet::new();
    for (key, edge) in &existing_edges {
        if !current_edges.contains_key(key)
            && fact_owned_by_repo(&edge.unpack().map_err(pack)?.props, repo)
        {
            patch
                .delete_edges
                .push((edge.src.clone(), edge.dst.clone(), edge.label.clone()));
            stats.deleted += 1;
        } else {
            remaining_edge_keys.insert(key.clone());
        }
    }
    remaining_edge_keys.extend(current_edges.keys().cloned());
    // Endpoints of every remaining edge, once: not a scan per node.
    let incident: std::collections::BTreeSet<&str> = remaining_edge_keys
        .iter()
        .flat_map(|(src, dst, _)| [src.as_str(), dst.as_str()])
        .collect();
    for (&id, node) in &existing_nodes {
        if current_nodes.contains_key(id) {
            continue;
        }
        let props = node.unpack().map_err(pack)?.props;
        let has_remaining_incident = incident.contains(id);
        let owned = fact_owned_by_repo(&props, repo) || id_explicitly_owned_by_repo(id, repo);
        let orphan_placeholder =
            props["placeholder"].as_bool() == Some(true) && !has_remaining_incident;
        if (owned && (id_explicitly_owned_by_repo(id, repo) || !has_remaining_incident))
            || orphan_placeholder
        {
            patch.delete_node_ids.push(id.to_owned());
            stats.deleted += 1;
        }
    }
    // Only changed facts enter the patch, and they move there.
    for node in current_nodes.into_values() {
        match existing_nodes.get(node.id.as_str()) {
            Some(existing) if same_node(existing, &node).map_err(pack)? => stats.unchanged += 1,
            _ => {
                patch.upsert_packed_nodes.push(node);
                stats.inserted_or_updated += 1;
            }
        }
    }
    for (key, edge) in current_edges {
        match existing_edges.get(&key) {
            Some(existing) if same_edge(existing, &edge).map_err(pack)? => stats.unchanged += 1,
            _ => {
                patch.upsert_packed_edges.push(edge);
                stats.inserted_or_updated += 1;
            }
        }
    }
    if !graph
        .apply_patch_with_source_roles_and_coverage_if_snapshot_matches(
            &expected,
            &patch,
            repo,
            bindings,
            observations,
            coverage,
            omissions,
        )
        .map_err(|error| error.to_string())?
    {
        return Err("Graph changed during recovery publication; recover the source again.".into());
    }
    Ok(stats)
}

#[cfg(test)]
fn deterministic_graph_hashes(graph: &impl GraphStore) -> Result<Vec<String>, String> {
    fn hash(props: &serde_json::Value) -> Option<&str> {
        (props["prov"]["tier"].as_str() == Some("Deterministic"))
            .then(|| props["prov"]["content_hash"].as_str())
            .flatten()
    }
    let (nodes, edges) = graph.read_snapshot().map_err(|error| error.to_string())?;
    let mut hashes = nodes
        .into_iter()
        .filter_map(|node| hash(&node.props).map(|hash| format!("node:{}:{hash}", node.id)))
        .chain(edges.into_iter().filter_map(|edge| {
            hash(&edge.props)
                .map(|hash| format!("edge:{}:{}:{}:{hash}", edge.src, edge.dst, edge.label))
        }))
        .collect::<Vec<_>>();
    hashes.sort();
    Ok(hashes)
}

/// Join observed infra to the event layer: insert a `BACKS` edge wherever
/// an enriched `Resource`'s observed identity names a `Channel` that code
/// actually publishes or subscribes (SPEC-00 §4.1, M6). Runs over the
/// whole graph after every load. Existing state-derived edges are reconciled
/// against the current candidates before `put_edge` upserts, so removed or
/// changed observations cannot leave stale cross-layer topology behind.
fn stitch_backings(
    graph: &mut SqliteGraphStore,
    facts: &mut OperationFacts,
    registries: &Registries,
) -> Result<u64, String> {
    let resources = graph
        .nodes_with_label("Resource")
        .map_err(|e| e.to_string())?;
    let mut candidates = std::collections::BTreeMap::new();
    for edge in dynamic::backing_candidates(&resources, registries) {
        let channel_exists = graph
            .get_node(&edge.dst)
            .map_err(|e| e.to_string())?
            .is_some();
        if channel_exists {
            candidates.insert(edge_key(&edge), edge);
        }
    }
    for edge in graph
        .edges_with_labels(&["BACKS"])
        .map_err(|error| error.to_string())?
    {
        let extractor_id = edge.props["prov"]["extractor_id"].as_str();
        let is_observed_backing = matches!(
            extractor_id,
            Some(dynamic::EXTRACTOR_ID | dynamic::PULUMI_EXTRACTOR_ID)
        );
        if is_observed_backing && !candidates.contains_key(&edge_key(&edge)) {
            graph
                .delete_edge(&edge.src, &edge.dst, &edge.label)
                .map_err(|error| error.to_string())?;
            facts.edges.remove(&edge_key(&edge));
        }
    }
    for (key, edge) in &candidates {
        graph.put_edge(edge).map_err(|error| error.to_string())?;
        facts.edges.insert(key.clone());
    }
    Ok(candidates.len() as u64)
}

/// Recompute the T0.5 exact-name fills (#257, AC-0270) over the whole graph
/// after every load. A stored fill the current graph no longer derives is
/// deleted before the current ones are upserted, so a second same-name
/// Symbol, a removed target or a scope change re-opens its Gap. Only edges
/// this rule wrote are ever deleted; the plan never targets a pair another
/// producer holds.
fn stitch_name_unique(
    graph: &mut SqliteGraphStore,
    facts: &mut OperationFacts,
) -> Result<semantic::name_unique::Plan, String> {
    let mut nodes = Vec::new();
    for label in ["Gap", "Symbol", "Component"] {
        nodes.extend(graph.nodes_with_label(label).map_err(|e| e.to_string())?);
    }
    let edges = graph
        .edges_with_labels(&["CALLS"])
        .map_err(|e| e.to_string())?;
    let plan = semantic::name_unique::plan(&nodes, &edges);
    let planned: std::collections::BTreeSet<_> = plan.edges.iter().map(edge_key).collect();
    for edge in &edges {
        let is_fill = edge.props["prov"]["extractor_id"] == semantic::name_unique::EXTRACTOR_ID;
        if is_fill && !planned.contains(&edge_key(edge)) {
            graph
                .delete_edge(&edge.src, &edge.dst, &edge.label)
                .map_err(|error| error.to_string())?;
            facts.edges.remove(&edge_key(edge));
        }
    }
    for edge in &plan.edges {
        graph.put_edge(edge).map_err(|error| error.to_string())?;
        facts.edges.insert(edge_key(edge));
    }
    Ok(plan)
}

/// Binding evidence a system manifest declares (#589, AC-0240): each
/// service base URL and each gateway route, resolved to the serving repo's
/// key through the manifest's own repo list.
fn manifest_http_bindings(
    manifest: &ingest::manifest::SystemManifest,
    repo_keys: &std::collections::BTreeMap<String, String>,
) -> Vec<events::HttpBinding> {
    let declared_in = ingest::manifest::MANIFEST_NAME.to_string();
    let mut bindings = Vec::new();
    for service in &manifest.services {
        let Some(repo) = repo_keys.get(&service.repo) else {
            continue;
        };
        for base_url in &service.base_urls {
            bindings.push(events::HttpBinding {
                service: service.name.clone(),
                repo: repo.clone(),
                base_url: base_url.clone(),
                kind: events::BindingKind::DeclaredService,
                declared_in: declared_in.clone(),
            });
        }
    }
    for route in &manifest.gateway_routes {
        let Some(repo) = manifest
            .services
            .iter()
            .find(|service| service.name == route.service)
            .and_then(|service| repo_keys.get(&service.repo))
        else {
            continue;
        };
        bindings.push(events::HttpBinding {
            service: route.service.clone(),
            repo: repo.clone(),
            base_url: route.base_url.clone(),
            kind: events::BindingKind::GatewayRoute {
                prefix: route.prefix.clone(),
                strip_prefix: route.strip_prefix,
            },
            declared_in: declared_in.clone(),
        });
    }
    bindings
}

/// After every repo is in, join client calls to endpoints in other repos
/// only on binding evidence (#589, AC-0240/AC-0241). A joined Gap and its
/// Gap edge are replaced by one cited `FETCHES` edge; every other evaluated
/// call keeps its Gap with a cross-repo reason and the binding state of
/// its origin (#1843). Runs once per system.
fn stitch_http_bindings(
    graph: &mut SqliteGraphStore,
    bindings: &[events::HttpBinding],
    facts: &mut OperationFacts,
) -> Result<u64, String> {
    if bindings.is_empty() {
        return Ok(0);
    }
    let gaps = graph.nodes_with_label("Gap").map_err(|e| e.to_string())?;
    let fetch_edges = graph
        .edges_with_labels(&["FETCHES"])
        .map_err(|e| e.to_string())?;
    let endpoints = graph
        .nodes_with_label("Endpoint")
        .map_err(|e| e.to_string())?;
    let prefix_kinds = events::prefix_kinds(&endpoints);
    let endpoint_ids: Vec<String> = endpoints.into_iter().map(|node| node.id).collect();
    let outcome =
        events::stitch_cross_repo_with(&gaps, &fetch_edges, &endpoint_ids, &prefix_kinds, bindings);
    for joined in &outcome.joined {
        for edge in fetch_edges.iter().filter(|edge| edge.dst == joined.gap_id) {
            facts.edges.remove(&edge_key(edge));
        }
        graph
            .delete_node(&joined.gap_id)
            .map_err(|e| e.to_string())?;
        facts.nodes.remove(&joined.gap_id);
    }
    // Several call sites of one symbol may bind to one endpoint: they are one
    // edge whose evidence unions every site, as on the load path (AC-0203).
    // An edge already stored under that key (a relative call from the same
    // symbol in the serving repo) is one more occurrence, never overwritten.
    let joined_keys: std::collections::BTreeSet<EdgeKey> = outcome
        .joined
        .iter()
        .map(|joined| edge_key(&joined.edge))
        .collect();
    // It came from the per-repo extractor, so it is re-stamped as an
    // occurrence of this pass (the merger unions one extractor's sites) and
    // the edge records which extractor first resolved it.
    let mut also_extracted_by = std::collections::BTreeMap::<EdgeKey, Vec<String>>::new();
    let stored: Vec<Edge> = fetch_edges
        .iter()
        .filter(|edge| joined_keys.contains(&edge_key(edge)))
        .map(|edge| {
            let mut edge = edge.clone();
            if let Some(extractor) = edge.props["prov"]["extractor_id"].as_str()
                && extractor != events::http_binding::EXTRACTOR_ID
            {
                also_extracted_by
                    .entry(edge_key(&edge))
                    .or_default()
                    .push(extractor.to_string());
                edge.props["prov"]["extractor_id"] = events::http_binding::EXTRACTOR_ID.into();
            }
            edge
        })
        .collect();
    let occurrences: Vec<Edge> = stored
        .into_iter()
        .chain(outcome.joined.iter().map(|joined| joined.edge.clone()))
        .collect();
    for (key, mut edge) in merge_edge_occurrences(occurrences) {
        if let Some(extractors) = also_extracted_by.remove(&key) {
            edge.props["also_extracted_by"] = extractors.into();
        }
        graph.put_edge(&edge).map_err(|e| e.to_string())?;
        facts.edges.insert(key);
    }
    for node in &outcome.kept_nodes {
        graph.put_node(node).map_err(|e| e.to_string())?;
    }
    for edge in &outcome.kept_edges {
        graph.put_edge(edge).map_err(|e| e.to_string())?;
    }
    // #1843: an observed HTTP client of a declared host cites the bindings
    // that name its origin, so the boundary projection does not list it as
    // leaving the system.
    let resources = graph
        .nodes_with_label("Resource")
        .map_err(|e| e.to_string())?;
    for node in events::bind_observed_clients(&resources, bindings) {
        graph.put_node(&node).map_err(|e| e.to_string())?;
    }
    Ok(outcome.joined.len() as u64)
}

/// Reconcile explicit found-ADR target ids against the complete graph.
/// Each repo is initially extracted in isolation, but decisions in a docs
/// repo may govern facts loaded later from another repo in the same system.
/// A rescan first drops links previously owned by that repo's found ADRs and
/// removes ADR nodes whose source file disappeared, so re-ingest cannot retain
/// declarations that are no longer present.
///
/// Each repo's inventory in `recorded` was built before the rescan, so its
/// found-ADR row counted only links into that repo. The row is restated with
/// the relinked edge count and republished with the patch, so a cross-repo
/// decision is never counted as linking nothing (#1847). Returns each
/// rescanned repo's found-ADR edge count.
///
/// Documentation is read only for `documented`, the repositories this
/// operation publishes: a document is a fact about one repository's own
/// files, so another repository's documents stay as its own publication
/// read them, matching the counts its stored inventory carries.
fn relink_found_adrs(
    state: &AppState,
    operation: &SourceOperation,
    execution: &JobExecution,
    facts: &mut OperationFacts,
    recorded: &mut std::collections::BTreeMap<String, RecordedCoverage>,
    documented: &Documented,
    api_doc_limits: ingest::api_docs::ApiDocLimits,
) -> Result<std::collections::BTreeMap<String, u64>, String> {
    ensure_running_job(state, execution)?;
    // The graph is read packed and each step decodes only the labels it
    // reads, so a large graph never exists as one props tree here (#2003).
    let snapshot = state
        .graph
        .lock()
        .map_err(|e| e.to_string())?
        .read_packed_snapshot()
        .map_err(|e| e.to_string())?;
    let nodes_where = |select: &dyn Fn(&str) -> bool| {
        snapshot
            .nodes
            .iter()
            .filter(|node| select(&node.label))
            .map(PackedNode::unpack)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    };
    let edges_where = |select: &dyn Fn(&str) -> bool| {
        snapshot
            .edges
            .iter()
            .filter(|edge| select(&edge.label))
            .map(PackedEdge::unpack)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    };
    let repos = nodes_where(&|label| label == "Repo")?;
    // Source guards were acquired before recovery. Root-dependent reads run
    // without a graph mutex and cannot recursively lock an exclusive owner.
    let candidates: std::collections::BTreeSet<&str> = snapshot
        .nodes
        .iter()
        .filter(|node| node.label != "ADR")
        .map(|node| node.id.as_str())
        .collect();
    // One walk per repository reads its found ADRs and, for the
    // repositories this operation publishes, its documentation files as
    // `Document` and `DocSection` facts (ADR-0065 decision 6, AC-1600). A
    // reference may name any fact of the system, so the index covers the
    // whole graph while documents are read only for the published
    // repositories (#2004).
    let detailed = nodes_where(&spec::ReferenceIndex::reads_props)?;
    let index = spec::ReferenceIndex::from_parts(
        snapshot
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.label.as_str())),
        &detailed,
    );
    let (updates, documents) = collect_documentation(
        &index,
        &repos,
        &|id| candidates.contains(id),
        documented,
        api_doc_limits,
        |repo| {
            ensure_running_job(state, execution)?;
            operation.root(repo)
        },
    )?;
    drop(index);
    drop(candidates);
    let (mut patch, linked) = found_adr_patch(
        &nodes_where(&|label| label == "ADR")?,
        &edges_where(&|label| label == "DECIDES")?,
        updates,
    );
    document_patch(
        &nodes_where(&|label| matches!(label, "Document" | "DocSection" | "Drift"))?,
        &edges_where(&|label| DOCUMENT_EDGE_LABELS.contains(&label))?,
        documents,
        &mut patch,
    );
    let mut restated = Vec::new();
    for (repo, entry) in recorded.iter_mut() {
        if let Some(&edges) = linked.get(repo)
            && restate_adr_edges(&mut entry.coverage, edges)
        {
            entry.record.inventory_json =
                serde_json::to_string(&entry.coverage).map_err(|e| e.to_string())?;
            restated.push(entry.record.clone());
        }
    }
    let mut graph = state.graph.lock().map_err(|e| e.to_string())?;
    ensure_running_job(state, execution)?;
    if !graph
        .apply_patch_with_coverage_if_snapshot_matches(&snapshot, &patch, &restated)
        .map_err(|e| e.to_string())?
    {
        return Err("graph context changed during ADR recovery; retry recovery".into());
    }
    facts.record_patch(&patch);
    Ok(linked)
}

/// A repo's coverage inventory and the record it was published as, kept so
/// a later system-wide pass can restate it (#1847).
struct RecordedCoverage {
    coverage: ingest::coverage::RepoCoverage,
    record: core_graph::coverage::CoverageRecord,
}

/// Set the found-ADR row's edge count to the relinked count. Returns whether
/// it changed.
fn restate_adr_edges(coverage: &mut ingest::coverage::RepoCoverage, edges: u64) -> bool {
    match coverage
        .producers
        .iter_mut()
        .find(|row| row.id == spec::FOUND_ADR_EXTRACTOR_ID)
    {
        Some(row) if row.edges != edges => {
            row.edges = edges;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
fn collect_found_adrs<'a>(
    nodes: &[Node],
    root_for: impl Fn(&str) -> Result<&'a std::path::Path, String>,
) -> Result<Vec<(String, spec::AdrFacts)>, String> {
    let candidates: std::collections::BTreeSet<&str> = nodes
        .iter()
        .filter(|node| node.label != "ADR")
        .map(|node| node.id.as_str())
        .collect();
    collect_found_adrs_among(nodes, |id| candidates.contains(id), root_for)
}

/// Found ADRs of every `Repo` node in `nodes`. A governed target is matched
/// by id alone, so `is_candidate` answers for the graph's non-ADR facts and
/// no fact is decoded for it (#2003).
#[cfg(test)]
fn collect_found_adrs_among<'a>(
    nodes: &[Node],
    is_candidate: impl Fn(&str) -> bool,
    root_for: impl Fn(&str) -> Result<&'a std::path::Path, String>,
) -> Result<Vec<(String, spec::AdrFacts)>, String> {
    let (adrs, _) = collect_documentation(
        &spec::ReferenceIndex::default(),
        nodes,
        &is_candidate,
        &Documented::new(),
        ingest::api_docs::ApiDocLimits::default(),
        root_for,
    )?;
    Ok(adrs)
}

/// The repositories whose documentation a pass reads, each with the API
/// references its system manifest declares (the repository's own
/// `.cartograph/api-docs.toml` is read with them).
type Documented = std::collections::BTreeMap<String, Vec<ingest::api_docs::ApiDocDeclaration>>;

/// Edge labels that start at a documentation fact and that a re-read of the
/// repository restates.
const DOCUMENT_EDGE_LABELS: [&str; 5] = [
    "HAS_SECTION",
    "MENTIONS",
    "DOCUMENTED_AS",
    "SERVED_AS",
    "RECORDS",
];

/// One documentation walk per `Repo` node in `repos` (ADR-0065 decision 6):
/// the found ADRs of every repository, and, for those in `documented`, the
/// documents with their references linked against `index` and their drift
/// (AC-0678, AC-0679, AC-1600).
#[allow(clippy::type_complexity)]
fn collect_documentation<'a>(
    index: &spec::ReferenceIndex<'_>,
    repos: &[Node],
    is_candidate: &dyn Fn(&str) -> bool,
    documented: &Documented,
    api_doc_limits: ingest::api_docs::ApiDocLimits,
    root_for: impl Fn(&str) -> Result<&'a std::path::Path, String>,
) -> Result<
    (
        Vec<(String, spec::AdrFacts)>,
        Vec<(String, spec::DocumentFacts)>,
    ),
    String,
> {
    let mut adrs = Vec::new();
    let mut documents = Vec::new();
    for node in repos.iter().filter(|node| node.label == "Repo") {
        let repo = node
            .id
            .strip_prefix("repo:")
            .ok_or("invalid repository fact identity")?;
        let commit = node.props["commit"].as_str().unwrap_or("workdir");
        let root = root_for(repo)?;
        let declared = documented.get(repo);
        let api_references = declared
            .map(|declared| {
                declared_api_docs(&ingest::api_docs::load_api_docs_within(
                    root,
                    declared,
                    api_doc_limits,
                ))
            })
            .unwrap_or_default();
        let read = spec::read_documentation(
            root,
            repo,
            commit,
            &spec::DocumentationCases {
                documents: declared.is_some(),
                api_references: &api_references,
                decisions: Some(is_candidate),
            },
        )
        .map_err(|e| e.to_string())?;
        adrs.push((repo.to_owned(), read.decisions.facts));
        if declared.is_none() {
            continue;
        }
        let mut facts = read.documents;
        let mentions = spec::link_references(repo, commit, &facts.references, index);
        facts.edges.extend(mentions);
        // Whether a repo-relative path is in the tree: an existence
        // check only, and a link is never followed out of it.
        let exists = |path: &str| tree_has_path(root, path);
        let (drift, compared) =
            spec::documentation_drift(repo, commit, &facts.references, index, &exists);
        facts.nodes.extend(drift);
        facts.edges.extend(compared);
        documents.push((repo.to_owned(), facts));
    }
    Ok((adrs, documents))
}

fn found_adr_patch(
    nodes: &[Node],
    edges: &[Edge],
    updates: Vec<(String, spec::AdrFacts)>,
) -> (
    core_graph::GraphPatch,
    std::collections::BTreeMap<String, u64>,
) {
    let mut patch = core_graph::GraphPatch::default();
    let mut linked = std::collections::BTreeMap::new();
    for (repo, facts) in updates {
        let adr_prefix = format!("adr:{repo}@");
        let existing_ids = nodes
            .iter()
            .filter(|node| {
                node.label == "ADR"
                    && node.id.starts_with(&adr_prefix)
                    && node.props["origin"].as_str() == Some("found")
            })
            .map(|node| node.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let current_ids = facts
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for edge in edges.iter().filter(|edge| {
            edge.label == "DECIDES"
                && (existing_ids.contains(&edge.src) || current_ids.contains(&edge.src))
        }) {
            patch.delete_edges.push(edge_key(edge));
        }
        patch
            .delete_node_ids
            .extend(existing_ids.difference(&current_ids).cloned());
        linked.insert(repo, distinct_edge_count(&facts.edges));
        patch.upsert_nodes.extend(facts.nodes);
        patch.upsert_edges.extend(facts.edges);
    }
    (patch, linked)
}

/// What the documentation reader found in one repository, for the coverage
/// inventory published with that repository's facts (AC-0677). It is the
/// read [`collect_documentation`] makes once the system is in, over the same
/// guarded tree and declared API references.
fn document_coverage(counts: spec::DocumentCounts) -> ingest::coverage::DocumentCoverage {
    ingest::coverage::DocumentCoverage {
        found: counts.found,
        read: counts.read,
        skipped: counts
            .skipped
            .into_iter()
            .map(|(reason, files)| (reason.to_owned(), files))
            .collect(),
        sections: counts.sections,
        sections_truncated: counts.sections_truncated,
    }
}

/// Read the documentation files of every repository in the graph, and link
/// each exact reference to the one fact of the system it names (AC-0678).
#[cfg(test)]
fn collect_documents<'a>(
    nodes: &[Node],
    root_for: impl Fn(&str) -> Result<&'a std::path::Path, String>,
) -> Result<Vec<(String, spec::DocumentFacts)>, String> {
    let documented = nodes
        .iter()
        .filter_map(|node| node.id.strip_prefix("repo:"))
        .map(|repo| (repo.to_owned(), Vec::new()))
        .collect();
    let (_, documents) = collect_documentation(
        &spec::ReferenceIndex::new(nodes),
        nodes,
        &|_| false,
        &documented,
        ingest::api_docs::ApiDocLimits::default(),
        root_for,
    )?;
    Ok(documents)
}

/// Whether `path` names an entry of the tree under `root`, read component
/// by component without following a symbolic link. A link on the way is
/// itself the entry the tree holds: what it points at, possibly outside the
/// repository, never decides the answer.
fn tree_has_path(root: &std::path::Path, path: &str) -> bool {
    let mut at = root.to_path_buf();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        if part == ".." {
            return false;
        }
        at.push(part);
        match at.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    true
}

/// Add each repository's documentation facts to `patch`, and remove the
/// documents, sections, documentation drift entries and their edges of
/// that repository that the read no longer yields, so a re-ingest keeps no
/// fact of a deleted file, section or reference and no drift entry of a
/// reference that matches again.
fn document_patch(
    nodes: &[Node],
    edges: &[Edge],
    updates: Vec<(String, spec::DocumentFacts)>,
    patch: &mut core_graph::GraphPatch,
) {
    for (repo, facts) in updates {
        let prefixes = [
            format!("doc:{repo}@"),
            format!("docsec:{repo}@"),
            format!("drift:documentation:{repo}@"),
        ];
        let owned = |id: &str| prefixes.iter().any(|prefix| id.starts_with(prefix));
        let current_ids = facts
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let current_edges = facts
            .edges
            .iter()
            .map(edge_key)
            .collect::<std::collections::BTreeSet<_>>();
        patch.delete_node_ids.extend(
            nodes
                .iter()
                .filter(|node| {
                    matches!(node.label.as_str(), "Document" | "DocSection" | "Drift")
                        && owned(&node.id)
                        && !current_ids.contains(node.id.as_str())
                })
                .map(|node| node.id.clone()),
        );
        patch.delete_edges.extend(
            edges
                .iter()
                .filter(|edge| {
                    DOCUMENT_EDGE_LABELS.contains(&edge.label.as_str()) && owned(&edge.src)
                })
                .map(edge_key)
                .filter(|key| !current_edges.contains(key)),
        );
        patch.upsert_nodes.extend(facts.nodes);
        patch.upsert_edges.extend(facts.edges);
    }
}

#[cfg(test)]
fn apply_found_adrs(
    graph: &mut SqliteGraphStore,
    updates: Vec<(String, spec::AdrFacts)>,
) -> Result<u64, String> {
    let snapshot = graph.read_snapshot().map_err(|e| e.to_string())?;
    let (patch, linked) = found_adr_patch(&snapshot.0, &snapshot.1, updates);
    if !graph
        .apply_patch_if_snapshot_matches(&snapshot, &patch)
        .map_err(|e| e.to_string())?
    {
        return Err("graph context changed during ADR recovery; retry recovery".into());
    }
    Ok(linked.values().sum())
}

/// The single source of truth for register counts (#116): every surface —
/// Workspace outcome, Gaps & Drift, Provenance & Eval — reads these numbers
/// from this one query so they always reconcile (handoff §Interactions #3).
#[derive(Debug, Serialize, PartialEq, Eq)]
struct FindingsSummary {
    /// Explicit System Gaps in the graph (spec-register definition).
    gaps: u64,
    /// Subset of `gaps` the interpretation seam fills (#994, AC-0359); stays
    /// counted in `gaps` and `open_findings`, and nothing is subtracted or
    /// suppressed.
    interpretation_backlog: u64,
    /// Unsupported patterns — tool limitations, never Gaps.
    unsupported: u64,
    /// Questions recovery found no evidence for.
    no_evidence: u64,
    /// ADR/code drift findings.
    drift: u64,
    /// gaps + unsupported + no_evidence (the register headline).
    open_findings: u64,
    /// Gaps a live T0.5 exact-name fill covers (#257, AC-0270). They stay
    /// open and counted in `gaps`: the fill is an inferred candidate, never
    /// a resolution.
    gaps_with_inferred_fill: u64,
    /// Total graph facts (nodes + edges).
    graph_facts: u64,
}

/// Count register entries from the graph using the spec register's own
/// predicates — one definition, every surface.
fn summarize_register(
    nodes: &[Node],
    edges: &[Edge],
    unsupported: u64,
    no_evidence: u64,
) -> FindingsSummary {
    // One finding per unresolved fact (#241): an unresolved call emits a Gap
    // node plus a supporting Gap CALLS edge — spec::count_gap_findings folds
    // the pair into one finding while keeping an edge-only gap (no gap node
    // on either end) as its own. Shared definition, every surface (#116).
    let gaps = spec::count_gap_findings(nodes.iter(), edges.iter()) as u64;
    let interpretation_backlog =
        spec::count_interpretation_backlog(nodes.iter(), edges.iter()) as u64;
    let drift = nodes
        .iter()
        .filter(|node| spec::is_drift_node(node))
        .count() as u64;
    FindingsSummary {
        gaps,
        interpretation_backlog,
        unsupported,
        no_evidence,
        drift,
        open_findings: gaps + unsupported + no_evidence,
        gaps_with_inferred_fill: semantic::name_unique::live_fills(nodes, edges)
            .into_iter()
            .map(|(gap, _)| gap)
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u64,
        graph_facts: (nodes.len() + edges.len()) as u64,
    }
}

/// Register tallies for every surface (#116): gap/drift counts from the
/// graph, unsupported/no-evidence from the findings store.
#[tauri::command]
fn findings_summary(state: State<'_, AppState>) -> Result<FindingsSummary, String> {
    let (nodes, edges) = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        graph.read_snapshot().map_err(|e| e.to_string())?
    };
    let (unsupported, no_evidence) = {
        let findings = state.findings.lock().map_err(|e| e.to_string())?;
        findings.counts().map_err(|e| e.to_string())?
    };
    Ok(summarize_register(&nodes, &edges, unsupported, no_evidence))
}

/// All persisted register findings (unsupported / no-evidence lanes).
#[tauri::command]
fn list_findings(state: State<'_, AppState>) -> Result<Vec<Finding>, String> {
    let findings = state.findings.lock().map_err(|e| e.to_string())?;
    findings.list().map_err(|e| e.to_string())
}

/// Local-only preflight over a directory (#116, AC-0055/AC-0059): detect
/// languages/frameworks/adapter coverage and classify constructs into the
/// three-way split — potential gaps vs unsupported patterns — before any
/// recovery runs. Zero egress; never invokes an LLM (T0 discipline).
/// Unsupported findings persist to the register so post-recovery surfaces
/// reconcile with what preflight predicted.
///
/// The scan parses every TS/JS file for eval-site claims, which takes tens of
/// minutes on a large repo, so it runs on a blocking worker like recovery
/// (AC-0078, AC-0197): the window stays interactive, `preflight://progress`
/// reports the file being read, and `cancel_preflight` stops it (#235).
#[tauri::command]
async fn preflight(
    path: String,
    run: u64,
    app: tauri::AppHandle,
) -> Result<PreflightResult, String> {
    start_preflight(path, run, app).await
}

/// The `preflight` command body, generic over the runtime so tests can prove
/// where it runs. The run is registered (superseding any earlier one) when
/// this is called — before the worker is queued — so a Cancel or a newer
/// preflight that arrives while the blocking pool is busy still reaches it,
/// and supersession follows invocation order (#434 review). `run` is the
/// UI's id for this scan, echoed on every progress ping so a superseded
/// scan's last ping can't overwrite the current one's display.
fn start_preflight<R: tauri::Runtime>(
    path: String,
    run: u64,
    app: tauri::AppHandle<R>,
) -> impl std::future::Future<Output = Result<PreflightResult, String>> {
    let token = app.state::<PreflightRuns>().begin();
    async move {
        let token = token?;
        off_ui_thread(move || {
            let state = app.state::<AppState>();
            let runs = app.state::<PreflightRuns>();
            let mut progress = preflight_progress_throttle(&app, run);
            preflight_blocking(&path, &app, &state, &runs, &token, &mut progress)
        })
        .await
    }
}

/// Stop the preflight in flight, if any (AC-0198). It stops before the next
/// file is parsed and persists no findings.
#[tauri::command]
fn cancel_preflight(runs: State<'_, PreflightRuns>) -> Result<(), String> {
    runs.cancel().map(drop)
}

/// The error a stopped preflight returns — the same word a cancelled job uses.
const PREFLIGHT_CANCELLED: &str = "cancelled";

/// The preflight in flight, if any (#235). Preflight is not a durable job (its
/// only output is the findings it persists on success), so cancellation is an
/// in-memory flag, not a job row. Starting a preflight cancels the previous
/// one, so an abandoned scan never keeps a core busy for its full duration.
///
/// Two locks: `current` is held only for a flag swap, so `begin` and the
/// synchronous `cancel_preflight` command never wait on the database
/// (#434 review); `writes` serializes register writes.
#[derive(Default)]
struct PreflightRuns {
    current: Mutex<RunsState>,
    writes: Mutex<()>,
}

#[derive(Default)]
struct RunsState {
    run: Option<Arc<RunState>>,
    /// Orders preflight starts against recovery reconciliations.
    epoch: u64,
    /// Each repo's latest recovery reconciliation (#439 review): a preflight
    /// begun before it holds only pending findings and must not overwrite the
    /// proven classification; it is answered with the reconciled report the
    /// register holds instead (#458).
    reconciled: std::collections::BTreeMap<String, Reconciled>,
    /// The epoch of each repo's latest persisted preflight (#497): a
    /// reconciliation whose fence is older must not overwrite it.
    persisted: std::collections::BTreeMap<String, u64>,
}

/// One preflight's lifecycle (#493 review). A run leaves `RUNNING` exactly
/// once, by compare-and-swap: to `CANCELLED` when the user cancels or a newer
/// preflight supersedes it, or to `COMMITTING` immediately before its register
/// write commits. Whichever lands first wins, so a commit never follows a
/// successful cancel, and a cancel that finds the run committing lost the
/// race — the run completes and reports its result. Neither side waits.
#[derive(Default)]
struct RunState(std::sync::atomic::AtomicU8);

impl RunState {
    const RUNNING: u8 = 0;
    const CANCELLED: u8 = 1;
    const COMMITTING: u8 = 2;

    fn leave_running(&self, to: u8) -> bool {
        self.0
            .compare_exchange(Self::RUNNING, to, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Cancel the run unless it already began committing; `false` means the
    /// cancel lost (or the run had already been cancelled).
    fn cancel(&self) -> bool {
        self.leave_running(Self::CANCELLED)
    }

    /// Claim the commit unless the run was cancelled first.
    fn claim_commit(&self) -> bool {
        self.leave_running(Self::COMMITTING)
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst) == Self::CANCELLED
    }
}

/// One registered preflight: cancelled when the user cancels or a newer
/// preflight supersedes it, unless its register write already began
/// committing.
struct PreflightToken {
    state: Arc<RunState>,
    epoch: u64,
}

/// A recovery's reconciled report and its epoch in the register order.
#[derive(Debug)]
struct Reconciled {
    epoch: u64,
    report: Arc<ingest::preflight::PreflightReport>,
}

/// What a preflight's register write amounted to.
#[derive(Debug)]
enum Persisted<T> {
    /// This run's findings are in the register.
    Written(T),
    /// A recovery reconciled the repo after this run began, so the register
    /// keeps that newer classification (#458).
    Reconciled {
        epoch: u64,
        report: Arc<ingest::preflight::PreflightReport>,
    },
}

/// A report's place in its repo's register order (#458). The register holds
/// the write with the highest epoch, so a surface that keeps, per repo, the
/// report with the highest epoch it has seen always matches the register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RegisterStamp {
    repo: String,
    epoch: u64,
}

/// The `preflight` command's result: the report the register now holds for
/// the scanned repo, stamped with its place in the register order.
#[derive(Debug, Serialize)]
struct PreflightResult {
    #[serde(flatten)]
    report: ingest::preflight::PreflightReport,
    register: RegisterStamp,
}

impl std::ops::Deref for PreflightResult {
    type Target = ingest::preflight::PreflightReport;

    fn deref(&self) -> &Self::Target {
        &self.report
    }
}

/// A recovery's place in the preflight order, reserved before the recovery
/// publishes `done` (#497): once the UI can see the job finished, any
/// preflight the user starts is newer than the reconciliation it precedes.
struct ReconcileFence {
    epoch: u64,
}

impl PreflightToken {
    fn is_cancelled(&self) -> bool {
        self.state.is_cancelled()
    }
}

impl PreflightRuns {
    fn current(&self) -> Result<std::sync::MutexGuard<'_, RunsState>, String> {
        self.current.lock().map_err(|e| e.to_string())
    }

    fn begin(&self) -> Result<PreflightToken, String> {
        let state = Arc::new(RunState::default());
        let mut current = self.current()?;
        current.epoch += 1;
        if let Some(previous) = current.run.replace(state.clone()) {
            previous.cancel();
        }
        Ok(PreflightToken {
            state,
            epoch: current.epoch,
        })
    }

    /// Cancel the run in flight, if any. Returns whether a running scan was
    /// stopped: `false` when there is none, or when its register write had
    /// already begun committing — that run completes (#493 review).
    fn cancel(&self) -> Result<bool, String> {
        Ok(self.current()?.run.as_ref().is_some_and(|run| run.cancel()))
    }

    /// Run `persist` only if `token` is still live and no recovery has
    /// reconciled `repo` since the token began, holding the write lock
    /// across the check and the write, so register writes never interleave.
    /// A superseding preflight's write therefore always lands after an older
    /// one it raced (#434 review). A cancel never waits here: one that lands
    /// before the check writes nothing and returns `cancelled`. `persist` is
    /// handed the commit claim to take inside its transaction immediately
    /// before committing (#493): a cancel that landed during the write wins
    /// the claim and rolls the write back, and one that arrives once the
    /// claim is taken loses — the scan completes and reports its result. A run
    /// that a recovery reconciled past writes nothing and is answered with
    /// the reconciled report the register keeps (#458).
    fn persist_if_current<T>(
        &self,
        token: &PreflightToken,
        repo: &str,
        persist: impl FnOnce(&dyn Fn() -> bool) -> Result<T, String>,
    ) -> Result<Persisted<T>, String> {
        let _writes = self.writes.lock().map_err(|e| e.to_string())?;
        if token.is_cancelled() {
            return Err(PREFLIGHT_CANCELLED.into());
        }
        if let Some(reconciled) = self
            .current()?
            .reconciled
            .get(repo)
            .filter(|reconciled| reconciled.epoch > token.epoch)
        {
            return Ok(Persisted::Reconciled {
                epoch: reconciled.epoch,
                report: reconciled.report.clone(),
            });
        }
        let written = persist(&|| token.state.claim_commit())?;
        let mut current = self.current()?;
        let latest = current.persisted.entry(repo.to_string()).or_default();
        *latest = (*latest).max(token.epoch);
        Ok(Persisted::Written(written))
    }

    /// Reserve a recovery's reconciliation epoch. Take it before the job is
    /// published as completed, and commit the write through `reconcile` after
    /// (#497): the cancel race still settles first, while a preflight started
    /// once the job shows `done` is always newer than the reconciliation.
    fn reserve_reconcile(&self) -> Result<ReconcileFence, String> {
        let mut current = self.current()?;
        current.epoch += 1;
        Ok(ReconcileFence {
            epoch: current.epoch,
        })
    }

    /// Recovery's reconciliation write (AC-0200): serialized with preflight
    /// writes and recorded at `fence`'s epoch, so a preflight begun before
    /// the fence can never replace the proven classification with its pending
    /// one (#439 review). A preflight begun after the fence is newer and wins
    /// either way (#497): if it already persisted, the reconciliation writes
    /// nothing (`None`); if it persists later, it is not refused. A recovery
    /// whose fence is older than another recovery's that already reconciled
    /// the repo writes nothing either. A written reconciliation returns its
    /// register stamp.
    fn reconcile(
        &self,
        fence: &ReconcileFence,
        repo: &str,
        report: &ingest::preflight::PreflightReport,
        persist: impl FnOnce() -> Result<(), String>,
    ) -> Result<Option<RegisterStamp>, String> {
        let _writes = self.writes.lock().map_err(|e| e.to_string())?;
        // A newer write of either kind already holds the register (#514
        // review): an overlapping recovery of the same repo whose fence is
        // newer reconciled first, or a newer preflight persisted.
        let newer_written = {
            let current = self.current()?;
            current
                .persisted
                .get(repo)
                .is_some_and(|epoch| *epoch > fence.epoch)
                || current
                    .reconciled
                    .get(repo)
                    .is_some_and(|reconciled| reconciled.epoch > fence.epoch)
        };
        if newer_written {
            return Ok(None);
        }
        persist()?;
        self.current()?.reconciled.insert(
            repo.to_string(),
            Reconciled {
                epoch: fence.epoch,
                report: Arc::new(report.clone()),
            },
        );
        Ok(Some(RegisterStamp {
            repo: repo.to_string(),
            epoch: fence.epoch,
        }))
    }
}

/// A preflight nothing can cancel — for tests that only need its result.
#[cfg(test)]
fn preflight_uncancelled<R: tauri::Runtime>(
    path: &str,
    app: &tauri::AppHandle<R>,
    state: &AppState,
) -> Result<PreflightResult, String> {
    let runs = PreflightRuns::default();
    let token = runs.begin()?;
    preflight_blocking(path, app, state, &runs, &token, &mut |_| {})
}

/// One live `preflight://progress` ping: the file about to be parsed and how
/// far through the walk the scan is — or, while the tree is still being
/// listed (`total` is `null`), the directory being entered and how many files
/// have been found so far (#453).
#[derive(Clone, Serialize)]
struct PreflightProgress<'a> {
    /// The UI-issued id of the scan this ping belongs to.
    run: u64,
    path: &'a str,
    done: usize,
    total: Option<usize>,
}

/// Emits at most one `preflight://progress` event every ~120ms (the same
/// budget as `job://detail`, #209) so thousands of files can't flood the IPC
/// bridge. The first step always fires, and so does the first file step
/// after listing, so the line never sits on a finished listing (#453).
fn preflight_progress_throttle<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    run: u64,
) -> impl FnMut(ingest::preflight::ScanStep<'_>) + '_ {
    let mut last: Option<std::time::Instant> = None;
    let mut listing = true;
    move |step| {
        let now = std::time::Instant::now();
        let listing_ended = listing && step.total.is_some();
        listing = step.total.is_none();
        if listing_ended
            || last.is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_millis(120))
        {
            let _ = app.emit(
                "preflight://progress",
                PreflightProgress {
                    run,
                    path: step.path,
                    done: step.done,
                    total: step.total,
                },
            );
            last = Some(now);
        }
    }
}

/// The preflight pipeline behind the command, generic over the runtime so
/// the request → gate → accept → re-scan lane is testable end to end. The
/// scanned directory registers as a resolved ingest root (it *is* one — a
/// canonicalized directory), so its `.cartograph/adapters/` is discoverable
/// before the first ingest; gated+enabled plugins then count as coverage
/// and their uncovered-language findings close on the re-scan (#201).
fn preflight_blocking<R: tauri::Runtime>(
    path: &str,
    app: &tauri::AppHandle<R>,
    state: &AppState,
    runs: &PreflightRuns,
    token: &PreflightToken,
    on_progress: &mut dyn FnMut(ingest::preflight::ScanStep<'_>),
) -> Result<PreflightResult, String> {
    let stopped = || token.is_cancelled();
    let source = register_local_source(state, std::path::Path::new(path))?;
    let operation = SourceOperation::acquire(&state.sources, [(source.clone(), false)])?;
    let root = operation.root(&source.repo_key)?;
    let repo = source.repo_key.clone();
    let plugins = plugin_coverage(&active_plugins_for_root(app, state, root)?);
    // No TS parse here (#243): the eval-site AST proof is a full extraction,
    // and recovery performs exactly that parse moments later. Inline-eval
    // lines are reported pending that proof, and recovery reconciles them
    // (`reconciled_preflight_report`) — never claimed closed before then.
    let mut report = ingest::preflight::preflight_scan(
        root,
        &plugins,
        ingest::preflight::EvalProofSource::PendingRecovery,
        &mut |step| {
            if stopped() {
                return std::ops::ControlFlow::Break(());
            }
            on_progress(step);
            std::ops::ControlFlow::Continue(())
        },
    )
    .map_err(|e| e.to_string())?
    .ok_or(PREFLIGHT_CANCELLED)?;
    // The lookup also names installed plugins whose gate has not passed, so
    // a cell they declare is never reported as having no source.
    report.refresh_adapter_lookup(&plugin_declarations_for_root(app, state, root)?);
    // A stopped scan must not replace the register with a result the user
    // abandoned.
    let persisted = runs.persist_if_current(token, &repo, |live| {
        persist_preflight_findings_if(state, &repo, &report, live)
    })?;
    Ok(match persisted {
        Persisted::Written(()) => PreflightResult {
            report,
            register: RegisterStamp {
                repo,
                epoch: token.epoch,
            },
        },
        Persisted::Reconciled { epoch, report } => PreflightResult {
            report: (*report).clone(),
            register: RegisterStamp { repo, epoch },
        },
    })
}

fn plugin_coverage(plugins: &[ActivePlugin]) -> Vec<ingest::preflight::PluginCoverage> {
    plugins
        .iter()
        .map(|plugin| ingest::preflight::PluginCoverage {
            plugin_id: plugin.plugin_id.clone(),
            version: plugin.content_hash.clone(),
            extensions: plugin.extensions.clone(),
            cells: plugin.cells.clone(),
            languages: plugin.languages.clone(),
        })
        .collect()
}

/// Scan `root` and refresh its adapter lookup the same way [`preflight_blocking`]
/// does, without persisting findings or touching the register (#1101).
pub(crate) fn preflight_adapter_lookup<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    root: &std::path::Path,
) -> Result<Vec<ingest::adapter_lookup::CellLookup>, String> {
    let plugins = plugin_coverage(&active_plugins_for_root(app, state, root)?);
    let mut report = ingest::preflight::preflight_scan(
        root,
        &plugins,
        ingest::preflight::EvalProofSource::PendingRecovery,
        &mut |_| std::ops::ControlFlow::Continue(()),
    )
    .map_err(|e| e.to_string())?
    .ok_or_else(|| PREFLIGHT_CANCELLED.to_string())?;
    report.refresh_adapter_lookup(&plugin_declarations_for_root(app, state, root)?);
    Ok(report.adapter_lookup)
}

/// The register holds one preflight result per repo: each scan replaces the
/// previous one, so every surface quotes the latest classification.
fn persist_preflight_findings(
    state: &AppState,
    repo: &str,
    report: &ingest::preflight::PreflightReport,
) -> Result<(), String> {
    persist_preflight_findings_if(state, repo, report, &|| true)
}

/// `persist_preflight_findings`, rolled back as `cancelled` unless
/// `claim_commit` succeeds immediately before the commit, so a preflight
/// cancelled mid-write leaves the register as it was (AC-0215).
fn persist_preflight_findings_if(
    state: &AppState,
    repo: &str,
    report: &ingest::preflight::PreflightReport,
    claim_commit: &dyn Fn() -> bool,
) -> Result<(), String> {
    let batch: Vec<NewFinding<'_>> = report
        .unsupported
        .iter()
        .map(|finding| NewFinding {
            kind: "unsupported",
            detector: &finding.detector,
            path: &finding.path,
            line: finding.line as i64,
            message: &finding.message,
        })
        .collect();
    let mut findings = state.findings.lock().map_err(|e| e.to_string())?;
    findings
        .replace_for_if(repo, ingest::preflight::DETECTOR_ID, &batch, claim_commit)
        .map_err(|e| e.to_string())?
        .map(drop)
        .ok_or_else(|| PREFLIGHT_CANCELLED.to_string())
}

/// Recovery's half of the inline-eval classification (AC-0099, AC-0200):
/// the extraction that just ran is the TS adapter's AST proof, so its eval
/// sites become the claims and the cheap textual scan re-runs with them.
/// Proven literals close, const-shaped-but-unproven sites downgrade to
/// potential Gaps, and the rest stay Unsupported.
///
/// The scan reads each captured file from `capture`, the exact bytes the
/// claims were proven on, so an edit made after capture cannot pair a claim
/// with different code (#439 review). It writes nothing: every recovery
/// replaces preflight's pending register findings with this report only once
/// its job has settled as completed, so a cancel that wins the race writes
/// nothing, and through `PreflightRuns::reconcile`, so a preflight already in
/// flight cannot overwrite it afterwards (AC-0200, AC-0209).
fn reconciled_preflight_report(
    root: &std::path::Path,
    capture: Option<&source_capture::Capture>,
    plugins: &[ingest::preflight::PluginCoverage],
    eval_sites: &[adapters_lang_ts::EvalSite],
) -> Result<ingest::preflight::PreflightReport, String> {
    let claims: Vec<ingest::preflight::EvalSiteCoverage> = eval_sites
        .iter()
        .map(|site| ingest::preflight::EvalSiteCoverage {
            path: site.path.clone(),
            line: site.line,
            proof: match site.proof {
                adapters_lang_ts::EvalProof::Covered => ingest::preflight::EvalProof::Covered,
                adapters_lang_ts::EvalProof::ConstUnproven => {
                    ingest::preflight::EvalProof::ConstUnproven
                }
                adapters_lang_ts::EvalProof::Dynamic => ingest::preflight::EvalProof::Dynamic,
            },
        })
        .collect();
    let members: Vec<&str> = capture
        .map(|capture| {
            capture
                .manifest()
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect()
        })
        .unwrap_or_default();
    let captured = |rel: &str| {
        capture
            .and_then(|capture| capture.file(rel).ok())
            .map(|file| file.bytes())
    };
    let eval = match capture {
        Some(_) => ingest::preflight::EvalProofSource::Captured {
            paths: &members,
            sites: &claims,
            bytes: &captured,
        },
        // Uncaptured extraction read the live tree itself.
        None => ingest::preflight::EvalProofSource::Claims(&claims),
    };
    let report = ingest::preflight::preflight_scan(root, plugins, eval, &mut |_| {
        std::ops::ControlFlow::Continue(())
    })
    .map_err(|e| e.to_string())?
    .ok_or("the reconciliation scan never stops early")?;
    Ok(report)
}

/// All configurable tier settings (T0 is always on and absent by design).
#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Result<Vec<settings::TierSettings>, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// Enable/disable a configurable tier (#118).
#[tauri::command]
fn set_tier_enabled(
    tier: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<Vec<settings::TierSettings>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .set_enabled(&tier, enabled)
        .map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// Choose local or cloud for an LLM tier; leaving cloud revokes consent.
#[tauri::command]
fn set_tier_provider(
    tier: String,
    provider: String,
    state: State<'_, AppState>,
) -> Result<Vec<settings::TierSettings>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .set_provider(&tier, &provider)
        .map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// Record standing cloud consent for a tier, storing the disclosure the
/// user saw. Only permits cloud — every call still needs the firewall's
/// per-payload grant (fail closed).
#[tauri::command]
fn grant_cloud_consent(
    tier: String,
    disclosure: String,
    state: State<'_, AppState>,
) -> Result<Vec<settings::TierSettings>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .grant_consent(&tier, &disclosure)
        .map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// Revoke a tier's standing cloud consent — immediate.
#[tauri::command]
fn revoke_cloud_consent(
    tier: String,
    state: State<'_, AppState>,
) -> Result<Vec<settings::TierSettings>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store.revoke_consent(&tier).map_err(|e| e.to_string())?;
    store.all().map_err(|e| e.to_string())
}

/// The "Ingest parallelism" setting and what it resolves to here (#236).
#[tauri::command]
fn get_ingest_parallelism(
    state: State<'_, AppState>,
) -> Result<settings::IngestParallelism, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.ingest_parallelism().map_err(|e| e.to_string())
}

/// Persist and apply "Ingest parallelism" (`0` = Auto). Takes effect for
/// the next extraction; recovered facts never depend on it (#236).
#[tauri::command]
fn set_ingest_parallelism(
    setting: u32,
    state: State<'_, AppState>,
) -> Result<settings::IngestParallelism, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    let applied = store
        .set_ingest_parallelism(setting)
        .map_err(|e| e.to_string())?;
    source_walk::parallel::set_parallelism(settings::IngestParallelism::parallelism(
        applied.setting,
    ));
    Ok(applied)
}

/// The user's last explicit Investigations budget and specialist (#1636).
#[tauri::command]
fn get_investigation_choice(
    state: State<'_, AppState>,
) -> Result<Option<settings::InvestigationChoice>, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.investigation_choice().map_err(|e| e.to_string())
}

/// Persist an explicit Investigations budget and specialist choice (#1636).
#[tauri::command]
fn set_investigation_choice(
    specialist_id: String,
    limit_profile: String,
    state: State<'_, AppState>,
) -> Result<Option<settings::InvestigationChoice>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .set_investigation_choice(&specialist_id, &limit_profile)
        .map_err(|e| e.to_string())
}

/// Investigation time limits for Configure → Run Limits (#1633).
#[tauri::command]
fn get_run_limits(state: State<'_, AppState>) -> Result<settings::RunLimits, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.run_limits().map_err(|e| e.to_string())
}

/// Persist both time limits; each must lie in [shipped default, maximum].
/// Applies to investigations started afterwards (#1633).
#[tauri::command]
fn set_run_limits(
    active_seconds: u64,
    request_timeout_ms: u64,
    state: State<'_, AppState>,
) -> Result<settings::RunLimits, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .set_run_limits(active_seconds, request_timeout_ms)
        .map_err(|e| e.to_string())
}

/// Return both time limits to the shipped defaults (#1633).
#[tauri::command]
fn reset_run_limits(state: State<'_, AppState>) -> Result<settings::RunLimits, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store.reset_run_limits().map_err(|e| e.to_string())
}

/// Installed Ollama models on this Mac, for Configure → Run Limits (#1643),
/// from the Ollama on the saved loopback port (#1653).
fn installed_local_models(port: u16) -> Result<Vec<llm::catalog::LocalModel>, String> {
    llm::OllamaProvider::with_models(
        &llm::OllamaProvider::loopback_url(port),
        llm::OllamaProvider::DEFAULT_EMBEDDING_MODEL,
        llm::OllamaProvider::DEFAULT_COMPLETION_MODEL,
        llm::bounded::CONNECT_TIMEOUT,
    )
    .and_then(|provider| provider.list_local_models())
    .map_err(|_| {
        "Ollama is not reachable on this Mac, so installed models cannot be listed.".to_owned()
    })
}

/// The saved loopback Ollama port, or the default when none is saved (#1653).
fn saved_ollama_port(state: &AppState) -> Result<u16, String> {
    settings::saved_ollama_port(&state.settings)
}

#[tauri::command]
async fn list_local_models(
    state: State<'_, AppState>,
) -> Result<Vec<llm::catalog::LocalModel>, String> {
    let port = saved_ollama_port(&state)?;
    tauri::async_runtime::spawn_blocking(move || installed_local_models(port))
        .await
        .map_err(|e| e.to_string())?
}

/// The loopback port Ollama listens on for new desktop investigations, or
/// `None` for the default 11434 (#1653).
#[tauri::command]
fn get_local_ollama_port(state: State<'_, AppState>) -> Result<Option<u16>, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.local_ollama_port().map_err(|e| e.to_string())
}

/// Save the loopback Ollama port, or `None` for the default (#1653). Only the
/// port is configurable; the host is always 127.0.0.1.
#[tauri::command]
fn set_local_ollama_port(
    port: Option<u16>,
    state: State<'_, AppState>,
) -> Result<Option<u16>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store.set_local_ollama_port(port).map_err(|e| e.to_string())
}

/// This project's own read-deny rules, added to the defaults that cloud
/// investigations never receive (ADR-0083, #679).
#[tauri::command]
fn get_read_deny_rules(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .read_deny_rules()
        .map(|rules| rules.project_rules().to_vec())
        .map_err(|e| e.to_string())
}

/// Replace this project's read-deny rules; all are validated before any is
/// saved (ADR-0083, #679). The defaults cannot be removed.
#[tauri::command]
fn set_read_deny_rules(
    rules: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .set_read_deny_rules(rules)
        .map(|rules| rules.project_rules().to_vec())
        .map_err(|e| e.to_string())
}

/// Every configurable operator limit with its range and value (#1653).
#[tauri::command]
fn get_operator_limits(
    state: State<'_, AppState>,
) -> Result<Vec<operator_limits::OperatorLimit>, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.operator_limits().map_err(|e| e.to_string())
}

/// Save one operator limit, or clear it (`value` null) so its default
/// applies; a key outside the registry or a value outside its range changes
/// nothing (#1653).
#[tauri::command]
fn set_operator_limit(
    key: String,
    value: Option<u64>,
    state: State<'_, AppState>,
) -> Result<operator_limits::OperatorLimit, String> {
    let saved = {
        let mut store = state.settings.lock().map_err(|e| e.to_string())?;
        store
            .set_operator_limit(&key, value)
            .map_err(|e| e.to_string())?
    };
    if [
        operator_limits::CAPTURE_STORE_MAX_MIB,
        operator_limits::CAPTURE_STORE_MAX_CAPTURES,
    ]
    .contains(&saved.key)
    {
        let limits = {
            let store = state.settings.lock().map_err(|e| e.to_string())?;
            capture_store_limits(&store)?
        };
        state.primary_sources.set_store_limits(limits)?;
    }
    Ok(saved)
}

/// The project's capture store quota (#1653, AC-1801), within the store's
/// hard maxima. A stored value that no longer validates is an error.
fn capture_store_limits(
    settings: &settings::SettingsStore,
) -> Result<source_capture::StoreLimits, String> {
    let limit = |key| {
        settings
            .operator_limit(key)
            .map_err(|error| error.to_string())
    };
    Ok(source_capture::StoreLimits {
        max_bytes: limit(operator_limits::CAPTURE_STORE_MAX_MIB)?
            .checked_mul(1024 * 1024)
            .ok_or("Capture store MiB overflows.")?,
        max_captures: limit(operator_limits::CAPTURE_STORE_MAX_CAPTURES)?,
    })
}

/// The local completion model for new desktop investigations (#1643).
#[tauri::command]
fn get_local_model(state: State<'_, AppState>) -> Result<settings::LocalModelChoice, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.local_model().map_err(|e| e.to_string())
}

/// Save the local completion model. Anything but the shipped default must be
/// installed in Ollama now; Cartograph never pulls a model (#1643).
#[tauri::command]
async fn set_local_model(
    model: String,
    state: State<'_, AppState>,
) -> Result<settings::LocalModelChoice, String> {
    if !settings::local_model_name(&model) {
        return Err(settings::SettingsError::InvalidLocalModel.to_string());
    }
    if model != llm::OllamaProvider::DEFAULT_COMPLETION_MODEL {
        let port = saved_ollama_port(&state)?;
        let installed = tauri::async_runtime::spawn_blocking(move || installed_local_models(port))
            .await
            .map_err(|e| e.to_string())??;
        if !installed.iter().any(|entry| entry.name == model) {
            return Err(format!(
                "'{model}' is not installed in Ollama on this Mac. Install it with `ollama pull {model}`, then choose it here."
            ));
        }
    }
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store.set_local_model(&model).map_err(|e| e.to_string())
}

/// Return to the shipped local completion model (#1643).
#[tauri::command]
fn reset_local_model(state: State<'_, AppState>) -> Result<settings::LocalModelChoice, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store.reset_local_model().map_err(|e| e.to_string())
}

/// The Ollama context window for new desktop investigations, or `None` for
/// Ollama's own default (#1653).
#[tauri::command]
fn get_local_context_window(state: State<'_, AppState>) -> Result<Option<u32>, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.local_context_window().map_err(|e| e.to_string())
}

/// Save one of the closed context windows, or `None` for Ollama's own (#1653).
#[tauri::command]
fn set_local_context_window(
    tokens: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Option<u32>, String> {
    let mut store = state.settings.lock().map_err(|e| e.to_string())?;
    store
        .set_local_context_window(tokens)
        .map_err(|e| e.to_string())
}

/// Which provider keys are configured and where each comes from (#1763).
/// Never returns a key.
#[tauri::command]
async fn provider_key_status() -> Result<Vec<provider_credentials::KeyStatus>, String> {
    off_ui_thread(|| Ok(provider_credentials::system().statuses())).await
}

/// Save or replace a provider key in the OS credential store (#1763). The
/// key is not written anywhere else, and saving it grants no egress.
#[tauri::command]
async fn set_provider_key(
    provider: String,
    key: String,
    app: tauri::AppHandle,
) -> Result<provider_credentials::KeyStatus, String> {
    off_ui_thread(move || {
        let provider = provider_credentials::ProviderId::parse(&provider)?;
        let status = provider_credentials::system().save(provider, &key)?;
        app.state::<AppState>()
            .investigations
            .forget_cloud_provider();
        Ok(status)
    })
    .await
}

/// Remove a provider key from the OS credential store (#1763).
#[tauri::command]
async fn remove_provider_key(
    provider: String,
    app: tauri::AppHandle,
) -> Result<provider_credentials::KeyStatus, String> {
    off_ui_thread(move || {
        let provider = provider_credentials::ProviderId::parse(&provider)?;
        let status = provider_credentials::system().remove(provider)?;
        app.state::<AppState>()
            .investigations
            .forget_cloud_provider();
        Ok(status)
    })
    .await
}

/// Ask a provider whether it accepts the configured key (#1795). One bodiless
/// metadata request: no source text, prompt or graph content is sent, so no
/// tier consent is involved. The request is written to the egress record
/// before it is sent. The result never carries the key or a response body.
#[tauri::command]
async fn check_provider_connection(
    provider: String,
    app: tauri::AppHandle,
) -> Result<provider_checks::ConnectionCheck, String> {
    off_ui_thread(move || {
        let provider = provider_credentials::ProviderId::parse(&provider)?;
        let state = app.state::<AppState>();
        provider_checks::check_connection_with(
            provider_credentials::system(),
            provider,
            provider_checks::check_url(provider),
            provider_checks::CHECK_TIMEOUT,
            |label, bytes| {
                state
                    .settings
                    .lock()
                    .is_ok_and(|mut settings| settings.record_egress(label, bytes).is_ok())
            },
        )
    })
    .await
}

/// Whether the local model server answers and which catalog models are
/// installed, with the command for each missing one (#1795). Loopback only.
#[tauri::command]
async fn local_lane_status(
    state: State<'_, AppState>,
) -> Result<provider_checks::LocalLaneStatus, String> {
    let port = saved_ollama_port(&state)?;
    off_ui_thread(move || Ok(provider_checks::probe_local_lane(port))).await
}

/// Live egress summary for the status bar (#103's seam, now real).
#[tauri::command]
fn egress_summary(state: State<'_, AppState>) -> Result<settings::EgressSummary, String> {
    let store = state.settings.lock().map_err(|e| e.to_string())?;
    store.egress_summary().map_err(|e| e.to_string())
}

/// The full consent disclosure for a tier's cloud lane (#112): everything
/// the Settings consent panel must show *before* consent is recordable.
/// T2 semantic triage runs the Haiku lane; T3 agentic reasoning runs Opus
/// (Fable stays a per-escalation opt-in, #120). T0/T1 have no cloud lane.
#[tauri::command]
fn cloud_disclosure(tier: String) -> Result<llm::anthropic::CloudDisclosure, String> {
    let lane = match tier.as_str() {
        "T2" => llm::anthropic::ClaudeLane::Haiku,
        "T3" => llm::anthropic::ClaudeLane::Opus,
        other => return Err(format!("tier '{other}' has no cloud lane")),
    };
    Ok(llm::anthropic::disclosure(lane))
}

/// Compute and persist one ingest's recovery metrics (#119): tier tallies,
/// register counts, extractor coverage, and the whole-graph content hash
/// that makes AC-0039's determinism observable as history data. Reads the
/// same whole-graph projection every other surface reads.
fn record_ingest_metrics(
    state: &AppState,
    job_id: i64,
    record_repo: &str,
    commit_sha: &str,
    layers: &LayerBreakdown,
    coverage_repos: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    // Packed: the metrics decode one fact at a time (AC-1201).
    let snapshot = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        graph.read_packed_snapshot().map_err(|e| e.to_string())?
    };
    // Only layers this ingest actually contained are in scope — a
    // zero-file layer's extractor did not run, so it reports null
    // coverage (not applicable), never a misleading 0%.
    let scope: std::collections::BTreeMap<String, u64> = [
        ("t0.adapter-ts", layers.ts.files),
        ("t0.adapter-python", layers.python.files),
        ("t0.adapter-go", layers.go.files),
        ("t0.adapter-rust", layers.rust.files),
        ("t0.adapter-java", layers.java.files),
        ("t0.adapter-kotlin", layers.kotlin.files),
        ("t0.iac-terraform", layers.tf.files),
        ("t0.webextension", layers.webext.files),
    ]
    .into_iter()
    .filter(|(_, files)| *files > 0)
    .map(|(extractor, files)| (extractor.to_string(), files))
    .collect();
    let computed =
        metrics::compute_packed(&snapshot, &scope, coverage_repos).map_err(|e| e.to_string())?;
    drop(snapshot);
    let (unsupported, no_evidence) = {
        let findings = state.findings.lock().map_err(|e| e.to_string())?;
        findings.counts().map_err(|e| e.to_string())?
    };
    let mut store = state.metrics.lock().map_err(|e| e.to_string())?;
    store
        .record(
            job_id,
            record_repo,
            commit_sha,
            &computed,
            unsupported,
            no_evidence,
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Ingest records, newest first (#119): tier tallies, register counts, and
/// the graph content hash per run — evidence health over re-ingests.
#[tauri::command]
fn ingest_history(state: State<'_, AppState>) -> Result<Vec<metrics::IngestRecord>, String> {
    let store = state.metrics.lock().map_err(|e| e.to_string())?;
    store.history(50).map_err(|e| e.to_string())
}

/// Per-extractor coverage for the most recent ingest (#119).
#[tauri::command]
fn extractor_coverage(
    state: State<'_, AppState>,
) -> Result<Vec<metrics::ExtractorCoverage>, String> {
    let store = state.metrics.lock().map_err(|e| e.to_string())?;
    store.latest_coverage().map_err(|e| e.to_string())
}

/// Strategy cards for one gap (#120): attempted tiers, stop reason, required
/// evidence, and the local/cloud options with exact egress estimates from
/// the firewall preview. Derivation only — nothing runs, nothing egresses.
#[tauri::command]
async fn gap_strategies(
    gap_id: String,
    app: tauri::AppHandle,
) -> Result<escalation::GapStrategyReport, String> {
    off_ui_thread(move || {
        let state = app.state::<AppState>();
        let preview = task_evidence::preview(&state, &gap_id)?;
        let (agent_task, source_basis, prepared) = match &preview {
            task_evidence::StrategyPreview::Escalatable(task) => {
                (task.task(), task.source_basis(), Some(task))
            }
            task_evidence::StrategyPreview::NotEscalatable { task, basis } => (task, basis, None),
        };
        // Strategy text is derived from the same graph basis as the prepared
        // task; a concurrent recovery requires a new preview.
        let (nodes, edges) = state
            .graph
            .lock()
            .map_err(|e| e.to_string())?
            .read_snapshot()
            .map_err(|e| e.to_string())?;
        let snapshot =
            context_hub::ContextSnapshot::new(nodes.clone(), edges).map_err(|e| e.to_string())?;
        if snapshot.id() != source_basis.graph_snapshot_id {
            return Err("Task evidence changed; refresh the strategy preview.".into());
        }
        let gap = nodes
            .iter()
            .find(|node| node.id == gap_id)
            .ok_or("Selected gap is unavailable; refresh recovery.")?;
        let cloud_allowed = state
            .settings
            .lock()
            .map_err(|e| e.to_string())?
            .egress_policy()
            .map_err(|e| e.to_string())?
            .cloud_allowed(llm::AnalysisTier::Agentic);
        let broker = agents::AgentBroker::bounded_default();
        // Check broker capability before ever previewing a payload (#238):
        // a gap whose edge label the broker cannot propose (e.g. IMPORTS)
        // would otherwise only be discovered by `preview_prepared` failing
        // validation, surfacing its internal error text to the UI.
        let edge_label_allowed = broker
            .allowed_edge_labels()
            .contains(&agent_task.edge_label);
        let payload_bytes = if let Some(task) = prepared.filter(|_| edge_label_allowed) {
            let firewall = llm::EgressFirewall::new(llm::EgressPolicy::default());
            let local = llm::OllamaProvider::local_on_port(saved_ollama_port(&state)?)
                .map_err(|e| e.to_string())?;
            let preview = broker
                .preview_prepared(&local, &firewall, task)
                .map_err(|e| e.to_string())?;
            serde_json::to_vec(&preview.payload)
                .map_err(|e| e.to_string())?
                .len() as u64
        } else {
            0
        };
        let mut report = escalation::strategies(
            agent_task,
            gap,
            cloud_allowed,
            payload_bytes,
            edge_label_allowed,
        );
        report.source_basis = Some(source_basis.clone());
        Ok(report)
    })
    .await
}

/// Edge labels the bounded T3 broker may currently propose (#238): lets the
/// gap register compute its escalation offer from real broker capability
/// instead of showing it unconditionally for gap classes the broker cannot
/// execute.
#[tauri::command]
fn escalation_capabilities() -> Vec<String> {
    agents::AgentBroker::bounded_default()
        .allowed_edge_labels()
        .iter()
        .cloned()
        .collect()
}

/// Exact redacted disclosure, including the receipt-bound input identity.
#[tauri::command]
async fn escalation_preview(
    gap_id: String,
    app: tauri::AppHandle,
) -> Result<llm::EgressPreview, String> {
    off_ui_thread(move || {
        let state = app.state::<AppState>();
        let task = task_evidence::prepare(&state, &gap_id, &format!("escalate:{gap_id}"))?;
        let policy = state
            .settings
            .lock()
            .map_err(|e| e.to_string())?
            .egress_policy()
            .map_err(|e| e.to_string())?;
        let provider = egress::escalation_provider("cloud", llm::OllamaProvider::DEFAULT_PORT)?;
        agents::AgentBroker::bounded_default()
            .preview_prepared(provider.as_ref(), &llm::EgressFirewall::new(policy), &task)
            .map_err(|e| e.to_string())
    })
    .await
}

/// Run one escalation as a durable job (#120): local runs immediately;
/// cloud requires standing consent (settings) AND a per-payload grant whose
/// hash matches this exact preview. The result is a staged proposal —
/// accept/reject goes through record_agent_decision; the graph is never
/// touched (R-INT-1/R-INT-3).
#[tauri::command]
async fn run_escalation(
    gap_id: String,
    mode: String,
    approved_payload_hash: Option<String>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<agents::StagedProposal, String> {
    let (running, execution) =
        agent_pool::start_job(&app, &format!("escalate:{gap_id}:{mode}")).await?;
    emit_job(&app, &running);
    let fail = |error: String| -> String {
        report_failure(&app, &state, &execution, &error);
        error
    };

    report_progress(&app, &state, &execution, "context", 20.0).map_err(&fail)?;
    let context_app = app.clone();
    let task = off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = context_app.state::<AppState>();
        if job_cancelled(&state, execution)? {
            return Err("cancelled".into());
        }
        task_evidence::prepare(&state, &gap_id, &format!("escalate:{gap_id}"))
    })
    .await
    .map_err(&fail)?;

    report_progress(&app, &state, &execution, "model", 70.0).map_err(&fail)?;
    let policy = (|| {
        let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        settings_store.egress_policy().map_err(|e| e.to_string())
    })()
    .map_err(&fail)?;
    let provider = egress::escalation_provider(&mode, saved_ollama_port(&state).map_err(&fail)?)
        .map_err(&fail)?;
    // #1767: a cloud payload is recorded when it is handed to the provider,
    // so a call that then fails or times out is counted. If the record
    // cannot be written the call is not made.
    let recording_app = app.clone();
    let firewall = llm::EgressFirewall::new(policy).with_egress_recorder(move |provider, bytes| {
        let state = recording_app.state::<AppState>();
        let Ok(mut settings) = state.settings.lock() else {
            return false;
        };
        settings.record_egress(provider, bytes).is_ok()
    });
    let broker = agents::AgentBroker::bounded_default();
    // A cloud run re-derives the preview and only proceeds when the user's
    // approved hash matches this exact payload (one-action consent).
    let consent = if mode == "cloud" {
        let preview = broker
            .preview_prepared(provider.as_ref(), &firewall, &task)
            .map_err(|e| fail(e.to_string()))?;
        Some(
            egress::consent_grant_for_approved_payload(&preview, approved_payload_hash)
                .map_err(&fail)?,
        )
    } else {
        None
    };

    // Cooperative cancellation: a cancel that landed after context assembly
    // must stop the run before any provider is invoked — for cloud, before
    // the consented payload could leave the device.
    if job_cancelled(&state, &execution)? {
        return Err("cancelled".to_string());
    }
    let staging_app = app.clone();
    let proposal = off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = staging_app.state::<AppState>();
        if job_cancelled(&state, execution)? {
            return Err("cancelled".to_string());
        }
        let proposal = broker
            .propose_prepared(provider.as_ref(), &firewall, &task, consent.as_ref())
            .map_err(|error| error.to_string())?;
        // Persist even if cancellation arrived during the model call. The job
        // remains cancelled, but its completed result is available for review.
        stage_completed_prepared_job_proposal(&state, execution, &task, &proposal)
    })
    .await
    .map_err(&fail)?;

    report_progress(&app, &state, &execution, "validate", 90.0).map_err(&fail)?;
    let job = (|| {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        jobs.finish_execution(&execution, std::slice::from_ref(&proposal.proposal_id))
            .map_err(|e| e.to_string())
    })()
    .map_err(&fail)?;
    emit_job(&app, &updated_job(job));
    Ok(proposal)
}

/// Escalate every instance of a gap class as one durable job (#167).
/// Local-tier only: a cloud consent grant binds to one exact payload hash
/// (AC-0063) and is never amortized across a class — cloud stays
/// per-instance. Each instance yields its own staged proposal through the
/// same propose-only path; one bad instance records a failure, never an
/// abort; cancel stops at the next instance boundary.
#[tauri::command]
async fn run_class_escalation(
    gap_ids: Vec<String>,
    mode: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<proposals::StagedBatchOutcome, String> {
    if mode != "local" {
        return Err(
            "class escalation runs local-only: a cloud consent grant binds to one exact \
             payload hash and cannot cover a whole class — escalate cloud per instance"
                .to_string(),
        );
    }
    if gap_ids.is_empty() {
        return Err("no gap instances to escalate".to_string());
    }
    let (running, execution) =
        agent_pool::start_job(&app, &format!("escalate-class:{}:{mode}", gap_ids.len())).await?;
    emit_job(&app, &running);
    let fail = |error: String| -> String {
        report_failure(&app, &state, &execution, &error);
        error
    };

    report_progress(&app, &state, &execution, "context", 5.0).map_err(&fail)?;
    // Source text is prepared lazily per instance inside the worker loop.
    // The class carries gap identities only, not an unbounded union of evidence.
    let tasks: Vec<(String, Result<String, String>)> = gap_ids
        .into_iter()
        .map(|gap_id| (gap_id.clone(), Ok(gap_id)))
        .collect();

    let policy = (|| {
        let settings_store = state.settings.lock().map_err(|e| e.to_string())?;
        settings_store.egress_policy().map_err(|e| e.to_string())
    })()
    .map_err(&fail)?;
    let provider = egress::escalation_provider(&mode, saved_ollama_port(&state).map_err(&fail)?)
        .map_err(&fail)?;
    let firewall = llm::EgressFirewall::new(policy);
    let broker = agents::AgentBroker::bounded_default();

    let batch_app = app.clone();
    let mut outcome = off_ui_thread_for_job(execution.clone(), move |execution| {
        let state = batch_app.state::<AppState>();
        let total = tasks.len();
        run_job_staged_batch(
            &state,
            execution,
            tasks,
            |index, _| {
                report_progress(
                    &batch_app,
                    &state,
                    execution,
                    &format!("escalate {}/{total}", index + 1),
                    5.0 + (index as f64 / total as f64) * 90.0,
                )
            },
            |gap_id| {
                let task = task_evidence::prepare(&state, gap_id, &format!("escalate:{gap_id}"))?;
                if job_cancelled(&state, execution)? {
                    return Err("cancelled".into());
                }
                let proposal = broker
                    .propose_prepared(provider.as_ref(), &firewall, &task, None)
                    .map_err(|error| error.to_string())?;
                stage_completed_prepared_job_proposal(&state, execution, &task, &proposal)
            },
        )
    })
    .await
    .map_err(&fail)?;

    let job = (|| {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        if outcome.proposals.is_empty() && !outcome.failures.is_empty() {
            jobs.fail_execution(
                &execution,
                "No instance produced a durable proposal; inspect the batch failures.",
            )
        } else {
            let mut artifacts: Vec<String> = outcome
                .proposals
                .iter()
                .map(|proposal| proposal.proposal_id.clone())
                .collect();
            artifacts.push(format!("failures:{}", outcome.failures.len()));
            jobs.finish_execution(&execution, &artifacts)
        }
        .map_err(|e| e.to_string())
    })()
    .map_err(&fail)?;
    let job = updated_job(job);
    outcome.cancelled |= job.status == "cancelled";
    emit_job(&app, &job);
    // A cancelled batch still returns its partial outcome (#194 review):
    // completed instances are real staged proposals the user can triage,
    // and `cancelled: true` tells the register the run stopped early.
    Ok(outcome)
}

/// Notify the shell of a job transition (`job://changed`); the Jobs surface
/// and the global progress bar stay live without polling (#117).
fn emit_job<R: tauri::Runtime>(app: &tauri::AppHandle<R>, job: &Job) {
    let _ = app.emit("job://changed", job);
}

use app_store::job_claim::claim_job;

fn start_job(state: &AppState, kind: &str) -> Result<(Job, JobExecution), String> {
    let plan = {
        let mut jobs = state.jobs.lock().map_err(|error| error.to_string())?;
        let job = jobs.enqueue(kind).map_err(|error| error.to_string())?;
        jobs.claim_plan(job.id, ClaimMode::StartQueued)
            .map_err(|error| error.to_string())?
    };
    claim_job(state, &plan)
}

fn updated_job(update: ExecutionUpdate) -> Job {
    match update {
        ExecutionUpdate::Applied(job) | ExecutionUpdate::Unchanged(job) => job,
    }
}

fn completed_job(job: &Job) -> Result<(), String> {
    match job.status.as_str() {
        "done" => Ok(()),
        "cancelled" => Err("cancelled".into()),
        _ => Err("Job execution has already finished without completing this work.".into()),
    }
}

fn check_completed_execution(state: &AppState, execution: &JobExecution) -> Result<(), String> {
    // A completed model result remains reviewable even if the cancelled job was
    // cleared during that call. The retained opaque execution supplies its
    // original identity; this exception never authorizes another model call or
    // a worker lifecycle write through a missing row.
    match state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .check_execution(execution)
    {
        Ok(ExecutionCheck::Running | ExecutionCheck::Cancelled)
        | Err(JobTransitionError::Missing) => {}
        Ok(ExecutionCheck::Terminal) => return Err("Job execution has already finished.".into()),
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

fn stage_completed_prepared_job_proposal(
    state: &AppState,
    execution: &JobExecution,
    task: &agents::PreparedAgentTask,
    proposal: &agents::AgentProposal,
) -> Result<agents::StagedProposal, String> {
    check_completed_execution(state, execution)?;
    state
        .proposals
        .lock()
        .map_err(|error| error.to_string())?
        .stage_prepared(
            task,
            proposal,
            execution.id(),
            &task.source_basis().graph_snapshot_id,
        )
        .map_err(|error| error.to_string())
}

#[cfg(test)]
fn stage_completed_job_proposal(
    state: &AppState,
    execution: &JobExecution,
    task: &agents::AgentTask,
    proposal: &agents::AgentProposal,
    graph_snapshot_id: &str,
) -> Result<agents::StagedProposal, String> {
    check_completed_execution(state, execution)?;
    state
        .proposals
        .lock()
        .map_err(|error| error.to_string())?
        .stage(task, proposal, execution.id(), graph_snapshot_id)
        .map_err(|error| error.to_string())
}

fn run_job_staged_batch<T>(
    state: &AppState,
    execution: &JobExecution,
    tasks: Vec<(String, Result<T, String>)>,
    mut progress: impl FnMut(usize, usize) -> Result<(), String>,
    mut execute_and_stage: impl FnMut(&T) -> Result<agents::StagedProposal, String>,
) -> Result<proposals::StagedBatchOutcome, String> {
    // The broker's bool cancellation callback must stop on ownership errors,
    // while the host preserves their distinct cause in the returned failure.
    let stop_error = std::cell::RefCell::new(None);
    let outcome = proposals::run_staged_batch(
        tasks,
        || match job_cancelled(state, execution) {
            Ok(cancelled) => cancelled || stop_error.borrow().is_some(),
            Err(error) => {
                *stop_error.borrow_mut() = Some(error);
                true
            }
        },
        |index, total| {
            if let Err(error) = progress(index, total) {
                *stop_error.borrow_mut() = Some(error);
            }
        },
        |task| {
            if let Some(error) = stop_error.borrow().as_ref() {
                return Err(error.clone());
            }
            match job_cancelled(state, execution) {
                Ok(false) => execute_and_stage(task),
                Ok(true) => Err("cancelled".into()),
                Err(error) => {
                    *stop_error.borrow_mut() = Some(error.clone());
                    Err(error)
                }
            }
        },
    );
    if let Some(error) = stop_error.into_inner() {
        return Err(error);
    }
    Ok(outcome)
}

fn recover_jobs(jobs: &Mutex<JobStore>, locks: &JobExecutionLocks) -> Result<Vec<Job>, String> {
    let candidates = jobs
        .lock()
        .map_err(|error| error.to_string())?
        .recovery_candidates()
        .map_err(|error| error.to_string())?;
    let mut recovered = Vec::new();
    for candidate in candidates {
        let reservation = match locks.try_reserve(candidate.lock_target()) {
            Ok(reservation) => reservation,
            Err(JobTransitionError::Busy) => continue,
            Err(error) => return Err(error.to_string()),
        };
        if let Some(job) = jobs
            .lock()
            .map_err(|error| error.to_string())?
            .recover_reserved(&candidate, reservation)
            .map_err(|error| error.to_string())?
        {
            recovered.push(job);
        }
    }
    Ok(recovered)
}

/// A live "what it's doing right now" ping (#209) — current adapter/file
/// being read. Deliberately not part of the durable `Job` row: it's
/// best-effort and streamed far more often than a SQLite write per file
/// would tolerate, so it rides its own event instead of `job://changed`.
#[derive(Clone, Serialize)]
struct JobDetail<'a> {
    id: i64,
    detail: &'a str,
}

/// The `job://detail` a local recovery reports while it reconciles its
/// preflight findings with the eval proof it just published (AC-0200).
const RECONCILE_PREFLIGHT_DETAIL: &str = "Reconciling preflight findings";

fn emit_detail<R: tauri::Runtime>(app: &tauri::AppHandle<R>, job_id: i64, detail: &str) {
    let _ = app.emit("job://detail", JobDetail { id: job_id, detail });
}

/// A throttled sink for `extract_tree_incremental`'s per-file detail pings:
/// at most one `job://detail` event every ~120ms, so a repo with thousands
/// of files can't flood the Tauri IPC bridge. The first call always fires.
fn detail_throttle<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    job_id: i64,
) -> impl FnMut(&str) + '_ {
    let mut last: Option<std::time::Instant> = None;
    move |detail: &str| {
        let now = std::time::Instant::now();
        let due =
            last.is_none_or(|t| now.duration_since(t) >= std::time::Duration::from_millis(120));
        if due {
            emit_detail(app, job_id, detail);
            last = Some(now);
        }
    }
}

/// Record stage + percent for a running job and notify the shell.
fn report_progress<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    execution: &JobExecution,
    stage: &str,
    percent: f64,
) -> Result<(), String> {
    let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
    let job = updated_job(
        jobs.progress_execution(execution, stage, percent)
            .map_err(|e| e.to_string())?,
    );
    emit_job(app, &job);
    Ok(())
}

/// Persist a job failure with its detail and notify the shell.
fn report_failure<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    execution: &JobExecution,
    error: &str,
) {
    let Ok(mut jobs) = state.jobs.lock() else {
        return;
    };
    if let Ok(update) = jobs.fail_execution(execution, error) {
        emit_job(app, &updated_job(update));
    }
}

fn finish_source_operation<R: tauri::Runtime>(
    state: &AppState,
    app: &tauri::AppHandle<R>,
    execution: &JobExecution,
    operation: &mut SourceOperation,
) -> Result<(), String> {
    if job_cancelled(state, execution)? {
        return Err("cancelled".into());
    }
    // Settle the cancellation race before publishing readiness. A failed final
    // availability transaction leaves all managed sources unavailable, even if
    // the historical job has already recorded its completed recovery work.
    let job = {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        updated_job(
            jobs.finish_execution(execution, &[])
                .map_err(|e| e.to_string())?,
        )
    };
    // Readiness and the released reservations come before `done` is
    // announced (#497 review): a preflight the user starts once they see it
    // must be able to read the recovered source.
    completed_job(&job)?;
    let ready = operation.set_writes_ready(&state.sources, true);
    operation.release();
    emit_job(app, &job);
    if ready.is_ok() {
        // AC-0506: managed-repo and system ingests settle here, so the
        // knowledge phase follows them as it follows local ingests.
        knowledge_phase::spawn_after_ingest(app);
    }
    ready
}

fn job_cancelled(state: &AppState, execution: &JobExecution) -> Result<bool, String> {
    match state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .check_execution(execution)
        .map_err(|error| error.to_string())?
    {
        ExecutionCheck::Running => Ok(false),
        ExecutionCheck::Cancelled => Ok(true),
        ExecutionCheck::Terminal => Err("Job execution has already finished.".into()),
    }
}

fn ensure_running_job(state: &AppState, execution: &JobExecution) -> Result<(), String> {
    if job_cancelled(state, execution)? {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

fn register_local_source(
    state: &AppState,
    path: &std::path::Path,
) -> Result<RegisteredSource, String> {
    let root = paths::canonicalize(path).map_err(|e| e.to_string())?;
    let mut registry = state.sources.lock().map_err(|e| e.to_string())?;
    if let Some(source) = registry.get_by_root(&root)? {
        return Ok(source);
    }
    registry.register_local(&root)
}

fn source_operation(
    state: &AppState,
    mut requested: Vec<(RegisteredSource, bool)>,
) -> Result<SourceOperation, String> {
    let nodes = state
        .graph
        .lock()
        .map_err(|e| e.to_string())?
        .read_snapshot()
        .map_err(|e| e.to_string())?
        .0;
    {
        let registry = state.sources.lock().map_err(|e| e.to_string())?;
        for node in nodes.iter().filter(|node| node.label == "Repo") {
            let repo = node
                .id
                .strip_prefix("repo:")
                .ok_or("invalid repository fact identity")?;
            let source = registry
                .get_by_repo(repo)?
                .ok_or("registered ADR source unavailable; reconnect source")?;
            requested.push((source, false));
        }
    }
    SourceOperation::acquire(&state.sources, requested)
}

/// The staged ingest pipeline behind `ingest_path` and `retry_job`: extract →
/// load → stitch, with progress events and cooperative cancellation.
fn run_ingest<R: tauri::Runtime>(
    source: &RegisteredSource,
    operation: &SourceOperation,
    execution: &JobExecution,
    app: &tauri::AppHandle<R>,
    state: &AppState,
) -> Result<IngestSummary, String> {
    let job_id = execution.id();
    let fail = |error: String| -> String {
        report_failure(app, state, execution, &error);
        error
    };
    let cancelled = || -> Result<(), String> {
        if job_cancelled(state, execution)? {
            // Cancelled by the user: status is already `cancelled`; the
            // pipeline just stops. Not a failure.
            return Err("cancelled".to_string());
        }
        Ok(())
    };

    report_progress(app, state, execution, "scan", 5.0)?;
    let root = operation.root(&source.repo_key).map_err(&fail)?;
    let repo = source.repo_key.clone();
    let local = local_commit::local_commit(root);
    let commit = local.as_str();

    cancelled()?;
    report_progress(app, state, execution, "extract", 15.0)?;
    let active_plugins = active_plugins_for_root(app, state, root).map_err(&fail)?;
    let limits = recovery_limits(state).map_err(&fail)?;
    let registries = event_registries_for(app).map_err(&fail)?;
    let mut on_file = detail_throttle(app, job_id);
    let primary = state
        .primary_sources
        .prepare_at(
            source,
            root,
            &[],
            None,
            capture_limits(state).map_err(&fail)?,
            selection_visited_entries(state).map_err(&fail)?,
        )
        .map_err(&fail)?;
    let mut receipts = Vec::new();
    let mut reads = ProducerReads {
        unfollowed_links: primary.unfollowed.clone(),
        ..ProducerReads::default()
    };
    let (extraction, facts, layers, delta) = {
        let mut caches = state
            .extraction_caches
            .lock()
            .map_err(|e| fail(e.to_string()))?;
        ensure_running_job(state, execution)?;
        let cache = caches.repos.entry(repo.clone()).or_default();
        extract_tree_packed(
            root,
            &repo,
            commit,
            &[],
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &observation_inputs::ObservationInputs::default(),
            &[],
            limits,
            cache,
            &active_plugins,
            &registries,
            &mut on_file,
            primary.capture.as_ref(),
            &mut receipts,
            &mut reads,
        )
        .map_err(fail)?
    };

    cancelled()?;
    let (omitted_stored_receipts, omitted_stored_facts) = state
        .primary_sources
        .persist(&primary, source, &receipts)
        .map_err(&fail)?
        .keep_retained(&mut receipts);
    reads.omitted_stored_receipts = omitted_stored_receipts;
    reads.record_stored_omissions(omitted_stored_facts);
    let bindings = primary_source::matching_bindings_packed(&facts, &receipts).map_err(&fail)?;
    // Scanned before publication so the inventory is published with the
    // facts it describes (AC-0239). Findings are still persisted only once
    // the graph holds the recovered facts and the job has settled.
    emit_detail(app, job_id, RECONCILE_PREFLIGHT_DETAIL);
    let preflight = reconciled_preflight_report(
        root,
        primary.capture.as_ref(),
        &plugin_coverage(&active_plugins),
        &extraction.eval_sites,
    )
    .map_err(&fail)?;
    let coverage = repo_coverage(
        &repo,
        commit,
        local.commit_note(),
        &[],
        &layers,
        &coverage_view(&facts).map_err(&fail)?,
        &DeclaredInputs::default(),
        &reads,
    )
    .with_unsupported(&preflight)
    .with_receipt_omissions(
        ingest::coverage::ReceiptOmission::Declarations,
        &reads.omitted_declaration_receipts,
    )
    .with_receipt_omissions(
        ingest::coverage::ReceiptOmission::Rules,
        &reads.omitted_rule_receipts,
    )
    .with_receipt_omissions(
        ingest::coverage::ReceiptOmission::StoreCapacity,
        &reads.omitted_stored_receipts,
    )
    .with_unfollowed_links(&reads.unfollowed_links)
    .with_unread_adrs(&reads.adr_too_large, &reads.adr_not_utf8)
    .with_documents(reads.documents.clone());
    let record =
        coverage_record(&coverage, source, job_id, &active_plugins, &registries).map_err(&fail)?;
    let omission_records = reads.source_omissions();
    let omissions = if reads.captured_ts_ran {
        core_graph::source::SourceOmissions {
            records: &omission_records,
            complete: reads.omitted_receipt_facts_complete,
        }
    } else {
        core_graph::source::SourceOmissions::NONE
    };
    report_progress(app, state, execution, "load", 70.0)?;
    let mut published = OperationFacts::default();
    // Everything recovery reads from `root` has been read; recheck before
    // taking the graph lock so a full status scan never blocks other jobs.
    local.ensure_unchanged(root).map_err(&fail)?;
    {
        let mut graph = state.graph.lock().map_err(|e| fail(e.to_string()))?;
        ensure_running_job(state, execution)?;
        published.record_load(
            load_packed_into_graph(
                &mut graph,
                facts,
                &repo,
                root,
                commit,
                &bindings,
                &[],
                Some(&record),
                omissions,
            )
            .map_err(&fail)?
            .published,
        );
        report_progress(app, state, execution, "stitch", 90.0)?;

        ensure_running_job(state, execution)?;
        stitch_backings(&mut graph, &mut published, &registries).map_err(&fail)?;
        stitch_name_unique(&mut graph, &mut published).map_err(&fail)?;
    }
    reclaim_superseded_receipts(state, source, primary, None);

    relink_found_adrs(
        state,
        operation,
        execution,
        &mut published,
        &mut Default::default(),
        &Documented::from([(repo.clone(), Vec::new())]),
        limits.api_docs,
    )
    .map_err(&fail)?;

    ensure_running_job(state, execution)?;
    record_ingest_metrics(
        state,
        job_id,
        &repo,
        commit,
        &layers,
        &std::collections::BTreeSet::from([repo.clone()]),
    )
    .map_err(&fail)?;

    // Ordered before `done` is published (#497), so a preflight the user
    // starts once they see it is newer than this reconciliation.
    let fence = app.state::<PreflightRuns>().reserve_reconcile()?;
    // A cancel can land at any point after the last check; `finish` is
    // guarded to only transition a running job, so whichever outcome hit
    // the store first wins — read the row back to learn which.
    {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = updated_job(
            jobs.finish_execution(execution, &[format!("graph:{repo}@{commit}")])
                .map_err(|e| e.to_string())?,
        );
        emit_job(app, &job);
        completed_job(&job)?;
    }
    // Written only once the job has settled as completed, so a cancel that
    // wins the race writes nothing (AC-0200, #489); through the per-repo
    // fence, as GitHub and manifest recoveries do (AC-0209).
    let preflight_register =
        app.state::<PreflightRuns>()
            .reconcile(&fence, &repo, &preflight, || {
                persist_preflight_findings(state, &repo, &preflight)
            })?;
    Ok(IngestSummary {
        job_id,
        repo,
        files: layers.files(),
        nodes: published.nodes(),
        edges: published.edges(),
        merged: published.merged,
        layers,
        delta,
        preflight,
        preflight_register,
        coverage: vec![coverage],
        registries: RegistryReport::from(&registries),
    })
}

/// Run T0 extraction over a local directory and load the facts into the
/// graph (US-0002 local path; GitHub clone ingest is `add_repo`).
/// Run `work` on a blocking worker thread. Recovery pipelines must never
/// execute on the invoking thread: synchronous Tauri commands run on the
/// main thread, so an inline ingest freezes the whole app for its duration
/// (AC-0078, #158 — the macOS beachball on large repos).
async fn off_ui_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

/// The blocking worker owns an execution clone independently of its async
/// waiter. Aborting the waiter cannot release a worker that is still running.
async fn off_ui_thread_for_job<T: Send + 'static>(
    execution: JobExecution,
    work: impl FnOnce(&JobExecution) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    off_ui_thread(move || work(&execution)).await
}

/// Classify dropped paths before intake (#587, AC-0237): each gets an
/// explicit accepted route or rejection reason, and nothing is ingested yet.
#[tauri::command]
async fn classify_drop(paths: Vec<String>) -> Result<Vec<drop_intake::DropCandidate>, String> {
    off_ui_thread(move || Ok(drop_intake::classify(&paths))).await
}

#[tauri::command]
async fn ingest_path(path: String, app: tauri::AppHandle) -> Result<IngestSummary, String> {
    off_ui_thread(move || ingest_path_blocking(path, app)).await
}

fn ingest_path_blocking(path: String, app: tauri::AppHandle) -> Result<IngestSummary, String> {
    let state = app.state::<AppState>();
    let source = register_local_source(&state, std::path::Path::new(&path))?;
    let operation = source_operation(&state, vec![(source.clone(), false)])?;
    let (running, execution) = start_job(&state, &source.ingest_job_kind())?;
    emit_job(&app, &running);
    let summary = run_ingest(&source, &operation, &execution, &app, &state)?;
    knowledge_phase::spawn_after_ingest(&app);
    Ok(summary)
}

/// Cancel a queued or running job; running work stops at its next stage
/// boundary (#117).
#[tauri::command]
fn cancel_job(id: i64, app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Job, String> {
    let job = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .cancel(id)
        .map_err(|e| e.to_string())?;
    if let Some(investigation_id) = &job.investigation_id {
        state.investigations.wake(investigation_id);
        let detail = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .investigation(investigation_id)
            .map_err(|e| e.to_string())?;
        investigations::emit_changed(&app, &detail);
    }
    emit_job(&app, &job);
    Ok(job)
}

/// Retry a failed or cancelled job, or resume an interrupted one: claim a new
/// running attempt on the same row for kinds the shell can re-run
/// (`ingest-source-v1:*` reuses the content-addressed cache, so a resume recomputes
/// only what the interrupted run didn't finish — ADR-0014).
#[tauri::command]
async fn retry_job(id: i64, app: tauri::AppHandle) -> Result<RetriedJob, String> {
    off_ui_thread(move || retry_job_blocking(id, app)).await
}

/// A retried job's row, plus — for a retried recovery — its reconciled
/// preflight report, so the Preflight surface receives it exactly as it does
/// from the recovery command itself (#458).
#[derive(Serialize)]
struct RetriedJob {
    #[serde(flatten)]
    job: Job,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery: Option<RetriedRecovery>,
}

#[derive(Serialize)]
struct RetriedRecovery {
    preflight: ingest::preflight::PreflightReport,
    preflight_register: Option<RegisterStamp>,
}

impl From<Job> for RetriedJob {
    fn from(job: Job) -> Self {
        Self {
            job,
            recovery: None,
        }
    }
}

fn retry_source(state: &AppState, kind: &str) -> Result<Option<RegisteredSource>, String> {
    if kind.starts_with("ingest:") || kind.starts_with("ingest-source-v1:") {
        let id = sources::source_id_from_ingest_job_kind(kind)?;
        let source = state
            .sources
            .lock()
            .map_err(|e| e.to_string())?
            .get_by_id(id)?
            .ok_or("registered source unavailable; re-run ingestion")?;
        if !source.is_ready() {
            return Err("registered source unavailable; reconnect source".into());
        }
        return Ok(Some(source));
    }
    if kind == "noop" || kind.starts_with("plugin-gate:") {
        return Ok(None);
    }
    Err("retry is not supported for this job; re-run the add".into())
}

fn prepare_job_retry(
    state: &AppState,
    id: i64,
) -> Result<
    (
        Job,
        JobExecution,
        Option<RegisteredSource>,
        Option<SourceOperation>,
    ),
    String,
> {
    let plan = state
        .jobs
        .lock()
        .map_err(|e| e.to_string())?
        .claim_plan(id, ClaimMode::RetryTerminal)
        .map_err(|e| e.to_string())?;
    // Validate the supported binding and complete source guard plan before the
    // retry claim. Historical/unavailable jobs remain byte-for-byte intact.
    let source = retry_source(state, &plan.job().kind)?;
    let operation = source
        .as_ref()
        .map(|source| source_operation(state, vec![(source.clone(), false)]))
        .transpose()?;
    let (job, execution) = claim_job(state, &plan)?;
    Ok((job, execution, source, operation))
}

fn retry_job_blocking<R: tauri::Runtime>(
    id: i64,
    app: tauri::AppHandle<R>,
) -> Result<RetriedJob, String> {
    let state = app.state::<AppState>();
    let (job, execution, source, operation) = prepare_job_retry(&state, id)?;
    emit_job(&app, &job);
    let kind = job.kind;
    if let (Some(source), Some(operation)) = (source, operation) {
        let summary = run_ingest(&source, &operation, &execution, &app, &state)?;
        knowledge_phase::spawn_after_ingest(&app);
        let job = state
            .jobs
            .lock()
            .map_err(|e| e.to_string())?
            .get(id)
            .map_err(|e| e.to_string())?;
        return Ok(RetriedJob {
            job,
            recovery: Some(RetriedRecovery {
                preflight: summary.preflight,
                preflight_register: summary.preflight_register,
            }),
        });
    }
    // A conformance gate re-runs whole (#206 review): the verdict re-binds
    // to whatever bytes are on disk now, which is exactly what a retry
    // after an interrupt or a fixed corpus should do.
    if let Some(plugin_id) = kind.strip_prefix("plugin-gate:") {
        plugin_gate_blocking(plugin_id, &execution, &app)?;
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        return jobs
            .get(id)
            .map(RetriedJob::from)
            .map_err(|e| e.to_string());
    }
    if kind == "noop" {
        let mut jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        let job = updated_job(
            jobs.finish_execution(&execution, &[])
                .map_err(|e| e.to_string())?,
        );
        emit_job(&app, &job);
        return Ok(job.into());
    }
    // add-repo / add-system re-dispatch needs their pipelines refactored
    // behind the same job-id seam; until then the caller is told explicitly
    // rather than silently doing nothing.
    let error = format!("retry re-dispatch not yet supported for kind '{kind}' — re-run the add");
    report_failure(&app, &state, &execution, &error);
    Err(error)
}

#[derive(Serialize)]
struct AddRepoSummary {
    job_id: i64,
    repo: String,
    commit_sha: String,
    files: u64,
    nodes: u64,
    edges: u64,
    merged: MergedFacts,
    layers: LayerBreakdown,
    delta: DeltaSummary,
    /// The clone's preflight report, reconciled with this recovery's eval
    /// proof exactly as a local recovery's is (AC-0209, #446).
    preflight: ingest::preflight::PreflightReport,
    /// As `IngestSummary::preflight_register` (#458).
    preflight_register: Option<RegisterStamp>,
    /// As `IngestSummary::coverage`.
    coverage: Vec<ingest::coverage::RepoCoverage>,
    /// As `IngestSummary::registries`.
    registries: RegistryReport,
}

/// Clone a GitHub repo (read-only, shallow) and ingest it with its real
/// identity — every fact's evidence carries owner/name@sha (US-0001,
/// AC-0001). Auth per the ADR-0009 ladder; failures carry remediation and
/// leave no partial clone (AC-0003).
#[tauri::command]
async fn add_repo(url: String, app: tauri::AppHandle) -> Result<AddRepoSummary, String> {
    off_ui_thread(move || add_repo_blocking(url, app)).await
}

fn add_repo_blocking<R: tauri::Runtime>(
    url: String,
    app: tauri::AppHandle<R>,
) -> Result<AddRepoSummary, String> {
    let state = app.state::<AppState>();
    let source = state
        .sources
        .lock()
        .map_err(|e| e.to_string())?
        .reserve_managed(&url)?;
    let mut operation = source_operation(&state, vec![(source.clone(), true)])?;
    let (running, execution) = start_job(&state, &format!("add-repo:{url}"))?;
    let job_id = execution.id();
    emit_job(&app, &running);
    let fail = |e: String| -> String {
        report_failure(&app, &state, &execution, &e);
        e
    };

    if job_cancelled(&state, &execution)? {
        return Err("cancelled".into());
    }
    let token = ingest::discover_token();
    let cloned = operation
        .clone_source(&source, token.as_deref())
        .map_err(&fail)?;
    if job_cancelled(&state, &execution)? {
        return Err("cancelled".into());
    }
    let root = operation.root(&source.repo_key).map_err(&fail)?;
    let active_plugins = active_plugins_for_root(&app, &state, root).map_err(&fail)?;
    let limits = recovery_limits(&state).map_err(&fail)?;
    let registries = event_registries_for(&app).map_err(&fail)?;
    let mut on_file = detail_throttle(&app, job_id);
    // A managed clone is captured from its cloned commit's Git objects (AC-1022).
    let primary = state
        .primary_sources
        .prepare_at(
            &source,
            root,
            &[],
            Some(&cloned.commit_sha),
            capture_limits(&state).map_err(&fail)?,
            selection_visited_entries(&state).map_err(&fail)?,
        )
        .map_err(&fail)?;
    let mut receipts = Vec::new();
    let mut reads = ProducerReads {
        unfollowed_links: primary.unfollowed.clone(),
        ..ProducerReads::default()
    };
    let (extraction, facts, layers, delta) = {
        let mut caches = state
            .extraction_caches
            .lock()
            .map_err(|e| fail(e.to_string()))?;
        ensure_running_job(&state, &execution)?;
        let cache = caches.repos.entry(source.repo_key.clone()).or_default();
        extract_tree_packed(
            root,
            &source.repo_key,
            &cloned.commit_sha,
            &[],
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
            &observation_inputs::ObservationInputs::default(),
            &[],
            limits,
            cache,
            &active_plugins,
            &registries,
            &mut on_file,
            primary.capture.as_ref(),
            &mut receipts,
            &mut reads,
        )
        .map_err(&fail)?
    };
    if job_cancelled(&state, &execution)? {
        return Err("cancelled".to_string());
    }
    let (omitted_stored_receipts, omitted_stored_facts) = state
        .primary_sources
        .persist(&primary, &source, &receipts)
        .map_err(&fail)?
        .keep_retained(&mut receipts);
    reads.omitted_stored_receipts = omitted_stored_receipts;
    reads.record_stored_omissions(omitted_stored_facts);
    let bindings = primary_source::matching_bindings_packed(&facts, &receipts).map_err(&fail)?;
    // As for a local recovery (AC-0200, AC-0209, AC-0239): scanned from the
    // captured bytes the claims were proven on, before publication so the
    // inventory is published with its facts; findings persist only later.
    let preflight = reconciled_preflight_report(
        root,
        primary.capture.as_ref(),
        &plugin_coverage(&active_plugins),
        &extraction.eval_sites,
    )
    .map_err(&fail)?;
    let coverage = repo_coverage(
        &source.repo_key,
        &cloned.commit_sha,
        None,
        &[],
        &layers,
        &coverage_view(&facts).map_err(&fail)?,
        &DeclaredInputs::default(),
        &reads,
    )
    .with_unsupported(&preflight)
    .with_receipt_omissions(
        ingest::coverage::ReceiptOmission::Declarations,
        &reads.omitted_declaration_receipts,
    )
    .with_receipt_omissions(
        ingest::coverage::ReceiptOmission::Rules,
        &reads.omitted_rule_receipts,
    )
    .with_receipt_omissions(
        ingest::coverage::ReceiptOmission::StoreCapacity,
        &reads.omitted_stored_receipts,
    )
    .with_unfollowed_links(&reads.unfollowed_links)
    .with_unread_adrs(&reads.adr_too_large, &reads.adr_not_utf8)
    .with_documents(reads.documents.clone());
    let record =
        coverage_record(&coverage, &source, job_id, &active_plugins, &registries).map_err(&fail)?;
    let omission_records = reads.source_omissions();
    let omissions = if reads.captured_ts_ran {
        core_graph::source::SourceOmissions {
            records: &omission_records,
            complete: reads.omitted_receipt_facts_complete,
        }
    } else {
        core_graph::source::SourceOmissions::NONE
    };
    let mut published = OperationFacts::default();
    {
        let mut graph = state.graph.lock().map_err(|e| fail(e.to_string()))?;
        ensure_running_job(&state, &execution)?;
        published.record_load(
            load_packed_into_graph(
                &mut graph,
                facts,
                &source.repo_key,
                root,
                &cloned.commit_sha,
                &bindings,
                &[],
                Some(&record),
                omissions,
            )
            .map_err(&fail)?
            .published,
        );

        ensure_running_job(&state, &execution)?;
        stitch_backings(&mut graph, &mut published, &registries).map_err(&fail)?;
        stitch_name_unique(&mut graph, &mut published).map_err(&fail)?;
    }
    reclaim_superseded_receipts(&state, &source, primary, None);
    relink_found_adrs(
        &state,
        &operation,
        &execution,
        &mut published,
        &mut Default::default(),
        &Documented::from([(source.repo_key.clone(), Vec::new())]),
        limits.api_docs,
    )
    .map_err(&fail)?;
    ensure_running_job(&state, &execution)?;
    record_ingest_metrics(
        &state,
        job_id,
        &source.repo_key,
        &cloned.commit_sha,
        &layers,
        &std::collections::BTreeSet::from([source.repo_key.clone()]),
    )
    .map_err(&fail)?;
    // Reserved before `done` is published (#497).
    let fence = app.state::<PreflightRuns>().reserve_reconcile()?;
    finish_source_operation(&state, &app, &execution, &mut operation)?;
    // Written only once the job has settled as completed, so a cancel that
    // wins the race writes nothing (AC-0209); through the per-repo fence.
    let preflight_register =
        app.state::<PreflightRuns>()
            .reconcile(&fence, &source.repo_key, &preflight, || {
                persist_preflight_findings(&state, &source.repo_key, &preflight)
            })?;
    Ok(AddRepoSummary {
        job_id,
        repo: source.repo_key,
        commit_sha: cloned.commit_sha,
        files: layers.files(),
        nodes: published.nodes(),
        edges: published.edges(),
        merged: published.merged,
        layers,
        delta,
        preflight,
        preflight_register,
        coverage: vec![coverage],
        registries: RegistryReport::from(&registries),
    })
}

/// GitHub-ish references clone; anything else is a path relative to the
/// manifest (local repos in one checkout, the dogfood case). A two-segment
/// entry like `services/api` is only owner/name shorthand when nothing by
/// that path exists next to the manifest — never resolved against the
/// process cwd.
fn manifest_entry_is_remote(url: &str, base: &std::path::Path) -> bool {
    url.starts_with("https://")
        || url.starts_with("git@")
        || url.starts_with("file://")
        || (url.split('/').count() == 2 && !base.join(url).exists())
}

#[derive(Serialize)]
struct AddSystemSummary {
    job_id: i64,
    /// `identity@sha12` per ingested repo, in manifest order.
    repos: Vec<String>,
    files: u64,
    nodes: u64,
    edges: u64,
    merged: MergedFacts,
    layers: LayerBreakdown,
    delta: DeltaSummary,
    /// Each recovered repo's reconciled preflight report, in manifest order
    /// (AC-0209, #446). Not `preflight`: the Preflight surface shows one
    /// repo's report, and a system has several.
    preflights: Vec<RepoPreflight>,
    /// Each repo's coverage inventory, in manifest order (AC-0238).
    coverage: Vec<ingest::coverage::RepoCoverage>,
    /// As `IngestSummary::registries`; one snapshot for every repo.
    registries: RegistryReport,
}

/// One repo's reconciled preflight report within a system recovery.
#[derive(Serialize)]
struct RepoPreflight {
    repo: String,
    report: ingest::preflight::PreflightReport,
}

fn manifest_dir(path: &std::path::Path) -> &std::path::Path {
    if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(std::path::Path::new("."))
    }
}

/// Ingest a whole system from `cartograph.system.toml` (US-0001 AC-0002):
/// clone/read every declared repo, apply its layer hints and the
/// manifest's known channel identities at ingest.
#[tauri::command]
async fn add_system(path: String, app: tauri::AppHandle) -> Result<AddSystemSummary, String> {
    off_ui_thread(move || add_system_blocking(path, app)).await
}

fn add_system_blocking<R: tauri::Runtime>(
    path: String,
    app: tauri::AppHandle<R>,
) -> Result<AddSystemSummary, String> {
    let state = app.state::<AppState>();
    let manifest_path = paths::canonicalize(&path).map_err(|e| e.to_string())?;
    let manifest =
        ingest::manifest::SystemManifest::load(&manifest_path).map_err(|e| e.to_string())?;
    let base = manifest_dir(&manifest_path);
    let admitted = {
        let mut registry = state.sources.lock().map_err(|e| e.to_string())?;
        manifest
            .repos
            .iter()
            .map(|entry| {
                let remote = manifest_entry_is_remote(&entry.url, base);
                let source = if remote {
                    registry.reserve_managed(&entry.url)?
                } else {
                    registry.register_local(&base.join(&entry.url))?
                };
                Ok((source, remote))
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    let mut operation = source_operation(&state, admitted.clone())?;
    let (running, execution) = start_job(&state, &format!("add-system:{path}"))?;
    let job_id = execution.id();
    emit_job(&app, &running);
    let fail = |e: String| -> String {
        report_failure(&app, &state, &execution, &e);
        e
    };

    let token = ingest::discover_token();
    let limits = recovery_limits(&state).map_err(&fail)?;
    let mut cloned_commits = std::collections::BTreeMap::new();
    for (source, remote) in &admitted {
        if *remote
            && let std::collections::btree_map::Entry::Vacant(entry) =
                cloned_commits.entry(source.source_id.clone())
        {
            if job_cancelled(&state, &execution)? {
                return Err("cancelled".into());
            }
            let cloned = operation
                .clone_source(source, token.as_deref())
                .map_err(&fail)?;
            entry.insert(cloned.commit_sha);
        }
    }

    let mut repos = Vec::new();
    let mut repo_identities = std::collections::BTreeSet::new();
    let mut files = 0u64;
    let mut published = OperationFacts::default();
    let mut layers = LayerBreakdown::default();
    let mut delta = DeltaSummary::default();
    // Each repo's eval proof, reconciled only once the whole system is
    // published: a later repo's failure must leave every register pending.
    let mut proofs = Vec::new();
    let mut recorded = std::collections::BTreeMap::new();
    let mut on_file = detail_throttle(&app, job_id);
    // AC-0240: declared services and gateway routes, resolved to repo keys
    // once. Their origins are deferred from each repo's own fetch matching.
    let repo_keys = manifest
        .repos
        .iter()
        .zip(&admitted)
        .map(|(entry, (source, _))| (entry.url.clone(), source.repo_key.clone()))
        .collect();
    let http_bindings = manifest_http_bindings(&manifest, &repo_keys);
    let deferred_origins = events::binding_origins(&http_bindings);
    // One registry snapshot for every repo of the system (#594).
    let registries = event_registries_for(&app).map_err(&fail)?;
    for (entry, (source, remote)) in manifest.repos.iter().zip(&admitted) {
        if job_cancelled(&state, &execution)? {
            return Err("cancelled".to_string());
        }
        let root = operation.root(&source.repo_key).map_err(&fail)?;
        let repo = source.repo_key.clone();
        let local = (!*remote).then(|| local_commit::local_commit(root));
        let commit = match &local {
            Some(local) => local.as_str().to_string(),
            None => cloned_commits
                .get(&source.source_id)
                .ok_or("missing managed clone result")?
                .clone(),
        };
        // state_json travels with the manifest, so it resolves against the
        // manifest dir — same rule as local repo paths.
        // Those inside the manifest folder are captured under their own input
        // identity, never the repo's (ADR-0080, AC-1041).
        let observed = observation_inputs::ObservationInputs::declared(
            base,
            &source.source_id,
            entry.state_json.as_deref(),
            entry.pulumi_json.as_deref(),
            &entry.otel_jsonl,
        );
        let active_plugins = active_plugins_for_root(&app, &state, root).map_err(&fail)?;
        // A managed clone is captured from its cloned commit's Git objects
        // (AC-1022); a local entry keeps a working-tree capture.
        let primary = state
            .primary_sources
            .prepare_at(
                source,
                root,
                &entry.layers,
                (*remote).then_some(commit.as_str()),
                capture_limits(&state).map_err(&fail)?,
                selection_visited_entries(&state).map_err(&fail)?,
            )
            .map_err(&fail)?;
        let mut receipts = Vec::new();
        let mut reads = ProducerReads {
            unfollowed_links: primary.unfollowed.clone(),
            ..ProducerReads::default()
        };
        let (extraction, facts, repo_layers, repo_delta) = {
            let mut caches = state
                .extraction_caches
                .lock()
                .map_err(|e| fail(e.to_string()))?;
            ensure_running_job(&state, &execution)?;
            let cache = caches.repos.entry(repo.clone()).or_default();
            extract_tree_packed(
                root,
                &repo,
                &commit,
                &entry.layers,
                &manifest.env,
                &manifest.env_read_keys(),
                &deferred_origins,
                &observed,
                &entry.api_docs,
                limits,
                cache,
                &active_plugins,
                &registries,
                &mut on_file,
                primary.capture.as_ref(),
                &mut receipts,
                &mut reads,
            )
            .map_err(&fail)?
        };
        ensure_running_job(&state, &execution)?;
        let retained = state
            .primary_sources
            .persist(&primary, source, &receipts)
            .map_err(&fail)?;
        let charged = retained.charged();
        let (mut omitted_stored_receipts, mut omitted_stored_facts) =
            retained.keep_retained(&mut receipts);
        // The captured observation inputs are retained with the source, so
        // the bytes their evidence cites outlive this recovery (ADR-0080),
        // with a receipt for each observation's exact span (ADR-0079).
        // Each retained observed receipt binds its fact in the observed role,
        // owned by this repo (AC-1047).
        let observations = match observed.capture() {
            Some(capture) => {
                let mut observed_receipts =
                    observed_receipts_packed(capture, &facts).map_err(&fail)?;
                let (omitted, omitted_facts) = state
                    .primary_sources
                    .persist_input(source, capture, &observed_receipts, charged)
                    .map_err(&fail)?
                    .keep_retained(&mut observed_receipts);
                // Observed receipts the store had no room for are store-capacity
                // omissions too. A fact keeps an omission record only when it has
                // no retained receipt of its own, so a declaration binding is
                // never reported as omitted.
                for (path, count) in omitted {
                    *omitted_stored_receipts.entry(path).or_default() += count;
                }
                let recorded: std::collections::BTreeSet<_> = receipts
                    .iter()
                    .map(adapters_lang_ts::captured::Citable::fact_key)
                    .chain(omitted_stored_facts.iter().map(|(fact, _)| fact))
                    .cloned()
                    .collect();
                omitted_stored_facts.extend(
                    omitted_facts
                        .into_iter()
                        .filter(|(fact, _)| !recorded.contains(fact)),
                );
                primary_source::matching_bindings_packed(&facts, &observed_receipts)
                    .map_err(&fail)?
                    .into_iter()
                    .map(|binding| core_graph::source::SourceBinding {
                        repo_key: repo.clone(),
                        ..binding
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        reads.omitted_stored_receipts = omitted_stored_receipts;
        reads.record_stored_omissions(omitted_stored_facts);
        let bindings =
            primary_source::matching_bindings_packed(&facts, &receipts).map_err(&fail)?;
        // Scanned before publication so each inventory is published with its
        // facts (AC-0239); findings persist only once the system is in.
        let report = reconciled_preflight_report(
            root,
            primary.capture.as_ref(),
            &plugin_coverage(&active_plugins),
            &extraction.eval_sites,
        )
        .map_err(&fail)?;
        let coverage = repo_coverage(
            &repo,
            &commit,
            local
                .as_ref()
                .and_then(local_commit::LocalCommit::commit_note),
            &entry.layers,
            &repo_layers,
            &coverage_view(&facts).map_err(&fail)?,
            &DeclaredInputs {
                state_json: entry.state_json.clone(),
                pulumi_json: entry.pulumi_json.clone(),
                otel_jsonl: entry.otel_jsonl.clone(),
                uncaptured: observed.uncaptured_paths(),
            },
            &reads,
        )
        .with_unsupported(&report)
        .with_receipt_omissions(
            ingest::coverage::ReceiptOmission::Declarations,
            &reads.omitted_declaration_receipts,
        )
        .with_receipt_omissions(
            ingest::coverage::ReceiptOmission::Rules,
            &reads.omitted_rule_receipts,
        )
        .with_receipt_omissions(
            ingest::coverage::ReceiptOmission::StoreCapacity,
            &reads.omitted_stored_receipts,
        )
        .with_unfollowed_links(&reads.unfollowed_links)
        .with_unread_adrs(&reads.adr_too_large, &reads.adr_not_utf8)
        .with_documents(reads.documents.clone());
        let record = coverage_record(&coverage, source, job_id, &active_plugins, &registries)
            .map_err(&fail)?;
        let omission_records = reads.source_omissions();
        let omissions = if reads.captured_ts_ran {
            core_graph::source::SourceOmissions {
                records: &omission_records,
                complete: reads.omitted_receipt_facts_complete,
            }
        } else {
            core_graph::source::SourceOmissions::NONE
        };
        files += repo_layers.files();
        layers.add(repo_layers);
        delta.add(
            repo_delta.recomputed_files,
            repo_delta.reused_files,
            repo_delta.deleted_files,
        );
        // Recheck outside the graph lock; `root` is fully read by now.
        if let Some(local) = &local {
            local.ensure_unchanged(root).map_err(&fail)?;
        }
        {
            let mut graph = state.graph.lock().map_err(|e| fail(e.to_string()))?;
            ensure_running_job(&state, &execution)?;
            published.record_load(
                load_packed_into_graph(
                    &mut graph,
                    facts,
                    &repo,
                    root,
                    &commit,
                    &bindings,
                    &observations,
                    Some(&record),
                    omissions,
                )
                .map_err(&fail)?
                .published,
            );
        }
        reclaim_superseded_receipts(
            &state,
            source,
            primary,
            observed.capture().map(source_capture::Capture::id),
        );
        // The latest publication of a repo owns its stored inventory.
        recorded.insert(
            repo.clone(),
            RecordedCoverage {
                coverage: coverage.clone(),
                record,
            },
        );
        let sha12: String = commit.chars().take(12).collect();
        repos.push(format!("{repo}@{sha12}"));
        if repo_identities.insert(repo.clone()) {
            proofs.push((RepoPreflight { repo, report }, coverage));
        }
    }
    {
        // After every repo is in: infra from one repo can back channels
        // published by another.
        let mut graph = state.graph.lock().map_err(|e| fail(e.to_string()))?;
        ensure_running_job(&state, &execution)?;
        stitch_backings(&mut graph, &mut published, &registries).map_err(&fail)?;
        stitch_name_unique(&mut graph, &mut published).map_err(&fail)?;
        // AC-0240: declared services and gateway routes join absolute-URL
        // fetches to the owning repo's endpoints.
        stitch_http_bindings(&mut graph, &http_bindings, &mut published).map_err(&fail)?;
    }
    // Each published repository with the API references its manifest entry
    // declares, so the documentation pass reads them as it did at ingest.
    let declared_by_repo: std::collections::BTreeMap<&str, &[ingest::api_docs::ApiDocDeclaration]> =
        manifest
            .repos
            .iter()
            .zip(&admitted)
            .map(|(entry, (source, _))| (source.repo_key.as_str(), entry.api_docs.as_slice()))
            .collect();
    let documented: Documented = recorded
        .keys()
        .map(|repo| {
            let declared = declared_by_repo
                .get(repo.as_str())
                .copied()
                .unwrap_or_default();
            (repo.clone(), declared.to_vec())
        })
        .collect();
    let relinked = relink_found_adrs(
        &state,
        &operation,
        &execution,
        &mut published,
        &mut recorded,
        &documented,
        limits.api_docs,
    )
    .map_err(&fail)?;
    for (_, coverage) in &mut proofs {
        if let Some(&edges) = relinked.get(&coverage.repo) {
            restate_adr_edges(coverage, edges);
        }
    }
    // One history record for the whole system; the per-repo identities are
    // the record's identity (a system has no single commit).
    ensure_running_job(&state, &execution)?;
    record_ingest_metrics(
        &state,
        job_id,
        &repos.join(","),
        "system",
        &layers,
        &repo_identities,
    )
    .map_err(&fail)?;
    // AC-0200, AC-0209: each repo reconciles exactly as a local recovery.
    let (preflights, coverage): (Vec<_>, Vec<_>) = proofs.into_iter().unzip();
    // Reserved before `done` is published (#497).
    let runs = app.state::<PreflightRuns>();
    let fence = runs.reserve_reconcile()?;
    finish_source_operation(&state, &app, &execution, &mut operation)?;
    // Written only once the job has settled as completed, so a cancel that
    // wins the race writes nothing (AC-0209); through each repo's fence.
    for preflight in &preflights {
        runs.reconcile(&fence, &preflight.repo, &preflight.report, || {
            persist_preflight_findings(&state, &preflight.repo, &preflight.report)
        })?;
    }
    Ok(AddSystemSummary {
        job_id,
        repos,
        files,
        nodes: published.nodes(),
        edges: published.edges(),
        merged: published.merged,
        layers,
        delta,
        preflights,
        coverage,
        registries: RegistryReport::from(&registries),
    })
}

/// The resource/topology map artifact as Mermaid text (SPEC-00 §7, M2 exit
/// gate; channels join via observed BACKS edges at M6). Deterministic for
/// a given graph.
#[tauri::command]
fn export_topology(mode: spec::ExportMode, state: State<'_, AppState>) -> Result<String, String> {
    let (nodes, edges) = {
        let graph = state.graph.lock().map_err(|e| e.to_string())?;
        read_filtered_graph(
            &*graph,
            &["Resource", "Channel"],
            spec::TOPOLOGY_EDGE_LABELS,
        )?
    };
    let rejected = rejected_hashes(&state)?;
    Ok(spec::topology_mermaid_in(&nodes, &edges, mode, &rejected))
}

/// Content hashes of the inferences the Workbench rejected: the same set
/// `export_spec` withholds, for the standalone exports (#1755).
fn rejected_hashes(
    state: &State<'_, AppState>,
) -> Result<std::collections::BTreeSet<String>, String> {
    let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
    Ok(decisions
        .list_assertions()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|record| record.decision == agents::AssertionDecision::Rejected)
        .map(|record| record.assertion.provenance.content_hash)
        .collect())
}

/// The flow-dossier artifact as Markdown (SPEC-00 §7, M3 exit gate):
/// every T0-traceable flow with per-hop tiers, Gap truncation, and score.
#[tauri::command]
fn export_flows(mode: spec::ExportMode, state: State<'_, AppState>) -> Result<String, String> {
    let (nodes, edges) = read_flow_graph(&state.graph)?;
    let flows = flowtracer::trace(&nodes, &edges);
    let rejected = rejected_hashes(&state)?;
    Ok(spec::flow_dossier_in(&flows, mode, &rejected))
}

/// The traced flows as data (same graph slice as `export_flows`) — the UI
/// surfaces status and score per R-INT-2 without parsing the dossier.
#[tauri::command]
fn list_flows(state: State<'_, AppState>) -> Result<Vec<flowtracer::Flow>, String> {
    let (nodes, edges) = read_flow_graph(&state.graph)?;
    Ok(flowtracer::trace(&nodes, &edges))
}

/// The anchor kinds the tracer sought in the current graph, with counts —
/// a zero-flow Inspector names what recovery looked for (#165, R-INT-4).
#[tauri::command]
fn list_flow_anchors(state: State<'_, AppState>) -> Result<Vec<flowtracer::AnchorProbe>, String> {
    let (nodes, edges) = read_flow_graph(&state.graph)?;
    Ok(flowtracer::anchor_probes(&nodes, &edges))
}

/// Copy the flow selection before tracing or rendering.
fn read_flow_graph(graph: &Mutex<SqliteGraphStore>) -> Result<(Vec<Node>, Vec<Edge>), String> {
    let graph = graph.lock().map_err(|e| e.to_string())?;
    read_filtered_graph(
        &*graph,
        flowtracer::FLOW_NODE_LABELS,
        flowtracer::FLOW_EDGE_LABELS,
    )
}

/// Preserve the legacy sequence of label queries without mixing database
/// revisions. The stable label-rank sort happens after the read transaction;
/// within each label, nodes keep the snapshot's stable id order. Edge order
/// remains (src, dst, label), as it was for the original filtered edge query.
fn read_filtered_graph(
    graph: &impl GraphStore,
    node_labels: &[&str],
    edge_labels: &[&str],
) -> Result<(Vec<Node>, Vec<Edge>), String> {
    let (mut nodes, edges) = graph
        .read_snapshot_filtered(Some(node_labels), Some(edge_labels))
        .map_err(|e| e.to_string())?;
    nodes.sort_by_key(|node| {
        node_labels
            .iter()
            .position(|label| *label == node.label)
            .unwrap_or(usize::MAX)
    });
    Ok((nodes, edges))
}

#[derive(Serialize)]
struct SemanticPreview {
    embedding_cache: Option<semantic::embedding_cache::CacheStats>,
    eval_id: Option<i64>,
    provider: String,
    eval: semantic::EvalReport,
    calibration_available: bool,
    evaluation: semantic::evaluation::report::EvaluationSummary,
    proposals: Vec<semantic::SemanticProposal>,
    approved: Vec<semantic::SemanticProposal>,
    gaps_filled: usize,
    flows: Vec<flowtracer::Flow>,
    dossier: String,
}

fn build_semantic_preview(
    provider: &dyn LlmProvider,
    nodes: &[Node],
    edges: &[core_graph::Edge],
    eval_pairs: &[semantic::LabeledPair],
    precision_floor: f32,
) -> Result<SemanticPreview, String> {
    // Zero-valued legacy storage columns are explicitly unavailable, never a
    // synthetic evaluation. Independent admission does not require caller labels.
    let eval = if eval_pairs.is_empty() {
        semantic::EvalReport {
            precision_floor: 1.0,
            similarity_threshold: 1.0,
            precision: 0.0,
            recall: 0.0,
            passed: false,
            true_positives: 0,
            false_positives: 0,
            false_negatives: 0,
        }
    } else {
        semantic::evaluate(provider, eval_pairs, precision_floor)
            .map_err(|error| error.to_string())?
    };
    let (hops, candidates) = semantic::graph_inputs(nodes, edges);
    let proposals =
        semantic::propose(provider, &hops, &candidates, 3).map_err(|error| error.to_string())?;
    // Caller-supplied calibration pairs are never independent admission evidence.
    // The trusted independent path is separate; these candidates remain staged.
    let approved = Vec::new();
    let evaluation = calibration_summary(&proposals, &hops)?;
    let overlay = semantic::overlay(nodes, edges, &approved, &eval);
    let flows = flowtracer::trace(&overlay.nodes, &overlay.edges);
    let dossier = spec::flow_dossier(&flows);
    Ok(SemanticPreview {
        embedding_cache: None,
        eval_id: None,
        provider: provider.id().to_string(),
        calibration_available: !eval_pairs.is_empty(),
        eval,
        evaluation,
        proposals,
        approved,
        gaps_filled: overlay.gaps_filled,
        flows,
        dossier,
    })
}

fn calibration_summary(
    proposals: &[semantic::SemanticProposal],
    hops: &[semantic::UnresolvedHop],
) -> Result<semantic::evaluation::report::EvaluationSummary, String> {
    use semantic::evaluation::report::{
        DecisionReason, EvaluationSummary, FamilyDecision, FamilyStatus, SummaryMode,
    };
    let mut counts = std::collections::BTreeMap::new();
    for hop in hops.iter().filter(|hop| hop.is_ann_candidate()) {
        counts.entry(hop.family()).or_insert(0_u64);
    }
    for proposal in proposals {
        let count = counts.entry(proposal.family().clone()).or_insert(0_u64);
        *count = count
            .checked_add(1)
            .ok_or("calibration family count overflow")?;
    }
    let summary = EvaluationSummary {
        schema_version: 1,
        mode: SummaryMode::CalibrationOnly,
        families: counts
            .into_iter()
            .map(|(family, staged)| FamilyDecision {
                family,
                status: FamilyStatus::CalibrationOnly,
                reason: DecisionReason::CalibrationOnly,
                staged,
                admitted: 0,
                ambiguous: 0,
                receipt_digest: None,
                metrics: None,
            })
            .collect(),
    };
    summary.validate()?;
    Ok(summary)
}

#[cfg(test)]
fn build_cached_semantic_preview(
    provider: &dyn LlmProvider,
    cache: Arc<Mutex<semantic::embedding_cache::EmbeddingCache>>,
    lease: semantic::embedding_cache::Lease,
    nodes: &[Node],
    edges: &[Edge],
    eval_pairs: &[semantic::LabeledPair],
    precision_floor: f32,
) -> Result<SemanticPreview, String> {
    build_reviewed_semantic_preview(
        provider,
        cache,
        lease,
        nodes,
        edges,
        eval_pairs,
        precision_floor,
        None,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn build_reviewed_semantic_preview(
    provider: &dyn LlmProvider,
    cache: Arc<Mutex<semantic::embedding_cache::EmbeddingCache>>,
    lease: semantic::embedding_cache::Lease,
    nodes: &[Node],
    edges: &[Edge],
    eval_pairs: &[semantic::LabeledPair],
    precision_floor: f32,
    reviewed: Option<&semantic_evaluation::PreparedEvaluation>,
) -> Result<SemanticPreview, String> {
    build_reviewed_semantic_preview_within(
        provider,
        cache,
        lease,
        nodes,
        edges,
        eval_pairs,
        precision_floor,
        reviewed,
        semantic::embedding_cache::OperationLimits::default(),
    )
}

/// [`build_reviewed_semantic_preview`] holding at most `limits` in its one
/// embedding operation (#1653, AC-1807).
#[allow(clippy::too_many_arguments)]
fn build_reviewed_semantic_preview_within(
    provider: &dyn LlmProvider,
    cache: Arc<Mutex<semantic::embedding_cache::EmbeddingCache>>,
    lease: semantic::embedding_cache::Lease,
    nodes: &[Node],
    edges: &[Edge],
    eval_pairs: &[semantic::LabeledPair],
    precision_floor: f32,
    reviewed: Option<&semantic_evaluation::PreparedEvaluation>,
    limits: semantic::embedding_cache::OperationLimits,
) -> Result<SemanticPreview, String> {
    let cached = semantic::embedding_cache::CachedProvider::within(provider, cache, lease, limits)
        .map_err(|e| e.to_string())?;
    if !eval_pairs.is_empty()
        && (!(0.0..=1.0).contains(&precision_floor)
            || !eval_pairs.iter().any(|p| p.is_match)
            || !eval_pairs.iter().any(|p| !p.is_match))
    {
        return Err("invalid semantic evaluation inputs".into());
    }
    let (hops, candidates) = semantic::graph_inputs(nodes, edges);
    let mut inputs =
        semantic::embedding_inputs(&hops, &candidates, eval_pairs).map_err(|e| e.to_string())?;
    // The reviewed texts share the one embedding operation with the live plan.
    // When the live plan leaves no room for them, independent admission
    // abstains instead of failing the whole preview (AC-1509).
    let mut reviewed = reviewed;
    let mut over_plan = false;
    if let Some(prepared) = reviewed {
        let planned = prepared.planned_texts()?;
        if inputs.len().saturating_add(planned.len()) > cached.limits().max_rows() {
            reviewed = None;
            over_plan = true;
        } else {
            inputs.extend(planned);
        }
    }
    cached.embed(&inputs).map_err(|e| e.to_string())?;
    cached.seal().map_err(|e| e.to_string())?;
    let mut preview = build_semantic_preview(&cached, nodes, edges, eval_pairs, precision_floor)?;
    if let Some(reviewed) = reviewed {
        match reviewed.run(&cached, &hops, &candidates) {
            Ok(admission) => {
                let overlay = admission.overlay(nodes, edges);
                preview.flows = flowtracer::trace(&overlay.nodes, &overlay.edges);
                preview.dossier = spec::flow_dossier(&preview.flows);
                preview.gaps_filled = overlay.gaps_filled;
                let (staged, admitted, summary) = admission.into_parts();
                preview.proposals = staged;
                preview.approved = admitted;
                preview.evaluation = summary;
            }
            Err(_) => mark_independent_abstention(
                &mut preview,
                semantic::evaluation::report::DecisionReason::IncompatibleEvidence,
            ),
        }
    }
    if over_plan {
        mark_independent_abstention(
            &mut preview,
            semantic::evaluation::report::DecisionReason::IncompatibleEvidence,
        );
    }
    preview.embedding_cache = Some(cached.finish().map_err(|e| e.to_string())?);
    Ok::<_, String>(preview)
}

fn mark_independent_abstention(
    preview: &mut SemanticPreview,
    reason: semantic::evaluation::report::DecisionReason,
) {
    use semantic::evaluation::report::{FamilyStatus, SummaryMode};
    preview.evaluation.mode = SummaryMode::IndependentEvaluation;
    for family in &mut preview.evaluation.families {
        family.status = FamilyStatus::Abstained;
        family.reason = reason;
        family.admitted = 0;
    }
}

/// Run the local-only T2 preview. Only compatible host-reviewed independent
/// evidence can admit inferred Gap fills; caller calibration never authorizes them.
#[tauri::command]
async fn semantic_preview(
    app: tauri::AppHandle,
    eval_pairs: Vec<semantic::LabeledPair>,
    precision_floor: f32,
    state: State<'_, AppState>,
) -> Result<SemanticPreview, String> {
    // The Flow Tracer and Contract Matcher run while this is held (AC-1808).
    let _activity = pipeline_activity::SemanticPreviewActivity::begin(&state);
    let (nodes, edges, cache, lease) = {
        let graph = state.graph.lock().map_err(|error| error.to_string())?;
        let cache = state
            .embedding_cache
            .clone()
            .ok_or("embedding cache unavailable")?;
        let lease = cache
            .lock()
            .map_err(|e| e.to_string())?
            .bind_instance(&graph.instance_id().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        // Computed channel gaps can be backed only by a T0 IaC Resource.
        // Resources are semantic candidates, not flow nodes, and any Channel
        // they imply is materialized only in the ephemeral approved overlay.
        let mut node_labels = flowtracer::FLOW_NODE_LABELS.to_vec();
        node_labels.push("Resource");
        let (nodes, edges) =
            read_filtered_graph(&*graph, &node_labels, flowtracer::FLOW_EDGE_LABELS)?;
        (nodes, edges, cache, lease)
    };
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let worker_cache = Arc::clone(&cache);
    let worker_lease = lease.clone();
    let (mut preview, reviewed, unavailable) = tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let prepared = semantic_evaluation::prepare(&data_dir, &state, &nodes, &edges);
        let (reviewed, unavailable) = match prepared {
            Ok(Some(reviewed)) => (Some(reviewed), None),
            Ok(None) => (
                None,
                Some(semantic::evaluation::report::DecisionReason::MissingEvidence),
            ),
            Err(_) => (
                None,
                Some(semantic::evaluation::report::DecisionReason::IncompatibleEvidence),
            ),
        };
        let provider = llm::OllamaProvider::local_on_port(saved_ollama_port(&state)?)
            .map_err(|error| error.to_string())?;
        let preview = build_reviewed_semantic_preview_within(
            &provider,
            worker_cache,
            worker_lease,
            &nodes,
            &edges,
            &eval_pairs,
            precision_floor,
            reviewed.as_ref(),
            semantic_operation_limits(&state)?,
        )?;
        Ok::<_, String>((preview, reviewed, unavailable))
    })
    .await
    .map_err(|error| error.to_string())??;
    if let Some(reason) = unavailable {
        mark_independent_abstention(&mut preview, reason);
    }
    record_reviewed_semantic_preview(&state, &cache, &lease, preview, reviewed.as_ref())
}

#[cfg(test)]
fn record_semantic_preview(
    graph: &Mutex<SqliteGraphStore>,
    jobs: &Mutex<JobStore>,
    cache: &Arc<Mutex<semantic::embedding_cache::EmbeddingCache>>,
    lease: &semantic::embedding_cache::Lease,
    preview: SemanticPreview,
) -> Result<SemanticPreview, String> {
    // No provider work under this guard. Clear and forget use the same graph lock.
    let _graph = graph.lock().map_err(|e| e.to_string())?;
    persist_semantic_preview(jobs, cache, lease, preview)
}

fn record_reviewed_semantic_preview(
    state: &AppState,
    cache: &Arc<Mutex<semantic::embedding_cache::EmbeddingCache>>,
    lease: &semantic::embedding_cache::Lease,
    preview: SemanticPreview,
    reviewed: Option<&semantic_evaluation::PreparedEvaluation>,
) -> Result<SemanticPreview, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    if let Some(reviewed) = reviewed {
        reviewed.revalidate(state, &graph)?;
    }
    persist_semantic_preview(&state.jobs, cache, lease, preview)
}

fn persist_semantic_preview(
    jobs: &Mutex<JobStore>,
    cache: &Arc<Mutex<semantic::embedding_cache::EmbeddingCache>>,
    lease: &semantic::embedding_cache::Lease,
    mut preview: SemanticPreview,
) -> Result<SemanticPreview, String> {
    if cache
        .lock()
        .map_err(|e| e.to_string())?
        .lease()
        .map_err(|e| e.to_string())?
        != *lease
    {
        return Err("stale semantic preview generation".into());
    }
    let mut jobs = jobs.lock().map_err(|error| error.to_string())?;
    let eval = jobs
        .record_eval_with_summary(
            &preview.provider,
            &preview.eval,
            preview.proposals.len(),
            preview.approved.len(),
            &preview.evaluation,
        )
        .map_err(|error| error.to_string())?;
    preview.eval_id = Some(eval.id);
    Ok(preview)
}

/// The graph, traced flows, rejections and curated projection one spec
/// compilation reads. Facts and inventories come from one read transaction,
/// so the coverage artifacts describe the same revision as every other
/// artifact (AC-0592); the curated projection is computed over that same
/// snapshot (SPEC-12, AC-0653).
struct SpecInputs {
    /// The graph rows and the store revision they were read at.
    base: core_graph::RevisionedSnapshot,
    /// Overlay Capabilities and Actors current for `base` (#1443).
    knowledge: KnowledgeInput,
    coverage: Vec<core_graph::coverage::StoredCoverage>,
    flows: Vec<flowtracer::Flow>,
    rejected_hashes: std::collections::BTreeSet<String>,
    curated: CuratedInput,
    /// `None` only when the projection was not read.
    graph_basis: Option<spec::GraphBasis>,
    /// Display names of the registered repositories, by registry key
    /// (#2049). Empty when the registry was not read.
    repo_names: Vec<(String, String)>,
    /// The diagram view caps (#1653): the defaults unless the export reads
    /// the project's operator limits.
    view_caps: spec::SpecViewCaps,
}

/// The knowledge-layer facts `user-stories` renders beside the base
/// Capabilities (ADR-0051, #1443): the overlay's Capability and Actor nodes,
/// read only when the overlay is current for the compiled revision. Empty
/// when there is no overlay or it is stale, so the artifact keeps its
/// base-only bytes.
#[derive(Default)]
struct KnowledgeInput {
    capabilities: Vec<core_graph::overlay::OverlayFact>,
    actors: Vec<core_graph::overlay::OverlayFact>,
    /// Subsystem candidates the document set arranges Capabilities by (#260).
    subsystems: Vec<core_graph::overlay::OverlayFact>,
}

impl KnowledgeInput {
    /// The overlay's Capabilities and Actors, when it is current for
    /// `revision`. The caller holds no graph lock.
    fn read(
        overlay: Option<&Mutex<core_graph::overlay::OverlayStore>>,
        revision: &core_graph::StoreRevision,
    ) -> Result<Self, String> {
        use core_graph::overlay::{MAX_BYTES, MAX_FACTS, OverlayItem, OverlayRead};
        let Some(overlay) = overlay else {
            return Ok(Self::default());
        };
        let read = overlay
            .lock()
            .map_err(|error| error.to_string())?
            .read_story_nodes_bounded(revision, MAX_FACTS, MAX_BYTES)
            .map_err(|error| format!("The knowledge overlay could not be read: {error}"))?;
        let OverlayRead::Current { mut facts, .. } = read else {
            return Ok(Self::default());
        };
        let subsystems = facts
            .extract_if(
                ..,
                |f| matches!(&f.item, OverlayItem::Node(n) if n.label == "Subsystem"),
            )
            .collect();
        let actors = facts
            .extract_if(
                ..,
                |f| matches!(&f.item, OverlayItem::Node(n) if n.label == "Actor"),
            )
            .collect();
        Ok(Self {
            capabilities: facts,
            actors,
            subsystems,
        })
    }
}

/// The curated projection of one compilation (SPEC-12, AC-0653).
#[allow(
    clippy::large_enum_variant,
    reason = "one value per compilation or session; boxing would touch every reader"
)]
enum CuratedInput {
    /// A single artifact whose bytes do not depend on it.
    NotRead,
    /// It could not be computed. The export still succeeds: `curated.md`
    /// states the reason and no `curated_basis` is stamped.
    NotComputed(spec::CuratedFailure),
    Computed(context_hub::curated::CuratedProjection),
}

impl SpecInputs {
    /// The overlay Capabilities as story candidates, borrowed.
    fn candidates(&self) -> Result<Vec<spec::CapabilityStory<'_>>, String> {
        self.knowledge
            .capabilities
            .iter()
            .map(|fact| match &fact.item {
                core_graph::overlay::OverlayItem::Node(capability) => Ok(spec::CapabilityStory {
                    capability,
                    cites: &fact.cites,
                }),
                _ => Err("The knowledge overlay holds a Capability that is not a node.".into()),
            })
            .collect()
    }

    /// The knowledge stories validated against this compilation's graph
    /// under `mode`, or none when the overlay supplied nothing. An invalid
    /// overlay fact refuses the compilation, as the MCP read does.
    fn knowledge<'a>(
        &'a self,
        candidates: &[spec::CapabilityStory<'a>],
        mode: spec::ExportMode,
    ) -> Result<Option<spec::KnowledgeStories<'a>>, String> {
        if candidates.is_empty() && self.knowledge.actors.is_empty() {
            return Ok(None);
        }
        spec::validate_knowledge_stories(
            &self.base,
            candidates,
            &self.knowledge.actors,
            mode,
            &self.rejected_hashes,
        )
        .map(|stories| {
            // Arranged by subsystem area where the overlay holds them (#260).
            Some(stories.with_areas(
                &self.base,
                &self.knowledge.subsystems,
                mode,
                &self.rejected_hashes,
            ))
        })
        .map_err(|error| format!("The knowledge overlay was refused: {error}"))
    }

    fn supplied<'a>(
        &'a self,
        knowledge: Option<&'a spec::KnowledgeStories<'a>>,
    ) -> spec::Supplied<'a> {
        spec::Supplied {
            knowledge,
            coverage: &self.coverage,
            // The export compares no inventory with a current registration.
            stale_coverage: &[],
            curated: match &self.curated {
                CuratedInput::NotRead => spec::Curated::NotSupplied,
                CuratedInput::NotComputed(failure) => spec::Curated::NotComputed(*failure),
                CuratedInput::Computed(projection) => spec::Curated::Computed(projection),
            },
            // Known whenever the review store was read, whether or not the
            // projection could be computed.
            graph_content_hash: self
                .graph_basis
                .as_ref()
                .map(|basis| basis.content_hash.as_str()),
            source_revisions: self
                .graph_basis
                .as_ref()
                .map(|basis| basis.sources.as_slice()),
            repo_names: &self.repo_names,
        }
    }

    /// Stamp what the bundle was compiled from: the graph basis (AC-0593)
    /// and the curated basis beside it (AC-0653).
    fn stamp<A>(self, bundle: &mut spec::SpecBundle<A>) {
        bundle.curated_basis = match &self.curated {
            CuratedInput::Computed(projection) => Some(spec::CuratedBasis::of(projection)),
            CuratedInput::NotRead | CuratedInput::NotComputed(_) => None,
        };
        bundle.graph_basis = self.graph_basis;
    }
}

/// Everything one spec compilation reads from the stores. It is read under
/// the graph and decision locks; nothing derived from it needs them, so an
/// export command releases both before it traces, projects and compiles
/// (#1834).
struct SpecRead {
    /// The store revision of the transaction that read the rows below.
    revision: core_graph::StoreRevision,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    coverage: Vec<core_graph::coverage::StoredCoverage>,
    assertion_decisions: Vec<agents::AssertionDecisionRecord>,
    /// Read after the graph and decision locks are released (#1443).
    knowledge: KnowledgeInput,
    /// Read from the source registry after the graph lock is released
    /// (#2049).
    repo_names: Vec<(String, String)>,
}

#[cfg(test)]
thread_local! {
    static SPEC_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many snapshot preparations ([`read_spec`]) this thread made (#1662).
#[cfg(test)]
fn spec_reads() -> usize {
    SPEC_READS.with(std::cell::Cell::get)
}

fn read_spec(
    graph: &SqliteGraphStore,
    decisions: &agents::DecisionLog,
) -> Result<SpecRead, String> {
    #[cfg(test)]
    SPEC_READS.with(|count| count.set(count.get() + 1));
    // Each fact's `prov` and rule payload is held as text as it is read, so
    // the whole graph is never decoded at once (#2215).
    let (revision, ((nodes, edges), coverage)) = curated::read_graph_compacted(graph)?;
    let assertion_decisions = decisions
        .list_assertions()
        .map_err(|error| error.to_string())?;
    Ok(SpecRead {
        revision,
        nodes,
        edges,
        coverage,
        assertion_decisions,
        knowledge: KnowledgeInput::default(),
        repo_names: Vec::new(),
    })
}

/// [`read_spec`] under the application's graph and decision locks, both
/// released before the knowledge overlay is read for the same revision.
fn read_spec_locked(state: &AppState) -> Result<SpecRead, String> {
    let read = {
        let graph = state.graph.lock().map_err(|error| error.to_string())?;
        let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
        read_spec(&graph, &decisions)?
    };
    let mut read = read.with_knowledge(state.knowledge_overlay.as_deref())?;
    read.repo_names = repo_names_of(state, &read.nodes)?;
    Ok(read)
}

/// The display name of each repository the graph holds a `Repo` fact for,
/// as the source registry records it (#2049).
fn repo_names_of(state: &AppState, nodes: &[Node]) -> Result<Vec<(String, String)>, String> {
    let registry = state.sources.lock().map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    for node in nodes.iter().filter(|node| node.label == "Repo") {
        let Some(repo) = node.id.strip_prefix("repo:") else {
            continue;
        };
        if let Some(source) = registry.get_by_repo(repo)? {
            names.push((repo.to_string(), source.display_name));
        }
    }
    Ok(names)
}

impl SpecRead {
    /// This read with the overlay's Capabilities and Actors when the overlay
    /// is current for its revision (#1443). Hold no graph lock.
    fn with_knowledge(
        mut self,
        overlay: Option<&Mutex<core_graph::overlay::OverlayStore>>,
    ) -> Result<Self, String> {
        self.knowledge = KnowledgeInput::read(overlay, &self.revision)?;
        Ok(self)
    }

    /// The flow trace and, with a `review`, the curated projection of what
    /// was read. No store is read and no lock is needed.
    fn inputs(self, review: Option<&curated::CuratedReview>) -> SpecInputs {
        let Self {
            revision,
            nodes,
            edges,
            coverage,
            assertion_decisions,
            knowledge,
            repo_names,
        } = self;
        let flows = flowtracer::trace(&nodes, &edges);
        let rejected_hashes = assertion_decisions
            .iter()
            .filter(|record| record.decision == agents::AssertionDecision::Rejected)
            .map(|record| record.assertion.provenance.content_hash.clone())
            .collect();
        let base = core_graph::RevisionedSnapshot {
            revision,
            graph: (nodes, edges),
        };
        let (curated, graph_basis) = match review {
            Some(review) => {
                let (nodes, edges) = &base.graph;
                let graph_basis = graph_basis_of(nodes, edges);
                // The derived facts D1 prints, so the projection and the
                // document agree on which drafts resolve (#2040).
                let overlay = curated::overlay_nodes(
                    &base,
                    &knowledge.capabilities,
                    &knowledge.actors,
                    &rejected_hashes,
                );
                let projection = curated::project(
                    review,
                    &curated::Snapshot {
                        nodes,
                        edges,
                        overlay: &overlay,
                        derived_decisions: curated::DerivedDecisions::Compile(&flows),
                        coverage: &coverage,
                        content_hash: &graph_basis.content_hash,
                        revision: &base.revision,
                    },
                    &assertion_decisions,
                );
                let curated = match projection {
                    Ok(projection) => CuratedInput::Computed(projection),
                    Err(failure) => CuratedInput::NotComputed(failure),
                };
                (curated, Some(graph_basis))
            }
            None => (CuratedInput::NotRead, None),
        };
        SpecInputs {
            base,
            knowledge,
            coverage,
            flows,
            rejected_hashes,
            curated,
            graph_basis,
            repo_names,
            view_caps: spec::SpecViewCaps::default(),
        }
    }
}

/// The inputs of one compilation over stores the caller holds. The export
/// commands run the same two steps, `read_spec` then `SpecRead::inputs`,
/// and release the application's locks between them (`export_inputs`).
#[cfg(test)]
fn spec_inputs(
    graph: &SqliteGraphStore,
    decisions: &agents::DecisionLog,
    review: Option<&curated::CuratedReview>,
) -> Result<SpecInputs, String> {
    Ok(read_spec(graph, decisions)?.inputs(review))
}

/// `review` is what `curated::gather` read before the caller took the graph
/// and decision locks. `export_spec` over stores the caller holds.
#[cfg(test)]
fn build_spec_bundle(
    graph: &SqliteGraphStore,
    decisions: &agents::DecisionLog,
    review: &curated::CuratedReview,
    mode: spec::ExportMode,
) -> Result<spec::SpecBundle, String> {
    compile_spec_bundle(spec_inputs(graph, decisions, Some(review))?, mode)
}

#[cfg(test)]
fn compile_spec_bundle(
    inputs: SpecInputs,
    mode: spec::ExportMode,
) -> Result<spec::SpecBundle, String> {
    compile_spec_bundle_kept(inputs, mode, |artifact| artifact)
}

/// [`compile_spec_bundle`] keeping `keep(artifact)` of each artifact as it
/// is compiled, so the whole bundle is never held at once (#2215).
fn compile_spec_bundle_kept<A>(
    inputs: SpecInputs,
    mode: spec::ExportMode,
    keep: impl FnMut(spec::SpecArtifact) -> A,
) -> Result<spec::SpecBundle<A>, String> {
    let mut bundle = {
        let candidates = inputs.candidates()?;
        let knowledge = inputs.knowledge(&candidates, mode)?;
        // An export whose citations do not resolve is refused (ADR-0058).
        spec::try_compile_spec_supplied_kept(
            &inputs.base.graph.0,
            &inputs.base.graph.1,
            &inputs.flows,
            mode,
            &inputs.rejected_hashes,
            inputs.supplied(knowledge.as_ref()),
            keep,
        )
        .map_err(|error| error.to_string())?
    };
    inputs.stamp(&mut bundle);
    Ok(bundle)
}

/// One official artifact compiled alone through the selected compiler, which
/// produces the same bytes, content id and assertions as that artifact in
/// `build_spec_bundle` without building the others (#1582). `review` is
/// required for an artifact that `curated::artifact_reads_projection` names
/// and is not read for any other. `read_spec_artifact` over stores the
/// caller holds.
#[cfg(test)]
fn build_spec_artifact(
    graph: &SqliteGraphStore,
    decisions: &agents::DecisionLog,
    review: Option<&curated::CuratedReview>,
    mode: spec::ExportMode,
    artifact_id: &str,
) -> Result<spec::SpecArtifact, String> {
    let review = artifact_review(review, artifact_id)?;
    compile_spec_artifact(spec_inputs(graph, decisions, review)?, mode, artifact_id)
}

/// The review one artifact's compilation projects from: `review` for an
/// artifact that `curated::artifact_reads_projection` names, where it is
/// required, and none for any other.
fn artifact_review<'a>(
    review: Option<&'a curated::CuratedReview>,
    artifact_id: &str,
) -> Result<Option<&'a curated::CuratedReview>, String> {
    if curated::artifact_reads_projection(artifact_id) {
        Ok(Some(review.ok_or(
            "The curated projection was not read for this artifact.",
        )?))
    } else {
        Ok(None)
    }
}

fn compile_spec_artifact(
    inputs: SpecInputs,
    mode: spec::ExportMode,
    artifact_id: &str,
) -> Result<spec::SpecArtifact, String> {
    compile_spec_artifacts(inputs, mode, &[artifact_id])?
        .pop()
        .ok_or_else(|| format!("unknown spec artifact id: {artifact_id}"))
}

/// Several official artifacts compiled together through the selected
/// compiler from one set of inputs: one snapshot, flow trace and projection
/// (#1662, AC-0720). Each produced artifact has the same bytes, content id
/// and assertions as that artifact in `compile_spec_bundle`. The result
/// follows `artifact_ids` one for one; an unknown or repeated id, an
/// artifact that could not be compiled, or a result that is missing or not
/// requested refuses the whole read.
fn compile_spec_artifacts(
    inputs: SpecInputs,
    mode: spec::ExportMode,
    artifact_ids: &[&str],
) -> Result<Vec<spec::SpecArtifact>, String> {
    let mut requested = std::collections::BTreeSet::new();
    if let Some(repeated) = artifact_ids.iter().find(|id| !requested.insert(**id)) {
        return Err(format!("spec artifact {repeated} was requested twice"));
    }
    let unbounded = spec::SelectedCompileLimits {
        max_bytes: usize::MAX,
        adr: spec::AdrLimits {
            max_rows: usize::MAX,
            max_output_bytes: usize::MAX,
        },
    };
    // Only `user-stories` renders the overlay's stories, and the fact index
    // cites them; any other selection neither reads nor validates them, so a
    // refused overlay fact cannot fail an unrelated read. The D1 document
    // frames the derived Capabilities and caller roles too (AC-0663), and
    // the security view lists access per derived Capability (AC-0853), but
    // only as context: a refused overlay leaves them with their base sections.
    let requires_knowledge = artifact_ids
        .iter()
        .any(|id| matches!(*id, "user-stories" | "fact-index" | "fact-index-json"));
    let frames_knowledge = artifact_ids
        .iter()
        .any(|id| matches!(*id, "d1-product-specification" | "security-view"));
    let candidates = if requires_knowledge || frames_knowledge {
        match inputs.candidates() {
            Ok(candidates) => candidates,
            Err(_) if !requires_knowledge => Vec::new(),
            Err(error) => return Err(error),
        }
    } else {
        Vec::new()
    };
    let knowledge = if requires_knowledge {
        inputs.knowledge(&candidates, mode)?
    } else if frames_knowledge {
        inputs.knowledge(&candidates, mode).ok().flatten()
    } else {
        None
    };
    let outcomes = spec::compile_spec_selected_supplied(
        &inputs.base.graph.0,
        &inputs.base.graph.1,
        &inputs.flows,
        mode,
        &inputs.rejected_hashes,
        artifact_ids,
        unbounded,
        inputs.supplied(knowledge.as_ref()),
    )
    .map_err(|spec::ArtifactSelectionError::UnknownArtifact { index }| {
        format!("unknown spec artifact id: {}", artifact_ids[index])
    })?;
    let mut produced = std::collections::BTreeMap::new();
    for outcome in outcomes {
        match outcome {
            spec::SelectedArtifactOutcome::Produced(artifact) => {
                if !requested.contains(artifact.id.as_str()) {
                    return Err(format!(
                        "spec artifact {} was compiled but not requested",
                        artifact.id
                    ));
                }
                if let Some(artifact) = produced.insert(artifact.id.clone(), artifact) {
                    return Err(format!("spec artifact {} was compiled twice", artifact.id));
                }
            }
            spec::SelectedArtifactOutcome::Limited { artifact_id, limit } => {
                return Err(format!(
                    "spec artifact {artifact_id} could not be compiled: {limit:?}"
                ));
            }
        }
    }
    artifact_ids
        .iter()
        .map(|id| {
            produced
                .remove(*id)
                .ok_or_else(|| format!("unknown spec artifact id: {id}"))
        })
        .collect()
}

/// `read_spec_artifacts` over stores the caller holds (#1662).
#[cfg(test)]
fn build_spec_artifacts(
    graph: &SqliteGraphStore,
    decisions: &agents::DecisionLog,
    review: Option<&curated::CuratedReview>,
    mode: spec::ExportMode,
    artifact_ids: &[&str],
) -> Result<Vec<spec::SpecArtifact>, String> {
    let review = artifacts_review(review, artifact_ids)?;
    compile_spec_artifacts(spec_inputs(graph, decisions, review)?, mode, artifact_ids)
}

/// [`artifact_review`] for several artifacts compiled together: `review`
/// when any of them reads the projection. An artifact that does not read it
/// has the same bytes either way.
fn artifacts_review<'a>(
    review: Option<&'a curated::CuratedReview>,
    artifact_ids: &[&str],
) -> Result<Option<&'a curated::CuratedReview>, String> {
    match artifact_ids
        .iter()
        .find(|id| curated::artifact_reads_projection(id))
    {
        Some(id) => artifact_review(review, id),
        None => Ok(None),
    }
}

#[cfg(test)]
fn build_spec_with_views(
    graph: &SqliteGraphStore,
    decisions: &agents::DecisionLog,
    review: &curated::CuratedReview,
    mode: spec::ExportMode,
) -> Result<(spec::SpecBundle, spec::SpecViewBundle), String> {
    let (bundle, views, _) = build_spec_with_views_and_curated(graph, decisions, review, mode)?;
    Ok((bundle, views))
}

/// `build_spec_with_views` with the Workbench's curated read (SPEC-12 slice
/// 3, AC-0655), made from the projection this compilation computed: the one
/// the bundle's `curated_basis` is stamped from. `export_spec_with_views`
/// over stores the caller holds.
#[cfg(test)]
fn build_spec_with_views_and_curated(
    graph: &SqliteGraphStore,
    decisions: &agents::DecisionLog,
    review: &curated::CuratedReview,
    mode: spec::ExportMode,
) -> Result<
    (
        spec::SpecBundle,
        spec::SpecViewBundle,
        curated_workbench::CuratedWorkbenchRead,
    ),
    String,
> {
    compile_spec_with_views_and_curated(spec_inputs(graph, decisions, Some(review))?, mode, |a| a)
}

/// The bundle, its views and the curated read from one compilation, keeping
/// `keep(artifact)` of each artifact as it is compiled (#2215).
fn compile_spec_with_views_and_curated<A>(
    inputs: SpecInputs,
    mode: spec::ExportMode,
    keep: impl FnMut(spec::SpecArtifact) -> A,
) -> Result<
    (
        spec::SpecBundle<A>,
        spec::SpecViewBundle,
        curated_workbench::CuratedWorkbenchRead,
    ),
    String,
> {
    let curated = curated_workbench::CuratedWorkbenchRead::of(&inputs.curated, mode)
        .ok_or("The curated projection was not read.")?;
    let (mut bundle, views) = {
        let candidates = inputs.candidates()?;
        let knowledge = inputs.knowledge(&candidates, mode)?;
        // An export whose citations do not resolve is refused (ADR-0058).
        spec::try_compile_spec_with_views_supplied_kept_capped(
            &inputs.base.graph.0,
            &inputs.base.graph.1,
            &inputs.flows,
            mode,
            &inputs.rejected_hashes,
            inputs.supplied(knowledge.as_ref()),
            keep,
            inputs.view_caps,
        )
        .map_err(|error| error.to_string())?
    };
    inputs.stamp(&mut bundle);
    Ok((bundle, views, curated))
}

#[cfg(test)]
fn build_spec_views(
    graph: &SqliteGraphStore,
    decisions: &agents::DecisionLog,
    review: &curated::CuratedReview,
    mode: spec::ExportMode,
) -> Result<spec::SpecViewBundle, String> {
    Ok(build_spec_with_views(graph, decisions, review, mode)?.1)
}

/// Per-artifact content cap applied before a spec bundle crosses IPC (#488):
/// full artifacts were seen up to 605 MB serialized. The complete text stays
/// reachable one artifact at a time via `read_spec_artifact`.
const SPEC_ARTIFACT_CONTENT_PREVIEW_BYTES: usize = 512 * 1024;
/// Per-artifact assertion-count cap applied before a spec bundle crosses IPC
/// (#488) — structured `assertions` (with full inline provenance/evidence)
/// made up most of the oversized payload. The complete list stays reachable
/// via `read_spec_artifact`.
const SPEC_ARTIFACT_ASSERTIONS_PREVIEW_COUNT: usize = 200;

/// Truncates `content` in place to at most `max_bytes`, respecting UTF-8
/// char boundaries, and reports whether truncation happened (#488).
fn truncate_utf8_in_place(content: &mut String, max_bytes: usize) -> bool {
    if content.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    content.truncate(end);
    true
}

/// One official spec artifact capped to `SPEC_ARTIFACT_CONTENT_PREVIEW_BYTES`
/// / `SPEC_ARTIFACT_ASSERTIONS_PREVIEW_COUNT` for the IPC boundary (#488).
/// The Workbench renders this preview and fetches the full artifact (content
/// and assertions) via `read_spec_artifact` for copy/export.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(PartialEq, serde::Deserialize))]
struct SpecArtifactPreview {
    id: String,
    file_name: String,
    title: String,
    format: String,
    /// Full artifact's schema and content identity (AC-0242), unchanged by
    /// the preview cap.
    schema: String,
    content_id: String,
    content: String,
    content_truncated: bool,
    content_byte_len: usize,
    assertions: Vec<spec::SpecAssertion>,
    assertions_truncated: bool,
    assertions_total: usize,
}

/// `spec::SpecBundle` with every artifact capped for IPC (#488).
#[derive(Debug, Serialize)]
struct SpecBundlePreview {
    mode: spec::ExportMode,
    artifacts: Vec<SpecArtifactPreview>,
    assertion_count: usize,
    gap_count: usize,
    drift_count: usize,
    security_count: usize,
    /// The graph the bundle was compiled from (#1765, AC-0593).
    #[serde(skip_serializing_if = "Option::is_none")]
    graph_basis: Option<spec::GraphBasis>,
    /// The curated revision and counts it was compiled with (AC-0653).
    #[serde(skip_serializing_if = "Option::is_none")]
    curated_basis: Option<spec::CuratedBasis>,
}

#[cfg(test)]
fn preview_spec_bundle(bundle: spec::SpecBundle) -> SpecBundlePreview {
    bundle_preview(spec::SpecBundle {
        mode: bundle.mode,
        artifacts: bundle.artifacts.into_iter().map(preview_artifact).collect(),
        assertion_count: bundle.assertion_count,
        gap_count: bundle.gap_count,
        drift_count: bundle.drift_count,
        security_count: bundle.security_count,
        graph_basis: bundle.graph_basis,
        curated_basis: bundle.curated_basis,
    })
}

/// One artifact capped before it crosses IPC (#488), keeping its true
/// length and assertion count. The export commands cap each artifact as it
/// is compiled, so the complete bundle is never held (#2215).
fn preview_artifact(artifact: spec::SpecArtifact) -> SpecArtifactPreview {
    let content_byte_len = artifact.content.len();
    let mut content = artifact.content;
    let content_truncated =
        truncate_utf8_in_place(&mut content, SPEC_ARTIFACT_CONTENT_PREVIEW_BYTES);
    content.shrink_to_fit();
    let assertions_total = artifact.assertions.len();
    let assertions_truncated = assertions_total > SPEC_ARTIFACT_ASSERTIONS_PREVIEW_COUNT;
    let mut assertions = artifact.assertions;
    assertions.truncate(SPEC_ARTIFACT_ASSERTIONS_PREVIEW_COUNT);
    assertions.shrink_to_fit();
    SpecArtifactPreview {
        id: artifact.id,
        file_name: artifact.file_name,
        title: artifact.title,
        format: artifact.format,
        schema: artifact.schema,
        content_id: artifact.content_id,
        content,
        content_truncated,
        content_byte_len,
        assertions,
        assertions_truncated,
        assertions_total,
    }
}

fn bundle_preview(bundle: spec::SpecBundle<SpecArtifactPreview>) -> SpecBundlePreview {
    let graph_basis = bundle.graph_basis;
    let curated_basis = bundle.curated_basis;
    SpecBundlePreview {
        mode: bundle.mode,
        artifacts: bundle.artifacts,
        assertion_count: bundle.assertion_count,
        gap_count: bundle.gap_count,
        drift_count: bundle.drift_count,
        security_count: bundle.security_count,
        graph_basis,
        curated_basis,
    }
}

/// `SpecBundlePreview` paired with `spec::SpecViewBundle` from a single
/// projection pass (US-0036, AC-0246, AC-0247).
#[derive(Debug, Serialize)]
struct SpecWithViewsPreview {
    bundle: SpecBundlePreview,
    views: spec::SpecViewBundle,
    /// The curated projection `bundle.curated_basis` was stamped from, as
    /// the Workbench shows it (SPEC-12 slice 3, AC-0655).
    curated: curated_workbench::CuratedWorkbenchRead,
}

/// Compile the full official spec set under one R-INT-5 export policy,
/// capped per artifact before it crosses IPC (#488).
///
/// Like every export command below it runs on a blocking worker, never on
/// the main thread (#1834, AC-0659): one export assesses up to
/// `curated::MAX_CURATED_ACCEPTED` records and compiles every document.
#[tauri::command]
async fn export_spec<R: tauri::Runtime>(
    mode: spec::ExportMode,
    app: tauri::AppHandle<R>,
) -> Result<SpecBundlePreview, String> {
    off_ui_thread(move || export_spec_blocking(mode, &app.state::<AppState>())).await
}

/// The three steps of every export. The review first, with no graph or
/// decision lock held: the assessments take the graph lock themselves
/// (SPEC-12). Then the snapshot and the decisions under both locks. Then
/// the trace, the projection and the compile with both released, so a
/// command that needs the graph meanwhile waits for the read alone.
fn export_inputs(state: &AppState) -> Result<SpecInputs, String> {
    let review = curated::gather(state);
    let mut inputs = read_spec_locked(state)?.inputs(Some(&review));
    inputs.view_caps = spec_view_caps(state)?;
    Ok(inputs)
}

/// Directory entries one TS/JS selection walk may examine (AC-1802),
/// within the adapter's hard ceiling.
fn selection_visited_entries(state: &AppState) -> Result<usize, String> {
    let settings = state.settings.lock().map_err(|error| error.to_string())?;
    let entries = settings
        .operator_limit(operator_limits::TS_SELECTION_MAX_VISITED_ENTRIES)
        .map_err(|error| error.to_string())?;
    usize::try_from(entries).map_err(|error| error.to_string())
}

/// The project's bounds on the inputs one recovery reads (#1653): declared
/// API references (AC-1804) and Python's auxiliary inputs (AC-1805). One
/// operation reads them once and every repository it recovers uses them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RecoveryLimits {
    api_docs: ingest::api_docs::ApiDocLimits,
    python: adapters_lang_python::PythonLimits,
}

fn recovery_limits(state: &AppState) -> Result<RecoveryLimits, String> {
    Ok(RecoveryLimits {
        api_docs: api_doc_limits(state)?,
        python: python_limits(state)?,
    })
}

/// Alembic revision files and size, and lockfile size, from the saved
/// operator limits (#1653, AC-1805), within the adapter's hard ceilings.
fn python_limits(state: &AppState) -> Result<adapters_lang_python::PythonLimits, String> {
    let settings = state.settings.lock().map_err(|error| error.to_string())?;
    let limit = |key| {
        settings
            .operator_limit(key)
            .map_err(|error| error.to_string())
    };
    let mib = |key| {
        limit(key)?
            .checked_mul(1024 * 1024)
            .ok_or_else(|| "Python limit overflows.".to_string())
    };
    let revisions = usize::try_from(limit(operator_limits::PYTHON_ALEMBIC_MAX_REVISIONS)?)
        .map_err(|error| error.to_string())?;
    adapters_lang_python::PythonLimits::within(
        revisions,
        mib(operator_limits::PYTHON_ALEMBIC_MAX_FILE_MIB)?,
        mib(operator_limits::PYTHON_LOCKFILE_MAX_MIB)?,
    )
    .ok_or_else(|| "Saved Python limits are outside the adapter's ceilings.".to_string())
}

/// Rows and vector MiB one semantic preview's embedding operation may hold
/// (#1653, AC-1807), from the saved operator limits.
fn semantic_operation_limits(
    state: &AppState,
) -> Result<semantic::embedding_cache::OperationLimits, String> {
    let settings = state.settings.lock().map_err(|error| error.to_string())?;
    let limit = |key| {
        settings
            .operator_limit(key)
            .map_err(|error| error.to_string())
    };
    let rows = usize::try_from(limit(operator_limits::SEMANTIC_OPERATION_MAX_ROWS)?)
        .map_err(|error| error.to_string())?;
    let bytes = limit(operator_limits::SEMANTIC_OPERATION_MAX_VECTOR_MIB)?
        .checked_mul(1024 * 1024)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or("Semantic vector bound overflows.")?;
    semantic::embedding_cache::OperationLimits::within(rows, bytes)
        .ok_or_else(|| "Saved semantic limits are outside the cache's bounds.".to_string())
}

/// How many API references a repository may declare and how large each may
/// be (#1653, AC-1804), from the saved operator limits. One operation reads
/// them once, so its recovery and its documentation re-read agree.
fn api_doc_limits(state: &AppState) -> Result<ingest::api_docs::ApiDocLimits, String> {
    let settings = state.settings.lock().map_err(|error| error.to_string())?;
    let limit = |key| {
        settings
            .operator_limit(key)
            .map_err(|error| error.to_string())
    };
    let docs = usize::try_from(limit(operator_limits::API_DOCS_MAX_DOCS)?)
        .map_err(|error| error.to_string())?;
    let bytes = limit(operator_limits::API_DOCS_MAX_MIB)?
        .checked_mul(1024 * 1024)
        .ok_or("API doc size overflows.")?;
    ingest::api_docs::ApiDocLimits::within(docs, bytes)
        .ok_or_else(|| "Saved API doc limits are outside the loader's ceilings.".to_string())
}

/// The project's source capture limits (#1653, AC-1800): the saved file
/// count and MiB, within the capture crate's hard ceilings. A stored value
/// that no longer validates is an error, not the default.
fn capture_limits(state: &AppState) -> Result<source_capture::CaptureLimits, String> {
    let settings = state.settings.lock().map_err(|error| error.to_string())?;
    let limit = |key| {
        settings
            .operator_limit(key)
            .map_err(|error| error.to_string())
    };
    let files = limit(operator_limits::SOURCE_CAPTURE_MAX_FILES)?;
    let mib = limit(operator_limits::SOURCE_CAPTURE_MAX_MIB)?;
    let limits = source_capture::CaptureLimits {
        max_files: usize::try_from(files).map_err(|error| error.to_string())?,
        max_total_bytes: mib
            .checked_mul(1024 * 1024)
            .ok_or("Source capture MiB overflows.")?,
        ..source_capture::CaptureLimits::default()
    };
    limits.validate().map_err(|error| error.to_string())?;
    Ok(limits)
}

/// The project's diagram view caps (#1653). A stored cap that no longer
/// validates is an error, not the default.
fn spec_view_caps(state: &AppState) -> Result<spec::SpecViewCaps, String> {
    let settings = state.settings.lock().map_err(|error| error.to_string())?;
    let cap = |key| {
        settings
            .operator_limit(key)
            .map_err(|error| error.to_string())
            .and_then(|value| usize::try_from(value).map_err(|error| error.to_string()))
    };
    Ok(spec::SpecViewCaps {
        nodes: cap(operator_limits::SPEC_VIEW_MAX_NODES)?,
        edges: cap(operator_limits::SPEC_VIEW_MAX_EDGES)?,
        flows: cap(operator_limits::SPEC_VIEW_MAX_FLOWS)?,
    })
}

fn export_spec_blocking(
    mode: spec::ExportMode,
    state: &AppState,
) -> Result<SpecBundlePreview, String> {
    compile_spec_bundle_kept(export_inputs(state)?, mode, preview_artifact).map(bundle_preview)
}

/// Compile structured diagram views (`topology`, `flow-dossiers`, `data-model`)
/// bound to the same projection and artifact content identities as `export_spec`
/// (US-0036, AC-0246, AC-0247).
#[tauri::command]
async fn export_spec_views<R: tauri::Runtime>(
    mode: spec::ExportMode,
    app: tauri::AppHandle<R>,
) -> Result<spec::SpecViewBundle, String> {
    off_ui_thread(move || export_spec_views_blocking(mode, &app.state::<AppState>())).await
}

fn export_spec_views_blocking(
    mode: spec::ExportMode,
    state: &AppState,
) -> Result<spec::SpecViewBundle, String> {
    // Nothing of an artifact is kept: the views carry its identity (#2215).
    Ok(compile_spec_with_views_and_curated(export_inputs(state)?, mode, |_| ())?.1)
}

/// Compile the capped official spec bundle and its structured diagram views
/// from one snapshot read, flow trace, and projection pass (US-0036, AC-0246,
/// AC-0247).
#[tauri::command]
async fn export_spec_with_views<R: tauri::Runtime>(
    mode: spec::ExportMode,
    app: tauri::AppHandle<R>,
) -> Result<SpecWithViewsPreview, String> {
    off_ui_thread(move || export_spec_with_views_blocking(mode, &app.state::<AppState>())).await
}

fn export_spec_with_views_blocking(
    mode: spec::ExportMode,
    state: &AppState,
) -> Result<SpecWithViewsPreview, String> {
    let (bundle, views, curated) =
        compile_spec_with_views_and_curated(export_inputs(state)?, mode, preview_artifact)?;
    Ok(SpecWithViewsPreview {
        bundle: bundle_preview(bundle),
        views,
        curated,
    })
}

/// `spec::SpecArtifact` reported as never truncated, so `read_spec_artifact`
/// returns the exact same wire shape `export_spec` does (#488) — callers
/// never have to special-case a "full" artifact missing these fields.
fn full_artifact_preview(artifact: spec::SpecArtifact) -> SpecArtifactPreview {
    let content_byte_len = artifact.content.len();
    let assertions_total = artifact.assertions.len();
    SpecArtifactPreview {
        id: artifact.id,
        file_name: artifact.file_name,
        title: artifact.title,
        format: artifact.format,
        schema: artifact.schema,
        content_id: artifact.content_id,
        content: artifact.content,
        content_truncated: false,
        content_byte_len,
        assertions: artifact.assertions,
        assertions_truncated: false,
        assertions_total,
    }
}

/// One official spec artifact's complete content and assertions, fetched on
/// demand for copy/export after `export_spec`'s IPC-capped preview (#488).
#[tauri::command]
async fn read_spec_artifact<R: tauri::Runtime>(
    mode: spec::ExportMode,
    artifact_id: String,
    app: tauri::AppHandle<R>,
) -> Result<SpecArtifactPreview, String> {
    off_ui_thread(move || read_spec_artifact_blocking(mode, &artifact_id, &app.state::<AppState>()))
        .await
}

fn read_spec_artifact_blocking(
    mode: spec::ExportMode,
    artifact_id: &str,
    state: &AppState,
) -> Result<SpecArtifactPreview, String> {
    // Read only for an artifact whose bytes depend on the projection, and
    // before the graph lock: the assessments take it themselves (SPEC-12).
    let review = curated::artifact_reads_projection(artifact_id).then(|| curated::gather(state));
    let review = artifact_review(review.as_ref(), artifact_id)?;
    let inputs = read_spec_locked(state)?.inputs(review);
    compile_spec_artifact(inputs, mode, artifact_id).map(full_artifact_preview)
}

/// Several official spec artifacts' complete content and assertions,
/// fetched together for Export bundle after `export_spec`'s IPC-capped
/// preview (#1662, AC-0720): one review read, one snapshot under the graph
/// lock and one selected compilation, however many artifacts were capped.
/// The result follows `artifact_ids` one for one.
#[tauri::command]
async fn read_spec_artifacts<R: tauri::Runtime>(
    mode: spec::ExportMode,
    artifact_ids: Vec<String>,
    app: tauri::AppHandle<R>,
) -> Result<Vec<SpecArtifactPreview>, String> {
    off_ui_thread(move || {
        read_spec_artifacts_blocking(mode, &artifact_ids, &app.state::<AppState>())
    })
    .await
}

fn read_spec_artifacts_blocking(
    mode: spec::ExportMode,
    artifact_ids: &[String],
    state: &AppState,
) -> Result<Vec<SpecArtifactPreview>, String> {
    let artifact_ids: Vec<&str> = artifact_ids.iter().map(String::as_str).collect();
    if artifact_ids.is_empty() {
        return Ok(Vec::new());
    }
    // As `read_spec_artifact`: the review only when an artifact's bytes
    // depend on the projection, and before the graph lock.
    let review = artifact_ids
        .iter()
        .any(|id| curated::artifact_reads_projection(id))
        .then(|| curated::gather(state));
    let review = artifacts_review(review.as_ref(), &artifact_ids)?;
    let inputs = read_spec_locked(state)?.inputs(review);
    Ok(compile_spec_artifacts(inputs, mode, &artifact_ids)?
        .into_iter()
        .map(full_artifact_preview)
        .collect())
}

/// Selected flow diagram, bound to the preview's full dossier identity (#1669).
#[tauri::command]
async fn read_selected_flow_view(
    mode: spec::ExportMode,
    trigger: String,
    content_id: String,
    app: tauri::AppHandle,
) -> Result<spec::SpecDiagramView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let graph = state.graph.lock().map_err(|error| error.to_string())?;
        let decisions = state.decisions.lock().map_err(|error| error.to_string())?;
        let (nodes, edges) = graph.read_snapshot().map_err(|error| error.to_string())?;
        let flows = flowtracer::trace(&nodes, &edges);
        let rejected = decisions
            .list_assertions()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|record| record.decision == agents::AssertionDecision::Rejected)
            .map(|record| record.assertion.provenance.content_hash)
            .collect();
        spec::compile_selected_flow_view_capped(
            &nodes,
            &edges,
            &flows,
            mode,
            &rejected,
            &trigger,
            &content_id,
            spec_view_caps(&state)?,
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Nodes carrying `label` (e.g. `Endpoint`, `Repo`), ordered by id.
#[tauri::command]
fn list_nodes(label: String, state: State<'_, AppState>) -> Result<Vec<Node>, String> {
    let graph = state.graph.lock().map_err(|e| e.to_string())?;
    graph.nodes_with_label(&label).map_err(|e| e.to_string())
}

/// Complete, deterministically ordered graph projection for the read-only
/// Atlas surface. Provenance remains attached to every returned fact.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct AtlasSnapshot {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

fn build_atlas_snapshot(graph: &impl GraphStore) -> Result<AtlasSnapshot, String> {
    let (nodes, edges) = graph.read_snapshot().map_err(|error| error.to_string())?;
    Ok(AtlasSnapshot { nodes, edges })
}

#[tauri::command]
fn atlas_snapshot(state: State<'_, AppState>) -> Result<AtlasSnapshot, String> {
    let graph = state.graph.lock().map_err(|error| error.to_string())?;
    build_atlas_snapshot(&*graph)
}

#[derive(Serialize)]
struct EvidenceSource {
    text: String,
    window_start: u64,
    window_start_line: u64,
    truncated: bool,
}

/// Read-only source window containing an evidence span, confined to the
/// exact registered repository root (NG1: navigation, never edit).
#[tauri::command]
fn read_evidence(
    repo: String,
    path: String,
    byte_start: u64,
    byte_end: u64,
    state: State<'_, AppState>,
) -> Result<EvidenceSource, String> {
    let window = source_access::with_registered_read(&state.sources, &repo, |root| {
        evidence::read_source(root, &path, &(byte_start..byte_end)).map_err(|e| e.to_string())
    })?;
    Ok(EvidenceSource {
        text: window.text,
        window_start: window.window_start,
        window_start_line: window.window_start_line,
        truncated: window.truncated,
    })
}

/// Where the repository lives (#1760). Every link the shell opens is built
/// from this, so a move is a one-line change. The UI holds the same value in
/// `ui/src/helpNotes.ts` (`REPOSITORY_URL`).
const REPOSITORY_URL: &str = "https://github.com/true-line/cartograph";

/// Open a URL in the system browser — never inside the webview (#154).
fn open_external(url: &str) {
    let Some((program, args)) = url_launcher(std::env::consts::OS, url) else {
        eprintln!(
            "cartograph: no system browser launcher for {}; not opening {url}",
            std::env::consts::OS
        );
        return;
    };
    launch(program, args, url);
}

/// Help → Debug Viewer (#1624): open the model debug log in the platform's
/// plain text editor, creating it with its header first if needed.
fn open_model_debug_log(app: &tauri::AppHandle) {
    let Some(log) = app.try_state::<Arc<model_debug::ModelDebugLog>>() else {
        return;
    };
    if let Err(error) = log.ensure_exists() {
        eprintln!("cartograph: could not create the model debug log: {error}");
        return;
    }
    let path = log.path().to_path_buf();
    match model_debug::text_viewer(std::env::consts::OS, &path) {
        Some((program, args)) => launch(program, args, &path.to_string_lossy()),
        None => eprintln!(
            "cartograph: no text viewer for {}; the log is at {}",
            std::env::consts::OS,
            path.display()
        ),
    }
}

fn launch(program: &str, args: Vec<String>, url: &str) {
    let mut command = std::process::Command::new(program);
    command.args(args);
    // A GUI app spawning a console program flashes a console window on
    // Windows unless the child is created without one (#226).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    if let Err(error) = command.spawn() {
        eprintln!("cartograph: could not launch {program} for {url}: {error}");
    }
}

/// The program and arguments that hand `url` to the system browser on `os`
/// (`std::env::consts::OS`), or `None` where no launcher is known (AC-0222).
///
/// Windows uses the URL protocol handler rather than `explorer`, which cuts a
/// URL at its first `&` and exits nonzero even on success. `rundll32` gets
/// the URL as one argument and no shell parses it, so `&` needs no quoting
/// (unlike `cmd /C start`).
fn url_launcher(os: &str, url: &str) -> Option<(&'static str, Vec<String>)> {
    match os {
        "macos" => Some(("open", vec![url.to_string()])),
        "linux" | "freebsd" | "netbsd" | "openbsd" | "dragonfly" => {
            Some(("xdg-open", vec![url.to_string()]))
        }
        "windows" => Some((
            "rundll32",
            vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()],
        )),
        _ => None,
    }
}

/// Native Help submenu (#154): in-app Help, the wiki user guide, issue
/// reporting, and About — appended to the platform-default menu so the
/// standard Edit/Window items survive.
/// Show the Configure window (#1633), creating it on first use.
fn open_configure_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("configure") {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }
    let built = tauri::WebviewWindowBuilder::new(
        app,
        "configure",
        tauri::WebviewUrl::App("index.html?view=configure".into()),
    )
    .title("Configure — Run Limits")
    .inner_size(520.0, 420.0)
    .resizable(true)
    .build();
    if let Err(error) = built {
        eprintln!("configure window: {error}");
    }
}

fn install_help_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
    let handle = app.handle();
    let help = Submenu::with_items(
        handle,
        "Help",
        true,
        &[
            &MenuItem::with_id(handle, "help:open", "Cartograph Help", true, None::<&str>)?,
            &MenuItem::with_id(
                handle,
                "help:guide",
                "User guide (wiki)",
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(handle, "help:issue", "Report an issue", true, None::<&str>)?,
            &MenuItem::with_id(handle, "help:debug", "Debug Viewer", true, None::<&str>)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::about(
                handle,
                Some("About Cartograph"),
                Some(AboutMetadata {
                    name: Some("Cartograph".into()),
                    version: Some(app.package_info().version.to_string()),
                    ..Default::default()
                }),
            )?,
        ],
    )?;
    let menu = Menu::default(handle)?;
    // The platform default may already ship a (near-empty) Help submenu —
    // replace it rather than presenting two Help menus (#195 review).
    for item in menu.items()? {
        let Some(existing) = item.as_submenu() else {
            continue;
        };
        if existing.text()? == "Help" {
            menu.remove(&item)?;
        }
    }
    // #1633: tunable run parameters live in their own window, apart from
    // the app Settings page.
    let configure = Submenu::with_items(
        handle,
        "Configure",
        true,
        &[&MenuItem::with_id(
            handle,
            "configure:run-limits",
            "Run Limits…",
            true,
            None::<&str>,
        )?],
    )?;
    menu.append(&configure)?;
    menu.append(&help)?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| match event.id().0.as_str() {
        "help:open" => {
            let _ = app.emit("help://open", ());
        }
        "help:guide" => open_external(&format!("{REPOSITORY_URL}/wiki")),
        "help:issue" => open_external(&format!("{REPOSITORY_URL}/issues/new")),
        "help:debug" => open_model_debug_log(app),
        "configure:run-limits" => open_configure_window(app),
        _ => {}
    });
    Ok(())
}

/// Every store the application keeps under `data_dir`, opened and
/// recovered as one launch does. A pending project change (#1994) is made
/// first, before any store is open.
fn open_app_state(data_dir: &std::path::Path) -> Result<AppState, Box<dyn std::error::Error>> {
    let data_dir = data_dir.to_path_buf();
    projects::apply(&data_dir, |state_path| {
        settings::SettingsStore::open(state_path).map_err(|e| e.to_string())?;
        JobStore::open(state_path).map_err(|e| e.to_string())?;
        Ok(())
    })?;
    let embedding_cache = Arc::new(Mutex::new(
        semantic::embedding_cache::EmbeddingCache::open_disposable(&data_dir)?,
    ));
    // ADR-0051 knowledge overlay (#1440): derived facts, discarded
    // with the cached vectors before any destructive graph lifecycle.
    let knowledge_overlay = Arc::new(Mutex::new(
        core_graph::overlay::OverlayStore::open_disposable(&data_dir)
            .map_err(std::io::Error::other)?,
    ));
    let invalidator = Arc::clone(&embedding_cache);
    let overlay_invalidator = Arc::clone(&knowledge_overlay);
    let graph = SqliteGraphStore::open_with_reset_hook(
        data_dir.join("graph.db"),
        Box::new(move || {
            overlay_invalidator
                .lock()
                .map_err(|e| e.to_string())?
                .invalidate()
                .map_err(|e| e.to_string())?;
            invalidator
                .lock()
                .map_err(|e| e.to_string())?
                .invalidate()
                .map_err(|e| e.to_string())
        }),
    )?;
    embedding_cache
        .lock()
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .bind_instance(&graph.instance_id()?)?;
    knowledge_overlay
        .lock()
        .map_err(|e| std::io::Error::other(e.to_string()))?
        .bind_instance(&graph.instance_id()?)
        .map_err(std::io::Error::other)?;
    let state_path = data_dir.join("state.db");
    let work_packets = work_packets::PacketRuntime::open(&state_path)?;
    let jobs = JobStore::open(&state_path)?;
    let job_execution_locks = JobExecutionLocks::open(&data_dir, jobs.execution_namespace())?;
    let jobs = Mutex::new(jobs);
    // Only unchanged recorded attempts whose OS ownership is available
    // are interrupted. Live and legacy-unknown jobs remain untouched.
    recover_jobs(&jobs, &job_execution_locks).map_err(std::io::Error::other)?;
    agent_pool::recover_held_jobs(&jobs, &job_execution_locks).map_err(std::io::Error::other)?;
    investigations::recover(&jobs, &job_execution_locks).map_err(std::io::Error::other)?;
    let findings = FindingStore::open(&state_path)?;
    let sources = SourceRegistry::open(&state_path, &data_dir).map_err(std::io::Error::other)?;
    let tier_settings = settings::SettingsStore::open(&state_path)?;
    // Extraction reads the worker count process-wide (#236).
    source_walk::parallel::set_parallelism(settings::IngestParallelism::parallelism(
        tier_settings.ingest_parallelism()?.setting,
    ));
    let decisions = agents::DecisionLog::open(&state_path)?;
    let staged_proposals = agents::ProposalStore::open(data_dir.join("proposals.sqlite"))?;
    let section_drafts =
        agents::draft_store::DraftStore::open(data_dir.join("section-drafts.sqlite"))
            .map_err(std::io::Error::other)?;
    let primary_sources =
        primary_source::PrimarySourceStore::open(&data_dir).map_err(std::io::Error::other)?;
    primary_sources
        .set_store_limits(capture_store_limits(&tier_settings).map_err(std::io::Error::other)?)
        .map_err(std::io::Error::other)?;
    let recovery_metrics = metrics::MetricsStore::open(&state_path)?;
    Ok(AppState {
        graph: Mutex::new(graph),
        jobs,
        investigations: crate::investigations::InvestigationRuntime::default(),
        job_execution_locks,
        findings: Mutex::new(findings),
        settings: Mutex::new(tier_settings),
        decisions: Mutex::new(decisions),
        proposals: Mutex::new(staged_proposals),
        drafts: Mutex::new(section_drafts),
        work_packets,
        extraction_caches: Mutex::new(ExtractionCaches::default()),
        embedding_cache: Some(embedding_cache),
        knowledge_overlay: Some(knowledge_overlay),
        sources: Arc::new(Mutex::new(sources)),
        primary_sources,
        metrics: Mutex::new(recovery_metrics),
        curated_settled: Arc::default(),
    })
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            install_help_menu(app)?;
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            // #1624: record every model exchange locally for Help → Debug Viewer.
            let model_debug_log = Arc::new(model_debug::ModelDebugLog::new(&data_dir.join("logs")));
            model_debug_log.clone().install();
            app.manage(model_debug_log);
            app.manage(PreflightRuns::default());
            #[cfg(unix)]
            app.manage(mcp_reads::service::Service::default());
            app.manage(open_app_state(&data_dir)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            classify_drop,
            work_packets::generate_work_packet,
            work_packets::assess_work_packet,
            work_packets::prepare_work_packet_selection,
            work_packets::list_work_packets,
            work_packets::get_work_packet,
            work_packets::review_work_packet,
            work_packets::export_work_packet,
            work_packets::draft_adapter_work_packets,
            export_save::save_export_file,
            spec_bundle_file::save_spec_bundle_export,
            manifest_draft::preview_system_manifest,
            manifest_draft::save_system_manifest,
            investigations::commands::investigation_specialists,
            investigations::commands::list_agents,
            investigations::commands::list_pipeline_agents,
            investigations::commands::start_investigation,
            investigations::section_drafts::section_redraft_question,
            investigations::section_drafts::section_draft_question,
            investigations::section_drafts::section_draft_outcome,
            area_briefing::area_briefing,
            area_expert::area_choices,
            area_expert::area_ask_preview,
            area_expert::run_area_ask,
            section_compose::section_compose_preview,
            section_compose::run_section_compose,
            section_compose::run_section_compose_gated,
            section_review::section_document,
            section_review::review_section_draft,
            investigations::commands::list_investigations,
            investigations::commands::list_specialist_task_activity,
            investigations::commands::check_local_ollama,
            investigations::commands::list_investigation_conversation,
            investigations::commands::get_investigation,
            investigations::commands::investigation_events,
            investigations::commands::investigation_result,
            investigations::commands::get_investigation_chat,
            investigations::finding_review_commands::get_investigation_finding_reviews,
            investigations::finding_review_commands::review_investigation_finding,
            investigations::finding_curation_commands::get_investigation_finding_curations,
            investigations::finding_curation_commands::curate_investigation_finding,
            investigations::commands::investigation_consent,
            investigations::commands::approve_investigation_step,
            investigations::commands::decline_investigation_step,
            investigations::commands::cancel_investigation,
            investigations::commands::read_investigation_citation,
            investigations::inspection::resolve_investigation_inspection_target,
            investigations::completion_choices::investigation_obligation_choices,
            investigations::completion_choices::resolve_investigation_source_obligation,
            investigations::inspection::inspection_captured_location,
            ping,
            graph_stats,
            clear_graph,
            start_new_project,
            switch_project,
            list_projects,
            rename_project,
            delete_project,
            system_contents,
            adapter_inventory,
            list_plugins,
            set_plugin_enabled,
            run_plugin_gate,
            clear_finished_jobs,
            list_jobs,
            list_job_usage,
            agent_pool::get_agent_pool_controls,
            agent_pool::set_agent_pool_controls,
            list_evals,
            proposals::record_agent_decision,
            proposals::list_staged_proposals,
            task_evidence::assessment::assess_staged_basis,
            list_agent_decisions,
            reapply_agent_decisions,
            record_assertion_decision,
            list_assertion_decisions,
            ingest_path,
            cancel_job,
            retry_job,
            preflight,
            cancel_preflight,
            system_preflight::preflight_system,
            findings_summary,
            list_findings,
            get_settings,
            set_tier_enabled,
            set_tier_provider,
            grant_cloud_consent,
            revoke_cloud_consent,
            egress_summary,
            get_ingest_parallelism,
            set_ingest_parallelism,
            get_investigation_choice,
            set_investigation_choice,
            get_run_limits,
            set_run_limits,
            reset_run_limits,
            list_local_models,
            get_local_model,
            set_local_model,
            reset_local_model,
            get_local_context_window,
            set_local_context_window,
            get_local_ollama_port,
            set_local_ollama_port,
            get_read_deny_rules,
            get_operator_limits,
            set_operator_limit,
            set_read_deny_rules,
            provider_key_status,
            knowledge_phase::knowledge_phase_status,
            set_provider_key,
            check_provider_connection,
            local_lane_status,
            remove_provider_key,
            cloud_disclosure,
            ingest_history,
            extractor_coverage,
            gap_strategies,
            escalation_capabilities,
            escalation_preview,
            run_escalation,
            run_class_escalation,
            list_nodes,
            atlas_snapshot,
            context::query_context,
            context::query_qualified_context,
            context::recorded_coverage,
            read_evidence,
            primary_source::describe_captured_source,
            primary_source::describe_observed_source,
            primary_source::read_observed_source,
            mcp_reads::commands::mcp_reads_status,
            mcp_reads::commands::mcp_reads_owner_status,
            mcp_reads::commands::mcp_task_client_create,
            mcp_reads::commands::mcp_task_clients,
            mcp_reads::commands::mcp_reads_enable,
            mcp_reads::commands::mcp_tasks_enable,
            mcp_reads::commands::mcp_reads_disable,
            mcp_reads::commands::mcp_reads_rotate,
            mcp_reads::commands::mcp_reads_client_configuration,
            mcp_reads::client_skill::mcp_reads_client_skill,
            primary_source::read_captured_source,
            primary_source::list_retained_sources,
            primary_source::preview_forget_source,
            primary_source::forget_retained_source,
            export_topology,
            export_flows,
            list_flows,
            list_flow_anchors,
            export_spec,
            export_spec_views,
            export_spec_with_views,
            read_spec_artifact,
            read_spec_artifacts,
            read_selected_flow_view,
            semantic_preview,
            add_repo,
            add_system
        ])
        .build(tauri::generate_context!())
        .expect("error while building Cartograph")
        .run(|_app, _event| {
            #[cfg(unix)]
            if matches!(_event, tauri::RunEvent::Exit)
                && let Some(service) = _app.try_state::<mcp_reads::service::Service>()
                && tauri::async_runtime::block_on(service.shutdown()).is_err()
            {
                // Exit waits only for native transport cleanup, not synchronous
                // graph workers. No paths, credentials or source enter this log.
                eprintln!("Cartograph MCP endpoint cleanup failed");
            }
        });
}

#[cfg(test)]
fn test_source_registry(
    state_path: &std::path::Path,
    roots: &std::collections::BTreeSet<String>,
) -> Arc<Mutex<SourceRegistry>> {
    let mut registry = SourceRegistry::open(state_path, state_path.parent().unwrap()).unwrap();
    for root in roots {
        registry.register_local(std::path::Path::new(root)).unwrap();
    }
    Arc::new(Mutex::new(registry))
}

#[cfg(test)]
mod tests {
    use super::test_source_registry;
    use core_graph::{Edge, GraphStore, Node, SqliteGraphStore};
    use llm::{Embedding, Locality, ProviderCaps, ProviderError};

    #[test]
    fn shell_and_ui_links_share_one_repository_location() {
        // #1760: the shell and the UI each hold the repository URL once, and
        // the two must agree, so a move cannot update one side only.
        let ui = include_str!("../../ui/src/helpNotes.ts");
        let declared = format!("export const REPOSITORY_URL = '{}';", super::REPOSITORY_URL);
        assert!(
            ui.contains(&declared),
            "ui/src/helpNotes.ts must declare {declared}"
        );
        assert_eq!(
            super::REPOSITORY_URL,
            "https://github.com/true-line/cartograph"
        );
    }

    #[test]
    fn windows_url_launcher_keeps_every_query_parameter() {
        // AC-0222 (#226): the URL reaches the protocol handler as one
        // argument, `&` and all; `explorer` truncated it at the first `&`.
        let url =
            "https://github.com/true-line/cartograph/issues/new?labels=bug&title=a%20b&body=x";
        let (program, args) = super::url_launcher("windows", url).unwrap();
        assert_eq!(program, "rundll32");
        assert_eq!(args, ["url.dll,FileProtocolHandler", url]);
    }

    #[test]
    fn url_launcher_covers_desktop_oses_and_declines_unknown_ones() {
        // AC-0222: macOS and Linux pass the URL through untouched; an OS
        // with no known launcher gets None (logged, never a guessed program).
        let url = "https://example.test/?a=1&b=2";
        assert_eq!(
            super::url_launcher("macos", url),
            Some(("open", vec![url.to_string()]))
        );
        assert_eq!(
            super::url_launcher("linux", url),
            Some(("xdg-open", vec![url.to_string()]))
        );
        assert_eq!(super::url_launcher("ios", url), None);
        assert_eq!(super::url_launcher("android", url), None);
    }

    #[test]
    fn truncate_utf8_in_place_respects_char_boundaries() {
        // A byte cap landing mid-character must back off rather than split a
        // multi-byte UTF-8 scalar (#488).
        let mut content = "a".repeat(9) + "€"; // '€' is 3 bytes.
        let truncated = super::truncate_utf8_in_place(&mut content, 10);
        assert!(truncated);
        assert_eq!(content, "a".repeat(9));
        assert!(content.len() <= 10);

        let mut short = String::from("hello");
        assert!(!super::truncate_utf8_in_place(&mut short, 10));
        assert_eq!(short, "hello");
    }

    // AC-1204/T-1204 (#2215): the export commands cap each artifact as it
    // is compiled instead of capping a complete bundle, and return in every
    // byte what capping the complete bundle returns, for artifacts over both
    // caps and for the views compiled beside them.
    #[test]
    fn capping_each_artifact_as_compiled_equals_capping_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::registered_source_tests::app_state(dir.path());
        // Each Gap is one register assertion and about 300 bytes of the JSON
        // fact index, so 2,000 pass both caps.
        let gaps = 2_000;
        {
            let mut graph = state.graph.lock().unwrap();
            for at in 0..gaps {
                let prov = core_prov::Provenance::new(
                    core_prov::Tier::Deterministic,
                    core_prov::ConfidenceTier::Gap,
                    vec![],
                    "t0.fixture",
                    format!("gap {at}").as_bytes(),
                )
                .unwrap();
                graph
                    .put_node(&Node {
                        id: format!("gap:{at:04}"),
                        label: "Gap".into(),
                        props: serde_json::json!({
                            "name": format!("gap {at}"),
                            "reason": "an unresolved call",
                            "prov": prov,
                        }),
                    })
                    .unwrap();
            }
        }
        fn json<T: serde::Serialize>(value: &T) -> serde_json::Value {
            serde_json::to_value(value).unwrap()
        }
        for mode in [spec::ExportMode::BestEffort, spec::ExportMode::VerifiedOnly] {
            let whole = super::preview_spec_bundle(
                super::compile_spec_bundle(super::export_inputs(&state).unwrap(), mode).unwrap(),
            );
            let gap_register = whole
                .artifacts
                .iter()
                .find(|artifact| artifact.id == "gap-register")
                .unwrap();
            if mode == spec::ExportMode::BestEffort {
                assert!(gap_register.assertions_truncated);
                assert_eq!(gap_register.assertions_total, gaps);
                assert!(
                    whole
                        .artifacts
                        .iter()
                        .any(|artifact| artifact.content_truncated)
                );
            }
            let capped = super::export_spec_blocking(mode, &state).unwrap();
            assert_eq!(json(&capped), json(&whole), "{mode:?}");

            let (bundle, views, curated) = super::compile_spec_with_views_and_curated(
                super::export_inputs(&state).unwrap(),
                mode,
                |artifact| artifact,
            )
            .unwrap();
            let whole = super::SpecWithViewsPreview {
                bundle: super::preview_spec_bundle(bundle),
                views,
                curated,
            };
            let capped = super::export_spec_with_views_blocking(mode, &state).unwrap();
            assert_eq!(json(&capped), json(&whole), "{mode:?}");
            assert_eq!(
                json(&super::export_spec_views_blocking(mode, &state).unwrap()),
                json(&whole.views),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn preview_spec_bundle_caps_content_and_assertions_but_keeps_totals() {
        // AC-0218 (#488): the export_spec IPC payload stays bounded per
        // artifact regardless of the underlying content size or assertion
        // count, while still reporting true totals so the Workbench can show
        // a counted truncation note and doc-chip counts stay accurate.
        let provenance = core_prov::Provenance::new(
            core_prov::Tier::Deterministic,
            core_prov::ConfidenceTier::Confirmed,
            vec![],
            "t0.fixture",
            b"fixture",
        )
        .expect("within ceiling");
        let big_content = "x".repeat(super::SPEC_ARTIFACT_CONTENT_PREVIEW_BYTES + 10);
        let assertion_total = super::SPEC_ARTIFACT_ASSERTIONS_PREVIEW_COUNT + 5;
        let assertions: Vec<spec::SpecAssertion> = (0..assertion_total)
            .map(|i| spec::SpecAssertion {
                id: format!("a{i}"),
                subject_id: format!("s{i}"),
                subject_kind: "Node".into(),
                summary: "fixture assertion".into(),
                edge_label: None,
                omission: None,
                provenance: provenance.clone(),
            })
            .collect();
        let bundle = spec::SpecBundle {
            mode: spec::ExportMode::VerifiedOnly,
            artifacts: vec![spec::SpecArtifact {
                id: "artifact-1".into(),
                file_name: "big.md".into(),
                title: "Big artifact".into(),
                format: "markdown".into(),
                schema: "cartograph.spec/artifact-1@1".into(),
                content_id: "spec-artifact-v1:fixture".into(),
                content: big_content,
                assertions,
            }],
            assertion_count: assertion_total,
            gap_count: 0,
            drift_count: 0,
            security_count: 0,
            graph_basis: None,
            curated_basis: None,
        };

        let preview = super::preview_spec_bundle(bundle);
        let artifact = &preview.artifacts[0];
        // AC-0242: the capped preview carries the full artifact's identity.
        assert_eq!(artifact.schema, "cartograph.spec/artifact-1@1");
        assert_eq!(artifact.content_id, "spec-artifact-v1:fixture");
        assert!(artifact.content_truncated);
        assert!(artifact.content.len() <= super::SPEC_ARTIFACT_CONTENT_PREVIEW_BYTES);
        assert_eq!(
            artifact.content_byte_len,
            super::SPEC_ARTIFACT_CONTENT_PREVIEW_BYTES + 10
        );
        assert!(artifact.assertions_truncated);
        assert_eq!(
            artifact.assertions.len(),
            super::SPEC_ARTIFACT_ASSERTIONS_PREVIEW_COUNT
        );
        assert_eq!(artifact.assertions_total, assertion_total);
    }

    #[test]
    fn spec_views_match_host_bundle_and_preserve_curation_and_provenance() {
        // AC-0246 / AC-0247 (T-0246, T-0247): the host view surface compiles
        // structured nodes, edges, flows, and deployment roles from the same
        // graph and curation log as `build_spec_bundle`, matching each
        // exported artifact's schema and content_id.
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        let decisions = agents::DecisionLog::open(":memory:").unwrap();
        let prov = |tier: core_prov::Tier, confidence: core_prov::ConfidenceTier, tag: &str| {
            core_prov::Provenance::new(
                tier,
                confidence,
                vec![core_prov::EvidenceRef {
                    repo: "local/shop".into(),
                    path: "src/orders.ts".into(),
                    byte_start: 1,
                    byte_end: 24,
                    commit_sha: "workdir".into(),
                }],
                "app.test",
                tag.as_bytes(),
            )
            .unwrap()
        };
        let obs_prov = prov(
            core_prov::Tier::Dynamic,
            core_prov::ConfidenceTier::Confirmed,
            "obs:queue",
        );
        graph
            .put_node(&Node {
                id: "res:aws_sqs_queue.orders".into(),
                label: "Resource".into(),
                props: serde_json::json!({
                    "logical_id": "aws_sqs_queue.orders",
                    "observed": { "arn": "arn:aws:sqs:us-east-1:1:orders" },
                    "observed_prov": obs_prov,
                    "prov": prov(core_prov::Tier::Deterministic, core_prov::ConfidenceTier::Confirmed, "res:q"),
                }),
            })
            .unwrap();
        graph
            .put_node(&Node {
                id: "chan:sqs:orders".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "identity": "sqs:orders",
                    "prov": prov(core_prov::Tier::Deterministic, core_prov::ConfidenceTier::Confirmed, "chan:q"),
                }),
            })
            .unwrap();
        graph
            .put_edge(&Edge {
                src: "res:aws_sqs_queue.orders".into(),
                dst: "chan:sqs:orders".into(),
                label: "BACKS".into(),
                props: serde_json::json!({
                    "prov": prov(core_prov::Tier::Dynamic, core_prov::ConfidenceTier::Confirmed, "edge:backs"),
                }),
            })
            .unwrap();

        let (bundle, views) = super::build_spec_with_views(
            &graph,
            &decisions,
            &crate::curated::CuratedReview::default(),
            spec::ExportMode::BestEffort,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&bundle).unwrap(),
            serde_json::to_value(
                super::build_spec_bundle(
                    &graph,
                    &decisions,
                    &crate::curated::CuratedReview::default(),
                    spec::ExportMode::BestEffort
                )
                .unwrap()
            )
            .unwrap()
        );
        assert_eq!(
            serde_json::to_value(&views).unwrap(),
            serde_json::to_value(
                super::build_spec_views(
                    &graph,
                    &decisions,
                    &crate::curated::CuratedReview::default(),
                    spec::ExportMode::BestEffort
                )
                .unwrap()
            )
            .unwrap()
        );
        // AC-0551: each artifact compiled alone equals its bundle entry.
        for artifact in &bundle.artifacts {
            assert_eq!(
                &super::build_spec_artifact(
                    &graph,
                    &decisions,
                    Some(&crate::curated::CuratedReview::default()),
                    spec::ExportMode::BestEffort,
                    &artifact.id
                )
                .unwrap(),
                artifact,
                "{}",
                artifact.id
            );
        }
        assert_eq!(
            super::build_spec_artifact(
                &graph,
                &decisions,
                Some(&crate::curated::CuratedReview::default()),
                spec::ExportMode::BestEffort,
                "nope"
            )
            .unwrap_err(),
            "unknown spec artifact id: nope"
        );
        // AC-0720 (#1662): the artifacts read together come from one
        // snapshot preparation, where one read per artifact made one each,
        // and each equals its bundle entry, in the requested order.
        let review = crate::curated::CuratedReview::default();
        let mode = spec::ExportMode::BestEffort;
        let ids: Vec<&str> = bundle
            .artifacts
            .iter()
            .rev()
            .map(|a| a.id.as_str())
            .collect();
        let before = super::spec_reads();
        for id in &ids {
            super::build_spec_artifact(&graph, &decisions, Some(&review), mode, id).unwrap();
        }
        let one_each = super::spec_reads() - before;
        let before = super::spec_reads();
        let together =
            super::build_spec_artifacts(&graph, &decisions, Some(&review), mode, &ids).unwrap();
        let shared = super::spec_reads() - before;
        eprintln!(
            "#1662 preparations for {} artifacts: {one_each} -> {shared}",
            ids.len()
        );
        assert_eq!((one_each, shared), (ids.len(), 1));
        assert_eq!(together.len(), ids.len());
        for (artifact, id) in together.iter().zip(&ids) {
            let expected = bundle.artifacts.iter().find(|a| a.id == *id).unwrap();
            assert_eq!(artifact, expected, "{id}");
        }
        // Neither of these reads the projection: no review is needed, and
        // they still equal their bundle entries.
        let unreviewed = super::build_spec_artifacts(
            &graph,
            &decisions,
            None,
            mode,
            &["topology", "data-model"],
        )
        .unwrap();
        for artifact in &unreviewed {
            let expected = bundle.artifacts.iter().find(|a| a.id == artifact.id);
            assert_eq!(Some(artifact), expected, "{}", artifact.id);
        }
        assert_eq!(
            unreviewed.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["topology", "data-model"]
        );
        // An unknown or repeated id, or a projection artifact without the
        // review, refuses the whole read.
        let refused = |ids: &[&str], review| {
            super::build_spec_artifacts(&graph, &decisions, review, mode, ids).unwrap_err()
        };
        assert_eq!(
            refused(&["topology", "nope"], Some(&review)),
            "unknown spec artifact id: nope"
        );
        assert_eq!(
            refused(&["topology", "topology"], Some(&review)),
            "spec artifact topology was requested twice"
        );
        assert_eq!(
            refused(&["topology", "curated"], None),
            "The curated projection was not read for this artifact."
        );
        let topology_art = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "topology")
            .unwrap();
        assert!(views.topology.matches_artifact(topology_art));
        assert_eq!(views.topology.deployments.len(), 1);
        assert_eq!(
            views.topology.deployments[0].backed_channels,
            vec!["chan:sqs:orders"]
        );
    }

    #[test]
    fn findings_summary_counts_with_register_predicates() {
        // #116: one predicate set (spec's) feeds every surface, and the
        // three lanes never bleed into each other. A fact without
        // provenance counts as a Gap (unknown ≠ confirmed), so the
        // non-gap fixtures carry real Confirmed provenance.
        let confirmed = serde_json::to_value(
            core_prov::Provenance::new(
                core_prov::Tier::Deterministic,
                core_prov::ConfidenceTier::Confirmed,
                vec![],
                "t0.adapter-ts",
                b"fixture",
            )
            .expect("within ceiling"),
        )
        .expect("serializes");
        let nodes = vec![
            Node {
                id: "gap:chan".into(),
                label: "Gap".into(),
                props: serde_json::json!({}),
            },
            Node {
                id: "drift:adr-3".into(),
                label: "Drift".into(),
                props: serde_json::json!({ "kind": "drift", "prov": confirmed }),
            },
            Node {
                id: "svc:api".into(),
                label: "Service".into(),
                props: serde_json::json!({ "prov": confirmed }),
            },
        ];
        let gap = serde_json::to_value(
            core_prov::Provenance::new(
                core_prov::Tier::Deterministic,
                core_prov::ConfidenceTier::Gap,
                vec![],
                "t0.adapter-ts",
                b"gap-fixture",
            )
            .expect("within ceiling"),
        )
        .expect("serializes");
        let edges = vec![
            Edge {
                src: "svc:api".into(),
                dst: "adr:3".into(),
                label: "CONFLICTS".into(),
                props: serde_json::json!({ "prov": confirmed }),
            },
            // An unresolved call emits a gap node AND a gap CALLS edge; the
            // edge supports the same finding and must not double it (#241).
            Edge {
                src: "svc:api".into(),
                dst: "gap:chan".into(),
                label: "CALLS".into(),
                props: serde_json::json!({
                    "reason": "unresolved call target after import/type resolution",
                    "prov": &gap,
                }),
            },
            // An edge-only gap between two real facts (no gap node on either
            // end) is a finding of its own — folding must not swallow it.
            Edge {
                src: "svc:api".into(),
                dst: "sym:handler".into(),
                label: "CALLS".into(),
                props: serde_json::json!({
                    "reason": "callee not statically resolvable",
                    "prov": gap,
                }),
            },
        ];
        let summary = super::summarize_register(&nodes, &edges, 2, 1);
        // One finding for the node+edge pair, one for the standalone edge.
        assert_eq!(summary.gaps, 2);
        assert_eq!(summary.interpretation_backlog, 0);
        // Drift keeps its own node-only rule: the CONFLICTS edge supports
        // the drift node's finding (parity with drift_register's count).
        assert_eq!(summary.drift, 1);
        assert_eq!(summary.unsupported, 2);
        assert_eq!(summary.no_evidence, 1);
        assert_eq!(summary.open_findings, 5); // 2 gaps + 2 unsupported + 1 no-evidence
        assert_eq!(summary.graph_facts, 6);

        // AC-0359 (#994): an established interpretation-seam gap is counted in
        // `interpretation_backlog` while remaining counted in `gaps` and
        // `open_findings`.
        let mut with_backlog = nodes;
        with_backlog.push(Node {
            id: "gap:rule".into(),
            label: "Gap".into(),
            props: serde_json::json!({
                "reason": "The caller's interpretation of this local return or throw is unknown.",
            }),
        });
        let summary_with_backlog = super::summarize_register(&with_backlog, &edges, 2, 1);
        assert_eq!(summary_with_backlog.gaps, 3);
        assert_eq!(summary_with_backlog.interpretation_backlog, 1);
        assert_eq!(summary_with_backlog.open_findings, 6);
    }

    #[test]
    fn adapter_inventory_matches_what_the_ts_walker_ingests() {
        // AC-0095 (#247): the Settings adapter cards derive their "Files:"
        // copy from ingest::preflight's registry, while the walker ingests
        // adapters_lang_ts::SOURCE_EXTENSIONS plus its Vue component
        // extensions (AC-0268). One assertion binds the two
        // hand-maintained lists so coverage and inventory cannot disagree.
        let registry: std::collections::BTreeSet<String> = ingest::preflight::INSTALLED_ADAPTERS
            .iter()
            .filter(|adapter| adapter.id == "t0.adapter-ts")
            .flat_map(|adapter| adapter.extensions.iter().map(|ext| format!(".{ext}")))
            .collect();
        let walker: std::collections::BTreeSet<String> = adapters_lang_ts::SOURCE_EXTENSIONS
            .iter()
            .chain(&adapters_lang_ts::vue::COMPONENT_EXTENSIONS)
            .map(|ext| (*ext).to_string())
            .collect();
        assert_eq!(
            registry, walker,
            "Settings/Preflight adapter inventory must list exactly the \
             extensions the TS/JS walker ingests"
        );
    }

    struct M7KeywordProvider;

    impl llm::LlmProvider for M7KeywordProvider {
        fn id(&self) -> &str {
            "test-keywords"
        }

        fn locality(&self) -> Locality {
            Locality::Local
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: true,
                chat: false,
                tool_use: false,
            }
        }

        fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Ok(batch
                .iter()
                .map(|text| {
                    let text = text.to_ascii_lowercase();
                    vec![
                        f32::from(text.contains("order")),
                        f32::from(text.contains("user")),
                        f32::from(text.contains("billing")),
                        0.01,
                    ]
                })
                .collect())
        }
    }

    struct M7RealInputsProvider;

    impl llm::LlmProvider for M7RealInputsProvider {
        fn id(&self) -> &str {
            "test-real-input-keywords"
        }

        fn locality(&self) -> Locality {
            Locality::Local
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: true,
                chat: false,
                tool_use: false,
            }
        }

        fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Ok(batch
                .iter()
                .map(|text| {
                    let text = text.to_ascii_lowercase();
                    vec![
                        f32::from(text.contains("order")),
                        f32::from(text.contains("process")),
                        f32::from(text.contains("queue")),
                        f32::from(text.contains("user")),
                        0.01,
                    ]
                })
                .collect())
        }
    }

    fn m7_prov(path: &str, confidence: &str) -> serde_json::Value {
        serde_json::json!({
            "tier": "Deterministic",
            "confidence_tier": confidence,
            "evidence": [{
                "repo": "local/shop",
                "path": path,
                "byte_start": 1,
                "byte_end": 5,
                "commit_sha": "abc123"
            }],
            "extractor_id": "t0.test",
            "content_hash": "hash"
        })
    }

    #[test]
    fn semantic_preview_keeps_calibration_only_candidates_staged() {
        // AC-0278: even passing caller calibration stages candidates only;
        // no independent receipt means no admitted overlay or promoted Gap.
        let nodes = vec![
            Node {
                id: "ep:shop@POST:/orders".into(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": "POST", "path": "/orders", "prov": m7_prov("api.ts", "Confirmed")
                }),
            },
            Node {
                id: "sym:shop@api.ts#placeOrder".into(),
                label: "Symbol".into(),
                props: serde_json::json!({
                    "name": "placeOrder", "prov": m7_prov("api.ts", "Confirmed")
                }),
            },
            Node {
                id: "gap:chan:shop@api.ts@10".into(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue",
                    "raw": "computed order destination",
                    "reason": "runtime-computed channel identity",
                    "prov": m7_prov("api.ts", "Gap")
                }),
            },
            Node {
                id: "chan:sqs-queue:orders".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue", "identity": "orders queue",
                    "prov": m7_prov("infra.tf", "Confirmed")
                }),
            },
            Node {
                id: "chan:sqs-queue:users".into(),
                label: "Channel".into(),
                props: serde_json::json!({
                    "kind": "sqs-queue", "identity": "users queue",
                    "prov": m7_prov("infra.tf", "Confirmed")
                }),
            },
        ];
        let edges = vec![
            Edge {
                src: "ep:shop@POST:/orders".into(),
                dst: "sym:shop@api.ts#placeOrder".into(),
                label: "HANDLES".into(),
                props: serde_json::json!({"prov": m7_prov("api.ts", "Confirmed")}),
            },
            Edge {
                src: "sym:shop@api.ts#placeOrder".into(),
                dst: "gap:chan:shop@api.ts@10".into(),
                label: "PUBLISHES".into(),
                props: serde_json::json!({"prov": m7_prov("api.ts", "Gap")}),
            },
        ];
        let eval_pairs = vec![
            semantic::LabeledPair {
                query: "order destination".into(),
                candidate: "orders queue".into(),
                is_match: true,
            },
            semantic::LabeledPair {
                query: "order destination".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
            semantic::LabeledPair {
                query: "billing event".into(),
                candidate: "billing channel".into(),
                is_match: true,
            },
            semantic::LabeledPair {
                query: "billing event".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
        ];
        let preview =
            crate::build_semantic_preview(&M7KeywordProvider, &nodes, &edges, &eval_pairs, 0.95)
                .unwrap();
        assert!(preview.eval.passed);
        assert!(preview.calibration_available);
        let uncalibrated =
            crate::build_semantic_preview(&M7KeywordProvider, &nodes, &edges, &[], 0.95).unwrap();
        assert!(!uncalibrated.calibration_available);
        assert!(!uncalibrated.eval.passed);
        assert_eq!(uncalibrated.proposals.len(), preview.proposals.len());
        assert!(uncalibrated.approved.is_empty());
        assert!(!preview.proposals.is_empty());
        assert!(
            preview
                .proposals
                .iter()
                .any(|proposal| proposal.target_id == "chan:sqs-queue:orders")
        );
        assert_eq!(preview.gaps_filled, 0);
        assert!(preview.approved.is_empty());
        assert_eq!(preview.flows[0].status, flowtracer::FlowStatus::Partial);
        assert!(!preview.dossier.contains("InferredStrong"));
        assert!(nodes.iter().any(|node| node.label == "Gap"));
        assert_eq!(
            preview.evaluation.mode,
            semantic::evaluation::report::SummaryMode::CalibrationOnly
        );
        assert_eq!(preview.evaluation.families.len(), 1);
        assert_eq!(
            preview.evaluation.families[0].staged,
            preview.proposals.len() as u64
        );
        assert_eq!(preview.evaluation.families[0].admitted, 0);
        // Persist exactly what the host displayed, through the real lease/journal path.
        let dir = tempfile::tempdir().unwrap();
        let graph =
            std::sync::Mutex::new(SqliteGraphStore::open(dir.path().join("graph.db")).unwrap());
        let cache = std::sync::Arc::new(std::sync::Mutex::new(
            semantic::embedding_cache::EmbeddingCache::open_disposable(dir.path()).unwrap(),
        ));
        let lease = cache
            .lock()
            .unwrap()
            .bind_instance(&graph.lock().unwrap().instance_id().unwrap())
            .unwrap();
        let path = dir.path().join("state.db");
        let jobs = std::sync::Mutex::new(crate::jobs::JobStore::open(&path).unwrap());
        let recorded =
            crate::record_semantic_preview(&graph, &jobs, &cache, &lease, preview).unwrap();
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(path)
            .unwrap()
            .list_evals()
            .unwrap();
        assert_eq!(reopened[0].evaluation.as_ref(), Some(&recorded.evaluation));
        assert_eq!(reopened[0].approved, 0);
    }

    #[test]
    fn semantic_preview_uses_real_ingested_resource_and_call_gaps() {
        // AC-0278 / #67: production extractor inputs still stage SQS and
        // unresolved relative-import candidates, without calibration-only admission.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("publisher.ts"),
            r#"
import { SendMessageCommand } from '@aws-sdk/client-sqs';
declare function lookupQueue(): string;
const ordersQueueUrl = lookupQueue();
export function publishOrder() {
  return new SendMessageCommand({ QueueUrl: ordersQueueUrl, MessageBody: '{}' });
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("caller.ts"),
            r#"
import { processOrder } from './missing';
export function run() { processOrder(); }
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("orders.ts"),
            "export function processOrder() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.tf"),
            r#"
resource "aws_sqs_queue" "orders" {
  name = "orders"
}
"#,
        )
        .unwrap();

        let extraction = crate::extract_tree(
            dir.path(),
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let gaps: Vec<_> = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Gap")
            .collect();
        assert_eq!(gaps.len(), 2, "real extraction gaps: {gaps:?}");
        assert!(gaps.iter().any(|node| node.props["kind"] == "sqs-queue"));
        assert!(
            gaps.iter()
                .any(|node| node.props["callee"] == "processOrder")
        );

        let eval_pairs = vec![
            semantic::LabeledPair {
                query: "order destination".into(),
                candidate: "orders queue".into(),
                is_match: true,
            },
            semantic::LabeledPair {
                query: "order destination".into(),
                candidate: "users queue".into(),
                is_match: false,
            },
            semantic::LabeledPair {
                query: "process order".into(),
                candidate: "process order".into(),
                is_match: true,
            },
            semantic::LabeledPair {
                query: "process order".into(),
                candidate: "publish order".into(),
                is_match: false,
            },
        ];
        let preview = crate::build_semantic_preview(
            &M7RealInputsProvider,
            &extraction.nodes,
            &extraction.edges,
            &eval_pairs,
            0.95,
        )
        .unwrap();
        assert_eq!(
            preview.gaps_filled, 0,
            "proposals: {:#?}",
            preview.proposals
        );
        assert!(preview.proposals.iter().any(|proposal| {
            proposal.edge_label == "PUBLISHES"
                && proposal.target_node.as_ref().is_some_and(|node| {
                    node.props["backing_resource"] == "res:local/shop@aws_sqs_queue.orders"
                })
        }));
        assert!(preview.proposals.iter().any(|proposal| {
            proposal.edge_label == "CALLS"
                && proposal.target_id == "sym:local/shop@orders.ts#processOrder"
        }));
        assert!(preview.eval.passed);
        assert!(preview.approved.is_empty());
        assert_eq!(preview.evaluation.families.len(), 2);
        assert!(
            preview
                .evaluation
                .families
                .iter()
                .all(|family| family.admitted == 0
                    && family.status
                        == semantic::evaluation::report::FamilyStatus::CalibrationOnly)
        );
        assert_eq!(
            serde_json::to_value(&preview.flows).unwrap(),
            serde_json::to_value(flowtracer::trace(&extraction.nodes, &extraction.edges)).unwrap(),
            "calibration-only preview preserves the original graph's flow projection"
        );
        assert!(
            extraction
                .nodes
                .iter()
                .all(|node| { node.props["prov"]["confidence_tier"] != "InferredStrong" })
        );
    }

    #[test]
    fn ingest_parses_found_adr_and_links_explicit_governed_target() {
        // AC-0036 (T-0036): the production ingest path turns an existing
        // Markdown ADR and explicit target id into Confirmed graph facts.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.ts"),
            "export function placeOrder() {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("docs/adr")).unwrap();
        std::fs::write(
            dir.path().join("docs/adr/ADR-0001-orders.md"),
            "# Place orders in one service\n\n- **Status:** Accepted\n- **Governs:** `sym:local/shop@orders.ts#placeOrder`\n",
        )
        .unwrap();

        let extraction = crate::extract_tree(
            dir.path(),
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let adr = extraction
            .nodes
            .iter()
            .find(|node| node.label == "ADR")
            .unwrap();
        assert_eq!(adr.props["origin"], "found");
        assert_eq!(adr.props["prov"]["confidence_tier"], "Confirmed");
        let decides = extraction
            .edges
            .iter()
            .find(|edge| edge.label == "DECIDES")
            .unwrap();
        assert_eq!(decides.src, adr.id);
        assert_eq!(decides.dst, "sym:local/shop@orders.ts#placeOrder");
        assert_eq!(decides.props["prov"]["confidence_tier"], "Confirmed");
    }

    #[test]
    fn ingest_redacts_found_adr_text_and_names_unread_adrs_in_coverage() {
        // AC-0685 (T-0685, #1847): a secret in a found ADR never reaches the
        // graph; a non-UTF-8 or oversized ADR no longer fails the ingest and
        // is named on the reader's coverage row and in the not-covered list.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("orders.ts"),
            "export function placeOrder() {}\n",
        )
        .unwrap();
        let adr = dir.path().join("docs/adr");
        std::fs::create_dir_all(&adr).unwrap();
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        std::fs::write(
            adr.join("ADR-0001-orders.md"),
            format!("# Orders\n\n- **Governs:** `sym:local/shop@orders.ts#placeOrder`\n\npassword: {secret}\n"),
        )
        .unwrap();
        std::fs::write(adr.join("ADR-0002-latin1.md"), b"# Caf\xe9\n").unwrap();
        std::fs::write(
            adr.join("ADR-0003-huge.md"),
            "x".repeat(spec::MAX_FOUND_ADR_BYTES as usize + 1),
        )
        .unwrap();

        let mut reads = super::ProducerReads::default();
        let (extraction, layers, _) = super::extract_tree_with_primary(
            dir.path(),
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeSet::new(),
            None,
            None,
            &[],
            &mut super::RepoExtractionCache::default(),
            &[],
            &mut |_| {},
            None,
            &mut Vec::new(),
            &mut reads,
        )
        .unwrap();
        let adrs: Vec<_> = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "ADR")
            .collect();
        assert_eq!(adrs.len(), 1);
        let stored = serde_json::to_string(&adrs[0].props).unwrap();
        assert!(!stored.contains(secret), "secret stored: {stored}");
        assert!(
            extraction
                .edges
                .iter()
                .any(|edge| edge.label == "DECIDES" && edge.src == adrs[0].id)
        );

        let coverage = super::repo_coverage(
            "local/shop",
            "workdir",
            None,
            &[],
            &layers,
            &extraction,
            &super::DeclaredInputs::default(),
            &reads,
        )
        .with_unsupported(&ingest::preflight::PreflightReport::default())
        .with_unread_adrs(&reads.adr_too_large, &reads.adr_not_utf8);
        let row = coverage
            .producers
            .iter()
            .find(|producer| producer.id == spec::FOUND_ADR_EXTRACTOR_ID)
            .expect("found-ADR reader row");
        assert_eq!((row.files, row.nodes, row.edges), (1, 1, 1));
        assert!(row.stopping_reason.starts_with("read 1 of the 3 ADR"));
        assert!(
            coverage
                .producers
                .iter()
                .any(|producer| producer.id == spec::CONTRACT_EXTRACTOR_ID)
        );
        let named: Vec<_> = coverage
            .unsupported
            .iter()
            .map(|entry| (entry.capability.as_str(), entry.first_site.as_str()))
            .collect();
        assert!(named.contains(&(
            ingest::coverage::ADR_TOO_LARGE_CAPABILITY,
            "docs/adr/ADR-0003-huge.md"
        )));
        assert!(named.contains(&(
            ingest::coverage::ADR_NOT_UTF8_CAPABILITY,
            "docs/adr/ADR-0002-latin1.md"
        )));
        assert!(coverage.limited);
    }

    #[test]
    fn polyglot_ingest_keeps_one_adapters_placeholder_gap() {
        // AC-0207 (#237 review): TS and Go both import `foo`. The TS closure
        // leaves `mod:foo` a Gap (a tsconfig paths alias with no file); the Go
        // closure proves it external. The merged extraction must hold one
        // `mod:foo`, the Gap — never the later Confirmed duplicate.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"foo":["./nowhere/foo"]}}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("app.ts"), "import { x } from \"foo\";\n").unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/app\n\ngo 1.22\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nimport \"foo\"\n\nfunc main() { foo.X() }\n",
        )
        .unwrap();

        let extraction = crate::extract_tree(
            dir.path(),
            "local/poly",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let modules: Vec<_> = extraction
            .nodes
            .iter()
            .filter(|node| node.id == "mod:foo")
            .collect();
        assert_eq!(modules.len(), 1, "one reconciled mod:foo");
        assert_eq!(modules[0].props["placeholder"], true);
        assert_eq!(modules[0].props["boundary"], "unresolved");
        assert_eq!(modules[0].props["prov"]["confidence_tier"], "Gap");
        // Both adapters really did import it.
        for importer in ["file:local/poly@app.ts", "file:local/poly@main.go"] {
            assert!(
                extraction
                    .edges
                    .iter()
                    .any(|edge| edge.src == importer && edge.dst == "mod:foo")
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_documented_path_is_checked_in_the_tree_without_following_a_link() {
        // AC-0679 (T-0679): what a symlink points at, possibly outside the
        // repository, never decides whether a documented path exists.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("docs/guide.md"), "# Guide\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
        assert!(crate::tree_has_path(&root, "docs/guide.md"));
        assert!(!crate::tree_has_path(&root, "docs/gone.md"));
        assert!(!crate::tree_has_path(&root, "../outside"));
        let before = crate::tree_has_path(&root, "linked/guide.md");
        std::fs::write(outside.join("guide.md"), "# Elsewhere\n").unwrap();
        assert_eq!(crate::tree_has_path(&root, "linked/guide.md"), before);
    }

    #[test]
    fn documentation_files_are_facts_and_a_reingest_drops_what_was_removed() {
        // AC-0676 (T-0676): the ingest pass stores each documentation file
        // as a Document with its DocSection facts; the same tree yields the
        // same graph content hash; a removed file or section leaves no fact.
        // AC-0678 (T-0678): a reference to a recovered file is one MENTIONS
        // edge, and it goes when the reference does.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("app.ts"), "export function handle() {}\n").unwrap();
        std::fs::write(
            root.join("README.md"),
            "# Shop\n\nSee `app.ts`.\n\n## Routes\n\n## Deploy\n",
        )
        .unwrap();
        std::fs::write(root.join("NOTES.txt"), "notes\n").unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let extraction = crate::extract_tree(
            root,
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(&mut store, &extraction, "local/shop", root, "workdir").unwrap();
        let read = |store: &mut SqliteGraphStore| {
            let snapshot = store.read_snapshot().unwrap();
            let updates = crate::collect_documents(&snapshot.0, |_| Ok(root)).unwrap();
            let mut patch = core_graph::GraphPatch::default();
            crate::document_patch(&snapshot.0, &snapshot.1, updates, &mut patch);
            assert!(
                store
                    .apply_patch_if_snapshot_matches(&snapshot, &patch)
                    .unwrap()
            );
            let (nodes, edges) = store.read_snapshot().unwrap();
            let ids: Vec<String> = nodes
                .iter()
                .filter(|node| matches!(node.label.as_str(), "Document" | "DocSection"))
                .map(|node| node.id.clone())
                .collect();
            let sections = edges
                .iter()
                .filter(|edge| edge.label == "HAS_SECTION")
                .count();
            let mentions: Vec<(String, String)> = edges
                .iter()
                .filter(|edge| edge.label == "MENTIONS")
                .map(|edge| (edge.src.clone(), edge.dst.clone()))
                .collect();
            (
                ids,
                (sections, mentions),
                spec::graph_content_hash(&nodes, &edges),
            )
        };

        let (ids, sections, hash) = read(&mut store);
        assert_eq!(
            ids,
            [
                "doc:local/shop@NOTES.txt",
                "doc:local/shop@README.md",
                "docsec:local/shop@NOTES.txt#0",
                "docsec:local/shop@README.md#0",
                "docsec:local/shop@README.md#1",
                "docsec:local/shop@README.md#2",
            ]
        );
        assert_eq!(
            sections,
            (
                4,
                vec![(
                    "docsec:local/shop@README.md#0".to_string(),
                    "file:local/shop@app.ts".to_string()
                )]
            )
        );
        // The same tree again: the same facts and the same content hash.
        assert_eq!(read(&mut store), (ids, sections, hash));

        std::fs::remove_file(root.join("NOTES.txt")).unwrap();
        std::fs::write(root.join("README.md"), "# Shop\n\n## Routes\n").unwrap();
        let (ids, sections, _) = read(&mut store);
        assert_eq!(
            ids,
            [
                "doc:local/shop@README.md",
                "docsec:local/shop@README.md#0",
                "docsec:local/shop@README.md#1",
            ]
        );
        assert_eq!(sections, (2, Vec::new()));
    }

    #[test]
    fn a_broken_file_reference_is_drift_until_the_documentation_matches_again() {
        // AC-0679 (T-0679): a relative link to a file that is not in the
        // tree is a Drift fact from the ingest pass; once the link names a
        // file that exists, a re-ingest leaves no entry.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("app.ts"), "export function handle() {}\n").unwrap();
        std::fs::write(
            root.join("README.md"),
            "# Shop\n\n[code](app.ts) [gone](setup.md)\n",
        )
        .unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let extraction = crate::extract_tree(
            root,
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(&mut store, &extraction, "local/shop", root, "workdir").unwrap();
        let read = |store: &mut SqliteGraphStore| {
            let snapshot = store.read_snapshot().unwrap();
            let updates = crate::collect_documents(&snapshot.0, |_| Ok(root)).unwrap();
            let mut patch = core_graph::GraphPatch::default();
            crate::document_patch(&snapshot.0, &snapshot.1, updates, &mut patch);
            assert!(
                store
                    .apply_patch_if_snapshot_matches(&snapshot, &patch)
                    .unwrap()
            );
            let (nodes, edges) = store.read_snapshot().unwrap();
            let drift: Vec<String> = nodes
                .iter()
                .filter(|node| node.label == "Drift")
                .map(|node| node.id.clone())
                .collect();
            let documented = edges
                .iter()
                .filter(|edge| edge.label == "DOCUMENTED_AS")
                .count();
            (drift, documented)
        };
        assert_eq!(
            read(&mut store),
            (
                vec![
                    "drift:documentation:local/shop@README.md:doc-file-missing:setup.md"
                        .to_string()
                ],
                1
            )
        );
        std::fs::write(root.join("setup.md"), "# Setup\n").unwrap();
        assert_eq!(read(&mut store), (Vec::new(), 0));
    }

    #[test]
    fn documentation_that_contradicts_the_code_changes_no_other_fact() {
        // AC-0680 (T-0680): documentation is stated intent. A README that
        // restates and contradicts what was recovered adds documents,
        // sections and drift, and every other fact and edge keeps its
        // content, tier, confidence and content hash; no fact holds the
        // text of a section.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("app.ts"), "export function handle() {}\n").unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let extraction = crate::extract_tree(
            root,
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(&mut store, &extraction, "local/shop", root, "workdir").unwrap();
        let before = store.read_snapshot().unwrap();
        assert!(!before.0.is_empty());

        std::fs::write(
            root.join("README.md"),
            "# Shop\n\nThe handler in `app.ts` is written in Python and was removed. \
             See [the old one](legacy.py).\n",
        )
        .unwrap();
        let updates = crate::collect_documents(&before.0, |_| Ok(root)).unwrap();
        let mut patch = core_graph::GraphPatch::default();
        crate::document_patch(&before.0, &before.1, updates, &mut patch);
        assert!(
            store
                .apply_patch_if_snapshot_matches(&before, &patch)
                .unwrap()
        );
        let (nodes, edges) = store.read_snapshot().unwrap();

        let documentation = |label: &str| matches!(label, "Document" | "DocSection" | "Drift");
        let added: Vec<&str> = nodes
            .iter()
            .filter(|node| documentation(&node.label))
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(
            added,
            [
                "doc:local/shop@README.md",
                "docsec:local/shop@README.md#0",
                "drift:documentation:local/shop@README.md:doc-file-missing:legacy.py",
            ]
        );
        let kept: Vec<&core_graph::Node> = nodes
            .iter()
            .filter(|node| !documentation(&node.label))
            .collect();
        assert_eq!(kept, before.0.iter().collect::<Vec<_>>());
        let added_edge = |label: &str| {
            matches!(
                label,
                "HAS_SECTION" | "MENTIONS" | "DOCUMENTED_AS" | "SERVED_AS"
            )
        };
        let kept: Vec<&core_graph::Edge> = edges
            .iter()
            .filter(|edge| !added_edge(&edge.label))
            .collect();
        assert_eq!(kept, before.1.iter().collect::<Vec<_>>());
        assert!(edges.iter().any(|edge| edge.label == "MENTIONS"));
        for node in &nodes {
            assert!(
                !node.props.to_string().contains("written in Python"),
                "{}",
                node.id
            );
        }
        for edge in &edges {
            assert!(
                !edge.props.to_string().contains("written in Python"),
                "{}",
                edge.label
            );
        }
    }

    #[test]
    fn the_coverage_inventory_counts_the_documentation_the_reader_stores() {
        // AC-0677 (T-0677): the counts recorded with a repository's
        // inventory are those of the read that stores its documents.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("README.md"), "# Shop\n\n## Routes\n").unwrap();
        std::fs::write(root.join("binary.txt"), [0xff, 0xfe]).unwrap();
        // The ingest read: documents and found decisions in one walk.
        let is_candidate = |_: &str| false;
        let read = spec::read_documentation(
            root,
            "local/shop",
            "workdir",
            &spec::DocumentationCases {
                documents: true,
                api_references: &[],
                decisions: Some(&is_candidate),
            },
        )
        .unwrap();
        let counted = crate::document_coverage(read.documents.counts);
        assert_eq!(
            counted,
            ingest::coverage::DocumentCoverage {
                found: 2,
                read: 1,
                skipped: std::collections::BTreeMap::from([("not_utf8".to_string(), 1)]),
                sections: 2,
                sections_truncated: 0,
            }
        );
        let stored = spec::extract_documents(root, "local/shop", "workdir").unwrap();
        let labelled = |label: &str| {
            stored
                .nodes
                .iter()
                .filter(|node| node.label == label)
                .count() as u64
        };
        assert_eq!(
            (labelled("Document"), labelled("DocSection")),
            (counted.read, counted.sections)
        );
    }

    #[test]
    fn system_relinks_found_adr_to_cross_repo_target() {
        // AC-0036 (T-0036): a decision repo may be loaded before the service
        // containing its explicit target; the full-system pass links it once
        // both repos are present.
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs-repo");
        let service = dir.path().join("service");
        std::fs::create_dir_all(docs.join("docs/adr")).unwrap();
        std::fs::create_dir_all(&service).unwrap();
        std::fs::write(
            docs.join("docs/adr/ADR-0001-service.md"),
            "# Service ownership\n\n- **Status:** Accepted\n- **Governs:** `sym:local/service@app.ts#handle`\n",
        )
        .unwrap();
        std::fs::write(service.join("app.ts"), "export function handle() {}\n").unwrap();

        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let docs_extraction = crate::extract_tree(
            &docs,
            "local/docs-repo",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(
            &mut store,
            &docs_extraction,
            "local/docs-repo",
            &docs,
            "workdir",
        )
        .unwrap();
        assert!(store.edges_with_labels(&["DECIDES"]).unwrap().is_empty());

        let service_extraction = crate::extract_tree(
            &service,
            "local/service",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(
            &mut store,
            &service_extraction,
            "local/service",
            &service,
            "workdir",
        )
        .unwrap();
        let updates = crate::collect_found_adrs(&store.all_nodes().unwrap(), |repo| match repo {
            "local/docs-repo" => Ok(docs.as_path()),
            "local/service" => Ok(service.as_path()),
            _ => Err("unexpected fixture source".into()),
        })
        .unwrap();
        crate::apply_found_adrs(&mut store, updates).unwrap();

        let decides = store.edges_with_labels(&["DECIDES"]).unwrap();
        assert_eq!(decides.len(), 1);
        assert_eq!(
            decides[0].src,
            "adr:local/docs-repo@docs/adr/ADR-0001-service.md"
        );
        assert_eq!(decides[0].dst, "sym:local/service@app.ts#handle");
        assert_eq!(decides[0].props["prov"]["confidence_tier"], "Confirmed");

        // Removing the declaration reconciles the previously confirmed edge;
        // it must not survive as a zombie after the next deterministic pass.
        std::fs::write(
            docs.join("docs/adr/ADR-0001-service.md"),
            "# Service ownership\n\n- **Status:** Accepted\n",
        )
        .unwrap();
        let updates = crate::collect_found_adrs(&store.all_nodes().unwrap(), |repo| match repo {
            "local/docs-repo" => Ok(docs.as_path()),
            "local/service" => Ok(service.as_path()),
            _ => Err("unexpected fixture source".into()),
        })
        .unwrap();
        crate::apply_found_adrs(&mut store, updates).unwrap();
        assert!(store.edges_with_labels(&["DECIDES"]).unwrap().is_empty());

        // Removing the source file reconciles the found ADR node as well.
        std::fs::remove_file(docs.join("docs/adr/ADR-0001-service.md")).unwrap();
        let updates = crate::collect_found_adrs(&store.all_nodes().unwrap(), |repo| match repo {
            "local/docs-repo" => Ok(docs.as_path()),
            "local/service" => Ok(service.as_path()),
            _ => Err("unexpected fixture source".into()),
        })
        .unwrap();
        crate::apply_found_adrs(&mut store, updates).unwrap();
        assert!(
            store
                .nodes_with_label("ADR")
                .unwrap()
                .into_iter()
                .all(|node| node.id != "adr:local/docs-repo@docs/adr/ADR-0001-service.md")
        );
    }

    #[test]
    fn layer_summary_reports_ts_and_tf_files_and_facts() {
        // AC-0049: zero counts are explicit, so a Pulumi/TS tree cannot look
        // like successful Terraform recovery.
        let ts_only = tempfile::tempdir().unwrap();
        std::fs::write(
            ts_only.path().join("infra.ts"),
            "export const bucket = 'pulumi-shaped';\n",
        )
        .unwrap();
        let (_, summary) = crate::extract_tree_with_summary(
            ts_only.path(),
            "local/pulumi",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.ts.files, 1);
        assert!(summary.ts.nodes > 0);
        assert_eq!(summary.tf, crate::LayerSummary::default());

        std::fs::write(
            ts_only.path().join("main.tf"),
            "resource \"aws_s3_bucket\" \"uploads\" {}\n",
        )
        .unwrap();
        let (_, summary) = crate::extract_tree_with_summary(
            ts_only.path(),
            "local/mixed",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.ts.files, 1);
        assert_eq!(summary.tf.files, 1);
        assert!(summary.tf.nodes > 0);
        assert_eq!(summary.files(), 2);
    }

    #[test]
    fn ingest_summary_quotes_distinct_published_facts_and_reports_merges() {
        // AC-0195 (#242): one relation cited from several sites, and distinct
        // declarations sharing one id, collapse to one stored fact each. The
        // summary must quote what the store holds and state what it merged.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("util.ts"),
            "export function helper() {}\nexport function other() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            "import { helper } from './util';\nimport { other } from './util';\n\
             export function run() { helper(); helper(); other(); }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Owner.java"),
            // Two declarations with the SAME signature (a duplicate, not a
            // real overload — #435 gives distinct overloads distinct ids,
            // so this fixture keeps testing the store's own collision
            // handling with an actual id collision instead).
            "package demo;\nclass Owner {\n  Pet getPet(String name) { return null; }\n  \
             Pet getPet(String other) { return null; }\n}\nclass Pet {}\n",
        )
        .unwrap();
        let (extraction, layers) = crate::extract_tree_with_summary(
            dir.path(),
            "local/merges",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let mut published = crate::OperationFacts::default();
        published.record_load(
            crate::load_into_graph(
                &mut store,
                &extraction,
                "local/merges",
                dir.path(),
                "workdir",
            )
            .unwrap()
            .published,
        );

        // The summary totals are the store's own counts after a first ingest.
        assert_eq!(
            store.fact_counts().unwrap(),
            (published.nodes(), published.edges())
        );
        // Nothing vanishes unreported: raw occurrences = published + merged
        // (the Repo node is the load's own addition).
        assert_eq!(
            extraction.nodes.len() as u64 + 1,
            published.nodes() + published.merged.nodes
        );
        assert_eq!(
            extraction.edges.len() as u64,
            published.edges() + published.merged.edges
        );
        // `run` calls `helper` twice and imports `./util` twice: one relation each.
        let calls_helper = extraction
            .edges
            .iter()
            .filter(|edge| {
                edge.label == "CALLS" && edge.src.ends_with("#run") && edge.dst.ends_with("#helper")
            })
            .count();
        assert_eq!(calls_helper, 2, "fixture must emit repeated call sites");
        assert!(published.merged.edges >= 2);
        // The two identical-signature `getPet` declarations share one symbol
        // id with differing spans (a real duplicate, not an overload — #435).
        assert_eq!(published.merged.node_collisions, 1);
        assert!(published.merged.nodes >= 1);
        // Per-layer rows count distinct facts, so they never exceed the store.
        let java_symbols = extraction
            .nodes
            .iter()
            .filter(|node| node.id.contains("Owner.java"))
            .map(|node| node.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(java_symbols.len() as u64 <= layers.java.nodes);
        assert_eq!(
            layers.ts.edges,
            crate::distinct_edge_count(
                &extraction
                    .edges
                    .iter()
                    .filter(|edge| edge.props["prov"]["extractor_id"] == "t0.adapter-ts")
                    .cloned()
                    .collect::<Vec<_>>()
            )
        );
    }

    #[test]
    fn collapsed_relation_keeps_every_call_site_as_evidence() {
        // AC-0203 (#436): `run` calls `helper` twice. The stored CALLS edge
        // cites both sites in source order, and re-ingesting yields the same
        // edge. A relation cited once keeps its extracted provenance.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("util.ts"),
            "export function helper() {}\nexport function other() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            "import { helper, other } from './util';\n\
             export function run() { helper(); other(); helper(); }\n",
        )
        .unwrap();
        let ingest = || {
            let extraction = crate::extract_tree(
                dir.path(),
                "local/sites",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            let mut store = SqliteGraphStore::open_in_memory().unwrap();
            crate::load_into_graph(
                &mut store,
                &extraction,
                "local/sites",
                dir.path(),
                "workdir",
            )
            .unwrap();
            (extraction, store)
        };
        let calls = |edges: &[Edge], dst: &str| {
            edges
                .iter()
                .filter(|edge| {
                    edge.label == "CALLS" && edge.src.ends_with("#run") && edge.dst.ends_with(dst)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let prov = |edge: &Edge| {
            serde_json::from_value::<core_prov::Provenance>(edge.props["prov"].clone()).unwrap()
        };
        let (extraction, store) = ingest();
        let sites = calls(&extraction.edges, "#helper");
        assert_eq!(sites.len(), 2, "fixture must emit repeated call sites");
        let stored = store.all_edges().unwrap();
        let [helper] = calls(&stored, "#helper").try_into().unwrap();
        let helper_prov = prov(&helper);
        let mut expected = sites
            .iter()
            .flat_map(|site| prov(site).evidence)
            .collect::<Vec<_>>();
        expected.sort_by_key(|span| span.byte_start);
        assert_eq!(helper_prov.evidence, expected);
        assert!(helper_prov.evidence[0].byte_start < helper_prov.evidence[1].byte_start);
        // Tier and confidence stay those of the relation itself.
        assert_eq!(helper_prov.tier, prov(&sites[1]).tier);
        assert_eq!(helper_prov.confidence_tier, prov(&sites[1]).confidence_tier);
        // The hash covers every cited site, not just the last one.
        assert!(
            sites
                .iter()
                .all(|site| prov(site).content_hash != helper_prov.content_hash)
        );
        let [other] = calls(&stored, "#other").try_into().unwrap();
        assert_eq!(other, calls(&extraction.edges, "#other")[0]);
        // Deterministic: a second ingest stores identical edges and hashes.
        let (_, again) = ingest();
        assert_eq!(again.all_edges().unwrap(), stored);
        assert_eq!(
            crate::deterministic_graph_hashes(&again).unwrap(),
            crate::deterministic_graph_hashes(&store).unwrap()
        );
    }

    #[test]
    fn collapsed_relation_evidence_is_bounded_and_keeps_its_confidence() {
        // AC-0203 (#436): evidence is sorted, de-duplicated and capped with
        // the overflow recorded; an occurrence at another confidence tier
        // never lends its span to the stored relation.
        let site = |start: u64, confidence: core_prov::ConfidenceTier| Edge {
            src: "sym:r@a.ts#run".into(),
            dst: "sym:r@b.ts#helper".into(),
            label: "CALLS".into(),
            props: serde_json::json!({
                "prov": core_prov::Provenance::new(
                    core_prov::Tier::Deterministic,
                    confidence,
                    vec![core_prov::EvidenceRef {
                        repo: "r".into(),
                        path: "a.ts".into(),
                        byte_start: start,
                        byte_end: start + 1,
                        commit_sha: "c".into(),
                    }],
                    "t0.test",
                    format!("CALLS at {start}").as_bytes(),
                )
                .unwrap(),
            }),
        };
        let confirmed = core_prov::ConfidenceTier::Confirmed;
        let over = crate::MAX_MERGED_EDGE_EVIDENCE as u64 + 3;
        let mut edges = (0..over)
            .rev()
            .map(|start| site(start, confirmed))
            .collect::<Vec<_>>();
        edges.push(site(0, confirmed)); // exact duplicate span
        edges.push(site(1_000, core_prov::ConfidenceTier::Gap));
        edges.push(site(5, confirmed)); // last occurrence supplies the props
        let merged = crate::merge_edge_occurrences(edges.clone());
        assert_eq!(merged.len(), 1);
        let edge = merged.values().next().unwrap();
        let prov =
            serde_json::from_value::<core_prov::Provenance>(edge.props["prov"].clone()).unwrap();
        assert_eq!(prov.confidence_tier, confirmed);
        assert_eq!(
            prov.evidence
                .iter()
                .map(|span| span.byte_start)
                .collect::<Vec<_>>(),
            (0..crate::MAX_MERGED_EDGE_EVIDENCE as u64).collect::<Vec<_>>()
        );
        assert_eq!(edge.props["evidence_omitted"], 3);
        // Occurrence order does not change the merged fact.
        edges.rotate_left(4);
        edges.push(site(5, confirmed));
        assert_eq!(
            crate::merge_edge_occurrences(edges).values().next(),
            Some(edge)
        );
    }

    #[test]
    fn collapsed_relation_hash_tracks_sites_that_share_one_fact_hash() {
        // AC-0203 (#456 review): duplicate imports hash the same fact bytes
        // at every site. The merged hash must still change when a site is
        // added, removed or moved, while a single site keeps its own hash.
        let site = |start: u64| Edge {
            src: "file:r@a.ts".into(),
            dst: "file:r@b.ts".into(),
            label: "IMPORTS".into(),
            props: serde_json::json!({
                "prov": core_prov::Provenance::new(
                    core_prov::Tier::Deterministic,
                    core_prov::ConfidenceTier::Confirmed,
                    vec![core_prov::EvidenceRef {
                        repo: "r".into(),
                        path: "a.ts".into(),
                        byte_start: start,
                        byte_end: start + 10,
                        commit_sha: "c".into(),
                    }],
                    "t0.test",
                    b"IMPORTS ./b",
                )
                .unwrap(),
            }),
        };
        let hash = |starts: &[u64]| {
            let edges = starts.iter().map(|start| site(*start)).collect::<Vec<_>>();
            let merged = crate::merge_edge_occurrences(edges);
            let edge = merged.values().next().unwrap();
            edge.props["prov"]["content_hash"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let single = hash(&[0]);
        assert_eq!(single, site(0).props["prov"]["content_hash"]);
        let two = hash(&[0, 40]);
        assert_ne!(two, single);
        assert_ne!(hash(&[0, 40, 80]), two); // site added
        assert_ne!(hash(&[0, 60]), two); // site moved
        assert_eq!(hash(&[40, 0]), two); // order-independent
        assert_eq!(hash(&[0, 40, 40]), two); // exact duplicate span
        // Spans beyond the bound still count toward the hash.
        let bound = crate::MAX_MERGED_EDGE_EVIDENCE as u64;
        let full = (0..bound).map(|n| n * 20).collect::<Vec<_>>();
        let mut over = full.clone();
        over.push(bound * 20);
        assert_ne!(hash(&over), hash(&full));
    }

    #[test]
    fn collapsed_relation_borrowed_view_matches_the_owned_merge() {
        // AC-0203 (#1235): receipt matching reads a borrowed view of the
        // merge that graph load performs by move. Both must yield the same
        // fact for every group shape, and an unchanged group stays borrowed.
        let edge = |dst: &str, start: u64, confidence, extractor: &str| Edge {
            src: "fn:r@a.ts#run".into(),
            dst: dst.into(),
            label: "CALLS".into(),
            props: serde_json::json!({
                "site": start,
                "prov": core_prov::Provenance::new(
                    core_prov::Tier::Deterministic,
                    confidence,
                    vec![core_prov::EvidenceRef {
                        repo: "r".into(),
                        path: "a.ts".into(),
                        byte_start: start,
                        byte_end: start + 4,
                        commit_sha: "c".into(),
                    }],
                    extractor,
                    format!("CALLS {dst}").as_bytes(),
                )
                .unwrap(),
            }),
        };
        let confirmed = core_prov::ConfidenceTier::Confirmed;
        let mut malformed = edge("fn:r@a.ts#bad", 9, confirmed, "t0.test");
        malformed.props["prov"] = serde_json::json!("not provenance");
        let edges = vec![
            edge("fn:r@a.ts#single", 0, confirmed, "t0.test"),
            edge("fn:r@a.ts#dup", 2, confirmed, "t0.test"),
            edge(
                "fn:r@a.ts#mixed",
                3,
                core_prov::ConfidenceTier::Gap,
                "t0.test",
            ),
            edge("fn:r@a.ts#bad", 8, confirmed, "t0.test"),
            edge("fn:r@a.ts#dup", 1, confirmed, "t0.test"),
            edge("fn:r@a.ts#mixed", 4, confirmed, "t0.other"),
            malformed,
        ];
        let view = crate::merged_edge_view(&edges);
        let owned = crate::merge_edge_occurrences(edges.clone());
        // AC-0549 (#1574): merging packed edges equals merging decoded ones.
        let packed = crate::merge_packed_edge_occurrences(
            edges
                .iter()
                .cloned()
                .map(|edge| core_graph::packed::PackedEdge::pack(edge).unwrap())
                .collect(),
        )
        .unwrap();
        assert_eq!(
            packed
                .into_iter()
                .map(|(key, edge)| (key, edge.unpack().unwrap()))
                .collect::<std::collections::BTreeMap<_, _>>(),
            owned
        );
        assert_eq!(
            view.iter()
                .map(|(key, edge)| (key.clone(), edge.as_ref().clone()))
                .collect::<std::collections::BTreeMap<_, _>>(),
            owned
        );
        let borrowed = |dst: &str| {
            matches!(
                view[&("fn:r@a.ts#run".into(), dst.into(), "CALLS".into())],
                std::borrow::Cow::Borrowed(_)
            )
        };
        assert!(borrowed("fn:r@a.ts#single"));
        assert!(
            borrowed("fn:r@a.ts#bad"),
            "malformed last provenance keeps the edge"
        );
        assert!(!borrowed("fn:r@a.ts#dup"));
        assert!(!borrowed("fn:r@a.ts#mixed"));
        // The last occurrence still supplies the props of each fact.
        let dup = &owned[&(
            "fn:r@a.ts#run".into(),
            "fn:r@a.ts#dup".into(),
            "CALLS".into(),
        )];
        assert_eq!(dup.props["site"], 1);
        assert_eq!(dup.props["prov"]["evidence"].as_array().unwrap().len(), 2);
        let mixed = &owned[&(
            "fn:r@a.ts#run".into(),
            "fn:r@a.ts#mixed".into(),
            "CALLS".into(),
        )];
        assert_eq!(mixed.props["prov"]["extractor_id"], "t0.other");
        assert_eq!(mixed.props["prov"]["evidence"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn system_summary_counts_a_channel_shared_by_two_repos_once() {
        // AC-0195 (#242 review): a channel is keyed globally
        // (`chan:{kind}:{identity}`), so two repos that publish to one queue
        // share one stored fact. Summing per-repo counts would count it
        // twice; the operation reports it once and states the repeat.
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let mut published = crate::OperationFacts::default();
        let mut per_repo_sum = (0u64, 0u64);
        let mut channel_ids = Vec::new();
        for repo in ["orders", "billing"] {
            let root = dir.path().join(repo);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("app.ts"),
                r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const sqs = new SQSClient({});
export function queue() {
  return sqs.send(new SendMessageCommand({ QueueUrl: 'https://sqs.us-east-1.amazonaws.com/9/orders', MessageBody: '{}' }));
}
"#,
            )
            .unwrap();
            let identity = format!("local/{repo}");
            let extraction = crate::extract_tree(
                &root,
                &identity,
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            channel_ids.extend(
                extraction
                    .nodes
                    .iter()
                    .filter(|node| node.label == "Channel")
                    .map(|node| node.id.clone()),
            );
            let loaded =
                crate::load_into_graph(&mut store, &extraction, &identity, &root, "workdir")
                    .unwrap()
                    .published;
            per_repo_sum.0 += loaded.nodes.len() as u64;
            per_repo_sum.1 += loaded.edges.len() as u64;
            published.record_load(loaded);
        }
        crate::stitch_backings(&mut store, &mut published, crate::Registries::builtin()).unwrap();

        assert_eq!(channel_ids.len(), 2, "each repo emits the shared channel");
        assert_eq!(channel_ids[0], channel_ids[1]);
        assert_eq!(
            store.fact_counts().unwrap(),
            (published.nodes(), published.edges())
        );
        assert!(
            per_repo_sum.0 > published.nodes(),
            "a per-repo sum double counts"
        );
        assert!(published.merged.nodes >= 1);
    }

    #[test]
    fn manifest_service_binding_joins_a_fetch_to_another_repos_endpoint() {
        // AC-0240/AC-0241 (#589): each repo is extracted alone, with the
        // manifest's bound origins deferred as in a system add, so a web
        // client's absolute-URL call to the API repo starts as a fetch Gap.
        // After both load, the declared service joins it to the API
        // endpoint; an undeclared host keeps its Gap with a reason.
        let dir = tempfile::tempdir().unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let mut published = crate::OperationFacts::default();
        let manifest_path = dir.path().join(ingest::manifest::MANIFEST_NAME);
        std::fs::write(
            &manifest_path,
            "[[repos]]\nurl = \"api\"\n[[repos]]\nurl = \"web\"\n[[services]]\nname = \"orders\"\nrepo = \"api\"\nbase_urls = [\"https://orders.example.com\"]\n",
        )
        .unwrap();
        let manifest = ingest::manifest::SystemManifest::load(&manifest_path).unwrap();
        let repo_keys = [("api", "local/api"), ("web", "local/web")]
            .into_iter()
            .map(|(url, key)| (url.to_string(), key.to_string()))
            .collect();
        let bindings = crate::manifest_http_bindings(&manifest, &repo_keys);
        assert_eq!(bindings.len(), 1);
        let deferred = events::binding_origins(&bindings);
        let files = [
            (
                "api",
                "server.ts",
                "import express from 'express';\nconst app = express();\napp.post('/v1/orders', (req, res) => {});\n",
            ),
            // #606 review: one serving-repo symbol calls the endpoint both
            // relatively and through its bound absolute URL.
            (
                "api",
                "sync.tsx",
                "export function sync() {\n  fetch('/v1/orders', { method: 'POST' });\n  fetch('https://orders.example.com/v1/orders', { method: 'POST' });\n}\n",
            ),
            (
                "web",
                "client.tsx",
                "export function Checkout() {\n  const submit = () => fetch('https://orders.example.com/v1/orders', { method: 'POST' });\n  const other = () => fetch('https://elsewhere.example.com/v1/orders', { method: 'POST' });\n  const again = () => fetch('https://orders.example.com/v1/orders', { method: 'POST' });\n  return <button onClick={submit}>Order</button>;\n}\n",
            ),
        ];
        for (repo, file, source) in files {
            let root = dir.path().join(repo);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(file), source).unwrap();
        }
        for repo in ["api", "web"] {
            let root = dir.path().join(repo);
            let identity = format!("local/{repo}");
            let (extraction, _, _) = crate::extract_tree_with_primary(
                &root,
                &identity,
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                &deferred,
                None,
                None,
                &[],
                &mut crate::RepoExtractionCache::default(),
                &[],
                &mut |_| {},
                None,
                &mut Vec::new(),
                &mut crate::ProducerReads::default(),
            )
            .unwrap();
            published.record_load(
                crate::load_into_graph(&mut store, &extraction, &identity, &root, "workdir")
                    .unwrap()
                    .published,
            );
        }
        let gaps_before = store.nodes_with_label("Gap").unwrap();
        let fetch_gaps: Vec<&Node> = gaps_before
            .iter()
            .filter(|gap| gap.id.starts_with("gap:fetch:"))
            .collect();
        assert_eq!(fetch_gaps.len(), 4, "{gaps_before:?}");

        assert_eq!(
            crate::stitch_http_bindings(&mut store, &bindings, &mut published).unwrap(),
            3
        );

        let fetches = store.edges_with_labels(&["FETCHES"]).unwrap();
        let evidence = |edge: &Edge| edge.props["prov"]["evidence"].as_array().unwrap().len();
        let sync: Vec<&Edge> = fetches
            .iter()
            .filter(|edge| edge.dst == "ep:local/api@POST:/v1/orders" && edge.src.contains("sync"))
            .collect();
        let [sync] = sync.as_slice() else {
            panic!("one serving-repo edge: {fetches:?}")
        };
        // The relative call's citation survives beside the bound one, and
        // the edge says the per-repo extractor resolved it first.
        assert_eq!(evidence(sync), 2, "{sync:?}");
        assert_eq!(
            sync.props["also_extracted_by"],
            serde_json::json!(["t0.events"])
        );
        let bound: Vec<&Edge> = fetches
            .iter()
            .filter(|edge| edge.dst == "ep:local/api@POST:/v1/orders" && edge.src.contains("web"))
            .collect();
        let [bound] = bound.as_slice() else {
            panic!("one bound edge: {fetches:?}")
        };
        assert_eq!(bound.props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(bound.props["binding"][0]["service"], "orders");
        // #606 review: both call sites of the one symbol are one edge whose
        // evidence cites each, not the last site alone.
        assert_eq!(
            bound.props["prov"]["evidence"].as_array().unwrap().len(),
            2,
            "{bound:?}"
        );
        assert_eq!(
            bound.props["binding"][0]["declared_in"],
            ingest::manifest::MANIFEST_NAME
        );
        let gaps_after = store.nodes_with_label("Gap").unwrap();
        let kept: Vec<&Node> = gaps_after
            .iter()
            .filter(|gap| gap.id.starts_with("gap:fetch:local/web@"))
            .collect();
        let [kept] = kept.as_slice() else {
            panic!("one kept gap: {gaps_after:?}")
        };
        assert_eq!(
            kept.props["reason"],
            "no binding evidence: no declared service or gateway route covers https://elsewhere.example.com/v1/orders"
        );
        assert_eq!(
            kept.props["t0_reason"],
            "no recovered endpoint matches POST /v1/orders"
        );
        // #1843 / AC-0660: the stored Gap and its Gap edge record that no
        // declared binding names the origin, so the boundary projection
        // lists the call as outside without reading the sentence.
        assert_eq!(
            kept.props[events::BINDING_STATE],
            events::BINDING_STATE_NONE
        );
        assert!(kept.props.get("binding").is_none());
        let kept_edge = store
            .edges_with_labels(&["FETCHES"])
            .unwrap()
            .into_iter()
            .find(|edge| edge.dst == kept.id)
            .unwrap();
        assert_eq!(
            kept_edge.props[events::BINDING_STATE],
            events::BINDING_STATE_NONE
        );
        // The operation's facts follow the store exactly.
        assert_eq!(
            store.fact_counts().unwrap(),
            (published.nodes(), published.edges())
        );
    }

    #[test]
    fn operation_facts_follow_a_whole_graph_patch() {
        // AC-0195 (#242 review): found-ADR relinking retracts and re-adds
        // facts after the load. The operation mirrors the store's patch
        // order, so a retracted node takes its incident edges with it and a
        // re-published key is neither lost nor counted as merged.
        let key = |src: &str, dst: &str| (src.to_string(), dst.to_string(), "DECIDES".to_string());
        let mut published = crate::OperationFacts::default();
        published.record_load(crate::PublishedFacts {
            nodes: ["adr:a", "adr:stale", "sym:x"].map(String::from).into(),
            edges: [key("adr:a", "sym:x"), key("adr:stale", "sym:x")].into(),
            merged: crate::MergedFacts::default(),
        });
        let node = |id: &str| Node {
            id: id.into(),
            label: "ADR".into(),
            props: serde_json::json!({}),
        };
        published.record_patch(&core_graph::GraphPatch {
            upsert_nodes: vec![node("adr:a"), node("adr:b")],
            upsert_edges: vec![Edge {
                src: "adr:b".into(),
                dst: "sym:x".into(),
                label: "DECIDES".into(),
                props: serde_json::json!({}),
            }],
            delete_node_ids: vec!["adr:stale".into()],
            delete_edges: vec![key("adr:a", "sym:x")],
            ..Default::default()
        });
        assert_eq!(
            published.nodes,
            ["adr:a", "adr:b", "sym:x"].map(String::from).into()
        );
        assert_eq!(published.edges, [key("adr:b", "sym:x")].into());
        assert_eq!(published.merged, crate::MergedFacts::default());
    }

    #[test]
    fn ingest_summary_includes_state_backed_backs_edges() {
        // AC-0195 (#242 review): BACKS edges are stitched after the load,
        // from observed state. The summary is taken after that stage, so it
        // still equals the store's counts.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tf"),
            "resource \"aws_sqs_queue\" \"orders\" {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const sqs = new SQSClient({});
export function queueOrder() {
  return sqs.send(new SendMessageCommand({ QueueUrl: 'https://sqs.us-east-1.amazonaws.com/9/orders', MessageBody: '{}' }));
}
"#,
        )
        .unwrap();
        let state = dir.path().join("shop.state.json");
        std::fs::write(
            &state,
            r#"{
  "format_version": "1.0",
  "values": { "root_module": { "resources": [{
    "address": "aws_sqs_queue.orders", "mode": "managed",
    "type": "aws_sqs_queue", "name": "orders",
    "values": { "url": "https://sqs.us-east-1.amazonaws.com/9/orders" },
    "sensitive_values": {}
  }] } }
}"#,
        )
        .unwrap();
        let extraction = crate::extract_tree(
            dir.path(),
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            Some(&state),
            None,
            &[],
        )
        .unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        let mut published = crate::OperationFacts::default();
        published.record_load(
            crate::load_into_graph(&mut store, &extraction, "local/shop", dir.path(), "workdir")
                .unwrap()
                .published,
        );
        let loaded_edges = published.edges();

        assert_eq!(
            crate::stitch_backings(&mut store, &mut published, crate::Registries::builtin())
                .unwrap(),
            1
        );
        assert_eq!(published.edges(), loaded_edges + 1);
        assert_eq!(
            store.fact_counts().unwrap(),
            (published.nodes(), published.edges())
        );
    }

    #[test]
    fn packed_recovery_helpers_match_the_decoded_reads() {
        // AC-0548 (#1574): the packed pipeline's id and count reads equal the
        // decoded ones. A value that merely mentions "placeholder" is decoded
        // and kept; a real placeholder flag is excluded.
        let node = |id: &str, label: &str, props: serde_json::Value| Node {
            id: id.into(),
            label: label.into(),
            props,
        };
        let nodes = vec![
            node("file:r@a.ts", "File", serde_json::json!({})),
            node(
                "file:r@b.ts",
                "File",
                serde_json::json!({ "note": "placeholder" }),
            ),
            node(
                "file:r@c.ts",
                "File",
                serde_json::json!({ "placeholder": true }),
            ),
            node("file:r@a.ts", "File", serde_json::json!({ "n": 2 })),
            node(
                "sym:r@a.ts#f",
                "Symbol",
                serde_json::json!({ "placeholder": false }),
            ),
            node("sym:r@a.ts#g", "Symbol", serde_json::json!({})),
        ];
        let edge = |src: &str, dst: &str, label: &str| Edge {
            src: src.into(),
            dst: dst.into(),
            label: label.into(),
            props: serde_json::json!({}),
        };
        let edges = vec![
            edge("file:r@a.ts", "file:r@b.ts", "IMPORTS"),
            edge("file:r@a.ts", "file:r@b.ts", "IMPORTS"),
            edge("file:r@a.ts", "file:r@b.ts", "CALLS"),
        ];
        let facts = super::FactStore::from_facts(nodes.clone(), edges.clone()).unwrap();
        for label in ["File", "Symbol", "Endpoint"] {
            let decoded: Vec<&str> = nodes
                .iter()
                .filter(|node| node.label == label && node.props.get("placeholder").is_none())
                .map(|node| node.id.as_str())
                .collect();
            assert_eq!(super::unplaceholdered(&facts, label).unwrap(), decoded);
            assert_eq!(
                super::unplaceholdered_set(&facts, label).unwrap(),
                decoded.iter().map(|id| id.to_string()).collect()
            );
        }
        assert_eq!(
            super::distinct_packed_node_count(facts.nodes()),
            super::distinct_node_count(&nodes)
        );
        assert_eq!(
            super::distinct_packed_edge_count(facts.edges()),
            super::distinct_edge_count(&edges)
        );
        assert_eq!(
            (
                super::distinct_node_count(&nodes),
                super::distinct_edge_count(&edges)
            ),
            (5, 2)
        );
    }

    #[test]
    fn packed_publication_keeps_value_equality_and_stored_rows() {
        // AC-0529: publication packs facts as they move. A fact whose stored
        // bytes spell an equal value differently is unchanged and not
        // rewritten, a repeated id with differing props is a collision, and
        // republishing the same extraction changes nothing. Floats the
        // default fast parser misrounds keep their exact value (#1525).
        let repo = "local/packed";
        let hard = serde_json::json!({
            "a": -2.481203691041865e275,
            "b": 51.248178375505404,
            "c": 1.0715660391465826e-75,
        });
        let next_up = f64::from_bits((-2.481203691041865e275_f64).to_bits() - 1);
        let node = |id: &str, props: serde_json::Value| Node {
            id: format!("sym:{repo}@{id}"),
            label: "Symbol".into(),
            props,
        };
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        store
            .put_node(&node("zero", serde_json::json!({"x": -0.0})))
            .unwrap();
        store.put_node(&node("float", hard.clone())).unwrap();
        let mut extraction = adapters_lang_ts::Extraction::default();
        extraction.nodes = vec![
            node("zero", serde_json::json!({"x": 0.0})),
            node("dup", serde_json::json!({"n": 1})),
            node("dup", serde_json::json!({"n": 2})),
            node("float", hard.clone()),
            node("ulp", serde_json::json!({"a": -2.481203691041865e275})),
            node("ulp", serde_json::json!({"a": next_up})),
            node(
                "other",
                serde_json::json!({"nested": {"b": [1, 2.5], "a": null}}),
            ),
        ];
        extraction.edges = vec![
            Edge {
                src: format!("sym:{repo}@dup"),
                dst: format!("sym:{repo}@other"),
                label: "CALLS".into(),
                props: serde_json::json!({"w": 1}),
            };
            2
        ];
        let dir = tempfile::tempdir().unwrap();
        let first =
            crate::load_into_graph(&mut store, &extraction, repo, dir.path(), "c1").unwrap();
        assert_eq!(
            first.unchanged, 2,
            "-0.0 and 0.0 are the same value; exact floats are unchanged"
        );
        assert_eq!(
            first.published.merged.node_collisions, 2,
            "a one-ulp difference is a different value"
        );
        assert_eq!(first.published.merged.nodes, 2);
        assert_eq!(first.published.merged.edges, 1);
        let zero: String = store
            .read_packed_snapshot()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.id.ends_with("@zero"))
            .unwrap()
            .props_json()
            .to_owned();
        assert_eq!(zero, r#"{"x":-0.0}"#, "an unchanged fact is not rewritten");
        let (nodes, _) = store.read_snapshot().unwrap();
        let dup = nodes.iter().find(|n| n.id.ends_with("@dup")).unwrap();
        assert_eq!(
            dup.props,
            serde_json::json!({"n": 2}),
            "last occurrence wins"
        );
        let float = nodes.iter().find(|n| n.id.ends_with("@float")).unwrap();
        assert_eq!(float.props, hard);
        let ulp = nodes.iter().find(|n| n.id.ends_with("@ulp")).unwrap();
        assert_eq!(
            ulp.props["a"].as_f64().map(f64::to_bits),
            Some(next_up.to_bits())
        );

        let again =
            crate::load_into_graph(&mut store, &extraction, repo, dir.path(), "c1").unwrap();
        assert_eq!(again.inserted_or_updated, 0);
        assert_eq!(again.deleted, 0);
        assert_eq!(again.unchanged, first.unchanged + first.inserted_or_updated);
    }

    #[test]
    fn toolchain_facts_land_in_the_graph_with_defined_in_edges() {
        // AC-0096 (#215): config files become Tool nodes with cited
        // settings, DEFINED_IN the config File node — end to end through
        // the extraction pipeline and the FK-enforcing graph load.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{ "name": "shop", "type": "module", "dependencies": { "react": "^19" } }"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("app.ts"), "export function run() {}\n").unwrap();
        // The vite config is a .ts source file: the TS adapter owns its
        // File node, and the toolchain must reuse it rather than clobber.
        std::fs::write(
            dir.path().join("vite.config.ts"),
            "export default { base: '/' };\n",
        )
        .unwrap();
        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/tools",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert!(summary.tools.files >= 2);
        assert!(summary.tools.nodes > 0);

        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        crate::load_into_graph(
            &mut graph,
            &extraction,
            "local/tools",
            dir.path(),
            "workdir",
        )
        .unwrap();
        let react = graph.get_node("tool:local/tools@react").unwrap().unwrap();
        assert_eq!(react.label, "Tool");
        assert_eq!(react.props["settings"]["requirement"], "^19");
        let edges = graph.all_edges().unwrap();
        assert!(edges.iter().any(|edge| edge.src == "tool:local/tools@react"
            && edge.dst == "file:local/tools@package.json"
            && edge.label == "DEFINED_IN"));
        // Presence-only code config: Tool node exists, DEFINED_IN targets
        // the TS adapter's own File node (no duplicate/clobbered file).
        let vite = graph
            .get_node("tool:local/tools@vite.config.ts")
            .unwrap()
            .unwrap();
        assert_eq!(vite.props["settings_behind_code"], true);
        let vite_file = graph
            .get_node("file:local/tools@vite.config.ts")
            .unwrap()
            .unwrap();
        assert!(
            vite_file.props.get("config").is_none(),
            "adapter-owned File node survives the toolchain pass"
        );
        assert!(
            edges
                .iter()
                .any(|edge| edge.src == "tool:local/tools@vite.config.ts"
                    && edge.dst == "file:local/tools@vite.config.ts"
                    && edge.label == "DEFINED_IN")
        );
    }

    #[test]
    fn pipeline_facts_land_in_the_graph_in_declared_order() {
        // AC-0598 (#1790): a normal extraction turns a workflow and a
        // Dockerfile into ordered Pipeline facts, through the FK-enforcing
        // graph load, without adding a hop to any flow.
        let dir = tempfile::tempdir().unwrap();
        let workflows = dir.path().join(".github/workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(
            workflows.join("ci.yml"),
            "on:\n  schedule:\n    - cron: '0 3 * * *'\njobs:\n  build:\n    steps:\n      - run: make\n  \
             deploy:\n    needs: build\n    steps:\n      - uses: acme/deploy@v1\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Dockerfile"),
            "FROM node:22 AS build\nRUN make\nFROM scratch\nCOPY --from=build /out /out\n",
        )
        .unwrap();
        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/ship",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.pipelines.files, 2);

        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        crate::load_into_graph(&mut graph, &extraction, "local/ship", dir.path(), "workdir")
            .unwrap();
        let job = |key: &str| format!("pipeline-job:local/ship@.github/workflows/ci.yml#{key}");
        let deploy = graph.get_node(&job("deploy")).unwrap().unwrap();
        assert_eq!(deploy.label, "PipelineJob");
        assert_eq!(deploy.props["index"], 1);
        assert_eq!(deploy.props["depth"], 1);
        assert_eq!(deploy.props["prov"]["extractor_id"], "t0.pipeline");
        let step = graph
            .get_node("pipeline-step:local/ship@.github/workflows/ci.yml#deploy/0")
            .unwrap()
            .unwrap();
        assert_eq!(step.props["uses"], "acme/deploy@v1");
        let edges = graph.all_edges().unwrap();
        assert!(edges.iter().any(|edge| edge.src == job("deploy")
            && edge.dst == job("build")
            && edge.label == "NEEDS"));
        assert!(edges.iter().any(
            |edge| edge.src == "pipeline-job:local/ship@Dockerfile#stage-1"
                && edge.dst == "pipeline-job:local/ship@Dockerfile#stage-0"
                && edge.label == "NEEDS"
        ));
        // No pipeline fact uses a label the flow tracer walks: the scheduled
        // workflow's flow is the one the schedule reader alone produces.
        assert!(
            edges
                .iter()
                .filter(|edge| edge.props["prov"]["extractor_id"] == "t0.pipeline")
                .all(|edge| !flowtracer::FLOW_EDGE_LABELS.contains(&edge.label.as_str()))
        );
    }

    #[test]
    fn webextension_manifest_ingest_reports_layer_and_security_grants() {
        // US-0016: a Manifest V3 extension yields deterministic topology,
        // entry bindings against .ts sources, and exact-scope GRANTS that
        // the security projection can flag — reported as its own layer.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/background")).unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{
  "manifest_version": 3,
  "name": "Image Trail",
  "version": "0.10.1",
  "background": { "service_worker": "src/background/service-worker.js" },
  "permissions": ["activeTab", "storage"],
  "optional_host_permissions": ["http://*/*"]
}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/background/service-worker.ts"),
            "export function main(): void {}\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/image-trail",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();

        assert_eq!(summary.webext.files, 1);
        assert!(summary.webext.nodes > 0);
        let ext = extraction
            .nodes
            .iter()
            .find(|node| node.label == "Extension")
            .expect("extension node");
        assert_eq!(ext.props["prov"]["extractor_id"], "t0.webextension");
        // The declared .js entry binds to the extracted .ts File node —
        // one node id shared between the manifest fact and the TS pass.
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "ENTRY"
                && edge.dst == "file:local/image-trail@src/background/service-worker.ts"
        }));
        assert_eq!(
            extraction
                .nodes
                .iter()
                .filter(|node| node.id == "file:local/image-trail@src/background/service-worker.ts")
                .count(),
            1,
            "manifest binding must reuse the TS pass's File node, not duplicate it"
        );
        // Wildcard host scope is an exact, projectable GRANTS fact.
        let grant = extraction
            .edges
            .iter()
            .find(|edge| edge.label == "GRANTS" && edge.dst.ends_with("host:http://*/*"))
            .expect("host grant");
        assert_eq!(grant.props["resource_scopes"][0], "http://*/*");
    }

    #[test]
    fn chrome_messaging_stitches_channels_across_extension_contexts() {
        // US-0016/AC-0072: literal + const-map message identities become
        // Confirmed chrome-message channels with PUBLISHES/SUBSCRIBES edges
        // connecting content script and service worker; a runtime-computed
        // identity stays an explicit Gap.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/protocol.ts"),
            "export const MessageType = { Capture: 'ext.capture' } as const;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/content.ts"),
            "import { MessageType } from './protocol.js';\n\
             export function capture(kind: string) {\n\
               void chrome.runtime.sendMessage({ type: MessageType.Capture });\n\
               void chrome.runtime.sendMessage({ type: `ext.${kind}` });\n\
             }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/worker.ts"),
            "import { MessageType } from './protocol.js';\n\
             export const handlers = {\n\
               [MessageType.Capture]: () => 'ok',\n\
             };\n\
             chrome.runtime.onMessage.addListener(() => true);\n",
        )
        .unwrap();

        let (extraction, _) = crate::extract_tree_with_summary(
            dir.path(),
            "local/ext",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();

        let channel = extraction
            .nodes
            .iter()
            .find(|node| node.id == "chan:chrome-message:ext.capture")
            .expect("confirmed chrome-message channel");
        assert_eq!(channel.props["prov"]["confidence_tier"], "Confirmed");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "PUBLISHES"
                && edge.src == "sym:local/ext@src/content.ts#capture"
                && edge.dst == channel.id
        }));
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "SUBSCRIBES"
                && edge.src == "file:local/ext@src/worker.ts"
                && edge.dst == channel.id
        }));
        // The template-string identity bottoms out as an explicit Gap.
        assert!(extraction.nodes.iter().any(|node| {
            node.label == "Gap"
                && node.props["kind"] == "chrome-message"
                && node.props["reason"] == "runtime-computed channel identity"
        }));
    }

    #[test]
    fn indexeddb_data_model_joins_the_graph() {
        // US-0016/AC-0073: store declarations plus repository operations
        // yield a cited DataEntity with READS/WRITES relations.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/schema.ts"),
            "export const DataStore = { History: 'history' } as const;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/repo.ts"),
            "import { DataStore } from './schema.js';\n\
             export function save(tx: IDBTransaction, record: unknown) {\n\
               tx.objectStore(DataStore.History).put(record);\n\
             }\n",
        )
        .unwrap();

        let (extraction, _) = crate::extract_tree_with_summary(
            dir.path(),
            "local/ext",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();

        let entity = extraction
            .nodes
            .iter()
            .find(|node| node.id == "data:local/ext@idb:history")
            .expect("data entity");
        assert_eq!(entity.props["prov"]["confidence_tier"], "Confirmed");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "WRITES"
                && edge.src == "sym:local/ext@src/repo.ts#save"
                && edge.dst == entity.id
        }));
    }

    #[test]
    fn java_jpa_data_model_compiles_into_data_model_artifact() {
        // AC-0284 (#239): JPA entities, their relations and Spring Data
        // repository access reach data_model.md with cited assertions, and
        // the verified-only bundle is identical across repeat ingest.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/main/java/com/demo")).unwrap();
        for (name, source) in [
            (
                "Owner.java",
                "package com.demo;\n\
                 import jakarta.persistence.*;\n\
                 import java.util.List;\n\
                 @Entity @Table(name = \"owners\")\n\
                 public class Owner {\n\
                   @Id private Integer id;\n\
                   @OneToMany @JoinColumn(name = \"owner_id\") private List<Pet> pets;\n\
                 }\n",
            ),
            (
                "Pet.java",
                "package com.demo;\n\
                 import jakarta.persistence.*;\n\
                 @Entity\n\
                 public class Pet {\n\
                   @Id private Integer id;\n\
                   @ManyToOne private Owner owner;\n\
                 }\n",
            ),
            (
                "OwnerRepository.java",
                "package com.demo;\n\
                 import org.springframework.data.repository.Repository;\n\
                 public interface OwnerRepository extends Repository<Owner, Integer> {\n\
                   Owner save(Owner owner);\n\
                 }\n",
            ),
            (
                "OwnerService.java",
                "package com.demo;\n\
                 public class OwnerService {\n\
                   private final OwnerRepository owners;\n\
                   public OwnerService(OwnerRepository owners) { this.owners = owners; }\n\
                   public void rename(Owner owner) { owners.save(owner); }\n\
                 }\n",
            ),
        ] {
            std::fs::write(dir.path().join("src/main/java/com/demo").join(name), source).unwrap();
        }
        let compile = || {
            let (extraction, _) = crate::extract_tree_with_summary(
                dir.path(),
                "local/petclinic",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            spec::compile_spec(
                &extraction.nodes,
                &extraction.edges,
                &[],
                spec::ExportMode::VerifiedOnly,
                &std::collections::BTreeSet::new(),
            )
        };
        let bundle = compile();
        let data = bundle
            .artifacts
            .iter()
            .find(|artifact| artifact.file_name == "data_model.md")
            .expect("data_model.md");
        assert!(
            !data.content.contains("No DataEntity facts"),
            "{}",
            data.content
        );
        for fact in [
            "data:local/petclinic@jpa:Owner",
            "data:local/petclinic@jpa:Pet",
            "| `data:local/petclinic@jpa:Owner` | REFERENCES | `data:local/petclinic@jpa:Pet` |",
            "| `data:local/petclinic@jpa:Pet` | REFERENCES | `data:local/petclinic@jpa:Owner` |",
            "WRITES",
        ] {
            assert!(data.content.contains(fact), "{fact}: {}", data.content);
        }
        assert!(!data.assertions.is_empty());
        assert!(
            data.assertions
                .iter()
                .all(|assertion| !assertion.provenance.evidence.is_empty())
        );
        assert_eq!(
            serde_json::to_string(&bundle).unwrap(),
            serde_json::to_string(&compile()).unwrap(),
            "verified-only bundle must be identical across repeat ingest"
        );
    }

    #[test]
    fn webextension_dogfood_compiles_a_useful_deterministic_spec() {
        // US-0016/AC-0074: a full fixture extension — manifest, messaging,
        // IndexedDB — compiles into artifacts with useful cited assertions,
        // and the verified-only bundle is byte-identical across repeat
        // ingest of the same tree.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/background")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/content")).unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{
  "manifest_version": 3,
  "name": "Fixture Trail",
  "version": "1.0.0",
  "action": { "default_title": "Toggle" },
  "background": { "service_worker": "src/background/worker.js" },
  "permissions": ["storage"],
  "optional_host_permissions": ["http://*/*"]
}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/protocol.ts"),
            "export const MessageType = { Capture: 'ext.capture' } as const;\n\
             export const DataStore = { History: 'history' } as const;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/content/content.ts"),
            "import { MessageType } from '../protocol.js';\n\
             export function capture(kind: string) {\n\
               void chrome.runtime.sendMessage({ type: MessageType.Capture });\n\
               void chrome.runtime.sendMessage({ type: `ext.${kind}` });\n\
             }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/background/worker.ts"),
            "import { MessageType, DataStore } from '../protocol.js';\n\
             export function persist(tx: IDBTransaction, record: unknown) {\n\
               tx.objectStore(DataStore.History).put(record);\n\
             }\n\
             export const handlers = {\n\
               [MessageType.Capture]: () => 'ok',\n\
             };\n\
             chrome.runtime.onMessage.addListener(() => true);\n",
        )
        .unwrap();

        let compile = || {
            let (extraction, _) = crate::extract_tree_with_summary(
                dir.path(),
                "local/fixture-trail",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            spec::compile_spec(
                &extraction.nodes,
                &extraction.edges,
                &[],
                spec::ExportMode::VerifiedOnly,
                &std::collections::BTreeSet::new(),
            )
        };
        let bundle = compile();

        let artifact = |name: &str| {
            bundle
                .artifacts
                .iter()
                .find(|artifact| artifact.file_name == name)
                .unwrap_or_else(|| panic!("{name} artifact"))
        };
        // Data model: the IndexedDB entity with its cited WRITES relation.
        let data = artifact("data_model.md");
        assert!(
            data.content
                .contains("data:local/fixture-trail@idb:history"),
            "data model names the store: {}",
            data.content
        );
        // Security view: the wildcard optional host permission is a finding.
        let security = artifact("security.md");
        assert!(
            security.content.contains("http://*/*"),
            "wildcard host grant projected: {}",
            security.content
        );
        // Gap register: the runtime-computed message identity is explicit.
        let gaps = artifact("gap_register.md");
        assert!(
            gaps.content.contains("runtime-computed channel identity"),
            "computed message identity is an explicit gap: {}",
            gaps.content
        );
        assert!(bundle.assertion_count > 0);

        // Determinism: a second independent ingest compiles byte-identically.
        let again = compile();
        assert_eq!(
            serde_json::to_string(&bundle).unwrap(),
            serde_json::to_string(&again).unwrap(),
            "verified-only bundle must be identical across repeat ingest"
        );
    }

    #[test]
    #[ignore = "manual dogfood (MT-DF-01): set CARTOGRAPH_DOGFOOD_ROOT to an image-trail checkout"]
    fn dogfood_extraction_against_local_image_trail_checkout() {
        let root = std::env::var("CARTOGRAPH_DOGFOOD_ROOT").expect("checkout path");
        let root = std::path::Path::new(&root);
        let compile = || {
            let (extraction, summary) = crate::extract_tree_with_summary(
                root,
                "qwts/image-trail",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            (
                spec::compile_spec(
                    &extraction.nodes,
                    &extraction.edges,
                    &[],
                    spec::ExportMode::VerifiedOnly,
                    &std::collections::BTreeSet::new(),
                ),
                extraction,
                summary,
            )
        };
        let (bundle, extraction, summary) = compile();
        let count = |label: &str| {
            extraction
                .nodes
                .iter()
                .filter(|node| node.label == label)
                .count()
        };
        println!(
            "layers: ts {} files / webext {} manifests · nodes {} edges {}",
            summary.ts.files,
            summary.webext.files,
            extraction.nodes.len(),
            extraction.edges.len()
        );
        println!(
            "extension {} · contexts {} · commands {} · permissions {} · channels {} · data entities {} · gaps {}",
            count("Extension"),
            count("ExtensionContext"),
            count("Command"),
            count("Permission"),
            count("Channel"),
            count("DataEntity"),
            count("Gap"),
        );
        for artifact in &bundle.artifacts {
            println!(
                "{}: {} assertions",
                artifact.file_name,
                artifact.assertions.len()
            );
        }
        assert!(summary.webext.files >= 1, "manifest recognized");
        assert!(count("Extension") >= 1);
        assert!(count("ExtensionContext") >= 1);
        assert!(count("Permission") >= 1);
        assert!(count("DataEntity") >= 1, "IndexedDB stores recovered");
        // Determinism against the real tree, not only the fixture.
        let (again, _, _) = compile();
        assert_eq!(
            serde_json::to_string(&bundle).unwrap(),
            serde_json::to_string(&again).unwrap(),
        );
    }

    #[test]
    fn python_server_ingest_reports_layer_and_endpoints() {
        // AC-0053/T-0053: the app runs the import-proven Python pass for the
        // server layer and reports it independently from TypeScript.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("api.py"),
            "from fastapi import FastAPI\n\napp = FastAPI()\n\n@app.get('/orders')\ndef orders():\n    return []\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/python-app",
            "workdir",
            &["server".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();

        assert_eq!(summary.python.files, 1);
        assert!(summary.python.nodes > 0);
        assert_eq!(summary.ts, crate::LayerSummary::default());
        assert_eq!(summary.tf, crate::LayerSummary::default());
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.id == "ep:local/python-app@GET:/orders")
            .expect("FastAPI endpoint");
        assert_eq!(endpoint.props["language"], "python");
        assert_eq!(endpoint.props["framework"], "fastapi");
        assert_eq!(endpoint.props["prov"]["extractor_id"], "t0.adapter-python");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "HANDLES"
                && edge.src == endpoint.id
                && edge.dst == "sym:local/python-app@api.py#orders"
        }));

        let (client_only, client_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/python-app",
            "workdir",
            &["client".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        // AC-0458: Python also carries client fetch sites and event sites,
        // so client- and events-only hints run it, as they do Java and Go.
        assert_eq!(client_summary.python.files, 1);
        assert!(
            client_only
                .nodes
                .iter()
                .any(|node| node.id == "ep:local/python-app@GET:/orders")
        );
        std::fs::write(
            dir.path().join("produce.py"),
            "from kafka import KafkaProducer\n\nproducer = KafkaProducer()\n\ndef publish():\n    producer.send('orders', b'1')\n",
        )
        .unwrap();
        let (events_only, events_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/python-app",
            "workdir",
            &["events".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(events_summary.python.files, 2);
        assert!(
            events_only
                .event_sites
                .iter()
                .any(|site| site.kind == "kafka-topic"
                    && site.identity
                        == adapters_fw::events::IdentityExpr::Literal("orders".into()))
        );
    }

    // AC-0313 (#739): repo recovery reads `.cartograph/api-docs.toml`, parses
    // the declared Markdown API reference into DocumentedRoute facts, compares
    // them with recovered FastAPI Endpoint nodes, and reports contract drift in
    // `drift_register.md` with both sides cited.
    #[test]
    fn python_server_ingest_recovers_contract_drift_and_register() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cartograph")).unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join(".cartograph/api-docs.toml"),
            "[[doc]]\npath = \"docs/api.md\"\nprefix = \"/api\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("docs/api.md"),
            "# API Reference\n\n#### GET /api/orders/:order_id\n#### POST /api/orders\n#### DELETE /api/legacy/:id\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("api.py"),
            concat!(
                "from fastapi import FastAPI\n\n",
                "app = FastAPI()\n\n",
                "@app.get('/api/orders/{id}')\n",
                "def get_order():\n    return {}\n\n",
                "@app.put('/api/orders')\n",
                "def replace_orders():\n    return {}\n\n",
                "@app.get('/api/internal/health')\n",
                "def health():\n    return {}\n",
            ),
        )
        .unwrap();

        let (extraction, _) = crate::extract_tree_with_summary(
            dir.path(),
            "local/contract-app",
            "workdir",
            &["server".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();

        let drifts: Vec<&Node> = extraction
            .nodes
            .iter()
            .filter(|n| spec::is_drift_node(n))
            .collect();
        assert_eq!(drifts.len(), 3, "{drifts:?}");

        let bundle = spec::compile_spec(
            &extraction.nodes,
            &extraction.edges,
            &[],
            spec::ExportMode::VerifiedOnly,
            &std::collections::BTreeSet::new(),
        );
        assert_eq!(bundle.drift_count, 3);
        let drift_reg = bundle
            .artifacts
            .iter()
            .find(|a| a.id == "drift-register")
            .expect("drift_register.md");
        assert!(drift_reg.content.contains("## API contract drift"));
        assert!(
            drift_reg
                .content
                .contains("| documented-not-served | `DELETE /api/legacy/{}` |")
        );
        assert!(
            drift_reg
                .content
                .contains("| method-mismatch | `/api/orders` |")
        );
        assert!(
            drift_reg
                .content
                .contains("| served-undocumented | `GET /api/internal/health` |")
        );
    }

    // AC-1804 (#1653): the project's API doc bounds reach recovery. Under a
    // byte bound smaller than the declared reference, the document is a
    // contract gap naming that bound and yields no documented routes; the
    // saved bounds are what an operation reads.
    #[test]
    fn a_projects_api_doc_bounds_reach_recovery() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".cartograph")).unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(
            dir.path().join(".cartograph/api-docs.toml"),
            "[[doc]]\npath = \"docs/api.md\"\nprefix = \"/api\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("docs/api.md"),
            "# API Reference\n\n#### GET /api/orders\n",
        )
        .unwrap();
        let root = super::paths::canonicalize(dir.path()).unwrap();
        let recover = |limits| {
            let (_, facts, _, _) = super::extract_tree_packed(
                &root,
                "local/bounded-docs",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                &std::collections::BTreeSet::new(),
                &std::collections::BTreeSet::new(),
                &super::observation_inputs::ObservationInputs::default(),
                &[],
                super::RecoveryLimits {
                    api_docs: limits,
                    ..Default::default()
                },
                &mut super::RepoExtractionCache::default(),
                &[],
                super::Registries::builtin(),
                &mut |_| {},
                None,
                &mut Vec::new(),
                &mut super::ProducerReads::default(),
            )
            .unwrap();
            facts.into_facts().unwrap().0
        };
        let label = |nodes: &[Node], prefix: &str| {
            nodes
                .iter()
                .filter(|node| node.id.starts_with(prefix))
                .cloned()
                .collect::<Vec<_>>()
        };
        let read = recover(ingest::api_docs::ApiDocLimits::default());
        assert_eq!(label(&read, "doc-route:").len(), 1);
        assert!(label(&read, "gap:contract:").is_empty());

        let tight = ingest::api_docs::ApiDocLimits::within(16, 8).unwrap();
        let refused = recover(tight);
        assert!(label(&refused, "doc-route:").is_empty());
        let gaps = label(&refused, "gap:contract:");
        assert_eq!(gaps.len(), 1);
        assert!(
            gaps[0]
                .props
                .to_string()
                .contains("exceeds the 8 byte limit"),
            "{gaps:?}"
        );

        let app_data = tempfile::tempdir().unwrap();
        let state = crate::registered_source_tests::app_state(app_data.path());
        assert_eq!(
            super::api_doc_limits(&state).unwrap(),
            ingest::api_docs::ApiDocLimits::default()
        );
        {
            let mut settings = state.settings.lock().unwrap();
            settings
                .set_operator_limit(crate::operator_limits::API_DOCS_MAX_DOCS, Some(64))
                .unwrap();
            settings
                .set_operator_limit(crate::operator_limits::API_DOCS_MAX_MIB, Some(8))
                .unwrap();
        }
        assert_eq!(
            super::api_doc_limits(&state).unwrap(),
            ingest::api_docs::ApiDocLimits::within(64, 8 << 20).unwrap()
        );
    }

    // AC-1807 / #1653: the semantic operation limits a preview reads are the
    // defaults until saved, then the saved values.
    #[test]
    fn a_projects_semantic_operation_limits_reach_the_preview() {
        use semantic::embedding_cache::OperationLimits;
        let app_data = tempfile::tempdir().unwrap();
        let state = crate::registered_source_tests::app_state(app_data.path());
        assert_eq!(
            super::semantic_operation_limits(&state).unwrap(),
            OperationLimits::default()
        );
        {
            let mut settings = state.settings.lock().unwrap();
            settings
                .set_operator_limit(
                    crate::operator_limits::SEMANTIC_OPERATION_MAX_ROWS,
                    Some(65_536),
                )
                .unwrap();
            settings
                .set_operator_limit(
                    crate::operator_limits::SEMANTIC_OPERATION_MAX_VECTOR_MIB,
                    Some(256),
                )
                .unwrap();
        }
        assert_eq!(
            super::semantic_operation_limits(&state).unwrap(),
            OperationLimits::within(65_536, 256 << 20).unwrap()
        );
    }

    // AC-1805 (#1653): the project's Python bounds reach recovery. Under a
    // revision bound below the history's length, the Alembic replay is a
    // gap naming the bound; the defaults replay it, and the saved keys are
    // what an operation reads.
    #[test]
    fn a_projects_python_bounds_reach_recovery() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("migrations/versions")).unwrap();
        std::fs::write(
            dir.path().join("alembic.ini"),
            "[alembic]\nscript_location = migrations\n",
        )
        .unwrap();
        for (name, id, down) in [("a1.py", "a1", "None"), ("b2.py", "b2", "'a1'")] {
            std::fs::write(
                dir.path().join("migrations/versions").join(name),
                format!(
                    "from alembic import op\nimport sqlalchemy as sa\n\nrevision = '{id}'\ndown_revision = {down}\n\n\ndef upgrade():\n    op.create_table('t_{id}', sa.Column('id', sa.Integer()))\n"
                ),
            )
            .unwrap();
        }
        let root = super::paths::canonicalize(dir.path()).unwrap();
        let recover = |python| {
            let (_, facts, _, _) = super::extract_tree_packed(
                &root,
                "local/bounded-py",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                &std::collections::BTreeSet::new(),
                &std::collections::BTreeSet::new(),
                &super::observation_inputs::ObservationInputs::default(),
                &[],
                super::RecoveryLimits {
                    python,
                    ..Default::default()
                },
                &mut super::RepoExtractionCache::default(),
                &[],
                super::Registries::builtin(),
                &mut |_| {},
                None,
                &mut Vec::new(),
                &mut super::ProducerReads::default(),
            )
            .unwrap();
            let mut reasons = facts
                .into_facts()
                .unwrap()
                .0
                .into_iter()
                .filter(|node| node.label == "Gap" && node.props["kind"] == "migration-gap")
                .map(|node| node.props["reason"].as_str().unwrap().to_string())
                .collect::<Vec<_>>();
            reasons.sort();
            reasons
        };
        assert!(recover(adapters_lang_python::PythonLimits::default()).is_empty());
        let one = adapters_lang_python::PythonLimits::within(1, 1 << 20, 8 << 20).unwrap();
        assert_eq!(
            recover(one),
            ["Alembic history not replayed: more than 1 revision files"]
        );

        let app_data = tempfile::tempdir().unwrap();
        let state = crate::registered_source_tests::app_state(app_data.path());
        assert_eq!(
            super::recovery_limits(&state).unwrap(),
            super::RecoveryLimits::default()
        );
        {
            let mut settings = state.settings.lock().unwrap();
            for (key, value) in [
                (crate::operator_limits::PYTHON_ALEMBIC_MAX_REVISIONS, 16_384),
                (crate::operator_limits::PYTHON_ALEMBIC_MAX_FILE_MIB, 4),
                (crate::operator_limits::PYTHON_LOCKFILE_MAX_MIB, 64),
            ] {
                settings.set_operator_limit(key, Some(value)).unwrap();
            }
        }
        assert_eq!(
            super::recovery_limits(&state).unwrap().python,
            adapters_lang_python::PythonLimits::within(16_384, 4 << 20, 64 << 20).unwrap()
        );
    }

    #[test]
    fn go_server_ingest_reports_layer_and_endpoints() {
        // AC-0054/T-0054: the app runs Go for application scope (its client
        // calls are client facts, AC-0434) and reports its facts
        // independently from the other deterministic languages.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/api\n").unwrap();
        std::fs::write(
            dir.path().join("main.go"),
            "package main\n\nimport \"net/http\"\n\nfunc orders(w http.ResponseWriter, r *http.Request) {}\nfunc routes() { http.HandleFunc(\"GET /orders\", orders) }\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/go-app",
            "workdir",
            &["server".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.go.files, 1);
        assert!(summary.go.nodes > 0);
        assert_eq!(summary.ts, crate::LayerSummary::default());
        assert_eq!(summary.python, crate::LayerSummary::default());
        assert_eq!(summary.tf, crate::LayerSummary::default());
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.id == "ep:local/go-app@GET:/orders")
            .expect("net/http endpoint");
        assert_eq!(endpoint.props["language"], "go");
        assert_eq!(endpoint.props["framework"], "net/http");
        assert_eq!(endpoint.props["prov"]["extractor_id"], "t0.adapter-go");

        let (_, client_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/go-app",
            "workdir",
            &["client".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(client_summary.go, summary.go);
        let (infra_only, infra_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/go-app",
            "workdir",
            &["infra".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(infra_summary.go, crate::LayerSummary::default());
        assert!(
            infra_only
                .nodes
                .iter()
                .all(|node| node.props["language"] != "go")
        );
    }

    #[test]
    fn java_server_ingest_reports_layer_and_endpoints() {
        // AC-0079/AC-0080: the app runs the annotation-proven Java pass for
        // the server layer and reports it independently.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/com/demo")).unwrap();
        std::fs::write(
            dir.path().join("src/com/demo/UserController.java"),
            "package com.demo;\n\nimport org.springframework.web.bind.annotation.*;\n\n@RestController\n@RequestMapping(\"/api\")\npublic class UserController {\n    @GetMapping(\"/users\")\n    public String users() { return \"[]\"; }\n}\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/java-app",
            "workdir",
            &["server".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.java.files, 1);
        assert!(summary.java.nodes > 0);
        assert_eq!(summary.ts, crate::LayerSummary::default());
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.id == "ep:local/java-app@GET:/api/users")
            .expect("Spring endpoint");
        assert_eq!(endpoint.props["language"], "java");
        assert_eq!(endpoint.props["framework"], "spring");
        assert_eq!(endpoint.props["prov"]["extractor_id"], "t0.adapter-java");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "HANDLES"
                && edge.src == endpoint.id
                && edge.dst
                    == "sym:local/java-app@src/com/demo/UserController.java#UserController.users()"
        }));

        let (client_only, client_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/java-app",
            "workdir",
            &["client".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        // #757 review: Java carries client calls (Retrofit), so a
        // client-only repository runs the pass; an infra-only one does not.
        assert_eq!(client_summary.java.files, 1);
        assert!(
            client_only
                .nodes
                .iter()
                .any(|node| node.props["language"] == "java")
        );
        let (infra_only, infra_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/java-app",
            "workdir",
            &["infra".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(infra_summary.java, crate::LayerSummary::default());
        assert!(
            infra_only
                .nodes
                .iter()
                .all(|node| node.props["language"] != "java")
        );
    }

    #[test]
    fn kotlin_server_ingest_reports_layer_and_endpoints() {
        // AC-0098: the app runs the annotation-proven Kotlin pass for the
        // server layer and reports it independently.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/com/demo")).unwrap();
        std::fs::write(
            dir.path().join("src/com/demo/UserController.kt"),
            "package com.demo\n\nimport org.springframework.web.bind.annotation.*\n\n@RestController\n@RequestMapping(\"/api\")\nclass UserController {\n    @GetMapping(\"/users\")\n    fun users(): String = \"[]\"\n}\n",
        )
        .unwrap();

        let (extraction, summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/kotlin-app",
            "workdir",
            &["server".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(summary.kotlin.files, 1);
        assert!(summary.kotlin.nodes > 0);
        assert_eq!(summary.ts, crate::LayerSummary::default());
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.id == "ep:local/kotlin-app@GET:/api/users")
            .expect("Spring endpoint");
        assert_eq!(endpoint.props["language"], "kotlin");
        assert_eq!(endpoint.props["framework"], "spring");
        assert_eq!(endpoint.props["prov"]["extractor_id"], "t0.adapter-kotlin");
        assert!(extraction.edges.iter().any(|edge| {
            edge.label == "HANDLES"
                && edge.src == endpoint.id
                && edge.dst
                    == "sym:local/kotlin-app@src/com/demo/UserController.kt#UserController.users"
        }));

        // #757 review: Kotlin carries client calls (Retrofit), so a
        // client-only repository runs the pass too; an infra-only one does not.
        std::fs::write(
            dir.path().join("src/com/demo/Api.kt"),
            "package com.demo\n\nimport retrofit2.http.GET\n\ninterface Api {\n    @GET(\"api/stats\")\n    fun stats(): String\n}\n",
        )
        .unwrap();
        let (client_only, client_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/kotlin-app",
            "workdir",
            &["client".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(client_summary.kotlin.files, 2);
        // #874: the client call's stitched FETCHES fact is the stitching
        // pass's own, not a TypeScript adapter count in a repo without TS.
        assert_eq!(client_summary.ts, crate::LayerSummary::default());
        assert_eq!(client_summary.events.files, 1);
        assert!(client_summary.events.edges >= 1, "{client_summary:?}");
        assert!(
            client_only
                .fetch_sites
                .iter()
                .any(|site| site.entry.as_deref() == Some("Api.stats")),
            "{:?}",
            client_only.fetch_sites
        );
        let (infra_only, infra_summary) = crate::extract_tree_with_summary(
            dir.path(),
            "local/kotlin-app",
            "workdir",
            &["infra".into()],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(infra_summary.kotlin, crate::LayerSummary::default());
        assert!(
            infra_only
                .nodes
                .iter()
                .all(|node| node.props["language"] != "kotlin")
        );
    }

    #[test]
    fn system_contents_lists_each_repo_from_graph_facts() {
        // AC-0085: composition comes from the graph's own evidence —
        // distinct repos with their recorded commit identity, sorted, one
        // entry per repo no matter how many facts it contributed. Evidence
        // (not File ids) is the source, so an infra-only repo whose
        // extractor emits Resource nodes still counts (#187 review).
        let fact = |id: &str, label: &str, repo: &str, path: &str, commit: &str| Node {
            id: id.into(),
            label: label.into(),
            props: serde_json::json!({
                "prov": { "evidence": [{ "repo": repo, "path": path, "commit_sha": commit }] },
            }),
        };
        let files = vec![
            fact(
                "res:aws_sqs_queue.orders",
                "Resource",
                "local/infra",
                "main.tf",
                "workdir",
            ),
            fact(
                "file:acme/shop@src/app.ts",
                "File",
                "acme/shop",
                "src/app.ts",
                "a1b2c3d",
            ),
            fact(
                "sym:app.ts#main",
                "Symbol",
                "acme/shop",
                "src/app.ts",
                "a1b2c3d",
            ),
            // Synthetic nodes without evidence assert nothing about repos.
            Node {
                id: "gap:x".into(),
                label: "Gap".into(),
                props: serde_json::json!({ "reason": "r" }),
            },
        ];
        let contents = crate::system_contents_of(&files);
        assert_eq!(
            contents,
            [
                crate::SystemRepo {
                    repo: "acme/shop".into(),
                    display_name: None,
                    commit: "a1b2c3d".into(),
                    graph_hash: None,
                },
                crate::SystemRepo {
                    repo: "local/infra".into(),
                    display_name: None,
                    commit: "workdir".into(),
                    graph_hash: None,
                },
            ]
        );

        // An empty graph names nothing — the UI states the system is empty.
        assert!(crate::system_contents_of(&[]).is_empty());
    }

    #[test]
    fn clear_graph_preserves_job_spine() {
        // AC-0050: only disposable graph facts are cleared; durable jobs live
        // in their separate state-spine database and remain untouched.
        let dir = tempfile::tempdir().unwrap();
        let mut graph = SqliteGraphStore::open(dir.path().join("graph.db")).unwrap();
        graph
            .put_node(&Node {
                id: "a".into(),
                label: "Resource".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        graph
            .put_node(&Node {
                id: "b".into(),
                label: "Resource".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        graph
            .put_edge(&Edge {
                src: "a".into(),
                dst: "b".into(),
                label: "REFERENCES".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        let mut jobs = crate::jobs::JobStore::open(dir.path().join("state.db")).unwrap();
        let job = jobs.enqueue("ingest:fixture").unwrap();

        let stats = crate::clear_graph_store(&mut graph).unwrap();
        assert_eq!(stats.nodes, 0);
        assert_eq!(stats.edges, 0);
        assert_eq!(jobs.list().unwrap()[0].id, job.id);
    }

    #[test]
    fn clear_finished_jobs_removes_terminal_rows_only() {
        // AC-0076: clearing finished jobs deletes done/failed/cancelled rows
        // while queued, running, and interrupted (resumable) jobs survive —
        // and graph facts are untouched (jobs live on their own spine).
        let dir = tempfile::tempdir().unwrap();
        let mut graph = SqliteGraphStore::open(dir.path().join("graph.db")).unwrap();
        graph
            .put_node(&Node {
                id: "a".into(),
                label: "Resource".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        let mut jobs = crate::jobs::JobStore::open(dir.path().join("state.db")).unwrap();
        let mut with_status = |status: &str| {
            let job = jobs.enqueue(&format!("ingest:{status}")).unwrap();
            if status != "queued" {
                jobs.set_status(job.id, status).unwrap();
            }
            job.id
        };
        for status in ["done", "failed", "cancelled"] {
            with_status(status);
        }
        let kept: Vec<i64> = ["queued", "running", "interrupted"]
            .into_iter()
            .map(&mut with_status)
            .collect();

        assert_eq!(jobs.clear_finished().unwrap(), 3);
        let remaining: Vec<i64> = jobs.list().unwrap().into_iter().map(|j| j.id).collect();
        assert_eq!(remaining.len(), 3);
        for id in kept {
            assert!(remaining.contains(&id));
        }
        // A second clear is a no-op, and the graph never lost its fact.
        assert_eq!(jobs.clear_finished().unwrap(), 0);
        assert_eq!(graph.node_count().unwrap(), 1);
    }

    #[test]
    fn clearing_a_live_cancelled_job_still_stops_the_worker() {
        // #157 review (P1): a cancelled job can be cleared while its worker
        // is still between cancellation checks. The guard must fail closed —
        // a missing row stops the exact execution, instead of
        // continuing to write graph facts after cancellation.
        let dir = tempfile::tempdir().unwrap();
        let state = super::registered_source_tests::app_state(dir.path());
        let (job, execution) = super::start_job(&state, "ingest:/big").unwrap();
        state.jobs.lock().unwrap().cancel(job.id).unwrap();
        assert_eq!(state.jobs.lock().unwrap().clear_finished().unwrap(), 1);
        assert_eq!(
            super::job_cancelled(&state, &execution).unwrap_err(),
            super::JobTransitionError::Missing.to_string()
        );
    }

    #[test]
    fn cancelled_add_jobs_never_flip_back_to_done() {
        // #166 review: with the UI responsive during an add, Cancel can race
        // the worker. The terminal transition must be atomic — a cancelled
        // job stays cancelled and the pipeline reports it, never a success.
        let dir = tempfile::tempdir().unwrap();
        let state = super::registered_source_tests::app_state(dir.path());
        let (job, execution) = super::start_job(&state, "add-repo:https://example.test/a").unwrap();
        state.jobs.lock().unwrap().cancel(job.id).unwrap();
        // The worker's completion attempt must not overwrite the cancel.
        let after = super::updated_job(
            state
                .jobs
                .lock()
                .unwrap()
                .finish_execution(&execution, &[])
                .unwrap(),
        );
        assert_eq!(after.status, "cancelled");

        // And an uncancelled run completes normally through the same path.
        let (_, execution) = super::start_job(&state, "add-repo:https://example.test/b").unwrap();
        assert_eq!(
            super::updated_job(
                state
                    .jobs
                    .lock()
                    .unwrap()
                    .finish_execution(&execution, &[])
                    .unwrap()
            )
            .status,
            "done"
        );

        let (_, failed_execution) = super::start_job(&state, "noop").unwrap();
        let failed = super::updated_job(
            state
                .jobs
                .lock()
                .unwrap()
                .fail_execution(&failed_execution, "fixture failure")
                .unwrap(),
        );
        assert_ne!(super::completed_job(&failed).unwrap_err(), "cancelled");
    }

    #[test]
    fn heavy_ingest_commands_run_off_the_calling_thread() {
        // AC-0078: every recovery command hops through off_ui_thread before
        // touching the pipeline — the invoking (webview/main) thread never
        // executes extraction, so a large repo cannot freeze the app.
        let caller = std::thread::current().id();
        let worker = tauri::async_runtime::block_on(crate::off_ui_thread(move || {
            Ok(std::thread::current().id())
        }))
        .unwrap();
        assert_ne!(caller, worker);
        // Errors from the pipeline pass through unchanged.
        let err = tauri::async_runtime::block_on(crate::off_ui_thread(|| {
            Err::<(), String>("boom".into())
        }));
        assert_eq!(err, Err("boom".into()));
    }

    #[test]
    fn unapplied_state_gap_is_owned_by_its_repository() {
        // AC-1501: the Gap carries no repository evidence, so re-ingest can
        // retract it only through explicit id ownership.
        assert!(crate::id_explicitly_owned_by_repo(
            "gap:tf-state-root:acme/infra@",
            "acme/infra"
        ));
        assert!(!crate::id_explicitly_owned_by_repo(
            "gap:tf-state-root:acme/infra@",
            "acme/infra2"
        ));
        assert!(!crate::id_explicitly_owned_by_repo(
            "gap:tf-state-root:acme/infra2@",
            "acme/infra"
        ));
    }

    #[test]
    fn state_json_resolves_from_manifest_directory_for_both_input_forms() {
        // AC-0009: observed state is relative to the topology manifest,
        // whether add_system receives that manifest file or its directory.
        let dir = tempfile::tempdir().unwrap();
        let manifest_file = dir.path().join(ingest::manifest::MANIFEST_NAME);
        std::fs::write(&manifest_file, "[[repos]]\nurl = \"acme/shop\"\n").unwrap();

        assert_eq!(crate::manifest_dir(dir.path()), dir.path());
        assert_eq!(crate::manifest_dir(&manifest_file), dir.path());
        assert_eq!(
            crate::manifest_dir(dir.path()).join("state.json"),
            dir.path().join("state.json")
        );
        assert_eq!(
            crate::manifest_dir(&manifest_file).join("state.json"),
            dir.path().join("state.json")
        );
    }

    #[test]
    fn system_manifest_enriches_pulumi_resources() {
        // AC-0051/T-0051 and AC-0052/T-0052: an infra-only repo still runs
        // Pulumi-via-TS extraction, filters unrelated app facts, and overlays
        // the manifest-relative observed deployment without inventing nodes.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("infra");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("index.ts"),
            r#"
import * as aws from '@pulumi/aws';
export function applicationLookalike() { return 'not infra'; }
export const orders = new aws.sqs.Queue('orders', {});
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("stack.json"),
            r#"{"deployment":{"resources":[
              {"urn":"urn:pulumi:dev::shop::aws:sqs/queue:Queue::orders","type":"aws:sqs/queue:Queue","inputs":{},"outputs":{"url":"https://sqs.example/orders"}},
              {"urn":"urn:pulumi:dev::shop::aws:s3/bucket:Bucket::unmatched","type":"aws:s3/bucket:Bucket","inputs":{},"outputs":{}}
            ]}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join(ingest::manifest::MANIFEST_NAME),
            "[[repos]]\nurl = \"infra\"\nlayers = [\"infra\"]\npulumi_json = \"stack.json\"\n",
        )
        .unwrap();
        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let entry = &manifest.repos[0];
        let pulumi_path = entry.pulumi_json.as_ref().map(|path| dir.path().join(path));
        let extraction = crate::extract_tree(
            &repo,
            "local/infra",
            "workdir",
            &entry.layers,
            &manifest.env,
            None,
            pulumi_path.as_deref(),
            &[],
        )
        .unwrap();
        assert!(extraction.nodes.iter().all(|node| node.label != "Symbol"));
        let resources = extraction
            .nodes
            .iter()
            .filter(|node| node.label == "Resource")
            .collect::<Vec<_>>();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].props["logical_id"], "orders");
        assert_eq!(
            resources[0].props["observed"]["outputs"]["url"],
            "https://sqs.example/orders"
        );
        assert_eq!(
            resources[0].props["observed_prov"]["extractor_id"],
            dynamic::PULUMI_EXTRACTOR_ID
        );

        // Pulumi observation can drive the same cross-layer join as Terraform
        // state, retaining its own extractor provenance. Removing the
        // observation on re-ingest must reconcile that derived edge.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        crate::load_into_graph(&mut store, &extraction, "local/infra", &repo, "workdir").unwrap();
        store
            .put_node(&Node {
                id: "chan:sqs-queue:https://sqs.example/orders".into(),
                label: "Channel".into(),
                props: serde_json::json!({}),
            })
            .unwrap();
        assert_eq!(
            crate::stitch_backings(
                &mut store,
                &mut crate::OperationFacts::default(),
                crate::Registries::builtin()
            )
            .unwrap(),
            1
        );
        let backing = store.edges_with_labels(&["BACKS"]).unwrap();
        assert_eq!(backing.len(), 1);
        assert_eq!(
            backing[0].props["prov"]["extractor_id"],
            dynamic::PULUMI_EXTRACTOR_ID
        );

        let without_observation = crate::extract_tree(
            &repo,
            "local/infra",
            "workdir",
            &entry.layers,
            &manifest.env,
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(
            &mut store,
            &without_observation,
            "local/infra",
            &repo,
            "workdir",
        )
        .unwrap();
        assert_eq!(
            crate::stitch_backings(
                &mut store,
                &mut crate::OperationFacts::default(),
                crate::Registries::builtin()
            )
            .unwrap(),
            0
        );
        assert!(store.edges_with_labels(&["BACKS"]).unwrap().is_empty());
    }

    #[test]
    fn cloned_repo_ingests_with_real_identity() {
        // US-0001 (AC-0001): clone -> extract -> every fact carries
        // owner-ish identity + commit SHA instead of local@workdir.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src-repo");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("app.ts"),
            "import express from 'express';\nconst app = express();\napp.get('/ping', (req, res) => {});\n",
        )
        .unwrap();
        let git = |args: &[&str], cwd: &std::path::Path| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q", "-b", "main"], &src);
        git(&["add", "."], &src);
        git(&["commit", "-q", "-m", "init"], &src);
        let bare = dir.path().join("shop.git");
        git(
            &[
                "clone",
                "-q",
                "--bare",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            dir.path(),
        );

        let mut registry =
            super::SourceRegistry::open(dir.path().join("state.db"), dir.path()).unwrap();
        let source = registry
            .reserve_managed(&format!("file://{}", bare.display()))
            .unwrap();
        let mut operation = super::SourceOperation::acquire(
            &std::sync::Mutex::new(registry),
            [(source.clone(), true)],
        )
        .unwrap();
        let cloned = operation.clone_source(&source, None).unwrap();
        assert_eq!(cloned.repo, source.repo_key);
        assert_eq!(cloned.commit_sha.len(), 40);

        let extraction = crate::extract_tree(
            &cloned.path,
            &cloned.repo,
            &cloned.commit_sha,
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        let ep = extraction
            .nodes
            .iter()
            .find(|n| n.label == "Endpoint")
            .expect("endpoint recovered from the clone");
        let ev = &ep.props["prov"]["evidence"][0];
        assert_eq!(ev["repo"], source.repo_key);
        assert_eq!(ev["commit_sha"].as_str().unwrap(), cloned.commit_sha);

        // Repo facts retain commit, while operational roots stay in the registry.
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        crate::load_into_graph(
            &mut store,
            &extraction,
            &cloned.repo,
            &cloned.path,
            &cloned.commit_sha,
        )
        .unwrap();
        let repos = store.nodes_with_label("Repo").unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].id, format!("repo:{}", source.repo_key));
        assert!(repos[0].props.get("root").is_none());
        assert_eq!(
            repos[0].props["commit"].as_str().unwrap(),
            cloned.commit_sha
        );
    }

    #[test]
    fn identical_repos_do_not_collide_in_one_graph() {
        // US-0001 slice 2 (#1 scope note): the same relative path, route,
        // and Terraform address in two repos stay two facts — ids are
        // repo-namespaced, provenance never cross-contaminates. Channels
        // stay global: they are the cross-repo stitch points.
        let fixture = |dir: &std::path::Path| {
            std::fs::write(
                dir.join("app.ts"),
                r#"
import express from 'express';
import { EventEmitter } from 'events';
const app = express();
const bus = new EventEmitter();
app.get('/health', (req, res) => { beat(); });
export function beat() { bus.emit('heartbeat'); }
"#,
            )
            .unwrap();
            std::fs::write(dir.join("main.tf"), "resource \"aws_sqs_queue\" \"q\" {}\n").unwrap();
        };
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        fixture(a.path());
        fixture(b.path());

        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for (dir, repo, sha) in [
            (a.path(), "acme/one", "a".repeat(40)),
            (b.path(), "acme/two", "b".repeat(40)),
        ] {
            let ex = crate::extract_tree(
                dir,
                repo,
                &sha,
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
            )
            .unwrap();
            crate::load_into_graph(&mut store, &ex, repo, dir, &sha).unwrap();
        }

        // Two of everything repo-scoped…
        let eps = store.nodes_with_label("Endpoint").unwrap();
        assert_eq!(eps.len(), 2);
        assert!(eps.iter().any(|e| e.id == "ep:acme/one@GET:/health"));
        assert!(eps.iter().any(|e| e.id == "ep:acme/two@GET:/health"));
        for ep in &eps {
            let ev = &ep.props["prov"]["evidence"][0];
            let repo = ev["repo"].as_str().unwrap();
            assert!(ep.id.contains(repo), "provenance matches its own repo");
        }
        assert_eq!(store.nodes_with_label("Repo").unwrap().len(), 2);
        let resources = store.nodes_with_label("Resource").unwrap();
        assert_eq!(resources.len(), 2, "same tf address, two nodes");
        // …and ONE of the global channel: both repos emit 'heartbeat', and
        // that shared identity is exactly what M5 stitches across repos.
        let chans = store.nodes_with_label("Channel").unwrap();
        assert_eq!(chans.len(), 1);
        assert_eq!(chans[0].id, "chan:inproc-event:heartbeat");
    }

    #[test]
    fn parallel_extraction_is_byte_identical_to_serial() {
        // AC-0208/T-0208 (#236): the same tree extracted with one worker and
        // with many yields byte-identical facts and an identical graph hash
        // set; parallelism changes only wall-clock time.
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: String| {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        for i in 0..24 {
            let next = (i + 1) % 24;
            write(
                &format!("web/src/m{i:02}.ts"),
                format!(
                    "import {{ f{next} }} from './m{next:02}';\n\
                     export function f{i}() {{ return f{next}(); }}\n\
                     app.get('/r{i}', f{i});\n"
                ),
            );
            write(
                &format!("svc/pkg{}/m{i:02}.py", i % 3),
                format!("def g{i}():\n    return g{i}()\n"),
            );
            write(
                &format!("go/p{}/m{i:02}.go", i % 4),
                format!("package p{}\n\nfunc H{i}() {{ H{i}() }}\n", i % 4),
            );
            write(
                &format!("jvm/src/C{i:02}.java"),
                format!("package demo;\nclass C{i:02} {{ void m() {{ m(); }} }}\n"),
            );
            write(
                &format!("jvm/src/K{i:02}.kt"),
                format!("package demo\nclass K{i:02} {{ fun m() {{ m() }} }}\n"),
            );
        }
        let extract = |workers: usize| {
            source_walk::parallel::with_workers(workers, || {
                let (extraction, layers, delta) = crate::extract_tree_incremental(
                    dir.path(),
                    "local/parallel",
                    "workdir",
                    &[],
                    &std::collections::BTreeMap::new(),
                    None,
                    None,
                    &[],
                    &mut crate::RepoExtractionCache::default(),
                    &[],
                    &mut |_| {},
                )
                .unwrap();
                let mut store = SqliteGraphStore::open_in_memory().unwrap();
                crate::load_into_graph(
                    &mut store,
                    &extraction,
                    "local/parallel",
                    dir.path(),
                    "workdir",
                )
                .unwrap();
                (
                    serde_json::to_vec(&(&extraction.nodes, &extraction.edges)).unwrap(),
                    serde_json::to_vec(&layers).unwrap(),
                    delta.recomputed_files,
                    crate::deterministic_graph_hashes(&store).unwrap(),
                )
            })
        };
        let serial = extract(1);
        assert!(serial.2 >= 120, "every fixture file is parsed");
        for workers in [2, 8] {
            let parallel = extract(workers);
            assert!(
                parallel.0 == serial.0,
                "facts differ with {workers} workers"
            );
            assert_eq!(parallel.1, serial.1);
            assert_eq!(parallel.2, serial.2);
            assert_eq!(parallel.3, serial.3);
        }
    }

    #[test]
    fn reingest_hashes_are_identical_and_delta_removes_stale_facts() {
        // AC-0039/T-0039 and AC-0040/T-0040: an unchanged tree reuses every
        // parse and yields the same ordered T0 hash set; one changed/deleted
        // file is the only parse work and its old facts cannot remain.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            "import { helper } from './helper';\nexport function run() { helper(); }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("helper.ts"),
            "export function helper() {}\n",
        )
        .unwrap();
        let repo = "local/delta";
        let mut cache = crate::RepoExtractionCache::default();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();

        let (first, _, initial_delta) = crate::extract_tree_incremental(
            dir.path(),
            repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            &mut cache,
            &[],
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(initial_delta.recomputed_files, 2);
        crate::load_into_graph(&mut store, &first, repo, dir.path(), "workdir").unwrap();
        let initial_hashes = crate::deterministic_graph_hashes(&store).unwrap();

        let (same, _, same_delta) = crate::extract_tree_incremental(
            dir.path(),
            repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            &mut cache,
            &[],
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(same_delta.recomputed_files, 0);
        assert_eq!(same_delta.reused_files, 2);
        let reconcile =
            crate::load_into_graph(&mut store, &same, repo, dir.path(), "workdir").unwrap();
        assert_eq!(reconcile.inserted_or_updated, 0);
        assert_eq!(
            crate::deterministic_graph_hashes(&store).unwrap(),
            initial_hashes
        );

        std::fs::write(
            dir.path().join("helper.ts"),
            "export function replacement() {}\n",
        )
        .unwrap();
        let (changed, _, changed_delta) = crate::extract_tree_incremental(
            dir.path(),
            repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            &mut cache,
            &[],
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(changed_delta.recomputed_files, 1);
        assert_eq!(changed_delta.reused_files, 1);
        crate::load_into_graph(&mut store, &changed, repo, dir.path(), "workdir").unwrap();
        assert!(
            store
                .get_node("sym:local/delta@helper.ts#helper")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_node("sym:local/delta@helper.ts#replacement")
                .unwrap()
                .is_some()
        );

        std::fs::remove_file(dir.path().join("helper.ts")).unwrap();
        let (deleted, _, deleted_delta) = crate::extract_tree_incremental(
            dir.path(),
            repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
            &mut cache,
            &[],
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(deleted_delta.deleted_files, 1);
        crate::load_into_graph(&mut store, &deleted, repo, dir.path(), "workdir").unwrap();
        assert!(
            store
                .get_node("sym:local/delta@helper.ts#replacement")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn manifest_local_paths_beat_owner_name_shorthand() {
        // AC-0002 classification: `services/api` next to the manifest is a
        // local repo; the same shape with nothing on disk is a GitHub
        // shorthand to clone.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("services/api")).unwrap();
        assert!(!crate::manifest_entry_is_remote("services/api", dir.path()));
        assert!(crate::manifest_entry_is_remote("acme/shop", dir.path()));
        assert!(crate::manifest_entry_is_remote(
            "https://github.com/acme/shop",
            dir.path()
        ));
        assert!(!crate::manifest_entry_is_remote(
            "./services/api",
            dir.path()
        ));
    }

    #[test]
    fn system_manifest_applies_hints_and_identities_at_ingest() {
        // AC-0002 end to end: two local repos declared in one manifest —
        // the infra-hinted repo's TS is skipped, and a producer whose
        // queue URL no env file defines resolves through the manifest's
        // declared identity.
        let dir = tempfile::tempdir().unwrap();
        // `services/api`: two segments, exactly the shape of owner/name
        // shorthand — must classify as local because it exists next to the
        // manifest (never resolved against the process cwd).
        let server = dir.path().join("services").join("api");
        let infra = dir.path().join("infra");
        std::fs::create_dir_all(&server).unwrap();
        std::fs::create_dir_all(&infra).unwrap();
        std::fs::write(
            server.join("send.ts"),
            r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const sqs = new SQSClient({});
export function push() {
  return sqs.send(new SendMessageCommand({ QueueUrl: process.env.ORDERS_QUEUE }));
}
"#,
        )
        .unwrap();
        std::fs::write(
            infra.join("main.tf"),
            "resource \"aws_sqs_queue\" \"orders\" {}\n",
        )
        .unwrap();
        // A .ts file in the infra repo that the layer hint must skip.
        std::fs::write(infra.join("script.ts"), "export function x() {}\n").unwrap();
        std::fs::write(
            dir.path().join("cartograph.system.toml"),
            r#"
[[repos]]
url = "services/api"
layers = ["server", "events"]

[[repos]]
url = "./infra"
layers = ["infra"]

[env]
ORDERS_QUEUE = "https://sqs.example/orders"
"#,
        )
        .unwrap();

        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for entry in &manifest.repos {
            let root = super::paths::canonicalize(dir.path().join(&entry.url)).unwrap();
            let name = root.file_name().unwrap().to_string_lossy().into_owned();
            let repo = format!("local/{name}");
            let ex = crate::extract_tree(
                &root,
                &repo,
                "workdir",
                &entry.layers,
                &manifest.env,
                None,
                None,
                &[],
            )
            .unwrap();
            crate::load_into_graph(&mut store, &ex, &repo, &root, "workdir").unwrap();
        }

        // Layer hint applied: the infra repo contributed no TS facts.
        let files = store.nodes_with_label("File").unwrap();
        assert!(files.iter().all(|f| !f.id.contains("script.ts")));
        // One declared queue from the infra repo; the API repo's SQS client
        // is observed infrastructure beside it (#734).
        let (observed, declared): (Vec<_>, Vec<_>) = store
            .nodes_with_label("Resource")
            .unwrap()
            .into_iter()
            .partition(|r| r.props["observed"] == true);
        assert_eq!(declared.len(), 1);
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].id, "res:local/api@observed:aws-sqs");

        // Manifest identity applied: the env-ref channel resolved Confirmed
        // via the manifest, not a Gap.
        let chans = store.nodes_with_label("Channel").unwrap();
        assert_eq!(chans.len(), 1);
        assert_eq!(chans[0].id, "chan:sqs-queue:https://sqs.example/orders");
        assert!(store.nodes_with_label("Gap").unwrap().is_empty());
        let publish = store.edges_with_labels(&["PUBLISHES"]).unwrap();
        assert_eq!(
            publish[0].props["resolver"],
            "config:cartograph.system.toml"
        );
    }

    #[test]
    fn cross_repo_flow_stitches_via_literal_channel_identity() {
        // M5 exit gate (US-0004 AC-0010 at cross-repo scope): a producer
        // repo and a consumer repo, declared as one system, stitch through
        // the global channel — one flow spans both repos, every hop T0.
        let dir = tempfile::tempdir().unwrap();
        let producer = dir.path().join("orders-api");
        let consumer = dir.path().join("mailer");
        std::fs::create_dir_all(&producer).unwrap();
        std::fs::create_dir_all(&consumer).unwrap();
        std::fs::write(
            producer.join("app.ts"),
            r#"
import express from 'express';
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const app = express();
const sqs = new SQSClient({});
app.post('/orders', (req, res) => { queueOrder(); });
export function queueOrder() {
  return sqs.send(new SendMessageCommand({ QueueUrl: 'https://sqs.us-east-1.amazonaws.com/1/orders', MessageBody: '{}' }));
}
"#,
        )
        .unwrap();
        std::fs::write(
            consumer.join("worker.ts"),
            r#"
import { Consumer } from 'sqs-consumer';
export function startWorker() {
  return new Consumer({ queueUrl: 'https://sqs.us-east-1.amazonaws.com/1/orders', handleMessage: handle });
}
function handle() {}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cartograph.system.toml"),
            r#"
[[repos]]
url = "orders-api"
layers = ["server", "events"]

[[repos]]
url = "mailer"
layers = ["events", "server"]
"#,
        )
        .unwrap();

        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for entry in &manifest.repos {
            let root = super::paths::canonicalize(dir.path().join(&entry.url)).unwrap();
            let name = root.file_name().unwrap().to_string_lossy().into_owned();
            let repo = format!("local/{name}");
            let ex = crate::extract_tree(
                &root,
                &repo,
                "workdir",
                &entry.layers,
                &manifest.env,
                None,
                None,
                &[],
            )
            .unwrap();
            crate::load_into_graph(&mut store, &ex, &repo, &root, "workdir").unwrap();
        }

        // One global channel, published from repo A, subscribed from repo B.
        let chans = store.nodes_with_label("Channel").unwrap();
        assert_eq!(chans.len(), 1);
        assert_eq!(
            chans[0].id,
            "chan:sqs-queue:https://sqs.us-east-1.amazonaws.com/1/orders"
        );

        // One flow, triggered in the producer repo, terminating in the
        // consumer repo — the cross-repo hop rides the channel.
        let mut flow_nodes = Vec::new();
        for label in flowtracer::FLOW_NODE_LABELS {
            flow_nodes.extend(store.nodes_with_label(label).unwrap());
        }
        let flow_edges = store
            .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
            .unwrap();
        let flows = flowtracer::trace(&flow_nodes, &flow_edges);
        assert_eq!(flows.len(), 1, "one system, one flow");
        let flow = &flows[0];
        assert_eq!(flow.trigger, "ep:local/orders-api@POST:/orders");
        assert_eq!(flow.status, flowtracer::FlowStatus::Verified);
        let sub = flow
            .hops
            .iter()
            .find(|h| h.label == "SUBSCRIBES")
            .expect("flow crosses the channel");
        assert!(
            sub.dst.contains("local/mailer@"),
            "consumer hop lands in the other repo: {}",
            sub.dst
        );
        // No gaps anywhere: both sides carry the same literal identity
        // (AC-0010); the config-resolved path is AC-0011's manifest test.
        assert!(store.nodes_with_label("Gap").unwrap().is_empty());
    }

    #[test]
    fn otel_trace_resolves_runtime_channel_gap_with_observed_provenance() {
        // M6 exit gate (issue #54, AC-0012, T-0012): T0 emits a Gap for a
        // runtime channel identity; OTLP/JSONL fills that exact source slot
        // at T1 and enriches the matching HTTP endpoint without touching T0.
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("shop");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(
            repo_dir.join("app.ts"),
            r#"
import express from 'express';
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const app = express();
const sqs = new SQSClient({});
function runtimeQueue() { return process.argv[2]; }
app.post('/orders', (_req, _res) => queueOrder());
export function queueOrder() {
  return sqs.send(new SendMessageCommand({ QueueUrl: runtimeQueue(), MessageBody: '{}' }));
}
"#,
        )
        .unwrap();
        let trace_path = dir.path().join("shop.otlp.jsonl");
        std::fs::write(
            &trace_path,
            r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{"traceId":"trace-shop","spanId":"span-send","name":"send order","attributes":[{"key":"messaging.system","value":{"stringValue":"aws_sqs"}},{"key":"messaging.destination.name","value":{"stringValue":"https://sqs.example/runtime-orders"}},{"key":"code.file.path","value":{"stringValue":"/checkout/app.ts"}}]},{"traceId":"trace-shop","spanId":"span-http","name":"POST /orders","attributes":[{"key":"http.request.method","value":{"stringValue":"POST"}},{"key":"http.route","value":{"stringValue":"/orders"}}]}]}]}]}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cartograph.system.toml"),
            r#"
[[repos]]
url = "shop"
layers = ["server", "events"]
otel_jsonl = ["shop.otlp.jsonl"]
"#,
        )
        .unwrap();

        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let entry = &manifest.repos[0];
        let trace_paths: Vec<_> = entry
            .otel_jsonl
            .iter()
            .map(|path| dir.path().join(path))
            .collect();
        let extraction = crate::extract_tree(
            &repo_dir,
            "local/shop",
            "workdir",
            &entry.layers,
            &manifest.env,
            None,
            None,
            &trace_paths,
        )
        .unwrap();

        assert!(extraction.nodes.iter().all(|node| node.label != "Gap"));
        let channel = extraction
            .nodes
            .iter()
            .find(|node| node.id == "chan:sqs-queue:https://sqs.example/runtime-orders")
            .unwrap();
        assert_eq!(channel.props["prov"]["tier"], "Dynamic");
        assert_eq!(channel.props["prov"]["confidence_tier"], "Confirmed");
        assert_eq!(channel.props["observed"]["span_id"], "span-send");
        let publish = extraction
            .edges
            .iter()
            .find(|edge| edge.label == "PUBLISHES")
            .unwrap();
        assert_eq!(publish.dst, channel.id);
        assert_eq!(publish.props["resolver"], dynamic::OTEL_EXTRACTOR_ID);
        assert_eq!(publish.props["prov"]["tier"], "Dynamic");
        let endpoint = extraction
            .nodes
            .iter()
            .find(|node| node.label == "Endpoint")
            .unwrap();
        assert_eq!(endpoint.props["prov"]["tier"], "Deterministic");
        assert_eq!(endpoint.props["observed"]["span_id"], "span-http");
        assert_eq!(endpoint.props["observed_prov"]["tier"], "Dynamic");
        assert_eq!(
            endpoint.props["observed_prov"]["evidence"][0]["path"],
            trace_path.to_string_lossy().as_ref()
        );
    }

    #[test]
    fn observed_state_backs_channels_and_resolves_placeholders() {
        // M6 slice 1 (AC-0009, T-0009): `terraform show -json` output
        // enriches the T0 graph — the module placeholder resolves, the
        // secret is redacted, and the observed queue URL joins infra to
        // the code-layer channel with a BACKS edge on the topology map.
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("shop");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(
            repo_dir.join("main.tf"),
            r#"
resource "aws_sqs_queue" "orders" {
  tags = { vpc = module.network.vpc_id }
}
"#,
        )
        .unwrap();
        std::fs::write(
            repo_dir.join("app.ts"),
            r#"
import { SQSClient, SendMessageCommand } from '@aws-sdk/client-sqs';
const sqs = new SQSClient({});
export function queueOrder() {
  return sqs.send(new SendMessageCommand({ QueueUrl: 'https://sqs.us-east-1.amazonaws.com/9/orders', MessageBody: '{}' }));
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("shop.state.json"),
            r#"{
  "format_version": "1.0",
  "values": { "root_module": {
    "resources": [{
      "address": "aws_sqs_queue.orders",
      "mode": "managed",
      "type": "aws_sqs_queue",
      "name": "orders",
      "values": {
        "url": "https://sqs.us-east-1.amazonaws.com/9/orders",
        "master_key": "hunter2"
      },
      "sensitive_values": { "master_key": true }
    }],
    "child_modules": [{
      "address": "module.network",
      "resources": [{
        "address": "module.network.aws_vpc.main",
        "mode": "managed",
        "type": "aws_vpc",
        "name": "main",
        "values": { "id": "vpc-123" },
        "sensitive_values": {}
      }]
    }]
  } }
}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("cartograph.system.toml"),
            r#"
[[repos]]
url = "shop"
state_json = "shop.state.json"
"#,
        )
        .unwrap();

        let manifest = ingest::manifest::SystemManifest::load(dir.path()).unwrap();
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        for entry in &manifest.repos {
            let root = super::paths::canonicalize(dir.path().join(&entry.url)).unwrap();
            let state_path = entry.state_json.as_ref().map(|p| dir.path().join(p));
            let ex = crate::extract_tree(
                &root,
                "local/shop",
                "workdir",
                &entry.layers,
                &manifest.env,
                state_path.as_deref(),
                None,
                &[],
            )
            .unwrap();
            crate::load_into_graph(&mut store, &ex, "local/shop", &root, "workdir").unwrap();
        }
        let backed = crate::stitch_backings(
            &mut store,
            &mut crate::OperationFacts::default(),
            crate::Registries::builtin(),
        )
        .unwrap();
        assert_eq!(backed, 1);

        let resources = store.nodes_with_label("Resource").unwrap();
        let queue = resources
            .iter()
            .find(|n| n.id == "res:local/shop@aws_sqs_queue.orders")
            .unwrap();
        // T0 provenance untouched; observation lands beside it (R-INT-1).
        assert_eq!(queue.props["prov"]["tier"], "Deterministic");
        assert_eq!(
            queue.props["observed"]["url"],
            "https://sqs.us-east-1.amazonaws.com/9/orders"
        );
        assert_eq!(queue.props["observed_prov"]["tier"], "Dynamic");
        // The secret never reaches the graph (US-0003 Security).
        assert_eq!(queue.props["observed"]["master_key"], dynamic::REDACTED);
        // The module placeholder was an ambiguous T0 ref; state resolved it.
        let module = resources
            .iter()
            .find(|n| n.id == "res:local/shop@module.network")
            .unwrap();
        assert!(module.props.get("placeholder").is_none());
        assert_eq!(module.props["resolved_by"], dynamic::EXTRACTOR_ID);

        // The join is on the artifact: channel cylinder + BACKS arrow.
        let mut nodes = store.nodes_with_label("Resource").unwrap();
        nodes.extend(store.nodes_with_label("Channel").unwrap());
        let edges = store.edges_with_labels(spec::TOPOLOGY_EDGE_LABELS).unwrap();
        let mmd = spec::topology_mermaid(&nodes, &edges);
        assert!(mmd.contains(r#"[("sqs-queue:https://sqs.us-east-1.amazonaws.com/9/orders · "#));
        // The observed join prints as T1, apart from the static edges (AC-0587).
        assert!(mmd.contains("-->|BACKS · T1 Confirmed|"));
        // Re-running the join is idempotent (US-0014 re-ingest).
        assert_eq!(
            crate::stitch_backings(
                &mut store,
                &mut crate::OperationFacts::default(),
                crate::Registries::builtin()
            )
            .unwrap(),
            1
        );
        assert_eq!(store.edges_with_labels(&["BACKS"]).unwrap().len(), 1);

        // AC-0009/T-0009 and AC-0040: removing the observation on re-ingest
        // removes its derived BACKS edge instead of retaining stale topology.
        let without_state = crate::extract_tree(
            &repo_dir,
            "local/shop",
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap();
        crate::load_into_graph(
            &mut store,
            &without_state,
            "local/shop",
            &repo_dir,
            "workdir",
        )
        .unwrap();
        assert_eq!(
            crate::stitch_backings(
                &mut store,
                &mut crate::OperationFacts::default(),
                crate::Registries::builtin()
            )
            .unwrap(),
            0
        );
        assert!(store.edges_with_labels(&["BACKS"]).unwrap().is_empty());
        let queue = store
            .get_node("res:local/shop@aws_sqs_queue.orders")
            .unwrap()
            .unwrap();
        assert!(queue.props.get("observed").is_none());
        assert!(queue.props.get("observed_prov").is_none());
    }

    #[test]
    fn ingest_chain_produces_topology_artifact() {
        // The ingest -> graph -> spec pipeline, minus the Tauri shell:
        // mixed TS + Terraform tree in, Mermaid topology out (M2 exit gate).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("main.tf"),
            r#"
resource "aws_sqs_queue" "orders" {}
resource "aws_lambda_function" "fulfill" {}
resource "aws_lambda_event_source_mapping" "m" {
  event_source_arn = aws_sqs_queue.orders.arn
  function_name    = aws_lambda_function.fulfill.arn
}
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("app.ts"),
            r#"
import express from 'express';
import { EventEmitter } from 'events';
const app = express();
const bus = new EventEmitter();
app.post('/orders', (req, res) => { placeOrder(); });
export function placeOrder() { bus.emit('order.placed'); }
export function listen() { bus.on('order.placed', () => {}); }
// A class-method producer: the TS pass emits a qualified, proven method symbol.
export class Shipper {
  ship() { bus.emit('order.shipped'); }
}
"#,
        )
        .unwrap();
        // Client layer (US-0005): a routed component fetching the endpoint
        // the server file above registers.
        std::fs::write(
            dir.path().join("client.tsx"),
            r#"
import { Routes, Route } from 'react-router-dom';
export function Checkout() {
  const submit = () => fetch('/orders', { method: 'POST' });
  return <button onClick={submit}>Order</button>;
}
export function App() {
  return <Routes><Route path="/checkout" element={<Checkout />} /></Routes>;
}
"#,
        )
        .unwrap();

        let ts_id = adapters_lang_ts::SourceId {
            repo: "local",
            commit: "workdir",
        };
        let tf_id = iac::SourceId {
            repo: "local",
            commit: "workdir",
        };
        let mut store = SqliteGraphStore::open_in_memory().unwrap();
        // Mirrors ingest_path: TS + TF + stitch into one extraction, closed
        // over before anything reaches the FK-enforcing store.
        let mut extraction = adapters_lang_ts::extract_dir(dir.path(), &ts_id).unwrap();
        let tf = iac::extract_dir(dir.path(), &tf_id).unwrap();
        extraction.nodes.extend(tf.nodes);
        extraction.edges.extend(tf.edges);
        let cfg = events::ConfigIndex::from_dir(dir.path(), &Default::default()).unwrap();
        let ev_id = events::SourceId {
            repo: "local",
            commit: "workdir",
        };
        let ev = events::stitch(&extraction.event_sites, &cfg, &ev_id);
        extraction.nodes.extend(ev.nodes);
        extraction.edges.extend(ev.edges);
        let endpoint_ids: Vec<String> = extraction
            .nodes
            .iter()
            .filter(|n| n.label == "Endpoint")
            .map(|n| n.id.clone())
            .collect();
        let fetched = events::stitch_fetches(&extraction.fetch_sites, &endpoint_ids, &cfg, &ev_id);
        extraction.nodes.extend(fetched.nodes);
        extraction.edges.extend(fetched.edges);
        extraction.close_over_endpoints();
        for n in &extraction.nodes {
            store.put_node(n).unwrap();
        }
        for e in &extraction.edges {
            store.put_edge(e).unwrap();
        }

        let nodes = store.nodes_with_label("Resource").unwrap();
        let edges = store.edges_with_labels(spec::TOPOLOGY_EDGE_LABELS).unwrap();
        let mmd = spec::topology_mermaid(&nodes, &edges);
        // A real ingest carries provenance, so the edge prints as T0 Confirmed (AC-0587).
        assert!(mmd.contains("|TRIGGERS · T0 Confirmed|"));
        // The TS layer coexists without leaking onto the infra artifact.
        assert!(!mmd.contains("app_ts"));

        // The event layer stitched: producer and consumer share one channel
        // (US-0004), and channels stay off the infra artifact too.
        let channels = store.nodes_with_label("Channel").unwrap();
        let ids: Vec<&str> = channels.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "chan:inproc-event:order.placed",
                "chan:inproc-event:order.shipped"
            ]
        );
        assert!(!mmd.contains("order.placed"));
        // The class-method producer is a real, provenance-bearing Symbol, not
        // a close-over placeholder.
        let symbols = store.nodes_with_label("Symbol").unwrap();
        let ship = symbols
            .iter()
            .find(|symbol| symbol.id == "sym:local@app.ts#Shipper.ship")
            .expect("qualified class method");
        assert!(ship.props.get("placeholder").is_none());
        assert_eq!(ship.props["prov"]["confidence_tier"], "Confirmed");

        // Client layer (US-0005): the route became a Screen, and the
        // component's fetch resolved Confirmed against the server endpoint.
        let screens = store.nodes_with_label("Screen").unwrap();
        assert_eq!(screens.len(), 1);
        assert_eq!(screens[0].id, "screen:local@/checkout");
        let fetches = store.edges_with_labels(&["FETCHES"]).unwrap();
        assert_eq!(fetches.len(), 1);
        assert_eq!(fetches[0].dst, "ep:local@POST:/orders");
        assert_eq!(
            fetches[0].props["prov"]["confidence_tier"], "Confirmed",
            "resolvable fetch is Confirmed (AC-0014)"
        );

        // M4 exit gate: the flow anchors at the Screen (the fetched endpoint
        // is mid-flow, not a trigger) and runs end to end through the
        // channel to the consumer, exported as a dossier.
        let mut flow_nodes = Vec::new();
        for label in flowtracer::FLOW_NODE_LABELS {
            flow_nodes.extend(store.nodes_with_label(label).unwrap());
        }
        let flow_edges = store
            .edges_with_labels(flowtracer::FLOW_EDGE_LABELS)
            .unwrap();
        let flows = flowtracer::trace(&flow_nodes, &flow_edges);
        let dossier = spec::flow_dossier(&flows);
        assert!(dossier.contains("## Screen /checkout — Verified (score 1.00)"));
        assert!(
            !dossier.contains("## POST /orders"),
            "the fetched endpoint must not double-report as its own flow"
        );
        assert!(dossier.contains("FETCHES [Confirmed]"));
        assert!(dossier.contains("SUBSCRIBES [Confirmed]"));
        assert!(dossier.contains("chan:inproc-event:order.placed"));
    }

    #[test]
    fn plugin_gate_job_honors_cancel_and_retry_reruns_the_gate() {
        // #206 review (P2s): a cancelled plugin-gate job must never persist
        // a verdict — the job outcome and the trusted artifact state cannot
        // diverge — and Jobs retry re-dispatches the gate on the same row
        // instead of failing with "not yet supported".
        use tauri::Manager;
        const OK_ADAPTER: &[u8] = include_bytes!(
            "../../crates/adapters-plugin-host/tests/fixtures/compiled/ok-adapter.wasm"
        );

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let adapters = project.join(".cartograph/adapters");
        std::fs::create_dir_all(&adapters).unwrap();
        std::fs::write(adapters.join("t0.plugin-fixture.wasm"), OK_ADAPTER).unwrap();
        // A correct corpus against the fixed gate source id (`golden`).
        std::fs::write(
            adapters.join("t0.plugin-fixture.golden.json"),
            serde_json::json!({
                "extensions": ["foo"],
                "cases": [{
                    "path": "src/lib.rs",
                    "source": "hello world",
                    "nodes": [
                        {"id": "golden:src/lib.rs", "label": "TestNode", "props": {"len": 11}}
                    ],
                    "edges": [{
                        "src": "golden:src/lib.rs",
                        "dst": "golden:src/lib.rs",
                        "label": "SELF",
                        "props": {}
                    }],
                }],
            })
            .to_string(),
        )
        .unwrap();

        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app"),
        );
        let state_path = dir.path().join("state.db");
        let mut roots = std::collections::BTreeSet::new();
        roots.insert(project.display().to_string());
        app.manage(super::AppState {
            graph: std::sync::Mutex::new(
                SqliteGraphStore::open(dir.path().join("graph.db")).unwrap(),
            ),
            jobs: std::sync::Mutex::new(super::JobStore::open(&state_path).unwrap()),
            investigations: crate::investigations::InvestigationRuntime::default(),
            job_execution_locks: super::job_execution_host_tests::locks(&state_path),
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            proposals: std::sync::Mutex::new(
                agents::ProposalStore::open(state_path.with_file_name("proposals.sqlite")).unwrap(),
            ),
            drafts: std::sync::Mutex::new(
                agents::draft_store::DraftStore::open(
                    state_path.with_file_name("section-drafts.sqlite"),
                )
                .unwrap(),
            ),
            work_packets: super::work_packets::PacketRuntime::open(&state_path).unwrap(),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            embedding_cache: None,
            knowledge_overlay: None,
            curated_settled: std::sync::Arc::default(),
            primary_sources: super::primary_source::PrimarySourceStore::open(
                state_path.parent().unwrap(),
            )
            .unwrap(),
            sources: test_source_registry(&state_path, &roots),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        });
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let hash = adapters_plugin_host::gate::gate_key(
            &core_prov::content_hash(OK_ADAPTER),
            std::fs::read(adapters.join("t0.plugin-fixture.golden.json"))
                .ok()
                .as_deref(),
        );

        // Enqueue a gate job, then cancel before the pipeline runs.
        let (job, execution) = super::start_job(&state, "plugin-gate:t0.plugin-fixture").unwrap();
        let job_id = job.id;
        state.jobs.lock().unwrap().cancel(job_id).unwrap();
        let result = super::plugin_gate_blocking("t0.plugin-fixture", &execution, &handle);
        assert_eq!(result.unwrap_err(), "cancelled");
        // The cancel won outright: no verdict, job row still cancelled.
        assert!(
            state
                .settings
                .lock()
                .unwrap()
                .plugin_gate("t0.plugin-fixture", &hash)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            state.jobs.lock().unwrap().get(job_id).unwrap().status,
            "cancelled"
        );

        // Retry claims the next attempt only after the old execution exits;
        // the production retry preparation then dispatches the same gate helper.
        drop(execution);
        let (_, execution, _, _) = super::prepare_job_retry(&state, job_id).unwrap();
        super::plugin_gate_blocking("t0.plugin-fixture", &execution, &handle)
            .expect("retried gate runs");
        assert_eq!(
            state.jobs.lock().unwrap().get(job_id).unwrap().status,
            "done"
        );
        let (passed, report_json) = state
            .settings
            .lock()
            .unwrap()
            .plugin_gate("t0.plugin-fixture", &hash)
            .unwrap()
            .expect("retried gate persists its verdict");
        assert!(passed, "gate report: {report_json}");
    }

    #[test]
    fn preflight_defers_eval_proof_and_recovery_reconciles_it() {
        // AC-0099, AC-0199, AC-0200, AC-0216 (#214, #243, #458): preflight no longer runs
        // the TS extraction, so every textual eval()/new Function() line is
        // reported pending AST proof — including the proven literal, which
        // preflight used to close. Recovery's own extraction then supplies
        // the adapter claims and replaces those findings in the register: a
        // proven literal closes, a const-shaped-but-unproven argument
        // downgrades to a potential Gap (leaving the Unsupported lane), and
        // runtime-computed ones stay Unsupported.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("app.ts"),
            concat!(
                "const CODE = getCode();\n",
                "export function boot() {\n",
                "  eval(\"function legacySetup() {}\");\n",
                "  eval(CODE);\n",
                "  eval(getCode() + '()');\n",
                "}\n",
                "new Function(getCode());\n",
                "new Function('return 1');\n",
            ),
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let path_arg = project.to_string_lossy().into_owned();
        let register_eval_lines = || -> Vec<i64> {
            state
                .findings
                .lock()
                .unwrap()
                .list()
                .unwrap()
                .into_iter()
                .filter(|f| f.detector == ingest::preflight::DETECTOR_ID)
                .filter(|f| f.message.contains("eval()") || f.message.contains("new Function()"))
                .map(|f| f.line)
                .collect()
        };

        let report = super::preflight_uncancelled(&path_arg, &handle, &state).unwrap();
        let pending: Vec<u64> = report
            .unsupported
            .iter()
            .filter(|f| f.kind == "inline-eval")
            .inspect(|f| assert!(f.message.contains("pending AST proof at recovery")))
            .map(|f| f.line)
            .collect();
        assert_eq!(
            pending,
            vec![3, 4, 5, 7, 8],
            "no line is closed or dropped before recovery proves it"
        );
        assert_eq!(register_eval_lines(), vec![3, 4, 5, 7, 8]);

        // A preflight already running when recovery starts (#439 review).
        let runs = app.state::<super::PreflightRuns>();
        let in_flight = runs.begin().unwrap();

        let source = super::register_local_source(&state, &project).unwrap();
        let operation = super::source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
        let summary = super::run_ingest(&source, &operation, &execution, &handle, &state).unwrap();
        drop(operation);
        // AC-0237: a local recovery names its repo; its commit is `workdir`.
        assert_eq!(summary.repo, source.repo_key);
        assert_eq!(register_eval_lines(), vec![5, 7]);
        // The summary carries the reconciled report for the Preflight
        // surface, including the potential-Gap lane the register never
        // holds (#439 review).
        let lines = |findings: &[ingest::preflight::PatternFinding]| -> Vec<u64> {
            findings
                .iter()
                .filter(|f| f.kind == "inline-eval")
                .map(|f| f.line)
                .collect()
        };
        assert_eq!(lines(&summary.preflight.unsupported), vec![5, 7]);
        assert_eq!(lines(&summary.preflight.potential_gaps), vec![4]);

        // It finishes after recovery: its pending findings must not replace
        // the proven classification, and it answers with that classification
        // stamped at the recovery's place in the register order (#458).
        let stale =
            super::preflight_blocking(&path_arg, &handle, &state, &runs, &in_flight, &mut |_| {})
                .unwrap();
        assert_eq!(Some(&stale.register), summary.preflight_register.as_ref());
        assert_eq!(lines(&stale.unsupported), vec![5, 7]);
        assert_eq!(lines(&stale.potential_gaps), vec![4]);

        assert_eq!(
            register_eval_lines(),
            vec![5, 7],
            "recovery closes the proven literals (3, 8), downgrades the \
             const-shaped argument (4) out of Unsupported, and keeps the \
             runtime-computed sites (5, 7)"
        );
        let findings = state.findings.lock().unwrap().list().unwrap();
        assert!(
            findings
                .iter()
                .all(|f| !f.message.contains("pending AST proof")),
            "no pending wording survives recovery"
        );
    }

    #[test]
    fn a_recovery_that_fails_before_publishing_leaves_preflight_pending() {
        // AC-0200 (#439 review): reconciliation runs only after the graph
        // holds the recovered facts. A recovery that fails first (here: the
        // graph store is unusable) must not close the literal eval whose
        // facts never reached the graph.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), "eval('function f() {}');\n").unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let pending = || -> Vec<(i64, bool)> {
            state
                .findings
                .lock()
                .unwrap()
                .list()
                .unwrap()
                .into_iter()
                .filter(|f| f.detector == ingest::preflight::DETECTOR_ID)
                .map(|f| (f.line, f.message.contains("pending AST proof")))
                .collect()
        };
        super::preflight_uncancelled(&project.to_string_lossy(), &handle, &state).unwrap();
        assert_eq!(pending(), vec![(1, true)]);

        let source = super::register_local_source(&state, &project).unwrap();
        let operation = super::source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _graph = state.graph.lock().unwrap();
                    panic!("poison the graph store");
                })
                .join()
                .unwrap_err();
        });
        assert!(super::run_ingest(&source, &operation, &execution, &handle, &state).is_err());

        assert_eq!(
            pending(),
            vec![(1, true)],
            "the unpublished proof closed nothing"
        );
    }

    #[test]
    fn a_local_recovery_cancelled_during_reconciliation_writes_no_preflight_findings() {
        // AC-0200 (#489): the reconciled findings are written only once the
        // job has settled as completed. A cancel that lands while the
        // reconciliation scan runs — after the last cancellation check —
        // wins the race, so the register keeps preflight's pending findings.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), EVAL_FIXTURE).unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        super::preflight_uncancelled(&project.to_string_lossy(), &handle, &state).unwrap();
        let source = super::register_local_source(&state, &project).unwrap();
        assert_eq!(
            preflight_register(&state, &source.repo_key),
            vec![(1, true), (2, true)]
        );

        let event_handle = handle.clone();
        let listener = handle.listen("job://detail", move |event| {
            let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            if payload["detail"] != super::RECONCILE_PREFLIGHT_DETAIL {
                return;
            }
            let state = event_handle.state::<super::AppState>();
            state
                .jobs
                .lock()
                .unwrap()
                .cancel(payload["id"].as_i64().unwrap())
                .unwrap();
        });
        let operation = super::source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
        let error = super::run_ingest(&source, &operation, &execution, &handle, &state)
            .err()
            .unwrap();
        handle.unlisten(listener);

        assert_eq!(error, "cancelled");
        let job = state.jobs.lock().unwrap().get(execution.id()).unwrap();
        assert_eq!(job.status, "cancelled");
        assert_eq!(
            preflight_register(&state, &source.repo_key),
            vec![(1, true), (2, true)],
            "a cancelled recovery leaves the pending findings untouched"
        );
    }

    /// A bare git repo holding `files`, cloneable as `file://<path>`.
    fn preflight_bare_repo(
        dir: &std::path::Path,
        name: &str,
        files: &[(&str, &str)],
    ) -> std::path::PathBuf {
        let src = dir.join(format!("{name}-src"));
        std::fs::create_dir_all(&src).unwrap();
        for (path, content) in files {
            std::fs::write(src.join(path), content).unwrap();
        }
        let git = |args: &[&str], cwd: &std::path::Path| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q", "-b", "main"], &src);
        git(&["add", "."], &src);
        git(&["commit", "-q", "-m", "init"], &src);
        let bare = dir.join(format!("{name}.git"));
        git(
            &[
                "clone",
                "-q",
                "--bare",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            dir,
        );
        bare
    }

    /// `repo`'s preflight register rows as (line, still pending AST proof).
    fn preflight_register(state: &super::AppState, repo: &str) -> Vec<(i64, bool)> {
        state
            .findings
            .lock()
            .unwrap()
            .list_for(repo)
            .unwrap()
            .into_iter()
            .filter(|f| f.detector == ingest::preflight::DETECTOR_ID)
            .inspect(|f| assert_eq!(f.kind, "unsupported"))
            .map(|f| (f.line, f.message.contains("pending AST proof")))
            .collect()
    }

    fn inline_eval_lines(findings: &[ingest::preflight::PatternFinding]) -> Vec<u64> {
        findings
            .iter()
            .filter(|f| f.kind == "inline-eval")
            .map(|f| f.line)
            .collect()
    }

    const EVAL_FIXTURE: &str = "eval('function legacySetup() {}');\neval(getCode());\n";

    #[test]
    fn github_recovery_writes_its_reconciled_preflight_findings() {
        // AC-0209 (#446): an `add_repo` recovery classifies the clone and
        // writes the result to the register exactly as a local recovery
        // does (AC-0200): the proven literal closes and the dynamic site
        // stays one Unsupported finding, with no pending wording.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "shop", &[("app.ts", EVAL_FIXTURE)]);
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();

        let summary =
            super::add_repo_blocking(format!("file://{}", bare.display()), handle.clone()).unwrap();

        assert_eq!(preflight_register(&state, &summary.repo), vec![(2, false)]);
        assert_eq!(inline_eval_lines(&summary.preflight.unsupported), vec![2]);
        assert!(summary.preflight.potential_gaps.is_empty());
    }

    #[test]
    fn a_cancelled_github_recovery_writes_no_preflight_findings() {
        // AC-0209 (#485 review): the findings are written only once the job
        // has settled as completed, so a cancel during recovery leaves the
        // register untouched.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "shop", &[("app.ts", EVAL_FIXTURE)]);
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let event_handle = handle.clone();
        let listener = handle.listen("job://detail", move |event| {
            let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            let state = event_handle.state::<super::AppState>();
            let _ = state
                .jobs
                .lock()
                .unwrap()
                .cancel(payload["id"].as_i64().unwrap());
        });

        let error = super::add_repo_blocking(format!("file://{}", bare.display()), handle.clone())
            .err()
            .unwrap();
        handle.unlisten(listener);

        assert_eq!(error, "cancelled");
        let findings = state.findings.lock().unwrap().list().unwrap();
        assert!(
            findings.is_empty(),
            "a cancelled recovery writes nothing: {findings:?}"
        );
    }

    #[test]
    fn manifest_recovery_writes_each_repos_reconciled_preflight_findings() {
        // AC-0209 (#446): every repo an `add_system` recovers, cloned or
        // local, gets its own reconciled register findings, and the summary
        // carries each report in manifest order.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "web", &[("app.ts", EVAL_FIXTURE)]);
        let api = dir.path().join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("server.ts"), "new Function(getCode());\n").unwrap();
        let manifest = dir.path().join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            format!(
                "[[repos]]\nurl = \"file://{}\"\n\n[[repos]]\nurl = \"api\"\n",
                bare.display()
            ),
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();

        let summary =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .unwrap();

        let reports: Vec<(&str, Vec<u64>)> = summary
            .preflights
            .iter()
            .map(|p| (p.repo.as_str(), inline_eval_lines(&p.report.unsupported)))
            .collect();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].1, vec![2], "the clone's dynamic eval");
        assert_eq!(reports[1].1, vec![1], "the local repo's dynamic Function");
        assert_eq!(preflight_register(&state, reports[0].0), vec![(2, false)]);
        assert_eq!(preflight_register(&state, reports[1].0), vec![(1, false)]);
    }

    #[test]
    fn coverage_basis_binds_each_plugins_corpus() {
        // AC-0263: a corpus edit that is re-gated with the same artifact
        // changes routing/coverage claims, so it must change the basis that
        // recorded inventories are compared against; no plugins keeps the
        // pre-existing basis.
        let plugin = |gate_key: &str| super::ActivePlugin {
            plugin_id: "t0.adapter-ruby".into(),
            path: std::path::PathBuf::from("/plugins/t0.adapter-ruby.wasm"),
            content_hash: "a".repeat(64),
            extensions: vec!["rb".into()],
            cells: vec![],
            languages: Default::default(),
            gate_key: gate_key.into(),
            limits: adapters_plugin_host::PluginLimits::default(),
        };
        let registries = crate::Registries::builtin();
        let first = super::coverage_config_hash(&[plugin("gate-1")], registries);
        assert_eq!(
            first,
            super::coverage_config_hash(&[plugin("gate-1")], registries)
        );
        assert_ne!(
            first,
            super::coverage_config_hash(&[plugin("gate-2")], registries)
        );
        let none = serde_json::json!({
            "basis": "coverage-config-v2",
            "plugins": [],
            "registries": registries.identity(),
        });
        assert_eq!(
            super::coverage_config_hash(&[], registries),
            core_prov::content_hash(none.to_string().as_bytes())
        );
        // AC-1803: non-default sandbox bounds change the basis; the default
        // bounds keep it.
        let mut tight = plugin("gate-1");
        tight.limits.max_fuel = 1_000_000_000;
        assert_ne!(first, super::coverage_config_hash(&[tight], registries));
    }

    #[test]
    fn system_recovery_counts_cross_repo_adr_links_in_coverage() {
        // AC-0685 (T-0685, #1847): a decision repo recovered before the service its
        // ADR governs counts the link the system-wide relink makes, in the
        // returned and the stored inventory, rather than the zero its own
        // isolated read saw.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs-repo");
        let service = dir.path().join("service");
        std::fs::create_dir_all(docs.join("docs/adr")).unwrap();
        std::fs::create_dir_all(&service).unwrap();
        std::fs::write(service.join("app.ts"), "export function handle() {}\n").unwrap();
        let manifest = dir.path().join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            "[[repos]]\nurl = \"docs-repo\"\n\n[[repos]]\nurl = \"service\"\n",
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        // The service's symbol id names its registered repo identity.
        let probe =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .unwrap();
        let service_repo = probe.coverage[1].repo.clone();
        std::fs::write(
            docs.join("docs/adr/ADR-0001-service.md"),
            format!(
                "# Service ownership\n\n- **Status:** Accepted\n- **Governs:** \
                 `sym:{service_repo}@app.ts#handle`\n"
            ),
        )
        .unwrap();

        let summary =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .unwrap();

        let adr_row = |coverage: &ingest::coverage::RepoCoverage| {
            coverage
                .producers
                .iter()
                .find(|row| row.id == spec::FOUND_ADR_EXTRACTOR_ID)
                .map(|row| (row.files, row.nodes, row.edges))
                .unwrap()
        };
        let docs_repo = summary.coverage[0].repo.clone();
        assert_eq!(adr_row(&summary.coverage[0]), (1, 1, 1));
        let graph = state.graph.lock().unwrap();
        let decides = graph.edges_with_labels(&["DECIDES"]).unwrap();
        assert_eq!(decides.len(), 1);
        assert_eq!(decides[0].dst, format!("sym:{service_repo}@app.ts#handle"));
        let stored = graph
            .coverage_records()
            .unwrap()
            .into_iter()
            .find(|stored| stored.record.repo_key == docs_repo)
            .unwrap();
        let inventory: ingest::coverage::RepoCoverage =
            serde_json::from_str(&stored.record.inventory_json).unwrap();
        assert_eq!(adr_row(&inventory), (1, 1, 1));
        assert_eq!(inventory, summary.coverage[0]);
    }

    #[test]
    fn another_systems_recovery_leaves_a_repos_documents_as_its_inventory_counts() {
        // AC-0677 (T-0677): ingesting one system re-reads only its own
        // repositories' documentation, so a repository published earlier keeps
        // the documents its stored inventory counts, even after its files change.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        for (root, name) in [(&first, "a"), (&second, "b")] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("README.md"), "# Readme\n").unwrap();
            std::fs::write(
                root.join("cartograph.system.toml"),
                format!("[[repos]]\nurl = \"{name}\"\n"),
            )
            .unwrap();
        }
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let manifest = |root: &std::path::Path| {
            root.join("cartograph.system.toml")
                .to_string_lossy()
                .into_owned()
        };
        let summary = super::add_system_blocking(manifest(&first), handle.clone()).unwrap();
        let repo = summary.coverage[0].repo.clone();
        std::fs::write(first.join("a/GUIDE.md"), "# Guide\n").unwrap();

        super::add_system_blocking(manifest(&second), handle.clone()).unwrap();

        let graph = state.graph.lock().unwrap();
        let (nodes, _) = graph.read_snapshot().unwrap();
        let documents = nodes
            .iter()
            .filter(|node| node.label == "Document" && node.id.starts_with(&format!("doc:{repo}@")))
            .count();
        let stored = graph
            .coverage_records()
            .unwrap()
            .into_iter()
            .find(|stored| stored.record.repo_key == repo)
            .unwrap();
        let inventory: ingest::coverage::RepoCoverage =
            serde_json::from_str(&stored.record.inventory_json).unwrap();
        assert_eq!(documents, 1);
        assert_eq!(inventory.documents.map(|counted| counted.read), Some(1));
    }

    #[test]
    fn manifest_observation_inputs_are_captured_under_their_own_identity() {
        // AC-1041 (ADR-0080, #385): a declared state file inside the manifest
        // folder is captured under `input/<source id>` and its observed
        // evidence cites that identity and the declared relative path; one
        // outside the folder is still read, but its evidence stays uncaptured
        // and coverage names it unverified. Neither becomes a system repo.
        use tauri::Manager;
        const STATE: &str = r#"{"format_version":"1.0","values":{"root_module":{"resources":[
            {"address":"aws_sqs_queue.jobs","mode":"managed","type":"aws_sqs_queue","name":"jobs",
             "values":{"url":"https://sqs.example/jobs"},"sensitive_values":{}}]}}}"#;
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        for repo in ["inside", "outside"] {
            let root = system.join(repo);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("main.tf"),
                "resource \"aws_sqs_queue\" \"jobs\" {\n  name = \"jobs\"\n}\n",
            )
            .unwrap();
        }
        std::fs::write(system.join("inside.tfstate.json"), STATE).unwrap();
        std::fs::write(dir.path().join("outside.tfstate.json"), STATE).unwrap();
        let manifest = system.join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            "[[repos]]\nurl = \"inside\"\nlayers = [\"infra\"]\nstate_json = \"./inside.tfstate.json\"\n\n\
             [[repos]]\nurl = \"outside\"\nlayers = [\"infra\"]\nstate_json = \"../outside.tfstate.json\"\n",
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let summary =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .unwrap();
        let sources = state.sources.lock().unwrap().list().unwrap();
        let source_of = |name: &str| {
            sources
                .iter()
                .find(|source| source.repo_key.ends_with(name) || source.display_name == name)
                .unwrap()
                .clone()
        };
        let inside = source_of("inside");
        let outside = source_of("outside");
        let resources = state
            .graph
            .lock()
            .unwrap()
            .nodes_with_label("Resource")
            .unwrap();
        let observed = |source: &crate::sources::RegisteredSource| {
            resources
                .iter()
                .find(|node| node.id == format!("res:{}@aws_sqs_queue.jobs", source.repo_key))
                .unwrap()
                .props["observed_prov"]["evidence"][0]
                .clone()
        };
        let cited = observed(&inside);
        assert_eq!(cited["repo"], format!("input/{}", inside.source_id));
        assert_eq!(cited["path"], "inside.tfstate.json");
        assert_eq!(cited["commit_sha"], "workdir");
        let uncited = observed(&outside);
        assert_eq!(uncited["repo"], "");
        assert_eq!(uncited["commit_sha"], "");
        let reason = |source: &crate::sources::RegisteredSource| {
            summary
                .coverage
                .iter()
                .find(|coverage| coverage.repo == source.repo_key)
                .unwrap()
                .producers
                .iter()
                .find(|producer| producer.id == dynamic::EXTRACTOR_ID)
                .unwrap()
                .stopping_reason
                .clone()
        };
        assert!(
            !reason(&inside).contains("unverified"),
            "{}",
            reason(&inside)
        );
        assert!(
            reason(&outside).ends_with("unverified (outside the manifest folder or past the capture limits): ../outside.tfstate.json"),
            "{}",
            reason(&outside)
        );
        // Captured inputs are not source registrations of the system.
        assert!(
            super::system_contents_of(&resources)
                .iter()
                .all(|entry| !entry.repo.starts_with("input/"))
        );
    }

    #[test]
    fn manifest_observation_input_captures_are_retained_and_forgotten_with_their_source() {
        // AC-1043 (ADR-0080 slice 2a, #385): a recovered system retains each
        // repo's captured observation inputs beside its own captures; the
        // retention preview counts them and a forget removes them too.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        let root = system.join("infra");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tf"),
            "resource \"aws_sqs_queue\" \"jobs\" {\n  name = \"jobs\"\n}\n",
        )
        .unwrap();
        std::fs::write(
            system.join("state.json"),
            r#"{"format_version":"1.0","values":{"root_module":{"resources":[]}}}"#,
        )
        .unwrap();
        let manifest = system.join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            "[[repos]]\nurl = \"infra\"\nlayers = [\"infra\"]\nstate_json = \"state.json\"\n",
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
            .unwrap();
        let source = state.sources.lock().unwrap().list().unwrap().remove(0);
        let retained = super::primary_source::preview(&state, &source.source_id).unwrap();
        assert_eq!(retained.input_capture_ids.len(), 1);
        assert_eq!(
            retained.captures,
            retained.capture_ids.len() + retained.input_capture_ids.len()
        );
        assert!(retained.files >= 1);
        super::primary_source::forget(&state, &source.source_id, &retained.fingerprint).unwrap();
        let after = super::primary_source::preview(&state, &source.source_id).unwrap();
        assert!(after.input_capture_ids.is_empty());
        assert_eq!(after.captures, after.capture_ids.len());
    }

    #[test]
    fn observed_facts_get_receipts_owned_by_their_input_and_forgotten_with_their_source() {
        // AC-1045 (ADR-0079, #385): a fact the captured Terraform state
        // enriched gets an observed receipt for its exact span, owned by the
        // input identity, counted in its source's retention preview and
        // reclaimed when the source is forgotten.
        use tauri::Manager;
        const STATE: &str = r#"{"format_version":"1.0","values":{"root_module":{"resources":[
            {"address":"aws_sqs_queue.jobs","mode":"managed","type":"aws_sqs_queue","name":"jobs",
             "values":{"url":"https://sqs.example/jobs"},"sensitive_values":{}}]}}}"#;
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        let root = system.join("infra");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tf"),
            "resource \"aws_sqs_queue\" \"jobs\" {\n  name = \"jobs\"\n}\n",
        )
        .unwrap();
        std::fs::write(system.join("state.json"), STATE).unwrap();
        let manifest = system.join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            "[[repos]]\nurl = \"infra\"\nlayers = [\"infra\"]\nstate_json = \"state.json\"\n",
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
            .unwrap();
        let source = state.sources.lock().unwrap().list().unwrap().remove(0);
        let retained = super::primary_source::preview(&state, &source.source_id).unwrap();
        assert_eq!(retained.input_receipts, 1);
        assert!(retained.receipts >= retained.input_receipts);
        super::primary_source::forget(&state, &source.source_id, &retained.fingerprint).unwrap();
        let after = super::primary_source::preview(&state, &source.source_id).unwrap();
        assert_eq!(after.input_receipts, 0);
    }

    #[test]
    fn observed_receipts_follow_the_published_fact_view() {
        // AC-1045 (ADR-0079): observed receipts are minted from the facts
        // publication keeps, the last occurrence of a node, so an earlier
        // occurrence's observation mints nothing.
        const INPUT: &str = "input/src_11111111111111111111111111111111";
        const STATE: &[u8] = br#"{"address":"aws_sqs_queue.jobs"}"#;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.json"), STATE).unwrap();
        let capture = source_capture::capture_working_tree(
            dir.path(),
            &source_capture::SourceId::new(INPUT).unwrap(),
            &["state.json".into()],
            source_capture::CaptureLimits::default(),
        )
        .unwrap();
        let observed = |repo: &str| {
            serde_json::to_value(
                core_prov::Provenance::new(
                    core_prov::Tier::Dynamic,
                    core_prov::ConfidenceTier::Confirmed,
                    vec![core_prov::EvidenceRef {
                        repo: repo.into(),
                        path: "state.json".into(),
                        byte_start: 0,
                        byte_end: STATE.len() as u64,
                        commit_sha: "workdir".into(),
                    }],
                    "t1.terraform-state",
                    b"Observed aws_sqs_queue.jobs",
                )
                .unwrap(),
            )
            .unwrap()
        };
        let resource = |id: &str, repo: &str, url: &str| Node {
            id: id.into(),
            label: "Resource".into(),
            props: serde_json::json!({"observed_prov": observed(repo), "url": url}),
        };
        let facts = core_graph::facts::FactStore::from_facts(
            vec![
                resource("res:a", INPUT, "first"),
                resource("res:a", INPUT, "last"),
                resource("res:b", INPUT, "first"),
                resource("res:b", "acme/api", "last"),
            ],
            vec![],
        )
        .unwrap();
        let receipts = super::observed_receipts_packed(&capture, &facts).unwrap();
        assert_eq!(receipts.len(), 1);
        assert!(receipts[0].matches_node(&resource("res:a", INPUT, "last")));
    }

    #[test]
    fn observed_receipts_bind_their_facts_and_superseded_ones_are_reclaimed() {
        // AC-1047 (ADR-0079, #385): a recovery publishes an observed binding
        // from each fact its captured state enriched to that fact's observed
        // receipt, beside its primary binding; the preview counts it as a
        // current reference; recovering a changed state rebinds the fact and
        // reclaims the receipt nothing binds any more, tombstoned with the
        // input capture that superseded it, not the code capture.
        use tauri::Manager;
        let state_with = |url: &str| {
            format!(
                r#"{{"format_version":"1.0","values":{{"root_module":{{"resources":[
            {{"address":"aws_sqs_queue.jobs","mode":"managed","type":"aws_sqs_queue","name":"jobs",
             "values":{{"url":"{url}"}},"sensitive_values":{{}}}}]}}}}}}"#
            )
        };
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        let root = system.join("infra");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("main.tf"),
            "resource \"aws_sqs_queue\" \"jobs\" {\n  name = \"jobs\"\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("ready.ts"),
            "export function ready(ok: boolean) { if (!ok) return false; return true; }\n",
        )
        .unwrap();
        std::fs::write(
            system.join("state.json"),
            state_with("https://sqs.example/a"),
        )
        .unwrap();
        let manifest = system.join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            "[[repos]]\nurl = \"infra\"\nlayers = [\"infra\", \"server\"]\nstate_json = \"state.json\"\n",
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let observed = || {
            let source = state.sources.lock().unwrap().list().unwrap().remove(0);
            let observations = state
                .graph
                .lock()
                .unwrap()
                .source_observations_for_repo(&source.repo_key)
                .unwrap();
            (source, observations)
        };
        super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
            .unwrap();
        let (source, first) = observed();
        assert_eq!(first.len(), 1);
        let fact = core_graph::FactKey::Node {
            id: format!("res:{}@aws_sqs_queue.jobs", source.repo_key),
        };
        assert_eq!(first[0].fact, fact);
        assert!(first[0].receipt_id.starts_with("obs-input-v1:"));
        assert_eq!(
            state
                .graph
                .lock()
                .unwrap()
                .current_source_observation(&fact)
                .unwrap()
                .as_ref(),
            Some(&first[0])
        );
        let preview = super::primary_source::preview(&state, &source.source_id).unwrap();
        assert_eq!(preview.input_receipts, 1);
        assert!(preview.current_references >= 1);
        let first_input = preview.input_capture_ids.clone();
        assert_eq!(first_input.len(), 1);

        std::fs::write(
            system.join("state.json"),
            state_with("https://sqs.example/b"),
        )
        .unwrap();
        super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
            .unwrap();
        let (_, second) = observed();
        assert_eq!(second.len(), 1);
        assert_ne!(second[0].receipt_id, first[0].receipt_id);
        let preview = super::primary_source::preview(&state, &source.source_id).unwrap();
        assert_eq!(preview.input_receipts, 1);
        assert!(!preview.capture_ids.is_empty());
        let receipts = state.primary_sources.receipts.lock().unwrap();
        assert!(matches!(
            receipts.receipt_status(&first[0].receipt_id).unwrap(),
            super::primary_source::ReceiptStatus::Superseded { .. }
        ));
        // The superseding capture is the new input capture.
        let superseding = receipts
            .tombstone_capture_ids(&first[0].receipt_id)
            .unwrap();
        assert_eq!(superseding.len(), 1);
        assert_ne!(superseding, first_input);
        assert!(preview.input_capture_ids.contains(&superseding[0]));
        assert!(!preview.capture_ids.contains(&superseding[0]));
    }

    #[test]
    fn observed_bindings_describe_and_read_back_their_input_span() {
        // AC-1049 (ADR-0079, #385): a fact's observed binding describes the
        // input identity, its registered source and owning repo, and reads
        // its cited span back from the retained input capture; a primary
        // read never accepts the observed receipt, and a superseded observed
        // receipt is no longer readable.
        use tauri::Manager;
        let state_with = |url: &str| {
            format!(
                r#"{{"format_version":"1.0","values":{{"root_module":{{"resources":[
            {{"address":"aws_sqs_queue.jobs","mode":"managed","type":"aws_sqs_queue","name":"jobs",
             "values":{{"url":"{url}"}},"sensitive_values":{{}}}}]}}}}}}"#
            )
        };
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system");
        std::fs::create_dir_all(system.join("infra")).unwrap();
        std::fs::write(
            system.join("infra/main.tf"),
            "resource \"aws_sqs_queue\" \"jobs\" {\n  name = \"jobs\"\n}\n",
        )
        .unwrap();
        std::fs::write(
            system.join("state.json"),
            state_with("https://sqs.example/a"),
        )
        .unwrap();
        let manifest = system.join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            "[[repos]]\nurl = \"infra\"\nlayers = [\"infra\"]\nstate_json = \"state.json\"\n",
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
            .unwrap();
        let source = state.sources.lock().unwrap().list().unwrap().remove(0);
        let fact = core_graph::FactKey::Node {
            id: format!("res:{}@aws_sqs_queue.jobs", source.repo_key),
        };
        let observed = super::primary_source::describe_observed(&state, &fact).unwrap();
        assert_eq!(observed.source_id, format!("input/{}", source.source_id));
        assert_eq!(observed.registered_source_id, source.source_id);
        assert_eq!(observed.repo_key, source.repo_key);
        assert_eq!(observed.scope, "observed_input_only");
        assert_eq!(observed.ranges.len(), 1);
        assert_eq!(observed.ranges[0].path, "state.json");
        let text =
            super::primary_source::read_observed(&state, &fact, &observed.receipt_id, 0).unwrap();
        // The span is the one the observation evidence records: the
        // resource's address in the captured state.
        assert_eq!(text.text, "\"aws_sqs_queue.jobs\"");
        assert_eq!(text.path, "state.json");
        assert!(super::primary_source::read_observed(&state, &fact, "receipt:other", 0).is_err());
        assert!(super::primary_source::read(&state, &fact, &observed.receipt_id, 0).is_err());

        std::fs::write(
            system.join("state.json"),
            state_with("https://sqs.example/b"),
        )
        .unwrap();
        super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle).unwrap();
        assert!(
            super::primary_source::read_observed(&state, &fact, &observed.receipt_id, 0).is_err()
        );
        let current = super::primary_source::describe_observed(&state, &fact).unwrap();
        assert_ne!(current.receipt_id, observed.receipt_id);
        let text =
            super::primary_source::read_observed(&state, &fact, &current.receipt_id, 0).unwrap();
        assert_eq!(text.text, "\"aws_sqs_queue.jobs\"");
    }

    #[test]
    fn system_recovery_reports_each_repos_coverage_inventory() {
        // AC-0238 (#588): each recovered repo lists which producers ran,
        // which its layer hints excluded, which inputs were missing and
        // which capabilities are unsupported, with scoped reasons.
        use ingest::coverage::ProducerState;
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let api = dir.path().join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("main.go"), "package main\n\nfunc main() {}\n").unwrap();
        std::fs::write(api.join("task.rb"), "puts 1\n").unwrap();
        let infra = dir.path().join("infra");
        std::fs::create_dir_all(&infra).unwrap();
        std::fs::write(
            infra.join("main.tf"),
            "resource \"aws_sqs_queue\" \"jobs\" {\n  name = \"jobs\"\n}\n",
        )
        .unwrap();
        // Pulumi only: its `res:` resources take no Terraform state.
        let stack = dir.path().join("stack");
        std::fs::create_dir_all(&stack).unwrap();
        std::fs::write(
            stack.join("index.ts"),
            "import * as aws from '@pulumi/aws';\nexport const queue = new aws.sqs.Queue('orders', {});\n\
             export function handler() { return 1; }\n",
        )
        .unwrap();
        // Terraform with no resource: observed state has nothing to enrich.
        let vars = dir.path().join("vars");
        std::fs::create_dir_all(&vars).unwrap();
        std::fs::write(vars.join("variables.tf"), "variable \"region\" {}\n").unwrap();
        let manifest = dir.path().join("cartograph.system.toml");
        // The api's state file is unparseable: its layer hints exclude
        // infrastructure, so it must never be read.
        std::fs::write(api.join("api.tfstate.json"), "not json").unwrap();
        std::fs::write(
            &manifest,
            "[[repos]]\nurl = \"api\"\nlayers = [\"server\"]\nstate_json = \"api/api.tfstate.json\"\n\n\
             [[repos]]\nurl = \"infra\"\nlayers = [\"infra\"]\n\n\
             [[repos]]\nurl = \"vars\"\nlayers = [\"infra\"]\n\n\
             [[repos]]\nurl = \"stack\"\nlayers = [\"infra\"]\n",
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let _state = app.state::<super::AppState>();

        let summary =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .unwrap();

        assert_eq!(summary.coverage.len(), 4, "one inventory per repo");
        let state_of = |coverage: &ingest::coverage::RepoCoverage, id: &str| {
            let producer = coverage
                .producers
                .iter()
                .find(|producer| producer.id == id)
                .unwrap();
            (producer.state, producer.stopping_reason.clone())
        };
        let api = &summary.coverage[0];
        assert_eq!(api.layer_hints, ["server"]);
        assert_eq!(state_of(api, "t0.adapter-go").0, ProducerState::Ran);
        assert_eq!(
            state_of(api, "t0.iac-terraform"),
            (
                ProducerState::NotSelected,
                "not selected: layer hints [server] exclude infra and cloud".to_string()
            )
        );
        assert_eq!(
            api.unsupported
                .iter()
                .map(|unsupported| unsupported.capability.as_str())
                .collect::<Vec<_>>(),
            ["Ruby"]
        );
        assert_eq!(
            state_of(api, dynamic::EXTRACTOR_ID),
            (
                ProducerState::NotSelected,
                "not selected: layer hints [server] exclude infra and cloud; \
                 declared state_json was not read"
                    .to_string()
            )
        );
        assert!(api.limited);

        let infra = &summary.coverage[1];
        let (tf_state, tf_reason) = state_of(infra, "t0.iac-terraform");
        assert_eq!(tf_state, ProducerState::Ran);
        assert!(
            tf_reason.starts_with("completed over 1 files of "),
            "{tf_reason}"
        );
        assert!(tf_reason.ends_with("for layers infra"), "{tf_reason}");
        assert_eq!(
            state_of(infra, dynamic::EXTRACTOR_ID).0,
            ProducerState::MissingInput
        );
        assert_eq!(
            infra
                .missing_inputs
                .iter()
                .map(|missing| missing.input.as_str())
                .collect::<Vec<_>>(),
            ["state_json"]
        );
        assert!(infra.limited);

        let vars = &summary.coverage[2];
        assert_eq!(state_of(vars, "t0.iac-terraform").0, ProducerState::Ran);
        assert_eq!(
            state_of(vars, dynamic::EXTRACTOR_ID).0,
            ProducerState::NotApplicable
        );
        assert!(vars.missing_inputs.is_empty());

        // Infra-only TS still reports the files it read, though projection
        // kept only the Pulumi facts; Pulumi resources need no state_json.
        let stack = &summary.coverage[3];
        let (ts_state, ts_reason) = state_of(stack, "t0.adapter-ts");
        assert_eq!(ts_state, ProducerState::Ran);
        assert!(
            ts_reason.starts_with("completed over 1 files of ")
                && ts_reason.ends_with(
                    "for Pulumi infrastructure bindings only: application layers were not selected"
                ),
            "{ts_reason}"
        );
        assert_eq!(
            state_of(stack, dynamic::EXTRACTOR_ID).0,
            ProducerState::NotApplicable
        );
        assert!(
            stack
                .missing_inputs
                .iter()
                .all(|missing| missing.input != "state_json")
        );

        // AC-0239: each inventory was published with its repo's facts, bound
        // to its registration, the job and the plugin basis, and qualifies a
        // context read under the current registrations. (T-0239)
        let state = app.state::<super::AppState>();
        let records = state.graph.lock().unwrap().coverage_records().unwrap();
        let sources = state.sources.lock().unwrap().list().unwrap();
        assert_eq!(records.len(), 4);
        let reported = |repo: &str| {
            summary
                .coverage
                .iter()
                .find(|coverage| coverage.repo == repo)
                .unwrap()
        };
        for stored in &records {
            let record = &stored.record;
            let reported = reported(&record.repo_key);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&record.inventory_json).unwrap(),
                serde_json::to_value(reported).unwrap()
            );
            assert_eq!(record.ingest_id, format!("job:{}", summary.job_id));
            assert_eq!(
                record.config_hash,
                super::coverage_config_hash(&[], crate::Registries::builtin())
            );
            let source = sources
                .iter()
                .find(|source| source.repo_key == record.repo_key)
                .unwrap();
            assert_eq!(
                record.registered_source_id.as_deref(),
                Some(source.source_id.as_str())
            );
        }
        let page = super::context::qualified_query_graph(
            &state.graph,
            || super::context::coverage_scope(&handle, &state),
            context_hub::coverage::QualifiedRequest {
                request: context_hub::QueryRequest {
                    max_facts: 1,
                    max_bytes: 65536,
                    ..context_hub::QueryRequest::default()
                },
                context_id: None,
            },
        )
        .unwrap();
        assert_eq!(page.coverage.repos.len(), 4);
        for repo in &page.coverage.repos {
            assert_eq!(repo.status, context_hub::coverage::CoverageStatus::Recorded);
            assert_eq!(repo.limited, Some(reported(&repo.repo).limited));
        }
    }

    #[test]
    fn a_failed_system_recovery_writes_no_preflight_findings() {
        // AC-0209 (#446): the first repo recovers, the second fails (its
        // declared state file is missing), so the system recovery fails and
        // no repo's register findings change — not even the first repo's.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "web", &[("app.ts", EVAL_FIXTURE)]);
        let api = dir.path().join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("server.ts"), "export const ok = 1;\n").unwrap();
        let manifest = dir.path().join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            format!(
                "[[repos]]\nurl = \"file://{}\"\n\n[[repos]]\nurl = \"api\"\nstate_json = \"missing.tfstate.json\"\n",
                bare.display()
            ),
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();

        let error =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .err()
                .unwrap();
        assert!(error.contains("state_json"), "{error}");

        let findings = state.findings.lock().unwrap().list().unwrap();
        assert!(
            findings.is_empty(),
            "a failed recovery writes nothing: {findings:?}"
        );
    }

    /// Each preflight started on `done`: its repo and whether it ran.
    type PreflightOutcomes = std::sync::Mutex<Vec<(String, Result<(), String>)>>;

    /// On the recovery's `done` event, run a real preflight (the `preflight`
    /// command's own path: register the run, scan on a worker, persist) over
    /// every registered source's root — the user saw the job finish and
    /// started a new preflight before the reconciliation wrote. Returns each
    /// scan's outcome, keyed by repo.
    fn preflight_on_done(
        handle: &tauri::AppHandle<tauri::test::MockRuntime>,
    ) -> (tauri::EventId, std::sync::Arc<PreflightOutcomes>) {
        use tauri::{Listener, Manager};
        let outcomes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = outcomes.clone();
        let event_handle = handle.clone();
        let listener = handle.listen("job://changed", move |event| {
            let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            if payload["status"] != "done" {
                return;
            }
            let state = event_handle.state::<super::AppState>();
            let sources = state.sources.lock().unwrap().list().unwrap();
            for source in sources {
                let path = source.root().to_string_lossy().into_owned();
                let outcome = tauri::async_runtime::block_on(super::start_preflight(
                    path,
                    1,
                    event_handle.clone(),
                ))
                .map(drop);
                seen.lock().unwrap().push((source.repo_key, outcome));
            }
        });
        (listener, outcomes)
    }

    /// Every preflight started on `done` succeeded, and each repo's register
    /// holds that preflight's pending findings, not the reconciliation's.
    fn assert_preflights_on_done_won(
        state: &super::AppState,
        outcomes: &PreflightOutcomes,
        expected: &[(&str, Vec<(i64, bool)>)],
    ) {
        let outcomes = outcomes.lock().unwrap();
        assert_eq!(outcomes.len(), expected.len(), "{outcomes:?}");
        for (repo, outcome) in outcomes.iter() {
            assert_eq!(outcome, &Ok(()), "the preflight of {repo} ran");
        }
        for (repo, rows) in expected {
            assert_eq!(&preflight_register(state, repo), rows, "{repo}");
        }
    }

    #[test]
    fn a_preflight_started_after_a_local_recovery_is_done_keeps_its_findings() {
        // AC-0214 (#497): the reconciliation fence is taken before `done` is
        // published, so a preflight begun once the user sees it is newer:
        // it runs, and the reconciliation neither refuses nor overwrites it.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), EVAL_FIXTURE).unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let source = super::register_local_source(&state, &project).unwrap();
        let operation = super::source_operation(&state, vec![(source.clone(), false)]).unwrap();
        let (_, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
        let (listener, outcomes) = preflight_on_done(&handle);

        super::run_ingest(&source, &operation, &execution, &handle, &state).unwrap();
        handle.unlisten(listener);

        let pending = vec![(1, true), (2, true)];
        assert_preflights_on_done_won(&state, &outcomes, &[(&source.repo_key, pending)]);
    }

    #[test]
    fn a_preflight_started_after_a_github_recovery_is_done_keeps_its_findings() {
        if crate::registered_source_tests::isolated_managed_exact_test(
            "tests::a_preflight_started_after_a_github_recovery_is_done_keeps_its_findings",
        ) {
            return;
        }
        // AC-0214 (#497 and its review): as for a local recovery. The managed
        // clone is ready and its write reservation released before `done`,
        // so the preflight can read it.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "shop", &[("app.ts", EVAL_FIXTURE)]);
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let (listener, outcomes) = preflight_on_done(&handle);

        let summary =
            super::add_repo_blocking(format!("file://{}", bare.display()), handle.clone()).unwrap();
        handle.unlisten(listener);

        let pending = vec![(1, true), (2, true)];
        assert_preflights_on_done_won(&state, &outcomes, &[(&summary.repo, pending)]);
    }

    #[test]
    fn a_preflight_started_after_a_manifest_recovery_is_done_keeps_its_findings() {
        if crate::registered_source_tests::isolated_managed_exact_test(
            "tests::a_preflight_started_after_a_manifest_recovery_is_done_keeps_its_findings",
        ) {
            return;
        }
        // AC-0214 (#497 and its review): as for a local recovery, for every
        // repo, cloned or local.
        use tauri::{Listener, Manager};
        let dir = tempfile::tempdir().unwrap();
        let bare = preflight_bare_repo(dir.path(), "web", &[("app.ts", EVAL_FIXTURE)]);
        let api = dir.path().join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("server.ts"), "new Function(getCode());\n").unwrap();
        let manifest = dir.path().join("cartograph.system.toml");
        std::fs::write(
            &manifest,
            format!(
                "[[repos]]\nurl = \"file://{}\"\n\n[[repos]]\nurl = \"api\"\n",
                bare.display()
            ),
        )
        .unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let (listener, outcomes) = preflight_on_done(&handle);

        let summary =
            super::add_system_blocking(manifest.to_string_lossy().into_owned(), handle.clone())
                .unwrap();
        handle.unlisten(listener);

        assert_eq!(summary.preflights.len(), 2);
        assert_preflights_on_done_won(
            &state,
            &outcomes,
            &[
                (&summary.preflights[0].repo, vec![(1, true), (2, true)]),
                (&summary.preflights[1].repo, vec![(1, true)]),
            ],
        );
    }

    #[test]
    fn a_retried_recovery_returns_its_reconciled_preflight_report() {
        // AC-0216 (#458): Jobs → Retry re-runs the recovery through
        // `run_ingest`; the reconciled preflight report and its register
        // stamp reach the UI with the retried job row, as they do from the
        // recovery command itself.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), EVAL_FIXTURE).unwrap();
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let source = super::register_local_source(&state, &project).unwrap();
        let job_id = {
            let (job, execution) = super::start_job(&state, &source.ingest_job_kind()).unwrap();
            state.jobs.lock().unwrap().cancel(job.id).unwrap();
            drop(execution);
            job.id
        };

        let retried = super::retry_job_blocking(job_id, handle.clone()).unwrap();

        assert_eq!(retried.job.status, "done");
        let recovery = retried.recovery.as_ref().expect("the recovery's report");
        assert_eq!(inline_eval_lines(&recovery.preflight.unsupported), vec![2]);
        let stamp = recovery.preflight_register.as_ref().unwrap();
        assert_eq!(stamp.repo, source.repo_key);
        assert_eq!(
            preflight_register(&state, &source.repo_key),
            vec![(2, false)]
        );
        let wire = serde_json::to_value(&retried).unwrap();
        assert_eq!(wire["id"], job_id, "the job row stays flat on the wire");
        assert_eq!(
            wire["recovery"]["preflight_register"]["repo"],
            source.repo_key
        );
    }

    #[test]
    fn an_older_recovery_fence_never_overwrites_a_newer_reconciliation() {
        // AC-0216 (#514 review): two overlapping recoveries of one repo. The
        // newer fence reconciles first; the older one, resuming late, writes
        // nothing, so the register and the stamps agree on the newer report.
        let runs = super::PreflightRuns::default();
        let older = runs.reserve_reconcile().unwrap();
        let newer = runs.reserve_reconcile().unwrap();
        let stamp = runs
            .reconcile(&newer, "repo", &Default::default(), || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(stamp.epoch, newer.epoch);
        let mut wrote = false;
        let late = runs
            .reconcile(&older, "repo", &Default::default(), || {
                wrote = true;
                Ok(())
            })
            .unwrap();
        assert!(
            late.is_none() && !wrote,
            "the older recovery writes nothing"
        );
    }

    #[test]
    fn a_reconcile_fence_orders_before_later_preflights() {
        // AC-0214 (#497): a preflight begun before the fence is refused after
        // the reconciliation; one begun after it wins whether it persists
        // before the reconciliation (which then writes nothing) or after.
        let runs = super::PreflightRuns::default();
        let fence = runs.reserve_reconcile().unwrap();
        let newer = runs.begin().unwrap();
        runs.persist_if_current(&newer, "repo", |_| Ok(())).unwrap();
        let mut wrote = false;
        let written = runs
            .reconcile(&fence, "repo", &Default::default(), || {
                wrote = true;
                Ok(())
            })
            .unwrap();
        assert!(written.is_none() && !wrote, "the newer findings stand");

        let runs = super::PreflightRuns::default();
        let fence = runs.reserve_reconcile().unwrap();
        let later = runs.begin().unwrap();
        let stamp = runs
            .reconcile(&fence, "repo", &Default::default(), || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!((stamp.repo.as_str(), stamp.epoch), ("repo", fence.epoch));
        assert!(matches!(
            runs.persist_if_current(&later, "repo", |_| Ok(())),
            Ok(super::Persisted::Written(()))
        ));

        let runs = super::PreflightRuns::default();
        let older = runs.begin().unwrap();
        let fence = runs.reserve_reconcile().unwrap();
        assert!(
            runs.reconcile(&fence, "repo", &Default::default(), || Ok(()))
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            runs.persist_if_current(&older, "repo", |_| Ok(())),
            Ok(super::Persisted::Reconciled { .. })
        ));
    }

    /// A mock app managing the stores a preflight touches, rooted in `dir`.
    pub(crate) fn preflight_test_app(dir: &std::path::Path) -> crate::test_app::TestApp {
        use tauri::Manager;
        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app"),
        );
        let state_path = dir.join("state.db");
        app.manage(super::PreflightRuns::default());
        app.manage(super::AppState {
            graph: std::sync::Mutex::new(SqliteGraphStore::open(dir.join("graph.db")).unwrap()),
            jobs: std::sync::Mutex::new(super::JobStore::open(&state_path).unwrap()),
            investigations: crate::investigations::InvestigationRuntime::default(),
            job_execution_locks: super::job_execution_host_tests::locks(&state_path),
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            proposals: std::sync::Mutex::new(
                agents::ProposalStore::open(state_path.with_file_name("proposals.sqlite")).unwrap(),
            ),
            drafts: std::sync::Mutex::new(
                agents::draft_store::DraftStore::open(
                    state_path.with_file_name("section-drafts.sqlite"),
                )
                .unwrap(),
            ),
            work_packets: super::work_packets::PacketRuntime::open(&state_path).unwrap(),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            embedding_cache: None,
            knowledge_overlay: None,
            curated_settled: std::sync::Arc::default(),
            primary_sources: super::primary_source::PrimarySourceStore::open(dir).unwrap(),
            sources: test_source_registry(&state_path, &std::collections::BTreeSet::new()),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        });
        app
    }

    #[test]
    fn preflight_command_scans_on_a_worker_and_reports_progress() {
        // AC-0197 (#235): the preflight command never parses on the invoking
        // (webview/main) thread — the eval-claims walk runs on a blocking
        // worker and streams `preflight://progress` while it reads.
        use tauri::Listener;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("app.ts"), "eval(getCode());\n").unwrap();
        let app = preflight_test_app(dir.path());
        let emitters = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = emitters.clone();
        app.listen_any("preflight://progress", move |event| {
            seen.lock()
                .unwrap()
                .push((std::thread::current().id(), event.payload().to_string()));
        });

        let caller = std::thread::current().id();
        let report = tauri::async_runtime::block_on(super::start_preflight(
            project.to_string_lossy().into_owned(),
            7,
            app.handle().clone(),
        ))
        .unwrap();

        assert!(report.unsupported.iter().any(|f| f.kind == "inline-eval"));
        let emitters = emitters.lock().unwrap();
        // The listing walk's first ping (the root) always fires, and so does
        // the first file ping after it (#453).
        assert_eq!(emitters.len(), 2, "{emitters:?}");
        assert!(
            emitters.iter().all(|(worker, _)| *worker != caller),
            "extraction ran on the calling thread"
        );
        assert_eq!(
            emitters[0].1,
            r#"{"run":7,"path":"","done":0,"total":null}"#
        );
        assert_eq!(
            emitters[1].1,
            r#"{"run":7,"path":"app.ts","done":0,"total":1}"#
        );
    }

    #[test]
    fn the_report_walk_reports_progress_too() {
        // AC-0197 (#434 review): the report walk reads every file, not just
        // the TS sources the eval pass parses, so it must keep the progress
        // line moving. A Go-only project gives the eval pass nothing to do.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("main.go"), "package main\n").unwrap();
        let app = preflight_test_app(dir.path());
        let state = app.state::<super::AppState>();
        let runs = app.state::<super::PreflightRuns>();
        let token = runs.begin().unwrap();

        let mut steps = Vec::new();
        super::preflight_blocking(
            &project.to_string_lossy(),
            app.handle(),
            &state,
            &runs,
            &token,
            &mut |step| steps.push((step.path.to_string(), step.done, step.total)),
        )
        .unwrap();
        assert_eq!(
            steps,
            vec![
                (String::new(), 0, None),
                ("main.go".to_string(), 0, Some(1)),
                // The Go skip pass (#1258) walks Go recovery's scope after the
                // report walk, at the finished count.
                (String::new(), 1, Some(1)),
                ("main.go".to_string(), 1, Some(1))
            ]
        );
    }

    #[test]
    fn cancelled_preflight_stops_before_the_next_file_and_persists_nothing() {
        // AC-0198 (#235): a cancel stops the walk before the next file and
        // leaves the finding register untouched; a new preflight supersedes
        // (cancels) the one in flight.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = preflight_eval_project(dir.path());
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let runs = app.state::<super::PreflightRuns>();
        let path_arg = project.to_string_lossy().into_owned();

        // Cancelled while reading the first file: nothing after it is parsed.
        let token = runs.begin().unwrap();
        let mut announced = Vec::new();
        let result =
            super::preflight_blocking(&path_arg, &handle, &state, &runs, &token, &mut |step| {
                if step.total.is_none() {
                    return; // still listing the tree
                }
                announced.push(step.path.to_string());
                runs.cancel().unwrap();
            });
        assert_eq!(result.unwrap_err(), super::PREFLIGHT_CANCELLED);
        assert_eq!(announced, vec!["a.ts".to_string()]);
        assert!(
            state.findings.lock().unwrap().list().unwrap().is_empty(),
            "a cancelled preflight persists no findings"
        );

        // A new preflight supersedes the one in flight.
        let first = runs.begin().unwrap();
        let second = runs.begin().unwrap();
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());

        // The uncancelled run completes and persists its findings.
        let report =
            super::preflight_blocking(&path_arg, &handle, &state, &runs, &second, &mut |_| {})
                .unwrap();
        assert_eq!(report.unsupported.len(), 3);
        assert_eq!(state.findings.lock().unwrap().list().unwrap().len(), 3);
    }

    /// Three TS files, each with one dynamic eval — three findings when a
    /// preflight completes.
    fn preflight_eval_project(dir: &std::path::Path) -> std::path::PathBuf {
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        for name in ["a.ts", "b.ts", "c.ts"] {
            std::fs::write(project.join(name), "eval(getCode());\n").unwrap();
        }
        project
    }

    #[test]
    fn cancelled_preflight_stops_while_listing_the_tree() {
        // AC-0198 (#453): a cancel that lands while the tree is still being
        // listed stops the listing walk at the next directory — no later
        // directory is entered, no file is announced or read, and nothing is
        // persisted.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        for sub in ["a", "b", "c"] {
            std::fs::create_dir_all(project.join(sub)).unwrap();
            std::fs::write(project.join(sub).join("x.ts"), "eval(getCode());\n").unwrap();
        }
        let app = preflight_test_app(dir.path());
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let runs = app.state::<super::PreflightRuns>();
        let token = runs.begin().unwrap();

        let mut steps = Vec::new();
        let result = super::preflight_blocking(
            &project.to_string_lossy(),
            &handle,
            &state,
            &runs,
            &token,
            &mut |step| {
                steps.push((step.path.to_string(), step.total));
                if step.path == "a" {
                    runs.cancel().unwrap();
                }
            },
        );
        assert_eq!(result.unwrap_err(), super::PREFLIGHT_CANCELLED);
        assert_eq!(
            steps,
            vec![(String::new(), None), ("a".to_string(), None)],
            "the walk stopped listing before directory b"
        );
        assert!(state.findings.lock().unwrap().list().unwrap().is_empty());
    }

    #[test]
    fn preflight_cancelled_before_its_worker_starts_never_runs() {
        // AC-0198 (#434 review): the run is registered when the command
        // starts, not when the blocking worker picks it up, so a Cancel that
        // arrives while the worker is still queued stops it.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = preflight_eval_project(dir.path());
        let app = preflight_test_app(dir.path());
        let state = app.state::<super::AppState>();

        let run = super::start_preflight(
            project.to_string_lossy().into_owned(),
            1,
            app.handle().clone(),
        );
        app.state::<super::PreflightRuns>().cancel().unwrap();
        let result = tauri::async_runtime::block_on(run);

        assert_eq!(result.unwrap_err(), super::PREFLIGHT_CANCELLED);
        assert!(state.findings.lock().unwrap().list().unwrap().is_empty());
    }

    #[test]
    fn preflight_supersession_follows_invocation_order() {
        // AC-0198 (#434 review): a newer preflight supersedes an older one
        // by invocation order, even when the older one's worker runs first.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let project = preflight_eval_project(dir.path());
        let app = preflight_test_app(dir.path());
        let path_arg = project.to_string_lossy().into_owned();

        let older = super::start_preflight(path_arg.clone(), 1, app.handle().clone());
        let newer = super::start_preflight(path_arg, 2, app.handle().clone());

        assert_eq!(
            tauri::async_runtime::block_on(older).unwrap_err(),
            super::PREFLIGHT_CANCELLED
        );
        let report = tauri::async_runtime::block_on(newer).unwrap();
        assert_eq!(report.unsupported.len(), 3);
        let state = app.state::<super::AppState>();
        assert_eq!(state.findings.lock().unwrap().list().unwrap().len(), 3);
    }

    #[test]
    fn recovery_fences_only_older_preflights_of_its_repo() {
        // AC-0200 (#439 review): a reconciliation refuses preflights of the
        // same repo that began before it; other repos and later preflights
        // are unaffected, and a failed reconciliation fences nothing.
        let runs = super::PreflightRuns::default();
        let older = runs.begin().unwrap();
        let fence = runs.reserve_reconcile().unwrap();
        runs.reconcile(&fence, "repo", &Default::default(), || Ok(()))
            .unwrap();
        let written = |token: &super::PreflightToken, repo: &str| {
            matches!(
                runs.persist_if_current(token, repo, |_| Ok(())),
                Ok(super::Persisted::Written(()))
            )
        };
        assert!(!written(&older, "repo"));
        assert!(written(&older, "other"));
        let newer = runs.begin().unwrap();
        assert!(written(&newer, "repo"));

        let fence = runs.reserve_reconcile().unwrap();
        runs.reconcile(&fence, "other", &Default::default(), || {
            Err("disk full".to_string())
        })
        .unwrap_err();
        assert!(written(&newer, "other"));
    }

    #[test]
    fn cancel_never_waits_on_persistence() {
        // AC-0198 (#434 review): `cancel_preflight` is synchronous, so it
        // must not block on a register write in progress; a cancel that
        // lands first still prevents the write.
        use std::sync::mpsc;
        let runs = std::sync::Arc::new(super::PreflightRuns::default());
        let token = runs.begin().unwrap();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        runs.persist_if_current(&token, "repo", |_| {
            let runs = runs.clone();
            std::thread::spawn(move || {
                runs.cancel().unwrap();
                cancelled_tx.send(()).unwrap();
            });
            cancelled_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| "cancel waited for the write".to_string())
        })
        .unwrap();

        let mut wrote = false;
        let late = runs.persist_if_current(&token, "repo", |_| {
            wrote = true;
            Ok(())
        });
        assert_eq!(late.unwrap_err(), super::PREFLIGHT_CANCELLED);
        assert!(!wrote, "a cancelled run writes nothing");
    }

    #[test]
    fn a_cancel_during_the_register_write_rolls_it_back() {
        // AC-0215 (#493): a cancel that lands while the preflight's register
        // write is in progress — after the pre-write check, before commit —
        // rolls the write back: the register keeps its previous rows and the
        // scan reports that it was cancelled. Cancel still never waits.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let app = preflight_test_app(dir.path());
        let state = app.state::<super::AppState>();
        let runs = std::sync::Arc::new(super::PreflightRuns::default());
        let report = |line: u64| ingest::preflight::PreflightReport {
            unsupported: vec![ingest::preflight::PatternFinding {
                kind: "inline-eval".into(),
                path: "app.ts".into(),
                line,
                message: "m".into(),
                detector: "d".into(),
                request_adapter: None,
            }],
            ..Default::default()
        };
        let write = |token: &super::PreflightToken, line: u64, cancel_mid_write: bool| {
            runs.persist_if_current(token, "repo", |live| {
                // The cancel arrives from another thread once the batch is
                // staged, exactly when `live` is re-checked before commit.
                let live_after_cancel = || {
                    if cancel_mid_write {
                        let runs = runs.clone();
                        std::thread::spawn(move || runs.cancel().unwrap())
                            .join()
                            .unwrap();
                    }
                    live()
                };
                super::persist_preflight_findings_if(
                    &state,
                    "repo",
                    &report(line),
                    &live_after_cancel,
                )
            })
        };
        write(&runs.begin().unwrap(), 1, false).unwrap();

        let abandoned = runs.begin().unwrap();
        assert_eq!(
            write(&abandoned, 2, true).unwrap_err(),
            super::PREFLIGHT_CANCELLED
        );
        let lines: Vec<i64> = state
            .findings
            .lock()
            .unwrap()
            .list_for("repo")
            .unwrap()
            .into_iter()
            .map(|f| f.line)
            .collect();
        assert_eq!(
            lines,
            vec![1],
            "the cancelled run's findings were rolled back"
        );
    }

    #[test]
    fn a_cancel_after_the_commit_claim_loses_and_the_scan_completes() {
        // AC-0215 (#493 review): the commit decision is atomic with
        // cancellation. A cancel that lands right after the run claimed its
        // commit — the last step before `tx.commit()` — loses: the write
        // commits and the scan reports its result, never a cancellation it
        // did not honour. Either the cancel wins and nothing is written, or
        // the run completes; never both.
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let app = preflight_test_app(dir.path());
        let state = app.state::<super::AppState>();
        let runs = super::PreflightRuns::default();
        let token = runs.begin().unwrap();
        let report = ingest::preflight::PreflightReport {
            unsupported: vec![ingest::preflight::PatternFinding {
                kind: "inline-eval".into(),
                path: "app.ts".into(),
                line: 1,
                message: "m".into(),
                detector: "d".into(),
                request_adapter: None,
            }],
            ..Default::default()
        };
        let cancel_won = std::cell::Cell::new(None);
        let result = runs.persist_if_current(&token, "repo", |claim_commit| {
            super::persist_preflight_findings_if(&state, "repo", &report, &|| {
                let claimed = claim_commit();
                // The cancel arrives between the claim and the commit.
                cancel_won.set(Some(runs.cancel().unwrap()));
                claimed
            })
        });

        assert_eq!(cancel_won.get(), Some(false), "the cancel lost the race");
        assert!(!token.is_cancelled());
        assert!(result.is_ok(), "the run completed: {result:?}");
        assert_eq!(
            state
                .findings
                .lock()
                .unwrap()
                .list_for("repo")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn preflight_register_writes_never_interleave() {
        // AC-0198 (#434 review): a superseding run's write waits for the
        // older run's write in progress, so the newer result lands last.
        use std::sync::atomic::{AtomicBool, Ordering};
        let runs = std::sync::Arc::new(super::PreflightRuns::default());
        let older = runs.begin().unwrap();
        let writing = std::sync::Arc::new(AtomicBool::new(false));
        let (inside_tx, inside_rx) = std::sync::mpsc::channel();
        let newer = {
            let runs = runs.clone();
            let writing = writing.clone();
            std::thread::spawn(move || {
                inside_rx.recv().unwrap();
                let newer = runs.begin().unwrap();
                runs.persist_if_current(&newer, "repo", |_| {
                    assert!(!writing.load(Ordering::SeqCst), "writes interleaved");
                    Ok(())
                })
            })
        };
        runs.persist_if_current(&older, "repo", |_| {
            writing.store(true, Ordering::SeqCst);
            inside_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
            writing.store(false, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        newer.join().unwrap().unwrap();
    }

    // AC-0658: a gated plugin that emits a malformed fact for one file of
    // several does not fail the ingest. The other files' facts join the
    // graph, no fact from the refused file by that plugin does, and coverage
    // names the plugin, the file and the reason class. Two runs give the
    // same graph and the same inventory.
    #[test]
    fn plugin_refused_on_one_file_keeps_the_ingest_and_is_named_in_coverage() {
        use ingest::coverage::RefusalReason;
        // A text-form component: the first byte of a source picks its
        // behaviour, `M` being a fact with a `prov` field a plugin may not
        // write.
        const REFUSING_ADAPTER: &[u8] =
            include_bytes!("../../crates/adapters-plugin-host/tests/fixtures/refusing-adapter.wat");

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("a.zz"), "fine").unwrap();
        std::fs::write(project.join("src/b.zz"), "Malformed").unwrap();
        std::fs::write(project.join("src/c.zz"), "fine too").unwrap();
        let artifact = dir.path().join("t0.plugin-refusing.wasm");
        std::fs::write(&artifact, REFUSING_ADAPTER).unwrap();
        let root = super::paths::canonicalize(&project).unwrap();
        let hash = core_prov::content_hash(REFUSING_ADAPTER);
        let active = [super::ActivePlugin {
            plugin_id: "t0.plugin-refusing".into(),
            path: artifact,
            content_hash: hash.clone(),
            extensions: vec!["zz".into()],
            cells: Vec::new(),
            languages: std::collections::BTreeMap::new(),
            gate_key: "gate".into(),
            limits: adapters_plugin_host::PluginLimits::default(),
        }];
        let pinned = format!("t0.plugin-refusing@{}", &hash[..12]);
        let repo = "acme/refused";

        let run = || {
            let mut reads = super::ProducerReads::default();
            let (extraction, layers, _) = super::extract_tree_with_primary(
                &root,
                repo,
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                &std::collections::BTreeSet::new(),
                None,
                None,
                &[],
                &mut super::RepoExtractionCache::default(),
                &active,
                &mut |_| {},
                None,
                &mut Vec::new(),
                &mut reads,
            )
            .expect("one refused file does not fail the ingest");
            let coverage = super::repo_coverage(
                repo,
                "workdir",
                None,
                &[],
                &layers,
                &extraction,
                &super::DeclaredInputs::default(),
                &reads,
            );
            (extraction, reads, coverage)
        };
        let (extraction, reads, coverage) = run();

        let by_plugin: Vec<&str> = extraction
            .nodes
            .iter()
            .filter(|node| node.props["prov"]["extractor_id"] == serde_json::json!(pinned))
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(
            by_plugin,
            ["acme/refused:a.zz", "acme/refused:src/c.zz"],
            "the other files' facts join the graph and the refused file's do not"
        );
        assert!(
            !extraction
                .nodes
                .iter()
                .any(|node| node.id == "acme/refused:src/b.zz"),
            "no fact from the refused file"
        );
        assert!(
            !extraction
                .edges
                .iter()
                .any(
                    |edge| edge.props["prov"]["extractor_id"] == serde_json::json!(pinned)
                        && (edge.src.contains("b.zz") || edge.dst.contains("b.zz"))
                )
        );

        assert_eq!(reads.plugins.len(), 1);
        assert_eq!(reads.plugins[0].id, pinned);
        assert_eq!(reads.plugins[0].count.files, 2);
        assert_eq!(
            reads.plugins[0].refused,
            [("src/b.zz".to_string(), RefusalReason::MalformedFact)]
        );

        let row = coverage
            .producers
            .iter()
            .find(|producer| producer.id == pinned)
            .expect("the plugin has a row");
        assert_eq!(row.files, 2, "the refused file is not counted as covered");
        assert_eq!(row.nodes, 2);
        assert_eq!(
            row.stopping_reason,
            "read 2 of the 3 files its extensions claim in acme/refused@workdir; the host \
             refused its extraction of 1 (1 malformed fact), and no fact from those files by \
             this plugin joined the graph"
        );
        let refusals = row.refusals.as_ref().expect("the refusal is recorded");
        assert_eq!(
            (
                refusals.malformed_fact,
                refusals.guest_error,
                refusals.bound_exceeded,
                refusals.omitted
            ),
            (1, 0, 0, 0)
        );
        assert_eq!(refusals.files.len(), 1);
        assert_eq!(refusals.files[0].path, "src/b.zz");
        assert_eq!(refusals.files[0].reason, RefusalReason::MalformedFact);
        assert!(coverage.limited);
        let entry = coverage
            .unsupported
            .iter()
            .find(|entry| entry.capability == RefusalReason::MalformedFact.capability())
            .expect("the refused file is a not-covered entry");
        assert_eq!(
            (entry.occurrences, entry.first_site.as_str()),
            (1, "src/b.zz")
        );
        // The reason class only: the host's error text quotes the guest.
        let inventory_json = serde_json::to_string(&coverage).unwrap();
        assert!(!inventory_json.contains("method"), "{inventory_json}");

        // The coverage-qualified read accepts the inventory and counts the
        // refusal as a limitation.
        let record = core_graph::coverage::CoverageRecord {
            repo_key: repo.into(),
            registered_source_id: Some("source:refused".into()),
            ingest_id: "job:1".into(),
            config_hash: "config".into(),
            inventory_json: inventory_json.clone(),
        };
        let qualification = context_hub::coverage::CoverageQualification::new(
            &[context_hub::coverage::ScopedRepo {
                repo_key: repo.into(),
                registered_source_id: Some("source:refused".into()),
                config_hash: "config".into(),
                layer_hints: None,
            }],
            &[core_graph::coverage::StoredCoverage {
                inventory_hash: record.inventory_hash(),
                record,
            }],
        )
        .expect("a refusal inventory is a valid coverage@1 inventory");
        assert_eq!(
            qualification.repos[0].status,
            context_hub::coverage::CoverageStatus::Recorded
        );
        assert_eq!(qualification.repos[0].limited, Some(true));
        assert_eq!(qualification.repos[0].counts.unwrap().unsupported, 1);

        // Determinism: the same tree gives the same graph and inventory.
        let (again, _, coverage_again) = run();
        assert_eq!(
            serde_json::to_string(&extraction.nodes).unwrap(),
            serde_json::to_string(&again.nodes).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&extraction.edges).unwrap(),
            serde_json::to_string(&again.edges).unwrap()
        );
        assert_eq!(
            inventory_json,
            serde_json::to_string(&coverage_again).unwrap()
        );
    }

    #[test]
    fn a_projects_plugin_bounds_reach_each_extraction_call() {
        // AC-1803 (#1653): the bounds an active plugin carries are the ones
        // each call runs under. Under a fuel bound too small for any call,
        // every claimed file is refused as bound-exceeded and none of its
        // facts join the graph; the default bounds read every file.
        const REFUSING_ADAPTER: &[u8] =
            include_bytes!("../../crates/adapters-plugin-host/tests/fixtures/refusing-adapter.wat");
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.zz"), "fine").unwrap();
        std::fs::write(project.join("b.zz"), "fine too").unwrap();
        let artifact = dir.path().join("t0.plugin-refusing.wasm");
        std::fs::write(&artifact, REFUSING_ADAPTER).unwrap();
        let root = super::paths::canonicalize(&project).unwrap();
        let run = |limits| {
            let active = [super::ActivePlugin {
                plugin_id: "t0.plugin-refusing".into(),
                path: artifact.clone(),
                content_hash: core_prov::content_hash(REFUSING_ADAPTER),
                extensions: vec!["zz".into()],
                cells: Vec::new(),
                languages: std::collections::BTreeMap::new(),
                gate_key: "gate".into(),
                limits,
            }];
            let mut reads = super::ProducerReads::default();
            super::extract_tree_with_primary(
                &root,
                "acme/bounded",
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                &std::collections::BTreeSet::new(),
                None,
                None,
                &[],
                &mut super::RepoExtractionCache::default(),
                &active,
                &mut |_| {},
                None,
                &mut Vec::new(),
                &mut reads,
            )
            .expect("bound refusals do not fail the ingest");
            reads.plugins.remove(0)
        };
        let starved = run(adapters_plugin_host::PluginLimits {
            max_fuel: 1,
            ..adapters_plugin_host::PluginLimits::default()
        });
        assert_eq!(starved.count.files, 0);
        assert_eq!(starved.count.nodes, 0);
        assert_eq!(
            starved.refused,
            [
                (
                    "a.zz".to_string(),
                    ingest::coverage::RefusalReason::BoundExceeded
                ),
                (
                    "b.zz".to_string(),
                    ingest::coverage::RefusalReason::BoundExceeded
                ),
            ]
        );
        let read = run(adapters_plugin_host::PluginLimits::default());
        assert_eq!(read.count.files, 2);
        assert!(read.refused.is_empty());
    }

    #[test]
    fn plugin_lane_end_to_end_gate_accept_reingest_closes_finding() {
        // AC-0093 (#201): request → gate pass → accept → re-scan. An
        // uncovered language surfaces with the request-adapter action; a
        // gated + enabled plugin then counts as coverage (the finding
        // closes) and routes its claimed files during extraction, with
        // pinned facts identical across runs (AC-0069 extension).
        use tauri::Manager;
        const OK_ADAPTER: &[u8] = include_bytes!(
            "../../crates/adapters-plugin-host/tests/fixtures/compiled/ok-adapter.wasm"
        );

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let adapters = project.join(".cartograph/adapters");
        std::fs::create_dir_all(&adapters).unwrap();
        std::fs::write(project.join("app.rb"), "puts 'hi'\n").unwrap();
        std::fs::write(adapters.join("t0.plugin-fixture.wasm"), OK_ADAPTER).unwrap();
        std::fs::write(
            adapters.join("t0.plugin-fixture.golden.json"),
            serde_json::json!({
                "extensions": ["rb"],
                "cases": [{
                    "path": "src/lib.rs",
                    "source": "hello world",
                    "nodes": [
                        {"id": "golden:src/lib.rs", "label": "TestNode", "props": {"len": 11}}
                    ],
                    "edges": [{
                        "src": "golden:src/lib.rs",
                        "dst": "golden:src/lib.rs",
                        "label": "SELF",
                        "props": {}
                    }],
                }],
                "coverage": [{
                    "language": "Ruby",
                    "framework": "core",
                    "capability": "types",
                    "support": "supported",
                    "cases": ["src/lib.rs"]
                }],
            })
            .to_string(),
        )
        .unwrap();

        let app = crate::test_app::TestApp::new(
            tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .expect("mock app"),
        );
        let state_path = dir.path().join("state.db");
        app.manage(super::AppState {
            graph: std::sync::Mutex::new(
                SqliteGraphStore::open(dir.path().join("graph.db")).unwrap(),
            ),
            jobs: std::sync::Mutex::new(super::JobStore::open(&state_path).unwrap()),
            investigations: crate::investigations::InvestigationRuntime::default(),
            job_execution_locks: super::job_execution_host_tests::locks(&state_path),
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            proposals: std::sync::Mutex::new(
                agents::ProposalStore::open(state_path.with_file_name("proposals.sqlite")).unwrap(),
            ),
            drafts: std::sync::Mutex::new(
                agents::draft_store::DraftStore::open(
                    state_path.with_file_name("section-drafts.sqlite"),
                )
                .unwrap(),
            ),
            work_packets: super::work_packets::PacketRuntime::open(&state_path).unwrap(),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            embedding_cache: None,
            knowledge_overlay: None,
            curated_settled: std::sync::Arc::default(),
            primary_sources: super::primary_source::PrimarySourceStore::open(
                state_path.parent().unwrap(),
            )
            .unwrap(),
            sources: test_source_registry(&state_path, &std::collections::BTreeSet::new()),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        });
        let handle = app.handle().clone();
        let state = app.state::<super::AppState>();
        let root = super::paths::canonicalize(&project).unwrap();
        let root_key = root.display().to_string();
        let path_arg = project.to_string_lossy().into_owned();

        // Before: Ruby is uncovered, with the request-adapter action, and
        // the finding persists to the register.
        let before = super::preflight_uncancelled(&path_arg, &handle, &state).unwrap();
        let finding = before
            .unsupported
            .iter()
            .find(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
            .expect("Ruby surfaces as uncovered");
        assert_eq!(finding.request_adapter.as_deref(), Some("Ruby"));
        let repo = state
            .sources
            .lock()
            .unwrap()
            .get_by_root(&root)
            .unwrap()
            .unwrap()
            .repo_key;
        assert!(
            state
                .findings
                .lock()
                .unwrap()
                .list_for(&repo)
                .unwrap()
                .iter()
                .any(|f| f.message.contains("Ruby"))
        );
        // Discovery alone never activates: nothing extracts yet.
        assert!(
            super::active_plugins_for_root(&handle, &state, &root)
                .unwrap()
                .is_empty()
        );

        // Gate the artifact (the request lane lands it here), then accept.
        let (_, execution) = super::start_job(&state, "plugin-gate:t0.plugin-fixture").unwrap();
        let report = super::plugin_gate_blocking("t0.plugin-fixture", &execution, &handle).unwrap();
        assert_eq!(report["passed"], serde_json::json!(true));
        let hash = core_prov::content_hash(OK_ADAPTER);
        state
            .settings
            .lock()
            .unwrap()
            .set_plugin_enabled(&root_key, "t0.plugin-fixture", &hash, true)
            .unwrap();

        // Re-scan: the plugin counts as coverage and the finding closes.
        let after = super::preflight_uncancelled(&path_arg, &handle, &state).unwrap();
        let ruby = after
            .languages
            .iter()
            .find(|l| l.language == "Ruby")
            .expect("Ruby still detected");
        assert_eq!(ruby.adapter.as_deref(), Some("t0.plugin-fixture"));
        // AC-0263: the gated corpus's declared cell is reported with it.
        assert_eq!(
            serde_json::to_value(&ruby.coverage).unwrap(),
            serde_json::json!([{
                "language": "Ruby",
                "framework": "core",
                "capability": "types",
                "support": "supported",
                "note": ""
            }])
        );
        assert!(
            !after
                .unsupported
                .iter()
                .any(|f| f.kind == "uncovered-language" && f.message.contains("Ruby"))
        );
        assert!(
            !state
                .findings
                .lock()
                .unwrap()
                .list_for(&repo)
                .unwrap()
                .iter()
                .any(|f| f.message.contains("Ruby"))
        );

        // Re-ingest routes the claimed file through the plugin: pinned
        // facts, identical across runs (plugin-active determinism).
        let active = super::active_plugins_for_root(&handle, &state, &root).unwrap();
        assert_eq!(active.len(), 1);
        let run = |cache: &mut super::RepoExtractionCache| {
            super::extract_tree_incremental(
                &root,
                &repo,
                "workdir",
                &[],
                &std::collections::BTreeMap::new(),
                None,
                None,
                &[],
                cache,
                &active,
                &mut |_| {},
            )
            .unwrap()
            .0
        };
        let first = run(&mut super::RepoExtractionCache::default());
        let second = run(&mut super::RepoExtractionCache::default());
        let plugin_node = first
            .nodes
            .iter()
            .find(|n| n.id == format!("{repo}:app.rb"))
            .expect("plugin fact joins the extraction");
        assert_eq!(
            plugin_node.props["plugin_artifact_hash"],
            serde_json::json!(hash)
        );
        // Host-filled provenance (#208 review): the routed fact satisfies
        // the tier/confidence invariant and cites its mediated source.
        assert_eq!(
            plugin_node.props["prov"]["tier"],
            serde_json::json!("Deterministic")
        );
        assert_eq!(
            plugin_node.props["prov"]["evidence"][0]["path"],
            serde_json::json!("app.rb")
        );
        assert_eq!(
            serde_json::to_string(&first.nodes).unwrap(),
            serde_json::to_string(&second.nodes).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&first.edges).unwrap(),
            serde_json::to_string(&second.edges).unwrap()
        );

        // AC-0238: the inventory names the gated artifact, not the bare
        // plugin id, and counts the claimed files it actually read.
        let mut reads = super::ProducerReads::default();
        let (extraction, layers, _) = super::extract_tree_with_primary(
            &root,
            &repo,
            "workdir",
            &[],
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeSet::new(),
            None,
            None,
            &[],
            &mut super::RepoExtractionCache::default(),
            &active,
            &mut |_| {},
            None,
            &mut Vec::new(),
            &mut reads,
        )
        .unwrap();
        let pinned = format!("t0.plugin-fixture@{}", &hash[..12]);
        assert_eq!(reads.plugins.len(), 1);
        assert_eq!(reads.plugins[0].id, pinned);
        assert_eq!(reads.plugins[0].count.files, 1, "one claimed file read");
        assert!(reads.plugins[0].refused.is_empty());
        let coverage = super::repo_coverage(
            &repo,
            "workdir",
            None,
            &[],
            &layers,
            &extraction,
            &super::DeclaredInputs::default(),
            &reads,
        );
        let row = coverage
            .producers
            .iter()
            .find(|producer| producer.id == pinned)
            .expect("pinned plugin row");
        assert_eq!(row.files, 1);
        assert!(row.nodes >= 1);

        // AC-0263: the verdict covers the exact corpus bytes. Editing the
        // claims after gating leaves the plugin ungated, so neither its
        // extensions nor its cells count until the gate reruns.
        let corpus_path = adapters.join("t0.plugin-fixture.golden.json");
        let mut corpus: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&corpus_path).unwrap()).unwrap();
        corpus["coverage"][0]["capability"] = serde_json::json!("http_endpoints");
        std::fs::write(&corpus_path, corpus.to_string()).unwrap();
        assert!(
            super::active_plugins_for_root(&handle, &state, &root)
                .unwrap()
                .is_empty()
        );
        let edited = super::preflight_uncancelled(&path_arg, &handle, &state).unwrap();
        let ruby = edited
            .languages
            .iter()
            .find(|l| l.language == "Ruby")
            .expect("Ruby still detected");
        assert_eq!((ruby.adapter.as_deref(), ruby.coverage.len()), (None, 0));
    }

    #[test]
    fn user_plugin_covers_roots_that_do_not_own_the_project_copy() {
        // #208 review: in a multi-root session, a project copy of an id in
        // root A must not shadow the gated+enabled user-level copy that
        // root B's coverage relies on — the active scan discovers per root.
        const OK_ADAPTER: &[u8] = include_bytes!(
            "../../crates/adapters-plugin-host/tests/fixtures/compiled/ok-adapter.wasm"
        );

        let dir = tempfile::tempdir().unwrap();
        let user_dir = dir.path().join("user-adapters");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join("t0.plugin-fixture.wasm"), OK_ADAPTER).unwrap();
        std::fs::write(
            user_dir.join("t0.plugin-fixture.golden.json"),
            serde_json::json!({ "extensions": ["rb"], "cases": [] }).to_string(),
        )
        .unwrap();
        // Root A owns a *different* project artifact under the same id;
        // root B has no project copy at all.
        let root_a = dir.path().join("a");
        let a_adapters = root_a.join(".cartograph/adapters");
        std::fs::create_dir_all(&a_adapters).unwrap();
        std::fs::write(a_adapters.join("t0.plugin-fixture.wasm"), b"other bytes").unwrap();
        let root_b = dir.path().join("b");
        std::fs::create_dir_all(&root_b).unwrap();

        let state_path = dir.path().join("state.db");
        let state = super::AppState {
            graph: std::sync::Mutex::new(
                SqliteGraphStore::open(dir.path().join("graph.db")).unwrap(),
            ),
            jobs: std::sync::Mutex::new(super::JobStore::open(&state_path).unwrap()),
            investigations: crate::investigations::InvestigationRuntime::default(),
            job_execution_locks: super::job_execution_host_tests::locks(&state_path),
            findings: std::sync::Mutex::new(super::FindingStore::open(&state_path).unwrap()),
            settings: std::sync::Mutex::new(
                super::settings::SettingsStore::open(&state_path).unwrap(),
            ),
            decisions: std::sync::Mutex::new(agents::DecisionLog::open(&state_path).unwrap()),
            proposals: std::sync::Mutex::new(
                agents::ProposalStore::open(state_path.with_file_name("proposals.sqlite")).unwrap(),
            ),
            drafts: std::sync::Mutex::new(
                agents::draft_store::DraftStore::open(
                    state_path.with_file_name("section-drafts.sqlite"),
                )
                .unwrap(),
            ),
            work_packets: super::work_packets::PacketRuntime::open(&state_path).unwrap(),
            extraction_caches: std::sync::Mutex::new(super::ExtractionCaches::default()),
            embedding_cache: None,
            knowledge_overlay: None,
            curated_settled: std::sync::Arc::default(),
            primary_sources: super::primary_source::PrimarySourceStore::open(
                state_path.parent().unwrap(),
            )
            .unwrap(),
            sources: test_source_registry(
                &state_path,
                &std::collections::BTreeSet::from([
                    root_a.display().to_string(),
                    root_b.display().to_string(),
                ]),
            ),
            metrics: std::sync::Mutex::new(
                super::metrics::MetricsStore::open(&state_path).unwrap(),
            ),
        };
        // Only the user copy is gated and enabled (user-scoped).
        let user_hash = core_prov::content_hash(OK_ADAPTER);
        {
            let mut settings = state.settings.lock().unwrap();
            settings
                .record_plugin_gate(
                    "t0.plugin-fixture",
                    &adapters_plugin_host::gate::gate_key(
                        &user_hash,
                        std::fs::read(user_dir.join("t0.plugin-fixture.golden.json"))
                            .ok()
                            .as_deref(),
                    ),
                    true,
                    "{}",
                )
                .unwrap();
            settings
                .set_plugin_enabled("user", "t0.plugin-fixture", &user_hash, true)
                .unwrap();
        }

        // Root B relies on the user copy — and gets it, even though root A
        // owns a project copy of the same id elsewhere in the session.
        let for_b = super::active_plugins_in(&state, &root_b, &user_dir).unwrap();
        assert_eq!(for_b.len(), 1);
        assert_eq!(for_b[0].content_hash, user_hash);
        assert!(for_b[0].path.starts_with(&user_dir));

        // Root A's own project copy shadows the id there, and that copy is
        // neither gated nor enabled: nothing extracts for root A.
        assert!(
            super::active_plugins_in(&state, &root_a, &user_dir)
                .unwrap()
                .is_empty()
        );

        // #1071 review: the adapter lookup still sees root A's installed,
        // ungated copy, as unverified; root B's user copy is verified.
        let declared_a = super::plugin_declarations_in(&state, &root_a, &user_dir).unwrap();
        assert_eq!(declared_a.len(), 1);
        assert_eq!(declared_a[0].id, "t0.plugin-fixture");
        assert!(!declared_a[0].gate_passed);
        let declared_b = super::plugin_declarations_in(&state, &root_b, &user_dir).unwrap();
        assert_eq!(declared_b.len(), 1);
        assert!(declared_b[0].gate_passed);
        assert_eq!(declared_b[0].version, user_hash);
    }
}
