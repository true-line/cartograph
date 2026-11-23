//! Durable section drafts and their review (#1748, SPEC-15 slice 3, ADR-0064
//! decisions 5 and 6, AC-0665).
//!
//! A draft is its own record kind in the review store: its own table beside
//! the staged proposals, in the same file. It is not an edge proposal and
//! shares neither that table nor its decision type. This module reads no
//! graph and no source, calls no model and writes no fact; accepting a draft
//! changes no tier (R-INT-1, R-INT-3).
//!
//! **Identity.** A draft's id is a digest over its complete immutable
//! content: the address, the claims, the cited fact hashes, the author role
//! and its definition, the producing task, the graph content hash it was
//! drafted against, and the returned draft it replaces, if any. Storing the
//! same content again returns the stored record, reviewed or not.
//!
//! **Review.** A review names a draft id, the revision it read and a
//! decision. It is one immediate transaction that fails on any other
//! revision, and it never changes the content. A trigger refuses an update
//! of the content columns from any caller.
//!
//! **Successors.** A returned draft admits exactly one successor for the same
//! address; a unique index holds that under concurrent writers. A successor
//! cannot be returned.
//!
//! **Bounds.** The immutable payload is at most [`MAX_DRAFT_RECORD_BYTES`]
//! and a review note at most [`MAX_DRAFT_NOTE_BYTES`]. A page holds at most
//! [`MAX_DRAFT_PAGE_ITEMS`] records. Every operation reads or writes a
//! bounded number of rows.

use crate::section_draft::{self, DraftRefusal, SectionDraft, SectionDraftFinish};
use core_prov::content_hash;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Version of the immutable draft envelope.
pub const DRAFT_SCHEMA_VERSION: u32 = 1;
/// Most bytes of one draft's immutable payload.
pub const MAX_DRAFT_RECORD_BYTES: usize = 256 * 1024;
/// Most bytes of one review note.
pub const MAX_DRAFT_NOTE_BYTES: usize = 4 * 1024;
/// Most records one page returns.
pub const MAX_DRAFT_PAGE_ITEMS: usize = 50;
const MAX_ORIGIN_TEXT_BYTES: usize = 256;
const IDENTITY_DOMAIN: &[u8] = b"cartograph.section-draft.v1\0";

/// Who produced a draft, and against what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftOrigin {
    /// The author role, such as `section-writer@1`.
    pub role: String,
    /// The fingerprint of that role's definition when it ran.
    pub role_definition: String,
    /// The durable job that produced the draft.
    pub job_id: i64,
    /// The graph content hash the draft was drafted against.
    pub graph_content_hash: String,
}

/// A human decision on a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftDecision {
    /// The draft may be shown, if the curated projection finds it eligible.
    /// It stays T3 / InferredWeak.
    Accepted,
    /// The draft is not shown.
    Rejected,
    /// The draft is sent back; one successor may replace it.
    Returned,
}

impl DraftDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Returned => "returned",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "returned" => Some(Self::Returned),
            _ => None,
        }
    }
}

/// A stored draft with its current review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredDraft {
    /// Version of the immutable envelope.
    pub schema_version: u32,
    /// Digest of the immutable content.
    pub draft_id: String,
    /// The admitted draft: address, claims and cited hashes.
    pub draft: SectionDraft,
    /// Who produced it, and against what.
    pub origin: DraftOrigin,
    /// The returned draft this one replaces.
    pub replaces: Option<String>,
    /// Compare-and-set review revision; zero means never reviewed.
    pub review_revision: u64,
    /// The current decision, if any.
    pub review_decision: Option<DraftDecision>,
    /// The reviewer's note, if any.
    pub review_note: Option<String>,
    /// UTC creation time; not part of the identity.
    pub created_at: String,
    /// UTC time of the last review; not part of the identity.
    pub reviewed_at: Option<String>,
}

/// One page of stored drafts, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPage {
    /// The records of this page.
    pub items: Vec<StoredDraft>,
    /// Pass as `before` to read the next page; `None` when this is the last.
    pub next_before: Option<u64>,
}

