//! Spring `WebClient` request chains (#1342, AC-0457), as Java reads them
//! (AC-0441).
//!
//! A chain `<client>.<verb>().uri(<url>, …)` is a client call only when its
//! client is proven to be
//! `org.springframework.web.reactive.function.client.WebClient`: a name
//! whose visible declaration (a local, parameter, constructor or class
//! property, or `this.x`) states that type, as for `RestTemplate`
//! ([`rest_template::Proof`]), or an inline `WebClient.create(…)` or
//! `WebClient.builder()…build()`. `WebClient` is an interface, so an
//! initializer alone proves nothing. The verb is `get()`, `head()`,
//! `post()`, `put()`, `patch()`, `delete()` or `options()`, or
//! `method(HttpMethod.X)` (else `?`). The URL is the first `uri` argument, a
//! literal only as a pure string literal; anything else is computed, as is
//! a URL with exactly one further argument that is neither a literal nor a
//! name stated as a built-in value type (`id: Long`), which may be a
//! `Function<UriBuilder, URI>` that rewrites it. A call with named arguments
//! or a trailing lambda is not read.
//!
//! The base is the inline `create("<literal>")` argument or the last
//! `baseUrl("<literal>")` of an inline builder; any other base is a runtime
//! value named by the client expression, which the stitcher reports as an
//! undeclared runtime base rather than guessing the service. A builder that
//! sets a `uriBuilderFactory` or runs an opaque `apply` callback has such a
//! runtime base. An absolute literal request URL ignores any base, as Spring
//! does. Every builder link must be a `WebClient.Builder` member: a member
//! wins over any extension of the same name, so the chain keeps its type,
//! while another name may be an extension returning anything.

use adapters_fw::client::{BaseJoin, FetchBase, FetchSite};
use adapters_fw::events::IdentityExpr;
use tree_sitter::Node as TsNode;

use super::rest_template::{self, Proof, named_children};
use super::{FileCx, Imports, pure_string_literal};

const WEB_CLIENT: &str = "org.springframework.web.reactive.function.client.WebClient";

/// `WebClient.Builder` members, each returning the builder, with the
/// argument counts their overloads take and whether a trailing lambda (a
/// `Consumer` or other functional parameter) may supply the last one. A
/// call of a member name outside its shape is not that member: a Kotlin
/// extension of another signature may be what it calls.
const BUILDER_MEMBERS: &[(&str, usize, usize, bool)] = &[
    ("apply", 1, 1, true),
    ("baseUrl", 1, 1, false),
    ("clientConnector", 1, 1, true),
    ("clone", 0, 0, false),
    ("codecs", 1, 1, true),
    ("defaultCookie", 1, usize::MAX, false),
    ("defaultCookies", 1, 1, true),
    ("defaultHeader", 1, usize::MAX, false),
    ("defaultHeaders", 1, 1, true),
    ("defaultRequest", 1, 1, true),
    ("defaultStatusHandler", 2, 2, false),
    ("defaultUriVariables", 1, 1, false),
    ("exchangeFunction", 1, 1, true),
    ("exchangeStrategies", 1, 1, true),
    ("filter", 1, 1, true),
    ("filters", 1, 1, true),
    ("observationConvention", 1, 1, false),
    ("observationRegistry", 1, 1, false),
    ("uriBuilderFactory", 1, 1, false),
];

/// Members whose every parameter is a `String`: another literal there
/// selects some other function.
const STRING_MEMBERS: &[&str] = &["baseUrl", "defaultCookie", "defaultHeader"];

/// Whether a link `name(args)` (`lambda`: one trailing lambda argument)
/// matches a `WebClient.Builder` member. A function of that name declared
/// or imported by the file may be the one called, so it proves nothing,
/// and a non-string literal argument of a `String` member selects another
/// function.
fn builder_member(proof: &Proof<'_>, name: &str, args: &[TsNode<'_>], lambda: bool) -> bool {
    let count = args.len() + usize::from(lambda);
    BUILDER_MEMBERS
        .iter()
        .any(|&(member, min, max, takes_lambda)| {
            member == name && (min..=max).contains(&count) && (takes_lambda || !lambda)
        })
        && !proof.shadows(name)
        && !(STRING_MEMBERS.contains(&name)
            && args.iter().any(|arg| {
                (arg.kind().ends_with("_literal") && arg.kind() != "string_literal")
                    || matches!(proof.cx.text(arg), "true" | "false")
            }))
}

