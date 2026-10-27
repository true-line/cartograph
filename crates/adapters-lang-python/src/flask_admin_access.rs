//! Whether a Flask-Admin view provably requires a logged-in user (#1216,
//! AC-0421).
//!
//! Flask-Admin (checked against 1.5.7 and 1.5.8, `flask_admin/base.py`)
//! wraps every exposed method, its own included, so `_handle_view` runs
//! first: when `is_accessible()` is false it returns
//! `inaccessible_callback(name, …)`, and a result that is not `None` is the
//! response, so the view never runs. `BaseView` is the only library class
//! defining these hooks; its `is_accessible` returns `True` and its
//! `inaccessible_callback` aborts with 403. A view requires login when, in
//! its C3 order:
//!
//! - the first class defining `is_accessible` is a repository class whose
//!   method is one `return` of `current_user.is_authenticated`
//!   (import-proven `flask_login.current_user`, its file's only binding of
//!   that name a single module-level import, not rebound or replaced), or
//!   of an `and` chain with it as an operand;
//! - the first class defining `inaccessible_callback`, if any, never
//!   returns `None`: no statement before its last returns or yields, and the
//!   last returns a string, an import-proven `flask` `redirect`, `abort`,
//!   `make_response` or `render_template` call, `x or <one of these>`, or a
//!   conditional both of whose branches are;
//! - no repository class defines `_handle_view` or `_run_view`, and every
//!   repository class defining a hook is itself a view class, so it comes
//!   before `BaseView`;
//! - every exact `flask-admin` pin is a checked release, and every exact
//!   `flask-login` pin is 0.3 to 0.6, where `is_authenticated` is a property
//!   that `AnonymousUserMixin` makes false (before 0.3 it is a method, so
//!   always true);
//! - no repository code writes an `anonymous_user` attribute, as in
//!   `login_manager.anonymous_user = …`, which could authenticate anonymous
//!   users, or a hook attribute (`View.is_accessible = …`, `del …`), which
//!   replaces the proven method, by any binding target (assignment, `for`,
//!   comprehension, `with … as`, `del`), or names either in a string
//!   (`setattr` in any spelling, `object.__setattr__`, a `__dict__` key). A
//!   computed or escaped name is not seen; that assumption is recorded.

use crate::auth_guards::{import_dotted, rebound, unparenthesize};
use crate::{FileCx, ImportBinding};
use core_prov::EvidenceRef;
use std::collections::HashMap;
use tree_sitter::Node as TsNode;

/// Hooks a repository class may only define in a view class.
pub(crate) const HOOKS: &[&str] = &[
    "is_accessible",
    "inaccessible_callback",
    "_handle_view",
    "_run_view",
];

/// The proven login check's dotted names.
const LOGIN_CHECKS: &[&str] = &[
    "flask_login.current_user.is_authenticated",
    "flask_login.utils.current_user.is_authenticated",
];

/// Flask calls that return a response or raise.
const RESPONSES: &[&str] = &[
    "flask.abort",
    "flask.helpers.abort",
    "flask.redirect",
    "flask.helpers.redirect",
    "flask.make_response",
    "flask.helpers.make_response",
    "flask.render_template",
    "flask.templating.render_template",
];

/// A hook method a repository class defines: proven, with its spans, or why
/// it is not.
#[derive(Debug, Clone)]
pub(crate) enum Hook {
    Proven(Vec<EvidenceRef>),
    Unproven(String),
}

/// Whether the class `class` binds `name` in its own scope, in any form a
/// Python class body can: a definition, an assignment or loop target, an
/// import, a `with … as`, a `match` capture, or `global name`.
pub(crate) fn binds(cx: &FileCx<'_>, class: TsNode<'_>, name: &str) -> bool {
    let binding = crate::scope_binding(cx, class, name);
    !binding.writes.is_empty() || binding.global || binding.nonlocal
}

