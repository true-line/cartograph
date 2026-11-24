//! Retained primary-source evidence; independent of tier and proposal review.
//! The receipt and capture stores; the app's `primary_source` module adds
//! the commands that read them through its state (#1924).

use adapters_lang_ts::captured::{self, Citable, Receipt};
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, DirBuilder, OpenOptions};
use core_graph::source::{FactKey, SourceBinding};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use source_capture::{Capture, CaptureLimits, CaptureStore, SourceId, StoreLimits};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[path = "primary_source/pinned.rs"]
mod pinned;

#[cfg(test)]
#[path = "primary_source/closure_tests.rs"]
mod closure_tests;
pub use pinned::PinnedReadError;

use crate::sources::RegisteredSource;

const MAX_RECEIPTS: u64 = 200_000;
const MAX_RECEIPT_BYTES: u64 = 128 * 1024;
const MAX_RECEIPT_HEADER_BYTES: u64 = 16 * 1024;
const MAX_RECEIPT_STORE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOMBSTONE_CAPTURE_IDS_BYTES: u64 = 64 * 1024;
const RECEIPT_HEADER_DOMAIN: &[u8] = b"cartograph:receipt-header:v1\0";
const RECEIPTS_SCHEMA_V1: &str = "CREATE TABLE receipts (id TEXT PRIMARY KEY NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 131072)) STRICT";
const META_SCHEMA_V1: &str =
    "CREATE TABLE receipt_meta (version INTEGER PRIMARY KEY CHECK(version = 1)) STRICT";
const RECEIPTS_ALTER_V2: &str = "ALTER TABLE receipts ADD COLUMN capture_forgotten_tombstone INTEGER REFERENCES receipt_tombstones(id)";
const RECEIPTS_SCHEMA_V3: &str = "CREATE TABLE receipts (id TEXT PRIMARY KEY NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 131072), capture_forgotten_tombstone INTEGER REFERENCES receipt_tombstones(id)) STRICT";
/// v4 (ADR-0046 decision 7): per-file shared header table and nullable
/// foreign key on receipts. Rows with `header_id IS NULL` hold full legacy
/// JSON in `payload`; rows with `header_id` set hold compact body JSON.
const RECEIPTS_ALTER_V4: &str =
    "ALTER TABLE receipts ADD COLUMN header_id TEXT REFERENCES receipt_headers(id)";
const RECEIPTS_SCHEMA_V4: &str = "CREATE TABLE receipts (id TEXT PRIMARY KEY NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 131072), capture_forgotten_tombstone INTEGER REFERENCES receipt_tombstones(id), header_id TEXT REFERENCES receipt_headers(id)) STRICT";
/// v5 (ADR-0072 slice 5, AC-1082): the input closure a contract-v4 receipt
/// names as established, stored once per closure id beside the receipts that
/// cite it.
const RECEIPTS_ALTER_V5: &str =
    "ALTER TABLE receipts ADD COLUMN closure_id TEXT REFERENCES input_closures(id)";
const RECEIPTS_SCHEMA: &str = "CREATE TABLE receipts (id TEXT PRIMARY KEY NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 131072), capture_forgotten_tombstone INTEGER REFERENCES receipt_tombstones(id), header_id TEXT REFERENCES receipt_headers(id), closure_id TEXT REFERENCES input_closures(id)) STRICT";
const CLOSURES_SCHEMA: &str = "CREATE TABLE input_closures (id TEXT PRIMARY KEY NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 2097152)) STRICT";
const HEADERS_SCHEMA: &str = "CREATE TABLE receipt_headers (id TEXT PRIMARY KEY NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, header TEXT NOT NULL CHECK(length(CAST(header AS BLOB)) <= 16384)) STRICT";
const TOMBSTONES_SCHEMA_V2: &str = "CREATE TABLE receipt_tombstones (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, capture_ids TEXT NOT NULL CHECK(length(CAST(capture_ids AS BLOB)) <= 65536), capture_ids_truncated INTEGER NOT NULL CHECK (capture_ids_truncated IN (0,1)), forgotten_at TEXT NOT NULL, receipts_reclaimed INTEGER NOT NULL CHECK (receipts_reclaimed >= 0), receipts_retained_pinned INTEGER NOT NULL CHECK (receipts_retained_pinned >= 0)) STRICT";
/// v3 (ADR-0046 decision 5): why the tombstone's receipts were reclaimed.
/// Existing tombstones were all written by forget.
const TOMBSTONES_ALTER_V3: &str = "ALTER TABLE receipt_tombstones ADD COLUMN cause TEXT NOT NULL DEFAULT 'forgotten' CHECK (cause IN ('forgotten','superseded'))";
const TOMBSTONES_SCHEMA: &str = "CREATE TABLE receipt_tombstones (id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, capture_ids TEXT NOT NULL CHECK(length(CAST(capture_ids AS BLOB)) <= 65536), capture_ids_truncated INTEGER NOT NULL CHECK (capture_ids_truncated IN (0,1)), forgotten_at TEXT NOT NULL, receipts_reclaimed INTEGER NOT NULL CHECK (receipts_reclaimed >= 0), receipts_retained_pinned INTEGER NOT NULL CHECK (receipts_retained_pinned >= 0), cause TEXT NOT NULL DEFAULT 'forgotten' CHECK (cause IN ('forgotten','superseded'))) STRICT";
const RECLAIMS_SCHEMA: &str = "CREATE TABLE receipt_reclaims (receipt_id TEXT PRIMARY KEY NOT NULL, tombstone_id INTEGER NOT NULL REFERENCES receipt_tombstones(id)) STRICT";
const META_SCHEMA_V2: &str =
    "CREATE TABLE receipt_meta (version INTEGER PRIMARY KEY CHECK(version = 2)) STRICT";
const META_SCHEMA_V3: &str =
    "CREATE TABLE receipt_meta (version INTEGER PRIMARY KEY CHECK(version = 3)) STRICT";
const META_SCHEMA_V4: &str =
    "CREATE TABLE receipt_meta (version INTEGER PRIMARY KEY CHECK(version = 4)) STRICT";
const META_SCHEMA: &str =
    "CREATE TABLE receipt_meta (version INTEGER PRIMARY KEY CHECK(version = 5)) STRICT";
const RECEIPT_INDEXES_V3: [(&str, &str); 3] = [
    (
        "receipt_reclaims_by_tombstone",
        "CREATE INDEX receipt_reclaims_by_tombstone ON receipt_reclaims (tombstone_id)",
    ),
    (
        "receipt_tombstones_by_source",
        "CREATE INDEX receipt_tombstones_by_source ON receipt_tombstones (source_id)",
    ),
    (
        "receipts_by_forgotten_tombstone",
        "CREATE INDEX receipts_by_forgotten_tombstone ON receipts (capture_forgotten_tombstone) WHERE capture_forgotten_tombstone IS NOT NULL",
    ),
];
/// Indexes that keep `receipt_storage`'s retention integrity checks linear;
/// without them a valid store with many tombstones or headers costs a
/// quadratic scan on every open.
const RECEIPT_INDEXES: [(&str, &str); 5] = [
    RECEIPT_INDEXES_V3[0],
    RECEIPT_INDEXES_V3[1],
    RECEIPT_INDEXES_V3[2],
    (
        "receipts_by_header",
        "CREATE INDEX receipts_by_header ON receipts (header_id)",
    ),
    (
        "receipts_by_closure",
        "CREATE INDEX receipts_by_closure ON receipts (closure_id) WHERE closure_id IS NOT NULL",
    ),
];

/// Delete tombstones that no reclaim-ledger row and no capture-forgotten
/// receipt references, so tombstones stay bounded by those rows.
fn delete_orphan_tombstones(tx: &Connection) -> Result<(), String> {
    tx.execute(
        "DELETE FROM receipt_tombstones
         WHERE NOT EXISTS (
             SELECT 1 FROM receipt_reclaims WHERE tombstone_id = receipt_tombstones.id
         )
         AND NOT EXISTS (
             SELECT 1 FROM receipts WHERE capture_forgotten_tombstone = receipt_tombstones.id
         )",
        [],
    )
    .map_err(storage_error)?;
    Ok(())
}

/// Delete receipt headers that no remaining receipt row references, so
/// retained headers stay bounded by referencing receipt rows.
fn delete_orphan_headers(tx: &Connection) -> Result<(), String> {
    tx.execute(
        "DELETE FROM receipt_headers
         WHERE NOT EXISTS (
             SELECT 1 FROM receipts WHERE header_id = receipt_headers.id
         )",
        [],
    )
    .map_err(storage_error)?;
    Ok(())
}

/// Delete input closures that no remaining receipt row names, so retained
/// closures stay bounded by the receipts that cite them (AC-1082).
fn delete_orphan_closures(tx: &Connection) -> Result<(), String> {
    tx.execute(
        "DELETE FROM input_closures
         WHERE NOT EXISTS (
             SELECT 1 FROM receipts WHERE closure_id = input_closures.id
         )",
        [],
    )
    .map_err(storage_error)?;
    Ok(())
}

fn create_v3_receipt_indexes(tx: &Connection) -> Result<(), String> {
    for (_, ddl) in RECEIPT_INDEXES_V3 {
        tx.execute_batch(ddl).map_err(storage_error)?;
    }
    Ok(())
}

fn create_receipt_indexes(tx: &Connection) -> Result<(), String> {
    for (_, ddl) in RECEIPT_INDEXES {
        tx.execute_batch(ddl).map_err(storage_error)?;
    }
    Ok(())
}

/// Per-file shared fields of a v3 receipt's `content` (everything except
/// `fact_key`, `fact_digest`, and `ranges`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptHeader {
    schema_version: u32,
    source_id: SourceId,
    repo_key: String,
    file: source_capture::CaptureFileRef,
    commit_sha: String,
    producer_contract: String,
    grammar: captured::Grammar,
    grammar_package: String,
    parser_package: String,
    source_scope: String,
    input_closure: StoredClosure,
}

/// A receipt's `input_closure` as stored in a compact header: the legacy
/// string of contracts v1 to v3, or the closure state of contract v4
/// (AC-1082). Exact bytes are still proven by the rebuild round trip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredClosure {
    Legacy(String),
    Closure(captured::ReceiptClosure),
}

/// Compact v3 and v4 receipt body stored in `receipts.payload` when `header_id` is set
/// (ADR-0046 decision 7):
/// - `k`: node `fact_key.id` with `<repo_key>@<path>` removed after `<kind-prefix>:`
///   (stored as `"<kind-prefix>:<suffix>"`, only when re-adding `<repo_key>@<path>`
///   reproduces `fact_key.id` byte-for-byte)
/// - `K`: verbatim node `fact_key.id` when the `<kind-prefix>:<repo_key>@<path>`
///   prefix is absent
/// - `e`: edge `[source, label, destination]` with `<repo_key>@<path>` removed
///   from both endpoint IDs
/// - `E`: verbatim edge `[source, label, destination]` when either endpoint ID
///   does not start with `<kind-prefix>:<repo_key>@<path>`
/// - `d`: `fact_digest`
/// - `r`: `ranges` as `[role, index, byte_start, byte_end]` arrays
/// - `o`: a multi-file receipt's `other_files` (AC-1039), absent when empty
/// - `f`: `[range position, file index]` for each range citing one of `o`,
///   absent when none does
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    k: Option<String>,
    #[serde(rename = "K", skip_serializing_if = "Option::is_none")]
    verbatim_k: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    e: Option<(String, String, String)>,
    #[serde(rename = "E", skip_serializing_if = "Option::is_none")]
    verbatim_e: Option<(String, String, String)>,
    d: String,
    r: Vec<(captured::RangeRole, u32, u64, u64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    o: Vec<source_capture::CaptureFileRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    f: Vec<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_sizes: Option<captured::ReceiptSourceSizes>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictV3WireReceipt {
    receipt_id: String,
    content: StrictV3WireContent,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictV3WireContent {
    schema_version: u32,
    source_id: SourceId,
    repo_key: String,
    file: source_capture::CaptureFileRef,
    commit_sha: String,
    producer_contract: String,
    grammar: captured::Grammar,
    grammar_package: String,
    parser_package: String,
    fact_key: FactKey,
    fact_digest: String,
    ranges: Vec<StrictV3WireRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    other_files: Vec<source_capture::CaptureFileRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_sizes: Option<captured::ReceiptSourceSizes>,
    source_scope: String,
    input_closure: StoredClosure,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictV3WireRange {
    role: captured::RangeRole,
    index: u32,
    byte_start: u64,
    byte_end: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<u32>,
}

struct NormalizedReceipt {
    header_id: String,
    header_json: String,
    body_json: String,
}

fn receipt_header_id(header_json: &str) -> String {
    let mut bytes = Vec::with_capacity(RECEIPT_HEADER_DOMAIN.len() + header_json.len());
    bytes.extend_from_slice(RECEIPT_HEADER_DOMAIN);
    bytes.extend_from_slice(header_json.as_bytes());
    core_prov::content_hash(&bytes)
}

fn strip_fact_id_prefix(id: &str, repo_key: &str, path: &str) -> Option<String> {
    if repo_key.is_empty() || path.is_empty() {
        return None;
    }
    let (kind_prefix, rest) = id.split_once(':')?;
    if kind_prefix.is_empty() {
        return None;
    }
    let suffix = rest
        .strip_prefix(repo_key)?
        .strip_prefix('@')?
        .strip_prefix(path)?;
    let compact = format!("{kind_prefix}:{suffix}");
    if restore_fact_id_prefix(&compact, repo_key, path).as_deref() == Some(id) {
        Some(compact)
    } else {
        None
    }
}

fn restore_fact_id_prefix(compact: &str, repo_key: &str, path: &str) -> Option<String> {
    if repo_key.is_empty() || path.is_empty() {
        return None;
    }
    let (kind_prefix, suffix) = compact.split_once(':')?;
    if kind_prefix.is_empty() {
        return None;
    }
    Some(format!("{kind_prefix}:{repo_key}@{path}{suffix}"))
}

fn normalize_v3_receipt(receipt: &impl Citable, canonical_json: &str) -> Option<NormalizedReceipt> {
    let wire: StrictV3WireReceipt = serde_json::from_str(canonical_json).ok()?;
    if !compact_contract(wire.content.schema_version, &wire.content.input_closure)
        || wire.receipt_id != receipt.id()
        || wire.content.source_id != *receipt.source_id()
        || wire.content.file.source_id != *receipt.source_id()
        || wire.content.repo_key != receipt.repo_key()
    {
        return None;
    }
    let repo_key = &wire.content.repo_key;
    let path = &wire.content.file.path;
    let (k, verbatim_k, e, verbatim_e) = match &wire.content.fact_key {
        FactKey::Node { id } => match strip_fact_id_prefix(id, repo_key, path) {
            Some(stripped) => (Some(stripped), None, None, None),
            None => (None, Some(id.clone()), None, None),
        },
        FactKey::Edge {
            source,
            label,
            destination,
        } => match (
            strip_fact_id_prefix(source, repo_key, path),
            strip_fact_id_prefix(destination, repo_key, path),
        ) {
            (Some(src_stripped), Some(dst_stripped)) => (
                None,
                None,
                Some((src_stripped, label.clone(), dst_stripped)),
                None,
            ),
            _ => (
                None,
                None,
                None,
                Some((source.clone(), label.clone(), destination.clone())),
            ),
        },
    };
    let header = ReceiptHeader {
        schema_version: wire.content.schema_version,
        source_id: wire.content.source_id,
        repo_key: wire.content.repo_key,
        file: wire.content.file,
        commit_sha: wire.content.commit_sha,
        producer_contract: wire.content.producer_contract,
        grammar: wire.content.grammar,
        grammar_package: wire.content.grammar_package,
        parser_package: wire.content.parser_package,
        source_scope: wire.content.source_scope,
        input_closure: wire.content.input_closure,
    };
    let body = ReceiptBody {
        k,
        verbatim_k,
        e,
        verbatim_e,
        d: wire.content.fact_digest,
        source_sizes: wire.content.source_sizes,
        o: wire.content.other_files,
        f: wire
            .content
            .ranges
            .iter()
            .enumerate()
            .filter_map(|(position, range)| Some((u32::try_from(position).ok()?, range.file?)))
            .collect(),
        r: wire
            .content
            .ranges
            .into_iter()
            .map(|range| (range.role, range.index, range.byte_start, range.byte_end))
            .collect(),
    };
    let header_json = serde_json::to_string(&header).ok()?;
    let body_json = serde_json::to_string(&body).ok()?;
    if header_json.is_empty()
        || header_json.len() as u64 > MAX_RECEIPT_HEADER_BYTES
        || body_json.is_empty()
        || body_json.len() as u64 > MAX_RECEIPT_BYTES
    {
        return None;
    }
    let header_id = receipt_header_id(&header_json);
    let rebuilt = rebuild_normalized_receipt(
        receipt.id(),
        receipt.source_id().as_str(),
        receipt.repo_key(),
        &header_id,
        &header_json,
        &body_json,
    )
    .ok()?;
    if rebuilt != canonical_json {
        return None;
    }
    Some(NormalizedReceipt {
        header_id,
        header_json,
        body_json,
    })
}

/// Contract v3 carries the legacy closure string and v4 a closure state;
/// both store compactly (ADR-0046 decision 7, AC-1082).
fn compact_contract(schema_version: u32, closure: &StoredClosure) -> bool {
    matches!(
        (schema_version, closure),
        (3, StoredClosure::Legacy(_)) | (4, StoredClosure::Closure(_))
    )
}

fn rebuild_normalized_receipt(
    id: &str,
    source: &str,
    repo: &str,
    header_id: &str,
    header_json: &str,
    body_json: &str,
) -> Result<String, String> {
    if id.is_empty()
        || id.len() > 256
        || header_id.is_empty()
        || header_id.len() > 256
        || header_json.is_empty()
        || header_json.len() as u64 > MAX_RECEIPT_HEADER_BYTES
        || body_json.is_empty()
        || body_json.len() as u64 > MAX_RECEIPT_BYTES
        || header_json.len().saturating_add(id.len()) > captured::MAX_RECEIPT_BYTES
    {
        return Err(storage_error("normalized receipt bounds"));
    }
    if receipt_header_id(header_json) != header_id {
        return Err(storage_error("receipt header id mismatch"));
    }
    let header: ReceiptHeader = serde_json::from_str(header_json).map_err(storage_error)?;
    if !compact_contract(header.schema_version, &header.input_closure)
        || header.source_id.as_str() != source
        || header.file.source_id.as_str() != source
        || header.repo_key != repo
    {
        return Err(storage_error("receipt header ownership"));
    }
    let body: ReceiptBody = serde_json::from_str(body_json).map_err(storage_error)?;
    // The header is bound by its id; the body must be bound by its bytes, so a
    // representation-only edit (whitespace, key order) fails closed too.
    if serde_json::to_string(&body).map_err(storage_error)? != body_json {
        return Err(storage_error("receipt body not canonical"));
    }
    // File indices name ranges in strictly ascending position, each in bounds.
    if body
        .f
        .iter()
        .zip(body.f.iter().skip(1))
        .any(|(a, b)| a.0 >= b.0)
        || body.f.iter().any(|(at, _)| *at as usize >= body.r.len())
    {
        return Err(storage_error("receipt body file index"));
    }
    let path = &header.file.path;
    let fact_key = match (body.k, body.verbatim_k, body.e, body.verbatim_e) {
        (Some(compact), None, None, None) => {
            let restored = restore_fact_id_prefix(&compact, repo, path)
                .ok_or_else(|| storage_error("receipt fact prefix"))?;
            if strip_fact_id_prefix(&restored, repo, path).as_deref() != Some(compact.as_str()) {
                return Err(storage_error("receipt fact prefix roundtrip"));
            }
            FactKey::Node { id: restored }
        }
        (None, Some(verbatim), None, None) => {
            if strip_fact_id_prefix(&verbatim, repo, path).is_some() {
                return Err(storage_error("receipt unstripped fact prefix"));
            }
            FactKey::Node { id: verbatim }
        }
        (None, None, Some((src_compact, label, dst_compact)), None) => {
            let src = restore_fact_id_prefix(&src_compact, repo, path)
                .ok_or_else(|| storage_error("receipt edge source prefix"))?;
            let dst = restore_fact_id_prefix(&dst_compact, repo, path)
                .ok_or_else(|| storage_error("receipt edge destination prefix"))?;
            if strip_fact_id_prefix(&src, repo, path).as_deref() != Some(src_compact.as_str())
                || strip_fact_id_prefix(&dst, repo, path).as_deref() != Some(dst_compact.as_str())
            {
                return Err(storage_error("receipt edge prefix roundtrip"));
            }
            FactKey::Edge {
                source: src,
                label,
                destination: dst,
            }
        }
        (None, None, None, Some((src, label, dst))) => {
            if strip_fact_id_prefix(&src, repo, path).is_some()
                && strip_fact_id_prefix(&dst, repo, path).is_some()
            {
                return Err(storage_error("receipt unstripped edge prefix"));
            }
            FactKey::Edge {
                source: src,
                label,
                destination: dst,
            }
        }
        _ => return Err(storage_error("receipt body fact key")),
    };
    let wire = StrictV3WireReceipt {
        receipt_id: id.to_owned(),
        content: StrictV3WireContent {
            schema_version: header.schema_version,
            source_id: header.source_id,
            repo_key: header.repo_key,
            file: header.file,
            commit_sha: header.commit_sha,
            producer_contract: header.producer_contract,
            grammar: header.grammar,
            grammar_package: header.grammar_package,
            parser_package: header.parser_package,
            fact_key,
            fact_digest: body.d,
            source_sizes: body.source_sizes,
            other_files: body.o,
            ranges: body
                .r
                .into_iter()
                .enumerate()
                .map(
                    |(position, (role, index, byte_start, byte_end))| StrictV3WireRange {
                        role,
                        index,
                        byte_start,
                        byte_end,
                        file: body
                            .f
                            .iter()
                            .find(|(at, _)| *at as usize == position)
                            .map(|(_, file)| *file),
                    },
                )
                .collect(),
            source_scope: header.source_scope,
            input_closure: header.input_closure,
        },
    };
    struct BoundedReceiptJson {
        bytes: Vec<u8>,
    }
    impl std::io::Write for BoundedReceiptJson {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.len() > captured::MAX_RECEIPT_BYTES.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("receipt json bound"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = BoundedReceiptJson {
        bytes: Vec::with_capacity(header_json.len().saturating_add(body_json.len())),
    };
    serde_json::to_writer(&mut writer, &wire).map_err(storage_error)?;
    if writer.bytes.len() > captured::MAX_RECEIPT_BYTES {
        return Err(storage_error("receipt json bound"));
    }
    String::from_utf8(writer.bytes).map_err(storage_error)
}

/// Summary of the forget operation that reclaimed or marked a receipt.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TombstoneSummary {
    pub source_id: String,
    pub forgotten_at: String,
    pub receipts_reclaimed: u64,
    pub receipts_retained_pinned: u64,
}

/// Identity-only receipt retention status (ADR-0046 decision 4).
/// Basis and capture validation stay the caller's job.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ReceiptStatus {
    Present,
    CaptureForgotten {
        tombstone: TombstoneSummary,
    },
    Reclaimed {
        tombstone: TombstoneSummary,
    },
    /// Reclaimed after a later publish of the same source stopped binding it
    /// (ADR-0046 decision 5); the tombstone records the superseding capture.
    Superseded {
        tombstone: TombstoneSummary,
    },
    Unknown,
}

/// Why a reclaim ran; persisted as the tombstone `cause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReclaimCause {
    Forgotten,
    Superseded,
}

impl ReclaimCause {
    fn as_str(self) -> &'static str {
        match self {
            Self::Forgotten => "forgotten",
            Self::Superseded => "superseded",
        }
    }
}

pub fn unavailable() -> String {
    "Captured primary source is unavailable; recover this source again or inspect the separately labeled current working tree.".into()
}

pub fn storage_error(_: impl std::fmt::Display) -> String {
    "Retained source storage is unavailable or invalid; no source was substituted.".into()
}

pub fn source_id(value: &str) -> Result<SourceId, String> {
    if value.len() != 36
        || !value.starts_with("src_")
        || !value[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("Invalid registered source identity".into());
    }
    SourceId::new(value).map_err(storage_error)
}

/// A receipt owner: a registered source, or the observation input identity
/// of one, whose observed receipts it owns (ADR-0079).
fn receipt_owner(value: &str) -> Result<SourceId, String> {
    match value.strip_prefix(crate::observation_inputs::INPUT_REPO_PREFIX) {
        Some(registered) => input_id(registered),
        None => source_id(value),
    }
}

/// The capture identity of a registered source's manifest observation
/// inputs (ADR-0080).
pub fn input_id(registered: &str) -> Result<SourceId, String> {
    source_id(registered)?;
    SourceId::new(crate::observation_inputs::input_source_id(registered)).map_err(storage_error)
}

fn same_entry(a: &cap_std::fs::Metadata, b: &cap_std::fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn private_dir(parent: &Dir, name: &str) -> Result<Dir, String> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use cap_std::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match parent.create_dir_with(name, &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(storage_error(error)),
    }
    let directory = parent.open_dir_nofollow(name).map_err(storage_error)?;
    let metadata = directory.dir_metadata().map_err(storage_error)?;
    let named = parent.symlink_metadata(name).map_err(storage_error)?;
    if !named.is_dir() || !same_entry(&metadata, &named) {
        return Err(storage_error("private directory changed"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Linux directory handles may use O_PATH; the rooted API reopens "."
        // with a permission-capable handle while preserving directory identity.
        directory
            .set_permissions(
                ".",
                cap_std::fs::Permissions::from_std(std::fs::Permissions::from_mode(0o700)),
            )
            .map_err(storage_error)?;
    }
    Ok(directory)
}

fn private_file(parent: &Dir, name: &std::ffi::OsStr, create: bool) -> Result<File, String> {
    let prior = match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => Some(metadata),
        Ok(_) => return Err(storage_error("nonregular private file")),
        Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(storage_error(error)),
    };
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .follow(FollowSymlinks::No)
        .nonblock(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // A racing creator is handled by the same no-follow open and post-open
    // identity check; an existing file is never truncated or recreated.
    let file = if prior.is_none() {
        options.create_new(true);
        match parent.open_with(name, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                options.create_new(false);
                parent.open_with(name, &options).map_err(storage_error)?
            }
            Err(error) => return Err(storage_error(error)),
        }
    } else {
        parent.open_with(name, &options).map_err(storage_error)?
    };
    let opened = file.metadata().map_err(storage_error)?;
    let named = parent.symlink_metadata(name).map_err(storage_error)?;
    if !opened.is_file()
        || opened.nlink() != 1
        || !named.is_file()
        || !same_entry(&opened, &named)
        || prior.is_some_and(|prior| !same_entry(&prior, &opened))
    {
        return Err(storage_error("private file changed"));
    }
    let file = file.into_std();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(storage_error)?;
    }
    Ok(file)
}

fn sqlite_sidecars(directory: &Dir, name: &str) -> Result<(), String> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = format!("{name}{suffix}");
        match directory.symlink_metadata(&sidecar) {
            Ok(_) => {
                drop(private_file(directory, sidecar.as_ref(), false)?);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage_error(error)),
        }
    }
    Ok(())
}

