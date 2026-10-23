//! Observed infrastructure (#734, AC-0316): what the code runs on or talks
//! to when the repository declares no IaC for it. Two deterministic rule
//! sources, both receipt-backed:
//!
//! - **Registry clients** ([`Registries::clients`]): `new <Export>(…)` or
//!   `<Export>(…)` whose binding is import- or require-proven from the
//!   entry's module (`new S3Client()`, `createClient(url, key)`). The data
//!   table layers like every registry, so a plugin adds SDKs without code.
//! - **Browser storage globals**: `indexedDB.open(name)`, `caches.open(name)`
//!   and any `localStorage`/`sessionStorage` use, unless the file binds the
//!   name itself (fail closed, like a shadowed `chrome`).
//! - **HTTP APIs**: the global `fetch(url)` and registry HTTP clients (axios,
//!   ky). Each call whose URL starts with a proven absolute `http(s)` origin
//!   uses one `http-api` resource per origin. A relative URL names no other
//!   origin and is skipped; a runtime-computed one is a Gap, except for
//!   `fetch`, whose computed URLs the client-call pass already records.
//!
//! Each proven site becomes a `USES` edge from its enclosing symbol to one
//! `Resource` per provider and instance identity (`props.observed = true`,
//! `kind` = the family). A literal identity names the instance; a URL keeps
//! only its scheme and authority (never credentials, paths or queries); an
//! env reference records the variable name and never its value. A
//! runtime-computed identity becomes a Gap naming the site, never a guess.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use tree_sitter::{Node as TsNode, Parser};

use adapters_fw::events::{
    ClientForm, Ecosystem, IdentityArg, IdentityExpr, ObservedClient, ObservedFamily, Registries,
    RegistryIdentity,
};

use crate::chrome_messaging::unwrap_assertions;
use crate::{
    ExtractError, Extraction, FileCx, SourceId, classify_identity, enclosing_symbol, file_id,
    pair_value_in_args,
};

const EXTRACTOR_ID: &str = "t0.observed-infra";

/// Longest literal identity kept on a resource id.
const MAX_IDENTITY: usize = 128;

/// How a local name is bound to a module.
#[derive(Debug, Clone, PartialEq)]
enum Binding {
    /// `import X from 'm'`, `const X = require('m')`.
    Default,
    /// `import { a as X } from 'm'`, `const { a: X } = require('m')`.
    Named(String),
    /// `import * as X from 'm'`.
    Namespace,
}

/// One proven site before identities are grouped into resources.
struct Site {
    family: ObservedFamily,
    provider: String,
    module: Option<String>,
    identity: Option<IdentityExpr>,
    registry: Option<RegistryIdentity>,
    symbol: Option<String>,
    path: String,
    byte_start: u64,
    byte_end: u64,
}

/// Browser storage globals and the call that names an instance.
const STORAGE_GLOBALS: &[(&str, Option<&str>, &str)] = &[
    ("indexedDB", Some("open"), "indexeddb"),
    ("caches", Some("open"), "cache-storage"),
    ("localStorage", None, "web-storage"),
    ("sessionStorage", None, "web-storage"),
];

/// Receivers that reach a browser global (`window.indexedDB`).
const GLOBAL_OBJECTS: &[&str] = &["window", "self", "globalThis"];

fn prov_value(
    id: &SourceId,
    path: &str,
    span: (u64, u64),
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        vec![EvidenceRef {
            repo: id.repo.into(),
            path: path.into(),
            byte_start: span.0,
            byte_end: span.1,
            commit_sha: id.commit.into(),
        }],
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic within ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

/// What a declaration binds its name to.
#[derive(Debug, Clone, PartialEq)]
enum Declared {
    /// An ES import or a `require` of a module.
    Module(String, Binding),
    /// `const X = 'literal'`.
    Literal(String),
    /// Anything else: a parameter, a local, a function, a class.
    Other,
}

/// One declaration and the byte range of the scope it binds in.
struct Decl {
    scope: (usize, usize),
    value: Declared,
    /// Declared through `require`, which a local `require` would void.
    required: bool,
    /// A `let` or `var` binding, which a write anywhere in the file voids.
    mutable: bool,
}

/// Every declaration in one file by name, for lexical resolution at a
/// site (#758 review): a binding proves a site only when it is the
/// innermost declaration of that name whose scope encloses the site.
#[derive(Default)]
struct Scopes {
    decls: HashMap<String, Vec<Decl>>,
    /// Properties some expression writes on a bare identifier, by object
    /// name: `None` when the key is unknown (`X[k] = …`,
    /// `Object.assign(X, …)`).
    member_writes: HashMap<String, HashSet<Option<String>>>,
}

impl Scopes {
    fn declare(&mut self, name: &str, scope: TsNode, value: Declared, required: bool) {
        self.decls.entry(name.to_string()).or_default().push(Decl {
            scope: (scope.start_byte(), scope.end_byte()),
            value,
            required,
            mutable: false,
        });
    }

    /// A `require` binding; `mutable` when declared with `let` or `var`.
    fn declare_required(&mut self, name: &str, scope: TsNode, value: Declared, mutable: bool) {
        self.declare(name, scope, value, true);
        if let Some(decl) = self.decls.get_mut(name).and_then(|decls| decls.last_mut()) {
            decl.mutable = mutable;
        }
    }

    /// What `name` means at `at`: `None` when no declaration encloses it
    /// (a global), else the innermost one. Two different declarations in
    /// that one scope prove nothing.
    fn resolve(&self, name: &str, at: TsNode) -> Option<Declared> {
        let (start, end) = (at.start_byte(), at.end_byte());
        let enclosing: Vec<&Decl> = self
            .decls
            .get(name)?
            .iter()
            .filter(|decl| decl.scope.0 <= start && end <= decl.scope.1)
            .collect();
        let innermost = enclosing.iter().map(|d| d.scope.1 - d.scope.0).min()?;
        let mut at_scope = enclosing
            .into_iter()
            .filter(|decl| decl.scope.1 - decl.scope.0 == innermost);
        let first = at_scope.next()?;
        if at_scope.any(|decl| decl.value != first.value) {
            return Some(Declared::Other);
        }
        Some(first.value.clone())
    }

    /// True when the file may overwrite `object.property` (#758 review).
    fn member_written(&self, object: &str, property: &str) -> bool {
        self.member_writes
            .get(object)
            .is_some_and(|keys| keys.contains(&None) || keys.contains(&Some(property.to_string())))
    }

    /// True when `name` is the platform global at `at`.
    fn global(&self, name: &str, at: TsNode) -> bool {
        self.resolve(name, at).is_none()
    }
}

const FUNCTIONS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "arrow_function",
    "method_definition",
    "generator_function_declaration",
    "generator_function",
];

