//! JPA data model (#239): deterministic T0 `DataEntity` nodes from
//! import-proven `jakarta.persistence`/`javax.persistence` annotations,
//! `REFERENCES` relations between entities, a Spring Data repository →
//! entity binding, and `READS`/`WRITES` edges from repository call sites.
//!
//! It mirrors the IndexedDB precedent in `adapters-lang-ts`: a `DataEntity`
//! node per persistence store (`storage: "jpa"`, `store` = table), operations
//! attributed to the calling method Symbol, and anything that cannot be
//! proven — a computed entity or table name, a relation whose target is not
//! a recovered entity — becomes an explicit Gap (R-INT-4), never a guess.
//!
//! Annotations are proven by import, never by name alone: a lookalike
//! `@Entity` from another package asserts nothing. Targets that live in other
//! files resolve at the directory join, through the same imports/package
//! candidate rule as every cross-file Java fact.

use super::{
    BTreeMap, BTreeSet, ConfidenceTier, EXTRACTOR_ID, Edge, EvidenceRef, Extraction, FileCx,
    HashMap, Imports, Node, Provenance, TYPE_KINDS, Tier, TsNode, enclosing_type_chain, is_comment,
    retarget_props_commit, string_literal, symbol_id,
};

/// The two Java Persistence API package roots (Jakarta EE and the legacy
/// pre-9 namespace).
const JPA_PACKAGES: &[&str] = &["jakarta.persistence", "javax.persistence"];

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

/// An entity declared in the repository, keyed by its class FQN.
#[derive(Debug, Clone)]
pub(crate) struct EntityDecl {
    pub fqn: String,
    pub id: String,
    /// Provenance for the Gap that replaces this entity when several classes
    /// declare the same entity name. It is built at extraction time because
    /// the directory join cannot recompute a source span.
    pub duplicate_prov: serde_json::Value,
}

/// A Spring Data repository interface, keyed by its FQN.
#[derive(Debug, Clone)]
pub(crate) struct RepositoryDecl {
    pub fqn: String,
}

/// An edge whose target is a type named in this file; the directory join
/// resolves it against the repo's entities (or fails closed to its Gap).
#[derive(Debug, Clone)]
pub(crate) struct PendingLink {
    candidates: Vec<String>,
    /// The resolved edge, `dst` filled in at the join.
    edge: Edge,
    gap: (Node, Edge),
    /// Set when this link is a repository → entity binding.
    repository: Option<String>,
}

/// A call to a Spring Data vocabulary method on a receiver whose declared
/// type is named in this file.
#[derive(Debug, Clone)]
pub(crate) struct PendingRepoCall {
    src: String,
    candidates: Vec<String>,
    method: String,
    writes: bool,
    prov: serde_json::Value,
}

pub(crate) fn retarget(extraction: &mut Extraction, commit: &str) {
    for link in &mut extraction.pending_links {
        retarget_props_commit(&mut link.edge.props, commit);
        retarget_props_commit(&mut link.gap.0.props, commit);
        retarget_props_commit(&mut link.gap.1.props, commit);
    }
    for call in &mut extraction.pending_repo_calls {
        let mut holder = serde_json::json!({ "prov": call.prov.take() });
        retarget_props_commit(&mut holder, commit);
        call.prov = holder["prov"].take();
    }
    for decl in &mut extraction.entity_index {
        let mut holder = serde_json::json!({ "prov": decl.duplicate_prov.take() });
        retarget_props_commit(&mut holder, commit);
        decl.duplicate_prov = holder["prov"].take();
    }
}

// ---------------------------------------------------------------------------
// Annotation helpers
// ---------------------------------------------------------------------------

/// A named annotation element read as a string, three-way like the Spring
/// mapping path: absent, a provable literal, or present but dynamic.
enum Str {
    Absent,
    Literal(String),
    Dynamic,
}

fn element<'t>(cx: &FileCx<'_>, annotation: TsNode<'t>, key: &str) -> Option<TsNode<'t>> {
    let arguments = annotation.child_by_field_name("arguments")?;
    let mut walk = arguments.walk();
    for argument in arguments.named_children(&mut walk) {
        if argument.kind() != "element_value_pair" {
            continue;
        }
        let (Some(k), Some(value)) = (
            argument.child_by_field_name("key"),
            argument.child_by_field_name("value"),
        ) else {
            continue;
        };
        if cx.text(&k) == key {
            return Some(value);
        }
    }
    None
}

fn element_str(cx: &FileCx<'_>, annotation: TsNode<'_>, key: &str) -> Str {
    match element(cx, annotation, key) {
        None => Str::Absent,
        Some(value) => string_literal(cx, value).map_or(Str::Dynamic, Str::Literal),
    }
}

fn literal(cx: &FileCx<'_>, annotation: TsNode<'_>, key: &str) -> Option<String> {
    match element_str(cx, annotation, key) {
        Str::Literal(value) => Some(value),
        _ => None,
    }
}

/// Persistence annotations on a declaration, proven by import (named or
/// wildcard) or by a fully qualified spelling: `(simple name, node)`.
fn persistence_annotations<'t>(
    cx: &FileCx<'_>,
    declaration: TsNode<'t>,
    imports: &Imports,
) -> Vec<(String, TsNode<'t>)> {
    let mut found = Vec::new();
    let mut walk = declaration.walk();
    for child in declaration.named_children(&mut walk) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut inner = child.walk();
        for annotation in child.named_children(&mut inner) {
            if !matches!(annotation.kind(), "annotation" | "marker_annotation") {
                continue;
            }
            let Some(name) = annotation.child_by_field_name("name") else {
                continue;
            };
            let full = cx.text(&name);
            let (prefix, simple) = match full.rsplit_once('.') {
                Some((prefix, simple)) => (Some(prefix), simple),
                None => (None, full),
            };
            let proven = match prefix {
                Some(prefix) => JPA_PACKAGES.contains(&prefix),
                None => match imports.bindings.get(simple) {
                    Some(fqn) => JPA_PACKAGES
                        .iter()
                        .any(|package| *fqn == format!("{package}.{simple}")),
                    None => imports
                        .wildcard_packages
                        .iter()
                        .any(|wildcard| JPA_PACKAGES.contains(&wildcard.as_str())),
                },
            };
            if proven {
                found.push((simple.to_string(), annotation));
            }
        }
    }
    found
}

