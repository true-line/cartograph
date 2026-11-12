//! reqwest HTTP client calls (#999).
//!
//! A call is a client call only when it provably reaches `reqwest`:
//! `reqwest::get(url)` / `reqwest::blocking::get(url)` through `use` or
//! qualified paths, or a verb method (`get`, `post`, `put`, `delete`,
//! `patch`, `head`, `request(Method::X, url)`) on a receiver proven to be a
//! reqwest `Client`. A receiver is proven when it is a `Client::new()` /
//! `Client::builder()…build()` expression, a name the function binds exactly
//! once to such an expression, a parameter typed `Client` (behind `&` or
//! the standard `Arc` / `Rc`; a generic parameter named `Client` shadows the
//! import), or `self.<field>` inside an `impl` of a struct this module
//! declares with that field typed `Client`. A literal URL is reported as written; anything
//! else is a computed URL the stitcher keeps as a Gap. Lookalikes yield
//! nothing.

use super::*;
use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;
use http::{call_arguments, resolve_external_paths, unquote_string_literal};

const VERBS: [(&str, &str); 6] = [
    ("get", "GET"),
    ("post", "POST"),
    ("put", "PUT"),
    ("delete", "DELETE"),
    ("patch", "PATCH"),
    ("head", "HEAD"),
];

/// Per-function proof context. It proves a receiver of the type
/// `is_type` names; rdkafka reuses it for consumers (#1371).
pub(super) struct Proof<'a, 't> {
    cx: &'a FileCx<'a>,
    bindings: &'a Bindings,
    function: TsNode<'t>,
    shadows: BTreeSet<String>,
    /// Fields of the proven type, per (module scope, struct name).
    client_fields: &'a ClientFields,
    is_type: fn(&[String]) -> bool,
    /// Whether `T::new()` / `T::default()` and reqwest builders prove it.
    constructible: bool,
}

impl<'a, 't> Proof<'a, 't> {
    pub(super) fn new(
        cx: &'a FileCx<'a>,
        bindings: &'a Bindings,
        function: TsNode<'t>,
        client_fields: &'a ClientFields,
        is_type: fn(&[String]) -> bool,
        constructible: bool,
    ) -> Self {
        Proof {
            cx,
            bindings,
            function,
            shadows: value_bindings(cx, function),
            client_fields,
            is_type,
            constructible,
        }
    }

    /// The single external path `text` resolves to at `node`, if exactly one.
    /// A generic parameter in scope named like its first segment shadows
    /// any import, so such a path resolves to nothing.
    pub(super) fn path(&self, node: TsNode<'_>, text: &str) -> Option<Vec<String>> {
        let head = text.split('<').next().unwrap_or(text);
        let first = head.split("::").next().unwrap_or(head).trim();
        if generic_names(self.cx, node).contains(first) {
            return None;
        }
        let scopes = visible_scopes(self.cx, node);
        let mut paths =
            resolve_external_paths(self.cx, self.bindings, &scopes, text, Some(&self.shadows));
        (paths.len() == 1).then(|| paths.remove(0))
    }

    fn is_client_type(&self, node: TsNode<'_>) -> bool {
        self.is_client_type_text(node, self.cx.text(&node))
    }

    /// A type spelling written at `node` that names the proven type
    /// (generic arguments aside), behind `&`, `&mut` and the standard
    /// `Arc` / `Rc` only.
    fn is_client_type_text(&self, node: TsNode<'_>, text: &str) -> bool {
        let mut text = text.trim();
        while let Some(rest) = text.strip_prefix('&') {
            let mut rest = rest.trim_start();
            // A reference may name its lifetime: `&'a T`, `&'_ mut T`.
            if let Some(lifetime) = rest.strip_prefix('\'') {
                rest = lifetime
                    .trim_start_matches(|c: char| c == '_' || c.is_alphanumeric())
                    .trim_start();
            }
            text = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
        }
        let head = text.split('<').next().unwrap_or(text).trim();
        if self
            .path(node, head)
            .is_some_and(|path| (self.is_type)(&path))
        {
            return true;
        }
        if let Some(inner) = text
            .strip_suffix('>')
            .and_then(|t| t.split_once('<'))
            .map(|(_, inner)| inner)
        {
            return self
                .path(node, head)
                .is_some_and(|path| is_shared_pointer(&path))
                && self.is_client_type_text(node, inner);
        }
        false
    }

