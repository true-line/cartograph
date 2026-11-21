//! Language-neutral assembly of configuration-read facts (#1789, ADR-0059):
//! the `ConfigRead` nodes and `READS_CONFIG` edges an adapter's proven
//! standard-library environment reads become. Every compiled-in adapter
//! assembles here, so the shape is one.
//!
//! One node per source file and key, id `cfgread:{repo}@{path}:env:{KEY}`,
//! whose evidence is the span of every read of that key in that file in byte
//! order. A read whose key is not a string literal is a dynamic read: one
//! node per file, id `cfgread:{repo}@{path}:env:?`, `dynamic: true` and no
//! `key`. Only the key name and the spans are recorded here, never a value,
//! a default or a fallback; an adapter may add a numeric or boolean literal
//! `defaults` list to an assembled node (ADR-0059 decision 5 as amended,
//! #1998).
//!
//! The adapter decides what a proven read is (ADR-0059 decision 4): a site
//! reaches this module only when the call is provably the language's own
//! environment read. A shadowed look-alike is never a site.

use crate::observed::{MAX_IDENTITY, ObservedSource};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};

/// Node label of a configuration read.
pub const CONFIG_READ: &str = "ConfigRead";

/// Edge from the enclosing symbol (or the file) of a read to its node.
pub const READS_CONFIG: &str = "READS_CONFIG";

/// `props.source` of an environment-variable read.
pub const SOURCE_ENV: &str = "env";

/// One proven environment read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigReadSite {
    /// The literal key name; `None` for a read whose key is not a string
    /// literal (a dynamic read).
    pub key: Option<String>,
    /// Language recorded on the node (`python`, `typescript`, `go`, …).
    pub language: &'static str,
    /// Enclosing symbol id; `None` when the read is at file level.
    pub symbol: Option<String>,
    /// Repo-relative file path.
    pub path: String,
    /// Span of the read expression.
    pub byte_start: u64,
    /// Span end.
    pub byte_end: u64,
}

/// Whether a literal key may be recorded by name. An empty name, one longer
/// than [`MAX_IDENTITY`], one with whitespace or a control character, or one
/// a secret recognizer matches is not a key name: the read is then recorded
/// as a dynamic read, which stores no name (fail closed).
fn recordable(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_IDENTITY
        && !key.chars().any(|c| c.is_whitespace() || c.is_control())
        && core_redact::recognized_secrets(key).is_empty()
}

fn prov_value(
    source: &ObservedSource,
    path: &str,
    spans: &BTreeSet<(u64, u64)>,
    fact: &str,
) -> serde_json::Value {
    let evidence = spans
        .iter()
        .map(|&(byte_start, byte_end)| EvidenceRef {
            repo: source.repo.into(),
            path: path.into(),
            byte_start,
            byte_end,
            commit_sha: source.commit.into(),
        })
        .collect();
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        evidence,
        source.extractor,
        fact.as_bytes(),
    )
    .expect("Deterministic within ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

/// `ConfigRead` nodes and `READS_CONFIG` edges for the collected sites.
///
/// The output does not depend on the order of `sites`: nodes are ordered by
/// path then key (the dynamic node of a file last), edges by node then
/// source id, and every evidence list by byte offset. One edge per source
/// and node, citing each read that source makes of it.
pub fn assemble(source: &ObservedSource, sites: &[ConfigReadSite]) -> (Vec<Node>, Vec<Edge>) {
    type Spans = BTreeSet<(u64, u64)>;
    struct Read {
        language: &'static str,
        spans: Spans,
        /// Reads per edge source id.
        sources: BTreeMap<String, Spans>,
    }
    let repo = source.repo;
    // (path, dynamic, key): a file's named keys in order, then its dynamic node.
    let mut reads: BTreeMap<(&str, bool, &str), Read> = BTreeMap::new();
    for site in sites {
        let key = site.key.as_deref().filter(|key| recordable(key));
        let span = (site.byte_start, site.byte_end);
        let read = reads
            .entry((site.path.as_str(), key.is_none(), key.unwrap_or("")))
            .or_insert_with(|| Read {
                language: site.language,
                spans: Spans::new(),
                sources: BTreeMap::new(),
            });
        read.spans.insert(span);
        let src = site
            .symbol
            .clone()
            .unwrap_or_else(|| format!("file:{repo}@{}", site.path));
        read.sources.entry(src).or_default().insert(span);
    }
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for ((path, dynamic, key), read) in reads {
        let id = if dynamic {
            format!("cfgread:{repo}@{path}:{SOURCE_ENV}:?")
        } else {
            format!("cfgread:{repo}@{path}:{SOURCE_ENV}:{key}")
        };
        let mut props = serde_json::json!({
            "source": SOURCE_ENV,
            "language": read.language,
            "prov": prov_value(source, path, &read.spans, &format!("{CONFIG_READ} {id}")),
        });
        if dynamic {
            props["dynamic"] = serde_json::json!(true);
        } else {
            props["key"] = serde_json::json!(key);
        }
        nodes.push(Node {
            id: id.clone(),
            label: CONFIG_READ.into(),
            props,
        });
        for (src, spans) in read.sources {
            let prov = prov_value(
                source,
                path,
                &spans,
                &format!("{READS_CONFIG} {src} -> {id}"),
            );
            edges.push(Edge {
                src,
                dst: id.clone(),
                label: READS_CONFIG.into(),
                props: serde_json::json!({ "prov": prov }),
            });
        }
    }
    (nodes, edges)
}

/// Environment keys that by a hosting platform's convention hold the
/// endpoint of one kind of data store (#2065), with that store's provider.
/// Only a key that names the store's URL is listed: a token or password key
/// proves nothing about which store it opens.
pub const STORE_ENDPOINT_KEYS: &[(&str, &str)] = &[
    // Upstash Redis, and Vercel KV, which is Upstash Redis.
    ("UPSTASH_REDIS_REST_URL", "redis"),
    ("KV_REST_API_URL", "redis"),
    ("KV_URL", "redis"),
    ("REDIS_URL", "redis"),
    // Vercel Postgres.
    ("POSTGRES_URL", "postgres"),
    ("MONGODB_URI", "mongodb"),
];

/// Marker on a store inferred from the key of an environment read alone.
pub const STORE_FROM_ENV_KEY: &str = "store_from_env_key";

/// Observed data stores the reads of [`STORE_ENDPOINT_KEYS`] point to
/// (#2065): a client that talks to the store by hand (a REST call to its
/// URL) uses no SDK the observed-client registry knows, yet reads the
/// conventional key. The store rests on that convention, so the site is
/// marked and its resource is `InferredStrong`, never `Confirmed`.
pub fn store_sites(sites: &[ConfigReadSite]) -> Vec<crate::observed::ObservedSite> {
    sites
        .iter()
        .filter_map(|site| {
            let key = site.key.as_deref()?;
            let (_, provider) = STORE_ENDPOINT_KEYS.iter().find(|(name, _)| *name == key)?;
            Some(crate::observed::ObservedSite {
                family: crate::events::ObservedFamily::SdkClient,
                provider: (*provider).to_string(),
                module: None,
                identity: Some(crate::events::IdentityExpr::EnvRef(key.to_string())),
                identity_source: None,
                identity_assumptions: vec![STORE_FROM_ENV_KEY],
                registry: None,
                symbol: site.symbol.clone(),
                path: site.path.clone(),
                byte_start: site.byte_start,
                byte_end: site.byte_end,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "config_read/tests.rs"]
mod tests;
