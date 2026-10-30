//! Knowledge overlay store (ADR-0051 decision 2, #1440): derived facts kept
//! beside the deterministic base, never in it. The overlay lives in its own
//! disposable SQLite file, binds the base [`StoreRevision`] it was derived
//! from, and is read back only while that revision is current. Every fact
//! cites the base facts it rests on and carries validated T2/T3 provenance;
//! the base graph and its generation are never written here.

use crate::{Edge, Node, RevisionedSnapshot, StoreRevision};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Most facts one overlay may hold.
pub const MAX_FACTS: usize = 200_000;
/// Most canonical fact bytes one overlay may hold.
pub const MAX_BYTES: usize = 256 * 1024 * 1024;
/// Most canonical bytes of one fact, citations included.
pub const MAX_FACT_BYTES: usize = 1024 * 1024;
/// Longest recipe or embedding namespace name.
pub const MAX_NAME_BYTES: usize = 256;

/// Longest stored basis; recipe names bound it far below this.
const MAX_BASIS_BYTES: usize = 64 * 1024;

const VERSION: i64 = 1;
const SCHEMA: &str = "CREATE TABLE overlay_meta (singleton INTEGER PRIMARY KEY CHECK(singleton=1), instance TEXT NOT NULL, basis TEXT, identity TEXT, CHECK((basis IS NULL) = (identity IS NULL))) STRICT; CREATE TABLE overlay_facts (key TEXT PRIMARY KEY NOT NULL, fact TEXT NOT NULL CHECK(length(CAST(fact AS BLOB)) <= 1048576)) STRICT;";

/// Failure of the overlay store or of a publication's admission rules.
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    /// Underlying storage failure.
    #[error("overlay storage: {0}")]
    Storage(#[from] rusqlite::Error),
    /// Fact or basis (de)serialization failure.
    #[error("overlay encoding: {0}")]
    Encoding(#[from] serde_json::Error),
    /// The store's schema or lifecycle metadata is not the admitted shape.
    #[error("overlay schema: {0}")]
    Schema(&'static str),
    /// The verified Capability selection exceeded caller admission; no facts returned.
    #[error("overlay Capability selection limit")]
    SelectionLimit,
    /// A publication broke an admission rule; nothing was written.
    #[error("overlay rejected: {0}")]
    Rejected(&'static str),
}

/// What an overlay was derived with, besides its base revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayRecipe {
    /// Versioned recipe of the producers that built the overlay.
    pub recipe: String,
    /// ADR-0037 embedding namespace, when vectors contributed.
    pub embedding_namespace: Option<String>,
    /// Whether the provider phase ran; a run without one is a distinct overlay.
    pub provider_ran: bool,
}

/// The complete basis an overlay identity binds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayBasis {
    /// Base revision the facts were derived from.
    pub base: StoreRevision,
    /// Producers, namespace and provider phase.
    pub recipe: OverlayRecipe,
}

/// A base fact an overlay fact rests on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum BaseRef {
    /// A base node, by id.
    Node(String),
    /// A base edge, by its `(src, dst, label)` key.
    Edge(String, String, String),
}

/// One derived node or edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum OverlayItem {
    /// A derived node, such as a `Capability` or `DomainEntity`.
    Node(Node),
    /// A derived edge between base or overlay nodes.
    Edge(Edge),
}

/// A derived fact and the base facts it cites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayFact {
    /// The derived node or edge.
    pub item: OverlayItem,
    /// Base facts it rests on; never empty.
    pub cites: BTreeSet<BaseRef>,
}

impl OverlayFact {
    /// Storage key, unique per node id or edge key and ordered for identity.
    fn key(&self) -> String {
        match &self.item {
            OverlayItem::Node(node) => format!("n\u{0}{}", node.id),
            OverlayItem::Edge(edge) => {
                format!("e\u{0}{}\u{0}{}\u{0}{}", edge.src, edge.dst, edge.label)
            }
        }
    }
}

