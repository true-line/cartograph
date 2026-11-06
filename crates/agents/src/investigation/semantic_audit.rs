//! Shape and binding checks for semantic audit replies, never semantic proof.
//! The host supplies an authenticated candidate/ledger and validates privacy
//! before using a reply. Decoding grants no publication or provider authority.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[path = "semantic_audit_payload.rs"]
mod payload;
pub(super) use payload::prepare_payload;
pub use payload::{PreparedSemanticAudit, advisory_contract_hash};
pub use payload::{projection_wire_bounds, projection_workspace_bytes};

#[path = "semantic_audit_account.rs"]
mod account;
pub use account::SemanticAuditAccount;

/// Stable paths distinguish overall and finding-local limitations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuditSubject {
    Finding {
        finding_index: usize,
    },
    Limitation {
        limitation_index: usize,
    },
    FindingLimitation {
        finding_index: usize,
        limitation_index: usize,
    },
}

/// No free-text rationale can enter correction feedback through this wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubjectVerdict {
    Pass {},
    Contradiction {
        citation_id: String,
    },
    UnsupportedScope {},
    /// Only a directly answered requested step omitted without honest disclosure.
    AnswerCoverage {
        citation_id: String,
    },
    Indeterminate {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectReview {
    pub subject: AuditSubject,
    pub assessment: SubjectVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticAuditReply {
    pub schema_version: u32,
    pub candidate_hash: String,
    pub acquired_ledger_hash: String,
    pub contract_hash: String,
    pub reviews: Vec<SubjectReview>,
}

/// Mechanical aggregation of MODEL judgments; not an independent truth verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditDisposition {
    Pass,
    Rejected,
    Indeterminate,
}

impl SemanticAuditReply {
    /// Strict bounded decode rejects duplicate keys before typed allocation.
    pub fn decode(raw: &str) -> Result<Self, InvestigationError> {
        strict::preflight(
            raw,
            MAX_ACTION_BYTES,
            RECORD_MAX_DEPTH,
            RECORD_MAX_VALUES,
            RECORD_MAX_ARRAY_ITEMS,
        )?;
        serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidInput)
    }

    /// `candidate` and all pins must come from the host's saved invocation.
    /// Citation membership is acquired-ledger membership, not merely the subset
    /// cited by the candidate: an uncited acquired span can contradict it.
    pub fn validate_bound(
        &self,
        candidate: &InvestigationResult,
        candidate_hash: &str,
        acquired_ledger_hash: &str,
        contract_hash: &str,
        acquired_citation_ids: &BTreeSet<String>,
    ) -> Result<AuditDisposition, InvestigationError> {
        bounded_bytes(self, MAX_ACTION_BYTES)?;
        let candidate_bytes = bounded_bytes(candidate, MAX_RESULT_BYTES)?;
        if self.schema_version != 1
            || core_prov::content_hash(&candidate_bytes) != candidate_hash
            || candidate.input_ledger_hash != acquired_ledger_hash
            || [candidate_hash, acquired_ledger_hash, contract_hash]
                .iter()
                .any(|hash| !crate::source_basis::hash(hash))
            || self.candidate_hash != candidate_hash
            || self.acquired_ledger_hash != acquired_ledger_hash
            || self.contract_hash != contract_hash
            || self.reviews.len() > RECORD_MAX_ARRAY_ITEMS
        {
            return Err(InvestigationError::InvalidInput);
        }
        let expected = subjects(candidate)?;
        if expected.is_empty() || self.reviews.len() != expected.len() {
            return Err(InvestigationError::InvalidInput);
        }
        let mut seen = BTreeSet::new();
        let mut rejected = false;
        let mut indeterminate = false;
        for review in &self.reviews {
            if !expected.contains(&review.subject) || !seen.insert(&review.subject) {
                return Err(InvestigationError::InvalidInput);
            }
            match &review.assessment {
                SubjectVerdict::Pass {} => (),
                SubjectVerdict::Indeterminate {} => indeterminate = true,
                SubjectVerdict::UnsupportedScope {} => rejected = true,
                SubjectVerdict::Contradiction { citation_id }
                | SubjectVerdict::AnswerCoverage { citation_id } => {
                    if !opaque_id(citation_id) || !acquired_citation_ids.contains(citation_id) {
                        return Err(InvestigationError::InvalidCitation);
                    }
                    rejected = true;
                }
            }
        }
        Ok(if indeterminate {
            AuditDisposition::Indeterminate
        } else if rejected {
            AuditDisposition::Rejected
        } else {
            AuditDisposition::Pass
        })
    }
}

