//! Square OkHttp request builders (#1405, AC-0476).
//!
//! A chain `new Request.Builder()…build()` is a client call only when its
//! root constructs a proven `okhttp3.Request.Builder` without a class body
//! and every link is a `Request.Builder` member called with that member's
//! arity; any other link proves nothing. The last method link wins and the
//! default is `GET`; `method("<NAME>", body)` gives its literal only as a
//! token without lowercase letters, else `?`. The last `url(…)` wins; only
//! `url("<lit>")` is a literal, any other argument is computed. A chain
//! without `url` yields no site. Request URLs are absolute, so there is no
//! base.

use super::rest_template::Proof;
use super::webclient::arguments;
use super::*;
use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;

const REQUEST: &str = "okhttp3.Request";
const BUILDER: &str = "okhttp3.Request.Builder";

/// Whether a method name is an RFC 9110 token without lowercase letters, so
/// `PROPFIND` keeps its literal while a mistyped `patch` does not.
fn uppercase_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_uppercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
        })
}

/// Whether `node` is `new Request.Builder()` on a proven OkHttp builder.
fn builder_root(proof: &Proof<'_>, node: TsNode<'_>) -> bool {
    let cx = proof.cx;
    if node.kind() != "object_creation_expression" || !arguments(node).is_empty() {
        return false;
    }
    let mut walk = node.walk();
    if node
        .named_children(&mut walk)
        .any(|child| child.kind() == "class_body")
    {
        return false;
    }
    node.child_by_field_name("type").is_some_and(|ty| {
        let spelling: String = cx
            .text(&ty)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        // A file-declared or imported `okhttp3` type captures a qualified
        // spelling, as in `webclient::static_call`.
        if spelling.starts_with("okhttp3.")
            && (proof.declared.contains("okhttp3")
                || proof.imports.bindings.contains_key("okhttp3"))
        {
            return false;
        }
        proof.names(&spelling, BUILDER)
            || spelling
                .strip_suffix(".Builder")
                .is_some_and(|outer| proof.names(outer, REQUEST))
    })
}

/// OkHttp request sites in this file, owned by their enclosing method or
/// constructor.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    methods_by_start: &HashMap<usize, String>,
) -> Vec<FetchSite> {
    let bound = |fqn: &str| {
        let simple = fqn.rsplit('.').next().unwrap_or(fqn);
        imports.bindings.get(simple).map(String::as_str) == Some(fqn)
    };
    // A qualified spelling may space its dots (`okhttp3 . Request`), so the
    // unimported fast path looks for the package name alone.
    if !bound(REQUEST) && !bound(BUILDER) && !cx.text(&root).contains("okhttp3") {
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
        let Some((_, symbol)) = rest_template::enclosing(build, methods_by_start) else {
            continue;
        };
        // Walk back to `new Request.Builder()`; links seen first are the last
        // called, so the first method and `url` found are the ones that apply.
        let mut verb = None;
        let mut url = None;
        let mut node = build;
        loop {
            let Some(object) = node.child_by_field_name("object") else {
                continue 'calls;
            };
            node = object;
            if builder_root(&proof, node) {
                break;
            }
            if node.kind() != "method_invocation" {
                continue 'calls;
            }
            let args = arguments(node);
            let Some(name) = node.child_by_field_name("name") else {
                continue 'calls;
            };
            let link = match (cx.text(&name), args.as_slice()) {
                ("get" | "head", []) | ("delete", [] | [_]) => {
                    Some(cx.text(&name).to_ascii_uppercase())
                }
                ("post" | "put" | "patch", [_]) => Some(cx.text(&name).to_ascii_uppercase()),
                ("method", [named, _]) => Some(
                    string_literal(cx, *named)
                        .filter(|name| uppercase_token(name))
                        .unwrap_or_else(|| "?".into()),
                ),
                ("url", [target]) => {
                    url.get_or_insert(*target);
                    None
                }
                ("header" | "addHeader", [_, _])
                | ("removeHeader" | "headers" | "cacheControl", [_])
                | ("tag", [_] | [_, _]) => None,
                _ => continue 'calls,
            };
            if let Some(link) = link {
                verb.get_or_insert(link);
            }
        }
        let Some(url) = url else {
            continue;
        };
        let url = match string_literal(cx, url) {
            Some(literal) => IdentityExpr::Literal(literal),
            None => IdentityExpr::Computed(cx.text(&url).to_string()),
        };
        // `Type.method`, naming the call in a computed-URL Gap.
        let entry = symbol
            .rsplit_once('#')
            .map(|(_, member)| member.split('(').next().unwrap_or(member).to_string());
        out.push(FetchSite {
            straight_line: None,
            method: verb.unwrap_or_else(|| "GET".into()),
            url,
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
