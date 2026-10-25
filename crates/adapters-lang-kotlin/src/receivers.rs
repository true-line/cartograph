//! Declared-type receivers (W11c-1, #1048): the lexical binding a call
//! receiver names, and the type its declaration states. Only a type written
//! in source proves anything; an inferred binding blocks outer ones.
use crate::FileCx;
use std::collections::HashSet;
use tree_sitter::Node as TsNode;

/// Hilt's Compose `hiltViewModel` (W11c-2). Its result is the view model
/// type: the type argument, or what its creation callback returns.
pub(crate) const HILT_VIEW_MODEL: &[&str] = &[
    "androidx.hilt.navigation.compose.hiltViewModel",
    "androidx.hilt.lifecycle.viewmodel.compose.hiltViewModel",
];

/// What a receiver name is bound to at a use site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Binding {
    /// The declaration states this type (dotted, without type arguments or
    /// nullability).
    Typed(String),
    /// Initialized by `T(…)`: proves `T` only if `T` is a constructible class.
    Constructed(String),
    /// `hiltViewModel(…) { f: F -> f.method(…) }`: the declared return type
    /// of `F.method`, resolved at the directory join.
    Factory { factory: String, method: String },
    /// Bound, but with no stated type. Outer bindings are shadowed.
    Untyped,
}

/// The innermost callee identifier of a call: `f` in `f(…)`, `f<T>(…)`,
/// `f { … }` and `f(…) { … }`.
fn call_head<'t>(call: TsNode<'t>) -> Option<TsNode<'t>> {
    let mut node = call;
    while node.kind() == "call_expression" {
        node = node.named_child(0)?;
    }
    (node.kind() == "identifier").then_some(node)
}

/// The type a proven `hiltViewModel` call produces. `None` when the call is
/// not one; `Some(Untyped)` when it states no type this rule reads.
pub(crate) fn hilt_binding(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    hilt: &HashSet<String>,
) -> Option<Binding> {
    if call.kind() != "call_expression" {
        return None;
    }
    let head = call_head(call)?;
    if !hilt.contains(cx.text(&head)) {
        return None;
    }
    let inner = head.parent()?;
    if let Some(args) = named_children(inner)
        .into_iter()
        .find(|part| part.kind() == "type_arguments")
    {
        // `hiltViewModel<VM>()` or `hiltViewModel<VM, VMF>(…)`: VM first.
        let ty = named_children(args)
            .into_iter()
            .next()
            .and_then(|projection| named_children(projection).into_iter().find(is_type_node))
            .and_then(|ty| declared_type(cx, ty));
        return Some(ty.map_or(Binding::Untyped, Binding::Typed));
    }
    let lambda = named_children(call)
        .into_iter()
        .find(|part| part.kind() == "annotated_lambda")
        .and_then(|annotated| named_children(annotated).into_iter().next())
        .filter(|lambda| lambda.kind() == "lambda_literal");
    let factory = lambda.and_then(|lambda| {
        let parts = named_children(lambda);
        let [params, body @ ..] = parts.as_slice() else {
            return None;
        };
        if params.kind() != "lambda_parameters" {
            return None;
        }
        let [param] = named_children(*params)[..] else {
            return None;
        };
        if param.kind() != "variable_declaration" {
            return None;
        }
        let param_parts = named_children(param);
        let name = param_parts.iter().find(|p| p.kind() == "identifier")?;
        let factory = param_parts
            .iter()
            .find(|p| is_type_node(p))
            .and_then(|ty| declared_type(cx, *ty))?;
        // The callback's value is its last expression: `f.method(…)`.
        let last = body
            .last()
            .filter(|last| last.kind() == "call_expression")?;
        let callee = last
            .named_child(0)
            .filter(|c| c.kind() == "navigation_expression")?;
        let [receiver, method] = named_children(callee)[..] else {
            return None;
        };
        (receiver.kind() == "identifier"
            && cx.text(&receiver) == cx.text(name)
            && method.kind() == "identifier")
            .then(|| Binding::Factory {
                factory,
                method: cx.text(&method).to_string(),
            })
    });
    Some(factory.unwrap_or(Binding::Untyped))
}

