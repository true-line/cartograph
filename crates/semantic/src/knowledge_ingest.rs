//! Knowledge-layer embedding phase (ADR-0051 decision 3, #1441).
//!
//! Runs after the deterministic base and never mutates it. Units are base
//! nodes and the traced flows the knowledge producers derived from that base,
//! chosen and rendered by a versioned recipe, redacted before any text leaves for
//! the provider, and embedded through [`CachedProvider`] so vectors persist
//! under ADR-0037's identity, bounds and eviction. Vectors are not returned:
//! retrieval reloads them from the cache namespace, which stays a rebuildable
//! cache, never authority. Without an embedding provider the phase is skipped
//! as a named omission.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use crate::embedding_cache::{
    CacheStats, CachedProvider, EmbeddingCache, EmbeddingSpaceBinding, Lease,
};
use core_graph::Node;
use llm::{LlmProvider, ProviderError};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// Versioned unit selection and rendering recipe.
pub const UNIT_RECIPE: &str = "knowledge-units@4";
/// Units admitted per run. At the cache's widest dimension (4096) this many
/// vectors still fit one operation's 256 MiB vector bound.
pub const MAX_UNITS: usize = 16_384;
/// Rendered bytes per unit after redaction; `MAX_UNITS` of them fit the
/// operation's 32 MiB input bound.
pub const MAX_UNIT_BYTES: usize = 2048;
/// Units handed to the cached provider per call; returned vectors are dropped.
const CHUNK: usize = 1024;
/// Omission when no local embedding provider is configured.
pub const NO_PROVIDER: &str = "embedding-provider-unavailable";
/// Omission for units past [`MAX_UNITS`].
pub const UNIT_CAP: &str = "embedding-unit-cap";

/// Traced flows admitted per run: an eighth of [`MAX_UNITS`], so flows
/// never take more than that share from the modules and symbols after them.
pub const MAX_FLOW_UNITS: usize = MAX_UNITS / 8;
/// Label of a traced flow the knowledge producers derived (#1443).
const FLOW_LABEL: &str = "BusinessFlow";
/// Label of a documentation section (ADR-0065).
pub const SECTION_LABEL: &str = "DocSection";
/// Documentation sections admitted per run, in id order: an eighth of
/// [`MAX_UNITS`], so documents never crowd out the modules and symbols
/// after them (AC-1505).
pub const MAX_SECTION_UNITS: usize = MAX_UNITS / 8;

/// Label of a commit read from an ingested repository's history (AC-1516).
/// Commits are not base facts: like flows, only the commits the host hands
/// to the phase are units, and a base node of this label is not one.
pub const COMMIT_LABEL: &str = "Commit";
/// Commits admitted per run: an eighth of [`MAX_UNITS`], so history never
/// crowds out the modules and symbols after it.
pub const MAX_COMMIT_UNITS: usize = MAX_UNITS / 8;
/// Touched paths a commit unit names; the rest are left out.
pub const MAX_COMMIT_PATHS: usize = 64;
/// Omission naming issue text: no issue tracker is read, so issue text is
/// never a unit (ADR-0051 decision 3 lists it; v1 reads no tracker).
pub const NO_ISSUE_SOURCE: &str = "issue-source-unavailable";

/// One commit of an ingested repository's first-parent history, as the host
/// read it: its message and the repository paths it touched. Author names
/// and emails are never read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitText {
    pub repo: String,
    pub sha: String,
    pub message: String,
    /// Touched paths, sorted, at most [`MAX_COMMIT_PATHS`].
    pub paths: Vec<String>,
}

impl CommitText {
    /// The unit id, `commit:{repo}@{sha}`, which never names a base node.
    pub fn id(&self) -> String {
        format!("commit:{}@{}", self.repo, self.sha)
    }

