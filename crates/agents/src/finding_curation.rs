//! Durable curation reviews of investigation findings (#1746 requirement 2,
//! ADR-0074 decisions 1 to 3 and 10, SPEC-12 "Findings", AC-0844).
//!
//! A curation review is not the follow-up review of SPEC-03. It has its own
//! table, in the section draft store's file, and its own decision type. It
//! is reached through [`DraftStore::finding_curation`], so a reader that
//! holds the draft store reads both under one lock. This module reads no
//! graph and no source, calls no model and writes no fact; accepting a
//! finding changes no tier (R-INT-1, R-INT-3).
//!
//! **Record.** The first review of a finding stores the finding as the host
//! resolved it from the immutable saved result: its subject, its producing
//! role, its content, and each cited fact with the digest the producing run
//! recorded. The record is keyed by the SPEC-03 review subject id, which
//! binds the result, its producing configuration and the result-local
//! finding id. A trigger refuses any later change of the content, and a
//! review that presents other content for a stored subject is refused.
//!
//! **Review.** A review names the revision it read and a decision. It is one
//! immediate transaction that fails on any other revision. Revision zero
//! means the finding has no curation review and no row. Rows are never
//! deleted: rejecting an accepted finding is how it is withdrawn.
//!
//! **Bounds.** The immutable payload is at most
//! [`MAX_CURATED_FINDING_BYTES`]. Every read returns at most the number of
//! rows its caller names and says when more exist.

use crate::draft_store::DraftStore;
use crate::investigation::{FindingReviewSubjectV1, InvestigationClaimKind};
use crate::source_basis::TaskFactKey;
use context_hub::curated::{FindingCuration, ReviewedFinding, SourceSupport};
use core_prov::content_hash;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Version of the immutable finding envelope.
pub const CURATED_FINDING_SCHEMA_VERSION: u32 = 1;
/// Most bytes of one finding's immutable payload.
pub const MAX_CURATED_FINDING_BYTES: usize = 64 * 1024;
/// Most cited facts of one finding; a saved finding has at most eight
/// citations and each names one fact.
pub const MAX_CURATED_FINDING_FACTS: usize = 8;
const MAX_ROLE_BYTES: usize = 256;
const MAX_TITLE_BYTES: usize = 160;
const MAX_STATEMENT_BYTES: usize = 2048;
const MAX_LIMITATIONS: usize = 16;
const MAX_LIMITATION_BYTES: usize = 2048;
const MAX_CITATION_BYTES: usize = 128;
const MAX_DIGEST_BYTES: usize = 128;
const IDENTITY_DOMAIN: &[u8] = b"cartograph.curated-finding.v1\0";

/// One graph fact a finding cites, as its producing run recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CitedFact {
    /// The ADR-0058 citation id of the fact.
    pub citation: String,
    /// The fact itself.
    pub fact: TaskFactKey,
    /// The digest the fact had when the run read it.
    pub digest: String,
    /// Whether every citation of the finding that names this fact rests on
    /// captured primary-source evidence.
    pub captured: bool,
}

/// A saved finding as the host resolved it. Immutable once stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CuratedFindingRecord {
    /// The exact historical finding (SPEC-03 review subject).
    pub subject: FindingReviewSubjectV1,
    /// The specialist that produced it, such as `analyst@27`.
    pub role: String,
    /// The finding's claim kind as saved.
    pub claim_kind: InvestigationClaimKind,
    /// The finding's title.
    pub title: String,
    /// The statement.
    pub statement: String,
    /// The limitations the finding was saved with, in order.
    pub limitations: Vec<String>,
    /// The cited facts, ordered by citation id, one entry per fact.
    pub cited: Vec<CitedFact>,
}

/// A human curation decision. It is not a follow-up decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurationDecision {
    /// The finding may be shown as curated, if the projection finds it
    /// eligible. It stays Agentic / InferredWeak.
    Accepted,
    /// The finding is not shown. This also withdraws an acceptance.
    Rejected,
}

impl CurationDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// A stored finding with its current curation review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredCuratedFinding {
    /// Version of the immutable envelope.
    pub schema_version: u32,
    /// The finding as resolved at its first review.
    pub record: CuratedFindingRecord,
    /// Compare-and-set review revision; at least one for a stored finding.
    pub review_revision: u64,
    /// The current decision.
    pub review_decision: CurationDecision,
    /// UTC time of the last review.
    pub reviewed_at: String,
}