/// The type a type node states: `T`, `a.T`, `T<X>` and `T?` give their
/// dotted user type; function, star and other types give `None`.
pub(crate) fn declared_type(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    match node.kind() {
        "nullable_type" | "parenthesized_type" => {
            let mut walk = node.walk();
            let inner = node.named_children(&mut walk).next()?;
            declared_type(cx, inner)
        }
        "user_type" => {
            let mut walk = node.walk();
            let parts: Vec<&str> = node
                .named_children(&mut walk)
                .filter(|part| part.kind() == "identifier")
                .map(|part| cx.text(&part))
                .collect();
            (!parts.is_empty()).then(|| parts.join("."))
        }
        _ => None,
    }
}

fn named_children<'t>(node: TsNode<'t>) -> Vec<TsNode<'t>> {
    let mut walk = node.walk();
    node.named_children(&mut walk).collect()
}

fn is_type_node(node: &TsNode<'_>) -> bool {
    matches!(
        node.kind(),
        "user_type"
            | "nullable_type"
            | "function_type"
            | "parenthesized_type"
            | "non_nullable_type"
    )
}

/// `identifier` plus optional stated type, for a `variable_declaration`,
/// `parameter`, or `class_parameter`.
fn declaration_binding(cx: &FileCx<'_>, decl: TsNode<'_>, name: &str) -> Option<Binding> {
    let children = named_children(decl);
    let ident = children.iter().find(|child| child.kind() == "identifier")?;
    if cx.text(ident) != name {
        return None;
    }
    Some(
        children
            .iter()
            .find(|child| is_type_node(child))
            .and_then(|ty| declared_type(cx, *ty))
            .map_or(Binding::Untyped, Binding::Typed),
    )
}

/// A `property_declaration` binding `name`: its stated type, else a
/// `T(…)` initializer, else untyped (delegates and other initializers).
fn property_binding(
    cx: &FileCx<'_>,
    prop: TsNode<'_>,
    name: &str,
    hilt: &HashSet<String>,
) -> Option<Binding> {
    let children = named_children(prop);
    if let Some(multi) = children
        .iter()
        .find(|child| child.kind() == "multi_variable_declaration")
    {
        return named_children(*multi)
            .into_iter()
            .any(|decl| declaration_binding(cx, decl, name).is_some())
            .then_some(Binding::Untyped);
    }
    let decl = children
        .iter()
        .find(|child| child.kind() == "variable_declaration")?;
    let binding = declaration_binding(cx, *decl, name)?;
    if binding != Binding::Untyped {
        return Some(binding);
    }
    if children
        .iter()
        .any(|child| child.kind() == "property_delegate")
    {
        return Some(Binding::Untyped);
    }
    if let Some(binding) = children
        .iter()
        .filter(|child| child.kind() == "call_expression")
        .find_map(|call| hilt_binding(cx, *call, hilt))
    {
        return Some(binding);
    }
    let constructed = children
        .iter()
        .filter(|child| child.kind() == "call_expression")
        .find_map(|call| {
            let callee = call.named_child(0)?;
            let text = cx.text(&callee);
            (callee.kind() == "identifier" && text.starts_with(|c: char| c.is_ascii_uppercase()))
                .then(|| text.to_string())
        });
    Some(constructed.map_or(Binding::Untyped, Binding::Constructed))
}

