//! Bounded, disposable local embedding vectors. No source text or verdicts persist.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use llm::{Embedding, EmbeddingIdentity, LlmProvider, Locality, ProviderCaps, ProviderError};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Rows one store and one operation hold: a whole-VSCode plan (476,907
/// candidate symbols plus 81,654 hops, measured 2026-11-22) fits in one pass
/// with room to spare (#259, AC-1507).
pub const MAX_ROWS: usize = 1_048_576;
/// Highest `cache_meta.sequence` a valid store may hold: it reserves one
/// sequence number for every row a full plan can publish, so sealing never
/// overflows the counter. A store past it is exhausted and is recovered.
pub(crate) const SEQUENCE_CEILING: i64 = i64::MAX - MAX_ROWS as i64;
/// Vector bytes one store and one operation hold: a whole-VSCode plan at 1,024
/// f32 dimensions (about 2.1 GiB) fits. Sealing borrows the retained vectors
/// and the ANN index consumes the returned ones, so a pass peaks near twice
/// this, never three times (AC-1507).
pub const MAX_VECTOR_BYTES: usize = 3 * 1024 * 1024 * 1024;
pub const MAX_DIMENSIONS: usize = 4096;

/// Rows and vector bytes one embedding operation may hold in memory (#1653,
/// AC-1807). They can only lower [`MAX_ROWS`] and [`MAX_VECTOR_BYTES`], so a
/// smaller machine can fail an oversized plan at admission instead of running
/// out of memory. The store's own capacity stays at the shipped bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationLimits {
    max_rows: usize,
    max_vector_bytes: usize,
}

impl OperationLimits {
    pub const MIN_ROWS: usize = 65_536;
    pub const MIN_VECTOR_BYTES: usize = 256 * 1024 * 1024;

    /// Limits inside the shipped bounds, or `None`.
    pub fn within(max_rows: usize, max_vector_bytes: usize) -> Option<Self> {
        ((Self::MIN_ROWS..=MAX_ROWS).contains(&max_rows)
            && (Self::MIN_VECTOR_BYTES..=MAX_VECTOR_BYTES).contains(&max_vector_bytes))
        .then_some(Self {
            max_rows,
            max_vector_bytes,
        })
    }

    pub fn max_rows(self) -> usize {
        self.max_rows
    }

    pub fn max_vector_bytes(self) -> usize {
        self.max_vector_bytes
    }
}

impl Default for OperationLimits {
    fn default() -> Self {
        Self {
            max_rows: MAX_ROWS,
            max_vector_bytes: MAX_VECTOR_BYTES,
        }
    }
}
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Plan text bytes: `MAX_ROWS` texts averaging 512 bytes.
pub const MAX_INPUT_BYTES: usize = 512 * 1024 * 1024;
pub const BATCH_COUNT: usize = 128;
pub const BATCH_BYTES: usize = 1024 * 1024;
mod lifecycle;

const RECIPE: &str = "embedding-cache-le-f32@1";
const SCHEMA: &str = "CREATE TABLE cache_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), instance TEXT NOT NULL, epoch TEXT NOT NULL CHECK(length(epoch)=32), generation INTEGER NOT NULL CHECK(generation>=0), sequence INTEGER NOT NULL CHECK(sequence>=0)) STRICT; CREATE TABLE spaces (id TEXT PRIMARY KEY, dimension INTEGER NOT NULL CHECK(dimension BETWEEN 1 AND 4096)) STRICT; CREATE TABLE vectors (space TEXT NOT NULL REFERENCES spaces(id), key TEXT NOT NULL CHECK(length(key)=64), text_bytes INTEGER NOT NULL CHECK(text_bytes BETWEEN 0 AND 65536), dimension INTEGER NOT NULL CHECK(dimension BETWEEN 1 AND 4096), vector BLOB NOT NULL CHECK(length(vector)=dimension*4), sequence INTEGER NOT NULL CHECK(sequence>0), PRIMARY KEY(space,key)) STRICT;";
fn error(value: impl std::fmt::Display) -> ProviderError {
    ProviderError::InvalidEmbeddings(format!("embedding cache: {value}"))
}
fn valid_vector(v: &[f32]) -> bool {
    !v.is_empty() && v.len() <= MAX_DIMENSIONS && v.iter().all(|x| x.is_finite()) && {
        let norm = v.iter().fold(0.0_f32, |sum, x| sum + x * x);
        norm.is_finite() && norm > 0.0
    }
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    instance: String,
    generation: i64,
    epoch: String,
}
#[derive(Debug, Default, Clone, Serialize)]
pub struct CacheStats {
    pub input_texts: usize,
    pub input_bytes: usize,
    pub returned_vector_bytes: usize,
    pub unique_misses: usize,
    pub hits: usize,
    pub calls: usize,
    pub rejected_rows: usize,
    pub unpinned_identity: usize,
    pub evictions: usize,
    pub conflicts: usize,
}