/// Why a finding was not stored, read or reviewed.
#[derive(Debug, thiserror::Error)]
pub enum FindingCurationError {
    /// The record is missing a field or holds an unusable one.
    #[error("invalid curated finding: {0}")]
    InvalidRecord(&'static str),
    /// The immutable payload exceeds the bound.
    #[error("curated finding exceeds {MAX_CURATED_FINDING_BYTES} bytes ({bytes} bytes)")]
    RecordTooLarge {
        /// Measured serialized bytes.
        bytes: usize,
    },
    /// The subject is stored with other content. The subject binds one
    /// immutable result, so this is never a legitimate update.
    #[error("the finding is stored with different content")]
    ContentChanged,
    /// Another review changed the finding first.
    #[error("stale review revision: expected {expected}, current {actual}")]
    StaleReviewRevision {
        /// Revision the reviewer read.
        expected: u64,
        /// Revision now stored.
        actual: u64,
    },
    /// Another connection held the write lock for the whole busy timeout.
    /// Raised only while taking the lock, so nothing was written.
    #[error("the finding curation store is busy")]
    Busy,
    /// A read limit of zero.
    #[error("read limit must be at least one")]
    InvalidLimit,
    /// A stored row failed its version, integrity or structure check.
    #[error("invalid stored curated finding")]
    InvalidStored,
    /// SQLite failed; nothing is reported as stored or reviewed.
    #[error("finding curation store: {0}")]
    Storage(#[from] rusqlite::Error),
    /// Serialization failed before anything was written.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    schema_version: u32,
    record: CuratedFindingRecord,
}

fn identity(json: &str) -> String {
    let mut bytes = IDENTITY_DOMAIN.to_vec();
    bytes.extend_from_slice(json.as_bytes());
    content_hash(&bytes)
}

fn plain(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn validate(record: &CuratedFindingRecord) -> Result<(), FindingCurationError> {
    let invalid = FindingCurationError::InvalidRecord;
    record.subject.validate().map_err(|_| invalid("subject"))?;
    if !plain(&record.role, MAX_ROLE_BYTES) {
        return Err(invalid("role"));
    }
    if !plain(&record.title, MAX_TITLE_BYTES) {
        return Err(invalid("title"));
    }
    if record.statement.trim().is_empty() || record.statement.len() > MAX_STATEMENT_BYTES {
        return Err(invalid("statement"));
    }
    if record.limitations.len() > MAX_LIMITATIONS
        || record
            .limitations
            .iter()
            .any(|text| text.trim().is_empty() || text.len() > MAX_LIMITATION_BYTES)
    {
        return Err(invalid("limitations"));
    }
    // A saved finding always cites; a record with no fact is not a finding
    // the host resolved.
    if record.cited.is_empty() || record.cited.len() > MAX_CURATED_FINDING_FACTS {
        return Err(invalid("cited facts"));
    }
    if record.cited.iter().any(|cited| {
        !plain(&cited.citation, MAX_CITATION_BYTES) || !plain(&cited.digest, MAX_DIGEST_BYTES)
    }) {
        return Err(invalid("cited fact"));
    }
    // Ordered by citation id, each id and each fact once.
    if !record
        .cited
        .windows(2)
        .all(|pair| pair[0].citation < pair[1].citation)
        || record
            .cited
            .iter()
            .map(|cited| &cited.fact)
            .collect::<BTreeSet<_>>()
            .len()
            != record.cited.len()
    {
        return Err(invalid("cited fact order"));
    }
    Ok(())
}

/// The projection input for a stored finding (ADR-0074 decision 4).
pub fn reviewed_finding(stored: &StoredCuratedFinding) -> ReviewedFinding {
    let record = &stored.record;
    let captured = record.cited.iter().filter(|cited| cited.captured).count();
    ReviewedFinding {
        finding_ref: record.subject.subject_id.clone(),
        investigation_id: record.subject.investigation_id.clone(),
        result_id: record.subject.result_id.clone(),
        finding_id: record.subject.finding_id.clone(),
        role: record.role.clone(),
        kind: claim_kind(record.claim_kind).to_owned(),
        title: record.title.clone(),
        statement: record.statement.clone(),
        limitations: record.limitations.clone(),
        cited_digests: record
            .cited
            .iter()
            .map(|cited| (cited.citation.clone(), cited.digest.clone()))
            .collect(),
        support: SourceSupport {
            corroborated: 0,
            captured,
            unverified: record.cited.len() - captured,
        },
        decision: Some(match stored.review_decision {
            CurationDecision::Accepted => FindingCuration::Accepted,
            CurationDecision::Rejected => FindingCuration::Rejected,
        }),
    }
}

/// The claim kind as every curated read spells it.
pub fn claim_kind(kind: InvestigationClaimKind) -> &'static str {
    match kind {
        InvestigationClaimKind::ImplementedBehavior => "implemented_behavior",
        InvestigationClaimKind::DocumentedIntent => "documented_intent",
        InvestigationClaimKind::InferredInterpretation => "inferred_interpretation",
        InvestigationClaimKind::ProposedDesign => "proposed_design",
    }
}

/// The current curation review of one finding of a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurationObservation {
    /// The SPEC-03 review subject id of the finding.
    pub subject_id: String,
    /// The finding's id within its result.
    pub finding_id: String,
    /// Compare-and-set review revision.
    pub review_revision: u64,
    /// The current decision.
    pub review_decision: CurationDecision,
    /// UTC time of the last review.
    pub reviewed_at: String,
}

/// A bounded read of reviewed findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedFindings {
    /// At most the requested number of findings, oldest first.
    pub items: Vec<StoredCuratedFinding>,
    /// Whether the store holds more than were returned.
    pub more: bool,
}

