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
/// An arrow's expression body is itself the scope's only statement. A
/// nested function declaration is included, because its name binds here,
/// but its body is another scope.
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
            if node.kind() == "function_declaration" {
                out.push(node);
            }
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

/// True when `name` at `node` may not be the top-level binding of that
/// name: an enclosing function declares it or takes it as a parameter, a
/// block below the program rebinds it, or the program declares it twice.
fn locally_bound(cx: &FileCx, node: TsNode, name: &str) -> bool {
    let declares = |decl: TsNode| match decl.kind() {
        "variable_declarator" | "function_declaration" | "class_declaration" => decl
            .child_by_field_name("name")
            .is_some_and(|n| binds(cx, n, name)),
        "catch_clause" => decl
            .child_by_field_name("parameter")
            .is_some_and(|n| binds(cx, n, name)),
        "for_in_statement" => decl
            .child_by_field_name("left")
            .is_some_and(|n| binds(cx, n, name)),
        _ => false,
    };
    let mut scope = scope_of(node);
    while scope.kind() != "program" {
        let in_params = scope
            .child_by_field_name("parameters")
            .or_else(|| scope.child_by_field_name("parameter"))
            .is_some_and(|params| binds(cx, params, name));
        if in_params
            || scope_nodes(scope)
                .into_iter()
                .any(|n| n != scope && declares(n))
        {
            return true;
        }
        // A named function expression binds its own name inside itself.
        if scope.kind() == "function_expression"
            && scope
                .child_by_field_name("name")
                .is_some_and(|n| cx.text(&n) == name)
        {
            return true;
        }
        scope = match scope.parent() {
            Some(parent) => scope_of(parent),
            None => return true,
        };
    }
    let program = scope;
    let top_level = |decl: TsNode| {
        let mut statement = decl;
        if decl.kind() == "variable_declarator"
            && let Some(parent) = decl.parent()
        {
            statement = parent;
        }
        let mut parent = statement.parent();
        if parent.is_some_and(|p| p.kind() == "export_statement") {
            parent = parent.and_then(|p| p.parent());
        }
        parent == Some(program)
    };
    let declarations: Vec<_> = scope_nodes(program)
        .into_iter()
        .filter(|n| declares(*n))
        .collect();
    // A block-scoped declaration shadows only inside its own block.
    let encloses = |decl: TsNode| {
        // A catch or for-of binding is scoped to the statement it heads.
        let mut block = match decl.kind() {
            "catch_clause" | "for_in_statement" => Some(decl),
            _ => decl.parent(),
        };
        while let Some(b) = block {
            if matches!(
                b.kind(),
                "statement_block" | "for_statement" | "for_in_statement" | "catch_clause"
            ) {
                break;
            }
            block = b.parent();
        }
        block
            .is_some_and(|b| b.start_byte() <= node.start_byte() && node.end_byte() <= b.end_byte())
    };
    let top: Vec<_> = declarations
        .iter()
        .filter(|decl| top_level(**decl))
        .collect();
    top.len() > 1
        || declarations
            .iter()
            .any(|decl| !top_level(*decl) && encloses(*decl))
}

