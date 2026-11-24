//! Historical receipt reads; current graph association is deliberately absent.

use super::*;
use source_capture::CaptureError;

/// Fixed categories keep missing content separate from storage/lock failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinnedReadError {
    Unavailable,
    Invalid,
    Operational,
}

impl std::fmt::Display for PinnedReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "Retained task evidence is unavailable; no source was substituted.",
            Self::Invalid => "Retained task evidence is invalid; no source was substituted.",
            Self::Operational => "Retained task evidence could not be checked; retry after the storage or source operation completes.",
        })
    }
}

impl std::error::Error for PinnedReadError {}

type AdmittedReceiptFields = (
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn admitted_receipt_text(
    value: rusqlite::types::ValueRef<'_>,
) -> Result<Option<String>, PinnedReadError> {
    match value {
        rusqlite::types::ValueRef::Null => Ok(None),
        rusqlite::types::ValueRef::Text(bytes) => std::str::from_utf8(bytes)
            .map(|text| Some(text.to_owned()))
            .map_err(|_| PinnedReadError::Invalid),
        _ => Err(PinnedReadError::Invalid),
    }
}

fn admitted_receipt_fields(
    has_header: bool,
    values: [rusqlite::types::ValueRef<'_>; 7],
) -> Result<AdmittedReceiptFields, PinnedReadError> {
    Ok((
        admitted_receipt_text(values[0])?,
        admitted_receipt_text(values[1])?,
        admitted_receipt_text(values[2])?,
        has_header,
        admitted_receipt_text(values[3])?,
        admitted_receipt_text(values[4])?,
        admitted_receipt_text(values[5])?,
        admitted_receipt_text(values[6])?,
    ))
}

/// The closure id a receipt row names; it must be exactly the closure the
/// receipt records as established (AC-1082).
fn stored_closure_id(conn: &Connection, id: &str) -> Result<Option<String>, PinnedReadError> {
    let (named, closure_id): (bool, Option<String>) = conn
        .query_row(
            "SELECT closure_id IS NOT NULL,
               CASE WHEN typeof(closure_id)='text' AND length(CAST(closure_id AS BLOB)) = 75 THEN closure_id END
             FROM receipts WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get::<_, Option<String>>(1).ok().flatten())),
        )
        .map_err(|_| PinnedReadError::Operational)?;
    if named && closure_id.is_none() {
        return Err(PinnedReadError::Invalid);
    }
    Ok(closure_id)
}

fn established_closure(receipt: &Receipt) -> Option<String> {
    match receipt.closure() {
        Some(captured::ReceiptClosure::Established { closure_id }) => Some(closure_id.clone()),
        _ => None,
    }
}

