//! Vue Router screens (#989, AC-0349; the journey ADR in #987, decision 2).
//!
//! A `new VueRouter({ routes })` or `createRouter({ routes })` call, with the
//! callee import-proven from `vue-router`, registers its route table. The
//! table is an array literal at the call, a `const` bound to one, or the
//! single returned array of a function or static method, local or imported
//! relatively. Each route object with a literal path (directly or through a
//! `const` object table such as `Navigation.PATH.MAIN`) becomes a `Screen`
//! that `RENDERS` its component. A component may be imported directly or
//! passed in a `const` object of imported components to the table function
//! (`getRoutes(components)` with `components.Main`). A `.vue` component is
//! its `file:` node. Tables can span files, so routes resolve in
//! [`complete`]. A path or table that cannot be proven is a Gap naming the
//! site; nothing is guessed.

use crate::{FileCx, callable, literal_string};
use core_graph::facts::FactAccess;
use core_graph::{Edge, GraphError, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, HashMap, HashSet};
use tree_sitter::Node as TsNode;

const MODULE: &str = "vue-router";
pub(crate) const ROUTER: &str = "vue-router";

/// `resolution` of a `RENDERS` edge whose extensionless import was found
/// only as a `.vue` file: the bundler's extension list is assumed.
pub const VUE_EXTENSION_ASSUMED: &str = "vue_extension_assumed";

/// Gap reason: a route path that is not a literal or a recovered constant.
pub const COMPUTED_PATH_REASON: &str = "route path is not a literal or a recovered constant";
/// Gap reason: a `routes` value that does not resolve to a route table.
pub const UNRESOLVED_TABLE_REASON: &str = "route table is not a recovered array of routes";
/// Gap reason: a route component that does not resolve to an imported file.
pub const UNRESOLVED_COMPONENT_REASON: &str = "route component is not a recovered import";

/// A relative import as the importer resolved it.
#[derive(Debug, Clone)]
pub(crate) struct Import {
    pub file: String,
    /// The specifier named no extension, so `.ts` was guessed and the other
    /// source extensions are candidates too. An explicit extension names only
    /// itself and its NodeNext siblings.
    pub guessed: bool,
}

/// Where a route's component comes from.
#[derive(Debug, Clone)]
enum Component {
    /// A resolved relative import.
    File(Import),
    /// `param.key` of the table function's `index`th parameter.
    Param {
        index: usize,
        key: String,
    },
    Unresolved,
}

#[derive(Debug, Clone)]
struct Route {
    /// The joined path, or `None` when it is computed.
    path: Option<String>,
    component: Component,
    redirect: bool,
    evidence: EvidenceRef,
}

/// A table function's routes, keyed in [`Pending::tables`] by its file,
/// exported owner and member.
#[derive(Debug, Clone)]
struct Table {
    routes: Vec<Route>,
}

/// A `const` object of imported components passed to a table function:
/// key → resolved import.
type Components = HashMap<String, Import>;

#[derive(Debug, Clone)]
enum TableRef {
    Inline(Vec<Route>),
    Call {
        file: Import,
        export: String,
        member: Option<String>,
        /// Per argument, its components when it is such an object.
        args: Vec<Option<Components>>,
    },
    Unresolved,
}

#[derive(Debug, Clone)]
pub(crate) struct Site {
    table: TableRef,
    evidence: EvidenceRef,
}

/// Per-file facts carried to directory completion.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    /// `(file, exported owner, member)` → table.
    tables: HashMap<(String, String, Option<String>), Table>,
    pub files: HashSet<String>,
    sites: Vec<Site>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Pending) {
        self.tables.extend(other.tables);
        self.files.extend(other.files);
        self.sites.extend(other.sites);
    }

    pub(crate) fn retarget(&mut self, commit: &str) {
        let routes = self
            .tables
            .values_mut()
            .flat_map(|t| t.routes.iter_mut())
            .chain(self.sites.iter_mut().flat_map(|s| match &mut s.table {
                TableRef::Inline(routes) => routes.iter_mut(),
                _ => [].iter_mut(),
            }));
        for route in routes {
            route.evidence.commit_sha = commit.into();
        }
        for site in &mut self.sites {
            site.evidence.commit_sha = commit.into();
        }
    }
}

