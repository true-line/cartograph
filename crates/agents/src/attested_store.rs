//! Durable attested records, their attestations and the project's
//! contributors (#2463, ADR-0090 decisions 1 to 4, AC-1073).
//!
//! Three tables in the review store's file, beside the staged proposals and
//! the section drafts, sharing neither their tables nor their decisions:
//!
//! - `attested_contributors` resolves a [`ContributorId`] to its kind, a
//!   display name and an optional handle. It is the only place a name is
//!   kept: records and attestations hold ids only. v1 holds at most one
//!   engineer, set once; a partial unique index holds that under concurrent
//!   writers. More engineers later need only more rows.
//! - `attested_records` holds each record's attested content under its
//!   [`identity`]. A trigger refuses any update.
//! - `attested_attestations` holds each record's attestations, numbered from
//!   zero, append-only: triggers refuse an update or a delete.
//!
//! **Appending.** An attestation is appended in one immediate transaction
//! that names the attestation count the caller read and fails on any other,
//! so a concurrent review is refused, never lost. The record with the new
//! attestation must pass [`admit`], and every contributor must be a known
//! contributor of the matching kind; an agent's directing contributor must
//! be the project's engineer.
//!
//! This module reads no graph and no source, calls no model and writes no
//! fact: nothing reaches the ingested target, the repository or the base
//! graph (ADR-0090 decisions 4 and 5).

use crate::attested::Attestation;
use crate::attested::{
    AttestationRefusal, AttestedRecord, AttestedTarget, ContributorId, Standing, admit,
    citation_id, identity, standing,
};
use core_prov::content_hash;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Most records one page returns.
pub const MAX_ATTESTED_PAGE_ITEMS: usize = 50;
/// Most bytes of a contributor's display name or handle.
pub const MAX_CONTRIBUTOR_TEXT_BYTES: usize = 256;
const ENGINEER_DOMAIN: &[u8] = b"cartograph:contributor:engineer:v1\0";
const AGENT_DOMAIN: &[u8] = b"cartograph:contributor:agent:v1\0";

/// What a contributor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContributorKind {
    /// A person working with the project.
    Engineer,
    /// An agent role on one provider and model.
    Agent,
}

impl ContributorKind {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "engineer" => Some(Self::Engineer),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// One row of the project's contributor table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contributor {
    /// The id attestations name.
    pub id: ContributorId,
    /// What it is.
    pub kind: ContributorKind,
    /// The name the project's own outputs print.
    pub display_name: String,
    /// An optional stable handle, such as a Git author identity.
    pub handle: Option<String>,
}

/// A stored record with its attestations and standing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredAttested {
    /// The record's identity.
    pub record_id: String,
    /// Its `A-` citation id.
    pub citation_id: String,
    /// The record with every attestation, oldest first.
    pub record: AttestedRecord,
    /// How the attestations stand.
    pub standing: Standing,
    /// UTC creation time; not part of the identity.
    pub created_at: String,
}

impl StoredAttested {
    /// The count a caller passes to [`AttestedStore::attest`].
    pub fn attestation_count(&self) -> u64 {
        self.record.attestations.len() as u64
    }
}

/// One page of stored records, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedPage {
    /// The records of this page.
    pub items: Vec<StoredAttested>,
    /// Pass as `before` to read the next page; `None` when this is the last.
    pub next_before: Option<u64>,
}

