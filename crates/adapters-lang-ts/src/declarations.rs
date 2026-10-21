//! Direct declaration facts that make non-callable source citable (#693).
//!
//! Top-level variables and the first-level properties of their object-literal
//! initializers become `Declaration` nodes, member decorators become
//! `Decorator` nodes, and a callable whose span exceeds the inspection range
//! cap gets a `Signature` node over its header. Existing Symbol spans and
//! identities are untouched (ADR-0039). Props carry names and spans only, so
//! no literal value is persisted beyond the cited source range.

use super::{Edge, Extraction, FileCx, Node, TsNode, callable, captured, file_id};
use std::collections::HashMap;

/// Mirrors the host's per-range inspection cap. A larger span is citable only
/// through narrower facts; a header that alone exceeds it is a named gap.
pub const MAX_CITABLE_SPAN_BYTES: usize = 8192;

pub(super) fn emit(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    syntax_nodes: &[TsNode<'_>],
    out: &mut Extraction,
    mut direct: Option<&mut captured::DirectFacts>,
) {
    let mut push = |node: Node, edge: Edge, out: &mut Extraction| {
        if let Some(direct) = direct.as_deref_mut() {
            direct.declarations.insert(node.id.clone(), node.clone());
        }
        out.nodes.push(node);
        out.edges.push(edge);
    };
    let file = file_id(cx.id.repo, cx.path);
    let top = top_level_declarators(root);
    let names = unique_names(cx, &top, |cx, declarator| {
        declarator
            .child_by_field_name("name")
            .filter(|name| name.kind() == "identifier")
            .map(|name| cx.text(&name).to_owned())
    });
    for (declarator, name) in top.iter().zip(names) {
        let Some(name) = name else { continue };
        let value = declarator.child_by_field_name("value").map(unwrap_value);
        if value.is_some_and(callable::is_callable) {
            continue; // Already a Symbol.
        }
        let id = decl_id(cx, &name);
        let binding = declarator
            .parent()
            .and_then(|statement| statement.child(0))
            .map(|keyword| cx.text(&keyword).to_owned())
            .unwrap_or_default();
        let exported = declarator
            .parent()
            .and_then(|statement| statement.parent())
            .is_some_and(|parent| parent.kind() == "export_statement");
        push(
            declaration(cx, &id, &name, "Variable", *declarator, |props| {
                props["binding"] = serde_json::json!(binding);
                props["exported"] = serde_json::json!(exported);
            }),
            defined_in(cx, &id, &file, *declarator),
            out,
        );
        let Some(object) = value.filter(|value| value.kind() == "object") else {
            continue;
        };
        let mut cursor = object.walk();
        let pairs: Vec<_> = object
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "pair")
            .collect();
        let keys = unique_names(cx, &pairs, |cx, pair| {
            let key = pair.child_by_field_name("key")?;
            (key.kind() != "computed_property_name").then(|| callable::property_name(cx, Some(key)))
        });
        for (pair, key) in pairs.iter().zip(keys) {
            let Some(key) = key else { continue };
            if pair
                .child_by_field_name("value")
                .is_some_and(callable::is_callable)
            {
                continue; // Already a Symbol.
            }
            let member = format!("{name}.{key}");
            let member_id = decl_id(cx, &member);
            push(
                declaration(cx, &member_id, &member, "Property", *pair, |props| {
                    props["declaration"] = serde_json::json!(id);
                }),
                defined_in(cx, &member_id, &file, *pair),
                out,
            );
        }
    }
    for function in syntax_nodes
        .iter()
        .copied()
        .filter(|node| callable::is_callable(*node))
    {
        let sid = callable::id(cx, function);
        let name = callable::name(cx, function);
        for (index, decorator) in decorators(function).into_iter().enumerate() {
            let id = format!("deco:{}@{}#{name}/{index}", cx.id.repo, cx.path);
            let label = decorator_name(cx, decorator);
            let mut node = declaration(cx, &id, &label, "Decorator", decorator, |props| {
                props["target"] = serde_json::json!(sid);
            });
            node.label = "Decorator".into();
            let edge = Edge {
                src: id.clone(),
                dst: sid.clone(),
                label: "DECORATES".into(),
                props: serde_json::json!({ "prov": cx.prov(&decorator, &format!("DECORATES {id}")) }),
            };
            push(node, edge, out);
        }
        let evidence = function
            .parent()
            .filter(|parent| parent.kind() == "variable_declarator")
            .unwrap_or(function);
        let Some(body) = function.child_by_field_name("body") else {
            continue;
        };
        if evidence.end_byte() - evidence.start_byte() <= MAX_CITABLE_SPAN_BYTES
            || body.start_byte() <= evidence.start_byte()
        {
            continue;
        }
        let (start, end) = (evidence.start_byte(), body.start_byte());
        let id = format!("sig:{}@{}#{name}", cx.id.repo, cx.path);
        let fact = format!("Signature {id}");
        let node = Node {
            id: id.clone(),
            label: "Signature".into(),
            props: serde_json::json!({
                "name": name,
                "kind": "Signature",
                "symbol": sid,
                "body_exceeds_range_cap": true,
                "header_exceeds_range_cap": end - start > MAX_CITABLE_SPAN_BYTES,
                "prov": cx.prov_bytes(start, end, &fact),
            }),
        };
        let edge = Edge {
            src: id.clone(),
            dst: sid.clone(),
            label: "SIGNATURE_OF".into(),
            props: serde_json::json!({ "prov": cx.prov_bytes(start, end, &format!("SIGNATURE_OF {id}")) }),
        };
        push(node, edge, out);
    }
}