/// Why a draft was not stored, read or reviewed.
#[derive(Debug, thiserror::Error)]
pub enum DraftStoreError {
    /// The draft does not pass the admission rule it must already have passed.
    #[error("draft is not admissible: {}", .0.code())]
    InvalidDraft(DraftRefusal),
    /// The draft records a hash for a fact no claim cites, or an unusable hash.
    #[error("draft cited hashes do not match its citations")]
    CitedHashes,
    /// The origin is missing a field or holds an unusable one.
    #[error("invalid draft origin: {0}")]
    InvalidOrigin(&'static str),
    /// The immutable payload exceeds the bound.
    #[error("draft exceeds {MAX_DRAFT_RECORD_BYTES} bytes ({bytes} bytes)")]
    RecordTooLarge {
        /// Measured serialized bytes.
        bytes: usize,
    },
    /// The review note exceeds the bound.
    #[error("review note exceeds {MAX_DRAFT_NOTE_BYTES} bytes")]
    NoteTooLarge,
    /// No stored draft has this id.
    #[error("unknown draft")]
    UnknownDraft,
    /// Another review changed the draft first.
    #[error("stale review revision: expected {expected}, current {actual}")]
    StaleReviewRevision {
        /// Revision the reviewer read.
        expected: u64,
        /// Revision now stored.
        actual: u64,
    },
    /// The draft named as replaced is not currently returned.
    #[error("the replaced draft is neither returned nor accepted")]
    PredecessorNotReturned,
    /// The draft named as replaced is for another address.
    #[error("the replaced draft is for another address")]
    PredecessorAddress,
    /// The returned draft already has its one successor.
    #[error("the replaced draft already has a successor")]
    SuccessorExists,
    /// A successor cannot be returned.
    #[error("a successor draft cannot be returned")]
    SuccessorCannotBeReturned,
    /// A page limit outside 1..=[`MAX_DRAFT_PAGE_ITEMS`].
    #[error("draft page limit must be between 1 and {MAX_DRAFT_PAGE_ITEMS}")]
    InvalidLimit,
    /// A stored row failed its version, integrity or structure check.
    #[error("invalid stored draft")]
    InvalidRecord,
    /// SQLite failed; nothing is reported as stored or reviewed.
    #[error("draft store: {0}")]
    Storage(#[from] rusqlite::Error),
    /// Serialization failed before anything was written.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftContent {
    schema_version: u32,
    draft: SectionDraft,
    origin: DraftOrigin,
    replaces: Option<String>,
}

impl DraftContent {
    fn identity(json: &str) -> String {
        let mut bytes = IDENTITY_DOMAIN.to_vec();
        bytes.extend_from_slice(json.as_bytes());
        content_hash(&bytes)
    }
}

fn plain(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_ORIGIN_TEXT_BYTES
        && !value.chars().any(char::is_control)
}

fn validate(draft: &SectionDraft, origin: &DraftOrigin) -> Result<(), DraftStoreError> {
    // The stored draft must be exactly what admission yields from its own
    // claims: every bound holds and a hash is recorded for each cited id
    // and for no other.
    let admitted = section_draft::admit(
        &draft.address,
        &draft.cited_hashes,
        SectionDraftFinish {
            address: draft.address.clone(),
            claims: draft.claims.clone(),
        },
    )
    .map_err(DraftStoreError::InvalidDraft)?;
    if admitted != *draft || !draft.cited_hashes.values().all(|hash| plain(hash)) {
        return Err(DraftStoreError::CitedHashes);
    }
    let address = &draft.address;
    if ![
        &address.catalogue_version,
        &address.document,
        &address.section,
        &address.scope,
    ]
    .into_iter()
    .all(|part| plain(part))
    {
        return Err(DraftStoreError::InvalidDraft(DraftRefusal::AddressMismatch));
    }
    if !plain(&origin.role) {
        return Err(DraftStoreError::InvalidOrigin("role"));
    }
    if !plain(&origin.role_definition) {
        return Err(DraftStoreError::InvalidOrigin("role definition"));
    }
    if origin.job_id <= 0 {
        return Err(DraftStoreError::InvalidOrigin("job"));
    }
    if !plain(&origin.graph_content_hash) {
        return Err(DraftStoreError::InvalidOrigin("graph content hash"));
    }
    Ok(())
}

/// SQLite store of section drafts, in the review store's file.
pub struct DraftStore {
    conn: Connection,
}

const SELECT: &str = "SELECT sequence, draft_id, immutable_json, replaces, review_revision, \
                      review_decision, review_note, created_at, reviewed_at \
                      FROM staged_section_drafts";

type Row = (
    i64,
    String,
    String,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn decode(row: Row) -> Result<(u64, StoredDraft), DraftStoreError> {
    let (sequence, draft_id, json, replaces, revision, decision, note, created_at, reviewed_at) =
        row;
    let content: DraftContent =
        serde_json::from_str(&json).map_err(|_| DraftStoreError::InvalidRecord)?;
    let review_decision = match decision {
        Some(value) => Some(DraftDecision::parse(&value).ok_or(DraftStoreError::InvalidRecord)?),
        None => None,
    };
    if content.schema_version != DRAFT_SCHEMA_VERSION
        || DraftContent::identity(&json) != draft_id
        || content.replaces != replaces
        || sequence <= 0
        || revision < 0
    {
        return Err(DraftStoreError::InvalidRecord);
    }
    Ok((
        sequence as u64,
        StoredDraft {
            schema_version: content.schema_version,
            draft_id,
            draft: content.draft,
            origin: content.origin,
            replaces: content.replaces,
            review_revision: revision as u64,
            review_decision,
            review_note: note,
            created_at,
            reviewed_at,
        },
    ))
}

fn get_on(conn: &Connection, draft_id: &str) -> Result<Option<StoredDraft>, DraftStoreError> {
    conn.query_row(
        &format!("{SELECT} WHERE draft_id = ?1"),
        [draft_id],
        read_row,
    )
    .optional()?
    .map(|row| decode(row).map(|(_, draft)| draft))
    .transpose()
}

const SUCCESSOR_COLUMNS: &str = "sequence, draft_id, immutable_json, replaces, review_revision, \
     review_decision, review_note, created_at, reviewed_at";

/// Rebuild a draft table created when `replaces` was unique for every
/// successor (before ADR-0045), keeping every row and its sequence. The
/// partial index created on open then holds the one-successor rule.
fn widen_successors(conn: &mut Connection) -> Result<(), DraftStoreError> {
    let created: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'staged_section_drafts'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if !created.is_some_and(|sql| sql.contains("replaces TEXT UNIQUE")) {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(&format!(
        "DROP TRIGGER IF EXISTS staged_section_drafts_immutable;
         ALTER TABLE staged_section_drafts RENAME TO staged_section_drafts_before_adr_0045;
         CREATE TABLE staged_section_drafts (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             draft_id TEXT NOT NULL UNIQUE,
             immutable_json TEXT NOT NULL CHECK (length(CAST(immutable_json AS BLOB)) <= 262144),
             replaces TEXT,
             review_revision INTEGER NOT NULL DEFAULT 0 CHECK (review_revision >= 0),
             review_decision TEXT CHECK (review_decision IN ('accepted', 'rejected', 'returned')),
             review_note TEXT CHECK (review_note IS NULL OR length(CAST(review_note AS BLOB)) <= 4096),
             created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
             reviewed_at TEXT,
             CHECK ((review_revision = 0 AND review_decision IS NULL AND review_note IS NULL AND reviewed_at IS NULL)
                 OR (review_revision > 0 AND review_decision IS NOT NULL AND reviewed_at IS NOT NULL)),
             CHECK (replaces IS NULL OR review_decision IS NULL OR review_decision <> 'returned')
         ) STRICT;
         INSERT INTO staged_section_drafts ({SUCCESSOR_COLUMNS})
             SELECT {SUCCESSOR_COLUMNS} FROM staged_section_drafts_before_adr_0045 ORDER BY sequence;
         DROP TABLE staged_section_drafts_before_adr_0045;"
    ))?;
    tx.commit()?;
    Ok(())
}

impl DraftStore {
    /// Open the draft table in the review store at `path`, creating it when
    /// absent, and the finding curation table beside it (ADR-0074). Other
    /// tables of the file are not read or changed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DraftStoreError> {
        let mut conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        widen_successors(&mut conn)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS staged_section_drafts (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 draft_id TEXT NOT NULL UNIQUE,
                 immutable_json TEXT NOT NULL CHECK (length(CAST(immutable_json AS BLOB)) <= 262144),
                 replaces TEXT,
                 review_revision INTEGER NOT NULL DEFAULT 0 CHECK (review_revision >= 0),
                 review_decision TEXT CHECK (review_decision IN ('accepted', 'rejected', 'returned')),
                 review_note TEXT CHECK (review_note IS NULL OR length(CAST(review_note AS BLOB)) <= 4096),
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                 reviewed_at TEXT,
                 CHECK ((review_revision = 0 AND review_decision IS NULL AND review_note IS NULL AND reviewed_at IS NULL)
                     OR (review_revision > 0 AND review_decision IS NOT NULL AND reviewed_at IS NOT NULL)),
                 CHECK (replaces IS NULL OR review_decision IS NULL OR review_decision <> 'returned')
             ) STRICT;
             CREATE UNIQUE INDEX IF NOT EXISTS staged_section_drafts_one_successor
             ON staged_section_drafts (replaces)
             WHERE replaces IS NOT NULL
                 AND (review_decision IS NULL OR review_decision <> 'rejected');
             CREATE TRIGGER IF NOT EXISTS staged_section_drafts_immutable
             BEFORE UPDATE OF sequence, draft_id, immutable_json, replaces, created_at ON staged_section_drafts
             BEGIN SELECT RAISE(ABORT, 'section draft content is immutable'); END;",
        )?;
        crate::finding_curation::create_schema(&conn)?;
        crate::attested_store::create_schema(&conn)?;
        Ok(Self { conn })
    }

    /// The store's connection, for the other record kinds of this file.
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Store an admitted draft before reporting it as staged.
    ///
    /// `replaces` names the draft this one succeeds: a returned draft
    /// (ADR-0064), which admits one successor ever, or an accepted draft
    /// being re-drafted (ADR-0045), which admits one successor that is not
    /// rejected. Identical content returns the stored record with its
    /// current review.
    pub fn store(
        &mut self,
        draft: &SectionDraft,
        origin: &DraftOrigin,
        replaces: Option<&str>,
    ) -> Result<StoredDraft, DraftStoreError> {
        validate(draft, origin)?;
        let content = DraftContent {
            schema_version: DRAFT_SCHEMA_VERSION,
            draft: draft.clone(),
            origin: origin.clone(),
            replaces: replaces.map(str::to_owned),
        };
        let json = serde_json::to_string(&content)?;
        if json.len() > MAX_DRAFT_RECORD_BYTES {
            return Err(DraftStoreError::RecordTooLarge { bytes: json.len() });
        }
        let draft_id = DraftContent::identity(&json);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = get_on(&tx, &draft_id)? {
            return Ok(stored);
        }
        if let Some(replaced) = replaces {
            let predecessor = get_on(&tx, replaced)?.ok_or(DraftStoreError::UnknownDraft)?;
            if predecessor.draft.address != draft.address {
                return Err(DraftStoreError::PredecessorAddress);
            }
            let taken = match predecessor.review_decision {
                Some(DraftDecision::Returned) => {
                    "SELECT EXISTS (SELECT 1 FROM staged_section_drafts WHERE replaces = ?1)"
                }
                // A rejected re-draft leaves the accepted draft free for
                // another one.
                Some(DraftDecision::Accepted) => {
                    "SELECT EXISTS (SELECT 1 FROM staged_section_drafts WHERE replaces = ?1
                         AND (review_decision IS NULL OR review_decision <> 'rejected'))"
                }
                Some(DraftDecision::Rejected) | None => {
                    return Err(DraftStoreError::PredecessorNotReturned);
                }
            };
            let taken: bool = tx.query_row(taken, [replaced], |row| row.get(0))?;
            if taken {
                return Err(DraftStoreError::SuccessorExists);
            }
        }
        tx.execute(
            "INSERT INTO staged_section_drafts (draft_id, immutable_json, replaces)
             VALUES (?1, ?2, ?3)",
            params![draft_id, json, replaces],
        )?;
        let stored = get_on(&tx, &draft_id)?.ok_or(DraftStoreError::InvalidRecord)?;
        tx.commit()?;
        Ok(stored)
    }

    /// One draft with its current review.
    pub fn get(&self, draft_id: &str) -> Result<Option<StoredDraft>, DraftStoreError> {
        get_on(&self.conn, draft_id)
    }

    /// The oldest draft produced by the durable job `job_id`, if any. Reads
    /// by origin directly, so the answer does not depend on the store size.
    pub fn for_job(&self, job_id: i64) -> Result<Option<StoredDraft>, DraftStoreError> {
        self.conn
            .query_row(
                &format!(
                    "{SELECT} WHERE json_extract(immutable_json, '$.origin.job_id') = ?1 \
                     ORDER BY sequence ASC LIMIT 1"
                ),
                [job_id],
                read_row,
            )
            .optional()?
            .map(|row| decode(row).map(|(_, draft)| draft))
            .transpose()
    }

    /// Record a decision on the draft at the revision the reviewer read.
    /// The content, the origin and the cited hashes are not changed.
    pub fn review(
        &mut self,
        draft_id: &str,
        expected_revision: u64,
        decision: DraftDecision,
        note: Option<&str>,
    ) -> Result<StoredDraft, DraftStoreError> {
        if note.is_some_and(|note| note.len() > MAX_DRAFT_NOTE_BYTES) {
            return Err(DraftStoreError::NoteTooLarge);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_on(&tx, draft_id)?.ok_or(DraftStoreError::UnknownDraft)?;
        if current.review_revision != expected_revision {
            return Err(DraftStoreError::StaleReviewRevision {
                expected: expected_revision,
                actual: current.review_revision,
            });
        }
        if current.replaces.is_some() && decision == DraftDecision::Returned {
            return Err(DraftStoreError::SuccessorCannotBeReturned);
        }
        // A rejected successor may not come back while another successor
        // of the same draft stands.
        if let Some(replaced) = current.replaces.as_deref()
            && current.review_decision == Some(DraftDecision::Rejected)
            && decision != DraftDecision::Rejected
        {
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM staged_section_drafts WHERE replaces = ?1
                     AND draft_id <> ?2
                     AND (review_decision IS NULL OR review_decision <> 'rejected'))",
                params![replaced, draft_id],
                |row| row.get(0),
            )?;
            if taken {
                return Err(DraftStoreError::SuccessorExists);
            }
        }
        let next = i64::try_from(expected_revision)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(DraftStoreError::InvalidRecord)?;
        let changed = tx.execute(
            "UPDATE staged_section_drafts SET review_revision = ?1, review_decision = ?2,
                 review_note = ?3, reviewed_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE draft_id = ?4 AND review_revision = ?5",
            params![next, decision.as_str(), note, draft_id, next - 1],
        )?;
        if changed != 1 {
            return Err(DraftStoreError::InvalidRecord);
        }
        let reviewed = get_on(&tx, draft_id)?.ok_or(DraftStoreError::InvalidRecord)?;
        tx.commit()?;
        Ok(reviewed)
    }

    /// Stored drafts, newest first, at most `limit` per page. `before` is
    /// the `next_before` of the previous page. Reviews are current as read.
    pub fn list(&self, limit: usize, before: Option<u64>) -> Result<DraftPage, DraftStoreError> {
        if limit == 0 || limit > MAX_DRAFT_PAGE_ITEMS {
            return Err(DraftStoreError::InvalidLimit);
        }
        let before = i64::try_from(before.unwrap_or(i64::MAX as u64)).unwrap_or(i64::MAX);
        let mut statement = self.conn.prepare(&format!(
            "{SELECT} WHERE sequence < ?1 ORDER BY sequence DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![before, (limit + 1) as i64], read_row)?;
        let mut items = Vec::with_capacity(limit);
        let mut next_before = None;
        let mut last = None;
        for row in rows {
            let (sequence, draft) = decode(row?)?;
            if items.len() == limit {
                next_before = last;
                break;
            }
            last = Some(sequence);
            items.push(draft);
        }
        Ok(DraftPage { items, next_before })
    }
}

#[cfg(test)]
#[path = "draft_store_tests.rs"]
mod tests;
