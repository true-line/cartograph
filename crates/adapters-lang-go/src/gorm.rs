//! GORM models and the reads and writes made through them (#2019).
//!
//! A file is read for GORM only when it imports `gorm.io/gorm` or
//! `github.com/jinzhu/gorm` (under any alias).
//!
//! A model is a struct that embeds the import's `Model` or has a field
//! tagged `gorm:"…"`. It is a Confirmed `DataEntity` (`storage: "gorm"`).
//! Its table is the literal that its `TableName()` method returns in the
//! same package (`table_source: "explicit"`). Without one, the table is
//! GORM's default name, the type name in snake case and plural
//! (`table_source: "default"`). Its columns are the fields of scalar types
//! (numbers, strings, booleans, bytes, times, `sql.Null*`, `gorm.DeletedAt`),
//! each named in snake case or by its `column:` tag. A field tagged `-`
//! is left out. An embedded `gorm.Model` adds `id`, `created_at`,
//! `updated_at` and `deleted_at`. Relation fields are not columns.
//!
//! A GORM method call names the model it reads or writes by its argument.
//! That argument can be a composite literal (`&T{…}`, `T{}`). It can be a
//! variable the function declares with a type, as a parameter or receiver
//! of type `T`, `*T` or `[]T`. It can be `make([]T, …)` or `new(T)`. It can
//! also be the argument of a `Model(…)` earlier in the same chain.
//!
//! The methods map to operations as follows:
//!
//! - `Create` inserts.
//! - `Save`, `Update`, `Updates`, `UpdateColumn` and `UpdateColumns` update.
//! - `Delete` deletes.
//! - `FirstOrCreate` reads and inserts.
//! - `First`, `Find`, `Take`, `Last`, `Count`, `Pluck`, `Scan`,
//!   `FirstOrInit` read.
//!
//! The edges are InferredStrong with resolution `gorm-model`, because the
//! receiver is not proven to be a `*gorm.DB`. A write whose model is not
//! proven is a Gap that names the argument.

use super::*;

const IMPORTS: &[&str] = &["gorm.io/gorm", "github.com/jinzhu/gorm"];

/// A model access held until the package's models are known: an edge whose
/// `dst` is the model's key (`gormtype:{package_dir}#{Type}`).
pub(super) type Pending = Edge;

/// A `TableName()` method's literal, by package directory and type.
#[derive(Debug, Clone)]
pub(super) struct TableName {
    package_dir: String,
    type_name: String,
    table: String,
    /// The `TableName()` method the table is cited from.
    pub(super) evidence: EvidenceRef,
}

fn key(package_dir: &str, type_name: &str) -> String {
    format!("gormtype:{package_dir}#{type_name}")
}

pub(super) fn entity_id(repo: &str, package_dir: &str, type_name: &str) -> String {
    format!("data:{repo}@gorm:{package_dir}#{type_name}")
}

fn gorm_binding(bindings: &HashMap<String, ImportBinding>) -> Option<&str> {
    bindings
        .iter()
        .find(|(_, binding)| IMPORTS.contains(&binding.path.as_str()))
        .map(|(name, _)| name.as_str())
}

