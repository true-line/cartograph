//! SQLite persistence for immutable bodies and independent, revisioned metadata.
#![allow(missing_docs)]
use crate::body::{MAX_BODY_BYTES, MAX_RESPONSE_BYTES, bounded_json, packet_id, valid_packet_id};
use crate::{
    FreshnessObservation, FreshnessState, Known, PacketBody, PacketError, PacketResult,
    assess_freshness,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Unoffered,
    /// Dispatched to an agent whose turn ended without a commit (ADR-0092).
    Dispatched,
    /// Dispatched, and the agent's change is a commit on a local branch.
    Submitted,
}

/// Most paths a dispatch record keeps; the rest are counted.
pub const MAX_DISPATCH_PATHS: usize = 200;

/// What one ACP dispatch of a reviewed packet returned (ADR-0092). The
/// host records it; the branch is local and nothing was pushed or merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchRecord {
    /// Review revision, with status reviewed, the packet was dispatched at.
    pub reviewed_revision: u64,
    pub branch: String,
    pub base_revision: String,
    /// The commit holding the agent's change, if the turn ended with
    /// `end_turn` and changed something.
    pub head_revision: Option<String>,
    /// How the turn ended (`end_turn`, `timed_out`, `agent_exited`, ...).
    pub stop: String,
    /// Changed paths, at most [`MAX_DISPATCH_PATHS`].
    pub changed_paths: Vec<String>,
    /// Changed paths left out past the limit.
    pub changed_paths_omitted: u32,
    /// Agent requests the host refused.
    pub refused_requests: u32,
}

