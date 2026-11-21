//! JetBrains Exposed persistence (#2021, AC-1027).
//!
//! An `object` whose supertype is an import-proven Exposed table base
//! (`Table`, or `IdTable`, `IntIdTable`, `LongIdTable`, `UIntIdTable`,
//! `ULongIdTable`, `UUIDTable` or `CompositeIdTable`; Exposed 0.x and 1.x
//! packages) is a `DataEntity` table (`storage: "sql"`, framework
//! `exposed`), the same table model as Room's ([`super::room`]). Its name
//! is the constructor's literal name, else the object name less a `Table`
//! suffix, as Exposed derives it. Its columns are the properties whose
//! initializer is rooted at a column builder call with a literal name
//! (`varchar("name", 50).nullable()`), plus `id` for an id table.
//!
//! A DSL statement on a table object, `Users.insert { … }` or
//! `Users.selectAll()`, is a `READS` or `WRITES` edge from the enclosing
//! function Symbol (InferredStrong, resolution `exposed-dsl`): `select`,
//! `selectAll` and `selectBatched` read; `insert`, `insertAndGetId`,
//! `insertIgnore`, `insertIgnoreAndGetId`, `batchInsert`, `replace` and
//! `batchReplace` insert; `update` updates; `upsert` and `batchUpsert`
//! insert and update; `deleteWhere`, `deleteIgnoreWhere` and `deleteAll`
//! delete. The receiver resolves at the directory join to a table object
//! by the file's imports or package. Statements are read only in a file
//! that imports from an Exposed package. A computed table name is a Gap,
//! and so is a statement whose receiver resolves to no recovered table. A
//! star-imported base that a same-package declaration obscures proves
//! nothing.

use core_graph::{Edge, Node};
use std::collections::HashMap;
use tree_sitter::Node as TsNode;

use super::rest_template::named_children;
use super::room::{self, candidates, framework_gap};
use super::{FileCx, Imports, enclosing_type_chain, pure_string_literal, spelling};
use adapters_fw::sql::entity_id;

const FRAMEWORK: &str = "exposed";
const TABLE_PACKAGES: &[&str] = &["org.jetbrains.exposed.sql", "org.jetbrains.exposed.v1.core"];
const ID_TABLE_PACKAGES: &[&str] = &[
    "org.jetbrains.exposed.dao.id",
    "org.jetbrains.exposed.v1.core.dao.id",
];
const ID_TABLES: &[&str] = &[
    "IdTable",
    "IntIdTable",
    "LongIdTable",
    "UIntIdTable",
    "ULongIdTable",
    "UUIDTable",
    "CompositeIdTable",
];

/// Exposed `Table` member functions that declare a column, named by
/// their first argument.
const COLUMN_BUILDERS: &[&str] = &[
    "array",
    "binary",
    "blob",
    "bool",
    "byte",
    "char",
    "customEnumeration",
    "date",
    "datetime",
    "decimal",
    "double",
    "duration",
    "entityId",
    "enumeration",
    "enumerationByName",
    "float",
    "integer",
    "javaUUID",
    "json",
    "jsonb",
    "largeText",
    "long",
    "mediumText",
    "optReference",
    "reference",
    "registerColumn",
    "short",
    "text",
    "time",
    "timestamp",
    "timestampWithTimeZone",
    "ubyte",
    "uinteger",
    "ulong",
    "ushort",
    "uuid",
    "varchar",
];

/// The operations a DSL statement of this name performs, with its label.
fn statement(name: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match name {
        "select" | "selectAll" | "selectBatched" => ("READS", &[]),
        "insert"
        | "insertAndGetId"
        | "insertIgnore"
        | "insertIgnoreAndGetId"
        | "batchInsert"
        | "replace"
        | "batchReplace" => ("WRITES", &["insert"]),
        "update" => ("WRITES", &["update"]),
        "upsert" | "batchUpsert" => ("WRITES", &["insert", "update"]),
        "deleteWhere" | "deleteIgnoreWhere" | "deleteAll" => ("WRITES", &["delete"]),
        _ => return None,
    })
}

