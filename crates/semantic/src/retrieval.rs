//! Hybrid graph-first retrieval (ADR-0051 decision 4, #1442).
//!
//! A query first resolves exact anchors, nodes whose id or name equals it,
//! and their one-hop graph neighbourhood. Only then is text ranked: SQLite
//! FTS5 over the knowledge units, fused by reciprocal rank with vector
//! similarity when unit vectors and a provider are present. Every hit cites
//! base facts and carries the T2 tier; nothing is written back to the base.
//! A traced flow the knowledge producers derived is a ranked unit too: its
//! hit names the flow and cites the base facts the flow was traced from.
//! Without a provider the lexical leg alone answers.
//!
//! The vector leg takes its candidates from a usearch index rebuilt from the
//! unit vectors each time they are embedded (ADR-0051 decision 3, #1441).
//! The index is a cache, never authority: every candidate is scored again
//! from its stored vector, and without an index every vector is scored.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use crate::knowledge_ingest::{self, KnowledgeUnit};
use core_graph::overlay::{BaseRef, OverlayFact, OverlayItem};
use core_graph::{Edge, Node};
use core_prov::Tier;
use llm::{EmbeddingIdentity, LlmProvider, ProviderError};
use rusqlite::Connection;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

/// Reciprocal-rank-fusion constant.
pub const FUSION_K: f64 = 60.0;
/// Hits returned per query, and candidates taken from each ranked leg.
pub const MAX_HITS: usize = 200;
/// Query bytes accepted; longer queries are refused, not truncated.
pub const MAX_QUERY_BYTES: usize = 512;
/// Candidates the vector index is asked for per query. A base with at most
/// this many units is scored whole, so the index cannot change its answer.
pub const VECTOR_CANDIDATES: usize = 4 * MAX_HITS;
/// Lexical terms taken from a query.
const MAX_TERMS: usize = 32;
/// Unit texts handed to the provider per call.
const CHUNK: usize = 1024;
/// Node properties that name a node for exact anchoring.
const NAME_KEYS: [&str; 3] = ["name", "path", "endpoint_name"];

#[derive(Debug, thiserror::Error)]
pub enum RetrievalError {
    #[error("retrieval index: {0}")]
    Store(#[from] rusqlite::Error),
    #[error("retrieval provider: {0}")]
    Provider(#[from] ProviderError),
    #[error("retrieval query: {0}")]
    Query(&'static str),
}

/// Why a hit is in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The query names this node exactly.
    Anchor,
    /// One edge away from an anchor.
    Neighbour,
    /// Ranked by text and, when present, vector similarity.
    Ranked,
}

/// One retrieved base node or derived flow. `score` is the fused rank score
/// for ranked hits and zero for anchors and neighbours, which precede every
/// ranked hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub node_id: String,
    pub reason: Reason,
    pub score: f64,
    /// Base facts the hit rests on: the node, plus the anchoring edge for a
    /// neighbour; for a derived flow, the base facts the flow cites.
    pub cites: Vec<BaseRef>,
    pub tier: Tier,
}

/// The embedding model a vector was produced by; query and unit vectors
/// compare only within one space.
type Space = (String, Option<EmbeddingIdentity>);

/// Query index over one base snapshot.
pub struct Retrieval {
    index: Connection,
    units: Vec<KnowledgeUnit>,
    /// Units past the knowledge-unit cap; not ranked.
    pub omitted: usize,
    /// Unit-normalized vectors, parallel to `units`, once embedded, with
    /// the space they were embedded in.
    vectors: Option<(Space, Vec<Vec<f32>>)>,
    /// usearch index over `vectors`, keyed by unit row. Rebuilt whenever the
    /// units are embedded; `None` when it could not be built, and then every
    /// vector is scored.
    vector_index: Option<Index>,
    names: BTreeMap<String, BTreeSet<String>>,
    edges: Vec<Edge>,
    /// Base citations of each derived flow and commit unit, by unit id.
    flow_cites: BTreeMap<String, Vec<BaseRef>>,
}

impl Retrieval {
    /// Index the knowledge units of a base snapshot. The indexed text is the
    /// redacted unit text the embedding phase uses.
    pub fn build(nodes: &[Node], edges: &[Edge]) -> Result<Self, RetrievalError> {
        Self::build_with_flows(nodes, edges, &[])
    }