fn has_modifier(declaration: TsNode<'_>, keyword: &str) -> bool {
    let mut walk = declaration.walk();
    for child in declaration.named_children(&mut walk) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut inner = child.walk();
        if child.children(&mut inner).any(|m| m.kind() == keyword) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Type helpers
// ---------------------------------------------------------------------------

/// A type's spelled name with generics stripped (`Set<Pet>` → `Set`);
/// `None` for arrays, wildcards and anything that names no single type.
fn type_name(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    match node.kind() {
        "type_identifier" | "scoped_type_identifier" => Some(cx.text(&node).to_string()),
        "generic_type" => {
            let mut walk = node.walk();
            let head = node
                .named_children(&mut walk)
                .find(|c| matches!(c.kind(), "type_identifier" | "scoped_type_identifier"))?;
            Some(cx.text(&head).to_string())
        }
        _ => None,
    }
}

/// The last type argument of a generic type (`Set<Pet>` → `Pet`,
/// `Map<K, Pet>` → `Pet`).
fn last_type_argument<'t>(node: TsNode<'t>) -> Option<TsNode<'t>> {
    if node.kind() != "generic_type" {
        return None;
    }
    let mut walk = node.walk();
    let arguments = node
        .named_children(&mut walk)
        .find(|c| c.kind() == "type_arguments")?;
    let mut inner = arguments.walk();
    arguments
        .named_children(&mut inner)
        .filter(|c| !is_comment(*c))
        .last()
}

fn normalize_type(cx: &FileCx<'_>, node: TsNode<'_>) -> String {
    cx.text(&node)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// FQNs a type spelled in this file may denote, in proof order: a named
/// import is exact; otherwise the file's own package and each wildcard
/// package are candidates (the join accepts exactly one declared match).
fn type_candidates(name: &str, imports: &Imports, package: Option<&str>) -> Vec<String> {
    if let Some(fqn) = imports.bindings.get(name) {
        return vec![fqn.clone()];
    }
    if name.contains('.') {
        return vec![name.to_string()];
    }
    let mut candidates = Vec::new();
    candidates.push(match package {
        Some(package) => format!("{package}.{name}"),
        None => name.to_string(),
    });
    for wildcard in &imports.wildcard_packages {
        candidates.push(format!("{wildcard}.{name}"));
    }
    candidates
}

// ---------------------------------------------------------------------------
// Per-file extraction
// ---------------------------------------------------------------------------

fn gap_pair(
    cx: &FileCx<'_>,
    src: &str,
    label: &str,
    evidence: &[TsNode<'_>],
    reason: &str,
    extra: serde_json::Value,
) -> (Node, Edge) {
    let first = evidence[0].start_byte();
    let gap_id = format!("gap:jpa:{}@{}@{}", cx.id.repo, cx.path, first);
    let mut props = serde_json::json!({
        "reason": reason,
        "attempted_tiers": ["T0"],
        "framework": "jpa",
        "language": "java",
        "prov": cx.prov_spanning(evidence, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
    });
    if let (Some(map), Some(extra)) = (props.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            map.insert(k.clone(), v.clone());
        }
    }
    (
        Node {
            id: gap_id.clone(),
            label: "Gap".into(),
            props,
        },
        Edge {
            src: src.to_string(),
            dst: gap_id.clone(),
            label: label.into(),
            props: serde_json::json!({
                "reason": reason,
                "attempted_resolution": "entity-type",
                "prov": cx.prov_spanning(
                    evidence,
                    ConfidenceTier::Gap,
                    &format!("{label} -> {gap_id}"),
                ),
            }),
        },
    )
}

fn cardinality(annotation: &str) -> Option<&'static str> {
    match annotation {
        "OneToMany" => Some("one-to-many"),
        "ManyToOne" => Some("many-to-one"),
        "OneToOne" => Some("one-to-one"),
        "ManyToMany" => Some("many-to-many"),
        _ => None,
    }
}

/// Extract JPA entities, relations, repository bindings and repository call
/// sites from one parsed Java file.
pub(crate) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    language: &tree_sitter::Language,
    imports: &Imports,
    package: Option<&str>,
    methods_by_start: &HashMap<usize, String>,
    out: &mut Extraction,
) {
    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Query, QueryCursor};

    let query = Query::new(
        language,
        "[(class_declaration) @class (interface_declaration) @interface]",
    )
    .expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    let mut classes = Vec::new();
    let mut interfaces = Vec::new();
    while let Some(found) = matches.next() {
        for capture in found.captures() {
            match query.capture_names()[capture.index as usize] {
                "class" => classes.push(capture.node),
                _ => interfaces.push(capture.node),
            }
        }
    }
    for class in classes {
        entity(cx, class, imports, package, out);
    }
    for interface in interfaces {
        repository(cx, interface, imports, package, out);
    }
    repository_calls(cx, root, language, imports, package, methods_by_start, out);
}

fn qualified_name(cx: &FileCx<'_>, declaration: TsNode<'_>) -> Option<(String, String)> {
    let name = cx
        .text(&declaration.child_by_field_name("name")?)
        .to_string();
    let mut chain = enclosing_type_chain(cx, declaration);
    chain.push(name);
    Some((chain.join("."), chain.last().cloned().unwrap_or_default()))
}

