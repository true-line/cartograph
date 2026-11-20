//! SQLAlchemy declarative data model (#738): deterministic T0 `DataEntity`
//! nodes from import-proven declarative bases (`sqlalchemy.orm` /
//! `sqlalchemy.ext.declarative` `declarative_base()`, `DeclarativeBase`,
//! `registry().generate_base()`, Flask-SQLAlchemy `db.Model`, and subclasses
//! across files), `REFERENCES` edges from literal `ForeignKey("table.col")`,
//! attribute `ForeignKey(Model.col)` naming a recovered model's declared column,
//! and `relationship("Target")` / `relationship(Target)`, and explicit `Gap`
//! nodes whenever a table name or relation target cannot be proven (R-INT-4).

use super::{
    BTreeMap, BTreeSet, ConfidenceTier, Edge, EvidenceRef, Extraction, FileCx, HashMap,
    ImportBinding, Node, TsNode, literal_string, module_file, named_arguments, prov_from,
    sqlalchemy_writes,
};
use std::path::{Path, PathBuf};
use streaming_iterator::StreamingIterator;
use tree_sitter::QueryCursor;

/// Per-file SQLAlchemy declarations collected during the file parse and joined
/// across the directory once every file is known.
#[derive(Debug, Clone)]
pub(crate) struct OrmFile {
    pub(crate) path: String,
    pub(crate) imports: HashMap<String, ImportBinding>,
    pub(crate) proven_bases: BTreeSet<String>,
    pub(crate) proven_registries: BTreeSet<String>,
    pub(crate) proven_flask_dbs: BTreeSet<String>,
    pub(crate) proven_sessions: BTreeSet<String>,
    /// Names bound more than once at module level (imports, `def`/`class`,
    /// assignments): no session proof survives a rebinding.
    pub(crate) rebound: BTreeSet<String>,
    assignments: Vec<(String, ModuleRhs)>,
    pub(crate) classes: Vec<OrmClassDecl>,
    pub(crate) functions: Vec<sqlalchemy_writes::FunctionWrites>,
}

/// Module-level assignment RHS that may establish a proven base after
/// cross-file import resolution.
#[derive(Debug, Clone)]
enum ModuleRhs {
    /// `Base = <recv>.generate_base(...)` where `<recv>` is a `registry()` instance.
    GenerateBase(String),
    /// `Base = <recv>.Model` where `<recv>` is a `SQLAlchemy(...)` instance.
    DotModel(String),
    /// `Session = <recv>.session` where `<recv>` is a `SQLAlchemy(...)` instance.
    DotSession(String),
    /// `Base = <other>` aliasing another proven base, registry, db, or session.
    Alias(String),
}

/// Superclass expression in a `class Name(...):` declaration.
#[derive(Debug, Clone)]
pub(crate) enum BaseExpr {
    /// Direct `DeclarativeBase` from `sqlalchemy.orm` / `sqlalchemy.ext.declarative`.
    DeclarativeBase,
    /// Inline `declarative_base()` or `registry().generate_base()`.
    InlineProvenBase,
    /// Inline `<recv>.generate_base()` where `<recv>` is a variable/attribute.
    GenerateBase(String),
    /// `<recv>.Model` (e.g. `db.Model` or `ext.db.Model`).
    DotModel(String),
    /// Identifier or attribute naming a base or mixin class (`Base`, `ModelMixin`, `mod.Base`).
    Symbol(String),
    /// Any base expression that cannot be resolved to a symbol or proven inline base.
    Unresolvable,
}

/// A class's `__tablename__` declaration in its body.
#[derive(Debug, Clone)]
enum TableName {
    Absent,
    Literal(String),
    Dynamic,
}

/// How an ORM member call (`Column`, `mapped_column`, `relationship`, `ForeignKey`)
/// was invoked in source, so receiver-based calls (`db.Column`) can be checked
/// against cross-file `proven_flask_dbs` during `resolve`.
#[derive(Debug, Clone)]
enum CallProof {
    /// Directly proven via file imports from `sqlalchemy`, `sqlalchemy.orm`,
    /// or `sqlalchemy.schema`.
    SqlAlchemyImport,
    /// Invoked as `<recv>.<member>`, proven iff `<recv>` resolves in `file`
    /// to an entry in `proven_flask_dbs`.
    FlaskDbReceiver { file: String, recv: String },
}

impl CallProof {
    fn is_proven(
        &self,
        imports_by_file: &BTreeMap<String, HashMap<String, ImportBinding>>,
        known_files: &BTreeSet<String>,
        proven_flask_dbs: &BTreeSet<(String, String)>,
    ) -> bool {
        match self {
            Self::SqlAlchemyImport => true,
            Self::FlaskDbReceiver { file, recv } => {
                resolve_symbol(file, recv, imports_by_file, known_files)
                    .is_some_and(|target| proven_flask_dbs.contains(&target))
            }
        }
    }
}

/// Recognized SQLAlchemy ORM member call kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrmMember {
    Column,
    MappedColumn,
    Relationship,
    ForeignKey,
}

impl OrmMember {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "Column" => Some(Self::Column),
            "mapped_column" => Some(Self::MappedColumn),
            "relationship" => Some(Self::Relationship),
            "ForeignKey" => Some(Self::ForeignKey),
            _ => None,
        }
    }
}

/// A `Column(...)` / `mapped_column(...)` declaration in a class body.
#[derive(Debug, Clone)]
struct ColumnDecl {
    name: String,
    col_type: Option<String>,
    nullable: Option<bool>,
    primary_key: Option<bool>,
    /// A literal positional or `name=` database name differs from the attribute.
    renamed: bool,
    /// That database name, when it is a literal.
    db_name: Option<String>,
    proof: CallProof,
    evidence: EvidenceRef,
}

/// Target of a `ForeignKey(...)` constraint.
#[derive(Debug, Clone)]
enum FkTarget {
    Literal(String),
    /// `Model.column` or `module.Model.column`: the class expression, the
    /// attribute, and the source text.
    Attribute(String, String, String),
    Unprovable(String),
}

/// A `ForeignKey(...)` declaration inside a column definition.
#[derive(Debug, Clone)]
struct ForeignKeyDecl {
    field: String,
    target: FkTarget,
    col_proof: CallProof,
    fk_proof: CallProof,
    evidence: EvidenceRef,
}

/// Target of a `relationship(...)` declaration.
#[derive(Debug, Clone)]
enum RelTarget {
    Name(String),
    Symbol(String),
    Unprovable(String),
}

/// A `relationship(...)` declaration in a class body.
#[derive(Debug, Clone)]
struct RelationshipDecl {
    field: String,
    target: RelTarget,
    proof: CallProof,
    evidence: EvidenceRef,
}

/// A resolved `ForeignKey` or `relationship` reference between two recovered
/// `DataEntity` nodes, accumulated per `(src_entity_id, dst_entity_id)` pair.
#[derive(Debug, Clone)]
struct ResolvedReference {
    field: String,
    relation: &'static str,
    target: String,
    target_table: Option<String>,
    target_column: Option<String>,
    evidence: EvidenceRef,
}

/// Result of searching a tablename-less model's base hierarchy for a concrete
/// parent table in single-table inheritance.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Parent {
    /// Nearest ancestor with a literal `__tablename__` is a uniquely-claimed
    /// concrete model.
    Concrete((String, String)),
    /// Nearest ancestor with a literal `__tablename__` is a candidate model
    /// whose table is claimed by more than one model.
    Ambiguous((String, String)),
    /// No ancestor with a literal `__tablename__` was found.
    None,
}

/// A Python class declaration with its candidate ORM bases, table name,
/// schema, columns, foreign keys, and relationships.
#[derive(Debug, Clone)]
pub(crate) struct OrmClassDecl {
    pub(crate) key: (String, String),
    pub(crate) name: String,
    pub(crate) bases: Vec<BaseExpr>,
    tablename: TableName,
    schema: Option<String>,
    is_abstract: bool,
    is_direct_declarative_base_subclass: bool,
    columns: Vec<ColumnDecl>,
    foreign_keys: Vec<ForeignKeyDecl>,
    relationships: Vec<RelationshipDecl>,
    pub(crate) defined_members: BTreeSet<String>,
    pub(crate) classmethods: BTreeMap<String, Vec<sqlalchemy_writes::ClassMethodWrite>>,
    evidence: EvidenceRef,
}

impl OrmClassDecl {
    /// Whether the class body itself declares a column named `name`.
    pub(crate) fn declares_column(&self, name: &str) -> bool {
        self.columns.iter().any(|column| column.name == name)
    }
}

pub(crate) fn retarget(extraction: &mut Extraction, commit: &str) {
    for file in &mut extraction.orm_files {
        for class in &mut file.classes {
            class.evidence.commit_sha = commit.into();
            for col in &mut class.columns {
                col.evidence.commit_sha = commit.into();
            }
            for fk in &mut class.foreign_keys {
                fk.evidence.commit_sha = commit.into();
            }
            for rel in &mut class.relationships {
                rel.evidence.commit_sha = commit.into();
            }
        }
        sqlalchemy_writes::retarget_file(file, commit);
    }
}