    /// Index the knowledge units of a base snapshot and of the flows derived
    /// from it, as the embedding phase chooses them. `derived` is the
    /// producers' output for that snapshot; only its `BusinessFlow` nodes
    /// become units. A flow is ranked, never an anchor or a neighbour, and
    /// its hit cites the base facts the flow cites.
    pub fn build_with_flows(
        nodes: &[Node],
        edges: &[Edge],
        derived: &[OverlayFact],
    ) -> Result<Self, RetrievalError> {
        Self::build_with_sources(nodes, edges, derived, &knowledge_ingest::SectionText::new())
    }

    /// [`Self::build_with_flows`] with the section text the host read for
    /// the embedding phase, so a documentation section is indexed by the
    /// same text that phase embedded (AC-1505).
    pub fn build_with_sources(
        nodes: &[Node],
        edges: &[Edge],
        derived: &[OverlayFact],
        section_text: &knowledge_ingest::SectionText,
    ) -> Result<Self, RetrievalError> {
        Self::build_with_history(nodes, edges, derived, section_text, &[])
    }

    /// [`Self::build_with_sources`] with the commits the host read for the
    /// embedding phase (AC-1516). A commit is ranked, never an anchor or a
    /// neighbour, and its hit cites the base `File` nodes it touched.
    pub fn build_with_history(
        nodes: &[Node],
        edges: &[Edge],
        derived: &[OverlayFact],
        section_text: &knowledge_ingest::SectionText,
        commits: &[knowledge_ingest::CommitText],
    ) -> Result<Self, RetrievalError> {
        let flows: Vec<(&Node, &OverlayFact)> = derived
            .iter()
            .filter_map(|fact| match &fact.item {
                OverlayItem::Node(node) => Some((node, fact)),
                OverlayItem::Edge(_) => None,
            })
            .collect();
        let flow_nodes: Vec<&Node> = flows.iter().map(|(node, _)| *node).collect();
        let (units, omitted) =
            knowledge_ingest::units_with_history(nodes, &flow_nodes, section_text, commits);
        let base_ids: BTreeSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
        let mut flow_cites: BTreeMap<String, Vec<BaseRef>> = BTreeMap::new();
        for (node, fact) in flows {
            // A base node of the same id is the unit; the first flow of an
            // id is the one the unit recipe kept.
            if !base_ids.contains(node.id.as_str()) {
                flow_cites
                    .entry(node.id.clone())
                    .or_insert_with(|| fact.cites.iter().cloned().collect());
            }
        }
        let files: BTreeSet<&str> = nodes
            .iter()
            .filter(|node| node.label == "File")
            .map(|node| node.id.as_str())
            .collect();
        for commit in knowledge_ingest::cited_commits(nodes, commits) {
            let id = commit.id();
            if !base_ids.contains(id.as_str()) {
                flow_cites.entry(id).or_insert_with(|| {
                    commit
                        .files()
                        .filter(|file| files.contains(file.as_str()))
                        .map(BaseRef::Node)
                        .collect()
                });
            }
        }
        let index = Connection::open_in_memory()?;
        index.execute_batch("CREATE VIRTUAL TABLE units USING fts5(text, node_id UNINDEXED)")?;
        {
            let mut insert =
                index.prepare("INSERT INTO units(rowid, text, node_id) VALUES (?1, ?2, ?3)")?;
            for (row, unit) in units.iter().enumerate() {
                insert.execute(rusqlite::params![row as i64, unit.text, unit.node_id])?;
            }
        }
        let mut names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for node in nodes {
            names
                .entry(node.id.clone())
                .or_default()
                .insert(node.id.clone());
            for key in NAME_KEYS {
                if let Some(name) = node.props.get(key).and_then(|v| v.as_str()) {
                    names
                        .entry(name.to_owned())
                        .or_default()
                        .insert(node.id.clone());
                }
            }
            if let (Some(method), Some(path)) = (
                node.props.get("method").and_then(|v| v.as_str()),
                node.props.get("path").and_then(|v| v.as_str()),
            ) {
                names
                    .entry(format!("{method} {path}"))
                    .or_default()
                    .insert(node.id.clone());
            }
        }
        let mut edges = edges.to_vec();
        edges.sort_by(|a, b| (&a.src, &a.dst, &a.label).cmp(&(&b.src, &b.dst, &b.label)));
        Ok(Self {
            index,
            units,
            omitted,
            vectors: None,
            vector_index: None,
            names,
            edges,
            flow_cites,
        })
    }