/// Content identity of one overlay: BLAKE3 over the canonical basis and the
/// facts in key order. Equal basis and facts always give an equal identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OverlayIdentity(pub String);

/// The overlay as seen from one current base revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayRead {
    /// Nothing has been published since the last invalidation.
    Absent,
    /// An overlay exists but was derived from another base revision; its
    /// facts are not returned.
    Stale {
        /// Revision the stored overlay was derived from.
        base: StoreRevision,
    },
    /// The overlay derived from the current base revision.
    Current {
        /// Its content identity.
        identity: OverlayIdentity,
        /// Its basis.
        basis: OverlayBasis,
        /// Its facts, in key order.
        facts: Vec<OverlayFact>,
    },
}

/// Disposable overlay storage bound to one graph instance.
pub struct OverlayStore {
    connection: Connection,
}

impl OverlayStore {
    /// Open (creating if absent) the overlay at `path`, bound to no instance
    /// until [`Self::bind_instance`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OverlayError> {
        Self::init(Connection::open(path)?)
    }

    /// Open an in-memory overlay (tests, scratch analysis).
    pub fn open_in_memory() -> Result<Self, OverlayError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// Host-only: the fixed app-data overlay, `knowledge-overlay.db`. The
    /// overlay is disposable, so a database that fails validation is reset
    /// and recreated rather than refusing startup. Linked or special files
    /// are never owned state and are refused.
    pub fn open_disposable(app_data: impl AsRef<Path>) -> Result<Self, OverlayError> {
        let path = app_data
            .as_ref()
            .canonicalize()
            .map_err(|_| OverlayError::Schema("app data directory"))?
            .join("knowledge-overlay.db");
        check_owned_files(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        match Self::create_if_empty(&connection) {
            Ok(()) => {}
            Err(OverlayError::Schema(_)) => {}
            Err(other) => return Err(other),
        }
        let store = Self { connection };
        match store.validate() {
            Ok(()) => Ok(store),
            Err(OverlayError::Schema(_)) => {
                check_owned_files(&path)?;
                // SQLite resets main without interpreting foreign schema or
                // unlinking paths; I/O and locking failures stay errors.
                use rusqlite::config::DbConfig;
                store
                    .connection
                    .set_db_config(DbConfig::SQLITE_DBCONFIG_RESET_DATABASE, true)?;
                let reset = store.connection.execute_batch("VACUUM main");
                store
                    .connection
                    .set_db_config(DbConfig::SQLITE_DBCONFIG_RESET_DATABASE, false)?;
                reset?;
                Self::create_if_empty(&store.connection)?;
                store.validate()?;
                Ok(store)
            }
            Err(other) => Err(other),
        }
    }

    fn init(connection: Connection) -> Result<Self, OverlayError> {
        Self::create_if_empty(&connection)?;
        let store = Self { connection };
        store.validate()?;
        Ok(store)
    }

    /// Create the schema in an empty, unversioned database.
    fn create_if_empty(connection: &Connection) -> Result<(), OverlayError> {
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            let objects: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if objects != 0 {
                return Err(OverlayError::Schema("unversioned database is not empty"));
            }
            let tx = connection.unchecked_transaction()?;
            tx.execute_batch(SCHEMA)?;
            tx.execute(
                "INSERT INTO overlay_meta (singleton, instance) VALUES (1, '')",
                [],
            )?;
            tx.pragma_update(None, "user_version", VERSION)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Admit only this module's exact schema and one metadata row.
    fn validate(&self) -> Result<(), OverlayError> {
        let version: i64 = self
            .connection
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != VERSION {
            return Err(OverlayError::Schema("unsupported version"));
        }
        let objects: Vec<String> = self
            .connection
            .prepare(
                "SELECT sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND length(CAST(sql AS BLOB)) <= 4096 ORDER BY name LIMIT 3",
            )?
            .query_map([], |r| r.get::<_, Option<String>>(0))?
            .map(|sql| sql.map(Option::unwrap_or_default))
            .collect::<Result<_, _>>()?;
        let expected: Vec<&str> = SCHEMA
            .split(';')
            .map(str::trim)
            .filter(|sql| !sql.is_empty())
            .collect();
        if objects.len() != expected.len()
            || objects.iter().any(|sql| !expected.contains(&sql.as_str()))
        {
            return Err(OverlayError::Schema("unexpected schema objects"));
        }
        let meta: i64 = self.connection.query_row(
            "SELECT count(*) FROM overlay_meta WHERE singleton = 1 AND typeof(instance) = 'text' AND length(CAST(instance AS BLOB)) <= 128",
            [],
            |r| r.get(0),
        )?;
        if meta != 1 {
            return Err(OverlayError::Schema("invalid lifecycle metadata"));
        }
        Ok(())
    }

    /// Delete every overlay fact and its basis. Wired before destructive
    /// graph lifecycle work: clear, schema reset and retained-source forget.
    pub fn invalidate(&mut self) -> Result<(), OverlayError> {
        let tx = self.connection.transaction()?;
        tx.execute_batch(
            "DELETE FROM overlay_facts; UPDATE overlay_meta SET basis = NULL, identity = NULL WHERE singleton = 1;",
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Bind the overlay to a graph instance; a different instance discards
    /// whatever was derived from the previous one.
    pub fn bind_instance(&mut self, instance: &str) -> Result<(), OverlayError> {
        if instance.is_empty() || instance.len() > 128 {
            return Err(OverlayError::Rejected("graph instance"));
        }
        let current: String = self.connection.query_row(
            "SELECT instance FROM overlay_meta WHERE singleton = 1",
            [],
            |r| r.get(0),
        )?;
        if current != instance {
            let tx = self.connection.transaction()?;
            tx.execute_batch(
                "DELETE FROM overlay_facts; UPDATE overlay_meta SET basis = NULL, identity = NULL WHERE singleton = 1;",
            )?;
            tx.execute(
                "UPDATE overlay_meta SET instance = ?1 WHERE singleton = 1",
                [instance],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Validate `facts` against the snapshot they were derived from and
    /// atomically replace the overlay with them. Any rule broken rejects the
    /// whole publication and leaves the previous overlay as it was.
    pub fn publish(
        &mut self,
        base: &RevisionedSnapshot,
        recipe: OverlayRecipe,
        facts: Vec<OverlayFact>,
    ) -> Result<OverlayIdentity, OverlayError> {
        let instance: String = self.connection.query_row(
            "SELECT instance FROM overlay_meta WHERE singleton = 1",
            [],
            |r| r.get(0),
        )?;
        if instance != base.revision.graph_instance {
            return Err(OverlayError::Rejected(
                "base belongs to another graph instance",
            ));
        }
        if recipe.recipe.is_empty()
            || recipe.recipe.len() > MAX_NAME_BYTES
            || recipe
                .embedding_namespace
                .as_ref()
                .is_some_and(|namespace| namespace.is_empty() || namespace.len() > MAX_NAME_BYTES)
        {
            return Err(OverlayError::Rejected("recipe names"));
        }
        if facts.len() > MAX_FACTS {
            return Err(OverlayError::Rejected("fact count"));
        }
        let basis = OverlayBasis {
            base: base.revision.clone(),
            recipe,
        };
        let rows = admit(&base.graph, &facts)?;
        let identity = identity(&basis, &rows)?;
        let tx = self.connection.transaction()?;
        tx.execute("DELETE FROM overlay_facts", [])?;
        {
            let mut insert = tx.prepare("INSERT INTO overlay_facts (key, fact) VALUES (?1, ?2)")?;
            for (key, json) in &rows {
                insert.execute(params![key, json])?;
            }
        }
        tx.execute(
            "UPDATE overlay_meta SET basis = ?1, identity = ?2 WHERE singleton = 1",
            params![serde_json::to_string(&basis)?, identity.0],
        )?;
        tx.commit()?;
        Ok(identity)
    }

    /// The overlay, only when it was derived from `current`. A stored overlay
    /// past the publication bounds, or whose rows no longer reproduce its
    /// identity or admitted shape, is a schema error.
    pub fn read(&self, current: &StoreRevision) -> Result<OverlayRead, OverlayError> {
        self.read_within(current, MAX_FACTS, MAX_BYTES)
    }

    /// Verify the entire current overlay, retaining only Capability nodes.
    /// `max_facts` and `max_bytes` admit selected count and stored JSON bytes
    /// (including citations), not heap/RSS. One decoded row, its temporary key,
    /// and bounded metadata coexist with retained facts and need host admission.
    /// After selection overflow, discard selection and finish integrity checks:
    /// corruption takes precedence over SelectionLimit. Absent/Stale do not scan.
    /// As with `read`, citations are shape-checked, not re-resolved against base.
    pub fn read_capabilities_bounded(
        &self,
        current: &StoreRevision,
        max_facts: usize,
        max_bytes: usize,
    ) -> Result<OverlayRead, OverlayError> {
        let tx = self.connection.unchecked_transaction()?;
        let meta: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT CASE WHEN length(CAST(basis AS BLOB)) <= ?1 THEN basis END, CASE WHEN length(CAST(identity AS BLOB)) = 64 THEN identity END FROM overlay_meta WHERE singleton = 1 AND basis IS NOT NULL",
                [MAX_BASIS_BYTES as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((basis, stored)) = meta else {
            return Ok(OverlayRead::Absent);
        };
        let basis = basis.ok_or(OverlayError::Schema("overlay basis bytes"))?;
        let basis: OverlayBasis = serde_json::from_str(&basis)?;
        if &basis.base != current {
            return Ok(OverlayRead::Stale { base: basis.base });
        }
        let stored = stored.ok_or(OverlayError::Schema("overlay identity bytes"))?;
        let mut hash = IdentityHash::new(&basis)?;
        let mut selected = Vec::new();
        let (mut count, mut fact_bytes, mut key_bytes, mut selected_bytes) =
            (0usize, 0usize, 0usize, 0usize);
        let mut limited = false;
        {
            let mut statement = tx.prepare("SELECT key, fact FROM overlay_facts ORDER BY key")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                // SQLite-owned text is borrowed, so check each row before copying
                // keys or decoding JSON. Publication maxima still apply globally.
                let key = row
                    .get_ref(0)?
                    .as_str()
                    .map_err(|_| OverlayError::Schema("overlay key text"))?;
                let json = row
                    .get_ref(1)?
                    .as_str()
                    .map_err(|_| OverlayError::Schema("overlay fact text"))?;
                if key.len() > MAX_FACT_BYTES || json.len() > MAX_FACT_BYTES {
                    return Err(OverlayError::Schema("overlay row bytes"));
                }
                count += 1;
                fact_bytes += json.len();
                key_bytes += key.len();
                if count > MAX_FACTS || fact_bytes > MAX_BYTES || key_bytes > MAX_BYTES {
                    return Err(OverlayError::Schema("overlay rows exceed their bounds"));
                }
                hash.row(key, json);
                let fact: OverlayFact = serde_json::from_str(json)?;
                if fact.key() != key {
                    return Err(OverlayError::Schema("overlay fact key"));
                }
                if fact.cites.is_empty() || check_provenance(&fact.item).is_err() {
                    return Err(OverlayError::Schema("overlay fact shape"));
                }
                if !limited && matches!(&fact.item, OverlayItem::Node(n) if n.label == "Capability")
                {
                    if selected.len() >= max_facts
                        || json.len() > max_bytes.saturating_sub(selected_bytes)
                    {
                        limited = true;
                        selected = Vec::new();
                    } else {
                        selected_bytes += json.len();
                        selected.push(fact);
                    }
                }
            }
        }
        if hash.finish().0 != stored {
            return Err(OverlayError::Schema(
                "overlay rows do not match their identity",
            ));
        }
        tx.commit()?;
        if limited {
            return Err(OverlayError::SelectionLimit);
        }
        Ok(OverlayRead::Current {
            identity: OverlayIdentity(stored),
            basis,
            facts: selected,
        })
    }

    fn read_within(
        &self,
        current: &StoreRevision,
        max_facts: usize,
        max_bytes: usize,
    ) -> Result<OverlayRead, OverlayError> {
        let tx = self.connection.unchecked_transaction()?;
        let meta: Option<(Option<String>, String)> = tx
            .query_row(
                "SELECT CASE WHEN length(CAST(basis AS BLOB)) <= ?1 THEN basis END, identity FROM overlay_meta WHERE singleton = 1 AND basis IS NOT NULL",
                [MAX_BASIS_BYTES as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((basis, stored)) = meta else {
            return Ok(OverlayRead::Absent);
        };
        let basis = basis.ok_or(OverlayError::Schema("overlay basis bytes"))?;
        let basis: OverlayBasis = serde_json::from_str(&basis)?;
        if &basis.base != current {
            return Ok(OverlayRead::Stale { base: basis.base });
        }
        // Measure before loading: a locally modified database must not make
        // this read allocate past the bounds a publication is held to.
        // Keys repeat ids inside their facts, so both sums share one bound.
        let (count, facts, keys): (i64, i64, i64) = tx.query_row(
            "SELECT count(*), coalesce(sum(length(CAST(fact AS BLOB))), 0), coalesce(sum(length(CAST(key AS BLOB))), 0) FROM overlay_facts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if count as u64 > max_facts as u64
            || facts as u64 > max_bytes as u64
            || keys as u64 > max_bytes as u64
        {
            return Err(OverlayError::Schema("overlay rows exceed their bounds"));
        }
        let rows: Vec<(String, String)> = tx
            .prepare("SELECT key, fact FROM overlay_facts ORDER BY key")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        tx.commit()?;
        if identity(&basis, &rows)?.0 != stored {
            return Err(OverlayError::Schema(
                "overlay rows do not match their identity",
            ));
        }
        let facts = rows
            .iter()
            .map(|(key, json)| {
                let fact: OverlayFact = serde_json::from_str(json)?;
                if &fact.key() != key {
                    return Err(OverlayError::Schema("overlay fact key"));
                }
                if fact.cites.is_empty() || check_provenance(&fact.item).is_err() {
                    return Err(OverlayError::Schema("overlay fact shape"));
                }
                Ok(fact)
            })
            .collect::<Result<_, OverlayError>>()?;
        Ok(OverlayRead::Current {
            identity: OverlayIdentity(stored),
            basis,
            facts,
        })
    }
}

/// Refuse a database or sidecar that is not a singly linked regular file.
fn check_owned_files(path: &Path) -> Result<(), OverlayError> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        match std::fs::symlink_metadata(std::path::PathBuf::from(name)) {
            Ok(metadata) => {
                if !metadata.is_file() {
                    return Err(OverlayError::Rejected("overlay file is not a regular file"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if metadata.nlink() != 1 {
                        return Err(OverlayError::Rejected("overlay file has multiple links"));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(OverlayError::Rejected("overlay file metadata")),
        }
    }
    Ok(())
}

/// Check every admission rule and return `(key, canonical json)` rows in key
/// order: unique keys, no overlay node shadowing a base node, edge endpoints
/// in the base or the overlay, valid T2/T3 provenance, and non-empty
/// citations of existing base facts.
fn admit(
    base: &(Vec<Node>, Vec<Edge>),
    facts: &[OverlayFact],
) -> Result<Vec<(String, String)>, OverlayError> {
    let base_nodes: BTreeSet<&str> = base.0.iter().map(|node| node.id.as_str()).collect();
    let base_edges: BTreeSet<(&str, &str, &str)> = base
        .1
        .iter()
        .map(|edge| (edge.src.as_str(), edge.dst.as_str(), edge.label.as_str()))
        .collect();
    let overlay_nodes: BTreeSet<String> = facts
        .iter()
        .filter_map(|fact| match &fact.item {
            OverlayItem::Node(node) => Some(node.id.clone()),
            OverlayItem::Edge(_) => None,
        })
        .collect();
    let mut rows = BTreeMap::new();
    let mut bytes = 0usize;
    for fact in facts {
        match &fact.item {
            OverlayItem::Node(node) => {
                if node.id.is_empty() || base_nodes.contains(node.id.as_str()) {
                    return Err(OverlayError::Rejected("overlay node id shadows the base"));
                }
            }
            OverlayItem::Edge(edge) => {
                let known = |id: &str| base_nodes.contains(id) || overlay_nodes.contains(id);
                if !known(&edge.src) || !known(&edge.dst) {
                    return Err(OverlayError::Rejected("overlay edge endpoint"));
                }
            }
        }
        check_provenance(&fact.item)?;
        if fact.cites.is_empty() {
            return Err(OverlayError::Rejected("uncited overlay fact"));
        }
        for cite in &fact.cites {
            let exists = match cite {
                BaseRef::Node(id) => base_nodes.contains(id.as_str()),
                BaseRef::Edge(src, dst, label) => {
                    base_edges.contains(&(src.as_str(), dst.as_str(), label.as_str()))
                }
            };
            if !exists {
                return Err(OverlayError::Rejected("citation of a missing base fact"));
            }
        }
        let json = serde_json::to_string(fact)?;
        if json.len() > MAX_FACT_BYTES {
            return Err(OverlayError::Rejected("fact bytes"));
        }
        bytes += json.len();
        if bytes > MAX_BYTES {
            return Err(OverlayError::Rejected("overlay bytes"));
        }
        if rows.insert(fact.key(), json).is_some() {
            return Err(OverlayError::Rejected("duplicate overlay fact"));
        }
    }
    Ok(rows.into_iter().collect())
}

/// R-INT-2: a derived fact carries its own provenance, within its tier's
/// confidence ceiling, from the semantic or agentic tier only. Without this
/// an overlay fact could read as confirmed base content.
fn check_provenance(item: &OverlayItem) -> Result<(), OverlayError> {
    let props = match item {
        OverlayItem::Node(node) => &node.props,
        OverlayItem::Edge(edge) => &edge.props,
    };
    let prov = props
        .get("prov")
        .and_then(|prov| core_prov::Provenance::deserialize(prov).ok())
        .ok_or(OverlayError::Rejected("overlay fact provenance"))?;
    prov.validate()
        .map_err(|_| OverlayError::Rejected("overlay fact confidence above its tier"))?;
    if !matches!(
        prov.tier,
        core_prov::Tier::Semantic | core_prov::Tier::Agentic
    ) {
        return Err(OverlayError::Rejected("overlay fact tier"));
    }
    Ok(())
}

/// BLAKE3 over the canonical basis, then each `(key, fact)` row in order,
/// each part length-prefixed so no two inputs share an encoding.
fn identity(
    basis: &OverlayBasis,
    rows: &[(String, String)],
) -> Result<OverlayIdentity, OverlayError> {
    let mut hash = IdentityHash::new(basis)?;
    for (key, json) in rows {
        hash.row(key, json);
    }
    Ok(hash.finish())
}

/// Shared streaming form of the existing length-prefixed identity encoding.
struct IdentityHash(blake3::Hasher);
impl IdentityHash {
    fn new(basis: &OverlayBasis) -> Result<Self, OverlayError> {
        let mut hash = Self(blake3::Hasher::new());
        hash.part(b"cartograph-knowledge-overlay@1");
        hash.part(serde_json::to_string(basis)?.as_bytes());
        Ok(hash)
    }
    fn part(&mut self, value: &[u8]) {
        self.0.update(&(value.len() as u64).to_be_bytes());
        self.0.update(value);
    }
    fn row(&mut self, key: &str, json: &str) {
        self.part(key.as_bytes());
        self.part(json.as_bytes());
    }
    fn finish(self) -> OverlayIdentity {
        OverlayIdentity(self.0.finalize().to_hex().to_string())
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
