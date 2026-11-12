//! Official selected compilation and identity-bound artifact text pages.
//! No cache: the caller owns every produced handle and its retention admission.

use super::{ReadError, ReadSession};
use serde::Serialize;
use std::{
    io::{self, Write},
    num::NonZeroUsize,
};

/// Original production limit, with no partial artifact or invented identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactLimit {
    Trace(flowtracer::TraceLimitExceeded),
    Compiler(spec::SelectedArtifactLimit),
}

#[derive(Debug)]
pub(crate) enum ArtifactCompilation {
    Produced(CompiledArtifact),
    Limited {
        artifact_id: String,
        limit: ArtifactLimit,
    },
}

/// Immutable complete production artifact and the complete session basis that
/// produced it. Not a client-writable/serializable cache or live graph reader.
#[derive(Debug)]
pub(crate) struct CompiledArtifact {
    basis_id: String,
    artifact: spec::SpecArtifact,
}

/// Content-only page, not a serialized complete `SpecArtifact`. Structured
/// assertions remain in the owned handle and are explicitly excluded here.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ArtifactPage<'a> {
    pub basis_id: &'a str,
    pub artifact_id: &'a str,
    pub schema: &'a str,
    pub content_id: &'a str,
    pub representation: &'static str,
    pub structured_assertions_included: bool,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub total_content_bytes: usize,
    pub partial: bool,
    pub content: &'a str,
}

/// The fact and evidence index (ADR-0058) is not readable over MCP: compiling
/// it compiles every citing artifact and holds the rows, which the reserved
/// `spec::logical_compile_bound` workspace does not model. The desktop export
/// and single-artifact read still produce it.
///
/// `curated` (SPEC-12, AC-0653) is not readable either. An MCP read does not
/// compute the curated projection, so the document compiled here could only
/// say that none was supplied. The curated read of SPEC-12 slice 4 is the MCP
/// surface for it, and `h3_curated_available` stays false until then.
const NOT_READABLE: [&str; 3] = ["fact-index", "fact-index-json", "curated"];

/// The official artifacts an MCP reader may select: the compiler's catalogue
/// without the ids in [`NOT_READABLE`]. The one list behind the advertised
/// catalogue, the tool's input schema and the read's own admission.
pub(crate) fn readable_catalog() -> Vec<spec::ArtifactDescriptor> {
    spec::artifact_catalog()
        .into_iter()
        .filter(|artifact| !NOT_READABLE.contains(&artifact.id))
        .collect()
}

impl ReadSession {
    /// Authoritative metadata only; availability/content identity requires an
    /// actual selected compilation, rather than an ID inferred from a title.
    pub(crate) fn artifact_catalog(&self) -> Vec<spec::ArtifactDescriptor> {
        readable_catalog()
    }

    pub(crate) fn compile_artifact(
        &self,
        id: &str,
        trace_limits: flowtracer::TraceLimits,
        compile_limits: spec::SelectedCompileLimits,
    ) -> Result<ArtifactCompilation, ReadError> {
        // An id that is not readable here is refused like an unknown one,
        // before any tracing or compilation.
        if !readable_catalog().iter().any(|artifact| artifact.id == id) {
            return Err(ReadError::InvalidRequest);
        }
        let security = spec::security_projection_bound(
            self.graph_limits.max_bytes,
            self.graph_limits.max_rows,
            self.graph_limits.max_values,
        )
        .ok_or(ReadError::Limit)?;
        let flows =
            match flowtracer::trace_bounded(&self.base.graph.0, &self.base.graph.1, trace_limits) {
                Ok(flows) => flows,
                Err(limit) => {
                    return Ok(ArtifactCompilation::Limited {
                        artifact_id: id.into(),
                        limit: ArtifactLimit::Trace(limit),
                    });
                }
            };
        let outcome = if id == "user-stories" {
            // Trace refusal precedes candidate validation; retained flows remain charged during compilation.
            let candidates = self
                .capabilities
                .iter()
                .map(|f| match &f.item {
                    core_graph::overlay::OverlayItem::Node(capability) => {
                        Ok(spec::CapabilityStory {
                            capability,
                            cites: &f.cites,
                        })
                    }
                    _ => Err(ReadError::Storage),
                })
                .collect::<Result<Vec<_>, _>>()?;
            spec::compile_knowledge_user_stories_with_flows_and_security(
                &self.base,
                &candidates,
                &self.actors,
                &flows,
                self.basis.export_mode,
                &self.rejected_hashes,
                compile_limits,
                spec::SecurityLimits {
                    max_rows: security.rows,
                    max_output_bytes: security.bytes,
                },
            )
            .map_err(|_| ReadError::Storage)?
        } else {
            spec::compile_spec_selected_with_security(
                &self.base.graph.0,
                &self.base.graph.1,
                &flows,
                self.basis.export_mode,
                &self.rejected_hashes,
                &[id],
                compile_limits,
                spec::SecurityLimits {
                    max_rows: security.rows,
                    max_output_bytes: security.bytes,
                },
            )
            .map_err(|_| ReadError::InvalidRequest)?
            .into_iter()
            .next()
            .ok_or(ReadError::Storage)?
        };
        Ok(match outcome {
            spec::SelectedArtifactOutcome::Produced(artifact) => {
                ArtifactCompilation::Produced(CompiledArtifact {
                    basis_id: self.basis_id()?,
                    artifact,
                })
            }
            spec::SelectedArtifactOutcome::Limited { artifact_id, limit } => {
                ArtifactCompilation::Limited {
                    artifact_id,
                    limit: ArtifactLimit::Compiler(limit),
                }
            }
        })
    }
}