    /// Embed every unit once so queries can add the vector leg. Run it with
    /// the cached provider of the embedding phase: unchanged units are cache
    /// hits. Only a local provider is accepted, and queries must later use
    /// the same embedding model.
    pub fn embed_units(&mut self, provider: &dyn LlmProvider) -> Result<(), RetrievalError> {
        local(provider)?;
        let space = space(provider)?;
        let mut vectors = Vec::with_capacity(self.units.len());
        for chunk in self.units.chunks(CHUNK) {
            let texts: Vec<String> = chunk.iter().map(|u| u.text.clone()).collect();
            let embedded = provider.embed(&texts)?;
            if embedded.len() != texts.len() {
                return Err(RetrievalError::Query("embedding count mismatch"));
            }
            vectors.extend(embedded);
        }
        let dims = vectors.first().map_or(0, Vec::len);
        let vectors: Vec<Vec<f32>> = vectors
            .into_iter()
            .map(|v| normalized(v, dims).ok_or(RetrievalError::Query("invalid unit vector")))
            .collect::<Result<_, _>>()?;
        self.vector_index = vector_index(&vectors, dims);
        self.vectors = Some((space, vectors));
        Ok(())
    }

    /// Units held by the vector index, when one was built.
    pub fn indexed_vectors(&self) -> Option<usize> {
        self.vector_index.as_ref().map(Index::size)
    }

    /// Anchors, then their neighbours, then fused ranked hits, at most
    /// `limit` (capped at [`MAX_HITS`]). The vector leg runs only when units
    /// were embedded and a provider is given, which must be local. Ranking,
    /// and with it any provider call, is skipped once graph hits fill the
    /// limit.
    pub fn search(
        &self,
        query: &str,
        provider: Option<&dyn LlmProvider>,
        limit: usize,
    ) -> Result<Vec<Hit>, RetrievalError> {
        let query = query.trim();
        if query.len() > MAX_QUERY_BYTES {
            return Err(RetrievalError::Query("query too long"));
        }
        let limit = limit.min(MAX_HITS);
        let mut hits = Vec::new();
        let mut seen = BTreeSet::new();
        let anchors = self.names.get(query).cloned().unwrap_or_default();
        for id in &anchors {
            seen.insert(id.clone());
            hits.push(hit(
                id,
                Reason::Anchor,
                0.0,
                vec![BaseRef::Node(id.clone())],
            ));
        }
        for edge in &self.edges {
            if hits.len() >= limit {
                break;
            }
            let other = match (anchors.contains(&edge.src), anchors.contains(&edge.dst)) {
                (true, _) => &edge.dst,
                (false, true) => &edge.src,
                _ => continue,
            };
            if seen.insert(other.clone()) {
                hits.push(hit(
                    other,
                    Reason::Neighbour,
                    0.0,
                    vec![
                        BaseRef::Node(other.clone()),
                        BaseRef::Edge(edge.src.clone(), edge.dst.clone(), edge.label.clone()),
                    ],
                ));
            }
        }
        if hits.len() >= limit {
            hits.truncate(limit);
            return Ok(hits);
        }
        let mut fused: BTreeMap<usize, f64> = BTreeMap::new();
        for leg in [self.lexical(query)?, self.semantic(query, provider)?] {
            for (rank, row) in leg.into_iter().enumerate() {
                *fused.entry(row).or_default() += 1.0 / (FUSION_K + rank as f64 + 1.0);
            }
        }
        let mut ranked: Vec<(usize, f64)> = fused.into_iter().collect();
        ranked.sort_by(|(a, x), (b, y)| {
            y.partial_cmp(x)
                .unwrap_or(Ordering::Equal)
                .then_with(|| self.units[*a].node_id.cmp(&self.units[*b].node_id))
        });
        for (row, score) in ranked {
            let id = &self.units[row].node_id;
            if seen.insert(id.clone()) {
                let cites = self
                    .flow_cites
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| vec![BaseRef::Node(id.clone())]);
                hits.push(hit(id, Reason::Ranked, score, cites));
            }
        }
        hits.truncate(limit);
        Ok(hits)
    }

