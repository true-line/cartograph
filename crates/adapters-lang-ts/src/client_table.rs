//! Table-driven HTTP clients (#737, AC-0315): deterministic T0 client calls
//! for code that keeps its API as data and joins it to a base URL at runtime.
//!
//! A **route table** is a top-level const object whose entries carry a
//! literal `method` and `path` (`const API = { LOGIN: { method: "POST",
//! path: "/api/auth/login" } }`). A **join site** prepends a base URL to a
//! route's path and hands it to the platform `fetch`, either directly
//! (`fetch(base + API.LOGIN.path, …)`) or inside a **wrapper** function whose
//! first parameter is the route (`const { method, path } = route; fetch(base
//! + path, { method })`), called as `callAPI(API.LOGIN, …)`.
//!
//! Each proven join becomes one [`FetchSite`] at its call: the entry's
//! literal method and path, the join's base expression as [`FetchBase`],
//! and the entry name. Bindings are proven like the messaging pass: the
//! table and wrapper are declared in the same file or imported through a
//! relative specifier resolving to the repo file that exports them. A
//! base read from runtime state stays `Computed`; the `events` crate
//! resolves it only through a system-manifest declaration and otherwise
//! records a Gap naming the entry — never a guess.

use adapters_fw::client::{BaseJoin, FetchBase, FetchSite};
use adapters_fw::events::IdentityExpr;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use crate::chrome_messaging::unwrap_assertions;
use crate::const_resolution::{ConstIndex, collect_imports, is_exported, module_candidates};
use crate::{ExtractError, FileCx, SourceId, enclosing_symbol, literal_string, object_entries};

const VERBS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// A route-table entry: literal method and path.
#[derive(Debug, Clone)]
struct Route {
    method: String,
    path: String,
}

/// A wrapper's join: the base expression and where it is written.
#[derive(Debug, Clone)]
struct Join {
    base: IdentityExpr,
    path: String,
    byte_start: u64,
}

/// How a join site chooses its HTTP method.
#[derive(Debug, Clone)]
enum Method {
    /// The route entry's own `method` (wrapper calls, `T.K.method`).
    Route,
    /// A literal in the fetch options.
    Literal(String),
    /// No `method` option: `fetch` defaults to GET.
    Default,
    /// Not statically visible.
    Computed,
}

/// A call or fetch that names a route entry, awaiting binding proof.
struct PendingCall {
    file: String,
    /// The wrapper called, or `None` for a direct `fetch(base + T.K.path)`.
    wrapper: Option<String>,
    /// The direct join's base, when `wrapper` is `None`.
    join: Option<Join>,
    table: String,
    key: String,
    method: Method,
    symbol: Option<String>,
    byte_start: u64,
    byte_end: u64,
}

#[derive(Default)]
struct RepoIndex {
    imports: ConstIndex,
    /// (file, table name) → entries.
    tables: BTreeMap<(String, String), BTreeMap<String, Route>>,
    /// (file, wrapper name) → its join.
    wrappers: BTreeMap<(String, String), Join>,
    /// (file, exported name) → local name, for `export { a as b }` clauses
    /// and exported declarations.
    exports: BTreeMap<(String, String), String>,
    calls: Vec<PendingCall>,
}

impl RepoIndex {
    /// Prove which definition `name` names as seen from `file`: a same-file
    /// definition, else an import of a name the target file exports.
    fn prove<T>(
        &self,
        defs: &BTreeMap<(String, String), T>,
        file: &str,
        name: &str,
    ) -> Option<(String, String)> {
        let local = (file.to_string(), name.to_string());
        if defs.contains_key(&local) {
            return Some(local);
        }
        let (spec, original) = self.imports.import_binding(file, name)?;
        for candidate in js_candidates(file, spec) {
            let Some(defined) = self.exports.get(&(candidate.clone(), original.clone())) else {
                continue;
            };
            let key = (candidate, defined.clone());
            if defs.contains_key(&key) {
                return Some(key);
            }
        }
        None
    }
}