pub(super) fn stored_receipt(conn: &Connection, id: &str) -> Result<Receipt, PinnedReadError> {
    // CASE projections keep malformed/oversized cells inside SQLite. They are
    // never copied into Rust Strings before admission. One statement also
    // preserves a coherent selection for callers already holding a transaction.
    let payload_limit =
        i64::try_from(captured::MAX_RECEIPT_BYTES).map_err(|_| PinnedReadError::Invalid)?;
    let header_limit =
        i64::try_from(MAX_RECEIPT_HEADER_BYTES).map_err(|_| PinnedReadError::Invalid)?;
    let row: Option<AdmittedReceiptFields> = conn
        .query_row(
            "SELECT
               CASE WHEN typeof(r.source_id)='text' AND length(CAST(r.source_id AS BLOB)) BETWEEN 1 AND 256 THEN r.source_id END,
               CASE WHEN typeof(r.repo_key)='text' AND length(CAST(r.repo_key AS BLOB)) BETWEEN 1 AND 256 THEN r.repo_key END,
               CASE WHEN typeof(r.payload)='text' AND length(CAST(r.payload AS BLOB)) BETWEEN 1 AND ?2 THEN r.payload END,
               r.header_id IS NOT NULL,
               CASE WHEN typeof(r.header_id)='text' AND length(CAST(r.header_id AS BLOB)) BETWEEN 1 AND 256 THEN r.header_id END,
               CASE WHEN typeof(h.source_id)='text' AND length(CAST(h.source_id AS BLOB)) BETWEEN 1 AND 256 THEN h.source_id END,
               CASE WHEN typeof(h.repo_key)='text' AND length(CAST(h.repo_key AS BLOB)) BETWEEN 1 AND 256 THEN h.repo_key END,
               CASE WHEN typeof(h.header)='text' AND length(CAST(h.header AS BLOB)) BETWEEN 1 AND ?3 THEN h.header END
             FROM receipts r
             LEFT JOIN receipt_headers h
               ON r.header_id IS NOT NULL
              AND typeof(r.header_id)='text'
              AND length(CAST(r.header_id AS BLOB)) BETWEEN 1 AND 256
              AND h.id = r.header_id
             WHERE r.id=?1",
            params![id, payload_limit, header_limit],
            |row| {
                // SQLite TEXT can contain invalid UTF-8. Borrow the admitted
                // bytes first so corrupt encoding is Invalid, while actual SQL
                // access failures remain Operational.
                Ok(admitted_receipt_fields(
                    row.get(3)?,
                    [
                        row.get_ref(0)?,
                        row.get_ref(1)?,
                        row.get_ref(2)?,
                        row.get_ref(4)?,
                        row.get_ref(5)?,
                        row.get_ref(6)?,
                        row.get_ref(7)?,
                    ],
                ))
            },
        )
        .optional()
        .map_err(|_| PinnedReadError::Operational)?
        .transpose()?;
    let (
        Some(source),
        Some(repo),
        Some(payload),
        has_header,
        header_id,
        header_source,
        header_repo,
        header,
    ) = row.ok_or(PinnedReadError::Unavailable)?
    else {
        return Err(PinnedReadError::Invalid);
    };
    let json = if has_header {
        let (Some(header_id), Some(header_source), Some(header_repo), Some(header)) =
            (header_id, header_source, header_repo, header)
        else {
            return Err(PinnedReadError::Invalid);
        };
        if header_source != source || header_repo != repo {
            return Err(PinnedReadError::Invalid);
        }
        rebuild_normalized_receipt(id, &source, &repo, &header_id, &header, &payload)
            .map_err(|_| PinnedReadError::Invalid)?
    } else {
        payload
    };
    let receipt = Receipt::from_json(&json).map_err(|_| PinnedReadError::Invalid)?;
    if receipt.id() != id
        || receipt.source_id().as_str() != source
        || receipt.repo_key() != repo
        || receipt.to_json().map_err(|_| PinnedReadError::Invalid)? != json
        || stored_closure_id(conn, id)? != established_closure(&receipt)
    {
        return Err(PinnedReadError::Invalid);
    }
    Ok(receipt)
}

impl PrimarySourceStore {
    /// Load one explicitly reviewer-selected corpus capture. This validates its
    /// bytes and source ownership, but does not attest that a graph fact was
    /// produced from it; that association belongs to the reviewed corpus.
    pub fn evaluation_capture(
        &self,
        guard: &RetentionGuard,
        capture_id: &str,
        remaining_raw_bytes: usize,
    ) -> Result<Capture, PinnedReadError> {
        self.validate_task_guard(guard, &guard.source_id)?;
        let store = self
            .captures
            .lock()
            .map_err(|_| PinnedReadError::Operational)?;
        let inventory = store
            .source_inventory(&source_id(&guard.source_id).map_err(|_| PinnedReadError::Invalid)?)
            .map_err(|_| PinnedReadError::Operational)?;
        let selected = inventory
            .iter()
            .find(|c| c.capture_id == capture_id)
            .ok_or(PinnedReadError::Unavailable)?;
        if selected.byte_len > remaining_raw_bytes.min(32 * 1024 * 1024) as u64
            || selected.file_count > 4096
        {
            return Err(PinnedReadError::Invalid);
        }
        let capture = store.load(capture_id).map_err(|error| match error {
            CaptureError::Missing(_) => PinnedReadError::Unavailable,
            CaptureError::Invalid(_)
            | CaptureError::Limit(_)
            | CaptureError::Corrupt(_)
            | CaptureError::Encoding
            | CaptureError::Json(_) => PinnedReadError::Invalid,
            _ => PinnedReadError::Operational,
        })?;
        if capture.manifest().source_id.as_str() != guard.source_id {
            return Err(PinnedReadError::Invalid);
        }
        Ok(capture)
    }

    pub fn task_guard(&self, source: &str) -> Result<RetentionGuard, PinnedReadError> {
        self.guard(source, false)
            .map_err(|_| PinnedReadError::Operational)
    }

    /// The guard of a registered source also covers its observation input
    /// identity, whose observed receipts it owns (ADR-0079).
    fn validate_task_guard(
        &self,
        guard: &RetentionGuard,
        source: &str,
    ) -> Result<(), PinnedReadError> {
        let covered = source == guard.source_id
            || source == crate::observation_inputs::input_source_id(&guard.source_id);
        if !covered || guard.app_path != self.storage.app_path {
            return Err(PinnedReadError::Invalid);
        }
        self.storage
            .verify()
            .map_err(|_| PinnedReadError::Operational)
    }

