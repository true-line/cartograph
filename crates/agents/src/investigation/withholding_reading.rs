//! How a child reads a withheld parent's saved findings (#598, AC-1336).
//!
//! A withholding witness numbers positions in the original finish from 0,
//! while the saved findings are renumbered `finding-1`.. in survivor order.
//! On the #598 frontier auditor attempt 6 the child read `withheld_indexes:
//! [2]` beside a saved `finding-3` and took that survivor for the withheld
//! finding. The parent history therefore names each saved finding's original
//! position and says the withheld ones are absent. The host derives it from
//! the saved result, and a supplied reading must equal that derivation.
use super::*;
use serde::{Deserialize, Serialize};

/// The reading's fixed explanation.
pub const WITHHOLDING_READING_NOTE: &str = "Positions count the original finish's findings \
from 0. The withheld findings are not in saved_result.findings; every saved finding is a \
survivor, renumbered from finding-1 in original order. A finding id never names an original \
position.";

/// The parent history's reading of a withheld parent's findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WithholdingReading {
    /// [`WITHHOLDING_READING_NOTE`].
    pub note: String,
    /// The withheld findings' positions in the original finish.
    pub withheld_original_positions: Vec<u32>,
    /// Each saved finding with its position in the original finish.
    pub saved_findings: Vec<SavedFindingPosition>,
}

/// One saved finding and its position in the original finish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedFindingPosition {
    /// The saved finding's id.
    pub finding_id: String,
    /// Its position in the original finish, from 0.
    pub original_position: u32,
}

impl WithholdingReading {
    /// The reading for `result`, or `None` when nothing was withheld.
    pub fn of(result: &InvestigationResult) -> Option<Self> {
        let witness = result.completion.as_ref()?.withholding.as_ref()?;
        Some(Self {
            note: WITHHOLDING_READING_NOTE.to_owned(),
            withheld_original_positions: witness.withheld_indexes.clone(),
            saved_findings: result
                .findings
                .iter()
                .zip(&witness.survivor_indexes)
                .map(|(finding, position)| SavedFindingPosition {
                    finding_id: finding.finding_id.clone(),
                    original_position: *position,
                })
                .collect(),
        })
    }
}
