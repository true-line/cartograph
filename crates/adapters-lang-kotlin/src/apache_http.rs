//! Apache HttpClient request constructors (#1420, AC-0471), as Java reads
//! them (AC-0478).
//!
//! `HttpGet(<url>)` and its siblings are client calls only when the called
//! name is proven to be one of the request classes of HttpClient 4
//! (`org.apache.http.client.methods`) or 5
//! (`org.apache.hc.client5.http.classic.methods`): a named import (or
//! alias) in a file declaring nothing of that name, or a fully qualified
//! spelling in a file without wildcard imports that declares, binds or
//! imports nothing named `org`. An explicit import wins over a wildcard-imported or
//! same-package function of that name, but a wildcard may bring an `org`
//! object that captures a qualified spelling. The call takes exactly one
//! positional argument and no trailing lambda. The class gives the method.
//! The URL is a literal only as a pure string literal; anything else is
//! computed. A no-argument constructor sets its URI later and yields no
//! site. Request URLs are absolute, so there is no base.

use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;
use tree_sitter::Node as TsNode;

use super::rest_template::{self, Proof, named_children};
use super::{FileCx, Imports, pure_string_literal, receivers};

/// The request-class packages of HttpClient 4 and 5.
const PACKAGES: &[&str] = &[
    "org.apache.http.client.methods",
    "org.apache.hc.client5.http.classic.methods",
];

/// Request classes and the method each sends.
const CLASSES: &[(&str, &str)] = &[
    ("HttpGet", "GET"),
    ("HttpHead", "HEAD"),
    ("HttpPost", "POST"),
    ("HttpPut", "PUT"),
    ("HttpPatch", "PATCH"),
    ("HttpDelete", "DELETE"),
    ("HttpOptions", "OPTIONS"),
    ("HttpTrace", "TRACE"),
];

/// The method a called request-class spelling sends; `captured`:
/// a file-level `org` declaration, import or wildcard import may win for a
/// qualified spelling.
fn method(proof: &Proof<'_>, captured: bool, head: TsNode<'_>) -> Option<&'static str> {
    let spelling: String = proof
        .cx
        .text(&head)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if captured && spelling.starts_with("org.") {
        return None;
    }
    CLASSES.iter().find_map(|&(class, verb)| {
        PACKAGES
            .iter()
            .any(|package| proof.names(&spelling, &format!("{package}.{class}")))
            .then_some(verb)
    })
}

/// Apache HttpClient request sites in this file, owned by their innermost
/// enclosing function, else by the file (#1426).
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &[(TsNode<'_>, String)],
    imports: &Imports,
) -> Vec<FetchSite> {
    // A qualified spelling may space its dots, so the unimported fast path
    // looks for the package's first segments alone.
    if !imports
        .bindings
        .values()
        .any(|fqn| PACKAGES.iter().any(|package| fqn.starts_with(package)))
        && !cx.text(&root).contains("apache")
    {
        return Vec::new();
    }
    let captured = imports.bindings.contains_key("org")
        || !imports.wildcard_packages.is_empty()
        || receivers::declares_name(cx, root, "org");
    let proof = Proof::new(cx, root, imports);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(call) = stack.pop() {
        stack.extend(named_children(call));
        if call.kind() != "call_expression" {
            continue;
        }
        let [head, arguments] = named_children(call)[..] else {
            continue;
        };
        if arguments.kind() != "value_arguments"
            || !matches!(head.kind(), "identifier" | "navigation_expression")
        {
            continue;
        }
        // A trailing lambda wraps the call and selects another function.
        if call.parent().is_some_and(|outer| {
            outer.kind() == "call_expression"
                && named_children(outer)
                    .iter()
                    .any(|child| child.kind() == "annotated_lambda")
        }) {
            continue;
        }
        let [value] = named_children(arguments)[..] else {
            continue;
        };
        // One positional URL; a named argument is not mapped to its parameter.
        let [url] = named_children(value)[..] else {
            continue;
        };
        let Some(verb) = method(&proof, captured, head) else {
            continue;
        };
        // A request outside any function (a property or top-level
        // initializer) is owned by its file (#1426).
        let symbol = rest_template::enclosing(functions, call);
        // `Type.function`, naming the call in a computed-URL Gap.
        let entry = symbol
            .and_then(|symbol| symbol.rsplit_once('#'))
            .map(|(_, member)| member.to_string());
        out.push(FetchSite {
            method: verb.to_string(),
            url: match pure_string_literal(cx, url) {
                Some(literal) => IdentityExpr::Literal(literal),
                None => IdentityExpr::Computed(cx.text(&url).to_string()),
            },
            symbol: symbol.cloned(),
            path: cx.path.to_string(),
            byte_start: call.start_byte() as u64,
            byte_end: call.end_byte() as u64,
            base: None,
            entry,
        });
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