/// Resolve an identifier or dotted attribute (`expr`) against a file's import
/// bindings into `(canonical_module, exported_symbol)`.
pub(crate) fn resolve_imported_symbol(
    expr: &str,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<(String, String)> {
    let expr = expr.trim();
    if let Some((head, tail)) = expr.split_once('.') {
        let full = match bindings.get(head)? {
            ImportBinding::Module { module } => {
                if module == head || module.starts_with(&format!("{head}.")) {
                    format!("{head}.{tail}")
                } else {
                    format!("{module}.{tail}")
                }
            }
            ImportBinding::Symbol {
                module, exported, ..
            } => format!("{module}.{exported}.{tail}"),
        };
        let (mod_path, sym) = full.rsplit_once('.')?;
        Some((mod_path.to_string(), sym.to_string()))
    } else {
        match bindings.get(expr)? {
            ImportBinding::Symbol {
                module, exported, ..
            } => Some((module.clone(), exported.clone())),
            ImportBinding::Module { .. } => None,
        }
    }
}

fn is_sqlalchemy_base_module(module: &str) -> bool {
    matches!(module, "sqlalchemy.orm" | "sqlalchemy.ext.declarative")
}

fn is_declarative_base_fn(callee: &str, bindings: &HashMap<String, ImportBinding>) -> bool {
    matches!(
        resolve_imported_symbol(callee, bindings),
        Some((ref m, ref s)) if is_sqlalchemy_base_module(m) && s == "declarative_base"
    )
}

fn is_declarative_base_class(expr: &str, bindings: &HashMap<String, ImportBinding>) -> bool {
    matches!(
        resolve_imported_symbol(expr, bindings),
        Some((ref m, ref s)) if is_sqlalchemy_base_module(m) && s == "DeclarativeBase"
    )
}

fn is_registry_fn(callee: &str, bindings: &HashMap<String, ImportBinding>) -> bool {
    matches!(
        resolve_imported_symbol(callee, bindings),
        Some((ref m, ref s)) if is_sqlalchemy_base_module(m) && s == "registry"
    )
}

fn is_flask_sqlalchemy_fn(callee: &str, bindings: &HashMap<String, ImportBinding>) -> bool {
    matches!(
        resolve_imported_symbol(callee, bindings),
        Some((ref m, ref s)) if m == "flask_sqlalchemy" && s == "SQLAlchemy"
    )
}

fn is_sqlalchemy_type_name(name: &str) -> bool {
    matches!(
        name,
        "BigInteger"
            | "Boolean"
            | "Date"
            | "DateTime"
            | "Double"
            | "Enum"
            | "Float"
            | "Interval"
            | "Integer"
            | "LargeBinary"
            | "Numeric"
            | "PickleType"
            | "SmallInteger"
            | "String"
            | "Text"
            | "Time"
            | "TupleType"
            | "Unicode"
            | "UnicodeText"
            | "Uuid"
            | "ARRAY"
            | "BIGINT"
            | "BINARY"
            | "BLOB"
            | "BOOLEAN"
            | "BOOL"
            | "BYTEA"
            | "CHAR"
            | "CIDR"
            | "CLOB"
            | "DATE"
            | "DATERANGE"
            | "DATETIME"
            | "DEC"
            | "DECIMAL"
            | "DOUBLE"
            | "DOUBLE_PRECISION"
            | "ENUM"
            | "FLOAT"
            | "HSTORE"
            | "INET"
            | "INT"
            | "INT4RANGE"
            | "INT8RANGE"
            | "INTEGER"
            | "INTERVAL"
            | "JSON"
            | "JSONB"
            | "MACADDR"
            | "MONEY"
            | "NCHAR"
            | "NUMERIC"
            | "NUMRANGE"
            | "NVARCHAR"
            | "OID"
            | "REAL"
            | "REGCLASS"
            | "SMALLINT"
            | "TEXT"
            | "TIME"
            | "TIMESTAMP"
            | "TSRANGE"
            | "TSTZRANGE"
            | "TSVECTOR"
            | "UUID"
            | "VARBINARY"
            | "VARCHAR"
    )
}

pub(crate) fn call_arguments(call: TsNode<'_>) -> Vec<TsNode<'_>> {
    named_arguments(call)
        .into_iter()
        .filter(|node| node.kind() != "comment")
        .collect()
}

fn callee_simple_name<'a>(cx: &'a FileCx<'_>, callee: TsNode<'_>) -> &'a str {
    match callee.kind() {
        "identifier" => cx.text(&callee),
        "attribute" => callee
            .child_by_field_name("attribute")
            .map(|attr| cx.text(&attr))
            .unwrap_or(""),
        _ => "",
    }
}

fn literal_bool(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<bool> {
    match node.kind() {
        "true" => Some(true),
        "false" => Some(false),
        _ => match cx.text(&node).trim() {
            "True" => Some(true),
            "False" => Some(false),
            _ => None,
        },
    }
}

fn sqlalchemy_type(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    let target = match node.kind() {
        "call" => node.child_by_field_name("function")?,
        _ => node,
    };
    let name = callee_simple_name(cx, target);
    is_sqlalchemy_type_name(name).then(|| name.to_string())
}

/// Whether `node` is an `identifier` or a dotted chain of `attribute` accesses
/// rooted at an `identifier`.
pub(crate) fn is_dotted_identifier(node: TsNode<'_>) -> bool {
    match node.kind() {
        "identifier" => true,
        "attribute" => {
            node.child_by_field_name("attribute")
                .is_some_and(|attr| attr.kind() == "identifier")
                && node
                    .child_by_field_name("object")
                    .is_some_and(is_dotted_identifier)
        }
        _ => false,
    }
}

/// Whether `module` is one of the SQLAlchemy packages that export `Column`,
/// `mapped_column`, `relationship`, and `ForeignKey`.
fn is_sqlalchemy_orm_module(module: &str) -> bool {
    matches!(
        module,
        "sqlalchemy" | "sqlalchemy.orm" | "sqlalchemy.schema"
    )
}

/// Classify a call's `function` node as a SQLAlchemy ORM member (`Column`,
/// `mapped_column`, `relationship`, `ForeignKey`) if its callee is either
/// import-proven from `sqlalchemy` / `sqlalchemy.orm` / `sqlalchemy.schema` or
/// an attribute on a candidate Flask-SQLAlchemy receiver to be verified against
/// `proven_flask_dbs` during `resolve`.
fn classify_orm_callee(
    cx: &FileCx<'_>,
    callee: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    shadowed_for_import: &BTreeSet<String>,
    shadowed_for_receiver: &BTreeSet<String>,
) -> Option<(OrmMember, CallProof)> {
    if !is_dotted_identifier(callee) {
        return None;
    }
    let text = cx.text(&callee).trim();
    let head = text.split('.').next().unwrap_or(text);
    if !shadowed_for_import.contains(head)
        && let Some((module, exported)) = resolve_imported_symbol(text, bindings)
        && is_sqlalchemy_orm_module(&module)
        && let Some(member) = OrmMember::from_name(&exported)
    {
        return Some((member, CallProof::SqlAlchemyImport));
    }
    if callee.kind() == "attribute" && !shadowed_for_receiver.contains(head) {
        let (recv, attr) = text.rsplit_once('.')?;
        let member = OrmMember::from_name(attr)?;
        return Some((
            member,
            CallProof::FlaskDbReceiver {
                file: cx.path.to_string(),
                recv: recv.to_string(),
            },
        ));
    }
    None
}

/// The column inside an import-proven `sqlalchemy.orm.deferred(column, …)`,
/// which loads it lazily but maps it all the same; otherwise `node`.
fn undeferred<'t>(
    cx: &FileCx<'_>,
    node: TsNode<'t>,
    bindings: &HashMap<String, ImportBinding>,
    shadowed_for_import: &BTreeSet<String>,
) -> TsNode<'t> {
    let Some(callee) = node
        .child_by_field_name("function")
        .filter(|callee| node.kind() == "call" && is_dotted_identifier(*callee))
    else {
        return node;
    };
    let text = cx.text(&callee).trim();
    let head = text.split('.').next().unwrap_or(text);
    let proven = !shadowed_for_import.contains(head)
        && resolve_imported_symbol(text, bindings)
            .is_some_and(|(module, name)| is_sqlalchemy_orm_module(&module) && name == "deferred");
    let positional: Vec<_> = call_arguments(node)
        .into_iter()
        .filter(|arg| arg.kind() != "keyword_argument")
        .collect();
    match positional.as_slice() {
        [column] if proven => *column,
        _ => node,
    }
}

/// Return the [`CallProof`] when `node` is a candidate `ForeignKey(...)` call.
fn foreign_key_call_proof(
    cx: &FileCx<'_>,
    node: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    shadowed_for_import: &BTreeSet<String>,
    shadowed_for_receiver: &BTreeSet<String>,
) -> Option<CallProof> {
    if node.kind() != "call" {
        return None;
    }
    let callee = node.child_by_field_name("function")?;
    match classify_orm_callee(
        cx,
        callee,
        bindings,
        shadowed_for_import,
        shadowed_for_receiver,
    ) {
        Some((OrmMember::ForeignKey, proof)) => Some(proof),
        _ => None,
    }
}

fn first_or_keyword_arg<'t>(
    cx: &FileCx<'_>,
    call: TsNode<'t>,
    keyword: &str,
) -> Option<TsNode<'t>> {
    let args = call_arguments(call);
    if let Some(pos) = args.iter().find(|arg| arg.kind() != "keyword_argument") {
        return Some(*pos);
    }
    for arg in args {
        if arg.kind() != "keyword_argument" {
            continue;
        }
        let (Some(name), Some(value)) = (
            arg.child_by_field_name("name"),
            arg.child_by_field_name("value"),
        ) else {
            continue;
        };
        if cx.text(&name) == keyword {
            return Some(value);
        }
    }
    None
}

