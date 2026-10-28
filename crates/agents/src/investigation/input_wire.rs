//! Exact production prompt/payload serialization, with allocation-free counting.
//! This knows no acquisition authority, future metadata, redaction or transport
//! framing. Missing future contributions cannot be certified by this kernel.
use super::*;
use serde::Serialize;
use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WireSize {
    pub prompt_bytes: usize,
    pub payload_bytes: usize,
}

#[derive(Default)]
struct JsonMeter {
    bytes: usize,
    escaped_content_bytes: usize,
    limit: usize,
}
impl Write for JsonMeter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|total| *total <= self.limit)
            .ok_or_else(|| io::Error::other("input wire bound"))?;
        for byte in bytes {
            // serde_json's ordinary string encoder: the inner JSON is itself
            // serialized as CompletionPayload.prompt. Work per byte so UTF-8
            // boundaries between writer calls cannot affect the exact count.
            let encoded = match byte {
                b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
                0..=0x1f => 6,
                _ => 1,
            };
            self.escaped_content_bytes = self
                .escaped_content_bytes
                .checked_add(encoded)
                .ok_or_else(|| io::Error::other("input wire overflow"))?;
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn count_json(value: &impl Serialize, limit: usize) -> Result<JsonMeter, InvestigationError> {
    let mut meter = JsonMeter {
        limit,
        ..JsonMeter::default()
    };
    // Preserve bounded_bytes' existing classification, including failures from
    // Serialize implementations. No failed contribution is treated as zero.
    serde_json::to_writer(&mut meter, value).map_err(|_| InvestigationError::LimitExceeded)?;
    Ok(meter)
}

pub(super) fn encode_prompt(
    value: &impl Serialize,
    limit: usize,
) -> Result<String, InvestigationError> {
    count_json(value, limit)?;
    String::from_utf8(bounded_bytes(value, limit)?).map_err(|_| InvestigationError::InvalidInput)
}

pub(super) fn validate_payload(
    payload: &CompletionPayload,
    limit: usize,
) -> Result<(), InvestigationError> {
    count_json(payload, limit).map(|_| ())
}

/// Count a replacement without constructing a prompt string or cloning spans.
/// The real CompletionPayload serializer supplies all field names, ordering and
/// framing. Its derived representation contains exactly one prompt string: remove
/// that field's current encoded value and add the new JSON-as-string value.
/// No separately maintained payload-shaped sizing DTO is used.
pub(super) fn measure_replacement(
    payload: &CompletionPayload,
    prompt: &impl Serialize,
    limit: usize,
) -> Result<WireSize, InvestigationError> {
    let prompt = count_json(prompt, limit)?;
    let old_prompt = count_json(&payload.prompt, usize::MAX)?.bytes;
    let original = count_json(payload, usize::MAX)?.bytes;
    let payload_bytes = original
        .checked_sub(old_prompt)
        .and_then(|bytes| bytes.checked_add(prompt.escaped_content_bytes))
        .and_then(|bytes| bytes.checked_add(2)) // prompt string's quotes
        .filter(|bytes| *bytes <= limit)
        .ok_or(InvestigationError::LimitExceeded)?;
    Ok(WireSize {
        prompt_bytes: prompt.bytes,
        payload_bytes,
    })
}

pub(super) fn replace_prompt(
    payload: &mut CompletionPayload,
    prompt: &impl Serialize,
    limit: usize,
) -> Result<(), InvestigationError> {
    measure_replacement(payload, prompt, limit)?;
    payload.prompt = encode_prompt(prompt, limit)?;
    // Recheck the actual owned representation as well. Future/custom Serialize
    // implementations cannot turn an earlier count into permission to overflow.
    validate_payload(payload, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> CompletionPayload {
        CompletionPayload {
            response_schema: None,
            system: "system \"quoted\" \\ é\n".into(),
            prompt: "previous \"prompt\"\u{0}".into(),
            spans: vec![PayloadSpan {
                id: "source-1".into(),
                repo: "repo/é".into(),
                path: "a\\\"b.ts".into(),
                byte_start: 1,
                byte_end: u64::MAX,
                commit_sha: "workdir".into(),
                text: "source\n\t\\\"\u{0}".into(),
            }],
        }
    }

    #[test]
    fn input_wire_count_and_write_match_production_serde_at_every_embedding() {
        // AC-0287: exact current representations, not a whole-continuation proof.
        let controls = (0..=31).map(char::from).collect::<String>();
        let values = [
            json!(null),
            json!({}),
            json!({"unicode":"é中🙂","control":controls,
            "nested_json":"{\"inventory\":\"quoted\\value\"}","number":u64::MAX}),
            json!([false, true, 0, usize::MAX, "\\\"/\n"]),
        ];
        for value in values {
            let mut actual = payload();
            let mut reference = actual.clone();
            reference.prompt = serde_json::to_string(&value).unwrap();
            let expected = serde_json::to_vec(&reference).unwrap();
            let size = measure_replacement(&actual, &value, expected.len()).unwrap();
            assert_eq!(size.prompt_bytes, reference.prompt.len());
            assert_eq!(size.payload_bytes, expected.len());
            replace_prompt(&mut actual, &value, expected.len()).unwrap();
            assert_eq!(actual, reference);
            assert_eq!(serde_json::to_vec(&actual).unwrap(), expected);
        }
    }

    #[test]
    fn input_wire_exact_boundary_and_one_over_preserve_error_parity() {
        let value = json!({"body":"\\\"\u{1}é".repeat(32)});
        let original = payload();
        let mut reference = original.clone();
        reference.prompt = serde_json::to_string(&value).unwrap();
        let length = serde_json::to_vec(&reference).unwrap().len();
        assert!(measure_replacement(&original, &value, length).is_ok());
        let mut actual = original.clone();
        assert_eq!(
            replace_prompt(&mut actual, &value, length - 1),
            Err(InvestigationError::LimitExceeded)
        );
        assert_eq!(actual, original);
        assert_eq!(
            validate_payload(&reference, length - 1),
            bounded_bytes(&reference, length - 1).map(|_| ())
        );
        let prompt_length = serde_json::to_vec(&value).unwrap().len();
        assert_eq!(
            encode_prompt(&value, prompt_length - 1),
            bounded_bytes(&value, prompt_length - 1).map(|bytes| String::from_utf8(bytes).unwrap())
        );
    }

    #[test]
    fn input_wire_serialization_failure_is_never_an_empty_contribution() {
        struct Unavailable;
        impl Serialize for Unavailable {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("representation unavailable"))
            }
        }
        let mut actual = payload();
        let original = actual.clone();
        assert_eq!(
            measure_replacement(&actual, &Unavailable, MAX_INPUT_BYTES),
            Err(InvestigationError::LimitExceeded)
        );
        assert_eq!(
            replace_prompt(&mut actual, &Unavailable, MAX_INPUT_BYTES),
            Err(InvestigationError::LimitExceeded)
        );
        assert_eq!(actual, original);
        assert_eq!(
            encode_prompt(&Unavailable, MAX_INPUT_BYTES),
            bounded_bytes(&Unavailable, MAX_INPUT_BYTES)
                .map(|bytes| String::from_utf8(bytes).unwrap())
        );
    }

    #[test]
    fn input_wire_counting_overflow_fails_closed_without_large_allocations() {
        let mut raw = JsonMeter {
            bytes: usize::MAX,
            limit: usize::MAX,
            ..JsonMeter::default()
        };
        assert!(raw.write_all(b"x").is_err());
        let mut escaped = JsonMeter {
            escaped_content_bytes: usize::MAX,
            limit: usize::MAX,
            ..JsonMeter::default()
        };
        assert!(escaped.write_all(b"\"").is_err());
        assert!(count_json(&json!({}), 0).is_err());
    }

    #[test]
    fn input_wire_all_published_families_match_the_prior_serialization_path() {
        // AC-0287: exercise complete production builders, including @9, rather
        // than certifying a small hand-built payload component. Existing literal
        // prompt/result fixtures independently keep their historical identities.
        for auditor in [false, true] {
            for version in 1..=9 {
                let (target, mut ledger, _) =
                    crate::investigation::tests::inspection_tests::fixture();
                ledger.selected_facts.clear();
                ledger.receipt_references.clear();
                ledger.citations.clear();
                ledger.queries.clear();
                ledger.revision = 0;
                if version >= 6 {
                    ledger.schema_version = 2;
                    ledger.coverage = Some(
                        context_hub::coverage::CoverageQualification::new(&[], &[])
                            .unwrap()
                            .durable_proof(&ledger.scope_snapshot_id)
                            .unwrap(),
                    );
                } else {
                    ledger.schema_version = 1;
                    ledger.coverage = None;
                }
                let name = format!(
                    "{}@{version}",
                    if auditor {
                        "evidence-auditor"
                    } else {
                        "domain-analyst"
                    }
                );
                let specialist: SpecialistId = serde_json::from_value(json!(name)).unwrap();
                let policy = if version >= 8 {
                    &INVESTIGATION_V2_POLICY
                } else {
                    &INVESTIGATION_V1_POLICY
                };
                let usage = InvestigationUsage::default();
                let basis = InvestigationTurnBasis {
                    input_ledger_hash: ledger.fingerprint_with_policy(policy).unwrap(),
                    previous_ledger: None,
                    completed_tool: None,
                };
                let question = "Explain the retained evidence. \"quoted\" é";
                let input = match version {
                    1..=3 => InvestigationInput::prepare(
                        question,
                        specialist,
                        ledger,
                        &[],
                        &[],
                        &[],
                        &usage,
                    ),
                    4 => InvestigationInput::prepare_v4(
                        question,
                        specialist,
                        ledger,
                        &[],
                        &[],
                        &[],
                        &usage,
                        &basis,
                    ),
                    5 => InvestigationInput::prepare_v5(
                        question,
                        specialist,
                        ledger,
                        &[],
                        &[],
                        &[],
                        &usage,
                        &basis,
                        &[target],
                    ),
                    6 => InvestigationInput::prepare_v6(
                        question,
                        specialist,
                        ledger,
                        &[],
                        &[],
                        &[],
                        &usage,
                        &basis,
                        Some(&[target]),
                        &[],
                    ),
                    _ => {
                        let mut source = Obligation::Source {
                            obligation_id: String::new(),
                            target: Box::new(target),
                        };
                        let id = source.content_id().unwrap();
                        if let Obligation::Source { obligation_id, .. } = &mut source {
                            *obligation_id = id;
                        }
                        let request = StagedObligationRequest {
                            schema_version: version - 3,
                            request_nonce: "wire-compatibility".into(),
                            specialist_id: name,
                            obligations: ObligationEnvelope {
                                schema_version: if version == 7 { 1 } else { 2 },
                                contract: format!(
                                    "investigation-completion-obligations@{}",
                                    if version == 7 { 1 } else { 2 }
                                ),
                                obligations: vec![source],
                            },
                            inspection_requirements: None,
                            question: question.into(),
                            scope: InvestigationScope::All,
                            provider_mode: InvestigationProviderMode::Local,
                            limit_profile: policy.profile().into(),
                            expected_graph_revision: Some(ledger.graph_snapshot_id.clone()),
                            expected_context_id: ledger
                                .coverage
                                .as_ref()
                                .unwrap()
                                .context_id
                                .clone(),
                            conversation_id: None,
                            parent_id: None,
                        };
                        let metadata = staged_refusal::ProtectedCompletionMetadata {
                            schema_version: 1,
                            request_hash: request.intent_hash_with_policy(policy).unwrap(),
                            obligations_hash: request
                                .obligations
                                .fingerprint_with_policy(policy)
                                .unwrap(),
                            completed_attempt_ids: vec![],
                            refusal_count: 0,
                            last_refusal_id: None,
                            feedback: None,
                        };
                        if version == 9 {
                            InvestigationInput::prepare_v8_with_policy(
                                question,
                                specialist,
                                ledger,
                                &[],
                                &[],
                                &[],
                                &usage,
                                &basis,
                                &request,
                                &metadata,
                                &[],
                                policy,
                            )
                        } else {
                            InvestigationInput::prepare_v7_with_policy(
                                question,
                                specialist,
                                ledger,
                                &[],
                                &[],
                                &[],
                                &usage,
                                &basis,
                                &request,
                                &metadata,
                                &[],
                                policy,
                            )
                        }
                    }
                }
                .unwrap_or_else(|error| panic!("{auditor}/{version}: {error:?}"));
                let payload = input
                    .action("inv:wire-compatibility".into())
                    .unwrap()
                    .payload;
                let value: Value = serde_json::from_str(&payload.prompt).unwrap();
                // Exact pre-refactor path: prompt JSON becomes a String, then
                // the ordinary CompletionPayload serializer writes that string.
                let reference = CompletionPayload {
                    response_schema: None,
                    system: specialist.prompt(),
                    prompt: String::from_utf8(bounded_bytes(&value, MAX_INPUT_BYTES).unwrap())
                        .unwrap(),
                    spans: payload.spans.clone(),
                };
                let expected = bounded_bytes(&reference, MAX_INPUT_BYTES).unwrap();
                assert_eq!(payload, reference, "{auditor}/{version}");
                assert_eq!(
                    bounded_bytes(&payload, MAX_INPUT_BYTES).unwrap(),
                    expected,
                    "{auditor}/{version}"
                );
                let measured = measure_replacement(&payload, &value, MAX_INPUT_BYTES).unwrap();
                assert_eq!(measured.prompt_bytes, payload.prompt.len());
                assert_eq!(measured.payload_bytes, expected.len());
            }
        }
    }
}
