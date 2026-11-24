//! Input closure records (ADR-0072 decision 1, slice 5, #385).
//!
//! An [`InputClosureV1`] names every input whose change could change one
//! emitted fact's bytes, or whether it is emitted at all, under one producer
//! contract. It is immutable metadata: no source text, no roots and no
//! timestamps. Its identity is BLAKE3 over a fixed domain and the canonical
//! JSON of the record without `closure_id`, the encoding SPEC-06 uses for
//! receipt ids. An established closure is not business truth and never
//! changes tier or confidence.

use super::CapturedError;
use core_graph::source::FactKey;
use serde::{Deserialize, Serialize};
use source_capture::CaptureFileRef;
use source_capture::SourceId;
use std::collections::BTreeMap;

/// Closure interpretation version.
pub const CLOSURE_SCHEMA_VERSION: u32 = 1;
/// Identity prefix of a closure id.
pub const CLOSURE_ID_PREFIX: &str = "closure-v1:";
/// Hash domain of a closure id; the id field is excluded from its preimage.
const CLOSURE_DOMAIN: &[u8] = b"cartograph:input-closure:v1\0";
/// Most inputs one closure may bind. A closure over more is not established,
/// with reason `limit_exceeded`; it is never truncated.
pub const MAX_CLOSURE_INPUTS: usize = 4096;
/// Largest canonical closure accepted by the decoder, id included.
pub const MAX_CLOSURE_BYTES: usize = 2 * 1024 * 1024;
/// Most exact package pins one closure may record.
const MAX_PACKAGES: usize = 16;
/// Most layer hints one closure may record.
const MAX_LAYER_HINTS: usize = 64;
/// Longest identifier-like string a closure may carry.
const MAX_TEXT_BYTES: usize = 4096;

/// Why an emitted fact's closure is not established. The codes are fixed by
/// ADR-0072; their order is the sorted order a receipt lists them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosureGapReason {
    /// The producer or fact kind does not take part in closure.
    ProducerNotParticipating,
    /// A file outside the parsed one may change the fact and is not bound.
    CrossFileInputUnbound,
    /// An external input may change the fact and is not bound.
    ExternalInputUnbound,
    /// A registry or remote reference, never fetched.
    RemoteReference,
    /// An upstream fact's own closure is not established.
    UpstreamClosureIncomplete,
    /// A later enrichment changed the fact after the producer emitted it.
    EnrichmentChangedFact,
    /// A cached result was reused under a key other than this closure.
    CacheReuseUnproven,
    /// The closure would exceed a declared bound.
    LimitExceeded,
}

/// The kind of an external evidence capture (ADR-0072 decision 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalMedia {
    /// `terraform show -json` output.
    TerraformShowJson,
    /// A Pulumi stack export.
    PulumiJson,
    /// OTLP JSON lines.
    OtlpJsonLines,
    /// A captured system manifest.
    SystemManifest,
}

/// Typed host inputs that can change a fact. No roots and no timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosureParameters {
    /// The registered repository key the producer recorded.
    pub repo_key: String,
    /// The source-walk policy version in effect.
    pub walk_policy: String,
    /// The layer hints in effect, sorted and unique.
    pub layer_hints: Vec<String>,
    /// The grammar selection rule version.
    pub grammar_rule: String,
}

/// Whether an upstream fact's closure is established.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClosureRef {
    /// The upstream fact's established closure.
    Established {
        /// Its closure id.
        closure_id: String,
    },
    /// The upstream fact has no established closure.
    NotEstablished,
}

/// One bound input of a closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClosureInput {
    /// Exact retained bytes the producer read.
    CapturedFile {
        /// The retained file.
        file: CaptureFileRef,
    },
    /// Membership of an enumerated selection: the capture manifest digest
    /// plus the walk policy that produced it, including skipped symlinks.
    Enumeration {
        /// The enumerated source.
        source_id: SourceId,
        /// Its capture manifest digest.
        capture_id: String,
        /// The walk policy that produced the membership.
        walk_policy: String,
    },
    /// A path the producer looked up and did not find in that enumeration.
    /// Absence is relative to the enumeration, never "absent on disk".
    AbsentFromEnumeration {
        /// The enumerated source.
        source_id: SourceId,
        /// Its capture manifest digest.
        capture_id: String,
        /// The relative path looked up.
        path: String,
    },
    /// A value read from a captured system manifest, by capture file and a
    /// fixed field path.
    Declared {
        /// The captured manifest.
        file: CaptureFileRef,
        /// The fixed field path read.
        field: String,
    },
    /// An external evidence capture.
    External {
        /// The captured evidence.
        file: CaptureFileRef,
        /// Its media.
        media: ExternalMedia,
    },
    /// A fact this producer consumed, with its own closure.
    UpstreamFact {
        /// The consumed fact.
        fact: FactKey,
        /// Its complete fact digest.
        fact_digest: String,
        /// Its closure.
        closure: ClosureRef,
    },
}

