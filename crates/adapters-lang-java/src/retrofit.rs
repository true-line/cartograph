//! Retrofit client calls (#737, AC-0315): import-proven HTTP annotations on
//! interface methods become [`FetchSite`]s with a literal path template, and
//! the repository's one `new Retrofit.Builder().baseUrl(...)` becomes their
//! base at the directory join ([`adapters_fw::client::join_repo_base`]).

use adapters_fw::client::{BaseJoin, FetchBase, FetchSite};
use adapters_fw::events::IdentityExpr;
use tree_sitter::Node as TsNode;

use super::{FileCx, Imports, PathArg, annotation_path, declaration_annotations, string_literal};

/// The library name Gap reasons use for an unresolved base.
pub(crate) const LIBRARY: &str = "Retrofit";

const PACKAGE: &str = "retrofit2.http";

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

/// Proven by a named import of exactly `retrofit2.http.<Name>`, a wildcard
/// of exactly that package, or a fully qualified use.
fn proven(cx: &FileCx<'_>, annotation: TsNode<'_>, simple: &str, imports: &Imports) -> bool {
    let qualified = annotation
        .child_by_field_name("name")
        .is_some_and(|name| cx.text(&name) == format!("{PACKAGE}.{simple}"));
    if qualified {
        return true;
    }
    if let Some(fqn) = imports.bindings.get(simple) {
        return fqn == &format!("{PACKAGE}.{simple}");
    }
    imports.wildcard_packages.iter().any(|w| w == PACKAGE)
}

/// The value of a `name = …` element of an annotation, if present.
pub(crate) fn element<'t>(
    cx: &FileCx<'_>,
    annotation: TsNode<'t>,
    name: &str,
) -> Option<TsNode<'t>> {
    let arguments = annotation.child_by_field_name("arguments")?;
    let mut walk = arguments.walk();
    arguments
        .named_children(&mut walk)
        .filter(|argument| argument.kind() == "element_value_pair")
        .find(|argument| {
            argument
                .child_by_field_name("key")
                .is_some_and(|key| cx.text(&key) == name)
        })
        .and_then(|argument| argument.child_by_field_name("value"))
}

/// Retrofit sites for the annotated methods of this file's interfaces.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    methods: &[(TsNode<'_>, String, Vec<String>, String)],
    imports: &Imports,
) -> Vec<FetchSite> {
    let mut out = Vec::new();
    for (method, symbol, chain, name) in methods {
        let in_interface = method
            .parent()
            .and_then(|body| body.parent())
            .is_some_and(|decl| decl.kind() == "interface_declaration");
        if method.kind() != "method_declaration" || !in_interface {
            continue;
        }
        for (simple, annotation) in declaration_annotations(cx, *method) {
            if !proven(cx, annotation, &simple, imports) {
                continue;
            }
            let http_method = match verb(&simple) {
                Some(verb) => verb.to_string(),
                None if simple == "HTTP" => element(cx, annotation, "method")
                    .and_then(|value| string_literal(cx, value))
                    .map_or_else(|| "?".into(), |method| method.to_ascii_uppercase()),
                None => continue,
            };
            let url = match annotation_path(cx, annotation) {
                PathArg::Literal(paths) => match paths.as_slice() {
                    [path] if !path.is_empty() => IdentityExpr::Literal(path.clone()),
                    [_] => IdentityExpr::Computed("@Url parameter".into()),
                    _ => IdentityExpr::Computed(cx.text(&annotation).to_string()),
                },
                // No path: the method takes its whole URL from a `@Url` parameter.
                PathArg::Absent => IdentityExpr::Computed("@Url parameter".into()),
                PathArg::Dynamic => IdentityExpr::Computed(cx.text(&annotation).to_string()),
            };
            let entry = if chain.is_empty() {
                name.clone()
            } else {
                format!("{}.{name}", chain.join("."))
            };
            out.push(FetchSite {
                method: http_method,
                url,
                symbol: Some(symbol.clone()),
                path: cx.path.to_string(),
                byte_start: method.start_byte() as u64,
                byte_end: method.end_byte() as u64,
                base: None,
                entry: Some(entry),
            });
        }
    }
    out
}

/// `new Retrofit.Builder()….baseUrl(arg)` calls in this file, proven by an
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
        if node.kind() != "method_invocation" {
            continue;
        }
        let (Some(name), Some(object), Some(arguments)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("object"),
            node.child_by_field_name("arguments"),
        ) else {
            continue;
        };
        if cx.text(&name) != "baseUrl" {
            continue;
        }
        let chain = cx.text(&object);
        let builder = (retrofit_imported && chain.starts_with("new Retrofit.Builder()"))
            || chain.starts_with("new retrofit2.Retrofit.Builder()");
        if !builder {
            continue;
        }
        let mut walk = arguments.walk();
        let Some(argument) = arguments.named_children(&mut walk).next() else {
            continue;
        };
        out.push(FetchBase {
            url: match string_literal(cx, argument) {
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
