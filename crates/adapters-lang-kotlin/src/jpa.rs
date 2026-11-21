//! JPA entities in Kotlin (#2021, AC-1028).
//!
//! A class annotated with an import-proven `jakarta.persistence` or
//! `javax.persistence` `@Entity` is a `DataEntity` in the shape the Java
//! reader gives (`storage: "jpa"`, `data:<repo>@jpa:<name>`): named by a
//! literal `@Entity(name = …)` or else the class name, its table a literal
//! `@Table(name = …)` or else the entity name, and its `fields` the stored
//! properties (`val`/`var` constructor parameters and body properties with
//! a backing field) with their spelled type, `@Id`/`@EmbeddedId` and a
//! literal `@Column(name = …)`. `@Transient` properties are not fields:
//! JPA's, or Kotlin's default-imported one unless a same-package
//! `Transient` obscures it.
//!
//! A relation property (`@OneToOne`, `@ManyToOne`, `@OneToMany`,
//! `@ManyToMany`) is a `REFERENCES` edge to the target entity: the
//! property's type, the element type of a collection for a to-many, or a
//! `targetEntity = X::class`. Targets resolve at the directory join through
//! the shared persistence join ([`super::room`]) by the file's imports or
//! package, among JPA entities only. A computed entity or table name, an
//! unprovable target, a target that is no recovered entity and an entity
//! name two classes declare are Gaps.
//!
//! A Spring Data repository is an interface extending an import-proven
//! `Repository`, `CrudRepository`, `ListCrudRepository`,
//! `PagingAndSortingRepository`, `ListPagingAndSortingRepository` or
//! `JpaRepository`: a `REFERENCES` edge (relation `repository`, framework
//! `spring-data`) from the interface Symbol to the JPA entity its first type
//! argument names, or a Gap when that is no recovered entity (AC-1100). A
//! call of a Spring Data vocabulary method (`find…`, `get…`, `count…`,
//! `exists…`, `save…`, `delete…`, …) on a receiver whose declaration states
//! a repository type is a `READS` or `WRITES` edge from the calling function
//! to that entity; any other receiver asserts nothing.

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
const SPRING_DATA: &str = "spring-data";
const PACKAGES: &[&str] = &["jakarta.persistence", "javax.persistence"];

/// Spring Data repository base interfaces and the package that declares each.
fn repository_package(name: &str) -> Option<&'static str> {
    match name {
        "Repository"
        | "CrudRepository"
        | "ListCrudRepository"
        | "PagingAndSortingRepository"
        | "ListPagingAndSortingRepository" => Some("org.springframework.data.repository"),
        "JpaRepository" => Some("org.springframework.data.jpa.repository"),
        _ => None,
    }
}

/// Spring Data method-name vocabulary: `Some(false)` reads, `Some(true)`
/// writes. The verb must end the name or be followed by an uppercase letter
/// or digit (`findAll`, `findByName`, `deleteById`; not `savings`), as in
/// the Java reader.
pub(crate) fn classify(method: &str) -> Option<bool> {
    const READS: &[&str] = &[
        "find", "get", "read", "query", "search", "stream", "count", "exists",
    ];
    const WRITES: &[&str] = &["save", "delete", "remove"];
    let boundary = |verb: &str| {
        method.strip_prefix(verb).is_some_and(|rest| {
            rest.chars()
                .next()
                .is_none_or(|c| c.is_uppercase() || c.is_ascii_digit())
        })
    };
    if READS.iter().any(|verb| boundary(verb)) {
        Some(false)
    } else if WRITES.iter().any(|verb| boundary(verb)) {
        Some(true)
    } else {
        None
    }
}

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

/// How a property's `@Transient` reads.
enum Transience {
    /// No `@Transient`: a field.
    Persistent,
    /// JPA's or an explicit `kotlin.jvm.Transient`: not a field.
    Transient,
    /// An unqualified, unimported `@Transient`: Kotlin's default-imported
    /// one, unless a same-package `Transient` (this FQN) obscures it.
    UnlessObscured(String),
}

