//! JPA entities in Kotlin (#2021, AC-1028).
//!
//! A class annotated with an import-proven `jakarta.persistence` or
//! `javax.persistence` `@Entity` is a `DataEntity` in the shape the Java
//! reader gives (`storage: "jpa"`, `data:<repo>@jpa:<name>`): named by a
//! literal `@Entity(name = …)` or else the class name, its table a literal
//! `@Table(name = …)` or else the entity name, and its `fields` the stored
//! properties (`val`/`var` constructor parameters and body properties with
//! a backing field) with their spelled type, `@Id`/`@EmbeddedId` and a
//! literal `@Column(name = …)`. `@Transient` properties are not fields.
//!
//! A relation property (`@OneToOne`, `@ManyToOne`, `@OneToMany`,
//! `@ManyToMany`) is a `REFERENCES` edge to the target entity: the
//! property's type, the element type of a collection for a to-many, or a
//! `targetEntity = X::class`. Targets resolve at the directory join through
//! the shared persistence join ([`super::room`]) by the file's imports or
//! package. A computed entity or table name, an unprovable target and a
//! target that is no recovered entity are Gaps. Spring Data repositories
//! are not read here.

use core_graph::{Edge, Node};
use tree_sitter::Node as TsNode;

use super::rest_template::named_children;
use super::room::{
    self, argument, candidates, class_annotations, framework_gap, proof_in, stored_properties,
};
use super::{
    AnnotationUse, FileCx, Imports, declaration_annotations, enclosing_type_chain,
    pure_string_literal, spelling,
};

const FRAMEWORK: &str = "jpa";
const PACKAGES: &[&str] = &["jakarta.persistence", "javax.persistence"];

/// The cardinality a relation annotation declares, and whether it is a
/// to-many (its type is a collection of the target).
fn cardinality(annotation: &str) -> Option<(&'static str, bool)> {
    Some(match annotation {
        "OneToOne" => ("one-to-one", false),
        "ManyToOne" => ("many-to-one", false),
        "OneToMany" => ("one-to-many", true),
        "ManyToMany" => ("many-to-many", true),
        _ => return None,
    })
}

/// The first of `annotations` proven as JPA's `name`, with the names that
/// would obscure its proof.
fn find<'a, 't>(
    annotations: &'a [AnnotationUse<'t>],
    imports: &Imports,
    package: Option<&str>,
    name: &str,
) -> Option<(&'a AnnotationUse<'t>, Vec<String>)> {
    annotations.iter().find_map(|annotation| {
        proof_in(PACKAGES, annotation, imports, package, name).map(|shadows| (annotation, shadows))
    })
}

/// A string argument: `Ok(None)` when absent, `Err` when present but not a
/// literal (or the arguments cannot be read).
fn literal_argument(
    cx: &FileCx<'_>,
    annotation: TsNode<'_>,
    key: &str,
    positional: bool,
) -> Result<Option<String>, ()> {
    match argument(cx, annotation, key, positional)? {
        None => Ok(None),
        Some(value) => pure_string_literal(cx, value)
            .map(|literal| (!literal.is_empty()).then_some(literal))
            .ok_or(()),
    }
}

/// Whether a property is `@Transient`: JPA's, or Kotlin's own
/// (`kotlin.jvm.Transient`, imported by default).
fn transient(annotations: &[AnnotationUse<'_>], imports: &Imports, package: Option<&str>) -> bool {
    annotations.iter().any(|annotation| {
        annotation.simple == "Transient"
            && (proof_in(PACKAGES, annotation, imports, package, "Transient").is_some()
                || annotation.spelled == "kotlin.jvm.Transient"
                || (annotation.spelled == "Transient"
                    && imports
                        .bindings
                        .get("Transient")
                        .is_none_or(|bound| bound == "kotlin.jvm.Transient")))
    })
}

/// A property's declared type node.
fn type_node<'t>(declaration: TsNode<'t>) -> Option<TsNode<'t>> {
    let is_type = |node: &TsNode<'_>| {
        matches!(
            node.kind(),
            "user_type" | "nullable_type" | "function_type" | "parenthesized_type"
        )
    };
    let parts = named_children(declaration);
    if let Some(ty) = parts.iter().copied().find(is_type) {
        return Some(ty);
    }
    parts
        .into_iter()
        .find(|part| part.kind() == "variable_declaration")
        .and_then(|variable| named_children(variable).into_iter().find(is_type))
}

