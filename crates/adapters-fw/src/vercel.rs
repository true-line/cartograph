//! `vercel.json` rewrites, read the same way by every language adapter that
//! recovers Vercel functions (#2010): Go (`api/**/*.go`) and TypeScript or
//! JavaScript (`api/**/*.{ts,js,…}`).

/// One `rewrites` entry: its `source`, its `destination`, the destination's
/// path without query, fragment or trailing `/` (`target`), and the byte span
/// of its `source` string in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    /// The rewrite's `source` pattern.
    pub source: String,
    /// The rewrite's `destination`, as written.
    pub destination: String,
    /// The destination's path, without query, fragment or trailing `/`.
    pub target: String,
    /// Where the `source` string starts in the file.
    pub start: usize,
    /// Where it ends.
    pub end: usize,
}

/// Every rewrite of a `vercel.json`, in order. Nothing for a file that is
/// not JSON or has no `rewrites` array.
pub fn rewrites(config: &[u8]) -> Vec<Rewrite> {
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(config) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(config);
    let mut searched_from = 0;
    let mut out = Vec::new();
    for rewrite in parsed["rewrites"].as_array().into_iter().flatten() {
        let (Some(source), Some(destination)) =
            (rewrite["source"].as_str(), rewrite["destination"].as_str())
        else {
            continue;
        };
        // Cite the rewrite's own `source` entry in `vercel.json`: the next
        // `"source"` key whose value is this string, so an equal string in
        // another field (an earlier destination) is never the one cited.
        let needle = serde_json::to_string(source).expect("string serializes");
        let start = source_value(&text, searched_from, &needle)
            .or_else(|| source_value(&text, 0, &needle))
            .unwrap_or(0);
        searched_from = start + needle.len();
        let target = destination.split(['?', '#']).next().unwrap_or(destination);
        out.push(Rewrite {
            source: source.to_owned(),
            destination: destination.to_owned(),
            target: target.trim_end_matches('/').to_owned(),
            start,
            end: start + needle.len().max(1),
        });
    }
    out
}

/// Where the value `needle` of a `"source"` key starts in `text`, at or
/// after `from`.
fn source_value(text: &str, from: usize, needle: &str) -> Option<usize> {
    const KEY: &str = "\"source\"";
    let mut at = from;
    while let Some(found) = text.get(at..)?.find(KEY) {
        let after = at + found + KEY.len();
        let rest = &text[after..];
        let value = rest.trim_start().strip_prefix(':').map(str::trim_start);
        if let Some(value) = value
            && value.starts_with(needle)
        {
            return Some(text.len() - value.len());
        }
        at = after;
    }
    None
}

/// A `vercel.json` rewrite source as concrete route paths: `(a|b)` gives
/// one path per alternative, `:name` is `{name}`, a trailing `:name*` or
/// `(.*)` is `{path...}`. `None` for any other pattern syntax.
pub fn source_paths(source: &str) -> Option<Vec<String>> {
    if !source.starts_with('/') {
        return None;
    }
    if source == "/" {
        return Some(vec!["/".to_owned()]);
    }
    let mut paths = vec![String::new()];
    // Split on `/` outside parentheses: `(revoke|admin/migrate)` is one.
    let mut segments: Vec<&str> = Vec::new();
    let (mut depth, mut start) = (0usize, 1);
    for (i, c) in source.char_indices().skip(1) {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            '/' if depth == 0 => {
                segments.push(&source[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    segments.push(&source[start..]);
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        let options: Vec<String> = if *segment == "(.*)" && last {
            vec!["{path...}".to_owned()]
        } else if let Some(name) = segment.strip_prefix(':') {
            let (name, rest) = match name.strip_suffix('*') {
                Some(name) if last => (name, "..."),
                Some(_) => return None,
                None => (name, ""),
            };
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return None;
            }
            vec![format!("{{{name}{rest}}}")]
        } else if let Some(inner) = segment.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            let options: Vec<String> = inner.split('|').map(str::to_owned).collect();
            if options.iter().any(|o| !literal(o)) {
                return None;
            }
            options
        } else if literal(segment) {
            vec![(*segment).to_owned()]
        } else {
            return None;
        };
        paths = paths
            .iter()
            .flat_map(|path| options.iter().map(move |o| format!("{path}/{o}")))
            .collect();
        if paths.len() > MAX_REWRITE_PATHS {
            return None;
        }
    }
    Some(paths)
}

const MAX_REWRITE_PATHS: usize = 64;

/// A plain path text: segments of letters, digits and `-_.~`, possibly
/// several joined by `/` inside an alternative.
fn literal(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '/'))
}