fn entity(
    cx: &FileCx<'_>,
    class: TsNode<'_>,
    imports: &Imports,
    package: Option<&str>,
    out: &mut Extraction,
) {
    let annotations = persistence_annotations(cx, class, imports);
    let Some((_, entity_annotation)) = annotations.iter().find(|(name, _)| name == "Entity") else {
        return;
    };
    let Some((qualified, simple)) = qualified_name(cx, class) else {
        return;
    };
    let fqn = match package {
        Some(package) => format!("{package}.{qualified}"),
        None => qualified.clone(),
    };
    let repo = cx.id.repo;

    // Entity name: `@Entity(name = …)`, else the unqualified class name.
    let name = match element_str(cx, *entity_annotation, "name") {
        Str::Literal(name) => name,
        Str::Absent => simple,
        Str::Dynamic => {
            let (node, edge) = gap_pair(
                cx,
                &symbol_id(repo, cx.path, &qualified),
                "REFERENCES",
                &[*entity_annotation],
                "runtime-computed JPA entity name",
                serde_json::json!({ "raw": cx.text(entity_annotation) }),
            );
            out.nodes.push(node);
            out.edges.push(edge);
            return;
        }
    };
    let entity_id = format!("data:{repo}@jpa:{name}");

    // Table: `@Table(name = …)`, else JPA's default (the entity name).
    let table_annotation = annotations
        .iter()
        .find(|(n, _)| n == "Table")
        .map(|(_, node)| *node);
    let mut evidence = vec![*entity_annotation];
    let mut table_source = "default";
    let mut table = Some(name.clone());
    let mut schema = None;
    if let Some(table_annotation) = table_annotation {
        evidence.push(table_annotation);
        schema = literal(cx, table_annotation, "schema");
        match element_str(cx, table_annotation, "name") {
            Str::Literal(explicit) => {
                table = Some(explicit);
                table_source = "explicit";
            }
            Str::Absent => {}
            Str::Dynamic => {
                table = None;
                table_source = "unresolved";
                let (node, edge) = gap_pair(
                    cx,
                    &entity_id,
                    "REFERENCES",
                    &[table_annotation],
                    "runtime-computed JPA table name",
                    serde_json::json!({ "raw": cx.text(&table_annotation) }),
                );
                out.nodes.push(node);
                out.edges.push(edge);
            }
        }
    }

    // Fields (own declarations only): name, spelled type, `@Id`, column.
    let mut fields = Vec::new();
    let mut relations = Vec::new();
    if let Some(body) = class.child_by_field_name("body") {
        let mut walk = body.walk();
        for member in body.named_children(&mut walk) {
            if member.kind() != "field_declaration" {
                continue;
            }
            let field_annotations = persistence_annotations(cx, member, imports);
            if has_modifier(member, "static")
                || has_modifier(member, "transient")
                || field_annotations.iter().any(|(n, _)| n == "Transient")
            {
                continue;
            }
            let Some(type_node) = member.child_by_field_name("type") else {
                continue;
            };
            let spelled = normalize_type(cx, type_node);
            let is_id = field_annotations
                .iter()
                .any(|(n, _)| matches!(n.as_str(), "Id" | "EmbeddedId"));
            let column = field_annotations
                .iter()
                .find(|(n, _)| n == "Column")
                .and_then(|(_, a)| literal(cx, *a, "name"));
            let relation = field_annotations
                .iter()
                .find(|(n, _)| cardinality(n).is_some());
            let mut declarators = member.walk();
            for declarator in member.named_children(&mut declarators) {
                if declarator.kind() != "variable_declarator" {
                    continue;
                }
                let Some(field_name) = declarator.child_by_field_name("name") else {
                    continue;
                };
                let field_name = cx.text(&field_name).to_string();
                let mut field = serde_json::json!({
                    "name": field_name,
                    "type": spelled,
                    "id": is_id,
                });
                if let Some(column) = &column {
                    field["column"] = column.clone().into();
                }
                if let Some((annotation, _)) = relation {
                    field["relation"] = (*annotation).clone().into();
                    relations.push((member, type_node, field_name, field_annotations.clone()));
                }
                fields.push(field);
            }
        }
    }

    let mut props = serde_json::json!({
        "name": name,
        "storage": "jpa",
        "table_source": table_source,
        "fqn": fqn,
        "fields": fields,
        "framework": "jpa",
        "language": "java",
        "prov": cx.prov_spanning(
            &evidence,
            ConfidenceTier::Confirmed,
            &format!("DataEntity {entity_id}"),
        ),
    });
    if let Some(table) = table {
        props["table"] = table.clone().into();
        props["store"] = table.into();
    }
    if let Some(schema) = schema {
        props["schema"] = schema.into();
    }
    out.nodes.push(Node {
        id: entity_id.clone(),
        label: "DataEntity".into(),
        props,
    });
    // The entity name is not unique across packages: two classes may claim
    // one entity id. The join decides whether this id collides, but it cannot
    // recompute a source span — so the Gap's provenance is built here.
    let duplicate_prov = cx.prov_spanning(
        &evidence,
        ConfidenceTier::Gap,
        &format!("Gap duplicate {entity_id}"),
    );
    out.entity_index.push(EntityDecl {
        fqn,
        id: entity_id.clone(),
        duplicate_prov,
    });

    // Relations, one REFERENCES edge per annotated field.
    for (member, type_node, field_name, field_annotations) in relations {
        let Some((annotation_name, annotation)) = field_annotations
            .iter()
            .find(|(n, _)| cardinality(n).is_some())
        else {
            continue;
        };
        let cardinality_label = cardinality(annotation_name).expect("filtered above");
        let to_many = matches!(annotation_name.as_str(), "OneToMany" | "ManyToMany");
        let target = match element(cx, *annotation, "targetEntity") {
            Some(value) if value.kind() == "class_literal" => {
                let mut walk = value.walk();
                value
                    .named_children(&mut walk)
                    .find_map(|c| type_name(cx, c))
            }
            Some(_) => None,
            None if to_many => last_type_argument(type_node).and_then(|t| type_name(cx, t)),
            None => type_name(cx, type_node),
        };
        let join_column = field_annotations
            .iter()
            .find(|(n, _)| n == "JoinColumn")
            .and_then(|(_, a)| literal(cx, *a, "name"));
        let join_table = field_annotations
            .iter()
            .find(|(n, _)| n == "JoinTable")
            .and_then(|(_, a)| literal(cx, *a, "name"));
        let mapped_by = literal(cx, *annotation, "mappedBy");
        let fact = format!("REFERENCES {entity_id} .{field_name}");
        let mut edge_props = serde_json::json!({
            "relation": annotation_name,
            "cardinality": cardinality_label,
            "field": field_name,
            "owning": element(cx, *annotation, "mappedBy").is_none(),
            "framework": "jpa",
            "prov": cx.prov(&member, &fact),
        });
        if let Some(mapped_by) = mapped_by {
            edge_props["mapped_by"] = mapped_by.into();
        }
        if let Some(join_column) = join_column {
            edge_props["join_column"] = join_column.into();
        }
        if let Some(join_table) = join_table {
            edge_props["join_table"] = join_table.into();
        }
        let Some(target) = target else {
            let (node, edge) = gap_pair(
                cx,
                &entity_id,
                "REFERENCES",
                &[member],
                "JPA relation target type is not provable",
                serde_json::json!({ "field": field_name }),
            );
            out.nodes.push(node);
            out.edges.push(edge);
            continue;
        };
        out.pending_links.push(PendingLink {
            candidates: type_candidates(&target, imports, package),
            edge: Edge {
                src: entity_id.clone(),
                dst: String::new(),
                label: "REFERENCES".into(),
                props: edge_props,
            },
            gap: gap_pair(
                cx,
                &entity_id,
                "REFERENCES",
                &[member],
                "JPA relation target is not a recovered entity",
                serde_json::json!({ "field": field_name, "target": target }),
            ),
            repository: None,
        });
    }
}