/// A type spelling without nullability or whitespace.
fn type_text(cx: &FileCx<'_>, ty: TsNode<'_>) -> String {
    spelling(cx.text(&ty)).trim_end_matches('?').to_string()
}

/// The class a type spelling names, without type arguments; for a to-many,
/// its last type argument's.
fn target_of(spelled: &str, to_many: bool) -> Option<String> {
    let spelled = spelled.trim_end_matches('?');
    let named = if to_many {
        let open = spelled.find('<')?;
        let inner = spelled[open + 1..].strip_suffix('>')?;
        inner.rsplit(',').next()?.trim().trim_end_matches('?')
    } else {
        spelled
    };
    let named = named.split('<').next()?.trim();
    let valid = !named.is_empty()
        && named
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_alphanumeric() || c == '_'));
    valid.then(|| named.to_string())
}

/// This file's JPA entities and relations, into the shared persistence
/// join.
pub(crate) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    pending: &mut room::Pending,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    if !cx.text(&root).contains("persistence") {
        return;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if node.kind() == "class_declaration" {
            entity(cx, node, package, imports, pending, nodes, edges);
        }
    }
}

fn entity(
    cx: &FileCx<'_>,
    class: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    pending: &mut room::Pending,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let Some(name) = class.child_by_field_name("name") else {
        return;
    };
    let annotations = class_annotations(cx, class);
    let Some((entity, shadows)) = find(&annotations, imports, package, "Entity") else {
        return;
    };
    let simple = cx.text(&name).to_string();
    let mut chain = enclosing_type_chain(cx, class);
    chain.push(simple.clone());
    let qualified = chain.join(".");
    let fqn = match package {
        Some(package) => format!("{package}.{qualified}"),
        None => qualified.clone(),
    };
    let entity_name = match literal_argument(cx, entity.node, "name", false) {
        Ok(Some(entity_name)) => entity_name,
        Ok(None) => simple,
        Err(()) => {
            let class_symbol = super::symbol_id(cx.id.repo, cx.path, &qualified);
            let (gap, edge) = framework_gap(
                cx,
                FRAMEWORK,
                entity.node,
                &class_symbol,
                "REFERENCES",
                "runtime-computed JPA entity name",
            );
            nodes.push(gap);
            edges.push(edge);
            return;
        }
    };
    let id = format!("data:{}@jpa:{entity_name}", cx.id.repo);

    let mut table = Some(entity_name.clone());
    let mut table_source = "default";
    let mut schema = None;
    if let Some((annotation, _)) = find(&annotations, imports, package, "Table") {
        schema = literal_argument(cx, annotation.node, "schema", false)
            .ok()
            .flatten();
        match literal_argument(cx, annotation.node, "name", false) {
            Ok(Some(explicit)) => {
                table = Some(explicit);
                table_source = "explicit";
            }
            Ok(None) => {}
            Err(()) => {
                table = None;
                table_source = "unresolved";
                let (gap, edge) = framework_gap(
                    cx,
                    FRAMEWORK,
                    annotation.node,
                    &id,
                    "REFERENCES",
                    "runtime-computed JPA table name",
                );
                nodes.push(gap);
                edges.push(edge);
            }
        }
    }

    let mut fields = Vec::new();
    for (declaration, field_name) in stored_properties(cx, class) {
        let field_annotations = declaration_annotations(cx, declaration);
        if transient(&field_annotations, imports, package) {
            continue;
        }
        let field_name = cx.text(&field_name).to_string();
        let spelled = type_node(declaration).map(|ty| type_text(cx, ty));
        let is_id = ["Id", "EmbeddedId"]
            .iter()
            .any(|marker| find(&field_annotations, imports, package, marker).is_some());
        let mut field = serde_json::json!({ "name": field_name, "id": is_id });
        if let Some(spelled) = &spelled {
            field["type"] = spelled.clone().into();
        }
        if let Some((column, _)) = find(&field_annotations, imports, package, "Column")
            && let Ok(Some(column)) = literal_argument(cx, column.node, "name", false)
        {
            field["column"] = column.into();
        }
        let relation = ["OneToOne", "ManyToOne", "OneToMany", "ManyToMany"]
            .iter()
            .find_map(|marker| find(&field_annotations, imports, package, marker));
        if let Some((annotation, relation_shadows)) = relation {
            field["relation"] = annotation.simple.clone().into();
            relate(
                cx,
                declaration,
                annotation,
                relation_shadows,
                &field_annotations,
                &field_name,
                spelled.as_deref(),
                &id,
                imports,
                package,
                pending,
                nodes,
                edges,
            );
        }
        fields.push(field);
    }

    let mut props = serde_json::json!({
        "name": entity_name,
        "storage": "jpa",
        "table_source": table_source,
        "fqn": fqn,
        "class": fqn,
        "fields": fields,
        "framework": FRAMEWORK,
        "language": "kotlin",
        "resolution": "jpa-entity",
        "prov": cx.prov(&class, &format!("DataEntity {id}")),
    });
    if let Some(table) = table {
        props["table"] = table.clone().into();
        props["store"] = table.into();
    }
    if let Some(schema) = schema {
        props["schema"] = schema.into();
    }
    pending.push_entity(
        fqn,
        cx.path,
        shadows,
        Node {
            id,
            label: "DataEntity".into(),
            props,
        },
    );
}