fn subjects(candidate: &InvestigationResult) -> Result<BTreeSet<AuditSubject>, InvestigationError> {
    // Enforce the existing subject ceiling before allocating the set, including
    // finding-local limitations. The final check alone allowed oversized scratch.
    let count = candidate.findings.iter().try_fold(
        candidate
            .limitations
            .len()
            .checked_add(candidate.findings.len())
            .ok_or(InvestigationError::LimitExceeded)?,
        |count, finding| {
            count
                .checked_add(finding.limitations.len())
                .ok_or(InvestigationError::LimitExceeded)
        },
    )?;
    if count > RECORD_MAX_ARRAY_ITEMS {
        return Err(InvestigationError::LimitExceeded);
    }
    let mut result = BTreeSet::new();
    for limitation_index in 0..candidate.limitations.len() {
        result.insert(AuditSubject::Limitation { limitation_index });
    }
    for (finding_index, finding) in candidate.findings.iter().enumerate() {
        result.insert(AuditSubject::Finding { finding_index });
        for limitation_index in 0..finding.limitations.len() {
            result.insert(AuditSubject::FindingLimitation {
                finding_index,
                limitation_index,
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (InvestigationResult, SemanticAuditReply, BTreeSet<String>) {
        let candidate = InvestigationResult {
            schema_version: 1,
            coverage: None,
            investigation_id: "task:1".into(),
            result_id: "result:1".into(),
            input_ledger_hash: "a".repeat(64),
            graph_snapshot_id: "snapshot:1".into(),
            invocation_step_id: None,
            disclosure_hash: None,
            inspection_assessment: None,
            completion: None,
            narrative: None,
            findings: vec![],
            knowledge_completeness: KnowledgeCompleteness::Partial,
            limitations: vec!["Server behavior was not inspected.".into()],
            observed_response_model: None,
            created_at: "2026-11-03T00:00:00Z".into(),
        };
        let reply = SemanticAuditReply {
            schema_version: 1,
            candidate_hash: core_prov::content_hash(
                &bounded_bytes(&candidate, MAX_RESULT_BYTES).unwrap(),
            ),
            acquired_ledger_hash: candidate.input_ledger_hash.clone(),
            contract_hash: "b".repeat(64),
            reviews: vec![SubjectReview {
                subject: AuditSubject::Limitation {
                    limitation_index: 0,
                },
                assessment: SubjectVerdict::Pass {},
            }],
        };
        (candidate, reply, BTreeSet::from(["evidence-4".into()]))
    }

    #[test]
    fn audit_subject_ceiling_is_checked_before_projection_allocation() {
        let (mut candidate, _, _) = fixture();
        candidate.limitations = vec![String::new(); RECORD_MAX_ARRAY_ITEMS];
        assert_eq!(subjects(&candidate).unwrap().len(), RECORD_MAX_ARRAY_ITEMS);
        candidate.limitations.push(String::new());
        assert!(matches!(
            subjects(&candidate),
            Err(InvestigationError::LimitExceeded)
        ));
    }

    // Scripted judgments test plumbing, not the model's semantic accuracy.
    #[test]
    fn authentic_partial_pass_and_nonpass_are_distinct() {
        let (candidate, mut reply, citations) = fixture();
        let hash = reply.candidate_hash.clone();
        let check = |r: &SemanticAuditReply| {
            r.validate_bound(
                &candidate,
                &hash,
                &"a".repeat(64),
                &"b".repeat(64),
                &citations,
            )
        };
        assert_eq!(check(&reply).unwrap(), AuditDisposition::Pass);
        reply.reviews[0].assessment = SubjectVerdict::Contradiction {
            citation_id: "evidence-4".into(),
        };
        assert_eq!(check(&reply).unwrap(), AuditDisposition::Rejected);
        reply.reviews[0].assessment = SubjectVerdict::UnsupportedScope {};
        assert_eq!(check(&reply).unwrap(), AuditDisposition::Rejected);
        reply.reviews[0].assessment = SubjectVerdict::Indeterminate {};
        assert_eq!(check(&reply).unwrap(), AuditDisposition::Indeterminate);
    }

    #[test]
    fn forged_candidate_missing_subject_and_foreign_witness_fail() {
        let (mut candidate, mut reply, citations) = fixture();
        let hash = reply.candidate_hash.clone();
        candidate.limitations[0] = "Changed after audit.".into();
        assert!(
            reply
                .validate_bound(
                    &candidate,
                    &hash,
                    &"a".repeat(64),
                    &"b".repeat(64),
                    &citations
                )
                .is_err()
        );
        let (candidate, _, _) = fixture();
        reply.reviews[0].assessment = SubjectVerdict::Contradiction {
            citation_id: "foreign".into(),
        };
        assert!(
            reply
                .validate_bound(
                    &candidate,
                    &hash,
                    &"a".repeat(64),
                    &"b".repeat(64),
                    &citations
                )
                .is_err()
        );
        reply.reviews.clear();
        assert!(
            reply
                .validate_bound(
                    &candidate,
                    &hash,
                    &"a".repeat(64),
                    &"b".repeat(64),
                    &citations
                )
                .is_err()
        );
    }

    #[test]
    fn compact_projection_keeps_exact_sources_and_drops_growing_context() {
        let (candidate, _, ids) = fixture();
        let original = llm::CompletionPayload {
            system: "historical analyst instructions".into(),
            prompt: serde_json::json!({"question":"How is an alias created?",
                "compact_context":{"secret_marker":"not audit context"},
                "consumed_budgets":{"model_invocations":16}})
            .to_string(),
            response_schema: None,
            spans: vec![llm::PayloadSpan {
                id: "evidence-4".into(),
                repo: "local".into(),
                path: "client.ts".into(),
                byte_start: 0,
                byte_end: 24,
                commit_sha: "c".repeat(40),
                text: "if (status === 201) save()".into(),
            }],
        };
        let audit = prepare_payload(&original, &candidate, &ids).unwrap();
        let action = audit.action("task:audit-1".into()).unwrap();
        assert_eq!(action.tier, llm::AnalysisTier::Agentic);
        assert_eq!(&action.payload, audit.payload());
        assert!(audit.action("".into()).is_err());
        assert_eq!(audit.payload().spans, original.spans);
        assert!(!audit.payload().prompt.contains("secret_marker"));
        assert!(!audit.payload().prompt.contains("consumed_budgets"));
        assert!(!audit.payload().system.contains("historical analyst"));
        assert!(
            audit
                .payload()
                .system
                .contains("Honest partial answers may pass")
        );
        assert!(audit.encoded_bytes().unwrap() < MAX_INPUT_BYTES);
        let schema: serde_json::Value =
            serde_json::from_str(audit.payload().response_schema.as_ref().unwrap()).unwrap();
        assert_eq!(
            schema["properties"]["candidate_hash"]["const"],
            audit.candidate_hash()
        );
        // Exercise the real compact builder at the subject ceiling with escaped
        // strings; the envelope must cover output, not merely its own formula.
        let mut large_candidate = candidate.clone();
        large_candidate.limitations = vec!["escaped \"text\" \\".repeat(8); RECORD_MAX_ARRAY_ITEMS];
        let mut large_original = original.clone();
        large_original.prompt =
            serde_json::json!({"question": "\"\\".repeat(MAX_QUESTION_BYTES / 2)}).to_string();
        let large = prepare_payload(&large_original, &large_candidate, &ids).unwrap();
        let (schema_bound, compact_bound) = projection_wire_bounds().unwrap();
        assert!(large.payload().prompt.len() <= compact_bound);
        assert!(large.payload().response_schema.as_ref().unwrap().len() <= schema_bound);
        let compact: serde_json::Value = serde_json::from_str(&large.payload().prompt).unwrap();
        assert_eq!(
            compact["subjects"].as_array().unwrap().len(),
            RECORD_MAX_ARRAY_ITEMS
        );
        let mut omitted = original.clone();
        omitted.spans.clear();
        assert!(prepare_payload(&omitted, &candidate, &ids).is_err());
        let mut duplicated = original.clone();
        duplicated.spans.push(original.spans[0].clone());
        assert!(prepare_payload(&duplicated, &candidate, &ids).is_err());
    }

    #[test]
    fn auditor_v2_subjects_link_parent_citations_at_finish_cardinality_limits() {
        let (mut candidate, _, _) = fixture();
        let ids: BTreeSet<_> = (1..=8).map(|i| format!("evidence-{i}")).collect();
        let finding: InvestigationFinding = serde_json::from_value(serde_json::json!({
            "finding_id":"finding-1", "claim_kind":"inferred_interpretation",
            "confidence_tier":"InferredWeak", "tier":"Agentic", "title":"title",
            "statement":"statement", "citation_ids":ids, "limitations":vec!["gap"; 8]
        }))
        .unwrap();
        candidate.findings = vec![finding; 12];
        candidate.limitations = vec!["overall".into(); 8];
        let original = llm::CompletionPayload {
            response_schema: None,
            system: String::new(),
            prompt: r#"{"question":"why?"}"#.into(),
            spans: ids
                .iter()
                .map(|id| llm::PayloadSpan {
                    id: id.clone(),
                    repo: "test".into(),
                    path: "p".into(),
                    byte_start: 0,
                    byte_end: 1,
                    commit_sha: "a".repeat(40),
                    text: "x".into(),
                })
                .collect(),
        };
        let audit = prepare_payload(&original, &candidate, &ids).unwrap();
        let prompt: serde_json::Value = serde_json::from_str(&audit.payload().prompt).unwrap();
        assert_eq!(prompt["role"], "candidate-evidence-auditor@3");
        let list = prompt["subjects"].as_array().unwrap();
        assert_eq!(list.len(), 12 * 9 + 8);
        for descriptor in list {
            match descriptor["subject"]["kind"].as_str().unwrap() {
                "limitation" => assert_eq!(descriptor["scope"], "all_acquired_spans"),
                "finding" | "finding_limitation" => {
                    assert_eq!(descriptor["parent_citation_ids"], serde_json::json!(ids))
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(audit.payload().spans, original.spans);
        assert!(audit.encoded_bytes().unwrap() <= MAX_INPUT_BYTES);
    }

    // AC-0566: model replies contain only closed, typed review data.
    #[test]
    fn strict_reply_rejects_duplicate_keys_and_untyped_prose() {
        let raw = r#"{"schema_version":1,"schema_version":1}"#;
        assert!(SemanticAuditReply::decode(raw).is_err());
        assert!(
            serde_json::from_str::<SubjectVerdict>(
                r#"{"verdict":"pass","rationale":"private source"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<SubjectVerdict>(
                r#"{"verdict":"unsupported_scope","citation_id":"fabricated"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn limitation_paths_do_not_alias() {
        let overall = AuditSubject::Limitation {
            limitation_index: 0,
        };
        let local = AuditSubject::FindingLimitation {
            finding_index: 0,
            limitation_index: 0,
        };
        assert_ne!(overall, local);
        assert_ne!(
            serde_json::to_vec(&overall).unwrap(),
            serde_json::to_vec(&local).unwrap()
        );
    }
}

/// Content-free terminal audit state. These are model observations, not proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdvisoryAudit {
    Completed { reviews: Vec<SubjectReview> },
    NotCompleted { reason: AuditFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditFailure {
    Transport,
    Timeout,
    Length,
    InvalidOutput,
}