/// Retained directory and database handles detect substitutions between host
/// operations. SQLite itself still opens sidecars by pathname: this is private
/// same-user application storage, not a sandbox against arbitrary concurrent
/// mutation by another process with that user's filesystem authority.
struct PrivateStorage {
    app_path: PathBuf,
    app: Dir,
    root: Dir,
    locks: Dir,
    databases: [File; 2],
}

impl PrivateStorage {
    fn open(app_data: &Path) -> Result<Self, String> {
        let app_path = dunce::canonicalize(app_data).map_err(storage_error)?;
        let app = Dir::open_ambient_dir(&app_path, cap_std::ambient_authority())
            .map_err(storage_error)?;
        let root = private_dir(&app, "retained-source")?;
        let locks = private_dir(&root, "locks")?;
        let databases = [
            private_file(&root, "captures.sqlite".as_ref(), true)?,
            private_file(&root, "receipts.sqlite".as_ref(), true)?,
        ];
        let storage = Self {
            app_path,
            app,
            root,
            locks,
            databases,
        };
        storage.verify()?;
        Ok(storage)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.app_path.join("retained-source").join(name)
    }

    fn verify(&self) -> Result<(), String> {
        let current_app = self.reopen_app()?;
        self.verify_directories(current_app)
    }

    #[cfg(unix)]
    fn reopen_app(&self) -> Result<Dir, String> {
        use std::path::Component;
        let mut components = self.app_path.components();
        if components.next() != Some(Component::RootDir) {
            return Err(storage_error("application path is not canonical"));
        }
        let mut directory =
            Dir::open_ambient_dir("/", cap_std::ambient_authority()).map_err(storage_error)?;
        // Borrow the stored canonical components. A replaced ancestor must
        // fail without resolving or retaining its potentially different path.
        for component in components {
            let Component::Normal(name) = component else {
                return Err(storage_error("application path is not canonical"));
            };
            directory = directory.open_dir_nofollow(name).map_err(storage_error)?;
        }
        Ok(directory)
    }

    #[cfg(not(unix))]
    fn reopen_app(&self) -> Result<Dir, String> {
        if dunce::canonicalize(&self.app_path).map_err(storage_error)? != self.app_path {
            return Err(storage_error("application directory changed"));
        }
        Dir::open_ambient_dir(&self.app_path, cap_std::ambient_authority()).map_err(storage_error)
    }

    fn verify_directories(&self, current_app: Dir) -> Result<(), String> {
        let current_root = self
            .app
            .open_dir_nofollow("retained-source")
            .map_err(storage_error)?;
        let current_locks = self
            .root
            .open_dir_nofollow("locks")
            .map_err(storage_error)?;
        for (expected, current, private) in [
            (&self.app, &current_app, false),
            (&self.root, &current_root, true),
            (&self.locks, &current_locks, true),
        ] {
            let metadata = current.dir_metadata().map_err(storage_error)?;
            if !same_entry(&expected.dir_metadata().map_err(storage_error)?, &metadata) {
                return Err(storage_error("private directory substituted"));
            }
            #[cfg(unix)]
            if private {
                use cap_std::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(storage_error("private directory permissions"));
                }
            }
            #[cfg(not(unix))]
            let _ = private;
        }
        for (name, expected) in ["captures.sqlite", "receipts.sqlite"]
            .into_iter()
            .zip(&self.databases)
        {
            let current = private_file(&self.root, name.as_ref(), false)?;
            // cap-std's metadata carries the Windows file index too (#227).
            let a = cap_std::fs::Metadata::from_file(&current).map_err(storage_error)?;
            let b = cap_std::fs::Metadata::from_file(expected).map_err(storage_error)?;
            if !same_entry(&a, &b) {
                return Err(storage_error("private database substituted"));
            }
            sqlite_sidecars(&self.root, name)?;
        }
        Ok(())
    }
}

/// A fresh OS handle, never unlinked/replaced as part of retention cleanup.
pub struct RetentionGuard {
    _file: File,
    source_id: String,
    app_path: PathBuf,
}

impl Drop for RetentionGuard {
    fn drop(&mut self) {
        // This fresh handle is never intentionally shared with descendants.
        // Explicit release also ends the lock if an unrelated fork briefly
        // inherited the open-file description; close remains the fallback.
        let _ = self._file.unlock();
    }
}

/// How long a receipt-store connection waits for a competing writer's lock;
/// every Cartograph SQLite store uses the same 5 s (AC-0822, #1653).
const RECEIPT_STORE_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct ReceiptStore {
    conn: Connection,
    /// Retained receipt count (and reclaim ledger bound) and payload bytes; tests narrow them.
    pub capacity: (u64, u64),
    #[cfg(any(test, feature = "test-support"))]
    pub fail_next_reclaim: bool,
}

fn require_one(affected: usize) -> Result<(), String> {
    if affected != 1 {
        return Err(storage_error("receipt publication row count"));
    }
    Ok(())
}

fn is_v1_migration_candidate(conn: &Connection) -> Result<bool, String> {
    for (name, ddl) in [
        ("receipts", RECEIPTS_SCHEMA_V1),
        ("receipt_meta", META_SCHEMA_V1),
    ] {
        let exact: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 AND sql=?2)",
                params![name, ddl],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if !exact {
            return Ok(false);
        }
    }
    conn.query_row(
        "SELECT count(*)=1 AND coalesce(min(typeof(version)='integer' AND version=1),0) FROM receipt_meta",
        [],
        |row| row.get(0),
    )
    .map_err(storage_error)
}

fn validate_v1_receipts(conn: &Connection) -> Result<(), String> {
    let count: i64 = conn
        .query_row("SELECT count(*) FROM receipts", [], |row| row.get(0))
        .map_err(storage_error)?;
    if count < 0 || count > MAX_RECEIPTS as i64 {
        return Err(storage_error("receipt inventory count"));
    }
    let invalid: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipts WHERE typeof(id) != 'text'
         OR length(CAST(id AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(source_id) != 'text' OR length(CAST(source_id AS BLOB)) != 36
         OR typeof(repo_key) != 'text' OR length(CAST(repo_key AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(payload) != 'text' OR length(CAST(payload AS BLOB)) NOT BETWEEN 1 AND ?1)",
            [MAX_RECEIPT_BYTES as i64],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if invalid {
        return Err(storage_error("receipt metadata bounds"));
    }
    let bytes: i64 = conn
        .query_row(
            "SELECT coalesce(sum(length(CAST(payload AS BLOB))),0) FROM receipts",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if bytes < 0 || bytes > MAX_RECEIPT_STORE_BYTES as i64 {
        return Err(storage_error("receipt inventory bytes"));
    }
    Ok(())
}

fn migrate_v1_to_v2(tx: &Connection) -> Result<(), String> {
    validate_v1_receipts(tx)?;
    tx.execute_batch(RECEIPTS_ALTER_V2).map_err(storage_error)?;
    tx.execute_batch(TOMBSTONES_SCHEMA_V2)
        .map_err(storage_error)?;
    tx.execute_batch(RECLAIMS_SCHEMA).map_err(storage_error)?;
    create_v3_receipt_indexes(tx)?;
    tx.execute_batch("DROP TABLE receipt_meta")
        .map_err(storage_error)?;
    tx.execute_batch(META_SCHEMA_V2).map_err(storage_error)?;
    require_one(
        tx.execute("INSERT INTO receipt_meta VALUES (2)", [])
            .map_err(storage_error)?,
    )
}

/// True when the store is exactly schema v2, whose tombstones predate
/// supersession and therefore all record a forget.
fn is_v2_migration_candidate(conn: &Connection) -> Result<bool, String> {
    for (name, ddl) in [
        ("receipt_tombstones", TOMBSTONES_SCHEMA_V2),
        ("receipt_meta", META_SCHEMA_V2),
    ] {
        let exact: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 AND sql=?2)",
                params![name, ddl],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if !exact {
            return Ok(false);
        }
    }
    conn.query_row(
        "SELECT count(*)=1 AND coalesce(min(typeof(version)='integer' AND version=2),0) FROM receipt_meta",
        [],
        |row| row.get(0),
    )
    .map_err(storage_error)
}

fn migrate_v2_to_v3(tx: &Connection) -> Result<(), String> {
    tx.execute_batch(TOMBSTONES_ALTER_V3)
        .map_err(storage_error)?;
    tx.execute_batch("DROP TABLE receipt_meta")
        .map_err(storage_error)?;
    tx.execute_batch(META_SCHEMA_V3).map_err(storage_error)?;
    require_one(
        tx.execute("INSERT INTO receipt_meta VALUES (3)", [])
            .map_err(storage_error)?,
    )
}

/// True when the store is schema v3, whose receipt rows all carry legacy full
/// canonical JSON in `payload` before `receipt_headers` and `header_id`.
fn is_v3_migration_candidate(conn: &Connection) -> Result<bool, String> {
    for (name, ddl) in [
        ("receipts", RECEIPTS_SCHEMA_V3),
        ("receipt_tombstones", TOMBSTONES_SCHEMA),
        ("receipt_reclaims", RECLAIMS_SCHEMA),
        ("receipt_meta", META_SCHEMA_V3),
    ] {
        let exact: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 AND sql=?2)",
                params![name, ddl],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if !exact {
            return Ok(false);
        }
    }
    conn.query_row(
        "SELECT count(*)=1 AND coalesce(min(typeof(version)='integer' AND version=3),0) FROM receipt_meta",
        [],
        |row| row.get(0),
    )
    .map_err(storage_error)
}

fn migrate_v3_to_v4(tx: &Connection) -> Result<(), String> {
    tx.execute_batch(RECEIPTS_ALTER_V4).map_err(storage_error)?;
    tx.execute_batch(HEADERS_SCHEMA).map_err(storage_error)?;
    tx.execute_batch(RECEIPT_INDEXES[3].1)
        .map_err(storage_error)?;
    tx.execute_batch("DROP TABLE receipt_meta")
        .map_err(storage_error)?;
    tx.execute_batch(META_SCHEMA_V4).map_err(storage_error)?;
    require_one(
        tx.execute("INSERT INTO receipt_meta VALUES (4)", [])
            .map_err(storage_error)?,
    )
}

/// True when the store is exactly schema v4, before input closures: every
/// row's closure state is inside its payload or header, none names a stored
/// closure.
fn is_v4_migration_candidate(conn: &Connection) -> Result<bool, String> {
    for (name, ddl) in [
        ("receipts", RECEIPTS_SCHEMA_V4),
        ("receipt_headers", HEADERS_SCHEMA),
        ("receipt_meta", META_SCHEMA_V4),
    ] {
        let exact: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 AND sql=?2)",
                params![name, ddl],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if !exact {
            return Ok(false);
        }
    }
    conn.query_row(
        "SELECT count(*)=1 AND coalesce(min(typeof(version)='integer' AND version=4),0) FROM receipt_meta",
        [],
        |row| row.get(0),
    )
    .map_err(storage_error)
}

fn migrate_v4_to_v5(tx: &Connection) -> Result<(), String> {
    tx.execute_batch(CLOSURES_SCHEMA).map_err(storage_error)?;
    tx.execute_batch(RECEIPTS_ALTER_V5).map_err(storage_error)?;
    tx.execute_batch(RECEIPT_INDEXES[4].1)
        .map_err(storage_error)?;
    tx.execute_batch("DROP TABLE receipt_meta")
        .map_err(storage_error)?;
    tx.execute_batch(META_SCHEMA).map_err(storage_error)?;
    require_one(
        tx.execute("INSERT INTO receipt_meta VALUES (5)", [])
            .map_err(storage_error)?,
    )
}

fn receipt_schema(conn: &Connection) -> Result<(), String> {
    let objects: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if objects != 6 + RECEIPT_INDEXES.len() as i64 {
        return Err(storage_error("receipt schema objects"));
    }
    for (name, ddl) in RECEIPT_INDEXES {
        let exact: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1 AND sql=?2)",
                params![name, ddl],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if !exact {
            return Err(storage_error("receipt schema"));
        }
    }
    for (name, ddl) in [
        ("receipts", RECEIPTS_SCHEMA),
        ("receipt_headers", HEADERS_SCHEMA),
        ("input_closures", CLOSURES_SCHEMA),
        ("receipt_tombstones", TOMBSTONES_SCHEMA),
        ("receipt_reclaims", RECLAIMS_SCHEMA),
        ("receipt_meta", META_SCHEMA),
    ] {
        let exact: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 AND sql=?2)",
                params![name, ddl],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if !exact {
            return Err(storage_error("receipt schema"));
        }
    }
    let valid: bool = conn
        .query_row(
            "SELECT count(*)=1 AND coalesce(min(typeof(version)='integer' AND version=5),0) FROM receipt_meta",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if !valid {
        return Err(storage_error("receipt version"));
    }
    Ok(())
}