/// The method a verb call sends, or `None` for `method(…)`.
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

/// A call `<receiver>.<name>(<positional arguments>)` with no trailing
/// lambda: its receiver, name and arguments.
pub(crate) fn member_call<'s, 't>(
    cx: &FileCx<'s>,
    call: TsNode<'t>,
) -> Option<(TsNode<'t>, &'s str, Vec<TsNode<'t>>)> {
    if call.kind() != "call_expression" {
        return None;
    }
    let [callee, arguments] = named_children(call)[..] else {
        return None;
    };
    if callee.kind() != "navigation_expression" || arguments.kind() != "value_arguments" {
        return None;
    }
    let [receiver, name] = named_children(callee)[..] else {
        return None;
    };
    if name.kind() != "identifier" {
        return None;
    }
    let values: Vec<TsNode<'t>> = named_children(arguments)
        .into_iter()
        .filter(|argument| argument.kind() == "value_argument")
        .collect();
    let mut args = Vec::new();
    for value in &values {
        match named_children(*value).as_slice() {
            [expression] => args.push(*expression),
            // A named argument is not mapped to its parameter.
            _ => return None,
        }
    }
    let name = name.utf8_text(cx.source).ok()?;
    Some((receiver, name, args))
}

/// A static call `WebClient.<name>(…)` whose qualifier names the proven
/// type and is obscured by no visible binding: its arguments.
fn static_call<'t>(
    proof: &Proof<'_>,
    site: TsNode<'_>,
    call: TsNode<'t>,
    name: &str,
) -> Option<Vec<TsNode<'t>>> {
    let cx = proof.cx;
    let (owner, called, args) = member_call(cx, call)?;
    (called == name
        && matches!(owner.kind(), "identifier" | "navigation_expression")
        && proof.names_value_at(site, cx.text(&owner), WEB_CLIENT))
    .then_some(args)
}

/// The base a literal argument gives, or a computed base spelled by `text`.
fn literal_base(cx: &FileCx<'_>, argument: TsNode<'_>) -> IdentityExpr {
    match pure_string_literal(cx, argument) {
        Some(url) => IdentityExpr::Literal(url),
        None => IdentityExpr::Computed(cx.text(&argument).to_string()),
    }
}

