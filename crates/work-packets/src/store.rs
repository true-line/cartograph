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
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketSummary {
    pub packet_id: String,
    pub objective: Known<String>,
    pub review: PacketReview,
    pub execution: ExecutionState,
}

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
    fn initialize(connection: Connection) -> PacketResult<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS work_packet_bodies(packet_id TEXT PRIMARY KEY, body BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS work_packet_reviews(packet_id TEXT PRIMARY KEY REFERENCES work_packet_bodies(packet_id), revision INTEGER NOT NULL, status TEXT NOT NULL, note TEXT);
            CREATE TABLE IF NOT EXISTS work_packet_lineage(packet_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id), predecessor_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id), PRIMARY KEY(packet_id,predecessor_id), CHECK(packet_id<>predecessor_id));
            CREATE TABLE IF NOT EXISTS work_packet_events(event_id INTEGER PRIMARY KEY,packet_id TEXT NOT NULL REFERENCES work_packet_bodies(packet_id),kind TEXT NOT NULL,at TEXT NOT NULL,revision INTEGER,note TEXT);
            CREATE TABLE IF NOT EXISTS work_packet_assessments(packet_id TEXT PRIMARY KEY REFERENCES work_packet_bodies(packet_id), assessment BLOB NOT NULL);
            CREATE TRIGGER IF NOT EXISTS work_packet_body_immutable_update BEFORE UPDATE ON work_packet_bodies BEGIN SELECT RAISE(ABORT,'immutable packet body'); END;
            CREATE TRIGGER IF NOT EXISTS work_packet_body_immutable_delete BEFORE DELETE ON work_packet_bodies BEGIN SELECT RAISE(ABORT,'immutable packet body'); END;
            CREATE TRIGGER IF NOT EXISTS work_packet_lineage_immutable_update BEFORE UPDATE ON work_packet_lineage BEGIN SELECT RAISE(ABORT,'immutable packet lineage'); END;
            CREATE TRIGGER IF NOT EXISTS work_packet_lineage_immutable_delete BEFORE DELETE ON work_packet_lineage BEGIN SELECT RAISE(ABORT,'immutable packet lineage'); END;")?;
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
                "INSERT INTO work_packet_bodies(packet_id,body) VALUES(?1,?2)",
                params![id, canonical],
            )?;
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
                execution: ExecutionState::Unoffered,
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
fn load_detail(connection: &Connection, id: &str) -> PacketResult<PacketDetail> {
    if !valid_packet_id(id) {
        return Err(PacketError::InvalidInput("packet identity"));
    }
    let length: Option<i64> = connection
        .query_row(
            "SELECT length(CAST(body AS BLOB)) FROM work_packet_bodies WHERE packet_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let length = length.ok_or(PacketError::NotFound)?;
    if length > MAX_BODY_BYTES as i64 {
        return Err(PacketError::LimitExceeded("stored body"));
    }
    let bytes: Vec<u8> = connection.query_row(
        "SELECT body FROM work_packet_bodies WHERE packet_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let body = PacketBody::from_json(&bytes)?;
    if body.canonical_bytes()? != bytes || packet_id(&bytes) != id {
        return Err(PacketError::InvalidInput("stored canonical identity"));
    }
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
        execution: ExecutionState::Unoffered,
    };
    validate_detail(&detail)?;
    Ok(detail)
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
