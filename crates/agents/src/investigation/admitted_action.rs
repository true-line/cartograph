//! Bounded retention of the action a task last decoded when a host failure
//! then stopped it (ADR-0076, AC-1405, #1616). The saved response already
//! commits to the action by hash; this record keeps the canonical body that
//! hash names, so the failure can be attributed without a rerun.
use super::{InvestigationError, bounded_bytes};
use serde::{Deserialize, Serialize};

/// Body allowance: larger canonical actions keep only their size and hash.
pub const MAX_ADMITTED_ACTION_BODY_BYTES: usize = 8192;
/// Serialized record allowance, separate from task and store occupancy.
pub const MAX_ADMITTED_ACTION_RECORD_BYTES: usize = 12288;
const MAX_STEP_ID_BYTES: usize = 128;

/// One per failed task: the decoded action's size, hash and canonical body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmittedAction {
    /// Always 1.
    pub schema_version: u32,
    /// The invocation step whose completed response carried the action.
    pub step_id: String,
    /// Size of the canonical action bytes.
    pub action_bytes: u64,
    /// Content hash of the canonical action bytes: the saved `action_hash`.
    pub action_hash: String,
    /// What of the action is kept.
    pub retention: ActionRetention,
}

/// Whether the canonical body is kept, or why it is not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionRetention {
    /// The canonical action JSON, byte for byte; it hashes to `action_hash`.
    Body {
        /// The canonical JSON text.
        json: String,
    },
    /// The body is not kept; only its size and hash are.
    Withheld {
        /// Why the body is not kept.
        reason: ActionWithheldReason,
    },
}

/// Why an action body is not kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionWithheldReason {
    /// Longer than the body allowance.
    TooLarge,
    /// Secret redaction would change it, so it can't be kept verbatim.
    Sensitive,
}

fn permitted(character: char) -> bool {
    !character.is_control() || character == '\n' || character == '\t'
}

impl AdmittedAction {
    /// Build the record from the canonical action bytes the saved hash covers.
    /// A body is kept only verbatim: if redaction would change it, or it is
    /// too large, only its size and hash are kept.
    pub fn capture(step_id: &str, canonical: &[u8]) -> Result<Self, InvestigationError> {
        let json = std::str::from_utf8(canonical).map_err(|_| InvestigationError::InvalidInput)?;
        serde_json::from_str::<serde::de::IgnoredAny>(json)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let retention = if json.len() > MAX_ADMITTED_ACTION_BODY_BYTES {
            ActionRetention::Withheld {
                reason: ActionWithheldReason::TooLarge,
            }
        } else if core_redact::redact_text(json).1 != 0 {
            ActionRetention::Withheld {
                reason: ActionWithheldReason::Sensitive,
            }
        } else {
            ActionRetention::Body {
                json: json.to_owned(),
            }
        };
        let record = Self {
            schema_version: 1,
            step_id: step_id.to_owned(),
            action_bytes: canonical.len() as u64,
            action_hash: core_prov::content_hash(canonical),
            retention,
        };
        record.bytes()?;
        Ok(record)
    }

    /// Canonical bounded bytes; every load and store goes through this check.
    pub fn bytes(&self) -> Result<Vec<u8>, InvestigationError> {
        if self.schema_version != 1
            || !super::text(&self.step_id, MAX_STEP_ID_BYTES)
            || !crate::source_basis::hash(&self.action_hash)
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let ActionRetention::Body { json } = &self.retention
            && (json.len() > MAX_ADMITTED_ACTION_BODY_BYTES
                || json.len() as u64 != self.action_bytes
                || core_prov::content_hash(json.as_bytes()) != self.action_hash
                || !json.chars().all(permitted)
                || core_redact::redact_text(json).1 != 0)
        {
            return Err(InvestigationError::InvalidInput);
        }
        bounded_bytes(self, MAX_ADMITTED_ACTION_RECORD_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-1405: the canonical body is kept verbatim and hashes to the saved hash.
    #[test]
    fn body_is_kept_verbatim_and_hashed() {
        let canonical = br#"{"action":{"query":{"kind":"entity","labels":["Alias"],"max_bytes":4096,"max_facts":8},"type":"query_context"},"schema_version":1}"#;
        let record = AdmittedAction::capture("step-2", canonical).unwrap();
        assert_eq!(record.action_hash, core_prov::content_hash(canonical));
        assert_eq!(record.action_bytes, canonical.len() as u64);
        assert_eq!(
            record.retention,
            ActionRetention::Body {
                json: String::from_utf8(canonical.to_vec()).unwrap()
            }
        );
        let bytes = record.bytes().unwrap();
        assert_eq!(
            serde_json::from_slice::<AdmittedAction>(&bytes).unwrap(),
            record
        );
    }

    // AC-1405: a body that is large or carries a secret keeps only its size
    // and hash.
    #[test]
    fn large_or_sensitive_bodies_are_withheld() {
        let large = format!(
            "{{\"labels\":\"{}\"}}",
            "a".repeat(MAX_ADMITTED_ACTION_BODY_BYTES)
        );
        let secret = r#"{"labels":["AKIAIOSFODNN7EXAMPLE"]}"#;
        for (canonical, reason) in [
            (large.as_str(), ActionWithheldReason::TooLarge),
            (secret, ActionWithheldReason::Sensitive),
        ] {
            let record = AdmittedAction::capture("step-1", canonical.as_bytes()).unwrap();
            assert_eq!(record.retention, ActionRetention::Withheld { reason });
            assert_eq!(
                record.action_hash,
                core_prov::content_hash(canonical.as_bytes())
            );
            assert!(record.bytes().unwrap().len() < 512);
        }
    }

    #[test]
    fn invalid_records_fail_closed() {
        assert!(AdmittedAction::capture("step-1", b"not json").is_err());
        // Canonical JSON escapes control characters; raw ones are refused.
        assert!(AdmittedAction::capture("step-1", b"{\"a\":1}\r").is_err());
        let record = AdmittedAction::capture("step-1", br#"{"type":"finish"}"#).unwrap();
        let mut wrong = record.clone();
        wrong.schema_version = 2;
        assert!(wrong.bytes().is_err());
        let mut wrong = record.clone();
        wrong.retention = ActionRetention::Body {
            json: r#"{"type":"query"}"#.into(),
        };
        assert!(wrong.bytes().is_err());
        let mut wrong = record;
        wrong.action_hash = "short".into();
        assert!(wrong.bytes().is_err());
    }
}
