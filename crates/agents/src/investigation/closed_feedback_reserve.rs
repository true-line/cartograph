//! @14 serializer-only closure for finite feedback, never feedback authority.
use super::*;
use crate::TaskFactKey;

/// Dimensions of already JSON-encoded bytes. An outer JSON string preserves N
/// ordinary bytes and doubles each of E quotes/backslashes, adding two quotes.
/// Counting UTF-8 bytes rather than scalars also dominates non-ASCII identities.
#[derive(Clone, Copy, Default, Debug)]
struct Dimensions {
    ordinary: usize,
    escaped: usize,
}
impl Dimensions {
    fn of(value: &impl Serialize) -> Result<Self, InvestigationError> {
        let bytes = bounded_bytes(value, MAX_INPUT_BYTES)?;
        // Classify the ALREADY serialized JSON, not raw string characters.
        // serde_json has escaped every U+0000..U+001F before this point; its
        // emitted backslashes belong to E. Non-ASCII stays literal UTF-8 and
        // is counted byte-wise in N. Reject any out-of-class serializer output.
        if bytes.iter().any(|byte| *byte < 0x20)
            || core_redact::redact_text(
                std::str::from_utf8(&bytes).map_err(|_| InvestigationError::InvalidInput)?,
            )
            .1 != 0
        {
            return Ok(Self {
                ordinary: MAX_INPUT_BYTES,
                escaped: MAX_INPUT_BYTES,
            });
        }
        let escaped = bytes.iter().filter(|b| matches!(b, b'"' | b'\\')).count();
        Ok(Self {
            ordinary: bytes.len() - escaped,
            escaped,
        })
    }
    fn add(self, other: Self) -> Result<Self, InvestigationError> {
        Ok(Self {
            ordinary: self
                .ordinary
                .checked_add(other.ordinary)
                .ok_or(InvestigationError::LimitExceeded)?,
            escaped: self
                .escaped
                .checked_add(other.escaped)
                .ok_or(InvestigationError::LimitExceeded)?,
        })
    }
    fn maximum(self, other: Self) -> Self {
        Self {
            ordinary: self.ordinary.max(other.ordinary),
            escaped: self.escaped.max(other.escaped),
        }
    }
    fn envelope(self, cap: usize) -> TypedFeedbackSizeEnvelope {
        // Each admitted feedback is independently bounded by cap. Retain the
        // historical arbitrary-string bound if the closed projection is larger.
        if self.ordinary.saturating_add(self.escaped) > cap {
            return TypedFeedbackSizeEnvelope("\\".repeat(cap));
        }
        // This is one escaping layer MORE than the value being dominated. Its
        // safe alphabet cannot create redact-text recognizers across boundaries.
        // It cannot deserialize as feedback or a citation correction.
        TypedFeedbackSizeEnvelope(format!(
            "{}{}",
            "f".repeat(self.ordinary),
            "\\".repeat(self.escaped)
        ))
    }
}

