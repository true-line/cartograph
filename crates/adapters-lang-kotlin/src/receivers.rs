//! Declared-type receivers (W11c-1, #1048): the lexical binding a call
//! receiver names, and the type its declaration states. Only a type written
//! in source proves anything; an inferred binding blocks outer ones.
use crate::FileCx;
use tree_sitter::Node as TsNode;

/// What a receiver name is bound to at a use site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Binding {
    /// The declaration states this type (dotted, without type arguments or
    /// nullability).
    Typed(String),
    /// Initialized by `T(…)`: proves `T` only if `T` is a constructible class.
    Constructed(String),
    /// Bound, but with no stated type. Outer bindings are shadowed.
    Untyped,
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
fn property_binding(cx: &FileCx<'_>, prop: TsNode<'_>, name: &str) -> Option<Binding> {
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
                        && let Some(binding) = property_binding(cx, member, name)
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
                    .filter_map(|stmt| property_binding(cx, stmt, name))
                    .next_back();
                if found.is_some() {
                    return found;
                }
            }
            "for_statement" | "catch_block" => {
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
                    .filter_map(|stmt| property_binding(cx, stmt, name))
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
                if let Some(binding) = class_binding(cx, scope, name, in_function) {
                    return Some(binding);
                }
                // Only an inner class reaches outer instance state; nested
                // classes, objects and companions see top-level properties.
                if !(scope.kind() == "class_declaration" && is_inner(cx, scope)) {
                    return top_level_binding(cx, scope, name);
                }
                in_function = true;
            }
            "source_file" => return top_level_binding(cx, scope, name),
            _ => {}
        }
        child = scope;
    }
    None
}

/// A top-level property of this file.
fn top_level_binding(cx: &FileCx<'_>, from: TsNode<'_>, name: &str) -> Option<Binding> {
    let mut root = from;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    named_children(root)
        .into_iter()
        .filter(|stmt| stmt.kind() == "property_declaration")
        .find_map(|stmt| property_binding(cx, stmt, name))
}

/// `this.name`: an instance property of the nearest enclosing class.
pub(crate) fn this_property_binding(
    cx: &FileCx<'_>,
    use_site: TsNode<'_>,
    name: &str,
) -> Option<Binding> {
    let mut node = use_site;
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "class_declaration" | "object_declaration" | "companion_object"
        ) {
            return class_binding(cx, parent, name, true);
        }
        node = parent;
    }
    None
}
