//! Qualified compiled-artifact reads for in-app investigations (ADR-0089,
//! AC-1812). An artifact page is untrusted context: it is never a citation,
//! and the ledger keeps only its manifest, not its content.
use super::{InvestigationError, MAX_QUERY_BYTES};
use context_hub::coverage::CoverageProof;
use serde::{Deserialize, Serialize};

/// Longest catalogue id the agent core admits. The host checks membership in
/// the readable artifact catalogue; the core only checks the id's shape.
pub const MAX_ARTIFACT_ID_BYTES: usize = 64;
/// Largest content offset a read may name. Compiled artifacts are bounded far
/// below this by the host's compile limits.
pub const MAX_ARTIFACT_OFFSET: usize = 64 * 1024 * 1024;

/// One requested page of one compiled artifact at the frozen basis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationArtifactRead {
    /// The investigation's `context-v2:` identity.
    pub context_id: String,
    /// A readable artifact catalogue id, such as `gap-register-index`.
    pub artifact_id: String,
    /// `None` on a first read; the issued content id on a continuation.
    pub expected_content_id: Option<String>,
    /// UTF-8 byte offset into the artifact content.
    pub offset: usize,
    /// Largest response the agent accepts, at most `MAX_QUERY_BYTES`.
    pub max_bytes: usize,
}

impl InvestigationArtifactRead {
    /// Shape only. A continuation must name the content it continues, and a
    /// first read starts at offset zero.
    pub fn validate_shape(&self) -> bool {
        artifact_id(&self.artifact_id)
            && (1..=MAX_QUERY_BYTES).contains(&self.max_bytes)
            && self.offset <= MAX_ARTIFACT_OFFSET
            && match &self.expected_content_id {
                None => self.offset == 0,
                Some(content) => content_id(content),
            }
    }
}

/// One acquisition of a compiled artifact page. Repeated reads still consume
/// the shared raw acquisition budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationArtifactReadManifest {
    /// The admitted request.
    pub request: InvestigationArtifactRead,
    /// The compiled artifact's content id at the frozen basis.
    pub content_id: String,
    /// Content hash of the exact response the model received.
    pub response_hash: String,
    /// Byte length of that response.
    pub response_bytes: usize,
    /// Where the next page starts, or `None` at the end of the content.
    pub next_offset: Option<usize>,
}

impl InvestigationArtifactReadManifest {
    /// Validate against the ledger's frozen coverage proof.
    pub fn validate(&self, proof: &CoverageProof) -> Result<(), InvestigationError> {
        if self.request.context_id != proof.context_id
            || !self.request.validate_shape()
            || !content_id(&self.content_id)
            || self
                .request
                .expected_content_id
                .as_ref()
                .is_some_and(|expected| expected != &self.content_id)
            || self.response_bytes == 0
            || self.response_bytes > self.request.max_bytes
            || !crate::source_basis::hash(&self.response_hash)
            || self
                .next_offset
                .is_some_and(|next| next <= self.request.offset || next > MAX_ARTIFACT_OFFSET)
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }

    /// Admit the exact retained response bytes for this manifest.
    pub fn admit_response(
        &self,
        proof: &CoverageProof,
        raw: &str,
    ) -> Result<InvestigationArtifactPage, InvestigationError> {
        self.validate(proof)?;
        if raw.len() != self.response_bytes
            || core_prov::content_hash(raw.as_bytes()) != self.response_hash
        {
            return Err(InvestigationError::InvalidInput);
        }
        super::strict::preflight(raw, MAX_QUERY_BYTES, 8, 64, 8)?;
        let page: InvestigationArtifactPage =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidInput)?;
        let end = page
            .offset
            .checked_add(page.content.len())
            .ok_or(InvestigationError::InvalidInput)?;
        if page.schema_version != 1
            || page.context_id != self.request.context_id
            || page.artifact_id != self.request.artifact_id
            || page.content_id != self.content_id
            || page.offset != self.request.offset
            || page.next_offset != self.next_offset
            || end > page.total_content_bytes
            || page
                .next_offset
                .map_or(end != page.total_content_bytes, |next| next != end)
            || !super::text(&page.schema, 128)
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(page)
    }
}

/// Untrusted compiled-artifact content, not graph evidence or a citation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigationArtifactPage {
    /// Always 1.
    pub schema_version: u32,
    /// The investigation's context identity.
    pub context_id: String,
    /// The catalogue id that was read.
    pub artifact_id: String,
    /// The artifact's schema name.
    pub schema: String,
    /// The compiled content id.
    pub content_id: String,
    /// First byte of `content` within the artifact content.
    pub offset: usize,
    /// Where the next page starts, or `None` at the end.
    pub next_offset: Option<usize>,
    /// Total artifact content bytes.
    pub total_content_bytes: usize,
    /// The page's UTF-8 content.
    pub content: String,
}

/// `SpecArtifact::compute_content_id` identities: `spec-artifact-v1:<hex>`.
fn content_id(value: &str) -> bool {
    value
        .strip_prefix("spec-artifact-v1:")
        .is_some_and(crate::source_basis::hash)
}

fn artifact_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ARTIFACT_ID_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(offset: usize, expected: Option<&str>) -> InvestigationArtifactRead {
        InvestigationArtifactRead {
            context_id: format!("context-v2:{}", "a".repeat(64)),
            artifact_id: "gap-register-index".into(),
            expected_content_id: expected.map(str::to_owned),
            offset,
            max_bytes: 1024,
        }
    }

    // AC-1812: a first read starts at zero; a continuation names its content.
    #[test]
    fn artifact_read_shape_binds_continuations_to_content() {
        let content = format!("spec-artifact-v1:{}", "c".repeat(64));
        assert!(read(0, None).validate_shape());
        assert!(!read(10, None).validate_shape());
        assert!(read(10, Some(&content)).validate_shape());
        assert!(!read(10, Some(&"c".repeat(64))).validate_shape());
        assert!(!read(10, Some("spec-artifact-v1:short")).validate_shape());
        for bad in ["", "Gap", "../x", "-a", "a-", &"a".repeat(65)] {
            let mut request = read(0, None);
            request.artifact_id = bad.into();
            assert!(!request.validate_shape(), "{bad}");
        }
        let mut request = read(0, None);
        request.max_bytes = MAX_QUERY_BYTES + 1;
        assert!(!request.validate_shape());
    }
}