/// The immutable closure record of one emitted fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputClosureV1 {
    /// Exactly [`CLOSURE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// `closure-v1:` plus BLAKE3 of the canonical record without this field.
    pub closure_id: String,
    /// The producer contract, e.g. `t0.adapter-ts/direct-lexical-v4`.
    pub producer_contract: String,
    /// Exact package pins, as receipts record them; sorted and unique.
    pub producer_packages: Vec<String>,
    /// Typed host inputs.
    pub parameters: ClosureParameters,
    /// Bound inputs, in canonical order and unique.
    pub inputs: Vec<ClosureInput>,
}

impl InputClosureV1 {
    /// Build a closure: packages, hints and inputs are put in canonical order
    /// and deduplicated, then the id is computed and the record checked.
    pub fn new(
        producer_contract: impl Into<String>,
        mut producer_packages: Vec<String>,
        mut parameters: ClosureParameters,
        inputs: Vec<ClosureInput>,
    ) -> Result<Self, CapturedError> {
        producer_packages.sort();
        producer_packages.dedup();
        parameters.layer_hints.sort();
        parameters.layer_hints.dedup();
        if inputs.len() > MAX_CLOSURE_INPUTS {
            return Err(CapturedError::Limit("closure inputs"));
        }
        let mut keyed = inputs
            .into_iter()
            .map(|input| Ok((input_key(&input)?, input)))
            .collect::<Result<Vec<_>, CapturedError>>()?;
        keyed.sort_by(|left, right| left.0.cmp(&right.0));
        keyed.dedup_by(|left, right| left.0 == right.0);
        let mut closure = Self {
            schema_version: CLOSURE_SCHEMA_VERSION,
            closure_id: String::new(),
            producer_contract: producer_contract.into(),
            producer_packages,
            parameters,
            inputs: keyed.into_iter().map(|(_, input)| input).collect(),
        };
        closure.closure_id = closure.compute_id()?;
        closure.validate()?;
        Ok(closure)
    }

    /// Canonical identity: BLAKE3 over the domain and the canonical JSON of
    /// the record without `closure_id` (sorted keys, no whitespace).
    pub fn compute_id(&self) -> Result<String, CapturedError> {
        let mut value = serde_json::to_value(self).map_err(|_| invalid())?;
        value
            .as_object_mut()
            .ok_or_else(invalid)?
            .remove("closure_id");
        let bytes = serde_json::to_vec(&canonical(value)).map_err(|_| invalid())?;
        if bytes.len() + CLOSURE_ID_PREFIX.len() + 64 + ",\"closure_id\":\"\"".len()
            > MAX_CLOSURE_BYTES
        {
            return Err(CapturedError::Limit("closure size"));
        }
        let mut input = CLOSURE_DOMAIN.to_vec();
        input.extend(bytes);
        Ok(format!(
            "{CLOSURE_ID_PREFIX}{}",
            core_prov::content_hash(&input)
        ))
    }

