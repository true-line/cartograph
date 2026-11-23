//! Extraction routing for gated plugins (#201): walk a project tree, hand
//! every file the plugin claims (by golden-corpus extension) to
//! `extract-source`, and merge the pinned facts. Routing is deterministic
//! (sorted walk) and fails closed per file (AC-0070): a failed call
//! contributes zero facts, never partial ones.
//!
//! What a failed call does to the rest of the pass depends on its cause
//! (#1810, AC-0658). A failure that is a function of the plugin, the file
//! and the limits alone ([`HostError::refusal`]) refuses that one file: it is
//! recorded with its reason class and the pass continues, so the same tree
//! gives the same facts and the same refusals on every run. Any other
//! failure (the wall-clock deadline, the native stack, instantiation, IO)
//! aborts the whole pass with no facts, because skipping the file would make
//! the result depend on the machine.

use crate::{
    HostError, LoadedPlugin, PluginExtraction, PluginHost, PluginLimits, RefusalReason, SourceId,
};
use std::path::{Path, PathBuf};

/// Directories never routed to plugins — preflight's skip list plus
/// `.cartograph` itself, so a plugin claiming `wasm`/`json` is never handed
/// its own artifact or golden corpus as project source (#208 review).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".venv",
    ".cartograph",
];

/// A routing failure: IO while walking/reading, or the plugin failing
/// closed on one named file.
#[derive(Debug)]
pub enum RouteError {
    /// Reading the tree or a claimed file failed.
    Io(std::io::Error),
    /// The plugin violated the SPI or its bounds on `path`.
    Host {
        /// Repo-relative path of the file the plugin failed on.
        path: String,
        /// The fail-closed host error.
        error: HostError,
    },
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "plugin routing: {error}"),
            Self::Host { path, error } => {
                write!(f, "plugin failed closed on {path}: {error}")
            }
        }
    }
}

impl std::error::Error for RouteError {}

impl From<std::io::Error> for RouteError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Repo-relative paths under `root` whose extension is claimed, in sorted
/// order so routing (and therefore the graph) is deterministic.
pub fn claimed_files(root: &Path, extensions: &[String]) -> std::io::Result<Vec<PathBuf>> {
    let skip = |name: &str| SKIP_DIRS.contains(&name);
    Ok(
        source_walk::files(root, &skip, source_walk::Gitignores::Honor)?
            .into_iter()
            .filter(|file| {
                file.path.extension().is_some_and(|extension| {
                    extensions.contains(&extension.to_string_lossy().into_owned())
                })
            })
            .map(|file| PathBuf::from(file.rel))
            .collect(),
    )
}

/// True when a plugin claiming `extensions` is routed `rel`: its extension
/// is claimed and no directory on its path is one routing never enters, the
/// same files [`claimed_files`] lists. A source capture retains them
/// (AC-1069).
pub fn claims(rel: &str, extensions: &[String]) -> bool {
    let mut parts: Vec<&str> = rel.split('/').collect();
    let name = parts.pop().unwrap_or(rel);
    !parts.iter().any(|part| SKIP_DIRS.contains(part))
        && Path::new(name)
            .extension()
            .is_some_and(|extension| extensions.contains(&extension.to_string_lossy().into_owned()))
}

/// One claimed file the host refused for this plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedFile {
    /// Repo-relative path of the file.
    pub path: String,
    /// Why its extraction was refused.
    pub reason: RefusalReason,
}

/// What one plugin pass over its claimed files produced.
#[derive(Debug, Clone, Default)]
pub struct ClaimedExtraction {
    /// Facts of every file whose extraction succeeded, in walk order.
    pub facts: PluginExtraction,
    /// Claimed files whose extraction succeeded, including files that
    /// produced no facts. Refused files are not counted.
    pub files_read: u64,
    /// Claimed files refused, in walk (sorted path) order. No fact from
    /// these files is in `facts`.
    pub refused: Vec<RefusedFile>,
}

/// Run one gated plugin over every file it claims under `root`, merging
/// the (host-pinned) facts of each call. All-or-nothing per plugin: any
/// failure, refusable or not, returns the error and no facts.
pub fn extract_claimed(
    host: &PluginHost,
    plugin: &LoadedPlugin,
    root: &Path,
    extensions: &[String],
    source_id: &SourceId,
    limits: PluginLimits,
) -> Result<PluginExtraction, RouteError> {
    let files = claimed_paths(root, extensions)?;
    let read = |rel: &str| std::fs::read(root.join(rel));
    route(host, plugin, &files, &read, source_id, limits, false).map(|claimed| claimed.facts)
}

fn claimed_paths(root: &Path, extensions: &[String]) -> std::io::Result<Vec<String>> {
    Ok(claimed_files(root, extensions)?
        .into_iter()
        .map(|rel| rel.to_string_lossy().into_owned())
        .collect())
}

/// Run one gated plugin over every file it claims under `root`, refusing
/// single files (#1810, AC-0658). A file whose call fails with a
/// [`HostError::refusal`] class is listed in
/// [`ClaimedExtraction::refused`], contributes no fact, and the pass
/// continues. Any other failure returns the error and no facts.
pub fn extract_claimed_counted(
    host: &PluginHost,
    plugin: &LoadedPlugin,
    root: &Path,
    extensions: &[String],
    source_id: &SourceId,
    limits: PluginLimits,
) -> Result<ClaimedExtraction, RouteError> {
    let files = claimed_paths(root, extensions)?;
    let read = |rel: &str| std::fs::read(root.join(rel));
    route(host, plugin, &files, &read, source_id, limits, true)
}

/// [`extract_claimed_counted`] over a source capture (AC-1069, #385):
/// `captured` is the capture's own paths and `read` their bytes. The plugin
/// is routed only the captured files it [`claims`], so a claimed file
/// edited, deleted or created in the checkout after capture changes
/// nothing.
pub fn extract_captured_counted(
    host: &PluginHost,
    plugin: &LoadedPlugin,
    captured: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    extensions: &[String],
    source_id: &SourceId,
    limits: PluginLimits,
) -> Result<ClaimedExtraction, RouteError> {
    let mut files: Vec<String> = captured
        .iter()
        .filter(|rel| claims(rel, extensions))
        .cloned()
        .collect();
    files.sort();
    files.dedup();
    route(host, plugin, &files, read, source_id, limits, true)
}

fn route(
    host: &PluginHost,
    plugin: &LoadedPlugin,
    files: &[String],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
    source_id: &SourceId,
    limits: PluginLimits,
    refuse_files: bool,
) -> Result<ClaimedExtraction, RouteError> {
    let mut claimed = ClaimedExtraction::default();
    for rel in files {
        let source = read(rel)?;
        let rel = rel.clone();
        match host.call_extract(plugin, &source, &rel, source_id, limits) {
            Ok(extraction) => {
                claimed.facts.nodes.extend(extraction.nodes);
                claimed.facts.edges.extend(extraction.edges);
                claimed.files_read += 1;
            }
            Err(error) => match error.refusal().filter(|_| refuse_files) {
                // The failed call's facts never left the host, so nothing
                // of this file is in `claimed.facts`.
                Some(reason) => claimed.refused.push(RefusedFile { path: rel, reason }),
                None => return Err(RouteError::Host { path: rel, error }),
            },
        }
    }
    Ok(claimed)
}
