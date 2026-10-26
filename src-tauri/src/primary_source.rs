//! Retained primary-source evidence; independent of tier and proposal review.

use adapters_lang_ts::captured::{self, Receipt};
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, DirBuilder, OpenOptions};
use core_graph::source::{FactKey, SourceBinding};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use source_capture::{Capture, CaptureLimits, CaptureStore, SourceId, StoreLimits};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;

mod pinned;
pub(crate) use pinned::PinnedReadError;

use crate::AppState;
use crate::sources::RegisteredSource;

const MAX_RECEIPTS: u64 = 100_000;
const MAX_RECEIPT_BYTES: u64 = 128 * 1024;
const MAX_RECEIPT_HEADER_BYTES: u64 = 16 * 1024;
const MAX_RECEIPT_STORE_BYTES: u64 = 64 * 1024 * 1024;
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
const RECEIPTS_SCHEMA: &str = "CREATE TABLE receipts (id TEXT PRIMARY KEY NOT NULL, source_id TEXT NOT NULL, repo_key TEXT NOT NULL, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 131072), capture_forgotten_tombstone INTEGER REFERENCES receipt_tombstones(id), header_id TEXT REFERENCES receipt_headers(id)) STRICT";
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
const META_SCHEMA: &str =
    "CREATE TABLE receipt_meta (version INTEGER PRIMARY KEY CHECK(version = 4)) STRICT";
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
const RECEIPT_INDEXES: [(&str, &str); 4] = [
    RECEIPT_INDEXES_V3[0],
    RECEIPT_INDEXES_V3[1],
    RECEIPT_INDEXES_V3[2],
    (
        "receipts_by_header",
        "CREATE INDEX receipts_by_header ON receipts (header_id)",
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
    input_closure: String,
}

/// Compact v3 receipt body stored in `receipts.payload` when `header_id` is set
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
    source_scope: String,
    input_closure: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictV3WireRange {
    role: captured::RangeRole,
    index: u32,
    byte_start: u64,
    byte_end: u64,
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

fn normalize_v3_receipt(receipt: &Receipt, canonical_json: &str) -> Option<NormalizedReceipt> {
    let wire: StrictV3WireReceipt = serde_json::from_str(canonical_json).ok()?;
    if wire.content.schema_version != 3
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
    if header.schema_version != 3
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
            ranges: body
                .r
                .into_iter()
                .map(|(role, index, byte_start, byte_end)| StrictV3WireRange {
                    role,
                    index,
                    byte_start,
                    byte_end,
                })
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
pub(crate) struct TombstoneSummary {
    pub source_id: String,
    pub forgotten_at: String,
    pub receipts_reclaimed: u64,
    pub receipts_retained_pinned: u64,
}

/// Identity-only receipt retention status (ADR-0046 decision 4).
/// Basis and capture validation stay the caller's job.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ReceiptStatus {
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

fn unavailable() -> String {
    "Captured primary source is unavailable; recover this source again or inspect the separately labeled current working tree.".into()
}

fn storage_error(_: impl std::fmt::Display) -> String {
    "Retained source storage is unavailable or invalid; no source was substituted.".into()
}

fn source_id(value: &str) -> Result<SourceId, String> {
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

fn same_entry(a: &cap_std::fs::Metadata, b: &cap_std::fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn private_dir(parent: &Dir, name: &str) -> Result<Dir, String> {
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
            let a = current.metadata().map_err(storage_error)?;
            let b = expected.metadata().map_err(storage_error)?;
            if a.dev() != b.dev() || a.ino() != b.ino() {
                return Err(storage_error("private database substituted"));
            }
            sqlite_sidecars(&self.root, name)?;
        }
        Ok(())
    }
}

/// A fresh OS handle, never unlinked/replaced as part of retention cleanup.
pub(crate) struct RetentionGuard {
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

pub(crate) struct ReceiptStore {
    conn: Connection,
    /// Retained receipt count (and reclaim ledger bound) and payload bytes; tests narrow them.
    pub(crate) capacity: (u64, u64),
    #[cfg(test)]
    pub(crate) fail_next_reclaim: bool,
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
    tx.execute_batch(META_SCHEMA).map_err(storage_error)?;
    require_one(
        tx.execute("INSERT INTO receipt_meta VALUES (4)", [])
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
    if objects != 5 + RECEIPT_INDEXES.len() as i64 {
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
            "SELECT count(*)=1 AND coalesce(min(typeof(version)='integer' AND version=4),0) FROM receipt_meta",
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
         OR typeof(source_id) != 'text' OR length(CAST(source_id AS BLOB)) != 36
         OR source_id NOT GLOB 'src_*' OR trim(substr(source_id, 5), '0123456789abcdef') != ''
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
    let invalid: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM receipts WHERE typeof(id) != 'text'
         OR length(CAST(id AS BLOB)) NOT BETWEEN 1 AND 256
         OR typeof(source_id) != 'text' OR length(CAST(source_id AS BLOB)) != 36
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
                                  AND receipt_headers.repo_key = receipts.repo_key))))",
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
                  + (SELECT coalesce(sum(length(CAST(header AS BLOB))),0) FROM receipt_headers)",
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
         OR typeof(source_id) != 'text' OR length(CAST(source_id AS BLOB)) != 36
         OR source_id NOT GLOB 'src_*' OR trim(substr(source_id, 5), '0123456789abcdef') != ''
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

// The caller retains a transaction after receipt_storage validated all lengths.
fn stored_receipt(conn: &Connection, id: &str) -> Result<Receipt, String> {
    pinned::stored_receipt(conn, id).map_err(|error| error.to_string())
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
        conn.busy_timeout(std::time::Duration::from_secs(1))
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
            tx.execute_batch(TOMBSTONES_SCHEMA).map_err(storage_error)?;
            tx.execute_batch(RECLAIMS_SCHEMA).map_err(storage_error)?;
            create_receipt_indexes(&tx)?;
            tx.execute_batch(META_SCHEMA).map_err(storage_error)?;
            require_one(
                tx.execute("INSERT INTO receipt_meta VALUES (4)", [])
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
        }
        receipt_storage(&tx)?;
        tx.commit().map_err(storage_error)?;
        let current = private_file(&parent, name.as_ref(), false)?;
        let a = checked.metadata().map_err(storage_error)?;
        let b = current.metadata().map_err(storage_error)?;
        if a.dev() != b.dev() || a.ino() != b.ino() {
            return Err(storage_error("receipt database changed"));
        }
        sqlite_sidecars(&parent, name)?;
        Ok(Self {
            conn,
            capacity: (MAX_RECEIPTS, MAX_RECEIPT_STORE_BYTES),
            #[cfg(test)]
            fail_next_reclaim: false,
        })
    }

    /// Retain `receipts` in order until the store's capacity is reached
    /// (#722). Integrity failures still reject the whole batch; a receipt that
    /// only lacks room is left out and counted under its file path, and the
    /// ids actually retained (new or already stored) are returned with them.
    fn persist(
        &mut self,
        capture: &Capture,
        source: &RegisteredSource,
        receipts: &[Receipt],
    ) -> Result<RetainedReceipts, String> {
        let mut rows = Vec::new();
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
            for range in receipt.ranges() {
                capture.read_span(&range.captured).map_err(storage_error)?;
            }
            let json = receipt.to_json().map_err(storage_error)?;
            if json.len() as u64 > MAX_RECEIPT_BYTES {
                return Err(storage_error("receipt bounds"));
            }
            match batch.get(receipt.id()) {
                Some(old) if old != &json => return Err(storage_error("receipt batch collision")),
                Some(_) => {}
                None => {
                    let normalized = normalize_v3_receipt(receipt, &json);
                    batch.insert(receipt.id().to_owned(), json.clone());
                    rows.push((
                        receipt.id().to_owned(),
                        receipt.file().path.clone(),
                        receipt.fact_key().clone(),
                        receipt.fact_digest().to_owned(),
                        json,
                        normalized,
                    ));
                }
            }
        }
        drop(batch);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let (mut count, mut bytes) = receipt_storage(&tx)?;
        let mut retained = RetainedReceipts::default();
        for (id, path, fact_key, fact_digest, canonical_json, normalized) in rows {
            let old: Option<(String, String, String, Option<String>)> = tx
                .query_row(
                    "SELECT source_id,repo_key,payload,header_id FROM receipts WHERE id=?1",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(storage_error)?;
            if let Some((old_source, old_repo, old_payload, old_header_id)) = old {
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
                let row_cost = normalized.body_json.len() as u64 + header_cost;
                if count + 1 > self.capacity.0 || bytes + row_cost > self.capacity.1 {
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
                require_one(
                    tx.execute(
                        "INSERT INTO receipts (id,source_id,repo_key,payload,header_id) VALUES (?1,?2,?3,?4,?5)",
                        params![
                            id,
                            source.source_id,
                            source.repo_key,
                            normalized.body_json,
                            normalized.header_id,
                        ],
                    )
                    .map_err(storage_error)?,
                )?;
            } else if count + 1 > self.capacity.0
                || bytes + canonical_json.len() as u64 > self.capacity.1
            {
                *retained.omitted.entry(path).or_default() += 1;
                retained.omitted_facts.push((fact_key, fact_digest));
                continue;
            } else {
                count += 1;
                bytes += canonical_json.len() as u64;
                require_one(
                    tx.execute(
                        "INSERT INTO receipts (id,source_id,repo_key,payload) VALUES (?1,?2,?3,?4)",
                        params![id, source.source_id, source.repo_key, canonical_json],
                    )
                    .map_err(storage_error)?,
                )?;
            }
            retained.ids.insert(id);
        }
        delete_orphan_tombstones(&tx)?;
        delete_orphan_headers(&tx)?;
        tx.commit().map_err(storage_error)?;
        Ok(retained)
    }

    fn get(&self, id: &str) -> Result<Receipt, String> {
        if id.is_empty() || id.len() > 256 {
            return Err(unavailable());
        }
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        receipt_storage(&tx)?;
        let receipt = stored_receipt(&tx, id)?;
        tx.commit().map_err(storage_error)?;
        Ok(receipt)
    }

    fn ids(&self, source: &str) -> Result<Vec<String>, String> {
        source_id(source)?;
        let tx = self.conn.unchecked_transaction().map_err(storage_error)?;
        receipt_storage(&tx)?;
        let mut ids = Vec::new();
        {
            let mut statement = tx
                .prepare("SELECT id FROM receipts ORDER BY id")
                .map_err(storage_error)?;
            let mut rows = statement.query([]).map_err(storage_error)?;
            while let Some(row) = rows.next().map_err(storage_error)? {
                let id: String = row.get(0).map_err(storage_error)?;
                let receipt = stored_receipt(&tx, &id)?;
                if receipt.source_id().as_str() == source {
                    ids.push(id);
                }
            }
        }
        tx.commit().map_err(storage_error)?;
        Ok(ids)
    }

    /// Reclaim unpinned receipts for `source` and mark pinned receipts as
    /// capture-forgotten in one atomic transaction (ADR-0046 decision 3).
    fn reclaim_forgotten(
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
    fn reclaim_superseded(
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
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_reclaim) {
            return Err(storage_error("injected reclaim failure"));
        }
        source_id(&source.source_id)?;
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
        receipt_storage(&tx)?;
        tx.commit().map_err(storage_error)?;
        Ok(reclaimed.len() as u64)
    }

    /// Look up the retention status of an exact receipt ID.
    ///
    /// This is the identity half of ADR-0046 decision 4. Basis and capture
    /// validation stay the caller's job.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn receipt_status(&self, id: &str) -> Result<ReceiptStatus, String> {
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
            source_id(&tombstone.source_id).map_err(storage_error)?;
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
            source_id(&tombstone.source_id).map_err(storage_error)?;
            return Ok(if cause == ReclaimCause::Superseded.as_str() {
                ReceiptStatus::Superseded { tombstone }
            } else {
                ReceiptStatus::Reclaimed { tombstone }
            });
        }
        Ok(ReceiptStatus::Unknown)
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
pub(crate) struct InFlightPin {
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

pub(crate) struct PrimarySourceStore {
    captures: Mutex<CaptureStore>,
    pub(crate) receipts: Mutex<ReceiptStore>,
    in_flight: InFlight,
    storage: PrivateStorage,
    #[cfg(test)]
    evidence_accesses: std::sync::atomic::AtomicUsize,
}

impl PrimarySourceStore {
    /// Bytes cloned into each retention guard; read without allocating a path.
    #[cfg(unix)]
    pub(crate) fn retained_app_path_bytes(&self) -> usize {
        self.storage.app_path.as_os_str().as_encoded_bytes().len()
    }

    pub(crate) fn open(app_data: &Path) -> Result<Self, String> {
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
            #[cfg(test)]
            evidence_accesses: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Test observation of retained-source lease attempts, including historical
    /// inspection. It does not count current-checkout reads or successful bytes.
    #[cfg(test)]
    pub(crate) fn evidence_access_attempts(&self) -> usize {
        self.evidence_accesses
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Pin `ids` for a prepared task until the returned pin is dropped.
    pub(crate) fn pin_in_flight(&self, ids: Vec<String>) -> Result<InFlightPin, String> {
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
    #[cfg(test)]
    pub(crate) fn exclusive_guard_for_test(&self, source: &str) -> RetentionGuard {
        self.guard(source, true).unwrap()
    }

    fn guard(&self, source: &str, exclusive: bool) -> Result<RetentionGuard, String> {
        #[cfg(test)]
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
    pub(crate) fn verify_task_guard(&self, guard: &RetentionGuard) -> Result<(), PinnedReadError> {
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

    pub(crate) fn prepare(
        &self,
        source: &RegisteredSource,
        root: &Path,
        layers: &[String],
    ) -> Result<PrimaryInput, String> {
        let wanted = layers.is_empty()
            || layers
                .iter()
                .any(|layer| ["server", "events", "client"].contains(&layer.as_str()));
        if !wanted {
            return Ok(PrimaryInput {
                capture: None,
                _guard: None,
            });
        }
        let guard = self.guard(&source.source_id, false)?;
        let paths = captured::enumerate_paths(root).map_err(|_| "Selected TS/JS source could not be captured safely within the supported path and size limits.".to_string())?;
        let capture = source_capture::capture_working_tree(root, &source_id(&source.source_id)?, &paths, CaptureLimits::default()).map_err(|_| "Selected TS/JS source could not be retained within the capture limits; recovery was not published.".to_string())?;
        Ok(PrimaryInput {
            capture: Some(capture),
            _guard: Some(guard),
        })
    }

    /// Retain the capture and as many `receipts` as the store has room for
    /// (#722). Callers keep only [`RetainedReceipts::ids`] for bindings, so
    /// no binding cites a receipt the store does not hold.
    pub(crate) fn persist(
        &self,
        input: &PrimaryInput,
        source: &RegisteredSource,
        receipts: &[Receipt],
    ) -> Result<RetainedReceipts, String> {
        self.storage.verify()?;
        if let Some(capture) = &input.capture {
            self.captures.lock().map_err(storage_error)?.persist(capture).map_err(|_| "Source retention failed or reached capacity. Use retained-source controls to free space; recovery was not published.".to_string())?;
            self.receipts
                .lock()
                .map_err(storage_error)?
                .persist(capture, source, receipts)
        } else if !receipts.is_empty() {
            Err(storage_error("uncaptured receipts"))
        } else {
            Ok(RetainedReceipts::default())
        }
    }
}

/// Receipts one recovery actually retained, and those left out for lack of
/// store capacity, by file path and fact key.
#[derive(Debug, Default)]
pub(crate) struct RetainedReceipts {
    ids: std::collections::BTreeSet<String>,
    omitted: std::collections::BTreeMap<String, u64>,
    pub(crate) omitted_facts: Vec<(FactKey, String)>,
}

impl RetainedReceipts {
    /// Drop every receipt the store did not retain and return the omissions.
    pub(crate) fn keep_retained(
        self,
        receipts: &mut Vec<Receipt>,
    ) -> (
        std::collections::BTreeMap<String, u64>,
        Vec<(FactKey, String)>,
    ) {
        receipts.retain(|receipt| self.ids.contains(receipt.id()));
        (self.omitted, self.omitted_facts)
    }
}

pub(crate) struct PrimaryInput {
    pub capture: Option<Capture>,
    _guard: Option<RetentionGuard>,
}

pub(crate) fn matching_bindings(
    extraction: &adapters_lang_ts::Extraction,
    receipts: &[Receipt],
) -> Vec<SourceBinding> {
    // Match the same facts that graph publication materializes: last-value
    // node winners, and edges after evidence union (AC-0203). A relation
    // collapsed from several sites is not the fact any one receipt attested,
    // so it publishes unbound rather than failing the digest check.
    let nodes: std::collections::BTreeMap<_, _> = extraction
        .nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect();
    let edges = crate::merge_edge_occurrences(&extraction.edges);
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
pub(crate) struct CapturedRange {
    pub index: usize,
    pub path: String,
    pub byte_start: u64,
    pub byte_end: u64,
}

#[derive(Serialize)]
pub(crate) struct CapturedDescription {
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

fn bound_receipt(
    state: &AppState,
    fact: &FactKey,
    expected: Option<&str>,
) -> Result<(Receipt, RetentionGuard), String> {
    // Preliminary metadata locates the guard; release all mutexes before the
    // try-only OS lock, then make the authoritative current-binding check.
    let preliminary = state
        .graph
        .lock()
        .map_err(storage_error)?
        .current_source_binding(fact)
        .map_err(storage_error)?
        .ok_or_else(unavailable)?;
    let source = state
        .sources
        .lock()
        .map_err(storage_error)?
        .get_by_repo(&preliminary.repo_key)?
        .ok_or_else(unavailable)?;
    let guard = state.primary_sources.guard(&source.source_id, false)?;
    let binding = state
        .graph
        .lock()
        .map_err(storage_error)?
        .current_source_binding(fact)
        .map_err(storage_error)?
        .ok_or_else(unavailable)?;
    if binding.repo_key != source.repo_key {
        return Err(unavailable());
    }
    if expected.is_some_and(|id| id != binding.receipt_id) {
        return Err("Captured source selection is stale; select the fact again.".into());
    }
    let receipt = state
        .primary_sources
        .receipts
        .lock()
        .map_err(storage_error)?
        .get(&binding.receipt_id)?;
    if receipt.source_id().as_str() != source.source_id
        || receipt.repo_key() != source.repo_key
        || receipt.fact_key() != fact
        || receipt.fact_digest() != binding.emitted_fact_digest
    {
        return Err(unavailable());
    }
    Ok((receipt, guard))
}

pub(crate) fn describe(state: &AppState, fact: &FactKey) -> Result<CapturedDescription, String> {
    let (receipt, _guard) = bound_receipt(state, fact, None)?;
    Ok(CapturedDescription {
        graph_snapshot_id: crate::investigations::inspection::graph_snapshot_for_receipt(
            state, &receipt,
        )?,
        fact: receipt.fact_key().clone(),
        receipt_id: receipt.id().to_owned(),
        emitted_fact_digest: receipt.fact_digest().to_owned(),
        source_id: receipt.source_id().as_str().to_owned(),
        repo_key: receipt.repo_key().to_owned(),
        ranges: receipt
            .ranges()
            .iter()
            .enumerate()
            .map(|(index, range)| CapturedRange {
                index,
                path: range.evidence.path.clone(),
                byte_start: range.evidence.byte_start,
                byte_end: range.evidence.byte_end,
            })
            .collect(),
        scope: "primary_source_only",
        input_closure: "input_closure_not_established",
    })
}

#[derive(Serialize)]
pub(crate) struct CapturedText {
    pub receipt_id: String,
    pub range_index: usize,
    pub text: String,
    pub path: String,
    pub byte_start: u64,
    pub byte_end: u64,
}

pub(crate) fn read(
    state: &AppState,
    fact: &FactKey,
    receipt_id: &str,
    range_index: usize,
) -> Result<CapturedText, String> {
    let (receipt, _guard) = bound_receipt(state, fact, Some(receipt_id))?;
    let range = receipt.ranges().get(range_index).ok_or_else(unavailable)?;
    let text = state
        .primary_sources
        .captures
        .lock()
        .map_err(storage_error)?
        .read_text_span(&range.captured)
        .map_err(|_| unavailable())?;
    Ok(CapturedText {
        receipt_id: receipt.id().to_owned(),
        range_index,
        text,
        path: range.evidence.path.clone(),
        byte_start: range.evidence.byte_start,
        byte_end: range.evidence.byte_end,
    })
}

#[derive(Serialize)]
pub(crate) struct RetainedSource {
    pub source_id: String,
    pub repo_key: String,
    pub display_name: String,
}

#[derive(Serialize)]
pub(crate) struct RetentionPreview {
    pub source_id: String,
    pub repo_key: String,
    pub display_name: String,
    pub capture_ids: Vec<String>,
    pub captures: usize,
    pub files: u64,
    pub bytes: u64,
    pub receipts: usize,
    pub current_references: usize,
    pub historical_references: usize,
    /// Actual v2 evidence references in immutable staged proposals.
    pub staged_references: usize,
    /// Actual investigation/receipt uses, including selected present bindings
    /// whose original source text has not been requested.
    pub investigation_references: usize,
    /// Receipts of this source cited by saved work packets.
    pub packet_references: usize,
    pub fingerprint: String,
}

struct LockedPreview {
    preview: RetentionPreview,
    pins: std::collections::BTreeSet<String>,
    /// This source's receipts the current graph binds.
    bound: std::collections::BTreeSet<String>,
}

fn preview_locked(state: &AppState, source: &RegisteredSource) -> Result<LockedPreview, String> {
    let inventory = state
        .primary_sources
        .captures
        .lock()
        .map_err(storage_error)?
        .source_inventory(&source_id(&source.source_id)?)
        .map_err(storage_error)?;
    let receipt_ids = state
        .primary_sources
        .receipts
        .lock()
        .map_err(storage_error)?
        .ids(&source.source_id)?;
    let current = state
        .graph
        .lock()
        .map_err(storage_error)?
        .source_bindings_for_repo(&source.repo_key)
        .map_err(storage_error)?;
    let current_count = current
        .iter()
        .filter(|binding| receipt_ids.binary_search(&binding.receipt_id).is_ok())
        .count();
    // Bind the complete associations, not only their count: retained receipts
    // can become current again without changing the capture/receipt inventory.
    // This records the associations observed at preview/recheck; it does not
    // freeze independent graph mutations after the recheck.
    let mut current_bindings: Vec<_> = current
        .iter()
        .map(|binding| {
            (
                &binding.fact,
                &binding.repo_key,
                &binding.receipt_id,
                &binding.emitted_fact_digest,
            )
        })
        .collect();
    current_bindings.sort_unstable();
    let capture_ids: Vec<_> = inventory
        .iter()
        .map(|capture| capture.capture_id.clone())
        .collect();
    let staged_references = state
        .proposals
        .lock()
        .map_err(storage_error)?
        .captured_receipt_references(&source.source_id)
        .map_err(storage_error)?;
    // The caller already holds this source's retention guard. Acquisition
    // publishes its ledger/index under the same shared guard, so an exclusive
    // forget cannot race a newly admitted reference. Hold no other store mutex
    // across this short validated JobStore read.
    let investigation_references = state
        .jobs
        .lock()
        .map_err(storage_error)?
        .investigation_receipt_references(&source.source_id)
        .map_err(storage_error)?;
    let packet_references: Vec<String> = state
        .work_packets
        .store
        .lock()
        .map_err(storage_error)?
        .referenced_receipts(&receipt_ids)
        .map_err(storage_error)?
        .into_iter()
        .collect();
    let fingerprint = core_prov::content_hash(
        &serde_json::to_vec(&(
            "retained-source-preview-v4",
            &source.source_id,
            &source.repo_key,
            &capture_ids,
            &receipt_ids,
            &current_bindings,
            &staged_references,
            &investigation_references,
            &packet_references,
        ))
        .map_err(storage_error)?,
    );
    let pins = staged_references
        .iter()
        .map(|reference| reference.receipt_id.clone())
        .chain(
            investigation_references
                .iter()
                .map(|reference| reference.receipt_id.clone()),
        )
        .chain(packet_references.iter().cloned())
        .collect();
    let bound = current
        .iter()
        .filter(|binding| receipt_ids.binary_search(&binding.receipt_id).is_ok())
        .map(|binding| binding.receipt_id.clone())
        .collect();
    Ok(LockedPreview {
        preview: RetentionPreview {
            source_id: source.source_id.clone(),
            repo_key: source.repo_key.clone(),
            display_name: source.display_name.clone(),
            captures: inventory.len(),
            files: inventory.iter().map(|capture| capture.file_count).sum(),
            bytes: inventory.iter().map(|capture| capture.byte_len).sum(),
            receipts: receipt_ids.len(),
            current_references: current_count,
            historical_references: receipt_ids.len().saturating_sub(current_count),
            staged_references: staged_references.len(),
            investigation_references: investigation_references.len(),
            packet_references: packet_references.len(),
            capture_ids,
            fingerprint,
        },
        pins,
        bound,
    })
}

fn registered(state: &AppState, id: &str) -> Result<RegisteredSource, String> {
    source_id(id)?;
    state
        .sources
        .lock()
        .map_err(storage_error)?
        .get_by_id(id)?
        .ok_or_else(unavailable)
}

pub(crate) fn preview(state: &AppState, id: &str) -> Result<RetentionPreview, String> {
    let source = registered(state, id)?;
    let _guard = state.primary_sources.guard(id, false)?;
    Ok(preview_locked(state, &source)?.preview)
}

pub(crate) fn forget(state: &AppState, id: &str, fingerprint: &str) -> Result<u64, String> {
    let source = registered(state, id)?;
    let _guard = state.primary_sources.guard(id, true)?;
    let locked = preview_locked(state, &source)?;
    if locked.preview.fingerprint != fingerprint {
        return Err("Retained source changed; refresh the preview before forgetting.".into());
    }
    // Serialize cache generation invalidation with preview snapshot and publication.
    // Rotate before capture deletion; a later failure leaves only safe cold misses.
    let graph_guard = state.graph.lock().map_err(storage_error)?;
    graph_guard
        .invalidate_derived_state()
        .map_err(storage_error)?;
    let forgotten = state
        .primary_sources
        .captures
        .lock()
        .map_err(storage_error)?
        .forget_source(&source_id(id)?, &locked.preview.capture_ids)
        .map_err(storage_error)?;
    state
        .primary_sources
        .receipts
        .lock()
        .map_err(storage_error)?
        .reclaim_forgotten(&source, &locked.preview.capture_ids, &locked.pins)?;
    Ok(forgotten)
}

/// Reclaim receipts of `source` that the just-published graph no longer binds
/// and nothing pins (ADR-0046 decision 5). The caller must have released its
/// prepared input first. The reclaim needs the exclusive retention guard, so
/// no other recovery of this source can be holding fresh, not yet published
/// receipts, and no task can prepare against it; a busy source is skipped
/// and settles on its next publish. Receipts a prepared task cites stay
/// pinned until it has staged ([`InFlightPin`]).
pub(crate) fn reclaim_superseded(
    state: &AppState,
    source: &RegisteredSource,
    capture_id: &str,
) -> Result<u64, String> {
    reclaim_superseded_with(state, source, capture_id, || {})
}

/// [`reclaim_superseded`], pausing between its two keep-set snapshots so
/// tests can stage a task in the handoff.
pub(crate) fn reclaim_superseded_with(
    state: &AppState,
    source: &RegisteredSource,
    capture_id: &str,
    between_snapshots: impl FnOnce(),
) -> Result<u64, String> {
    let Ok(_guard) = state.primary_sources.guard(&source.source_id, true) else {
        return Ok(0);
    };
    // In-flight pins first, durable pins second (#762 review). The
    // exclusive guard admits no new preparation, so a pin missing here was
    // dropped already, after its task staged: the durable snapshot below
    // then holds that staged pin. Reading them the other way round lets a
    // task stage after the durable read and drop before the in-flight one.
    let in_flight = state
        .primary_sources
        .in_flight
        .lock()
        .map_err(storage_error)?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    between_snapshots();
    let locked = preview_locked(state, source)?;
    let keep = locked
        .pins
        .union(&locked.bound)
        .cloned()
        .chain(in_flight)
        .collect();
    state
        .primary_sources
        .receipts
        .lock()
        .map_err(storage_error)?
        .reclaim_superseded(source, &[capture_id.to_owned()], &keep)
}

#[tauri::command]
pub(crate) async fn describe_captured_source(
    fact: FactKey,
    expected_node: Option<core_graph::Node>,
    expected_edge: Option<core_graph::Edge>,
    app: tauri::AppHandle,
) -> Result<CapturedDescription, String> {
    crate::off_ui_thread(move || {
        let expected_digest = match (&fact, expected_node, expected_edge) {
            (FactKey::Node { id }, Some(node), None) if node.id == *id => {
                core_graph::source::node_digest(&node).map_err(storage_error)?
            }
            (
                FactKey::Edge {
                    source,
                    label,
                    destination,
                },
                None,
                Some(edge),
            ) if edge.src == *source && edge.label == *label && edge.dst == *destination => {
                core_graph::source::edge_digest(&edge).map_err(storage_error)?
            }
            _ => return Err(unavailable()),
        };
        let description = describe(&app.state::<AppState>(), &fact)?;
        if description.emitted_fact_digest != expected_digest {
            return Err(
                "Selected fact changed; refresh the graph before inspecting captured source."
                    .into(),
            );
        }
        Ok(description)
    })
    .await
}

#[tauri::command]
pub(crate) async fn read_captured_source(
    fact: FactKey,
    receipt_id: String,
    range_index: usize,
    app: tauri::AppHandle,
) -> Result<CapturedText, String> {
    crate::off_ui_thread(move || read(&app.state::<AppState>(), &fact, &receipt_id, range_index))
        .await
}

#[tauri::command]
pub(crate) async fn list_retained_sources(
    app: tauri::AppHandle,
) -> Result<Vec<RetainedSource>, String> {
    crate::off_ui_thread(move || {
        let state = app.state::<AppState>();
        let sources = state.sources.lock().map_err(storage_error)?.list()?;
        Ok(sources
            .into_iter()
            .map(|source| RetainedSource {
                source_id: source.source_id,
                repo_key: source.repo_key,
                display_name: source.display_name,
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub(crate) async fn preview_forget_source(
    source_id: String,
    app: tauri::AppHandle,
) -> Result<RetentionPreview, String> {
    crate::off_ui_thread(move || preview(&app.state::<AppState>(), &source_id)).await
}

#[tauri::command]
pub(crate) async fn forget_retained_source(
    source_id: String,
    fingerprint: String,
    app: tauri::AppHandle,
) -> Result<u64, String> {
    crate::off_ui_thread(move || forget(&app.state::<AppState>(), &source_id, &fingerprint)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (
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
            "PRAGMA ignore_check_constraints=ON; UPDATE receipt_meta SET version=5",
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

    // AC-0153: SQL ownership indexes cannot hide a receipt from its actual
    // source's preview; every bounded payload is validated before filtering.
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
    }

    // AC-0150: a later enrichment sharing an identity wins graph publication;
    // receipt matching follows that same final winner, never the first duplicate.
    #[test]
    fn primary_matching_uses_final_duplicate_fact_winners() {
        let (_directory, _, _, mut extraction, receipts) = fixture();
        let before = matching_bindings(&extraction, &receipts);
        assert!(!before.is_empty());
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

    fn schema_objects(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
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
        assert_eq!(migrated_objects.len(), 5 + RECEIPT_INDEXES.len());
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