const MAX_UNPROVABLE_TARGET_BYTES: usize = 200;

/// Cap raw source text stored in `Unprovable` relation targets at 200 bytes on
/// a UTF-8 char boundary, appending `…` when truncated.
fn bound_target_text(text: &str) -> String {
    if text.len() <= MAX_UNPROVABLE_TARGET_BYTES {
        return text.to_string();
    }
    let mut end = MAX_UNPROVABLE_TARGET_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn parse_foreign_key(
    cx: &FileCx<'_>,
    field: &str,
    fk_call: TsNode<'_>,
    col_proof: CallProof,
    fk_proof: CallProof,
) -> ForeignKeyDecl {
    let target = match first_or_keyword_arg(cx, fk_call, "column") {
        Some(arg) => match literal_string(cx, arg) {
            Some(lit) => FkTarget::Literal(lit),
            None => match (
                arg.kind(),
                arg.child_by_field_name("object"),
                arg.child_by_field_name("attribute"),
            ) {
                ("attribute", Some(object), Some(attr)) if is_dotted_identifier(arg) => {
                    FkTarget::Attribute(
                        cx.text(&object).to_string(),
                        cx.text(&attr).to_string(),
                        bound_target_text(cx.text(&arg)),
                    )
                }
                _ => FkTarget::Unprovable(bound_target_text(cx.text(&arg))),
            },
        },
        None => FkTarget::Unprovable(bound_target_text(cx.text(&fk_call))),
    };
    ForeignKeyDecl {
        field: field.to_string(),
        target,
        col_proof,
        fk_proof,
        evidence: cx.evidence(&fk_call),
    }
}

/// Peel outer tree-sitter `type` wrapper nodes to reach the inner type expression.
fn unwrap_type_node(mut node: TsNode<'_>) -> TsNode<'_> {
    while node.kind() == "type" {
        let mut walk = node.walk();
        let children: Vec<TsNode<'_>> = node
            .named_children(&mut walk)
            .filter(|c| c.kind() != "comment")
            .collect();
        if children.len() == 1 {
            node = children[0];
        } else {
            break;
        }
    }
    node
}

/// Split a generic or subscripted type expression `Base[Arg1, ...]` into
/// `(base_text, type_args)`.
fn split_generic<'a, 't>(
    cx: &'a FileCx<'_>,
    node: TsNode<'t>,
) -> Option<(&'a str, Vec<TsNode<'t>>)> {
    let node = unwrap_type_node(node);
    match node.kind() {
        "generic_type" => {
            let mut walk = node.walk();
            let mut children = node
                .named_children(&mut walk)
                .filter(|c| c.kind() != "comment");
            let base = children.next()?;
            let param = children.next()?;
            if param.kind() != "type_parameter" {
                return None;
            }
            let mut param_walk = param.walk();
            let args: Vec<TsNode<'t>> = param
                .named_children(&mut param_walk)
                .filter(|c| c.kind() != "comment")
                .map(unwrap_type_node)
                .collect();
            Some((cx.text(&base).trim(), args))
        }
        "subscript" => {
            let value = node.child_by_field_name("value")?;
            let mut walk = node.walk();
            let mut args: Vec<TsNode<'t>> = node
                .named_children(&mut walk)
                .filter(|c| c.id() != value.id() && c.kind() != "comment")
                .map(unwrap_type_node)
                .collect();
            if args.len() == 1 && args[0].kind() == "tuple" {
                let tuple = args[0];
                let mut tuple_walk = tuple.walk();
                args = tuple
                    .named_children(&mut tuple_walk)
                    .filter(|c| c.kind() != "comment")
                    .map(unwrap_type_node)
                    .collect();
            }
            Some((cx.text(&value).trim(), args))
        }
        _ => None,
    }
}

/// Whether `name` is a single-target collection or optional wrapper inside
/// `Mapped[...]`.
fn is_relationship_container(name: &str) -> bool {
    let bare = name.strip_prefix("typing.").unwrap_or(name);
    matches!(
        bare,
        "list"
            | "List"
            | "set"
            | "Set"
            | "Optional"
            | "Sequence"
            | "MutableSequence"
            | "AbstractSet"
            | "MutableSet"
            | "Collection"
    )
}

/// Extract a `RelTarget` from a SQLAlchemy 2 `Mapped[...]` type annotation,
/// unwrapping `list`/`List`/`set`/`Set`/`Optional`/`typing.*` one level.
fn parse_mapped_annotation_target(cx: &FileCx<'_>, type_node: TsNode<'_>) -> RelTarget {
    let raw_annotation = bound_target_text(cx.text(&type_node).trim());
    let Some((outer_base, outer_args)) = split_generic(cx, type_node) else {
        return RelTarget::Unprovable(raw_annotation);
    };
    if !(outer_base == "Mapped" || outer_base.ends_with(".Mapped")) || outer_args.len() != 1 {
        return RelTarget::Unprovable(raw_annotation);
    }
    let mut inner = outer_args[0];
    if let Some((inner_base, inner_args)) = split_generic(cx, inner)
        && is_relationship_container(inner_base)
        && inner_args.len() == 1
    {
        inner = inner_args[0];
    }
    if let Some(lit) = literal_string(cx, inner) {
        RelTarget::Name(lit)
    } else if matches!(inner.kind(), "identifier" | "attribute") && is_dotted_identifier(inner) {
        RelTarget::Symbol(cx.text(&inner).trim().to_string())
    } else {
        RelTarget::Unprovable(raw_annotation)
    }
}

fn parse_relationship(
    cx: &FileCx<'_>,
    field: &str,
    rel_call: TsNode<'_>,
    assignment: TsNode<'_>,
    proof: CallProof,
) -> RelationshipDecl {
    let target = match first_or_keyword_arg(cx, rel_call, "argument") {
        Some(arg) => {
            if let Some(lit) = literal_string(cx, arg) {
                RelTarget::Name(lit)
            } else if matches!(arg.kind(), "identifier" | "attribute") && is_dotted_identifier(arg)
            {
                RelTarget::Symbol(cx.text(&arg).to_string())
            } else {
                RelTarget::Unprovable(bound_target_text(cx.text(&arg)))
            }
        }
        None => match assignment.child_by_field_name("type") {
            Some(type_node) => parse_mapped_annotation_target(cx, type_node),
            None => RelTarget::Unprovable(bound_target_text(cx.text(&rel_call))),
        },
    };
    RelationshipDecl {
        field: field.to_string(),
        target,
        proof,
        evidence: cx.evidence(&assignment),
    }
}

/// Extract a non-empty literal `"schema"` entry from a dict literal.
fn dict_literal_schema(cx: &FileCx<'_>, dict: TsNode<'_>) -> Option<String> {
    if dict.kind() != "dictionary" {
        return None;
    }
    let mut schema = None;
    let mut walk = dict.walk();
    for child in dict.named_children(&mut walk) {
        if child.kind() != "pair" {
            continue;
        }
        let (Some(key), Some(value)) = (
            child.child_by_field_name("key"),
            child.child_by_field_name("value"),
        ) else {
            continue;
        };
        if literal_string(cx, key).as_deref() == Some("schema") {
            schema = literal_string(cx, value).filter(|s| !s.is_empty());
        }
    }
    schema
}

/// Extract the literal `"schema"` from `__table_args__` when it is a dict
/// literal or a tuple literal whose last element is a dict literal.
fn parse_table_args_schema(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    match node.kind() {
        "dictionary" => dict_literal_schema(cx, node),
        "tuple" => {
            let mut walk = node.walk();
            let last = node
                .named_children(&mut walk)
                .filter(|c| c.kind() != "comment")
                .last()?;
            dict_literal_schema(cx, last)
        }
        _ => None,
    }
}

/// Return the declared name when `node` is a function or class definition
/// (including decorated definitions).
fn def_or_class_name<'a>(cx: &'a FileCx<'_>, mut node: TsNode<'_>) -> Option<&'a str> {
    if node.kind() == "decorated_definition" {
        let mut walk = node.walk();
        node = node
            .named_children(&mut walk)
            .find(|c| matches!(c.kind(), "function_definition" | "class_definition"))?;
    }
    if matches!(node.kind(), "function_definition" | "class_definition") {
        let name = node.child_by_field_name("name")?;
        return Some(cx.text(&name));
    }
    None
}

fn statement_assignment(node: TsNode<'_>) -> Option<TsNode<'_>> {
    if node.kind() == "assignment" {
        return Some(node);
    }
    if node.kind() == "expression_statement" {
        let mut walk = node.walk();
        return node
            .named_children(&mut walk)
            .find(|child| child.kind() == "assignment");
    }
    None
}

fn is_inside_class_or_function(mut node: TsNode<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if matches!(parent.kind(), "class_definition" | "function_definition") {
            return true;
        }
        node = parent;
    }
    false
}

/// The names, among module-level assignment targets and imports, that the
/// module binds more than once in any form (`for`, `with`/`except` aliases,
/// walrus, `del`, imports, `def`/`class`, a nested `global`; see
/// `module_binding`). A bare annotation binds nothing.
fn module_rebound_names(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = bindings.keys().cloned().collect();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "function_definition" | "class_definition" | "lambda"
        ) {
            continue;
        }
        if node.kind() == "assignment"
            && let Some(left) = node.child_by_field_name("left")
        {
            let mut bound = Vec::new();
            sqlalchemy_writes::collect_bound_identifiers(cx, left, &mut bound);
            names.extend(bound);
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    names
        .into_iter()
        .filter(|name| crate::module_binding(cx, root, name).writes.len() > 1)
        .collect()
}

