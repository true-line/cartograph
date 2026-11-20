//! Request URLs read from a struct field or a package constant (#2065).
//!
//! A Go client commonly keeps its base URL in a field set where the value
//! is built (`&Client{API: "https://api.github.com"}`) or in a package
//! constant, and builds each request as `c.API + path`. A file records
//! what it binds: each package constant, and each keyed field of a
//! composite literal or `x.F = …` assignment. A client call whose URL is
//! `x.F`, `x.F + rest`, `NAME` or `NAME + rest` is held until every file of
//! the package is read, then joined to the base when every binding of that
//! field (or constant) in the package is the same literal. A field is
//! matched by type when the operand is the enclosing method's receiver and
//! the literal names that type; an assignment, whose operand type is not
//! known, always counts. Any other binding, or two literals, leave the call
//! as the computed URL it was. Test files bind nothing.

use super::*;
use adapters_fw::client::{BaseJoin, FetchBase, FetchSite};
use adapters_fw::events::IdentityExpr;

/// What a URL operand names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Key {
    /// A package constant.
    Const(String),
    /// A struct field, with the struct type when it is known.
    Field { ty: Option<String>, field: String },
}

/// What one binding gives a constant or field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Value {
    Literal(String),
    /// Another package constant.
    Const(String),
    /// Anything else: the binding is not proven.
    Other,
}

/// One binding a file makes.
#[derive(Debug, Clone)]
pub(super) struct Binding {
    pub package_dir: String,
    pub key: Key,
    pub value: Value,
}

/// A client call whose base is a field or constant, held for the package.
#[derive(Debug, Clone)]
pub(super) struct Pending {
    pub package_dir: String,
    pub key: Key,
    /// The call as recovered, kept unchanged when the base is not proven.
    pub site: FetchSite,
    /// The path joined to the base; `None` when the URL is the base alone.
    pub rest: Option<IdentityExpr>,
    /// Offset of the operand that names the base.
    pub base_start: u64,
}

fn literal(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    let text = cx.text(&node);
    match node.kind() {
        "raw_string_literal" if !text.contains('\r') => unquote(text),
        "interpreted_string_literal" if !text.contains('\\') => unquote(text),
        _ => None,
    }
}

fn value(cx: &FileCx<'_>, node: TsNode<'_>) -> Value {
    if let Some(text) = literal(cx, node) {
        return Value::Literal(text);
    }
    match node.kind() {
        "identifier" => Value::Const(cx.text(&node).to_string()),
        "parenthesized_expression" => node
            .named_child(0)
            .map_or(Value::Other, |inner| value(cx, inner)),
        _ => Value::Other,
    }
}

/// The type a composite literal builds: its last name, generics dropped.
fn literal_type(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    let ty = node.child_by_field_name("type")?;
    let named = match ty.kind() {
        "type_identifier" => ty,
        "qualified_type" => ty.child_by_field_name("name")?,
        "generic_type" => ty.child_by_field_name("type")?,
        _ => return None,
    };
    Some(cx.text(&named).to_string())
}

