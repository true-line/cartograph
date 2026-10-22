//! Closed stored-fact partitions. Only the pinned storage reader can construct
//! the private closure; caller-supplied graph vectors are not closure proofs.
use super::*;
use core_graph::scoped::{EdgeKey, Remaining, ScopedGraphReader, ScopedReadUsage};
use std::fmt::Write;

/// Finite producer-owned partition catalog; flows require a separate seed API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionArtifactKind {
    /// Whole connected DataEntity component, with incident context endpoints.
    DataModel,
    /// One explicitly selected source-rule observation and rendering context.
    RuleEvidence,
    /// Index of the same single-observation closure.
    RuleEvidenceIndex,
}
impl PartitionArtifactKind {
    /// Official producer selection key, with a distinct partition schema.
    pub fn id(self) -> &'static str {
        match self {
            Self::DataModel => "data-model",
            Self::RuleEvidence => "rule-evidence",
            Self::RuleEvidenceIndex => "rule-evidence-index",
        }
    }
    /// Required visible entry-root label.
    pub fn root_label(self) -> &'static str {
        match self {
            Self::DataModel => "DataEntity",
            _ => "BusinessRule",
        }
    }
}
/// Exact producer labels, also used by the native complete renderer.
pub const DATA_PARTITION_EDGE_LABELS: &[&str] = producers::DATA_PARTITION_EDGE_LABELS;
/// Exhaustion proves only this revision's stored-fact dependency closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PartitionArtifact {
    /// Domain-separated artifact schema, never cartograph.spec/<id>@1.
    pub schema: String,
    /// Native producer key.
    pub artifact_kind: PartitionArtifactKind,
    /// Domain-separated content identity binding qualified closure and output.
    pub content_id: String,
    /// Domain-separated qualified closure identity.
    pub partition_basis_id: String,
    /// Sorted visible component entities or explicit observation roots.
    pub roots: Vec<String>,
    /// False even when the producer-specific dependency closure is exhausted.
    pub complete_system: bool,
    /// Partial-scope rendered content.
    pub content: String,
    /// Complete native assertions for this output.
    pub assertions: Vec<SpecAssertion>,
    /// Charged acquisition work; excluded from root-independent artifact hash.
    pub acquisition: PartitionAcquisition,
}
/// Charged work, independent of component artifact identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PartitionAcquisition {
    /// Raw visited records, including duplicates and missing probes.
    pub visits: usize,
    /// Cumulative inspected field bytes.
    pub raw_bytes: usize,
    /// Cumulative canonical acquisition bytes.
    pub canonical_bytes: usize,
    /// Adjacency existence probes reserved by the storage reader.
    pub adjacency_probes: usize,
}
impl From<ScopedReadUsage> for PartitionAcquisition {
    fn from(v: ScopedReadUsage) -> Self {
        Self {
            visits: v.visits,
            raw_bytes: v.raw_bytes,
            canonical_bytes: v.canonical_bytes,
            adjacency_probes: v.adjacency_probes,
        }
    }
}
/// Closed acquisition or output refusal; no prefix artifact is returned.
#[derive(Debug)]
pub enum PartitionError {
    /// Storage admission, corruption or stale-revision failure.
    Graph(core_graph::GraphError),
    /// Missing, hidden, malformed, wrong-label or dangling required context.
    Unavailable,
    /// Complete rendering cannot fit the supplied existing output budget.
    Limit,
    /// Invalid scoped session identity or entry selection.
    InvalidRequest,
}
impl From<core_graph::GraphError> for PartitionError {
    fn from(error: core_graph::GraphError) -> Self {
        Self::Graph(error)
    }
}
impl std::fmt::Display for PartitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Graph(e) => write!(f, "{e}"),
            Self::Unavailable => f.write_str("partition unavailable"),
            Self::Limit => f.write_str("partition output limited"),
            Self::InvalidRequest => f.write_str("invalid partition request"),
        }
    }
}
impl std::error::Error for PartitionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Graph(e) => Some(e),
            _ => None,
        }
    }
}
struct ClosedPartition {
    nodes: BTreeMap<String, Node>,
    edges: BTreeMap<(String, String, String), Edge>,
}
impl ClosedPartition {
    fn node(&mut self, reader: &mut ScopedGraphReader<'_>, id: &str) -> Result<(), PartitionError> {
        if !self.nodes.contains_key(id) {
            let node = reader.node(id)?.ok_or(PartitionError::Unavailable)?;
            self.nodes.insert(id.into(), node);
        }
        Ok(())
    }
    fn edge(
        &mut self,
        reader: &mut ScopedGraphReader<'_>,
        edge: Edge,
    ) -> Result<(), PartitionError> {
        self.node(reader, &edge.src)?;
        self.node(reader, &edge.dst)?;
        self.edges.insert(
            (edge.src.clone(), edge.dst.clone(), edge.label.clone()),
            edge,
        );
        Ok(())
    }
}
fn identity(domain: &str, value: &impl Serialize) -> String {
    struct Hash<'a>(&'a mut blake3::Hasher);
    impl std::io::Write for Hash<'_> {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.update(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hash = blake3::Hasher::new();
    hash.update(domain.as_bytes());
    hash.update(&[0]);
    serde_json::to_writer(Hash(&mut hash), value).expect("partition typed serialization");
    format!("{domain}:{}", hash.finalize().to_hex())
}
fn acquire(
    reader: &mut ScopedGraphReader<'_>,
    kind: PartitionArtifactKind,
    root: &str,
) -> Result<ClosedPartition, PartitionError> {
    let mut closed = ClosedPartition {
        nodes: BTreeMap::new(),
        edges: BTreeMap::new(),
    };
    closed.node(reader, root)?;
    if closed.nodes[root].label != kind.root_label() {
        return Err(PartitionError::Unavailable);
    }
    let mut frontier = BTreeSet::from([root.to_owned()]);
    let mut done = BTreeSet::new();
    while let Some(id) = frontier.pop_first() {
        if !done.insert(id.clone()) {
            continue;
        }
        let mut out: Option<EdgeKey> = None;
        loop {
            let page = reader.outgoing(&id, out.as_ref(), reader.limits().max_rows)?;
            for edge in page.edges {
                let relevant = if kind == PartitionArtifactKind::DataModel {
                    DATA_PARTITION_EDGE_LABELS.contains(&edge.label.as_str())
                } else {
                    crate::rules::PARTITION_EDGE_LABELS.contains(&edge.label.as_str())
                };
                if relevant {
                    closed.edge(reader, edge)?;
                }
            }
            if page.remaining == Remaining::Exhausted {
                break;
            }
            out = page.next_outgoing;
            if out.is_none() {
                return Err(PartitionError::Unavailable);
            }
        }
        if kind == PartitionArtifactKind::DataModel {
            let mut incoming = None;
            loop {
                let page = reader.incoming(&id, incoming, reader.limits().max_rows)?;
                for edge in page.edges {
                    if DATA_PARTITION_EDGE_LABELS.contains(&edge.label.as_str()) {
                        closed.edge(reader, edge)?;
                    }
                }
                if page.remaining == Remaining::Exhausted {
                    break;
                }
                incoming = page.next_incoming;
                if incoming.is_none() {
                    return Err(PartitionError::Unavailable);
                }
            }
            for node in closed.nodes.values() {
                if node.label == "DataEntity" && !done.contains(&node.id) {
                    frontier.insert(node.id.clone());
                }
            }
        } else {
            // Context observations never become inventory roots. Their payloads
            // are not rendered, so only the selected observation is dereferenced.
            let ids = crate::rules::partition_references(&closed.nodes[&id]);
            for dependency in ids {
                closed.node(reader, &dependency)?;
            }
        }
    }
    Ok(closed)
}
/// Compile one producer-specific closed partition inside the caller's pinned
/// reader transaction. The requested entry root is deliberately absent from
/// component identity/header: callers put it only in their response envelope.
/// `session_basis_id` must be the host-qualified scoped session identity.
pub fn compile_partition_selected(
    reader: &mut ScopedGraphReader<'_>,
    kind: PartitionArtifactKind,
    requested_root: &str,
    session_basis_id: &str,
    mode: ExportMode,
    rejected: &BTreeSet<String>,
    max_bytes: usize,
) -> Result<PartitionArtifact, PartitionError> {
    if max_bytes == 0 {
        return Err(PartitionError::Limit);
    }
    if !session_basis_id
        .strip_prefix("mcp-scoped-session-v1:")
        .is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err(PartitionError::InvalidRequest);
    }
    let closed = acquire(reader, kind, requested_root)?;
    let nodes: Vec<_> = closed.nodes.into_values().collect();
    let edges: Vec<_> = closed.edges.into_values().collect();
    let (visible_nodes, visible_edges) =
        selected::project_recovered_facts(&nodes, &edges, mode, rejected);
    if !visible_nodes.iter().any(|n| n.id == requested_root) {
        return Err(PartitionError::Unavailable);
    }
    let roots: Vec<String> = if kind == PartitionArtifactKind::DataModel {
        visible_nodes
            .iter()
            .filter(|n| n.label == "DataEntity")
            .map(|n| n.id.clone())
            .collect()
    } else {
        vec![requested_root.into()]
    };
    let partition_basis_id = identity(
        "spec-partition-basis-v1",
        &(
            kind,
            &roots,
            reader.revision(),
            session_basis_id,
            mode,
            &nodes,
            &edges,
        ),
    );
    let mut header = String::new();
    // Count and bound header formatting with the same producer budget before
    // allocating rendered output. Escaped root identifiers cannot add headings.
    let budget = render::Budget::new(Some(max_bytes)).map_err(|_| PartitionError::Limit)?;
    let mut text=render::Text::new(&budget,"# Partial stored-fact partition\n\nComplete-system coverage and behavioral completeness are not established.\n\nComponent/observation roots:\n").map_err(|_|PartitionError::Limit)?;
    for root in &roots {
        text.push_str("- ").map_err(|_| PartitionError::Limit)?;
        for c in root.chars() {
            if c.is_control() {
                write!(text, "\\u{{{:x}}}", c as u32).map_err(|_| PartitionError::Limit)?;
            } else {
                if c.is_ascii_punctuation() {
                    text.push_str("\\").map_err(|_| PartitionError::Limit)?;
                }
                write!(text, "{c}").map_err(|_| PartitionError::Limit)?;
            }
        }
        text.push_str("\n").map_err(|_| PartitionError::Limit)?;
    }
    text.push_str("\n").map_err(|_| PartitionError::Limit)?;
    header.push_str(&text.finish());
    let remaining = max_bytes
        .checked_sub(header.len())
        .ok_or(PartitionError::Limit)?;
    let (body, assertions) = match kind {
        PartitionArtifactKind::DataModel => {
            let output = producers::data(&visible_nodes, &visible_edges, Some(remaining))
                .map_err(|_| PartitionError::Limit)?;
            (output.content, output.assertions)
        }
        _ => {
            let selection = crate::rules::InventorySelection {
                markdown: kind == PartitionArtifactKind::RuleEvidence,
                index: kind == PartitionArtifactKind::RuleEvidenceIndex,
            };
            let output = crate::rules::inventory_selected_roots(
                &visible_nodes,
                &visible_edges,
                mode,
                selection,
                Some(remaining),
                Some(&roots.iter().cloned().collect()),
            )
            .map_err(|_| PartitionError::Limit)?;
            (
                if selection.markdown {
                    output.markdown.unwrap()
                } else {
                    output.index.unwrap()
                },
                output.assertions,
            )
        }
    };
    let content = if kind == PartitionArtifactKind::RuleEvidenceIndex {
        // JSON index remains valid JSON; the partition qualifier is outside the
        // unchanged native inventory object rather than a Markdown prefix.
        #[derive(Serialize)]
        struct Qualifier<'a> {
            complete_system: bool,
            roots: &'a [String],
        }
        let budget = render::Budget::new(Some(max_bytes)).map_err(|_| PartitionError::Limit)?;
        let mut output =
            render::Text::new(&budget, "{\"partition\":").map_err(|_| PartitionError::Limit)?;
        serde_json::to_writer(
            &mut output,
            &Qualifier {
                complete_system: false,
                roots: &roots,
            },
        )
        .map_err(|_| PartitionError::Limit)?;
        output
            .push_str(",\"inventory\":")
            .map_err(|_| PartitionError::Limit)?;
        output.push_str(&body).map_err(|_| PartitionError::Limit)?;
        output.push_str("}").map_err(|_| PartitionError::Limit)?;
        output.finish()
    } else {
        header.push_str(&body);
        header
    };
    let schema = format!("cartograph.spec-partition/{}@1", kind.id());
    let content_id = identity(
        "spec-partition-artifact-v1",
        &(&schema, kind, &partition_basis_id, &content, &assertions),
    );
    let artifact = PartitionArtifact {
        schema,
        artifact_kind: kind,
        content_id,
        partition_basis_id,
        roots,
        complete_system: false,
        content,
        assertions,
        acquisition: reader.usage().into(),
    };
    if !serialized_fits(&artifact, max_bytes) {
        return Err(PartitionError::Limit);
    }
    Ok(artifact)
}

fn serialized_fits(value: &impl Serialize, maximum: usize) -> bool {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(b.len())
                .ok_or_else(|| std::io::Error::other("partition limit"))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(maximum), value).is_ok()
}

mod accounting;
pub use accounting::{PartitionWorkspaceBound, logical_partition_bound};

#[cfg(test)]
mod tests;
