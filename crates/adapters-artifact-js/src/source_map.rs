//! Source map v3 parsing: the `sourceMappingURL` comment of a bundle, an
//! inline `data:` map, and the sources and contents a map carries,
//! including those of an index map's sections. Mappings are not decoded:
//! the reader needs which original sources a bundle holds and their text,
//! not the position of each generated token.

use base64::Engine as _;

/// One original source a map names: its URL joined with the map's
/// `sourceRoot`, and its text when the map carries `sourcesContent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedSource {
    /// Source URL as the map spells it, after `sourceRoot`.
    pub url: String,
    /// The original text, if carried.
    pub content: Option<String>,
}

/// Why a map was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapRefusal {
    /// Not JSON, not an object, or `sources` is not a list of strings.
    Malformed,
    /// A `version` other than 3.
    Version,
    /// An index map section that names its map by URL instead of
    /// embedding it.
    SectionUrl,
}

impl MapRefusal {
    /// Stable Gap reason code.
    pub fn code(self) -> &'static str {
        match self {
            MapRefusal::Malformed => "bad_map",
            MapRefusal::Version => "map_version",
            MapRefusal::SectionUrl => "section_url",
        }
    }
}

/// The URL of the last `//# sourceMappingURL=` (or legacy `//@`) comment
/// on a line of its own among the bundle's trailing lines, or `None`.
pub fn mapping_url(bundle: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bundle);
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rest = line
            .strip_prefix("//# sourceMappingURL=")
            .or_else(|| line.strip_prefix("//@ sourceMappingURL="))?;
        let url = rest.split_whitespace().next().unwrap_or_default();
        return (!url.is_empty()).then(|| url.to_string());
    }
    None
}

/// The bytes of a `data:` URL, base64 or percent-encoded; `None` for
/// another URL. `Some(Err(()))` for a `data:` URL that does not decode.
pub fn data_url(url: &str) -> Option<Result<Vec<u8>, ()>> {
    let rest = url.strip_prefix("data:")?;
    let Some((meta, payload)) = rest.split_once(',') else {
        return Some(Err(()));
    };
    if meta.split(';').any(|param| param == "base64") {
        return Some(
            base64::engine::general_purpose::STANDARD
                .decode(payload.trim())
                .map_err(|_| ()),
        );
    }
    Some(percent_decode(payload))
}

fn percent_decode(text: &str) -> Result<Vec<u8>, ()> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).ok_or(())?;
            let hex = std::str::from_utf8(hex).map_err(|_| ())?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| ())?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// The sources of a v3 map, sections flattened in order.
pub fn sources(map: &[u8]) -> Result<Vec<MappedSource>, MapRefusal> {
    let value: serde_json::Value =
        serde_json::from_slice(map).map_err(|_| MapRefusal::Malformed)?;
    let mut out = Vec::new();
    collect(&value, &mut out)?;
    Ok(out)
}

fn collect(map: &serde_json::Value, out: &mut Vec<MappedSource>) -> Result<(), MapRefusal> {
    let object = map.as_object().ok_or(MapRefusal::Malformed)?;
    if object.get("version").and_then(serde_json::Value::as_u64) != Some(3) {
        return Err(MapRefusal::Version);
    }
    if let Some(sections) = object.get("sections") {
        for section in sections.as_array().ok_or(MapRefusal::Malformed)? {
            if section.get("url").is_some() {
                return Err(MapRefusal::SectionUrl);
            }
            collect(section.get("map").ok_or(MapRefusal::Malformed)?, out)?;
        }
        return Ok(());
    }
    let root = match object.get("sourceRoot") {
        None | Some(serde_json::Value::Null) => "",
        Some(value) => value.as_str().ok_or(MapRefusal::Malformed)?,
    };
    let urls = object
        .get("sources")
        .and_then(serde_json::Value::as_array)
        .ok_or(MapRefusal::Malformed)?;
    let contents = object
        .get("sourcesContent")
        .and_then(serde_json::Value::as_array);
    for (i, url) in urls.iter().enumerate() {
        // A null source has no name to recover it under.
        let Some(url) = url.as_str() else {
            if url.is_null() {
                continue;
            }
            return Err(MapRefusal::Malformed);
        };
        let url = if root.is_empty() || url.contains("://") || url.starts_with('/') {
            url.to_string()
        } else if root.ends_with('/') {
            format!("{root}{url}")
        } else {
            format!("{root}/{url}")
        };
        let content = contents
            .and_then(|contents| contents.get(i))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        out.push(MappedSource { url, content });
    }
    Ok(())
}
