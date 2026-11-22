//! Alembic migration replay (#833): the table and column shape the
//! repository's migration history builds, compared with the recovered
//! SQLAlchemy `DataEntity` facts. `alembic.ini` names the script location;
//! its `versions/*.py` revisions must form one `down_revision` history with
//! one root and one head (branches and merges allowed).
//! Each `upgrade()` is replayed with literal arguments only (`create_table`,
//! `drop_table`, `rename_table`, `add_column`, `drop_column`,
//! `alter_column(new_column_name=)`); raw SQL is read only when it is a
//! literal whose statements cannot change a table's columns, or names the one
//! table they may change. A table an op cannot be replayed for is unknown and
//! never reported, as is a table two parallel branches both change (their
//! order is not fixed); a history that cannot be replayed at all is one `Gap`.
//! Each difference is a `Drift` node (`kind = "schema-drift"`):
//! `modeled-not-migrated` or `migrated-not-modeled`, InferredStrong because
//! the replay assumes Alembic runs exactly this chain.

use super::{
    BTreeMap, BTreeSet, ConfidenceTier, Edge, EvidenceRef, ExtractError, Extraction, FileCx,
    HashMap, Node, SourceId, TsNode, literal_string, prov_from,
};
use std::path::{Component, Path};
use tree_sitter::Parser;

/// Revision files read at most by default; more leaves the replay a Gap.
pub(crate) const MAX_REVISIONS: usize = 4096;
/// Bytes read at most by default from `alembic.ini` or one revision file.
pub(crate) const MAX_FILE_BYTES: u64 = 1 << 20;

/// Alembic operations that never change which tables or columns exist.
const SHAPELESS_OPS: &[&str] = &[
    "bulk_insert",
    "create_check_constraint",
    "create_exclude_constraint",
    "create_foreign_key",
    "create_index",
    "create_primary_key",
    "create_table_comment",
    "create_unique_constraint",
    "drop_constraint",
    "drop_index",
    "drop_table_comment",
    "f",
    "get_bind",
    "get_context",
    "inline_literal",
];

/// The replayed state of one table.
#[derive(Clone, PartialEq)]
struct Table {
    columns: BTreeMap<String, EvidenceRef>,
    /// The last op that touched the table.
    evidence: EvidenceRef,
}

#[derive(Clone, Default)]
struct Schema {
    tables: BTreeMap<String, Table>,
    /// Tables some op could not be replayed for, with why and where:
    /// never compared.
    unknown: BTreeMap<String, (String, EvidenceRef)>,
}

impl Schema {
    /// The table's shape is unknown from here on; the first cause is kept.
    fn forget(&mut self, table: &str, reason: &str, evidence: &EvidenceRef) {
        self.tables.remove(table);
        self.unknown
            .entry(table.to_string())
            .or_insert_with(|| (reason.to_string(), evidence.clone()));
    }

    /// Tables whose state differs from `before`.
    fn changed_since(&self, before: &Schema) -> BTreeSet<String> {
        let names = self.tables.keys().chain(before.tables.keys());
        let mut changed: BTreeSet<String> = names
            .filter(|name| self.tables.get(*name) != before.tables.get(*name))
            .cloned()
            .collect();
        let unknown = |schema: &Schema| schema.unknown.keys().cloned().collect::<BTreeSet<_>>();
        changed.extend(
            unknown(self)
                .symmetric_difference(&unknown(before))
                .cloned(),
        );
        changed
    }
}

/// A replayed history: where it lives, its head and the schema it builds.
struct History {
    location: String,
    head: String,
    head_path: String,
    revisions: usize,
    schema: Schema,
}

/// Why the history as a whole cannot be replayed, and where.
struct Inconclusive {
    reason: String,
    evidence: EvidenceRef,
}

type Replay<T> = Result<T, Inconclusive>;

struct Revision {
    path: String,
    source: Vec<u8>,
    tree: tree_sitter::Tree,
    revision: String,
    /// `down_revision`: none for the root, several for a merge.
    parents: Vec<String>,
}

/// Replay the repository's Alembic history, if it has one, and compare its
/// head schema with the `DataEntity` facts already in `out`.
pub(crate) fn extract(
    root: &Path,
    id: &SourceId<'_>,
    out: &mut Extraction,
    max_revisions: usize,
    max_file_bytes: u64,
) -> Result<(), ExtractError> {
    let ini = root.join("alembic.ini");
    let Ok(meta) = std::fs::symlink_metadata(&ini) else {
        return Ok(());
    };
    let whole = |path: &str| EvidenceRef {
        repo: id.repo.into(),
        path: path.into(),
        byte_start: 0,
        byte_end: 0,
        commit_sha: id.commit.into(),
    };
    let result = (|| {
        let inconclusive = |reason: &str| Inconclusive {
            reason: reason.into(),
            evidence: whole("alembic.ini"),
        };
        if !meta.is_file() || meta.len() > max_file_bytes {
            return Err(inconclusive(
                "alembic.ini is not a regular file within the size limit",
            ));
        }
        let text = String::from_utf8(
            std::fs::read(&ini).map_err(|_| inconclusive("alembic.ini is unreadable"))?,
        )
        .map_err(|_| inconclusive("alembic.ini is not UTF-8"))?;
        let location = script_location(&text)
            .ok_or_else(|| inconclusive("alembic.ini names no literal relative script_location"))?;
        let revisions = read_revisions(root, id, &location, max_revisions, max_file_bytes)
            .map_err(|reason| inconclusive(&reason))?;
        let (order, ancestors) = order(revisions, id)?;
        let at = |revision: &Revision| whole(&revision.path);
        let mut schema = Schema::default();
        let mut touched = Vec::with_capacity(order.len());
        for revision in &order {
            let before = schema.clone();
            replay(revision, id, &mut schema)?;
            touched.push(schema.changed_since(&before));
        }
        // Alembic fixes no order between parallel branches: a table two of
        // them change is unknown.
        for later in 0..order.len() {
            for earlier in 0..later {
                if !ancestors[later].contains(&earlier) {
                    for table in touched[earlier].intersection(&touched[later]) {
                        schema.forget(
                            table,
                            "changed on two parallel migration branches",
                            &at(&order[later]),
                        );
                    }
                }
            }
        }
        // The one head comes last in any upgrade order.
        Ok(order.last().map(|head| History {
            location,
            head: head.revision.clone(),
            head_path: head.path.clone(),
            revisions: order.len(),
            schema,
        }))
    })();
    match result {
        Ok(Some(history)) => compare(root, id, &history, out),
        Ok(None) => {}
        Err(inconclusive) => push_gap(id, inconclusive, out),
    }
    Ok(())
}

