//! What an ingest's configuration and a graph's sources are identified by:
//! the active plugin set, its coverage basis hash, and the source revisions a
//! graph cites. Split out of the app's `main.rs` so the investigation and
//! export layers can use them from app-store (#1924).

use adapters_fw::events::Registries;
use core_graph::Node;
use serde::Serialize;

use crate::observation_inputs;

/// The lifecycle key for a plugin's enablement rows: the resolved project
/// root that supplied a project copy, or `"user"` for user-level copies.
pub fn plugin_settings_root(plugin: &adapters_plugin_host::discovery::DiscoveredPlugin) -> String {
    plugin
        .project_root
        .as_ref()
        .map(|root| root.display().to_string())
        .unwrap_or_else(|| "user".to_string())
}

/// A plugin cleared for extraction on one project root (#201): discovered,
/// explicitly enabled for these exact bytes, and gate-passed for these
/// exact bytes. `extensions` is the coverage claim from its golden corpus
/// — the gate already proved the corpus, so the claim is trusted as far as
/// routing; the facts themselves are still bounded and pinned per call.
pub struct ActivePlugin {
    pub plugin_id: String,
    pub path: std::path::PathBuf,
    /// The gated hash — extraction re-verifies the bytes on disk still
    /// match before running them (fail closed on a swap).
    pub content_hash: String,
    pub extensions: Vec<String>,
    /// Coverage cells the gated corpus declares (AC-0263).
    pub cells: Vec<ingest::matrix::ReportedCell>,
    /// The corpus's names for extensions the core cannot name.
    pub languages: std::collections::BTreeMap<String, String>,
    /// The gate key over the artifact and its exact corpus bytes; it enters
    /// the coverage basis, so a corpus edit stales recorded inventories.
    pub gate_key: String,
}

/// One repo currently contributing facts to the unified graph (#162).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SystemRepo {
    pub repo: String,
    pub commit: String,
    pub display_name: Option<String>,
    /// Graph content hash recorded by the latest completed ingest of this
    /// registration at `commit` (#1765, AC-0593); `None` when none is
    /// recorded. Scoped per registration: a `local/` key is random per
    /// installation, so the value is not comparable across installations.
    pub graph_hash: Option<String>,
}

pub fn system_contents_of(nodes: &[Node]) -> Vec<SystemRepo> {
    let mut repos: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for node in nodes {
        let prov = core_graph::stored::stored_prov(&node.props);
        let evidence = &prov["evidence"][0];
        // A captured observation input is not a source registration (ADR-0080).
        let Some(repo) = evidence["repo"].as_str().filter(|repo| {
            !repo.is_empty() && !repo.starts_with(observation_inputs::INPUT_REPO_PREFIX)
        }) else {
            continue;
        };
        let commit = evidence["commit_sha"].as_str().unwrap_or("workdir");
        repos.entry(repo.to_string()).or_insert(commit.to_string());
    }
    repos
        .into_iter()
        .map(|(repo, commit)| SystemRepo {
            repo,
            commit,
            display_name: None,
            graph_hash: None,
        })
        .collect()
}

/// The source registrations `nodes` cite and the revision of each, ordered
/// by registry key: the `sources` of a graph basis (AC-0593).
pub fn source_revisions_of(nodes: &[Node]) -> Vec<spec::SourceRevision> {
    system_contents_of(nodes)
        .into_iter()
        .map(|entry| spec::SourceRevision {
            repo: entry.repo,
            commit: entry.commit,
        })
        .collect()
}

/// Identity of the plugin and registry basis an inventory was recorded
/// under: the only parts of an ingest's configuration re-derivable at read
/// time. Layer hints and declared inputs are per-ingest options the
/// inventory records itself. The registry identity covers every accepted
/// file's bytes — SDK entries, qualifiers, batch keys, and backings alike.
pub fn coverage_config_hash(plugins: &[ActivePlugin], registries: &Registries) -> String {
    let mut pinned: Vec<String> = plugins
        .iter()
        .map(|plugin| {
            // The gate key binds the corpus: edited routing or coverage
            // claims change the basis even when the artifact did not.
            let pinned =
                adapters_plugin_host::pinned_extractor_id(&plugin.plugin_id, &plugin.content_hash);
            format!("{pinned}#{}", plugin.gate_key)
        })
        .collect();
    pinned.sort();
    pinned.dedup();
    let basis = serde_json::json!({
        "basis": "coverage-config-v2",
        "plugins": pinned,
        "registries": registries.identity(),
    });
    core_prov::content_hash(basis.to_string().as_bytes())
}