fn repository(
    cx: &FileCx<'_>,
    interface: TsNode<'_>,
    imports: &Imports,
    package: Option<&str>,
    out: &mut Extraction,
) {
    let Some((qualified, _)) = qualified_name(cx, interface) else {
        return;
    };
    let mut parameters = BTreeSet::new();
    let mut extends = None;
    let mut walk = interface.walk();
    for child in interface.named_children(&mut walk) {
        match child.kind() {
            "type_parameters" => {
                let mut inner = child.walk();
                for parameter in child.named_children(&mut inner) {
                    if let Some(name) = parameter.named_child(0) {
                        parameters.insert(cx.text(&name).to_string());
                    }
                }
            }
            "extends_interfaces" => extends = Some(child),
            _ => {}
        }
    }
    let Some(extends) = extends else { return };
    let mut supers = Vec::new();
    let mut walk = extends.walk();
    for child in extends.named_children(&mut walk) {
        if child.kind() == "type_list" {
            let mut inner = child.walk();
            supers.extend(child.named_children(&mut inner));
        } else {
            supers.push(child);
        }
    }
    for supertype in supers {
        let Some(full) = type_name(cx, supertype) else {
            continue;
        };
        let (prefix, simple) = match full.rsplit_once('.') {
            Some((prefix, simple)) => (Some(prefix), simple.to_string()),
            None => (None, full.clone()),
        };
        let Some(declaring) = repository_package(&simple) else {
            continue;
        };
        let proven = match prefix {
            Some(prefix) => prefix == declaring,
            None => match imports.bindings.get(&simple) {
                Some(fqn) => *fqn == format!("{declaring}.{simple}"),
                None => imports.wildcard_packages.iter().any(|w| w == declaring),
            },
        };
        if !proven || supertype.kind() != "generic_type" {
            continue;
        }
        // First type argument is the managed entity type.
        let mut walk = supertype.walk();
        let Some(arguments) = supertype
            .named_children(&mut walk)
            .find(|c| c.kind() == "type_arguments")
        else {
            continue;
        };
        let Some(entity_type) = arguments.named_child(0) else {
            continue;
        };
        let Some(target) = type_name(cx, entity_type) else {
            continue;
        };
        if parameters.contains(&target) {
            // A type variable of a generic base repository names no entity.
            continue;
        }
        let symbol = symbol_id(cx.id.repo, cx.path, &qualified);
        let fqn = match package {
            Some(package) => format!("{package}.{qualified}"),
            None => qualified.clone(),
        };
        out.repository_index
            .push(RepositoryDecl { fqn: fqn.clone() });
        out.pending_links.push(PendingLink {
            candidates: type_candidates(&target, imports, package),
            edge: Edge {
                src: symbol.clone(),
                dst: String::new(),
                label: "REFERENCES".into(),
                props: serde_json::json!({
                    "relation": "repository",
                    "framework": "spring-data",
                    "repository": simple,
                    "entity_type": target,
                    "prov": cx.prov(
                        &supertype,
                        &format!("REFERENCES {symbol} -> {target}"),
                    ),
                }),
            },
            gap: gap_pair(
                cx,
                &symbol,
                "REFERENCES",
                &[supertype],
                "Spring Data repository entity is not a recovered entity",
                serde_json::json!({ "target": target }),
            ),
            repository: Some(fqn),
        });
        // One entity binding per repository interface.
        break;
    }
}