/// Extract per-file SQLAlchemy base bindings and class declarations.
pub(crate) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) {
    let mut file = OrmFile {
        path: cx.path.to_string(),
        imports: bindings.clone(),
        proven_bases: BTreeSet::new(),
        proven_registries: BTreeSet::new(),
        proven_flask_dbs: BTreeSet::new(),
        proven_sessions: BTreeSet::new(),
        rebound: BTreeSet::new(),
        assignments: Vec::new(),
        classes: Vec::new(),
        functions: Vec::new(),
    };
    file.rebound = module_rebound_names(cx, root, bindings);

    for local in bindings.keys() {
        if is_declarative_base_class(local, bindings) {
            file.proven_bases.insert(local.clone());
        }
    }

    let mut module_shadowed_imports: BTreeSet<String> = BTreeSet::new();
    let mut module_shadowed_receivers: BTreeSet<String> = BTreeSet::new();
    {
        let mut walk = root.walk();
        for stmt in root.named_children(&mut walk) {
            if let Some(name) = def_or_class_name(cx, stmt) {
                module_shadowed_imports.insert(name.to_string());
                module_shadowed_receivers.insert(name.to_string());
            }
        }
    }

    let assign_query =
        crate::queries::query("(assignment left: (identifier) @lhs right: (_) @rhs) @assign");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(assign_query, root, cx.source);
    while let Some(found) = matches.next() {
        let (mut assign, mut lhs, mut rhs) = (None, None, None);
        for capture in found.captures() {
            match assign_query.capture_names()[capture.index as usize] {
                "assign" => assign = Some(capture.node),
                "lhs" => lhs = Some(cx.text(&capture.node).to_string()),
                "rhs" => rhs = Some(capture.node),
                _ => {}
            }
        }
        let (Some(assign), Some(lhs), Some(rhs)) = (assign, lhs, rhs) else {
            continue;
        };
        if is_inside_class_or_function(assign) {
            continue;
        }
        module_shadowed_imports.insert(lhs.clone());
        let mut is_candidate_receiver = false;
        if !file.rebound.contains(&lhs)
            && sqlalchemy_writes::is_session_factory_call(cx, rhs, bindings, &file.rebound)
        {
            file.proven_sessions.insert(lhs.clone());
            is_candidate_receiver = true;
        } else {
            file.proven_sessions.remove(&lhs);
        }
        match rhs.kind() {
            "call" => {
                let Some(function) = rhs.child_by_field_name("function") else {
                    module_shadowed_receivers.insert(lhs);
                    continue;
                };
                let callee = cx.text(&function);
                if is_declarative_base_fn(callee, bindings) {
                    file.proven_bases.insert(lhs.clone());
                } else if is_registry_fn(callee, bindings) {
                    file.proven_registries.insert(lhs.clone());
                } else if is_flask_sqlalchemy_fn(callee, bindings) {
                    file.proven_flask_dbs.insert(lhs.clone());
                    is_candidate_receiver = true;
                } else if function.kind() == "attribute" {
                    let (Some(obj), Some(attr)) = (
                        function.child_by_field_name("object"),
                        function.child_by_field_name("attribute"),
                    ) else {
                        module_shadowed_receivers.insert(lhs);
                        continue;
                    };
                    if cx.text(&attr) == "generate_base" {
                        if obj.kind() == "call" {
                            if let Some(inner_fn) = obj.child_by_field_name("function")
                                && is_registry_fn(cx.text(&inner_fn), bindings)
                            {
                                file.proven_bases.insert(lhs.clone());
                            }
                        } else if matches!(obj.kind(), "identifier" | "attribute") {
                            file.assignments.push((
                                lhs.clone(),
                                ModuleRhs::GenerateBase(cx.text(&obj).to_string()),
                            ));
                        }
                    }
                }
            }
            "attribute" => {
                let text = cx.text(&rhs);
                if let Some(recv) = text.strip_suffix(".Model") {
                    file.assignments
                        .push((lhs.clone(), ModuleRhs::DotModel(recv.to_string())));
                } else if let Some(recv) = text.strip_suffix(".session") {
                    file.assignments
                        .push((lhs.clone(), ModuleRhs::DotSession(recv.to_string())));
                    is_candidate_receiver = true;
                } else {
                    file.assignments
                        .push((lhs.clone(), ModuleRhs::Alias(text.to_string())));
                    is_candidate_receiver = true;
                }
            }
            "identifier" => {
                file.assignments
                    .push((lhs.clone(), ModuleRhs::Alias(cx.text(&rhs).to_string())));
                is_candidate_receiver = true;
            }
            _ => {}
        }
        if !is_candidate_receiver {
            module_shadowed_receivers.insert(lhs);
        }
    }

    let class_query = crate::queries::query("(class_definition) @class");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(class_query, root, cx.source);
    while let Some(found) = matches.next() {
        let class = found.captures()[0].node;
        let Some(name_node) = class.child_by_field_name("name") else {
            continue;
        };
        let name = cx.text(&name_node).to_string();
        let mut bases = Vec::new();
        let mut is_direct_declarative_base_subclass = false;
        if let Some(superclasses) = class.child_by_field_name("superclasses") {
            let mut walk = superclasses.walk();
            for base in superclasses.named_children(&mut walk) {
                if matches!(base.kind(), "comment" | "keyword_argument") {
                    continue;
                }
                let text = cx.text(&base);
                if is_declarative_base_class(text, bindings) {
                    is_direct_declarative_base_subclass = true;
                    bases.push(BaseExpr::DeclarativeBase);
                } else if base.kind() == "call" {
                    let Some(function) = base.child_by_field_name("function") else {
                        bases.push(BaseExpr::Unresolvable);
                        continue;
                    };
                    let callee = cx.text(&function);
                    if is_declarative_base_fn(callee, bindings) {
                        bases.push(BaseExpr::InlineProvenBase);
                    } else if function.kind() == "attribute" {
                        let (Some(obj), Some(attr)) = (
                            function.child_by_field_name("object"),
                            function.child_by_field_name("attribute"),
                        ) else {
                            bases.push(BaseExpr::Unresolvable);
                            continue;
                        };
                        if cx.text(&attr) == "generate_base" {
                            if obj.kind() == "call" {
                                if let Some(inner_fn) = obj.child_by_field_name("function")
                                    && is_registry_fn(cx.text(&inner_fn), bindings)
                                {
                                    bases.push(BaseExpr::InlineProvenBase);
                                } else {
                                    bases.push(BaseExpr::Unresolvable);
                                }
                            } else if is_dotted_identifier(obj) {
                                bases.push(BaseExpr::GenerateBase(cx.text(&obj).to_string()));
                            } else {
                                bases.push(BaseExpr::Unresolvable);
                            }
                        } else {
                            bases.push(BaseExpr::Unresolvable);
                        }
                    } else {
                        bases.push(BaseExpr::Unresolvable);
                    }
                } else if base.kind() == "attribute"
                    && is_dotted_identifier(base)
                    && let Some(recv) = text.strip_suffix(".Model")
                {
                    bases.push(BaseExpr::DotModel(recv.to_string()));
                } else if is_dotted_identifier(base) {
                    bases.push(BaseExpr::Symbol(text.to_string()));
                } else {
                    bases.push(BaseExpr::Unresolvable);
                }
            }
        }

        let mut tablename = TableName::Absent;
        let mut schema = None;
        let mut is_abstract = false;
        let mut columns = Vec::new();
        let mut foreign_keys = Vec::new();
        let mut relationships = Vec::new();

        if let Some(body) = class.child_by_field_name("body") {
            let mut shadowed_for_import = module_shadowed_imports.clone();
            let mut shadowed_for_receiver = module_shadowed_receivers.clone();
            {
                let mut walk = body.walk();
                for stmt in body.named_children(&mut walk) {
                    if let Some(local_name) = def_or_class_name(cx, stmt) {
                        shadowed_for_import.insert(local_name.to_string());
                        shadowed_for_receiver.insert(local_name.to_string());
                    }
                }
            }

            let mut walk = body.walk();
            for stmt in body.named_children(&mut walk) {
                let Some(assignment) = statement_assignment(stmt) else {
                    continue;
                };
                let (Some(left), Some(right)) = (
                    assignment.child_by_field_name("left"),
                    assignment.child_by_field_name("right"),
                ) else {
                    continue;
                };
                if left.kind() != "identifier" {
                    continue;
                }
                let attr_name = cx.text(&left).to_string();
                if attr_name == "__tablename__" {
                    tablename = match literal_string(cx, right) {
                        Some(lit) => TableName::Literal(lit),
                        None => TableName::Dynamic,
                    };
                    continue;
                }
                if attr_name == "__table_args__" {
                    schema = parse_table_args_schema(cx, right);
                    continue;
                }
                if attr_name == "__abstract__" {
                    if literal_bool(cx, right) == Some(true) {
                        is_abstract = true;
                    }
                    continue;
                }
                let right = undeferred(cx, right, bindings, &shadowed_for_import);
                if right.kind() != "call" {
                    continue;
                }
                let Some(callee) = right.child_by_field_name("function") else {
                    continue;
                };
                let Some((member, call_proof)) = classify_orm_callee(
                    cx,
                    callee,
                    bindings,
                    &shadowed_for_import,
                    &shadowed_for_receiver,
                ) else {
                    continue;
                };
                match member {
                    OrmMember::Column | OrmMember::MappedColumn => {
                        let mut col_type = None;
                        let mut nullable = None;
                        let mut primary_key = None;
                        let mut renamed = false;
                        let mut db_name = None;
                        let mut computed_first = false;
                        for (index, arg) in call_arguments(right).into_iter().enumerate() {
                            if arg.kind() == "keyword_argument" {
                                let (Some(k), Some(v)) = (
                                    arg.child_by_field_name("name"),
                                    arg.child_by_field_name("value"),
                                ) else {
                                    continue;
                                };
                                match cx.text(&k) {
                                    "nullable" => nullable = literal_bool(cx, v),
                                    "primary_key" => primary_key = literal_bool(cx, v),
                                    "name" => {
                                        renamed = true;
                                        db_name = literal_string(cx, v);
                                    }
                                    "type_" if col_type.is_none() => {
                                        col_type = sqlalchemy_type(cx, v);
                                    }
                                    _ => {}
                                }
                            } else if let Some(fk_proof) = foreign_key_call_proof(
                                cx,
                                arg,
                                bindings,
                                &shadowed_for_import,
                                &shadowed_for_receiver,
                            ) {
                                foreign_keys.push(parse_foreign_key(
                                    cx,
                                    &attr_name,
                                    arg,
                                    call_proof.clone(),
                                    fk_proof,
                                ));
                            } else if let Some(name) = literal_string(cx, arg) {
                                renamed = true;
                                db_name = Some(name);
                            } else if col_type.is_none() {
                                col_type = sqlalchemy_type(cx, arg);
                                computed_first |= index == 0 && col_type.is_none();
                            }
                        }
                        // A first argument that is neither a name literal nor
                        // a known type, followed by a known type, is a
                        // computed database name.
                        renamed |= computed_first && col_type.is_some();
                        columns.push(ColumnDecl {
                            name: attr_name,
                            col_type,
                            nullable,
                            primary_key,
                            renamed,
                            db_name,
                            proof: call_proof,
                            evidence: cx.evidence(&assignment),
                        });
                    }
                    OrmMember::Relationship => {
                        relationships.push(parse_relationship(
                            cx, &attr_name, right, assignment, call_proof,
                        ));
                    }
                    OrmMember::ForeignKey => {}
                }
            }
        }

        let (defined_members, classmethods) =
            sqlalchemy_writes::extract_class_methods(cx, class, &module_shadowed_imports, bindings);

        file.classes.push(OrmClassDecl {
            key: (cx.path.to_string(), name.clone()),
            name,
            bases,
            tablename,
            schema,
            is_abstract,
            is_direct_declarative_base_subclass,
            columns,
            foreign_keys,
            relationships,
            defined_members,
            classmethods,
            evidence: cx.evidence(&class),
        });
    }

    sqlalchemy_writes::extract_functions(cx, root, &mut file);
    file.imports
        .retain(|name, _| !module_shadowed_imports.contains(name));

    if !file.proven_bases.is_empty()
        || !file.proven_registries.is_empty()
        || !file.proven_flask_dbs.is_empty()
        || !file.proven_sessions.is_empty()
        || !file.assignments.is_empty()
        || !file.classes.is_empty()
        || !file.functions.is_empty()
        || !file.imports.is_empty()
    {
        out.orm_files.push(file);
    }
}

