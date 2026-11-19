//! Human follow-up decisions on immutable investigation findings. These records
//! carry no graph, evidence-freshness, provider or execution authority.
use super::{InvestigationError, MAX_ACTION_BYTES, opaque_id, strict};
use serde::{Deserialize, Serialize};

/// Versioned public identity prefix; the suffix is a host-computed BLAKE3 hash.
pub const FINDING_REVIEW_SUBJECT_PREFIX: &str = "investigation-finding-review-v1:";

/// Host-resolved exact historical finding reference, without its private basis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingReviewSubjectV1 {
    /// Owning saved investigation.
    pub investigation_id: String,
    /// Complete immutable saved result identity.
    pub result_id: String,
    /// Result-local finding identity; insufficient on its own.
    pub finding_id: String,
    /// Domain-separated identity of the complete host-resolved review basis.
    pub subject_id: String,
}

impl FindingReviewSubjectV1 {
    /// Validate public shape only; the host must independently resolve identity.
    pub fn validate(&self) -> Result<(), InvestigationError> {
        if !opaque_id(&self.investigation_id)
            || !opaque_id(&self.result_id)
            || !opaque_id(&self.finding_id)
            || !self
                .subject_id
                .strip_prefix(FINDING_REVIEW_SUBJECT_PREFIX)
                .is_some_and(crate::source_basis::hash)
        {
            return Err(InvestigationError::InvalidRequest);
        }
        Ok(())
    }
}

/// A human's qualified follow-up selection, never evidence or graph acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingReviewDecision {
    /// Selected for possible subsequent work.
    AcceptedForFollowUp,
    /// Not selected for subsequent work; original finding remains readable.
    RejectedForFollowUp,
}

/// Current metadata for one finding; an absent row derives revision zero/nulls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingReviewObservation {
    /// Exact historical subject resolved by the host.
    pub subject: FindingReviewSubjectV1,
    /// Current compare-and-set revision; zero represents no persisted decision.
    pub revision: u64,
    /// Latest committed decision, or pending review.
    pub decision: Option<FindingReviewDecision>,
    /// Host UTC commit time, absent for pending review.
    pub reviewed_at: Option<String>,
}

/// Metadata-only observation in the immutable result's saved finding order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingReviews {
    /// Review wire version, currently one; independent of the SQL schema version.
    pub schema_version: u32,
    /// Saved investigation that owns these findings.
    pub investigation_id: String,
    /// Exact expected immutable result identity.
    pub result_id: String,
    /// Exactly one observation per finding; no finding prose or result body.
    pub reviews: Vec<FindingReviewObservation>,
}

/// Closed mutation input; all basis fields are resolved independently on host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInvestigationFindingRequest {
    /// Mutation wire version, currently one.
    pub schema_version: u32,
    /// Public identity only, never caller-supplied content/citations/configuration.
    pub subject: FindingReviewSubjectV1,
    /// Exact current review revision; zero admits the first decision.
    pub expected_revision: u64,
    /// Explicit next human follow-up decision.
    pub decision: FindingReviewDecision,
}

impl ReviewInvestigationFindingRequest {
    /// Admit bounded JSON before typed allocation, preserving duplicate/trailing
    /// rejection. No execution-policy or stored-result byte limit is changed.
    pub fn decode(raw: &str) -> Result<Self, InvestigationError> {
        strict::preflight(raw, MAX_ACTION_BYTES, 16, 4096, 32)?;
        let request: Self =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidRequest)?;
        request.validate()?;
        Ok(request)
    }

    /// Validate input shape even for trusted typed callers, before any write.
    pub fn validate(&self) -> Result<(), InvestigationError> {
        if self.schema_version != 1 || self.expected_revision > i64::MAX as u64 {
            return Err(InvestigationError::InvalidRequest);
        }
        self.subject.validate()
    }
}

/// Closed curation input (ADR-0074). It is not a follow-up request: it has
/// its own decision type and its own store. As there, the caller names the
/// finding and nothing else; the host resolves every other field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurateInvestigationFindingRequest {
    /// Mutation wire version, currently one.
    pub schema_version: u32,
    /// Public identity only, never caller-supplied content or citations.
    pub subject: FindingReviewSubjectV1,
    /// Exact current curation revision; zero admits the first decision.
    pub expected_revision: u64,
    /// The next curation decision.
    pub decision: crate::finding_curation::CurationDecision,
}