/// Spring Data method-name vocabulary: `Some(false)` reads, `Some(true)`
/// writes. The verb must end the name or be followed by an uppercase letter
/// or digit (`findAll`, `findByName`, `deleteById`; not `savings`). Custom
/// `@Query`/`@Modifying` methods outside this vocabulary assert nothing.
fn classify(method: &str) -> Option<bool> {
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

/// A name bound at a call site: the declared name and the type it is
/// declared with. `ty` is `None` when the spelling proves no type — an
/// untyped lambda parameter — which fails the fact closed.
struct Binding {
    ty: Option<String>,
}

/// The binding of `name` in a declaration that carries `name` and `type`
/// fields (a formal parameter, an enhanced-for variable, a
/// try-with-resources resource).
fn field_binding(cx: &FileCx<'_>, node: TsNode<'_>, name: &str) -> Option<Binding> {
    let declared = node.child_by_field_name("name")?;
    if cx.text(&declared) != name {
        return None;
    }
    Some(Binding {
        ty: node
            .child_by_field_name("type")
            .and_then(|ty| type_name(cx, ty)),
    })
}

/// The binding of `name` in a local variable declaration.
fn declaration_binding(cx: &FileCx<'_>, declaration: TsNode<'_>, name: &str) -> Option<Binding> {
    let mut walk = declaration.walk();
    for declarator in declaration.named_children(&mut walk) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(declared) = declarator.child_by_field_name("name") else {
            continue;
        };
        if cx.text(&declared) != name {
            continue;
        }
        return Some(Binding {
            ty: declaration
                .child_by_field_name("type")
                .and_then(|ty| type_name(cx, ty)),
        });
    }
    None
}

/// The binding of `name` in a catch parameter. A multi-catch
/// (`catch (A | B e)`) declares no single type, so it proves none.
fn catch_binding(cx: &FileCx<'_>, clause: TsNode<'_>, name: &str) -> Option<Binding> {
    let mut walk = clause.walk();
    for child in clause.named_children(&mut walk) {
        if child.kind() != "catch_formal_parameter" {
            continue;
        }
        let Some(declared) = child.child_by_field_name("name") else {
            continue;
        };
        if cx.text(&declared) != name {
            continue;
        }
        let mut types = Vec::new();
        let mut inner = child.walk();
        for caught in child.named_children(&mut inner) {
            if caught.kind() != "catch_type" {
                continue;
            }
            let mut walk = caught.walk();
            for ty in caught.named_children(&mut walk) {
                if let Some(ty) = type_name(cx, ty) {
                    types.push(ty);
                }
            }
        }
        return Some(Binding {
            ty: if types.len() == 1 {
                types.into_iter().next()
            } else {
                None
            },
        });
    }
    None
}

/// The binding of `name` in a lambda's parameters. An inferred (untyped)
/// parameter binds the name but proves no type.
fn lambda_binding(cx: &FileCx<'_>, lambda: TsNode<'_>, name: &str) -> Option<Binding> {
    let parameters = lambda.child_by_field_name("parameters")?;
    match parameters.kind() {
        "identifier" => (cx.text(&parameters) == name).then_some(Binding { ty: None }),
        "inferred_parameters" => {
            let mut walk = parameters.walk();
            let bound = parameters
                .named_children(&mut walk)
                .any(|child| child.kind() == "identifier" && cx.text(&child) == name);
            bound.then_some(Binding { ty: None })
        }
        "formal_parameters" => {
            let mut walk = parameters.walk();
            for parameter in parameters.named_children(&mut walk) {
                if parameter.kind() != "formal_parameter" {
                    continue;
                }
                if let Some(binding) = field_binding(cx, parameter, name) {
                    return Some(binding);
                }
            }
            None
        }
        _ => None,
    }
}

/// The names bound by a pattern variable anywhere in `node`: an
/// `instanceof Foo x` binding, a record pattern component or a switch
/// pattern. Flow scoping is not modelled, so a receiver bound by one of
/// these names asserts nothing (#702 review).
fn collect_pattern_names(cx: &FileCx<'_>, node: TsNode<'_>, into: &mut BTreeSet<String>) {
    match node.kind() {
        "type_pattern" | "record_pattern_component" => {
            let mut walk = node.walk();
            for child in node.named_children(&mut walk) {
                if child.kind() == "identifier" {
                    into.insert(cx.text(&child).to_string());
                }
            }
        }
        "instanceof_expression" => {
            if let Some(name) = node.child_by_field_name("name") {
                into.insert(cx.text(&name).to_string());
            }
        }
        _ => {}
    }
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        collect_pattern_names(cx, child, into);
    }
}