/// Why a record, attestation or contributor was not stored or read.
#[derive(Debug, thiserror::Error)]
pub enum AttestedStoreError {
    /// The record does not pass [`admit`].
    #[error("attested record is not admissible: {}", .0.code())]
    Refused(AttestationRefusal),
    /// An attestation names a contributor the project does not know.
    #[error("attestation {attestation} names an unknown contributor")]
    UnknownContributor {
        /// Zero-based position of the attestation.
        attestation: usize,
    },
    /// An attestation names a contributor of the other kind.
    #[error("attestation {attestation} names a contributor of another kind")]
    ContributorKind {
        /// Zero-based position of the attestation.
        attestation: usize,
    },
    /// An agent's directing contributor is not the project's engineer.
    #[error("attestation {attestation} is directed by a contributor who is not an engineer")]
    DirectorNotEngineer {
        /// Zero-based position of the attestation.
        attestation: usize,
    },
    /// The project already has its engineer (v1 holds one).
    #[error("the project already has its engineer")]
    EngineerExists,
    /// A display name or handle is blank, too long or not plain text.
    #[error("invalid contributor {0}")]
    InvalidContributor(&'static str),
    /// No stored record has this id.
    #[error("unknown attested record")]
    UnknownRecord,
    /// Another review appended first.
    #[error("stale attestation count: expected {expected}, current {actual}")]
    StaleAttestationCount {
        /// Count the caller read.
        expected: u64,
        /// Count now stored.
        actual: u64,
    },
    /// A page limit outside 1..=[`MAX_ATTESTED_PAGE_ITEMS`].
    #[error("attested page limit must be between 1 and {MAX_ATTESTED_PAGE_ITEMS}")]
    InvalidLimit,
    /// A stored row failed its identity or structure check.
    #[error("invalid stored attested record")]
    InvalidRecord,
    /// SQLite failed; nothing is reported as stored.
    #[error("attested store: {0}")]
    Storage(#[from] rusqlite::Error),
    /// Serialization failed before anything was written.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    target: AttestedTarget,
    statement: String,
    supporting: Vec<String>,
    recorded_hashes: BTreeMap<String, String>,
}

fn plain(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_CONTRIBUTOR_TEXT_BYTES
        && !value.chars().any(char::is_control)
}

fn derived_id(domain: &[u8], parts: &[&str]) -> ContributorId {
    let mut bytes = domain.to_vec();
    bytes.extend_from_slice(
        serde_json::to_string(parts)
            .expect("strings serialize")
            .as_bytes(),
    );
    ContributorId(format!("C-{}", &content_hash(&bytes)[..16]))
}

/// SQLite store of attested records, in the review store's file.
pub struct AttestedStore {
    conn: Connection,
}

impl AttestedStore {
    /// Open the attested tables in the review store at `path`, creating
    /// them when absent. Other tables of the file are not read or changed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AttestedStoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS attested_contributors (
                 contributor_id TEXT PRIMARY KEY CHECK (length(contributor_id) = 18),
                 kind TEXT NOT NULL CHECK (kind IN ('engineer', 'agent')),
                 display_name TEXT NOT NULL
                     CHECK (length(CAST(display_name AS BLOB)) BETWEEN 1 AND 256),
                 handle TEXT CHECK (handle IS NULL OR length(CAST(handle AS BLOB)) <= 256),
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ) STRICT;
             CREATE UNIQUE INDEX IF NOT EXISTS attested_contributors_one_engineer
             ON attested_contributors (kind) WHERE kind = 'engineer';
             CREATE TABLE IF NOT EXISTS attested_records (
                 sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 record_id TEXT NOT NULL UNIQUE,
                 content_json TEXT NOT NULL CHECK (length(CAST(content_json AS BLOB)) <= 65536),
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ) STRICT;
             CREATE TRIGGER IF NOT EXISTS attested_records_immutable
             BEFORE UPDATE ON attested_records
             BEGIN SELECT RAISE(ABORT, 'attested content is immutable'); END;
             CREATE TABLE IF NOT EXISTS attested_attestations (
                 record_id TEXT NOT NULL REFERENCES attested_records (record_id),
                 position INTEGER NOT NULL CHECK (position >= 0 AND position < 64),
                 contributor_id TEXT NOT NULL REFERENCES attested_contributors (contributor_id),
                 attestation_json TEXT NOT NULL
                     CHECK (length(CAST(attestation_json AS BLOB)) <= 4096),
                 recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                 PRIMARY KEY (record_id, position)
             ) STRICT;
             CREATE TRIGGER IF NOT EXISTS attested_attestations_no_update
             BEFORE UPDATE ON attested_attestations
             BEGIN SELECT RAISE(ABORT, 'attestations are append-only'); END;
             CREATE TRIGGER IF NOT EXISTS attested_attestations_no_delete
             BEFORE DELETE ON attested_attestations
             BEGIN SELECT RAISE(ABORT, 'attestations are append-only'); END;",
        )?;
        Ok(Self { conn })
    }

    /// Set the project's engineer, once (v1 holds one). The id is opaque and
    /// derived from nothing a reader can recover the name from.
    pub fn set_engineer(
        &mut self,
        display_name: &str,
        handle: Option<&str>,
    ) -> Result<Contributor, AttestedStoreError> {
        if !plain(display_name) {
            return Err(AttestedStoreError::InvalidContributor("display name"));
        }
        if handle.is_some_and(|handle| !plain(handle)) {
            return Err(AttestedStoreError::InvalidContributor("handle"));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if engineer_on(&tx)?.is_some() {
            return Err(AttestedStoreError::EngineerExists);
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos().to_string())
            .unwrap_or_default();
        let id = derived_id(ENGINEER_DOMAIN, &[&nonce, display_name]);
        tx.execute(
            "INSERT INTO attested_contributors (contributor_id, kind, display_name, handle)
             VALUES (?1, 'engineer', ?2, ?3)",
            params![id.0, display_name, handle],
        )?;
        let engineer = contributor_on(&tx, &id)?.ok_or(AttestedStoreError::InvalidRecord)?;
        tx.commit()?;
        Ok(engineer)
    }

    /// The contributor id of an agent role on one provider and model,
    /// registering it on first use. The same role, provider and model always
    /// yield the same id.
    pub fn agent(
        &mut self,
        role: &str,
        provider: &str,
        model: &str,
    ) -> Result<Contributor, AttestedStoreError> {
        if ![role, provider, model].into_iter().all(plain) {
            return Err(AttestedStoreError::InvalidContributor("agent"));
        }
        let id = derived_id(AGENT_DOMAIN, &[role, provider, model]);
        let display_name: String = format!("{role} ({provider}/{model})")
            .chars()
            .take(MAX_CONTRIBUTOR_TEXT_BYTES / 4)
            .collect();
        self.conn.execute(
            "INSERT INTO attested_contributors (contributor_id, kind, display_name)
             VALUES (?1, 'agent', ?2) ON CONFLICT (contributor_id) DO NOTHING",
            params![id.0, display_name],
        )?;
        contributor_on(&self.conn, &id)?.ok_or(AttestedStoreError::InvalidRecord)
    }

    /// One contributor, by id.
    pub fn contributor(
        &self,
        id: &ContributorId,
    ) -> Result<Option<Contributor>, AttestedStoreError> {
        contributor_on(&self.conn, id)
    }

    /// The project's engineer, when set.
    pub fn engineer(&self) -> Result<Option<Contributor>, AttestedStoreError> {
        engineer_on(&self.conn)
    }

    /// Store an admitted record with its attestations. Identical attested
    /// content returns the stored record with its current attestations; the
    /// given ones are then not appended, and the caller attests instead.
    pub fn stage(&mut self, record: AttestedRecord) -> Result<StoredAttested, AttestedStoreError> {
        let record = admit(record).map_err(AttestedStoreError::Refused)?;
        let record_id = identity(&record);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(stored) = get_on(&tx, &record_id)? {
            return Ok(stored);
        }
        check_contributors(&tx, &record.attestations, 0)?;
        let content = serde_json::to_string(&Content {
            target: record.target.clone(),
            statement: record.statement.clone(),
            supporting: record.supporting.clone(),
            recorded_hashes: record.recorded_hashes.clone(),
        })?;
        tx.execute(
            "INSERT INTO attested_records (record_id, content_json) VALUES (?1, ?2)",
            params![record_id, content],
        )?;
        for (position, attestation) in record.attestations.iter().enumerate() {
            insert_attestation(&tx, &record_id, position, attestation)?;
        }
        let stored = get_on(&tx, &record_id)?.ok_or(AttestedStoreError::InvalidRecord)?;
        tx.commit()?;
        Ok(stored)
    }

    /// Append `attestation` to the record, at the attestation count the
    /// caller read. The record's content and earlier attestations are not
    /// changed.
    pub fn attest(
        &mut self,
        record_id: &str,
        expected_count: u64,
        attestation: Attestation,
    ) -> Result<StoredAttested, AttestedStoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_on(&tx, record_id)?.ok_or(AttestedStoreError::UnknownRecord)?;
        if current.attestation_count() != expected_count {
            return Err(AttestedStoreError::StaleAttestationCount {
                expected: expected_count,
                actual: current.attestation_count(),
            });
        }
        let position = current.record.attestations.len();
        let mut next = current.record;
        next.attestations.push(attestation.clone());
        let next = admit(next).map_err(AttestedStoreError::Refused)?;
        check_contributors(&tx, &next.attestations[position..], position)?;
        insert_attestation(&tx, record_id, position, &attestation)?;
        let stored = get_on(&tx, record_id)?.ok_or(AttestedStoreError::InvalidRecord)?;
        tx.commit()?;
        Ok(stored)
    }

    /// One record with its attestations.
    pub fn get(&self, record_id: &str) -> Result<Option<StoredAttested>, AttestedStoreError> {
        get_on(&self.conn, record_id)
    }

    /// Stored records, newest first, at most `limit` per page. `before` is
    /// the `next_before` of the previous page.
    pub fn list(
        &self,
        limit: usize,
        before: Option<u64>,
    ) -> Result<AttestedPage, AttestedStoreError> {
        if limit == 0 || limit > MAX_ATTESTED_PAGE_ITEMS {
            return Err(AttestedStoreError::InvalidLimit);
        }
        let before = i64::try_from(before.unwrap_or(i64::MAX as u64)).unwrap_or(i64::MAX);
        let mut statement = self.conn.prepare(
            "SELECT sequence, record_id FROM attested_records
             WHERE sequence < ?1 ORDER BY sequence DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![before, (limit + 1) as i64], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut items = Vec::with_capacity(limit);
        let mut next_before = None;
        let mut last = None;
        for row in rows {
            let (sequence, record_id) = row?;
            if items.len() == limit {
                next_before = last;
                break;
            }
            last = Some(u64::try_from(sequence).map_err(|_| AttestedStoreError::InvalidRecord)?);
            items.push(get_on(&self.conn, &record_id)?.ok_or(AttestedStoreError::InvalidRecord)?);
        }
        Ok(AttestedPage { items, next_before })
    }
}