    /// Unit rows by FTS5 bm25 over the query's terms, ties by node id, both
    /// applied before the row limit.
    fn lexical(&self, query: &str) -> Result<Vec<usize>, RetrievalError> {
        let terms: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .take(MAX_TERMS)
            .map(|t| format!("\"{t}\""))
            .collect();
        if terms.is_empty() {
            return Ok(vec![]);
        }
        let mut statement = self.index.prepare(
            "SELECT rowid FROM units WHERE units MATCH ?1 ORDER BY bm25(units), node_id, rowid LIMIT ?2",
        )?;
        let rows = statement
            .query_map(
                rusqlite::params![terms.join(" OR "), MAX_HITS as i64],
                |r| Ok(r.get::<_, i64>(0)? as usize),
            )?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Unit rows by cosine similarity to the redacted query, ties by node id.
    fn semantic(
        &self,
        query: &str,
        provider: Option<&dyn LlmProvider>,
    ) -> Result<Vec<usize>, RetrievalError> {
        let (Some((indexed, vectors)), Some(provider)) = (&self.vectors, provider) else {
            return Ok(vec![]);
        };
        let (Some(dims), false) = (vectors.first().map(Vec::len), query.is_empty()) else {
            return Ok(vec![]);
        };
        local(provider)?;
        if space(provider)? != *indexed {
            return Err(RetrievalError::Query(
                "provider is not the indexed embedding model",
            ));
        }
        let (redacted, _) = core_redact::redact_text(query);
        let embedded = provider.embed(&[redacted])?;
        let query = embedded
            .into_iter()
            .next()
            .and_then(|v| normalized(v, dims))
            .ok_or(RetrievalError::Query("invalid query vector"))?;
        let score =
            |row: usize| -> f32 { vectors[row].iter().zip(&query).map(|(a, b)| a * b).sum() };
        // The index only proposes rows. Scores and order come from the stored
        // vectors, so a row the index returns twice or out of range is
        // harmless and a failed lookup falls back to scoring every vector.
        let candidates: Option<BTreeSet<usize>> = self
            .vector_index
            .as_ref()
            .filter(|_| vectors.len() > VECTOR_CANDIDATES)
            .and_then(|index| index.search(&query, VECTOR_CANDIDATES).ok())
            .map(|matches| {
                matches
                    .keys
                    .iter()
                    .filter_map(|key| usize::try_from(*key).ok())
                    .filter(|row| *row < vectors.len())
                    .collect()
            });
        let mut rows: Vec<(usize, f32)> = match candidates {
            Some(candidates) => candidates
                .into_iter()
                .map(|row| (row, score(row)))
                .collect(),
            None => (0..vectors.len()).map(|row| (row, score(row))).collect(),
        };
        rows.sort_by(|(a, x), (b, y)| {
            y.partial_cmp(x)
                .unwrap_or(Ordering::Equal)
                .then_with(|| self.units[*a].node_id.cmp(&self.units[*b].node_id))
        });
        rows.truncate(MAX_HITS);
        Ok(rows.into_iter().map(|(row, _)| row).collect())
    }
}

/// Unit and query text never leave the machine from retrieval.
fn local(provider: &dyn LlmProvider) -> Result<(), RetrievalError> {
    match provider.locality() {
        llm::Locality::Local => Ok(()),
        llm::Locality::Cloud => Err(RetrievalError::Query("provider is not local")),
    }
}

fn space(provider: &dyn LlmProvider) -> Result<Space, RetrievalError> {
    Ok((
        provider.embedding_model_id(),
        provider.embedding_cache_identity()?,
    ))
}

fn hit(id: &str, reason: Reason, score: f64, cites: Vec<BaseRef>) -> Hit {
    Hit {
        node_id: id.to_owned(),
        reason,
        score,
        cites,
        tier: Tier::Semantic,
    }
}

/// A cosine index over unit-normalized `vectors`, keyed by row and filled in
/// row order from one thread. `None` when usearch refuses any step: the
/// caller then scores every vector.
fn vector_index(vectors: &[Vec<f32>], dims: usize) -> Option<Index> {
    if vectors.is_empty() || dims == 0 {
        return None;
    }
    let index = Index::new(&IndexOptions {
        dimensions: dims,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        ..IndexOptions::default()
    })
    .ok()?;
    index.reserve(vectors.len()).ok()?;
    for (row, vector) in vectors.iter().enumerate() {
        index.add(row as u64, vector).ok()?;
    }
    Some(index)
}

/// A non-zero vector of `dims` scaled to unit length; a non-finite
/// component makes the norm non-finite and refuses it.
fn normalized(mut v: Vec<f32>, dims: usize) -> Option<Vec<f32>> {
    if v.len() != dims || dims == 0 {
        return None;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return None;
    }
    v.iter_mut().for_each(|x| *x /= norm);
    Some(v)
}

#[cfg(test)]
mod tests;
