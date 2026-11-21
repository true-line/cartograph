//! Android Room persistence (#2021, AC-1025).
//!
//! An import-proven `@Entity` class is a `DataEntity` table (`storage:
//! "sql"`, framework `room`): its table is the `tableName` literal, else
//! the class name, and its columns are the constructor and body
//! properties, named by a literal `@ColumnInfo(name = …)` where one is
//! given (`@Ignore`, `@Embedded` and `@Relation` properties are not
//! columns). In an import-proven `@Dao` type:
//!
//! - a function's `@Query` literal is read as SQL by the shared reader
//!   ([`adapters_fw::sql`]), each table it touches a `READS` or `WRITES`
//!   edge from the function Symbol (InferredStrong, resolution
//!   `sql-literal`);
//! - `@Insert`, `@Update`, `@Delete` and `@Upsert` write the entity their
//!   first parameter's type names (through `List`, `Set`, `Collection`,
//!   `Iterable`, `Array` or `vararg`), a Confirmed `WRITES` edge
//!   (resolution `room-entity-parameter`).
//!
//! The table a query names, and the class a parameter names, resolve at
//! the directory join against the repository's `@Entity` classes. A query
//! that is not one pure string literal, one that names a computed table,
//! a table or class no entity declares, a computed `tableName`, and a
//! write annotation with an `entity =` argument are Gaps (R-INT-4). A
//! star-imported Room annotation that a same-package declaration of that
//! name obscures proves nothing, and its facts become Gaps at the join.

use adapters_fw::sql::{entity_id, parse_with_gaps};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

use super::rest_template::named_children;
use super::{
    AnnotationUse, FileCx, Imports, ShadowName, declaration_annotations, enclosing_type_chain,
    preceding_expression_annotations, pure_string_literal, retarget_props_commit,
};

/// The Room annotation packages: AndroidX and the pre-AndroidX one.
const PACKAGES: &[&str] = &["androidx.room", "android.arch.persistence.room"];
const FRAMEWORK: &str = "room";

/// An `@Entity` class awaiting the join.
#[derive(Debug, Clone)]
pub(crate) struct Entity {
    fqn: String,
    path: String,
    /// Same-package names that would obscure the star-imported proof.
    shadows: Vec<String>,
    node: Node,
    /// The join index this entity lives in: `sql` (Room and Exposed tables)
    /// or `jpa`. A lookup never crosses indexes.
    storage: &'static str,
    /// A JPA entity's provenance for the Gap that replaces it when another
    /// class declares the same entity name (built here: the join cannot
    /// recompute a span).
    duplicate_prov: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
enum Target {
    /// A table a query names, folded to lower case.
    Table(String),
    /// The FQNs a parameter's type name can denote.
    Class(Vec<String>),
}

/// A DAO function's read or write awaiting the join.
#[derive(Debug, Clone)]
pub(crate) struct Access {
    path: String,
    shadows: Vec<String>,
    src: String,
    target: Target,
    label: &'static str,
    props: serde_json::Value,
    gap: (Node, Edge),
    /// The entity index the target is looked up in.
    storage: &'static str,
}

/// A Spring Data repository interface's managed-entity binding awaiting
/// the join (AC-1100).
#[derive(Debug, Clone)]
pub(crate) struct Repository {
    /// The interface's FQN, as call receivers name it.
    fqn: String,
    path: String,
    /// Same-package names that would obscure a star-imported base.
    shadows: Vec<String>,
    /// The interface Symbol.
    src: String,
    /// The FQNs the entity type argument can denote.
    entity: Vec<String>,
    props: serde_json::Value,
    gap: (Node, Edge),
}

/// A Spring Data vocabulary call on a receiver whose declared type is named
/// at the call site (AC-1100).
#[derive(Debug, Clone)]
pub(crate) struct RepositoryCall {
    src: String,
    /// The calling file, for which same-package declarations are visible.
    path: String,
    /// The FQNs the receiver's declared type can denote.
    candidates: Vec<String>,
    label: &'static str,
    props: serde_json::Value,
}

/// One file's Room facts awaiting the directory join.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    entities: Vec<Entity>,
    accesses: Vec<Access>,
    repositories: Vec<Repository>,
    repository_calls: Vec<RepositoryCall>,
}

impl Pending {
    pub(crate) fn extend(&mut self, other: Pending) {
        self.entities.extend(other.entities);
        self.accesses.extend(other.accesses);
        self.repositories.extend(other.repositories);
        self.repository_calls.extend(other.repository_calls);
    }