/// Whether a supertype spelling is a proven Exposed table base: `None` if
/// not, else whether it is an id table and the same-package names that
/// would obscure a star-import proof.
fn base_proof(
    spelled: &str,
    imports: &Imports,
    package: Option<&str>,
) -> Option<(bool, Vec<String>)> {
    let simple = spelled.rsplit('.').next().unwrap_or(spelled);
    let id_table = ID_TABLES.contains(&simple);
    if !id_table && simple != "Table" {
        return None;
    }
    let packages = if id_table {
        ID_TABLE_PACKAGES
    } else {
        TABLE_PACKAGES
    };
    if spelled.contains('.') {
        return packages
            .iter()
            .any(|exposed| spelled == format!("{exposed}.{simple}"))
            .then(|| (id_table, Vec::new()));
    }
    if let Some(bound) = imports.bindings.get(simple) {
        return packages
            .iter()
            .any(|exposed| *bound == format!("{exposed}.{simple}"))
            .then(|| (id_table, Vec::new()));
    }
    imports
        .wildcard_packages
        .iter()
        .any(|wildcard| packages.contains(&wildcard.as_str()))
        .then(|| {
            (
                id_table,
                vec![match package {
                    Some(package) => format!("{package}.{simple}"),
                    None => simple.to_string(),
                }],
            )
        })
}

/// The table base an object declares: its spelling and constructor
/// arguments.
fn supertype<'t>(
    cx: &FileCx<'_>,
    object: TsNode<'t>,
    imports: &Imports,
) -> Option<(String, Option<TsNode<'t>>)> {
    let specifiers = named_children(object)
        .into_iter()
        .find(|child| child.kind() == "delegation_specifiers")?;
    for specifier in named_children(specifiers) {
        let Some(first) = specifier.named_child(0) else {
            continue;
        };
        let (ty, arguments) = match first.kind() {
            "user_type" => (first, None),
            "constructor_invocation" => {
                let parts = named_children(first);
                let Some(ty) = parts.iter().find(|part| part.kind() == "user_type") else {
                    continue;
                };
                (
                    *ty,
                    parts
                        .iter()
                        .find(|part| part.kind() == "value_arguments")
                        .copied(),
                )
            }
            _ => continue,
        };
        let mut spelled = spelling(cx.text(&ty));
        // An aliased import (`Table as ExposedTable`) names its target.
        if !spelled.contains('.')
            && let Some(bound) = imports.bindings.get(spelled.as_str())
        {
            spelled = bound.clone();
        }
        let simple = spelled.rsplit('.').next().unwrap_or(&spelled);
        if simple == "Table" || ID_TABLES.contains(&simple) {
            return Some((spelled, arguments));
        }
    }
    None
}

/// The table name a base's constructor arguments give: `Ok(None)` when
/// absent, `Err` when present but not a literal.
fn name_argument(cx: &FileCx<'_>, arguments: Option<TsNode<'_>>) -> Result<Option<String>, ()> {
    let Some(arguments) = arguments else {
        return Ok(None);
    };
    for argument in named_children(arguments) {
        let parts = named_children(argument);
        let value = match parts[..] {
            [value] => value,
            [name, value] if name.kind() == "identifier" && cx.text(&name) == "name" => value,
            _ => continue,
        };
        return match pure_string_literal(cx, value) {
            Some(name) if !name.is_empty() => Ok(Some(name)),
            Some(_) => Ok(None),
            None => Err(()),
        };
    }
    Ok(None)
}

/// A column property's name: the literal first argument of the builder
/// call its initializer chain is rooted at.
fn column_name(cx: &FileCx<'_>, property: TsNode<'_>) -> Option<String> {
    let mut node = named_children(property)
        .into_iter()
        .find(|part| part.kind() == "call_expression")?;
    loop {
        let parts = named_children(node);
        let callee = *parts.first()?;
        match callee.kind() {
            "navigation_expression" => {
                node = named_children(callee)
                    .into_iter()
                    .next()
                    .filter(|inner| inner.kind() == "call_expression")?;
            }
            "identifier" if COLUMN_BUILDERS.contains(&cx.text(&callee)) => {
                let arguments = parts.iter().find(|part| part.kind() == "value_arguments")?;
                let first = named_children(*arguments).into_iter().next()?;
                let value = match named_children(first)[..] {
                    [value] => value,
                    _ => return None,
                };
                return pure_string_literal(cx, value);
            }
            _ => return None,
        }
    }
}

/// The function Symbol that encloses `node`.
fn owner(node: TsNode<'_>, functions_by_start: &HashMap<usize, String>) -> Option<String> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "function_declaration"
            && let Some(symbol) = functions_by_start.get(&parent.start_byte())
        {
            return Some(symbol.clone());
        }
        current = parent;
    }
    None
}

