//! Retrofit client calls (#737, AC-0315): import-proven HTTP annotations on
//! interface methods become [`FetchSite`]s with a literal path template, and
//! the repository's one `Retrofit.Builder().baseUrl(...)` becomes their base
//! at the directory join ([`adapters_fw::client::join_repo_base`]).

use adapters_fw::client::{BaseJoin, FetchBase, FetchSite};
use adapters_fw::events::IdentityExpr;
use tree_sitter::Node as TsNode;

use super::{
    FileCx, Imports, PathArg, declaration_annotations, declaration_kind, pure_string_literal,
};

/// The library name Gap reasons use for an unresolved base.
pub(crate) const LIBRARY: &str = "Retrofit";

const PACKAGE: &str = "retrofit2.http";

/// The verb a Retrofit method annotation sends; `HTTP` names it inline.
fn verb(simple: &str) -> Option<&'static str> {
    match simple {
        "GET" => Some("GET"),
        "POST" => Some("POST"),
        "PUT" => Some("PUT"),
        "PATCH" => Some("PATCH"),
        "DELETE" => Some("DELETE"),
        "HEAD" => Some("HEAD"),
        "OPTIONS" => Some("OPTIONS"),
        _ => None,
    }
}

/// A `retrofit2.http` annotation is proven by a named import of exactly
/// `retrofit2.http.<Name>`, a wildcard of exactly that package, or a fully
/// qualified use. A same-named annotation from elsewhere proves nothing.
fn proven(cx: &FileCx<'_>, annotation: TsNode<'_>, simple: &str, imports: &Imports) -> bool {
    if cx
        .text(&annotation)
        .trim_start_matches('@')
        .starts_with("retrofit2.http.")
    {
        return true;
    }
    if let Some(fqn) = imports.bindings.get(simple) {
        return fqn == &format!("{PACKAGE}.{simple}");
    }
    imports.wildcard_packages.iter().any(|w| w == PACKAGE)
}

/// The value of a `name = …` argument of an annotation, if present.
fn named_argument<'t>(cx: &FileCx<'_>, annotation: TsNode<'t>, name: &str) -> Option<TsNode<'t>> {
    let mut stack = vec![annotation];
    while let Some(node) = stack.pop() {
        if node.kind() == "value_argument" {
            let mut walk = node.walk();
            let children: Vec<_> = node.named_children(&mut walk).collect();
            if let [key, value] = children.as_slice()
                && key.kind() == "identifier"
                && cx.text(key) == name
            {
                return Some(*value);
            }
            continue;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    None
}

/// Retrofit sites for the annotated methods of this file's interfaces.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    functions: &[(TsNode<'_>, String)],
    imports: &Imports,
) -> Vec<FetchSite> {
    let mut out = Vec::new();
    for (function, symbol) in functions {
        let Some(owner) = function
            .parent()
            .and_then(|body| body.parent())
            .filter(|decl| decl.kind() == "class_declaration")
        else {
            continue;
        };
        if declaration_kind(cx, owner) != "interface" {
            continue;
        }
        for annotation in declaration_annotations(cx, *function) {
            if !proven(cx, annotation.node, &annotation.simple, imports) {
                continue;
            }
            let method = match verb(&annotation.simple) {
                Some(verb) => verb.to_string(),
                None if annotation.simple == "HTTP" => {
                    match named_argument(cx, annotation.node, "method")
                        .and_then(|value| pure_string_literal(cx, value))
                    {
                        Some(method) => method.to_ascii_uppercase(),
                        None => "?".into(),
                    }
                }
                None => continue,
            };
            let url = match &annotation.path {
                PathArg::Literal(path) if !path.is_empty() => IdentityExpr::Literal(path.clone()),
                // No path: the method takes its whole URL from a `@Url` parameter.
                PathArg::Literal(_) | PathArg::Absent => {
                    IdentityExpr::Computed("@Url parameter".into())
                }
                PathArg::Dynamic | PathArg::Literals(_) => {
                    IdentityExpr::Computed(cx.text(&annotation.node).to_string())
                }
            };
            let entry = symbol.rsplit('#').next().unwrap_or(symbol).to_string();
            out.push(FetchSite {
                straight_line: None,
                method,
                url,
                symbol: Some(symbol.clone()),
                path: cx.path.to_string(),
                byte_start: function.start_byte() as u64,
                byte_end: function.end_byte() as u64,
                base: None,
                entry: Some(entry),
            });
        }
    }
    out
}

/// `Retrofit.Builder()….baseUrl(arg)` calls in this file, proven by an
/// import of `retrofit2.Retrofit` (or its package) or a qualified builder.
pub(crate) fn bases(cx: &FileCx<'_>, root: TsNode<'_>, imports: &Imports) -> Vec<FetchBase> {
    let retrofit_imported = imports.bindings.get("Retrofit").map(String::as_str)
        == Some("retrofit2.Retrofit")
        || imports.wildcard_packages.iter().any(|w| w == "retrofit2");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() != "call_expression" {
            continue;
        }
        let Some(callee) = node.named_child(0) else {
            continue;
        };
        if callee.kind() != "navigation_expression" {
            continue;
        }
        let mut walk = callee.walk();
        let parts: Vec<_> = callee.named_children(&mut walk).collect();
        let [receiver, name] = parts.as_slice() else {
            continue;
        };
        if cx.text(name) != "baseUrl" {
            continue;
        }
        let chain = cx.text(receiver);
        let builder = (retrofit_imported && chain.starts_with("Retrofit.Builder()"))
            || chain.starts_with("retrofit2.Retrofit.Builder()");
        if !builder {
            continue;
        }
        // The call's single value argument.
        let Some(argument) = node
            .named_child(1)
            .and_then(|args| {
                let mut walk = args.walk();
                args.named_children(&mut walk)
                    .find(|child| child.kind() == "value_argument")
            })
            .and_then(|argument| argument.named_child(0))
        else {
            continue;
        };
        out.push(FetchBase {
            url: match pure_string_literal(cx, argument) {
                Some(url) => IdentityExpr::Literal(url),
                None => IdentityExpr::Computed(cx.text(&argument).to_string()),
            },
            join: BaseJoin::Resolve,
            path: cx.path.to_string(),
            byte_start: argument.start_byte() as u64,
        });
    }
    out
}