    /// A Spring Data repository interface `src` (FQN `fqn`) managing the
    /// JPA entity one of `entity` declares, or `gap` when none resolves.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_repository(
        &mut self,
        fqn: String,
        path: &str,
        shadows: Vec<String>,
        src: String,
        entity: Vec<String>,
        props: serde_json::Value,
        gap: (Node, Edge),
    ) {
        self.repositories.push(Repository {
            fqn,
            path: path.to_string(),
            shadows,
            src,
            entity,
            props,
            gap,
        });
    }

    /// A read or write through a receiver declared as one of `candidates`;
    /// it asserts nothing unless exactly one is a repository whose entity
    /// resolves.
    pub(crate) fn push_repository_call(
        &mut self,
        path: &str,
        src: String,
        candidates: Vec<String>,
        writes: bool,
        props: serde_json::Value,
    ) {
        self.repository_calls.push(RepositoryCall {
            src,
            path: path.to_string(),
            candidates,
            label: if writes { "WRITES" } else { "READS" },
            props,
        });
    }

    /// A table another persistence reader recovered (Exposed, AC-1027),
    /// joined with Room's.
    pub(crate) fn push_entity(
        &mut self,
        fqn: String,
        path: &str,
        shadows: Vec<String>,
        node: Node,
    ) {
        self.entities.push(Entity {
            fqn,
            path: path.to_string(),
            shadows,
            node,
            storage: "sql",
            duplicate_prov: None,
        });
    }

    /// A JPA entity (AC-1028), joined only with JPA entities.
    pub(crate) fn push_jpa_entity(
        &mut self,
        fqn: String,
        path: &str,
        shadows: Vec<String>,
        node: Node,
        duplicate_prov: serde_json::Value,
    ) {
        self.entities.push(Entity {
            fqn,
            path: path.to_string(),
            shadows,
            node,
            storage: "jpa",
            duplicate_prov: Some(duplicate_prov),
        });
    }

    /// A JPA relation from entity `src` to the JPA entity one of
    /// `candidates` declares, or `gap` when none resolves.
    pub(crate) fn push_jpa_reference(
        &mut self,
        path: &str,
        shadows: Vec<String>,
        src: &str,
        candidates: Vec<String>,
        props: serde_json::Value,
        gap: (Node, Edge),
    ) {
        self.accesses.push(Access {
            path: path.to_string(),
            shadows,
            src: src.to_string(),
            target: Target::Class(candidates),
            label: "REFERENCES",
            props,
            gap,
            storage: "jpa",
        });
    }

    /// A read or write of the table declared by one of `candidates`, or
    /// `gap` when none resolves.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_class_access(
        &mut self,
        path: &str,
        shadows: Vec<String>,
        src: &str,
        candidates: Vec<String>,
        label: &'static str,
        props: serde_json::Value,
        gap: (Node, Edge),
    ) {
        self.accesses.push(Access {
            path: path.to_string(),
            shadows,
            src: src.to_string(),
            target: Target::Class(candidates),
            label,
            props,
            gap,
            storage: "sql",
        });
    }

    pub(crate) fn retarget(&mut self, commit: &str) {
        for entity in &mut self.entities {
            retarget_props_commit(&mut entity.node.props, commit);
        }
        for access in &mut self.accesses {
            retarget_props_commit(&mut access.props, commit);
            retarget_props_commit(&mut access.gap.0.props, commit);
            retarget_props_commit(&mut access.gap.1.props, commit);
        }
        for repository in &mut self.repositories {
            retarget_props_commit(&mut repository.props, commit);
            retarget_props_commit(&mut repository.gap.0.props, commit);
            retarget_props_commit(&mut repository.gap.1.props, commit);
        }
        for call in &mut self.repository_calls {
            retarget_props_commit(&mut call.props, commit);
        }
    }
}

/// Whether a Room annotation is proven, and if so the same-package names
/// that would obscure a star-import proof (empty for a direct proof).
fn proof(
    annotation: &AnnotationUse<'_>,
    imports: &Imports,
    package: Option<&str>,
    name: &str,
) -> Option<Vec<String>> {
    proof_in(PACKAGES, annotation, imports, package, name)
}

/// Whether an annotation is proven as `name` from one of `packages`, and if
/// so the same-package names that would obscure a star-import proof.
pub(crate) fn proof_in(
    packages: &[&str],
    annotation: &AnnotationUse<'_>,
    imports: &Imports,
    package: Option<&str>,
    name: &str,
) -> Option<Vec<String>> {
    if annotation.simple != name {
        return None;
    }
    if annotation.spelled.contains('.') {
        return packages
            .iter()
            .any(|room| annotation.spelled == format!("{room}.{name}"))
            .then(Vec::new);
    }
    if let Some(bound) = imports.bindings.get(name) {
        return packages
            .iter()
            .any(|room| *bound == format!("{room}.{name}"))
            .then(Vec::new);
    }
    imports
        .wildcard_packages
        .iter()
        .any(|wildcard| packages.contains(&wildcard.as_str()))
        .then(|| {
            vec![match package {
                Some(package) => format!("{package}.{name}"),
                None => name.to_string(),
            }]
        })
}

