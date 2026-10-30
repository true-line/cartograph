//! JDK `java.net.http` request builders (#1395, AC-0475).
//!
//! A chain `HttpRequest.newBuilder([uri])…build()` is a client call only
//! when its root is a static call on a proven `java.net.http.HttpRequest`
//! (as [`webclient::static_call`] proves `WebClient`) and every link is an
//! `HttpRequest.Builder` member called with that member's arity; any other
//! link proves nothing. The last method link wins and the default is `GET`;
//! `method("<NAME>", body)` gives its literal only as a token without
//! lowercase letters,
//! else `?`. The last `uri(…)` wins over the `newBuilder` argument. The URL
//! is a literal only as `URI.create("<lit>")` or `new URI("<lit>")` on a
//! proven `java.net.URI`; anything else is computed. A chain without a URI
//! yields no site. Request URLs are absolute, so there is no base.

use super::rest_template::Proof;
use super::webclient::{arguments, static_call};
use super::*;
use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;

const HTTP_REQUEST: &str = "java.net.http.HttpRequest";
const URI: &str = "java.net.URI";

/// Whether a method name is an RFC 9110 token without lowercase letters, so
/// `M-SEARCH` keeps its literal while a mistyped `patch` does not.
fn uppercase_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_uppercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
        })
}

/// The URL a `URI` expression names: a literal only for a proven
/// `URI.create("<lit>")` or `new URI("<lit>")`, else its computed spelling.
fn url(proof: &Proof<'_>, method: TsNode<'_>, uri: TsNode<'_>) -> IdentityExpr {
    let cx = proof.cx;
    let argument = match uri.kind() {
        "method_invocation" if static_call(proof, method, uri, URI, "create") => {
            Some(arguments(uri))
        }
        "object_creation_expression"
            if uri.child_by_field_name("body").is_none()
                && uri
                    .child_by_field_name("type")
                    .is_some_and(|ty| proof.names(cx.text(&ty), URI)) =>
        {
            Some(arguments(uri))
        }
        _ => None,
    };
    match argument.as_deref() {
        Some([argument]) => match string_literal(cx, *argument) {
            Some(url) => IdentityExpr::Literal(url),
            None => IdentityExpr::Computed(cx.text(argument).to_string()),
        },
        _ => IdentityExpr::Computed(cx.text(&uri).to_string()),
    }
}

/// `java.net.http` request sites in this file, owned by their enclosing
/// method or constructor.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    methods_by_start: &HashMap<usize, String>,
) -> Vec<FetchSite> {
    let simple = HTTP_REQUEST.rsplit('.').next().unwrap_or(HTTP_REQUEST);
    if imports.bindings.get(simple).map(String::as_str) != Some(HTTP_REQUEST)
        && !cx.text(&root).contains(HTTP_REQUEST)
    {
        return Vec::new();
    }
    let proof = Proof::new(cx, root, imports);
    let query = crate::queries::query("(method_invocation) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    let mut out = Vec::new();
    'calls: while let Some(found) = matches.next() {
        let build = found.captures()[0].node;
        if build
            .child_by_field_name("name")
            .is_none_or(|name| cx.text(&name) != "build")
            || !arguments(build).is_empty()
        {
            continue;
        }
        let Some((method, symbol)) = rest_template::enclosing(build, methods_by_start) else {
            continue;
        };
        // Walk back to `HttpRequest.newBuilder(…)`; links seen first are the
        // last called, so the first method and `uri` found are the ones that
        // apply.
        let mut verb = None;
        let mut uri = None;
        let mut node = build;
        loop {
            let Some(object) = node.child_by_field_name("object") else {
                continue 'calls;
            };
            node = object;
            if node.kind() != "method_invocation" {
                continue 'calls;
            }
            let args = arguments(node);
            if static_call(&proof, method, node, HTTP_REQUEST, "newBuilder") {
                match args.as_slice() {
                    [] => {}
                    [first] => {
                        uri.get_or_insert(*first);
                    }
                    _ => continue 'calls,
                }
                break;
            }
            let Some(name) = node.child_by_field_name("name") else {
                continue 'calls;
            };
            let link = match (cx.text(&name), args.as_slice()) {
                ("GET" | "DELETE" | "HEAD", []) => Some(cx.text(&name).to_string()),
                ("POST" | "PUT", [_]) => Some(cx.text(&name).to_string()),
                ("method", [named, _]) => Some(
                    string_literal(cx, *named)
                        .filter(|name| uppercase_token(name))
                        .unwrap_or_else(|| "?".into()),
                ),
                ("uri", [target]) => {
                    uri.get_or_insert(*target);
                    None
                }
                ("header" | "setHeader", [_, _])
                | ("headers", [_, _, ..])
                | ("timeout" | "version" | "expectContinue", [_]) => None,
                _ => continue 'calls,
            };
            if let Some(link) = link {
                verb.get_or_insert(link);
            }
        }
        let Some(uri) = uri else {
            continue;
        };
        // `Type.method`, naming the call in a computed-URL Gap.
        let entry = symbol
            .rsplit_once('#')
            .map(|(_, member)| member.split('(').next().unwrap_or(member).to_string());
        out.push(FetchSite {
            method: verb.unwrap_or_else(|| "GET".into()),
            url: url(&proof, method, uri),
            symbol: Some(symbol),
            path: cx.path.to_string(),
            byte_start: node.start_byte() as u64,
            byte_end: build.end_byte() as u64,
            base: None,
            entry,
        });
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