impl CurateInvestigationFindingRequest {
    /// Admit bounded JSON before typed allocation, as the follow-up request
    /// does.
    pub fn decode(raw: &str) -> Result<Self, InvestigationError> {
        strict::preflight(raw, MAX_ACTION_BYTES, 16, 4096, 32)?;
        let request: Self =
            serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidRequest)?;
        request.validate()?;
        Ok(request)
    }

    /// Validate input shape even for trusted typed callers, before any write.
    pub fn validate(&self) -> Result<(), InvestigationError> {
        if self.schema_version != 1 || self.expected_revision > i64::MAX as u64 {
            return Err(InvestigationError::InvalidRequest);
        }
        self.subject.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-0844: the curation request is its own closed wire shape.
    #[test]
    fn curation_request_is_strict_and_does_not_take_a_follow_up_decision() {
        let raw = REQUEST.replace("accepted_for_follow_up", "accepted");
        let request = CurateInvestigationFindingRequest::decode(&raw).unwrap();
        assert_eq!(request.subject.finding_id, "finding-1");
        assert_eq!(
            request.decision,
            crate::finding_curation::CurationDecision::Accepted
        );
        assert_eq!(serde_json::to_string(&request).unwrap(), raw);
        assert_eq!(
            CurateInvestigationFindingRequest::decode(&raw.replace("\"accepted\"", "\"rejected\""))
                .unwrap()
                .decision,
            crate::finding_curation::CurationDecision::Rejected
        );
        let invalid = [
            // A follow-up decision is not a curation decision.
            REQUEST.to_owned(),
            raw.replace("\"accepted\"", "\"returned\""),
            raw.replacen("{", "{\"statement\":\"caller supplied\",", 1),
            raw.replace("\"schema_version\":1", "\"schema_version\":2"),
            raw.replace(
                "\"expected_revision\":0",
                "\"expected_revision\":9223372036854775808",
            ),
            raw.replace("\"expected_revision\":0", "\"expected_revision\":-1"),
            raw.replace("finding-1", "finding/1"),
            raw.replace(
                "investigation-finding-review-v1:",
                "investigation-finding-review-v2:",
            ),
            format!("{raw} {{}}"),
        ];
        for raw in invalid {
            assert!(
                CurateInvestigationFindingRequest::decode(&raw).is_err(),
                "{raw}"
            );
        }
    }

    const REQUEST: &str = concat!(
        r#"{"schema_version":1,"subject":{"investigation_id":"investigation-1","result_id":"result-1","finding_id":"finding-1","subject_id":"investigation-finding-review-v1:"#,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        r#""},"expected_revision":0,"decision":"accepted_for_follow_up"}"#,
    );

    // AC-0293/AC-0294: literal independent wire fixture and strict public shape.
    #[test]
    fn finding_review_request_literal_preserves_qualified_metadata_only_wire() {
        let request = ReviewInvestigationFindingRequest::decode(REQUEST).unwrap();
        assert_eq!(request.subject.finding_id, "finding-1");
        assert_eq!(request.decision, FindingReviewDecision::AcceptedForFollowUp);
        assert_eq!(serde_json::to_string(&request).unwrap(), REQUEST);
        let mut maximum = request.clone();
        maximum.expected_revision = i64::MAX as u64;
        assert!(maximum.validate().is_ok());
        maximum.expected_revision += 1;
        assert!(maximum.validate().is_err());
        let rejected = REQUEST.replace("accepted_for_follow_up", "rejected_for_follow_up");
        assert_eq!(
            ReviewInvestigationFindingRequest::decode(&rejected)
                .unwrap()
                .decision,
            FindingReviewDecision::RejectedForFollowUp
        );
        let pending = FindingReviews {
            schema_version: 1,
            investigation_id: request.subject.investigation_id.clone(),
            result_id: request.subject.result_id.clone(),
            reviews: vec![FindingReviewObservation {
                subject: request.subject,
                revision: 0,
                decision: None,
                reviewed_at: None,
            }],
        };
        let wire = serde_json::to_value(pending).unwrap();
        assert!(wire.get("result").is_none());
        assert_eq!(wire["reviews"][0]["decision"], serde_json::Value::Null);
    }

    // AC-0294: malformed requests are rejected without permissive Value parsing.
    #[test]
    fn finding_review_request_rejects_duplicate_trailing_unknown_and_overflow() {
        let invalid = [
            REQUEST.replacen(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1",
                1,
            ),
            format!("{REQUEST} {{}}"),
            REQUEST.replacen("{", "{\"content\":\"caller supplied\",", 1),
            REQUEST.replace("\"finding_id\":", "\"provider\":\"caller\",\"finding_id\":"),
            REQUEST.replace("\"schema_version\":1", "\"schema_version\":2"),
            REQUEST.replace(
                "\"expected_revision\":0",
                "\"expected_revision\":9223372036854775808",
            ),
            REQUEST.replace("\"expected_revision\":0", "\"expected_revision\":-1"),
            REQUEST.replace("\"expected_revision\":0", "\"expected_revision\":0.0"),
            REQUEST.replace("accepted_for_follow_up", "accepted"),
            REQUEST.replace("finding-1", "finding/1"),
            REQUEST.replace("result-1", ""),
            REQUEST.replace("0123456789abcdef", "0123456789abcdeF"),
            REQUEST.replace(
                "investigation-finding-review-v1:",
                "investigation-finding-review-v2:",
            ),
        ];
        for raw in invalid {
            assert!(ReviewInvestigationFindingRequest::decode(&raw).is_err());
        }
    }

    // AC-0294: existing raw-action boundary and streaming shape admission apply.
    #[test]
    fn finding_review_request_rejects_excess_bytes_and_depth() {
        let oversized = format!("{REQUEST}{}", " ".repeat(MAX_ACTION_BYTES));
        assert_eq!(
            ReviewInvestigationFindingRequest::decode(&oversized),
            Err(InvestigationError::LimitExceeded)
        );
        let deep = format!("{}0{}", "[".repeat(18), "]".repeat(18));
        assert!(ReviewInvestigationFindingRequest::decode(&deep).is_err());
        let oversized_id = REQUEST.replace("finding-1", &"x".repeat(129));
        assert!(ReviewInvestigationFindingRequest::decode(&oversized_id).is_err());
    }
}