/// The first annotation among `annotations` proven as Room's `name`.
fn find<'a, 't>(
    annotations: &'a [AnnotationUse<'t>],
    imports: &Imports,
    package: Option<&str>,
    name: &str,
) -> Option<(&'a AnnotationUse<'t>, Vec<String>)> {
    annotations.iter().find_map(|annotation| {
        proof(annotation, imports, package, name).map(|shadows| (annotation, shadows))
    })
}

/// An annotation's arguments as `(name, value)` pairs, the name `None` for
/// a positional one; `Some(empty)` without parentheses, `None` when its
/// arguments are not where the grammar normally puts them.
pub(crate) fn arguments<'t>(
    cx: &FileCx<'_>,
    annotation: TsNode<'t>,
) -> Option<Vec<(Option<String>, TsNode<'t>)>> {
    let Some(invocation) = named_children(annotation)
        .into_iter()
        .find(|child| child.kind() == "constructor_invocation")
    else {
        return (!cx.text(&annotation).contains('(')).then(Vec::new);
    };
    let Some(list) = named_children(invocation)
        .into_iter()
        .find(|child| child.kind() == "value_arguments")
    else {
        return Some(Vec::new());
    };
    let mut found = Vec::new();
    for argument in named_children(list) {
        match named_children(argument)[..] {
            [value] => found.push((None, value)),
            [name, value] if name.kind() == "identifier" => {
                found.push((Some(cx.text(&name).to_string()), value));
            }
            _ => return None,
        }
    }
    Some(found)
}

/// The value of the argument named `key`, or of the first positional one
/// when `positional` is set. `Err` when the arguments cannot be read.
pub(crate) fn argument<'t>(
    cx: &FileCx<'_>,
    annotation: TsNode<'t>,
    key: &str,
    positional: bool,
) -> Result<Option<TsNode<'t>>, ()> {
    let found = arguments(cx, annotation).ok_or(())?;
    Ok(found.iter().find_map(|(name, value)| match name {
        Some(name) if name == key => Some(*value),
        None if positional => Some(*value),
        _ => None,
    }))
}

/// A query's SQL: a pure string literal, or a raw (`"""`) one with no
/// template.
fn sql_literal(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "multiline_string_literal" {
        return pure_string_literal(cx, node);
    }
    let text = cx.text(&node);
    if named_children(node)
        .iter()
        .any(|child| child.kind() != "string_content")
        || text.contains('$')
    {
        return None;
    }
    text.strip_prefix("\"\"\"")?
        .strip_suffix("\"\"\"")
        .map(str::to_string)
}

fn short(cx: &FileCx<'_>, node: TsNode<'_>) -> String {
    let mut text = cx.text(&node).to_string();
    if text.len() > 160 {
        let mut cut = 160;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push('…');
    }
    text
}

fn gap_pair(cx: &FileCx<'_>, at: TsNode<'_>, src: &str, label: &str, reason: &str) -> (Node, Edge) {
    framework_gap(cx, FRAMEWORK, at, src, label, reason)
}

/// A Gap for `framework` at `at`, and the `label` edge from `src` to it.
pub(crate) fn framework_gap(
    cx: &FileCx<'_>,
    framework: &str,
    at: TsNode<'_>,
    src: &str,
    label: &str,
    reason: &str,
) -> (Node, Edge) {
    let gap = format!(
        "gap:{framework}:{}@{}@{}",
        cx.id.repo,
        cx.path,
        at.start_byte()
    );
    (
        Node {
            id: gap.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "reason": reason,
                "framework": framework,
                "language": "kotlin",
                "expression": short(cx, at),
                "attempted_tiers": ["T0"],
                "prov": cx.prov_with_confidence(&at, ConfidenceTier::Gap, &format!("Gap {gap}")),
            }),
        },
        Edge {
            src: src.to_string(),
            dst: gap.clone(),
            label: label.into(),
            props: serde_json::json!({
                "reason": reason,
                "prov": cx.prov_with_confidence(
                    &at,
                    ConfidenceTier::Gap,
                    &format!("{label} {src} -> {gap}"),
                ),
            }),
        },
    )
}

/// The annotations on a class, including those the grammar attaches to a
/// preceding expression statement.
pub(crate) fn class_annotations<'t>(
    cx: &FileCx<'_>,
    declaration: TsNode<'t>,
) -> Vec<AnnotationUse<'t>> {
    let mut found = declaration_annotations(cx, declaration);
    found.extend(preceding_expression_annotations(cx, declaration));
    found
}