/// A relation property's `REFERENCES` edge, queued for the join, or its Gap.
#[allow(clippy::too_many_arguments)]
fn relate(
    cx: &FileCx<'_>,
    declaration: TsNode<'_>,
    annotation: &AnnotationUse<'_>,
    shadows: Vec<String>,
    field_annotations: &[AnnotationUse<'_>],
    field_name: &str,
    spelled: Option<&str>,
    id: &str,
    imports: &Imports,
    package: Option<&str>,
    pending: &mut room::Pending,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let (cardinality_label, to_many) = cardinality(&annotation.simple).expect("a relation marker");
    let target = match argument(cx, annotation.node, "targetEntity", false) {
        Ok(Some(value)) => cx
            .text(&value)
            .strip_suffix("::class")
            .and_then(|named| target_of(&spelling(named), false)),
        Ok(None) => spelled.and_then(|spelled| target_of(spelled, to_many)),
        Err(()) => None,
    };
    let Some(target) = target else {
        let (gap, edge) = framework_gap(
            cx,
            FRAMEWORK,
            declaration,
            id,
            "REFERENCES",
            "JPA relation target type is not provable",
        );
        nodes.push(gap);
        edges.push(edge);
        return;
    };
    let mapped_by = literal_argument(cx, annotation.node, "mappedBy", false);
    let mut props = serde_json::json!({
        "relation": annotation.simple,
        "cardinality": cardinality_label,
        "field": field_name,
        "owning": matches!(mapped_by, Ok(None)),
        "framework": FRAMEWORK,
        "prov": cx.prov(&declaration, &format!("REFERENCES {id} .{field_name}")),
    });
    if let Ok(Some(mapped_by)) = mapped_by {
        props["mapped_by"] = mapped_by.into();
    }
    for (marker, key) in [("JoinColumn", "join_column"), ("JoinTable", "join_table")] {
        if let Some((join, _)) = find(field_annotations, imports, package, marker)
            && let Ok(Some(value)) = literal_argument(cx, join.node, "name", false)
        {
            props[key] = value.into();
        }
    }
    let gap = framework_gap(
        cx,
        FRAMEWORK,
        declaration,
        id,
        "REFERENCES",
        &format!("JPA relation target `{target}` is not a recovered entity"),
    );
    pending.push_class_access(
        cx.path,
        shadows,
        id,
        candidates(&target, imports, package),
        "REFERENCES",
        props,
        gap,
    );
}

#[cfg(test)]
mod tests {
    use super::target_of;

    #[test]
    fn relation_targets_from_type_spellings() {
        assert_eq!(target_of("Customer?", false).as_deref(), Some("Customer"));
        assert_eq!(target_of("List<Order>", true).as_deref(), Some("Order"));
        assert_eq!(
            target_of("MutableMap<String,com.x.Tag?>", true).as_deref(),
            Some("com.x.Tag")
        );
        assert_eq!(target_of("List<Order>", false).as_deref(), Some("List"));
        assert_eq!(target_of("Order", true), None);
        assert_eq!(target_of("(Int)->Unit", false), None);
    }
}