/// Candidate repository file paths a module specifier from `from_file` may target.
fn module_candidate_files(
    from_file: &str,
    module: &str,
    known_files: &BTreeSet<String>,
) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(direct) = module_file(from_file, module) {
        if let Some(stem) = direct.strip_suffix(".py") {
            let init = format!("{stem}/__init__.py");
            if known_files.contains(&direct) {
                candidates.push(direct);
            } else if known_files.contains(&init) {
                candidates.push(init);
            } else {
                candidates.push(direct);
            }
        } else {
            candidates.push(direct);
        }
    }
    if module == "." {
        let init = match Path::new(from_file).parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                let mut p = PathBuf::from(parent);
                p.push("__init__.py");
                p.to_string_lossy().replace('\\', "/")
            }
            _ => "__init__.py".to_string(),
        };
        if !candidates.contains(&init) {
            candidates.push(init);
        }
    }
    candidates
}

/// Resolve `(file_path, expr)` across `imports_by_file` to its canonical
/// `(defining_file, symbol_name)`.
pub(crate) fn resolve_symbol(
    file: &str,
    expr: &str,
    imports_by_file: &BTreeMap<String, HashMap<String, ImportBinding>>,
    known_files: &BTreeSet<String>,
) -> Option<(String, String)> {
    let mut cur_file = file.to_string();
    let mut cur_expr = expr.trim().to_string();
    let mut visited = BTreeSet::new();
    while visited.insert((cur_file.clone(), cur_expr.clone())) {
        let bindings = imports_by_file.get(&cur_file);
        if let Some((head, tail)) = cur_expr.split_once('.') {
            let binding = bindings.and_then(|b| b.get(head))?;
            match binding {
                ImportBinding::Module { module } => {
                    let full_mod = if module == head || module.starts_with(&format!("{head}.")) {
                        let (mod_part, sym_part) = cur_expr.rsplit_once('.')?;
                        let target = module_candidate_files(&cur_file, mod_part, known_files)
                            .into_iter()
                            .next()?;
                        cur_file = target;
                        cur_expr = sym_part.to_string();
                        continue;
                    } else {
                        module.clone()
                    };
                    let target = module_candidate_files(&cur_file, &full_mod, known_files)
                        .into_iter()
                        .next()?;
                    cur_file = target;
                    cur_expr = tail.to_string();
                }
                ImportBinding::Symbol {
                    module, exported, ..
                } => {
                    let combined = if module == "." {
                        format!(".{exported}")
                    } else if module.ends_with('.') {
                        format!("{module}{exported}")
                    } else {
                        format!("{module}.{exported}")
                    };
                    let target = module_candidate_files(&cur_file, &combined, known_files)
                        .into_iter()
                        .next()?;
                    cur_file = target;
                    cur_expr = tail.to_string();
                }
            }
        } else {
            let Some(binding) = bindings.and_then(|b| b.get(&cur_expr)) else {
                return Some((cur_file, cur_expr));
            };
            match binding {
                ImportBinding::Symbol {
                    module,
                    exported,
                    target_file,
                    ..
                } => {
                    let candidates = module_candidate_files(&cur_file, module, known_files);
                    let next_file = candidates
                        .into_iter()
                        .find(|c| known_files.contains(c))
                        .or_else(|| target_file.clone())?;
                    cur_file = next_file;
                    cur_expr = exported.clone();
                }
                ImportBinding::Module { .. } => return None,
            }
        }
    }
    None
}

fn is_declarative_base_root(
    class: &OrmClassDecl,
    subclassed_keys: &BTreeSet<(String, String)>,
) -> bool {
    class.is_direct_declarative_base_subclass
        && matches!(class.tablename, TableName::Absent)
        && ((class.columns.is_empty()
            && class.foreign_keys.is_empty()
            && class.relationships.is_empty())
            || subclassed_keys.contains(&class.key))
}

/// Find the concrete recovered model ancestor for a single-table inheritance
/// subclass (a proven, non-abstract model with no `__tablename__` whose base
/// hierarchy includes a candidate model with a literal `__tablename__`).
fn find_concrete_parent(
    class: &OrmClassDecl,
    concrete_models: &BTreeMap<(String, String), &OrmClassDecl>,
    ambiguous_models: &BTreeMap<(String, String), &OrmClassDecl>,
    classes_by_key: &BTreeMap<(String, String), &OrmClassDecl>,
    subclassed_keys: &BTreeSet<(String, String)>,
    imports_by_file: &BTreeMap<String, HashMap<String, ImportBinding>>,
    known_files: &BTreeSet<String>,
) -> Parent {
    let mut visited = BTreeSet::from([class.key.clone()]);
    let mut queue = std::collections::VecDeque::from([class]);
    while let Some(current) = queue.pop_front() {
        for base in &current.bases {
            let BaseExpr::Symbol(sym) = base else {
                continue;
            };
            let Some(target_key) =
                resolve_symbol(&current.key.0, sym, imports_by_file, known_files)
            else {
                continue;
            };
            if concrete_models.contains_key(&target_key) {
                return Parent::Concrete(target_key);
            }
            if ambiguous_models.contains_key(&target_key) {
                return Parent::Ambiguous(target_key);
            }
            if !visited.insert(target_key.clone()) {
                continue;
            }
            if let Some(base_class) = classes_by_key.get(&target_key).copied()
                && !base_class.is_abstract
                && !is_declarative_base_root(base_class, subclassed_keys)
                && matches!(base_class.tablename, TableName::Absent)
            {
                queue.push_back(base_class);
            }
        }
    }
    Parent::None
}