/// The column a property declares, or `None` when it is not one.
fn column(
    cx: &FileCx<'_>,
    property: TsNode<'_>,
    name: &str,
    imports: &Imports,
    package: Option<&str>,
) -> Option<String> {
    let annotations = declaration_annotations(cx, property);
    if ["Ignore", "Embedded", "Relation"]
        .iter()
        .any(|skip| find(&annotations, imports, package, skip).is_some())
    {
        return None;
    }
    if let Some((info, _)) = find(&annotations, imports, package, "ColumnInfo")
        && let Ok(Some(value)) = argument(cx, info.node, "name", true)
        && let Some(literal) = pure_string_literal(cx, value)
    {
        return Some(literal);
    }
    Some(name.to_string())
}

/// The property columns of an entity class, in declaration order.
fn columns(
    cx: &FileCx<'_>,
    class: TsNode<'_>,
    imports: &Imports,
    package: Option<&str>,
) -> Vec<String> {
    stored_properties(cx, class)
        .into_iter()
        .filter_map(|(declaration, name)| column(cx, declaration, cx.text(&name), imports, package))
        .collect()
}

/// A class's stored properties in declaration order: `val`/`var`
/// constructor parameters and body properties with a backing field, each
/// as its declaration and name.
pub(crate) fn stored_properties<'t>(
    cx: &FileCx<'_>,
    class: TsNode<'t>,
) -> Vec<(TsNode<'t>, TsNode<'t>)> {
    let mut found = Vec::new();
    for child in named_children(class) {
        match child.kind() {
            "primary_constructor" => {
                for parameters in named_children(child) {
                    for parameter in named_children(parameters) {
                        if parameter.kind() != "class_parameter" {
                            continue;
                        }
                        // `val`/`var` are the grammar's anonymous tokens.
                        let mut walk = parameter.walk();
                        let is_property = parameter
                            .children(&mut walk)
                            .any(|part| matches!(part.kind(), "val" | "var"));
                        let Some(name) = named_children(parameter)
                            .into_iter()
                            .find(|part| part.kind() == "identifier")
                        else {
                            continue;
                        };
                        if is_property {
                            found.push((parameter, name));
                        }
                    }
                }
            }
            "class_body" => {
                for member in named_children(child) {
                    if member.kind() != "property_declaration" {
                        continue;
                    }
                    let Some(name) = named_children(member)
                        .into_iter()
                        .find(|part| part.kind() == "variable_declaration")
                        .and_then(|declaration| {
                            named_children(declaration)
                                .into_iter()
                                .find(|part| part.kind() == "identifier")
                        })
                    else {
                        continue;
                    };
                    // A property with a getter has no backing field unless it
                    // is initialized or an accessor uses `field`.
                    let parts = named_children(member);
                    let accessors: Vec<TsNode<'_>> = parts
                        .iter()
                        .copied()
                        .filter(|part| matches!(part.kind(), "getter" | "setter"))
                        .collect();
                    let mut walk = member.walk();
                    let initialized = member.children(&mut walk).any(|part| part.kind() == "=");
                    if !accessors.is_empty()
                        && !initialized
                        && !accessors.iter().any(|accessor| uses_field(cx, *accessor))
                    {
                        continue;
                    }
                    found.push((member, name));
                }
            }
            _ => {}
        }
    }
    found
}

/// Whether an accessor reads or writes its backing `field`.
fn uses_field(cx: &FileCx<'_>, accessor: TsNode<'_>) -> bool {
    let mut stack = vec![accessor];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" && cx.text(&node) == "field" && !qualified(node) {
            return true;
        }
        stack.extend(named_children(node));
    }
    false
}

/// Whether an identifier is the member half of `x.name`, which names a
/// property called `name`, not the accessor's backing `field`.
fn qualified(node: TsNode<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "navigation_expression"
            && named_children(parent)
                .first()
                .is_some_and(|first| first.id() != node.id())
    })
}

/// The entity type a write function's first parameter names, unwrapped
/// from a collection or array, as its simple name.
fn parameter_entity<'t>(cx: &FileCx<'_>, function: TsNode<'t>) -> Option<(String, TsNode<'t>)> {
    let parameters = named_children(function)
        .into_iter()
        .find(|child| child.kind() == "function_value_parameters")?;
    let parameter = named_children(parameters)
        .into_iter()
        .find(|child| child.kind() == "parameter")?;
    let mut ty = named_children(parameter)
        .into_iter()
        .find(|child| matches!(child.kind(), "user_type" | "nullable_type"))?;
    loop {
        if ty.kind() == "nullable_type" {
            ty = named_children(ty)
                .into_iter()
                .find(|child| child.kind() == "user_type")?;
        }
        let parts = named_children(ty);
        let identifiers: Vec<TsNode<'_>> = parts
            .iter()
            .copied()
            .filter(|part| part.kind() == "identifier")
            .collect();
        let name = cx.text(identifiers.last()?).to_string();
        let arguments = parts.iter().find(|part| part.kind() == "type_arguments");
        match (name.as_str(), arguments) {
            (
                "List" | "MutableList" | "Set" | "MutableSet" | "Collection" | "Iterable" | "Array",
                Some(arguments),
            ) => {
                let projection = named_children(*arguments).into_iter().next()?;
                ty = named_children(projection)
                    .into_iter()
                    .find(|child| matches!(child.kind(), "user_type" | "nullable_type"))?;
            }
            (_, None) => {
                let spelled: Vec<&str> = identifiers.iter().map(|part| cx.text(part)).collect();
                return Some((spelled.join("."), ty));
            }
            _ => return None,
        }
    }
}

