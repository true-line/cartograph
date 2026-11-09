//! @27 correction primitives. This module never admits or persists result text.
//! The host authenticates the saved refusal before constructing a basis.
use super::{staged_finish::*, staged_refusal::RefusalRecord, *};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Safe to persist: only identities, indices and counts, never candidate prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchBasis {
    pub finish_hash: String,
    pub refusal_hash: String,
    pub finding_count: usize,
    pub flagged_indices: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchRejection {
    InvalidShape,
    InvalidIndex,
    BindingMismatch,
    CandidateUnavailable,
    EmptyResult,
    Capacity,
    UnsupportedRefusal,
    AdmissionRejected,
}

/// Text-free measurements only; this is not an admission verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchCounts {
    pub finding_count: usize,
    pub flagged_count: usize,
    pub patch_entries: usize,
    pub patched_count: usize,
    pub withdrawn_count: usize,
    pub kept_count: usize,
}

impl PatchBasis {
    /// Caller must authenticate `record` against saved invocation witnesses.
    pub fn from_refusal(
        record: &RefusalRecord,
        finding_count: usize,
    ) -> Result<Self, PatchRejection> {
        if record.refusal_count != 1
            || record.refusal_id
                != record
                    .content_id()
                    .map_err(|_| PatchRejection::BindingMismatch)?
            || !crate::source_basis::hash(&record.finish_hash)
            || record.feedback.schema_version != 6
            || !record.feedback.unmet.is_empty()
        {
            return Err(PatchRejection::UnsupportedRefusal);
        }
        let Some(ProseCorrection::Combined(corrections)) = &record.feedback.prose_correction else {
            return Err(PatchRejection::UnsupportedRefusal);
        };
        if corrections.omitted != 0 {
            return Err(PatchRejection::UnsupportedRefusal);
        }
        let mut flagged = BTreeSet::new();
        for entry in &corrections.entries {
            let location = match entry {
                CombinedCorrection::ReplayAt(location)
                | CombinedCorrection::EnumerationAt(location)
                | CombinedCorrection::ConfigurationAt(location) => location,
                // No finding index is authenticated by a whole-answer message.
                _ => return Err(PatchRejection::UnsupportedRefusal),
            };
            location
                .validate()
                .map_err(|_| PatchRejection::InvalidIndex)?;
            let index = |field: &ProseField| match field {
                ProseField::FindingTitle { finding }
                | ProseField::FindingStatement { finding }
                | ProseField::FindingLimitation { finding, .. } => Some(*finding as usize),
                _ => None,
            };
            let first = index(&location.start.field).ok_or(PatchRejection::UnsupportedRefusal)?;
            let last = index(&location.end.field).ok_or(PatchRejection::UnsupportedRefusal)?;
            if first > last || last >= finding_count {
                return Err(PatchRejection::InvalidIndex);
            }
            flagged.extend(first..=last);
        }
        let basis = Self {
            finish_hash: record.finish_hash.clone(),
            refusal_hash: record.refusal_id.clone(),
            finding_count,
            flagged_indices: flagged.into_iter().collect(),
        };
        basis.validate()?;
        Ok(basis)
    }

    /// Tool-free correction schema. Finding field bounds come from the compiled
    /// ordinary finish schema; index eligibility comes only from saved feedback.
    pub fn response_schema(
        &self,
        ledger: &InvestigationInputLedger,
    ) -> Result<String, PatchRejection> {
        self.validate()?;
        let ordinary: serde_json::Value = serde_json::from_str(super::response_schema::FINISH)
            .map_err(|_| PatchRejection::InvalidShape)?;
        let mut finding = ordinary["$defs"]["finding"].clone();
        let citations: Vec<_> = ledger.citations.iter().map(|c| &c.citation_id).collect();
        finding["properties"]["citation_ids"]["items"] = serde_json::json!({"enum":citations});
        let indices = serde_json::json!({"enum":self.flagged_indices});
        let schema = serde_json::json!({
            "$defs":{"finding":finding},
            "type":"object", "additionalProperties":false,
            "required":["type","patches"],
            "properties":{
                "type":{"const":"finish_patch"},
                "patches":{"type":"array","minItems":0,"maxItems":self.flagged_indices.len(),
                    "items":{"oneOf":[
                        {"type":"object","additionalProperties":false,
                         "required":["finding_index","replace"],
                         "properties":{"finding_index":indices,"replace":{"$ref":"#/$defs/finding"}}},
                        {"type":"object","additionalProperties":false,
                         "required":["finding_index","withdraw"],
                         "properties":{"finding_index":indices,"withdraw":{"const":true}}}
                    ]}}
            }
        });
        String::from_utf8(
            bounded_bytes(&schema, MAX_INPUT_BYTES).map_err(|_| PatchRejection::Capacity)?,
        )
        .map_err(|_| PatchRejection::InvalidShape)
    }

    pub fn validate(&self) -> Result<(), PatchRejection> {
        if !crate::source_basis::hash(&self.finish_hash)
            || !crate::source_basis::hash(&self.refusal_hash)
            || self.finding_count == 0
            || self.finding_count > 12 // existing Finish shape, not a new policy
            || self.flagged_indices.is_empty()
            || self.flagged_indices.iter().any(|index| *index >= self.finding_count)
            || self.flagged_indices.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(PatchRejection::InvalidIndex);
        }
        Ok(())
    }
}

// Deliberately no Debug/Clone/Serialize: the holder is consumed once. Scoped
// Drop releases allocations; they are released, not securely erased.
pub struct VolatileFinish {
    basis: PatchBasis,
    finish: StagedFinish,
}