    /// Positive retention classification for a binding already checked against
    /// the caller's exact graph basis. Never call this with a client receipt ID
    /// alone. Reclaimed metadata cannot attest old ranges or full receipt fields.
    pub fn task_binding_forgotten(
        &self,
        guard: &RetentionGuard,
        binding: &core_graph::source::SourceBinding,
        validated_receipt: Option<&Receipt>,
    ) -> Result<bool, PinnedReadError> {
        self.task_binding_forgotten_as(
            guard,
            core_graph::BindingRole::Primary,
            binding,
            validated_receipt,
        )
    }

    /// The receipt owner a binding in `role` admits under `guard`: the
    /// registered source and the binding's repo for a primary binding, the
    /// source's input identity for an observed one (ADR-0079).
    fn role_owner(
        guard: &RetentionGuard,
        role: core_graph::BindingRole,
        binding_repo: &str,
    ) -> (String, String) {
        match role {
            core_graph::BindingRole::Primary => (guard.source_id.clone(), binding_repo.to_owned()),
            core_graph::BindingRole::Observed => {
                let input = crate::observation_inputs::input_source_id(&guard.source_id);
                (input.clone(), input)
            }
        }
    }

    /// [`Self::task_binding_forgotten`] for a binding in `role`.
    pub fn task_binding_forgotten_as(
        &self,
        guard: &RetentionGuard,
        role: core_graph::BindingRole,
        binding: &core_graph::source::SourceBinding,
        validated_receipt: Option<&Receipt>,
    ) -> Result<bool, PinnedReadError> {
        self.validate_task_guard(guard, &guard.source_id)?;
        let (owner, owner_repo) = Self::role_owner(guard, role, &binding.repo_key);
        let store = self
            .receipts
            .lock()
            .map_err(|_| PinnedReadError::Operational)?;
        let status = store
            .receipt_status(&binding.receipt_id)
            .map_err(|_| PinnedReadError::Operational)?;
        match status {
            // A superseded receipt was not forgotten by the user: a later
            // publish replaced it, so the caller's basis is merely stale.
            ReceiptStatus::Present | ReceiptStatus::Unknown | ReceiptStatus::Superseded { .. } => {
                Ok(false)
            }
            ReceiptStatus::CaptureForgotten { tombstone } => {
                if tombstone.source_id != owner {
                    return Err(PinnedReadError::Invalid);
                }
                // receipt_status already validates the current stored receipt.
                // Reuse the original guarded read instead of a third decode.
                let receipt = validated_receipt.ok_or(PinnedReadError::Unavailable)?;
                if receipt.id() != binding.receipt_id
                    || receipt.source_id().as_str() != owner
                    || receipt.repo_key() != owner_repo
                    || receipt.fact_key() != &binding.fact
                    || receipt.fact_digest() != binding.emitted_fact_digest
                {
                    return Err(PinnedReadError::Invalid);
                }
                Ok(true)
            }
            ReceiptStatus::Reclaimed { tombstone } => {
                if tombstone.source_id != owner {
                    return Err(PinnedReadError::Invalid);
                }
                Ok(true)
            }
        }
    }

    pub fn task_receipt(
        &self,
        guard: &RetentionGuard,
        fact: &FactKey,
        digest: &str,
        repo: &str,
        receipt_id: &str,
    ) -> Result<Receipt, PinnedReadError> {
        self.task_receipt_as(
            guard,
            core_graph::BindingRole::Primary,
            fact,
            digest,
            repo,
            receipt_id,
        )
    }

    /// [`Self::task_receipt`] for a binding in `role`; `repo` is the
    /// binding's repo key.
    pub fn task_receipt_as(
        &self,
        guard: &RetentionGuard,
        role: core_graph::BindingRole,
        fact: &FactKey,
        digest: &str,
        repo: &str,
        receipt_id: &str,
    ) -> Result<Receipt, PinnedReadError> {
        self.validate_task_guard(guard, &guard.source_id)?;
        let (owner, owner_repo) = Self::role_owner(guard, role, repo);
        if receipt_id.is_empty() || receipt_id.len() > 256 {
            return Err(PinnedReadError::Invalid);
        }
        let store = self
            .receipts
            .lock()
            .map_err(|_| PinnedReadError::Operational)?;
        let tx = store
            .conn
            .unchecked_transaction()
            .map_err(|_| PinnedReadError::Operational)?;
        receipt_storage(&tx).map_err(|_| PinnedReadError::Operational)?;
        let receipt = stored_receipt(&tx, receipt_id)?;
        if receipt.source_id().as_str() != owner
            || receipt.repo_key() != owner_repo
            || receipt.fact_key() != fact
            || receipt.fact_digest() != digest
        {
            return Err(PinnedReadError::Invalid);
        }
        tx.commit().map_err(|_| PinnedReadError::Operational)?;
        Ok(receipt)
    }