/// `script_location` in the `[alembic]` section: a relative path of plain
/// segments, no interpolation.
fn script_location(ini: &str) -> Option<String> {
    let mut section = "";
    for line in ini.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim();
            continue;
        }
        if section != "alembic" {
            continue;
        }
        let Some((key, value)) = line.split_once(['=', ':']) else {
            continue;
        };
        if key.trim() != "script_location" {
            continue;
        }
        let value = value.trim();
        let path = Path::new(value);
        let plain = !value.is_empty()
            && path
                .components()
                .all(|c| matches!(c, Component::Normal(s) if s.to_str().is_some_and(|s| s.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')))));
        return plain.then(|| value.trim_end_matches('/').to_string());
    }
    None
}

/// Every `<location>/versions/*.py`, parsed, with its literal `revision` and
/// `down_revision`. Symlinks anywhere on the way are refused.
fn read_revisions(
    root: &Path,
    id: &SourceId<'_>,
    location: &str,
    max_revisions: usize,
    max_file_bytes: u64,
) -> Result<Vec<Revision>, String> {
    let mut dir = root.to_path_buf();
    for segment in Path::new(location).join("versions").components() {
        dir.push(segment);
        let meta = std::fs::symlink_metadata(&dir)
            .map_err(|_| format!("{location}/versions is missing"))?;
        if !meta.is_dir() {
            return Err(format!("{location}/versions is not a plain directory"));
        }
    }
    let mut names = Vec::new();
    for entry in
        std::fs::read_dir(&dir).map_err(|_| format!("{location}/versions is unreadable"))?
    {
        let entry = entry.map_err(|_| format!("{location}/versions is unreadable"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".py") || name.starts_with('.') {
            continue;
        }
        let meta = entry
            .metadata()
            .map_err(|_| format!("{location}/versions/{name} is unreadable"))?;
        if entry
            .file_type()
            .map_err(|_| format!("{location}/versions/{name} is unreadable"))?
            .is_symlink()
            || !meta.is_file()
            || meta.len() > max_file_bytes
        {
            return Err(format!(
                "{location}/versions/{name} is not a regular file within the size limit"
            ));
        }
        names.push(name);
        if names.len() > max_revisions {
            return Err(format!("more than {max_revisions} revision files"));
        }
    }
    names.sort();
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .map_err(|_| "Python grammar unavailable".to_string())?;
    let mut revisions = Vec::new();
    for name in names {
        let path = format!("{location}/versions/{name}");
        let source = std::fs::read(dir.join(&name)).map_err(|_| format!("{path} is unreadable"))?;
        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| format!("{path} does not parse"))?;
        let (revision, parents) = {
            let cx = FileCx {
                source: &source,
                path: &path,
                id,
            };
            let root = tree.root_node();
            let revision = module_value(&cx, root, "revision")
                .and_then(|value| literal_string(&cx, value))
                .ok_or_else(|| format!("{path} has no literal revision"))?;
            let parents = module_value(&cx, root, "down_revision")
                .and_then(|value| revision_list(&cx, value))
                .ok_or_else(|| format!("{path} has no literal down_revision"))?;
            (revision, parents)
        };
        revisions.push(Revision {
            path,
            source,
            tree,
            revision,
            parents,
        });
    }
    Ok(revisions)
}

/// The value of the one module-level `name = …` (or annotated)
/// assignment; none when there are several.
fn module_value<'t>(cx: &FileCx<'_>, root: TsNode<'t>, name: &str) -> Option<TsNode<'t>> {
    let mut found = None;
    let mut walk = root.walk();
    for statement in root.named_children(&mut walk) {
        let Some(assignment) = statement
            .named_child(0)
            .filter(|n| statement.kind() == "expression_statement" && n.kind() == "assignment")
        else {
            continue;
        };
        let (Some(left), Some(right)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) else {
            continue;
        };
        if left.kind() != "identifier" || cx.text(&left) != name {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(right);
    }
    found
}

/// `None`, `"a"`, or a tuple or list of literals.
fn revision_list(cx: &FileCx<'_>, value: TsNode<'_>) -> Option<Vec<String>> {
    match value.kind() {
        "none" => Some(Vec::new()),
        "tuple" | "list" => {
            let mut walk = value.walk();
            value
                .named_children(&mut walk)
                .filter(|n| n.kind() != "comment")
                .map(|n| literal_string(cx, n))
                .collect()
        }
        _ => literal_string(cx, value).map(|one| vec![one]),
    }
}

/// The revisions in an upgrade order, with each one's ancestors (as
/// positions in that order): unique ids, every parent present, one root,
/// one head and no cycle. Ready revisions go in file-name order.
fn order(
    revisions: Vec<Revision>,
    id: &SourceId<'_>,
) -> Replay<(Vec<Revision>, Vec<BTreeSet<usize>>)> {
    let refuse = |revision: &Revision, reason: &str| Inconclusive {
        reason: reason.into(),
        evidence: EvidenceRef {
            repo: id.repo.into(),
            path: revision.path.clone(),
            byte_start: 0,
            byte_end: 0,
            commit_sha: id.commit.into(),
        },
    };
    let mut index = HashMap::new();
    for (at, revision) in revisions.iter().enumerate() {
        if index.insert(revision.revision.clone(), at).is_some() {
            return Err(refuse(revision, "two migrations share a revision id"));
        }
    }
    let mut children = vec![Vec::new(); revisions.len()];
    let mut waiting = vec![0usize; revisions.len()];
    for (at, revision) in revisions.iter().enumerate() {
        for parent in &revision.parents {
            let Some(&parent) = index.get(parent) else {
                return Err(refuse(revision, "a down_revision names no migration"));
            };
            children[parent].push(at);
            waiting[at] += 1;
        }
    }
    let roots = revisions.iter().filter(|r| r.parents.is_empty()).count();
    let heads = children.iter().filter(|c| c.is_empty()).count();
    if let Some(first) = revisions.first()
        && (roots != 1 || heads != 1)
    {
        return Err(refuse(
            first,
            "migration history has more than one root or head",
        ));
    }
    let mut ready: BTreeSet<usize> = (0..revisions.len())
        .filter(|&at| waiting[at] == 0)
        .collect();
    let mut position = vec![usize::MAX; revisions.len()];
    let mut sequence = Vec::with_capacity(revisions.len());
    let mut ancestors: Vec<BTreeSet<usize>> = Vec::with_capacity(revisions.len());
    while let Some(at) = ready.pop_first() {
        let mut known = BTreeSet::new();
        for parent in &revisions[at].parents {
            let parent = position[index[parent]];
            known.insert(parent);
            known.extend(ancestors[parent].iter().copied());
        }
        position[at] = sequence.len();
        sequence.push(at);
        ancestors.push(known);
        for &child in &children[at] {
            waiting[child] -= 1;
            if waiting[child] == 0 {
                ready.insert(child);
            }
        }
    }
    if let Some((_, stuck)) = revisions
        .iter()
        .enumerate()
        .find(|(at, _)| position[*at] == usize::MAX)
    {
        return Err(refuse(stuck, "migration history has a cycle"));
    }
    let mut slots: Vec<Option<Revision>> = revisions.into_iter().map(Some).collect();
    let ordered = sequence
        .into_iter()
        .map(|at| slots[at].take().expect("each revision is placed once"))
        .collect();
    Ok((ordered, ancestors))
}

/// Replay one revision's `upgrade()` into `schema`.
fn replay(revision: &Revision, id: &SourceId<'_>, schema: &mut Schema) -> Replay<()> {
    let cx = FileCx {
        source: &revision.source,
        path: &revision.path,
        id,
    };
    let root = revision.tree.root_node();
    let inconclusive = |node: TsNode<'_>, reason: &str| Inconclusive {
        reason: reason.into(),
        evidence: cx.evidence(&node),
    };
    let mut upgrade = None;
    let mut op_bound = false;
    let mut walk = root.walk();
    for statement in root.named_children(&mut walk) {
        match statement.kind() {
            "function_definition" => {
                let name = statement.child_by_field_name("name").map(|n| cx.text(&n));
                match name {
                    Some("upgrade") if upgrade.is_none() => upgrade = Some(statement),
                    Some("upgrade") => {
                        return Err(inconclusive(statement, "revision defines upgrade() twice"));
                    }
                    Some("downgrade") => {}
                    // A helper may run ops the replay would not see.
                    _ if mentions_op(&cx, statement) => {
                        return Err(inconclusive(statement, "migration op outside upgrade()"));
                    }
                    _ => {}
                }
            }
            "import_from_statement" => {
                let text = cx
                    .text(&statement)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                op_bound |= text == "from alembic import op";
            }
            "import_statement" => {}
            // A module-level op (a statement, class body or decorated
            // helper) runs outside the replay.
            _ if mentions_op(&cx, statement) => {
                return Err(inconclusive(statement, "migration op outside upgrade()"));
            }
            _ => {}
        }
    }
    let Some(upgrade) = upgrade else {
        return Err(inconclusive(root, "revision has no upgrade()"));
    };
    let body = upgrade
        .child_by_field_name("body")
        .ok_or_else(|| inconclusive(upgrade, "revision has no upgrade() body"))?;
    if !op_bound && mentions_op(&cx, body) {
        return Err(inconclusive(
            upgrade,
            "`op` is not bound by `from alembic import op`",
        ));
    }
    if let Some(node) = op_replaced(&cx, root) {
        return Err(inconclusive(
            node,
            "`op` is rebound or its members replaced",
        ));
    }
    let mut calls = Vec::new();
    collect_calls(body, &mut calls);
    for call in calls {
        let Some(function) = call.child_by_field_name("function") else {
            continue;
        };
        let args: Vec<TsNode<'_>> = super::named_arguments(call)
            .into_iter()
            .filter(|n| n.kind() != "comment")
            .collect();
        let positional: Vec<TsNode<'_>> = args
            .iter()
            .copied()
            .filter(|a| a.kind() != "keyword_argument")
            .collect();
        let keyword = |key: &str| {
            args.iter().copied().find_map(|a| {
                (a.kind() == "keyword_argument"
                    && a.child_by_field_name("name")
                        .is_some_and(|n| cx.text(&n) == key))
                .then(|| a.child_by_field_name("value"))
                .flatten()
            })
        };
        let attribute = (function.kind() == "attribute")
            .then(|| {
                function
                    .child_by_field_name("attribute")
                    .map(|a| cx.text(&a))
            })
            .flatten();
        let is_op = function.kind() == "attribute"
            && function
                .child_by_field_name("object")
                .is_some_and(|o| o.kind() == "identifier" && cx.text(&o) == "op");
        // Raw SQL through `op.execute`, or any `.execute(...)` or
        // `.exec_driver_sql(...)` on a bind or session.
        if matches!(attribute, Some("execute" | "exec_driver_sql")) {
            let sql = positional
                .first()
                .and_then(|arg| sql_literal(&cx, *arg))
                .ok_or_else(|| inconclusive(call, "raw SQL is not a literal"))?;
            let changes = sql_effect(&sql)
                .ok_or_else(|| inconclusive(call, "raw SQL may change any table"))?;
            let evidence = cx.evidence(&call);
            // SQL run on some paths only leaves its tables unknown.
            if conditional(&cx, call, body) {
                for change in changes {
                    let (SqlChange::AddColumn(table, _)
                    | SqlChange::DropColumn(table, _)
                    | SqlChange::Reshape(table)) = change;
                    schema.forget(
                        &table,
                        "a migration op on it runs conditionally or in batch mode",
                        &evidence,
                    );
                }
                continue;
            }
            for change in changes {
                match change {
                    SqlChange::AddColumn(table, column) => match schema.tables.get_mut(&table) {
                        Some(state) => {
                            state.columns.insert(column, evidence.clone());
                            state.evidence = evidence.clone();
                        }
                        None => schema.forget(
                            &table,
                            "a column is added to a table the replay lacks",
                            &evidence,
                        ),
                    },
                    SqlChange::DropColumn(table, column) => {
                        let dropped = schema
                            .tables
                            .get_mut(&table)
                            .filter(|state| state.columns.contains_key(&column));
                        match dropped {
                            Some(state) => {
                                state.columns.remove(&column);
                                state.evidence = evidence.clone();
                            }
                            None => schema.forget(
                                &table,
                                "a dropped column is not in the replay",
                                &evidence,
                            ),
                        }
                    }
                    SqlChange::Reshape(table) => {
                        schema.forget(&table, "raw SQL reshapes it", &evidence)
                    }
                }
            }
            continue;
        }
        if !is_op {
            continue;
        }
        let name = attribute.unwrap_or_default();
        if SHAPELESS_OPS.contains(&name) {
            continue;
        }
        let first = positional.first().and_then(|a| literal_string(&cx, *a));
        let Some(table) = first.clone() else {
            return Err(inconclusive(call, "migration op names no literal table"));
        };
        // A `schema=` names `schema.table`, as the models do.
        let qualify = match keyword("schema") {
            None => None,
            Some(value) => Some(
                literal_string(&cx, value)
                    .ok_or_else(|| inconclusive(call, "migration op names no literal schema"))?,
            ),
        };
        let qualified = |name: String| match &qualify {
            Some(schema) => format!("{schema}.{name}"),
            None => name,
        };
        let table = qualified(table);
        if conditional(&cx, call, body) || name == "batch_alter_table" {
            schema.forget(
                &table,
                "a migration op on it runs conditionally or in batch mode",
                &cx.evidence(&call),
            );
            continue;
        }
        let evidence = cx.evidence(&call);
        match name {
            "create_table" => {
                if schema.tables.contains_key(&table) {
                    schema.forget(&table, "created while it already exists", &evidence);
                    continue;
                }
                let mut columns = BTreeMap::new();
                let mut known = true;
                for arg in &positional[1..] {
                    match column_name(&cx, *arg) {
                        ColumnArg::Named(column) => {
                            columns.insert(column, cx.evidence(arg));
                        }
                        ColumnArg::Other => {}
                        ColumnArg::Unknown => known = false,
                    }
                }
                if known {
                    schema.unknown.remove(&table);
                    schema.tables.insert(table, Table { columns, evidence });
                } else {
                    schema.forget(
                        &table,
                        "a create_table argument is not a literal Column",
                        &evidence,
                    );
                }
            }
            "drop_table" => {
                schema.tables.remove(&table);
                schema.unknown.remove(&table);
            }
            "rename_table" => {
                let new = positional
                    .get(1)
                    .and_then(|a| literal_string(&cx, *a))
                    .map(qualified);
                match (new, schema.tables.remove(&table)) {
                    (Some(new), Some(mut state)) if !schema.unknown.contains_key(&table) => {
                        state.evidence = evidence;
                        schema.tables.insert(new, state);
                    }
                    (Some(new), _) => {
                        schema.forget(&table, "renamed while its shape is unknown", &evidence);
                        schema.forget(
                            &new,
                            "renamed from a table whose shape is unknown",
                            &evidence,
                        );
                    }
                    (None, _) => return Err(inconclusive(call, "rename names no literal table")),
                }
            }
            "add_column" => match positional.get(1).map(|a| column_name(&cx, *a)) {
                Some(ColumnArg::Named(column)) => {
                    if let Some(state) = schema.tables.get_mut(&table) {
                        state.columns.insert(column, evidence.clone());
                        state.evidence = evidence;
                    } else {
                        schema.forget(
                            &table,
                            "a column is added to a table the replay lacks",
                            &evidence,
                        );
                    }
                }
                _ => schema.forget(&table, "an added column is not a literal Column", &evidence),
            },
            "drop_column" => match positional.get(1).and_then(|a| literal_string(&cx, *a)) {
                Some(column) => {
                    let dropped = schema
                        .tables
                        .get_mut(&table)
                        .filter(|state| state.columns.contains_key(&column));
                    match dropped {
                        Some(state) => {
                            state.columns.remove(&column);
                            state.evidence = evidence;
                        }
                        None => schema.forget(
                            &table,
                            "a dropped column is not in the replay",
                            &evidence,
                        ),
                    }
                }
                None => schema.forget(&table, "a dropped column is not a literal", &evidence),
            },
            "alter_column" => {
                let Some(rename) = keyword("new_column_name") else {
                    continue;
                };
                let column = positional.get(1).and_then(|a| literal_string(&cx, *a));
                match (
                    column,
                    literal_string(&cx, rename),
                    schema.tables.get_mut(&table),
                ) {
                    (Some(old), Some(new), Some(state)) if state.columns.contains_key(&old) => {
                        state.columns.remove(&old);
                        state.columns.insert(new, evidence.clone());
                        state.evidence = evidence;
                    }
                    _ => schema.forget(&table, "a column rename is not replayable", &evidence),
                }
            }
            _ => schema.forget(&table, &format!("op.{name} is not replayed"), &evidence),
        }
    }
    Ok(())
}

/// Every call under `node`, in source order.
fn collect_calls<'t>(node: TsNode<'t>, out: &mut Vec<TsNode<'t>>) {
    if node.kind() == "call" {
        out.push(node);
    }
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        collect_calls(child, out);
    }
}