/// [`module_candidates`] plus the JavaScript and Vue files an
/// extensionless or `.js` specifier may name in a JavaScript repo.
fn js_candidates(from: &str, spec: &str) -> Vec<String> {
    let mut out = module_candidates(from, spec);
    let Some(first) = out.first().cloned() else {
        return out;
    };
    let base = first
        .strip_suffix(".ts")
        .or_else(|| first.strip_suffix(".tsx"))
        .unwrap_or(&first)
        .to_string();
    for ext in ["", ".js", ".jsx", ".mjs", ".vue", "/index.js"] {
        let candidate = format!("{base}{ext}");
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// A route-table entry: an object with a literal HTTP verb and path.
fn route(cx: &FileCx, value: TsNode) -> Option<Route> {
    let value = unwrap_assertions(value);
    if value.kind() != "object" {
        return None;
    }
    let entries = object_entries(cx, value);
    let field = |name: &str| {
        entries
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, node)| literal_string(cx, *node))
    };
    let method = field("method")?.to_ascii_uppercase();
    let path = field("path")?;
    VERBS
        .contains(&method.as_str())
        .then_some(Route { method, path })
}

/// Classify a base expression: a literal, `process.env.KEY`, or the
/// runtime expression's source text.
fn classify_base(cx: &FileCx, node: TsNode) -> IdentityExpr {
    crate::classify_identity(cx, &node, &HashMap::new())
}

/// The nearest enclosing function body, or the program.
fn scope_of(node: TsNode) -> TsNode {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(
            parent.kind(),
            "function_declaration" | "function_expression" | "arrow_function" | "method_definition"
        ) {
            return parent;
        }
        current = parent;
    }
    current
}

/// `scope` and every node under it that is not inside a nested function.
/// An arrow's expression body is itself the scope's only statement.
fn scope_nodes(scope: TsNode) -> Vec<TsNode> {
    let mut out = Vec::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if node != scope
            && matches!(
                node.kind(),
                "function_declaration"
                    | "function_expression"
                    | "arrow_function"
                    | "method_definition"
            )
        {
            continue;
        }
        out.push(node);
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    out
}

/// The value of the only declaration of `name` in `scope`, if exactly one.
fn single_binding<'t>(cx: &FileCx, scope: TsNode<'t>, name: &str) -> Option<TsNode<'t>> {
    let mut found = scope_nodes(scope).into_iter().filter(|node| {
        node.kind() == "variable_declarator"
            && node
                .child_by_field_name("name")
                .is_some_and(|n| n.kind() == "identifier" && cx.text(&n) == name)
    });
    let declarator = found.next()?;
    if found.next().is_some() {
        return None;
    }
    // A later assignment makes the binding unstable.
    let reassigned = scope_nodes(scope).into_iter().any(|node| {
        matches!(
            node.kind(),
            "assignment_expression" | "augmented_assignment_expression"
        ) && node
            .child_by_field_name("left")
            .is_some_and(|left| cx.text(&left) == name)
    });
    (!reassigned)
        .then(|| declarator.child_by_field_name("value"))
        .flatten()
        .map(unwrap_assertions)
}

/// Follow one local identifier to its single binding in the call's scope.
fn through_binding<'t>(cx: &FileCx, node: TsNode<'t>) -> Option<TsNode<'t>> {
    let node = unwrap_assertions(node);
    if node.kind() == "identifier" {
        return single_binding(cx, scope_of(node), cx.text(&node));
    }
    Some(node)
}

/// `left + right` split, when `node` is a string concatenation.
fn concat<'t>(cx: &FileCx, node: TsNode<'t>) -> Option<(TsNode<'t>, TsNode<'t>)> {
    if node.kind() != "binary_expression" {
        return None;
    }
    let operator = node.child_by_field_name("operator")?;
    if cx.text(&operator) != "+" {
        return None;
    }
    Some((
        node.child_by_field_name("left")?,
        node.child_by_field_name("right")?,
    ))
}

