//! Project the sole raw JSON fragment in a bounded OpenAI request (its
//! response schema) directly into the capped request writer.
use serde::Deserializer;
use serde::de::{DeserializeSeed, Error, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;
use std::{fmt, io};

pub(crate) struct OpenAiSchemaFormatter;
impl serde_json::ser::Formatter for OpenAiSchemaFormatter {
    fn write_raw_fragment<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        raw: &str,
    ) -> io::Result<()> {
        if !raw.contains("uniqueItems") && !raw.contains('\\') {
            return w.write_all(raw.as_bytes());
        }
        Node {
            writer: w,
            names: false,
        }
        .deserialize(&mut serde_json::Deserializer::from_str(raw))
        .map_err(io::Error::other)
    }
}

// In properties and definitions maps the keys are names, even when a name
// happens to be "uniqueItems". Literal instance values remain opaque.
struct Node<'a, W: ?Sized> {
    writer: &'a mut W,
    names: bool,
}

impl<'de, W: ?Sized + io::Write> DeserializeSeed<'de> for Node<'_, W> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de, W: ?Sized + io::Write> Visitor<'de> for Node<'_, W> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a schema, named schema map, or schema list")
    }
    fn visit_bool<E: Error>(self, value: bool) -> Result<(), E> {
        self.writer
            .write_all(if value { b"true" } else { b"false" })
            .map_err(E::custom)
    }
    // Legacy dependencies maps allow an array of property names instead of a schema.
    fn visit_str<E: Error>(self, value: &str) -> Result<(), E> {
        serde_json::to_writer(self.writer, value).map_err(E::custom)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        self.writer.write_all(b"{").map_err(A::Error::custom)?;
        let mut first = true;
        // Bounded preparation has already rejected all escaped schema text.
        while let Some(key) = map.next_key::<&str>()? {
            if !self.names && key == "uniqueItems" {
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            if !first {
                self.writer.write_all(b",").map_err(A::Error::custom)?;
            }
            first = false;
            serde_json::to_writer(&mut *self.writer, key).map_err(A::Error::custom)?;
            self.writer.write_all(b":").map_err(A::Error::custom)?;
            if self.names
                || matches!(
                    key,
                    "items"
                        | "additionalItems"
                        | "allOf"
                        | "anyOf"
                        | "oneOf"
                        | "prefixItems"
                        | "contains"
                        | "not"
                        | "if"
                        | "then"
                        | "else"
                        | "additionalProperties"
                        | "unevaluatedItems"
                        | "unevaluatedProperties"
                        | "propertyNames"
                        | "contentSchema"
                )
            {
                map.next_value_seed(Node {
                    writer: self.writer,
                    names: false,
                })?;
            } else if matches!(
                key,
                "properties"
                    | "patternProperties"
                    | "$defs"
                    | "definitions"
                    | "dependentSchemas"
                    | "dependencies"
            ) {
                map.next_value_seed(Node {
                    writer: self.writer,
                    names: true,
                })?;
            } else {
                let value: &RawValue = map.next_value()?;
                self.writer
                    .write_all(value.get().as_bytes())
                    .map_err(A::Error::custom)?;
            }
        }
        self.writer.write_all(b"}").map_err(A::Error::custom)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        self.writer.write_all(b"[").map_err(A::Error::custom)?;
        // Peek via a seed so the comma is emitted only for a present element.
        let mut first = true;
        while seq
            .next_element_seed(Element {
                writer: self.writer,
                first,
            })?
            .is_some()
        {
            first = false;
        }
        self.writer.write_all(b"]").map_err(A::Error::custom)
    }
}

struct Element<'a, W: ?Sized> {
    writer: &'a mut W,
    first: bool,
}
impl<'de, W: ?Sized + io::Write> DeserializeSeed<'de> for Element<'_, W> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        if !self.first {
            self.writer.write_all(b",").map_err(D::Error::custom)?;
        }
        Node {
            writer: self.writer,
            names: false,
        }
        .deserialize(d)
    }
}