/// GORM's default column name: `UserModelID` is `user_model_id`.
pub(super) fn snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let previous = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if previous.is_lowercase()
                || previous.is_ascii_digit()
                || (previous.is_uppercase() && next_lower)
            {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// GORM's default table name: the snake-case name made plural as GORM's
/// inflection library does (`jinzhu/inflection`): uncountable words, then
/// irregular endings, then its suffix rules, the last-declared first.
pub(super) fn table(type_name: &str) -> String {
    plural(&snake(type_name))
}

const UNCOUNTABLE: &[&str] = &[
    "equipment",
    "information",
    "rice",
    "money",
    "species",
    "series",
    "fish",
    "sheep",
    "jeans",
    "police",
];

const IRREGULAR: &[(&str, &str)] = &[
    ("person", "people"),
    ("man", "men"),
    ("child", "children"),
    ("sex", "sexes"),
    ("move", "moves"),
    ("mombie", "mombies"),
];

/// `jinzhu/inflection`'s `Plural` on a lower-case word.
pub(super) fn plural(word: &str) -> String {
    // Its first rule, `([a-z])$`, needs a final letter.
    if UNCOUNTABLE.contains(&word) || !word.ends_with(|c: char| c.is_ascii_alphabetic()) {
        return word.to_string();
    }
    for (singular, plural) in IRREGULAR {
        if let Some(stem) = word.strip_suffix(singular) {
            return format!("{stem}{plural}");
        }
    }
    let ends = |suffix: &str| word.ends_with(suffix);
    let cut = |n: usize| &word[..word.len() - n];
    let before = |n: usize| word[..word.len() - n].chars().next_back();
    // `(drive)$`, `(quiz)$`
    if ends("drive") {
        return format!("{word}s");
    }
    if ends("quiz") {
        return format!("{word}zes");
    }
    // `^(oxen)$`, `^(ox)$`, `^(m|l)ice$`, `^(m|l)ouse$`
    match word {
        "oxen" | "mice" | "lice" => return word.to_string(),
        "ox" => return "oxen".into(),
        "mouse" => return "mice".into(),
        "louse" => return "lice".into(),
        _ => {}
    }
    // `(matr|vert|ind)(?:ix|ex)$`
    for stem in ["matr", "vert", "ind"] {
        for end in ["ix", "ex"] {
            if let Some(rest) = word.strip_suffix(end)
                && rest.ends_with(stem)
            {
                return format!("{rest}ices");
            }
        }
    }
    // `(x|ch|ss|sh)$`
    if ["x", "ch", "ss", "sh"].iter().any(|e| ends(e)) {
        return format!("{word}es");
    }
    // `([^aeiouy]|qu)y$`
    if ends("y") && (ends("quy") || before(1).is_some_and(|c| !"aeiouy".contains(c))) {
        return format!("{}ies", cut(1));
    }
    // `(hive)$`
    if ends("hive") {
        return format!("{word}s");
    }
    // `(?:([^f])fe|([lr])f)$`
    if ends("fe") && before(2).is_some_and(|c| c != 'f') {
        return format!("{}ves", cut(2));
    }
    if ends("f") && before(1).is_some_and(|c| c == 'l' || c == 'r') {
        return format!("{}ves", cut(1));
    }
    // `sis$`
    if ends("sis") {
        return format!("{}ses", cut(3));
    }
    // `([ti])a$`, `([ti])um$`
    if ends("ta") || ends("ia") {
        return word.to_string();
    }
    if ends("tum") || ends("ium") {
        return format!("{}a", cut(2));
    }
    // `(buffal|tomat)o$`, `(bu)s$`, `(alias|status|campus)$`
    if ends("buffalo") || ends("tomato") {
        return format!("{word}es");
    }
    if ends("bus") {
        return format!("{word}es");
    }
    if ["alias", "status", "campus"].iter().any(|e| ends(e)) {
        return format!("{word}es");
    }
    // `(octop|vir)i$`, `(octop|vir)us$`, `^(ax|test)is$`
    if ends("octopi") || ends("viri") {
        return word.to_string();
    }
    if ends("octopus") || ends("virus") {
        return format!("{}i", cut(2));
    }
    if word == "axis" || word == "testis" {
        return format!("{}es", cut(2));
    }
    // `s$`, `$`
    if ends("s") {
        return word.to_string();
    }
    format!("{word}s")
}

/// The base type a field or variable type names, with the package
/// qualifier when it has one, through `*`, `[]` and `[N]`.
fn base_type<'t>(cx: &FileCx<'_>, mut ty: TsNode<'t>) -> Option<(Option<String>, String)> {
    loop {
        match ty.kind() {
            "pointer_type" | "parenthesized_type" => ty = ty.named_child(0)?,
            "slice_type" | "array_type" => ty = ty.child_by_field_name("element")?,
            "type_identifier" => return Some((None, cx.text(&ty).to_string())),
            "qualified_type" => {
                return Some((
                    Some(cx.text(&ty.child_by_field_name("package")?).to_string()),
                    cx.text(&ty.child_by_field_name("name")?).to_string(),
                ));
            }
            "generic_type" => ty = ty.child_by_field_name("type")?,
            _ => return None,
        }
    }
}