/// `T.K.<field>` → (`T`, `K`), when `node` reads that field of a table entry.
fn entry_field(cx: &FileCx, node: TsNode, field: &str) -> Option<(String, String)> {
    let node = unwrap_assertions(node);
    if node.kind() != "member_expression" {
        return None;
    }
    let property = node.child_by_field_name("property")?;
    if cx.text(&property) != field {
        return None;
    }
    entry_ref(cx, node.child_by_field_name("object")?)
}

/// `T.K` → (`T`, `K`).
fn entry_ref(cx: &FileCx, node: TsNode) -> Option<(String, String)> {
    let node = unwrap_assertions(node);
    if node.kind() != "member_expression" {
        return None;
    }
    let object = node.child_by_field_name("object")?;
    let property = node.child_by_field_name("property")?;
    (object.kind() == "identifier" && property.kind() == "property_identifier")
        .then(|| (cx.text(&object).to_string(), cx.text(&property).to_string()))
}

/// The `method` option of a fetch call's second argument.
fn method_option<'t>(cx: &FileCx, args: &[TsNode<'t>]) -> Option<TsNode<'t>> {
    let options = through_binding(cx, *args.get(1)?)?;
    if options.kind() != "object" {
        return None;
    }
    object_entries(cx, options)
        .into_iter()
        .find(|(key, _)| key == "method")
        .map(|(_, node)| node)
}

/// Names a wrapper binds to its route parameter's `field`: `route.path`,
/// or the identifier destructured from it (`const { path } = route`).
fn route_names(cx: &FileCx, body: TsNode, param: &str, field: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::from([format!("{param}.{field}")]);
    for node in scope_nodes(body) {
        if node.kind() != "variable_declarator" {
            continue;
        }
        let (Some(pattern), Some(value)) = (
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
        ) else {
            continue;
        };
        if pattern.kind() != "object_pattern" || cx.text(&value) != param {
            continue;
        }
        let mut walk = pattern.walk();
        for part in pattern.named_children(&mut walk) {
            match part.kind() {
                "shorthand_property_identifier_pattern" if cx.text(&part) == field => {
                    names.insert(field.to_string());
                }
                "pair_pattern" => {
                    let key = part.child_by_field_name("key");
                    let value = part.child_by_field_name("value");
                    if let (Some(key), Some(value)) = (key, value)
                        && cx.text(&key) == field
                        && value.kind() == "identifier"
                    {
                        names.insert(cx.text(&value).to_string());
                    }
                }
                _ => {}
            }
        }
    }
    names
}

/// True when `node` or a descendant reads one of `names`.
fn mentions(cx: &FileCx, node: TsNode, names: &BTreeSet<String>) -> bool {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if matches!(current.kind(), "identifier" | "member_expression")
            && names.contains(cx.text(&current))
        {
            return true;
        }
        let mut walk = current.walk();
        stack.extend(current.named_children(&mut walk));
    }
    false
}

/// The arguments of a call, named children only.
fn call_args<'t>(call: TsNode<'t>) -> Vec<TsNode<'t>> {
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut walk = args.walk();
    args.named_children(&mut walk).collect()
}

/// The wrapper's join: its body hands `base + <route path>` to `fetch`
/// with the route's own method.
fn wrapper_join(cx: &FileCx, function: TsNode) -> Option<Join> {
    let parameters = function.child_by_field_name("parameters")?;
    let mut walk = parameters.walk();
    let first = parameters.named_children(&mut walk).next()?;
    let param = match first.kind() {
        "required_parameter" | "optional_parameter" => first.child_by_field_name("pattern")?,
        "identifier" => first,
        _ => return None,
    };
    if param.kind() != "identifier" {
        return None;
    }
    let param = cx.text(&param);
    let body = function.child_by_field_name("body")?;
    let paths = route_names(cx, body, param, "path");
    let methods = route_names(cx, body, param, "method");
    let mut joins = scope_nodes(body).into_iter().filter_map(|node| {
        let function = node.child_by_field_name("function")?;
        if node.kind() != "call_expression" || cx.text(&function) != "fetch" {
            return None;
        }
        let args = call_args(node);
        let (base, rest) = concat(cx, through_binding(cx, *args.first()?)?)?;
        if !mentions(cx, rest, &paths) || mentions(cx, base, &paths) {
            return None;
        }
        let method = method_option(cx, &args)?;
        methods.contains(cx.text(&method)).then(|| Join {
            base: classify_base(cx, base),
            path: cx.path.to_string(),
            byte_start: base.start_byte() as u64,
        })
    });
    let join = joins.next()?;
    // Two joins in one wrapper may use different bases: not one join site.
    joins.next().is_none().then_some(join)
}

