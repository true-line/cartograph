//! Streaming raw JSON admission before typed packet decoding.
//!
//! The byte ceiling bounds decoder scratch and decoded object-key storage.
//! Preflight retains keys only for duplicate detection and a borrowed path;
//! it never constructs a `serde_json::Value` tree. Domain validation (including
//! the `Known` contract and supported packet schema) remains the typed caller's
//! responsibility, as does bounding acquisition of the raw input itself.

use serde::de::{DeserializeOwned, DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use std::collections::BTreeSet;
use std::fmt;

const MAX_DEPTH: usize = 40;

/// Bounds for raw packet JSON, independent of typed field validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonInputLimits {
    /// Maximum original UTF-8 bytes, including whitespace and escapes.
    pub bytes: usize,
    /// Maximum value depth; the root is zero. Must be at most 40.
    pub depth: usize,
    /// Maximum values, counting both containers and scalar values, not keys.
    pub values: usize,
    /// Maximum keys in each object classified as a protocol object.
    pub protocol_object_keys: usize,
}

impl JsonInputLimits {
    /// SPEC-11 generation-request limits. Raw fact/provenance bodies are not
    /// accepted by the generation request's separately validated typed schema.
    pub const GENERATION_REQUEST: Self = Self {
        bytes: 128 * 1024,
        depth: MAX_DEPTH,
        values: 65_536,
        protocol_object_keys: 64,
    };

    /// SPEC-11 retained-body limits. Use a scoped entry point to exempt the
    /// exact source-authored fact-property subtrees from protocol key counts.
    pub const PACKET_BODY: Self = Self {
        bytes: 896 * 1024,
        ..Self::GENERATION_REQUEST
    };
}

/// The independent resource that exhausted its declared allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonInputLimit {
    /// Original raw UTF-8 bytes.
    Bytes,
    /// Value nesting depth.
    Depth,
    /// Total values, including containers.
    Values,
    /// Keys in one protocol object.
    ProtocolObjectKeys,
}

/// Rejected input, without echoing source-authored text in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JsonInputError {
    /// Malformed/trailing JSON, duplicate decoded keys, typed-schema mismatch,
    /// or invalid limits (a configured depth greater than 40).
    #[error("invalid packet JSON input")]
    InvalidInput,
    /// A declared raw-input or structural limit was exceeded.
    #[error("packet JSON {limit:?} limit exceeded (maximum {maximum})")]
    LimitExceeded {
        /// Resource whose next admission failed.
        limit: JsonInputLimit,
        /// Configured maximum for that resource.
        maximum: usize,
    },
}

/// A decoded object key or an array position in a borrowed input path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonPathSegment<'a> {
    /// Exact decoded key. Unicode normalization is deliberately not applied.
    Key(&'a str),
    /// Zero-based array position.
    Index(usize),
}

/// One segment of a root-relative subtree pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonPathPattern<'a> {
    /// Exact decoded object key.
    Key(&'a str),
    /// One exact array position.
    Index(usize),
    /// Any single array position; never matches an object key.
    AnyIndex,
}

/// A linked borrowed path to the current JSON value. The root has no segment.
/// Object-scope callbacks receive the path of the object being inspected.
#[derive(Debug)]
pub struct JsonPath<'a> {
    parent: Option<&'a JsonPath<'a>>,
    segment: Option<JsonPathSegment<'a>>,
    depth: usize,
}