    /// The base `File` ids of the touched paths, as a hit on the unit cites
    /// them when the base holds them.
    pub fn files(&self) -> impl Iterator<Item = String> + '_ {
        self.paths
            .iter()
            .map(|path| format!("file:{}@{path}", self.repo))
    }

    fn node(&self) -> Node {
        Node {
            id: self.id(),
            label: COMMIT_LABEL.into(),
            props: serde_json::json!({
                "sha": self.sha,
                "message": self.message.split_whitespace().collect::<Vec<_>>().join(" "),
                "paths": self.paths.iter().take(MAX_COMMIT_PATHS).collect::<Vec<_>>(),
            }),
        }
    }
}

/// Section text the host read for the phase, by `DocSection` id. ADR-0065
/// keeps document text out of the graph, so the host reads each admitted
/// section's span from its source and hands it in; a section without an
/// entry is embedded by its heading alone.
pub type SectionText = BTreeMap<String, String>;

/// Labels embedded, in admission priority order. Within a label units are
/// ordered by node id, so a cap keeps the same units on every run. Every
/// label is a base label except [`FLOW_LABEL`], which only the derived flows
/// handed to the phase can fill: a base node of that label is not a unit.
const LABELS: [&str; 13] = [
    "Endpoint",
    "Screen",
    "DataEntity",
    "Service",
    "Channel",
    "Resource",
    "ADR",
    "Rule",
    FLOW_LABEL,
    SECTION_LABEL,
    COMMIT_LABEL,
    "Module",
    "Symbol",
];

