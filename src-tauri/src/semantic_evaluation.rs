//! Fixed host-owned evaluation store. No IPC parameter supplies review authority.
//!
//! Two layouts under `<app data>/semantic-evaluation/`. With
//! `locked-receipt.json` present the store is locked (ADR-0082 decision 7):
//! `runner-key.txt` (the pinned runner key), `locked-receipt.json` (the
//! runner's signed output), `policy.json` and `corpora.json`, and no dataset
//! or labels on this host. Otherwise it is the in-process reviewed layout:
//! `review-anchor.json`, `policy.json` and `dataset/` (ADR-0038).
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
        locked_receipt::{MAX_SIGNED_BYTES, VerifiedReceipt},
        policy::{
            CalibrationView, EvaluationInputs, FamilyPolicy, FrozenPolicy, planned_embedding_texts,
        },
        reviewed::{LiveAdmission, ObservedCorpus, ReviewAnchor, ReviewedDataset},
    },
};
use serde::Deserialize;
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
struct CorporaFile {
    schema_version: u32,
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
/// Largest file the store reads: the signed receipt; config files stay smaller.
fn max_bytes(name: &str) -> usize {
    if name == LOCKED_RECEIPT {
        MAX_SIGNED_BYTES
    } else {
        MAX_CONFIG_BYTES
    }
}
const LOCKED_RECEIPT: &str = "locked-receipt.json";
const RUNNER_KEY: &str = "runner-key.txt";
const LOCKED_FILES: [&str; 4] = [LOCKED_RECEIPT, RUNNER_KEY, "policy.json", "corpora.json"];
const REVIEWED_FILES: [&str; 2] = ["review-anchor.json", "policy.json"];
fn read_file(root: &Dir, name: &str) -> Result<Vec<u8>, String> {
    let max = max_bytes(name);
    let prior = root.symlink_metadata(name).map_err(err)?;
    if !prior.is_file() || prior.nlink() != 1 || prior.len() > max as u64 {
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
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    let after = root.symlink_metadata(name).map_err(err)?;
    if bytes.len() > max || bytes.len() as u64 != opened.len() || !same(&opened, &after) {
        return Err(err("host config changed"));
    }
    Ok(bytes)
}
struct Store {
    app_path: PathBuf,
    app: Dir,
    root: Dir,
    /// The reviewed dataset; a locked store has none on this host.
    dataset: Option<Dir>,
    /// Every trust file, as read once; `verify` requires the same bytes.
    files: BTreeMap<&'static str, Vec<u8>>,
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
        let locked = match root.symlink_metadata(LOCKED_RECEIPT) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(err(e)),
        };
        let (dataset, names) = if locked {
            (None, &LOCKED_FILES[..])
        } else {
            (
                Some(root.open_dir_nofollow("dataset").map_err(err)?),
                &REVIEWED_FILES[..],
            )
        };
        let files = names
            .iter()
            .map(|name| Ok((*name, read_file(&root, name)?)))
            .collect::<Result<_, String>>()?;
        let store = Self {
            app_path,
            app,
            root,
            dataset,
            files,
        };
        store.verify()?;
        Ok(Some(store))
    }
    fn dataset_path(&self) -> PathBuf {
        self.app_path.join("semantic-evaluation/dataset")
    }
    fn bytes(&self, name: &str) -> &[u8] {
        self.files.get(name).map_or(&[], Vec::as_slice)
    }
    fn verify(&self) -> Result<(), String> {
        let app =
            Dir::open_ambient_dir(&self.app_path, cap_std::ambient_authority()).map_err(err)?;
        let root = self
            .app
            .open_dir_nofollow("semantic-evaluation")
            .map_err(err)?;
        let dataset = match &self.dataset {
            Some(_) => Some(self.root.open_dir_nofollow("dataset").map_err(err)?),
            None => None,
        };
        let mut pairs = vec![(&self.app, &app), (&self.root, &root)];
        if let (Some(expected), Some(current)) = (&self.dataset, &dataset) {
            pairs.push((expected, current));
        }
        for (expected, current) in pairs {
            if !same(
                &expected.dir_metadata().map_err(err)?,
                &current.dir_metadata().map_err(err)?,
            ) {
                return Err(err("evaluation directory changed"));
            }
        }
        // A locked store that gains a dataset, or a reviewed store that gains
        // a receipt, is a different store.
        let locked_now = match self.root.symlink_metadata(LOCKED_RECEIPT) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(err(e)),
        };
        if locked_now != self.dataset.is_none() {
            return Err(err("evaluation store layout changed"));
        }
        for (name, bytes) in &self.files {
            if &read_file(&self.root, name)? != bytes {
                return Err(err("evaluation trust config changed"));
            }
        }
        Ok(())
    }
}

/// The evidence one prepared evaluation admits on.
enum Evidence {
    /// The in-process reviewed dataset (ADR-0038).
    Reviewed {
        anchor: ReviewAnchor,
        calibration: CalibrationView,
        evaluation: EvaluationInputs,
    },
    /// A receipt from the locked runner, verified against the pinned key.
    Locked(VerifiedReceipt),
}

