//! Bounded, redacted retention of a provider response that the host rejected
//! (ADR-0076, AC-1403, #1616). The terminal diagnostic names the closed cause;
//! this record keeps enough of the response to attribute it without a rerun.
use super::{
    InvestigationError, InvestigationFinishRejection, InvestigationTerminalDiagnostic,
    bounded_bytes,
};
use serde::{Deserialize, Serialize};

/// Excerpt allowance: a prefix of the redacted response, cut on a UTF-8 boundary.
pub const MAX_REJECTED_EXCERPT_BYTES: usize = 1024;
/// Serialized record allowance, separate from task and store occupancy.
pub const MAX_REJECTED_RESPONSE_BYTES: usize = 8192;
const MAX_STEP_ID_BYTES: usize = 128;

/// One per failed task: the rejected response's size, hash and bounded excerpt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectedResponse {
    /// Always 1.
    pub schema_version: u32,
    /// The invocation step whose completed response was rejected.
    pub step_id: String,
    /// Size and content hash of the whole response; the response itself is not kept.
    pub response_bytes: u64,
    /// Content hash of the whole response.
    pub response_hash: String,
    /// What of the response text is kept.
    pub retention: ExcerptRetention,
}

/// Whether a redacted excerpt is kept, or why none is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExcerptRetention {
    /// A prefix of the response after secret redaction, with control
    /// characters other than newline and tab replaced by spaces.
    Excerpt {
        /// The redacted prefix.
        text: String,
        /// The redacted response was longer than the prefix.
        truncated: bool,
        /// Secrets replaced across the whole response.
        redactions: u32,
    },
    /// Nothing of the response is kept, because the rejection itself says the
    /// response carried sensitive output or copied source.
    Withheld {
        /// The rejection that rules out keeping text.
        reason: WithheldReason,
    },
}

/// Rejections whose response text is never kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithheldReason {
    /// Rejected as sensitive output.
    SensitiveOutput,
    /// Rejected for copying supplied source.
    SourceReplay,
}

fn withheld(diagnostic: Option<&InvestigationTerminalDiagnostic>) -> Option<WithheldReason> {
    let reason = match diagnostic? {
        InvestigationTerminalDiagnostic::RejectedAction { reason, .. }
        | InvestigationTerminalDiagnostic::RejectedFinish { reason } => *reason,
        _ => return None,
    };
    match reason {
        InvestigationFinishRejection::SensitiveOutput => Some(WithheldReason::SensitiveOutput),
        InvestigationFinishRejection::SourceReplay => Some(WithheldReason::SourceReplay),
        _ => None,
    }
}