pub(crate) fn evidence(cx: &FileCx<'_>, node: TsNode<'_>) -> EvidenceRef {
    EvidenceRef {
        repo: cx.id.repo.into(),
        path: cx.path.into(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        commit_sha: cx.id.commit.into(),
    }
}

pub(crate) fn prov(
    evidence: Vec<EvidenceRef>,
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        evidence,
        crate::EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("route provenance has evidence");
    serde_json::to_value(provenance).expect("provenance serializes")
}

/// What the file's imports and bindings prove at a site.
pub(crate) struct Proofs<'a, 'tree> {
    pub bindings: &'a callable::Bindings<'tree>,
    /// Default and namespace imports, and named imports of packages: local →
    /// module specifier as written.
    pub import_modules: &'a HashMap<String, String>,
    pub imported_names: &'a HashMap<String, String>,
    /// Named relative imports: local → (resolved file, exported name).
    pub imported_files: &'a HashMap<String, (String, String)>,
    /// Named relative imports whose specifier named no extension.
    pub guessed_imports: &'a HashSet<String>,
}

impl Proofs<'_, '_> {
    /// `(resolved import, exported name)` of a relatively imported local.
    pub(crate) fn relative(
        &self,
        cx: &FileCx<'_>,
        site: TsNode<'_>,
        local: &str,
    ) -> Option<(Import, String)> {
        if !self.bindings.imported(site, local) {
            return None;
        }
        if let Some((file, exported)) = self.imported_files.get(local) {
            let guessed = self.guessed_imports.contains(local);
            return Some((
                Import {
                    file: file.clone(),
                    guessed,
                },
                exported.clone(),
            ));
        }
        let spec = self.import_modules.get(local)?;
        let file = crate::resolve_relative(cx.path, spec)?;
        let guessed = crate::guesses_extension(spec);
        Some((Import { file, guessed }, "default".into()))
    }

    /// Whether `local` at `site` is `exported` of `vue-router`.
    fn vue_router(&self, site: TsNode<'_>, local: &str, exported: &str) -> bool {
        self.bindings.imported(site, local)
            && self.import_modules.get(local).map(String::as_str) == Some(MODULE)
            && match self.imported_names.get(local) {
                Some(name) => name == exported,
                None => exported == "default",
            }
    }
}

/// A `const` declarator's value.
pub(crate) fn const_value(declarator: TsNode<'_>) -> Option<TsNode<'_>> {
    let is_const = declarator
        .parent()
        .filter(|p| p.kind() == "lexical_declaration")
        .and_then(|p| p.child(0))
        .is_some_and(|keyword| keyword.kind() == "const");
    (declarator.kind() == "variable_declarator" && is_const)
        .then(|| declarator.child_by_field_name("value"))
        .flatten()
}

fn property_key(cx: &FileCx<'_>, key: TsNode<'_>) -> Option<String> {
    match key.kind() {
        "property_identifier" | "identifier" => Some(cx.text(&key).to_string()),
        "string" => literal_string(cx, key),
        _ => None,
    }
}

/// An object literal's `key → value` entries (pairs and shorthands).
pub(crate) fn object_entries<'tree>(
    cx: &FileCx<'_>,
    object: TsNode<'tree>,
) -> Vec<(String, TsNode<'tree>)> {
    let mut walk = object.walk();
    object
        .named_children(&mut walk)
        .filter_map(|entry| match entry.kind() {
            "pair" => Some((
                property_key(cx, entry.child_by_field_name("key")?)?,
                entry.child_by_field_name("value")?,
            )),
            "shorthand_property_identifier" => Some((cx.text(&entry).to_string(), entry)),
            _ => None,
        })
        .collect()
}

/// The file's literal constant tables: top-level `const X = { k: "…" }`
/// objects, and class static fields that name one (`static PATH = PATH`)
/// or hold one.
#[derive(Default)]
pub(crate) struct Constants {
    pub(crate) objects: HashMap<String, HashMap<String, String>>,
    /// `(class, field)` → table.
    pub(crate) statics: HashMap<(String, String), HashMap<String, String>>,
}

fn literal_object(cx: &FileCx<'_>, object: TsNode<'_>) -> HashMap<String, String> {
    object_entries(cx, object)
        .into_iter()
        .filter_map(|(key, value)| Some((key, literal_string(cx, value)?)))
        .collect()
}

pub(crate) fn constants(cx: &FileCx<'_>, root: TsNode<'_>) -> Constants {
    let mut out = Constants::default();
    let mut walk = root.walk();
    let statements: Vec<_> = root
        .named_children(&mut walk)
        .map(|s| {
            if s.kind() == "export_statement" {
                s.child_by_field_name("declaration").unwrap_or(s)
            } else {
                s
            }
        })
        .collect();
    for statement in &statements {
        if statement.kind() != "lexical_declaration" {
            continue;
        }
        let mut inner = statement.walk();
        for declarator in statement.named_children(&mut inner) {
            if let (Some(value), Some(name)) = (
                const_value(declarator),
                declarator.child_by_field_name("name"),
            ) && value.kind() == "object"
            {
                out.objects
                    .insert(cx.text(&name).to_string(), literal_object(cx, value));
            }
        }
    }
    for statement in &statements {
        if statement.kind() != "class_declaration" {
            continue;
        }
        let (Some(class), Some(body)) = (
            statement.child_by_field_name("name"),
            statement.child_by_field_name("body"),
        ) else {
            continue;
        };
        let mut inner = body.walk();
        for field in body.named_children(&mut inner) {
            if field.kind() != "public_field_definition" || !is_static(field) {
                continue;
            }
            let (Some(name), Some(value)) = (
                field.child_by_field_name("name"),
                field.child_by_field_name("value"),
            ) else {
                continue;
            };
            let table = match value.kind() {
                "object" => Some(literal_object(cx, value)),
                "identifier" => out.objects.get(cx.text(&value)).cloned(),
                _ => None,
            };
            if let Some(table) = table {
                out.statics.insert(
                    (cx.text(&class).to_string(), cx.text(&name).to_string()),
                    table,
                );
            }
        }
    }
    out
}

fn is_static(member: TsNode<'_>) -> bool {
    let mut walk = member.walk();
    member
        .children(&mut walk)
        .any(|child| child.kind() == "static")
}

/// A route path: a string literal, or `TABLE.KEY` / `Class.FIELD.KEY`
/// through the file's constant tables.
pub(crate) fn path_value(
    cx: &FileCx<'_>,
    constants: &Constants,
    value: TsNode<'_>,
) -> Option<String> {
    if let Some(text) = literal_string(cx, value) {
        return Some(text);
    }
    if value.kind() != "member_expression" {
        return None;
    }
    let key = cx.text(&value.child_by_field_name("property")?);
    let object = value.child_by_field_name("object")?;
    let table = match object.kind() {
        "identifier" => constants.objects.get(cx.text(&object))?,
        "member_expression" => {
            let class = object.child_by_field_name("object")?;
            let field = object.child_by_field_name("property")?;
            (class.kind() == "identifier").then_some(())?;
            constants
                .statics
                .get(&(cx.text(&class).to_string(), cx.text(&field).to_string()))?
        }
        _ => return None,
    };
    table.get(key).cloned()
}

fn join(prefix: &str, path: &str) -> String {
    // A default child (`path: ''`) is the parent's own URL.
    if path.is_empty() {
        return prefix.to_string();
    }
    if path.starts_with('/') || prefix.is_empty() {
        return path.to_string();
    }
    format!("{}/{path}", prefix.trim_end_matches('/'))
}

/// Context for reading one route array.
struct Reader<'a, 'b, 'tree> {
    cx: &'a FileCx<'b>,
    proofs: &'a Proofs<'a, 'tree>,
    constants: &'a Constants,
    /// The table function's parameter names, in order.
    params: &'a [String],
}