/// The FQNs a type name written in this file can denote.
pub(crate) fn candidates(name: &str, imports: &Imports, package: Option<&str>) -> Vec<String> {
    let (head, rest) = match name.split_once('.') {
        Some((head, rest)) => (head, Some(rest)),
        None => (name, None),
    };
    if let Some(bound) = imports.bindings.get(head) {
        return vec![match rest {
            Some(rest) => format!("{bound}.{rest}"),
            None => bound.clone(),
        }];
    }
    let mut found = vec![match package {
        Some(package) => format!("{package}.{name}"),
        None => name.to_string(),
    }];
    found.extend(
        imports
            .wildcard_packages
            .iter()
            .map(|wildcard| format!("{wildcard}.{name}")),
    );
    if rest.is_some() {
        found.push(name.to_string());
    }
    found
}

/// This file's Room entities and DAO accesses.
pub(crate) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    functions_by_start: &HashMap<usize, String>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) -> Pending {
    let mut pending = Pending::default();
    if !cx.text(&root).contains("room") {
        return pending;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        if !matches!(node.kind(), "class_declaration" | "object_declaration") {
            continue;
        }
        let Some(name) = node.child_by_field_name("name") else {
            continue;
        };
        let mut chain = enclosing_type_chain(cx, node);
        chain.push(cx.text(&name).to_string());
        let qualified = chain.join(".");
        let fqn = match package {
            Some(package) => format!("{package}.{qualified}"),
            None => qualified.clone(),
        };
        let annotations = class_annotations(cx, node);
        if let Some((entity, shadows)) = find(&annotations, imports, package, "Entity") {
            let class = super::symbol_id(cx.id.repo, cx.path, &qualified);
            let table = match argument(cx, entity.node, "tableName", false) {
                Ok(None) => Some(cx.text(&name).to_string()),
                // Room's default `""` means the class name.
                Ok(Some(value)) => pure_string_literal(cx, value).map(|table| {
                    if table.is_empty() {
                        cx.text(&name).to_string()
                    } else {
                        table
                    }
                }),
                Err(()) => None,
            };
            let Some(table) = table else {
                let (gap, edge) = gap_pair(
                    cx,
                    entity.node,
                    &class,
                    "REFERENCES",
                    "computed Room entity table name",
                );
                nodes.push(gap);
                edges.push(edge);
                continue;
            };
            let id = entity_id(cx.id.repo, &table.to_lowercase());
            let columns: Vec<serde_json::Value> = columns(cx, node, imports, package)
                .into_iter()
                .map(|name| serde_json::json!({ "name": name }))
                .collect();
            pending.entities.push(Entity {
                fqn: fqn.clone(),
                path: cx.path.to_string(),
                shadows,
                node: Node {
                    id: id.clone(),
                    label: "DataEntity".into(),
                    props: serde_json::json!({
                        "name": table,
                        "storage": "sql",
                        "store": table,
                        "table": table,
                        "class": fqn,
                        "columns": columns,
                        "framework": FRAMEWORK,
                        "language": "kotlin",
                        "resolution": "room-entity",
                        "prov": cx.prov(&node, &format!("DataEntity {id}")),
                    }),
                },
                storage: "sql",
                duplicate_prov: None,
            });
        }
        let Some((_, dao_shadows)) = find(&annotations, imports, package, "Dao") else {
            continue;
        };
        let Some(body) = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "class_body")
        else {
            continue;
        };
        for function in named_children(body) {
            if function.kind() != "function_declaration" {
                continue;
            }
            let Some(src) = functions_by_start.get(&function.start_byte()) else {
                continue;
            };
            let annotations = declaration_annotations(cx, function);
            if let Some((query, shadows)) = find(&annotations, imports, package, "Query") {
                let shadows: Vec<String> = dao_shadows.iter().chain(&shadows).cloned().collect();
                let literal = match argument(cx, query.node, "value", true) {
                    Ok(Some(value)) => sql_literal(cx, value),
                    _ => None,
                };
                let Some(sql) = literal else {
                    let (gap, edge) = gap_pair(
                        cx,
                        query.node,
                        src,
                        "READS",
                        "Room query is not a string literal",
                    );
                    nodes.push(gap);
                    edges.push(edge);
                    continue;
                };
                let (accesses, computed) = parse_with_gaps(&sql);
                if computed || accesses.is_empty() {
                    let (gap, edge) =
                        gap_pair(cx, query.node, src, "READS", "Room query names no table");
                    nodes.push(gap);
                    edges.push(edge);
                    continue;
                }
                let mut seen: BTreeMap<(String, &'static str), BTreeSet<&'static str>> =
                    BTreeMap::new();
                for access in accesses {
                    let label = if access.write.is_some() {
                        "WRITES"
                    } else {
                        "READS"
                    };
                    let operations = seen
                        .entry((access.table.to_lowercase(), label))
                        .or_default();
                    if let Some(operation) = access.write {
                        operations.insert(operation);
                    }
                }
                for ((table, label), operations) in seen {
                    let mut props = serde_json::json!({
                        "resolution": "sql-literal",
                        "framework": FRAMEWORK,
                        "prov": cx.prov_with_confidence(
                            &query.node,
                            ConfidenceTier::InferredStrong,
                            &format!("{label} {src} -> {}", entity_id(cx.id.repo, &table)),
                        ),
                    });
                    if let Some(first) = operations.first() {
                        props["operation"] = (*first).into();
                        props["operations"] = operations.iter().copied().collect::<Vec<_>>().into();
                    }
                    pending.accesses.push(Access {
                        path: cx.path.to_string(),
                        shadows: shadows.clone(),
                        src: src.clone(),
                        target: Target::Table(table.clone()),
                        label,
                        props,
                        storage: "sql",
                        gap: gap_pair(
                            cx,
                            query.node,
                            src,
                            label,
                            &format!("Room query table `{table}` is not a recovered @Entity"),
                        ),
                    });
                }
                continue;
            }
            let write = [
                ("Insert", &["insert"][..]),
                ("Update", &["update"][..]),
                ("Delete", &["delete"][..]),
                ("Upsert", &["insert", "update"][..]),
            ]
            .into_iter()
            .find_map(|(name, operations)| {
                find(&annotations, imports, package, name)
                    .map(|(annotation, shadows)| (annotation, shadows, operations))
            });
            let Some((annotation, shadows, operations)) = write else {
                continue;
            };
            let shadows: Vec<String> = dao_shadows.iter().chain(&shadows).cloned().collect();
            if !matches!(argument(cx, annotation.node, "entity", false), Ok(None)) {
                let (gap, edge) = gap_pair(
                    cx,
                    annotation.node,
                    src,
                    "WRITES",
                    "Room write names its entity by argument, not read at T0",
                );
                nodes.push(gap);
                edges.push(edge);
                continue;
            }
            let Some((ty, at)) = parameter_entity(cx, function) else {
                let (gap, edge) = gap_pair(
                    cx,
                    annotation.node,
                    src,
                    "WRITES",
                    "Room write parameter type not read",
                );
                nodes.push(gap);
                edges.push(edge);
                continue;
            };
            let props = serde_json::json!({
                "resolution": "room-entity-parameter",
                "framework": FRAMEWORK,
                "operation": operations[0],
                "operations": operations,
                "prov": cx.prov(&function, &format!("WRITES {src} -> {ty}")),
            });
            pending.accesses.push(Access {
                path: cx.path.to_string(),
                shadows,
                src: src.clone(),
                target: Target::Class(candidates(&ty, imports, package)),
                label: "WRITES",
                props,
                storage: "sql",
                gap: gap_pair(
                    cx,
                    at,
                    src,
                    "WRITES",
                    &format!("Room write type `{ty}` is not a recovered @Entity"),
                ),
            });
        }
    }
    pending
}