/// Every binding in a file. A test file binds nothing.
pub(super) fn bindings(cx: &FileCx<'_>, root: TsNode<'_>, package_dir: &str) -> Vec<Binding> {
    if cx.path.ends_with("_test.go") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut push = |key: Key, value: Value| {
        out.push(Binding {
            package_dir: package_dir.to_string(),
            key,
            value,
        })
    };
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            // A top-level constant only: a function's constant is local.
            "const_spec" if node.parent().and_then(|p| p.parent()) == Some(root) => {
                let mut walk = node.walk();
                let names: Vec<_> = node
                    .children_by_field_name("name", &mut walk)
                    .map(|name| cx.text(&name).to_string())
                    .collect();
                let values: Vec<_> = node
                    .child_by_field_name("value")
                    .map(|list| {
                        let mut walk = list.walk();
                        list.named_children(&mut walk).collect()
                    })
                    .unwrap_or_default();
                for (i, name) in names.into_iter().enumerate() {
                    let bound = values.get(i).map_or(Value::Other, |v| value(cx, *v));
                    push(Key::Const(name), bound);
                }
            }
            "keyed_element" => {
                let key = node
                    .child_by_field_name("key")
                    .and_then(|k| k.named_child(0))
                    .filter(|k| matches!(k.kind(), "identifier" | "field_identifier"));
                let body = node.parent().filter(|p| p.kind() == "literal_value");
                let composite = body
                    .and_then(|b| b.parent())
                    .filter(|c| c.kind() == "composite_literal");
                if let (Some(key), Some(composite)) = (key, composite) {
                    let bound = node
                        .child_by_field_name("value")
                        .and_then(|v| v.named_child(0))
                        .map_or(Value::Other, |v| value(cx, v));
                    push(
                        Key::Field {
                            ty: literal_type(cx, composite),
                            field: cx.text(&key).to_string(),
                        },
                        bound,
                    );
                }
            }
            "assignment_statement" => {
                let left = node.child_by_field_name("left");
                let right = node.child_by_field_name("right");
                if let (Some(left), Some(right)) = (left, right) {
                    let mut walk = left.walk();
                    let targets: Vec<_> = left.named_children(&mut walk).collect();
                    let mut walk = right.walk();
                    let sources: Vec<_> = right.named_children(&mut walk).collect();
                    for (i, target) in targets.iter().enumerate() {
                        if target.kind() != "selector_expression" {
                            continue;
                        }
                        let Some(field) = target.child_by_field_name("field") else {
                            continue;
                        };
                        let bound = match sources.get(i) {
                            Some(source) if targets.len() == sources.len() => value(cx, *source),
                            _ => Value::Other,
                        };
                        push(
                            Key::Field {
                                ty: None,
                                field: cx.text(&field).to_string(),
                            },
                            bound,
                        );
                    }
                }
            }
            _ => {}
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    out
}