impl Reader<'_, '_, '_> {
    fn component(&self, value: TsNode<'_>) -> Component {
        match value.kind() {
            "identifier" | "shorthand_property_identifier" => {
                let local = self.cx.text(&value);
                match self.proofs.relative(self.cx, value, local) {
                    Some((file, _)) => Component::File(file),
                    None => Component::Unresolved,
                }
            }
            "member_expression" => {
                let (Some(object), Some(property)) = (
                    value.child_by_field_name("object"),
                    value.child_by_field_name("property"),
                ) else {
                    return Component::Unresolved;
                };
                let name = self.cx.text(&object);
                match self.params.iter().position(|p| p == name) {
                    Some(index) if object.kind() == "identifier" => Component::Param {
                        index,
                        key: self.cx.text(&property).to_string(),
                    },
                    _ => Component::Unresolved,
                }
            }
            _ => Component::Unresolved,
        }
    }

    fn routes(&self, array: TsNode<'_>, prefix: &str, out: &mut Vec<Route>) {
        let mut walk = array.walk();
        for element in array.named_children(&mut walk) {
            if element.kind() != "object" {
                continue;
            }
            let entries: BTreeMap<_, _> = object_entries(self.cx, element).into_iter().collect();
            let Some(path) = entries.get("path") else {
                continue;
            };
            // A parameter or local named like a module table is not that table.
            let shadowed = member_root(*path)
                .is_some_and(|root| locally_bound(self.cx, *path, self.cx.text(&root)));
            let path = (!shadowed)
                .then(|| path_value(self.cx, self.constants, *path))
                .flatten()
                .map(|p| join(prefix, &p));
            let component = entries
                .get("component")
                .map_or(Component::Unresolved, |c| self.component(*c));
            out.push(Route {
                redirect: entries.contains_key("redirect") && !entries.contains_key("component"),
                path: path.clone(),
                component,
                evidence: evidence(self.cx, element),
            });
            if let (Some(children), Some(path)) = (entries.get("children"), &path)
                && children.kind() == "array"
            {
                self.routes(*children, path, out);
            }
        }
    }
}