    /// The caller charges complete file-validation work before invoking this.
    /// The receipt and lease are already pinned; no current graph lookup occurs.
    pub fn task_text(
        &self,
        guard: &RetentionGuard,
        receipt: &Receipt,
        range_index: usize,
    ) -> Result<String, PinnedReadError> {
        self.validate_task_guard(guard, receipt.source_id().as_str())?;
        let range = receipt
            .ranges()
            .get(range_index)
            .ok_or(PinnedReadError::Invalid)?;
        self.captures
            .lock()
            .map_err(|_| PinnedReadError::Operational)?
            .read_text_span(&range.captured)
            .map_err(capture_error)
    }
    /// [`Self::task_text`] without decoding: the exact retained span bytes,
    /// for comparisons that must tell strict UTF-8 from lossy text (ADR-0072).
    pub fn task_bytes(
        &self,
        guard: &RetentionGuard,
        receipt: &Receipt,
        range_index: usize,
    ) -> Result<Vec<u8>, PinnedReadError> {
        self.validate_task_guard(guard, receipt.source_id().as_str())?;
        let range = receipt
            .ranges()
            .get(range_index)
            .ok_or(PinnedReadError::Invalid)?;
        self.captures
            .lock()
            .map_err(|_| PinnedReadError::Operational)?
            .read_span(&range.captured)
            .map_err(capture_error)
    }
    /// MCP callers pass the admitted host file-validation and returned-span
    /// maxima; the capture store also enforces its own hard/stored ceilings.
    /// Ordinary task_text intentionally keeps its existing capture-store policy
    /// without these additional per-session MCP limits.
    pub fn task_text_bounded(
        &self,
        guard: &RetentionGuard,
        receipt: &Receipt,
        range_index: usize,
        max_file_bytes: u64,
        max_span_bytes: u64,
    ) -> Result<String, PinnedReadError> {
        self.validate_task_guard(guard, receipt.source_id().as_str())?;
        let range = receipt
            .ranges()
            .get(range_index)
            .ok_or(PinnedReadError::Invalid)?;
        self.captures
            .lock()
            .map_err(|_| PinnedReadError::Operational)?
            .read_text_span_bounded(&range.captured, max_file_bytes, max_span_bytes)
            .map_err(capture_error)
    }
}

fn capture_error(error: CaptureError) -> PinnedReadError {
    match error {
        CaptureError::Missing(_) => PinnedReadError::Unavailable,
        CaptureError::Invalid(_)
        | CaptureError::Limit(_)
        | CaptureError::Corrupt(_)
        | CaptureError::Encoding
        | CaptureError::Json(_) => PinnedReadError::Invalid,
        CaptureError::Sqlite(_) | CaptureError::Io(_) | CaptureError::Git(_) => {
            PinnedReadError::Operational
        }
    }
}
impl PrimarySourceStore {
    pub fn task_capture_present(
        &self,
        guard: &RetentionGuard,
        span: &agents::TaskCaptureSpanRef,
    ) -> Result<(), PinnedReadError> {
        self.validate_task_guard(guard, &span.file.source_id)?;
        let source = source_capture::SourceId::new(&span.file.source_id).map_err(capture_error)?;
        let present = self
            .captures
            .lock()
            .map_err(|_| PinnedReadError::Operational)?
            .capture_belongs_to_source(&source, &span.file.capture_id)
            .map_err(capture_error)?;
        if !present {
            return Err(PinnedReadError::Unavailable);
        }
        Ok(())
    }
    /// Human location inspection only; this is not agent evidence acquisition.
    pub fn task_location(
        &self,
        guard: &RetentionGuard,
        receipt: &Receipt,
        index: usize,
    ) -> Result<source_capture::CapturedSpanLocation, PinnedReadError> {
        self.validate_task_guard(guard, receipt.source_id().as_str())?;
        let range = receipt
            .ranges()
            .get(index)
            .ok_or(PinnedReadError::Invalid)?;
        self.captures
            .lock()
            .map_err(|_| PinnedReadError::Operational)?
            .read_span_location(&range.captured)
            .map_err(capture_error)
    }
}

#[cfg(any(test, feature = "test-support"))]
#[path = "pinned/preflight_tests.rs"]
mod preflight_tests;