fn collect_inherited_members(
    class: &OrmClassDecl,
    classes_by_key: &BTreeMap<(String, String), &OrmClassDecl>,
    imports_by_file: &BTreeMap<String, HashMap<String, ImportBinding>>,
    known_files: &BTreeSet<String>,
) -> (Vec<ColumnDecl>, Vec<ForeignKeyDecl>, Vec<RelationshipDecl>) {
    let mut columns = Vec::new();
    let mut foreign_keys = Vec::new();
    let mut relationships = Vec::new();
    let mut seen_cols: BTreeSet<String> = BTreeSet::new();
    let mut seen_rels: BTreeSet<String> = BTreeSet::new();
    for col in &class.columns {
        if seen_cols.insert(col.name.clone()) {
            columns.push(col.clone());
            for fk in &class.foreign_keys {
                if fk.field == col.name {
                    foreign_keys.push(fk.clone());
                }
            }
        }
    }
    for rel in &class.relationships {
        if seen_rels.insert(rel.field.clone()) {
            relationships.push(rel.clone());
        }
    }
    let mut visited_classes = BTreeSet::from([class.key.clone()]);

    #[allow(clippy::too_many_arguments)]
    fn visit_bases(
        current: &OrmClassDecl,
        classes_by_key: &BTreeMap<(String, String), &OrmClassDecl>,
        imports_by_file: &BTreeMap<String, HashMap<String, ImportBinding>>,
        known_files: &BTreeSet<String>,
        visited_classes: &mut BTreeSet<(String, String)>,
        seen_cols: &mut BTreeSet<String>,
        seen_rels: &mut BTreeSet<String>,
        columns: &mut Vec<ColumnDecl>,
        foreign_keys: &mut Vec<ForeignKeyDecl>,
        relationships: &mut Vec<RelationshipDecl>,
    ) {
        for base in &current.bases {
            let BaseExpr::Symbol(sym) = base else {
                continue;
            };
            let Some(target_key) =
                resolve_symbol(&current.key.0, sym, imports_by_file, known_files)
            else {
                continue;
            };
            let Some(base_class) = classes_by_key.get(&target_key).copied() else {
                continue;
            };
            if !visited_classes.insert(target_key) {
                continue;
            }
            for col in &base_class.columns {
                if seen_cols.insert(col.name.clone()) {
                    columns.push(col.clone());
                    for fk in &base_class.foreign_keys {
                        if fk.field == col.name {
                            foreign_keys.push(fk.clone());
                        }
                    }
                }
            }
            for rel in &base_class.relationships {
                if seen_rels.insert(rel.field.clone()) {
                    relationships.push(rel.clone());
                }
            }
            visit_bases(
                base_class,
                classes_by_key,
                imports_by_file,
                known_files,
                visited_classes,
                seen_cols,
                seen_rels,
                columns,
                foreign_keys,
                relationships,
            );
        }
    }

    visit_bases(
        class,
        classes_by_key,
        imports_by_file,
        known_files,
        &mut visited_classes,
        &mut seen_cols,
        &mut seen_rels,
        &mut columns,
        &mut foreign_keys,
        &mut relationships,
    );
    (columns, foreign_keys, relationships)
}

/// Emit a `Gap` node and a `REFERENCES` edge to it for an unresolved
/// `ForeignKey` or `relationship` declaration, deduplicated by `gap_id`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_reference_gap(
    out: &mut Extraction,
    emitted_gaps: &mut BTreeSet<String>,
    repo: &str,
    src_entity_id: &str,
    field: &str,
    target: &str,
    reason: &str,
    evidence: EvidenceRef,
) {
    let gap_id = format!(
        "gap:sqlalchemy:{repo}@{src_entity_id}@{}@{}",
        evidence.path, evidence.byte_start
    );
    if emitted_gaps.insert(gap_id.clone()) {
        out.nodes.push(Node {
            id: gap_id.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "reason": reason,
                "field": field,
                "target": target,
                "framework": "sqlalchemy",
                "language": "python",
                "attempted_tiers": ["T0"],
                "prov": prov_from(
                    vec![evidence.clone()],
                    ConfidenceTier::Gap,
                    &format!("Gap {gap_id}"),
                ),
            }),
        });
        out.edges.push(Edge {
            src: src_entity_id.to_string(),
            dst: gap_id.clone(),
            label: "REFERENCES".into(),
            props: serde_json::json!({
                "reason": reason,
                "field": field,
                "target": target,
                "framework": "sqlalchemy",
                "prov": prov_from(
                    vec![evidence],
                    ConfidenceTier::Gap,
                    &format!("REFERENCES {src_entity_id} -> {gap_id}"),
                ),
            }),
        });
    }
}