impl JsonPath<'_> {
    /// Value depth, with the root at zero.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Segments from the current value towards the root, without allocation.
    pub fn segments_rev(&self) -> impl Iterator<Item = JsonPathSegment<'_>> + '_ {
        std::iter::successors(Some(self), |path| path.parent).filter_map(|path| path.segment)
    }

    /// Whether this value is the named subtree root or one of its descendants.
    /// Patterns start at the document root; an empty pattern matches everything.
    /// This lets a caller exempt an exact fact-property envelope, rather than
    /// accidentally exempting every unrelated field named `props`.
    pub fn is_within(&self, pattern: &[JsonPathPattern<'_>]) -> bool {
        if self.depth < pattern.len() {
            return false;
        }
        let mut ancestor = self;
        for _ in pattern.len()..self.depth {
            let Some(parent) = ancestor.parent else {
                return false;
            };
            ancestor = parent;
        }
        for expected in pattern.iter().rev() {
            let matches = match (expected, ancestor.segment) {
                (JsonPathPattern::Key(expected), Some(JsonPathSegment::Key(actual))) => {
                    *expected == actual
                }
                (JsonPathPattern::Index(expected), Some(JsonPathSegment::Index(actual))) => {
                    *expected == actual
                }
                (JsonPathPattern::AnyIndex, Some(JsonPathSegment::Index(_))) => true,
                _ => false,
            };
            if !matches {
                return false;
            }
            let Some(parent) = ancestor.parent else {
                return false;
            };
            ancestor = parent;
        }
        true
    }
}

/// Preflight one complete JSON value, applying protocol key counts everywhere.
/// Duplicate decoded object keys fail, including escaped equivalent spellings.
pub fn preflight_json(json: &str, limits: JsonInputLimits) -> Result<(), JsonInputError> {
    preflight_json_scoped(json, limits, |_| true)
}

/// Preflight with caller-owned protocol-object classification. Returning false
/// exempts only that object's key count; duplicates and byte/depth/value limits
/// still apply. Use an exact root-relative subtree pattern for source properties.
/// This module deliberately does not guess the packet's typed envelope shape.
pub fn preflight_json_scoped(
    json: &str,
    limits: JsonInputLimits,
    mut is_protocol_object: impl FnMut(&JsonPath<'_>) -> bool,
) -> Result<(), JsonInputError> {
    if limits.depth > MAX_DEPTH {
        return Err(JsonInputError::InvalidInput);
    }
    if json.len() > limits.bytes {
        return Err(JsonInputError::LimitExceeded {
            limit: JsonInputLimit::Bytes,
            maximum: limits.bytes,
        });
    }
    let mut decoder = serde_json::Deserializer::from_str(json);
    let root = JsonPath {
        parent: None,
        segment: None,
        depth: 0,
    };
    let mut context = Context {
        limits,
        remaining: limits.values,
        failure: None,
        is_protocol_object: &mut is_protocol_object,
    };
    if (Seed {
        context: &mut context,
        path: &root,
    })
    .deserialize(&mut decoder)
    .is_err()
    {
        return Err(context.failure.unwrap_or(JsonInputError::InvalidInput));
    }
    decoder.end().map_err(|_| JsonInputError::InvalidInput)
}

/// Run strict preflight before entering typed deserialization. The caller must
/// still validate domain requirements that its serde type cannot establish.
pub fn deserialize_json<T: DeserializeOwned>(
    json: &str,
    limits: JsonInputLimits,
) -> Result<T, JsonInputError> {
    deserialize_json_scoped(json, limits, |_| true)
}

/// Typed decoding after scoped raw admission; the same exact raw bytes are
/// decoded only after all duplicate, syntax and structural checks have passed.
pub fn deserialize_json_scoped<T: DeserializeOwned>(
    json: &str,
    limits: JsonInputLimits,
    is_protocol_object: impl FnMut(&JsonPath<'_>) -> bool,
) -> Result<T, JsonInputError> {
    preflight_json_scoped(json, limits, is_protocol_object)?;
    serde_json::from_str(json).map_err(|_| JsonInputError::InvalidInput)
}

struct Context<'a> {
    limits: JsonInputLimits,
    remaining: usize,
    failure: Option<JsonInputError>,
    is_protocol_object: &'a mut dyn FnMut(&JsonPath<'_>) -> bool,
}

impl Context<'_> {
    fn exceeded<E: Error>(&mut self, limit: JsonInputLimit, maximum: usize) -> E {
        self.failure = Some(JsonInputError::LimitExceeded { limit, maximum });
        E::custom("packet JSON limit")
    }
}

struct Seed<'a, 'p, 'scope> {
    context: &'a mut Context<'scope>,
    path: &'p JsonPath<'p>,
}

