//! Direct declaration facts that make non-callable source citable (#693).
//!
//! Top-level variables and the first-level properties of their object-literal
//! initializers (deeper only inside a section too large to inspect whole)
//! become `Declaration` nodes, member decorators become
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
    let mut push = |node: Node, edge: Edge, local: bool, out: &mut Extraction| {
        if let Some(direct) = direct.as_deref_mut() {
            let facts = if local {
                &mut direct.locals
            } else {
                &mut direct.declarations
            };
            facts.insert(node.id.clone(), node.clone());
        }
        out.nodes.push(node);
        out.edges.push(edge);
    };
    let file = file_id(cx.id.repo, cx.path);
    let mut facts = Vec::new();
    variables(cx, &declarators(root), None, &file, &mut facts);
    for (node, edge) in facts.drain(..) {
        push(node, edge, false, out);
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
            push(node, edge, false, out);
        }
        let evidence = function
            .parent()
            .filter(|parent| parent.kind() == "variable_declarator")
            .unwrap_or(function);
        let Some(body) = function.child_by_field_name("body") else {
            continue;
        };
        if !exceeds_cap(evidence) || body.start_byte() <= evidence.start_byte() {
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
        push(node, edge, false, out);
        // Locals of a body too large to inspect whole get their own ranges.
        if body.kind() == "statement_block" {
            variables(
                cx,
                &declarators(body),
                Some((&name, &sid)),
                &file,
                &mut facts,
            );
            for (node, edge) in facts.drain(..) {
                push(node, edge, true, out);
            }
        }
    }
}

/// Variable facts, with object-literal members, for `declarators`. `local`
/// is the enclosing oversized callable's name and Symbol id: it qualifies
/// each name as `{callable}/{name}` and is recorded on the fact.
fn variables(
    cx: &FileCx<'_>,
    declarators: &[TsNode<'_>],
    local: Option<(&str, &str)>,
    file: &str,
    facts: &mut Vec<(Node, Edge)>,
) {
    let names = unique_names(cx, declarators, |cx, declarator| {
        declarator
            .child_by_field_name("name")
            .filter(|name| name.kind() == "identifier")
            .map(|name| cx.text(&name).to_owned())
    });
    for (declarator, name) in declarators.iter().zip(names) {
        let Some(name) = name else { continue };
        let value = declarator.child_by_field_name("value").map(unwrap_value);
        if value.is_some_and(callable::is_callable) {
            continue; // Already a Symbol.
        }
        let name = match local {
            Some((function, _)) => format!("{function}/{name}"),
            None => name,
        };
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
        facts.push((
            declaration(cx, &id, &name, "Variable", *declarator, |props| {
                props["binding"] = serde_json::json!(binding);
                props["exported"] = serde_json::json!(exported);
                if let Some((_, symbol)) = local {
                    props["scope"] = serde_json::json!("local");
                    props["symbol"] = serde_json::json!(symbol);
                }
            }),
            defined_in(cx, &id, file, *declarator),
        ));
        if let Some(object) = value.filter(|value| value.kind() == "object") {
            properties(cx, object, &name, &id, file, facts);
        }
    }
}

/// Nesting levels described below a top-level declarator. Deeper oversized
/// sections stay flagged gaps; the walk is iterative, so hostile nesting can
/// neither exhaust the stack nor multiply work beyond this bound.
pub const MAX_PROPERTY_DEPTH: usize = 4;

/// First-level properties of an object literal. A property whose own span
/// exceeds the inspection cap is flagged, and its object value is described
/// one level deeper (to [`MAX_PROPERTY_DEPTH`]), so every config section
/// reaches a citable range or is a named gap.
fn properties(
    cx: &FileCx<'_>,
    object: TsNode<'_>,
    prefix: &str,
    parent: &str,
    file: &str,
    facts: &mut Vec<(Node, Edge)>,
) {
    let mut pending = vec![(object, prefix.to_owned(), parent.to_owned(), 1)];
    while let Some((object, prefix, parent, depth)) = pending.pop() {
        let mut cursor = object.walk();
        let pairs: Vec<_> = object
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "pair")
            .collect();
        let keys = unique_names(cx, &pairs, |cx, pair| {
            let key = pair.child_by_field_name("key")?;
            (key.kind() != "computed_property_name").then(|| callable::property_name(cx, Some(key)))
        });
        let mut nested = Vec::new();
        for (pair, key) in pairs.iter().zip(keys) {
            let Some(key) = key else { continue };
            let value = pair.child_by_field_name("value").map(unwrap_value);
            if value.is_some_and(callable::is_callable) {
                continue; // Already a Symbol.
            }
            let member = format!("{prefix}.{key}");
            let id = decl_id(cx, &member);
            let object = value
                .filter(|value| value.kind() == "object" && exceeds_cap(*pair))
                .filter(|_| depth < MAX_PROPERTY_DEPTH);
            facts.push((
                declaration(cx, &id, &member, "Property", *pair, |props| {
                    props["declaration"] = serde_json::json!(parent);
                }),
                defined_in(cx, &id, file, *pair),
            ));
            if let Some(object) = object {
                nested.push((object, member, id, depth + 1));
            }
        }
        // Reverse so the stack visits nested sections in source order.
        pending.extend(nested.into_iter().rev());
    }
}

fn exceeds_cap(node: TsNode<'_>) -> bool {
    node.end_byte() - node.start_byte() > MAX_CITABLE_SPAN_BYTES
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
    if exceeds_cap(span) {
        // Receipted, but too large to inspect whole: a named gap that narrower
        // member facts, where they exist, close.
        props["span_exceeds_range_cap"] = serde_json::json!(true);
    }
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

/// Declarators of `const`/`let`/`var` statements directly in `container` (the
/// program or a function body), including exported ones, in source order.
fn declarators(container: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut result = Vec::new();
    let mut cursor = container.walk();
    for statement in container.named_children(&mut cursor) {
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
    // Comments may sit between decorators and the member; the scan stops only
    // at the preceding real member.
    while let Some(node) = previous.filter(|node| matches!(node.kind(), "decorator" | "comment")) {
        if node.kind() == "decorator" {
            result.push(node);
        }
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