/// Instance properties a class or object declares: primary-constructor
/// `val`/`var` parameters and body properties. Plain constructor parameters
/// count only outside member functions (initializers and `init` blocks).
fn class_binding(
    cx: &FileCx<'_>,
    class: TsNode<'_>,
    name: &str,
    in_function: bool,
    hilt: &HashSet<String>,
) -> Option<Binding> {
    for child in named_children(class) {
        match child.kind() {
            "primary_constructor" => {
                for params in named_children(child) {
                    if params.kind() != "class_parameters" {
                        continue;
                    }
                    for param in named_children(params) {
                        let is_property = {
                            let mut walk = param.walk();
                            param
                                .children(&mut walk)
                                .any(|token| matches!(token.kind(), "val" | "var"))
                        };
                        if (is_property || !in_function)
                            && let Some(binding) = declaration_binding(cx, param, name)
                        {
                            return Some(binding);
                        }
                    }
                }
            }
            "class_body" => {
                for member in named_children(child) {
                    if member.kind() == "property_declaration"
                        && let Some(binding) = property_binding(cx, member, name, hilt)
                    {
                        return Some(binding);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn is_inner(cx: &FileCx<'_>, class: TsNode<'_>) -> bool {
    named_children(class)
        .into_iter()
        .filter(|child| child.kind() == "modifiers")
        .flat_map(named_children)
        .any(|modifier| cx.text(&modifier) == "inner")
}

/// The nearest lexical binding of `name` visible at `use_site`, if any.
/// `None` means the name is not bound in this file (an import, an inherited
/// member, or a global), so other resolution applies.
pub(crate) fn lexical_binding(
    cx: &FileCx<'_>,
    use_site: TsNode<'_>,
    name: &str,
    hilt: &HashSet<String>,
) -> Option<Binding> {
    let mut child = use_site;
    let mut in_function = false;
    while let Some(scope) = child.parent() {
        match scope.kind() {
            "block" | "statements" => {
                // Locals are visible after their declaration; the latest
                // preceding one wins.
                let found = named_children(scope)
                    .into_iter()
                    .filter(|stmt| stmt.end_byte() <= use_site.start_byte())
                    .filter(|stmt| stmt.kind() == "property_declaration")
                    .filter_map(|stmt| property_binding(cx, stmt, name, hilt))
                    .next_back();
                if found.is_some() {
                    return found;
                }
            }
            // A loop variable is in scope only in the body, not the iterable.
            "for_statement" | "catch_block"
                if scope.kind() == "catch_block"
                    || named_children(scope)
                        .last()
                        .is_some_and(|body| body.id() == child.id()) =>
            {
                for decl in named_children(scope) {
                    if matches!(decl.kind(), "variable_declaration" | "parameter")
                        && let Some(binding) = declaration_binding(cx, decl, name)
                    {
                        return Some(binding);
                    }
                    if decl.kind() == "multi_variable_declaration"
                        && named_children(decl)
                            .into_iter()
                            .any(|inner| declaration_binding(cx, inner, name).is_some())
                    {
                        return Some(Binding::Untyped);
                    }
                }
            }
            "lambda_literal" => {
                // A lambda's statements are its direct children.
                let local = named_children(scope)
                    .into_iter()
                    .filter(|stmt| stmt.end_byte() <= use_site.start_byte())
                    .filter(|stmt| stmt.kind() == "property_declaration")
                    .filter_map(|stmt| property_binding(cx, stmt, name, hilt))
                    .next_back();
                if local.is_some() {
                    return local;
                }
                let params = named_children(scope)
                    .into_iter()
                    .find(|part| part.kind() == "lambda_parameters");
                match params {
                    Some(params) => {
                        for decl in named_children(params) {
                            if decl.kind() == "variable_declaration"
                                && let Some(binding) = declaration_binding(cx, decl, name)
                            {
                                return Some(binding);
                            }
                            if decl.kind() == "multi_variable_declaration"
                                && named_children(decl)
                                    .into_iter()
                                    .any(|inner| declaration_binding(cx, inner, name).is_some())
                            {
                                return Some(Binding::Untyped);
                            }
                        }
                    }
                    None if name == "it" => return Some(Binding::Untyped),
                    None => {}
                }
            }
            "function_declaration" | "anonymous_function" | "secondary_constructor" => {
                in_function = true;
                for part in named_children(scope) {
                    if part.kind() != "function_value_parameters" {
                        continue;
                    }
                    for param in named_children(part) {
                        if param.kind() == "parameter"
                            && let Some(binding) = declaration_binding(cx, param, name)
                        {
                            return Some(binding);
                        }
                    }
                }
            }
            "getter" | "setter" => in_function = true,
            "class_declaration" | "object_declaration" | "companion_object" => {
                if let Some(binding) = class_binding(cx, scope, name, in_function, hilt) {
                    return Some(binding);
                }
                // A local class captures its enclosing function's bindings
                // and an inner class reaches outer instance state; member
                // nested classes, objects and companions see only the top level.
                let local = scope
                    .parent()
                    .is_some_and(|parent| !matches!(parent.kind(), "class_body" | "source_file"));
                let inner = scope.kind() == "class_declaration" && is_inner(cx, scope);
                if !local && !inner {
                    return top_level_binding(cx, scope, name, hilt);
                }
                in_function = true;
            }
            "source_file" => return top_level_binding(cx, scope, name, hilt),
            _ => {}
        }
        child = scope;
    }
    None
}

/// A top-level property of this file.
fn top_level_binding(
    cx: &FileCx<'_>,
    from: TsNode<'_>,
    name: &str,
    hilt: &HashSet<String>,
) -> Option<Binding> {
    let mut root = from;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    named_children(root)
        .into_iter()
        .filter(|stmt| stmt.kind() == "property_declaration")
        .find_map(|stmt| property_binding(cx, stmt, name, hilt))
}

/// `this.name`: an instance property of the nearest enclosing class.
pub(crate) fn this_property_binding(
    cx: &FileCx<'_>,
    use_site: TsNode<'_>,
    name: &str,
    hilt: &HashSet<String>,
) -> Option<Binding> {
    let mut node = use_site;
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "class_declaration" | "object_declaration" | "companion_object"
        ) {
            return class_binding(cx, parent, name, true, hilt);
        }
        node = parent;
    }
    None
}

/// Whether this file declares `name` anywhere: a function, type, property,
/// or parameter. Any of them can shadow an imported or default-imported
/// callable, so a rule relying on that callable's identity fails closed.
pub(crate) fn declares_name(cx: &FileCx<'_>, root: TsNode<'_>, name: &str) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "function_declaration"
                | "class_declaration"
                | "object_declaration"
                | "class_parameter"
                | "parameter"
                | "variable_declaration"
        ) && named_children(node)
            .into_iter()
            .find(|part| part.kind() == "identifier")
            .is_some_and(|ident| cx.text(&ident) == name)
        {
            return true;
        }
        stack.extend(named_children(node));
    }
    false
}

/// Whether `name` is a type parameter of a declaration enclosing `site`:
/// `fun <T> f(x: T)` or `class C<T>` (W11c-1). Such a type proves no target.
pub(crate) fn is_type_parameter(cx: &FileCx<'_>, site: TsNode<'_>, name: &str) -> bool {
    let mut node = site;
    while let Some(parent) = node.parent() {
        node = parent;
        if !matches!(
            parent.kind(),
            "function_declaration" | "class_declaration" | "anonymous_function"
        ) {
            continue;
        }
        let declares = named_children(parent)
            .into_iter()
            .filter(|part| part.kind() == "type_parameters")
            .flat_map(named_children)
            .filter(|param| param.kind() == "type_parameter")
            .any(|param| {
                named_children(param)
                    .into_iter()
                    .find(|part| part.kind() == "identifier")
                    .is_some_and(|ident| cx.text(&ident) == name)
            });
        if declares {
            return true;
        }
    }
    false
}

/// The receiver of the nearest enclosing `with(x) { … }` lambda: `x`'s
/// binding, or a proven `hiltViewModel` call's type. `Some(Untyped)` when the
/// receiver is an expression with no stated type.
pub(crate) fn with_receiver_binding(
    cx: &FileCx<'_>,
    use_site: TsNode<'_>,
    hilt: &HashSet<String>,
) -> Option<Binding> {
    let mut node = use_site;
    while let Some(parent) = node.parent() {
        node = parent;
        if parent.kind() != "lambda_literal" {
            continue;
        }
        let Some(call) = parent
            .parent()
            .filter(|a| a.kind() == "annotated_lambda")
            .and_then(|a| a.parent())
            .filter(|c| c.kind() == "call_expression")
        else {
            continue;
        };
        let Some(head) = call
            .named_child(0)
            .filter(|h| h.kind() == "call_expression")
        else {
            continue;
        };
        if head
            .named_child(0)
            .is_none_or(|callee| callee.kind() != "identifier" || cx.text(&callee) != "with")
        {
            continue;
        }
        let args: Vec<TsNode<'_>> = named_children(head)
            .into_iter()
            .filter(|part| part.kind() == "value_arguments")
            .flat_map(named_children)
            .collect();
        let [arg] = args[..] else {
            return Some(Binding::Untyped);
        };
        let Some(expr) = named_children(arg).into_iter().next_back() else {
            return Some(Binding::Untyped);
        };
        return Some(match expr.kind() {
            "identifier" => {
                lexical_binding(cx, call, cx.text(&expr), hilt).unwrap_or(Binding::Untyped)
            }
            "call_expression" => hilt_binding(cx, expr, hilt).unwrap_or(Binding::Untyped),
            _ => Binding::Untyped,
        });
    }
    None
}