/// The nearest ancestor of `node` (exclusive) whose kind is in `kinds`, or
/// the root.
fn nearest<'t>(node: TsNode<'t>, kinds: &[&str]) -> TsNode<'t> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if kinds.contains(&parent.kind()) {
            return parent;
        }
        current = parent;
    }
    current
}

/// Block scope of a `let`/`const`, function or class declaration.
fn block_of(node: TsNode) -> TsNode {
    let mut kinds = vec![
        "statement_block",
        "program",
        "for_statement",
        "for_in_statement",
        "switch_body",
        "class_body",
    ];
    kinds.extend_from_slice(FUNCTIONS);
    nearest(node, &kinds)
}

/// Every name a binding pattern introduces (defaults and keys excluded).
fn pattern_names<'t>(pattern: TsNode<'t>, out: &mut Vec<TsNode<'t>>) {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => out.push(pattern),
        "pair_pattern" => {
            if let Some(value) = pattern.child_by_field_name("value") {
                pattern_names(value, out);
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            if let Some(left) = pattern.child_by_field_name("left") {
                pattern_names(left, out);
            }
        }
        "required_parameter" | "optional_parameter" => {
            if let Some(inner) = pattern.child_by_field_name("pattern") {
                pattern_names(inner, out);
            }
        }
        _ => {
            let mut walk = pattern.walk();
            for child in pattern.named_children(&mut walk) {
                if !matches!(child.kind(), "type_annotation" | "accessibility_modifier") {
                    pattern_names(child, out);
                }
            }
        }
    }
}