/// Where the file may make `op` other than Alembic's: any `op` that is not a
/// direct call of one of its members or the autocommit wrapper (a bare read,
/// a binding, a member read into a value, a computed or chained member, a
/// member write or deletion), or a wildcard import that may rebind it.
fn op_replaced<'t>(cx: &FileCx<'_>, root: TsNode<'t>) -> Option<TsNode<'t>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if node.kind() == "wildcard_import" {
            return Some(node);
        }
        if node.kind() != "identifier" || cx.text(&node) != "op" {
            continue;
        }
        let Some(parent) = node.parent() else {
            continue;
        };
        let is_field = |parent: TsNode<'_>, field: &str| {
            parent
                .child_by_field_name(field)
                .is_some_and(|child| child.id() == node.id())
        };
        // Another object's member (`column.op(...)`), a keyword's name, or
        // `from alembic import op` itself.
        if (parent.kind() == "attribute" && is_field(parent, "attribute"))
            || (parent.kind() == "keyword_argument" && is_field(parent, "name"))
        {
            continue;
        }
        if parent.kind() == "dotted_name"
            && parent.parent().is_some_and(|import| {
                import.kind() == "import_from_statement"
                    && cx.text(&import).split_whitespace().collect::<Vec<_>>()
                        == ["from", "alembic", "import", "op"]
            })
        {
            continue;
        }
        if parent.kind() != "attribute" || !is_field(parent, "object") {
            return Some(node);
        }
        // Only the grammar replay reads: a direct call `op.add_column(...)`,
        // or the whole `op.get_context().autocommit_block()` wrapper. A
        // member read into a value, a computed or chained member, a write
        // or a deletion may hide or replace an op.
        fn called(callee: TsNode<'_>) -> Option<TsNode<'_>> {
            callee.parent().filter(move |up| {
                up.kind() == "call"
                    && up
                        .child_by_field_name("function")
                        .is_some_and(|function| function.id() == callee.id())
            })
        }
        let member = parent.child_by_field_name("attribute").map(|a| cx.text(&a));
        let admitted = match called(parent) {
            Some(call) if member == Some("get_context") => call
                .parent()
                .filter(|up| {
                    up.kind() == "attribute"
                        && up
                            .child_by_field_name("object")
                            .is_some_and(|object| object.id() == call.id())
                })
                .and_then(called)
                .is_some_and(|wrapper| cx.text(&wrapper) == "op.get_context().autocommit_block()"),
            // Its result is not chained: `op.get_bind().exec_driver_sql(...)`
            // would run SQL replay never reads.
            Some(call) => !call.parent().is_some_and(|up| {
                ["attribute", "subscript", "call"].contains(&up.kind())
                    && ["object", "value", "function"].iter().any(|field| {
                        up.child_by_field_name(field)
                            .is_some_and(|child| child.id() == call.id())
                    })
            }),
            None => false,
        };
        if !admitted {
            return Some(node);
        }
    }
    None
}