    /// Canonical stored bytes (sorted keys, no whitespace), id included.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CapturedError> {
        serde_json::to_vec(&canonical(
            serde_json::to_value(self).map_err(|_| invalid())?,
        ))
        .map_err(|_| invalid())
    }

    /// Strict decode: unknown fields, a wrong id, inputs out of canonical
    /// order or repeated, or any broken invariant is rejected.
    pub fn decode(bytes: &[u8]) -> Result<Self, CapturedError> {
        if bytes.len() > MAX_CLOSURE_BYTES {
            return Err(CapturedError::Limit("closure size"));
        }
        let closure: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        closure.validate()?;
        Ok(closure)
    }

    /// The reasons this record alone does not establish its fact's closure:
    /// `upstream_closure_incomplete` when any upstream fact's closure is not
    /// established. Empty when nothing in the record prevents it. The
    /// producer still decides whether every consulted input was bound.
    pub fn gaps(&self) -> Vec<ClosureGapReason> {
        let incomplete = self.inputs.iter().any(|input| {
            matches!(
                input,
                ClosureInput::UpstreamFact {
                    closure: ClosureRef::NotEstablished,
                    ..
                }
            )
        });
        if incomplete {
            vec![ClosureGapReason::UpstreamClosureIncomplete]
        } else {
            Vec::new()
        }
    }

    /// Check that this record can be the closure of `fact`: a fact is never
    /// its own upstream.
    pub fn validate_for(&self, fact: &FactKey) -> Result<(), CapturedError> {
        let own = self.inputs.iter().any(|input| {
            matches!(input, ClosureInput::UpstreamFact { fact: upstream, .. } if upstream == fact)
        });
        if own {
            return Err(CapturedError::Invalid("closure upstream"));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), CapturedError> {
        let p = &self.parameters;
        let text = |value: &str| !value.is_empty() && value.len() <= MAX_TEXT_BYTES;
        if self.schema_version != CLOSURE_SCHEMA_VERSION
            || !text(&self.producer_contract)
            || self.producer_packages.is_empty()
            || self.producer_packages.len() > MAX_PACKAGES
            || !self.producer_packages.iter().all(|package| text(package))
            || !strictly_sorted(&self.producer_packages)
            || !text(&p.repo_key)
            || !text(&p.walk_policy)
            || !text(&p.grammar_rule)
            || p.layer_hints.len() > MAX_LAYER_HINTS
            || !p.layer_hints.iter().all(|hint| text(hint))
            || !strictly_sorted(&p.layer_hints)
        {
            return Err(CapturedError::Invalid("closure header"));
        }
        if self.inputs.is_empty() || self.inputs.len() > MAX_CLOSURE_INPUTS {
            return Err(CapturedError::Invalid("closure inputs"));
        }
        let keys = self
            .inputs
            .iter()
            .map(input_key)
            .collect::<Result<Vec<_>, _>>()?;
        if !strictly_sorted(&keys) {
            return Err(CapturedError::Invalid("closure input order"));
        }
        for input in &self.inputs {
            self.validate_input(input)?;
        }
        if self.closure_id != self.compute_id()? {
            return Err(CapturedError::Invalid("closure id"));
        }
        Ok(())
    }

    fn validate_input(&self, input: &ClosureInput) -> Result<(), CapturedError> {
        let text = |value: &str| !value.is_empty() && value.len() <= MAX_TEXT_BYTES;
        // A file binding names retained bytes as receipts do: a versioned
        // capture id, a full-file digest, a selected path and a bounded length.
        let file_ok = |file: &CaptureFileRef| {
            hex_with_prefix(&file.capture_id, "capture-v1:")
                && hex_with_prefix(&file.digest, "")
                && file.byte_len <= source_capture::MAX_FILE_BYTES
                && super::valid_path(&file.path)
        };
        let valid = match input {
            ClosureInput::CapturedFile { file } | ClosureInput::External { file, .. } => {
                file_ok(file)
            }
            ClosureInput::Declared { file, field } => file_ok(file) && text(field),
            ClosureInput::Enumeration {
                capture_id,
                walk_policy,
                ..
            } => hex_with_prefix(capture_id, "capture-v1:") && text(walk_policy),
            // Absence is relative to an enumeration this closure binds.
            ClosureInput::AbsentFromEnumeration {
                source_id,
                capture_id,
                path,
            } => {
                hex_with_prefix(capture_id, "capture-v1:")
                    && super::valid_path(path)
                    && self.inputs.iter().any(|other| {
                        matches!(other, ClosureInput::Enumeration {
                            source_id: s, capture_id: c, ..
                        } if s == source_id && c == capture_id)
                    })
            }
            ClosureInput::UpstreamFact {
                fact,
                fact_digest,
                closure,
            } => {
                // The fact key and its complete digest, as graph facts carry
                // them, so a consumer can match the binding to the fact bytes.
                let (ids, prefix) = match fact {
                    FactKey::Node { id } => (vec![id.as_str()], "node-v1:"),
                    FactKey::Edge {
                        source,
                        label,
                        destination,
                    } => (
                        vec![source.as_str(), label.as_str(), destination.as_str()],
                        "edge-v1:",
                    ),
                };
                ids.iter()
                    .all(|id| text(id) && !id.chars().any(char::is_control))
                    && hex_with_prefix(fact_digest, prefix)
                    && match closure {
                        ClosureRef::Established { closure_id } => is_closure_id(closure_id),
                        ClosureRef::NotEstablished => true,
                    }
            }
        };
        if valid {
            Ok(())
        } else {
            Err(CapturedError::Invalid("closure input"))
        }
    }
}

/// Whether `value` is a well-formed closure id.
pub fn is_closure_id(value: &str) -> bool {
    hex_with_prefix(value, CLOSURE_ID_PREFIX)
}

/// `prefix` followed by 64 lowercase hex digits.
fn hex_with_prefix(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// An input's canonical order key: its canonical JSON bytes.
fn input_key(input: &ClosureInput) -> Result<Vec<u8>, CapturedError> {
    serde_json::to_vec(&canonical(
        serde_json::to_value(input).map_err(|_| invalid())?,
    ))
    .map_err(|_| invalid())
}

fn strictly_sorted<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn invalid() -> CapturedError {
    CapturedError::Invalid("closure serialization")
}

fn canonical(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical).collect())
        }
        other => other,
    }
}

#[cfg(test)]
#[path = "closure_tests.rs"]
mod tests;