fn transience(
    annotations: &[AnnotationUse<'_>],
    imports: &Imports,
    package: Option<&str>,
) -> Transience {
    let mut found = Transience::Persistent;
    for annotation in annotations {
        if annotation.simple != "Transient" {
            continue;
        }
        if proof_in(PACKAGES, annotation, imports, package, "Transient")
            .is_some_and(|shadows| shadows.is_empty())
            || annotation.spelled == "kotlin.jvm.Transient"
            || imports.bindings.get("Transient").map(String::as_str) == Some("kotlin.jvm.Transient")
        {
            return Transience::Transient;
        }
        if annotation.spelled == "Transient" && !imports.bindings.contains_key("Transient") {
            found = Transience::UnlessObscured(match package {
                Some(package) => format!("{package}.Transient"),
                None => "Transient".to_string(),
            });
        }
    }
    found
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
    let text = cx.text(&root);
    let entities = text.contains("persistence");
    let repositories = text.contains("springframework.data");
    if !entities && !repositories {
        return;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if node.kind() != "class_declaration" {
            continue;
        }
        if entities {
            entity(cx, node, package, imports, pending, nodes, edges);
        }
        if repositories {
            repository(cx, node, package, imports, pending);
        }
    }
}

/// Whether a declaration is an `interface` (the grammar's anonymous token).
fn is_interface(declaration: TsNode<'_>) -> bool {
    let mut walk = declaration.walk();
    declaration
        .children(&mut walk)
        .any(|part| part.kind() == "interface")
}

/// A Spring Data repository interface's entity binding, into the join.
fn repository(
    cx: &FileCx<'_>,
    interface: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    pending: &mut room::Pending,
) {
    if !is_interface(interface) {
        return;
    }
    let Some(name) = interface.child_by_field_name("name") else {
        return;
    };
    let Some(specifiers) = named_children(interface)
        .into_iter()
        .find(|child| child.kind() == "delegation_specifiers")
    else {
        return;
    };
    for specifier in named_children(specifiers) {
        let Some(ty) = specifier
            .named_child(0)
            .filter(|first| first.kind() == "user_type")
        else {
            continue;
        };
        let Some(spelled) = super::receivers::declared_type(cx, ty) else {
            continue;
        };
        let (prefix, simple) = match spelled.rsplit_once('.') {
            Some((prefix, simple)) => (Some(prefix), simple.to_string()),
            None => (None, spelled.clone()),
        };
        // A qualified spelling or a named (possibly aliased) import proves
        // the base directly; a star import only unless obscured.
        let shadows = match prefix {
            Some(prefix) => match repository_package(&simple) {
                Some(declaring) if declaring == prefix => Vec::new(),
                _ => continue,
            },
            None => match imports.bindings.get(&simple) {
                Some(bound) => {
                    let Some((bound_package, bound_simple)) = bound.rsplit_once('.') else {
                        continue;
                    };
                    match repository_package(bound_simple) {
                        Some(declaring) if declaring == bound_package => Vec::new(),
                        _ => continue,
                    }
                }
                None => {
                    let Some(declaring) = repository_package(&simple) else {
                        continue;
                    };
                    if !imports.wildcard_packages.iter().any(|w| w == declaring) {
                        continue;
                    }
                    let shadow = match package {
                        Some(package) => format!("{package}.{simple}"),
                        None => simple.clone(),
                    };
                    vec![shadow]
                }
            },
        };
        // The first type argument is the managed entity type.
        let Some(argument) = named_children(ty)
            .into_iter()
            .find(|part| part.kind() == "type_arguments")
            .and_then(|arguments| named_children(arguments).into_iter().next())
            .and_then(|projection| {
                named_children(projection)
                    .into_iter()
                    .find(|part| matches!(part.kind(), "user_type" | "nullable_type"))
            })
        else {
            continue;
        };
        let Some(target) = super::receivers::declared_type(cx, argument) else {
            continue;
        };
        let head = target.split('.').next().unwrap_or(&target);
        if super::receivers::declares_type_parameter(cx, interface, head) {
            // A generic base repository's type variable names no entity.
            continue;
        }
        let mut chain = enclosing_type_chain(cx, interface);
        chain.push(cx.text(&name).to_string());
        let qualified = chain.join(".");
        let fqn = match package {
            Some(package) => format!("{package}.{qualified}"),
            None => qualified.clone(),
        };
        let symbol = super::symbol_id(cx.id.repo, cx.path, &qualified);
        let props = serde_json::json!({
            "relation": "repository",
            "framework": SPRING_DATA,
            "repository": simple,
            "entity_type": target,
            "language": "kotlin",
            "prov": cx.prov(&specifier, &format!("REFERENCES {symbol} -> {target}")),
        });
        let gap = framework_gap(
            cx,
            SPRING_DATA,
            specifier,
            &symbol,
            "REFERENCES",
            &format!("Spring Data repository entity `{target}` is not a recovered JPA entity"),
        );
        pending.push_repository(
            fqn,
            cx.path,
            shadows,
            symbol,
            candidates(&target, imports, package),
            props,
            gap,
        );
        // One entity binding per repository interface.
        return;
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
        let transience = transience(&field_annotations, imports, package);
        if matches!(transience, Transience::Transient) {
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
        if let Transience::UnlessObscured(shadow) = transience {
            // Kept only if the join finds a same-package `Transient`; a
            // property that is probably transient asserts no relation.
            field["unless_obscured"] = shadow.into();
        } else if let Some((annotation, relation_shadows)) = relation {
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
    let duplicate_prov = cx.prov_with_confidence(
        &class,
        core_prov::ConfidenceTier::Gap,
        &format!("Gap duplicate {id}"),
    );
    pending.push_jpa_entity(
        fqn,
        cx.path,
        shadows,
        Node {
            id,
            label: "DataEntity".into(),
            props,
        },
        duplicate_prov,
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
    pending.push_jpa_reference(
        cx.path,
        shadows,
        id,
        candidates(&target, imports, package),
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
