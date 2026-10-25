use super::{FixtureError, MAX_MANIFEST_BYTES, Result};
use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use std::{fmt, io::Write};
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}
/// Integer-only RFC8785 with ASCII schema keys and ordinary JSON string escaping.
pub fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut writer = BoundedWriter(Vec::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| FixtureError::Capacity)?;
    let StrictValue(v) = serde_json::from_slice(&writer.0)
        .map_err(|_| FixtureError::Invalid("canonical integer-only shape"))?;
    let mut out = Vec::new();
    emit(&v, &mut out)?;
    Ok(out)
}
struct BoundedWriter(Vec<u8>);
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_MANIFEST_BYTES {
            return Err(std::io::Error::other("fixture canonical capacity"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn emit(v: &Value, out: &mut Vec<u8>) -> Result<()> {
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n)
            if n.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991)
                || n.as_i64()
                    .is_some_and(|n| n.unsigned_abs() <= 9_007_199_254_740_991) =>
        {
            out.extend_from_slice(n.to_string().as_bytes())
        }
        Value::Number(_) => return Err(FixtureError::Invalid("noninteger number")),
        Value::String(s) => serde_json::to_writer(&mut *out, s)
            .map_err(|_| FixtureError::Invalid("string encoding"))?,
        Value::Array(a) => {
            out.push(b'[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                emit(v, out)?;
            }
            out.push(b']');
        }
        Value::Object(m) => {
            out.push(b'{');
            let mut entries: Vec<_> = m.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if !k.is_ascii() {
                    return Err(FixtureError::Invalid("non-ASCII schema key"));
                }
                if i > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, k)
                    .map_err(|_| FixtureError::Invalid("key encoding"))?;
                out.push(b':');
                emit(v, out)?;
            }
            out.push(b'}');
        }
    }
    if out.len() > MAX_MANIFEST_BYTES {
        return Err(FixtureError::Capacity);
    }
    Ok(())
}
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = StrictValue;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("bounded integer-only JSON without duplicate keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(v.into())))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(Number::from(v))))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Err(E::custom("noninteger number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                if v.chars().count() > 8192 {
                    return Err(E::custom("string bound"));
                }
                self.visit_string(v.to_owned())
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
                if v.chars().count() > 8192 {
                    return Err(E::custom("string bound"));
                }
                Ok(StrictValue(Value::String(v)))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(StrictValue(v)) = a.next_element()? {
                    if values.len() >= 1024 {
                        return Err(de::Error::custom("array bound"));
                    }
                    values.push(v);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut m = Map::new();
                while let Some(k) = a.next_key::<String>()? {
                    if m.len() >= 16 || !k.is_ascii() || k.len() > 64 || m.contains_key(&k) {
                        return Err(de::Error::custom("invalid or duplicate key"));
                    }
                    let StrictValue(v) = a.next_value()?;
                    m.insert(k, v);
                }
                Ok(StrictValue(Value::Object(m)))
            }
        }
        d.deserialize_any(V)
    }
}
#[cfg(test)]
pub(super) fn decode<T: serde::de::DeserializeOwned + Serialize>(
    bytes: &[u8],
    definition: &str,
) -> Result<T> {
    decode_for_purpose(bytes, definition, super::ExpectedPurpose::VisibleMechanism)
}
pub(super) fn decode_for_purpose<T: serde::de::DeserializeOwned + Serialize>(
    bytes: &[u8],
    definition: &str,
    purpose: super::ExpectedPurpose,
) -> Result<T> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(FixtureError::Capacity);
    }
    let StrictValue(raw) =
        serde_json::from_slice(bytes).map_err(|_| FixtureError::Invalid("raw JSON"))?;
    super::validation::shape(&raw, definition, purpose)?;
    let typed: T = serde_json::from_value(raw.clone())
        .map_err(|_| FixtureError::Invalid("closed fixture shape"))?;
    // Option normally accepts absent keys. Full serialized shape equality requires
    // every nullable property, including FamilyKey.subtype, to be present.
    if serde_json::to_value(&typed).map_err(|_| FixtureError::Invalid("fixture shape"))? != raw {
        return Err(FixtureError::Invalid("required nullable field"));
    }
    Ok(typed)
}
