//! Spring `RestTemplate` client calls (#1340), as Java reads them (AC-0435).
//!
//! A call is a client call only when its receiver is proven to be
//! `org.springframework.web.client.RestTemplate`: a `RestTemplate(…)`
//! constructor call, or a name whose visible declaration (a local,
//! parameter, constructor or class property, or `this.x`, as
//! [`receivers`] reads it) states that type or is initialized by that
//! constructor. The spelling is the fully qualified name, or a name its
//! named import binds to that type in a file that declares nothing of that
//! name. Star imports prove nothing, since a same-package type would win. An
//! enclosing type parameter named like a type spelling's first segment, or a
//! visible binding named like an expression spelling's (`org` before
//! `org.springframework…RestTemplate()`), obscures it.
//!
//! Verb methods give their method, including Spring's reified Kotlin
//! extensions of the same names (`getForObject<T>(url, v)`; the grammar
//! reads a one-argument `getForObject<T>(url)` as comparisons, so it is not
//! read). A call with type arguments is that extension only when the file
//! imports it from `org.springframework.web.client`: by name, or by
//! wildcard when no named import binds that name.
//! A file that declares a function of a verb's name reads no call of that
//! name, since an extension it declares wins whenever the member does not
//! apply; a same-package extension in another file is not seen. `exchange`/
//! `execute(url, method, …)` read an import-proven `HttpMethod.X` second
//! argument. With four or more arguments an unproven method is `?`. With two
//! or three, the shape `exchange(RequestEntity, …)` overloads share, an
//! unproven method yields no site. The URL is the first argument, a literal
//! only as a pure string literal (URI templates included, `$` templates
//! not); anything else is a computed URL the stitcher keeps as a Gap. A call
//! with named arguments is not read.

use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;
use std::collections::HashSet;
use tree_sitter::Node as TsNode;

use super::{FileCx, Imports, pure_string_literal, receivers};

const REST_TEMPLATE: &str = "org.springframework.web.client.RestTemplate";
const HTTP_METHOD: &str = "org.springframework.http.HttpMethod";
/// The package of `RestTemplate` and its Kotlin extensions.
const CLIENT_PACKAGE: &str = "org.springframework.web.client";

/// The names of every function this file declares, at any depth.
pub(crate) fn declared_functions(cx: &FileCx<'_>, root: TsNode<'_>) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if node.kind() == "function_declaration"
            && let Some(name) = node.child_by_field_name("name")
        {
            names.insert(cx.text(&name).to_string());
        }
    }
    names
}

/// The method a verb method sends, or `None` for `exchange`/`execute`.
fn verb(name: &str) -> Option<Option<&'static str>> {
    Some(Some(match name {
        "getForObject" | "getForEntity" => "GET",
        "headForHeaders" => "HEAD",
        "postForObject" | "postForEntity" | "postForLocation" => "POST",
        "put" => "PUT",
        "patchForObject" => "PATCH",
        "delete" => "DELETE",
        "optionsForAllow" => "OPTIONS",
        "exchange" | "execute" => return Some(None),
        _ => return None,
    }))
}

pub(crate) fn named_children<'t>(node: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut walk = node.walk();
    node.named_children(&mut walk).collect()
}

/// Receiver and type-spelling proof for a Spring client type in one file,
/// shared with [`super::webclient`].
pub(crate) struct Proof<'a> {
    pub(crate) cx: &'a FileCx<'a>,
    root: TsNode<'a>,
    imports: &'a Imports,
    hilt: HashSet<String>,
}

impl<'a> Proof<'a> {
    pub(crate) fn new(cx: &'a FileCx<'a>, root: TsNode<'a>, imports: &'a Imports) -> Self {
        Self {
            cx,
            root,
            imports,
            hilt: HashSet::new(),
        }
    }

    /// Whether the file declares or imports a function `name`, which a call
    /// of that name may resolve to instead of a member of another signature.
    pub(crate) fn shadows(&self, name: &str) -> bool {
        self.imports.bindings.contains_key(name)
            || declared_functions(self.cx, self.root).contains(name)
    }

    /// Whether `name` is lexically bound (a local, parameter or property) at
    /// `site`, which obscures a type of that name.
    pub(crate) fn binds(&self, site: TsNode<'_>, name: &str) -> bool {
        receivers::lexical_binding(self.cx, site, name, &self.hilt).is_some()
    }