/// The base of a proven client expression: `Some(None)` for a client with no
/// base, `Some(Some(base))` for a literal or runtime base, `None` when the
/// expression is not a proven `WebClient`.
fn client_base(
    proof: &Proof<'_>,
    site: TsNode<'_>,
    client: TsNode<'_>,
) -> Option<Option<IdentityExpr>> {
    let cx = proof.cx;
    if let Some(args) = static_call(proof, site, client, "create") {
        return Some(match args.as_slice() {
            [] => None,
            [url] => Some(literal_base(cx, *url)),
            _ => return None,
        });
    }
    if let Some((mut node, "build", args)) = member_call(cx, client) {
        if !args.is_empty() {
            return None;
        }
        // Walk the builder chain back to `WebClient.builder()`; the last
        // `baseUrl` call is the one that applies, unless a URI builder
        // factory or an opaque callback decides the base at runtime.
        let mut base = None;
        let mut opaque = false;
        loop {
            if static_call(proof, site, node, "builder").is_some_and(|args| args.is_empty()) {
                return Some(if opaque {
                    Some(IdentityExpr::Computed(cx.text(&client).to_string()))
                } else {
                    base
                });
            }
            let Some((receiver, name, args)) = member_call(cx, node) else {
                // A link with a trailing lambda (`apply { … }`, Spring's or
                // Kotlin's) configures the builder opaquely.
                let [callee, lambda] = named_children(node)[..] else {
                    return None;
                };
                let [receiver, name] = named_children(callee)[..] else {
                    return None;
                };
                if node.kind() != "call_expression"
                    || callee.kind() != "navigation_expression"
                    || lambda.kind() != "annotated_lambda"
                    || !builder_member(proof, cx.text(&name), &[], true)
                {
                    return None;
                }
                opaque = true;
                node = receiver;
                continue;
            };
            if !builder_member(proof, name, &args, false) {
                return None;
            }
            // An exchange function sends the request wherever it likes.
            opaque |= matches!(name, "uriBuilderFactory" | "apply" | "exchangeFunction");
            if name == "baseUrl" && base.is_none() {
                let [url] = args.as_slice() else {
                    return None;
                };
                base = Some(literal_base(cx, *url));
            }
            node = receiver;
        }
    }
    proof
        .is_client(site, client, WEB_CLIENT, false)
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

/// `WebClient` fetch sites in this file, owned by their innermost enclosing
/// function.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &[(TsNode<'_>, String)],
    imports: &Imports,
) -> Vec<FetchSite> {
    if !imports.bindings.values().any(|fqn| fqn == WEB_CLIENT)
        && !cx.text(&root).contains(WEB_CLIENT)
    {
        return Vec::new();
    }
    let proof = Proof::new(cx, root, imports);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(uri) = stack.pop() {
        stack.extend(named_children(uri));
        let Some((call, "uri", uri_args)) = member_call(cx, uri) else {
            continue;
        };
        let Some((client, name, verb_args)) = member_call(cx, call) else {
            continue;
        };
        let Some(fixed) = verb(name) else {
            continue;
        };
        if verb_args.len() != usize::from(fixed.is_none()) {
            continue;
        }
        let Some(url) = uri_args.first().copied() else {
            continue;
        };
        let Some(symbol) = rest_template::enclosing(functions, uri) else {
            continue;
        };
        let Some(base) = client_base(&proof, call, client) else {
            continue;
        };
        let http_method = match fixed {
            Some(verb) => verb.to_string(),
            None => proof
                .method(call, verb_args[0])
                .unwrap_or_else(|| "?".to_string()),
        };
        // `uri(String, Function<UriBuilder, URI>)` builds the URI itself; a
        // lone further argument that is not a literal may be that function.
        // Overloads resolve on static types: a literal or a name stated as
        // a plain value type selects the template-variable overload.
        let value = |extra: &TsNode<'_>| {
            (extra.kind().ends_with("_literal")
                && !matches!(extra.kind(), "lambda_literal" | "object_literal"))
                || (extra.kind() == "identifier" && proof.binds_value(call, cx.text(extra)))
        };
        let rewritten = matches!(uri_args.as_slice(), [_, extra] if !value(extra));
        let literal = pure_string_literal(cx, url).filter(|_| !rewritten);
        // An absolute request URL replaces the configured base.
        let base = base.filter(|_| !literal.as_deref().is_some_and(absolute));
        // Spring appends the request path to the base path and collapses a
        // doubled `/`: `/api/` + `orders` and `/api/` + `/orders` both give
        // `/api/orders`, while `/api` + `orders` gives `/apiorders`. The
        // base keeps its trailing `/` only before a relative literal path.
        let relative = literal
            .as_deref()
            .is_some_and(|path| !path.starts_with('/'));
        let base = base.map(|base| match base {
            IdentityExpr::Literal(url) if !relative => {
                IdentityExpr::Literal(url.trim_end_matches('/').to_string())
            }
            base => base,
        });
        // `Type.function`, naming the call in a runtime-base Gap.
        let entry = symbol
            .rsplit_once('#')
            .map(|(_, member)| member.to_string());
        out.push(FetchSite {
            method: http_method,
            url: match literal {
                Some(literal) => IdentityExpr::Literal(literal),
                None => IdentityExpr::Computed(cx.text(&url).to_string()),
            },
            symbol: Some(symbol.clone()),
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