    /// Whether `expr` evaluates to the proven type.
    pub(super) fn is_client_expr(&self, expr: TsNode<'_>) -> bool {
        match expr.kind() {
            "parenthesized_expression" | "try_expression" => expr
                .named_child(0)
                .is_some_and(|inner| self.is_client_expr(inner)),
            "call_expression" => {
                let Some(callee) = expr.child_by_field_name("function") else {
                    return false;
                };
                match callee.kind() {
                    "field_expression" => {
                        let (Some(receiver), Some(method)) = (
                            callee.child_by_field_name("value"),
                            callee.child_by_field_name("field"),
                        ) else {
                            return false;
                        };
                        match self.cx.text(&method) {
                            // Only reqwest's own `Result<Client>` is forwarded: a
                            // proven type may carry a local trait's `unwrap`.
                            "unwrap" | "expect" => {
                                self.constructible && self.is_client_expr(receiver)
                            }
                            "build" => self.constructible && self.is_builder_expr(receiver),
                            _ => false,
                        }
                    }
                    "scoped_identifier" | "identifier" => {
                        self.constructible
                            && self
                                .path(callee, self.cx.text(&callee))
                                .is_some_and(|path| {
                                    path.split_last().is_some_and(|(last, ty)| {
                                        matches!(last.as_str(), "new" | "default")
                                            && (self.is_type)(ty)
                                    })
                                })
                    }
                    _ => false,
                }
            }
            "identifier" => self.is_client_binding(expr),
            "field_expression" => self.is_client_field(expr),
            _ => false,
        }
    }

    /// `Client::builder()` / `ClientBuilder::new()` followed by any methods.
    fn is_builder_expr(&self, expr: TsNode<'_>) -> bool {
        if expr.kind() != "call_expression" {
            return false;
        }
        let Some(callee) = expr.child_by_field_name("function") else {
            return false;
        };
        match callee.kind() {
            "field_expression" => callee
                .child_by_field_name("value")
                .is_some_and(|receiver| self.is_builder_expr(receiver)),
            "scoped_identifier" => self
                .path(callee, self.cx.text(&callee))
                .is_some_and(|path| {
                    path.split_last().is_some_and(|(last, ty)| {
                        (last == "builder" && is_client_path(ty))
                            || (last == "new" && is_builder_path(ty))
                    })
                }),
            _ => false,
        }
    }

    /// A name the function binds exactly once, to a Client.
    fn is_client_binding(&self, name: TsNode<'_>) -> bool {
        let wanted = self.cx.text(&name);
        let mut sites = Vec::new();
        let mut stack = vec![self.function];
        while let Some(node) = stack.pop() {
            let binds =
                |pattern: TsNode<'_>| pattern_names(self.cx, pattern).iter().any(|n| n == wanted);
            match node.kind() {
                "let_declaration" | "parameter"
                    if node.child_by_field_name("pattern").is_some_and(binds) =>
                {
                    sites.push(node);
                }
                // Any other binding of the name fails closed.
                "for_expression" | "let_condition" | "match_pattern" | "closure_parameters"
                    if binds(node) =>
                {
                    return false;
                }
                _ => {}
            }
            let mut walk = node.walk();
            for child in node.named_children(&mut walk) {
                if child.kind() != "function_item" {
                    stack.push(child);
                }
            }
        }
        let [site] = sites.as_slice() else {
            return false;
        };
        let simple = site
            .child_by_field_name("pattern")
            .is_some_and(|pattern| pattern.kind() == "identifier");
        if !simple {
            return false;
        }
        match site.kind() {
            "parameter" => site
                .child_by_field_name("type")
                .is_some_and(|ty| self.is_client_type(ty)),
            _ => {
                site.child_by_field_name("type")
                    .is_some_and(|ty| self.is_client_type(ty))
                    || site
                        .child_by_field_name("value")
                        .is_some_and(|value| self.is_client_expr(value))
            }
        }
    }