impl CompiledArtifact {
    #[cfg(test)]
    pub(crate) fn basis_id(&self) -> &str {
        &self.basis_id
    }
    pub(crate) fn content_id(&self) -> &str {
        &self.artifact.content_id
    }

    /// Return the largest valid UTF-8 prefix allowed by both explicit caps.
    /// `max_response_bytes` counts the entire compact JSON `ArtifactPage` only;
    /// a future MCP wrapper must separately admit its own additional envelope.
    /// The end-of-content offset yields one empty final page, never a cursor.
    pub(crate) fn page(
        &self,
        expected_basis_id: &str,
        expected_content_id: &str,
        offset: usize,
        max_content_bytes: NonZeroUsize,
        max_response_bytes: NonZeroUsize,
    ) -> Result<ArtifactPage<'_>, ReadError> {
        if expected_basis_id != self.basis_id || expected_content_id != self.artifact.content_id {
            return Err(ReadError::StaleBasis);
        }
        let content = &self.artifact.content;
        if offset > content.len() || !content.is_char_boundary(offset) {
            return Err(ReadError::InvalidRequest);
        }
        let mut end = offset
            .saturating_add(max_content_bytes.get())
            .min(content.len());
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = self.page_at(offset, end);
        if fits(&candidate, max_response_bytes.get()) {
            return if end > offset || offset == content.len() {
                Ok(candidate)
            } else {
                Err(ReadError::Limit)
            };
        }
        // The complete page may have a smaller null-cursor envelope; it was
        // tried above. All remaining candidates are partial, with monotonic
        // serialized prefix lengths and numeric continuation widths.
        let (mut low, mut high) = (offset, end);
        while low < high {
            let mut middle = low + (high - low).div_ceil(2);
            while !content.is_char_boundary(middle) {
                middle -= 1;
            }
            if middle == low {
                break;
            }
            if fits(&self.page_at(offset, middle), max_response_bytes.get()) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        // `high` can lie inside a multibyte character after a failed candidate.
        // Check the next complete character to avoid losing a fitting boundary.
        let next = content[low..end]
            .chars()
            .next()
            .map(|character| low + character.len_utf8());
        if let Some(next) = next
            && fits(&self.page_at(offset, next), max_response_bytes.get())
        {
            low = next;
        }
        if low == offset {
            return Err(ReadError::Limit);
        }
        Ok(self.page_at(offset, low))
    }

    fn page_at(&self, offset: usize, end: usize) -> ArtifactPage<'_> {
        ArtifactPage {
            basis_id: &self.basis_id,
            artifact_id: &self.artifact.id,
            schema: &self.artifact.schema,
            content_id: &self.artifact.content_id,
            representation: "artifact-content",
            structured_assertions_included: false,
            offset,
            next_offset: (end < self.artifact.content.len()).then_some(end),
            total_content_bytes: self.artifact.content.len(),
            partial: offset != 0 || end != self.artifact.content.len(),
            content: &self.artifact.content[offset..end],
        }
    }
}

fn fits(value: &impl Serialize, cap: usize) -> bool {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("response limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(cap), value).is_ok()
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
#[path = "artifacts/captured_slice_acceptance.rs"]
mod captured_slice_acceptance;