impl<'de> DeserializeSeed<'de> for Seed<'_, '_, '_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
        if self.path.depth > self.context.limits.depth {
            return Err(self
                .context
                .exceeded(JsonInputLimit::Depth, self.context.limits.depth));
        }
        if self.context.remaining == 0 {
            return Err(self
                .context
                .exceeded(JsonInputLimit::Values, self.context.limits.values));
        }
        self.context.remaining -= 1;
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Seed<'_, '_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded packet JSON")
    }
    fn visit_bool<E: Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> Result<(), A::Error> {
        let mut index = 0;
        loop {
            let child = JsonPath {
                parent: Some(self.path),
                segment: Some(JsonPathSegment::Index(index)),
                depth: self.path.depth + 1,
            };
            if array
                .next_element_seed(Seed {
                    context: self.context,
                    path: &child,
                })?
                .is_none()
            {
                return Ok(());
            }
            index += 1;
        }
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<(), A::Error> {
        let protocol = (self.context.is_protocol_object)(self.path);
        let mut keys = BTreeSet::new();
        while let Some(key) = object.next_key::<String>()? {
            if keys.contains(&key) {
                return Err(A::Error::custom("duplicate packet JSON key"));
            }
            if protocol && keys.len() >= self.context.limits.protocol_object_keys {
                return Err(self.context.exceeded(
                    JsonInputLimit::ProtocolObjectKeys,
                    self.context.limits.protocol_object_keys,
                ));
            }
            let child = JsonPath {
                parent: Some(self.path),
                segment: Some(JsonPathSegment::Key(&key)),
                depth: self.path.depth + 1,
            };
            object.next_value_seed(Seed {
                context: self.context,
                path: &child,
            })?;
            keys.insert(key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn limits() -> JsonInputLimits {
        JsonInputLimits {
            bytes: 4096,
            depth: 8,
            values: 256,
            protocol_object_keys: 64,
        }
    }

    fn exceeded(limit: JsonInputLimit, maximum: usize) -> JsonInputError {
        JsonInputError::LimitExceeded { limit, maximum }
    }

    fn wide_object(keys: usize) -> String {
        format!(
            "{{{}}}",
            (0..keys)
                .map(|key| format!("\"key{key}\":0"))
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    #[test]
    fn raw_bytes_are_charged_exactly_before_decoding() {
        let json = " \"é\"\n";
        let exact = JsonInputLimits {
            bytes: json.len(),
            ..limits()
        };
        assert!(preflight_json(json, exact).is_ok());
        assert_eq!(
            preflight_json(
                json,
                JsonInputLimits {
                    bytes: json.len() - 1,
                    ..exact
                }
            ),
            Err(exceeded(JsonInputLimit::Bytes, json.len() - 1))
        );
        for maximum in [
            JsonInputLimits::GENERATION_REQUEST,
            JsonInputLimits::PACKET_BODY,
        ] {
            let raw = format!("0{}", " ".repeat(maximum.bytes - 1));
            assert!(preflight_json(&raw, maximum).is_ok());
            assert_eq!(
                preflight_json(&(raw + " "), maximum),
                Err(exceeded(JsonInputLimit::Bytes, maximum.bytes))
            );
        }
    }

    #[test]
    fn root_zero_depth_and_container_values_have_exact_boundaries() {
        for json in ["null", "[]", "{}"] {
            assert!(
                preflight_json(
                    json,
                    JsonInputLimits {
                        depth: 0,
                        values: 1,
                        ..limits()
                    }
                )
                .is_ok()
            );
        }
        assert_eq!(
            preflight_json(
                "null",
                JsonInputLimits {
                    values: 0,
                    ..limits()
                }
            ),
            Err(exceeded(JsonInputLimit::Values, 0))
        );
        let json = r#"{"key":[true,null]}"#;
        assert!(
            preflight_json(
                json,
                JsonInputLimits {
                    depth: 2,
                    values: 4,
                    ..limits()
                }
            )
            .is_ok()
        );
        assert_eq!(
            preflight_json(
                json,
                JsonInputLimits {
                    depth: 1,
                    ..limits()
                }
            ),
            Err(exceeded(JsonInputLimit::Depth, 1))
        );
        assert_eq!(
            preflight_json(
                json,
                JsonInputLimits {
                    values: 3,
                    ..limits()
                }
            ),
            Err(exceeded(JsonInputLimit::Values, 3))
        );
        let nested = |depth| format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        assert!(preflight_json(&nested(40), JsonInputLimits::PACKET_BODY).is_ok());
        assert_eq!(
            preflight_json(&nested(41), JsonInputLimits::PACKET_BODY),
            Err(exceeded(JsonInputLimit::Depth, 40))
        );
        assert_eq!(
            preflight_json(
                "null",
                JsonInputLimits {
                    depth: 41,
                    ..limits()
                }
            ),
            Err(JsonInputError::InvalidInput)
        );
    }

    #[test]
    fn default_value_limit_counts_array_items_and_the_container() {
        let array = |items| format!("[{}]", vec!["0"; items].join(","));
        assert!(preflight_json(&array(65_535), JsonInputLimits::PACKET_BODY).is_ok());
        assert_eq!(
            preflight_json(&array(65_536), JsonInputLimits::PACKET_BODY),
            Err(exceeded(JsonInputLimit::Values, 65_536))
        );
    }

    #[test]
    fn protocol_key_bound_is_per_object_and_checked_before_its_next_value() {
        assert!(preflight_json(&wide_object(64), limits()).is_ok());
        assert_eq!(
            preflight_json(&wide_object(65), limits()),
            Err(exceeded(JsonInputLimit::ProtocolObjectKeys, 64))
        );
        assert!(
            preflight_json(
                "{}",
                JsonInputLimits {
                    protocol_object_keys: 0,
                    ..limits()
                }
            )
            .is_ok()
        );
        assert_eq!(
            preflight_json(
                r#"{"first":null}"#,
                JsonInputLimits {
                    protocol_object_keys: 0,
                    ..limits()
                }
            ),
            Err(exceeded(JsonInputLimit::ProtocolObjectKeys, 0))
        );
        let siblings = format!(
            "{{\"first\":{},\"second\":{}}}",
            wide_object(64),
            wide_object(64)
        );
        assert!(preflight_json(&siblings, limits()).is_ok());
    }

    #[test]
    fn duplicate_decoded_keys_fail_even_when_escape_spelling_differs() {
        for json in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":1,"\u0061":2}"#,
            r#"{"a/b":1,"a\/b":2}"#,
            r#"{"😀":1,"\ud83d\ude00":2}"#,
            r#"{"nested":[{"a":1,"\u0061":2}]}"#,
        ] {
            assert_eq!(
                preflight_json(json, limits()),
                Err(JsonInputError::InvalidInput),
                "{json}"
            );
        }
        // Exact strings remain distinct; no NFC/NFD normalization is invented.
        assert!(preflight_json(r#"{"é":1,"e\u0301":2}"#, limits()).is_ok());
    }

    fn protocol(path: &JsonPath<'_>) -> bool {
        !path.is_within(&[
            JsonPathPattern::Key("evidence"),
            JsonPathPattern::Key("facts"),
            JsonPathPattern::AnyIndex,
            JsonPathPattern::Key("fact"),
            JsonPathPattern::Key("value"),
            JsonPathPattern::Key("props"),
        ])
    }

    fn source_properties(props: &str) -> String {
        [
            "{\"evidence\":{\"facts\":[{\"fact\":{\"value\":{\"props\":",
            props,
            "}}}]}}",
        ]
        .concat()
    }

    #[test]
    fn source_property_exemption_only_relaxes_object_key_count() {
        let json = source_properties(&wide_object(70));
        assert_eq!(
            preflight_json(&json, limits()),
            Err(exceeded(JsonInputLimit::ProtocolObjectKeys, 64))
        );
        assert!(preflight_json_scoped(&json, limits(), protocol).is_ok());
        assert!(deserialize_json_scoped::<serde_json::Value>(&json, limits(), protocol).is_ok());
        assert!(
            preflight_json_scoped(
                &source_properties(&format!("{{\"nested\":{}}}", wide_object(70))),
                limits(),
                protocol
            )
            .is_ok()
        );
        let unrelated = format!("{{\"intent\":{{\"props\":{}}}}}", wide_object(70));
        assert_eq!(
            preflight_json_scoped(&unrelated, limits(), protocol),
            Err(exceeded(JsonInputLimit::ProtocolObjectKeys, 64))
        );
        assert_eq!(
            preflight_json_scoped(
                &json,
                JsonInputLimits {
                    values: 12,
                    ..limits()
                },
                protocol
            ),
            Err(exceeded(JsonInputLimit::Values, 12))
        );
        assert_eq!(
            preflight_json_scoped(
                &json,
                JsonInputLimits {
                    depth: 6,
                    ..limits()
                },
                protocol
            ),
            Err(exceeded(JsonInputLimit::Depth, 6))
        );
        assert_eq!(
            preflight_json_scoped(
                &json,
                JsonInputLimits {
                    bytes: json.len() - 1,
                    ..limits()
                },
                protocol
            ),
            Err(exceeded(JsonInputLimit::Bytes, json.len() - 1))
        );
        assert_eq!(
            preflight_json_scoped(
                &source_properties(r#"{"a":1,"\u0061":2}"#),
                limits(),
                protocol
            ),
            Err(JsonInputError::InvalidInput)
        );
    }

    #[test]
    fn path_patterns_are_root_relative_and_indices_are_typed() {
        let mut matched = false;
        preflight_json_scoped(r#"{"items":[{},{}],"0":{}}"#, limits(), |path| {
            if path.is_within(&[JsonPathPattern::Key("items"), JsonPathPattern::Index(1)]) {
                assert_eq!(path.depth(), 2);
                assert_eq!(
                    path.segments_rev().collect::<Vec<_>>(),
                    vec![JsonPathSegment::Index(1), JsonPathSegment::Key("items")]
                );
                matched = true;
            }
            assert!(!path.is_within(&[JsonPathPattern::AnyIndex]));
            true
        })
        .unwrap();
        assert!(matched);
    }

    #[test]
    fn trailing_and_malformed_json_never_disappear_during_preflight() {
        for json in [
            "",
            " ",
            "null true",
            "[]{}",
            "1 2",
            "{",
            "[1,]",
            "{\"a\":}",
            r#""\x""#,
            r#""\ud800""#,
            "NaN",
            "1e400",
            "// comment\n0",
        ] {
            assert_eq!(
                preflight_json(json, limits()),
                Err(JsonInputError::InvalidInput),
                "{json}"
            );
        }
        for json in [
            "null \n",
            " true",
            "-1.25e-2",
            "18446744073709551615",
            r#"{"quoted":"\\\"\n é"}"#,
        ] {
            assert!(preflight_json(json, limits()).is_ok(), "{json}");
        }
    }

    thread_local! {static TYPED_ENTRIES:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};}
    #[derive(Debug)]
    struct TypedWitness;
    impl<'de> Deserialize<'de> for TypedWitness {
        fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
            TYPED_ENTRIES.with(|counter| counter.set(counter.get() + 1));
            serde::de::IgnoredAny::deserialize(decoder)?;
            Ok(Self)
        }
    }

    #[test]
    fn typed_decoding_starts_only_after_complete_raw_admission() {
        for json in [r#"{"a":1,"\u0061":2}"#, "null []", "[null,null,null]"] {
            TYPED_ENTRIES.with(|counter| counter.set(0));
            assert!(
                deserialize_json::<TypedWitness>(
                    json,
                    JsonInputLimits {
                        values: 3,
                        ..limits()
                    }
                )
                .is_err()
            );
            TYPED_ENTRIES.with(|counter| assert_eq!(counter.get(), 0));
        }
        TYPED_ENTRIES.with(|counter| counter.set(0));
        deserialize_json::<TypedWitness>("{\"a\":[]}", limits()).unwrap();
        TYPED_ENTRIES.with(|counter| assert_eq!(counter.get(), 1));
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Protocol {
            value: u64,
        }
        assert_eq!(
            deserialize_json::<Protocol>(r#"{"value":7}"#, limits())
                .unwrap()
                .value,
            7
        );
        assert!(matches!(
            deserialize_json::<Protocol>(r#"{"value":7,"body":{}}"#, limits()),
            Err(JsonInputError::InvalidInput)
        ));
    }
}