fn mentions_op(cx: &FileCx<'_>, node: TsNode<'_>) -> bool {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" && cx.text(&node) == "op" {
            return true;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    false
}

/// True when `call` runs only on some paths through `body`.
fn conditional(cx: &FileCx<'_>, call: TsNode<'_>, body: TsNode<'_>) -> bool {
    let mut node = call.parent();
    while let Some(current) = node {
        if current.id() == body.id() {
            return false;
        }
        // Alembic's own transaction wrapper always runs its body.
        if current.kind() == "with_statement" && autocommit_block(cx, current) {
            node = current.parent();
            continue;
        }
        if matches!(
            current.kind(),
            "if_statement"
                | "match_statement"
                | "case_clause"
                | "for_statement"
                | "while_statement"
                | "try_statement"
                | "with_statement"
                | "conditional_expression"
                | "boolean_operator"
                | "lambda"
                | "function_definition"
                | "list_comprehension"
                | "set_comprehension"
                | "dictionary_comprehension"
                | "generator_expression"
        ) {
            return true;
        }
        node = current.parent();
    }
    true
}

/// `with op.get_context().autocommit_block():`, its only item.
fn autocommit_block(cx: &FileCx<'_>, with: TsNode<'_>) -> bool {
    let mut walk = with.walk();
    let Some(clause) = with
        .named_children(&mut walk)
        .find(|child| child.kind() == "with_clause")
    else {
        return false;
    };
    let mut walk = clause.walk();
    let items: Vec<_> = clause.named_children(&mut walk).collect();
    let text = |node: TsNode<'_>| cx.text(&node).split_whitespace().collect::<String>();
    matches!(items.as_slice(), [item] if text(*item) == "op.get_context().autocommit_block()")
}