/// Entities and accesses joined over the repository: `shadowed` holds the
/// names product sources declare that obscure a star import.
pub(crate) fn resolve(
    pending: Pending,
    shadowed: &[ShadowName],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let obscured = |names: &[String], path: &str| {
        names
            .iter()
            .any(|name| shadowed.iter().any(|shadow| shadow.obscures(name, path)))
    };
    // A JPA entity name two classes declare names no single class: both
    // become one Gap and the id resolves to nothing (as in Java).
    let mut claims: BTreeMap<String, Vec<(String, serde_json::Value)>> = BTreeMap::new();
    for entity in &pending.entities {
        if let Some(prov) = &entity.duplicate_prov
            && !obscured(&entity.shadows, &entity.path)
        {
            claims
                .entry(entity.node.id.clone())
                .or_default()
                .push((entity.fqn.clone(), prov.clone()));
        }
    }
    let mut collided: BTreeSet<String> = BTreeSet::new();
    for (id, mut declared) in claims {
        if declared.len() < 2 {
            continue;
        }
        declared.sort_by(|a, b| a.0.cmp(&b.0));
        let fqns: Vec<&String> = declared.iter().map(|(fqn, _)| fqn).collect();
        nodes.push(Node {
            id: format!("gap:jpa:duplicate:{id}"),
            label: "Gap".into(),
            props: serde_json::json!({
                "reason": "JPA entity name is declared by more than one class",
                "attempted_tiers": ["T0"],
                "framework": "jpa",
                "language": "kotlin",
                "fqns": fqns,
                "prov": declared[0].1,
            }),
        });
        collided.insert(id);
    }
    let mut by_table: BTreeMap<String, String> = BTreeMap::new();
    let mut by_class: BTreeMap<(&'static str, String), Option<String>> = BTreeMap::new();
    let mut emitted: BTreeMap<String, usize> = BTreeMap::new();
    for mut entity in pending.entities {
        if obscured(&entity.shadows, &entity.path) {
            continue;
        }
        let id = entity.node.id.clone();
        if collided.contains(&id) {
            by_class.insert((entity.storage, entity.fqn), None);
            continue;
        }
        if let Some(fields) = entity.node.props["fields"].as_array_mut() {
            // A default-imported `@Transient` holds unless a same-package
            // `Transient` obscures it (AC-1028).
            fields.retain_mut(|field| match field["unless_obscured"].take() {
                serde_json::Value::String(shadow) => obscured(&[shadow], &entity.path),
                _ => true,
            });
            for field in fields.iter_mut() {
                if let Some(object) = field.as_object_mut() {
                    object.remove("unless_obscured");
                }
            }
        }
        if entity.storage == "sql" {
            let table = entity.node.props["table"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase();
            by_table.insert(table, id.clone());
        }
        by_class
            .entry((entity.storage, entity.fqn.clone()))
            .and_modify(|unique| *unique = None)
            .or_insert(Some(id.clone()));
        match emitted.get(&id) {
            // Two classes on one table: one node, its columns the union.
            Some(&at) => {
                let extra = entity.node.props["columns"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if let Some(columns) = nodes[at].props["columns"].as_array_mut() {
                    for column in extra {
                        if !columns.contains(&column) {
                            columns.push(column);
                        }
                    }
                }
            }
            None => {
                emitted.insert(id, nodes.len());
                nodes.push(entity.node);
            }
        }
    }
    // A type name resolves in Kotlin's precedence (the order `candidates`
    // lists): the first candidate the repository declares at all decides,
    // so a same-package non-entity hides a star-imported entity of that
    // name rather than falling through to it.
    let declared = |fqn: &str, path: &str| {
        shadowed.iter().any(|shadow| shadow.obscures(fqn, path))
            || ["jpa", "sql"]
                .iter()
                .any(|storage| by_class.contains_key(&(*storage, fqn.to_string())))
    };
    let unique_jpa = |candidates: &[String], path: &str| -> Option<String> {
        let first = candidates.iter().find(|fqn| declared(fqn, path))?;
        by_class.get(&("jpa", first.clone())).cloned().flatten()
    };
    // Spring Data repositories (AC-1100): each interface binds the JPA
    // entity its type argument names. An interface FQN declared twice binds
    // nothing, and a call through it asserts nothing.
    let mut repository_entity: BTreeMap<String, Option<String>> = BTreeMap::new();
    for repository in pending.repositories {
        let entity = if obscured(&repository.shadows, &repository.path) {
            None
        } else {
            unique_jpa(&repository.entity, &repository.path)
        };
        match entity {
            Some(entity) => {
                repository_entity
                    .entry(repository.fqn)
                    .and_modify(|bound| *bound = None)
                    .or_insert_with(|| Some(entity.clone()));
                edges.push(Edge {
                    src: repository.src,
                    dst: entity,
                    label: "REFERENCES".into(),
                    props: repository.props,
                });
            }
            None => {
                repository_entity.insert(repository.fqn, None);
                nodes.push(repository.gap.0);
                edges.push(repository.gap.1);
            }
        }
    }
    // Every call site is its own occurrence: the reconciler unions repeated
    // `(src, dst, label)` evidence, so each keeps its citation.
    // The receiver's type resolves the same way: the first declared
    // candidate must itself be a repository.
    for call in pending.repository_calls {
        let Some(first) = call
            .candidates
            .iter()
            .find(|fqn| repository_entity.contains_key(*fqn) || declared(fqn, &call.path))
        else {
            continue;
        };
        let Some(Some(entity)) = repository_entity.get(first) else {
            continue;
        };
        edges.push(Edge {
            src: call.src,
            dst: entity.clone(),
            label: call.label.into(),
            props: call.props,
        });
    }
    let mut seen = BTreeSet::new();
    let mut emitted_edges: HashMap<(String, String, &'static str), usize> = HashMap::new();
    for access in pending.accesses {
        let dst = if obscured(&access.shadows, &access.path) || collided.contains(&access.src) {
            None
        } else {
            match &access.target {
                Target::Table(table) => by_table.get(table).cloned(),
                Target::Class(candidates) => {
                    let found: BTreeSet<&String> = candidates
                        .iter()
                        .filter_map(|fqn| {
                            by_class
                                .get(&(access.storage, fqn.clone()))
                                .and_then(Option::as_ref)
                        })
                        .collect();
                    match found.into_iter().collect::<Vec<_>>()[..] {
                        [id] => Some(id.clone()),
                        _ => None,
                    }
                }
            }
        };
        match dst {
            Some(dst) => {
                let key = (access.src.clone(), dst.clone(), access.label);
                let relation = access.storage == "jpa";
                match emitted_edges.get(&key) {
                    Some(&at) if relation => merge_relations(&mut edges[at].props, &access.props),
                    Some(&at) => merge_access_props(&mut edges[at].props, &access.props),
                    None => {
                        emitted_edges.insert(key, edges.len());
                        let mut props = access.props;
                        if relation {
                            props["relations"] = serde_json::json!([relation_props(&props)]);
                        }
                        edges.push(Edge {
                            src: access.src,
                            dst,
                            label: access.label.into(),
                            props,
                        });
                    }
                }
            }
            None => {
                let (gap, edge) = access.gap;
                if seen.insert((edge.src.clone(), gap.id.clone(), access.label)) {
                    nodes.push(gap);
                    edges.push(edge);
                }
            }
        }
    }
}

/// Folds a second access of the same table by the same function into the
/// edge already emitted, keeping the union of their operations and
/// statements, so an `insert` then an `update` keep both.
fn merge_access_props(kept: &mut serde_json::Value, other: &serde_json::Value) {
    union(kept, other, "operations", "operation");
    union(kept, other, "statements", "statement");
}

/// One JPA relation field's metadata, as the Java reader's `relations`
/// entries carry it.
fn relation_props(props: &serde_json::Value) -> serde_json::Value {
    let mut relation = serde_json::Map::new();
    for key in [
        "field",
        "relation",
        "cardinality",
        "owning",
        "mapped_by",
        "join_column",
        "join_table",
    ] {
        if let Some(value) = props.get(key) {
            relation.insert(key.to_string(), value.clone());
        }
    }
    serde_json::Value::Object(relation)
}

/// Folds a second JPA relation field to the same target into the emitted
/// edge, as the Java reader does: the per-field metadata in `relations`,
/// sorted by field, and provenance citing every field (AC-1028).
fn merge_relations(kept: &mut serde_json::Value, other: &serde_json::Value) {
    let mut relations = kept["relations"].as_array().cloned().unwrap_or_default();
    relations.push(relation_props(other));
    relations.sort_by(|a, b| a["field"].as_str().cmp(&b["field"].as_str()));
    let mut evidence: Vec<EvidenceRef> = Vec::new();
    for side in [&*kept, other] {
        if let Ok(prov) = serde_json::from_value::<Provenance>(side["prov"].clone()) {
            evidence.extend(prov.evidence);
        }
    }
    let fact = serde_json::to_vec(&serde_json::json!({ "relations": relations }))
        .expect("relations serialize");
    let prov = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        evidence,
        super::EXTRACTOR_ID,
        &fact,
    )
    .expect("Deterministic confidence is within its ceiling");
    *kept = serde_json::json!({
        "framework": "jpa",
        "relations": relations,
        "prov": serde_json::to_value(prov).expect("provenance serializes"),
    });
}

/// `kept[plural]` becomes the distinct values of both sides' `plural`
/// lists, or their `singular` value where a side has no list, sorted;
/// written only when it holds more than `kept` already says.
fn union(kept: &mut serde_json::Value, other: &serde_json::Value, plural: &str, singular: &str) {
    let values = |side: &serde_json::Value| match side[plural].as_array() {
        Some(list) => list.clone(),
        None => side
            .get(singular)
            .filter(|v| v.is_string())
            .cloned()
            .into_iter()
            .collect(),
    };
    let mut merged = values(kept);
    let before = merged.len();
    for value in values(other) {
        if !merged.contains(&value) {
            merged.push(value);
        }
    }
    if merged.len() > before || (kept[plural].is_null() && merged.len() > 1) {
        merged.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        if kept.get(singular).is_none_or(serde_json::Value::is_null) {
            kept[singular] = merged[0].clone();
        }
        kept[plural] = merged.into();
    }
}
