//! OpenFeign client calls (#1287, AC-0437).
//!
//! An interface is a Feign client only when its `@FeignClient` annotation
//! is proven: the fully qualified spelling, or the simple name under its
//! single-type import in a file that declares no type of that name. Each
//! abstract method carrying a proven Spring mapping annotation becomes a
//! [`FetchSite`] whose path is the client's `path` prefix joined to the
//! mapping path, as Spring Cloud OpenFeign cleans and joins them.
//!
//! The base is the client's `url`: a literal, with `http://` prepended when
//! it does not start with `http` as OpenFeign does, or anything else as a
//! computed base only the system manifest resolves. A client without a
//! `url` calls its load-balanced service, so its base is that service name,
//! computed too. OpenFeign rejects a class-level `@RequestMapping` on a
//! client, so one makes every site of that client computed.

use super::*;
use adapters_fw::client::{BaseJoin, FetchBase, FetchSite};
use adapters_fw::events::IdentityExpr;

const FEIGN_CLIENT: &str = "org.springframework.cloud.openfeign.FeignClient";
const SPRING_WEB: &str = "org.springframework.web.bind.annotation";

/// Whether a type spelling names `fqn` in this file: the fully qualified
/// spelling, or the simple name under its single-type import (or, when
/// `wildcard` allows, a wildcard of exactly its package) that no type the
/// file declares obscures.
fn names(
    spelling: &str,
    fqn: &str,
    imports: &Imports,
    declared: &BTreeSet<String>,
    wildcard: bool,
) -> bool {
    let spelling: String = spelling.chars().filter(|c| !c.is_whitespace()).collect();
    if spelling == fqn {
        return true;
    }
    let (package, simple) = fqn.rsplit_once('.').unwrap_or(("", fqn));
    if spelling != simple || declared.contains(simple) {
        return false;
    }
    match imports.bindings.get(simple) {
        Some(bound) => bound == fqn,
        None => wildcard && imports.wildcard_packages.iter().any(|w| w == package),
    }
}

/// Whether a proven simple-name spelling rests on a wildcard import alone,
/// which a same-package type of that name would obscure (JLS §6.4.1).
fn wildcard_only(spelling: &str, simple: &str, imports: &Imports) -> bool {
    !spelling.contains('.') && !imports.bindings.contains_key(simple)
}

/// A Spring mapping annotation, proven as Spring endpoints prove it (its
/// named import, or a wildcard of exactly its package), spelled by its
/// simple name with no type of that name declared in the file to shadow
/// the import, or by its exact fully qualified name. `Some(true)` when the
/// proof rests on a wildcard alone, settled at the directory join.
fn mapping_proven(
    cx: &FileCx<'_>,
    annotation: TsNode<'_>,
    simple: &str,
    imports: &Imports,
    declared: &BTreeSet<String>,
) -> Option<bool> {
    let spelling = annotation.child_by_field_name("name")?;
    let spelling: String = cx
        .text(&spelling)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let proven = match spelling.contains('.') {
        true => spelling == format!("{SPRING_WEB}.{simple}"),
        false => spring_proven(simple, imports) && !declared.contains(simple),
    };
    proven.then(|| wildcard_only(&spelling, simple, imports))
}