    /// Whether `name` is bound at `site` with a stated built-in value type
    /// (`String`, `Int`, …) that no import, declaration or type parameter
    /// visible where the binding is declared rebinds: a value that is never
    /// a function.
    pub(crate) fn binds_value(&self, site: TsNode<'_>, name: &str) -> bool {
        const VALUE_TYPES: &[&str] = &[
            "Boolean", "Byte", "Char", "Double", "Float", "Int", "Long", "Short", "String",
        ];
        matches!(
            receivers::lexical_binding_scope(self.cx, site, name, &self.hilt),
            Some((receivers::Binding::Typed(ty), scope))
                if VALUE_TYPES.contains(&ty.as_str())
                    && !self.imports.bindings.contains_key(&ty)
                    && !receivers::declares_name(self.cx, self.root, &ty)
                    && !receivers::declares_type_parameter(self.cx, scope, &ty)
                    && !receivers::is_type_parameter(self.cx, scope, &ty)
        )
    }

    /// Whether a type spelling names `fqn` in this file.
    pub(crate) fn names(&self, spelling: &str, fqn: &str) -> bool {
        let spelling: String = spelling.chars().filter(|c| !c.is_whitespace()).collect();
        spelling == fqn
            || (!spelling.contains('.')
                && self.imports.bindings.get(&spelling).map(String::as_str) == Some(fqn)
                && !receivers::declares_name(self.cx, self.root, &spelling))
    }

    /// Whether a type spelling declared in `scope` names `fqn`, where a type
    /// parameter of its first segment's name, declared by `scope` or a
    /// function or class enclosing it, would win.
    fn names_type_in(&self, scope: TsNode<'_>, spelling: &str, fqn: &str) -> bool {
        let first = spelling.split('.').next().unwrap_or(spelling).trim();
        self.names(spelling, fqn)
            && !receivers::declares_type_parameter(self.cx, scope, first)
            && !receivers::is_type_parameter(self.cx, scope, first)
    }

    /// Whether an expression spelling names `fqn` at `site`, where a visible
    /// binding of its first segment's name would win.
    pub(crate) fn names_value_at(&self, site: TsNode<'_>, spelling: &str, fqn: &str) -> bool {
        let first = spelling.split('.').next().unwrap_or(spelling).trim();
        self.names(spelling, fqn) && !self.binds(site, first)
    }