/// Whether a field type is a column: a scalar, a time, or a nullable or
/// soft-delete wrapper. A qualified type is read through the file's import
/// of its package, whatever name it is bound to.
fn scalar(cx: &FileCx<'_>, ty: TsNode<'_>, bindings: &HashMap<String, ImportBinding>) -> bool {
    let text = cx.text(&ty).trim_start_matches('*');
    if matches!(
        text,
        "string"
            | "bool"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "float32"
            | "float64"
            | "byte"
            | "rune"
            | "[]byte"
    ) {
        return true;
    }
    let Some((qualifier, name)) = text.split_once('.') else {
        return false;
    };
    let Some(binding) = bindings.get(qualifier) else {
        return false;
    };
    match binding.path.as_str() {
        "time" => name == "Time",
        "gorm.io/gorm" | "github.com/jinzhu/gorm" => name == "DeletedAt",
        "github.com/google/uuid" | "github.com/gofrs/uuid" | "github.com/satori/go.uuid" => {
            name == "UUID"
        }
        "encoding/json" => name == "RawMessage",
        "database/sql" => name.starts_with("Null"),
        "gorm.io/datatypes" => true,
        _ => false,
    }
}

fn tag_value(tag: &str, key: &str) -> Option<String> {
    let gorm = tag.split("gorm:\"").nth(1)?.split('"').next()?;
    gorm.split(';').find_map(|part| {
        let (k, v) = part.split_once(':').unwrap_or((part, ""));
        (k.trim().eq_ignore_ascii_case(key)).then(|| v.trim().to_string())
    })
}

fn has_gorm_tag(tag: &str) -> bool {
    tag.contains("gorm:\"")
}

/// Append the file's GORM models, `TableName()` literals and pending
/// accesses.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &HashMap<usize, String>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) {
    let dir = package_dir(cx.path);
    // `TableName()` may sit in a file that does not import GORM.
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        if node.kind() == "method_declaration" {
            table_name(cx, node, &dir, out);
        }
    }
    let Some(gorm) = gorm_binding(bindings) else {
        return;
    };
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
        match node.kind() {
            "type_spec" => model(cx, node, gorm, bindings, &dir, out),
            "call_expression" => access(cx, node, functions, bindings, &dir, out),
            _ => {}
        }
    }
}