/// Join per-file SQLAlchemy declarations across the directory, emitting
/// `DataEntity` nodes, `REFERENCES` edges, and `Gap` nodes.
pub(crate) fn resolve(out: &mut Extraction, repo: &str) {
    let orm_files = std::mem::take(&mut out.orm_files);
    if orm_files.is_empty() {
        return;
    }

    let mut known_files = BTreeSet::new();
    let mut imports_by_file = BTreeMap::new();
    let mut proven_bases: BTreeSet<(String, String)> = BTreeSet::new();
    let mut proven_registries: BTreeSet<(String, String)> = BTreeSet::new();
    let mut proven_flask_dbs: BTreeSet<(String, String)> = BTreeSet::new();
    let mut proven_sessions: BTreeSet<(String, String)> = BTreeSet::new();
    let mut assignments: Vec<((String, String), ModuleRhs)> = Vec::new();
    let mut all_classes: Vec<OrmClassDecl> = Vec::new();
    let mut rebound: BTreeSet<(String, String)> = BTreeSet::new();
    let mut functions_by_file: Vec<(String, Vec<sqlalchemy_writes::FunctionWrites>)> = Vec::new();

    for file in orm_files {
        known_files.insert(file.path.clone());
        for b in file.proven_bases {
            proven_bases.insert((file.path.clone(), b));
        }
        for r in file.proven_registries {
            proven_registries.insert((file.path.clone(), r));
        }
        for db in file.proven_flask_dbs {
            proven_flask_dbs.insert((file.path.clone(), db));
        }
        for s in file.proven_sessions {
            proven_sessions.insert((file.path.clone(), s));
        }
        for name in file.rebound {
            rebound.insert((file.path.clone(), name));
        }
        for (lhs, rhs) in file.assignments {
            assignments.push(((file.path.clone(), lhs), rhs));
        }
        if !file.functions.is_empty() {
            functions_by_file.push((file.path.clone(), file.functions));
        }
        imports_by_file.insert(file.path, file.imports);
        all_classes.extend(file.classes);
    }

    // Fixed-point propagation of proven registries, Flask-SQLAlchemy instances,
    // sessions, base aliases, and model subclasses across files.
    loop {
        let mut changed = false;
        for ((path, lhs), rhs) in &assignments {
            let key = (path.clone(), lhs.clone());
            match rhs {
                ModuleRhs::GenerateBase(recv) => {
                    if !proven_bases.contains(&key)
                        && resolve_symbol(path, recv, &imports_by_file, &known_files)
                            .is_some_and(|target| proven_registries.contains(&target))
                    {
                        proven_bases.insert(key);
                        changed = true;
                    }
                }
                ModuleRhs::DotModel(recv) => {
                    if !proven_bases.contains(&key)
                        && resolve_symbol(path, recv, &imports_by_file, &known_files)
                            .is_some_and(|target| proven_flask_dbs.contains(&target))
                    {
                        proven_bases.insert(key);
                        changed = true;
                    }
                }
                ModuleRhs::DotSession(recv) => {
                    if !proven_sessions.contains(&key)
                        && !rebound.contains(&key)
                        && resolve_symbol(path, recv, &imports_by_file, &known_files)
                            .is_some_and(|target| proven_flask_dbs.contains(&target))
                    {
                        proven_sessions.insert(key);
                        changed = true;
                    }
                }
                ModuleRhs::Alias(other) => {
                    if let Some(target) =
                        resolve_symbol(path, other, &imports_by_file, &known_files)
                    {
                        if !proven_bases.contains(&key) && proven_bases.contains(&target) {
                            proven_bases.insert(key.clone());
                            changed = true;
                        }
                        if !proven_registries.contains(&key) && proven_registries.contains(&target)
                        {
                            proven_registries.insert(key.clone());
                            changed = true;
                        }
                        if !proven_flask_dbs.contains(&key) && proven_flask_dbs.contains(&target) {
                            proven_flask_dbs.insert(key.clone());
                            changed = true;
                        }
                        if !proven_sessions.contains(&key)
                            && !rebound.contains(&key)
                            && proven_sessions.contains(&target)
                        {
                            proven_sessions.insert(key);
                            changed = true;
                        }
                    }
                }
            }
        }
        for class in &all_classes {
            if proven_bases.contains(&class.key) {
                continue;
            }
            let is_proven = class.bases.iter().any(|base| match base {
                BaseExpr::DeclarativeBase | BaseExpr::InlineProvenBase => true,
                BaseExpr::GenerateBase(recv) => {
                    resolve_symbol(&class.key.0, recv, &imports_by_file, &known_files)
                        .is_some_and(|target| proven_registries.contains(&target))
                }
                BaseExpr::DotModel(recv) => {
                    resolve_symbol(&class.key.0, recv, &imports_by_file, &known_files)
                        .is_some_and(|target| proven_flask_dbs.contains(&target))
                }
                BaseExpr::Symbol(sym) => {
                    resolve_symbol(&class.key.0, sym, &imports_by_file, &known_files)
                        .is_some_and(|target| proven_bases.contains(&target))
                }
                BaseExpr::Unresolvable => false,
            });
            if is_proven {
                proven_bases.insert(class.key.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    for class in &mut all_classes {
        class.columns.retain(|col| {
            col.proof
                .is_proven(&imports_by_file, &known_files, &proven_flask_dbs)
        });
        class.foreign_keys.retain(|fk| {
            fk.col_proof
                .is_proven(&imports_by_file, &known_files, &proven_flask_dbs)
                && fk
                    .fk_proof
                    .is_proven(&imports_by_file, &known_files, &proven_flask_dbs)
        });
        class.relationships.retain(|rel| {
            rel.proof
                .is_proven(&imports_by_file, &known_files, &proven_flask_dbs)
        });
    }

    // Track which classes are subclassed by another class in the repository, so
    // `class Base(DeclarativeBase): ...` used as a project base is distinguished
    // from a leaf model class that forgot `__tablename__`.
    let mut subclassed_keys: BTreeSet<(String, String)> = BTreeSet::new();
    for class in &all_classes {
        for base in &class.bases {
            if let BaseExpr::Symbol(sym) = base
                && let Some(target) =
                    resolve_symbol(&class.key.0, sym, &imports_by_file, &known_files)
            {
                subclassed_keys.insert(target);
            }
        }
    }

    let classes_by_key: BTreeMap<(String, String), &OrmClassDecl> =
        all_classes.iter().map(|c| (c.key.clone(), c)).collect();

    let candidate_classes: Vec<&OrmClassDecl> = all_classes
        .iter()
        .filter(|class| {
            proven_bases.contains(&class.key)
                && !class.is_abstract
                && !is_declarative_base_root(class, &subclassed_keys)
        })
        .collect();

    let mut table_claimants: BTreeMap<(Option<String>, String), Vec<&OrmClassDecl>> =
        BTreeMap::new();
    for &class in &candidate_classes {
        if let TableName::Literal(store) = &class.tablename {
            table_claimants
                .entry((class.schema.clone(), store.clone()))
                .or_default()
                .push(class);
        }
    }

    let mut concrete_models: BTreeMap<(String, String), &OrmClassDecl> = BTreeMap::new();
    let mut ambiguous_models: BTreeMap<(String, String), &OrmClassDecl> = BTreeMap::new();
    for claimants in table_claimants.values() {
        if claimants.len() == 1 {
            let class = claimants[0];
            concrete_models.insert(class.key.clone(), class);
        } else {
            for &class in claimants {
                ambiguous_models.insert(class.key.clone(), class);
            }
        }
    }

    let mut sti_children: BTreeMap<(String, String), Vec<&OrmClassDecl>> = BTreeMap::new();
    let mut missing_table_classes: Vec<(&OrmClassDecl, Option<String>)> = Vec::new();
    for &class in &candidate_classes {
        match &class.tablename {
            TableName::Literal(_) => {}
            TableName::Absent => match find_concrete_parent(
                class,
                &concrete_models,
                &ambiguous_models,
                &classes_by_key,
                &subclassed_keys,
                &imports_by_file,
                &known_files,
            ) {
                Parent::Concrete(parent_key) => {
                    sti_children.entry(parent_key).or_default().push(class);
                }
                Parent::Ambiguous(parent_key) => {
                    missing_table_classes.push((class, Some(parent_key.1)));
                }
                Parent::None => {
                    missing_table_classes.push((class, None));
                }
            },
            TableName::Dynamic => {
                missing_table_classes.push((class, None));
            }
        }
    }

    let mut emitted_gaps: BTreeSet<String> = BTreeSet::new();
    let mut emitted_dup_tables: BTreeSet<(Option<String>, String)> = BTreeSet::new();
    let mut by_table: HashMap<(Option<String>, String), Option<String>> = HashMap::new();
    let mut by_class_key: HashMap<(String, String), String> = HashMap::new();
    let mut by_class_name: HashMap<String, Option<String>> = HashMap::new();
    let mut active_models: Vec<(
        &OrmClassDecl,
        String,
        Vec<ForeignKeyDecl>,
        Vec<RelationshipDecl>,
    )> = Vec::new();

    for (class, ambiguous_parent) in missing_table_classes {
        let gap_id = format!(
            "gap:sqlalchemy:{repo}@{}@{}",
            class.evidence.path, class.evidence.byte_start
        );
        if emitted_gaps.insert(gap_id.clone()) {
            let props = match ambiguous_parent {
                Some(parent_name) => serde_json::json!({
                    "reason": "parent table declared by more than one model",
                    "class": class.name,
                    "parent": parent_name,
                    "framework": "sqlalchemy",
                    "language": "python",
                    "attempted_tiers": ["T0"],
                    "prov": prov_from(
                        vec![class.evidence.clone()],
                        ConfidenceTier::Gap,
                        &format!("Gap {gap_id}"),
                    ),
                }),
                None => serde_json::json!({
                    "reason": "table name not declared",
                    "class": class.name,
                    "framework": "sqlalchemy",
                    "language": "python",
                    "attempted_tiers": ["T0"],
                    "prov": prov_from(
                        vec![class.evidence.clone()],
                        ConfidenceTier::Gap,
                        &format!("Gap {gap_id}"),
                    ),
                }),
            };
            out.nodes.push(Node {
                id: gap_id,
                label: "Gap".into(),
                props,
            });
        }
    }

    // Class key -> (table, the class's own and inherited column attributes ->
    // renamed), for `ForeignKey(Model.column)` targets. Per class, not per
    // entity: STI siblings share a table but not their columns (#813 review).
    let mut class_columns: HashMap<(String, String), (String, BTreeMap<String, bool>)> =
        HashMap::new();
    let declared = |columns: &[ColumnDecl]| -> BTreeMap<String, bool> {
        columns
            .iter()
            .map(|c| (c.name.clone(), c.renamed))
            .collect()
    };
    for &class in &candidate_classes {
        let TableName::Literal(store) = &class.tablename else {
            continue;
        };
        let table_key = (class.schema.clone(), store.clone());
        let qualified_table = match &class.schema {
            Some(schema) => format!("{schema}.{store}"),
            None => store.clone(),
        };
        let claimants = &table_claimants[&table_key];
        if claimants.len() > 1 {
            by_table.insert(table_key.clone(), None);
            by_class_name.insert(class.name.clone(), None);
            if emitted_dup_tables.insert(table_key) {
                let gap_id = format!("gap:sqlalchemy:{repo}@table:{qualified_table}");
                if emitted_gaps.insert(gap_id.clone()) {
                    let classes: Vec<&str> = claimants.iter().map(|c| c.name.as_str()).collect();
                    let evidence: Vec<EvidenceRef> =
                        claimants.iter().map(|c| c.evidence.clone()).collect();
                    let mut props = serde_json::json!({
                        "reason": "table declared by more than one model",
                        "table": store,
                        "store": store,
                        "classes": classes,
                        "framework": "sqlalchemy",
                        "language": "python",
                        "attempted_tiers": ["T0"],
                        "prov": prov_from(
                            evidence,
                            ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    });
                    if let Some(schema) = &class.schema {
                        props["schema"] = serde_json::Value::String(schema.clone());
                    }
                    out.nodes.push(Node {
                        id: gap_id.clone(),
                        label: "Gap".into(),
                        props,
                    });
                }
            }
            continue;
        }

        let entity_id = format!("data:{repo}@sqlalchemy:{qualified_table}");
        let (mut resolved_columns, mut resolved_fks, mut resolved_rels) =
            collect_inherited_members(class, &classes_by_key, &imports_by_file, &known_files);
        class_columns.insert(
            class.key.clone(),
            (store.clone(), declared(&resolved_columns)),
        );
        let mut seen_cols: BTreeSet<String> =
            resolved_columns.iter().map(|c| c.name.clone()).collect();
        let mut seen_rels: BTreeSet<String> =
            resolved_rels.iter().map(|r| r.field.clone()).collect();

        if let Some(children) = sti_children.get(&class.key) {
            for &child in children {
                let (child_cols, child_fks, child_rels) = collect_inherited_members(
                    child,
                    &classes_by_key,
                    &imports_by_file,
                    &known_files,
                );
                class_columns.insert(child.key.clone(), (store.clone(), declared(&child_cols)));
                for col in child_cols {
                    if seen_cols.insert(col.name.clone()) {
                        for fk in &child_fks {
                            if fk.field == col.name {
                                resolved_fks.push(fk.clone());
                            }
                        }
                        resolved_columns.push(col);
                    }
                }
                for rel in child_rels {
                    if seen_rels.insert(rel.field.clone()) {
                        resolved_rels.push(rel);
                    }
                }
                by_class_key.insert(child.key.clone(), entity_id.clone());
                by_class_name
                    .entry(child.name.clone())
                    .and_modify(|id| *id = None)
                    .or_insert_with(|| Some(entity_id.clone()));
            }
        }

        let mut evidence = vec![class.evidence.clone()];
        let mut columns_json = Vec::with_capacity(resolved_columns.len());
        for col in &resolved_columns {
            evidence.push(col.evidence.clone());
            let mut map = serde_json::Map::new();
            map.insert("name".into(), serde_json::Value::String(col.name.clone()));
            // The database name, when it differs: `null` if not a literal.
            if col.renamed {
                map.insert("column".into(), serde_json::json!(col.db_name));
            }
            if let Some(ty) = &col.col_type {
                map.insert("type".into(), serde_json::Value::String(ty.clone()));
            }
            if let Some(nullable) = col.nullable {
                map.insert("nullable".into(), serde_json::Value::Bool(nullable));
            }
            if let Some(primary_key) = col.primary_key {
                map.insert("primary_key".into(), serde_json::Value::Bool(primary_key));
            }
            columns_json.push(serde_json::Value::Object(map));
        }

        let mut props = serde_json::json!({
            "name": class.name,
            "storage": "sqlalchemy",
            "store": store,
            "table": store,
            "columns": columns_json,
            "framework": "sqlalchemy",
            "language": "python",
            "prov": prov_from(
                evidence,
                ConfidenceTier::Confirmed,
                &format!("DataEntity {entity_id}"),
            ),
        });
        if let Some(schema) = &class.schema {
            props["schema"] = serde_json::Value::String(schema.clone());
        }
        out.nodes.push(Node {
            id: entity_id.clone(),
            label: "DataEntity".into(),
            props,
        });

        by_table
            .entry(table_key)
            .and_modify(|id| *id = None)
            .or_insert_with(|| Some(entity_id.clone()));
        by_class_key.insert(class.key.clone(), entity_id.clone());
        by_class_name
            .entry(class.name.clone())
            .and_modify(|id| *id = None)
            .or_insert_with(|| Some(entity_id.clone()));
        active_models.push((class, entity_id, resolved_fks, resolved_rels));
    }

    let mut resolved_references: BTreeMap<(String, String), Vec<ResolvedReference>> =
        BTreeMap::new();

    // Emit REFERENCES edges (or Gaps) for ForeignKey and relationship declarations.
    for (class, src_entity_id, foreign_keys, relationships) in active_models {
        for fk in &foreign_keys {
            match &fk.target {
                FkTarget::Literal(raw) => {
                    let parts: Vec<&str> = raw.split('.').collect();
                    if !matches!(parts.len(), 2 | 3) || parts.iter().any(|p| p.is_empty()) {
                        push_reference_gap(
                            out,
                            &mut emitted_gaps,
                            repo,
                            &src_entity_id,
                            &fk.field,
                            raw,
                            "ForeignKey target is not a table.column reference",
                            fk.evidence.clone(),
                        );
                        continue;
                    }
                    let (schema, table, column) = match parts.as_slice() {
                        [table, column] => (None, *table, *column),
                        [schema, table, column] => (Some((*schema).to_string()), *table, *column),
                        _ => unreachable!(),
                    };
                    let lookup_key = (schema, table.to_string());
                    match by_table.get(&lookup_key).and_then(|id| id.as_ref()) {
                        Some(dst_entity_id) => {
                            resolved_references
                                .entry((src_entity_id.clone(), dst_entity_id.clone()))
                                .or_default()
                                .push(ResolvedReference {
                                    field: fk.field.clone(),
                                    relation: "ForeignKey",
                                    target: raw.clone(),
                                    target_table: Some(table.to_string()),
                                    target_column: Some(column.to_string()),
                                    evidence: fk.evidence.clone(),
                                });
                        }
                        None => {
                            push_reference_gap(
                                out,
                                &mut emitted_gaps,
                                repo,
                                &src_entity_id,
                                &fk.field,
                                raw,
                                "ForeignKey target table is not a recovered entity",
                                fk.evidence.clone(),
                            );
                        }
                    }
                }
                FkTarget::Attribute(class_expr, column, raw) => {
                    let dst = resolve_symbol(
                        &fk.evidence.path,
                        class_expr,
                        &imports_by_file,
                        &known_files,
                    )
                    // The expression is evaluated where it is written, so only
                    // the declaring file's bindings name the class.
                    .and_then(|key| {
                        Some((by_class_key.get(&key).cloned()?, class_columns.get(&key)?))
                    });
                    let resolved = match dst {
                        None => Err("ForeignKey target class is not a recovered entity"),
                        Some((dst, (table, columns))) => match columns.get(column) {
                            Some(false) => Ok((dst, table.clone())),
                            Some(true) => Err("ForeignKey target column declares its own name"),
                            None => Err("ForeignKey target is not a declared column of the class"),
                        },
                    };
                    match resolved {
                        Ok((dst_entity_id, table)) => {
                            resolved_references
                                .entry((src_entity_id.clone(), dst_entity_id))
                                .or_default()
                                .push(ResolvedReference {
                                    field: fk.field.clone(),
                                    relation: "ForeignKey",
                                    target: raw.clone(),
                                    target_table: Some(table),
                                    target_column: Some(column.clone()),
                                    evidence: fk.evidence.clone(),
                                });
                        }
                        Err(reason) => push_reference_gap(
                            out,
                            &mut emitted_gaps,
                            repo,
                            &src_entity_id,
                            &fk.field,
                            raw,
                            reason,
                            fk.evidence.clone(),
                        ),
                    }
                }
                FkTarget::Unprovable(raw) => {
                    push_reference_gap(
                        out,
                        &mut emitted_gaps,
                        repo,
                        &src_entity_id,
                        &fk.field,
                        raw,
                        "ForeignKey target is not a string literal",
                        fk.evidence.clone(),
                    );
                }
            }
        }

        for rel in &relationships {
            match &rel.target {
                RelTarget::Name(target_name) => {
                    let resolved = resolve_symbol(
                        &rel.evidence.path,
                        target_name,
                        &imports_by_file,
                        &known_files,
                    )
                    .or_else(|| {
                        resolve_symbol(&class.key.0, target_name, &imports_by_file, &known_files)
                    })
                    .and_then(|key| by_class_key.get(&key).cloned())
                    .or_else(|| {
                        if !target_name.contains('.') {
                            by_class_name.get(target_name).and_then(|id| id.clone())
                        } else {
                            let (mod_part, cls_part) = target_name.rsplit_once('.')?;
                            module_candidate_files(&rel.evidence.path, mod_part, &known_files)
                                .into_iter()
                                .chain(module_candidate_files(&class.key.0, mod_part, &known_files))
                                .find_map(|f| by_class_key.get(&(f, cls_part.to_string())).cloned())
                        }
                    });
                    match resolved {
                        Some(dst_entity_id) => {
                            resolved_references
                                .entry((src_entity_id.clone(), dst_entity_id))
                                .or_default()
                                .push(ResolvedReference {
                                    field: rel.field.clone(),
                                    relation: "relationship",
                                    target: target_name.clone(),
                                    target_table: None,
                                    target_column: None,
                                    evidence: rel.evidence.clone(),
                                });
                        }
                        None => {
                            push_reference_gap(
                                out,
                                &mut emitted_gaps,
                                repo,
                                &src_entity_id,
                                &rel.field,
                                target_name,
                                "relationship target is not a recovered entity",
                                rel.evidence.clone(),
                            );
                        }
                    }
                }
                RelTarget::Symbol(target_expr) => {
                    let resolved = resolve_symbol(
                        &rel.evidence.path,
                        target_expr,
                        &imports_by_file,
                        &known_files,
                    )
                    .or_else(|| {
                        resolve_symbol(&class.key.0, target_expr, &imports_by_file, &known_files)
                    })
                    .and_then(|key| by_class_key.get(&key).cloned());
                    match resolved {
                        Some(dst_entity_id) => {
                            resolved_references
                                .entry((src_entity_id.clone(), dst_entity_id))
                                .or_default()
                                .push(ResolvedReference {
                                    field: rel.field.clone(),
                                    relation: "relationship",
                                    target: target_expr.clone(),
                                    target_table: None,
                                    target_column: None,
                                    evidence: rel.evidence.clone(),
                                });
                        }
                        None => {
                            push_reference_gap(
                                out,
                                &mut emitted_gaps,
                                repo,
                                &src_entity_id,
                                &rel.field,
                                target_expr,
                                "relationship target is not a recovered entity",
                                rel.evidence.clone(),
                            );
                        }
                    }
                }
                RelTarget::Unprovable(raw) => {
                    push_reference_gap(
                        out,
                        &mut emitted_gaps,
                        repo,
                        &src_entity_id,
                        &rel.field,
                        raw,
                        "relationship target is not provable",
                        rel.evidence.clone(),
                    );
                }
            }
        }
    }

    for ((src_entity_id, dst_entity_id), mut entries) in resolved_references {
        entries.sort_by(|a, b| {
            (a.field.as_str(), a.relation, a.target.as_str()).cmp(&(
                b.field.as_str(),
                b.relation,
                b.target.as_str(),
            ))
        });
        let mut references_json = Vec::with_capacity(entries.len());
        let mut evidence = Vec::with_capacity(entries.len());
        for entry in entries {
            let item = match (entry.target_table, entry.target_column) {
                (Some(table), Some(column)) => serde_json::json!({
                    "field": entry.field,
                    "relation": entry.relation,
                    "target": entry.target,
                    "target_table": table,
                    "target_column": column,
                }),
                _ => serde_json::json!({
                    "field": entry.field,
                    "relation": entry.relation,
                    "target": entry.target,
                }),
            };
            references_json.push(item);
            evidence.push(entry.evidence);
        }
        out.edges.push(Edge {
            src: src_entity_id.clone(),
            dst: dst_entity_id.clone(),
            label: "REFERENCES".into(),
            props: serde_json::json!({
                "references": references_json,
                "framework": "sqlalchemy",
                "prov": prov_from(
                    evidence,
                    ConfidenceTier::Confirmed,
                    &format!("REFERENCES {src_entity_id} -> {dst_entity_id}"),
                ),
            }),
        });
    }

    sqlalchemy_writes::resolve_writes(
        out,
        repo,
        functions_by_file,
        &classes_by_key,
        &by_class_key,
        &proven_bases,
        &proven_registries,
        &proven_flask_dbs,
        &proven_sessions,
        &imports_by_file,
        &known_files,
    );
}