/// The method of a `@RequestMapping`: GET when it names none, as
/// OpenFeign's Spring contract defaults it, the one proven `RequestMethod.X`
/// it names, else `?`; and whether that `RequestMethod` rests on a
/// wildcard import alone.
fn request_mapping_method(
    cx: &FileCx<'_>,
    annotation: TsNode<'_>,
    imports: &Imports,
    declared: &BTreeSet<String>,
) -> (String, bool) {
    let Some(value) = retrofit::element(cx, annotation, "method") else {
        return ("GET".into(), false);
    };
    let constants = if value.kind() == "element_value_array_initializer" {
        let mut walk = value.walk();
        value
            .named_children(&mut walk)
            .filter(|element| !is_comment(*element))
            .collect::<Vec<_>>()
    } else {
        vec![value]
    };
    let [constant] = constants.as_slice() else {
        return ("?".into(), false);
    };
    let verb = (constant.kind() == "field_access")
        .then(|| {
            let object = constant.child_by_field_name("object")?;
            let field = constant.child_by_field_name("field")?;
            let spelling = cx.text(&object);
            names(
                spelling,
                &format!("{SPRING_WEB}.RequestMethod"),
                imports,
                declared,
                true,
            )
            .then(|| {
                (
                    cx.text(&field),
                    wildcard_only(spelling, "RequestMethod", imports),
                )
            })
        })
        .flatten();
    match verb {
        Some((
            verb @ ("GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "TRACE"),
            wildcard,
        )) => (verb.to_string(), wildcard),
        _ => ("?".into(), false),
    }
}

/// A Feign site whose proof may rest on wildcard imports: `mapping` and
/// `method` name the `<file package>.<Name>` types that would obscure its
/// mapping annotation and its `RequestMethod`.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    site: FetchSite,
    mapping: Option<String>,
    method: Option<String>,
    /// A class-level `@RequestMapping` proven only by a wildcard makes the
    /// path computed unless this same-package type obscures it, in which
    /// case the method's own path stands.
    class_mapping: Option<(String, IdentityExpr)>,
}

/// Settle pending sites against the types this repository declares
/// (`declared` takes a fully qualified name): an obscured mapping proves no
/// site, and an obscured `RequestMethod` leaves the method `?`.
pub(crate) fn resolve(pending: Vec<Pending>, declared: impl Fn(&str) -> bool) -> Vec<FetchSite> {
    pending
        .into_iter()
        .filter(|pending| !pending.mapping.as_deref().is_some_and(&declared))
        .map(|mut pending| {
            if pending.method.as_deref().is_some_and(&declared) {
                pending.site.method = "?".into();
            }
            if let Some((fqn, url)) = pending.class_mapping
                && declared(&fqn)
            {
                pending.site.url = url;
            }
            pending.site
        })
        .collect()
}

/// The literal value of the first present element among `keys`, or of the
/// positional argument when `positional` (Java binds it to `value`).
fn literal_element(
    cx: &FileCx<'_>,
    annotation: TsNode<'_>,
    keys: &[&str],
    positional: bool,
) -> Option<Result<String, String>> {
    let value = keys
        .iter()
        .find_map(|key| retrofit::element(cx, annotation, key))
        .or_else(|| {
            let arguments = annotation.child_by_field_name("arguments")?;
            let mut walk = arguments.walk();
            positional
                .then(|| {
                    arguments
                        .named_children(&mut walk)
                        .find(|argument| !is_comment(*argument))
                        .filter(|argument| argument.kind() != "element_value_pair")
                })
                .flatten()
        })?;
    Some(string_literal(cx, value).ok_or_else(|| cx.text(&value).to_string()))
}

/// The base a client's sites join to.
fn client_base(cx: &FileCx<'_>, client: TsNode<'_>) -> IdentityExpr {
    match literal_element(cx, client, &["url"], false) {
        Some(Ok(url)) if url.contains("${") => IdentityExpr::Computed(url),
        Some(Ok(url)) if !url.is_empty() => {
            let url = url.trim_end_matches('/');
            IdentityExpr::Literal(match url.starts_with("http") {
                true => url.to_string(),
                false => format!("http://{url}"),
            })
        }
        Some(Err(text)) => IdentityExpr::Computed(text),
        // No (or an empty) `url`: the load-balanced service.
        _ => IdentityExpr::Computed(
            match literal_element(cx, client, &["name", "value"], true) {
                Some(Ok(name)) if !name.is_empty() => name,
                Some(Err(text)) => text,
                _ => "unnamed Feign client".into(),
            },
        ),
    }
}

/// The client's `path` prefix, cleaned as OpenFeign does (one leading `/`,
/// no trailing `/`), or `None` when it is not a literal.
fn client_prefix(cx: &FileCx<'_>, client: TsNode<'_>) -> Option<String> {
    match literal_element(cx, client, &["path"], false) {
        None => Some(String::new()),
        Some(Ok(path)) => {
            let path = path.trim_end_matches('/');
            Some(match path.is_empty() || path.starts_with('/') {
                true => path.to_string(),
                false => format!("/{path}"),
            })
        }
        Some(Err(_)) => None,
    }
}

