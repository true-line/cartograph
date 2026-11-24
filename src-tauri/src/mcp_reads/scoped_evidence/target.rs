//! Scoped descriptors are deliberately incompatible with investigation targets.
use super::*;
use agents::{TaskCaptureSpanRef, TaskRangeRole};
use core_graph::StoreRevision;
use core_prov::EvidenceRef;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_TARGET_BYTES: usize = 8 * 1024;
const TARGET_DOMAIN: &str = "mcp-scoped-evidence-target-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScopedEvidenceTarget {
    pub schema_version: u32,
    pub target_id: String,
    pub basis_id: String,
    pub revision: StoreRevision,
    pub fact: TaskFactKey,
    pub fact_digest: String,
    pub registered_source_id: String,
    pub receipt_id: String,
    pub receipt_inventory_index: u32,
    pub role: TaskRangeRole,
    pub index: u32,
    pub source: EvidenceRef,
    pub captured: TaskCaptureSpanRef,
}

pub(super) fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn prefixed(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(hash)
}
pub(super) fn fact_valid(fact: &TaskFactKey) -> bool {
    match fact {
        TaskFactKey::Node { id } => text(id, 8192),
        TaskFactKey::Edge {
            source,
            label,
            destination,
        } => text(source, 8192) && text(label, 256) && text(destination, 8192),
    }
}
impl ScopedEvidenceTarget {
    /// Bound the borrowed descriptor before copying its DOM or strings. Close
    /// EvidenceRef ingress explicitly because its shared DTO permits extra fields.
    pub(crate) fn decode(value: &serde_json::Value) -> Result<Self, ReadError> {
        admit(value, MAX_TARGET_BYTES)?;
        let source = value
            .get("source")
            .and_then(serde_json::Value::as_object)
            .ok_or(ReadError::InvalidRequest)?;
        let fields = ["repo", "path", "byte_start", "byte_end", "commit_sha"];
        if source.len() != fields.len() || !fields.iter().all(|key| source.contains_key(*key)) {
            return Err(ReadError::InvalidRequest);
        }
        let target: Self =
            serde_json::from_value(value.clone()).map_err(|_| ReadError::InvalidRequest)?;
        target.validate()?;
        Ok(target)
    }
    pub(crate) fn validate(&self) -> Result<(), ReadError> {
        admit(self, MAX_TARGET_BYTES)?;
        let file = &self.captured.file;
        let path = &self.source.path;
        // An observed target names the source's input identity and cites an
        // observed receipt; a primary target names the source (ADR-0079).
        let observed = self.binding_role() == BindingRole::Observed;
        let source_id = self
            .registered_source_id
            .strip_prefix(crate::observation_inputs::INPUT_REPO_PREFIX)
            .unwrap_or(&self.registered_source_id)
            .strip_prefix("src_")
            .is_some_and(|s| {
                s.len() == 32
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
        let primary_receipt = prefixed(&self.receipt_id, "ts-primary-v1:")
            || prefixed(&self.receipt_id, "ts-primary-v2:")
            || prefixed(&self.receipt_id, "ts-primary-v3:")
            || prefixed(&self.receipt_id, "ts-primary-v4:")
            || prefixed(&self.receipt_id, "py-primary-v1:")
            || prefixed(&self.receipt_id, "go-primary-v1:")
            || prefixed(&self.receipt_id, "java-primary-v1:")
            || prefixed(&self.receipt_id, "rs-primary-v1:")
            || prefixed(&self.receipt_id, "kt-primary-v1:")
            || prefixed(&self.receipt_id, "tf-primary-v1:")
            || prefixed(&self.receipt_id, "tf-primary-v2:")
            || prefixed(&self.receipt_id, "pipe-primary-v1:")
            || prefixed(&self.receipt_id, "pipe-primary-v2:");
        let receipt_kind = if observed {
            prefixed(&self.receipt_id, "obs-input-v1:")
        } else {
            primary_receipt
        };
        let digest_prefix = match self.fact {
            TaskFactKey::Node { .. } => "node-v1:",
            _ => "edge-v1:",
        };
        if self.schema_version != 1
            || !prefixed(&self.target_id, &format!("{TARGET_DOMAIN}:"))
            || !prefixed(&self.basis_id, "mcp-scoped-session-v1:")
            || self.revision.graph_instance.len() != 32
            || !self
                .revision
                .graph_instance
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.revision.generation > i64::MAX as u64
            || !fact_valid(&self.fact)
            || !prefixed(&self.fact_digest, digest_prefix)
            || !source_id
            || !receipt_kind
            || self.receipt_inventory_index >= 1024
            || self.index >= 1024
            || !text(&self.source.repo, 256)
            || !text(&self.source.commit_sha, 256)
            || !text(path, source_capture::MAX_PATH_BYTES)
            || path.contains(['\\', ':'])
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || self.source.byte_start >= self.source.byte_end
            || self.source.byte_end - self.source.byte_start > 8192
            || self.captured.byte_start != self.source.byte_start
            || self.captured.byte_end != self.source.byte_end
            || file.source_id != self.registered_source_id
            || file.path != *path
            || file.byte_len > source_capture::MAX_FILE_BYTES
            || self.captured.byte_end > file.byte_len
            || !prefixed(&file.capture_id, "capture-v1:")
            || !hash(&file.digest)
            || self.target_id != self.content_id()?
        {
            return Err(ReadError::InvalidRequest);
        }
        Ok(())
    }
    /// The binding role this target cites, implied by its owner identity.
    pub(crate) fn binding_role(&self) -> BindingRole {
        if self
            .registered_source_id
            .starts_with(crate::observation_inputs::INPUT_REPO_PREFIX)
        {
            BindingRole::Observed
        } else {
            BindingRole::Primary
        }
    }
    pub(super) fn content_id(&self) -> Result<String, ReadError> {
        admit(self, MAX_TARGET_BYTES)?;
        // Clear only after bounded admission; the independent domain cannot be
        // accepted as a complete context or an ordinary inspection target.
        let mut identity = self.clone();
        identity.target_id.clear();
        super::super::digest(TARGET_DOMAIN, &identity)
    }
}