    /// Whether a binding, with the scope that declares it, proves `fqn`; a
    /// constructor call proves it only for a `constructible` class.
    fn binding_is_client(
        &self,
        binding: Option<(receivers::Binding, TsNode<'_>)>,
        fqn: &str,
        constructible: bool,
    ) -> bool {
        match binding {
            Some((receivers::Binding::Typed(ty), scope)) => self.names_type_in(scope, &ty, fqn),
            Some((receivers::Binding::Constructed(ty), scope)) => {
                constructible && self.names_type_in(scope, &ty, fqn)
            }
            _ => false,
        }
    }

    /// Whether `receiver` at `call` is a proven `fqn`: a typed binding, or
    /// for a `constructible` class a constructed binding or inline `T()`.
    pub(crate) fn is_client(
        &self,
        call: TsNode<'_>,
        receiver: TsNode<'_>,
        fqn: &str,
        constructible: bool,
    ) -> bool {
        let cx = self.cx;
        match receiver.kind() {
            "identifier" => self.binding_is_client(
                receivers::lexical_binding_scope(cx, call, cx.text(&receiver), &self.hilt),
                fqn,
                constructible,
            ),
            "navigation_expression" => match named_children(receiver).as_slice() {
                [this, property]
                    if this.kind() == "this_expression" && property.kind() == "identifier" =>
                {
                    self.binding_is_client(
                        receivers::this_property_binding_scope(
                            cx,
                            call,
                            cx.text(property),
                            &self.hilt,
                        ),
                        fqn,
                        constructible,
                    )
                }
                _ => false,
            },
            // `RestTemplate()` with no trailing lambda, which could be an
            // anonymous override or a builder.
            "call_expression" => match named_children(receiver).as_slice() {
                [head, args] if constructible && args.kind() == "value_arguments" => {
                    matches!(head.kind(), "identifier" | "navigation_expression")
                        && self.names_value_at(call, cx.text(head), fqn)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// The method an `HttpMethod.X` argument names, if proven.
    pub(crate) fn method(&self, call: TsNode<'_>, argument: TsNode<'_>) -> Option<String> {
        let cx = self.cx;
        if argument.kind() != "navigation_expression" {
            return None;
        }
        let [owner, constant] = named_children(argument)[..] else {
            return None;
        };
        if constant.kind() != "identifier" {
            return None;
        }
        let spelling = cx.text(&owner);
        // A local or property of the first segment's name obscures the type.
        let proven = self.names_value_at(call, spelling, HTTP_METHOD);
        let verb = cx.text(&constant);
        (proven
            && matches!(
                verb,
                "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "TRACE"
            ))
        .then(|| verb.to_string())
    }
}

/// The innermost function among `functions` enclosing `node`, and its symbol.
pub(crate) fn enclosing<'f, 't>(
    functions: &'f [(TsNode<'t>, String)],
    node: TsNode<'_>,
) -> Option<&'f String> {
    functions
        .iter()
        .filter(|(function, _)| {
            function.start_byte() <= node.start_byte() && node.end_byte() <= function.end_byte()
        })
        .min_by_key(|(function, _)| function.end_byte() - function.start_byte())
        .map(|(_, symbol)| symbol)
}

/// Whether `node`, outside every function, runs as a property or top-level
/// initializer, the only context a file owns (#1426). Accessors, `init`
/// blocks, constructors, delegates and enum entries own nothing.
pub(crate) fn initializer(node: TsNode<'_>) -> bool {
    let mut property = false;
    let mut at = node.parent();
    while let Some(ancestor) = at {
        match ancestor.kind() {
            "getter"
            | "setter"
            | "anonymous_initializer"
            | "secondary_constructor"
            | "class_parameter"
            | "property_delegate"
            | "explicit_delegation"
            | "constructor_delegation_call"
            | "enum_entry" => return false,
            "property_declaration" => property = true,
            _ => {}
        }
        at = ancestor.parent();
    }
    property
}

/// `RestTemplate` fetch sites in this file, owned by their innermost
/// enclosing function, else by the file (#1426).
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &[(TsNode<'_>, String)],
    imports: &Imports,
) -> Vec<FetchSite> {
    let source = cx.text(&root);
    if !imports.bindings.values().any(|fqn| fqn == REST_TEMPLATE) && !source.contains(REST_TEMPLATE)
    {
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
        let parts = named_children(call);
        let Some(callee) = parts
            .first()
            .filter(|callee| callee.kind() == "navigation_expression")
        else {
            continue;
        };
        // A trailing lambda is not a RestTemplate argument shape.
        let Some(arguments) = parts
            .iter()
            .skip(1)
            .find(|part| part.kind() == "value_arguments")
        else {
            continue;
        };
        if parts.iter().any(|part| part.kind() == "annotated_lambda") {
            continue;
        }
        let [receiver, name] = named_children(*callee)[..] else {
            continue;
        };
        if name.kind() != "identifier" {
            continue;
        }
        let Some(fixed) = verb(cx.text(&name)) else {
            continue;
        };
        if declared.contains(cx.text(&name)) {
            continue;
        }
        // Spring's reified extension needs its own import; a named import
        // takes priority over a wildcard one.
        if parts.iter().any(|part| part.kind() == "type_arguments")
            && !match imports.bindings.get(cx.text(&name)) {
                Some(fqn) => *fqn == format!("{CLIENT_PACKAGE}.{}", cx.text(&name)),
                None => imports
                    .wildcard_packages
                    .iter()
                    .any(|p| p == CLIENT_PACKAGE),
            }
        {
            continue;
        }
        let values: Vec<TsNode<'_>> = named_children(*arguments)
            .into_iter()
            .filter(|argument| argument.kind() == "value_argument")
            .collect();
        let mut args = Vec::new();
        for value in &values {
            match named_children(*value).as_slice() {
                [expression] => args.push(*expression),
                _ => break,
            }
        }
        // Named arguments are not mapped to parameters.
        if args.len() != values.len() {
            continue;
        }
        let Some(url) = args.first().copied() else {
            continue;
        };
        let http_method = match fixed {
            Some(verb) => verb.to_string(),
            None if args.len() < 2 => continue,
            None => match proof.method(call, args[1]) {
                Some(verb) => verb,
                None if args.len() >= 4 => "?".to_string(),
                None => continue,
            },
        };
        // A request outside any function (a property or top-level
        // initializer) is owned by its file (#1426).
        let symbol = enclosing(functions, call);
        if symbol.is_none() && !initializer(call) {
            continue;
        }
        if !proof.is_client(call, receiver, REST_TEMPLATE, true) {
            continue;
        }
        out.push(FetchSite {
            straight_line: None,
            method: http_method,
            url: match pure_string_literal(cx, url) {
                Some(literal) => IdentityExpr::Literal(literal),
                None => IdentityExpr::Computed(cx.text(&url).to_string()),
            },
            symbol: symbol.cloned(),
            path: cx.path.to_string(),
            byte_start: call.start_byte() as u64,
            byte_end: call.end_byte() as u64,
            base: None,
            entry: None,
        });
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
