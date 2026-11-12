//! The curated read of the Spec Workbench (SPEC-12 slice 3, ADR-0061,
//! AC-0655).
//!
//! It is not a second read. `export_spec_with_views` returns it beside the
//! bundle, made from the one projection that compile computed, so its
//! revision and counts are the bundle's `curated_basis` by construction and
//! the Workbench cannot show a projection of another revision.
//!
//! Each record carries its reason code and the sentence `curated.md` prints
//! for that code, so the UI maps no code to prose. Every curated item is
//! InferredWeak (ADR-0061 decision 7): a `verified-only` read returns no
//! item, only the number left out (R-INT-5), as the export and the MCP read
//! do. Listed records and assertion decisions assert nothing and are the
//! same in both modes.
//!
//! Nothing is written and no fact, tier or review changes (R-INT-1,
//! R-INT-3).

use context_hub::curated::{
    AssertionDecisionSummary, CuratedCounts, CuratedProjection, CuratedRecord, CuratedView,
};
use serde::Serialize;
use spec::{CuratedFailure, ExportMode};

use crate::CuratedInput;

/// One accepted record with its DR-4 fields and the sentences for them.
#[derive(Debug, Serialize)]
pub(crate) struct CuratedRow {
    #[serde(flatten)]
    record: CuratedRecord,
    /// What `reason` means, as `curated.md` states it.
    reason_sentence: &'static str,
    /// The SPEC-09 dimensions of `basis`, as `curated.md` prints them.
    basis_summary: String,
}

impl CuratedRow {
    fn of(record: &CuratedRecord) -> Self {
        Self {
            reason_sentence: spec::curated_reason_sentence(record.reason),
            basis_summary: spec::curated_basis_summary(&record.basis),
            record: record.clone(),
        }
    }
}

/// What the Workbench reads of the curated projection.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum CuratedWorkbenchRead {
    /// The projection, with the items this export mode shows.
    Computed {
        export_mode: ExportMode,
        view: CuratedView,
        revision: String,
        graph_basis: String,
        items: Vec<CuratedRow>,
        /// Curated items the export mode leaves out.
        items_omitted: usize,
        listed: Vec<CuratedRow>,
        assertion_decisions: AssertionDecisionSummary,
        counts: CuratedCounts,
    },
    /// It was not computed. `reason` (and `limit`, where the reason has one)
    /// is the failure as the export serializes it; `sentence` is the one
    /// `curated.md` states.
    NotComputed {
        export_mode: ExportMode,
        #[serde(flatten)]
        failure: CuratedFailure,
        sentence: String,
    },
}

impl CuratedWorkbenchRead {
    fn computed(projection: &CuratedProjection, mode: ExportMode) -> Self {
        let shown = match mode {
            ExportMode::BestEffort => projection.items.as_slice(),
            // R-INT-5: no InferredWeak item, and the number left out.
            ExportMode::VerifiedOnly => &[],
        };
        Self::Computed {
            export_mode: mode,
            view: projection.view,
            revision: projection.revision.clone(),
            graph_basis: projection.graph_basis.clone(),
            items: shown.iter().map(CuratedRow::of).collect(),
            items_omitted: projection.items.len() - shown.len(),
            listed: projection.listed.iter().map(CuratedRow::of).collect(),
            assertion_decisions: projection.assertion_decisions.clone(),
            counts: projection.counts,
        }
    }
}

impl CuratedWorkbenchRead {
    /// The read of the projection one compilation computed. `None` when
    /// that compilation did not read the projection at all.
    pub(crate) fn of(curated: &CuratedInput, mode: ExportMode) -> Option<Self> {
        match curated {
            CuratedInput::Computed(projection) => Some(Self::computed(projection, mode)),
            CuratedInput::NotComputed(failure) => Some(Self::NotComputed {
                export_mode: mode,
                failure: *failure,
                sentence: failure.sentence(),
            }),
            CuratedInput::NotRead => None,
        }
    }
}