    /// `self.<field>` inside a module-level `impl` of a module-level struct
    /// declared in the same module whose field of that name is typed
    /// the proven type. Items declared inside a block fail closed: a
    /// module scope cannot tell same-named block-local structs apart.
    fn is_client_field(&self, expr: TsNode<'_>) -> bool {
        let (Some(value), Some(field)) = (
            expr.child_by_field_name("value"),
            expr.child_by_field_name("field"),
        ) else {
            return false;
        };
        if value.kind() != "self" {
            return false;
        }
        let mut node = expr;
        let imp = loop {
            match node.parent() {
                Some(parent) if parent.kind() == "impl_item" => break parent,
                Some(parent) => node = parent,
                None => return false,
            }
        };
        if in_block(imp) {
            return false;
        }
        let Some(ty) = imp.child_by_field_name("type") else {
            return false;
        };
        let ty = match ty.kind() {
            "generic_type" => ty.child_by_field_name("type").unwrap_or(ty),
            _ => ty,
        };
        // A path (`other::Api`) names a struct this file may not declare.
        if ty.kind() != "type_identifier" {
            return false;
        }
        let key = (
            scope_of(self.cx, imp).join("::"),
            self.cx.text(&ty).to_string(),
        );
        self.client_fields
            .get(&key)
            .and_then(|fields| fields.get(self.cx.text(&field)))
            .copied()
            .unwrap_or(false)
    }

    fn url(&self, node: TsNode<'_>) -> IdentityExpr {
        let node = if node.kind() == "reference_expression" {
            node.child_by_field_name("value").unwrap_or(node)
        } else {
            node
        };
        match unquote_string_literal(self.cx, node) {
            Some(literal) => IdentityExpr::Literal(literal),
            None => IdentityExpr::Computed(self.cx.text(&node).trim().to_string()),
        }
    }

    /// The method and URL argument of a proven client call.
    fn site(&self, call: TsNode<'t>) -> Option<(String, TsNode<'t>)> {
        let callee = call.child_by_field_name("function")?;
        let args = call_arguments(call);
        match callee.kind() {
            "scoped_identifier" | "identifier" => {
                let path = self.path(callee, self.cx.text(&callee))?;
                let free = matches!(
                    path.iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .as_slice(),
                    ["reqwest", "get"] | ["reqwest", "blocking", "get"]
                );
                let url = *args.first()?;
                free.then(|| ("GET".to_string(), url))
            }
            "field_expression" => {
                let receiver = callee.child_by_field_name("value")?;
                let method = self.cx.text(&callee.child_by_field_name("field")?);
                let (verb, url) = if method == "request" {
                    let verb = args.first()?;
                    let path = self.path(*verb, self.cx.text(verb))?;
                    let [krate, ty, name] = path.as_slice() else {
                        return None;
                    };
                    let known = matches!(krate.as_str(), "reqwest" | "http")
                        && ty == "Method"
                        && VERBS.iter().any(|(_, upper)| upper == name);
                    (known.then(|| name.clone())?, *args.get(1)?)
                } else {
                    let (_, upper) = VERBS.iter().find(|(lower, _)| *lower == method)?;
                    (upper.to_string(), *args.first()?)
                };
                self.is_client_expr(receiver).then_some((verb, url))
            }
            _ => None,
        }
    }
}

fn is_client_path(path: &[String]) -> bool {
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["reqwest", "Client"] | ["reqwest", "blocking", "Client"]
    )
}

/// `std::sync::Arc` / `std::rc::Rc` (or their `alloc` paths).
fn is_shared_pointer(path: &[String]) -> bool {
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["std" | "alloc", "sync", "Arc"] | ["std" | "alloc", "rc", "Rc"]
    )
}