/// Feign sites for the abstract mapped methods of this file's clients,
/// pending the directory join's same-package check.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    methods: &[(TsNode<'_>, String, Vec<String>, String)],
    imports: &Imports,
    package: Option<&str>,
) -> Vec<Pending> {
    if !cx.text(&root).contains("FeignClient") {
        return Vec::new();
    }
    let declared = rest_template::declared_names(cx, root);
    let mut out = Vec::new();
    for (method, symbol, chain, name) in methods {
        let Some(interface) = method
            .parent()
            .and_then(|body| body.parent())
            .filter(|decl| decl.kind() == "interface_declaration")
        else {
            continue;
        };
        // Default and static methods run locally; only abstract ones call.
        if method.kind() != "method_declaration" || method.child_by_field_name("body").is_some() {
            continue;
        }
        let client_annotations = declaration_annotations(cx, interface);
        let Some(client) = client_annotations.iter().find_map(|(simple, annotation)| {
            (simple == "FeignClient"
                && annotation
                    .child_by_field_name("name")
                    .is_some_and(|spelling| {
                        names(cx.text(&spelling), FEIGN_CLIENT, imports, &declared, false)
                    }))
            .then_some(*annotation)
        }) else {
            continue;
        };
        // A proven class-level mapping makes paths computed; one proven only
        // by a wildcard is settled at the join, and a direct proof dominates.
        let class_mapping = client_annotations
            .iter()
            .filter(|(simple, _)| simple == "RequestMapping")
            .filter_map(|(simple, annotation)| {
                mapping_proven(cx, *annotation, simple, imports, &declared)
            })
            .min();
        let obscuring = |name: &str| match package {
            Some(package) => format!("{package}.{name}"),
            None => name.to_string(),
        };
        for (simple, annotation) in declaration_annotations(cx, *method) {
            let Some(wildcard) = mapping_proven(cx, annotation, &simple, imports, &declared) else {
                continue;
            };
            let (http_method, method_wildcard) = match mapping_method(&simple) {
                Some(verb) => (verb.to_string(), false),
                None if simple == "RequestMapping" => {
                    request_mapping_method(cx, annotation, imports, &declared)
                }
                None => continue,
            };
            let path = match client_prefix(cx, client) {
                Some(prefix) => match annotation_path(cx, annotation) {
                    PathArg::Absent => Some(prefix),
                    PathArg::Literal(paths) => match paths.as_slice() {
                        [tail] if tail.is_empty() || tail.starts_with('/') => {
                            Some(format!("{prefix}{tail}"))
                        }
                        [tail] => Some(format!("{prefix}/{tail}")),
                        _ => None,
                    },
                    PathArg::Dynamic => None,
                },
                None => None,
            };
            let entry = if chain.is_empty() {
                name.clone()
            } else {
                format!("{}.{name}", chain.join("."))
            };
            let computed = IdentityExpr::Computed(cx.text(&annotation).to_string());
            let own = match path {
                Some(path) => IdentityExpr::Literal(path),
                None => computed.clone(),
            };
            let (url, class_mapping) = match class_mapping {
                None => (own, None),
                Some(false) => (computed, None),
                Some(true) => (computed, Some((obscuring("RequestMapping"), own))),
            };
            let site = FetchSite {
                method: http_method,
                url,
                symbol: Some(symbol.clone()),
                path: cx.path.to_string(),
                byte_start: method.start_byte() as u64,
                byte_end: method.end_byte() as u64,
                base: Some(FetchBase {
                    url: client_base(cx, client),
                    join: BaseJoin::Concat,
                    path: cx.path.to_string(),
                    byte_start: client.start_byte() as u64,
                }),
                entry: Some(entry),
            };
            out.push(Pending {
                site,
                mapping: wildcard.then(|| obscuring(&simple)),
                method: method_wildcard.then(|| obscuring("RequestMethod")),
                class_mapping,
            });
        }
    }
    out
}