/// The binding of `name` that is visible at `call`, if any.
///
/// Scopes are searched innermost first, so the nearest declaration wins
/// (Java's shadowing rule): a parameter of the enclosing method,
/// constructor or lambda, a catch parameter, an enhanced-for variable, a
/// try-with-resources resource, or a local declared in an enclosing block
/// before the call. A declaration that starts after the call, or that
/// lives in a sibling block, is not visible there — the receiver is then
/// the field, as Java resolves it (#702 review).
fn visible_binding(
    cx: &FileCx<'_>,
    method: TsNode<'_>,
    call: TsNode<'_>,
    name: &str,
) -> Option<Binding> {
    let mut node = call;
    while let Some(parent) = node.parent() {
        match parent.kind() {
            // Constructor bodies and switch groups hold statements exactly
            // like a block does, so their locals scope the same way.
            "block" | "constructor_body" | "switch_block_statement_group" => {
                let mut walk = parent.walk();
                for child in parent.named_children(&mut walk) {
                    if child.kind() != "local_variable_declaration" {
                        continue;
                    }
                    // A local is in scope from its declaration to the end of
                    // its block: one declared later captures no earlier call.
                    if child.start_byte() > call.start_byte() {
                        continue;
                    }
                    if let Some(binding) = declaration_binding(cx, child, name) {
                        return Some(binding);
                    }
                }
            }
            "catch_clause" => {
                if let Some(binding) = catch_binding(cx, parent, name) {
                    return Some(binding);
                }
            }
            "enhanced_for_statement" => {
                if let Some(binding) = field_binding(cx, parent, name) {
                    return Some(binding);
                }
            }
            "try_with_resources_statement" => {
                let mut walk = parent.walk();
                for child in parent.named_children(&mut walk) {
                    if child.kind() != "resource_specification" {
                        continue;
                    }
                    let mut inner = child.walk();
                    for resource in child.named_children(&mut inner) {
                        if resource.kind() != "resource" {
                            continue;
                        }
                        if let Some(binding) = field_binding(cx, resource, name) {
                            return Some(binding);
                        }
                    }
                }
            }
            "lambda_expression" => {
                if let Some(binding) = lambda_binding(cx, parent, name) {
                    return Some(binding);
                }
            }
            "for_statement" => {
                let mut walk = parent.walk();
                for child in parent.named_children(&mut walk) {
                    if child.kind() != "local_variable_declaration" {
                        continue;
                    }
                    if let Some(binding) = declaration_binding(cx, child, name) {
                        return Some(binding);
                    }
                }
            }
            "method_declaration" | "constructor_declaration" => {
                if let Some(parameters) = parent.child_by_field_name("parameters") {
                    let mut walk = parameters.walk();
                    for parameter in parameters.named_children(&mut walk) {
                        if !matches!(parameter.kind(), "formal_parameter" | "spread_parameter") {
                            continue;
                        }
                        if let Some(binding) = field_binding(cx, parameter, name) {
                            return Some(binding);
                        }
                    }
                }
                // The enclosing method ends the search: nothing outside it
                // is visible here.
                if parent.start_byte() == method.start_byte() {
                    break;
                }
            }
            _ => {}
        }
        node = parent;
    }
    None
}

fn collect_fields(
    cx: &FileCx<'_>,
    declaration: TsNode<'_>,
    into: &mut HashMap<String, BTreeSet<String>>,
) {
    let Some(body) = declaration.child_by_field_name("body") else {
        return;
    };
    let mut walk = body.walk();
    for member in body.named_children(&mut walk) {
        if member.kind() != "field_declaration" {
            continue;
        }
        let Some(ty) = member
            .child_by_field_name("type")
            .and_then(|ty| type_name(cx, ty))
        else {
            continue;
        };
        let mut inner = member.walk();
        for declarator in member.named_children(&mut inner) {
            if declarator.kind() == "variable_declarator"
                && let Some(name) = declarator.child_by_field_name("name")
            {
                into.entry(cx.text(&name).to_string())
                    .or_default()
                    .insert(ty.clone());
            }
        }
    }
}