/// The one array a function body returns: an arrow's expression body, or a
/// block whose only return statement returns an array literal.
fn returned_array(function: TsNode<'_>) -> Option<TsNode<'_>> {
    let body = function.child_by_field_name("body")?;
    if body.kind() == "array" {
        return Some(body);
    }
    if body.kind() != "statement_block" {
        return None;
    }
    let mut returns = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        for child in node.named_children(&mut walk) {
            if child.kind() == "return_statement" {
                returns.push(child);
            } else if !callable::is_callable(child) {
                stack.push(child);
            }
        }
    }
    let [statement] = returns[..] else {
        return None;
    };
    let mut walk = statement.walk();
    let value = statement.named_children(&mut walk).next()?;
    (value.kind() == "array").then_some(value)
}

pub(crate) fn parameter_names(cx: &FileCx<'_>, function: TsNode<'_>) -> Vec<String> {
    let Some(parameters) = function.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut walk = parameters.walk();
    parameters
        .named_children(&mut walk)
        .map(|p| {
            let pattern = p.child_by_field_name("pattern").unwrap_or(p);
            cx.text(&pattern).to_string()
        })
        .collect()
}

/// Exported name → local name, for `export default X`, `export class X`,
/// `export function f` and `export { a as b }`.
fn exports(cx: &FileCx<'_>, root: TsNode<'_>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut walk = root.walk();
    for statement in root.named_children(&mut walk) {
        if statement.kind() != "export_statement" {
            continue;
        }
        let default = {
            let mut w = statement.walk();
            statement.children(&mut w).any(|c| c.kind() == "default")
        };
        if let Some(declaration) = statement.child_by_field_name("declaration") {
            if let Some(name) = declaration.child_by_field_name("name") {
                let local = cx.text(&name).to_string();
                out.insert(
                    if default {
                        "default".into()
                    } else {
                        local.clone()
                    },
                    local,
                );
            }
        } else if let Some(value) = statement.child_by_field_name("value")
            && default
            && value.kind() == "identifier"
        {
            out.insert("default".into(), cx.text(&value).to_string());
        } else if statement.child_by_field_name("source").is_none() {
            // `export { a as b }` of this file's own bindings.
            let mut w = statement.walk();
            let clauses: Vec<_> = statement
                .named_children(&mut w)
                .filter(|c| c.kind() == "export_clause")
                .collect();
            for clause in clauses {
                let mut cw = clause.walk();
                for specifier in clause.named_children(&mut cw) {
                    if specifier.kind() != "export_specifier" {
                        continue;
                    }
                    let Some(name) = specifier.child_by_field_name("name") else {
                        continue;
                    };
                    let local = cx.text(&name).to_string();
                    let exported = specifier
                        .child_by_field_name("alias")
                        .map_or_else(|| local.clone(), |a| cx.text(&a).to_string());
                    out.insert(exported, local);
                }
            }
        }
    }
    out
}