// Metadata-only preallocation checks apply even after open; indexed ownership
// fields are not trusted just because a SQL predicate selected their row.
// An owner is a registered source or its observation input identity
// (`input/<source id>`, ADR-0079).
fn receipt_storage(conn: &Connection) -> Result<(u64, u64), String> {
    receipt_schema(conn)?;
    let count: i64 = conn
        .query_row("SELECT count(*) FROM receipts", [], |row| row.get(0))
        .map_err(storage_error)?;
    if count < 0 || count > MAX_RECEIPTS as i64 {
        return Err(storage_error("receipt inventory count"));
    }
    let header_count: i64 = conn
        .query_row("SELECT count(*) FROM receipt_headers", [], |row| row.get(0))
        .map_err(storage_error)?;
    if header_count < 0 || header_count > count {
        return Err(storage_error("receipt header count"));
    }
    let invalid_headers: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipt_headers WHERE typeof(id) != 'text'
         OR length(CAST(id AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(source_id) != 'text' OR NOT ((length(CAST(source_id AS BLOB)) = 36 AND source_id GLOB 'src_*'
                  AND trim(substr(source_id, 5), '0123456789abcdef') = '')
              OR (length(CAST(source_id AS BLOB)) = 42 AND source_id GLOB 'input/src_*'
                  AND trim(substr(source_id, 11), '0123456789abcdef') = ''))
         OR typeof(repo_key) != 'text' OR length(CAST(repo_key AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(header) != 'text' OR length(CAST(header AS BLOB)) NOT BETWEEN 1 AND ?1
         OR NOT EXISTS(SELECT 1 FROM receipts WHERE receipts.header_id = receipt_headers.id))",
            [MAX_RECEIPT_HEADER_BYTES as i64],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if invalid_headers {
        return Err(storage_error("receipt header bounds"));
    }
    let closure_count: i64 = conn
        .query_row("SELECT count(*) FROM input_closures", [], |row| row.get(0))
        .map_err(storage_error)?;
    if closure_count < 0 || closure_count > count {
        return Err(storage_error("input closure count"));
    }
    // Each closure is a well-formed id owned like the receipts that name it.
    let invalid_closures: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM input_closures WHERE typeof(id) != 'text'
         OR length(CAST(id AS BLOB)) != 75 OR id NOT GLOB 'closure-v1:*'
         OR trim(substr(id, 12), '0123456789abcdef') != ''
         OR typeof(source_id) != 'text' OR NOT ((length(CAST(source_id AS BLOB)) = 36 AND source_id GLOB 'src_*'
                  AND trim(substr(source_id, 5), '0123456789abcdef') = '')
              OR (length(CAST(source_id AS BLOB)) = 42 AND source_id GLOB 'input/src_*'
                  AND trim(substr(source_id, 11), '0123456789abcdef') = ''))
         OR typeof(repo_key) != 'text' OR length(CAST(repo_key AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(payload) != 'text' OR length(CAST(payload AS BLOB)) NOT BETWEEN 1 AND ?1
         OR NOT EXISTS(SELECT 1 FROM receipts WHERE receipts.closure_id = input_closures.id))",
            [captured::MAX_CLOSURE_BYTES as i64],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if invalid_closures {
        return Err(storage_error("input closure bounds"));
    }
    let invalid: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipts WHERE typeof(id) != 'text'
         OR length(CAST(id AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(source_id) != 'text' OR NOT ((length(CAST(source_id AS BLOB)) = 36 AND source_id GLOB 'src_*'
                  AND trim(substr(source_id, 5), '0123456789abcdef') = '')
              OR (length(CAST(source_id AS BLOB)) = 42 AND source_id GLOB 'input/src_*'
                  AND trim(substr(source_id, 11), '0123456789abcdef') = ''))
         OR typeof(repo_key) != 'text' OR length(CAST(repo_key AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(payload) != 'text' OR length(CAST(payload AS BLOB)) NOT BETWEEN 1 AND ?1
         OR (capture_forgotten_tombstone IS NOT NULL
             AND (typeof(capture_forgotten_tombstone) != 'integer'
                  OR NOT EXISTS(SELECT 1 FROM receipt_tombstones
                                WHERE receipt_tombstones.id = receipts.capture_forgotten_tombstone
                                  AND receipt_tombstones.source_id = receipts.source_id)))
         OR (header_id IS NOT NULL
             AND (typeof(header_id) != 'text'
                  OR length(CAST(header_id AS BLOB)) NOT BETWEEN 1 AND 256
                  OR NOT EXISTS(SELECT 1 FROM receipt_headers
                                WHERE receipt_headers.id = receipts.header_id
                                  AND receipt_headers.source_id = receipts.source_id
                                  AND receipt_headers.repo_key = receipts.repo_key)))
         OR (closure_id IS NOT NULL
             AND (typeof(closure_id) != 'text'
                  OR NOT EXISTS(SELECT 1 FROM input_closures
                                WHERE input_closures.id = receipts.closure_id
                                  AND input_closures.source_id = receipts.source_id
                                  AND input_closures.repo_key = receipts.repo_key))))",
            [MAX_RECEIPT_BYTES as i64],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if invalid {
        return Err(storage_error("receipt metadata bounds"));
    }
    let bytes: i64 = conn
        .query_row(
            "SELECT (SELECT coalesce(sum(length(CAST(payload AS BLOB))),0) FROM receipts)
                  + (SELECT coalesce(sum(length(CAST(header AS BLOB))),0) FROM receipt_headers)
                  + (SELECT coalesce(sum(length(CAST(payload AS BLOB))),0) FROM input_closures)",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if bytes < 0 || bytes > MAX_RECEIPT_STORE_BYTES as i64 {
        return Err(storage_error("receipt inventory bytes"));
    }
    let reclaim_count: i64 = conn
        .query_row("SELECT count(*) FROM receipt_reclaims", [], |row| {
            row.get(0)
        })
        .map_err(storage_error)?;
    if reclaim_count < 0 || reclaim_count > MAX_RECEIPTS as i64 {
        return Err(storage_error("receipt reclaim count"));
    }
    let invalid_reclaims: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipt_reclaims WHERE typeof(receipt_id) != 'text'
         OR length(CAST(receipt_id AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(tombstone_id) != 'integer' OR tombstone_id <= 0
         OR NOT EXISTS(SELECT 1 FROM receipt_tombstones WHERE id = receipt_reclaims.tombstone_id))",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if invalid_reclaims {
        return Err(storage_error("receipt reclaim bounds"));
    }
    let invalid_tombstones: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipt_tombstones WHERE typeof(id) != 'integer' OR id <= 0
         OR typeof(source_id) != 'text' OR NOT ((length(CAST(source_id AS BLOB)) = 36 AND source_id GLOB 'src_*'
                  AND trim(substr(source_id, 5), '0123456789abcdef') = '')
              OR (length(CAST(source_id AS BLOB)) = 42 AND source_id GLOB 'input/src_*'
                  AND trim(substr(source_id, 11), '0123456789abcdef') = ''))
         OR typeof(repo_key) != 'text' OR length(CAST(repo_key AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(capture_ids) != 'text' OR length(CAST(capture_ids AS BLOB)) NOT BETWEEN 2 AND ?1
         OR typeof(capture_ids_truncated) != 'integer' OR capture_ids_truncated NOT IN (0, 1)
         OR (capture_ids_truncated = 1 AND capture_ids != '[]')
         OR typeof(forgotten_at) != 'text' OR length(CAST(forgotten_at AS BLOB)) != 20
         OR forgotten_at NOT GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z'
         OR typeof(receipts_reclaimed) != 'integer' OR receipts_reclaimed NOT BETWEEN 0 AND ?2
         OR typeof(receipts_retained_pinned) != 'integer' OR receipts_retained_pinned NOT BETWEEN 0 AND ?2
         OR (receipts_reclaimed = 0 AND receipts_retained_pinned = 0)
         OR typeof(cause) != 'text' OR cause NOT IN ('forgotten', 'superseded')
         OR (cause = 'superseded' AND (receipts_retained_pinned != 0
             OR EXISTS(SELECT 1 FROM receipts WHERE capture_forgotten_tombstone = receipt_tombstones.id)))
         OR (NOT EXISTS(SELECT 1 FROM receipt_reclaims WHERE tombstone_id = receipt_tombstones.id)
             AND NOT EXISTS(SELECT 1 FROM receipts WHERE capture_forgotten_tombstone = receipt_tombstones.id)))",
            params![MAX_TOMBSTONE_CAPTURE_IDS_BYTES as i64, MAX_RECEIPTS as i64],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if invalid_tombstones {
        return Err(storage_error("receipt tombstone bounds"));
    }
    let tombstone_bytes: i64 = conn
        .query_row(
            "SELECT coalesce(sum(length(CAST(capture_ids AS BLOB))),0) FROM receipt_tombstones",
            [],
            |row| row.get(0),
        )
        .map_err(storage_error)?;
    if tombstone_bytes < 0 || tombstone_bytes > MAX_RECEIPT_STORE_BYTES as i64 {
        return Err(storage_error("receipt tombstone bytes"));
    }
    // SQLite integers are signed; convert only after the nonnegative bounds.
    Ok((count as u64, bytes as u64))
}

fn encode_capture_ids(capture_ids: &[String]) -> Result<(String, i64), String> {
    struct BoundedJson {
        bytes: Vec<u8>,
    }
    impl std::io::Write for BoundedJson {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if buf.len()
                > (MAX_TOMBSTONE_CAPTURE_IDS_BYTES as usize).saturating_sub(self.bytes.len())
            {
                return Err(std::io::Error::other("capture_ids bound"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let ordered: std::collections::BTreeSet<&str> =
        capture_ids.iter().map(String::as_str).collect();
    let mut writer = BoundedJson { bytes: Vec::new() };
    match serde_json::to_writer(&mut writer, &ordered) {
        Ok(()) => {
            let json = String::from_utf8(writer.bytes).map_err(storage_error)?;
            Ok((json, 0))
        }
        Err(error) if error.is_io() => Ok(("[]".into(), 1)),
        Err(error) => Err(storage_error(error)),
    }
}

/// The input closure a receipt being retained names as established.
struct ReceiptClosureRow {
    id: String,
    /// Its canonical bytes' length, charged against the source's share.
    bytes: u64,
    /// Its canonical JSON when the store does not hold it yet.
    new_payload: Option<String>,
}

/// `closure`'s canonical JSON, once it is proven its own canonical record
/// for `source` (AC-1082).
fn closure_payload(
    source: &RegisteredSource,
    closure: &captured::InputClosureV1,
) -> Result<String, String> {
    let bytes = closure.canonical_bytes().map_err(storage_error)?;
    if captured::InputClosureV1::decode(&bytes).map_err(storage_error)? != *closure
        || closure.parameters.repo_key != source.repo_key
    {
        return Err(storage_error("input closure"));
    }
    String::from_utf8(bytes).map_err(storage_error)
}

/// Index `closures` by id after proving each is its own canonical record
/// for `source` (AC-1082).
fn supplied_closures<'a>(
    source: &RegisteredSource,
    closures: &'a [captured::InputClosureV1],
) -> Result<std::collections::BTreeMap<&'a str, (&'a captured::InputClosureV1, String)>, String> {
    let mut supplied = std::collections::BTreeMap::new();
    for closure in closures {
        let payload = closure_payload(source, closure)?;
        supplied.insert(closure.closure_id.as_str(), (closure, payload));
    }
    Ok(supplied)
}

/// The closure `receipt` names as established, if any: already stored for
/// this source, or supplied with the batch or carried by the receipt
/// (AC-1084) when it has no gap and does not name this receipt's own fact
/// upstream. A receipt naming a closure none of them holds is refused,
/// never retained without it.
fn receipt_closure(
    tx: &Connection,
    source: &RegisteredSource,
    receipt: &impl Citable,
    supplied: &std::collections::BTreeMap<&str, (&captured::InputClosureV1, String)>,
) -> Result<Option<ReceiptClosureRow>, String> {
    let Some(captured::ReceiptClosure::Established { closure_id }) = receipt.closure() else {
        return Ok(None);
    };
    let stored: Option<(String, String, i64)> = tx
        .query_row(
            "SELECT source_id,repo_key,length(CAST(payload AS BLOB)) FROM input_closures WHERE id=?1",
            [closure_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(storage_error)?;
    if let Some((stored_source, stored_repo, bytes)) = stored {
        if stored_source != source.source_id || stored_repo != source.repo_key {
            return Err(storage_error("input closure collision"));
        }
        if let Some((_, payload)) = supplied.get(closure_id.as_str()) {
            let same: bool = tx
                .query_row(
                    "SELECT payload = ?2 FROM input_closures WHERE id=?1",
                    params![closure_id, payload],
                    |row| row.get(0),
                )
                .map_err(storage_error)?;
            if !same {
                return Err(storage_error("input closure collision"));
            }
        }
        return Ok(Some(ReceiptClosureRow {
            id: closure_id.clone(),
            bytes: u64::try_from(bytes).map_err(storage_error)?,
            new_payload: None,
        }));
    }
    let carried;
    let (closure, payload) = match (supplied.get(closure_id.as_str()), receipt.input_closure()) {
        (Some((closure, payload)), _) => (*closure, payload),
        (None, Some(closure)) if closure.closure_id == *closure_id => {
            carried = closure_payload(source, closure)?;
            (closure, &carried)
        }
        _ => return Err(storage_error("receipt input closure missing")),
    };
    if !closure.gaps().is_empty() {
        return Err(storage_error("receipt input closure not established"));
    }
    closure
        .validate_for(receipt.fact_key())
        .map_err(storage_error)?;
    Ok(Some(ReceiptClosureRow {
        id: closure_id.clone(),
        bytes: payload.len() as u64,
        new_payload: Some(payload.clone()),
    }))
}

/// Store a retained receipt's new closure before the receipt row names it.
fn insert_closure(
    tx: &Connection,
    source: &RegisteredSource,
    closure: Option<&ReceiptClosureRow>,
) -> Result<(), String> {
    let Some(ReceiptClosureRow {
        id,
        new_payload: Some(payload),
        ..
    }) = closure
    else {
        return Ok(());
    };
    // An earlier receipt of this batch may have stored it already.
    tx.execute(
        "INSERT INTO input_closures (id,source_id,repo_key,payload) VALUES (?1,?2,?3,?4)
         ON CONFLICT(id) DO NOTHING",
        params![id, source.source_id, source.repo_key, payload],
    )
    .map_err(storage_error)?;
    Ok(())
}

// The caller retains a transaction after receipt_storage validated all lengths.
fn stored_receipt(conn: &Connection, id: &str) -> Result<Receipt, String> {
    #[cfg(any(test, feature = "test-support"))]
    STORED_RECEIPT_DECODES.with(|count| count.set(count.get() + 1));
    pinned::stored_receipt(conn, id).map_err(|error| error.to_string())
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Full receipt rebuilds and decodes on this thread, so tests can bound
    /// how many stored rows a store operation hydrates (AC-0836).
    static STORED_RECEIPT_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ReceiptStore {
    fn open(path: &Path) -> Result<Self, String> {
        let parent_path =
            dunce::canonicalize(path.parent().ok_or_else(unavailable)?).map_err(storage_error)?;
        let parent = Dir::open_ambient_dir(&parent_path, cap_std::ambient_authority())
            .map_err(storage_error)?;
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(unavailable)?;
        let checked = private_file(&parent, name.as_ref(), true)?;
        sqlite_sidecars(&parent, name)?;
        let mut conn = Connection::open_with_flags(
            parent_path.join(name),
            OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(storage_error)?;
        conn.busy_timeout(RECEIPT_STORE_BUSY_TIMEOUT)
            .map_err(storage_error)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(storage_error)?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(storage_error)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let count: i64 = tx
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if count == 0 {
            tx.execute_batch(RECEIPTS_SCHEMA).map_err(storage_error)?;
            tx.execute_batch(HEADERS_SCHEMA).map_err(storage_error)?;
            tx.execute_batch(CLOSURES_SCHEMA).map_err(storage_error)?;
            tx.execute_batch(TOMBSTONES_SCHEMA).map_err(storage_error)?;
            tx.execute_batch(RECLAIMS_SCHEMA).map_err(storage_error)?;
            create_receipt_indexes(&tx)?;
            tx.execute_batch(META_SCHEMA).map_err(storage_error)?;
            require_one(
                tx.execute("INSERT INTO receipt_meta VALUES (5)", [])
                    .map_err(storage_error)?,
            )?;
        } else {
            if is_v1_migration_candidate(&tx)? {
                migrate_v1_to_v2(&tx)?;
            }
            if is_v2_migration_candidate(&tx)? {
                migrate_v2_to_v3(&tx)?;
            }
            if is_v3_migration_candidate(&tx)? {
                migrate_v3_to_v4(&tx)?;
            }
            if is_v4_migration_candidate(&tx)? {
                migrate_v4_to_v5(&tx)?;
            }
        }
        receipt_storage(&tx)?;
        tx.commit().map_err(storage_error)?;
        let current = private_file(&parent, name.as_ref(), false)?;
        let a = cap_std::fs::Metadata::from_file(&checked).map_err(storage_error)?;
        let b = cap_std::fs::Metadata::from_file(&current).map_err(storage_error)?;
        if !same_entry(&a, &b) {
            return Err(storage_error("receipt database changed"));
        }
        sqlite_sidecars(&parent, name)?;
        Ok(Self {
            conn,
            capacity: (MAX_RECEIPTS, MAX_RECEIPT_STORE_BYTES),
            #[cfg(any(test, feature = "test-support"))]
            fail_next_reclaim: false,
        })
    }

    /// Retain `receipts` in order until the store's capacity is reached
    /// (#722). Integrity failures still reject the whole batch; a receipt that
    /// only lacks room is left out and counted under its file path, and the
    /// ids actually retained (new or already stored) are returned with them.
    #[cfg(any(test, feature = "test-support"))]
    fn persist(
        &mut self,
        capture: &Capture,
        source: &RegisteredSource,
        receipts: &[impl Citable],
    ) -> Result<RetainedReceipts, String> {
        let share = self.capacity;
        self.persist_within(capture, source, receipts, &[], share)
    }

    /// [`Self::persist`], also leaving out each receipt that would take the
    /// batch's retained rows past `share`, the source's (count, bytes) ceiling
    /// (#725). Rows from the source's earlier recoveries still count against
    /// the store-wide bound but not against the share; nothing is evicted.
    ///
    /// A contract-v4 receipt whose closure is established is retained only
    /// with that closure, in the same transaction: one of `closures` or one
    /// the source already stored (AC-1082). A closure is charged like a
    /// header, once, and only when a receipt that names it is retained.
    fn persist_within(
        &mut self,
        capture: &Capture,
        source: &RegisteredSource,
        receipts: &[impl Citable],
        closures: &[captured::InputClosureV1],
        share: (u64, u64),
    ) -> Result<RetainedReceipts, String> {
        let supplied = supplied_closures(source, closures)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let (mut count, mut bytes) = receipt_storage(&tx)?;
        let mut charged_closures = std::collections::BTreeSet::new();
        // The batch's retained rows, each normalized header counted once
        // whether new or already stored, measured against `share`.
        let (mut source_count, mut source_bytes) = (0u64, 0u64);
        let mut charged_headers = std::collections::BTreeSet::new();
        let mut retained = RetainedReceipts::default();
        // Each receipt is checked, serialized and admitted in turn, so one
        // canonical form is held at a time; the batch keeps only each id's
        // content hash, to reject a repeat that differs (#725). A rejection
        // drops the transaction, so nothing of the batch is kept.
        let mut batch = std::collections::BTreeMap::new();
        for receipt in receipts {
            if receipt.source_id().as_str() != source.source_id
                || receipt.repo_key() != source.repo_key
            {
                return Err(storage_error("receipt ownership"));
            }
            if capture
                .file(&receipt.file().path)
                .map_err(storage_error)?
                .reference()
                != receipt.file()
            {
                return Err(storage_error("receipt capture"));
            }
            for span in receipt.captured_spans() {
                capture.read_span(&span).map_err(storage_error)?;
            }
            let json = receipt.to_json().map_err(storage_error)?;
            if json.len() as u64 > MAX_RECEIPT_BYTES {
                return Err(storage_error("receipt bounds"));
            }
            let hash = core_prov::content_hash(json.as_bytes());
            match batch.get(receipt.id()) {
                Some(old) if old != &hash => return Err(storage_error("receipt batch collision")),
                Some(_) => continue,
                None => {
                    batch.insert(receipt.id().to_owned(), hash);
                }
            }
            let normalized = normalize_v3_receipt(receipt, &json);
            let closure = receipt_closure(&tx, source, receipt, &supplied)?;
            let (id, path, fact_key, fact_digest, canonical_json) = (
                receipt.id().to_owned(),
                receipt.file().path.clone(),
                receipt.fact_key().clone(),
                receipt.fact_digest().to_owned(),
                json,
            );
            let old: Option<(String, String, String, Option<String>)> = tx
                .query_row(
                    "SELECT source_id,repo_key,payload,header_id FROM receipts WHERE id=?1",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(storage_error)?;
            // A closure not yet charged to this batch costs its bytes against
            // the share; one not yet stored costs them store-wide too.
            let (closure_id, closure_row_cost, closure_source_cost) = match &closure {
                Some(closure) => (
                    Some(closure.id.clone()),
                    closure.new_payload.as_ref().map_or(0, |p| p.len() as u64),
                    if charged_closures.contains(&closure.id) {
                        0
                    } else {
                        closure.bytes
                    },
                ),
                None => (None, 0, 0),
            };
            if let Some((old_source, old_repo, old_payload, old_header_id)) = old {
                source_count += 1;
                source_bytes += old_payload.len() as u64 + closure_source_cost;
                if let Some(closure_id) = &closure_id {
                    charged_closures.insert(closure_id.clone());
                }
                let old_json = if let Some(header_id) = old_header_id {
                    let (header_source, header_repo, header_json): (String, String, String) = tx
                        .query_row(
                            "SELECT source_id,repo_key,header FROM receipt_headers WHERE id=?1",
                            [&header_id],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                        )
                        .map_err(storage_error)?;
                    if header_source != old_source || header_repo != old_repo {
                        return Err(storage_error("receipt header ownership"));
                    }
                    if charged_headers.insert(header_id.clone()) {
                        source_bytes += header_json.len() as u64;
                    }
                    rebuild_normalized_receipt(
                        &id,
                        &old_source,
                        &old_repo,
                        &header_id,
                        &header_json,
                        &old_payload,
                    )?
                } else {
                    old_payload
                };
                if old_source != source.source_id
                    || old_repo != source.repo_key
                    || old_json != canonical_json
                {
                    return Err(storage_error("receipt collision"));
                }
                // The same bytes were captured again, so a pinned receipt that
                // was marked forgotten is present once more.
                tx.execute(
                    "UPDATE receipts SET capture_forgotten_tombstone = NULL WHERE id = ?1",
                    [&id],
                )
                .map_err(storage_error)?;
            } else if let Some(normalized) = normalized {
                let existing_header: Option<(String, String, String)> = tx
                    .query_row(
                        "SELECT source_id,repo_key,header FROM receipt_headers WHERE id=?1",
                        [&normalized.header_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(storage_error)?;
                let header_cost = match &existing_header {
                    Some((header_source, header_repo, header_json)) => {
                        if header_source != &source.source_id
                            || header_repo != &source.repo_key
                            || header_json != &normalized.header_json
                        {
                            return Err(storage_error("receipt header collision"));
                        }
                        0
                    }
                    None => normalized.header_json.len() as u64,
                };
                let row_cost = normalized.body_json.len() as u64 + header_cost + closure_row_cost;
                let source_cost = normalized.body_json.len() as u64
                    + closure_source_cost
                    + if charged_headers.contains(&normalized.header_id) {
                        0
                    } else {
                        normalized.header_json.len() as u64
                    };
                if count + 1 > self.capacity.0
                    || bytes + row_cost > self.capacity.1
                    || source_count + 1 > share.0
                    || source_bytes + source_cost > share.1
                {
                    *retained.omitted.entry(path).or_default() += 1;
                    retained.omitted_facts.push((fact_key, fact_digest));
                    continue;
                }
                if existing_header.is_none() {
                    require_one(
                        tx.execute(
                            "INSERT INTO receipt_headers (id,source_id,repo_key,header) VALUES (?1,?2,?3,?4)",
                            params![
                                normalized.header_id,
                                source.source_id,
                                source.repo_key,
                                normalized.header_json,
                            ],
                        )
                        .map_err(storage_error)?,
                    )?;
                }
                count += 1;
                bytes += row_cost;
                source_count += 1;
                source_bytes += source_cost;
                charged_headers.insert(normalized.header_id.clone());
                insert_closure(&tx, source, closure.as_ref())?;
                require_one(
                    tx.execute(
                        "INSERT INTO receipts (id,source_id,repo_key,payload,header_id,closure_id) VALUES (?1,?2,?3,?4,?5,?6)",
                        params![
                            id,
                            source.source_id,
                            source.repo_key,
                            normalized.body_json,
                            normalized.header_id,
                            closure_id,
                        ],
                    )
                    .map_err(storage_error)?,
                )?;
            } else if count + 1 > self.capacity.0
                || bytes + canonical_json.len() as u64 + closure_row_cost > self.capacity.1
                || source_count + 1 > share.0
                || source_bytes + canonical_json.len() as u64 + closure_source_cost > share.1
            {
                *retained.omitted.entry(path).or_default() += 1;
                retained.omitted_facts.push((fact_key, fact_digest));
                continue;
            } else {
                count += 1;
                bytes += canonical_json.len() as u64 + closure_row_cost;
                source_count += 1;
                source_bytes += canonical_json.len() as u64 + closure_source_cost;
                insert_closure(&tx, source, closure.as_ref())?;
                require_one(
                    tx.execute(
                        "INSERT INTO receipts (id,source_id,repo_key,payload,closure_id) VALUES (?1,?2,?3,?4,?5)",
                        params![id, source.source_id, source.repo_key, canonical_json, closure_id],
                    )
                    .map_err(storage_error)?,
                )?;
            }
            if let Some(closure_id) = closure_id {
                charged_closures.insert(closure_id);
            }
            retained.ids.insert(id);
        }
        delete_orphan_tombstones(&tx)?;
        delete_orphan_headers(&tx)?;
        tx.commit().map_err(storage_error)?;
        retained.charged = (source_count, source_bytes);
        Ok(retained)
    }

    pub fn get(&self, id: &str) -> Result<Receipt, String> {
        if id.is_empty() || id.len() > 256 {
            return Err(unavailable());
        }
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        receipt_storage(&tx)?;
        let receipt = stored_receipt(&tx, id)?;
        tx.commit().map_err(storage_error)?;
        Ok(receipt)
    }

    /// The stored input closure `id`, decoded from its canonical bytes
    /// (AC-1082). A stored record that is not its own canonical form, or
    /// names another id, is invalid.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn closure(&self, id: &str) -> Result<captured::InputClosureV1, String> {
        if !captured::is_closure_id(id) {
            return Err(unavailable());
        }
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        receipt_storage(&tx)?;
        let payload: Option<String> = tx
            .query_row(
                "SELECT payload FROM input_closures WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        let closure = captured::InputClosureV1::decode(payload.ok_or_else(unavailable)?.as_bytes())
            .map_err(storage_error)?;
        if closure.closure_id != id {
            return Err(storage_error("input closure id"));
        }
        tx.commit().map_err(storage_error)?;
        Ok(closure)
    }

    pub fn ids(&self, source: &str) -> Result<Vec<String>, String> {
        receipt_owner(source)?;
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        receipt_storage(&tx)?;
        // ADR-0046 decision 5 (#1233): only this source's rows are rebuilt
        // and decoded, so the work does not grow with other sources' rows.
        // A row whose `source_id` column disagrees with its decoded payload
        // fails closed when the source that column names is read.
        let mut ids = Vec::new();
        {
            let mut statement = tx
                .prepare("SELECT id FROM receipts WHERE source_id = ?1 ORDER BY id")
                .map_err(storage_error)?;
            let mut rows = statement.query([source]).map_err(storage_error)?;
            while let Some(row) = rows.next().map_err(storage_error)? {
                let id: String = row.get(0).map_err(storage_error)?;
                let receipt = stored_receipt(&tx, &id)?;
                if receipt.source_id().as_str() != source {
                    return Err(storage_error("receipt ownership"));
                }
                ids.push(id);
            }
        }
        tx.commit().map_err(storage_error)?;
        Ok(ids)
    }

    /// Reclaim unpinned receipts for `source` and mark pinned receipts as
    /// capture-forgotten in one atomic transaction (ADR-0046 decision 3).
    pub fn reclaim_forgotten(
        &mut self,
        source: &RegisteredSource,
        capture_ids: &[String],
        pins: &std::collections::BTreeSet<String>,
    ) -> Result<(), String> {
        self.reclaim(source, capture_ids, pins, ReclaimCause::Forgotten)
            .map(|_| ())
    }

    /// Reclaim `source` receipts outside `keep` after a later publish stopped
    /// binding them (ADR-0046 decision 5). Kept and capture-forgotten rows are
    /// left untouched; `capture_ids` names the superseding capture.
    pub fn reclaim_superseded(
        &mut self,
        source: &RegisteredSource,
        capture_ids: &[String],
        keep: &std::collections::BTreeSet<String>,
    ) -> Result<u64, String> {
        self.reclaim(source, capture_ids, keep, ReclaimCause::Superseded)
    }

    fn reclaim(
        &mut self,
        source: &RegisteredSource,
        capture_ids: &[String],
        pins: &std::collections::BTreeSet<String>,
        cause: ReclaimCause,
    ) -> Result<u64, String> {
        #[cfg(any(test, feature = "test-support"))]
        if std::mem::take(&mut self.fail_next_reclaim) {
            return Err(storage_error("injected reclaim failure"));
        }
        receipt_owner(&source.source_id)?;
        if source.repo_key.is_empty() || source.repo_key.len() > 256 {
            return Err(storage_error("receipt ownership"));
        }
        let (capture_ids_json, capture_ids_truncated) = encode_capture_ids(capture_ids)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        receipt_storage(&tx)?;
        let mut reclaimed = Vec::new();
        let mut newly_marked = Vec::new();
        {
            let mut statement = tx
                .prepare(
                    "SELECT id, capture_forgotten_tombstone IS NOT NULL FROM receipts
                     WHERE source_id = ?1 ORDER BY id",
                )
                .map_err(storage_error)?;
            let mut rows = statement
                .query([&source.source_id])
                .map_err(storage_error)?;
            while let Some(row) = rows.next().map_err(storage_error)? {
                let id: String = row.get(0).map_err(storage_error)?;
                let already_marked: bool = row.get(1).map_err(storage_error)?;
                let receipt = stored_receipt(&tx, &id)?;
                if receipt.source_id().as_str() != source.source_id
                    || receipt.repo_key() != source.repo_key
                {
                    return Err(storage_error("receipt ownership"));
                }
                if cause == ReclaimCause::Superseded && already_marked {
                    continue;
                }
                if !pins.contains(&id) {
                    reclaimed.push(id);
                } else if cause == ReclaimCause::Forgotten && !already_marked {
                    // A pinned row is bound once, to the forget that first
                    // marked it, so its status never borrows a later operation.
                    newly_marked.push(id);
                }
            }
        }
        if reclaimed.is_empty() && newly_marked.is_empty() {
            tx.commit().map_err(storage_error)?;
            return Ok(0);
        }
        // Tombstone capture ids are a separate pool bounded by the receipt
        // store's byte ceiling, as the ledger reuses its count ceiling. A full
        // pool records the forget without them, flagged as truncated.
        let tombstone_bytes: i64 = tx
            .query_row(
                "SELECT coalesce(sum(length(CAST(capture_ids AS BLOB))),0) FROM receipt_tombstones",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        let (capture_ids_json, capture_ids_truncated) = if u64::try_from(tombstone_bytes)
            .map_err(storage_error)?
            + capture_ids_json.len() as u64
            > self.capacity.1
        {
            ("[]".to_owned(), 1)
        } else {
            (capture_ids_json, capture_ids_truncated)
        };
        require_one(
            tx.execute(
                "INSERT INTO receipt_tombstones (
                     source_id, repo_key, capture_ids, capture_ids_truncated,
                     forgotten_at, receipts_reclaimed, receipts_retained_pinned, cause
                 ) VALUES (
                     ?1, ?2, ?3, ?4,
                     strftime('%Y-%m-%dT%H:%M:%SZ','now'), ?5, ?6, ?7
                 )",
                params![
                    source.source_id,
                    source.repo_key,
                    capture_ids_json,
                    capture_ids_truncated,
                    reclaimed.len() as i64,
                    newly_marked.len() as i64,
                    cause.as_str(),
                ],
            )
            .map_err(storage_error)?,
        )?;
        let tombstone_id = tx.last_insert_rowid();
        for id in &newly_marked {
            require_one(
                tx.execute(
                    "UPDATE receipts SET capture_forgotten_tombstone = ?2 WHERE id = ?1",
                    params![id, tombstone_id],
                )
                .map_err(storage_error)?,
            )?;
        }
        for id in &reclaimed {
            require_one(
                tx.execute("DELETE FROM receipts WHERE id = ?1", [id])
                    .map_err(storage_error)?,
            )?;
            require_one(
                tx.execute(
                    "INSERT OR REPLACE INTO receipt_reclaims (receipt_id, tombstone_id) VALUES (?1, ?2)",
                    params![id, tombstone_id],
                )
                .map_err(storage_error)?,
            )?;
        }
        let reclaim_count: i64 = tx
            .query_row("SELECT count(*) FROM receipt_reclaims", [], |row| {
                row.get(0)
            })
            .map_err(storage_error)?;
        let max_reclaims = i64::try_from(self.capacity.0).unwrap_or(i64::MAX);
        if reclaim_count > max_reclaims {
            let excess = reclaim_count - max_reclaims;
            tx.execute(
                "DELETE FROM receipt_reclaims WHERE receipt_id IN (
                     SELECT receipt_id FROM receipt_reclaims
                     ORDER BY tombstone_id ASC, receipt_id ASC
                     LIMIT ?1
                 )",
                [excess],
            )
            .map_err(storage_error)?;
        }
        delete_orphan_tombstones(&tx)?;
        delete_orphan_headers(&tx)?;
        delete_orphan_closures(&tx)?;
        receipt_storage(&tx)?;
        tx.commit().map_err(storage_error)?;
        Ok(reclaimed.len() as u64)
    }

    /// Look up the retention status of an exact receipt ID.
    ///
    /// This is the identity half of ADR-0046 decision 4. Basis and capture
    /// validation stay the caller's job.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn receipt_status(&self, id: &str) -> Result<ReceiptStatus, String> {
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        receipt_storage(&tx)?;
        if id.is_empty() || id.len() > 256 {
            tx.commit().map_err(storage_error)?;
            return Ok(ReceiptStatus::Unknown);
        }
        let present: Option<Option<i64>> = tx
            .query_row(
                "SELECT capture_forgotten_tombstone FROM receipts WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?;
        if let Some(marked) = present {
            stored_receipt(&tx, id)?;
            let Some(tombstone_id) = marked else {
                tx.commit().map_err(storage_error)?;
                return Ok(ReceiptStatus::Present);
            };
            let tombstone: TombstoneSummary = tx
                .query_row(
                    "SELECT source_id, forgotten_at, receipts_reclaimed, receipts_retained_pinned
                     FROM receipt_tombstones
                     WHERE id = ?1",
                    [tombstone_id],
                    |row| {
                        Ok(TombstoneSummary {
                            source_id: row.get(0)?,
                            forgotten_at: row.get(1)?,
                            receipts_reclaimed: row.get::<_, i64>(2)? as u64,
                            receipts_retained_pinned: row.get::<_, i64>(3)? as u64,
                        })
                    },
                )
                .map_err(storage_error)?;
            receipt_owner(&tombstone.source_id).map_err(storage_error)?;
            tx.commit().map_err(storage_error)?;
            return Ok(ReceiptStatus::CaptureForgotten { tombstone });
        }
        let tombstone: Option<(TombstoneSummary, String)> = tx
            .query_row(
                "SELECT t.source_id, t.forgotten_at, t.receipts_reclaimed, t.receipts_retained_pinned, t.cause
                 FROM receipt_reclaims r
                 JOIN receipt_tombstones t ON t.id = r.tombstone_id
                 WHERE r.receipt_id = ?1",
                [id],
                |row| {
                    Ok((
                        TombstoneSummary {
                            source_id: row.get(0)?,
                            forgotten_at: row.get(1)?,
                            receipts_reclaimed: row.get::<_, i64>(2)? as u64,
                            receipts_retained_pinned: row.get::<_, i64>(3)? as u64,
                        },
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_error)?;
        tx.commit().map_err(storage_error)?;
        if let Some((tombstone, cause)) = tombstone {
            // An observed receipt's tombstone names its input owner (ADR-0079).
            receipt_owner(&tombstone.source_id).map_err(storage_error)?;
            return Ok(if cause == ReclaimCause::Superseded.as_str() {
                ReceiptStatus::Superseded { tombstone }
            } else {
                ReceiptStatus::Reclaimed { tombstone }
            });
        }
        Ok(ReceiptStatus::Unknown)
    }

    /// The capture ids the tombstone of a reclaimed receipt records.
    #[cfg(any(test, feature = "test-support"))]
    pub fn tombstone_capture_ids(&self, receipt_id: &str) -> Result<Vec<String>, String> {
        let ids: String = self
            .conn
            .query_row(
                "SELECT t.capture_ids FROM receipt_reclaims r
                 JOIN receipt_tombstones t ON t.id = r.tombstone_id
                 WHERE r.receipt_id = ?1",
                [receipt_id],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        serde_json::from_str(&ids).map_err(storage_error)
    }
}

/// Private stores are locked only for short operations, never while parsing.
/// Receipts that prepared tasks cite, counted per receipt.
type InFlight = std::sync::Arc<Mutex<std::collections::BTreeMap<String, usize>>>;

/// A prepared task's pin on the receipts it cites (#762 review). Taken while
/// the task still holds its shared retention guards and released when the
/// task is dropped after staging, so a supersession reclaim never deletes
/// evidence a proposal is about to cite. Forget does not consult it: a
/// forgotten source's tasks still stage their copied content (AC-0177/0178).
pub struct InFlightPin {
    ids: Vec<String>,
    pins: InFlight,
}

impl Drop for InFlightPin {
    fn drop(&mut self) {
        // A poisoned set keeps every pin, which only ever keeps receipts.
        if let Ok(mut pins) = self.pins.lock() {
            for id in &self.ids {
                if let Some(count) = pins.get_mut(id) {
                    *count -= 1;
                    if *count == 0 {
                        pins.remove(id);
                    }
                }
            }
        }
    }
}

pub struct PrimarySourceStore {
    pub captures: Mutex<CaptureStore>,
    pub receipts: Mutex<ReceiptStore>,
    pub in_flight: InFlight,
    storage: PrivateStorage,
    #[cfg(any(test, feature = "test-support"))]
    evidence_accesses: std::sync::atomic::AtomicUsize,
}

impl PrimarySourceStore {
    /// Bytes cloned into each retention guard; read without allocating a path.
    #[cfg(unix)]
    pub fn retained_app_path_bytes(&self) -> usize {
        self.storage.app_path.as_os_str().as_encoded_bytes().len()
    }

    pub fn open(app_data: &Path) -> Result<Self, String> {
        let storage = PrivateStorage::open(app_data)?;
        let captures = CaptureStore::open(&storage.path("captures.sqlite"), StoreLimits::default())
            .map_err(storage_error)?;
        let receipts = ReceiptStore::open(&storage.path("receipts.sqlite"))?;
        storage.verify()?;
        Ok(Self {
            captures: Mutex::new(captures),
            receipts: Mutex::new(receipts),
            in_flight: InFlight::default(),
            storage,
            #[cfg(any(test, feature = "test-support"))]
            evidence_accesses: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Apply the project's capture store quota (#1653, AC-1801) to captures
    /// retained from now on.
    pub fn set_store_limits(&self, limits: StoreLimits) -> Result<(), String> {
        self.captures
            .lock()
            .map_err(storage_error)?
            .set_limits(limits)
            .map_err(storage_error)
    }

    /// Test observation of retained-source lease attempts, including historical
    /// inspection. It does not count current-checkout reads or successful bytes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn evidence_access_attempts(&self) -> usize {
        self.evidence_accesses
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Pin `ids` for a prepared task until the returned pin is dropped.
    pub fn pin_in_flight(&self, ids: Vec<String>) -> Result<InFlightPin, String> {
        let mut pins = self.in_flight.lock().map_err(storage_error)?;
        for id in &ids {
            *pins.entry(id.clone()).or_default() += 1;
        }
        Ok(InFlightPin {
            ids,
            pins: self.in_flight.clone(),
        })
    }

    /// Hold the exclusive guard a forget takes, so tests can race against it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn exclusive_guard_for_test(&self, source: &str) -> RetentionGuard {
        self.guard(source, true).unwrap()
    }

    pub fn guard(&self, source: &str, exclusive: bool) -> Result<RetentionGuard, String> {
        #[cfg(any(test, feature = "test-support"))]
        self.evidence_accesses
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        source_id(source)?;
        self.storage.verify()?;
        let file = private_file(&self.storage.locks, format!("{source}.lock").as_ref(), true)?;
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        result.map_err(|_| "Retained source is busy or locking is unavailable; retry after the active operation finishes.".to_string())?;
        let guard = RetentionGuard {
            _file: file,
            source_id: source.to_owned(),
            app_path: self.storage.app_path.clone(),
        };
        self.storage.verify()?;
        Ok(guard)
    }

    /// Revalidate the retained OS lock handle without acquiring another lock or
    /// reading captured source bytes. The prepared-read path keeps this same
    /// guard alive through its eventual publication acknowledgement.
    pub fn verify_task_guard(&self, guard: &RetentionGuard) -> Result<(), PinnedReadError> {
        if guard.app_path != self.storage.app_path || source_id(&guard.source_id).is_err() {
            return Err(PinnedReadError::Invalid);
        }
        self.storage
            .verify()
            .map_err(|_| PinnedReadError::Operational)?;
        let current = self
            .storage
            .locks
            .symlink_metadata(format!("{}.lock", guard.source_id))
            .map_err(|_| PinnedReadError::Operational)?;
        let held = cap_std::fs::Metadata::from_file(&guard._file)
            .map_err(|_| PinnedReadError::Operational)?;
        if !current.is_file() || current.nlink() != 1 || !same_entry(&current, &held) {
            return Err(PinnedReadError::Invalid);
        }
        #[cfg(unix)]
        {
            use cap_std::fs::PermissionsExt;
            if held.permissions().mode() & 0o077 != 0 {
                return Err(PinnedReadError::Invalid);
            }
        }
        Ok(())
    }

    pub fn prepare(
        &self,
        source: &RegisteredSource,
        root: &Path,
        layers: &[String],
    ) -> Result<PrimaryInput, String> {
        self.prepare_at(
            source,
            root,
            layers,
            None,
            CaptureLimits::default(),
            captured::MAX_VISITED_ENTRIES,
            CallerSelections::default(),
        )
    }

    /// [`Self::prepare`], reading the selected files from the exact local Git
    /// objects of `commit` when one is given (AC-1022). Only a managed clone
    /// passes its cloned commit: the parser then consumes blob bytes, never
    /// checkout bytes, so line-ending conversion and checkout filters cannot
    /// change what a citation means. A selected path that is not a regular
    /// blob of that commit, or a missing object, fails the recovery; there is
    /// no fallback to the checkout. Local folders keep a working-tree capture.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_at(
        &self,
        source: &RegisteredSource,
        root: &Path,
        layers: &[String],
        commit: Option<&str>,
        limits: CaptureLimits,
        max_visited_entries: usize,
        callers: CallerSelections<'_>,
    ) -> Result<PrimaryInput, String> {
        let api_docs = callers.api_docs;
        // A managed clone's commit is captured whenever the TS pass runs,
        // including for Pulumi infrastructure alone (AC-1022).
        let wanted = layers.is_empty()
            || layers
                .iter()
                .any(|layer| ["server", "events", "client"].contains(&layer.as_str()))
            || (commit.is_some()
                && ingest::coverage::wants(layers, ingest::coverage::INFRA_LAYERS));
        if !wanted {
            return Ok(PrimaryInput {
                capture: None,
                unfollowed: Vec::new(),
                environments_captured: false,
                starts_captured: false,
                toolchain_captured: false,
                documents_captured: false,
                api_doc_paths: None,
                plugin_paths: None,
                _guard: None,
            });
        }
        let guard = self.guard(&source.source_id, false)?;
        // CapturedError's diagnostics are fixed strings that never carry
        // source contents, so the reason is safe to name (#1229).
        // The walk selects as many files as the capture may retain, so a
        // raised capture file limit (AC-1800) reaches the TS/JS selection
        // (AC-1802).
        let selection = captured::enumerate_selection_within(
            root,
            captured::SelectionLimits {
                max_visited_entries,
                max_files: limits.max_files,
            },
        )
        .map_err(|error| format!("Selected TS/JS source could not be captured safely ({error}); recovery was not published."))?;
        let id = source_id(&source.source_id)?;
        let capture_paths = |paths: &[String]| match commit {
            Some(commit) => source_capture::capture_git(root, &id, commit, paths, limits),
            None => source_capture::capture_working_tree(root, &id, paths, limits),
        };
        // The Python (AC-1024), Go (AC-1026), Java (AC-1031), Rust (AC-1033)
        // and Kotlin (AC-1035) lanes' files join the same capture. A lane whose files
        // cannot be listed is left out; when the combined set cannot be
        // retained, those lanes keep their ordinary uncaptured reads and the
        // TS/JS capture stands alone.
        let wants_lanes = ingest::coverage::wants(layers, ingest::coverage::APPLICATION_LAYERS);
        let lanes: Vec<String> = if wants_lanes {
            let python = adapters_lang_python::python_paths(root).unwrap_or_default();
            let go = adapters_lang_go::go_paths(root).unwrap_or_default();
            let java = adapters_lang_java::java_paths(root).unwrap_or_default();
            let rust = adapters_lang_rust::rust_paths(root).unwrap_or_default();
            let kotlin = adapters_lang_kotlin::kotlin_paths(root).unwrap_or_default();
            python
                .into_iter()
                .chain(go)
                .chain(java)
                .chain(rust)
                .chain(kotlin)
                .collect()
        } else {
            Vec::new()
        };
        // Terraform's `.tf` files (AC-1053) join it when infrastructure is
        // recovered, under the same fallback.
        let mut lanes = lanes;
        if ingest::coverage::wants(layers, ingest::coverage::INFRA_LAYERS) {
            lanes.extend(iac::tf_paths(root).unwrap_or_default());
        }
        // Workflows and Dockerfiles (AC-1057) are read for every layer
        // selection, so they are retained with the TS/JS selection itself,
        // even when the lanes fall back: a capture always holds the pipeline
        // reader's whole selection, and the reader never falls back to the
        // checkout. Directory completion's resolution configs join it too,
        // so a walk can show they are the capture's own bytes (ADR-0072
        // slice 6).
        let mut base = selection.files.clone();
        base.extend(selection.configs.iter().cloned());
        base.extend(ingest::pipelines::pipeline_paths(root).map_err(|_| {
            "Pipeline files could not be listed for capture; recovery was not published."
                .to_string()
        })?);
        base.sort();
        base.dedup();
        let with = |extra: &[String]| {
            let mut paths = base.clone();
            paths.extend(lanes.iter().chain(extra).cloned());
            paths.sort();
            paths.dedup();
            paths
        };
        // The environment (AC-1059), schedule and entry-point (AC-1061) and
        // toolchain (AC-1063) readers' selections, read for every layer
        // selection, are tried with the lanes first. When the whole set
        // cannot be retained, the lanes are retained without them and the
        // readers read the checkout, so they never cost a lane its capture.
        let environments = ingest::environments::environment_paths(root).ok();
        let starts = ingest::schedules::schedule_paths(root)
            .and_then(|mut paths| {
                paths.extend(ingest::entry_points::entry_point_paths(root)?);
                Ok(paths)
            })
            .ok();
        let toolchain = ingest::toolchain::toolchain_paths(root).ok();
        // The declared API references and their config (AC-1067), listed by
        // the caller, who knows the manifest's declarations.
        let readers: Vec<String> = environments
            .iter()
            .chain(&starts)
            .chain(&toolchain)
            .flatten()
            .chain(api_docs.into_iter().flatten())
            .cloned()
            .collect();
        // Droppable groups, in the order they are kept: shipped JVM class
        // files (AC-1215) and JavaScript bundle maps (AC-1216), the files the
        // active plugins claim (AC-1069), then the documentation walk's
        // selection (AC-1065). Each is tried with the readers and everything
        // before it, and the last is the first left out, so a later group
        // never costs an earlier one, a reader or a lane its capture.
        let classes = if wants_lanes {
            let mut shipped = adapters_artifact_jvm::class_paths(root).unwrap_or_default();
            shipped.extend(adapters_artifact_js::artifact_paths(root).unwrap_or_default());
            shipped
        } else {
            Vec::new()
        };
        let documents = spec::documentation_paths(root).ok();
        let plugins = callers.plugins.map(<[String]>::to_vec);
        let groups = [(!classes.is_empty()).then_some(classes), plugins, documents];
        let mut capture = None;
        let mut kept = 0;
        for count in (1..=groups.len()).rev() {
            if groups[count - 1].is_none() {
                continue;
            }
            let mut extra = readers.clone();
            extra.extend(groups[..count].iter().flatten().flatten().cloned());
            if let Ok(held) = capture_paths(&with(&extra)) {
                capture = Some(held);
                kept = count;
                break;
            }
        }
        let plugin_paths = if kept >= 2 { groups[1].clone() } else { None };
        let documents_captured = kept == groups.len();
        if capture.is_none()
            && (environments.is_some()
                || starts.is_some()
                || toolchain.is_some()
                || api_docs.is_some())
        {
            capture = capture_paths(&with(&readers)).ok();
        }
        let environments_captured = capture.is_some() && environments.is_some();
        let starts_captured = capture.is_some() && starts.is_some();
        let toolchain_captured = capture.is_some() && toolchain.is_some();
        let api_doc_paths = api_docs
            .filter(|_| capture.is_some())
            .map(<[String]>::to_vec);
        if capture.is_none() && !lanes.is_empty() {
            capture = capture_paths(&with(&[])).ok();
        }
        let capture = match capture {
            Some(capture) => capture,
            None => capture_paths(&base).map_err(|_| match commit {
                Some(_) => "Selected TS/JS source could not be read from the cloned commit's Git objects within the capture limits; recovery was not published.".to_string(),
                None => "Selected TS/JS source could not be retained within the capture limits; recovery was not published.".to_string(),
            })?,
        };
        Ok(PrimaryInput {
            capture: Some(capture),
            unfollowed: selection.unfollowed,
            environments_captured,
            starts_captured,
            toolchain_captured,
            documents_captured,
            api_doc_paths,
            plugin_paths,
            _guard: Some(guard),
        })
    }

    /// Retain `source`'s manifest observation input capture beside its own
    /// captures (ADR-0080), with as many of its observed `receipts` as fit
    /// (ADR-0079, AC-1045). They are owned by the input identity and stored
    /// within the source's share, less `charged`, what the source's own
    /// receipts of this recovery took; the input is listed and forgotten
    /// with the source.
    pub fn persist_input(
        &self,
        source: &RegisteredSource,
        capture: &source_capture::Capture,
        receipts: &[impl Citable],
        charged: (u64, u64),
    ) -> Result<RetainedReceipts, String> {
        let input = crate::observation_inputs::input_source_id(&source.source_id);
        if capture.manifest().source_id.as_str() != input {
            return Err(storage_error("observation input of another source"));
        }
        let _guard = self.guard(&source.source_id, false)?;
        self.storage.verify()?;
        let mut captures = self.captures.lock().map_err(storage_error)?;
        captures
            .persist(capture)
            .map_err(|_| "Observation input retention failed or reached capacity. Use retained-source controls to free space; recovery was not published.".to_string())?;
        let weights = captures.source_byte_weights().map_err(storage_error)?;
        let weight = weights
            .get(&source_id(&source.source_id)?)
            .copied()
            .unwrap_or(0);
        let total = receipt_weight_total(&weights);
        let mut store = self.receipts.lock().map_err(storage_error)?;
        let share = (
            source_share(store.capacity.0, weight, total).saturating_sub(charged.0),
            source_share(store.capacity.1, weight, total).saturating_sub(charged.1),
        );
        store.persist_within(capture, &source.owning_as(input), receipts, &[], share)
    }

    /// Retain the capture and as many `receipts` as the store has room for
    /// (#722). Callers keep only [`RetainedReceipts::ids`] for bindings, so
    /// no binding cites a receipt the store does not hold.
    pub fn persist(
        &self,
        input: &PrimaryInput,
        source: &RegisteredSource,
        receipts: &[impl Citable],
    ) -> Result<RetainedReceipts, String> {
        self.persist_with_closures(input, source, receipts, &[])
    }

    /// [`Self::persist`], also supplying the input closure records that full
    /// receipts name but do not carry (AC-1084).
    pub fn persist_with_closures(
        &self,
        input: &PrimaryInput,
        source: &RegisteredSource,
        receipts: &[impl Citable],
        closures: &[captured::InputClosureV1],
    ) -> Result<RetainedReceipts, String> {
        self.storage.verify()?;
        if let Some(capture) = &input.capture {
            // The capture guard is held through receipt admission so the
            // weights cannot go stale under a concurrent recovery or forget.
            // Lock order is captures then receipts; nothing holds receipts
            // while taking captures.
            let mut captures = self.captures.lock().map_err(storage_error)?;
            captures.persist(capture).map_err(|_| "Source retention failed or reached capacity. Use retained-source controls to free space; recovery was not published.".to_string())?;
            let weights = captures.source_byte_weights().map_err(storage_error)?;
            let weight = weights
                .get(&capture.manifest().source_id)
                .copied()
                .unwrap_or(0);
            let total = receipt_weight_total(&weights);
            let mut store = self.receipts.lock().map_err(storage_error)?;
            let share = (
                source_share(store.capacity.0, weight, total),
                source_share(store.capacity.1, weight, total),
            );
            store.persist_within(capture, source, receipts, closures, share)
        } else if !receipts.is_empty() {
            Err(storage_error("uncaptured receipts"))
        } else {
            Ok(RetainedReceipts::default())
        }
    }
}

/// The captured bytes receipt shares are divided among: every registered
/// source's, but not its observation inputs' (ADR-0080), which hold no
/// receipt and would otherwise shrink their owner's share.
fn receipt_weight_total(weights: &std::collections::BTreeMap<SourceId, u64>) -> u64 {
    weights
        .iter()
        .filter(|(id, _)| {
            !id.as_str()
                .starts_with(crate::observation_inputs::INPUT_REPO_PREFIX)
        })
        .map(|(_, weight)| weight)
        .sum()
}

/// The most one source's recovery may retain of a store-wide `capacity`:
/// proportional to its captured bytes `weight` among all retained sources'
/// `total`, never below a sixteenth, and never above the store (#725). Shares
/// are ceilings, not reservations: they can sum past `capacity`, and the
/// store-wide bound still applies, so a source may get less than its share.
fn source_share(capacity: u64, weight: u64, total: u64) -> u64 {
    if total == 0 {
        return capacity;
    }
    let proportional = u128::from(capacity) * u128::from(weight) / u128::from(total);
    u64::try_from(proportional)
        .unwrap_or(capacity)
        .max(capacity / 16)
        .min(capacity)
}

/// Receipts one recovery actually retained, and those left out for lack of
/// store capacity, by file path and fact key.
#[derive(Debug, Default)]
pub struct RetainedReceipts {
    ids: std::collections::BTreeSet<String>,
    omitted: std::collections::BTreeMap<String, u64>,
    pub omitted_facts: Vec<(FactKey, String)>,
    /// The (count, bytes) of the source's share the retained rows took.
    charged: (u64, u64),
}

impl RetainedReceipts {
    /// What the retained rows took of the source's share.
    pub fn charged(&self) -> (u64, u64) {
        self.charged
    }

    /// Drop every receipt the store did not retain and return the omissions.
    pub fn keep_retained(
        self,
        receipts: &mut Vec<impl Citable>,
    ) -> (
        std::collections::BTreeMap<String, u64>,
        Vec<(FactKey, String)>,
    ) {
        receipts.retain(|receipt| self.ids.contains(receipt.id()));
        (self.omitted, self.omitted_facts)
    }
}

/// Selections only the caller can list, because they depend on what the
/// caller knows of the source: they join the reader selections tried beside
/// the lanes.
#[derive(Debug, Default, Clone, Copy)]
pub struct CallerSelections<'a> {
    /// The declared API references and their config (AC-1067), from the
    /// repository config and the system manifest; `None` when their load
    /// names a gap.
    pub api_docs: Option<&'a [String]>,
    /// The files the active plugins claim (AC-1069), in path order; `None`
    /// when no plugin is active or they could not be listed.
    pub plugins: Option<&'a [String]>,
}

pub struct PrimaryInput {
    pub capture: Option<Capture>,
    /// Symlinks the capture did not follow, reported in coverage (#1229).
    pub unfollowed: Vec<String>,
    /// Whether the capture holds the environment reader's whole selection
    /// (AC-1059); when it does not, that reader reads the checkout.
    pub environments_captured: bool,
    /// Whether the capture holds the schedule and entry-point readers' whole
    /// selections (AC-1061); when it does not, they read the checkout.
    pub starts_captured: bool,
    /// Whether the capture holds the toolchain reader's whole selection
    /// (AC-1063); when it does not, that reader reads the checkout.
    pub toolchain_captured: bool,
    /// Whether the capture holds the documentation walk's whole selection
    /// (AC-1065); when it does not, documents and found ADRs are read from
    /// the checkout.
    pub documents_captured: bool,
    /// The declared API references and their config the capture holds
    /// (AC-1067); `None` when it does not hold them, and their loader reads
    /// the checkout.
    pub api_doc_paths: Option<Vec<String>>,
    /// The files the active plugins claim, as the caller listed them, when
    /// the capture holds them all (AC-1069); `None` when it does not, and
    /// plugins are routed the checkout's files.
    pub plugin_paths: Option<Vec<String>>,
    _guard: Option<RetentionGuard>,
}

/// [`matching_bindings`] over packed facts: only the nodes and edges a
/// receipt names are decoded, every occurrence of a named edge included, so
/// the merged edge view and last-value node winners are the same (#1574).
/// Each named fact is decoded, matched and dropped before the next one, so
/// the publication never holds every receipted fact as a props tree at once
/// (AC-1200): on a large repository nearly every fact carries a receipt.
pub fn matching_bindings_packed<R: Citable>(
    facts: &core_graph::facts::FactStore,
    receipts: &[R],
) -> Result<Vec<SourceBinding>, String> {
    if receipts.is_empty() {
        return Ok(Vec::new());
    }
    // Per named fact: the receipts naming it and its occurrences, both as
    // indices in their original order.
    type Named<K> = std::collections::BTreeMap<K, (Vec<usize>, Vec<usize>)>;
    let mut nodes = Named::<&str>::new();
    let mut edges = Named::<(&str, &str, &str)>::new();
    for (at, receipt) in receipts.iter().enumerate() {
        match receipt.fact_key() {
            FactKey::Node { id } => nodes.entry(id.as_str()).or_default().0.push(at),
            FactKey::Edge {
                source,
                label,
                destination,
            } => edges
                .entry((source.as_str(), destination.as_str(), label.as_str()))
                .or_default()
                .0
                .push(at),
        }
    }
    // Occurrence indices in extraction order: the last node occurrence is
    // the published winner, and every edge occurrence feeds the merge.
    for (index, node) in facts.nodes().iter().enumerate() {
        if let Some((_, occurrences)) = nodes.get_mut(node.id.as_str()) {
            occurrences.push(index);
        }
    }
    for (index, edge) in facts.edges().iter().enumerate() {
        let key = (edge.src.as_str(), edge.dst.as_str(), edge.label.as_str());
        if let Some((_, occurrences)) = edges.get_mut(&key) {
            occurrences.push(index);
        }
    }
    let error = |error: core_graph::GraphError| error.to_string();
    let mut bound = std::collections::BTreeMap::new();
    // The last receipt that matches a fact binds it, as in `matching_bindings_in`.
    let mut bind = |named: &[usize], matches: &dyn Fn(&R) -> bool| {
        if let Some(receipt) = named
            .iter()
            .rev()
            .map(|&at| &receipts[at])
            .find(|receipt| matches(receipt))
        {
            bound.insert(
                receipt.fact_key().clone(),
                SourceBinding {
                    fact: receipt.fact_key().clone(),
                    repo_key: receipt.repo_key().to_owned(),
                    receipt_id: receipt.id().to_owned(),
                    emitted_fact_digest: receipt.fact_digest().to_owned(),
                },
            );
        }
    };
    for (named, occurrences) in nodes.values() {
        let Some(&winner) = occurrences.last() else {
            continue;
        };
        let node = facts.nodes()[winner].unpack().map_err(error)?;
        bind(named, &|receipt| receipt.matches_node(&node));
    }
    for (named, occurrences) in edges.values() {
        if occurrences.is_empty() {
            continue;
        }
        let decoded = occurrences
            .iter()
            .map(|&index| facts.edges()[index].unpack())
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let merged = crate::edge_merge::merged_edge_view(&decoded);
        let edge = merged.values().next().expect("one key, one merged edge");
        bind(named, &|receipt| receipt.matches_edge(edge));
    }
    Ok(bound.into_values().collect())
}

/// The decoded reference [`matching_bindings_packed`] must equal.
#[cfg(any(test, feature = "test-support"))]
pub fn matching_bindings(
    extraction: &adapters_lang_ts::Extraction,
    receipts: &[impl Citable],
) -> Vec<SourceBinding> {
    matching_bindings_in(&extraction.nodes, &extraction.edges, receipts)
}

/// [`matching_bindings`] over any producer's nodes and edges.
pub fn matching_bindings_in(
    nodes: &[core_graph::Node],
    edges: &[core_graph::Edge],
    receipts: &[impl Citable],
) -> Vec<SourceBinding> {
    // Match the same facts that graph publication materializes: last-value
    // node winners, and edges after evidence union (AC-0203). A relation
    // collapsed from several sites is not the fact any one receipt attested,
    // so it publishes unbound rather than failing the digest check.
    let nodes: std::collections::BTreeMap<_, _> =
        nodes.iter().map(|node| (&node.id, node)).collect();
    let edges = crate::edge_merge::merged_edge_view(edges);
    receipts
        .iter()
        .filter(|receipt| match receipt.fact_key() {
            FactKey::Node { id } => nodes.get(id).is_some_and(|node| receipt.matches_node(node)),
            FactKey::Edge {
                source,
                label,
                destination,
            } => edges
                .get(&(source.clone(), destination.clone(), label.clone()))
                .is_some_and(|edge| receipt.matches_edge(edge)),
        })
        .map(|receipt| {
            (
                receipt.fact_key().clone(),
                SourceBinding {
                    fact: receipt.fact_key().clone(),
                    repo_key: receipt.repo_key().to_owned(),
                    receipt_id: receipt.id().to_owned(),
                    emitted_fact_digest: receipt.fact_digest().to_owned(),
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect()
}

#[derive(Serialize)]
pub struct CapturedRange {
    pub index: usize,
    pub path: String,
    pub byte_start: u64,
    pub byte_end: u64,
}

#[derive(Serialize)]
pub struct CapturedDescription {
    pub graph_snapshot_id: String,
    pub fact: FactKey,
    pub receipt_id: String,
    pub emitted_fact_digest: String,
    pub source_id: String,
    pub repo_key: String,
    pub ranges: Vec<CapturedRange>,
    pub scope: &'static str,
    pub input_closure: &'static str,
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    pub(super) fn fixture() -> (
        tempfile::TempDir,
        RegisteredSource,
        Capture,
        adapters_lang_ts::Extraction,
        Vec<Receipt>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(
            target.join("rule.ts"),
            b"export function ready(ok: boolean) { if (!ok) return false; return true; }",
        )
        .unwrap();
        let state_path = directory.path().join("state.sqlite");
        let _findings = crate::findings::FindingStore::open(&state_path).unwrap();
        let mut registry =
            crate::sources::SourceRegistry::open(&state_path, directory.path()).unwrap();
        let source = registry.register_local(&target).unwrap();
        let capture = source_capture::capture_working_tree(
            &target,
            &source_id(&source.source_id).unwrap(),
            &["rule.ts".into()],
            CaptureLimits::default(),
        )
        .unwrap();
        let (extraction, receipts) = captured::extract_file(
            capture.file("rule.ts").unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        assert!(!receipts.is_empty());
        (directory, source, capture, extraction, receipts)
    }

    // AC-0822: the receipt store waits 5 s for a competing writer, the same
    // busy timeout as every other Cartograph store (#1653).
    #[test]
    fn receipt_store_waits_five_seconds_for_a_busy_writer() {
        let directory = tempfile::tempdir().unwrap();
        let store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        let timeout: i64 = store
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5_000);
    }

    // AC-0292: a full receipt store keeps what fits in order, names the rest
    // by file instead of refusing the recovery, keeps already-stored evidence,
    // and returns only retained ids for bindings (#722).
    #[test]
    fn full_receipt_store_retains_in_order_and_names_omissions() {
        let (_directory, source, capture, _, receipts) = fixture();
        assert!(receipts.len() >= 2);
        let directory = tempfile::tempdir().unwrap();
        let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        store.capacity.0 = 1;
        for _ in 0..2 {
            let mut kept = receipts.clone();
            let (omitted, omitted_facts) = store
                .persist(&capture, &source, &receipts)
                .unwrap()
                .keep_retained(&mut kept);
            assert_eq!(kept, receipts[..1]);
            assert_eq!(
                omitted,
                std::collections::BTreeMap::from([(
                    "rule.ts".to_string(),
                    receipts.len() as u64 - 1
                )])
            );
            let expected_omitted_facts: Vec<_> = receipts[1..]
                .iter()
                .map(|receipt| (receipt.fact_key().clone(), receipt.fact_digest().to_owned()))
                .collect();
            assert_eq!(omitted_facts, expected_omitted_facts);
            assert_eq!(receipt_count(&store), 1);
        }
        // Room again: the omitted receipts are admitted on the next recovery.
        store.capacity.0 = MAX_RECEIPTS;
        let mut kept = receipts.clone();
        let (omitted, omitted_facts) = store
            .persist(&capture, &source, &receipts)
            .unwrap()
            .keep_retained(&mut kept);
        assert!(omitted.is_empty());
        assert!(omitted_facts.is_empty());
        assert_eq!(kept, receipts);
        assert_eq!(receipt_count(&store), receipts.len() as i64);
    }

    // AC-0497: a source's share of the receipt store is proportional to its
    // captured bytes, never below a sixteenth of the store and never above it
    // (#725).
    #[test]
    fn source_share_is_proportional_with_a_floor_and_a_ceiling() {
        assert_eq!(source_share(1600, 1, 2), 800);
        assert_eq!(source_share(1600, 3, 4), 1200);
        assert_eq!(source_share(1600, 99, 1000), 158);
        assert_eq!(source_share(1600, 63, 1000), 100);
        assert_eq!(source_share(1600, 62, 1000), 100);
        assert_eq!(source_share(1600, 1, 1000), 100);
        assert_eq!(source_share(1600, 5, 5), 1600);
        assert_eq!(source_share(1600, 7, 5), 1600);
        assert_eq!(source_share(1600, 0, 0), 1600);
        assert_eq!(source_share(u64::MAX, u64::MAX - 1, u64::MAX), u64::MAX - 1);
    }

    // AC-1043 (Codex on #2235): an observation input capture holds no receipt,
    // so its bytes never shrink a registered source's receipt share.
    #[test]
    fn observation_input_bytes_are_outside_receipt_shares() {
        let source = "src_0123456789abcdef0123456789abcdef";
        let weights = std::collections::BTreeMap::from([
            (SourceId::new(source).unwrap(), 10),
            (
                SourceId::new("src_fedcba9876543210fedcba9876543210").unwrap(),
                30,
            ),
            (
                SourceId::new(crate::observation_inputs::input_source_id(source)).unwrap(),
                1_000,
            ),
        ]);
        assert_eq!(receipt_weight_total(&weights), 40);
    }

    fn payload_bytes(store: &ReceiptStore) -> u64 {
        store
            .conn
            .query_row(
                "SELECT COALESCE(SUM(length(payload)),0) FROM receipts",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap() as u64
    }

    // AC-0497: once a batch's retained rows reach the source's share, by count
    // or bytes, the rest are named omissions; identical rows already stored
    // stay retained and count against the share (#725).
    #[test]
    fn receipts_beyond_a_source_share_are_named_omissions() {
        let (_directory, source, capture, _, receipts) = fixture();
        assert!(receipts.len() >= 2);
        let named = |n: usize| {
            std::collections::BTreeMap::from([("rule.ts".to_string(), (receipts.len() - n) as u64)])
        };
        let directory = tempfile::tempdir().unwrap();
        let open = |name: &str| ReceiptStore::open(&directory.path().join(name)).unwrap();

        let mut store = open("count.sqlite");
        for share in [(1, u64::MAX), (0, 0)] {
            let mut kept = receipts.clone();
            let (omitted, omitted_facts) = store
                .persist_within(&capture, &source, &receipts, &[], share)
                .unwrap()
                .keep_retained(&mut kept);
            assert_eq!(kept, receipts[..1]);
            assert_eq!(omitted, named(1));
            assert_eq!(omitted_facts.len(), receipts.len() - 1);
        }

        // The first receipt's whole cost, header included, is exactly enough.
        let mut store = open("first.sqlite");
        store.persist(&capture, &source, &receipts[..1]).unwrap();
        let (_, first_cost) = receipt_storage(&store.conn).unwrap();
        let first_payload = payload_bytes(&store);
        for (name, share, retained) in [
            ("exact.sqlite", first_cost, 1),
            ("below.sqlite", first_cost - 1, 0),
        ] {
            let mut store = open(name);
            let mut kept = receipts[..1].to_vec();
            let (omitted, _) = store
                .persist_within(&capture, &source, &receipts[..1], &[], (u64::MAX, share))
                .unwrap()
                .keep_retained(&mut kept);
            assert_eq!(kept, receipts[..retained]);
            assert_eq!(omitted.values().sum::<u64>(), 1 - retained as u64);
        }

        // An already-stored identical row counts against the share by count
        // and by its stored bytes, its header included once.
        let mut both = open("both.sqlite");
        both.persist(&capture, &source, &receipts[..2]).unwrap();
        let second_body = payload_bytes(&both) - first_payload;
        // Two new rows sharing a header are charged it once.
        for (name, share, retained) in [
            ("pair-exact.sqlite", first_cost + second_body, 2),
            ("pair-below.sqlite", first_cost + second_body - 1, 1),
        ] {
            let mut store = open(name);
            let mut kept = receipts[..2].to_vec();
            store
                .persist_within(&capture, &source, &receipts[..2], &[], (u64::MAX, share))
                .unwrap()
                .keep_retained(&mut kept);
            assert_eq!(kept, receipts[..retained], "{name}");
        }
        for (share, retained) in [
            ((1, u64::MAX), 1),
            ((2, u64::MAX), 2),
            ((u64::MAX, first_cost + second_body - 1), 1),
            ((u64::MAX, first_cost + second_body), 2),
        ] {
            let mut kept = receipts[..2].to_vec();
            let mut store = open("first.sqlite");
            let (omitted, _) = store
                .persist_within(&capture, &source, &receipts[..2], &[], share)
                .unwrap()
                .keep_retained(&mut kept);
            assert_eq!(kept, receipts[..retained]);
            assert_eq!(omitted.values().sum::<u64>(), 2 - retained as u64);
            drop(store);
            if retained == 2 {
                // Undo the admission so the next case starts from one row.
                let store = open("first.sqlite");
                store
                    .conn
                    .execute("DELETE FROM receipts WHERE id = ?1", [receipts[1].id()])
                    .unwrap();
            }
        }

        // A legacy row is charged its whole JSON, alone or before a
        // normalized one.
        let legacy = mint_v2_receipt_from_v3(&receipts[0]);
        let legacy_cost = legacy.to_json().unwrap().len() as u64;
        let mixed = [legacy.clone(), receipts[1].clone()];
        let mut alone = open("second.sqlite");
        alone.persist(&capture, &source, &receipts[1..2]).unwrap();
        let (_, second_cost) = receipt_storage(&alone.conn).unwrap();
        for (name, batch, share, retained) in [
            ("legacy-exact.sqlite", 1, (u64::MAX, legacy_cost), 1),
            ("legacy-below.sqlite", 1, (u64::MAX, legacy_cost - 1), 0),
            ("legacy-count.sqlite", 2, (1, u64::MAX), 1),
            (
                "mixed-exact.sqlite",
                2,
                (u64::MAX, legacy_cost + second_cost),
                2,
            ),
            (
                "mixed-below.sqlite",
                2,
                (u64::MAX, legacy_cost + second_cost - 1),
                1,
            ),
            ("legacy-none.sqlite", 2, (0, u64::MAX), 0),
        ] {
            let mut store = open(name);
            let mut kept = mixed[..batch].to_vec();
            store
                .persist_within(&capture, &source, &mixed[..batch], &[], share)
                .unwrap()
                .keep_retained(&mut kept);
            assert_eq!(kept, mixed[..retained], "{name}");
        }
    }

    #[test]
    fn prepared_source_guard_verifies_identity_without_capture_access() {
        // AC-0287: no capture or receipt has been persisted for this guard.
        let (directory, source, _, _, _) = fixture();
        let store = PrimarySourceStore::open(directory.path()).unwrap();
        let guard = store.task_guard(&source.source_id).unwrap();
        store.verify_task_guard(&guard).unwrap();
        let other_directory = tempfile::tempdir().unwrap();
        let other = PrimarySourceStore::open(other_directory.path()).unwrap();
        assert!(matches!(
            other.verify_task_guard(&guard),
            Err(PinnedReadError::Invalid)
        ));
        store.verify_task_guard(&guard).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn prepared_source_guard_rejects_replaced_lock_and_widened_permissions() {
        // AC-0287: retaining a handle does not prove its pathname still names it.
        use std::os::unix::fs::PermissionsExt;
        let (directory, source, _, _, _) = fixture();
        let store = PrimarySourceStore::open(directory.path()).unwrap();
        let guard = store.task_guard(&source.source_id).unwrap();
        guard
            ._file
            .set_permissions(std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(matches!(
            store.verify_task_guard(&guard),
            Err(PinnedReadError::Invalid)
        ));
        guard
            ._file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        store.verify_task_guard(&guard).unwrap();
        let name = format!("{}.lock", source.source_id);
        store
            .storage
            .locks
            .rename(&name, &store.storage.locks, "old-held.lock")
            .unwrap();
        let replacement = private_file(&store.storage.locks, name.as_ref(), true).unwrap();
        assert!(matches!(
            store.verify_task_guard(&guard),
            Err(PinnedReadError::Invalid)
        ));
        drop(replacement);
    }

    fn receipt_count(store: &ReceiptStore) -> i64 {
        store
            .conn
            .query_row("SELECT count(*) FROM receipts", [], |row| row.get(0))
            .unwrap()
    }

    // AC-0274: evaluation corpus identity comes from validated retained bytes,
    // not a dataset's claimed capture hash or a current working-tree read.
    #[test]
    fn evaluation_capture_manifest_revalidates_retained_objects_and_guard() {
        let (directory, source, capture, _, _) = fixture();
        let store = PrimarySourceStore::open(directory.path()).unwrap();
        store.captures.lock().unwrap().persist(&capture).unwrap();
        let guard = store.task_guard(&source.source_id).unwrap();
        assert_eq!(
            store
                .evaluation_capture(&guard, capture.id(), 32 * 1024 * 1024)
                .unwrap()
                .manifest()
                .clone(),
            *capture.manifest()
        );
        assert!(matches!(
            store.evaluation_capture(&guard, capture.id(), 0),
            Err(PinnedReadError::Invalid)
        ));
        let other_directory = tempfile::tempdir().unwrap();
        let other = PrimarySourceStore::open(other_directory.path()).unwrap();
        let wrong_guard = other.task_guard(&source.source_id).unwrap();
        assert!(matches!(
            store.evaluation_capture(&wrong_guard, capture.id(), 32 * 1024 * 1024),
            Err(PinnedReadError::Invalid)
        ));
        let conn = Connection::open(store.storage.path("captures.sqlite")).unwrap();
        conn.execute("UPDATE objects SET bytes=X'00'", []).unwrap();
        assert!(matches!(
            store.evaluation_capture(&guard, capture.id(), 32 * 1024 * 1024),
            Err(PinnedReadError::Invalid)
        ));
    }

    // AC-0153: schema/version validation remains active after open, and a
    // suppressed insert never becomes successful source-retention evidence.
    #[test]
    fn receipt_store_revalidates_schema_before_publication_and_reads() {
        let (_directory, source, capture, _, receipts) = fixture();
        for mutation in [
            "CREATE TRIGGER suppress_receipt BEFORE INSERT ON receipts BEGIN SELECT RAISE(IGNORE); END",
            "CREATE INDEX unexpected_receipt_index ON receipts(source_id)",
            "PRAGMA ignore_check_constraints=ON; UPDATE receipt_meta SET version=6",
            "DELETE FROM receipt_meta",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("receipts.sqlite");
            let mut store = ReceiptStore::open(&path).unwrap();
            store.persist(&capture, &source, &receipts).unwrap();
            let count = receipt_count(&store);
            store.conn.execute_batch(mutation).unwrap();
            assert!(store.persist(&capture, &source, &receipts).is_err());
            assert!(store.get(receipts[0].id()).is_err());
            assert!(store.ids(&source.source_id).is_err());
            assert!(store.receipt_status(receipts[0].id()).is_err());
            assert_eq!(receipt_count(&store), count);
            drop(store);
            assert!(ReceiptStore::open(&path).is_err());
        }
    }

    // AC-0153: length/type prechecks reject oversized indexed columns and bodies
    // before loading them; diagnostics never copy corrupt payload contents.
    #[test]
    fn receipt_store_bounds_all_stored_columns_before_loading() {
        let (_directory, source, capture, _, receipts) = fixture();
        for mutation in [
            "UPDATE receipts SET id=printf('%0257d',0) WHERE id=?1",
            "UPDATE receipts SET source_id=printf('%0100000d',0) WHERE id=?1",
            "UPDATE receipts SET repo_key=printf('%0100000d',0) WHERE id=?1",
            "UPDATE receipts SET payload=printf('%0131073d',0) WHERE id=?1",
            "UPDATE receipts SET payload='do-not-echo-private-input' WHERE id=?1",
            "UPDATE receipts SET capture_forgotten_tombstone=0 WHERE id=?1",
            "UPDATE receipts SET capture_forgotten_tombstone=99 WHERE id=?1",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
            store.persist(&capture, &source, &receipts).unwrap();
            let count = receipt_count(&store);
            store
                .conn
                .pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            store
                .conn
                .pragma_update(None, "foreign_keys", false)
                .unwrap();
            store.conn.execute(mutation, [receipts[0].id()]).unwrap();
            let error = store
                .get(receipts[0].id())
                .expect_err("corrupt receipt must fail");
            assert!(!error.contains("do-not-echo-private-input"));
            assert!(store.ids(&source.source_id).is_err());
            assert!(store.receipt_status(receipts[0].id()).is_err());
            assert!(store.persist(&capture, &source, &receipts).is_err());
            assert_eq!(receipt_count(&store), count);
        }
    }

    // AC-0153, AC-0837: a `source_id` column that disagrees with a row's
    // header or payload cannot hide a receipt. A normalized row fails the
    // bounded store-wide header check on every read; a legacy row (no header)
    // fails closed when the source its column names is read, and on `get`,
    // while the payload's source no longer decodes it (#1233).
    #[test]
    fn receipt_inventory_rejects_hidden_ownership_and_retains_history() {
        let (_directory, source, capture, _, receipts) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipts.sqlite");
        let mut store = ReceiptStore::open(&path).unwrap();
        store.persist(&capture, &source, &receipts).unwrap();
        let expected = store.ids(&source.source_id).unwrap();
        drop(store);
        let store = ReceiptStore::open(&path).unwrap();
        assert_eq!(store.ids(&source.source_id).unwrap(), expected);
        let wrong_source = format!("src_{}", "f".repeat(32));
        assert_ne!(wrong_source, source.source_id);
        store
            .conn
            .execute(
                "UPDATE receipts SET source_id=?1 WHERE id=?2",
                params![wrong_source, receipts[0].id()],
            )
            .unwrap();
        assert!(store.ids(&source.source_id).is_err());
        assert!(store.ids(&wrong_source).is_err());
        assert_eq!(receipt_count(&store), expected.len() as i64);
        store
            .conn
            .execute(
                "UPDATE receipts SET source_id=?1 WHERE id=?2",
                params![source.source_id, receipts[0].id()],
            )
            .unwrap();
        assert_eq!(store.ids(&source.source_id).unwrap(), expected);

        // A legacy row whose column names another source than its payload.
        store
            .conn
            .execute(
                "UPDATE receipts SET source_id=?1, header_id=NULL, payload=?2 WHERE id=?3",
                params![
                    wrong_source,
                    receipts[0].to_json().unwrap(),
                    receipts[0].id()
                ],
            )
            .unwrap();
        let without_hidden: Vec<String> = expected
            .iter()
            .filter(|id| id.as_str() != receipts[0].id())
            .cloned()
            .collect();
        assert_eq!(store.ids(&source.source_id).unwrap(), without_hidden);
        assert!(store.ids(&wrong_source).is_err());
        assert!(store.get(receipts[0].id()).is_err());
        assert_eq!(receipt_count(&store), expected.len() as i64);
    }

    // AC-1200/T-1200: binding decodes one named fact at a time and still
    // equals the decoded path when several receipts name one fact (the last
    // match binds it) and when an edge repeats (its occurrences merge).
    #[test]
    fn packed_binding_decodes_one_named_fact_at_a_time() {
        let (_directory, _, _, mut extraction, receipts) = fixture();
        let packed = |extraction: &adapters_lang_ts::Extraction, receipts: &[Receipt]| {
            let facts = core_graph::facts::FactStore::from_facts(
                extraction.nodes.clone(),
                extraction.edges.clone(),
            )
            .unwrap();
            matching_bindings_packed(&facts, receipts).unwrap()
        };
        let doubled: Vec<Receipt> = receipts
            .iter()
            .chain(receipts.iter().rev())
            .cloned()
            .collect();
        let before = matching_bindings(&extraction, &doubled);
        assert!(!before.is_empty());
        assert_eq!(packed(&extraction, &doubled), before);
        assert_eq!(before, matching_bindings(&extraction, &receipts));
        // Every edge occurs twice: the merged view decides what binds.
        let repeated = extraction.edges.clone();
        extraction.edges.extend(repeated);
        assert_eq!(
            packed(&extraction, &doubled),
            matching_bindings(&extraction, &doubled)
        );
    }

    // AC-0150: a later enrichment sharing an identity wins graph publication;
    // receipt matching follows that same final winner, never the first duplicate.
    #[test]
    fn primary_matching_uses_final_duplicate_fact_winners() {
        let (_directory, _, _, mut extraction, receipts) = fixture();
        // AC-0549 (#1574): packed facts bind exactly as decoded ones.
        let packed = |extraction: &adapters_lang_ts::Extraction| {
            let facts = core_graph::facts::FactStore::from_facts(
                extraction.nodes.clone(),
                extraction.edges.clone(),
            )
            .unwrap();
            matching_bindings_packed(&facts, &receipts).unwrap()
        };
        let before = matching_bindings(&extraction, &receipts);
        assert!(!before.is_empty());
        assert_eq!(packed(&extraction), before);
        assert!(
            matching_bindings_packed(&core_graph::facts::FactStore::default(), &[] as &[Receipt])
                .unwrap()
                .is_empty()
        );
        for binding in &before {
            match &binding.fact {
                FactKey::Node { id } => {
                    let mut changed = extraction
                        .nodes
                        .iter()
                        .find(|node| &node.id == id)
                        .unwrap()
                        .clone();
                    changed.props["later_enrichment"] = serde_json::json!(true);
                    extraction.nodes.push(changed);
                }
                FactKey::Edge {
                    source,
                    label,
                    destination,
                } => {
                    let mut changed = extraction
                        .edges
                        .iter()
                        .find(|edge| {
                            &edge.src == source && &edge.label == label && &edge.dst == destination
                        })
                        .unwrap()
                        .clone();
                    changed.props["later_enrichment"] = serde_json::json!(true);
                    extraction.edges.push(changed);
                }
            }
        }
        assert!(matching_bindings(&extraction, &receipts).is_empty());
        assert!(packed(&extraction).is_empty());
    }

    type SourceReceipts = (RegisteredSource, Vec<Receipt>);

    /// Two registered sources in one receipt store: `a` with one small file,
    /// `b` with several files so it holds more rows than `a`.
    fn two_source_store() -> (
        tempfile::TempDir,
        ReceiptStore,
        SourceReceipts,
        SourceReceipts,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.sqlite");
        let _findings = crate::findings::FindingStore::open(&state_path).unwrap();
        let mut registry =
            crate::sources::SourceRegistry::open(&state_path, directory.path()).unwrap();
        let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        let mut sources = Vec::new();
        for (name, files) in [("target-a", 1), ("target-b", 4)] {
            let target = directory.path().join(name);
            std::fs::create_dir(&target).unwrap();
            let paths: Vec<String> = (0..files).map(|file| format!("rule{file}.ts")).collect();
            for (file, path) in paths.iter().enumerate() {
                std::fs::write(
                    target.join(path),
                    format!(
                        "export function ready{file}(ok: boolean) {{ if (!ok) return false; return true; }}\n\
                         export function steady{file}(ok: boolean) {{ if (ok) return 1; return 0; }}"
                    ),
                )
                .unwrap();
            }
            let source = registry.register_local(&target).unwrap();
            let capture = source_capture::capture_working_tree(
                &target,
                &source_id(&source.source_id).unwrap(),
                &paths
                    .iter()
                    .map(|path| path.as_str().into())
                    .collect::<Vec<_>>(),
                CaptureLimits::default(),
            )
            .unwrap();
            let mut receipts = Vec::new();
            for path in &paths {
                let (_, file_receipts) = captured::extract_file(
                    capture.file(path).unwrap(),
                    &adapters_lang_ts::SourceId {
                        repo: &source.repo_key,
                        commit: "workdir",
                    },
                )
                .unwrap();
                receipts.extend(file_receipts);
            }
            assert!(!receipts.is_empty());
            store.persist(&capture, &source, &receipts).unwrap();
            sources.push((source, receipts));
        }
        let b = sources.pop().unwrap();
        let a = sources.pop().unwrap();
        assert!(b.1.len() > a.1.len());
        (directory, store, a, b)
    }

    fn decodes_during<T>(operation: impl FnOnce() -> T) -> (T, usize) {
        let before = STORED_RECEIPT_DECODES.with(std::cell::Cell::get);
        let result = operation();
        let after = STORED_RECEIPT_DECODES.with(std::cell::Cell::get);
        (result, after - before)
    }

    /// The pre-#1233 inventory: decode every stored row, keep the payloads
    /// that name `source`. The result the indexed read must reproduce.
    fn store_wide_inventory(store: &ReceiptStore, source: &str) -> Vec<String> {
        let mut statement = store
            .conn
            .prepare("SELECT id FROM receipts ORDER BY id")
            .unwrap();
        let all: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        all.into_iter()
            .filter(|id| {
                stored_receipt(&store.conn, id)
                    .unwrap()
                    .source_id()
                    .as_str()
                    == source
            })
            .collect()
    }

    // AC-0836: the supersession inventory of a source in a shared store returns
    // exactly the ids the store-wide decode returned, in the same order, with
    // unchanged receipt bytes.
    #[test]
    fn ac0836_source_inventory_matches_the_store_wide_result() {
        let (_directory, store, (a, receipts_a), (b, receipts_b)) = two_source_store();
        for (source, receipts) in [(&a, &receipts_a), (&b, &receipts_b)] {
            let ids = store.ids(&source.source_id).unwrap();
            assert_eq!(ids, store_wide_inventory(&store, &source.source_id));
            let mut expected: Vec<String> = receipts
                .iter()
                .map(|receipt| receipt.id().to_owned())
                .collect();
            expected.sort();
            expected.dedup();
            assert_eq!(ids, expected);
            for receipt in receipts {
                assert_eq!(&store.get(receipt.id()).unwrap(), receipt);
            }
        }
    }

    // AC-0836: the inventory rebuilds and decodes only the source's own rows,
    // however many rows other sources hold.
    #[test]
    fn ac0836_source_inventory_decodes_only_its_own_rows() {
        let (_directory, store, (a, _), (b, _)) = two_source_store();
        let total = receipt_count(&store) as usize;
        let (ids_a, decodes_a) = decodes_during(|| store.ids(&a.source_id).unwrap());
        assert_eq!(decodes_a, ids_a.len());
        assert!(decodes_a < total);
        let (ids_b, decodes_b) = decodes_during(|| store.ids(&b.source_id).unwrap());
        assert_eq!(decodes_b, ids_b.len());
        assert_eq!(ids_a.len() + ids_b.len(), total);
        let unknown = format!("src_{}", "e".repeat(32));
        let (ids, decodes) = decodes_during(|| store.ids(&unknown).unwrap());
        assert!(ids.is_empty());
        assert_eq!(decodes, 0);
    }

    // AC-0837: a corrupt row of another source no longer fails this source's
    // inventory; it still fails closed on its own source's inventory and read.
    #[test]
    fn ac0837_foreign_corruption_fails_closed_on_its_own_source_only() {
        let (_directory, store, (a, receipts_a), (b, receipts_b)) = two_source_store();
        let expected_a = store.ids(&a.source_id).unwrap();
        let count = receipt_count(&store);
        store
            .conn
            .execute(
                "UPDATE receipts SET payload='do-not-echo-private-input' WHERE id=?1",
                [receipts_b[0].id()],
            )
            .unwrap();
        assert_eq!(store.ids(&a.source_id).unwrap(), expected_a);
        assert_eq!(store.get(receipts_a[0].id()).unwrap(), receipts_a[0]);
        let error = store.ids(&b.source_id).expect_err("corrupt row must fail");
        assert!(!error.contains("do-not-echo-private-input"));
        assert!(store.get(receipts_b[0].id()).is_err());
        assert_eq!(receipt_count(&store), count);
    }

    // AC-0153: private storage rejects known symlink/hardlink substitutions and
    // directory replacement. Fresh OS handles coordinate independent app opens.
    #[cfg(unix)]
    #[test]
    fn primary_storage_rejects_substitution_and_releases_guards() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let source = format!("src_{}", "a".repeat(32));
        let directory = tempfile::tempdir().unwrap();
        let store = PrimarySourceStore::open(directory.path()).unwrap();
        let other = PrimarySourceStore::open(directory.path()).unwrap();
        let shared = store.guard(&source, false).unwrap();
        assert!(other.guard(&source, true).is_err());
        drop(shared);
        drop(other.guard(&source, true).unwrap());
        for (path, mode) in [
            (directory.path().join("retained-source"), 0o700),
            (
                directory.path().join("retained-source/captures.sqlite"),
                0o600,
            ),
            (
                directory
                    .path()
                    .join(format!("retained-source/locks/{source}.lock")),
                0o600,
            ),
        ] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                mode
            );
        }
        let locks = directory.path().join("retained-source/locks");
        std::fs::rename(&locks, directory.path().join("old-locks")).unwrap();
        std::fs::create_dir(&locks).unwrap();
        std::fs::set_permissions(&locks, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(store.guard(&source, false).is_err());

        for name in [
            "captures.sqlite",
            "receipts.sqlite",
            "captures.sqlite-wal",
            "receipts.sqlite-shm",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("retained-source");
            std::fs::create_dir(&root).unwrap();
            let outside = directory.path().join("outside");
            std::fs::write(&outside, b"untouched").unwrap();
            symlink(&outside, root.join(name)).unwrap();
            assert!(PrimarySourceStore::open(directory.path()).is_err());
            assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("retained-source");
        std::fs::create_dir(&root).unwrap();
        let outside = directory.path().join("outside");
        std::fs::write(&outside, b"untouched").unwrap();
        std::fs::hard_link(&outside, root.join("captures.sqlite")).unwrap();
        assert!(PrimarySourceStore::open(directory.path()).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
    }

    // AC-0153: initially supplied aliases remain supported, but a later
    // ancestor/final symlink cannot redirect revalidation to the same inode.
    #[cfg(unix)]
    #[test]
    fn primary_storage_revalidates_canonical_components_without_following_replacements() {
        use std::os::unix::fs::symlink;
        let fixture = tempfile::tempdir().unwrap();
        let parent = fixture.path().join("parent");
        let app = parent.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let alias = fixture.path().join("initial-alias");
        symlink(&app, &alias).unwrap();
        let store = PrimarySourceStore::open(&alias).unwrap();
        store.storage.verify().unwrap();
        assert_eq!(store.storage.app_path, dunce::canonicalize(&app).unwrap());

        for replaced in [&app, &parent] {
            let moved = fixture.path().join("moved");
            std::fs::rename(replaced, &moved).unwrap();
            symlink(&moved, replaced).unwrap();
            assert!(store.storage.verify().is_err());
            std::fs::remove_file(replaced).unwrap();
            std::fs::create_dir(replaced).unwrap();
            assert!(store.storage.verify().is_err());
            std::fs::remove_dir(replaced).unwrap();
            std::fs::rename(&moved, replaced).unwrap();
            store.storage.verify().unwrap();
        }
    }

    pub(super) fn schema_objects(
        conn: &Connection,
    ) -> Vec<(String, String, String, Option<String>)> {
        let mut stmt = conn
            .prepare(
                "SELECT type, name, tbl_name, sql
                 FROM sqlite_master
                 WHERE name NOT GLOB 'sqlite_*'
                 ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    fn v1_rows(conn: &Connection) -> Vec<(String, String, String, String)> {
        let mut stmt = conn
            .prepare("SELECT id, source_id, repo_key, payload FROM receipts ORDER BY id")
            .unwrap();
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    #[test]
    fn receipt_store_v1_to_v2_migration_is_atomic_exact_and_reopenable() {
        let (_directory, source, _capture, _, receipts) = fixture();
        assert!(receipts.len() >= 2);
        let directory = tempfile::tempdir().unwrap();
        let v1_path = directory.path().join("migrated-receipts.sqlite");
        let conn = Connection::open(&v1_path).unwrap();
        conn.execute_batch(RECEIPTS_SCHEMA_V1).unwrap();
        conn.execute_batch(META_SCHEMA_V1).unwrap();
        conn.execute("INSERT INTO receipt_meta VALUES (1)", [])
            .unwrap();
        let mut expected_rows = Vec::new();
        for receipt in &receipts {
            let payload = receipt.to_json().unwrap();
            conn.execute(
                "INSERT INTO receipts (id, source_id, repo_key, payload) VALUES (?1, ?2, ?3, ?4)",
                params![receipt.id(), source.source_id, source.repo_key, payload],
            )
            .unwrap();
            expected_rows.push((
                receipt.id().to_owned(),
                source.source_id.clone(),
                source.repo_key.clone(),
                payload,
            ));
        }
        expected_rows.sort_by(|a, b| a.0.cmp(&b.0));
        let expected_v1_objects = schema_objects(&conn);
        assert_eq!(expected_v1_objects.len(), 2);

        // Pre-create a conflicting table so migrate_v1_to_v2 fails after
        // ALTER TABLE receipts ADD COLUMN ... and CREATE TABLE receipt_tombstones ...
        conn.execute_batch("CREATE TABLE receipt_reclaims (x INTEGER)")
            .unwrap();
        drop(conn);
        assert!(ReceiptStore::open(&v1_path).is_err());

        let conn = Connection::open(&v1_path).unwrap();
        conn.execute_batch("DROP TABLE receipt_reclaims").unwrap();
        assert_eq!(schema_objects(&conn), expected_v1_objects);
        assert_eq!(
            conn.query_row("SELECT version FROM receipt_meta", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(v1_rows(&conn), expected_rows);

        // An unexpected index on v1 also fails closed and rolls back completely.
        conn.execute_batch("CREATE INDEX unexpected_v1_idx ON receipts(source_id)")
            .unwrap();
        drop(conn);
        assert!(ReceiptStore::open(&v1_path).is_err());
        let conn = Connection::open(&v1_path).unwrap();
        conn.execute_batch("DROP INDEX unexpected_v1_idx").unwrap();
        assert_eq!(schema_objects(&conn), expected_v1_objects);
        assert_eq!(v1_rows(&conn), expected_rows);
        drop(conn);

        // Migrate v1 -> v2 atomically on open.
        let migrated = ReceiptStore::open(&v1_path).unwrap();
        let fresh_path = directory.path().join("fresh-receipts.sqlite");
        let fresh = ReceiptStore::open(&fresh_path).unwrap();
        let migrated_objects = schema_objects(&migrated.conn);
        let fresh_objects = schema_objects(&fresh.conn);
        assert_eq!(migrated_objects.len(), 6 + RECEIPT_INDEXES.len());
        assert_eq!(migrated_objects, fresh_objects);

        let mut migrated_rows: Vec<(String, String, String, String, i64)> = migrated
            .conn
            .prepare(
                "SELECT id, source_id, repo_key, payload, capture_forgotten_tombstone IS NOT NULL
                 FROM receipts
                 ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            migrated_rows
                .iter()
                .map(|(id, src, repo, payload, _)| (
                    id.clone(),
                    src.clone(),
                    repo.clone(),
                    payload.clone()
                ))
                .collect::<Vec<_>>(),
            expected_rows
        );
        assert!(migrated_rows.drain(..).all(|(_, _, _, _, cf)| cf == 0));
        for receipt in &receipts {
            assert_eq!(&migrated.get(receipt.id()).unwrap(), receipt);
            assert_eq!(
                migrated.receipt_status(receipt.id()).unwrap(),
                ReceiptStatus::Present
            );
        }
        let expected_storage = receipt_storage(&migrated.conn).unwrap();
        drop(migrated);

        // Reopen migrated v2 DB.
        let reopened = ReceiptStore::open(&v1_path).unwrap();
        assert_eq!(schema_objects(&reopened.conn), fresh_objects);
        assert_eq!(receipt_storage(&reopened.conn).unwrap(), expected_storage);
        for receipt in &receipts {
            assert_eq!(&reopened.get(receipt.id()).unwrap(), receipt);
            assert_eq!(
                reopened.receipt_status(receipt.id()).unwrap(),
                ReceiptStatus::Present
            );
        }
    }

    #[test]
    fn receipt_store_bounds_tombstones_reclaims_and_capture_ids_truncation() {
        let (_directory, source, capture, _, receipts) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        store.persist(&capture, &source, &receipts).unwrap();

        let oversized_captures: Vec<String> =
            (0..1100).map(|i| format!("capture-v1:{i:064x}")).collect();
        let pins = std::collections::BTreeSet::from([receipts[0].id().to_owned()]);
        store
            .reclaim_forgotten(&source, &oversized_captures, &pins)
            .unwrap();
        let (capture_ids_json, truncated): (String, i64) = store
            .conn
            .query_row(
                "SELECT capture_ids, capture_ids_truncated FROM receipt_tombstones",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(capture_ids_json, "[]");
        assert_eq!(truncated, 1);

        for mutation in [
            "UPDATE receipt_tombstones SET source_id='src_ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ'",
            "UPDATE receipt_tombstones SET repo_key=''",
            "UPDATE receipt_tombstones SET capture_ids='['",
            "UPDATE receipt_tombstones SET capture_ids='[\"capture-v1:0\"]', capture_ids_truncated=1",
            "UPDATE receipt_tombstones SET forgotten_at='2026-10-22 00:00:00Z'",
            "UPDATE receipt_tombstones SET receipts_reclaimed=0, receipts_retained_pinned=0",
            "UPDATE receipt_reclaims SET receipt_id='' WHERE rowid=1",
            "PRAGMA foreign_keys=OFF; UPDATE receipt_reclaims SET tombstone_id=999",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
            store.persist(&capture, &source, &receipts).unwrap();
            store
                .reclaim_forgotten(&source, &[capture.id().to_owned()], &pins)
                .unwrap();
            store
                .conn
                .pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            store.conn.execute_batch(mutation).unwrap();
            assert!(
                store.receipt_status(receipts[0].id()).is_err(),
                "mutation must fail validation: {mutation}"
            );
            assert!(store.ids(&source.source_id).is_err());
        }
    }

    fn tombstone_count(store: &ReceiptStore) -> i64 {
        store
            .conn
            .query_row("SELECT count(*) FROM receipt_tombstones", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    // #748 review: a pinned receipt binds the tombstone of the forget that
    // marked it, so repeated forgets of the same source neither keep every
    // later tombstone alive nor lend the old receipt a later operation.
    #[test]
    fn repeated_forgets_keep_tombstones_bounded_and_pinned_status_exact() {
        let (_directory, source, capture, _, receipts) = fixture();
        assert!(
            receipts.len() >= 2,
            "fixture must yield a pinned and an unpinned receipt"
        );
        let directory = tempfile::tempdir().unwrap();
        let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        store.persist(&capture, &source, &receipts).unwrap();
        let pinned = receipts[0].id().to_owned();
        let pins = std::collections::BTreeSet::from([pinned.clone()]);
        store
            .reclaim_forgotten(&source, &[capture.id().to_owned()], &pins)
            .unwrap();
        let first = match store.receipt_status(&pinned).unwrap() {
            ReceiptStatus::CaptureForgotten { tombstone } => tombstone,
            other => panic!("pinned receipt must be capture-forgotten, got {other:?}"),
        };
        assert_eq!(first.receipts_retained_pinned, 1);
        let unpinned = receipts.len() as u64 - 1;
        let ledger_bound = unpinned;
        for _ in 0..6 {
            store.capacity.0 = MAX_RECEIPTS;
            store.persist(&capture, &source, &receipts[1..]).unwrap();
            store.capacity.0 = ledger_bound;
            store
                .reclaim_forgotten(&source, &[capture.id().to_owned()], &pins)
                .unwrap();
            // The first tombstone (held by the pinned row) plus at most one
            // per surviving ledger row.
            assert!(tombstone_count(&store) <= 1 + ledger_bound as i64);
            assert_eq!(
                store.receipt_status(&pinned).unwrap(),
                ReceiptStatus::CaptureForgotten {
                    tombstone: first.clone()
                }
            );
        }
        assert_eq!(tombstone_count(&store), 2);
        drop(store);
        ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
    }

    // #748 review: identical capture bytes reappearing make a pinned,
    // capture-forgotten receipt present again, and a tombstone held only by
    // that mark is dropped.
    #[test]
    fn recapturing_identical_bytes_clears_the_forgotten_mark() {
        let (_directory, source, capture, _, receipts) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        store.persist(&capture, &source, &receipts).unwrap();
        let pins: std::collections::BTreeSet<String> = receipts
            .iter()
            .map(|receipt| receipt.id().to_owned())
            .collect();
        store
            .reclaim_forgotten(&source, &[capture.id().to_owned()], &pins)
            .unwrap();
        assert!(matches!(
            store.receipt_status(receipts[0].id()).unwrap(),
            ReceiptStatus::CaptureForgotten { .. }
        ));
        assert_eq!(tombstone_count(&store), 1);
        let retained = store.persist(&capture, &source, &receipts).unwrap();
        assert_eq!(retained.ids.len(), pins.len());
        for receipt in &receipts {
            assert_eq!(
                store.receipt_status(receipt.id()).unwrap(),
                ReceiptStatus::Present
            );
        }
        assert_eq!(tombstone_count(&store), 0);
    }

    // #748 review: tombstone capture ids share one store-wide byte pool;
    // once it is full a forget is still recorded, with its ids truncated.
    #[test]
    fn tombstone_capture_ids_share_a_store_wide_byte_pool() {
        let (_directory, source, capture, _, receipts) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        let pins = std::collections::BTreeSet::new();
        let ids = [capture.id().to_owned()];
        let one = encode_capture_ids(&ids).unwrap().0.len() as u64;
        store.persist(&capture, &source, &receipts).unwrap();
        store.capacity.1 = one;
        store.reclaim_forgotten(&source, &ids, &pins).unwrap();
        store.capacity.1 = MAX_RECEIPT_STORE_BYTES;
        store.persist(&capture, &source, &receipts).unwrap();
        store.capacity.1 = one;
        store.reclaim_forgotten(&source, &ids, &pins).unwrap();
        let rows: Vec<(String, i64)> = store
            .conn
            .prepare(
                "SELECT capture_ids, capture_ids_truncated FROM receipt_tombstones ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        // The second forget replaced every ledger row, so the first tombstone
        // is orphaned and gone; the pool was full when the second was written.
        assert_eq!(rows, vec![("[]".to_owned(), 1)]);
        for receipt in &receipts {
            assert!(matches!(
                store.receipt_status(receipt.id()).unwrap(),
                ReceiptStatus::Reclaimed { .. }
            ));
        }
    }

    // ADR-0046 decision 5: a superseded reclaim deletes rows outside `keep`
    // and leaves kept and capture-forgotten rows untouched.
    #[test]
    fn superseded_reclaim_spares_kept_and_capture_forgotten_rows() {
        let (_directory, source, capture, _, receipts) = fixture();
        assert!(receipts.len() >= 3);
        let directory = tempfile::tempdir().unwrap();
        let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        store.persist(&capture, &source, &receipts).unwrap();
        let forgotten = receipts[0].id().to_owned();
        let kept = receipts[1].id().to_owned();
        let pins = std::collections::BTreeSet::from([forgotten.clone()]);
        let ids = [capture.id().to_owned()];
        // Mark one row capture-forgotten, then bring the rest back.
        store.reclaim_forgotten(&source, &ids, &pins).unwrap();
        store.persist(&capture, &source, &receipts[1..]).unwrap();
        let keep = std::collections::BTreeSet::from([kept.clone()]);
        let reclaimed = receipts.len() as u64 - 2;
        assert_eq!(
            store.reclaim_superseded(&source, &ids, &keep).unwrap(),
            reclaimed
        );
        assert_eq!(store.reclaim_superseded(&source, &ids, &keep).unwrap(), 0);
        assert!(matches!(
            store.receipt_status(&forgotten).unwrap(),
            ReceiptStatus::CaptureForgotten { .. }
        ));
        assert_eq!(store.receipt_status(&kept).unwrap(), ReceiptStatus::Present);
        for receipt in &receipts[2..] {
            match store.receipt_status(receipt.id()).unwrap() {
                ReceiptStatus::Superseded { tombstone } => {
                    assert_eq!(tombstone.receipts_reclaimed, reclaimed);
                    assert_eq!(tombstone.receipts_retained_pinned, 0);
                }
                other => panic!("expected Superseded, got {other:?}"),
            }
        }
        let causes: Vec<String> = store
            .conn
            .prepare("SELECT cause FROM receipt_tombstones ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(causes, ["forgotten", "superseded"]);
        // Recapturing a superseded receipt makes it present again.
        store.persist(&capture, &source, &receipts[2..]).unwrap();
        assert_eq!(
            store.receipt_status(receipts[2].id()).unwrap(),
            ReceiptStatus::Present
        );
        drop(store);
        ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
    }

    #[test]
    fn tombstone_cause_is_validated() {
        let (_directory, source, capture, _, receipts) = fixture();
        let ids = [capture.id().to_owned()];
        let pins = std::collections::BTreeSet::from([receipts[0].id().to_owned()]);
        for (superseded, mutation) in [
            (true, "UPDATE receipt_tombstones SET cause='expired'"),
            (true, "UPDATE receipt_tombstones SET cause=''"),
            (
                true,
                "UPDATE receipt_tombstones SET receipts_retained_pinned=1",
            ),
            // A forget's tombstone holding a capture-forgotten mark cannot
            // claim supersession.
            (
                false,
                "UPDATE receipt_tombstones SET cause='superseded', receipts_retained_pinned=0",
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
            store.persist(&capture, &source, &receipts).unwrap();
            if superseded {
                store.reclaim_superseded(&source, &ids, &pins).unwrap();
            } else {
                store.reclaim_forgotten(&source, &ids, &pins).unwrap();
            }
            store.receipt_status(receipts[1].id()).unwrap();
            store
                .conn
                .pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            store.conn.execute_batch(mutation).unwrap();
            assert!(
                store.receipt_status(receipts[1].id()).is_err(),
                "mutation must fail validation: {mutation}"
            );
        }
    }

    // ADR-0046 decision 5: v2 tombstones all record a forget; v2 -> v3 adds
    // their cause atomically and matches a fresh store exactly.
    #[test]
    fn receipt_store_v2_to_v3_migration_backfills_forgotten_cause() {
        let (_directory, source, capture, _, receipts) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v2-receipts.sqlite");
        let pins = std::collections::BTreeSet::from([receipts[0].id().to_owned()]);
        {
            let mut store = ReceiptStore::open(&path).unwrap();
            store.persist(&capture, &source, &receipts).unwrap();
            store
                .reclaim_forgotten(&source, &[capture.id().to_owned()], &pins)
                .unwrap();
        }
        let expected: Vec<ReceiptStatus> = {
            let store = ReceiptStore::open(&path).unwrap();
            receipts
                .iter()
                .map(|receipt| store.receipt_status(receipt.id()).unwrap())
                .collect()
        };
        // Rewrite the tables as v2 left them.
        let pinned_legacy_payload = receipts[0].to_json().unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys=OFF;
             BEGIN;
             CREATE TABLE receipts_v2 AS SELECT id, source_id, repo_key, payload,
                 capture_forgotten_tombstone FROM receipts;
             DROP TABLE receipts;
             DROP TABLE receipt_headers;
             DROP TABLE input_closures;
             {RECEIPTS_SCHEMA_V3};
             INSERT INTO receipts SELECT * FROM receipts_v2;
             DROP TABLE receipts_v2;
             CREATE TABLE tombstones_v2 AS SELECT id, source_id, repo_key, capture_ids,
                 capture_ids_truncated, forgotten_at, receipts_reclaimed,
                 receipts_retained_pinned FROM receipt_tombstones;
             DROP TABLE receipt_tombstones;
             {TOMBSTONES_SCHEMA_V2};
             INSERT INTO receipt_tombstones SELECT * FROM tombstones_v2;
             DROP TABLE tombstones_v2;
             DROP TABLE receipt_meta;
             {META_SCHEMA_V2};
             INSERT INTO receipt_meta VALUES (2);
             COMMIT;"
        ))
        .unwrap();
        conn.execute(
            "UPDATE receipts SET payload = ?1 WHERE id = ?2",
            params![pinned_legacy_payload, receipts[0].id()],
        )
        .unwrap();
        conn.execute_batch(RECEIPT_INDEXES[1].1).unwrap();
        conn.execute_batch(RECEIPT_INDEXES[2].1).unwrap();
        let v2_objects = schema_objects(&conn);
        // An unexpected object fails closed and rolls the migration back.
        conn.execute_batch("CREATE INDEX unexpected_v2_idx ON receipts(source_id)")
            .unwrap();
        drop(conn);
        assert!(ReceiptStore::open(&path).is_err());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP INDEX unexpected_v2_idx").unwrap();
        assert_eq!(schema_objects(&conn), v2_objects);
        drop(conn);

        let migrated = ReceiptStore::open(&path).unwrap();
        let fresh = ReceiptStore::open(&directory.path().join("fresh.sqlite")).unwrap();
        assert_eq!(schema_objects(&migrated.conn), schema_objects(&fresh.conn));
        let causes: Vec<String> = migrated
            .conn
            .prepare("SELECT cause FROM receipt_tombstones")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(causes, ["forgotten"]);
        for (receipt, status) in receipts.iter().zip(&expected) {
            assert_eq!(&migrated.receipt_status(receipt.id()).unwrap(), status);
        }
        drop(migrated);
        ReceiptStore::open(&path).unwrap();
    }

    fn canonical_json_value(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => serde_json::Value::Object(
                object
                    .into_iter()
                    .map(|(k, v)| (k, canonical_json_value(v)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(canonical_json_value).collect())
            }
            other => other,
        }
    }

    fn resign_v3_receipt(mut raw: serde_json::Value) -> Receipt {
        let content_bytes =
            serde_json::to_vec(&canonical_json_value(raw["content"].clone())).unwrap();
        let mut domain_input = b"cartograph:ts-primary-receipt:v3\0".to_vec();
        domain_input.extend_from_slice(&content_bytes);
        raw["receipt_id"] = serde_json::Value::String(format!(
            "ts-primary-v3:{}",
            core_prov::content_hash(&domain_input)
        ));
        Receipt::from_json(&serde_json::to_string(&raw).unwrap()).unwrap()
    }

    fn mint_v2_receipt_from_v3(template: &Receipt) -> Receipt {
        let mut raw: serde_json::Value =
            serde_json::from_str(&template.to_json().unwrap()).unwrap();
        raw["content"]["schema_version"] = serde_json::json!(2);
        raw["content"].as_object_mut().unwrap().remove("commit_sha");
        raw["content"]["producer_contract"] = serde_json::json!("t0.adapter-ts/direct-lexical-v2");
        let full_ranges = serde_json::to_value(template.ranges()).unwrap();
        raw["content"]["ranges"] = full_ranges;
        let content_bytes =
            serde_json::to_vec(&canonical_json_value(raw["content"].clone())).unwrap();
        let mut domain_input = b"cartograph:ts-primary-receipt:v2\0".to_vec();
        domain_input.extend_from_slice(&content_bytes);
        raw["receipt_id"] = serde_json::Value::String(format!(
            "ts-primary-v2:{}",
            core_prov::content_hash(&domain_input)
        ));
        let receipt = Receipt::from_json(&serde_json::to_string(&raw).unwrap()).unwrap();
        assert!(receipt.id().starts_with("ts-primary-v2:"));
        receipt
    }

    // AC-0373: persisting v3 receipts from a file stores one per-file header and
    // N compact bodies, reduces stored byte count below the sum of full JSON
    // payloads, and round-trips byte-identically (covering both stripped and
    // verbatim fact IDs plus legacy fallback for non-v3 receipts).
    #[test]
    fn ac0373_normalized_v3_receipts_round_trip_with_single_header_and_reduced_bytes() {
        let (_directory, source, capture, _, receipts) = fixture();
        assert!(receipts.len() >= 3);
        assert!(
            receipts
                .iter()
                .any(|r| matches!(r.fact_key(), FactKey::Edge { .. })),
            "fixture should include edge receipts as well as node receipts"
        );

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receipts.sqlite");
        let mut store = ReceiptStore::open(&path).unwrap();
        let retained = store.persist(&capture, &source, &receipts).unwrap();
        assert_eq!(retained.ids.len(), receipts.len());
        assert!(retained.omitted.is_empty());

        let header_count: i64 = store
            .conn
            .query_row("SELECT count(*) FROM receipt_headers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(header_count, 1);

        let normalized_count: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM receipts WHERE header_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(normalized_count, receipts.len() as i64);

        let full_payload_bytes: u64 = receipts
            .iter()
            .map(|r| r.to_json().unwrap().len() as u64)
            .sum();
        let (stored_count, stored_bytes) = receipt_storage(&store.conn).unwrap();
        assert_eq!(stored_count, receipts.len() as u64);
        assert!(
            stored_bytes < full_payload_bytes,
            "normalized storage ({stored_bytes} bytes) must be smaller than full payloads ({full_payload_bytes} bytes)"
        );

        for receipt in &receipts {
            let loaded = store.get(receipt.id()).unwrap();
            assert_eq!(&loaded, receipt);
            assert_eq!(loaded.to_json().unwrap(), receipt.to_json().unwrap());
            let payload: String = store
                .conn
                .query_row(
                    "SELECT payload FROM receipts WHERE id = ?1",
                    [receipt.id()],
                    |r| r.get(0),
                )
                .unwrap();
            let body: serde_json::Value = serde_json::from_str(&payload).unwrap();
            match receipt.fact_key() {
                FactKey::Node { .. } => {
                    assert!(body.get("k").is_some());
                    assert!(body.get("K").is_none());
                }
                FactKey::Edge { .. } => {
                    assert!(body.get("e").is_some());
                    assert!(body.get("E").is_none());
                }
            }
        }

        // Also verify verbatim fact_key fallback ("K" and "E") and legacy v2 fallback.
        let mut raw_verbatim_node: serde_json::Value =
            serde_json::from_str(&receipts[0].to_json().unwrap()).unwrap();
        raw_verbatim_node["content"]["fact_key"] = serde_json::json!({
            "kind": "node",
            "id": "custom_unprefixed_node_id"
        });
        let verbatim_node = resign_v3_receipt(raw_verbatim_node);

        let edge_template = receipts
            .iter()
            .find(|r| matches!(r.fact_key(), FactKey::Edge { .. }))
            .unwrap();
        let mut raw_verbatim_edge: serde_json::Value =
            serde_json::from_str(&edge_template.to_json().unwrap()).unwrap();
        raw_verbatim_edge["content"]["fact_key"] = serde_json::json!({
            "kind": "edge",
            "source": "custom_src_without_repo_prefix",
            "label": "GOVERNS",
            "destination": "custom_dst_without_repo_prefix"
        });
        let verbatim_edge = resign_v3_receipt(raw_verbatim_edge);

        let legacy_v2 = mint_v2_receipt_from_v3(&receipts[0]);
        let extra = [
            verbatim_node.clone(),
            verbatim_edge.clone(),
            legacy_v2.clone(),
        ];
        let retained_extra = store.persist(&capture, &source, &extra).unwrap();
        assert_eq!(retained_extra.ids.len(), 3);

        // Still 1 shared header (the two verbatim v3 receipts share the file's header;
        // the v2 receipt is stored legacy-style with header_id IS NULL).
        let header_count_after: i64 = store
            .conn
            .query_row("SELECT count(*) FROM receipt_headers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(header_count_after, 1);

        let v_node_payload: String = store
            .conn
            .query_row(
                "SELECT payload FROM receipts WHERE id = ?1",
                [verbatim_node.id()],
                |r| r.get(0),
            )
            .unwrap();
        let v_node_body: serde_json::Value = serde_json::from_str(&v_node_payload).unwrap();
        assert_eq!(v_node_body["K"], "custom_unprefixed_node_id");
        assert!(v_node_body.get("k").is_none());

        let v_edge_payload: String = store
            .conn
            .query_row(
                "SELECT payload FROM receipts WHERE id = ?1",
                [verbatim_edge.id()],
                |r| r.get(0),
            )
            .unwrap();
        let v_edge_body: serde_json::Value = serde_json::from_str(&v_edge_payload).unwrap();
        assert!(v_edge_body.get("E").is_some());
        assert!(v_edge_body.get("e").is_none());

        let (v2_payload, v2_header_id): (String, Option<String>) = store
            .conn
            .query_row(
                "SELECT payload, header_id FROM receipts WHERE id = ?1",
                [legacy_v2.id()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(v2_header_id.is_none());
        assert_eq!(v2_payload, legacy_v2.to_json().unwrap());

        for r in &extra {
            let loaded = store.get(r.id()).unwrap();
            assert_eq!(&loaded, r);
            assert_eq!(loaded.to_json().unwrap(), r.to_json().unwrap());
        }
    }

    // AC-0373: headers are never shared across sources even for identical file
    // content/path, and distinct grammars or producer contracts produce distinct
    // headers.
    #[test]
    fn ac0373_headers_are_isolated_by_source_and_distinct_across_grammars_and_producers() {
        let directory = tempfile::tempdir().unwrap();
        let target_a = directory.path().join("target-a");
        let target_b = directory.path().join("target-b");
        std::fs::create_dir(&target_a).unwrap();
        std::fs::create_dir(&target_b).unwrap();
        let content = b"export function check(x: boolean) { if (!x) return 0; return 1; }";
        std::fs::write(target_a.join("rule.ts"), content).unwrap();
        std::fs::write(target_a.join("rule.tsx"), content).unwrap();
        std::fs::write(target_b.join("rule.ts"), content).unwrap();

        let state_path = directory.path().join("state.sqlite");
        let _findings = crate::findings::FindingStore::open(&state_path).unwrap();
        let mut registry =
            crate::sources::SourceRegistry::open(&state_path, directory.path()).unwrap();
        let source_a = registry.register_local(&target_a).unwrap();
        let source_b = registry.register_local(&target_b).unwrap();
        assert_ne!(source_a.source_id, source_b.source_id);

        let capture_a = source_capture::capture_working_tree(
            &target_a,
            &source_id(&source_a.source_id).unwrap(),
            &["rule.ts".into(), "rule.tsx".into()],
            CaptureLimits::default(),
        )
        .unwrap();
        let capture_b = source_capture::capture_working_tree(
            &target_b,
            &source_id(&source_b.source_id).unwrap(),
            &["rule.ts".into()],
            CaptureLimits::default(),
        )
        .unwrap();

        let (_, receipts_a_ts) = captured::extract_file(
            capture_a.file("rule.ts").unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source_a.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        let (_, receipts_a_tsx) = captured::extract_file(
            capture_a.file("rule.tsx").unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source_a.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        let (_, receipts_b_ts) = captured::extract_file(
            capture_b.file("rule.ts").unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source_b.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();

        let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
        store
            .persist(&capture_a, &source_a, &receipts_a_ts)
            .unwrap();
        store
            .persist(&capture_b, &source_b, &receipts_b_ts)
            .unwrap();

        // Two sources with identical file content and path do not share a header.
        let headers_two_sources: Vec<(String, String, String)> = store
            .conn
            .prepare("SELECT id, source_id, repo_key FROM receipt_headers ORDER BY source_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(headers_two_sources.len(), 2);
        assert_ne!(headers_two_sources[0].0, headers_two_sources[1].0);

        // Different grammar (tsx vs typescript) in the same source gets a distinct header.
        store
            .persist(&capture_a, &source_a, &receipts_a_tsx)
            .unwrap();
        let header_count: i64 = store
            .conn
            .query_row("SELECT count(*) FROM receipt_headers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(header_count, 3);

        // Distinct producer_contract or commit_sha on the same file also produces a distinct header ID.
        let norm_base =
            normalize_v3_receipt(&receipts_a_ts[0], &receipts_a_ts[0].to_json().unwrap()).unwrap();
        let mut alt_header: ReceiptHeader = serde_json::from_str(&norm_base.header_json).unwrap();
        alt_header.producer_contract = "t0.adapter-ts/direct-lexical-v3-alt".into();
        let alt_header_json = serde_json::to_string(&alt_header).unwrap();
        assert_ne!(receipt_header_id(&alt_header_json), norm_base.header_id);

        for r in receipts_a_ts
            .iter()
            .chain(receipts_a_tsx.iter())
            .chain(receipts_b_ts.iter())
        {
            assert_eq!(&store.get(r.id()).unwrap(), r);
        }
    }

    // AC-0373: corrupt header (edited JSON, foreign source_id/repo_key, deleted
    // row with foreign keys off) and corrupt body fail closed with Invalid,
    // never returning a wrong receipt.
    #[test]
    fn ac0373_corrupt_header_and_body_fail_closed_as_invalid() {
        let (_directory, source, capture, _, receipts) = fixture();
        let target_id = receipts[0].id();
        let wrong_source = format!("src_{}", "f".repeat(32));

        type StoreMutation<'a> = Box<dyn Fn(&Connection) + 'a>;
        let header_mutations: Vec<StoreMutation<'_>> = vec![
            // 1. Edit header JSON (change commit_sha).
            Box::new(|conn| {
                conn.execute(
                    "UPDATE receipt_headers SET header = replace(header, '\"workdir\"', '\"tampered\"')",
                    [],
                )
                .unwrap();
            }),
            // 2. Append trailing whitespace to header JSON (non-canonical header bytes).
            Box::new(|conn| {
                conn.execute("UPDATE receipt_headers SET header = header || ' '", [])
                    .unwrap();
            }),
            // 3. Malformed header JSON.
            Box::new(|conn| {
                conn.execute("UPDATE receipt_headers SET header = '{bad'", [])
                    .unwrap();
            }),
            // 4. Change header source_id to another source.
            Box::new({
                let wrong_source = wrong_source.clone();
                move |conn| {
                    conn.execute("UPDATE receipt_headers SET source_id = ?1", [&wrong_source])
                        .unwrap();
                }
            }),
            // 5. Change header repo_key to another repo.
            Box::new(|conn| {
                conn.execute("UPDATE receipt_headers SET repo_key = 'other/repo'", [])
                    .unwrap();
            }),
            // 6. Delete header row with foreign_keys = OFF.
            Box::new(|conn| {
                conn.execute_batch("PRAGMA foreign_keys = OFF; DELETE FROM receipt_headers;")
                    .unwrap();
            }),
            // 7. Corrupt body: malformed JSON.
            Box::new(|conn| {
                conn.execute("UPDATE receipts SET payload = '{bad'", [])
                    .unwrap();
            }),
            // 8. Corrupt body: wrong fact_digest.
            Box::new(|conn| {
                let wrong_digest = format!("node-v1:{}", "0".repeat(64));
                conn.execute(
                    "UPDATE receipts SET payload = json_set(payload, '$.d', ?1) WHERE id = ?2",
                    params![wrong_digest, target_id],
                )
                .unwrap();
            }),
            // 9. Corrupt body: both k and K present.
            Box::new(|conn| {
                conn.execute(
                    "UPDATE receipts SET payload = json_set(payload, '$.K', 'extra') WHERE id = ?1",
                    [target_id],
                )
                .unwrap();
            }),
            // 10. Corrupt body: verbatim K holding a strippable id (non-canonical body).
            Box::new({
                let FactKey::Node { id } = receipts[0].fact_key() else {
                    panic!("expected node receipt");
                };
                let full_id = id.clone();
                move |conn| {
                    conn.execute(
                        "UPDATE receipts SET payload = json_remove(json_set(payload, '$.K', ?1), '$.k') WHERE id = ?2",
                        params![full_id, target_id],
                    )
                    .unwrap();
                }
            }),
            // 11. Corrupt body: representation-only edits (whitespace, key order).
            Box::new(|conn| {
                conn.execute(
                    "UPDATE receipts SET payload = payload || ' ' WHERE id = ?1",
                    [target_id],
                )
                .unwrap();
            }),
            Box::new(|conn| {
                conn.execute(
                    "UPDATE receipts SET payload = json_set(json_remove(payload, '$.d'), '$.d', json_extract(payload, '$.d')) WHERE id = ?1",
                    [target_id],
                )
                .unwrap();
            }),
            // 12. Corrupt body: altered range span.
            Box::new(|conn| {
                conn.execute(
                    "UPDATE receipts SET payload = json_set(payload, '$.r[0][3]', 999999) WHERE id = ?1",
                    [target_id],
                )
                .unwrap();
            }),
        ];

        for (idx, mutate) in header_mutations.iter().enumerate() {
            let directory = tempfile::tempdir().unwrap();
            let mut store = ReceiptStore::open(&directory.path().join("receipts.sqlite")).unwrap();
            store.persist(&capture, &source, &receipts).unwrap();
            mutate(&store.conn);
            assert_eq!(
                pinned::stored_receipt(&store.conn, target_id).unwrap_err(),
                PinnedReadError::Invalid,
                "mutation {idx} must fail pinned::stored_receipt with Invalid"
            );
            assert!(
                store.get(target_id).is_err(),
                "mutation {idx} must fail store.get"
            );
        }
    }

    // AC-0373: a v3 store with legacy rows migrates to v4 on open, preserves
    // legacy rows unchanged (never rewritten), normalizes newly persisted v3
    // rows, and reopens stably.
    #[test]
    fn ac0373_v3_store_migrates_to_v4_preserving_legacy_rows_and_normalizing_new_rows() {
        let (_directory, source, capture, _, receipts) = fixture();
        assert!(receipts.len() >= 3);
        let directory = tempfile::tempdir().unwrap();
        let v3_path = directory.path().join("v3-receipts.sqlite");

        // Build an exact v3 store with legacy rows (receipts[..2]).
        {
            let conn = Connection::open(&v3_path).unwrap();
            conn.execute_batch(RECEIPTS_SCHEMA_V3).unwrap();
            conn.execute_batch(TOMBSTONES_SCHEMA).unwrap();
            conn.execute_batch(RECLAIMS_SCHEMA).unwrap();
            create_v3_receipt_indexes(&conn).unwrap();
            conn.execute_batch(META_SCHEMA_V3).unwrap();
            conn.execute("INSERT INTO receipt_meta VALUES (3)", [])
                .unwrap();
            for receipt in &receipts[..2] {
                conn.execute(
                    "INSERT INTO receipts (id, source_id, repo_key, payload) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        receipt.id(),
                        source.source_id,
                        source.repo_key,
                        receipt.to_json().unwrap()
                    ],
                )
                .unwrap();
            }
        }

        // Open with v4 code -> migrates to v4.
        let mut store = ReceiptStore::open(&v3_path).unwrap();
        let fresh = ReceiptStore::open(&directory.path().join("fresh-v4.sqlite")).unwrap();
        assert_eq!(schema_objects(&store.conn), schema_objects(&fresh.conn));

        // Legacy rows remain legacy (header_id IS NULL, full JSON in payload) and read unchanged.
        for receipt in &receipts[..2] {
            let (payload, header_id): (String, Option<String>) = store
                .conn
                .query_row(
                    "SELECT payload, header_id FROM receipts WHERE id = ?1",
                    [receipt.id()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert!(header_id.is_none());
            assert_eq!(payload, receipt.to_json().unwrap());
            assert_eq!(&store.get(receipt.id()).unwrap(), receipt);
        }

        // Persist the full batch (including legacy receipts[..2] and new receipts[2..]):
        // existing legacy rows are NOT rewritten; new v3 rows are normalized.
        let retained = store.persist(&capture, &source, &receipts).unwrap();
        assert_eq!(retained.ids.len(), receipts.len());

        for receipt in &receipts[..2] {
            let (payload, header_id): (String, Option<String>) = store
                .conn
                .query_row(
                    "SELECT payload, header_id FROM receipts WHERE id = ?1",
                    [receipt.id()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert!(header_id.is_none(), "legacy row must not be rewritten");
            assert_eq!(payload, receipt.to_json().unwrap());
        }
        for receipt in &receipts[2..] {
            let (payload, header_id): (String, Option<String>) = store
                .conn
                .query_row(
                    "SELECT payload, header_id FROM receipts WHERE id = ?1",
                    [receipt.id()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert!(header_id.is_some(), "new v3 row must be normalized");
            assert!(payload.len() < receipt.to_json().unwrap().len());
        }
        let header_count: i64 = store
            .conn
            .query_row("SELECT count(*) FROM receipt_headers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(header_count, 1);

        // Collision check against a normalized row compares rebuilt canonical JSON.
        store
            .conn
            .execute(
                "UPDATE receipts SET payload = json_set(payload, '$.d', ?1) WHERE id = ?2",
                params![format!("node-v1:{}", "0".repeat(64)), receipts[2].id()],
            )
            .unwrap();
        assert!(store.persist(&capture, &source, &receipts[2..3]).is_err());
        // Restore valid normalized row and verify reopen stability.
        let norm2 = normalize_v3_receipt(&receipts[2], &receipts[2].to_json().unwrap()).unwrap();
        store
            .conn
            .execute(
                "UPDATE receipts SET payload = ?1 WHERE id = ?2",
                params![norm2.body_json, receipts[2].id()],
            )
            .unwrap();

        let expected_storage = receipt_storage(&store.conn).unwrap();
        drop(store);

        let reopened = ReceiptStore::open(&v3_path).unwrap();
        assert_eq!(schema_objects(&reopened.conn), schema_objects(&fresh.conn));
        assert_eq!(receipt_storage(&reopened.conn).unwrap(), expected_storage);
        for receipt in &receipts {
            let loaded = reopened.get(receipt.id()).unwrap();
            assert_eq!(&loaded, receipt);
            assert_eq!(loaded.to_json().unwrap(), receipt.to_json().unwrap());
        }
    }

    // AC-0373: an interrupted v3 -> v4 migration rolls back completely in one
    // transaction, leaving the v3 store unchanged and readable; non-exact stores
    // are rejected without mutation.
    #[test]
    fn ac0373_interrupted_or_non_exact_v3_migration_rolls_back_leaving_v3_readable() {
        let (_directory, source, _capture, _, receipts) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let v3_path = directory.path().join("v3-rollback.sqlite");

        let conn = Connection::open(&v3_path).unwrap();
        conn.execute_batch(RECEIPTS_SCHEMA_V3).unwrap();
        conn.execute_batch(TOMBSTONES_SCHEMA).unwrap();
        conn.execute_batch(RECLAIMS_SCHEMA).unwrap();
        create_v3_receipt_indexes(&conn).unwrap();
        conn.execute_batch(META_SCHEMA_V3).unwrap();
        conn.execute("INSERT INTO receipt_meta VALUES (3)", [])
            .unwrap();
        let mut expected_v3_rows = Vec::new();
        for receipt in &receipts {
            let payload = receipt.to_json().unwrap();
            conn.execute(
                "INSERT INTO receipts (id, source_id, repo_key, payload) VALUES (?1, ?2, ?3, ?4)",
                params![receipt.id(), source.source_id, source.repo_key, payload],
            )
            .unwrap();
            expected_v3_rows.push((
                receipt.id().to_owned(),
                source.source_id.clone(),
                source.repo_key.clone(),
                payload,
            ));
        }
        expected_v3_rows.sort_by(|a, b| a.0.cmp(&b.0));
        let expected_v3_objects = schema_objects(&conn);

        // 1. Pre-create a conflicting `receipt_headers` table so `migrate_v3_to_v4`
        // fails after `ALTER TABLE receipts ADD COLUMN header_id ...`.
        conn.execute_batch("CREATE TABLE receipt_headers (x INTEGER)")
            .unwrap();
        drop(conn);
        assert!(ReceiptStore::open(&v3_path).is_err());

        let conn = Connection::open(&v3_path).unwrap();
        conn.execute_batch("DROP TABLE receipt_headers").unwrap();
        assert_eq!(schema_objects(&conn), expected_v3_objects);
        assert_eq!(
            conn.query_row("SELECT version FROM receipt_meta", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(v1_rows(&conn), expected_v3_rows);

        // 2. An unexpected index on v3 also fails closed and rolls back the
        // entire migration transaction.
        conn.execute_batch("CREATE INDEX unexpected_v3_idx ON receipts(source_id)")
            .unwrap();
        drop(conn);
        assert!(ReceiptStore::open(&v3_path).is_err());

        let conn = Connection::open(&v3_path).unwrap();
        conn.execute_batch("DROP INDEX unexpected_v3_idx").unwrap();
        assert_eq!(schema_objects(&conn), expected_v3_objects);
        assert_eq!(v1_rows(&conn), expected_v3_rows);
        drop(conn);

        // After removing the conflicting object, the untouched v3 store migrates cleanly.
        let migrated = ReceiptStore::open(&v3_path).unwrap();
        for receipt in &receipts {
            assert_eq!(&migrated.get(receipt.id()).unwrap(), receipt);
        }
    }

    // AC-0373: capacity accounting charges each retained header once plus every
    // row's payload length; exact and one-below bounds behave accurately for
    // normalized, legacy, and mixed stores.
    #[test]
    fn ac0373_capacity_accounting_charges_new_headers_once_across_exact_and_one_below_bounds() {
        let (_directory, source, capture, _, receipts) = fixture();
        assert!(receipts.len() >= 3);
        let norm0 = normalize_v3_receipt(&receipts[0], &receipts[0].to_json().unwrap()).unwrap();
        let norm1 = normalize_v3_receipt(&receipts[1], &receipts[1].to_json().unwrap()).unwrap();
        assert_eq!(norm0.header_id, norm1.header_id);
        let header_len = norm0.header_json.len() as u64;
        let body0_len = norm0.body_json.len() as u64;
        let body1_len = norm1.body_json.len() as u64;

        let directory = tempfile::tempdir().unwrap();
        let mut store = ReceiptStore::open(&directory.path().join("capacity.sqlite")).unwrap();

        // First normalized receipt needs header_len + body0_len.
        // One-below rejects the receipt and leaves no orphan header.
        store.capacity.1 = header_len + body0_len - 1;
        let omitted_first = store.persist(&capture, &source, &receipts[..1]).unwrap();
        assert!(omitted_first.ids.is_empty());
        assert_eq!(omitted_first.omitted.get("rule.ts"), Some(&1));
        assert_eq!(receipt_storage(&store.conn).unwrap(), (0, 0));

        // Exact capacity admits the first receipt and its header.
        store.capacity.1 = header_len + body0_len;
        let admitted_first = store.persist(&capture, &source, &receipts[..1]).unwrap();
        assert_eq!(admitted_first.ids.len(), 1);
        assert_eq!(
            receipt_storage(&store.conn).unwrap(),
            (1, header_len + body0_len)
        );

        // Second receipt shares the existing header, so it costs only body1_len.
        // One-below omits the second receipt while keeping the first.
        store.capacity.1 = header_len + body0_len + body1_len - 1;
        let omitted_second = store.persist(&capture, &source, &receipts[..2]).unwrap();
        assert_eq!(omitted_second.ids.len(), 1);
        assert_eq!(omitted_second.omitted.get("rule.ts"), Some(&1));
        assert_eq!(
            receipt_storage(&store.conn).unwrap(),
            (1, header_len + body0_len)
        );

        // Exact capacity admits the second receipt with zero additional header charge.
        store.capacity.1 = header_len + body0_len + body1_len;
        let admitted_second = store.persist(&capture, &source, &receipts[..2]).unwrap();
        assert_eq!(admitted_second.ids.len(), 2);
        assert!(admitted_second.omitted.is_empty());
        assert_eq!(
            receipt_storage(&store.conn).unwrap(),
            (2, header_len + body0_len + body1_len)
        );

        // Mixed store: adding a legacy v2 receipt charges its full payload length.
        let legacy_v2 = mint_v2_receipt_from_v3(&receipts[2]);
        let legacy_len = legacy_v2.to_json().unwrap().len() as u64;
        store.capacity.1 = header_len + body0_len + body1_len + legacy_len - 1;
        let omitted_legacy = store
            .persist(&capture, &source, std::slice::from_ref(&legacy_v2))
            .unwrap();
        assert!(omitted_legacy.ids.is_empty());

        store.capacity.1 = header_len + body0_len + body1_len + legacy_len;
        let admitted_legacy = store
            .persist(&capture, &source, std::slice::from_ref(&legacy_v2))
            .unwrap();
        assert_eq!(admitted_legacy.ids.len(), 1);
        assert_eq!(
            receipt_storage(&store.conn).unwrap(),
            (3, header_len + body0_len + body1_len + legacy_len)
        );
    }

    // AC-0373: forget and supersession retain a header while any pinned, kept,
    // or capture-forgotten row references it, and delete the header in the same
    // transaction when its last referencing row is removed.
    #[test]
    fn ac0373_forget_and_supersession_retain_referenced_headers_and_remove_unreferenced_headers() {
        let (_directory, source, capture, _, receipts) = fixture();
        assert!(receipts.len() >= 3);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retention-headers.sqlite");
        let mut store = ReceiptStore::open(&path).unwrap();
        store.persist(&capture, &source, &receipts).unwrap();

        let header_count = |store: &ReceiptStore| -> i64 {
            store
                .conn
                .query_row("SELECT count(*) FROM receipt_headers", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(header_count(&store), 1);

        // 1. Forget with 1 pinned receipt: header survives because the pinned
        // (capture-forgotten) receipt still references it.
        let pin0 = std::collections::BTreeSet::from([receipts[0].id().to_owned()]);
        store
            .reclaim_forgotten(&source, &[capture.id().to_owned()], &pin0)
            .unwrap();
        assert_eq!(header_count(&store), 1);
        assert_eq!(&store.get(receipts[0].id()).unwrap(), &receipts[0]);

        // 2. Re-persist receipts[1..] and run supersession keeping only receipts[1]:
        // receipts[2..] are deleted, receipts[0] is capture-forgotten, receipts[1] is kept,
        // and the shared header survives.
        store.persist(&capture, &source, &receipts[1..]).unwrap();
        let keep1 = std::collections::BTreeSet::from([receipts[1].id().to_owned()]);
        assert_eq!(
            store
                .reclaim_superseded(&source, &[capture.id().to_owned()], &keep1)
                .unwrap(),
            receipts.len() as u64 - 2
        );
        assert_eq!(header_count(&store), 1);
        assert_eq!(&store.get(receipts[0].id()).unwrap(), &receipts[0]);
        assert_eq!(&store.get(receipts[1].id()).unwrap(), &receipts[1]);

        // 3. Forget with no pins: the remaining rows (including the previously
        // capture-forgotten row) are deleted, and the header is removed in the
        // same transaction.
        let empty = std::collections::BTreeSet::new();
        store
            .reclaim_forgotten(&source, &[capture.id().to_owned()], &empty)
            .unwrap();
        assert_eq!(header_count(&store), 0);
        assert_eq!(receipt_storage(&store.conn).unwrap(), (0, 0));

        // 4. Re-persist and reclaim everything via supersession (no pins/keep):
        // header is removed with the last referencing row.
        store.persist(&capture, &source, &receipts).unwrap();
        assert_eq!(header_count(&store), 1);
        assert_eq!(
            store
                .reclaim_superseded(&source, &[capture.id().to_owned()], &empty)
                .unwrap(),
            receipts.len() as u64
        );
        assert_eq!(header_count(&store), 0);
        assert_eq!(receipt_storage(&store.conn).unwrap(), (0, 0));
    }
}