/// The curation reviews of a section draft store's file.
pub struct FindingCurationStore<'a> {
    conn: &'a Connection,
}

impl DraftStore {
    /// The finding curation table of this store's file (ADR-0074).
    pub fn finding_curation(&self) -> FindingCurationStore<'_> {
        FindingCurationStore {
            conn: self.connection(),
        }
    }
}

const SELECT: &str = "SELECT subject_id, content_digest, immutable_json, review_revision, \
                      review_decision, reviewed_at FROM curated_findings";

type Row = (String, String, String, i64, String, String);

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn decode(row: Row) -> Result<(String, StoredCuratedFinding), FindingCurationError> {
    let (subject_id, digest, json, revision, decision, reviewed_at) = row;
    let content: Content =
        serde_json::from_str(&json).map_err(|_| FindingCurationError::InvalidStored)?;
    let review_decision =
        CurationDecision::parse(&decision).ok_or(FindingCurationError::InvalidStored)?;
    if content.schema_version != CURATED_FINDING_SCHEMA_VERSION
        || identity(&json) != digest
        || content.record.subject.subject_id != subject_id
        || revision <= 0
        || validate(&content.record).is_err()
    {
        return Err(FindingCurationError::InvalidStored);
    }
    Ok((
        digest,
        StoredCuratedFinding {
            schema_version: content.schema_version,
            record: content.record,
            review_revision: revision as u64,
            review_decision,
            reviewed_at,
        },
    ))
}

fn get_on(
    conn: &Connection,
    subject_id: &str,
) -> Result<Option<(String, StoredCuratedFinding)>, FindingCurationError> {
    conn.query_row(
        &format!("{SELECT} WHERE subject_id = ?1"),
        [subject_id],
        read_row,
    )
    .optional()?
    .map(decode)
    .transpose()
}

/// Create the curation table when absent. [`DraftStore::open`] calls this;
/// no other table is read or changed.
pub(crate) fn create_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS curated_findings (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             subject_id TEXT NOT NULL UNIQUE,
             investigation_id TEXT NOT NULL,
             result_id TEXT NOT NULL,
             finding_id TEXT NOT NULL,
             content_digest TEXT NOT NULL,
             immutable_json TEXT NOT NULL CHECK (length(CAST(immutable_json AS BLOB)) <= 65536),
             review_revision INTEGER NOT NULL CHECK (review_revision > 0),
             review_decision TEXT NOT NULL CHECK (review_decision IN ('accepted', 'rejected')),
             reviewed_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
         ) STRICT;
         CREATE INDEX IF NOT EXISTS curated_findings_result
         ON curated_findings (investigation_id, result_id);
         CREATE TRIGGER IF NOT EXISTS curated_findings_immutable
         BEFORE UPDATE OF sequence, subject_id, investigation_id, result_id, finding_id,
             content_digest, immutable_json ON curated_findings
         BEGIN SELECT RAISE(ABORT, 'curated finding content is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS curated_findings_no_delete
         BEFORE DELETE ON curated_findings
         BEGIN SELECT RAISE(ABORT, 'durable curation review'); END;",
    )
}