enum ColumnArg {
    Named(String),
    /// A constraint or index: no column.
    Other,
    Unknown,
}

/// A `create_table` / `add_column` argument: `sa.Column("name", ...)`.
fn column_name(cx: &FileCx<'_>, arg: TsNode<'_>) -> ColumnArg {
    if arg.kind() != "call" {
        return ColumnArg::Unknown;
    }
    let Some(function) = arg.child_by_field_name("function") else {
        return ColumnArg::Unknown;
    };
    let callee = match function.kind() {
        "identifier" => cx.text(&function),
        "attribute" => function
            .child_by_field_name("attribute")
            .map(|a| cx.text(&a))
            .unwrap_or_default(),
        _ => return ColumnArg::Unknown,
    };
    if callee.ends_with("Constraint") || callee == "Index" {
        return ColumnArg::Other;
    }
    if callee != "Column" {
        return ColumnArg::Unknown;
    }
    super::named_arguments(arg)
        .into_iter()
        .find(|a| !matches!(a.kind(), "comment" | "keyword_argument"))
        .and_then(|a| literal_string(cx, a))
        .map_or(ColumnArg::Unknown, ColumnArg::Named)
}

/// A SQL literal: `"..."`, or `sa.text("...")` / `text("...")`.
fn sql_literal(cx: &FileCx<'_>, arg: TsNode<'_>) -> Option<String> {
    if let Some(sql) = exact_string(cx, arg) {
        return Some(sql);
    }
    let function = arg
        .child_by_field_name("function")
        .filter(|_| arg.kind() == "call")?;
    let name = match function.kind() {
        "identifier" => cx.text(&function),
        "attribute" => cx.text(&function.child_by_field_name("attribute")?),
        _ => return None,
    };
    if name != "text" {
        return None;
    }
    let args = super::named_arguments(arg);
    match args.as_slice() {
        [only] => exact_string(cx, *only),
        _ => None,
    }
}