/// The receiver name and type of the method around `node`.
fn receiver(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Option<(String, String)> {
    while node.kind() != "method_declaration" {
        node = node.parent()?;
    }
    let list = node.child_by_field_name("receiver")?;
    let parameter = list.named_child(0)?;
    let name = cx.text(&parameter.child_by_field_name("name")?).to_string();
    let mut ty = parameter.child_by_field_name("type")?;
    if ty.kind() == "pointer_type" {
        ty = ty.named_child(0)?;
    }
    if ty.kind() == "generic_type" {
        ty = ty.child_by_field_name("type")?;
    }
    Some((name, cx.text(&ty).to_string()))
}

/// Whether the declaration `scope` declares `name` itself: a parameter,
/// result, local variable or constant, or a range or short-declaration
/// name, any of which hides the package constant.
fn declared_locally(cx: &FileCx<'_>, scope: TsNode<'_>, name: &str) -> bool {
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        let declared = match node.kind() {
            "parameter_declaration"
            | "variadic_parameter_declaration"
            | "var_spec"
            | "const_spec" => {
                let mut walk = node.walk();
                node.children_by_field_name("name", &mut walk)
                    .any(|n| cx.text(&n) == name)
            }
            "short_var_declaration" | "range_clause" => {
                node.child_by_field_name("left").is_some_and(|left| {
                    let mut walk = left.walk();
                    let found = left
                        .named_children(&mut walk)
                        .any(|n| n.kind() == "identifier" && cx.text(&n) == name);
                    found || (left.kind() == "identifier" && cx.text(&left) == name)
                })
            }
            _ => false,
        };
        if declared {
            return true;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    false
}

/// The key a URL operand names, if it names a field or a constant.
fn key(cx: &FileCx<'_>, node: TsNode<'_>, at: TsNode<'_>) -> Option<Key> {
    match node.kind() {
        "identifier" => {
            let name = cx.text(&node);
            // A local or parameter of the name shadows the constant.
            (!declared_locally(cx, client::top_level(at), name)).then(|| Key::Const(name.into()))
        }
        "selector_expression" => {
            let operand = node.child_by_field_name("operand")?;
            let field = cx.text(&node.child_by_field_name("field")?).to_string();
            let ty = receiver(cx, at)
                .filter(|(name, _)| {
                    operand.kind() == "identifier" && cx.text(&operand) == name.as_str()
                })
                .map(|(_, ty)| ty);
            Some(Key::Field { ty, field })
        }
        _ => None,
    }
}

/// Hold `site` for its package when its URL is a field or constant, alone
/// or with a path added: `url` is the URL argument of the call.
pub(super) fn pending(
    cx: &FileCx<'_>,
    site: FetchSite,
    url: TsNode<'_>,
    call: TsNode<'_>,
    package_dir: &str,
    classify: impl Fn(TsNode<'_>) -> IdentityExpr,
) -> Result<Pending, Box<FetchSite>> {
    if !matches!(site.url, IdentityExpr::Computed(_)) {
        return Err(Box::new(site));
    }
    let (base, rest) = match url.kind() {
        "binary_expression"
            if url
                .child_by_field_name("operator")
                .is_some_and(|op| cx.text(&op) == "+") =>
        {
            let (Some(left), Some(right)) = (
                url.child_by_field_name("left"),
                url.child_by_field_name("right"),
            ) else {
                return Err(Box::new(site));
            };
            (left, Some(classify(right)))
        }
        _ => (url, None),
    };
    let Some(key) = key(cx, base, call) else {
        return Err(Box::new(site));
    };
    Ok(Pending {
        package_dir: package_dir.to_string(),
        key,
        base_start: base.start_byte() as u64,
        site,
        rest,
    })
}

/// The one literal every binding of `key` in the package gives it.
fn resolve(
    bindings: &BTreeMap<String, Vec<(Key, Value)>>,
    package_dir: &str,
    key: &Key,
    depth: usize,
) -> Option<String> {
    let package = bindings.get(package_dir)?;
    let matching = package.iter().filter(|(bound, _)| match (key, bound) {
        (Key::Const(name), Key::Const(other)) => name == other,
        (
            Key::Field { ty, field },
            Key::Field {
                ty: other,
                field: f,
            },
        ) => field == f && (ty.is_none() || other.is_none() || ty == other),
        _ => false,
    });
    let mut found: Option<String> = None;
    let mut any = false;
    for (_, value) in matching {
        any = true;
        let text = match value {
            Value::Literal(text) => text.clone(),
            Value::Const(name) if depth < 4 => {
                resolve(bindings, package_dir, &Key::Const(name.clone()), depth + 1)?
            }
            _ => return None,
        };
        match &found {
            Some(seen) if *seen != text => return None,
            _ => found = Some(text),
        }
    }
    any.then_some(found).flatten()
}

/// Join each held call to its base when the package proves it; the rest
/// keep the URL they were recovered with.
pub(super) fn join(pending: Vec<Pending>, bindings: &[Binding]) -> Vec<FetchSite> {
    let mut by_package: BTreeMap<String, Vec<(Key, Value)>> = BTreeMap::new();
    for binding in bindings {
        by_package
            .entry(binding.package_dir.clone())
            .or_default()
            .push((binding.key.clone(), binding.value.clone()));
    }
    pending
        .into_iter()
        .map(|held| {
            let Some(base) = resolve(&by_package, &held.package_dir, &held.key, 0) else {
                return held.site;
            };
            let mut site = held.site;
            match held.rest {
                None => site.url = IdentityExpr::Literal(base),
                Some(rest) => {
                    site.url = rest;
                    site.base = Some(FetchBase {
                        url: IdentityExpr::Literal(base),
                        join: BaseJoin::Concat,
                        path: site.path.clone(),
                        byte_start: held.base_start,
                    });
                }
            }
            site
        })
        .collect()
}