fn contributor_on(
    conn: &Connection,
    id: &ContributorId,
) -> Result<Option<Contributor>, AttestedStoreError> {
    conn.query_row(
        "SELECT contributor_id, kind, display_name, handle FROM attested_contributors
         WHERE contributor_id = ?1",
        [&id.0],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        },
    )
    .optional()?
    .map(|(id, kind, display_name, handle)| {
        Ok(Contributor {
            id: ContributorId(id),
            kind: ContributorKind::parse(&kind).ok_or(AttestedStoreError::InvalidRecord)?,
            display_name,
            handle,
        })
    })
    .transpose()
}

fn engineer_on(conn: &Connection) -> Result<Option<Contributor>, AttestedStoreError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT contributor_id FROM attested_contributors WHERE kind = 'engineer'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match id {
        Some(id) => contributor_on(conn, &ContributorId(id)),
        None => Ok(None),
    }
}

/// Every attestation names a known contributor of its kind, and an agent's
/// director is the project's engineer. `offset` is the position of the
/// first attestation given.
fn check_contributors(
    conn: &Connection,
    attestations: &[Attestation],
    offset: usize,
) -> Result<(), AttestedStoreError> {
    for (index, value) in attestations.iter().enumerate() {
        let attestation = offset + index;
        let expected = match value {
            Attestation::Agent { .. } => ContributorKind::Agent,
            Attestation::Engineer { .. } => ContributorKind::Engineer,
        };
        let known = contributor_on(conn, value.contributor())?
            .ok_or(AttestedStoreError::UnknownContributor { attestation })?;
        if known.kind != expected {
            return Err(AttestedStoreError::ContributorKind { attestation });
        }
        if let Attestation::Agent {
            directed_by: Some(director),
            ..
        } = value
            && !contributor_on(conn, director)?
                .is_some_and(|known| known.kind == ContributorKind::Engineer)
        {
            return Err(AttestedStoreError::DirectorNotEngineer { attestation });
        }
    }
    Ok(())
}

