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
    /// @29 (AC-1431): flagged top-level limitations. Absent for older roles,
    /// so their saved markers keep their exact bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flagged_limitations: Vec<usize>,
}

/// The Finish shape's top-level limitation bound, not a new policy.
pub const MAX_LIMITATIONS: usize = 8;

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
        Self::from_refusal_for(record, finding_count, false)
    }

    /// As [`Self::from_refusal`]. With `limitations` (@29 only), a location
    /// wholly inside top-level limitations flags those limitations instead of
    /// sending the model back to a whole-answer rewrite.
    pub fn from_refusal_for(
        record: &RefusalRecord,
        finding_count: usize,
        limitations: bool,
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
        let mut flagged_limitations = BTreeSet::new();
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
            let limitation = |field: &ProseField| match field {
                ProseField::Limitation { limitation } => Some(*limitation as usize),
                _ => None,
            };
            if limitations
                && let (Some(first), Some(last)) = (
                    limitation(&location.start.field),
                    limitation(&location.end.field),
                )
            {
                if first > last || last >= MAX_LIMITATIONS {
                    return Err(PatchRejection::InvalidIndex);
                }
                flagged_limitations.extend(first..=last);
                continue;
            }
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
            flagged_limitations: flagged_limitations.into_iter().collect(),
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
        let mut variants = Vec::new();
        if !self.flagged_indices.is_empty() {
            variants.push(
                serde_json::json!({"type":"object","additionalProperties":false,
                "required":["finding_index","replace"],
                "properties":{"finding_index":indices,"replace":{"$ref":"#/$defs/finding"}}}),
            );
            variants.push(
                serde_json::json!({"type":"object","additionalProperties":false,
                "required":["finding_index","withdraw"],
                "properties":{"finding_index":indices,"withdraw":{"const":true}}}),
            );
        }
        if !self.flagged_limitations.is_empty() {
            let limitation =
                ordinary["$defs"]["supported_action"]["properties"]["limitations"]["items"].clone();
            let indices = serde_json::json!({"enum":self.flagged_limitations});
            variants.push(
                serde_json::json!({"type":"object","additionalProperties":false,
                "required":["limitation_index","replace"],
                "properties":{"limitation_index":indices,"replace":limitation}}),
            );
            variants.push(
                serde_json::json!({"type":"object","additionalProperties":false,
                "required":["limitation_index","withdraw"],
                "properties":{"limitation_index":indices,"withdraw":{"const":true}}}),
            );
        }
        let schema = if self.flagged_limitations.is_empty() {
            // Older roles keep their exact schema bytes.
            serde_json::json!({
                "$defs":{"finding":finding},
                "type":"object", "additionalProperties":false,
                "required":["type","patches"],
                "properties":{
                    "type":{"const":"finish_patch"},
                    "patches":{"type":"array","minItems":0,"maxItems":self.flagged_indices.len(),
                        "items":{"oneOf":variants}}
                }
            })
        } else {
            serde_json::json!({
                "$defs":{"finding":finding},
                "type":"object", "additionalProperties":false,
                "required":["type","patches"],
                "properties":{
                    "type":{"const":"finish_patch"},
                    "patches":{"type":"array","minItems":0,
                        "maxItems":self.flagged_indices.len() + self.flagged_limitations.len(),
                        "items":{"oneOf":variants}}
                }
            })
        };
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
            || (self.flagged_indices.is_empty() && self.flagged_limitations.is_empty())
            || self.flagged_indices.iter().any(|index| *index >= self.finding_count)
            || self.flagged_indices.windows(2).any(|pair| pair[0] >= pair[1])
            || self.flagged_limitations.iter().any(|index| *index >= MAX_LIMITATIONS)
            || self.flagged_limitations.windows(2).any(|pair| pair[0] >= pair[1])
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
    ReplaceLimitation {
        limitation_index: usize,
        replace: String,
    },
    WithdrawLimitation {
        limitation_index: usize,
        withdraw: bool,
    },
}
impl Edit {
    /// Findings and limitations index separate lists.
    fn key(&self) -> (bool, usize) {
        match self {
            Self::Replace { finding_index, .. } | Self::Withdraw { finding_index, .. } => {
                (false, *finding_index)
            }
            Self::ReplaceLimitation {
                limitation_index, ..
            }
            | Self::WithdrawLimitation {
                limitation_index, ..
            } => (true, *limitation_index),
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
        let InvestigationAction::Finish {
            findings,
            limitations,
            ..
        } = &finish.action
        else {
            return Err(PatchRejection::InvalidShape);
        };
        if findings.len() != basis.finding_count
            || basis
                .flagged_limitations
                .last()
                .is_some_and(|index| *index >= limitations.len())
        {
            return Err(PatchRejection::BindingMismatch);
        }
        Ok(Self { basis, finish })
    }

    /// Consuming self makes both success and failure release the first-candidate
    /// holder. Surviving strings move into the transient merged admission input.
    pub fn merge(self, raw: &str, saved: &PatchBasis) -> Result<MergedFinish, PatchRejection> {
        self.merge_with(raw, saved, false)
    }

    /// As [`Self::merge`]. With `whole` (@29 only), a reply that is a full
    /// finish rather than a patch is read as one: each flagged finding and
    /// flagged limitation is taken from the same index of that finish, so the
    /// unflagged rows stay exactly as first admitted. Index correspondence
    /// needs the same finding count; otherwise the reply stays InvalidShape.
    pub fn merge_with(
        mut self,
        raw: &str,
        saved: &PatchBasis,
        whole: bool,
    ) -> Result<MergedFinish, PatchRejection> {
        if self.basis != *saved
            || self.finish.fingerprint().ok().as_ref() != Some(&saved.finish_hash)
        {
            return Err(PatchRejection::BindingMismatch);
        }
        strict::preflight(raw, MAX_ACTION_BYTES, 32, 4096, 32)
            .map_err(|_| PatchRejection::InvalidShape)?;
        let original: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| PatchRejection::InvalidShape)?;
        let patches = match serde_json::from_value::<Reply>(original.clone()) {
            Ok(reply)
                if serde_json::to_value(&reply).map_err(|_| PatchRejection::InvalidShape)?
                    == original =>
            {
                let Reply::FinishPatch { patches } = reply;
                patches
            }
            _ if whole => whole_finish_edits(&original, saved)?,
            _ => return Err(PatchRejection::InvalidShape),
        };
        let patch_entries = patches.len();
        let mut edits = BTreeMap::new();
        let mut limitation_edits = BTreeMap::new();
        for edit in patches {
            let (limitation, index) = edit.key();
            let flagged = if limitation {
                &saved.flagged_limitations
            } else {
                &saved.flagged_indices
            };
            let target = if limitation {
                &mut limitation_edits
            } else {
                &mut edits
            };
            if flagged.binary_search(&index).is_err()
                || matches!(
                    &edit,
                    Edit::Withdraw {
                        withdraw: false,
                        ..
                    } | Edit::WithdrawLimitation {
                        withdraw: false,
                        ..
                    }
                )
                || target.insert(index, edit).is_some()
            {
                return Err(PatchRejection::InvalidIndex);
            }
        }
        let InvestigationAction::Finish {
            findings,
            limitations,
            ..
        } = &mut self.finish.action
        else {
            return Err(PatchRejection::InvalidShape);
        };
        if !limitation_edits.is_empty() {
            let mut kept = Vec::new();
            for (index, text) in std::mem::take(limitations).into_iter().enumerate() {
                match limitation_edits.remove(&index) {
                    Some(Edit::ReplaceLimitation { replace, .. }) => kept.push(replace),
                    Some(_) => {}
                    None => kept.push(text),
                }
            }
            if !limitation_edits.is_empty() {
                return Err(PatchRejection::InvalidIndex);
            }
            *limitations = kept;
        }
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
                Some(_) => counts.withdrawn_count += 1,
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

/// A full finish sent in place of a patch, as edits at the flagged indices.
/// Stringified nested arrays are read as their JSON, as native calls send them.
fn whole_finish_edits(
    original: &serde_json::Value,
    saved: &PatchBasis,
) -> Result<Vec<Edit>, PatchRejection> {
    let field = |name: &str| -> Result<Vec<serde_json::Value>, PatchRejection> {
        let value = &original[name];
        let value = match value.as_str() {
            Some(text) => serde_json::from_str(text).map_err(|_| PatchRejection::InvalidShape)?,
            None => value.clone(),
        };
        serde_json::from_value(value).map_err(|_| PatchRejection::InvalidShape)
    };
    let findings = field("findings")?;
    let limitations = field("limitations")?;
    if !matches!(original["type"].as_str(), Some("finish" | "finish_patch"))
        || findings.len() != saved.finding_count
    {
        return Err(PatchRejection::InvalidShape);
    }
    let mut edits = Vec::new();
    for &index in &saved.flagged_indices {
        edits.push(Edit::Replace {
            finding_index: index,
            replace: serde_json::from_value(findings[index].clone())
                .map_err(|_| PatchRejection::InvalidShape)?,
        });
    }
    for &index in &saved.flagged_limitations {
        edits.push(match limitations.get(index) {
            Some(text) => Edit::ReplaceLimitation {
                limitation_index: index,
                replace: text
                    .as_str()
                    .ok_or(PatchRejection::InvalidShape)?
                    .to_owned(),
            },
            None => Edit::WithdrawLimitation {
                limitation_index: index,
                withdraw: true,
            },
        });
    }
    Ok(edits)
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