fn model(
    cx: &FileCx<'_>,
    spec: TsNode<'_>,
    gorm: &str,
    bindings: &HashMap<String, ImportBinding>,
    dir: &str,
    out: &mut Extraction,
) {
    let (Some(name), Some(ty)) = (
        spec.child_by_field_name("name"),
        spec.child_by_field_name("type"),
    ) else {
        return;
    };
    if ty.kind() != "struct_type" {
        return;
    }
    let Some(fields) = ty.named_child(0) else {
        return;
    };
    let mut is_model = false;
    let mut columns: Vec<serde_json::Value> = Vec::new();
    let mut cursor = fields.walk();
    for field in fields.named_children(&mut cursor) {
        if field.kind() != "field_declaration" {
            continue;
        }
        let tag = field
            .child_by_field_name("tag")
            .map(|tag| cx.text(&tag).to_string())
            .unwrap_or_default();
        is_model |= has_gorm_tag(&tag);
        let Some(field_type) = field.child_by_field_name("type") else {
            continue;
        };
        let mut names_cursor = field.walk();
        let names: Vec<String> = field
            .children_by_field_name("name", &mut names_cursor)
            .map(|n| cx.text(&n).to_string())
            .collect();
        if names.is_empty() {
            // An embedded field.
            if cx.text(&field_type).trim_start_matches('*') == format!("{gorm}.Model") {
                is_model = true;
                for column in ["id", "created_at", "updated_at", "deleted_at"] {
                    columns.push(serde_json::json!({ "name": column, "embedded": "gorm.Model" }));
                }
            }
            continue;
        }
        if tag_value(&tag, "-").is_some() || !scalar(cx, field_type, bindings) {
            continue;
        }
        for field_name in names {
            if !field_name.starts_with(|c: char| c.is_uppercase()) {
                continue;
            }
            let column = tag_value(&tag, "column")
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| snake(&field_name));
            let mut entry = serde_json::json!({
                "name": column,
                "field": field_name,
                "type": cx.text(&field_type),
            });
            if tag_value(&tag, "primaryKey").is_some() || tag_value(&tag, "primary_key").is_some() {
                entry["primary_key"] = true.into();
            }
            columns.push(entry);
        }
    }
    if !is_model {
        return;
    }
    let type_name = cx.text(&name).to_string();
    let id = entity_id(cx.id.repo, dir, &type_name);
    let table = table(&type_name);
    out.nodes.push(Node {
        props: serde_json::json!({
            "name": type_name,
            "storage": "gorm",
            "store": table,
            "table": table,
            "table_source": "default",
            "columns": columns,
            "framework": "gorm",
            "language": "go",
            "package_dir": dir,
            "prov": cx.prov(&spec, &format!("DataEntity {id}")),
        }),
        id,
        label: "DataEntity".into(),
    });
}

/// `func (T) TableName() string { return "literal" }`.
fn table_name(cx: &FileCx<'_>, method: TsNode<'_>, dir: &str, out: &mut Extraction) {
    if method
        .child_by_field_name("name")
        .is_none_or(|name| cx.text(&name) != "TableName")
    {
        return;
    }
    let Some(type_name) = receiver_type(cx, method) else {
        return;
    };
    let Some(body) = method.child_by_field_name("body") else {
        return;
    };
    let body = match body.named_child(0) {
        Some(list) if list.kind() == "statement_list" => list,
        _ => body,
    };
    let mut cursor = body.walk();
    let statements: Vec<_> = body
        .named_children(&mut cursor)
        .filter(|s| s.kind() != "comment")
        .collect();
    let [statement] = statements[..] else {
        return;
    };
    if statement.kind() != "return_statement" {
        return;
    }
    let Some(value) = statement.named_child(0).and_then(|list| {
        if list.kind() == "expression_list" {
            list.named_child(0)
        } else {
            Some(list)
        }
    }) else {
        return;
    };
    if let Some(table) = literal_string(cx, value) {
        out.gorm_table_names.push(TableName {
            package_dir: dir.to_string(),
            type_name,
            table,
            evidence: EvidenceRef {
                repo: cx.id.repo.into(),
                path: cx.path.into(),
                byte_start: method.start_byte() as u64,
                byte_end: method.end_byte() as u64,
                commit_sha: cx.id.commit.into(),
            },
        });
    }
}

/// The operation each GORM method performs, and whether it writes.
fn operation(method: &str) -> Option<(&'static [&'static str], bool)> {
    Some(match method {
        "Create" => (&["insert"], true),
        "Save" | "Update" | "Updates" | "UpdateColumn" | "UpdateColumns" => (&["update"], true),
        "Delete" => (&["delete"], true),
        "FirstOrCreate" => (&["read", "insert"], true),
        "First" | "Find" | "Take" | "Last" | "Count" | "Pluck" | "Scan" | "FirstOrInit" => {
            (&["read"], false)
        }
        _ => return None,
    })
}

/// Where a method's model argument is: `Pluck(column, &dest)`, `Count(&n)`
/// and `Update(column, value)` take none of their own.
fn model_argument(method: &str) -> Option<usize> {
    match method {
        // `Updates(&user)` names its model; `Update("name", v)` names a column.
        "Count" | "Pluck" | "Update" | "UpdateColumn" => None,
        _ => Some(0),
    }
}