/// A plain string literal's exact text: its own delimiters removed, quotes
/// inside kept. Escapes, byte strings and f-strings are not read.
pub(crate) fn exact_string(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let text = cx.text(&node).trim();
    let quote_at = text.find(['\'', '"'])?;
    let prefix = &text[..quote_at];
    if prefix.chars().any(|p| matches!(p, 'f' | 'F' | 'b' | 'B')) {
        return None;
    }
    let body = &text[quote_at..];
    let delimiter = [&body[..1], &body[..1], &body[..1]].concat();
    let delimiter = if body.starts_with(&delimiter) {
        delimiter
    } else {
        body[..1].to_string()
    };
    let inner = body
        .strip_prefix(&delimiter)?
        .strip_suffix(&delimiter)
        .filter(|_| body.len() >= 2 * delimiter.len())?;
    let raw = prefix.chars().any(|p| matches!(p, 'r' | 'R'));
    (raw || !inner.contains('\\')).then(|| inner.to_string())
}

/// One shape change a literal SQL statement makes.
enum SqlChange {
    AddColumn(String, String),
    DropColumn(String, String),
    /// Some other change to this table's columns.
    Reshape(String),
}

/// A PostgreSQL identifier as stored: quoted names keep their case.
fn identifier(token: &str) -> String {
    match token.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        Some(quoted) => quoted.to_string(),
        None => token.to_ascii_lowercase(),
    }
}