/// One base node or derived flow rendered for embedding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeUnit {
    pub node_id: String,
    /// Redacted, bounded text; this exact text is what reaches the provider.
    pub text: String,
}
impl KnowledgeUnit {
    /// Cache key of the unit's text (SHA-256 hex, as ADR-0037 rows use).
    pub fn key(&self) -> String {
        Sha256::digest(self.text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// Deterministic units for a base snapshot and the count past the cap.
pub fn units(nodes: &[Node]) -> (Vec<KnowledgeUnit>, usize) {
    units_with_flows(nodes, &[])
}

/// The `DocSection` nodes the phase embeds, in order: the host reads the
/// text of exactly these before the phase runs. They are the sections that
/// survive every cap of the final unit list, so no read is wasted on a
/// section the global cap drops.
pub fn admitted_sections<'a>(nodes: &'a [Node], flows: &[&'a Node]) -> Vec<&'a Node> {
    // Commits rank after every section, so they never change which
    // sections survive the caps.
    kept(nodes, flows, &[])
        .0
        .into_iter()
        .filter(|node| node.label == SECTION_LABEL)
        .collect()
}

/// Deterministic units for a base snapshot and the flows derived from it,
/// and the count past the caps. `flows` are the producers' `BusinessFlow`
/// nodes; anything else in it, and a flow with a base node's id, is ignored.
/// Flows past [`MAX_FLOW_UNITS`] and
/// units past [`MAX_UNITS`] are counted, never embedded.
pub fn units_with_flows(nodes: &[Node], flows: &[&Node]) -> (Vec<KnowledgeUnit>, usize) {
    units_with_sources(nodes, flows, &SectionText::new())
}

/// [`units_with_flows`] with the section text the host read: an admitted
/// `DocSection` renders its heading and that text (AC-1505). Sections past
/// [`MAX_SECTION_UNITS`] are counted, never embedded.
pub fn units_with_sources(
    nodes: &[Node],
    flows: &[&Node],
    section_text: &SectionText,
) -> (Vec<KnowledgeUnit>, usize) {
    units_with_history(nodes, flows, section_text, &[])
}

/// [`units_with_sources`] with the commits the host read from each
/// ingested repository's history (AC-1516): each commit that touched a
/// file the base holds is a unit after the sections and before the
/// modules, by id, so its hit can cite those files. Commits past
/// [`MAX_COMMIT_UNITS`] are counted, never embedded.
pub fn units_with_history(
    nodes: &[Node],
    flows: &[&Node],
    section_text: &SectionText,
    commits: &[CommitText],
) -> (Vec<KnowledgeUnit>, usize) {
    let commits: Vec<Node> = cited_commits(nodes, commits)
        .map(CommitText::node)
        .collect();
    let commits: Vec<&Node> = commits.iter().collect();
    let (kept, omitted) = kept(nodes, flows, &commits);
    let units = kept
        .into_iter()
        .map(|node| KnowledgeUnit {
            node_id: node.id.clone(),
            text: render(node, section_text.get(&node.id).map(String::as_str)),
        })
        .collect();
    (units, omitted)
}

/// The commits that touched a file the base holds.
pub fn cited_commits<'a>(
    nodes: &[Node],
    commits: &'a [CommitText],
) -> impl Iterator<Item = &'a CommitText> {
    let files: BTreeSet<&str> = if commits.is_empty() {
        BTreeSet::new()
    } else {
        nodes
            .iter()
            .filter(|node| node.label == "File")
            .map(|node| node.id.as_str())
            .collect()
    };
    commits
        .iter()
        .filter(move |commit| commit.files().any(|file| files.contains(file.as_str())))
}

/// The nodes that become units, in unit order, and the count past the caps.
fn kept<'a>(nodes: &'a [Node], flows: &[&'a Node], commits: &[&'a Node]) -> (Vec<&'a Node>, usize) {
    let rank = |node: &Node| LABELS.iter().position(|label| *label == node.label);
    let base = nodes
        .iter()
        .filter(|node| node.label != FLOW_LABEL && node.label != COMMIT_LABEL);
    // A flow or commit cannot take the id of a base node: the base node is
    // the unit.
    let base_ids: BTreeSet<&str> = if flows.is_empty() && commits.is_empty() {
        BTreeSet::new()
    } else {
        nodes.iter().map(|node| node.id.as_str()).collect()
    };
    let flows = flows
        .iter()
        .copied()
        .filter(|node| node.label == FLOW_LABEL && !base_ids.contains(node.id.as_str()));
    let commits = commits
        .iter()
        .copied()
        .filter(|node| node.label == COMMIT_LABEL && !base_ids.contains(node.id.as_str()));
    let mut ranked: Vec<_> = base
        .chain(flows)
        .chain(commits)
        .filter_map(|node| rank(node).map(|rank| (rank, node)))
        .collect();
    ranked.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.id.cmp(&y.id)));
    ranked.dedup_by(|(_, x), (_, y)| x.id == y.id);
    let (mut flows_kept, mut sections_kept, mut commits_kept) = (0, 0, 0);
    let before = ranked.len();
    ranked.retain(|(_, node)| {
        if node.label == SECTION_LABEL {
            sections_kept += 1;
            return sections_kept <= MAX_SECTION_UNITS;
        }
        if node.label == COMMIT_LABEL {
            commits_kept += 1;
            return commits_kept <= MAX_COMMIT_UNITS;
        }
        if node.label != FLOW_LABEL {
            return true;
        }
        flows_kept += 1;
        flows_kept <= MAX_FLOW_UNITS
    });
    let omitted = before - ranked.len() + ranked.len().saturating_sub(MAX_UNITS);
    let kept = ranked
        .into_iter()
        .take(MAX_UNITS)
        .map(|(_, node)| node)
        .collect();
    (kept, omitted)
}

/// Redaction passes allowed before a unit falls back to its bare label.
const REDACTION_PASSES: usize = 8;