/// The proof of the hook `name` the class `class` (named `class_name`)
/// binds: its only binding must be a plain method of the class body.
pub(crate) fn hook(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    class: TsNode<'_>,
    class_name: &str,
    name: &str,
    bindings: &HashMap<String, ImportBinding>,
) -> Hook {
    let binding = crate::scope_binding(cx, class, name);
    let body = class.child_by_field_name("body");
    let function = match binding.writes.as_slice() {
        [function]
            if !binding.global
                && !binding.nonlocal
                && function.kind() == "function_definition"
                && function.parent() == body
                && !cx.text(function).starts_with("async") =>
        {
            *function
        }
        _ => {
            return Hook::Unproven(format!(
                "flask_admin_hook_not_a_plain_method:{name}:{class_name}"
            ));
        }
    };
    let proof = match name {
        "is_accessible" => login_check(cx, root, function, bindings),
        _ => never_none(cx, root, function, bindings),
    };
    proof.map_or_else(
        || {
            Hook::Unproven(match name {
                "is_accessible" => {
                    format!("flask_admin_is_accessible_not_login_check:{class_name}")
                }
                _ => format!("flask_admin_inaccessible_callback_may_return_none:{class_name}"),
            })
        },
        Hook::Proven,
    )
}

fn named(node: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut walk = node.walk();
    node.named_children(&mut walk)
        .filter(|child| child.kind() != "comment")
        .collect()
}

/// A body's statements, without a leading docstring.
fn statements(function: TsNode<'_>) -> Vec<TsNode<'_>> {
    let Some(body) = function.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut statements = named(body);
    if statements.first().is_some_and(|first| {
        first.kind() == "expression_statement"
            && matches!(named(*first).as_slice(), [only] if only.kind() == "string")
    }) {
        statements.remove(0);
    }
    statements
}

/// `return <check>` as `is_accessible`'s only statement.
fn login_check(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    function: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<Vec<EvidenceRef>> {
    let [statement] = statements(function)[..] else {
        return None;
    };
    if statement.kind() != "return_statement" {
        return None;
    }
    let [value] = named(statement)[..] else {
        return None;
    };
    let check = requires_login(cx, root, function, value, bindings)?;
    Some(vec![cx.evidence(&statement), cx.evidence(&check)])
}

/// The `current_user.is_authenticated` operand that makes `expr` false for
/// an anonymous user.
fn requires_login<'t>(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    function: TsNode<'_>,
    expr: TsNode<'t>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<TsNode<'t>> {
    let expr = unparenthesize(expr);
    match expr.kind() {
        "boolean_operator" if operator(cx, expr) == Some("and") => {
            let left = expr.child_by_field_name("left")?;
            let right = expr.child_by_field_name("right")?;
            requires_login(cx, root, function, left, bindings)
                .or_else(|| requires_login(cx, root, function, right, bindings))
        }
        "attribute" => {
            let dotted = import_dotted(cx, expr, bindings)?;
            let name = root_name(cx, expr)?;
            (LOGIN_CHECKS.contains(&dotted.as_str())
                && !rebound(cx, root, name, Some(function))
                && sole_module_import(cx, root, name))
            .then_some(expr)
        }
        _ => None,
    }
}

/// Whether the file's only binding of `name` is one module-level import,
/// so the file-wide import map names what a module-level use sees: an
/// import of it in a function or class elsewhere may be the one the map
/// recorded.
fn sole_module_import(cx: &FileCx<'_>, root: TsNode<'_>, name: &str) -> bool {
    let module = crate::module_binding(cx, root, name);
    let [import] = module.writes[..] else {
        return false;
    };
    if module.global || !matches!(import.kind(), "import_statement" | "import_from_statement") {
        return false;
    }
    let mut imports = 0;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "import_statement" | "import_from_statement") {
            imports += usize::from(crate::import_binds(cx, node, name));
            continue;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    imports == 1
}

fn operator<'s>(cx: &'s FileCx<'_>, expr: TsNode<'_>) -> Option<&'s str> {
    expr.child_by_field_name("operator")
        .map(|operator| cx.text(&operator))
}

