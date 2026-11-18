//! Ktor client requests (#1416, AC-0479).
//!
//! A call `<client>.<verb>(<url>)`, optionally with a trailing request
//! block, is a client call only when its receiver is proven to be
//! `io.ktor.client.HttpClient` (a typed or constructed local, parameter or
//! property, or `this.x`, as [`rest_template::Proof`] proves its clients)
//! and its function is a named import (an alias keeps the imported verb)
//! of an `io.ktor.client.request` extension that no function the file
//! declares may obscure. A wildcard import of the package proves nothing:
//! a same-package extension in another file wins over it, and this file
//! cannot see one (#1418 review). The verbs are `get`, `post`, `put`, `delete`, `patch`, `head`
//! and `options`; `request` sends `GET` unless its block names `method`.
//! The URL is the one positional argument, a literal only as a pure string
//! literal. A block that names `method` makes the method `?`, and one that
//! names `url` may rewrite the URL, which is then computed. A call without
//! a URL argument, with named arguments or with more than one argument is
//! not read.
//!
//! A client may carry a `defaultRequest` base that the file does not show,
//! so a URL that is not an absolute literal joins a runtime base named by
//! the client expression, which the stitcher reports as an undeclared
//! runtime base; Ktor resolves the request URL against it as an RFC 3986
//! reference.

use adapters_fw::client::{BaseJoin, FetchBase, FetchSite};
use adapters_fw::events::IdentityExpr;
use tree_sitter::Node as TsNode;

use super::rest_template::{self, Proof, declared_functions, named_children};
use super::{FileCx, Imports, pure_string_literal};

const HTTP_CLIENT: &str = "io.ktor.client.HttpClient";
const REQUEST_PACKAGE: &str = "io.ktor.client.request";

/// The method a request extension sends, or `None` for `request`.
fn verb(name: &str) -> Option<Option<&'static str>> {
    Some(Some(match name {
        "get" => "GET",
        "post" => "POST",
        "put" => "PUT",
        "delete" => "DELETE",
        "patch" => "PATCH",
        "head" => "HEAD",
        "options" => "OPTIONS",
        "request" => return Some(None),
        _ => return None,
    }))
}

/// The `io.ktor.client.request` extension a called name resolves to: a
/// named import (or alias) of the package's function, unless the file
/// declares a function of that name, which may be the one called.
fn extension<'a>(
    imports: &'a Imports,
    declared: &std::collections::HashSet<String>,
    name: &'a str,
) -> Option<&'a str> {
    if declared.contains(name) {
        return None;
    }
    imports
        .bindings
        .get(name)?
        .strip_prefix(REQUEST_PACKAGE)?
        .strip_prefix('.')
        .filter(|function| !function.contains('.'))
}

/// Whether the request block `lambda` names `word` anywhere, as a property
/// or a call that may set it.
fn names(cx: &FileCx<'_>, lambda: TsNode<'_>, word: &str) -> bool {
    let mut stack = vec![lambda];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" && cx.text(&node) == word {
            return true;
        }
        stack.extend(named_children(node));
    }
    false
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

/// Ktor client fetch sites in this file, owned by their innermost enclosing
/// function, else by the file (#1426).
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &[(TsNode<'_>, String)],
    imports: &Imports,
) -> Vec<FetchSite> {
    if !cx.text(&root).contains("io.ktor.client") {
        return Vec::new();
    }
    let proof = Proof::new(cx, root, imports);
    let declared = declared_functions(cx, root);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(call) = stack.pop() {
        stack.extend(named_children(call));
        if call.kind() != "call_expression" {
            continue;
        }
        let [callee, arguments] = named_children(call)[..] else {
            continue;
        };
        if arguments.kind() != "value_arguments" {
            continue;
        }
        // A trailing request block wraps the call: `(call (call …) lambda)`.
        let lambda = call
            .parent()
            .filter(|outer| outer.kind() == "call_expression")
            .and_then(|outer| match named_children(outer)[..] {
                [inner, lambda] if inner == call && lambda.kind() == "annotated_lambda" => {
                    Some(lambda)
                }
                _ => None,
            });
        if callee.kind() != "navigation_expression" {
            continue;
        }
        let [client, name] = named_children(callee)[..] else {
            continue;
        };
        if name.kind() != "identifier" {
            continue;
        }
        let Some(fixed) = extension(imports, &declared, cx.text(&name)).and_then(verb) else {
            continue;
        };
        let values: Vec<TsNode<'_>> = named_children(arguments)
            .into_iter()
            .filter(|argument| argument.kind() == "value_argument")
            .collect();
        // One positional URL; a named argument is not mapped to its parameter.
        let [value] = values[..] else {
            continue;
        };
        let [url] = named_children(value)[..] else {
            continue;
        };
        if !proof.is_client(call, client, HTTP_CLIENT, true) {
            continue;
        }
        // A request outside any function (a property or top-level
        // initializer) is owned by its file (#1426).
        let symbol = rest_template::enclosing(functions, call);
        if symbol.is_none() && !rest_template::initializer(call) {
            continue;
        }
        let block_names = |word| lambda.is_some_and(|lambda| names(cx, lambda, word));
        let method = match fixed {
            _ if block_names("method") => "?",
            Some(verb) => verb,
            None => "GET",
        };
        let literal = pure_string_literal(cx, url).filter(|_| !block_names("url"));
        let base = (!literal.as_deref().is_some_and(absolute)).then(|| FetchBase {
            url: IdentityExpr::Computed(cx.text(&client).to_string()),
            join: BaseJoin::Resolve,
            path: cx.path.to_string(),
            byte_start: client.start_byte() as u64,
        });
        // `Type.function`, naming the call in a runtime-base Gap.
        let entry = symbol
            .and_then(|symbol| symbol.rsplit_once('#'))
            .map(|(_, member)| member.to_string());
        out.push(FetchSite {
            straight_line: None,
            method: method.to_string(),
            url: match literal {
                Some(literal) => IdentityExpr::Literal(literal),
                None => IdentityExpr::Computed(cx.text(&url).to_string()),
            },
            symbol: symbol.cloned(),
            path: cx.path.to_string(),
            byte_start: call.start_byte() as u64,
            // The block is part of the request when it is present.
            byte_end: lambda
                .map_or(call, |lambda| lambda.parent().unwrap_or(call))
                .end_byte() as u64,
            base,
            entry,
        });
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
