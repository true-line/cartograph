//! TypeORM data model: deterministic T0 `DataEntity` nodes from
//! import-proven `typeorm` decorators, and `REFERENCES` relations between
//! entities.
//!
//! It mirrors the JPA adapter (#239): an `@Entity` class is a `DataEntity`
//! (`storage: "typeorm"`) with its own column properties, and each relation
//! decorator (`@ManyToOne(() => Target)`, …) is a relation field of one
//! `REFERENCES` edge per target entity. Decorators are proven by import from
//! `typeorm`, never by name alone. A relation target resolves to a class in
//! the same file or to the file a relative import names; a target that does
//! not resolve is a `REFERENCES` edge to an explicit Gap, never a same-name
//! guess. A `@ChildEntity` argument is its discriminator value, never a
//! table: the subclass is stored in its parent's table.
//! Inherited columns (a base class's `id`, `createdAt`) are not copied in:
//! the base class states them, and the `extends` clause is recorded.

use super::{
    BTreeMap, Edge, Extraction, FileCx, HashMap, Node, TsNode, decorator_call, leading_decorators,
};

/// The module the decorators must be imported from.
pub(crate) const MODULE: &str = "typeorm";

/// Column decorators, and whether each declares a primary key.
fn column_kind(decorator: &str) -> Option<bool> {
    match decorator {
        "Column" | "CreateDateColumn" | "UpdateDateColumn" | "DeleteDateColumn"
        | "VersionColumn" => Some(false),
        "PrimaryColumn" | "PrimaryGeneratedColumn" | "ObjectIdColumn" => Some(true),
        _ => None,
    }
}

/// Relation decorators and their cardinality.
fn cardinality(decorator: &str) -> Option<&'static str> {
    match decorator {
        "ManyToOne" => Some("many-to-one"),
        "OneToMany" => Some("one-to-many"),
        "OneToOne" => Some("one-to-one"),
        "ManyToMany" => Some("many-to-many"),
        _ => None,
    }
}

/// The id of the `DataEntity` for class `class` declared in `path`.
pub(crate) fn entity_id(repo: &str, path: &str, class: &str) -> String {
    format!("data:{repo}@typeorm:{path}#{class}")
}

/// The call expression of a decorator, if it is a call.
fn decorator_expression<'t>(decorator: TsNode<'t>) -> Option<TsNode<'t>> {
    let mut cursor = decorator.walk();
    let expression = decorator
        .children(&mut cursor)
        .find(|child| child.is_named())?;
    (expression.kind() == "call_expression").then_some(expression)
}

/// The named arguments of a call.
fn arguments<'t>(call: TsNode<'t>) -> Vec<TsNode<'t>> {
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = args.walk();
    args.named_children(&mut cursor).collect()
}

/// `true`/`false` of `key` in any object-literal argument of `call`.
fn boolean_option(cx: &FileCx, call: TsNode, key: &str) -> Option<bool> {
    arguments(call)
        .into_iter()
        .filter(|arg| arg.kind() == "object")
        .find_map(|object| {
            let mut cursor = object.walk();
            object.named_children(&mut cursor).find_map(|pair| {
                if pair.kind() != "pair" {
                    return None;
                }
                let name = pair.child_by_field_name("key")?;
                (cx.text(&name) == key).then_some(())?;
                match cx.text(&pair.child_by_field_name("value")?) {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                }
            })
        })
}

/// The string value of `key` in any object-literal argument of `call`.
fn string_option(cx: &FileCx, call: TsNode, key: &str) -> Option<String> {
    arguments(call)
        .into_iter()
        .filter(|arg| arg.kind() == "object")
        .find_map(|object| {
            let mut cursor = object.walk();
            object.named_children(&mut cursor).find_map(|pair| {
                if pair.kind() != "pair" {
                    return None;
                }
                let name = pair.child_by_field_name("key")?;
                (cx.text(&name) == key).then_some(())?;
                super::literal_string(cx, pair.child_by_field_name("value")?)
            })
        })
}

/// The class a relation names: the body of `() => Target` or
/// `type => Target`, the relation decorator's first argument.
fn relation_target(cx: &FileCx, call: TsNode) -> Option<String> {
    let first = arguments(call).into_iter().next()?;
    if first.kind() != "arrow_function" {
        return None;
    }
    let body = first.child_by_field_name("body")?;
    (body.kind() == "identifier").then(|| cx.text(&body).to_string())
}