fn insert_attestation(
    conn: &Connection,
    record_id: &str,
    position: usize,
    attestation: &Attestation,
) -> Result<(), AttestedStoreError> {
    conn.execute(
        "INSERT INTO attested_attestations (record_id, position, contributor_id, attestation_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            record_id,
            position as i64,
            attestation.contributor().0,
            serde_json::to_string(attestation)?
        ],
    )?;
    Ok(())
}

fn get_on(
    conn: &Connection,
    record_id: &str,
) -> Result<Option<StoredAttested>, AttestedStoreError> {
    let Some((content, created_at)) = conn
        .query_row(
            "SELECT content_json, created_at FROM attested_records WHERE record_id = ?1",
            [record_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let content: Content =
        serde_json::from_str(&content).map_err(|_| AttestedStoreError::InvalidRecord)?;
    let mut statement = conn.prepare(
        "SELECT position, contributor_id, attestation_json FROM attested_attestations
         WHERE record_id = ?1 ORDER BY position",
    )?;
    let rows = statement.query_map([record_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut attestations = Vec::new();
    for row in rows {
        let (position, contributor, json) = row?;
        let attestation: Attestation =
            serde_json::from_str(&json).map_err(|_| AttestedStoreError::InvalidRecord)?;
        if position != attestations.len() as i64 || attestation.contributor().0 != contributor {
            return Err(AttestedStoreError::InvalidRecord);
        }
        attestations.push(attestation);
    }
    let record = AttestedRecord {
        target: content.target,
        statement: content.statement,
        supporting: content.supporting,
        recorded_hashes: content.recorded_hashes,
        attestations,
    };
    let record = admit(record).map_err(|_| AttestedStoreError::InvalidRecord)?;
    if identity(&record) != record_id {
        return Err(AttestedStoreError::InvalidRecord);
    }
    Ok(Some(StoredAttested {
        record_id: record_id.to_owned(),
        citation_id: citation_id(&record),
        standing: standing(&record.attestations),
        record,
        created_at,
    }))
}

#[cfg(test)]
#[path = "attested_store_tests.rs"]
mod tests;