fn access(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    functions: &HashMap<usize, String>,
    bindings: &HashMap<String, ImportBinding>,
    dir: &str,
    out: &mut Extraction,
) {
    let Some(function) = call.child_by_field_name("function") else {
        return;
    };
    if function.kind() != "selector_expression" {
        return;
    }
    let Some(method) = function.child_by_field_name("field").map(|f| cx.text(&f)) else {
        return;
    };
    let Some((operations, write)) = operation(method) else {
        return;
    };
    let Some(operand) = function.child_by_field_name("operand") else {
        return;
    };
    // A package function (`gorm.Open`) is no access.
    if operand.kind() == "identifier" && bindings.contains_key(cx.text(&operand)) {
        return;
    }
    let own = model_argument(method).and_then(|index| call_arguments(call).get(index).copied());
    let chained = chain_model(cx, operand);
    // GORM takes the table from `Model(…)` when the chain sets one, and from
    // the destination otherwise (`Model(&User{}).Scan(&summary)` reads users).
    let Some(argument) = chained.or(own) else {
        return;
    };
    let src = enclosing_function(call, functions).unwrap_or_else(|| file_id(cx.id.repo, cx.path));
    // An explicit `Model(…)` selects the table: when it is not resolved the
    // destination does not stand in for it.
    let resolved = match chained {
        Some(model) => model_type(cx, model, call, bindings, dir),
        None => own.and_then(|a| model_type(cx, a, call, bindings, dir)),
    };
    let label = if write { "WRITES" } else { "READS" };
    let Some((package, type_name)) = resolved else {
        if write {
            gap(cx, call, argument, &src, label, out);
        }
        return;
    };
    let dst = key(&package, &type_name);
    // A write whose type is no recovered model is a Gap at the join.
    let unresolved = write.then(|| gap_facts(cx, call, argument, &src, label));
    out.pending_gorm.push(Edge {
        props: serde_json::json!({
            "unresolved": unresolved,
            "operation": operations[0],
            "operations": operations,
            "method": method,
            "resolution": "gorm-model",
            "prov": cx.prov_with_confidence(&call, ConfidenceTier::InferredStrong, &format!("{label} {src} -> {type_name}")),
        }),
        src,
        dst,
        label: label.into(),
    });
}

/// The argument of a `Model(…)` call earlier in the chain `operand` ends.
fn chain_model<'t>(cx: &FileCx<'_>, mut operand: TsNode<'t>) -> Option<TsNode<'t>> {
    while operand.kind() == "call_expression" {
        let function = operand.child_by_field_name("function")?;
        if function.kind() != "selector_expression" {
            return None;
        }
        if function
            .child_by_field_name("field")
            .is_some_and(|f| cx.text(&f) == "Model")
        {
            return call_arguments(operand).first().copied();
        }
        operand = function.child_by_field_name("operand")?;
    }
    None
}

/// The (package directory, type) a model argument names.
fn model_type(
    cx: &FileCx<'_>,
    argument: TsNode<'_>,
    at: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    dir: &str,
) -> Option<(String, String)> {
    let mut expression = argument;
    if expression.kind() == "unary_expression" {
        expression = expression.child_by_field_name("operand")?;
    }
    let ty = match expression.kind() {
        "composite_literal" => expression.child_by_field_name("type")?,
        "identifier" => variable_type(cx, at, cx.text(&expression))?,
        "call_expression" => {
            let function = expression.child_by_field_name("function")?;
            if !matches!(cx.text(&function), "make" | "new") {
                return None;
            }
            expression
                .child_by_field_name("arguments")?
                .named_child(0)?
        }
        _ => return None,
    };
    let (package, name) = base_type(cx, ty)?;
    // `any` is `interface{}`: no model.
    if package.is_none() && name == "any" {
        return None;
    }
    let package_dir = match package {
        None => dir.to_string(),
        Some(package) => bindings.get(&package)?.package_dir.clone()?,
    };
    Some((package_dir, name))
}