/// Whether `name` is declared again inside a callable enclosing `site`, as a
/// parameter or by any declaration in its body. Module bindings are matched
/// by spelling, so this over-approximates (a sibling block counts) and can
/// only refuse a match.
pub(crate) fn locally_bound(cx: &FileCx<'_>, site: TsNode<'_>, name: &str) -> bool {
    let mut node = site;
    while let Some(parent) = node.parent() {
        if callable::is_callable(parent)
            && (parameter_names(cx, parent).iter().any(|p| p == name) || declares(cx, parent, name))
        {
            return true;
        }
        node = parent;
    }
    false
}

fn declares(cx: &FileCx<'_>, scope: TsNode<'_>, name: &str) -> bool {
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        let bound = match node.kind() {
            "variable_declarator" => node.child_by_field_name("name"),
            "function_declaration" | "class_declaration" if node.id() != scope.id() => {
                node.child_by_field_name("name")
            }
            "catch_clause" => node.child_by_field_name("parameter"),
            _ => None,
        };
        let Some(bound) = bound else {
            continue;
        };
        let mut inner = vec![bound];
        while let Some(part) = inner.pop() {
            if matches!(
                part.kind(),
                "identifier" | "shorthand_property_identifier_pattern"
            ) && cx.text(&part) == name
            {
                return true;
            }
            let mut walk = part.walk();
            inner.extend(part.named_children(&mut walk));
        }
    }
    false
}

/// The identifier a `TABLE.KEY` or `Class.FIELD.KEY` path starts from.
pub(crate) fn member_root(node: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut node = node;
    while node.kind() == "member_expression" {
        node = node.child_by_field_name("object")?;
    }
    (node.kind() == "identifier").then_some(node)
}

