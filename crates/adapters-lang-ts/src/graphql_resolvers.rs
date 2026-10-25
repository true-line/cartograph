//! NestJS GraphQL resolver operations (#968, AC-0347).
//!
//! A method decorated `@Query()` or `@Mutation()` in a class decorated
//! `@Resolver()`, all three import-proven from `@nestjs/graphql`, becomes an
//! `Endpoint` (`protocol: "graphql"`) that `HANDLES` the method. The field is
//! the method name unless the decorator names it with a literal. The API the
//! resolver is mounted on is not visible in the class, so it is read from
//! the directory below the nearest `resolvers/` segment and the endpoint is
//! InferredStrong with that assumption recorded. Field resolvers
//! (`@ResolveField`) are not operations and are not emitted.

use crate::{FileCx, callable, literal_string, object_entries};
use core_graph::{Edge, Node};
use core_prov::ConfidenceTier;
use tree_sitter::Node as TsNode;

/// The NestJS GraphQL package that proves the decorators.
pub(crate) const MODULE: &str = "@nestjs/graphql";
/// Gap reason when the decorator names its field with a computed value.
pub const COMPUTED_FIELD_REASON: &str = "GraphQL operation field name is computed";
/// The API scope used when no `resolvers/<api>/` directory names one.
pub const DEFAULT_API: &str = "default";

/// The operation kinds emitted, by exported decorator name.
fn kind(decorator: &str) -> Option<&'static str> {
    match decorator {
        "Query" => Some("query"),
        "Mutation" => Some("mutation"),
        _ => None,
    }
}

/// The directory below the last `resolvers` segment, when the file is nested
/// under one (`src/api/resolvers/admin/x.resolver.ts` → `admin`).
pub(crate) fn api_scope(path: &str) -> Option<&str> {
    let segments: Vec<&str> = path.split('/').collect();
    let at = segments.iter().rposition(|s| *s == "resolvers")?;
    // The segment after `resolvers` must be a directory, not the file.
    (at + 2 < segments.len()).then(|| segments[at + 1])
}

enum Field {
    Named(String),
    Computed,
}

/// The field a `@Query(...)`/`@Mutation(...)` decorator names: a literal
/// first argument, or a literal `name` in its options object, else the
/// method name. A type thunk (`() => T`) first argument names nothing.
fn field(cx: &FileCx<'_>, call: Option<TsNode<'_>>, method: &str) -> Field {
    let Some(call) = call else {
        return Field::Named(method.into());
    };
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Field::Named(method.into());
    };
    let mut walk = arguments.walk();
    let arguments: Vec<_> = arguments
        .named_children(&mut walk)
        .filter(|n| n.kind() != "comment")
        .collect();
    let options = match arguments.first() {
        None => return Field::Named(method.into()),
        Some(first) if matches!(first.kind(), "string" | "template_string") => {
            return literal_string(cx, *first).map_or(Field::Computed, Field::Named);
        }
        Some(first) if matches!(first.kind(), "arrow_function" | "function_expression") => {
            arguments.get(1).copied()
        }
        Some(first) => Some(*first),
    };
    let Some(options) = options else {
        return Field::Named(method.into());
    };
    if options.kind() != "object" {
        return Field::Computed;
    }
    match object_entries(cx, options)
        .into_iter()
        .find(|(key, _)| key == "name")
    {
        None => Field::Named(method.into()),
        Some((_, value)) => literal_string(cx, value).map_or(Field::Computed, Field::Named),
    }
}

/// `@Allow(Permission.A, Permission.B)` → `["A", "B"]` when every argument
/// is a `Permission.<Ident>` member; otherwise nothing is recorded.
fn permissions(cx: &FileCx<'_>, call: TsNode<'_>) -> Option<Vec<String>> {
    let arguments = call.child_by_field_name("arguments")?;
    let mut walk = arguments.walk();
    let mut out = Vec::new();
    for argument in arguments.named_children(&mut walk) {
        if argument.kind() == "comment" {
            continue;
        }
        let object = argument.child_by_field_name("object")?;
        let property = argument.child_by_field_name("property")?;
        if argument.kind() != "member_expression" || cx.text(&object) != "Permission" {
            return None;
        }
        out.push(cx.text(&property).to_string());
    }
    (!out.is_empty()).then_some(out)
}

