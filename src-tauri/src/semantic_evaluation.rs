//! Fixed host-owned evaluation store. No IPC parameter supplies review authority.
mod source;
use source::{observe, validate_dataset};

use crate::{AppState, primary_source::RetentionGuard};
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use core_graph::{Edge, Node, SnapshotReadLimits, SqliteGraphStore};
use semantic::{
    Candidate, UnresolvedHop,
    embedding_cache::CachedProvider,
    evaluation::{
        fixtures::{canonical_json, sha256},
        policy::{
            CalibrationView, EvaluationInputs, FamilyPolicy, FrozenPolicy, planned_embedding_texts,
        },
        reviewed::{LiveAdmission, ObservedCorpus, ReviewAnchor, ReviewedDataset},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: usize = 128 * 1024;
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusSelector {
    corpus_id: String,
    repo_id: String,
    revision: String,
    capture_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnchorFile {
    schema_version: u32,
    envelope_sha256: String,
    policy_implementer: String,
    corpora: Vec<CorpusSelector>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    schema_version: u32,
    families: Vec<PolicyEntry>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyEntry {
    family: semantic::FamilyKey,
    threshold: f32,
    ambiguity_margin: f32,
    neighbors: usize,
    min_evaluation_cases: u64,
    min_answerable_cases: u64,
    min_no_match_cases: u64,
    precision_floor: f64,
}
impl From<PolicyEntry> for FamilyPolicy {
    fn from(p: PolicyEntry) -> Self {
        Self {
            family: p.family,
            threshold: p.threshold,
            ambiguity_margin: p.ambiguity_margin,
            neighbors: p.neighbors,
            min_evaluation_cases: p.min_evaluation_cases,
            min_answerable_cases: p.min_answerable_cases,
            min_no_match_cases: p.min_no_match_cases,
            precision_floor: p.precision_floor,
        }
    }
}
fn err(error: impl std::fmt::Display) -> String {
    format!("Independent evaluation store: {error}")
}
fn same(a: &cap_std::fs::Metadata, b: &cap_std::fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn read_file(root: &Dir, name: &str) -> Result<Vec<u8>, String> {
    let prior = root.symlink_metadata(name).map_err(err)?;
    if !prior.is_file() || prior.nlink() != 1 || prior.len() > MAX_CONFIG_BYTES as u64 {
        return Err(err("invalid host config file"));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut file = root.open_with(name, &options).map_err(err)?;
    let opened = file.metadata().map_err(err)?;
    if !same(&prior, &opened) || !opened.is_file() || opened.nlink() != 1 {
        return Err(err("host config replaced"));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    let after = root.symlink_metadata(name).map_err(err)?;
    if bytes.len() > MAX_CONFIG_BYTES
        || bytes.len() as u64 != opened.len()
        || !same(&opened, &after)
    {
        return Err(err("host config changed"));
    }
    Ok(bytes)
}
struct Store {
    app_path: PathBuf,
    app: Dir,
    root: Dir,
    dataset: Dir,
    anchor_bytes: Vec<u8>,
    policy_bytes: Vec<u8>,
}
impl Store {
    fn open(app_data: &Path) -> Result<Option<Self>, String> {
        let app_path = dunce::canonicalize(app_data).map_err(err)?;
        let app = Dir::open_ambient_dir(&app_path, cap_std::ambient_authority()).map_err(err)?;
        let root = match app.open_dir_nofollow("semantic-evaluation") {
            Ok(root) => root,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(err(e)),
        };
        let dataset = root.open_dir_nofollow("dataset").map_err(err)?;
        let anchor_bytes = read_file(&root, "review-anchor.json")?;
        let policy_bytes = read_file(&root, "policy.json")?;
        let store = Self {
            app_path,
            app,
            root,
            dataset,
            anchor_bytes,
            policy_bytes,
        };
        store.verify()?;
        Ok(Some(store))
    }
    fn dataset_path(&self) -> PathBuf {
        self.app_path.join("semantic-evaluation/dataset")
    }
    fn verify(&self) -> Result<(), String> {
        let app =
            Dir::open_ambient_dir(&self.app_path, cap_std::ambient_authority()).map_err(err)?;
        let root = self
            .app
            .open_dir_nofollow("semantic-evaluation")
            .map_err(err)?;
        let dataset = self.root.open_dir_nofollow("dataset").map_err(err)?;
        for (expected, current) in [
            (&self.app, &app),
            (&self.root, &root),
            (&self.dataset, &dataset),
        ] {
            if !same(
                &expected.dir_metadata().map_err(err)?,
                &current.dir_metadata().map_err(err)?,
            ) {
                return Err(err("evaluation directory changed"));
            }
        }
        if read_file(&self.root, "review-anchor.json")? != self.anchor_bytes
            || read_file(&self.root, "policy.json")? != self.policy_bytes
        {
            return Err(err("evaluation trust config changed"));
        }
        Ok(())
    }
}

/// Retained source guards survive provider work and final graph-locked recording.
pub(crate) struct PreparedEvaluation {
    store: Store,
    anchor: ReviewAnchor,
    selectors: Vec<CorpusSelector>,
    parameters: Vec<PolicyEntry>,
    observed: Vec<ObservedCorpus>,
    calibration: CalibrationView,
    evaluation: EvaluationInputs,
    _guards: Vec<RetentionGuard>,
}
impl PreparedEvaluation {
    pub(crate) fn planned_texts(&self) -> Result<Vec<String>, String> {
        planned_embedding_texts(&self.calibration, &self.evaluation)
    }
    pub(crate) fn run(
        &self,
        cached: &CachedProvider<'_>,
        hops: &[UnresolvedHop],
        candidates: &[Candidate],
    ) -> Result<LiveAdmission, String> {
        self.store.verify()?;
        let (_, binding) = cached.finish_with_binding().map_err(err)?;
        let binding = binding.ok_or_else(|| err("unpinned embedding identity"))?;
        let policy = FrozenPolicy::freeze(
            &self.calibration,
            &binding,
            self.parameters.iter().cloned().map(Into::into).collect(),
        )?;
        let dataset =
            ReviewedDataset::load(&self.store.dataset_path(), &self.anchor, &self.observed)?;
        let receipt = dataset.predict_and_score(&policy, cached)?;
        let admission = receipt.predict_live(
            &self.store.dataset_path(),
            &self.anchor,
            &self.observed,
            &policy,
            cached,
            hops,
            candidates,
        );
        self.store.verify()?;
        Ok(admission)
    }
    /// Caller already owns the graph mutex. Read a fresh snapshot here rather
    /// than accepting the worker's original copies as evidence of current state.
    pub(crate) fn revalidate(
        &self,
        state: &AppState,
        graph: &SqliteGraphStore,
    ) -> Result<(), String> {
        self.store.verify()?;
        let (nodes, edges) = graph
            .read_snapshot_bounded(SnapshotReadLimits::default())
            .map_err(err)?;
        let nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| {
                flowtracer::FLOW_NODE_LABELS.contains(&n.label.as_str()) || n.label == "Resource"
            })
            .collect();
        let edges: Vec<_> = edges
            .into_iter()
            .filter(|e| flowtracer::FLOW_EDGE_LABELS.contains(&e.label.as_str()))
            .collect();
        let observation = observe(state, graph, &nodes, &edges, &self.selectors)?;
        if canonical_json(&observation.observed).map_err(err)?
            != canonical_json(&self.observed).map_err(err)?
        {
            return Err(err("live corpus changed during evaluation"));
        }
        // Reload the envelope, manifests and retained dataset sources too.
        let dataset = ReviewedDataset::load(
            &self.store.dataset_path(),
            &self.anchor,
            &observation.observed,
        )?;
        validate_dataset(&dataset, &observation, &nodes, &edges, &self.selectors)?;
        self.store.verify()
    }
}
pub(crate) fn prepare(
    app_data: &Path,
    state: &AppState,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<Option<PreparedEvaluation>, String> {
    let Some(store) = Store::open(app_data)? else {
        return Ok(None);
    };
    let anchor_file: AnchorFile = serde_json::from_slice(&store.anchor_bytes).map_err(err)?;
    let policy_file: PolicyFile = serde_json::from_slice(&store.policy_bytes).map_err(err)?;
    if anchor_file.schema_version != 1
        || policy_file.schema_version != 1
        || anchor_file.corpora.is_empty()
        || anchor_file.corpora.len() > 32
        || policy_file.families.is_empty()
        || policy_file.families.len() > 256
    {
        return Err(err("host config schema or count bound"));
    }
    let mut families = BTreeSet::new();
    for p in &policy_file.families {
        if !p.threshold.is_finite()
            || !(0.0..=1.0).contains(&p.threshold)
            || !p.ambiguity_margin.is_finite()
            || !(0.0..=1.0).contains(&p.ambiguity_margin)
            || !p.precision_floor.is_finite()
            || !(0.0..=1.0).contains(&p.precision_floor)
            || !(2..=256).contains(&p.neighbors)
            || [
                p.min_evaluation_cases,
                p.min_answerable_cases,
                p.min_no_match_cases,
            ]
            .iter()
            .any(|n| !(1..=1_000_000).contains(n))
            || p.family.subtype().is_some_and(|s| s.len() > 128)
            || !families.insert(p.family.clone())
        {
            return Err(err("invalid or duplicate family policy"));
        }
    }
    let mut ids = BTreeSet::new();
    let mut repos = BTreeSet::new();
    for selector in &anchor_file.corpora {
        if [
            &selector.corpus_id,
            &selector.repo_id,
            &selector.revision,
            &selector.capture_id,
        ]
        .iter()
        .any(|s| s.is_empty() || s.len() > 512 || s.chars().any(char::is_control))
            || !ids.insert(&selector.corpus_id)
            || !repos.insert(&selector.repo_id)
        {
            return Err(err("invalid corpus selectors"));
        }
    }
    let anchor = ReviewAnchor::new(anchor_file.envelope_sha256, anchor_file.policy_implementer)?;
    let observation = {
        let graph = state.graph.lock().map_err(err)?;
        observe(state, &graph, nodes, edges, &anchor_file.corpora)?
    };
    let dataset = ReviewedDataset::load(&store.dataset_path(), &anchor, &observation.observed)?;
    validate_dataset(&dataset, &observation, nodes, edges, &anchor_file.corpora)?;
    let calibration = dataset.calibration();
    let evaluation = dataset.evaluation_inputs();
    store.verify()?;
    Ok(Some(PreparedEvaluation {
        store,
        anchor,
        selectors: anchor_file.corpora,
        parameters: policy_file.families,
        observed: observation.observed,
        calibration,
        evaluation,
        _guards: observation.guards,
    }))
}

#[cfg(test)]
mod tests;