/// True when the binding pattern `pattern` introduces `name`.
fn binds(cx: &FileCx, pattern: TsNode, name: &str) -> bool {
    let mut stack = vec![pattern];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "identifier" | "shorthand_property_identifier_pattern"
        ) && cx.text(&node) == name
        {
            return true;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    false
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

/// What a fetch call's options prove about its `method`.
enum MethodOption<'t> {
    /// No options argument, or an object literal without `method`: GET.
    Absent,
    /// The `method` value, written after any spread that could override it.
    Value(TsNode<'t>),
    /// Options the extractor cannot see (`fetch(url, options)`), or a
    /// spread that may supply the method: never assumed to be GET.
    Unknown,
}

/// The `method` option of a fetch call's second argument.
fn method_option<'t>(cx: &FileCx, args: &[TsNode<'t>]) -> MethodOption<'t> {
    let Some(options) = args.get(1) else {
        return MethodOption::Absent;
    };
    let Some(options) = through_binding(cx, *options).filter(|o| o.kind() == "object") else {
        return MethodOption::Unknown;
    };
    // Later properties override earlier ones, spreads and keys the
    // extractor cannot read included.
    let mut walk = options.walk();
    let mut proven = MethodOption::Absent;
    for property in options.named_children(&mut walk) {
        let key = match property.kind() {
            "shorthand_property_identifier" => Some(cx.text(&property).to_string()),
            "pair" | "method_definition" => property
                .child_by_field_name(if property.kind() == "pair" {
                    "key"
                } else {
                    "name"
                })
                .and_then(|key| static_key(cx, key)),
            "comment" => continue,
            _ => None,
        };
        match (property.kind(), key.as_deref()) {
            (_, Some(key)) if key != "method" => {}
            ("shorthand_property_identifier", _) => proven = MethodOption::Value(property),
            ("pair", Some(_)) => match property.child_by_field_name("value") {
                Some(value) => proven = MethodOption::Value(value),
                None => proven = MethodOption::Unknown,
            },
            // A spread, an accessor or method named `method`, or a computed
            // key: any of them may set the method.
            _ => proven = MethodOption::Unknown,
        }
    }
    proven
}

/// The name an object key spells exactly: `method`, `"method"`, or a
/// computed `['method']`. `None` when the key is computed from anything else.
fn static_key(cx: &FileCx, key: TsNode) -> Option<String> {
    match key.kind() {
        "property_identifier" | "private_property_identifier" | "number" => {
            Some(cx.text(&key).to_string())
        }
        "string" => literal_text(cx, key),
        "computed_property_name" => key
            .named_child(0)
            .filter(|inner| key.named_child_count() == 1 && inner.kind() == "string")
            .and_then(|inner| literal_text(cx, inner)),
        _ => None,
    }
}