/// The decorator's callee identifier and its call expression, if called.
fn decorator_parts<'tree>(
    cx: &FileCx<'_>,
    decorator: TsNode<'tree>,
) -> Option<(String, Option<TsNode<'tree>>)> {
    let mut walk = decorator.walk();
    let expression = decorator.named_children(&mut walk).next()?;
    match expression.kind() {
        "identifier" => Some((cx.text(&expression).to_string(), None)),
        "call_expression" => {
            let function = expression.child_by_field_name("function")?;
            (function.kind() == "identifier")
                .then(|| (cx.text(&function).to_string(), Some(expression)))
        }
        _ => None,
    }
}

/// Emit the operations of one class body whose class is a proven resolver.
/// `exported` maps a decorator's local binding at a site to the name it was
/// imported as from [`MODULE`] (`None` when it is not import-proven from it),
/// and `allow` does the same for a decorator exported as `Allow` from any
/// module.
pub(crate) fn operations(
    cx: &FileCx<'_>,
    out: &mut crate::Extraction,
    body: TsNode<'_>,
    exported: &dyn Fn(TsNode<'_>, &str) -> Option<String>,
    allow: &dyn Fn(TsNode<'_>, &str) -> bool,
) {
    let api = api_scope(cx.path);
    let mut walk = body.walk();
    for method in body
        .children(&mut walk)
        .filter(|child| child.kind() == "method_definition")
    {
        let Some(name) = method
            .child_by_field_name("name")
            .map(|n| cx.text(&n).to_string())
        else {
            continue;
        };
        let decorators = crate::leading_decorators(method);
        let granted = decorators.iter().find_map(|decorator| {
            let (local, call) = decorator_parts(cx, *decorator)?;
            allow(*decorator, &local).then_some(())?;
            permissions(cx, call?)
        });
        for decorator in &decorators {
            let Some((local, call)) = decorator_parts(cx, *decorator) else {
                continue;
            };
            let Some(kind) = exported(*decorator, &local).as_deref().and_then(kind) else {
                continue;
            };
            let api = api.unwrap_or(DEFAULT_API);
            let field = match field(cx, call, &name) {
                Field::Named(field) => field,
                Field::Computed => {
                    let id = format!(
                        "gap:graphql-field:{}@{}@{}",
                        cx.id.repo,
                        cx.path,
                        decorator.start_byte()
                    );
                    out.nodes.push(Node {
                        id: id.clone(),
                        label: "Gap".into(),
                        props: serde_json::json!({
                            "kind": "graphql-field-computed",
                            "reason": COMPUTED_FIELD_REASON,
                            "operation": kind,
                            "attempted_tiers": ["T0"],
                            "prov": cx.prov_with_confidence(
                                decorator,
                                ConfidenceTier::Gap,
                                &format!("Gap {id}"),
                            ),
                        }),
                    });
                    out.edges.push(Edge {
                        src: callable::id(cx, method),
                        dst: id.clone(),
                        label: "DEPENDS_ON".into(),
                        props: serde_json::json!({
                            "reason": COMPUTED_FIELD_REASON,
                            "prov": cx.prov_with_confidence(
                                decorator,
                                ConfidenceTier::Gap,
                                &format!("DEPENDS_ON {id}"),
                            ),
                        }),
                    });
                    continue;
                }
            };
            let ep = format!("ep:{}@graphql:{api}:{kind}:{field}", cx.id.repo);
            let mut props = serde_json::json!({
                "method": kind.to_ascii_uppercase(),
                "path": format!("{api}.{field}"),
                "protocol": "graphql",
                "framework": "nestjs-graphql",
                "operation": kind,
                "field": field,
                "api": api,
                "scope_basis": if api == DEFAULT_API { "none" } else { "resolver_directory" },
                "prov": cx.prov_with_confidence(
                    decorator,
                    ConfidenceTier::InferredStrong,
                    &format!("Endpoint graphql {api} {kind} {field}"),
                ),
            });
            if let Some(granted) = &granted {
                props["permissions"] = serde_json::json!(granted);
            }
            out.nodes.push(Node {
                id: ep.clone(),
                label: "Endpoint".into(),
                props,
            });
            out.edges.push(Edge {
                src: ep,
                dst: callable::id(cx, method),
                label: "HANDLES".into(),
                props: serde_json::json!({
                    "prov": cx.prov_with_confidence(
                        decorator,
                        ConfidenceTier::InferredStrong,
                        &format!("HANDLES graphql {api} {kind} {field}"),
                    ),
                }),
            });
        }
    }
}
