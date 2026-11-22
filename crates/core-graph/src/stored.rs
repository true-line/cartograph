//! Payloads a reader holds as their compact JSON text (#2215). A decoded
//! JSON object costs a B-tree leaf of some 620 bytes however few keys it
//! has, so a base read only to compile an export can hold a large payload
//! as text, about a sixth of the memory. The text is marked, so a string
//! the store itself holds is never taken for one: it reads as it is.

use serde_json::Value;
use std::borrow::Cow;

/// The first character of a payload held as text: a Unicode noncharacter,
/// which no JSON text starts with.
pub const COMPACT_MARK: char = '\u{FDD0}';

/// `value` as its marked compact JSON text, when it is an object. Anything
/// else stays as it is.
pub fn compact_object(value: &mut Value) {
    if !value.is_object() {
        return;
    }
    let mut text = String::from(COMPACT_MARK);
    text.push_str(&serde_json::to_string(value).expect("a JSON value serializes"));
    text.shrink_to_fit();
    *value = Value::String(text);
}

/// [`compact_object`] for a fact's own `prov`, and only when its text needs
/// no escape, so a reader can borrow every string of the text (#2215).
pub fn compact_borrowable(value: &mut Value) {
    if !value.is_object() {
        return;
    }
    let text = serde_json::to_string(value).expect("a JSON value serializes");
    if text.contains('\\') {
        return;
    }
    let mut marked = String::with_capacity(COMPACT_MARK.len_utf8() + text.len());
    marked.push(COMPACT_MARK);
    marked.push_str(&text);
    *value = Value::String(marked);
}

/// A fact's own `prov` as the store holds it ([`stored`]). Every reader of
/// a fact's provenance that may see a compacted read goes through here.
pub fn stored_prov(props: &Value) -> Cow<'_, Value> {
    stored(&props["prov"])
}

/// The props with a compacted `prov` decoded back, for a reader that hashes
/// or prints them whole.
pub fn restore_prov(props: &mut Value) {
    if let Cow::Owned(prov) = stored_prov(props) {
        props["prov"] = prov;
    }
}

/// The JSON text of a payload held by [`compact_object`], or `None` for any
/// value the store holds as it is, a string included.
pub fn compact_text(value: &Value) -> Option<&str> {
    value.as_str()?.strip_prefix(COMPACT_MARK)
}

/// A payload as the store holds it: decoded when it is held as text,
/// borrowed otherwise.
pub fn stored(value: &Value) -> Cow<'_, Value> {
    match compact_text(value).map(serde_json::from_str) {
        Some(Ok(decoded)) => Cow::Owned(decoded),
        _ => Cow::Borrowed(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_compacted_object_reads_as_stored_and_a_stored_string_as_itself() {
        let object = json!({"kind": "rule", "nested": {"display": "a\"b"}});
        let mut held = object.clone();
        compact_object(&mut held);
        assert!(compact_text(&held).is_some());
        assert_eq!(*stored(&held), object);
        // A string the store holds, even one whose text is JSON, is itself.
        for value in [
            json!("text"),
            json!(object.to_string()),
            json!([1]),
            json!(null),
        ] {
            let mut kept = value.clone();
            compact_object(&mut kept);
            assert_eq!(kept, value);
            assert!(compact_text(&kept).is_none());
            assert_eq!(*stored(&kept), value);
        }
    }

    // AC-1208 (T-1208): only a `prov` whose text needs no escape is compacted.
    #[test]
    fn a_prov_is_compacted_only_when_its_text_needs_no_escape() {
        let stored =
            json!({"name": "a", "prov": {"tier": "Deterministic", "evidence": [{"path": "a.ts"}]}});
        let mut props = stored.clone();
        compact_borrowable(&mut props["prov"]);
        assert!(compact_text(&props["prov"]).is_some());
        assert_eq!(*stored_prov(&props), stored["prov"]);
        restore_prov(&mut props);
        assert_eq!(props, stored);
        let mut escaped = json!({"path": "a\"b"});
        compact_borrowable(&mut escaped);
        assert!(escaped.is_object());
    }
}