/// Calls of the form `receiver.verb(…)` / `this.receiver.verb(…)` with a
/// Spring Data vocabulary verb, on a receiver whose type is declared in this
/// file (a local, parameter, or field of the enclosing type).
fn repository_calls(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    language: &tree_sitter::Language,
    imports: &Imports,
    package: Option<&str>,
    methods_by_start: &HashMap<usize, String>,
    out: &mut Extraction,
) {
    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Query, QueryCursor};

    let query = Query::new(language, "(method_invocation) @call").expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    let mut patterns: HashMap<usize, BTreeSet<String>> = HashMap::new();
    let mut fields: HashMap<usize, HashMap<String, BTreeSet<String>>> = HashMap::new();
    while let Some(found) = matches.next() {
        let call = found.captures()[0].node;
        let (Some(object), Some(method)) = (
            call.child_by_field_name("object"),
            call.child_by_field_name("name"),
        ) else {
            continue;
        };
        let method = cx.text(&method).to_string();
        let Some(writes) = classify(&method) else {
            continue;
        };
        // `x.verb()` (local, parameter or field) or `this.x.verb()` (field).
        let (receiver, this_only) = match object.kind() {
            "identifier" => (cx.text(&object).to_string(), false),
            "field_access" => {
                let (Some(base), Some(field)) = (
                    object.child_by_field_name("object"),
                    object.child_by_field_name("field"),
                ) else {
                    continue;
                };
                if base.kind() != "this" {
                    continue;
                }
                (cx.text(&field).to_string(), true)
            }
            _ => continue,
        };
        // Enclosing method symbol (the edge source) and its declaration.
        let mut enclosing = None;
        let mut node = call;
        while let Some(parent) = node.parent() {
            if matches!(
                parent.kind(),
                "method_declaration" | "constructor_declaration"
            ) && let Some(symbol) = methods_by_start.get(&parent.start_byte())
            {
                enclosing = Some((parent, symbol.clone()));
                break;
            }
            node = parent;
        }
        let Some((method_node, src)) = enclosing else {
            continue;
        };
        let mut types: Option<BTreeSet<String>> = None;
        if !this_only {
            // A name bound by a pattern variable proves no type: flow
            // scoping is not modelled, so the call asserts nothing at all
            // rather than falling back to a field of the same name.
            let bound = patterns.entry(method_node.start_byte()).or_insert_with(|| {
                let mut names = BTreeSet::new();
                collect_pattern_names(cx, method_node, &mut names);
                names
            });
            if bound.contains(&receiver) {
                continue;
            }
            // The receiver is the declaration visible at the call site —
            // not every declaration in the method.
            match visible_binding(cx, method_node, call, &receiver) {
                // A binding with no proven type (an untyped lambda parameter)
                // fails closed too.
                Some(Binding { ty: Some(ty) }) => types = Some(BTreeSet::from([ty])),
                Some(Binding { ty: None }) => continue,
                None => {}
            }
        }
        if types.is_none() {
            let mut node = call;
            while let Some(parent) = node.parent() {
                if TYPE_KINDS.contains(&parent.kind()) {
                    let table = fields.entry(parent.start_byte()).or_insert_with(|| {
                        let mut table = HashMap::new();
                        collect_fields(cx, parent, &mut table);
                        table
                    });
                    if let Some(found) = table.get(&receiver) {
                        types = Some(found.clone());
                        break;
                    }
                    // `this.x` names a field of the immediately enclosing
                    // type: an enclosing type's field of the same name is a
                    // different receiver, so the search stops here whether or
                    // not the field was found.
                    if this_only {
                        break;
                    }
                }
                node = parent;
            }
        }
        // A name declared with several types in one scope is ambiguous.
        let Some(types) = types.filter(|t| t.len() == 1) else {
            continue;
        };
        let ty = types.into_iter().next().expect("one type");
        out.pending_repo_calls.push(PendingRepoCall {
            src,
            candidates: type_candidates(&ty, imports, package),
            method: method.clone(),
            writes,
            prov: cx.prov(
                &call,
                &format!(
                    "{} {receiver}.{method}",
                    if writes { "WRITES" } else { "READS" }
                ),
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// Directory join
// ---------------------------------------------------------------------------

/// An entity id claimed by more than one class FQN: the entity name is not
/// unique across packages, so the id would merge two classes into one fact.
#[derive(Debug, Clone)]
struct DuplicateEntity {
    /// The colliding DataEntity id.
    id: String,
    /// Every class FQN claiming it, sorted.
    fqns: Vec<String>,
    /// Provenance of the Gap that replaces the merged node.
    prov: serde_json::Value,
}

/// The entity ids that more than one declaration claims, with the claiming
/// FQNs sorted and the provenance of the first one in that order.
///
/// A collision is counted per declaration, not per distinct FQN: two source
/// roots that declare the same class claim one DataEntity id just as two
/// packages that declare the same entity name do (#702 review).
fn duplicate_entities(index: &[EntityDecl]) -> Vec<DuplicateEntity> {
    let mut by_id: BTreeMap<&str, Vec<&EntityDecl>> = BTreeMap::new();
    for decl in index {
        by_id.entry(decl.id.as_str()).or_default().push(decl);
    }
    by_id
        .into_iter()
        .filter_map(|(id, mut decls)| {
            if decls.len() < 2 {
                return None;
            }
            // Every claiming FQN, sorted, duplicates included: the Gap names
            // the declarations that collide, not the distinct names.
            let mut fqns: Vec<String> = decls.iter().map(|decl| decl.fqn.clone()).collect();
            fqns.sort();
            // The first FQN in sorted order supplies the Gap's provenance:
            // the join cannot recompute a source span.
            decls.sort_by(|a, b| a.fqn.cmp(&b.fqn));
            Some(DuplicateEntity {
                id: id.to_string(),
                fqns,
                prov: decls
                    .first()
                    .expect("a claimed id has at least one declaration")
                    .duplicate_prov
                    .clone(),
            })
        })
        .collect()
}

/// The Gap node that stands in for a colliding entity id.
fn duplicate_gap_id(entity_id: &str) -> String {
    format!("gap:jpa:duplicate:{entity_id}")
}

/// The relation metadata of one field, as a relation edge carries it: the
/// same keys the per-field edge emits today.
fn relation_props(edge: &Edge) -> serde_json::Value {
    let mut props = serde_json::Map::new();
    for key in [
        "field",
        "relation",
        "cardinality",
        "owning",
        "mapped_by",
        "join_column",
        "join_table",
    ] {
        if let Some(value) = edge.props.get(key) {
            props.insert(key.to_string(), value.clone());
        }
    }
    serde_json::Value::Object(props)
}

/// Whether `edge` is a JPA relation edge — not a repository binding, not a
/// relation Gap.
fn is_relation_edge(edge: &Edge) -> bool {
    edge.label == "REFERENCES"
        && edge.props.get("framework").and_then(|v| v.as_str()) == Some("jpa")
}

/// The relation field an edge declares.
fn relation_field(edge: &Edge) -> &str {
    edge.props
        .get("field")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

/// Provenance citing every relation field's evidence, built the way the
/// file builds multi-span provenance: one span per proving declaration, one
/// deterministic hash over the merged fact.
fn merged_relation_prov(edges: &[&Edge], relations: &[serde_json::Value]) -> serde_json::Value {
    let mut evidence: Vec<EvidenceRef> = Vec::new();
    for edge in edges {
        let Ok(prov) = serde_json::from_value::<Provenance>(edge.props["prov"].clone()) else {
            continue;
        };
        evidence.extend(prov.evidence);
    }
    let fact = serde_json::to_vec(&serde_json::json!({ "relations": relations }))
        .expect("relations serialize");
    let provenance = Provenance::new(
        Tier::Deterministic,
        ConfidenceTier::Confirmed,
        evidence,
        EXTRACTOR_ID,
        &fact,
    )
    .expect("Deterministic confidence is within its ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

/// One edge for the relation fields one DataEntity declares to the same
/// target entity. Several fields to one target are one fact: the
/// reconciler keeps one edge per `(src, dst, label)` and would drop every
/// field but one (#702 review). The per-field metadata moves into
/// `relations` — sorted by field name — and the provenance cites every
/// field's evidence.
fn merge_relation_group(edges: &[Edge]) -> Edge {
    let mut ordered: Vec<&Edge> = edges.iter().collect();
    ordered.sort_by(|a, b| relation_field(a).cmp(relation_field(b)));
    let relations: Vec<serde_json::Value> =
        ordered.iter().map(|edge| relation_props(edge)).collect();
    let mut merged = (*ordered.first().expect("a group holds an edge")).clone();
    merged.props = serde_json::json!({
        "framework": "jpa",
        "relations": relations,
        "prov": merged_relation_prov(&ordered, &relations),
    });
    merged
}

/// Aggregate the relation edges one DataEntity declares to the same target
/// entity into one edge. A single-field relation keeps the shape it has
/// today and gains the same `relations` list, so a consumer reads the
/// per-field metadata in one place either way.
fn aggregate_relation_edges(edges: &mut Vec<Edge>) {
    let mut slots: Vec<Vec<Edge>> = Vec::new();
    let mut groups: BTreeMap<(String, String), usize> = BTreeMap::new();
    for edge in edges.drain(..) {
        if is_relation_edge(&edge) {
            let key = (edge.src.clone(), edge.dst.clone());
            match groups.get(&key) {
                Some(index) => slots[*index].push(edge),
                None => {
                    groups.insert(key, slots.len());
                    slots.push(vec![edge]);
                }
            }
        } else {
            slots.push(vec![edge]);
        }
    }
    let mut merged: Vec<Edge> = Vec::with_capacity(slots.len());
    for slot in slots {
        if slot.len() == 1 {
            let mut edge = slot.into_iter().next().expect("a slot holds one edge");
            if is_relation_edge(&edge) {
                let relations = serde_json::json!([relation_props(&edge)]);
                edge.props["relations"] = relations;
            }
            merged.push(edge);
        } else {
            merged.push(merge_relation_group(&slot));
        }
    }
    *edges = merged;
}

/// Resolve pending links and repository call sites against the repo-wide
/// entity and repository indexes. A target resolves only when exactly one of
/// its candidate FQNs is declared exactly once; otherwise the link fails
/// closed to its Gap.
pub(crate) fn resolve(out: &mut Extraction) {
    // An entity name shared by classes in different packages claims one
    // DataEntity id. The merged node asserts facts no single class proves,
    // so it is replaced by one explicit Gap and the id resolves to nothing.
    let duplicates = duplicate_entities(&out.entity_index);
    let collided: BTreeSet<&str> = duplicates.iter().map(|d| d.id.as_str()).collect();
    if !duplicates.is_empty() {
        out.nodes
            .retain(|node| !(node.label == "DataEntity" && collided.contains(node.id.as_str())));
        for duplicate in &duplicates {
            out.nodes.push(Node {
                id: duplicate_gap_id(&duplicate.id),
                label: "Gap".into(),
                props: serde_json::json!({
                    "reason": "JPA entity name is declared by more than one class",
                    "attempted_tiers": ["T0"],
                    "framework": "jpa",
                    "language": "java",
                    "fqns": duplicate.fqns,
                    "prov": duplicate.prov,
                }),
            });
        }
    }

    let mut entities: BTreeMap<String, Option<String>> = BTreeMap::new();
    for decl in &out.entity_index {
        entities
            .entry(decl.fqn.clone())
            .and_modify(|id| *id = None)
            .or_insert_with(|| Some(decl.id.clone()));
    }
    // A colliding entity resolves to no class: links to it fail closed.
    for duplicate in &duplicates {
        for fqn in &duplicate.fqns {
            entities.insert(fqn.clone(), None);
        }
    }
    let mut repositories: BTreeMap<String, bool> = BTreeMap::new();
    for decl in &out.repository_index {
        repositories
            .entry(decl.fqn.clone())
            .and_modify(|unique| *unique = false)
            .or_insert(true);
    }
    let unique_target = |candidates: &[String]| -> Option<String> {
        let mut found = candidates
            .iter()
            .filter_map(|c| entities.get(c.as_str()))
            .collect::<Vec<_>>();
        found.dedup();
        match found.as_slice() {
            [Some(id)] => Some(id.clone()),
            _ => None,
        }
    };

    let mut repository_entity: BTreeMap<String, Option<String>> = BTreeMap::new();
    for link in std::mem::take(&mut out.pending_links) {
        let PendingLink {
            candidates,
            mut edge,
            gap,
            repository,
        } = link;
        // A relation owned by a colliding entity names no single class, so
        // it fails closed like a relation to one.
        let ambiguous_source = collided.contains(edge.src.as_str());
        match unique_target(&candidates) {
            Some(id) if !ambiguous_source => {
                edge.dst = id.clone();
                if let Some(repository) = repository {
                    // Two bindings for one repository are ambiguous.
                    repository_entity
                        .entry(repository)
                        .and_modify(|bound| *bound = None)
                        .or_insert(Some(id));
                }
                out.edges.push(edge);
            }
            _ => {
                out.nodes.push(gap.0);
                out.edges.push(gap.1);
            }
        }
    }

    // Several relation fields to the same target entity are one fact: the
    // reconciler keeps one edge per `(src, dst, label)`, so separate edges
    // would collapse into whichever field came last (#702 review).
    aggregate_relation_edges(&mut out.edges);

    // Every call site is its own occurrence: the reconciler unions the
    // evidence of repeated `(src, dst, label)` edges, so dropping the later
    // ones here would lose their citations and their fact hash (#702
    // review).
    for call in std::mem::take(&mut out.pending_repo_calls) {
        let matched: Vec<&String> = call
            .candidates
            .iter()
            .filter(|c| repositories.get(c.as_str()) == Some(&true))
            .collect();
        let [repository] = matched.as_slice() else {
            continue;
        };
        let Some(Some(entity)) = repository_entity.get(repository.as_str()) else {
            continue;
        };
        let label = if call.writes { "WRITES" } else { "READS" };
        out.edges.push(Edge {
            src: call.src,
            dst: entity.clone(),
            label: label.into(),
            props: serde_json::json!({
                "resolution": "declared-type",
                "framework": "spring-data",
                "method": call.method,
                "prov": call.prov,
            }),
        });
    }

    // No edge may stay sourced at a removed DataEntity: the duplicate Gap
    // takes its place, so every edge keeps a live endpoint.
    for edge in &mut out.edges {
        if collided.contains(edge.src.as_str()) {
            edge.src = duplicate_gap_id(&edge.src);
        }
    }
}
