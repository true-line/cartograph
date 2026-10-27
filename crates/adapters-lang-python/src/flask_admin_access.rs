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
//!   (import-proven `flask_login.current_user`, not rebound or replaced), or
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
//!   users. A `setattr` with a computed name is not seen; that assumption is
//!   recorded.

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

/// The proof of the hook `name` in the class body `body`, when the class
/// binds that name.
pub(crate) fn hook(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    body: TsNode<'_>,
    class: &str,
    name: &str,
    bindings: &HashMap<String, ImportBinding>,
) -> Hook {
    let mut found = Vec::new();
    for child in named(body) {
        let mut names = std::collections::BTreeSet::new();
        match child.kind() {
            "function_definition" | "class_definition" => {
                if let Some(own) = child.child_by_field_name("name") {
                    names.insert(cx.text(&own).to_string());
                }
            }
            "decorated_definition" => {
                if let Some(own) = child
                    .child_by_field_name("definition")
                    .and_then(|definition| definition.child_by_field_name("name"))
                {
                    names.insert(cx.text(&own).to_string());
                }
            }
            _ => crate::flask_admin::class_names(cx, child, &mut names),
        }
        if names.contains(name) {
            found.push(child);
        }
    }
    let function = match found.as_slice() {
        [function]
            if function.kind() == "function_definition"
                && !cx.text(function).starts_with("async") =>
        {
            *function
        }
        _ => {
            return Hook::Unproven(format!(
                "flask_admin_hook_not_a_plain_method:{name}:{class}"
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
                "is_accessible" => format!("flask_admin_is_accessible_not_login_check:{class}"),
                _ => format!("flask_admin_inaccessible_callback_may_return_none:{class}"),
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
            (LOGIN_CHECKS.contains(&dotted.as_str())
                && !rebound(cx, root, root_name(cx, expr)?, Some(function)))
            .then_some(expr)
        }
        _ => None,
    }
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
                && root_name(cx, callee)
                    .is_some_and(|name| !rebound(cx, root, name, Some(function)));
            if proven {
                evidence.push(cx.evidence(&expr));
            }
            proven
        }
        _ => false,
    }
}

/// Whether `target` writes an `anonymous_user` attribute, or the call
/// `setattr(…, "anonymous_user", …)` does.
pub(crate) fn writes_anonymous_user(cx: &FileCx<'_>, node: TsNode<'_>) -> bool {
    match node.kind() {
        "attribute" => node
            .child_by_field_name("attribute")
            .is_some_and(|attribute| cx.text(&attribute) == "anonymous_user"),
        "call" => {
            node.child_by_field_name("function").is_some_and(|callee| {
                callee.kind() == "identifier" && cx.text(&callee) == "setattr"
            }) && node
                .child_by_field_name("arguments")
                .and_then(|arguments| named(arguments).get(1).copied())
                .and_then(|name| crate::literal_string(cx, unparenthesize(name)))
                .is_some_and(|name| name == "anonymous_user")
        }
        _ => named(node)
            .into_iter()
            .any(|child| writes_anonymous_user(cx, child)),
    }
}