pub struct EmbeddingCache {
    connection: Connection,
    disposable: bool,
    path: PathBuf,
}
impl EmbeddingCache {
    fn validate(&self) -> Result<(), ProviderError> {
        let version: i64 = self
            .connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(error)?;
        if version != 2 {
            return Err(error("unsupported schema"));
        }
        let admitted: bool = self.connection.query_row("SELECT count(*)=3 AND coalesce(min(typeof(name)='text' AND length(CAST(name AS BLOB))<=64 AND typeof(sql)='text' AND length(CAST(sql AS BLOB))<=4096),0) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",[],|r|r.get(0)).map_err(error)?;
        if !admitted {
            return Err(error("schema metadata bounds"));
        }
        // No application triggers/views or altered tables are admitted.
        let objects: Vec<(String, String)> = self
            .connection
            .prepare(
                "SELECT name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND length(CAST(name AS BLOB))<=64 AND length(CAST(sql AS BLOB))<=4096 ORDER BY name LIMIT 4",
            )
            .map_err(error)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?;
        let expected: Vec<_> = SCHEMA
            .split(';')
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_owned())
            .collect();
        if objects.len() != 3 || objects.iter().any(|(_, sql)| !expected.contains(sql)) {
            return Err(error("unexpected schema objects"));
        }
        let invalid: bool = self.connection.query_row("SELECT count(*) != 1 OR coalesce(max(NOT(singleton=1 AND typeof(instance)='text' AND length(CAST(instance AS BLOB))<=128 AND typeof(epoch)='text' AND length(CAST(epoch AS BLOB))=32 AND epoch NOT GLOB '*[^0-9a-f]*' AND typeof(generation)='integer' AND generation BETWEEN 0 AND 9223372036854775805 AND typeof(sequence)='integer' AND sequence BETWEEN 0 AND ?1)),1) FROM cache_meta",[SEQUENCE_CEILING],|r|r.get(0)).map_err(error)?;
        if invalid {
            return Err(error("invalid lifecycle metadata"));
        }
        let spaces: i64 = self
            .connection
            .query_row("SELECT count(*) FROM spaces", [], |r| r.get(0))
            .map_err(error)?;
        let valid_spaces:bool=self.connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM spaces WHERE typeof(id)!='text' OR length(CAST(id AS BLOB))!=64 OR id GLOB '*[^0-9a-f]*' OR typeof(dimension)!='integer' OR dimension NOT BETWEEN 1 AND 4096)",[],|r|r.get(0)).map_err(error)?;
        if spaces > 256 || !valid_spaces {
            return Err(error("namespace count bound"));
        }
        let bounded_keys:bool=self.connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM vectors WHERE typeof(space)!='text' OR length(CAST(space AS BLOB))!=64 OR typeof(key)!='text' OR length(CAST(key AS BLOB))!=64 OR typeof(sequence)!='integer' OR sequence<1)",[],|r|r.get(0)).map_err(error)?;
        if !bounded_keys {
            return Err(error("vector metadata bounds"));
        }
        let (count, bytes): (i64, i64) = self
            .connection
            .query_row(
                "SELECT count(*),coalesce(sum(length(vector)),0) FROM vectors",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(error)?;
        if count < 0 || bytes < 0 || count as usize > MAX_ROWS || bytes as usize > MAX_VECTOR_BYTES
        {
            return Err(error("store bounds"));
        }
        Ok(())
    }
    /// Rotate before destructive lifecycle work; transactionally delete every vector.
    pub fn invalidate(&mut self) -> Result<(), ProviderError> {
        self.ensure()?;
        let tx = self.connection.transaction().map_err(error)?;
        tx.execute_batch("DELETE FROM vectors; DELETE FROM spaces; UPDATE cache_meta SET epoch=lower(hex(randomblob(16))), generation=0, sequence=0 WHERE singleton=1;").map_err(error)?;
        tx.commit().map_err(error)
    }
    pub fn bind_instance(&mut self, instance: &str) -> Result<Lease, ProviderError> {
        if instance.len() != 32 || !instance.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(error("invalid graph instance"));
        }
        self.ensure()?;
        let current: String = self
            .connection
            .query_row(
                "SELECT instance FROM cache_meta WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(error)?;
        if current != instance {
            let tx = self.connection.transaction().map_err(error)?;
            tx.execute_batch("DELETE FROM vectors; DELETE FROM spaces;")
                .map_err(error)?;
            tx.execute("UPDATE cache_meta SET instance=?1,generation=generation+1,sequence=0 WHERE singleton=1",[instance]).map_err(error)?;
            tx.commit().map_err(error)?;
        }
        self.lease()
    }
    pub fn lease(&self) -> Result<Lease, ProviderError> {
        self.connection
            .query_row(
                "SELECT instance,generation,epoch FROM cache_meta WHERE singleton=1 AND typeof(instance)='text' AND length(CAST(instance AS BLOB))<=128 AND typeof(generation)='integer' AND generation>=0 AND typeof(epoch)='text' AND length(CAST(epoch AS BLOB))=32 AND epoch NOT GLOB '*[^0-9a-f]*'",
                [],
                |r| {
                    Ok(Lease {
                        instance: r.get(0)?,
                        generation: r.get(1)?,
                        epoch: r.get(2)?,
                    })
                },
            )
            .map_err(error)
    }
    fn check(&self, lease: &Lease) -> Result<(), ProviderError> {
        if &self.lease()? != lease {
            Err(error("stale graph generation"))
        } else {
            Ok(())
        }
    }
    fn load(
        &mut self,
        lease: &Lease,
        space: &str,
        key: &str,
        len: usize,
        stats: &mut CacheStats,
    ) -> Result<Option<Embedding>, ProviderError> {
        self.check(lease)?;
        // CASE bounds the BLOB in the very statement that reads it: a concurrent
        // connection cannot replace a separately preflighted value before fetch.
        let row:Option<Option<Vec<u8>>>=self.connection.query_row(
            "SELECT CASE WHEN typeof(v.vector)='blob' AND typeof(v.dimension)='integer' AND v.dimension BETWEEN 1 AND 4096 AND length(v.vector)=v.dimension*4 AND v.dimension=s.dimension AND v.text_bytes=?3 THEN v.vector ELSE NULL END FROM vectors v LEFT JOIN spaces s ON s.id=v.space WHERE v.space=?1 AND v.key=?2",
            params![space,key,len as i64],|r|r.get(0)).optional().map_err(error)?;
        let Some(blob) = row else { return Ok(None) };
        let vector = blob.map(|blob| {
            blob.chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect::<Vec<_>>()
        });
        if let Some(vector) = vector.filter(|v| valid_vector(v)) {
            return Ok(Some(vector));
        }
        self.connection
            .execute(
                "DELETE FROM vectors WHERE space=?1 AND key=?2",
                params![space, key],
            )
            .map_err(error)?;
        stats.rejected_rows += 1;
        Ok(None)
    }
    fn publish(
        &mut self,
        lease: &Lease,
        space: &str,
        items: &[(&str, usize, &[f32])],
        protected: &BTreeSet<(String, String)>,
        stats: &mut CacheStats,
    ) -> Result<(), ProviderError> {
        self.check(lease)?;
        let dim = items
            .first()
            .ok_or_else(|| error("empty publication"))?
            .2
            .len();
        if dim == 0
            || dim > MAX_DIMENSIONS
            || items
                .iter()
                .any(|(_, _, v)| v.len() != dim || !valid_vector(v))
        {
            return Err(error("invalid provider batch"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(error)?;
        let current: Lease = tx
            .query_row(
                "SELECT instance,generation,epoch FROM cache_meta WHERE singleton=1",
                [],
                |r| {
                    Ok(Lease {
                        instance: r.get(0)?,
                        generation: r.get(1)?,
                        epoch: r.get(2)?,
                    })
                },
            )
            .map_err(error)?;
        if &current != lease {
            return Err(error("stale graph generation"));
        }
        tx.execute(
            "INSERT INTO spaces VALUES(?1,?2) ON CONFLICT DO NOTHING",
            params![space, dim as i64],
        )
        .map_err(error)?;
        let pinned: i64 = tx
            .query_row("SELECT dimension FROM spaces WHERE id=?1", [space], |r| {
                r.get(0)
            })
            .map_err(error)?;
        if pinned != dim as i64 {
            return Err(error("model dimension changed"));
        }
        for (key, len, vector) in items {
            tx.execute(
                "UPDATE cache_meta SET sequence=sequence+1 WHERE singleton=1",
                [],
            )
            .map_err(error)?;
            let blob: Vec<u8> = vector.iter().flat_map(|x| x.to_le_bytes()).collect();
            let inserted=tx.execute("INSERT INTO vectors SELECT ?1,?2,?3,?4,?5,sequence FROM cache_meta WHERE singleton=1 ON CONFLICT DO NOTHING",params![space,key,*len as i64,dim as i64,blob]).map_err(error)?;
            stats.conflicts += usize::from(inserted == 0);
        }
        let (mut rows, mut bytes): (i64, i64) = tx
            .query_row(
                "SELECT count(*),coalesce(sum(length(vector)),0) FROM vectors",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(error)?;
        if rows as usize > MAX_ROWS || bytes as usize > MAX_VECTOR_BYTES {
            // Keys/lengths only, admitted by fixed row count before retaining an eviction list.
            if rows as usize > MAX_ROWS * 2 {
                return Err(error("publication row bound"));
            }
            let entries: Vec<(String, String, i64)> = tx
                .prepare("SELECT space,key,length(vector) FROM vectors ORDER BY sequence,space,key")
                .map_err(error)?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(error)?
                .collect::<Result<_, _>>()
                .map_err(error)?;
            for (owner, key, len) in entries {
                if rows as usize <= MAX_ROWS && bytes as usize <= MAX_VECTOR_BYTES {
                    break;
                }
                if protected.contains(&(owner.clone(), key.clone())) {
                    continue;
                }
                tx.execute(
                    "DELETE FROM vectors WHERE space=?1 AND key=?2",
                    params![owner, key],
                )
                .map_err(error)?;
                rows -= 1;
                bytes -= len;
                stats.evictions += 1;
            }
            if rows as usize > MAX_ROWS || bytes as usize > MAX_VECTOR_BYTES {
                return Err(error("current operation exceeds cache capacity"));
            }
        }
        tx.execute("DELETE FROM spaces WHERE NOT EXISTS(SELECT 1 FROM vectors WHERE vectors.space=spaces.id)",[]).map_err(error)?;
        let spaces: i64 = tx
            .query_row("SELECT count(*) FROM spaces", [], |r| r.get(0))
            .map_err(error)?;
        if spaces > 256 {
            // Whole-namespace FIFO eviction is independent of vector byte/row pressure.
            // The current operation's namespace (or any protected namespace) survives.
            let oldest:Vec<String>=tx.prepare("SELECT s.id FROM spaces s JOIN vectors v ON v.space=s.id GROUP BY s.id ORDER BY min(v.sequence),s.id LIMIT 257").map_err(error)?.query_map([],|r|r.get(0)).map_err(error)?.collect::<Result<_,_>>().map_err(error)?;
            let mut remaining = spaces;
            for owner in oldest {
                if remaining <= 256 {
                    break;
                }
                if protected.iter().any(|(space, _)| space == &owner) {
                    continue;
                }
                stats.evictions += tx
                    .execute("DELETE FROM vectors WHERE space=?1", [&owner])
                    .map_err(error)?;
                tx.execute("DELETE FROM spaces WHERE id=?1", [&owner])
                    .map_err(error)?;
                remaining -= 1;
            }
            if remaining > 256 {
                return Err(error("protected namespaces exceed capacity"));
            }
        }
        tx.commit().map_err(error)
    }
}

/// One preview operation. Prefill every planned text, seal atomically, then compute.
/// A sealed wrapper never dispatches a provider call or admits an unplanned key.
#[derive(Default)]
struct Operation {
    stats: CacheStats,
    vectors: BTreeMap<String, (usize, Embedding)>,
    staged: BTreeSet<String>,
    sealed: bool,
    failed: bool,
    unique_bytes: usize,
    vector_bytes: usize,
}
pub struct CachedProvider<'a> {
    provider: &'a dyn LlmProvider,
    cache: Arc<Mutex<EmbeddingCache>>,
    lease: Lease,
    identity: Option<EmbeddingIdentity>,
    space: Option<String>,
    limits: OperationLimits,
    state: Mutex<Operation>,
}
/// Observed embedding space of a successfully sealed operation.
/// This binds receipt compatibility, not label independence or model quality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmbeddingSpaceBinding {
    namespace: String,
    dimensions: usize,
}
impl EmbeddingSpaceBinding {
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }
}
impl<'a> CachedProvider<'a> {
    pub fn new(
        provider: &'a dyn LlmProvider,
        cache: Arc<Mutex<EmbeddingCache>>,
        lease: Lease,
    ) -> Result<Self, ProviderError> {
        Self::within(provider, cache, lease, OperationLimits::default())
    }
    /// [`Self::new`] holding at most `limits` in this operation.
    pub fn within(
        provider: &'a dyn LlmProvider,
        cache: Arc<Mutex<EmbeddingCache>>,
        lease: Lease,
        limits: OperationLimits,
    ) -> Result<Self, ProviderError> {
        if provider.locality() != Locality::Local || provider.id().len() > 512 {
            return Err(error("local provider required"));
        }
        cache.lock().map_err(error)?.ensure()?;
        let identity = provider.embedding_cache_identity()?;
        if identity.as_ref().is_some_and(|id| {
            id.endpoint.is_empty()
                || id.endpoint.len() > 512
                || id.model.is_empty()
                || id.model.len() > 512
                || id.recipe.is_empty()
                || id.recipe.len() > 128
                || id.digest.len() != 71
                || !id.digest.starts_with("sha256:")
                || !id.digest[7..].bytes().all(|b| b.is_ascii_hexdigit())
        }) {
            return Err(error("invalid model identity"));
        }
        let space = identity.as_ref().map(|id| {
            hash(
                &serde_json::to_vec(&(provider.id(), id, RECIPE, core_redact::TEXT_POLICY_VERSION))
                    .unwrap(),
            )
        });
        Ok(Self {
            provider,
            cache,
            lease,
            identity,
            space,
            limits,
            state: Mutex::new(Operation::default()),
        })
    }
    /// The bounds this operation holds.
    pub fn limits(&self) -> OperationLimits {
        self.limits
    }
    /// Publish only after all provider calls and final manifest observation succeed.
    /// Read first-writer winning bytes back before any final semantic computation.
    pub fn seal(&self) -> Result<(), ProviderError> {
        if self.provider.embedding_cache_identity()? != self.identity {
            return Err(error("model identity changed"));
        }
        let mut state = self.state.lock().map_err(error)?;
        if state.sealed || state.failed {
            return Err(error("operation already sealed or failed"));
        }
        let mut cache = self.cache.lock().map_err(error)?;
        cache.check(&self.lease)?;
        if let Some(space) = &self.space
            && !state.staged.is_empty()
        {
            // Borrow the retained vectors: publication never copies the set.
            let op = &mut *state;
            let items: Vec<(&str, usize, &[f32])> = op
                .staged
                .iter()
                .map(|key| {
                    let (len, v) = &op.vectors[key];
                    (key.as_str(), *len, v.as_slice())
                })
                .collect();
            let protected: BTreeSet<_> = op
                .vectors
                .keys()
                .map(|key| (space.clone(), key.clone()))
                .collect();
            cache.publish(&self.lease, space, &items, &protected, &mut op.stats)?;
            let published: Vec<(String, usize)> = items
                .iter()
                .map(|(key, len, _)| ((*key).to_owned(), *len))
                .collect();
            drop(items);
            for (key, len) in published {
                let winning = cache
                    .load(&self.lease, space, &key, len, &mut state.stats)?
                    .ok_or_else(|| error("published vector missing"))?;
                state.vectors.insert(key, (len, winning));
            }
        }
        state.sealed = true;
        Ok(())
    }
    pub fn finish(&self) -> Result<CacheStats, ProviderError> {
        self.finish_with_binding().map(|(stats, _)| stats)
    }
    /// Recheck identity and lease, then expose the exact cache namespace and
    /// observed dimension. Unpinned or empty operations have no usable binding.
    pub fn finish_with_binding(
        &self,
    ) -> Result<(CacheStats, Option<EmbeddingSpaceBinding>), ProviderError> {
        if self.provider.embedding_cache_identity()? != self.identity {
            // Conservative invalidation also removes rows sealed by this operation.
            self.cache.lock().map_err(error)?.invalidate()?;
            return Err(error("model identity changed"));
        }
        self.cache.lock().map_err(error)?.check(&self.lease)?;
        let state = self.state.lock().map_err(error)?;
        if !state.sealed || state.failed {
            return Err(error("unsealed operation"));
        }
        let binding = self.space.as_ref().and_then(|namespace| {
            state
                .vectors
                .values()
                .next()
                .map(|(_, vector)| EmbeddingSpaceBinding {
                    namespace: namespace.clone(),
                    dimensions: vector.len(),
                })
        });
        if let Some(binding) = &binding
            && state
                .vectors
                .values()
                .any(|(_, vector)| vector.len() != binding.dimensions)
        {
            return Err(error("inconsistent operation dimensions"));
        }
        Ok((state.stats.clone(), binding))
    }
}
impl LlmProvider for CachedProvider<'_> {
    fn id(&self) -> &str {
        self.provider.id()
    }
    fn locality(&self) -> Locality {
        self.provider.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.provider.capabilities()
    }
    fn embedding_model_id(&self) -> String {
        self.provider.embedding_model_id()
    }
    fn completion_model_id(&self) -> String {
        self.provider.completion_model_id()
    }
    fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        let result = self.embed_inner(batch);
        if result.is_err() {
            self.state.lock().map_err(error)?.failed = true;
        }
        result
    }
}
impl CachedProvider<'_> {
    fn embed_inner(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        if batch.len() > MAX_ROWS
            || batch.iter().any(|t| t.len() > MAX_TEXT_BYTES)
            || batch
                .iter()
                .try_fold(0usize, |n, t| n.checked_add(t.len()))
                .is_none_or(|n| n > MAX_INPUT_BYTES)
        {
            return Err(error("input bounds"));
        }
        let texts: Vec<String> = batch
            .iter()
            .map(|t| core_redact::redact_text(t).0)
            .collect();
        if texts.iter().any(|t| t.len() > MAX_TEXT_BYTES) {
            return Err(error("redacted input bounds"));
        }
        let mut state = self.state.lock().map_err(error)?;
        if state.failed {
            return Err(error("operation failed"));
        }
        self.cache.lock().map_err(error)?.check(&self.lease)?;
        state.stats.input_texts = state
            .stats
            .input_texts
            .checked_add(texts.len())
            .ok_or_else(|| error("operation count overflow"))?;
        state.stats.input_bytes = state
            .stats
            .input_bytes
            .checked_add(texts.iter().map(String::len).sum())
            .ok_or_else(|| error("operation byte overflow"))?;
        if state.stats.input_texts > MAX_ROWS * 4 || state.stats.input_bytes > MAX_INPUT_BYTES * 4 {
            return Err(error("whole-preview request bounds"));
        }
        let mut unique = BTreeMap::new();
        for text in &texts {
            unique.entry(hash(text.as_bytes())).or_insert(text);
        }
        let mut misses = Vec::new();
        for (key, text) in &unique {
            if state.vectors.contains_key(key) {
                state.stats.hits += 1;
                continue;
            }
            if state.sealed {
                return Err(error("unplanned text after seal"));
            }
            state.unique_bytes = state
                .unique_bytes
                .checked_add(text.len())
                .ok_or_else(|| error("unique input overflow"))?;
            if state.unique_bytes > MAX_INPUT_BYTES
                || state
                    .vectors
                    .len()
                    .checked_add(misses.len())
                    .and_then(|n| n.checked_add(1))
                    .is_none_or(|n| n > self.limits.max_rows)
            {
                return Err(error("whole-preview working set"));
            }
            let hit = if let Some(space) = &self.space {
                self.cache.lock().map_err(error)?.load(
                    &self.lease,
                    space,
                    key,
                    text.len(),
                    &mut state.stats,
                )?
            } else {
                state.stats.unpinned_identity += 1;
                None
            };
            if let Some(vector) = hit {
                state.vector_bytes += vector.len() * 4;
                if state.vector_bytes > self.limits.max_vector_bytes {
                    return Err(error("whole-preview vector bytes"));
                }
                state.vectors.insert(key.clone(), (text.len(), vector));
                state.stats.hits += 1;
            } else {
                misses.push((key.clone(), (*text).clone()));
            }
        }
        state.stats.unique_misses += misses.len();
        let mut offset = 0;
        while offset < misses.len() {
            let mut end = offset;
            let mut bytes = 0;
            while end < misses.len()
                && end - offset < BATCH_COUNT
                && bytes + misses[end].1.len() <= BATCH_BYTES
            {
                bytes += misses[end].1.len();
                end += 1;
            }
            let input: Vec<_> = misses[offset..end].iter().map(|(_, t)| t.clone()).collect();
            if self.provider.embedding_cache_identity()? != self.identity {
                return Err(error("model identity changed"));
            }
            state.stats.calls += 1;
            let output = self.provider.embed(&input)?;
            if self.provider.embedding_cache_identity()? != self.identity {
                return Err(error("model identity changed"));
            }
            let dim = output.first().map_or(0, Vec::len);
            if output.len() != input.len()
                || dim == 0
                || dim > MAX_DIMENSIONS
                || output.iter().any(|v| v.len() != dim || !valid_vector(v))
            {
                return Err(error("invalid provider batch"));
            }
            if let Some((_, prior)) = state.vectors.values().next()
                && prior.len() != dim
            {
                return Err(error("operation mixed dimensions"));
            }
            for ((key, text), vector) in misses[offset..end].iter().zip(output) {
                state.vector_bytes = state
                    .vector_bytes
                    .checked_add(vector.len() * 4)
                    .ok_or_else(|| error("vector overflow"))?;
                if state.vector_bytes > self.limits.max_vector_bytes {
                    return Err(error("whole-preview vector bytes"));
                }
                state.staged.insert(key.clone());
                state.vectors.insert(key.clone(), (text.len(), vector));
            }
            offset = end;
        }
        self.cache.lock().map_err(error)?.check(&self.lease)?;
        let output_bytes = texts
            .iter()
            .try_fold(0usize, |n, t| {
                state
                    .vectors
                    .get(&hash(t.as_bytes()))
                    .and_then(|(_, v)| v.len().checked_mul(4))
                    .and_then(|b| n.checked_add(b))
            })
            .ok_or_else(|| error("output byte overflow"))?;
        if output_bytes > self.limits.max_vector_bytes {
            return Err(error("returned vector byte limit"));
        }
        state.stats.returned_vector_bytes = output_bytes;
        texts
            .iter()
            .map(|t| {
                state
                    .vectors
                    .get(&hash(t.as_bytes()))
                    .map(|(_, v)| v.clone())
                    .ok_or_else(|| error("missing vector"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