// Neither provider patch prose nor a host merged candidate has a diagnostic
// formatting surface here. Only PatchRejection may cross error boundaries.
#[derive(Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Edit {
    Replace {
        finding_index: usize,
        replace: ProposedInvestigationFinding,
    },
    Withdraw {
        finding_index: usize,
        withdraw: bool,
    },
}
impl Edit {
    fn index(&self) -> usize {
        match self {
            Self::Replace { finding_index, .. } | Self::Withdraw { finding_index, .. } => {
                *finding_index
            }
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    FinishPatch { patches: Vec<Edit> },
}

/// Still untrusted. The host must run full privacy/admission and withholding.
/// In particular, a successful merge cannot authorize persistence by itself.
pub struct MergedFinish {
    pub finish: StagedFinish,
    pub counts: PatchCounts,
    /// Indices in the merged order that came from first-refusal flagged rows.
    pub flagged_merged_indices: Vec<usize>,
}

impl VolatileFinish {
    /// Text-free identity for pre-call comparison; exposes no candidate prose.
    pub fn basis(&self) -> &PatchBasis {
        &self.basis
    }

    pub fn capture(finish: StagedFinish, basis: PatchBasis) -> Result<Self, PatchRejection> {
        basis.validate()?;
        if finish.schema_version != 1
            || finish.narrative.is_some()
            || finish.fingerprint().map_err(|_| PatchRejection::Capacity)? != basis.finish_hash
        {
            return Err(PatchRejection::BindingMismatch);
        }
        let InvestigationAction::Finish { findings, .. } = &finish.action else {
            return Err(PatchRejection::InvalidShape);
        };
        if findings.len() != basis.finding_count {
            return Err(PatchRejection::BindingMismatch);
        }
        Ok(Self { basis, finish })
    }

    /// Consuming self makes both success and failure release the first-candidate
    /// holder. Surviving strings move into the transient merged admission input.
    pub fn merge(mut self, raw: &str, saved: &PatchBasis) -> Result<MergedFinish, PatchRejection> {
        if self.basis != *saved
            || self.finish.fingerprint().ok().as_ref() != Some(&saved.finish_hash)
        {
            return Err(PatchRejection::BindingMismatch);
        }
        strict::preflight(raw, MAX_ACTION_BYTES, 32, 4096, 32)
            .map_err(|_| PatchRejection::InvalidShape)?;
        let reply: Reply = serde_json::from_str(raw).map_err(|_| PatchRejection::InvalidShape)?;
        let original: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| PatchRejection::InvalidShape)?;
        if serde_json::to_value(&reply).map_err(|_| PatchRejection::InvalidShape)? != original {
            return Err(PatchRejection::InvalidShape);
        }
        let Reply::FinishPatch { patches } = reply;
        let patch_entries = patches.len();
        let mut edits = BTreeMap::new();
        for edit in patches {
            let index = edit.index();
            if saved.flagged_indices.binary_search(&index).is_err()
                || matches!(
                    &edit,
                    Edit::Withdraw {
                        withdraw: false,
                        ..
                    }
                )
                || edits.insert(index, edit).is_some()
            {
                return Err(PatchRejection::InvalidIndex);
            }
        }
        let InvestigationAction::Finish { findings, .. } = &mut self.finish.action else {
            return Err(PatchRejection::InvalidShape);
        };
        let mut counts = PatchCounts {
            finding_count: 0,
            flagged_count: saved.flagged_indices.len(),
            patch_entries,
            patched_count: 0,
            withdrawn_count: 0,
            kept_count: 0,
        };
        let mut flagged_merged_indices = Vec::new();
        for (index, finding) in std::mem::take(findings).into_iter().enumerate() {
            let retained_index = findings.len();
            match edits.remove(&index) {
                Some(Edit::Replace { replace, .. }) => {
                    findings.push(replace);
                    counts.patched_count += 1;
                }
                Some(Edit::Withdraw { .. }) => counts.withdrawn_count += 1,
                None => {
                    findings.push(finding);
                    counts.kept_count += 1;
                }
            }
            if findings.len() > retained_index
                && saved.flagged_indices.binary_search(&index).is_ok()
            {
                flagged_merged_indices.push(retained_index);
            }
        }
        counts.finding_count = findings.len();
        if findings.is_empty() {
            return Err(PatchRejection::EmptyResult);
        }
        // Applies the original finding, field and complete finish byte bounds.
        self.finish
            .fingerprint()
            .map_err(|_| PatchRejection::InvalidShape)?;
        Ok(MergedFinish {
            finish: self.finish,
            counts,
            flagged_merged_indices,
        })
    }
}

/// Extra live ownership during correction; add outside the provider/admission
/// phase maxima. This is an existing-cap-derived bound, not a new ceiling.
pub fn workspace_bytes() -> Option<usize> {
    use context_hub::logical::{json_bytes, map, slots, sum};
    sum(&[
        json_bytes(MAX_ACTION_BYTES)?.checked_mul(4)?, // held, patch, original Value, merged
        MAX_ACTION_BYTES.checked_mul(2)?,              // canonical encoding and validation scratch
        slots::<PatchBasis>(2)?,
        slots::<usize>(12)?.checked_mul(4)?,
        map::<usize, Edit>(12)?,
        4 * 64, // two SHA-256 identities in each of two basis owners
    ])
}

#[cfg(test)]
#[path = "finish_patch_tests.rs"]
mod tests;