fn decl_id(cx: &FileCx<'_>, name: &str) -> String {
    format!("decl:{}@{}#{name}", cx.id.repo, cx.path)
}

fn declaration(
    cx: &FileCx<'_>,
    id: &str,
    name: &str,
    kind: &str,
    span: TsNode<'_>,
    extra: impl FnOnce(&mut serde_json::Value),
) -> Node {
    let mut props = serde_json::json!({
        "name": name,
        "kind": kind,
        "prov": cx.prov(&span, &format!("{kind} {id}")),
    });
    extra(&mut props);
    Node {
        id: id.into(),
        label: "Declaration".into(),
        props,
    }
}

fn defined_in(cx: &FileCx<'_>, id: &str, file: &str, span: TsNode<'_>) -> Edge {
    Edge {
        src: id.into(),
        dst: file.into(),
        label: "DEFINED_IN".into(),
        props: serde_json::json!({ "prov": cx.prov(&span, &format!("DEFINED_IN {id}")) }),
    }
}

/// Declarators of `const`/`let`/`var` statements directly in the program,
/// including exported ones, in source order.
fn top_level_declarators(root: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut result = Vec::new();
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        let statement = if statement.kind() == "export_statement" {
            match statement.child_by_field_name("declaration") {
                Some(declaration) => declaration,
                None => continue,
            }
        } else {
            statement
        };
        if matches!(
            statement.kind(),
            "lexical_declaration" | "variable_declaration"
        ) {
            let mut inner = statement.walk();
            result.extend(
                statement
                    .named_children(&mut inner)
                    .filter(|child| child.kind() == "variable_declarator"),
            );
        }
    }
    result
}

/// Names in source order; a repeated name (legal for `var` and duplicate
/// object keys) is position-qualified so every identity stays unique.
fn unique_names<'t>(
    cx: &FileCx<'_>,
    nodes: &[TsNode<'t>],
    name: impl Fn(&FileCx<'_>, TsNode<'t>) -> Option<String>,
) -> Vec<Option<String>> {
    let names: Vec<_> = nodes.iter().map(|node| name(cx, *node)).collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for name in names.iter().flatten() {
        *counts.entry(name).or_default() += 1;
    }
    names
        .iter()
        .zip(nodes)
        .map(|(name, node)| {
            name.as_ref().map(|name| {
                if counts[name.as_str()] > 1 {
                    format!("{name}@{}", node.start_byte())
                } else {
                    name.clone()
                }
            })
        })
        .collect()
}

/// `x as T`, `x satisfies T` and `(x)` keep the literal they wrap.
fn unwrap_value(mut value: TsNode<'_>) -> TsNode<'_> {
    while matches!(
        value.kind(),
        "as_expression" | "satisfies_expression" | "parenthesized_expression"
    ) {
        match value.named_child(0) {
            Some(inner) => value = inner,
            None => break,
        }
    }
    value
}

/// Decorators applied to a method: preceding `class_body` siblings in the
/// TypeScript grammar, or the method's own children where a grammar nests them.
fn decorators(function: TsNode<'_>) -> Vec<TsNode<'_>> {
    if function.kind() != "method_definition" {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut previous = function.prev_named_sibling();
    while let Some(node) = previous.filter(|node| node.kind() == "decorator") {
        result.push(node);
        previous = node.prev_named_sibling();
    }
    result.reverse();
    let mut cursor = function.walk();
    result.extend(
        function
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "decorator"),
    );
    result
}

/// The decorator's callee path (`Allow`, `Nest.Get`), never its arguments.
/// The name is rebuilt from identifier tokens alone, never sliced from source,
/// so comments or a call, computed or parenthesized base never reach the
/// graph; anything but a plain dotted path is position-named instead.
fn decorator_name(cx: &FileCx<'_>, decorator: TsNode<'_>) -> String {
    let mut target = decorator.named_child(0);
    if let Some(call) = target.filter(|node| node.kind() == "call_expression") {
        target = call.child_by_field_name("function");
    }
    target
        .and_then(|node| dotted_path(cx, node))
        .unwrap_or_else(|| format!("decorator@{}", decorator.start_byte()))
}

fn dotted_path(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    match node.kind() {
        "identifier" => Some(cx.text(&node).to_owned()),
        "member_expression" => {
            let property = node
                .child_by_field_name("property")
                .filter(|property| property.kind() == "property_identifier")?;
            let object = dotted_path(cx, node.child_by_field_name("object")?)?;
            Some(format!("{object}.{}", cx.text(&property)))
        }
        _ => None,
    }
}
