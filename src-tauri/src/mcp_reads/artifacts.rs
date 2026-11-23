//! Official selected compilation and identity-bound artifact text pages.
//! No cache: the caller owns every produced handle and its retention admission.

use super::{ReadError, ReadSession, curated_read};
use context_hub::coverage::CoverageStatus;
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
/// and single-artifact read still produce it. A `curated` read therefore
/// prints citation ids that this surface cannot look up, and so does a read
/// of the D1 document.
const NOT_READABLE: [&str; 2] = ["fact-index", "fact-index-json"];

/// The D1 document (#1748, SPEC-15 slice 5, AC-0667). It reads the session's
/// curated projection, which carries the accepted section drafts, and the
/// session's graph content hash; for the same inputs and export mode its
/// bytes are the desktop export's.
const D1_ID: &str = "d1-product-specification";
const SECURITY_ID: &str = "security-view";

/// The two projections of the stored coverage inventories (AC-0592).
const COVERAGE_IDS: [&str; 2] = ["coverage", "coverage-index"];
/// The bundle manifest, which states the graph hash, curated revision and
/// source revisions the export was compiled from (AC-0682).
const MANIFEST_ID: &str = "bundle-manifest";

/// The artifacts an MCP reader may select: the compiler's catalogue without
/// the ids in [`NOT_READABLE`], and the structured form of the curated
/// projection after `curated` (SPEC-12 slice 4, AC-0654). The one list
/// behind the advertised catalogue, the tool's input schema and the read's
/// own admission.
pub(crate) fn readable_catalog() -> Vec<spec::ArtifactDescriptor> {
    let mut catalog = Vec::new();
    for artifact in spec::artifact_catalog() {
        if NOT_READABLE.contains(&artifact.id) {
            continue;
        }
        let curated = artifact.id == curated_read::ARTIFACT_ID;
        catalog.push(artifact);
        if curated {
            catalog.push(curated_read::json_descriptor());
        }
    }
    catalog
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
        if id == curated_read::JSON_ARTIFACT_ID {
            // The projection the session holds, serialized. Nothing is
            // traced or compiled, and no partial document is returned.
            let artifact = self
                .curated
                .json_artifact(self.basis.export_mode)
                .map_err(|()| ReadError::Storage)?;
            return Ok(if artifact.content.len() > compile_limits.max_bytes {
                ArtifactCompilation::Limited {
                    artifact_id: id.into(),
                    limit: ArtifactLimit::Compiler(spec::SelectedArtifactLimit::OutputBytes {
                        remaining: compile_limits.max_bytes,
                    }),
                }
            } else {
                ArtifactCompilation::Produced(CompiledArtifact {
                    basis_id: self.basis_id()?,
                    artifact,
                })
            });
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
        } else if id == curated_read::ARTIFACT_ID
            || id == D1_ID
            || id == MANIFEST_ID
            || COVERAGE_IDS.contains(&id)
        {
            // The selected compiler the desktop single-artifact read uses,
            // supplied as the export supplies it. `curated.md` and the D1
            // document read the projection this session computed and no
            // coverage inventory, as does the bundle manifest, which states
            // the graph hash, curated revision and source revisions.
            // The coverage artifacts (#1814, AC-0657) read the inventories
            // of this session's own bounded read, borrowed, and no
            // projection. Neither reads a security projection.
            let coverage = COVERAGE_IDS.contains(&id);
            let stale = if coverage {
                self.stale_coverage()
            } else {
                Vec::new()
            };
            // D1 frames the derived Capabilities and caller roles beside its
            // base sections (AC-0663); a refused overlay leaves it without
            // them rather than failing the read.
            let knowledge = (id == D1_ID || id == "architecture" || id == "architecture-dsl")
                .then(|| self.framed_knowledge())
                .flatten();
            spec::compile_spec_selected_supplied(
                &self.base.graph.0,
                &self.base.graph.1,
                &flows,
                self.basis.export_mode,
                &self.rejected_hashes,
                &[id],
                compile_limits,
                spec::Supplied {
                    knowledge: knowledge.as_ref(),
                    coverage: if coverage { &self.recorded } else { &[] },
                    stale_coverage: &stale,
                    curated: if coverage {
                        spec::Curated::NotSupplied
                    } else {
                        self.curated.supplied()
                    },
                    // The D1 header states it, computed or not (AC-0663).
                    graph_content_hash: (!coverage).then_some(self.graph_content_hash.as_str()),
                    // The bundle manifest states them (AC-0682).
                    source_revisions: (!coverage).then_some(self.source_revisions.as_slice()),
                    repo_names: &[],
                },
            )
            .map_err(|_| ReadError::InvalidRequest)?
            .into_iter()
            .next()
            .ok_or(ReadError::Storage)?
        } else {
            // The security view lists access per derived Capability
            // (AC-0853); a refused overlay leaves it with its base sections.
            let knowledge = (id == SECURITY_ID)
                .then(|| self.framed_knowledge())
                .flatten();
            spec::compile_spec_selected_with_security_supplied(
                &self.base.graph.0,
                &self.base.graph.1,
                &flows,
                self.basis.export_mode,
                &self.rejected_hashes,
                &[id],
                compile_limits,
                (
                    spec::SecurityLimits {
                        max_rows: security.rows,
                        max_output_bytes: security.bytes,
                    },
                    spec::Supplied {
                        knowledge: knowledge.as_ref(),
                        ..spec::Supplied::default()
                    },
                ),
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

impl ReadSession {
    /// The overlay's derived Capabilities and caller roles, validated, for
    /// an artifact that frames them as context: the D1 document (AC-0663)
    /// and the security view (AC-0853). A refused or empty overlay frames
    /// nothing rather than failing the read.
    fn framed_knowledge(&self) -> Option<spec::KnowledgeStories<'_>> {
        let candidates = self
            .capabilities
            .iter()
            .map(|f| match &f.item {
                core_graph::overlay::OverlayItem::Node(capability) => Some(spec::CapabilityStory {
                    capability,
                    cites: &f.cites,
                }),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        if candidates.is_empty() && self.actors.is_empty() {
            return None;
        }
        spec::validate_knowledge_stories(
            &self.base,
            &candidates,
            &self.actors,
            self.basis.export_mode,
            &self.rejected_hashes,
        )
        .ok()
    }

    /// The repositories whose stored inventory this session's coverage
    /// qualification does not hold as recorded under the current
    /// registration and plugin basis, with what it compared. Fail closed:
    /// every status but `Recorded` is named, so no such inventory is read
    /// as current. A repository with no stored inventory has no row to mark.
    fn stale_coverage(&self) -> Vec<spec::StaleCoverage<'_>> {
        self.qualified
            .coverage()
            .repos
            .iter()
            .filter(|repo| repo.status != CoverageStatus::Recorded)
            .map(|repo| spec::StaleCoverage {
                repo_key: &repo.repo,
                compared: &repo.compared,
            })
            .collect()
    }
}

impl CompiledArtifact {
    #[cfg(test)]
    pub(crate) fn basis_id(&self) -> &str {
        &self.basis_id
    }
    #[cfg(test)]
    pub(crate) fn artifact(&self) -> &spec::SpecArtifact {
        &self.artifact
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
        let end = fitting_end(&self.artifact.content, offset, max_content_bytes, |end| {
            fits(&self.page_at(offset, end), max_response_bytes.get())
        })?;
        Ok(self.page_at(offset, end))
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

/// The end of the largest UTF-8-safe page from `offset` that `fits_at`
/// admits, at most `max_content_bytes` long. Shared by MCP artifact reads and
/// in-app artifact reads (ADR-0089). The end-of-content offset yields one
/// empty final page; any other page must make progress.
pub(crate) fn fitting_end(
    content: &str,
    offset: usize,
    max_content_bytes: NonZeroUsize,
    fits_at: impl Fn(usize) -> bool,
) -> Result<usize, ReadError> {
    if offset > content.len() || !content.is_char_boundary(offset) {
        return Err(ReadError::InvalidRequest);
    }
    let mut end = offset
        .saturating_add(max_content_bytes.get())
        .min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    if fits_at(end) {
        return if end > offset || offset == content.len() {
            Ok(end)
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
        if fits_at(middle) {
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
        && fits_at(next)
    {
        low = next;
    }
    if low == offset {
        return Err(ReadError::Limit);
    }
    Ok(low)
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

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod tests;

#[cfg(all(test, unix))]
#[path = "artifacts/captured_slice_acceptance.rs"]
mod captured_slice_acceptance;