/// True when the file binds its own `fetch`, shadowing the platform API.
fn fetch_shadowed(cx: &FileCx, nodes: &[TsNode], imports: &ConstIndex) -> bool {
    imports.import_binding(cx.path, "fetch").is_some()
        || nodes.iter().any(|node| {
            matches!(
                node.kind(),
                "variable_declarator" | "function_declaration" | "required_parameter"
            ) && node
                .child_by_field_name("name")
                .or_else(|| node.child_by_field_name("pattern"))
                .is_some_and(|name| cx.text(&name) == "fetch")
        })
}

fn extract_file(
    source: &[u8],
    path: &str,
    id: &SourceId,
    index: &mut RepoIndex,
) -> Result<(), ExtractError> {
    let is_tsx = crate::vue::component_tsx(source, path).unwrap_or(path.ends_with(".tsx"));
    let language: tree_sitter::Language = if is_tsx {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    };
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let view = crate::vue::parse_view(source, path);
    let source: &[u8] = &view;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ExtractError::NoTree(path.into()))?;
    let root = tree.root_node();
    let cx = FileCx {
        source,
        path,
        id,
        registries: adapters_fw::events::Registries::builtin(),
    };
    collect_imports(&cx, root, &language, &mut index.imports);

    let mut nodes = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        nodes.push(node);
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    nodes.sort_by_key(|node| node.start_byte());
    let shadowed = fetch_shadowed(&cx, &nodes, &index.imports);

    for node in &nodes {
        match node.kind() {
            // `export { a, b as c }` without a source re-exports local names.
            "export_specifier"
                if node.parent().and_then(|p| p.parent()).is_some_and(|s| {
                    s.kind() == "export_statement" && s.child_by_field_name("source").is_none()
                }) =>
            {
                let Some(name) = node.child_by_field_name("name") else {
                    continue;
                };
                let exported = node.child_by_field_name("alias").unwrap_or(name);
                index.exports.insert(
                    (path.to_string(), cx.text(&exported).to_string()),
                    cx.text(&name).to_string(),
                );
            }
            "variable_declarator" | "function_declaration" => {
                let Some(name) = node.child_by_field_name("name") else {
                    continue;
                };
                if name.kind() != "identifier" {
                    continue;
                }
                let name = cx.text(&name).to_string();
                let key = (path.to_string(), name.clone());
                let function = match node.kind() {
                    "function_declaration" => Some(*node),
                    _ => node
                        .child_by_field_name("value")
                        .map(unwrap_assertions)
                        .filter(|v| matches!(v.kind(), "function_expression" | "arrow_function")),
                };
                let top_level = scope_of(*node).kind() == "program";
                if let Some(function) = function {
                    if top_level
                        && !shadowed
                        && let Some(join) = wrapper_join(&cx, function)
                    {
                        index.wrappers.insert(key.clone(), join);
                    }
                } else if top_level
                    && let Some(value) = node.child_by_field_name("value")
                    && unwrap_assertions(value).kind() == "object"
                {
                    let routes: BTreeMap<String, Route> =
                        object_entries(&cx, unwrap_assertions(value))
                            .into_iter()
                            .filter_map(|(entry, value)| Some((entry, route(&cx, value)?)))
                            .collect();
                    if !routes.is_empty() {
                        index.tables.insert(key.clone(), routes);
                    }
                }
                if is_exported(*node) {
                    index.exports.insert(key, name);
                }
            }
            "call_expression" => {
                let Some(function) = node.child_by_field_name("function") else {
                    continue;
                };
                let args = call_args(*node);
                let site = |wrapper, join, (table, key): (String, String), method| PendingCall {
                    file: path.to_string(),
                    wrapper,
                    join,
                    table,
                    key,
                    method,
                    symbol: enclosing_symbol(&cx, *node),
                    byte_start: node.start_byte() as u64,
                    byte_end: node.end_byte() as u64,
                };
                if function.kind() != "identifier" {
                    continue;
                }
                let callee = cx.text(&function);
                if callee == "fetch" {
                    // Direct join: `fetch(base + T.K.path, { method })`.
                    if shadowed {
                        continue;
                    }
                    let Some((base, rest)) = args
                        .first()
                        .and_then(|url| through_binding(&cx, *url))
                        .and_then(|url| concat(&cx, url))
                    else {
                        continue;
                    };
                    let Some(entry) = entry_field(&cx, rest, "path") else {
                        continue;
                    };
                    let method = match method_option(&cx, &args) {
                        None => Method::Default,
                        Some(m) => match literal_string(&cx, m) {
                            Some(lit) => Method::Literal(lit.to_ascii_uppercase()),
                            None if entry_field(&cx, m, "method").as_ref() == Some(&entry) => {
                                Method::Route
                            }
                            None => Method::Computed,
                        },
                    };
                    let join = Join {
                        base: classify_base(&cx, base),
                        path: path.to_string(),
                        byte_start: base.start_byte() as u64,
                    };
                    index.calls.push(site(None, Some(join), entry, method));
                } else if let Some(entry) = args.first().and_then(|arg| entry_ref(&cx, *arg)) {
                    // Wrapper call: `callAPI(T.K, …)`, proven at directory level.
                    index
                        .calls
                        .push(site(Some(callee.to_string()), None, entry, Method::Route));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Recover table-driven client calls across the repo (AC-0315). Sites are
/// returned in deterministic file and byte order.
pub fn extract_dir(root: &Path, id: &SourceId) -> Result<Vec<FetchSite>, ExtractError> {
    let mut files = Vec::new();
    crate::collect_ts_files(root, &mut files)?;
    files.sort(); // deterministic order (US-0014)
    let mut index = RepoIndex::default();
    for rel in &files {
        let source = std::fs::read(root.join(rel))?;
        extract_file(&source, rel, id, &mut index)?;
    }

    let mut out = Vec::new();
    for call in &index.calls {
        let Some(table_key) = index.prove(&index.tables, &call.file, &call.table) else {
            continue;
        };
        let Some(route) = index.tables[&table_key].get(&call.key) else {
            continue;
        };
        let join = match &call.wrapper {
            Some(wrapper) => match index.prove(&index.wrappers, &call.file, wrapper) {
                Some(key) => index.wrappers[&key].clone(),
                None => continue,
            },
            None => match &call.join {
                Some(join) => join.clone(),
                None => continue,
            },
        };
        let method = match &call.method {
            Method::Route => route.method.clone(),
            Method::Literal(method) => method.clone(),
            Method::Default => "GET".into(),
            Method::Computed => "?".into(),
        };
        out.push(FetchSite {
            method,
            url: IdentityExpr::Literal(route.path.clone()),
            symbol: call.symbol.clone(),
            path: call.file.clone(),
            byte_start: call.byte_start,
            byte_end: call.byte_end,
            base: Some(FetchBase {
                url: join.base,
                join: BaseJoin::Concat,
                path: join.path,
                byte_start: join.byte_start,
            }),
            // Named as declared, whatever the call site imported it as.
            entry: Some(format!("{}.{}", table_key.1, call.key)),
        });
    }
    out.sort_by(|a, b| (&a.path, a.byte_start).cmp(&(&b.path, b.byte_start)));
    Ok(out)
}

#[cfg(test)]
mod tests;