/// Label plus scalar properties in key order; nested values, including
/// `prov`, are left out. A traced flow adds its trigger, its status and one
/// line per hop in traversal order, by the names the tracer recorded.
/// Redaction runs before truncation so a cut cannot
/// split a secret past its pattern, and is repeated to a fixed point: the
/// cached provider redacts again, so only a fixed point is the exact text
/// it embeds and hashes under [`KnowledgeUnit::key`].
fn render(node: &Node, section_text: Option<&str>) -> String {
    let mut text = node.label.clone();
    let mut line = |key: &str, value: &str| {
        text.push('\n');
        text.push_str(key);
        text.push_str(": ");
        text.push_str(value);
    };
    if let Some(props) = node.props.as_object() {
        for (key, value) in props {
            // A `prov` the base holds as its text is the nested object it
            // stands for, left out as that object is (#2215).
            if key == "prov" && core_graph::stored::compact_text(value).is_some() {
                continue;
            }
            let value = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            line(key, &value);
        }
    }
    if node.label == FLOW_LABEL {
        let trace = &node.props["trace"];
        let field = |value: &serde_json::Value, key: &str| value[key].as_str().map(str::to_owned);
        if let (Some(kind), Some(name)) =
            (field(trace, "trigger_kind"), field(trace, "trigger_name"))
        {
            line("trigger", &format!("{kind} {name}"));
        }
        if let Some(status) = field(trace, "status") {
            line("status", &status);
        }
        for hop in trace["hops"].as_array().into_iter().flatten() {
            if let (Some(src), Some(label), Some(dst)) = (
                field(hop, "src_name"),
                field(hop, "label"),
                field(hop, "dst_name"),
            ) {
                line("hop", &format!("{src} {label} {dst}"));
            }
        }
    }
    if node.label == COMMIT_LABEL {
        for path in node.props["paths"].as_array().into_iter().flatten() {
            if let Some(path) = path.as_str() {
                line("path", path);
            }
        }
    }
    if node.label == SECTION_LABEL
        && let Some(body) = section_text
    {
        line(
            "text",
            &body.split_whitespace().collect::<Vec<_>>().join(" "),
        );
    }
    for _ in 0..REDACTION_PASSES {
        let (mut next, _) = core_redact::redact_text(&text);
        if next.len() > MAX_UNIT_BYTES {
            let mut end = MAX_UNIT_BYTES;
            while !next.is_char_boundary(end) {
                end -= 1;
            }
            next.truncate(end);
        }
        if next == text {
            return text;
        }
        text = next;
    }
    // No fixed point within the bound: embed nothing a pattern could match.
    node.label.clone()
}

/// Result of the phase. Neither variant touches the base graph.
#[derive(Debug)]
pub enum PhaseOutcome {
    Skipped {
        omission: &'static str,
    },
    Embedded {
        /// Exact cache namespace and dimension; `None` when the provider's
        /// identity is unpinned, so nothing was persisted for reuse.
        binding: Option<EmbeddingSpaceBinding>,
        /// Embedded units with their cache keys, in recipe order.
        units: Vec<(String, String)>,
        /// Units past [`MAX_UNITS`] and flows past [`MAX_FLOW_UNITS`],
        /// reported under [`UNIT_CAP`].
        omitted: usize,
        stats: CacheStats,
    },
}

/// Embed the units of a base snapshot and of the flows derived from it
/// through the bounded vector cache.
pub fn run(
    nodes: &[Node],
    flows: &[&Node],
    section_text: &SectionText,
    commits: &[CommitText],
    provider: Option<&dyn LlmProvider>,
    cache: Arc<Mutex<EmbeddingCache>>,
    lease: Lease,
) -> Result<PhaseOutcome, ProviderError> {
    let Some(provider) = provider.filter(|p| p.capabilities().embeddings) else {
        return Ok(PhaseOutcome::Skipped {
            omission: NO_PROVIDER,
        });
    };
    let (units, omitted) = units_with_history(nodes, flows, section_text, commits);
    let cached = CachedProvider::new(provider, cache, lease)?;
    for chunk in units.chunks(CHUNK) {
        let texts: Vec<String> = chunk.iter().map(|unit| unit.text.clone()).collect();
        cached.embed(&texts)?;
    }
    cached.seal()?;
    let (stats, binding) = cached.finish_with_binding()?;
    Ok(PhaseOutcome::Embedded {
        binding,
        units: units
            .iter()
            .map(|unit| (unit.node_id.clone(), unit.key()))
            .collect(),
        omitted,
        stats,
    })
}

#[cfg(test)]
mod tests;
