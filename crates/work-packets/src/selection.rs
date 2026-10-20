//! Typed references selected by the browser, resolved authoritatively by the host.

use core_graph::source::FactKey;
use serde::{Deserialize, Serialize};
use spec::{ExportMode, SpecArtifact};
use std::collections::BTreeMap;

/// Exact complete-fact selection, never a browser-supplied graph body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactSelection {
    /// Typed node or directed edge identity.
    pub reference: FactKey,
    /// Digest of every original fact field and property.
    pub expected_digest: String,
}

/// Requested full artifact identity; this does not establish verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSelection {
    /// Native compiler artifact identifier.
    pub id: String,
    /// Expected versioned native schema.
    pub schema: String,
    /// Expected complete content identity, including the assertion tail.
    pub content_id: String,
    /// Projection under which this artifact was selected.
    pub mode: ExportMode,
}

/// Exact historical review record selection; it confers no review authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSelection {
    /// Existing assertion content hash, used as the decision-log lookup key.
    pub content_hash: String,
    /// Expected hash of the complete selected decision record.
    pub expected_record_hash: String,
}

/// Selection component of a generation request, after bounded JSON admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSelection {
    /// Exact recovered snapshot selected by the user.
    pub expected_snapshot_id: String,
    /// At most 64 raw selections, before normalization.
    pub facts: Vec<FactSelection>,
    /// At most four raw selections, before normalization.
    pub artifacts: Vec<ArtifactSelection>,
    /// At most 64 raw selections, before normalization.
    pub interpretation_decisions: Vec<DecisionSelection>,
}

/// Invalid or conflicting selections are rejected before source access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SelectionError {
    /// Malformed identity, digest, schema, or an empty evidence selection.
    #[error("invalid evidence selection")]
    InvalidInput,
    /// Raw selection count exceeds the fixed generation contract.
    #[error("evidence selection limit exceeded")]
    LimitExceeded,
    /// One typed key was supplied with incompatible expected content.
    #[error("conflicting evidence selections")]
    SelectionConflict,
}

fn hex_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(hex_hash)
}

fn text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum
}

fn validate_fact(selection: &FactSelection) -> bool {
    match &selection.reference {
        FactKey::Node { id } => text(id, 8192) && digest(&selection.expected_digest, "node-v1:"),
        FactKey::Edge {
            source,
            label,
            destination,
        } => {
            text(source, 8192)
                && text(label, 256)
                && text(destination, 8192)
                && digest(&selection.expected_digest, "edge-v1:")
        }
    }
}

fn insert<K: Ord, V: PartialEq>(
    values: &mut BTreeMap<K, V>,
    key: K,
    value: V,
) -> Result<(), SelectionError> {
    use std::collections::btree_map::Entry;
    match values.entry(key) {
        Entry::Vacant(entry) => {
            entry.insert(value);
            Ok(())
        }
        Entry::Occupied(entry) if entry.get() == &value => Ok(()),
        Entry::Occupied(_) => Err(SelectionError::SelectionConflict),
    }
}