/// Extensions of the JavaScript and TypeScript function files the TS
/// adapter recovers under `api/`.
pub const NODE_FUNCTION_EXTENSIONS: [&str; 4] = ["ts", "tsx", "js", "jsx"];

/// The path Vercel serves the repository file `rel` at when it is a
/// JavaScript or TypeScript function: under the top-level `api/`, with a
/// [`NODE_FUNCTION_EXTENSIONS`] extension, not a declaration (`x.d.ts`) and
/// under no `_`- or `.`-prefixed segment, which Vercel does not serve.
/// `api/x.ts` and `api/x/index.ts` are both `/api/x`. Both the TypeScript
/// adapter, which recovers the function, and the Go adapter, which leaves a
/// rewrite onto it alone, decide with this.
pub fn node_function_route(rel: &str) -> Option<String> {
    let rest = rel.strip_prefix("api/")?;
    let (stem, ext) = rest.rsplit_once('.')?;
    if !NODE_FUNCTION_EXTENSIONS.contains(&ext)
        || stem.ends_with(".d")
        || rest
            .split('/')
            .any(|s| s.is_empty() || s.starts_with(['_', '.']))
    {
        return None;
    }
    let route = stem.strip_suffix("/index").unwrap_or(stem);
    Some(if route == "index" {
        "/api".to_owned()
    } else {
        format!("/api/{route}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_sources_are_route_paths() {
        assert_eq!(
            source_paths("/devices/(revoke|tags|admin/migrate)").unwrap(),
            ["/devices/revoke", "/devices/tags", "/devices/admin/migrate"]
        );
        assert_eq!(source_paths("/docs/(.*)").unwrap(), ["/docs/{path...}"]);
        assert_eq!(source_paths("/u/:id/x").unwrap(), ["/u/{id}/x"]);
        assert_eq!(source_paths("/f/:rest*").unwrap(), ["/f/{rest...}"]);
        assert_eq!(source_paths("/(.*)/x"), None);
        assert_eq!(source_paths("/a/(?!b).*"), None);
    }

    #[test]
    fn rewrites_cite_their_source_and_strip_the_destination() {
        let config = br#"{"rewrites": [{"source": "/a", "destination": "/api/a?x=1"}, {"source": "/a", "destination": "/api/b/"}]}"#;
        let found = rewrites(config);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].target, "/api/a");
        assert_eq!(found[1].target, "/api/b");
        assert!(found[1].start > found[0].start, "the second `/a` is cited");
        // A source equal to an earlier destination cites its own field.
        let config = br#"{"rewrites": [{"source": "/a", "destination": "/b"}, {"source": "/b", "destination": "/api/b"}]}"#;
        let found = rewrites(config);
        let text = std::str::from_utf8(config).unwrap();
        assert!(text[..found[1].start].ends_with("\"source\": "));
        assert!(rewrites(b"not json").is_empty());
    }

    #[test]
    fn node_functions_are_served_at_their_path() {
        assert_eq!(
            node_function_route("api/hooks.ts").as_deref(),
            Some("/api/hooks")
        );
        assert_eq!(
            node_function_route("api/a/index.js").as_deref(),
            Some("/api/a")
        );
        assert_eq!(node_function_route("api/index.ts").as_deref(), Some("/api"));
        assert_eq!(
            node_function_route("api/foo.bar.ts").as_deref(),
            Some("/api/foo.bar")
        );
        assert_eq!(node_function_route("api/types.d.ts"), None);
        assert_eq!(node_function_route("api/_lib/db.ts"), None);
        assert_eq!(node_function_route("api/x.go"), None);
        assert_eq!(node_function_route("src/api/x.ts"), None);
    }
}
