//! Spring `WebClient` request chains (#1297, AC-0441).
//!
//! A chain `<client>.<verb>().uri(<url>, …)` is a client call only when its
//! client is proven to be
//! `org.springframework.web.reactive.function.client.WebClient`, either as a
//! name whose visible declaration spells that type (as for `RestTemplate`,
//! [`rest_template::Proof::is_client`]) or as an inline `WebClient.create(…)`
//! or `WebClient.builder()…build()` expression. The verb is `get()`,
//! `head()`, `post()`, `put()`, `patch()`, `delete()` or `options()`, or
//! `method(HttpMethod.X)` (else `?`). The URL is the first `uri` argument, a
//! literal only as an exact string literal; anything else is computed.
//!
//! The base is the inline `create("<literal>")` argument or the last
//! `baseUrl("<literal>")` of an inline builder; any other base is a runtime
//! value named by the client expression, which the stitcher reports as an
//! undeclared runtime base rather than guessing the service. A builder that
//! sets a `uriBuilderFactory` (which overrides `baseUrl` in either order) or
//! runs an opaque `apply` callback has such a runtime base. An absolute
//! literal request URL ignores any base, as Spring does.

use super::rest_template::Proof;
use super::*;
use adapters_fw::client::{BaseJoin, FetchBase, FetchSite};
use adapters_fw::events::IdentityExpr;

const WEB_CLIENT: &str = "org.springframework.web.reactive.function.client.WebClient";

fn verb(name: &str) -> Option<Option<&'static str>> {
    Some(Some(match name {
        "get" => "GET",
        "head" => "HEAD",
        "post" => "POST",
        "put" => "PUT",
        "patch" => "PATCH",
        "delete" => "DELETE",
        "options" => "OPTIONS",
        "method" => return Some(None),
        _ => return None,
    }))
}

/// The call's non-comment arguments.
fn arguments<'t>(call: TsNode<'t>) -> Vec<TsNode<'t>> {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut walk = arguments.walk();
    arguments
        .named_children(&mut walk)
        .filter(|argument| !is_comment(*argument))
        .collect()
}

/// A static call `WebClient.<name>(…)` whose qualifier names the proven type
/// and is obscured by no visible variable.
fn static_call(proof: &Proof<'_>, method: TsNode<'_>, call: TsNode<'_>, name: &str) -> bool {
    let cx = proof.cx;
    call.kind() == "method_invocation"
        && call
            .child_by_field_name("name")
            .is_some_and(|called| cx.text(&called) == name)
        && call.child_by_field_name("object").is_some_and(|object| {
            let spelling = cx.text(&object);
            let first = spelling.split('.').next().unwrap_or(spelling).trim();
            proof.names(spelling, WEB_CLIENT)
                && !jpa::names_a_variable(cx, method, call, first)
                && (!spelling.contains('.')
                    || (!proof.declared.contains(first)
                        && !proof.imports.bindings.contains_key(first)))
        })
}

/// The base a literal argument gives, or a computed base spelled by `text`.
fn literal_base(cx: &FileCx<'_>, argument: Option<TsNode<'_>>, text: &str) -> IdentityExpr {
    match argument.and_then(|argument| string_literal(cx, argument)) {
        Some(url) => IdentityExpr::Literal(url.trim_end_matches('/').to_string()),
        None => IdentityExpr::Computed(text.to_string()),
    }
}