/// A string literal's characters without its quotes; `None` when an
/// escape sequence makes the source spelling differ from the value.
fn literal_text(cx: &FileCx, string: TsNode) -> Option<String> {
    let text = cx.text(&string);
    (!text.contains('\\')).then(|| text[1..text.len() - 1].to_string())
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

/// True when `node` is exactly one of `names`, not an expression over it.
fn is_name(cx: &FileCx, node: TsNode, names: &BTreeSet<String>) -> bool {
    let mut node = unwrap_assertions(node);
    while node.kind() == "parenthesized_expression" {
        match node.named_child(0) {
            Some(inner) => node = unwrap_assertions(inner),
            None => return false,
        }
    }
    matches!(node.kind(), "identifier" | "member_expression") && names.contains(cx.text(&node))
}

/// A same-file top-level function named `name` that only substitutes
/// placeholders in its first parameter and returns it: every assignment to
/// the parameter is `p = p.replace(…)` (or `replaceAll`) and every return
/// is `return p`. Its result keeps the route's path template, so the join
/// still names that route (`bindQueryParams(path, params)`).
fn path_binder(cx: &FileCx, at: TsNode, name: &str) -> bool {
    let mut program = at;
    while let Some(parent) = program.parent() {
        program = parent;
    }
    let mut walk = program.walk();
    let functions: Vec<_> = program
        .named_children(&mut walk)
        .filter_map(|statement| {
            let statement = match statement.kind() {
                "export_statement" => statement.child_by_field_name("declaration")?,
                _ => statement,
            };
            match statement.kind() {
                "function_declaration" => (statement
                    .child_by_field_name("name")
                    .is_some_and(|n| cx.text(&n) == name))
                .then_some(statement),
                _ => None,
            }
        })
        .collect();
    let [function] = functions.as_slice() else {
        return false;
    };
    let Some(param) = function
        .child_by_field_name("parameters")
        .and_then(|params| params.named_child(0))
        .and_then(|first| match first.kind() {
            "identifier" => Some(first),
            _ => first.child_by_field_name("pattern"),
        })
        .filter(|param| param.kind() == "identifier")
    else {
        return false;
    };
    let param = cx.text(&param);
    let Some(body) = function.child_by_field_name("body") else {
        return false;
    };
    let mut returns = 0;
    for node in scope_nodes(body) {
        match node.kind() {
            "return_statement" => {
                let returned = node.named_child(0);
                if !returned.is_some_and(|r| r.kind() == "identifier" && cx.text(&r) == param) {
                    return false;
                }
                returns += 1;
            }
            "augmented_assignment_expression" | "update_expression"
                if node
                    .named_child(0)
                    .is_some_and(|target| cx.text(&target) == param) =>
            {
                return false;
            }
            "assignment_expression"
                if node
                    .child_by_field_name("left")
                    .is_some_and(|left| cx.text(&left) == param) =>
            {
                let call = node
                    .child_by_field_name("right")
                    .filter(|right| right.kind() == "call_expression");
                let substitutes = call
                    .and_then(|call| call.child_by_field_name("function"))
                    .filter(|callee| callee.kind() == "member_expression")
                    .is_some_and(|callee| {
                        callee
                            .child_by_field_name("object")
                            .is_some_and(|object| cx.text(&object) == param)
                            && callee
                                .child_by_field_name("property")
                                .is_some_and(|p| matches!(cx.text(&p), "replace" | "replaceAll"))
                    })
                    && call
                        .and_then(|call| call_args(call).first().copied())
                        .is_some_and(|pattern| placeholder(cx, pattern));
                if !substitutes {
                    return false;
                }
            }
            _ => {}
        }
    }
    returns > 0
}

/// True when `pattern` can only match a route placeholder: `` `:${key}` ``,
/// `` `{${key}}` ``, `':id'`, or `'{id}'`. Replacing anything else rewrites
/// the route into some other URL.
fn placeholder(cx: &FileCx, pattern: TsNode) -> bool {
    let pattern = unwrap_assertions(pattern);
    match pattern.kind() {
        "string" => {
            let Some(text) = literal_text(cx, pattern) else {
                return false;
            };
            let name = text
                .strip_prefix(':')
                .or_else(|| text.strip_prefix('{').and_then(|t| t.strip_suffix('}')));
            name.is_some_and(|name| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        }
        "template_string" => {
            let mut walk = pattern.walk();
            let parts: Vec<_> = pattern.named_children(&mut walk).collect();
            let fragments: Vec<_> = parts
                .iter()
                .filter(|part| part.kind() == "string_fragment")
                .map(|part| cx.text(part))
                .collect();
            let substitutions = parts
                .iter()
                .filter(|part| part.kind() == "template_substitution")
                .count();
            let text = cx.text(&pattern);
            substitutions == 1
                && parts.len() == fragments.len() + 1
                && (fragments == [":"] && text.starts_with("`:${")
                    || fragments == ["{", "}"] && text.starts_with("`{${"))
        }
        _ => false,
    }
}

/// True when `node` is the route path itself, or a [`path_binder`] call
/// taking it as its first argument: anything else (`path + "/x"`) is some
/// other URL.
fn route_path(cx: &FileCx, node: TsNode, names: &BTreeSet<String>) -> bool {
    if is_name(cx, node, names) {
        return true;
    }
    let node = unwrap_assertions(node);
    node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .filter(|f| f.kind() == "identifier")
            .is_some_and(|f| {
                !locally_bound(cx, node, cx.text(&f)) && path_binder(cx, node, cx.text(&f))
            })
        && call_args(node)
            .first()
            .is_some_and(|first| is_name(cx, *first, names))
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
        // The route's path itself, untransformed: `route.path + "/x"` is
        // some other URL.
        if !route_path(cx, rest, &paths) || mentions(cx, base, &paths) {
            return None;
        }
        let MethodOption::Value(method) = method_option(cx, &args) else {
            return None;
        };
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
                    if locally_bound(&cx, *node, &entry.0) {
                        continue;
                    }
                    let method = match method_option(&cx, &args) {
                        MethodOption::Absent => Method::Default,
                        MethodOption::Unknown => Method::Computed,
                        MethodOption::Value(m) => match literal_string(&cx, m) {
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
                } else if let Some(entry) = args.first().and_then(|arg| entry_ref(&cx, *arg))
                    && !locally_bound(&cx, *node, callee)
                    && !locally_bound(&cx, *node, &entry.0)
                {
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