impl DispatchRecord {
    fn validate(&self) -> PacketResult<()> {
        let text = |value: &str, maximum: usize| {
            if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
                Err(PacketError::InvalidInput("dispatch record"))
            } else {
                Ok(())
            }
        };
        text(&self.branch, 256)?;
        text(&self.base_revision, 64)?;
        text(&self.stop, 64)?;
        if let Some(head) = &self.head_revision {
            text(head, 64)?;
        }
        if self.changed_paths.len() > MAX_DISPATCH_PATHS {
            return Err(PacketError::LimitExceeded("dispatch paths"));
        }
        for path in &self.changed_paths {
            text(path, 1024)?;
        }
        Ok(())
    }

    fn execution(&self) -> ExecutionState {
        if self.head_revision.is_some() {
            ExecutionState::Submitted
        } else {
            ExecutionState::Dispatched
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Draft,
    Reviewed,
    ChangesRequested,
}
impl ReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Reviewed => "reviewed",
            Self::ChangesRequested => "changes_requested",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Reviewed,
    ChangesRequested,
}
impl From<ReviewDecision> for ReviewStatus {
    fn from(value: ReviewDecision) -> Self {
        match value {
            ReviewDecision::Reviewed => Self::Reviewed,
            ReviewDecision::ChangesRequested => Self::ChangesRequested,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketReview {
    pub revision: u64,
    pub status: ReviewStatus,
    pub note: Option<String>,
}
impl Default for PacketReview {
    fn default() -> Self {
        Self {
            revision: 0,
            status: ReviewStatus::Draft,
            note: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PacketLineage {
    pub predecessor_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketFreshness {
    pub checked_at: Option<String>,
    pub state: FreshnessState,
    pub observed_snapshot_id: Option<String>,
    pub checks: Vec<FreshnessObservation>,
    pub limitations: Vec<String>,
}
impl Default for PacketFreshness {
    fn default() -> Self {
        Self { checked_at:None,state:FreshnessState::Unknown,observed_snapshot_id:None,checks:vec![],limitations:vec!["No live-source assessment has been performed; producer input closure is incomplete".into()] }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketDetail {
    pub packet_id: String,
    pub body: PacketBody,
    pub review: PacketReview,
    pub lineage: PacketLineage,
    pub freshness: PacketFreshness,
    pub execution: ExecutionState,
    /// The latest dispatch, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<DispatchRecord>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketSummary {
    pub packet_id: String,
    pub objective: Known<String>,
    pub review: PacketReview,
    pub execution: ExecutionState,
}

/// Frozen v1 SQLite schema accepted for one-way migration to v2.
const SCHEMA_V1: &[(&str, &str, &str)] = &[
    (
        "table",
        "work_packet_meta",
        "CREATE TABLE work_packet_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1))",
    ),
    (
        "table",
        "work_packet_bodies",
        "CREATE TABLE work_packet_bodies(packet_id TEXT PRIMARY KEY, body BLOB NOT NULL)",
    ),
    (
        "table",
        "work_packet_reviews",
        "CREATE TABLE work_packet_reviews(packet_id TEXT PRIMARY KEY REFERENCES work_packet_bodies(packet_id), revision INTEGER NOT NULL, status TEXT NOT NULL, note TEXT)",
    ),
    (
        "table",
        "work_packet_lineage",
        "CREATE TABLE work_packet_lineage(packet_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id), predecessor_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id), PRIMARY KEY(packet_id,predecessor_id), CHECK(packet_id<>predecessor_id))",
    ),
    (
        "table",
        "work_packet_events",
        "CREATE TABLE work_packet_events(event_id INTEGER PRIMARY KEY,packet_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id),kind TEXT NOT NULL,at TEXT NOT NULL,revision INTEGER,note TEXT)",
    ),
    (
        "table",
        "work_packet_assessments",
        "CREATE TABLE work_packet_assessments(packet_id TEXT PRIMARY KEY REFERENCES work_packet_bodies(packet_id), assessment BLOB NOT NULL)",
    ),
    (
        "trigger",
        "work_packet_body_immutable_update",
        "CREATE TRIGGER work_packet_body_immutable_update BEFORE UPDATE ON work_packet_bodies BEGIN SELECT RAISE(ABORT,'immutable packet body'); END",
    ),
    (
        "trigger",
        "work_packet_body_immutable_delete",
        "CREATE TRIGGER work_packet_body_immutable_delete BEFORE DELETE ON work_packet_bodies BEGIN SELECT RAISE(ABORT,'immutable packet body'); END",
    ),
    (
        "trigger",
        "work_packet_lineage_immutable_update",
        "CREATE TRIGGER work_packet_lineage_immutable_update BEFORE UPDATE ON work_packet_lineage BEGIN SELECT RAISE(ABORT,'immutable packet lineage'); END",
    ),
    (
        "trigger",
        "work_packet_lineage_immutable_delete",
        "CREATE TRIGGER work_packet_lineage_immutable_delete BEFORE DELETE ON work_packet_lineage BEGIN SELECT RAISE(ABORT,'immutable packet lineage'); END",
    ),
    (
        "trigger",
        "work_packet_event_immutable_update",
        "CREATE TRIGGER work_packet_event_immutable_update BEFORE UPDATE ON work_packet_events BEGIN SELECT RAISE(ABORT,'immutable packet event'); END",
    ),
    (
        "trigger",
        "work_packet_event_immutable_delete",
        "CREATE TRIGGER work_packet_event_immutable_delete BEFORE DELETE ON work_packet_events BEGIN SELECT RAISE(ABORT,'immutable packet event'); END",
    ),
];

const META_SCHEMA_V2: &str = "CREATE TABLE work_packet_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=2))";

/// Objects v2 adds to v1. A body insert needs a matching index claim, which
/// only v2 code writes in the same transaction as the body's receipt refs, so
/// a v1 binary that opened the store before migration cannot save a packet
/// whose cited receipts are missing from the index (it fails closed).
const RECEIPT_REFS_SCHEMA: &[(&str, &str, &str)] = &[
    (
        "table",
        "work_packet_receipt_index_claims",
        "CREATE TABLE work_packet_receipt_index_claims(packet_id TEXT PRIMARY KEY) STRICT, WITHOUT ROWID",
    ),
    (
        "trigger",
        "work_packet_body_requires_receipt_index",
        "CREATE TRIGGER work_packet_body_requires_receipt_index BEFORE INSERT ON work_packet_bodies WHEN NOT EXISTS(SELECT 1 FROM work_packet_receipt_index_claims WHERE packet_id=NEW.packet_id) BEGIN SELECT RAISE(ABORT,'packet receipt index required'); END",
    ),
    (
        "trigger",
        "work_packet_receipt_index_claim_immutable_update",
        "CREATE TRIGGER work_packet_receipt_index_claim_immutable_update BEFORE UPDATE ON work_packet_receipt_index_claims BEGIN SELECT RAISE(ABORT,'immutable packet receipt index claim'); END",
    ),
    (
        "trigger",
        "work_packet_receipt_index_claim_immutable_delete",
        "CREATE TRIGGER work_packet_receipt_index_claim_immutable_delete BEFORE DELETE ON work_packet_receipt_index_claims BEGIN SELECT RAISE(ABORT,'immutable packet receipt index claim'); END",
    ),
    (
        "table",
        "work_packet_receipt_refs",
        "CREATE TABLE work_packet_receipt_refs(packet_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id), receipt_id TEXT NOT NULL, PRIMARY KEY(packet_id,receipt_id)) STRICT, WITHOUT ROWID",
    ),
    (
        "index",
        "work_packet_receipt_refs_receipt_id",
        "CREATE INDEX work_packet_receipt_refs_receipt_id ON work_packet_receipt_refs(receipt_id)",
    ),
    (
        "trigger",
        "work_packet_receipt_ref_immutable_update",
        "CREATE TRIGGER work_packet_receipt_ref_immutable_update BEFORE UPDATE ON work_packet_receipt_refs BEGIN SELECT RAISE(ABORT,'immutable packet receipt ref'); END",
    ),
    (
        "trigger",
        "work_packet_receipt_ref_immutable_delete",
        "CREATE TRIGGER work_packet_receipt_ref_immutable_delete BEFORE DELETE ON work_packet_receipt_refs BEGIN SELECT RAISE(ABORT,'immutable packet receipt ref'); END",
    ),
];

const SCHEMA: &[(&str, &str, &str)] = &[
    ("table", "work_packet_meta", META_SCHEMA_V2),
    (
        "table",
        "work_packet_bodies",
        "CREATE TABLE work_packet_bodies(packet_id TEXT PRIMARY KEY, body BLOB NOT NULL)",
    ),
    (
        "table",
        "work_packet_reviews",
        "CREATE TABLE work_packet_reviews(packet_id TEXT PRIMARY KEY REFERENCES work_packet_bodies(packet_id), revision INTEGER NOT NULL, status TEXT NOT NULL, note TEXT)",
    ),
    (
        "table",
        "work_packet_lineage",
        "CREATE TABLE work_packet_lineage(packet_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id), predecessor_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id), PRIMARY KEY(packet_id,predecessor_id), CHECK(packet_id<>predecessor_id))",
    ),
    (
        "table",
        "work_packet_events",
        "CREATE TABLE work_packet_events(event_id INTEGER PRIMARY KEY,packet_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id),kind TEXT NOT NULL,at TEXT NOT NULL,revision INTEGER,note TEXT)",
    ),
    (
        "table",
        "work_packet_assessments",
        "CREATE TABLE work_packet_assessments(packet_id TEXT PRIMARY KEY REFERENCES work_packet_bodies(packet_id), assessment BLOB NOT NULL)",
    ),
    (
        "table",
        "work_packet_receipt_refs",
        "CREATE TABLE work_packet_receipt_refs(packet_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id), receipt_id TEXT NOT NULL, PRIMARY KEY(packet_id,receipt_id)) STRICT, WITHOUT ROWID",
    ),
    (
        "index",
        "work_packet_receipt_refs_receipt_id",
        "CREATE INDEX work_packet_receipt_refs_receipt_id ON work_packet_receipt_refs(receipt_id)",
    ),
    (
        "trigger",
        "work_packet_body_immutable_update",
        "CREATE TRIGGER work_packet_body_immutable_update BEFORE UPDATE ON work_packet_bodies BEGIN SELECT RAISE(ABORT,'immutable packet body'); END",
    ),
    (
        "trigger",
        "work_packet_body_immutable_delete",
        "CREATE TRIGGER work_packet_body_immutable_delete BEFORE DELETE ON work_packet_bodies BEGIN SELECT RAISE(ABORT,'immutable packet body'); END",
    ),
    (
        "trigger",
        "work_packet_lineage_immutable_update",
        "CREATE TRIGGER work_packet_lineage_immutable_update BEFORE UPDATE ON work_packet_lineage BEGIN SELECT RAISE(ABORT,'immutable packet lineage'); END",
    ),
    (
        "trigger",
        "work_packet_lineage_immutable_delete",
        "CREATE TRIGGER work_packet_lineage_immutable_delete BEFORE DELETE ON work_packet_lineage BEGIN SELECT RAISE(ABORT,'immutable packet lineage'); END",
    ),
    (
        "trigger",
        "work_packet_event_immutable_update",
        "CREATE TRIGGER work_packet_event_immutable_update BEFORE UPDATE ON work_packet_events BEGIN SELECT RAISE(ABORT,'immutable packet event'); END",
    ),
    (
        "trigger",
        "work_packet_event_immutable_delete",
        "CREATE TRIGGER work_packet_event_immutable_delete BEFORE DELETE ON work_packet_events BEGIN SELECT RAISE(ABORT,'immutable packet event'); END",
    ),
    (
        "trigger",
        "work_packet_receipt_ref_immutable_update",
        "CREATE TRIGGER work_packet_receipt_ref_immutable_update BEFORE UPDATE ON work_packet_receipt_refs BEGIN SELECT RAISE(ABORT,'immutable packet receipt ref'); END",
    ),
    (
        "trigger",
        "work_packet_receipt_ref_immutable_delete",
        "CREATE TRIGGER work_packet_receipt_ref_immutable_delete BEFORE DELETE ON work_packet_receipt_refs BEGIN SELECT RAISE(ABORT,'immutable packet receipt ref'); END",
    ),
    (
        "table",
        "work_packet_receipt_index_claims",
        "CREATE TABLE work_packet_receipt_index_claims(packet_id TEXT PRIMARY KEY) STRICT, WITHOUT ROWID",
    ),
    (
        "trigger",
        "work_packet_body_requires_receipt_index",
        "CREATE TRIGGER work_packet_body_requires_receipt_index BEFORE INSERT ON work_packet_bodies WHEN NOT EXISTS(SELECT 1 FROM work_packet_receipt_index_claims WHERE packet_id=NEW.packet_id) BEGIN SELECT RAISE(ABORT,'packet receipt index required'); END",
    ),
    (
        "trigger",
        "work_packet_receipt_index_claim_immutable_update",
        "CREATE TRIGGER work_packet_receipt_index_claim_immutable_update BEFORE UPDATE ON work_packet_receipt_index_claims BEGIN SELECT RAISE(ABORT,'immutable packet receipt index claim'); END",
    ),
    (
        "trigger",
        "work_packet_receipt_index_claim_immutable_delete",
        "CREATE TRIGGER work_packet_receipt_index_claim_immutable_delete BEFORE DELETE ON work_packet_receipt_index_claims BEGIN SELECT RAISE(ABORT,'immutable packet receipt index claim'); END",
    ),
];

/// Extract every receipt id cited by fact associations or source excerpts.
fn receipt_ids(body: &PacketBody) -> BTreeSet<String> {
    body.evidence
        .facts
        .iter()
        .filter_map(|fact| {
            fact.source_association
                .value()
                .map(|binding| binding.receipt_id.clone())
        })
        .chain(
            body.evidence
                .source_excerpts
                .iter()
                .map(|excerpt| excerpt.receipt_id.clone()),
        )
        .collect()
}

/// Read owned `sqlite_schema` entries after the caller has bounded their count and lengths.
fn read_schema(
    transaction: &Connection,
    predicate: &str,
) -> PacketResult<std::collections::BTreeMap<String, (String, String)>> {
    let mut statement = transaction.prepare(&format!(
        "SELECT name, type, sql FROM sqlite_schema WHERE {predicate}"
    ))?;
    Ok(statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, String>(1)?, row.get::<_, String>(2)?),
            ))
        })?
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?)
}

/// Compare the owned `sqlite_schema` snapshot against an exact DDL inventory.
fn matches_schema(
    actual: &std::collections::BTreeMap<String, (String, String)>,
    expected: &[(&str, &str, &str)],
) -> bool {
    actual.len() == expected.len()
        && expected.iter().all(|(kind, name, sql)| {
            actual.get(*name).map(|(k, s)| (k.as_str(), s.as_str())) == Some((*kind, *sql))
        })
}

/// Verify the singleton `work_packet_meta` row carries `version`.
fn has_version(transaction: &Connection, version: i64) -> PacketResult<bool> {
    Ok(transaction.query_row(
        "SELECT count(*)=1 AND coalesce(min(typeof(singleton)='integer' AND singleton=1 AND typeof(version)='integer' AND version=?1),0) FROM work_packet_meta",
        [version],
        |row| row.get(0),
    )?)
}

/// Migrate an exact v1 packet store to v2 and backfill receipt references.
fn migrate_v1_to_v2(transaction: &Connection) -> PacketResult<()> {
    for (_, _, sql) in RECEIPT_REFS_SCHEMA {
        transaction.execute_batch(sql)?;
    }
    let invalid_id: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM work_packet_bodies WHERE typeof(packet_id)!='text' OR length(CAST(packet_id AS BLOB))>256)",
        [],
        |r| r.get(0),
    )?;
    if invalid_id {
        return Err(PacketError::InvalidInput("packet identity"));
    }
    let mut after: Option<String> = None;
    while let Some(id) = transaction
        .query_row(
            "SELECT packet_id FROM work_packet_bodies WHERE (?1 IS NULL OR packet_id>?1) ORDER BY packet_id LIMIT 1",
            [after.as_deref()],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        let body = load_body(transaction, &id)?;
        transaction.execute(
            "INSERT INTO work_packet_receipt_index_claims(packet_id) VALUES(?1)",
            [&id],
        )?;
        for receipt_id in receipt_ids(&body) {
            transaction.execute(
                "INSERT OR IGNORE INTO work_packet_receipt_refs(packet_id,receipt_id) VALUES(?1,?2)",
                params![&id, receipt_id],
            )?;
        }
        after = Some(id);
    }
    transaction.execute_batch("DROP TABLE work_packet_meta")?;
    transaction.execute_batch(META_SCHEMA_V2)?;
    transaction.execute("INSERT INTO work_packet_meta VALUES(1,2)", [])?;
    Ok(())
}

fn admit_schema(connection: &mut Connection) -> PacketResult<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Include extra triggers/indexes on owned tables, regardless of their names.
    let predicate =
        "name GLOB 'work_packet_*' OR (tbl_name GLOB 'work_packet_*' AND name NOT GLOB 'sqlite_*')";
    let (count, bounded): (i64, bool) = transaction.query_row(
        &format!("SELECT count(*), coalesce(min(typeof(name)='text' AND length(CAST(name AS BLOB))<=256 AND typeof(type)='text' AND length(CAST(type AS BLOB))<=16 AND typeof(sql)='text' AND length(CAST(sql AS BLOB))<=4096), 1) FROM sqlite_schema WHERE {predicate}"),
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let incompatible = || PacketError::InvalidInput("incompatible packet store schema");
    let count_usize = usize::try_from(count).ok();
    if count != 0
        && (!bounded || (count_usize != Some(SCHEMA.len()) && count_usize != Some(SCHEMA_V1.len())))
    {
        return Err(incompatible());
    }
    if count == 0 {
        for (_, _, sql) in SCHEMA {
            transaction.execute_batch(sql)?;
        }
        transaction.execute("INSERT INTO work_packet_meta VALUES(1,2)", [])?;
    } else if count_usize == Some(SCHEMA_V1.len()) {
        let actual = read_schema(&transaction, predicate)?;
        if !matches_schema(&actual, SCHEMA_V1) || !has_version(&transaction, 1)? {
            return Err(incompatible());
        }
        migrate_v1_to_v2(&transaction)?;
    }
    let actual = read_schema(&transaction, predicate)?;
    if !matches_schema(&actual, SCHEMA) || !has_version(&transaction, 2)? {
        return Err(incompatible());
    }
    transaction.commit()?;
    Ok(())
}

/// How long a packet-store connection waits for a competing writer's lock;
/// every Cartograph SQLite store uses the same 5 s (AC-0823, #1653).
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub struct PacketStore {
    connection: Connection,
}
impl PacketStore {
    pub fn open(path: impl AsRef<Path>) -> PacketResult<Self> {
        let connection = Connection::open(path)?;
        Self::initialize(connection)
    }
    pub fn in_memory() -> PacketResult<Self> {
        Self::initialize(Connection::open_in_memory()?)
    }
    fn initialize(mut connection: Connection) -> PacketResult<Self> {
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.execute_batch("PRAGMA foreign_keys=ON;")?;
        admit_schema(&mut connection)?;
        Ok(Self { connection })
    }
    /// Admit the entire get envelope before committing any body or metadata.
    /// Reusing a body preserves its existing review and assessment.
    pub fn put(
        &mut self,
        mut body: PacketBody,
        predecessor_ids: &[String],
        created_at: &str,
    ) -> PacketResult<PacketDetail> {
        metadata_text(created_at, 128)?;
        if predecessor_ids.len() > 1024 {
            return Err(PacketError::LimitExceeded("lineage"));
        }
        body.normalize()?;
        let canonical = body.canonical_bytes()?;
        let id = packet_id(&canonical);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_packet_bodies WHERE packet_id=?1)",
            [&id],
            |r| r.get(0),
        )?;
        let mut detail = if exists {
            load_detail(&transaction, &id)?
        } else {
            PacketDetail {
                packet_id: id.clone(),
                body,
                review: PacketReview::default(),
                lineage: PacketLineage::default(),
                freshness: PacketFreshness::default(),
                execution: ExecutionState::Unoffered,
                dispatch: None,
            }
        };
        if exists && detail.body.canonical_bytes()? != canonical {
            return Err(PacketError::InvalidInput("stored identity collision"));
        }
        let mut predecessors: BTreeSet<String> =
            detail.lineage.predecessor_ids.iter().cloned().collect();
        for predecessor in predecessor_ids {
            if !valid_packet_id(predecessor) {
                return Err(PacketError::InvalidInput("predecessor identity"));
            }
            if predecessor == &id {
                return Err(PacketError::InvalidInput("self lineage"));
            }
            // Verify every referenced saved body's schema and identity, too.
            load_detail(&transaction, predecessor)?;
            check_ancestors(&transaction, predecessor, &id)?;
            predecessors.insert(predecessor.clone());
        }
        if predecessors.len() > 1024 {
            return Err(PacketError::LimitExceeded("lineage"));
        }
        detail.lineage.predecessor_ids = predecessors.into_iter().collect();
        check_combined_ancestors(&transaction, &detail.lineage.predecessor_ids, &id)?;
        validate_detail(&detail)?;
        if !exists {
            transaction.execute(
                "INSERT INTO work_packet_receipt_index_claims(packet_id) VALUES(?1)",
                [&id],
            )?;
            transaction.execute(
                "INSERT INTO work_packet_bodies(packet_id,body) VALUES(?1,?2)",
                params![id, canonical],
            )?;
            for receipt_id in receipt_ids(&detail.body) {
                transaction.execute(
                    "INSERT OR IGNORE INTO work_packet_receipt_refs(packet_id,receipt_id) VALUES(?1,?2)",
                    params![id, receipt_id],
                )?;
            }
            transaction.execute("INSERT INTO work_packet_reviews(packet_id,revision,status,note) VALUES(?1,0,'draft',NULL)",[&id])?;
        }
        for predecessor in &detail.lineage.predecessor_ids {
            transaction.execute(
                "INSERT OR IGNORE INTO work_packet_lineage(packet_id,predecessor_id) VALUES(?1,?2)",
                params![id, predecessor],
            )?;
        }
        transaction.execute(
            "INSERT INTO work_packet_events(packet_id,kind,at) VALUES(?1,'generated',?2)",
            params![id, created_at],
        )?;
        transaction.commit()?;
        Ok(detail)
    }
    pub fn get(&self, id: &str) -> PacketResult<PacketDetail> {
        let transaction = self.connection.unchecked_transaction()?;
        let detail = load_detail(&transaction, id)?;
        check_combined_ancestors(&transaction, &detail.lineage.predecessor_ids, id)?;
        transaction.commit()?;
        Ok(detail)
    }
    /// Bounded stable page ordered by packet identity. No body is regenerated.
    pub fn list(&self, limit: usize) -> PacketResult<Vec<PacketSummary>> {
        self.list_after(limit, None)
    }
    pub fn list_after(
        &self,
        limit: usize,
        after: Option<&str>,
    ) -> PacketResult<Vec<PacketSummary>> {
        if limit == 0 || limit > 50 {
            return Err(PacketError::LimitExceeded("list page count"));
        }
        if after.is_some_and(|s| !valid_packet_id(s)) {
            return Err(PacketError::InvalidInput("list cursor"));
        }
        let transaction = self.connection.unchecked_transaction()?;
        let mut statement=transaction.prepare("SELECT packet_id FROM work_packet_bodies WHERE packet_id>?1 ORDER BY packet_id LIMIT ?2")?;
        let ids = statement
            .query_map(params![after.unwrap_or(""), limit as i64], |row| {
                read_id(row, 0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut summaries = Vec::new();
        let mut remaining: usize = 64 * 1024 - 2;
        for id in ids {
            let detail = load_detail(&transaction, &id)?;
            let summary = PacketSummary {
                packet_id: id,
                objective: detail.body.intent.objective,
                review: detail.review,
                execution: detail.execution,
            };
            let comma = usize::from(!summaries.is_empty());
            let budget = remaining
                .checked_sub(comma)
                .ok_or(PacketError::LimitExceeded("list bytes"))?;
            remaining = budget - bounded_json(&summary, budget)?.len();
            summaries.push(summary);
        }
        transaction.commit()?;
        Ok(summaries)
    }
    pub fn review(
        &mut self,
        id: &str,
        expected_revision: u64,
        decision: ReviewDecision,
        note: Option<String>,
        at: &str,
    ) -> PacketResult<PacketDetail> {
        metadata_text(at, 128)?;
        if let Some(note) = &note
            && note.len() > 8192
        {
            return Err(PacketError::LimitExceeded("review note"));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut detail = load_detail(&transaction, id)?;
        if detail.review.revision != expected_revision {
            return Err(PacketError::ReviewConflict {
                revision: detail.review.revision,
                status: detail.review.status.as_str().into(),
            });
        }
        let revision = expected_revision
            .checked_add(1)
            .filter(|r| *r <= i64::MAX as u64)
            .ok_or(PacketError::LimitExceeded("review revision"))?;
        let status = ReviewStatus::from(decision);
        detail.review = PacketReview {
            revision,
            status,
            note: note.clone(),
        };
        validate_detail(&detail)?;
        let changed=transaction.execute("UPDATE work_packet_reviews SET revision=?1,status=?2,note=?3 WHERE packet_id=?4 AND revision=?5",params![revision as i64,status.as_str(),note,id,expected_revision as i64])?;
        if changed != 1 {
            return Err(PacketError::ReviewConflict {
                revision: expected_revision,
                status: status.as_str().into(),
            });
        }
        transaction.execute("INSERT INTO work_packet_events(packet_id,kind,at,revision,note) VALUES(?1,?2,?3,?4,?5)",params![id,status.as_str(),at,revision as i64,note])?;
        transaction.commit()?;
        Ok(detail)
    }
    /// Records what an ACP dispatch returned (ADR-0092). The packet must have
    /// been reviewed at `record.reviewed_revision`; a later review does not
    /// undo a dispatch that already ran. Records are append-only events and
    /// the latest one sets the packet's execution state.
    pub fn record_dispatch(
        &mut self,
        id: &str,
        record: &DispatchRecord,
        at: &str,
    ) -> PacketResult<PacketDetail> {
        metadata_text(at, 128)?;
        record.validate()?;
        let note = bounded_json(record, 64 * 1024)?;
        let note =
            String::from_utf8(note).map_err(|_| PacketError::InvalidInput("dispatch record"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reviewed_revision = i64::try_from(record.reviewed_revision)
            .map_err(|_| PacketError::InvalidInput("dispatch revision"))?;
        let reviewed: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM work_packet_events WHERE packet_id=?1 AND kind='reviewed' AND revision=?2)",
            params![id, reviewed_revision],
            |r| r.get(0),
        )?;
        if !reviewed {
            return Err(PacketError::InvalidInput(
                "dispatch of an unreviewed packet",
            ));
        }
        transaction.execute(
            "INSERT INTO work_packet_events(packet_id,kind,at,revision,note) VALUES(?1,'dispatched',?2,?3,?4)",
            params![id, at, reviewed_revision, note],
        )?;
        let detail = load_detail(&transaction, id)?;
        transaction.commit()?;
        Ok(detail)
    }
    /// Host observations are reduced here; a caller cannot assert a conflicting
    /// stronger state. The host remains responsible for complete input closure.
    pub fn record_assessment(
        &mut self,
        id: &str,
        mut assessment: PacketFreshness,
    ) -> PacketResult<PacketDetail> {
        assessment.state = assess_freshness(&assessment.checks);
        validate_assessment(&assessment)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut detail = load_detail(&transaction, id)?;
        detail.freshness = assessment;
        validate_detail(&detail)?;
        let bytes = bounded_json(&detail.freshness, 64 * 1024)?;
        transaction.execute("INSERT INTO work_packet_assessments(packet_id,assessment) VALUES(?1,?2) ON CONFLICT(packet_id) DO UPDATE SET assessment=excluded.assessment",params![id,bytes])?;
        transaction.execute(
            "INSERT INTO work_packet_events(packet_id,kind,at) VALUES(?1,'assessed',?2)",
            params![id, detail.freshness.checked_at],
        )?;
        transaction.commit()?;
        Ok(detail)
    }
    /// Return the subset of `candidates` referenced by any saved packet body.
    pub fn referenced_receipts(&self, candidates: &[String]) -> PacketResult<BTreeSet<String>> {
        let json = serde_json::to_string(candidates)
            .map_err(|_| PacketError::InvalidInput("receipt candidates"))?;
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT r.receipt_id FROM work_packet_receipt_refs r JOIN json_each(?1) c ON r.receipt_id = c.value",
        )?;
        let mut rows = statement.query([&json])?;
        let mut referenced = BTreeSet::new();
        while let Some(row) = rows.next()? {
            if referenced.len() >= candidates.len() {
                return Err(PacketError::InvalidInput("receipt reference"));
            }
            let id = match row.get_ref(0)? {
                rusqlite::types::ValueRef::Text(bytes)
                    if !bytes.is_empty() && bytes.len() <= 256 =>
                {
                    std::str::from_utf8(bytes)
                        .map_err(|_| PacketError::InvalidInput("receipt reference"))?
                        .to_owned()
                }
                _ => return Err(PacketError::InvalidInput("receipt reference")),
            };
            referenced.insert(id);
        }
        Ok(referenced)
    }
}
fn metadata_text(value: &str, maximum: usize) -> PacketResult<()> {
    if value.trim().is_empty() {
        return Err(PacketError::InvalidInput("metadata text"));
    }
    if value.len() > maximum {
        return Err(PacketError::LimitExceeded("metadata text"));
    }
    Ok(())
}
fn validate_assessment(value: &PacketFreshness) -> PacketResult<()> {
    if value.checks.len() > 1024 || value.limitations.len() > 64 {
        return Err(PacketError::LimitExceeded("assessment"));
    }
    if let Some(at) = &value.checked_at {
        metadata_text(at, 128)?;
    } else if !value.checks.is_empty()
        || value.observed_snapshot_id.is_some()
        || value.state != FreshnessState::Unknown
    {
        return Err(PacketError::InvalidInput("assessment time"));
    }
    if let Some(id) = &value.observed_snapshot_id
        && !id.strip_prefix("context-v1:").is_some_and(|h| {
            h.len() == 64
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err(PacketError::InvalidInput("assessment snapshot"));
    }
    for check in &value.checks {
        metadata_text(&check.subject, 4096)?;
    }
    for limitation in &value.limitations {
        metadata_text(limitation, 4096)?;
    }
    if value.state != assess_freshness(&value.checks) {
        return Err(PacketError::InvalidInput("assessment reduction"));
    }
    bounded_json(value, 64 * 1024)?;
    Ok(())
}
pub fn validate_detail(detail: &PacketDetail) -> PacketResult<()> {
    if detail.packet_id != detail.body.content_id()? {
        return Err(PacketError::InvalidInput("packet identity"));
    }
    if detail.review.revision > i64::MAX as u64
        || detail.review.note.as_ref().is_some_and(|n| n.len() > 8192)
    {
        return Err(PacketError::LimitExceeded("review"));
    }
    if detail.lineage.predecessor_ids.len() > 1024
        || detail
            .lineage
            .predecessor_ids
            .iter()
            .any(|id| !valid_packet_id(id) || id == &detail.packet_id)
    {
        return Err(PacketError::InvalidInput("lineage"));
    }
    validate_assessment(&detail.freshness)?;
    bounded_json(detail, MAX_RESPONSE_BYTES)?;
    Ok(())
}
/// Load one stored packet body with a bounded length precheck and canonical-identity verification.
fn load_body(connection: &Connection, id: &str) -> PacketResult<PacketBody> {
    if !valid_packet_id(id) {
        return Err(PacketError::InvalidInput("packet identity"));
    }
    let length: Option<Option<i64>> = connection
        .query_row(
            "SELECT length(CAST(body AS BLOB)) FROM work_packet_bodies WHERE packet_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let length = length
        .ok_or(PacketError::NotFound)?
        .ok_or(PacketError::InvalidInput("stored body"))?;
    if length > MAX_BODY_BYTES as i64 {
        return Err(PacketError::LimitExceeded("stored body"));
    }
    let bytes: Vec<u8> = connection.query_row(
        "SELECT CAST(body AS BLOB) FROM work_packet_bodies WHERE packet_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let body = PacketBody::from_json(&bytes)?;
    if body.canonical_bytes()? != bytes || packet_id(&bytes) != id {
        return Err(PacketError::InvalidInput("stored canonical identity"));
    }
    Ok(body)
}

fn load_detail(connection: &Connection, id: &str) -> PacketResult<PacketDetail> {
    let body = load_body(connection, id)?;
    let (revision, status, note_length): (i64, ReviewStatus, Option<i64>) = connection.query_row(
        "SELECT revision,status,length(CAST(note AS BLOB)) FROM work_packet_reviews WHERE packet_id=?1",
        [id],
        |r| {let status=match r.get_ref(1)?.as_str()? {"draft"=>ReviewStatus::Draft,"reviewed"=>ReviewStatus::Reviewed,"changes_requested"=>ReviewStatus::ChangesRequested,_=>return Err(rusqlite::Error::InvalidQuery)};Ok((r.get(0)?,status,r.get(2)?))},
    )?;
    if note_length.is_some_and(|n| n > 8192) {
        return Err(PacketError::LimitExceeded("stored review note"));
    }
    let note: Option<String> = connection.query_row(
        "SELECT note FROM work_packet_reviews WHERE packet_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let mut statement=connection.prepare("SELECT predecessor_id FROM work_packet_lineage WHERE packet_id=?1 ORDER BY predecessor_id LIMIT 1025")?;
    let predecessor_ids = statement
        .query_map([id], |r| read_id(r, 0))?
        .collect::<Result<Vec<_>, _>>()?;
    if predecessor_ids.len() > 1024 {
        return Err(PacketError::LimitExceeded("stored lineage"));
    }
    let assessment_length: Option<i64> = connection
        .query_row(
            "SELECT length(CAST(assessment AS BLOB)) FROM work_packet_assessments WHERE packet_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let freshness = if let Some(length) = assessment_length {
        if length > 64 * 1024 {
            return Err(PacketError::LimitExceeded("stored assessment"));
        }
        let bytes: Vec<u8> = connection.query_row(
            "SELECT assessment FROM work_packet_assessments WHERE packet_id=?1",
            [id],
            |r| r.get(0),
        )?;
        serde_json::from_slice(&bytes)
            .map_err(|_| PacketError::InvalidInput("stored assessment"))?
    } else {
        PacketFreshness::default()
    };
    let dispatch = load_dispatch(connection, id)?;
    let detail = PacketDetail {
        packet_id: id.into(),
        body,
        review: PacketReview {
            revision: u64::try_from(revision)
                .map_err(|_| PacketError::InvalidInput("stored review revision"))?,
            status,
            note,
        },
        lineage: PacketLineage { predecessor_ids },
        freshness,
        execution: dispatch
            .as_ref()
            .map_or(ExecutionState::Unoffered, DispatchRecord::execution),
        dispatch,
    };
    validate_detail(&detail)?;
    Ok(detail)
}
/// The latest dispatch record of a packet, if any.
fn load_dispatch(connection: &Connection, id: &str) -> PacketResult<Option<DispatchRecord>> {
    let latest: Option<(i64, Option<i64>)> = connection
        .query_row(
            "SELECT event_id,length(CAST(note AS BLOB)) FROM work_packet_events WHERE packet_id=?1 AND kind='dispatched' ORDER BY event_id DESC LIMIT 1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((event_id, length)) = latest else {
        return Ok(None);
    };
    if length.is_none_or(|n| n > 64 * 1024) {
        return Err(PacketError::LimitExceeded("stored dispatch record"));
    }
    let note: Option<String> = connection.query_row(
        "SELECT note FROM work_packet_events WHERE event_id=?1",
        [event_id],
        |r| r.get(0),
    )?;
    let record: DispatchRecord = note
        .as_deref()
        .and_then(|note| serde_json::from_str(note).ok())
        .ok_or(PacketError::InvalidInput("stored dispatch record"))?;
    record.validate()?;
    Ok(Some(record))
}
fn check_ancestors(connection: &Connection, root: &str, forbidden: &str) -> PacketResult<()> {
    check_combined_ancestors(connection, &[root.into()], forbidden)
}
fn check_combined_ancestors(
    connection: &Connection,
    roots: &[String],
    forbidden: &str,
) -> PacketResult<()> {
    // Walk with one SQL cursor per active node, rather than retaining every
    // fan-out edge. Shared ancestors count once; active-path repeats are cycles.
    let mut seen = BTreeSet::new();
    let mut active = BTreeSet::new();
    let mut finished = BTreeSet::new();
    for root in roots {
        if !valid_packet_id(root) {
            return Err(PacketError::InvalidInput("lineage identity"));
        }
        if root == forbidden {
            return Err(PacketError::InvalidInput("lineage cycle"));
        }
        if finished.contains(root) {
            continue;
        }
        seen.insert(root.clone());
        if seen.len() > 1024 {
            return Err(PacketError::LimitExceeded("lineage ancestors"));
        }
        active.insert(root.clone());
        let mut stack = vec![(root.clone(), String::new())];
        while let Some((node, cursor)) = stack.last_mut() {
            let child: Option<String> = connection.query_row(
                "SELECT predecessor_id FROM work_packet_lineage WHERE packet_id=?1 AND predecessor_id>?2 ORDER BY predecessor_id LIMIT 1",
                params![node.as_str(),cursor.as_str()], |r| read_id(r,0)).optional()?;
            let Some(child) = child else {
                let (done, _) = stack.pop().expect("active lineage frame");
                active.remove(&done);
                finished.insert(done);
                continue;
            };
            *cursor = child.clone();
            if child == forbidden || active.contains(&child) {
                return Err(PacketError::InvalidInput("lineage cycle"));
            }
            if finished.contains(&child) {
                continue;
            }
            seen.insert(child.clone());
            if seen.len() > 1024 {
                return Err(PacketError::LimitExceeded("lineage ancestors"));
            }
            active.insert(child.clone());
            stack.push((child, String::new()));
        }
    }
    Ok(())
}
fn read_id(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<String> {
    let value = row.get_ref(index)?.as_str()?;
    if !valid_packet_id(value) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
#[path = "packet_tests.rs"]
mod tests;

#[cfg(test)]
mod schema_tests {
    use super::*;

    // T-0301: v2 is exactly v1 with the v2 meta table plus the receipt-ref
    // objects, so the hand-written inventories cannot drift apart.
    #[test]
    fn v2_schema_is_v1_plus_receipt_refs() {
        let mut expected: Vec<_> = SCHEMA_V1
            .iter()
            .map(|(kind, name, sql)| {
                if *name == "work_packet_meta" {
                    (*kind, *name, META_SCHEMA_V2)
                } else {
                    (*kind, *name, *sql)
                }
            })
            .chain(RECEIPT_REFS_SCHEMA.iter().copied())
            .collect();
        let mut actual = SCHEMA.to_vec();
        expected.sort_unstable();
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }
}
