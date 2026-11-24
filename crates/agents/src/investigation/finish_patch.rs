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
    /// @29 (AC-1430): obligations refused only as excluded_by_capacity. The
    /// patch discloses each in a new top-level limitation the host prefixes
    /// with its [`capacity_name`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capacity_gaps: Vec<String>,
}

/// The Finish shape's top-level limitation bound, not a new policy.
pub const MAX_LIMITATIONS: usize = 8;
/// `capacity_name` plus ": " before a disclosure's text.
const DISCLOSURE_PREFIX: usize = CAPACITY_NAME_CHARS + 2;

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
    /// @29 (AC-1434): new limitation prose claims a finding was withdrawn or
    /// replaced when the merge did not do so.
    UnsupportedEditClaim,
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

    /// The basis a role's refusal yields. A role whose capacity gaps the host
    /// discloses (AC-1444) asks the model for none, and a refusal that flags
    /// nothing else needs a whole answer.
    pub fn from_refusal_for_role(
        record: &RefusalRecord,
        finding_count: usize,
        role: SpecialistId,
    ) -> Result<Self, PatchRejection> {
        let mut basis = Self::from_refusal_for(record, finding_count, role.patches_whole_finish())?;
        if role.uses_host_capacity_disclosure() {
            basis.capacity_gaps.clear();
            if basis.flagged_indices.is_empty() && basis.flagged_limitations.is_empty() {
                return Err(PatchRejection::UnsupportedRefusal);
            }
        }
        Ok(basis)
    }

    /// As [`Self::from_refusal`]. With `limitations` (@29 only), a location
    /// wholly inside top-level limitations flags those limitations instead of
    /// sending the model back to a whole-answer rewrite.
    pub fn from_refusal_for(
        record: &RefusalRecord,
        finding_count: usize,
        limitations: bool,
    ) -> Result<Self, PatchRejection> {
        // @29 alone may carry capacity exclusions into a patch; any other
        // unmet reason still needs a whole answer.
        let capacity_gaps: Vec<String> = if limitations
            && record
                .feedback
                .unmet
                .iter()
                .all(|entry| entry.reason == RefusalReason::ExcludedByCapacity)
        {
            record
                .feedback
                .unmet
                .iter()
                .map(|entry| entry.obligation_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        } else if record.feedback.unmet.is_empty() {
            Vec::new()
        } else {
            return Err(PatchRejection::UnsupportedRefusal);
        };
        if !distinct_names(&capacity_gaps) {
            // Gaps the 12-char name cannot tell apart need a whole answer.
            return Err(PatchRejection::UnsupportedRefusal);
        }
        if record.refusal_count != 1
            || record.refusal_id
                != record
                    .content_id()
                    .map_err(|_| PatchRejection::BindingMismatch)?
            || !crate::source_basis::hash(&record.finish_hash)
            || record.feedback.citation_correction.is_some()
        {
            return Err(PatchRejection::UnsupportedRefusal);
        }
        let entries = match &record.feedback.prose_correction {
            Some(ProseCorrection::Combined(corrections))
                if record.feedback.schema_version == 6 && corrections.omitted == 0 =>
            {
                corrections.entries.as_slice()
            }
            None if !capacity_gaps.is_empty() => &[],
            _ => return Err(PatchRejection::UnsupportedRefusal),
        };
        let mut flagged = BTreeSet::new();
        let mut flagged_limitations = BTreeSet::new();
        for entry in entries {
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
            capacity_gaps,
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
        if self.addresses_every_row() {
            return self.row_schema(finding, &ordinary);
        }
        let indices = serde_json::json!({"enum":self.flagged_indices});
        // Older roles keep their exact schema bytes.
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

    /// @29 (AC-1431): with a flagged limitation or a capacity gap the patch
    /// names every flagged row and every gap; nothing flagged can be skipped.
    pub fn addresses_every_row(&self) -> bool {
        !self.flagged_limitations.is_empty() || !self.capacity_gaps.is_empty()
    }

    fn row_schema(
        &self,
        finding: serde_json::Value,
        ordinary: &serde_json::Value,
    ) -> Result<String, PatchRejection> {
        use serde_json::{Map, Value, json};
        let text =
            ordinary["$defs"]["supported_action"]["properties"]["limitations"]["items"].clone();
        let withdraw = json!({"type":"object","additionalProperties":false,
            "required":["withdraw"],"properties":{"withdraw":{"const":true}}});
        let rows = |keys: Vec<String>, replace: Value| {
            let mut properties = Map::new();
            for key in &keys {
                properties.insert(
                    key.clone(),
                    json!({"oneOf":[
                    {"type":"object","additionalProperties":false,
                     "required":["replace"],"properties":{"replace":replace}},
                    withdraw]}),
                );
            }
            json!({"type":"object","additionalProperties":false,"required":keys,"properties":properties})
        };
        let mut properties = Map::new();
        let mut required = vec![json!("type")];
        properties.insert("type".into(), json!({"const":"finish_patch"}));
        if !self.flagged_indices.is_empty() {
            let keys = self.flagged_indices.iter().map(usize::to_string).collect();
            properties.insert(
                "findings".into(),
                rows(keys, json!({"$ref":"#/$defs/finding"})),
            );
            required.push(json!("findings"));
        }
        if !self.flagged_limitations.is_empty() || !self.capacity_gaps.is_empty() {
            let keys = self
                .flagged_limitations
                .iter()
                .map(usize::to_string)
                .collect();
            let mut group = rows(keys, text.clone());
            if !self.capacity_gaps.is_empty() {
                // Optional: withdraw an unflagged limitation to make room.
                for index in (0..MAX_LIMITATIONS)
                    .filter(|index| self.flagged_limitations.binary_search(index).is_err())
                {
                    group["properties"][index.to_string()] = withdraw.clone();
                }
            }
            properties.insert("limitations".into(), group);
            if !self.flagged_limitations.is_empty() {
                required.push(json!("limitations"));
            }
        }
        if !self.capacity_gaps.is_empty() {
            let mut disclosure = text;
            let room = disclosure["maxLength"].as_u64().unwrap_or(512) - DISCLOSURE_PREFIX as u64;
            disclosure["maxLength"] = json!(room);
            let keys: Vec<String> = self
                .capacity_gaps
                .iter()
                .map(|id| capacity_name(id).to_owned())
                .collect();
            let gaps: Map<String, Value> = keys
                .iter()
                .map(|key| (key.clone(), disclosure.clone()))
                .collect();
            properties.insert(
                "disclosures".into(),
                json!({"type":"object",
                "additionalProperties":false,"required":keys,"properties":gaps}),
            );
            required.push(json!("disclosures"));
        }
        let schema = json!({"$defs":{"finding":finding},"type":"object",
            "additionalProperties":false,"required":required,"properties":properties});
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
            || (self.flagged_indices.is_empty()
                && self.flagged_limitations.is_empty()
                && self.capacity_gaps.is_empty())
            || self.capacity_gaps.len() > MAX_LIMITATIONS
            || self.capacity_gaps.iter().any(|id| !crate::source_basis::hash(id))
            || self.capacity_gaps.windows(2).any(|pair| pair[0] >= pair[1])
            || !distinct_names(&self.capacity_gaps)
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

    /// @29 (#2431): the rejection [`Self::merge_with`] would return, while the
    /// holder stays. A rejected patch can then go back to the model within the
    /// run's `invalid_action` allowance. The merge runs on a transient copy of
    /// the candidate, which is dropped (released, not securely erased).
    pub fn rejection(&self, raw: &str, saved: &PatchBasis, whole: bool) -> Option<PatchRejection> {
        Self {
            basis: self.basis.clone(),
            finish: self.finish.clone(),
        }
        .merge_with(raw, saved, whole)
        .err()
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
        let (patches, disclosures) = if saved.addresses_every_row() {
            match row_edits(&original, saved) {
                Ok(edits) => edits,
                Err(_) if whole => whole_finish_edits(&original, saved)?,
                Err(error) => return Err(error),
            }
        } else {
            match serde_json::from_value::<Reply>(original.clone()) {
                Ok(reply)
                    if serde_json::to_value(&reply)
                        .map_err(|_| PatchRejection::InvalidShape)?
                        == original =>
                {
                    let Reply::FinishPatch { patches } = reply;
                    (patches, Vec::new())
                }
                _ if whole => whole_finish_edits(&original, saved)?,
                _ => return Err(PatchRejection::InvalidShape),
            }
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
            // With capacity gaps an unflagged limitation may be withdrawn to
            // make room for the disclosures.
            let room = limitation
                && !saved.capacity_gaps.is_empty()
                && index < MAX_LIMITATIONS
                && matches!(edit, Edit::WithdrawLimitation { withdraw: true, .. });
            if (flagged.binary_search(&index).is_err() && !room)
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
        // Rows holding new limitation prose (replacements, then disclosures),
        // at most MAX_LIMITATIONS indices.
        let mut new_rows = Vec::new();
        if !limitation_edits.is_empty() {
            let mut kept = Vec::new();
            for (index, text) in std::mem::take(limitations).into_iter().enumerate() {
                match limitation_edits.remove(&index) {
                    Some(Edit::ReplaceLimitation { replace, .. }) => {
                        new_rows.push(kept.len());
                        kept.push(replace);
                    }
                    Some(_) => {}
                    None => kept.push(text),
                }
            }
            // The room schema offers every unflagged slot below the cap; a
            // withdraw of a slot this finish does not have removes nothing.
            if limitation_edits.iter().any(|(index, edit)| {
                saved.flagged_limitations.binary_search(index).is_ok()
                    || !matches!(edit, Edit::WithdrawLimitation { withdraw: true, .. })
            }) {
                return Err(PatchRejection::InvalidIndex);
            }
            *limitations = kept;
        }
        new_rows.extend(limitations.len()..limitations.len() + disclosures.len());
        limitations.extend(disclosures);
        if limitations.len() > MAX_LIMITATIONS {
            return Err(PatchRejection::InvalidShape);
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
        let (mut withdrawn, mut patched) = (BTreeSet::new(), BTreeSet::new());
        for (index, finding) in std::mem::take(findings).into_iter().enumerate() {
            let retained_index = findings.len();
            match edits.remove(&index) {
                Some(Edit::Replace { replace, .. }) => {
                    findings.push(replace);
                    counts.patched_count += 1;
                    patched.insert(index);
                }
                Some(_) => {
                    counts.withdrawn_count += 1;
                    withdrawn.insert(index);
                }
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
        // Like `whole`, @29 only, so earlier roles keep their exact merges.
        if whole {
            // Read in place: the check owns no copy of the prose.
            let texts = new_rows.iter().map(|row| limitations[*row].as_str());
            claims::check(texts, &withdrawn, &patched)?;
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
) -> Result<(Vec<Edit>, Vec<String>), PatchRejection> {
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
    // A gap is disclosed only by a limitation of that finish that names it.
    let mut disclosures = Vec::new();
    for id in &saved.capacity_gaps {
        let name = capacity_name(id);
        let text = limitations
            .iter()
            .filter_map(serde_json::Value::as_str)
            .find(|text| text.contains(name))
            .ok_or(PatchRejection::InvalidShape)?;
        disclosures.push(text.to_owned());
    }
    Ok((edits, disclosures))
}

/// Each gap must have its own 12-char name, or its disclosure is ambiguous.
fn distinct_names(gaps: &[String]) -> bool {
    gaps.iter()
        .map(|id| capacity_name(id))
        .collect::<BTreeSet<_>>()
        .len()
        == gaps.len()
}

/// The @29 row form: every flagged finding and limitation is replaced or
/// withdrawn, and every capacity gap gets a disclosure the host prefixes with
/// its name. Exactly the saved keys; stringified nested objects are read.
fn row_edits(
    original: &serde_json::Value,
    saved: &PatchBasis,
) -> Result<(Vec<Edit>, Vec<String>), PatchRejection> {
    use serde_json::Value;
    let object = |value: &Value| -> Result<serde_json::Map<String, Value>, PatchRejection> {
        let value = match value.as_str() {
            Some(text) => serde_json::from_str(text).map_err(|_| PatchRejection::InvalidShape)?,
            None => value.clone(),
        };
        match value {
            Value::Object(map) => Ok(map),
            _ => Err(PatchRejection::InvalidShape),
        }
    };
    let top = original.as_object().ok_or(PatchRejection::InvalidShape)?;
    let mut expected = vec!["type"];
    if !saved.flagged_indices.is_empty() {
        expected.push("findings");
    }
    // With gaps, limitations may also withdraw unflagged rows to make room.
    let room = !saved.capacity_gaps.is_empty();
    if !saved.flagged_limitations.is_empty() || (room && top.contains_key("limitations")) {
        expected.push("limitations");
    }
    if room {
        expected.push("disclosures");
    }
    if original["type"] != "finish_patch"
        || top.len() != expected.len()
        || expected.iter().any(|key| !top.contains_key(*key))
    {
        return Err(PatchRejection::InvalidShape);
    }
    let rows = |name: &str,
                flagged: &[usize],
                room: bool|
     -> Result<Vec<(usize, Value)>, PatchRejection> {
        if !top.contains_key(name) {
            return Ok(Vec::new());
        }
        let map = object(&original[name])?;
        let mut out = Vec::new();
        for (key, row) in &map {
            let index: usize = key.parse().map_err(|_| PatchRejection::InvalidIndex)?;
            if key != &index.to_string() {
                return Err(PatchRejection::InvalidIndex);
            }
            let row = object(row)?;
            let flagged = flagged.binary_search(&index).is_ok();
            out.push(match (row.len(), row.get("replace"), row.get("withdraw")) {
                (1, Some(replace), None) if flagged => (index, replace.clone()),
                (1, None, Some(Value::Bool(true)))
                    if flagged || (room && index < MAX_LIMITATIONS) =>
                {
                    (index, Value::Null)
                }
                (1, Some(_), None) | (1, None, Some(Value::Bool(true))) => {
                    return Err(PatchRejection::InvalidIndex);
                }
                _ => return Err(PatchRejection::InvalidShape),
            });
        }
        if flagged
            .iter()
            .any(|index| !map.contains_key(&index.to_string()))
        {
            return Err(PatchRejection::InvalidIndex);
        }
        out.sort_by_key(|(index, _)| *index);
        Ok(out)
    };
    let mut edits = Vec::new();
    for (index, replace) in rows("findings", &saved.flagged_indices, false)? {
        edits.push(if replace.is_null() {
            Edit::Withdraw {
                finding_index: index,
                withdraw: true,
            }
        } else {
            let replace = match replace.as_str() {
                Some(text) => {
                    serde_json::from_str(text).map_err(|_| PatchRejection::InvalidShape)?
                }
                None => replace,
            };
            Edit::Replace {
                finding_index: index,
                replace: serde_json::from_value(replace)
                    .map_err(|_| PatchRejection::InvalidShape)?,
            }
        });
    }
    for (index, replace) in rows("limitations", &saved.flagged_limitations, room)? {
        edits.push(match replace {
            Value::Null => Edit::WithdrawLimitation {
                limitation_index: index,
                withdraw: true,
            },
            Value::String(text) => Edit::ReplaceLimitation {
                limitation_index: index,
                replace: text,
            },
            _ => return Err(PatchRejection::InvalidShape),
        });
    }
    let mut disclosures = Vec::new();
    if !saved.capacity_gaps.is_empty() {
        let map = object(&original["disclosures"])?;
        if map.len() != saved.capacity_gaps.len() {
            return Err(PatchRejection::InvalidIndex);
        }
        for id in &saved.capacity_gaps {
            let name = capacity_name(id);
            let text = map
                .get(name)
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .ok_or(PatchRejection::InvalidShape)?;
            disclosures.push(if text.contains(name) {
                text.to_owned()
            } else {
                format!("{name}: {text}")
            });
        }
    }
    Ok((edits, disclosures))
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

#[path = "finish_patch_claims.rs"]
mod claims;

#[cfg(test)]
#[path = "finish_patch_tests.rs"]
mod tests;