impl RefusalFeedback {
    /// Both saved feedback and its current citation projection can coexist.
    /// Bound both independently; never assume their query/read alternatives
    /// coincide. Historical roles keep their original backslash envelope.
    pub(in crate::investigation) fn sizing_closed_contract_envelope(
        contract: &InvestigationCompletionContract,
        ledger: &InvestigationInputLedger,
        policy: &InvestigationPolicy,
    ) -> Result<TypedFeedbackSizeEnvelope, InvestigationError> {
        contract.validate_with_policy(policy)?;
        let context = &ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?
            .context_id;
        let mut feedback =
            Self::sizing_upper_envelope_with_policy(&contract.obligations, policy)?.feedback;
        let mut frame =
            serde_json::to_value(&feedback).map_err(|_| InvestigationError::InvalidInput)?;
        frame["schema_version"] = 2.into();
        frame["citation_correction"] =
            serde_json::json!({"schema_version":1,"choices_hash":"f".repeat(64),"entries":[]});
        let mut citation = Dimensions::of(&frame)?;
        let mut count = 0;
        for obligation in &contract.obligations.obligations {
            let Obligation::Source {
                obligation_id,
                target,
            } = obligation
            else {
                continue;
            };
            let entry = |action: acquisition_choices::AcquisitionAction| {
                serde_json::json!({
                    "target_id":target.target_id,"obligation_id":obligation_id,"action":action
                })
            };
            let mut dims = Dimensions::of(&entry(
                acquisition_choices::AcquisitionAction::ReadEvidence {
                    fact: target.fact.clone(),
                    role: target.role,
                    index: target.index,
                },
            ))?;
            if let TaskFactKey::Node { id } = &target.fact {
                let query = entry(acquisition_choices::AcquisitionAction::QueryContext {
                    context_id: context.clone(),
                    query: InvestigationQuery {
                        scope: InvestigationScope::Node { id: id.clone() },
                        kind: Some(InvestigationFactKind::Node),
                        labels: vec![],
                        max_facts: 1,
                        max_bytes: MAX_QUERY_BYTES,
                        cursor: None,
                    },
                });
                // Independent maxima cover every mixture, including different
                // original and reprojected actions after another acquisition.
                dims = dims.maximum(Dimensions::of(&query)?);
            }
            citation = citation.add(dims)?;
            if count > 0 {
                citation = citation.add(Dimensions {
                    ordinary: 1,
                    escaped: 0,
                })?;
            }
            count += 1;
        }
        let mut maximum = Dimensions::of(&feedback)?.maximum(citation);
        let position = ProsePosition {
            field: ProseField::FindingLimitation {
                finding: 11,
                limitation: 7,
            },
            sentence: MAX_INPUT_BYTES as u32,
        };
        let location = ReplayLocation {
            start: position.clone(),
            end: position,
        };
        // Superset of @14's finite non-combined feedback. No refused prose,
        // arbitrary model string or private response can enter these records.
        for correction in [
            ProseCorrection::RewriteSchemaPlaceholdersAsPlainProse,
            ProseCorrection::ParaphraseWithoutQuotingSource,
            ProseCorrection::ParaphraseReplayAt(location.clone()),
            ProseCorrection::UnverifiedEnumerationClaimAt(location.clone()),
            ProseCorrection::UnverifiedConfigurationClaimAt(location),
            ProseCorrection::LimitationDeniesAcquiredEvidence(Box::new(ConflictingEvidence {
                evidence_ids: (1..=policy.limits().evidence_items)
                    .map(|i| format!("evidence-{i}"))
                    .collect(),
            })),
        ] {
            feedback.schema_version = 5; // All permitted schema numbers have one digit.
            feedback.prose_correction = Some(correction);
            maximum = maximum.maximum(Dimensions::of(&feedback)?);
        }
        Ok(maximum.envelope(feedback_bytes(policy)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auditor14_closed_dimensions_dominate_nested_utf8_and_escaping() {
        let controls: String = (0u8..32).map(char::from).collect();
        for value in [
            serde_json::json!({"x": controls}),
            serde_json::json!({"x":"\u{007f}\u{0080}\u{2028}\u{2029}\u{ffff}\u{10ffff}"}),
            serde_json::json!({"x":"ordinary"}),
            serde_json::json!({"x":"é雪\n\t\u{0001}\"\\"}),
            serde_json::json!(["a", "b"]),
        ] {
            let dims = Dimensions::of(&value).unwrap();
            let cap = 4096;
            let envelope = dims.envelope(cap);
            let mut actual = serde_json::to_string(&value).unwrap();
            let mut upper = serde_json::to_string(&envelope).unwrap();
            assert!(serde_json::from_str::<RefusalFeedback>(&upper).is_err());
            assert_eq!(core_redact::redact_text(&upper).1, 0);
            for _ in 0..5 {
                assert!(
                    actual.len() <= upper.len(),
                    "{} > {}",
                    actual.len(),
                    upper.len()
                );
                assert!(
                    core_redact::redact_text(&actual).0.len()
                        <= core_redact::redact_text(&upper).0.len()
                );
                actual = serde_json::to_string(&actual).unwrap();
                upper = serde_json::to_string(&upper).unwrap();
            }
        }
        let secret = serde_json::json!({"fact":"sk-synthetic000000000"});
        let envelope = Dimensions::of(&secret).unwrap().envelope(4096);
        assert_eq!(envelope.0, "\\".repeat(4096));
        assert_eq!(
            Dimensions {
                ordinary: 4097,
                escaped: 0
            }
            .envelope(4096)
            .0,
            envelope.0
        );
    }
}
