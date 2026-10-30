//! Knowledge-layer embedding phase (ADR-0051 decision 3, #1441).
//!
//! Runs after the deterministic base and never mutates it. Units are chosen
//! and rendered by a versioned recipe, redacted before any text leaves for
//! the provider, and embedded through [`CachedProvider`] so vectors persist
//! under ADR-0037's identity, bounds and eviction. Vectors are not returned:
//! retrieval reloads them from the cache namespace, which stays a rebuildable
//! cache, never authority. Without an embedding provider the phase is skipped
//! as a named omission.
use crate::embedding_cache::{
    CacheStats, CachedProvider, EmbeddingCache, EmbeddingSpaceBinding, Lease,
};
use core_graph::Node;
use llm::{LlmProvider, ProviderError};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

/// Versioned unit selection and rendering recipe.
pub const UNIT_RECIPE: &str = "knowledge-units@1";
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

/// Base labels embedded, in admission priority order. Within a label units
/// are ordered by node id, so a cap keeps the same units on every run.
const LABELS: [&str; 10] = [
    "Endpoint",
    "Screen",
    "DataEntity",
    "Service",
    "Channel",
    "Resource",
    "ADR",
    "Rule",
    "Module",
    "Symbol",
];

/// One base node rendered for embedding.
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
    let mut ranked: Vec<_> = nodes
        .iter()
        .filter_map(|node| {
            LABELS
                .iter()
                .position(|label| *label == node.label)
                .map(|rank| (rank, node))
        })
        .collect();
    ranked.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.id.cmp(&y.id)));
    ranked.dedup_by(|(_, x), (_, y)| x.id == y.id);
    let omitted = ranked.len().saturating_sub(MAX_UNITS);
    let units = ranked
        .into_iter()
        .take(MAX_UNITS)
        .map(|(_, node)| KnowledgeUnit {
            node_id: node.id.clone(),
            text: render(node),
        })
        .collect();
    (units, omitted)
}

/// Label plus scalar properties in key order; nested values, including
/// `prov`, are left out. Redaction runs before truncation so a cut cannot split a secret
/// past its pattern.
fn render(node: &Node) -> String {
    let mut text = node.label.clone();
    if let Some(props) = node.props.as_object() {
        for (key, value) in props {
            let value = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            text.push('\n');
            text.push_str(key);
            text.push_str(": ");
            text.push_str(&value);
        }
    }
    let (mut text, _) = core_redact::redact_text(&text);
    if text.len() > MAX_UNIT_BYTES {
        let mut end = MAX_UNIT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
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
        /// Units past [`MAX_UNITS`], reported under [`UNIT_CAP`].
        omitted: usize,
        stats: CacheStats,
    },
}

/// Embed the base snapshot's units through the bounded vector cache.
pub fn run(
    nodes: &[Node],
    provider: Option<&dyn LlmProvider>,
    cache: Arc<Mutex<EmbeddingCache>>,
    lease: Lease,
) -> Result<PhaseOutcome, ProviderError> {
    let Some(provider) = provider.filter(|p| p.capabilities().embeddings) else {
        return Ok(PhaseOutcome::Skipped {
            omission: NO_PROVIDER,
        });
    };
    let (units, omitted) = units(nodes);
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