/// The module string of `require('m')`, if `node` is exactly that call.
fn required_module(cx: &FileCx, node: TsNode) -> Option<String> {
    let node = unwrap_assertions(node);
    if node.kind() != "call_expression"
        || node.child_by_field_name("function").map(|f| cx.text(&f)) != Some("require")
    {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    let mut walk = args.walk();
    let first = args.named_children(&mut walk).next()?;
    crate::literal_string(cx, first)
}

/// Every declaration in the file: imports, `require` and literal `const`
/// bindings with their values, and every other binding as [`Declared::Other`].
fn collect_scopes(cx: &FileCx, root: TsNode) -> Scopes {
    let mut scopes = Scopes::default();
    // Names some expression writes, wherever it is: a mutable binding
    // written anywhere proves nothing (#758 review).
    let mut written = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        let target = match node.kind() {
            "assignment_expression" | "augmented_assignment_expression" => {
                node.child_by_field_name("left")
            }
            "update_expression" => node.child_by_field_name("argument"),
            "unary_expression"
                if node
                    .child_by_field_name("operator")
                    .is_some_and(|op| cx.text(&op) == "delete") =>
            {
                node.child_by_field_name("argument")
            }
            "call_expression" => {
                let reflective = node
                    .child_by_field_name("function")
                    .is_some_and(|f| REFLECTIVE_WRITES.contains(&cx.text(&unwrap_assertions(f))));
                if reflective
                    && let Some(object) = node
                        .child_by_field_name("arguments")
                        .and_then(|args| args.named_child(0))
                        .map(write_object)
                        .filter(|object| object.kind() == "identifier")
                {
                    scopes
                        .member_writes
                        .entry(cx.text(&object).to_string())
                        .or_default()
                        .insert(None);
                }
                None
            }
            "for_in_statement" if node.child_by_field_name("kind").is_none() => {
                node.child_by_field_name("left")
            }
            _ => None,
        };
        if let Some(target) = target {
            member_targets(cx, target, &mut scopes.member_writes);
        }
        // A member write (`AWS.config.region = …`) leaves the binding intact.
        if let Some(target) = target.filter(|t| {
            matches!(
                t.kind(),
                "identifier" | "object_pattern" | "array_pattern" | "parenthesized_expression"
            )
        }) {
            let mut names = Vec::new();
            pattern_names(target, &mut names);
            written.extend(names.iter().map(|name| cx.text(name).to_string()));
        }
        match node.kind() {
            "import_statement" => {
                let Some(source) = node
                    .child_by_field_name("source")
                    .and_then(|s| crate::literal_string(cx, s))
                else {
                    continue;
                };
                let mut walk = node.walk();
                let Some(clause) = node
                    .named_children(&mut walk)
                    .find(|c| c.kind() == "import_clause")
                else {
                    continue;
                };
                let mut walk = clause.walk();
                for part in clause.named_children(&mut walk) {
                    match part.kind() {
                        "identifier" => scopes.declare(
                            cx.text(&part),
                            root,
                            Declared::Module(source.clone(), Binding::Default),
                            false,
                        ),
                        "namespace_import" => {
                            if let Some(local) = part.named_child(0) {
                                scopes.declare(
                                    cx.text(&local),
                                    root,
                                    Declared::Module(source.clone(), Binding::Namespace),
                                    false,
                                );
                            }
                        }
                        "named_imports" => {
                            let mut walk = part.walk();
                            for spec in part.named_children(&mut walk) {
                                let Some(name) = spec.child_by_field_name("name") else {
                                    continue;
                                };
                                let local = spec.child_by_field_name("alias").unwrap_or(name);
                                scopes.declare(
                                    cx.text(&local),
                                    root,
                                    Declared::Module(
                                        source.clone(),
                                        Binding::Named(cx.text(&name).to_string()),
                                    ),
                                    false,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            "variable_declarator" => {
                let Some(name) = node.child_by_field_name("name") else {
                    continue;
                };
                let declaration = node.parent();
                let is_var = declaration.is_some_and(|d| d.kind() == "variable_declaration");
                let is_const = declaration.is_some_and(|d| {
                    d.kind() == "lexical_declaration"
                        && d.child(0).is_some_and(|k| cx.text(&k) == "const")
                });
                let scope = if is_var {
                    nearest(node, FUNCTIONS)
                } else {
                    block_of(node)
                };
                let value = node.child_by_field_name("value");
                let module = value.and_then(|v| required_module(cx, v));
                match (name.kind(), &module) {
                    ("identifier", Some(module)) => scopes.declare_required(
                        cx.text(&name),
                        scope,
                        Declared::Module(module.clone(), Binding::Default),
                        !is_const,
                    ),
                    ("identifier", None) => {
                        let literal = value
                            .map(unwrap_assertions)
                            .filter(|_| is_const)
                            .and_then(|v| crate::literal_string(cx, v));
                        scopes.declare(
                            cx.text(&name),
                            scope,
                            literal.map_or(Declared::Other, Declared::Literal),
                            false,
                        );
                    }
                    ("object_pattern", Some(module)) => {
                        let mut walk = name.walk();
                        for part in name.named_children(&mut walk) {
                            let named = match part.kind() {
                                "shorthand_property_identifier_pattern" => {
                                    Some((part, cx.text(&part).to_string()))
                                }
                                "pair_pattern" => part
                                    .child_by_field_name("value")
                                    .filter(|v| v.kind() == "identifier")
                                    .zip(part.child_by_field_name("key"))
                                    .map(|(v, k)| (v, cx.text(&k).to_string())),
                                _ => None,
                            };
                            match named {
                                Some((local, export)) => scopes.declare_required(
                                    cx.text(&local),
                                    scope,
                                    Declared::Module(module.clone(), Binding::Named(export)),
                                    !is_const,
                                ),
                                None => {
                                    let mut names = Vec::new();
                                    pattern_names(part, &mut names);
                                    for local in names {
                                        scopes.declare(
                                            cx.text(&local),
                                            scope,
                                            Declared::Other,
                                            false,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        let mut names = Vec::new();
                        pattern_names(name, &mut names);
                        for local in names {
                            scopes.declare(cx.text(&local), scope, Declared::Other, false);
                        }
                    }
                }
            }
            "function_declaration" | "generator_function_declaration" | "class_declaration" => {
                if let (Some(name), Some(parent)) =
                    (node.child_by_field_name("name"), node.parent())
                {
                    let scope = if parent.kind() == "export_statement" {
                        block_of(parent)
                    } else {
                        block_of(node)
                    };
                    scopes.declare(cx.text(&name), scope, Declared::Other, false);
                }
            }
            "catch_clause" | "for_in_statement" => {
                let field = if node.kind() == "catch_clause" {
                    "parameter"
                } else {
                    "left"
                };
                if let Some(pattern) = node.child_by_field_name(field) {
                    let mut names = Vec::new();
                    pattern_names(pattern, &mut names);
                    for local in names {
                        scopes.declare(cx.text(&local), node, Declared::Other, false);
                    }
                }
            }
            _ => {}
        }
        if FUNCTIONS.contains(&node.kind()) {
            // A function expression binds its own name inside itself.
            if matches!(node.kind(), "function_expression" | "generator_function")
                && let Some(name) = node.child_by_field_name("name")
            {
                scopes.declare(cx.text(&name), node, Declared::Other, false);
            }
            let params = node
                .child_by_field_name("parameters")
                .or_else(|| node.child_by_field_name("parameter"));
            if let Some(params) = params {
                let mut names = Vec::new();
                pattern_names(params, &mut names);
                for local in names {
                    scopes.declare(cx.text(&local), node, Declared::Other, false);
                }
            }
        }
    }
    // A file that declares its own `require` proves no module through it.
    let own_require = scopes.decls.contains_key("require");
    for (name, decls) in scopes.decls.iter_mut() {
        let rewritten = written.contains(name);
        for decl in decls.iter_mut() {
            if decl.required && own_require || decl.mutable && rewritten {
                decl.value = Declared::Other;
            }
        }
    }
    scopes
}

/// Classify an identity argument at its site. An identifier is a literal
/// only when the innermost binding visible there is a literal `const`;
/// `process.env.X` and `import.meta.env.X` are env references while
/// `process` is the global.
fn classify(cx: &FileCx, node: TsNode, scopes: &Scopes) -> IdentityExpr {
    let node = unwrap_assertions(node);
    let text = cx.text(&node);
    match node.kind() {
        "identifier" => match scopes.resolve(text, node) {
            Some(Declared::Literal(value)) => IdentityExpr::Literal(value),
            _ => IdentityExpr::Computed(text.to_string()),
        },
        "member_expression" if text.starts_with("import.meta.env.") => {
            match text.strip_prefix("import.meta.env.") {
                Some(key) if !key.is_empty() && !key.contains('.') => {
                    IdentityExpr::EnvRef(key.to_string())
                }
                _ => IdentityExpr::Computed(text.to_string()),
            }
        }
        _ if text.starts_with("process") && !scopes.global("process", node) => {
            IdentityExpr::Computed(text.to_string())
        }
        _ => classify_identity(cx, &node, &HashMap::new()),
    }
}

/// The instance identity a client entry declares. `None` when the call
/// does not carry it (no argument, or the key is absent): the call still
/// proves the provider, just not which instance.
fn client_identity(
    cx: &FileCx,
    args: TsNode,
    spec: &IdentityArg,
    scopes: &Scopes,
    family: ObservedFamily,
) -> Option<IdentityExpr> {
    let first = {
        let mut walk = args.walk();
        args.named_children(&mut walk)
            .find(|arg| arg.kind() != "comment")
    };
    if family == ObservedFamily::HttpApi {
        // An HTTP config object names its URL by a top-level property the
        // object provably ends with (#765 review): `axios({ url })`,
        // `axios.create({ baseURL })`.
        let first = first?;
        let object = unwrap_assertions(first);
        let key = match spec {
            IdentityArg::First if object.kind() != "object" => {
                return Some(url_head(cx, first, scopes));
            }
            IdentityArg::First => "url",
            IdentityArg::Key(key) => key.as_str(),
        };
        if object.kind() != "object" {
            return None;
        }
        return match effective_property(cx, object, key)? {
            Some(value) => Some(url_head(cx, value, scopes)),
            None => Some(IdentityExpr::Computed(cx.text(&object).to_string())),
        };
    }
    let value = match spec {
        IdentityArg::First => first?,
        IdentityArg::Key(key) => pair_value_in_args(cx, &args, key)?,
    };
    Some(classify(cx, value, scopes))
}

/// The value an object literal provably ends with for `key`, looking at its
/// own top-level properties only: the last `key: v` or shorthand `key`
/// wins. `Some(None)` when a later spread, computed key or method may
/// override it (or a spread may supply it), `None` when nothing names it.
fn effective_property<'t>(
    cx: &FileCx,
    object: TsNode<'t>,
    key: &str,
) -> Option<Option<TsNode<'t>>> {
    let mut found: Option<Option<TsNode<'t>>> = None;
    let mut walk = object.walk();
    for property in object.named_children(&mut walk) {
        match property.kind() {
            "pair" => match property.child_by_field_name("key") {
                Some(name) if name.kind() == "computed_property_name" => {
                    found = Some(None);
                }
                Some(name) if property_name(cx, name).as_deref() == Some(key) => {
                    found = Some(property.child_by_field_name("value"));
                }
                _ => {}
            },
            "shorthand_property_identifier" if cx.text(&property) == key => {
                found = Some(Some(property));
            }
            "spread_element" => found = Some(None),
            "method_definition" => {
                let name = property.child_by_field_name("name");
                if name.is_none_or(|name| {
                    name.kind() == "computed_property_name"
                        || property_name(cx, name).as_deref() == Some(key)
                }) {
                    found = Some(None);
                }
            }
            _ => {}
        }
    }
    found
}

/// A property key's name: an identifier or a string literal.
fn property_name(cx: &FileCx, name: TsNode) -> Option<String> {
    match name.kind() {
        "property_identifier" => Some(cx.text(&name).to_string()),
        _ => crate::literal_string(cx, name),
    }
}

/// The URL an HTTP call's argument starts with, as far as the file proves
/// it: the known leading text of a literal, a literal `const`, a template
/// or a `+` concatenation, or a leading env reference. Text cut short by an
/// unknown piece is kept only once its authority is closed
/// (`https://api.example.com/` + id); a head that leaves the host open
/// (`https://` + tenant) is computed, never a truncated origin.
fn url_head(cx: &FileCx, node: TsNode, scopes: &Scopes) -> IdentityExpr {
    match url_parts(cx, node, scopes, String::new()) {
        Head::Env(key) => IdentityExpr::EnvRef(key),
        Head::Text(text, true) => IdentityExpr::Literal(canonical_origin(&text)),
        Head::Text(text, false) => match text.split_once("://") {
            Some((_, rest)) if rest.contains(['/', '?', '#']) => {
                IdentityExpr::Literal(canonical_origin(&text))
            }
            None if !text.is_empty() => IdentityExpr::Literal(text),
            _ => IdentityExpr::Computed(cx.text(&node).to_string()),
        },
    }
}

/// A URL with its origin in canonical form, so one origin is one resource
/// (#765 review): scheme and host lowercased and the scheme's default port
/// dropped. Userinfo and the rest are kept for `recorded_literal` to cut.
fn canonical_origin(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let scheme = scheme.to_ascii_lowercase();
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let (userinfo, host) = match authority.rsplit_once('@') {
        Some((userinfo, host)) => (Some(userinfo), host),
        None => (None, authority),
    };
    let mut host = host.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => Some(":80"),
        "https" => Some(":443"),
        _ => None,
    };
    if let Some(port) = default
        && host.ends_with(port)
    {
        host.truncate(host.len() - port.len());
    }
    match userinfo {
        Some(userinfo) => format!("{scheme}://{userinfo}@{host}{tail}"),
        None => format!("{scheme}://{host}{tail}"),
    }
}

/// A URL's known leading text: `Text(text, complete)`, or an env
/// reference that leads it.
enum Head {
    Text(String, bool),
    Env(String),
}

fn url_parts(cx: &FileCx, node: TsNode, scopes: &Scopes, lead: String) -> Head {
    let node = unwrap_assertions(node);
    match node.kind() {
        "parenthesized_expression" => match node.named_child(0) {
            Some(inner) => url_parts(cx, inner, scopes, lead),
            None => Head::Text(lead, false),
        },
        "template_string" => {
            let mut lead = lead;
            let mut walk = node.walk();
            for child in node.named_children(&mut walk) {
                match child.kind() {
                    "string_fragment" => lead.push_str(cx.text(&child)),
                    "template_substitution" => {
                        let Some(inner) = child.named_child(0) else {
                            return Head::Text(lead, false);
                        };
                        match url_parts(cx, inner, scopes, lead) {
                            Head::Text(text, true) => lead = text,
                            head => return head,
                        }
                    }
                    _ => {}
                }
            }
            Head::Text(lead, true)
        }
        "binary_expression"
            if node
                .child_by_field_name("operator")
                .is_some_and(|op| cx.text(&op) == "+") =>
        {
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                return Head::Text(lead, false);
            };
            match url_parts(cx, left, scopes, lead) {
                Head::Text(text, true) => url_parts(cx, right, scopes, text),
                head => head,
            }
        }
        _ => match classify(cx, node, scopes) {
            IdentityExpr::Literal(value) => Head::Text(lead + &value, true),
            IdentityExpr::EnvRef(key) if lead.is_empty() => Head::Env(key),
            _ => Head::Text(lead, false),
        },
    }
}

/// Whether a literal URL is absolute `http(s)`, so it names an origin.
fn absolute_http(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.starts_with("http://") || value.starts_with("https://")
}

/// `(module, export)` a callee names through the binding visible at it:
/// `X` → its binding; `ns.Y` → `Y` of the module `ns` is bound to.
/// Calls that can write any property of their first argument.
const REFLECTIVE_WRITES: &[&str] = &[
    "Object.assign",
    "Object.defineProperty",
    "Object.defineProperties",
    "Object.setPrototypeOf",
    "Reflect.set",
    "Reflect.defineProperty",
    "Reflect.deleteProperty",
    "Reflect.setPrototypeOf",
];

/// The object a write reaches through casts, `!` and parentheses:
/// `(A as any)`, `A!` and `((A))` all write `A` (#758 review).
fn write_object(node: TsNode) -> TsNode {
    let mut current = node;
    while matches!(
        current.kind(),
        "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
            | "parenthesized_expression"
    ) {
        let Some(inner) = current.named_child(0) else {
            return current;
        };
        current = inner;
    }
    current
}

/// Record every `X.p` or `X[k]` a write target reaches, destructuring
/// patterns included. Deeper members (`X.config.region`) write a property
/// of something else and are not recorded.
fn member_targets(cx: &FileCx, target: TsNode, out: &mut HashMap<String, HashSet<Option<String>>>) {
    let target = unwrap_assertions(target);
    match target.kind() {
        "member_expression" | "subscript_expression" => {
            let Some(object) = target
                .child_by_field_name("object")
                .map(write_object)
                .filter(|object| object.kind() == "identifier")
            else {
                return;
            };
            let key = if target.kind() == "member_expression" {
                target
                    .child_by_field_name("property")
                    .map(|p| cx.text(&p).to_string())
            } else {
                target
                    .child_by_field_name("index")
                    .and_then(|index| crate::literal_string(cx, index))
            };
            out.entry(cx.text(&object).to_string())
                .or_default()
                .insert(key);
        }
        _ => {
            let mut walk = target.walk();
            for child in target.named_children(&mut walk) {
                member_targets(cx, child, out);
            }
        }
    }
}

fn callee_export(cx: &FileCx, callee: TsNode, scopes: &Scopes) -> Option<(String, String)> {
    match callee.kind() {
        "identifier" => match scopes.resolve(cx.text(&callee), callee)? {
            Declared::Module(module, Binding::Default) => Some((module, "default".into())),
            Declared::Module(module, Binding::Named(export)) => Some((module, export)),
            _ => None,
        },
        "member_expression" => {
            let object = callee.child_by_field_name("object")?;
            let property = callee.child_by_field_name("property")?;
            if object.kind() != "identifier"
                || scopes.member_written(cx.text(&object), cx.text(&property))
            {
                return None;
            }
            match scopes.resolve(cx.text(&object), object)? {
                Declared::Module(module, Binding::Default | Binding::Namespace) => {
                    Some((module, cx.text(&property).to_string()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The browser global a member expression's object reaches, if any:
/// `indexedDB` or `window.indexedDB`, each name the platform global at the
/// site (#758 review: neither the storage name nor the receiver bound).
fn storage_global(
    cx: &FileCx,
    object: TsNode,
    scopes: &Scopes,
) -> Option<&'static (&'static str, Option<&'static str>, &'static str)> {
    let object = unwrap_assertions(object);
    let name = match object.kind() {
        "identifier" => cx.text(&object),
        "member_expression" => {
            let receiver = object.child_by_field_name("object")?;
            let property = object.child_by_field_name("property")?;
            if receiver.kind() != "identifier"
                || !GLOBAL_OBJECTS.contains(&cx.text(&receiver))
                || !scopes.global(cx.text(&receiver), receiver)
            {
                return None;
            }
            cx.text(&property)
        }
        _ => return None,
    };
    let entry = STORAGE_GLOBALS
        .iter()
        .find(|(global, _, _)| *global == name)?;
    // A bare name must itself be the global; a qualified one is the
    // receiver's property whatever the file binds.
    (object.kind() != "identifier" || scopes.global(name, object)).then_some(entry)
}

/// Whether a callee is the platform `fetch`: the bare global, or a
/// global receiver's `fetch`, neither bound nor written by the file.
fn global_fetch(cx: &FileCx, callee: TsNode, scopes: &Scopes) -> bool {
    match callee.kind() {
        // Bare `fetch` and `window.fetch` are one global property: a write
        // through any alias voids both spellings (#765 review).
        "identifier" => {
            cx.text(&callee) == "fetch"
                && scopes.global("fetch", callee)
                && !fetch_rewritten(scopes)
        }
        "member_expression" => {
            let (Some(receiver), Some(property)) = (
                callee.child_by_field_name("object"),
                callee.child_by_field_name("property"),
            ) else {
                return false;
            };
            receiver.kind() == "identifier"
                && GLOBAL_OBJECTS.contains(&cx.text(&receiver))
                && scopes.global(cx.text(&receiver), receiver)
                && cx.text(&property) == "fetch"
                && !fetch_rewritten(scopes)
        }
        _ => false,
    }
}

/// Whether the file writes `fetch` through any global object.
fn fetch_rewritten(scopes: &Scopes) -> bool {
    GLOBAL_OBJECTS
        .iter()
        .any(|global| scopes.member_written(global, "fetch"))
}

fn extract_file(
    source: &[u8],
    path: &str,
    id: &SourceId,
    registries: &Registries,
    sites: &mut Vec<Site>,
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
        registries,
    };
    let scopes = collect_scopes(&cx, root);
    let site = |node: TsNode| {
        (
            enclosing_symbol(&cx, node),
            node.start_byte() as u64,
            node.end_byte() as u64,
        )
    };

    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        let children: Vec<_> = node.named_children(&mut walk).collect();
        stack.extend(children.into_iter().rev());
        match node.kind() {
            "new_expression" | "call_expression" => {
                let (callee, form) = if node.kind() == "new_expression" {
                    (node.child_by_field_name("constructor"), ClientForm::New)
                } else {
                    (node.child_by_field_name("function"), ClientForm::Call)
                };
                let Some(callee) = callee.map(unwrap_assertions) else {
                    continue;
                };
                let args = node.child_by_field_name("arguments");
                if let Some((module, export)) = callee_export(&cx, callee, &scopes)
                    && let Some(client) = registries.clients.iter().find(|c: &&ObservedClient| {
                        c.ecosystem == Ecosystem::Npm
                            && c.select.is_none()
                            && c.module == module
                            && c.export == export
                            && c.form == form
                    })
                {
                    let identity = match (&client.identity, args) {
                        (Some(spec), Some(args)) => {
                            client_identity(&cx, args, spec, &scopes, client.family)
                        }
                        _ => None,
                    };
                    let (symbol, byte_start, byte_end) = site(node);
                    sites.push(Site {
                        family: client.family,
                        provider: client.provider.clone(),
                        module: Some(client.module.clone()),
                        identity,
                        registry: Some(client.registry.clone()),
                        symbol,
                        path: path.to_string(),
                        byte_start,
                        byte_end,
                    });
                    continue;
                }
                // The global `fetch(url)` or `window.fetch(url)`.
                if form == ClientForm::Call
                    && global_fetch(&cx, callee, &scopes)
                    && let Some(args) = args
                {
                    let identity = client_identity(
                        &cx,
                        args,
                        &IdentityArg::First,
                        &scopes,
                        ObservedFamily::HttpApi,
                    );
                    let (symbol, byte_start, byte_end) = site(node);
                    sites.push(Site {
                        family: ObservedFamily::HttpApi,
                        provider: "fetch".into(),
                        module: None,
                        identity,
                        registry: None,
                        symbol,
                        path: path.to_string(),
                        byte_start,
                        byte_end,
                    });
                    continue;
                }
                // `indexedDB.open(name)`, `caches.open(name)`.
                if form == ClientForm::Call
                    && callee.kind() == "member_expression"
                    && let (Some(object), Some(property)) = (
                        callee.child_by_field_name("object"),
                        callee.child_by_field_name("property"),
                    )
                    && let Some((_, Some(method), provider)) = storage_global(&cx, object, &scopes)
                    && cx.text(&property) == *method
                {
                    let identity = args.and_then(|args| {
                        client_identity(
                            &cx,
                            args,
                            &IdentityArg::First,
                            &scopes,
                            ObservedFamily::BrowserStorage,
                        )
                    });
                    let (symbol, byte_start, byte_end) = site(node);
                    sites.push(Site {
                        family: ObservedFamily::BrowserStorage,
                        provider: provider.to_string(),
                        module: None,
                        identity,
                        registry: None,
                        symbol,
                        path: path.to_string(),
                        byte_start,
                        byte_end,
                    });
                }
            }
            // `localStorage.getItem(…)`, `window.sessionStorage.x`: the
            // global itself is the instance. Only the access whose object
            // is the global matches, so a chain counts once.
            "member_expression" => {
                let Some(object) = node.child_by_field_name("object") else {
                    continue;
                };
                if let Some((global, None, provider)) = storage_global(&cx, object, &scopes) {
                    let (symbol, byte_start, byte_end) = site(node);
                    sites.push(Site {
                        family: ObservedFamily::BrowserStorage,
                        provider: provider.to_string(),
                        module: None,
                        identity: Some(IdentityExpr::Literal(global.to_string())),
                        registry: None,
                        symbol,
                        path: path.to_string(),
                        byte_start,
                        byte_end,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A computed identity's source text as a Gap may keep it: URL userinfo
/// removed, recognized secrets redacted, at most [`MAX_IDENTITY`] bytes.
fn withheld_raw(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| matches!(c, '/' | '?' | '#' | '"' | '\'' | '`') || c.is_whitespace())
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        out.push_str(authority.rsplit('@').next().unwrap_or(authority));
        rest = &tail[end..];
    }
    out.push_str(rest);
    let (mut out, _) = core_redact::redact_text(&out);
    if out.len() > MAX_IDENTITY {
        let mut cut = MAX_IDENTITY;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

/// A literal identity as recorded: a URL keeps its scheme and authority
/// without credentials; anything else is kept up to [`MAX_IDENTITY`].
/// `None` for a value that cannot be recorded safely (empty, too long,
/// token-shaped or a recognized secret): the Gap then withholds it whole.
fn recorded_literal(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if !core_redact::recognized_secrets(value).is_empty() {
        return None;
    }
    let recorded = match value.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
            let host = authority.rsplit('@').next().unwrap_or(authority);
            if host.is_empty() {
                return None;
            }
            format!("{scheme}://{host}")
        }
        None if core_redact::is_token_shaped(value) => return None,
        None => value.to_string(),
    };
    (recorded.len() <= MAX_IDENTITY).then_some(recorded)
}

/// Observed infrastructure for the whole tree.
pub fn extract_dir(
    root: &Path,
    id: &SourceId,
    registries: &Registries,
) -> Result<Extraction, ExtractError> {
    let mut files = Vec::new();
    crate::collect_ts_files(root, &mut files)?;
    files.sort(); // deterministic order (US-0014)
    let mut sites = Vec::new();
    for rel in &files {
        let source = std::fs::read(root.join(rel))?;
        extract_file(&source, rel, id, registries, &mut sites)?;
    }
    Ok(assemble(id, &sites))
}

/// Observed infrastructure from a retained capture: exactly the bytes the
/// captured TS pass parsed, never a reread of the live checkout, so one
/// recovery cannot mix snapshots (#758 review).
pub fn extract_captured(
    capture: &source_capture::Capture,
    id: &SourceId,
    registries: &Registries,
) -> Result<Extraction, crate::captured::CapturedError> {
    let mut files: Vec<&str> = capture
        .manifest()
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .filter(|path| crate::is_ts_source(path))
        .collect();
    files.sort(); // deterministic order (US-0014)
    let mut sites = Vec::new();
    for rel in files {
        extract_file(capture.file(rel)?.bytes(), rel, id, registries, &mut sites)?;
    }
    Ok(assemble(id, &sites))
}

/// Resources, `USES` edges and Gaps for the collected sites.
fn assemble(id: &SourceId, sites: &[Site]) -> Extraction {
    let mut out = Extraction::default();
    let mut resources: BTreeMap<String, Node> = BTreeMap::new();
    for site in sites {
        let http = site.family == ObservedFamily::HttpApi;
        // An HTTP call names an origin only through an absolute URL. One
        // with no URL, or a relative one, is the caller's own origin or a
        // base set elsewhere; a computed `fetch` is already the client-call
        // pass's Gap.
        if http
            && !match &site.identity {
                Some(IdentityExpr::Literal(value)) => absolute_http(value),
                Some(IdentityExpr::EnvRef(_)) => true,
                Some(IdentityExpr::Computed(_)) => site.provider != "fetch",
                None => false,
            }
        {
            continue;
        }
        // Every HTTP client of one origin uses the same resource.
        let provider = if http {
            "http-api"
        } else {
            site.provider.as_str()
        };
        let span = (site.byte_start, site.byte_end);
        let src = site
            .symbol
            .clone()
            .unwrap_or_else(|| file_id(id.repo, &site.path));
        let mut edge_props = serde_json::Map::new();
        if let Some(registry) = &site.registry {
            edge_props.insert("registry".into(), serde_json::json!(registry.name));
        }
        if http {
            edge_props.insert("client".into(), serde_json::json!(site.provider));
        }
        // Instance identity: `None` = the provider alone, `Err` = a Gap
        // (reason, raw text a Gap may keep). A literal that cannot be
        // recorded is withheld whole, never kept as raw (#758 review).
        let identity = match &site.identity {
            None => Ok(None),
            Some(IdentityExpr::Literal(value)) => match recorded_literal(value) {
                Some(value) => Ok(Some((
                    value.clone(),
                    serde_json::json!({ "identity": value }),
                ))),
                None => Err((
                    format!("{} identity withheld: literal is not recordable", provider),
                    None,
                )),
            },
            Some(IdentityExpr::EnvRef(key)) => Ok(Some((
                format!("env:{key}"),
                serde_json::json!({ "env_key": key }),
            ))),
            Some(IdentityExpr::Computed(raw)) => Err((
                format!("runtime-computed {} identity", provider),
                Some(withheld_raw(raw)),
            )),
        };
        match identity {
            Ok(instance) => {
                let logical_id = match &instance {
                    Some((name, _)) => format!("observed:{}:{name}", provider),
                    None => format!("observed:{}", provider),
                };
                let resource_id = format!("res:{}@{logical_id}", id.repo);
                resources.entry(resource_id.clone()).or_insert_with(|| {
                    let mut props = serde_json::json!({
                        "type": provider,
                        "provider": provider,
                        "kind": site.family.as_str(),
                        "observed": true,
                        "logical_id": logical_id,
                        "prov": prov_value(
                            id, &site.path, span, ConfidenceTier::Confirmed,
                            &format!("Resource {resource_id}"),
                        ),
                    });
                    if let Some(module) = site.module.as_ref().filter(|_| !http) {
                        props["module"] = serde_json::json!(module);
                    }
                    if let Some((_, extra)) = &instance
                        && let (Some(props), Some(extra)) =
                            (props.as_object_mut(), extra.as_object())
                    {
                        props.extend(extra.clone());
                    }
                    Node {
                        id: resource_id.clone(),
                        label: "Resource".into(),
                        props,
                    }
                });
                edge_props.insert(
                    "prov".into(),
                    prov_value(
                        id,
                        &site.path,
                        span,
                        ConfidenceTier::Confirmed,
                        &format!("USES {src} -> {resource_id}"),
                    ),
                );
                out.edges.push(Edge {
                    src,
                    dst: resource_id,
                    label: "USES".into(),
                    props: serde_json::Value::Object(edge_props),
                });
            }
            Err((reason, raw)) => {
                let gap_id = format!("gap:observed:{}@{}@{}", id.repo, site.path, site.byte_start);
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "reason": reason,
                        "raw": raw,
                        "kind": site.family.as_str(),
                        "provider": provider,
                        "attempted_tiers": ["T0"],
                        "prov": prov_value(
                            id, &site.path, span, ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    }),
                });
                edge_props.insert("reason".into(), serde_json::json!(reason));
                edge_props.insert(
                    "prov".into(),
                    prov_value(
                        id,
                        &site.path,
                        span,
                        ConfidenceTier::Gap,
                        &format!("USES {src} -> {gap_id}"),
                    ),
                );
                out.edges.push(Edge {
                    src,
                    dst: gap_id,
                    label: "USES".into(),
                    props: serde_json::Value::Object(edge_props),
                });
            }
        }
    }
    out.nodes.extend(resources.into_values());
    out
}

#[cfg(test)]
#[path = "observed_infra/tests.rs"]
mod tests;