/// The file's exported top-level functions and exported classes' static
/// methods: `(exported owner, member, function)`.
pub(crate) fn exported_functions<'tree>(
    cx: &FileCx<'_>,
    root: TsNode<'tree>,
) -> Vec<(String, Option<String>, TsNode<'tree>)> {
    let exports = exports(cx, root);
    let local_to_export: HashMap<&str, &str> = exports
        .iter()
        .map(|(exported, local)| (local.as_str(), exported.as_str()))
        .collect();
    let mut out = Vec::new();
    let mut walk = root.walk();
    for statement in root.named_children(&mut walk) {
        let declaration = if statement.kind() == "export_statement" {
            statement
                .child_by_field_name("declaration")
                .unwrap_or(statement)
        } else {
            statement
        };
        let Some(name) = declaration.child_by_field_name("name") else {
            continue;
        };
        let Some(export) = local_to_export.get(cx.text(&name)) else {
            continue;
        };
        match declaration.kind() {
            "function_declaration" => out.push((export.to_string(), None, declaration)),
            "class_declaration" => {
                let Some(body) = declaration.child_by_field_name("body") else {
                    continue;
                };
                let mut inner = body.walk();
                for method in body.named_children(&mut inner) {
                    if method.kind() == "method_definition"
                        && is_static(method)
                        && let Some(member) = method.child_by_field_name("name")
                    {
                        out.push((
                            export.to_string(),
                            Some(cx.text(&member).to_string()),
                            method,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Index the file's route-table functions and its router registrations.
pub(crate) fn extract<'tree>(
    cx: &FileCx<'_>,
    root: TsNode<'tree>,
    proofs: &Proofs<'_, 'tree>,
    pending: &mut Pending,
) {
    pending.files.insert(cx.path.to_string());
    let constants = constants(cx, root);
    // Route-table functions: exported top-level functions and class static
    // methods that return one array of route objects.
    for (export, member, function) in exported_functions(cx, root) {
        let Some(array) = returned_array(function) else {
            continue;
        };
        let params = parameter_names(cx, function);
        let reader = Reader {
            cx,
            proofs,
            constants: &constants,
            params: &params,
        };
        let mut routes = Vec::new();
        reader.routes(array, "", &mut routes);
        if !routes.is_empty() {
            pending
                .tables
                .insert((cx.path.to_string(), export, member), Table { routes });
        }
    }
    // Router registrations.
    for node in callable::walk(root) {
        let (callee, arguments) = match node.kind() {
            "new_expression" => (
                node.child_by_field_name("constructor"),
                node.child_by_field_name("arguments"),
            ),
            "call_expression" => (
                node.child_by_field_name("function"),
                node.child_by_field_name("arguments"),
            ),
            _ => continue,
        };
        let (Some(callee), Some(arguments)) = (callee, arguments) else {
            continue;
        };
        if callee.kind() != "identifier" {
            continue;
        }
        let local = cx.text(&callee);
        let proven = match node.kind() {
            "new_expression" => proofs.vue_router(node, local, "default"),
            _ => proofs.vue_router(node, local, "createRouter"),
        };
        if !proven {
            continue;
        }
        let mut aw = arguments.walk();
        let Some(options) = arguments
            .named_children(&mut aw)
            .next()
            .filter(|o| o.kind() == "object")
        else {
            continue;
        };
        let Some((_, value)) = object_entries(cx, options)
            .into_iter()
            .find(|(key, _)| key == "routes")
        else {
            continue;
        };
        let reader = Reader {
            cx,
            proofs,
            constants: &constants,
            params: &[],
        };
        let table = table_ref(cx, proofs, &reader, value);
        pending.sites.push(Site {
            table,
            evidence: evidence(cx, node),
        });
    }
}

/// Resolve a `routes` value at a router registration.
fn table_ref<'tree>(
    cx: &FileCx<'_>,
    proofs: &Proofs<'_, 'tree>,
    reader: &Reader<'_, '_, 'tree>,
    value: TsNode<'tree>,
) -> TableRef {
    let mut value = value;
    if matches!(value.kind(), "identifier" | "shorthand_property_identifier") {
        let Some(bound) = proofs
            .bindings
            .stable_declaration(value, cx.text(&value))
            .and_then(const_value)
        else {
            return TableRef::Unresolved;
        };
        value = bound;
    }
    match value.kind() {
        "array" => {
            let mut routes = Vec::new();
            reader.routes(value, "", &mut routes);
            TableRef::Inline(routes)
        }
        "call_expression" => call_table(cx, proofs, value).unwrap_or(TableRef::Unresolved),
        _ => TableRef::Unresolved,
    }
}

/// `Owner.member(args)` or `f(args)` with the callee imported relatively.
fn call_table(cx: &FileCx<'_>, proofs: &Proofs<'_, '_>, call: TsNode<'_>) -> Option<TableRef> {
    let function = call.child_by_field_name("function")?;
    let (local, member) = match function.kind() {
        "identifier" => (function, None),
        "member_expression" => (
            function.child_by_field_name("object")?,
            Some(
                cx.text(&function.child_by_field_name("property")?)
                    .to_string(),
            ),
        ),
        _ => return None,
    };
    if local.kind() != "identifier" {
        return None;
    }
    let (file, export) = proofs.relative(cx, call, cx.text(&local))?;
    let arguments = call.child_by_field_name("arguments")?;
    let mut walk = arguments.walk();
    let args = arguments
        .named_children(&mut walk)
        .map(|argument| {
            let object = if argument.kind() == "object" {
                argument
            } else if argument.kind() == "identifier" {
                proofs
                    .bindings
                    .stable_declaration(argument, cx.text(&argument))
                    .and_then(const_value)
                    .filter(|v| v.kind() == "object")?
            } else {
                return None;
            };
            Some(
                object_entries(cx, object)
                    .into_iter()
                    .filter_map(|(key, value)| {
                        matches!(value.kind(), "identifier" | "shorthand_property_identifier")
                            .then_some(())?;
                        let (file, _) = proofs.relative(cx, value, cx.text(&value))?;
                        Some((key, file))
                    })
                    .collect(),
            )
        })
        .collect();
    Some(TableRef::Call {
        file,
        export,
        member,
        args,
    })
}

/// Candidate paths a relative import names, in resolution order.
pub(crate) fn file_candidates(import: &Import) -> Vec<String> {
    let file = import.file.as_str();
    if !import.guessed {
        return crate::target_tests::explicit_candidates(file);
    }
    let mut out = vec![file.to_string()];
    if let Some(stem) = file.strip_suffix(".ts") {
        for ext in [".tsx", ".js", ".mjs", ".cjs", ".jsx", ".vue"] {
            out.push(format!("{stem}{ext}"));
        }
        for ext in [".ts", ".tsx", ".js", ".vue"] {
            out.push(format!("{stem}/index{ext}"));
        }
    }
    out
}

fn push_gap(
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    id: String,
    reason: &str,
    evidence: Vec<EvidenceRef>,
    from: Option<&str>,
) {
    nodes.push(Node {
        id: id.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "kind": "route-unresolved",
            "reason": reason,
            "attempted_tiers": ["T0"],
            "prov": prov(evidence.clone(), ConfidenceTier::Gap, &format!("Gap {id}")),
        }),
    });
    if let Some(src) = from {
        edges.push(Edge {
            src: src.into(),
            dst: id.clone(),
            label: "RENDERS".into(),
            props: serde_json::json!({
                "prov": prov(evidence, ConfidenceTier::Gap, &format!("RENDERS {src} -> {id}")),
            }),
        });
    }
}

/// Resolve every router registration against the directory's route tables.
pub(crate) fn complete(
    pending: Pending,
    repo: &str,
    facts: &mut impl FactAccess,
) -> Result<(), GraphError> {
    let resolve = |import: &Import| {
        file_candidates(import)
            .into_iter()
            .find(|candidate| pending.files.contains(candidate))
    };
    let gap_id = |e: &EvidenceRef| format!("gap:route:{}@{}@{}", e.repo, e.path, e.byte_start);
    let mut screens: BTreeMap<String, (Node, Vec<Edge>)> = BTreeMap::new();
    let (mut gap_nodes, mut gap_edges) = (Vec::new(), Vec::new());
    for site in &pending.sites {
        let (routes, args): (Vec<Route>, &[Option<Components>]) = match &site.table {
            TableRef::Inline(routes) => (routes.clone(), &[]),
            TableRef::Call {
                file,
                export,
                member,
                args,
            } => {
                let table = resolve(file)
                    .and_then(|file| pending.tables.get(&(file, export.clone(), member.clone())));
                match table {
                    Some(table) => (table.routes.clone(), args.as_slice()),
                    None => {
                        push_gap(
                            &mut gap_nodes,
                            &mut gap_edges,
                            gap_id(&site.evidence),
                            UNRESOLVED_TABLE_REASON,
                            vec![site.evidence.clone()],
                            None,
                        );
                        continue;
                    }
                }
            }
            TableRef::Unresolved => {
                push_gap(
                    &mut gap_nodes,
                    &mut gap_edges,
                    gap_id(&site.evidence),
                    UNRESOLVED_TABLE_REASON,
                    vec![site.evidence.clone()],
                    None,
                );
                continue;
            }
        };
        for route in routes {
            let evidence = vec![route.evidence.clone(), site.evidence.clone()];
            let Some(path) = route.path else {
                push_gap(
                    &mut gap_nodes,
                    &mut gap_edges,
                    gap_id(&route.evidence),
                    COMPUTED_PATH_REASON,
                    evidence,
                    None,
                );
                continue;
            };
            let screen = format!("screen:{repo}@{path}");
            let mut props = serde_json::json!({
                "route": path,
                "router": ROUTER,
                "prov": prov(evidence.clone(), ConfidenceTier::Confirmed, &format!("Screen {path}")),
            });
            if route.redirect {
                props["redirect"] = true.into();
            }
            let imported = match &route.component {
                Component::File(file) => Some(file),
                Component::Param { index, key } => args
                    .get(*index)
                    .and_then(Option::as_ref)
                    .and_then(|map| map.get(key)),
                Component::Unresolved => None,
            };
            let component = imported.and_then(|imported| Some((imported, resolve(imported)?)));
            let renders = match component {
                Some((imported, file)) => {
                    // An extensionless import found only as a `.vue` file
                    // assumes the bundler resolves that extension.
                    let assumed = file != imported.file && file.ends_with(".vue");
                    let confidence = if assumed {
                        ConfidenceTier::InferredStrong
                    } else {
                        ConfidenceTier::Confirmed
                    };
                    let mut props = serde_json::json!({
                        "prov": prov(evidence, confidence, &format!("RENDERS {screen} -> {file}")),
                    });
                    if assumed {
                        props["resolution"] = VUE_EXTENSION_ASSUMED.into();
                    }
                    Some(Edge {
                        src: screen.clone(),
                        dst: crate::file_id(repo, &file),
                        label: "RENDERS".into(),
                        props,
                    })
                }
                None if route.redirect => None,
                None => {
                    push_gap(
                        &mut gap_nodes,
                        &mut gap_edges,
                        gap_id(&route.evidence),
                        UNRESOLVED_COMPONENT_REASON,
                        evidence,
                        Some(&screen),
                    );
                    None
                }
            };
            // The same route from two registrations is one screen; a
            // rendering registration wins over a redirect. A layout and its
            // default child (`path: ''`) both render at that URL.
            let entry = screens.entry(screen.clone()).or_insert((
                Node {
                    id: screen,
                    label: "Screen".into(),
                    props: props.clone(),
                },
                Vec::new(),
            ));
            if let Some(renders) = renders {
                if entry.1.is_empty() {
                    entry.0.props = props;
                }
                if !entry.1.iter().any(|e| e.dst == renders.dst) {
                    entry.1.push(renders);
                }
            }
        }
    }
    let (mut nodes, mut edges) = (Vec::new(), Vec::new());
    for (_, (node, renders)) in screens {
        nodes.push(node);
        edges.extend(renders);
    }
    nodes.extend(gap_nodes);
    edges.extend(gap_edges);
    facts.append(nodes, edges)
}

#[cfg(test)]
mod fact_access_tests {
    use super::*;
    use core_graph::facts::{FactLists, FactStore};

    fn evidence(byte_start: u64) -> EvidenceRef {
        EvidenceRef {
            repo: "r".into(),
            path: "web/router.ts".into(),
            byte_start,
            byte_end: byte_start + 1,
            commit_sha: "c".into(),
        }
    }

    fn route(path: Option<&str>, component: Component, at: u64) -> Route {
        Route {
            path: path.map(str::to_string),
            component,
            redirect: false,
            evidence: evidence(at),
        }
    }

    fn pending() -> Pending {
        let home = Import {
            file: "web/Home.vue".into(),
            guessed: false,
        };
        Pending {
            tables: HashMap::new(),
            files: HashSet::from(["web/Home.vue".to_string()]),
            sites: vec![
                Site {
                    table: TableRef::Inline(vec![
                        route(Some("/home"), Component::File(home), 1),
                        route(None, Component::Unresolved, 2),
                        route(Some("/lost"), Component::Unresolved, 3),
                    ]),
                    evidence: evidence(10),
                },
                Site {
                    table: TableRef::Unresolved,
                    evidence: evidence(20),
                },
            ],
        }
    }

    // AC-0538: completion over the packed store appends exactly what it
    // appends over plain lists, after the facts already recovered.
    #[test]
    fn route_completion_is_identical_over_lists_and_store() {
        let nodes = vec![Node {
            id: "file:r@web/Home.vue".into(),
            label: "File".into(),
            props: serde_json::json!({}),
        }];
        let (mut listed, mut listed_edges) = (nodes.clone(), Vec::new());
        complete(
            pending(),
            "r",
            &mut FactLists {
                nodes: &mut listed,
                edges: &mut listed_edges,
            },
        )
        .unwrap();
        let mut store = FactStore::from_facts(nodes.clone(), Vec::new()).unwrap();
        complete(pending(), "r", &mut store).unwrap();
        assert_eq!(
            store.into_facts().unwrap(),
            (listed.clone(), listed_edges.clone())
        );
        assert_eq!(listed[0], nodes[0]);
        let labels: Vec<_> = listed.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labels, ["File", "Screen", "Screen", "Gap", "Gap", "Gap"]);
        assert!(listed_edges.iter().any(|e| e.src == "screen:r@/home"
            && e.dst == "file:r@web/Home.vue"
            && e.label == "RENDERS"));
    }
}