/// The shape changes a literal SQL string makes, or `None` when it may
/// change any table. Data statements, extensions, indexes, types,
/// sequences, constraints and a column's type, default or nullability
/// change no shape; `ALTER TABLE t ADD|DROP [COLUMN] c` is read exactly.
fn sql_effect(sql: &str) -> Option<Vec<SqlChange>> {
    let mut changes = Vec::new();
    for statement in statements(sql)? {
        let tokens: Vec<&str> = statement.split_whitespace().collect();
        let words: Vec<String> = tokens.iter().map(|w| w.to_ascii_uppercase()).collect();
        let word = |i: usize| words.get(i).map(String::as_str);
        match (word(0), word(1)) {
            (None, _) => {}
            (Some("UPDATE" | "INSERT" | "DELETE" | "SELECT" | "ROLLBACK" | "COMMIT"), _) => {}
            (Some("CREATE" | "DROP"), Some("EXTENSION" | "INDEX" | "TYPE" | "SEQUENCE")) => {}
            (Some("CREATE"), Some("UNIQUE")) if word(2) == Some("INDEX") => {}
            (Some("ALTER"), Some("TYPE" | "SEQUENCE" | "INDEX")) => {}
            (Some("ALTER"), Some("TABLE")) => {
                let table = match word(2) {
                    None | Some("ONLY" | "IF") => return None,
                    Some(_) if tokens[2].contains('.') => return None,
                    Some(_) => identifier(tokens[2]),
                };
                // Several actions, separated outside parentheses, are not
                // read one by one.
                let mut depth = 0i32;
                let several = statement.chars().any(|ch| {
                    match ch {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    ch == ',' && depth == 0
                });
                let mut at = 4;
                let skip = |at: &mut usize, expected: &[&str]| {
                    if expected
                        .iter()
                        .enumerate()
                        .all(|(k, w)| word(*at + k) == Some(*w))
                    {
                        *at += expected.len();
                    }
                };
                match word(3) {
                    Some("ALTER") => {}
                    Some("ADD" | "DROP")
                        if matches!(
                            word(4),
                            Some(
                                "CONSTRAINT"
                                    | "PRIMARY"
                                    | "UNIQUE"
                                    | "FOREIGN"
                                    | "CHECK"
                                    | "EXCLUDE"
                            )
                        ) => {}
                    Some(action @ ("ADD" | "DROP")) if !several => {
                        skip(&mut at, &["COLUMN"]);
                        skip(&mut at, &["IF", "NOT", "EXISTS"]);
                        skip(&mut at, &["IF", "EXISTS"]);
                        let column = tokens.get(at).map(|t| identifier(t))?;
                        changes.push(if action == "ADD" {
                            SqlChange::AddColumn(table, column)
                        } else {
                            SqlChange::DropColumn(table, column)
                        });
                    }
                    _ => changes.push(SqlChange::Reshape(table)),
                }
            }
            _ => return None,
        }
    }
    Some(changes)
}

/// `sql` split at semicolons outside quotes; `None` when its boundaries
/// cannot be read (an unclosed quote, a comment or dollar quoting).
fn statements(sql: &str) -> Option<Vec<&str>> {
    if sql.contains("--") || sql.contains("/*") || sql.contains('$') {
        return None;
    }
    let (mut out, mut start, mut quote) = (Vec::new(), 0, None);
    for (index, ch) in sql.char_indices() {
        match (quote, ch) {
            (None, '\'' | '"') => quote = Some(ch),
            (Some(open), _) if ch == open => quote = None,
            (None, ';') => {
                out.push(&sql[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() {
        return None;
    }
    out.push(&sql[start..]);
    Some(out)
}

/// One modeled table: every `DataEntity` mapping it, merged.
struct Modeled<'a> {
    entity: &'a Node,
    /// Database column name and the span declaring it.
    columns: BTreeMap<String, EvidenceRef>,
    /// Some column's database name is not a literal.
    opaque: bool,
}

fn compare(root: &Path, id: &SourceId<'_>, history: &History, out: &mut Extraction) {
    let (head, schema) = (history.head.as_str(), &history.schema);
    let mut modeled: BTreeMap<String, Modeled<'_>> = BTreeMap::new();
    let entities: Vec<Node> = out
        .nodes
        .iter()
        .filter(|n| n.label == "DataEntity" && n.props["storage"] == "sqlalchemy")
        .cloned()
        .collect();
    for entity in &entities {
        let Some(table) = entity.props["table"].as_str() else {
            continue;
        };
        let table = match entity.props["schema"].as_str() {
            Some(schema) => format!("{schema}.{table}"),
            None => table.to_string(),
        };
        let evidence: Vec<EvidenceRef> =
            serde_json::from_value(entity.props["prov"]["evidence"].clone()).unwrap_or_default();
        let entry = modeled.entry(table).or_insert_with(|| Modeled {
            entity,
            columns: BTreeMap::new(),
            opaque: false,
        });
        let columns = entity.props["columns"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for (index, column) in columns.iter().enumerate() {
            let name = match column.get("column") {
                Some(serde_json::Value::String(name)) => name.as_str(),
                Some(_) => {
                    entry.opaque = true;
                    continue;
                }
                None => column["name"].as_str().unwrap_or_default(),
            };
            // The class span comes first, then one span per column.
            if let Some(span) = evidence.get(index + 1).or(evidence.first()) {
                entry
                    .columns
                    .entry(name.to_string())
                    .or_insert_with(|| span.clone());
            }
        }
    }
    if modeled.is_empty() {
        return;
    }
    let model_files: BTreeSet<String> = modeled
        .values()
        .flat_map(|m| m.columns.values().map(|e| e.path.clone()))
        .collect();
    let model_text: String = model_files
        .iter()
        .filter_map(|path| std::fs::read_to_string(root.join(path)).ok())
        .collect();
    let mut findings = Vec::new();
    let mut gaps = Vec::new();
    for (table, model) in &modeled {
        if let Some((reason, evidence)) = schema.unknown.get(table) {
            gaps.push((table.clone(), reason.clone(), evidence.clone()));
            continue;
        }
        let class = serde_json::from_value::<Vec<EvidenceRef>>(
            model.entity.props["prov"]["evidence"].clone(),
        )
        .ok()
        .and_then(|e| e.into_iter().next());
        let Some(state) = schema.tables.get(table) else {
            findings.push(finding(
                id,
                "modeled-not-migrated",
                table,
                Some(model.entity),
                class,
                None,
                &format!("absent at head {head}"),
            ));
            continue;
        };
        for (column, span) in &model.columns {
            if !state.columns.contains_key(column) {
                findings.push(finding(
                    id,
                    "modeled-not-migrated",
                    &format!("{table}.{column}"),
                    Some(model.entity),
                    Some(span.clone()),
                    Some(state.evidence.clone()),
                    &format!("absent at head {head}"),
                ));
            }
        }
        if model.opaque {
            continue;
        }
        for (column, span) in &state.columns {
            if !model.columns.contains_key(column) {
                findings.push(finding(
                    id,
                    "migrated-not-modeled",
                    &format!("{table}.{column}"),
                    Some(model.entity),
                    class.clone(),
                    Some(span.clone()),
                    "no model declares it",
                ));
            }
        }
    }
    // A migration-only table the replay lost is a Gap too.
    for (table, (reason, evidence)) in &schema.unknown {
        if !modeled.contains_key(table) {
            gaps.push((table.clone(), reason.clone(), evidence.clone()));
        }
    }
    for (table, state) in &schema.tables {
        // A name a model file spells may be a `Table(...)` or a computed
        // `__tablename__`: not reported.
        let bare = table.rsplit('.').next().unwrap_or(table);
        let spelled = [format!("\"{bare}\""), format!("'{bare}'")]
            .iter()
            .any(|quoted| model_text.contains(quoted.as_str()));
        if !modeled.contains_key(table) && !spelled {
            findings.push(finding(
                id,
                "migrated-not-modeled",
                table,
                None,
                None,
                Some(state.evidence.clone()),
                "no model maps it",
            ));
        }
    }
    // What was compared, so an empty register reads as checked.
    let compared: Vec<&Modeled<'_>> = modeled
        .iter()
        .filter(|(table, _)| schema.tables.contains_key(*table))
        .map(|(_, model)| model)
        .collect();
    let history_id = format!("migrations:{}@{}", id.repo, history.location);
    let cite = |path: &str| EvidenceRef {
        repo: id.repo.into(),
        path: path.into(),
        byte_start: 0,
        byte_end: 0,
        commit_sha: id.commit.into(),
    };
    out.nodes.push(Node {
        id: history_id.clone(),
        label: "MigrationHistory".into(),
        props: serde_json::json!({
            "tool": "alembic",
            "location": history.location,
            "head": head,
            "revisions": history.revisions,
            "tables": schema.tables.len(),
            "modeled_tables_compared": compared.len(),
            "modeled_columns_compared": compared.iter().map(|m| m.columns.len()).sum::<usize>(),
            "prov": prov_from(
                vec![cite("alembic.ini"), cite(&history.head_path)],
                ConfidenceTier::Confirmed,
                &format!("MigrationHistory {history_id}"),
            ),
        }),
    });
    for (node, edge) in findings {
        out.nodes.push(node);
        out.edges.extend(edge);
    }
    // A modeled table the replay lost track of is named, not silently
    // passed.
    for (table, reason, evidence) in gaps {
        let gap_id = format!("gap:alembic-table:{}@{table}", id.repo);
        out.nodes.push(Node {
            id: gap_id.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "kind": "migration-gap",
                "table": table,
                "reason": format!("schema drift not checked for `{table}`: {reason}"),
                "attempted_tiers": ["T0"],
                "prov": prov_from(vec![evidence], ConfidenceTier::Gap, &format!("Gap {gap_id}")),
            }),
        });
    }
}

fn finding(
    id: &SourceId<'_>,
    drift_kind: &str,
    key: &str,
    entity: Option<&Node>,
    modeled: Option<EvidenceRef>,
    migrated: Option<EvidenceRef>,
    note: &str,
) -> (Node, Option<Edge>) {
    let drift_id = format!("drift:schema:{}@{key}:{drift_kind}", id.repo);
    let side = |evidence: &Option<EvidenceRef>| {
        evidence
            .as_ref()
            .map(|e| format!("{} bytes {}..{}", e.path, e.byte_start, e.byte_end))
    };
    let (modeled_cell, migrated_cell) = match drift_kind {
        "modeled-not-migrated" => (
            side(&modeled).unwrap_or_else(|| "—".into()),
            note.to_string(),
        ),
        _ => (
            note.to_string(),
            side(&migrated).unwrap_or_else(|| "—".into()),
        ),
    };
    let evidence: Vec<EvidenceRef> = modeled.into_iter().chain(migrated).collect();
    let node = Node {
        id: drift_id.clone(),
        label: "Drift".into(),
        props: serde_json::json!({
            "kind": "schema-drift",
            "drift_kind": drift_kind,
            "key": key,
            "title": format!("{drift_kind}: {key}"),
            "modeled": modeled_cell,
            "migrated": migrated_cell,
            "prov": prov_from(
                evidence.clone(),
                ConfidenceTier::InferredStrong,
                &format!("Drift {drift_id}"),
            ),
        }),
    };
    let edge = entity.map(|entity| Edge {
        src: drift_id.clone(),
        dst: entity.id.clone(),
        label: "MODELED_AS".into(),
        props: serde_json::json!({
            "prov": prov_from(
                evidence,
                ConfidenceTier::InferredStrong,
                &format!("{drift_id} MODELED_AS {}", entity.id),
            ),
        }),
    });
    (node, edge)
}

fn push_gap(id: &SourceId<'_>, inconclusive: Inconclusive, out: &mut Extraction) {
    let evidence = inconclusive.evidence;
    let gap_id = format!(
        "gap:alembic:{}@{}@{}",
        id.repo, evidence.path, evidence.byte_start
    );
    out.nodes.push(Node {
        id: gap_id.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "kind": "migration-gap",
            "reason": format!("Alembic history not replayed: {}", inconclusive.reason),
            "attempted_tiers": ["T0"],
            "prov": prov_from(vec![evidence], ConfidenceTier::Gap, &format!("Gap {gap_id}")),
        }),
    });
}

#[cfg(test)]
#[path = "alembic/tests.rs"]
mod tests;
