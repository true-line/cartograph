//! The curated read of an MCP session (SPEC-12 slice 4, ADR-0061, AC-0654).
//!
//! The session computes the projection once, through `crate::curated`, and
//! keeps it. Two artifacts read it through `cartograph_read_artifact`:
//!
//! - `curated`: the official `curated.md`, compiled by the selected compiler
//!   from the session's snapshot and this projection. For the same curated
//!   revision and export mode its bytes are the desktop export's.
//! - `curated-json`: the same projection as one JSON document with the DR-4
//!   fields. It is an MCP representation, not an export artifact: the export
//!   holds no such file.
//!
//! Both follow the session's export mode. Every curated item is InferredWeak
//! (ADR-0061 decision 7), so a `verified-only` session gets no item in
//! either form, only the number left out (R-INT-5). Listed records and
//! assertion decisions assert nothing and are the same in both modes.
//!
//! Findings accepted as curated (ADR-0074, AC-0845) follow the same rule:
//! `curated.md` prints them as the export does, and `curated-json` carries
//! `findings` (the eligible ones, in `best-effort` only), `findings_omitted`,
//! `findings_listed` (every other accepted finding, without its statement)
//! and `finding_counts`, the split the Workbench reads.
//!
//! A projection that was not computed is stated with its reason in both
//! forms. Nothing here writes a fact or changes a tier (R-INT-1, R-INT-3).

use context_hub::curated::{
    AssertionDecisionSummary, CuratedCounts, CuratedProjection, CuratedRecord, CuratedView,
};
use serde::Serialize;
use spec::{CuratedFailure, ExportMode};

/// Artifact id of the official `curated.md`.
pub(crate) const ARTIFACT_ID: &str = "curated";
/// Artifact id of the structured form.
pub(crate) const JSON_ARTIFACT_ID: &str = "curated-json";
/// Schema of the structured form. It is not a `cartograph.spec` schema: the
/// export has no such artifact.
pub(crate) const JSON_SCHEMA: &str = "cartograph.mcp/curated-json@1";

/// The curated revision in a session's basis, or why none was computed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum CuratedBasis {
    /// The projection was computed for this session.
    Computed {
        /// The curated revision (ADR-0061 decision 2).
        revision: String,
    },
    /// It was not. The reason is the one `curated.md` states.
    NotComputed(CuratedFailure),
}

impl CuratedBasis {
    /// The basis with the longest wire form, for the reservation ledger.
    pub(super) fn reservation_fixture() -> Self {
        Self::Computed {
            revision: format!(
                "{}:{}",
                context_hub::curated::CURATED_REVISION_PREFIX,
                core_prov::content_hash(&[])
            ),
        }
    }
}

/// What a session holds of the curated projection.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "one value per compilation or session; boxing would touch every reader"
)]
pub(crate) enum CuratedRead {
    /// The projection over the session's snapshot.
    Computed(CuratedProjection),
    /// Why there is none.
    NotComputed(CuratedFailure),
}

impl CuratedRead {
    pub(super) fn of(projected: Result<CuratedProjection, CuratedFailure>) -> Self {
        match projected {
            Ok(projection) => Self::Computed(projection),
            Err(failure) => Self::NotComputed(failure),
        }
    }

    pub(super) fn computed(&self) -> bool {
        matches!(self, Self::Computed(_))
    }

    pub(super) fn basis(&self) -> CuratedBasis {
        match self {
            Self::Computed(projection) => CuratedBasis::Computed {
                revision: projection.revision.clone(),
            },
            Self::NotComputed(failure) => CuratedBasis::NotComputed(*failure),
        }
    }

    /// What the compiler is supplied for `curated.md`: what the export
    /// supplies for the same outcome.
    pub(super) fn supplied(&self) -> spec::Curated<'_> {
        match self {
            Self::Computed(projection) => spec::Curated::Computed(projection),
            Self::NotComputed(failure) => spec::Curated::NotComputed(*failure),
        }
    }

    /// The structured form, as one artifact with its own content identity.
    pub(super) fn json_artifact(&self, mode: ExportMode) -> Result<spec::SpecArtifact, ()> {
        let content = match self {
            Self::Computed(projection) => {
                let shown = match mode {
                    ExportMode::BestEffort => projection.items.as_slice(),
                    // R-INT-5: no InferredWeak item, and the number left out.
                    ExportMode::VerifiedOnly => &[],
                };
                serde_json::to_string_pretty(&Computed {
                    schema: JSON_SCHEMA,
                    export_mode: mode,
                    status: "computed",
                    view: projection.view,
                    revision: &projection.revision,
                    graph_basis: &projection.graph_basis,
                    items: shown,
                    items_omitted: projection.items.len() - shown.len(),
                    listed: &projection.listed,
                    findings: crate::curated_workbench::FindingsRead::of(projection, mode),
                    assertion_decisions: &projection.assertion_decisions,
                    counts: projection.counts,
                })
            }
            Self::NotComputed(failure) => serde_json::to_string_pretty(&NotComputed {
                schema: JSON_SCHEMA,
                export_mode: mode,
                status: "not_computed",
                failure: *failure,
            }),
        }
        .map_err(|_| ())?;
        let mut artifact = spec::SpecArtifact {
            id: JSON_ARTIFACT_ID.into(),
            file_name: "curated.json".into(),
            title: JSON_TITLE.into(),
            format: "json".into(),
            schema: JSON_SCHEMA.into(),
            content_id: String::new(),
            content,
            assertions: Vec::new(),
        };
        artifact.content_id = artifact.compute_content_id();
        Ok(artifact)
    }
}

const JSON_TITLE: &str = "Curated items (structured)";

/// The catalogue entry of the structured form.
pub(super) fn json_descriptor() -> spec::ArtifactDescriptor {
    spec::ArtifactDescriptor {
        id: JSON_ARTIFACT_ID,
        file_name: "curated.json",
        title: JSON_TITLE,
        format: "json",
        schema: JSON_SCHEMA.into(),
    }
}

/// The DR-4 fields of a computed projection (SPEC-12 "DR-4 fields"). Each
/// record carries its identity, proposed edge, Gap, tier and confidence as
/// produced, review status, source support, basis, state and reason.
#[derive(Serialize)]
struct Computed<'a> {
    schema: &'static str,
    export_mode: ExportMode,
    status: &'static str,
    view: CuratedView,
    revision: &'a str,
    graph_basis: &'a str,
    /// The curated items this export mode shows.
    items: &'a [CuratedRecord],
    /// Curated items the export mode leaves out.
    items_omitted: usize,
    listed: &'a [CuratedRecord],
    /// `findings`, `findings_omitted`, `findings_listed` and
    /// `finding_counts` (ADR-0074 decision 11).
    #[serde(flatten)]
    findings: crate::curated_workbench::FindingsRead,
    assertion_decisions: &'a AssertionDecisionSummary,
    counts: CuratedCounts,
}

#[derive(Serialize)]
struct NotComputed {
    schema: &'static str,
    export_mode: ExportMode,
    status: &'static str,
    failure: CuratedFailure,
}

#[cfg(all(test, not(app_test_group = "acceptance")))]
mod tests;