/// Generic type parameters in scope at `node`: those of every enclosing
/// function, impl, trait and struct.
pub(super) fn generic_names(cx: &FileCx<'_>, node: TsNode<'_>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut current = Some(node);
    while let Some(node) = current {
        if matches!(
            node.kind(),
            "function_item"
                | "function_signature_item"
                | "impl_item"
                | "trait_item"
                | "struct_item"
        ) && let Some(params) = node.child_by_field_name("type_parameters")
        {
            let mut walk = params.walk();
            for param in params.named_children(&mut walk) {
                let name = if param.kind() == "type_identifier" {
                    Some(param)
                } else {
                    param
                        .child_by_field_name("name")
                        .or_else(|| param.child_by_field_name("left"))
                };
                if let Some(name) = name {
                    names.insert(cx.text(&name).to_string());
                }
            }
        }
        current = node.parent();
    }
    names
}

/// Whether `node` sits inside a block (a function body or a block
/// expression) rather than directly in a module.
fn in_block(node: TsNode<'_>) -> bool {
    let mut current = node.parent();
    while let Some(node) = current {
        if node.kind() == "block" {
            return true;
        }
        current = node.parent();
    }
    false
}

fn is_builder_path(path: &[String]) -> bool {
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["reqwest", "ClientBuilder"] | ["reqwest", "blocking", "ClientBuilder"]
    )
}

/// Identifiers a pattern binds.
fn pattern_names(cx: &FileCx<'_>, node: TsNode<'_>) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            out.push(cx.text(&node).to_string());
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    out
}

/// (module scope, struct name) -> field name -> typed as the proven type.
pub(super) type ClientFields = BTreeMap<(String, String), BTreeMap<String, bool>>;

/// The named fields of every struct in the file, and whether each is typed
/// reqwest `Client`.
pub(super) fn client_fields(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &Bindings,
) -> ClientFields {
    typed_fields(cx, root, bindings, is_client_path)
}

/// The named fields of every struct in the file, and whether each is typed
/// as the type `is_type` names.
pub(super) fn typed_fields(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &Bindings,
    is_type: fn(&[String]) -> bool,
) -> ClientFields {
    let empty = ClientFields::new();
    let mut out = ClientFields::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "struct_item"
            && !in_block(node)
            && let (Some(name), Some(body)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("body"),
            )
        {
            let key = (scope_of(cx, node).join("::"), cx.text(&name).to_string());
            let fields = out.entry(key).or_default();
            let mut walk = body.walk();
            for field in body.named_children(&mut walk) {
                if field.kind() != "field_declaration" {
                    continue;
                }
                let (Some(field_name), Some(ty)) = (
                    field.child_by_field_name("name"),
                    field.child_by_field_name("type"),
                ) else {
                    continue;
                };
                let proof = Proof {
                    cx,
                    bindings,
                    function: field,
                    shadows: BTreeSet::new(),
                    client_fields: &empty,
                    is_type,
                    constructible: false,
                };
                fields.insert(cx.text(&field_name).to_string(), proof.is_client_type(ty));
            }
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    out
}

/// Client calls made in `function` (nested functions excluded).
pub(super) fn extract_function_calls(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    symbol: &str,
    bindings: &Bindings,
    client_fields: &ClientFields,
    out: &mut Extraction,
) {
    let proof = Proof::new(cx, bindings, function, client_fields, is_client_path, true);
    let mut calls = Vec::new();
    let mut stack = vec![function];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            calls.push(node);
        }
        let mut walk = node.walk();
        for child in node.named_children(&mut walk) {
            if child.kind() != "function_item" {
                stack.push(child);
            }
        }
    }
    calls.sort_by_key(|call| call.start_byte());
    for call in calls {
        let Some((method, url)) = proof.site(call) else {
            continue;
        };
        out.fetch_sites.push(FetchSite {
            method,
            url: proof.url(url),
            symbol: Some(symbol.to_string()),
            path: cx.path.to_string(),
            byte_start: call.start_byte() as u64,
            byte_end: call.end_byte() as u64,
            base: None,
            entry: None,
        });
    }
}