impl EvidenceSelection {
    /// Validate identities and normalize only declared set-valued selections.
    ///
    /// Count bounds apply before duplicate removal. This is not graph/receipt
    /// verification: the host must compare the normalized request with one
    /// coherent authoritative observation before retaining any evidence.
    pub fn normalize(self) -> Result<Self, SelectionError> {
        if self.facts.len() > 64
            || self.artifacts.len() > 4
            || self.interpretation_decisions.len() > 64
        {
            return Err(SelectionError::LimitExceeded);
        }
        if !digest(&self.expected_snapshot_id, "context-v1:")
            || (self.facts.is_empty() && self.artifacts.is_empty())
        {
            return Err(SelectionError::InvalidInput);
        }
        let mut facts = BTreeMap::new();
        for selected in self.facts {
            if !validate_fact(&selected) {
                return Err(SelectionError::InvalidInput);
            }
            insert(&mut facts, selected.reference.clone(), selected)?;
        }
        let mut artifacts = BTreeMap::new();
        for selected in self.artifacts {
            if !text(&selected.id, 128)
                || selected.schema != SpecArtifact::expected_schema(&selected.id)
                || !digest(&selected.content_id, "spec-artifact-v1:")
            {
                return Err(SelectionError::InvalidInput);
            }
            let mode = match selected.mode {
                ExportMode::VerifiedOnly => 0,
                ExportMode::BestEffort => 1,
            };
            insert(&mut artifacts, (selected.id.clone(), mode), selected)?;
        }
        let mut decisions = BTreeMap::new();
        for selected in self.interpretation_decisions {
            if !hex_hash(&selected.content_hash) || !hex_hash(&selected.expected_record_hash) {
                return Err(SelectionError::InvalidInput);
            }
            insert(&mut decisions, selected.content_hash.clone(), selected)?;
        }
        Ok(Self {
            expected_snapshot_id: self.expected_snapshot_id,
            facts: facts.into_values().collect(),
            artifacts: artifacts.into_values().collect(),
            interpretation_decisions: decisions.into_values().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selection() -> EvidenceSelection {
        EvidenceSelection {
            expected_snapshot_id: format!("context-v1:{}", "a".repeat(64)),
            facts: vec![FactSelection {
                reference: FactKey::Node {
                    id: "node:a b".into(),
                },
                expected_digest: format!("node-v1:{}", "b".repeat(64)),
            }],
            artifacts: vec![],
            interpretation_decisions: vec![],
        }
    }

    #[test]
    fn normalize_preserves_typed_keys_and_rejects_conflicting_content() {
        let mut request = selection();
        request.facts.push(request.facts[0].clone());
        request.facts.push(FactSelection {
            reference: FactKey::Edge {
                source: "node:a".into(),
                label: "b".into(),
                destination: "c".into(),
            },
            expected_digest: format!("edge-v1:{}", "c".repeat(64)),
        });
        let normalized = request.clone().normalize().unwrap();
        assert_eq!(normalized.facts.len(), 2);
        request.facts.reverse();
        assert_eq!(request.normalize().unwrap(), normalized);
        let mut conflicting = normalized.clone();
        let mut changed = conflicting.facts[0].clone();
        changed.expected_digest = format!("node-v1:{}", "d".repeat(64));
        conflicting.facts.push(changed);
        assert_eq!(
            conflicting.normalize(),
            Err(SelectionError::SelectionConflict)
        );
        let mut oversized = normalized;
        oversized.facts = vec![oversized.facts[0].clone(); 65];
        assert_eq!(oversized.normalize(), Err(SelectionError::LimitExceeded));
    }

    #[test]
    fn artifact_modes_remain_distinct_and_decision_record_conflicts_fail() {
        let mut request = selection();
        let selected = ArtifactSelection {
            id: "topology".into(),
            schema: SpecArtifact::expected_schema("topology"),
            content_id: format!("spec-artifact-v1:{}", "e".repeat(64)),
            mode: ExportMode::VerifiedOnly,
        };
        request.artifacts = vec![selected.clone(), selected];
        request.artifacts[1].mode = ExportMode::BestEffort;
        assert_eq!(request.clone().normalize().unwrap().artifacts.len(), 2);
        let decision = DecisionSelection {
            content_hash: "a".repeat(64),
            expected_record_hash: "b".repeat(64),
        };
        request.interpretation_decisions = vec![decision.clone(), decision];
        request.interpretation_decisions[1].expected_record_hash = "c".repeat(64);
        assert_eq!(request.normalize(), Err(SelectionError::SelectionConflict));
    }

    #[test]
    fn wrong_digest_kind_unknown_fields_and_history_only_requests_are_rejected() {
        let mut request = selection();
        request.facts[0].expected_digest = format!("edge-v1:{}", "b".repeat(64));
        assert_eq!(request.normalize(), Err(SelectionError::InvalidInput));
        let mut value = serde_json::to_value(selection()).unwrap();
        value["facts"][0]["fact"] = serde_json::json!({"props": {"confirmed": true}});
        assert!(serde_json::from_value::<EvidenceSelection>(value).is_err());
        let mut request = selection();
        request.facts.clear();
        request.interpretation_decisions.push(DecisionSelection {
            content_hash: "a".repeat(64),
            expected_record_hash: "b".repeat(64),
        });
        assert_eq!(request.normalize(), Err(SelectionError::InvalidInput));
    }
}