impl FindingCurationStore<'_> {
    /// Record a curation decision on `record` at the revision the reviewer
    /// read. Revision zero is a finding with no curation review: the record
    /// is stored with its first decision. A later review must present the
    /// same content, which is never changed.
    pub fn review(
        &self,
        record: &CuratedFindingRecord,
        expected_revision: u64,
        decision: CurationDecision,
    ) -> Result<StoredCuratedFinding, FindingCurationError> {
        validate(record)?;
        let json = serde_json::to_string(&Content {
            schema_version: CURATED_FINDING_SCHEMA_VERSION,
            record: record.clone(),
        })?;
        if json.len() > MAX_CURATED_FINDING_BYTES {
            return Err(FindingCurationError::RecordTooLarge { bytes: json.len() });
        }
        let digest = identity(&json);
        let subject = &record.subject;
        // Lock contention is retryable; any other failure is storage.
        let tx = Transaction::new_unchecked(self.conn, TransactionBehavior::Immediate).map_err(
            |error| match error.sqlite_error_code() {
                Some(rusqlite::ErrorCode::DatabaseBusy) => FindingCurationError::Busy,
                _ => error.into(),
            },
        )?;
        let current = get_on(&tx, &subject.subject_id)?;
        let actual = match &current {
            Some((stored_digest, stored)) => {
                if *stored_digest != digest {
                    return Err(FindingCurationError::ContentChanged);
                }
                stored.review_revision
            }
            None => 0,
        };
        if actual != expected_revision {
            return Err(FindingCurationError::StaleReviewRevision {
                expected: expected_revision,
                actual,
            });
        }
        let next = i64::try_from(expected_revision)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(FindingCurationError::InvalidStored)?;
        let changed = if current.is_none() {
            tx.execute(
                "INSERT INTO curated_findings (subject_id, investigation_id, result_id,
                     finding_id, content_digest, immutable_json, review_revision, review_decision)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    subject.subject_id,
                    subject.investigation_id,
                    subject.result_id,
                    subject.finding_id,
                    digest,
                    json,
                    next,
                    decision.as_str()
                ],
            )?
        } else {
            tx.execute(
                "UPDATE curated_findings SET review_revision = ?1, review_decision = ?2,
                     reviewed_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
                 WHERE subject_id = ?3 AND review_revision = ?4",
                params![next, decision.as_str(), subject.subject_id, next - 1],
            )?
        };
        if changed != 1 {
            return Err(FindingCurationError::InvalidStored);
        }
        let (_, reviewed) =
            get_on(&tx, &subject.subject_id)?.ok_or(FindingCurationError::InvalidStored)?;
        tx.commit()?;
        Ok(reviewed)
    }

    /// One finding with its current curation review; `None` when it has
    /// never been reviewed for curation.
    pub fn get(
        &self,
        subject_id: &str,
    ) -> Result<Option<StoredCuratedFinding>, FindingCurationError> {
        Ok(get_on(self.conn, subject_id)?.map(|(_, stored)| stored))
    }

    /// The curation reviews of the findings of one result, in finding id
    /// order. At most `limit` are read; a result with more fails closed.
    pub fn for_result(
        &self,
        investigation_id: &str,
        result_id: &str,
        limit: usize,
    ) -> Result<Vec<CurationObservation>, FindingCurationError> {
        let page = self.read(
            "WHERE investigation_id = ?1 AND result_id = ?2 ORDER BY finding_id ASC LIMIT ?3",
            params![investigation_id, result_id, bound(limit)?],
            limit,
        )?;
        if page.more {
            return Err(FindingCurationError::InvalidStored);
        }
        Ok(page
            .items
            .into_iter()
            .map(|stored| CurationObservation {
                subject_id: stored.record.subject.subject_id,
                finding_id: stored.record.subject.finding_id,
                review_revision: stored.review_revision,
                review_decision: stored.review_decision,
                reviewed_at: stored.reviewed_at,
            })
            .collect())
    }

    /// Every reviewed finding, oldest first, at most `limit`. `more` says
    /// the store holds others, so a caller never takes a partial read for a
    /// complete one.
    pub fn reviewed(&self, limit: usize) -> Result<ReviewedFindings, FindingCurationError> {
        self.read(
            "ORDER BY sequence ASC LIMIT ?1",
            params![bound(limit)?],
            limit,
        )
    }

    fn read(
        &self,
        clause: &str,
        params: impl rusqlite::Params,
        limit: usize,
    ) -> Result<ReviewedFindings, FindingCurationError> {
        let mut statement = self.conn.prepare(&format!("{SELECT} {clause}"))?;
        let rows = statement.query_map(params, read_row)?;
        let mut items = Vec::new();
        let mut more = false;
        for row in rows {
            if items.len() == limit {
                more = true;
                break;
            }
            items.push(decode(row?)?.1);
        }
        Ok(ReviewedFindings { items, more })
    }
}

/// `limit + 1` rows are read so that one more than the bound is seen.
fn bound(limit: usize) -> Result<i64, FindingCurationError> {
    if limit == 0 {
        return Err(FindingCurationError::InvalidLimit);
    }
    i64::try_from(limit)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(FindingCurationError::InvalidLimit)
}

#[cfg(test)]
#[path = "finding_curation_tests.rs"]
mod tests;