/// The type node of the declaration of `name` visible at `at`: the nearest
/// enclosing scope that declares it before `at` (a parameter or receiver,
/// `var name T`, or `name := T{…}` / `&T{…}` / `make([]T…)` / `new(T)`),
/// its latest declaration there. A visible declaration whose type is not
/// stated (`name := load()`, a range variable) shadows any outer one and
/// gives no type.
fn variable_type<'t>(cx: &FileCx<'_>, at: TsNode<'t>, name: &str) -> Option<TsNode<'t>> {
    let mut child = at;
    while let Some(scope) = child.parent() {
        let mut found: Option<Option<TsNode<'t>>> = None;
        let mut cursor = scope.walk();
        for node in scope.named_children(&mut cursor) {
            if node.start_byte() >= at.start_byte() {
                break;
            }
            // The statement holding `at` declares after its right side.
            if node == child && node.kind() != "for_clause" {
                continue;
            }
            if let Some(declared) = declaration(cx, node, name) {
                found = Some(declared);
            }
        }
        if let Some(found) = found {
            return found;
        }
        child = scope;
    }
    None
}

/// Whether `node` declares `name`, and with which stated type.
fn declaration<'t>(cx: &FileCx<'_>, node: TsNode<'t>, name: &str) -> Option<Option<TsNode<'t>>> {
    let named = |node: TsNode<'t>| {
        let mut cursor = node.walk();
        node.children_by_field_name("name", &mut cursor)
            .any(|n| cx.text(&n) == name)
    };
    match node.kind() {
        "parameter_list" | "var_declaration" | "var_spec_list" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter_map(|c| declaration(cx, c, name))
                .last()
        }
        "parameter_declaration" | "variadic_parameter_declaration" if named(node) => {
            Some(node.child_by_field_name("type"))
        }
        "var_spec" if named(node) => Some(node.child_by_field_name("type").or_else(|| {
            let value = node.child_by_field_name("value")?;
            value_type(cx, value.named_child(0)?)
        })),
        "for_clause" => declaration(cx, node.child_by_field_name("initializer")?, name),
        "short_var_declaration" | "range_clause" => {
            let left = node.child_by_field_name("left")?;
            let mut cursor = left.walk();
            let lefts: Vec<_> = left.named_children(&mut cursor).collect();
            let position = lefts.iter().position(|l| cx.text(l) == name)?;
            if node.kind() == "range_clause" {
                return Some(None);
            }
            let right = node.child_by_field_name("right")?;
            let mut cursor = right.walk();
            let rights: Vec<_> = right.named_children(&mut cursor).collect();
            if rights.len() != lefts.len() {
                return Some(None);
            }
            Some(value_type(cx, rights[position]))
        }
        _ => None,
    }
}

/// The type a value expression has when it states it: `T{…}`, `&T{…}`,
/// `make([]T, …)`, `new(T)`.
fn value_type<'t>(cx: &FileCx<'_>, mut value: TsNode<'t>) -> Option<TsNode<'t>> {
    if value.kind() == "unary_expression" {
        value = value.child_by_field_name("operand")?;
    }
    match value.kind() {
        "composite_literal" => value.child_by_field_name("type"),
        "call_expression" => {
            let function = value.child_by_field_name("function")?;
            if !matches!(cx.text(&function), "make" | "new") {
                return None;
            }
            value.child_by_field_name("arguments")?.named_child(0)
        }
        _ => None,
    }
}

fn gap(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    argument: TsNode<'_>,
    src: &str,
    label: &str,
    out: &mut Extraction,
) {
    push_gap(gap_facts(cx, call, argument, src, label), out);
}