/// Retained source guards survive provider work and final graph-locked recording.
pub(crate) struct PreparedEvaluation {
    store: Store,
    evidence: Evidence,
    selectors: Vec<CorpusSelector>,
    parameters: Vec<PolicyEntry>,
    observed: Vec<ObservedCorpus>,
    _guards: Vec<RetentionGuard>,
}
impl PreparedEvaluation {
    /// Texts embedded beside the live plan: the reviewed dataset's cases, or
    /// none for a locked receipt, whose cases never leave the lock.
    pub(crate) fn planned_texts(&self) -> Result<Vec<String>, String> {
        match &self.evidence {
            Evidence::Reviewed {
                calibration,
                evaluation,
                ..
            } => planned_embedding_texts(calibration, evaluation),
            Evidence::Locked(_) => Ok(Vec::new()),
        }
    }
    pub(crate) fn run(
        &self,
        cached: &CachedProvider<'_>,
        hops: &[UnresolvedHop],
        candidates: &[Candidate],
    ) -> Result<LiveAdmission, String> {
        self.store.verify()?;
        let parameters = || self.parameters.iter().cloned().map(Into::into).collect();
        let admission = match &self.evidence {
            Evidence::Locked(receipt) => {
                receipt.admit_live(parameters(), &self.observed, cached, hops, candidates)
            }
            Evidence::Reviewed {
                anchor,
                calibration,
                ..
            } => {
                let (_, binding) = cached.finish_with_binding().map_err(err)?;
                let binding = binding.ok_or_else(|| err("unpinned embedding identity"))?;
                let policy = FrozenPolicy::freeze(calibration, &binding, parameters())?;
                let dataset =
                    ReviewedDataset::load(&self.store.dataset_path(), anchor, &self.observed)?;
                let receipt = dataset.predict_and_score(&policy, cached)?;
                receipt.predict_live(
                    &self.store.dataset_path(),
                    anchor,
                    &self.observed,
                    &policy,
                    cached,
                    hops,
                    candidates,
                )
            }
        };
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
        if let Evidence::Reviewed { anchor, .. } = &self.evidence {
            let dataset =
                ReviewedDataset::load(&self.store.dataset_path(), anchor, &observation.observed)?;
            validate_dataset(&dataset, &observation, &nodes, &edges, &self.selectors)?;
        }
        self.store.verify()
    }
}
fn validate_policy(policy_file: &PolicyFile) -> Result<(), String> {
    if policy_file.schema_version != 1
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
    Ok(())
}
fn validate_selectors(selectors: &[CorpusSelector]) -> Result<(), String> {
    if selectors.is_empty() || selectors.len() > 32 {
        return Err(err("host config schema or count bound"));
    }
    let mut ids = BTreeSet::new();
    let mut repos = BTreeSet::new();
    for selector in selectors {
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
    Ok(())
}
/// The pinned key: 64 lowercase hex, with at most one trailing newline.
fn runner_key(bytes: &[u8]) -> Result<&str, String> {
    let text = std::str::from_utf8(bytes).map_err(err)?;
    let key = text.strip_suffix('\n').unwrap_or(text);
    if key.len() != 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(err("invalid pinned runner key"));
    }
    Ok(key)
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
    let policy_file: PolicyFile =
        serde_json::from_slice(store.bytes("policy.json")).map_err(err)?;
    validate_policy(&policy_file)?;
    if store.dataset.is_none() {
        let corpora: CorporaFile =
            serde_json::from_slice(store.bytes("corpora.json")).map_err(err)?;
        if corpora.schema_version != 1 {
            return Err(err("host config schema or count bound"));
        }
        validate_selectors(&corpora.corpora)?;
        let key = runner_key(store.bytes(RUNNER_KEY))?;
        let receipt = VerifiedReceipt::verify(store.bytes(LOCKED_RECEIPT), key)
            .map_err(|e| err(format!("locked receipt refused: {e:?}")))?;
        let observation = {
            let graph = state.graph.lock().map_err(err)?;
            observe(state, &graph, nodes, edges, &corpora.corpora)?
        };
        store.verify()?;
        return Ok(Some(PreparedEvaluation {
            store,
            evidence: Evidence::Locked(receipt),
            selectors: corpora.corpora,
            parameters: policy_file.families,
            observed: observation.observed,
            _guards: observation.guards,
        }));
    }
    let anchor_file: AnchorFile =
        serde_json::from_slice(store.bytes("review-anchor.json")).map_err(err)?;
    if anchor_file.schema_version != 1 {
        return Err(err("host config schema or count bound"));
    }
    validate_selectors(&anchor_file.corpora)?;
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
        evidence: Evidence::Reviewed {
            anchor,
            calibration,
            evaluation,
        },
        selectors: anchor_file.corpora,
        parameters: policy_file.families,
        observed: observation.observed,
        _guards: observation.guards,
    }))
}

#[cfg(test)]
mod tests;