/// The type annotation of a property, as written.
fn spelled_type(cx: &FileCx, member: TsNode) -> Option<String> {
    let annotation = member.child_by_field_name("type")?;
    let text = cx.text(&annotation).trim_start_matches(':').trim();
    (!text.is_empty()).then(|| text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The class names declared in this file.
fn local_classes(cx: &FileCx, root: TsNode) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "class_declaration" | "abstract_class_declaration"
        ) && let Some(name) = node.child_by_field_name("name")
        {
            found.push(cx.text(&name).to_string());
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    found
}

/// One relation field, before its edge is built.
struct Relation<'t> {
    /// Empty when the decorator names no class literally.
    target: String,
    field: serde_json::Value,
    site: TsNode<'t>,
}

/// Emit the `DataEntity` and `REFERENCES` facts of `class_node`, if it is an
/// import-proven TypeORM `@Entity`. `from_typeorm` gives the exported name
/// of a decorator local imported from `typeorm` at a site.
pub(crate) fn entity<'t>(
    cx: &FileCx,
    out: &mut Extraction,
    (class_node, body, root): (TsNode<'t>, TsNode<'t>, TsNode<'t>),
    from_typeorm: &dyn Fn(TsNode<'_>, &str) -> Option<String>,
    imported_types: &HashMap<String, (String, String)>,
) {
    let entity = leading_decorators(class_node)
        .into_iter()
        .filter_map(|decorator| {
            let (local, argument, evidence) = decorator_call(cx, decorator)?;
            match from_typeorm(evidence, &local).as_deref() {
                Some("Entity") => Some((argument, None, decorator)),
                Some("ChildEntity") => Some((None, argument, decorator)),
                _ => None,
            }
        })
        .next();
    let Some((explicit_table, discriminator, decorator)) = entity else {
        return;
    };
    let child = discriminator.is_some()
        || decorator_call(cx, decorator).is_some_and(|(local, _, site)| {
            from_typeorm(site, &local).as_deref() == Some("ChildEntity")
        });
    let Some(name) = class_node.child_by_field_name("name") else {
        return;
    };
    let class = cx.text(&name).to_string();
    let id = entity_id(cx.id.repo, cx.path, &class);
    let table = explicit_table.or_else(|| {
        (!child)
            .then(|| {
                decorator_expression(decorator).and_then(|call| string_option(cx, call, "name"))
            })
            .flatten()
    });
    let extends = {
        let mut cursor = class_node.walk();
        class_node
            .named_children(&mut cursor)
            .find(|child| child.kind() == "class_heritage")
            .and_then(|heritage| {
                let mut cursor = heritage.walk();
                heritage
                    .named_children(&mut cursor)
                    .find(|clause| clause.kind() == "extends_clause")
                    .and_then(|clause| clause.child_by_field_name("value"))
                    .map(|value| cx.text(&value).to_string())
            })
    };

    let mut columns = Vec::new();
    let mut relations: Vec<Relation<'t>> = Vec::new();
    let mut evidence = vec![decorator, name];
    let mut members = body.walk();
    for member in body.named_children(&mut members) {
        if member.kind() != "public_field_definition" {
            continue;
        }
        let Some(field_name) = member.child_by_field_name("name") else {
            continue;
        };
        let field_name = cx.text(&field_name).to_string();
        for decorator in leading_decorators(member) {
            let Some((local, _, site)) = decorator_call(cx, decorator) else {
                continue;
            };
            let Some(exported) = from_typeorm(site, &local) else {
                continue;
            };
            let call = decorator_expression(decorator);
            if let Some(by_decorator) = column_kind(&exported) {
                evidence.push(decorator);
                let mut column = serde_json::Map::new();
                column.insert("name".into(), field_name.clone().into());
                if let Some(ty) = spelled_type(cx, member) {
                    column.insert("type".into(), ty.into());
                }
                let mut primary = by_decorator;
                if let Some(call) = call {
                    // `@Column({ primary: true })` declares a key too.
                    primary |= boolean_option(cx, call, "primary") == Some(true);
                    if let Some(db_name) = string_option(cx, call, "name") {
                        column.insert("column".into(), db_name.into());
                    }
                    if let Some(nullable) = boolean_option(cx, call, "nullable") {
                        column.insert("nullable".into(), nullable.into());
                    }
                    if let Some(unique) = boolean_option(cx, call, "unique") {
                        column.insert("unique".into(), unique.into());
                    }
                }
                column.insert("primary_key".into(), primary.into());
                column.insert("decorator".into(), exported.clone().into());
                columns.push(serde_json::Value::Object(column));
            } else if let Some(cardinality) = cardinality(&exported) {
                let target = call
                    .and_then(|call| relation_target(cx, call))
                    .unwrap_or_default();
                // The owning side holds the foreign key or join table.
                let joins = leading_decorators(member).into_iter().any(|other| {
                    decorator_call(cx, other).is_some_and(|(local, _, site)| {
                        matches!(
                            from_typeorm(site, &local).as_deref(),
                            Some("JoinColumn" | "JoinTable")
                        )
                    })
                });
                let owning = match exported.as_str() {
                    "ManyToOne" => true,
                    "OneToMany" => false,
                    _ => joins,
                };
                relations.push(Relation {
                    target: target.clone(),
                    field: serde_json::json!({
                        "field": field_name,
                        "relation": exported,
                        "cardinality": cardinality,
                        "owning": owning,
                        "target": (!target.is_empty()).then_some(target),
                    }),
                    site: member,
                });
            }
        }
    }

    let mut props = serde_json::json!({
        "name": class,
        "storage": "typeorm",
        "columns": columns,
        "framework": "typeorm",
        "language": "typescript",
        "relations": relations.iter().map(|r| r.field.clone()).collect::<Vec<_>>(),
        "prov": cx.prov_spanning(
            &evidence,
            core_prov::ConfidenceTier::Confirmed,
            &format!("DataEntity {id}"),
        ),
    });
    if let Some(table) = table {
        props["table"] = table.clone().into();
        props["store"] = table.into();
    }
    if let Some(extends) = extends {
        props["extends"] = extends.into();
    }
    if let Some(discriminator) = discriminator {
        props["discriminator"] = discriminator.into();
    }
    if child {
        props["inheritance"] = "single-table child".into();
    }
    out.nodes.push(Node {
        id: id.clone(),
        label: "DataEntity".into(),
        props,
    });

    // One REFERENCES edge per target entity, holding every relation field
    // that names it (several relations to one target are one fact).
    let locals = local_classes(cx, root);
    let mut by_target: BTreeMap<String, (Vec<serde_json::Value>, Vec<TsNode<'t>>)> =
        BTreeMap::new();
    for relation in relations {
        let target = if relation.target.is_empty() {
            None
        } else if locals.contains(&relation.target) {
            Some(entity_id(cx.id.repo, cx.path, &relation.target))
        } else {
            imported_types
                .get(&relation.target)
                .map(|(file, exported)| entity_id(cx.id.repo, file, exported))
        };
        let Some(target) = target else {
            unresolved(cx, out, &id, relation);
            continue;
        };
        let entry = by_target.entry(target).or_default();
        entry.0.push(relation.field);
        entry.1.push(relation.site);
    }
    for (target, (fields, sites)) in by_target {
        let fact = format!("REFERENCES {id} -> {target}");
        let first = &fields[0];
        out.edges.push(Edge {
            src: id.clone(),
            dst: target,
            label: "REFERENCES".into(),
            props: serde_json::json!({
                "relation": first["relation"],
                "cardinality": first["cardinality"],
                "field": first["field"],
                "owning": first["owning"],
                "relations": fields,
                "framework": "typeorm",
                "prov": cx.prov_spanning(&sites, core_prov::ConfidenceTier::Confirmed, &fact),
            }),
        });
    }
}

/// The explicit Gap of a relation whose target class is not proven: not a
/// literal `() => Class`, or a class neither declared in this file nor
/// imported by a relative path (a path alias, a package). The relation stays
/// an edge, so the data model shows the hop as unresolved.
fn unresolved(cx: &FileCx, out: &mut Extraction, entity: &str, relation: Relation<'_>) {
    let reason = if relation.target.is_empty() {
        "TypeORM relation target is not a literal class"
    } else {
        "TypeORM relation target is not a recovered entity"
    };
    let gap = format!(
        "gap:typeorm:{}@{}@{}",
        cx.id.repo,
        cx.path,
        relation.site.start_byte()
    );
    let mut props = serde_json::json!({
        "reason": reason,
        "attempted_tiers": ["T0"],
        "framework": "typeorm",
        "language": "typescript",
        "field": relation.field["field"],
        "prov": cx.prov_spanning(
            &[relation.site],
            core_prov::ConfidenceTier::Gap,
            &format!("Gap {gap}"),
        ),
    });
    if !relation.target.is_empty() {
        props["target"] = relation.target.clone().into();
    }
    out.nodes.push(Node {
        id: gap.clone(),
        label: "Gap".into(),
        props,
    });
    let fact = format!("REFERENCES {entity} -> {gap}");
    out.edges.push(Edge {
        src: entity.to_string(),
        dst: gap,
        label: "REFERENCES".into(),
        props: serde_json::json!({
            "relation": relation.field["relation"],
            "cardinality": relation.field["cardinality"],
            "field": relation.field["field"],
            "owning": relation.field["owning"],
            "relations": [relation.field],
            "reason": reason,
            "attempted_resolution": "entity-type",
            "framework": "typeorm",
            "prov": cx.prov_spanning(&[relation.site], core_prov::ConfidenceTier::Gap, &fact),
        }),
    });
}