/// The base of a proven client expression: `Some(None)` for a client with no
/// base, `Some(Some(base))` for a literal or runtime base, `None` when the
/// expression is not a proven `WebClient`.
fn client_base(
    proof: &Proof<'_>,
    method: TsNode<'_>,
    call: TsNode<'_>,
    client: TsNode<'_>,
) -> Option<Option<IdentityExpr>> {
    let cx = proof.cx;
    if static_call(proof, method, client, "create") {
        let args = arguments(client);
        return Some(match args.as_slice() {
            [] => None,
            [url] => Some(literal_base(cx, Some(*url), cx.text(url))),
            _ => return None,
        });
    }
    if client.kind() == "method_invocation"
        && client
            .child_by_field_name("name")
            .is_some_and(|name| cx.text(&name) == "build")
    {
        // Walk the builder chain back to `WebClient.builder()`; the last
        // `baseUrl` call is the one that applies, unless a URI builder
        // factory or an opaque callback decides the base at runtime.
        let mut base = None;
        let mut opaque = false;
        let mut node = client.child_by_field_name("object")?;
        loop {
            if static_call(proof, method, node, "builder") {
                return Some(if opaque {
                    Some(IdentityExpr::Computed(cx.text(&client).to_string()))
                } else {
                    base
                });
            }
            if node.kind() != "method_invocation" {
                return None;
            }
            let name = node.child_by_field_name("name")?;
            opaque |= matches!(cx.text(&name), "uriBuilderFactory" | "apply");
            if cx.text(&name) == "baseUrl" && base.is_none() {
                let args = arguments(node);
                let [url] = args.as_slice() else {
                    return None;
                };
                base = Some(literal_base(cx, Some(*url), cx.text(url)));
            }
            node = node.child_by_field_name("object")?;
        }
    }
    proof
        .is_client(method, call, client, WEB_CLIENT)
        .then(|| Some(IdentityExpr::Computed(cx.text(&client).to_string())))
}

/// Whether a URL literal carries a scheme (RFC 3986 §3.1).
fn absolute(url: &str) -> bool {
    url.split_once("://").is_some_and(|(scheme, _)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// `WebClient` fetch sites in this file, owned by their enclosing method or
/// constructor.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    methods_by_start: &HashMap<usize, String>,
) -> Vec<FetchSite> {
    let simple = WEB_CLIENT.rsplit('.').next().unwrap_or(WEB_CLIENT);
    if imports.bindings.get(simple).map(String::as_str) != Some(WEB_CLIENT)
        && !cx.text(&root).contains(WEB_CLIENT)
    {
        return Vec::new();
    }
    let proof = Proof::new(cx, root, imports);
    let query = crate::queries::query("(method_invocation) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    let mut out = Vec::new();
    while let Some(found) = matches.next() {
        let uri = found.captures()[0].node;
        if uri
            .child_by_field_name("name")
            .is_none_or(|name| cx.text(&name) != "uri")
        {
            continue;
        }
        let Some(call) = uri
            .child_by_field_name("object")
            .filter(|call| call.kind() == "method_invocation")
        else {
            continue;
        };
        let (Some(client), Some(name)) = (
            call.child_by_field_name("object"),
            call.child_by_field_name("name"),
        ) else {
            continue;
        };
        let Some(fixed) = verb(cx.text(&name)) else {
            continue;
        };
        let verb_args = arguments(call);
        if verb_args.len() != usize::from(fixed.is_none()) {
            continue;
        }
        let Some(url) = arguments(uri).first().copied() else {
            continue;
        };
        let Some((method, symbol)) = rest_template::enclosing(uri, methods_by_start) else {
            continue;
        };
        let Some(base) = client_base(&proof, method, call, client) else {
            continue;
        };
        let http_method = match fixed {
            Some(verb) => verb.to_string(),
            None => proof.method(method, call, verb_args[0]),
        };
        let literal = string_literal(cx, url);
        // An absolute request URL replaces the configured base.
        let base = base.filter(|_| !literal.as_deref().is_some_and(absolute));
        // `Type.method`, naming the call in a runtime-base Gap.
        let entry = symbol
            .rsplit_once('#')
            .map(|(_, member)| member.split('(').next().unwrap_or(member).to_string());
        out.push(FetchSite {
            method: http_method,
            url: match literal {
                Some(literal) => IdentityExpr::Literal(literal),
                None => IdentityExpr::Computed(cx.text(&url).to_string()),
            },
            symbol: Some(symbol),
            path: cx.path.to_string(),
            byte_start: call.start_byte() as u64,
            byte_end: uri.end_byte() as u64,
            base: base.map(|url| FetchBase {
                url,
                join: BaseJoin::Concat,
                path: cx.path.to_string(),
                byte_start: client.start_byte() as u64,
            }),
            entry,
        });
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