/// This file's Exposed tables and DSL statements, into the shared table
/// join.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    functions_by_start: &HashMap<usize, String>,
    pending: &mut room::Pending,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let imports_exposed = imports
        .bindings
        .values()
        .chain(&imports.wildcard_packages)
        .any(|target| target.starts_with("org.jetbrains.exposed."));
    if !imports_exposed {
        return;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        stack.extend(named_children(node));
        match node.kind() {
            "object_declaration" => table(cx, node, package, imports, pending, nodes, edges),
            "call_expression" => {
                access(cx, node, package, imports, functions_by_start, pending);
            }
            _ => {}
        }
    }
}

fn table(
    cx: &FileCx<'_>,
    object: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    pending: &mut room::Pending,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let Some(name) = object.child_by_field_name("name") else {
        return;
    };
    let Some((spelled, arguments)) = supertype(cx, object, imports) else {
        return;
    };
    let Some((id_table, shadows)) = base_proof(&spelled, imports, package) else {
        return;
    };
    let mut chain = enclosing_type_chain(cx, object);
    chain.push(cx.text(&name).to_string());
    let qualified = chain.join(".");
    let fqn = match package {
        Some(package) => format!("{package}.{qualified}"),
        None => qualified.clone(),
    };
    let table = match name_argument(cx, arguments) {
        Ok(Some(table)) => table,
        Ok(None) => {
            let object_name = cx.text(&name);
            object_name
                .strip_suffix("Table")
                .filter(|stem| !stem.is_empty())
                .unwrap_or(object_name)
                .to_string()
        }
        Err(()) => {
            let class = super::symbol_id(cx.id.repo, cx.path, &qualified);
            let (gap, edge) = framework_gap(
                cx,
                FRAMEWORK,
                arguments.unwrap_or(object),
                &class,
                "REFERENCES",
                "computed Exposed table name",
            );
            nodes.push(gap);
            edges.push(edge);
            return;
        }
    };
    let mut columns: Vec<String> = Vec::new();
    if id_table {
        columns.push("id".into());
    }
    if let Some(body) = named_children(object)
        .into_iter()
        .find(|child| child.kind() == "class_body")
    {
        for member in named_children(body) {
            if member.kind() == "property_declaration"
                && let Some(column) = column_name(cx, member)
                && !columns.contains(&column)
            {
                columns.push(column);
            }
        }
    }
    let id = entity_id(cx.id.repo, &table.to_lowercase());
    let columns: Vec<serde_json::Value> = columns
        .into_iter()
        .map(|name| serde_json::json!({ "name": name }))
        .collect();
    pending.push_entity(
        fqn.clone(),
        cx.path,
        shadows,
        Node {
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
                "resolution": "exposed-table",
                "prov": cx.prov(&object, &format!("DataEntity {id}")),
            }),
        },
    );
}

fn access(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    package: Option<&str>,
    imports: &Imports,
    functions_by_start: &HashMap<usize, String>,
    pending: &mut room::Pending,
) {
    let Some(callee) = call.named_child(0) else {
        return;
    };
    if callee.kind() != "navigation_expression" {
        return;
    }
    let [receiver, method] = named_children(callee)[..] else {
        return;
    };
    if receiver.kind() != "identifier" || method.kind() != "identifier" {
        return;
    }
    let Some((label, operations)) = statement(cx.text(&method)) else {
        return;
    };
    let receiver_name = cx.text(&receiver);
    // A table object is named in UpperCamelCase; a local value is not.
    if !receiver_name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return;
    }
    let Some(src) = owner(call, functions_by_start) else {
        return;
    };
    let mut props = serde_json::json!({
        "resolution": "exposed-dsl",
        "framework": FRAMEWORK,
        "statement": cx.text(&method),
        "prov": cx.prov_with_confidence(
            &call,
            core_prov::ConfidenceTier::InferredStrong,
            &format!("{label} {src} -> {receiver_name}"),
        ),
    });
    if let Some(first) = operations.first() {
        props["operation"] = (*first).into();
        props["operations"] = operations.to_vec().into();
    }
    let gap = framework_gap(
        cx,
        FRAMEWORK,
        callee,
        &src,
        label,
        &format!("Exposed statement receiver `{receiver_name}` is not a recovered table"),
    );
    pending.push_class_access(
        cx.path,
        Vec::new(),
        &src,
        candidates(receiver_name, imports, package),
        label,
        props,
        gap,
    );
}