/// The Gap node and edge of an unproven write, as JSON a pending access can
/// carry to the join.
fn gap_facts(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    argument: TsNode<'_>,
    src: &str,
    label: &str,
) -> serde_json::Value {
    let reason = "GORM model type is not proven";
    let gap = format!("gap:gorm:{}@{}@{}", cx.id.repo, cx.path, call.start_byte());
    serde_json::json!({
        "node": {
            "id": gap,
            "props": {
                "reason": reason,
                "expression": cx.text(&argument).chars().take(160).collect::<String>(),
                "attempted_tiers": ["T0"],
                "prov": cx.prov_with_confidence(&call, ConfidenceTier::Gap, &format!("Gap {gap}")),
            },
        },
        "edge": {
            "src": src,
            "label": label,
            "props": {
                "reason": reason,
                "prov": cx.prov_with_confidence(&call, ConfidenceTier::Gap, &format!("{label} {src} -> GORM")),
            },
        },
    })
}

fn push_gap(facts: serde_json::Value, out: &mut Extraction) {
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let id = text(&facts["node"]["id"]);
    out.nodes.push(Node {
        id: id.clone(),
        label: "Gap".into(),
        props: facts["node"]["props"].clone(),
    });
    out.edges.push(Edge {
        src: text(&facts["edge"]["src"]),
        dst: id,
        label: text(&facts["edge"]["label"]),
        props: facts["edge"]["props"].clone(),
    });
}

/// Apply `TableName()` literals and join the pending accesses to the
/// models, merging accesses of one function to one model.
pub(super) fn resolve(out: &mut Extraction) {
    let tables: BTreeMap<(String, String), (String, EvidenceRef)> =
        std::mem::take(&mut out.gorm_table_names)
            .into_iter()
            .map(|t| ((t.package_dir, t.type_name), (t.table, t.evidence)))
            .collect();
    let mut models: BTreeMap<String, String> = BTreeMap::new();
    for node in &mut out.nodes {
        if node.label != "DataEntity" || node.props["storage"] != "gorm" {
            continue;
        }
        let dir = node.props["package_dir"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let name = node.props["name"].as_str().unwrap_or_default().to_string();
        if let Some((table, evidence)) = tables.get(&(dir.clone(), name.clone())) {
            node.props["table"] = table.clone().into();
            node.props["store"] = table.clone().into();
            node.props["table_source"] = "explicit".into();
            // The table literal is cited from its `TableName()` method too.
            if let Ok(mut provenance) =
                serde_json::from_value::<core_prov::Provenance>(node.props["prov"].clone())
                && !provenance.evidence.contains(evidence)
            {
                provenance.evidence.push(evidence.clone());
                node.props["prov"] =
                    serde_json::to_value(provenance).expect("provenance serializes");
            }
        }
        models.insert(key(&dir, &name), node.id.clone());
    }
    let mut merged: BTreeMap<(String, String, String), Edge> = BTreeMap::new();
    for mut edge in std::mem::take(&mut out.pending_gorm) {
        let unresolved = edge
            .props
            .as_object_mut()
            .and_then(|props| props.remove("unresolved"))
            .filter(|facts| !facts.is_null());
        let Some(dst) = models.get(&edge.dst) else {
            if let Some(facts) = unresolved {
                push_gap(facts, out);
            }
            continue;
        };
        edge.dst = dst.clone();
        let slot = (edge.src.clone(), edge.dst.clone(), edge.label.clone());
        match merged.get_mut(&slot) {
            None => {
                merged.insert(slot, edge);
            }
            Some(kept) => {
                let mut operations: BTreeSet<String> = kept.props["operations"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(edge.props["operations"].as_array().into_iter().flatten())
                    .filter_map(|o| o.as_str().map(str::to_string))
                    .collect();
                if kept.label == "WRITES" {
                    operations.remove("read");
                }
                kept.props["operations"] = operations.iter().cloned().collect::<Vec<_>>().into();
            }
        }
    }
    for (_, mut edge) in merged {
        if edge.label == "WRITES"
            && let Some(ops) = edge.props["operations"].as_array_mut()
        {
            ops.retain(|o| o != "read");
            if let Some(first) = ops.first().cloned() {
                edge.props["operation"] = first;
            }
        }
        out.edges.push(edge);
    }
}