/// The identifier an attribute chain starts from.
fn root_name<'s>(cx: &'s FileCx<'_>, mut node: TsNode<'_>) -> Option<&'s str> {
    while node.kind() == "attribute" {
        node = node.child_by_field_name("object")?;
    }
    (node.kind() == "identifier").then(|| cx.text(&node))
}

/// `inaccessible_callback` returns a value that is not `None` on every path.
fn never_none(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    function: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<Vec<EvidenceRef>> {
    let statements = statements(function);
    let (last, rest) = statements.split_last()?;
    if last.kind() != "return_statement" || rest.iter().any(|statement| exits(*statement)) {
        return None;
    }
    let [value] = named(*last)[..] else {
        return None;
    };
    if exits(value) {
        return None;
    }
    let mut evidence = vec![cx.evidence(last)];
    responds(cx, root, function, value, bindings, &mut evidence).then_some(evidence)
}

/// Whether `node` holds a `return`, `yield` or `await` outside a nested scope.
fn exits(node: TsNode<'_>) -> bool {
    match node.kind() {
        "return_statement" | "yield" | "await" => true,
        "function_definition" | "class_definition" | "lambda" => false,
        _ => named(node).into_iter().any(exits),
    }
}

/// Whether `expr` is never `None`, citing each response call.
fn responds(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    function: TsNode<'_>,
    expr: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    evidence: &mut Vec<EvidenceRef>,
) -> bool {
    let expr = unparenthesize(expr);
    match expr.kind() {
        "string" | "concatenated_string" => true,
        "boolean_operator" if operator(cx, expr) == Some("or") => expr
            .child_by_field_name("right")
            .is_some_and(|right| responds(cx, root, function, right, bindings, evidence)),
        "conditional_expression" => match named(expr)[..] {
            [body, _, alternative] => {
                responds(cx, root, function, body, bindings, evidence)
                    && responds(cx, root, function, alternative, bindings, evidence)
            }
            _ => false,
        },
        "call" => {
            let Some(callee) = expr.child_by_field_name("function") else {
                return false;
            };
            let proven = import_dotted(cx, callee, bindings)
                .is_some_and(|dotted| RESPONSES.contains(&dotted.as_str()))
                && root_name(cx, callee).is_some_and(|name| {
                    !rebound(cx, root, name, Some(function)) && sole_module_import(cx, root, name)
                });
            if proven {
                evidence.push(cx.evidence(&expr));
            }
            proven
        }
        _ => false,
    }
}

/// The target a binding statement writes: an assignment's or `for`
/// loop's left side, a comprehension's `for` target, a `with … as` or
/// `except … as` alias, or a `del` statement's operands.
pub(crate) fn binding_target(node: TsNode<'_>) -> Option<TsNode<'_>> {
    match node.kind() {
        "assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" => {
            node.child_by_field_name("left")
        }
        "as_pattern" | "except_clause" => node.child_by_field_name("alias"),
        "delete_statement" => Some(node),
        _ => None,
    }
}

/// Whether `node` may write an attribute named one of `names`: as a
/// binding target (`target`), an attribute of that name anywhere in it, or
/// otherwise a string literal naming it, as `setattr`, `builtins.setattr`,
/// `object.__setattr__` or a `__dict__` key would. A name computed or
/// spelled with escapes is not seen.
pub(crate) fn names_attribute(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    names: &[&str],
    target: bool,
) -> bool {
    match node.kind() {
        "attribute" if target => node
            .child_by_field_name("attribute")
            .is_some_and(|attribute| names.contains(&cx.text(&attribute))),
        "string" => {
            crate::literal_string(cx, node).is_some_and(|text| names.contains(&text.as_str()))
        }
        _ if target => named(node)
            .into_iter()
            .any(|child| names_attribute(cx, child, names, true)),
        _ => false,
    }
}