fn floor_boundary(text: &str, maximum: usize) -> usize {
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn permitted(character: char) -> bool {
    !character.is_control() || character == '\n' || character == '\t'
}

impl RejectedResponse {
    /// Build the record for a rejected response. The whole response is
    /// redacted before it is cut, so a secret straddling the cut is still caught.
    pub fn capture(
        step_id: &str,
        response: &str,
        diagnostic: Option<&InvestigationTerminalDiagnostic>,
    ) -> Result<Self, InvestigationError> {
        let retention = match withheld(diagnostic) {
            Some(reason) => ExcerptRetention::Withheld { reason },
            None => {
                let (redacted, redactions) = core_redact::redact_text(response);
                let sanitized: String = redacted
                    .chars()
                    .map(|character| if permitted(character) { character } else { ' ' })
                    .collect();
                let end = floor_boundary(&sanitized, MAX_REJECTED_EXCERPT_BYTES);
                ExcerptRetention::Excerpt {
                    text: sanitized[..end].to_owned(),
                    truncated: end < sanitized.len(),
                    redactions: u32::try_from(redactions).unwrap_or(u32::MAX),
                }
            }
        };
        let record = Self {
            schema_version: 1,
            step_id: step_id.to_owned(),
            response_bytes: response.len() as u64,
            response_hash: core_prov::content_hash(response.as_bytes()),
            retention,
        };
        record.bytes()?;
        Ok(record)
    }

    /// Canonical bounded bytes; every load and store goes through this check.
    pub fn bytes(&self) -> Result<Vec<u8>, InvestigationError> {
        if self.schema_version != 1
            || !super::text(&self.step_id, MAX_STEP_ID_BYTES)
            || !crate::source_basis::hash(&self.response_hash)
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let ExcerptRetention::Excerpt { text, .. } = &self.retention
            && (text.len() > MAX_REJECTED_EXCERPT_BYTES || !text.chars().all(permitted))
        {
            return Err(InvestigationError::InvalidInput);
        }
        bounded_bytes(self, MAX_REJECTED_RESPONSE_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::investigation::InvestigationActionDecodeDiagnostic;

    fn rejected(reason: InvestigationFinishRejection) -> InvestigationTerminalDiagnostic {
        InvestigationTerminalDiagnostic::RejectedAction {
            stage: InvestigationActionDecodeDiagnostic::TypedEnvelope,
            reason,
        }
    }

    // AC-1403: a bounded redacted prefix, with the whole response hashed.
    #[test]
    fn excerpt_is_redacted_bounded_and_hashed() {
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let response = format!(
            "{{\"type\":\"qery\",\"key\":\"{secret}\"\u{7}}}{}",
            "é".repeat(2000)
        );
        let record = RejectedResponse::capture(
            "step-1",
            &response,
            Some(&rejected(InvestigationFinishRejection::InvalidAction)),
        )
        .unwrap();
        assert_eq!(record.response_bytes, response.len() as u64);
        assert_eq!(
            record.response_hash,
            core_prov::content_hash(response.as_bytes())
        );
        let ExcerptRetention::Excerpt {
            text,
            truncated,
            redactions,
        } = &record.retention
        else {
            panic!("expected an excerpt");
        };
        assert!(text.starts_with("{\"type\":\"qery\""));
        assert!(!text.contains(secret));
        assert!(*redactions >= 1);
        assert!(*truncated);
        assert!(text.len() <= MAX_REJECTED_EXCERPT_BYTES);
        assert!(!text.contains('\u{7}'));
        let bytes = record.bytes().unwrap();
        assert_eq!(
            serde_json::from_slice::<RejectedResponse>(&bytes).unwrap(),
            record
        );
    }

    #[test]
    fn short_response_is_kept_whole() {
        let record = RejectedResponse::capture("step-1", "not json", None).unwrap();
        assert_eq!(
            record.retention,
            ExcerptRetention::Excerpt {
                text: "not json".into(),
                truncated: false,
                redactions: 0
            }
        );
    }

    // AC-1403: a response rejected as sensitive or as copied source keeps no text.
    #[test]
    fn sensitive_or_replayed_responses_keep_no_text() {
        for (reason, kept) in [
            (
                InvestigationFinishRejection::SensitiveOutput,
                WithheldReason::SensitiveOutput,
            ),
            (
                InvestigationFinishRejection::SourceReplay,
                WithheldReason::SourceReplay,
            ),
        ] {
            for diagnostic in [
                rejected(reason),
                InvestigationTerminalDiagnostic::RejectedFinish { reason },
            ] {
                let record =
                    RejectedResponse::capture("step-1", "copied source", Some(&diagnostic))
                        .unwrap();
                assert_eq!(
                    record.retention,
                    ExcerptRetention::Withheld { reason: kept }
                );
                assert!(
                    !String::from_utf8(record.bytes().unwrap())
                        .unwrap()
                        .contains("copied source")
                );
            }
        }
    }

    #[test]
    fn invalid_records_fail_closed() {
        let record = RejectedResponse::capture("step-1", "x", None).unwrap();
        let mut wrong = record.clone();
        wrong.schema_version = 2;
        assert!(wrong.bytes().is_err());
        let mut wrong = record.clone();
        wrong.response_hash = "short".into();
        assert!(wrong.bytes().is_err());
        let mut wrong = record.clone();
        wrong.retention = ExcerptRetention::Excerpt {
            text: "a".repeat(MAX_REJECTED_EXCERPT_BYTES + 1),
            truncated: true,
            redactions: 0,
        };
        assert!(wrong.bytes().is_err());
        let mut wrong = record;
        wrong.retention = ExcerptRetention::Excerpt {
            text: "\u{1b}[31m".into(),
            truncated: false,
            redactions: 0,
        };
        assert!(wrong.bytes().is_err());
        let mut value =
            serde_json::to_value(RejectedResponse::capture("step-1", "x", None).unwrap()).unwrap();
        value["extra"] = true.into();
        assert!(serde_json::from_value::<RejectedResponse>(value).is_err());
    }
}
