//! Python deterministic (T0) language adapter (SPEC-00 §3.3, M10).
//!
//! Import-proven FastAPI and Flask decorators become Endpoint/HANDLES facts;
//! functions, imports, and direct calls become the server graph. Directory
//! joins prove local imported call targets after every incremental pass. This
//! tier never calls an LLM and every emitted fact carries exact source-span
//! provenance.

use core_graph::placeholder::Boundary;
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, QueryCursor};

mod alembic;
mod app_scope;
mod auth_guards;
mod decorator_proof;
mod flask_admin;
mod flask_admin_access;
mod flask_admin_library;
mod flask_pages;
pub mod observed_infra;
mod queries;
mod rate_limits;
mod request_inputs;
mod responses;
mod rule_evidence;
mod sqlalchemy;
mod sqlalchemy_writes;
mod target_tests;

const EXTRACTOR_ID: &str = "t0.adapter-python";

/// Python extraction errors.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// tree-sitter grammar/version mismatch.
    #[error("language: {0}")]
    Language(#[from] tree_sitter::LanguageError),
    /// The parser returned no tree.
    #[error("parse produced no tree for {0}")]
    NoTree(String),
    /// Filesystem failure while walking a directory.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Bounds on the auxiliary inputs directory resolution reads (#1653,
/// AC-1805): Alembic revision files and their size, and lockfile size.
/// [`Default`] is the long-standing 4,096 revisions of up to 1 MiB and 8 MiB
/// lockfiles; a project may choose other bounds up to the hard ceilings.
/// Past a bound the result is the same explicit gap as before, never a
/// failed extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythonLimits {
    max_alembic_revisions: usize,
    max_alembic_file_bytes: u64,
    max_lockfile_bytes: u64,
}

impl PythonLimits {
    /// Most Alembic revision files a project may allow. Every revision's
    /// source and syntax tree are held for the replay.
    pub const HARD_MAX_ALEMBIC_REVISIONS: usize = 16_384;
    /// Largest `alembic.ini` or revision file a project may allow (4 MiB).
    pub const HARD_MAX_ALEMBIC_FILE_BYTES: u64 = 4 << 20;
    /// Largest lockfile a project may allow (64 MiB).
    pub const HARD_MAX_LOCKFILE_BYTES: u64 = 64 << 20;

    /// These bounds, or `None` when one is zero or above its hard ceiling.
    pub fn within(
        max_alembic_revisions: usize,
        max_alembic_file_bytes: u64,
        max_lockfile_bytes: u64,
    ) -> Option<Self> {
        ((1..=Self::HARD_MAX_ALEMBIC_REVISIONS).contains(&max_alembic_revisions)
            && (1..=Self::HARD_MAX_ALEMBIC_FILE_BYTES).contains(&max_alembic_file_bytes)
            && (1..=Self::HARD_MAX_LOCKFILE_BYTES).contains(&max_lockfile_bytes))
        .then_some(Self {
            max_alembic_revisions,
            max_alembic_file_bytes,
            max_lockfile_bytes,
        })
    }
}

impl Default for PythonLimits {
    fn default() -> Self {
        Self {
            max_alembic_revisions: alembic::MAX_REVISIONS,
            max_alembic_file_bytes: alembic::MAX_FILE_BYTES,
            max_lockfile_bytes: flask_admin_library::MAX_LOCK_BYTES,
        }
    }
}

/// Identity of the source being recovered.
pub struct SourceId<'a> {
    /// Repository identity.
    pub repo: &'a str,
    /// Commit SHA, or `workdir` for an unversioned tree.
    pub commit: &'a str,
}

/// Python graph facts from one file or directory.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
    pending_calls: Vec<PendingCall>,
    receiver_defs: Vec<ReceiverDef>,
    pending_routes: Vec<PendingRoute>,
    mounts: Vec<Mount>,
    orm_files: Vec<sqlalchemy::OrmFile>,
    function_facts: Vec<responses::FunctionFacts>,
    handler_facts: Vec<responses::HandlerFacts>,
    auth_handlers: Vec<auth_guards::HandlerAuthFacts>,
    fn_effects: Vec<auth_guards::FnEffects>,
    hooks: Vec<auth_guards::HookFact>,
    wsgi_facts: Vec<auth_guards::WsgiFact>,
    patched_guards: Vec<String>,
    client_fixtures: Vec<target_tests::ClientFixture>,
    client_calls: Vec<target_tests::ClientCall>,
    app_calls: Vec<app_scope::AppCall>,
    name_uses: BTreeSet<String>,
    /// What each Flask route handler's receiver may be (#1149).
    endpoint_apps: Vec<(String, app_scope::AppRef)>,
    /// The parent of a `register_blueprint` whose argument is not a name.
    wild_mounts: Vec<app_scope::AppRef>,
    app_defs: HashSet<(String, String)>,
    /// Names assigned as attributes of an imported module (`m.app = …`).
    module_attr_writes: BTreeSet<String>,
    /// Module-level flask-limiter `Limiter` instances (#1157).
    limiter_defs: Vec<(String, String)>,
    /// The limiters among them constructed with an `on_breach` callback,
    /// whose response replaces flask-limiter's 429 (#2539).
    limiter_breach: Vec<(String, String)>,
    /// Limit decorators on route handlers and module limit constants (#1998).
    pending_limits: Vec<rate_limits::PendingLimit>,
    limit_constants: Vec<rate_limits::LimitConstant>,
    /// Names an attribute write or `setattr` / `delattr` goes through.
    attribute_write_objects: BTreeSet<String>,
    /// Attribute names written anywhere, through any object or a literal
    /// `setattr` / `delattr` name (#1173).
    written_attributes: BTreeSet<String>,
    /// Names read as an attribute (`m.name`) or imported by name
    /// (`from m import name`) anywhere (#1173).
    member_references: BTreeSet<String>,
    /// Pytest modules' `(stem, written_attributes, member_references)`,
    /// counted only once a counted file imports the module (#1173).
    pytest_modules: Vec<(String, BTreeSet<String>, BTreeSet<String>)>,
    /// `(path, name)` for each name a package `__init__.py` binds at module
    /// scope; `*` for a star import (#1173).
    package_names: BTreeSet<(String, String)>,
    /// Flask-Admin views, classes and registrations (#1193).
    flask_admin: flask_admin::Facts,
    /// Literal `render_template` / `redirect(url_for(…))` calls (AC-0854).
    page_facts: Vec<flask_pages::PageFact>,
}

/// A module-level framework receiver: an application instance, or a Flask
/// `Blueprint` / FastAPI `APIRouter` whose routes are joined across files
/// after every file is parsed.
#[derive(Debug, Clone)]
struct ReceiverDef {
    key: (String, String),
    framework: Framework,
    router: bool,
    /// Assigned at module scope, so a module-level or imported name proves it;
    /// a function-local assignment proves only reads in its own function.
    module_level: bool,
    /// This assignment is the module's only binding of the name (#1149).
    sole: bool,
    prefix: Prefix,
    /// A Flask Blueprint's name, which prefixes its endpoint names.
    name: Prefix,
    evidence: EvidenceRef,
}

/// A router prefix as written: absent, a string literal, or something this
/// tier cannot evaluate (which becomes a Gap, never a guess).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Prefix {
    Absent,
    Literal(String),
    Unknown,
}

/// A route decorator whose receiver is defined in this file as a router or
/// imported from another local module; resolved once every file is known.
#[derive(Debug, Clone)]
struct PendingRoute {
    receiver: (String, String),
    verb: String,
    path: String,
    listed_methods: Vec<String>,
    handler: String,
    /// The route's own endpoint name (`endpoint=`, else the view function's
    /// name), when it is a literal.
    endpoint: Option<String>,
    evidence: EvidenceRef,
    /// Request inputs the handler body reads (Flask), as endpoint props.
    inputs: Option<serde_json::Value>,
    /// Responses the handler body returns (Flask), as endpoint props.
    responses: Option<serde_json::Value>,
}

/// `parent.register_blueprint(bp, url_prefix=...)` /
/// `parent.include_router(r, prefix=...)`.
#[derive(Debug, Clone)]
struct Mount {
    receiver: (String, String),
    parent: MountParent,
    framework: Framework,
    prefix: Prefix,
    /// `register_blueprint(bp, name=…)`, which replaces the Blueprint's name.
    name: Prefix,
    /// What the mount's parent may be, for scoping request hooks.
    app: app_scope::AppRef,
    evidence: EvidenceRef,
}

/// The object a mount call is made on (`parent` above).
#[derive(Debug, Clone, PartialEq, Eq)]
enum MountParent {
    /// A module-level or imported name, proven at the join against a
    /// module-level `ReceiverDef` (an application, or a router mounted in turn).
    Def((String, String)),
    /// An application assigned from an import-proven factory in the enclosing
    /// function.
    LocalApp(Framework),
    /// A parameter annotated with an import-proven application type: declared,
    /// not proven, so routes under it stay InferredStrong.
    AnnotatedApp(Framework),
    /// An unannotated parameter, an attribute or any other expression.
    Unproven,
}

#[derive(Debug, Clone)]
struct PendingCall {
    resolved: Edge,
    gap: Option<(Node, Edge)>,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file Python parse cache (AC-0053).
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Physical Python source work performed by an incremental extraction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncrementalStats {
    /// New or byte-changed files parsed.
    pub recomputed_files: u64,
    /// Content-identical files reused.
    pub reused_files: u64,
    /// Cached files removed because the source disappeared.
    pub deleted_files: u64,
}

struct FileCx<'a> {
    source: &'a [u8],
    path: &'a str,
    id: &'a SourceId<'a>,
}

impl FileCx<'_> {
    fn text(&self, node: &TsNode<'_>) -> &str {
        node.utf8_text(self.source).unwrap_or("")
    }

    fn evidence(&self, node: &TsNode<'_>) -> EvidenceRef {
        EvidenceRef {
            repo: self.id.repo.into(),
            path: self.path.into(),
            byte_start: node.start_byte() as u64,
            byte_end: node.end_byte() as u64,
            commit_sha: self.id.commit.into(),
        }
    }

    fn prov(&self, node: &TsNode<'_>, fact: &str) -> serde_json::Value {
        self.prov_with_confidence(node, ConfidenceTier::Confirmed, fact)
    }

    fn prov_with_confidence(
        &self,
        node: &TsNode<'_>,
        confidence: ConfidenceTier,
        fact: &str,
    ) -> serde_json::Value {
        let provenance = Provenance::new(
            Tier::Deterministic,
            confidence,
            vec![EvidenceRef {
                repo: self.id.repo.into(),
                path: self.path.into(),
                byte_start: node.start_byte() as u64,
                byte_end: node.end_byte() as u64,
                commit_sha: self.id.commit.into(),
            }],
            EXTRACTOR_ID,
            fact.as_bytes(),
        )
        .expect("Deterministic confidence is within its ceiling");
        serde_json::to_value(provenance).expect("provenance serializes")
    }
}

fn prov_from(
    evidence: Vec<EvidenceRef>,
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let provenance = Provenance::new(
        Tier::Deterministic,
        confidence,
        evidence,
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic confidence is within its ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

fn file_id(repo: &str, path: &str) -> String {
    format!("file:{repo}@{path}")
}

pub(crate) fn symbol_id(repo: &str, path: &str, name: &str) -> String {
    format!("sym:{repo}@{path}#{name}")
}

fn retarget_props_commit(props: &mut serde_json::Value, commit: &str) {
    // Handler request inputs and responses carry their own provenance.
    for nested in NESTED_PROV {
        if let Some(value) = props.get_mut(nested)
            && value.is_object()
        {
            retarget_props_commit(value, commit);
        }
    }
    let Ok(mut provenance) =
        serde_json::from_value::<Provenance>(props.get("prov").cloned().unwrap_or_default())
    else {
        return;
    };
    for evidence in &mut provenance.evidence {
        evidence.commit_sha = commit.into();
    }
    props["prov"] = serde_json::to_value(provenance).expect("provenance serializes");
}

/// Endpoint props whose objects carry a nested `prov`.
const NESTED_PROV: &[&str] = &["request_inputs", "responses", "auth", "rate_limits"];

fn retarget_commit(extraction: &mut Extraction, commit: &str) {
    for node in &mut extraction.nodes {
        retarget_props_commit(&mut node.props, commit);
    }
    for edge in &mut extraction.edges {
        retarget_props_commit(&mut edge.props, commit);
    }
    for def in &mut extraction.receiver_defs {
        def.evidence.commit_sha = commit.into();
    }
    for route in &mut extraction.pending_routes {
        route.evidence.commit_sha = commit.into();
        for nested in [&mut route.inputs, &mut route.responses]
            .into_iter()
            .flatten()
        {
            retarget_props_commit(nested, commit);
        }
    }
    for mount in &mut extraction.mounts {
        mount.evidence.commit_sha = commit.into();
    }
    for pending in &mut extraction.pending_calls {
        retarget_props_commit(&mut pending.resolved.props, commit);
        if let Some((node, edge)) = &mut pending.gap {
            retarget_props_commit(&mut node.props, commit);
            retarget_props_commit(&mut edge.props, commit);
        }
    }
    sqlalchemy::retarget(extraction, commit);
    responses::retarget(extraction, commit);
    auth_guards::retarget(extraction, commit);
    rate_limits::retarget(extraction, commit);
    target_tests::retarget(extraction, commit);
    flask_admin::retarget(extraction, commit);
    flask_pages::retarget(extraction, commit);
    rule_evidence::retarget(extraction, commit);
}

fn normalize_path(path: PathBuf) -> Option<String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized.to_string_lossy().replace('\\', "/"))
}

fn module_file(path: &str, module: &str) -> Option<String> {
    let dots = module.chars().take_while(|ch| *ch == '.').count();
    let tail = module[dots..].trim();
    let mut base = if dots == 0 {
        PathBuf::new()
    } else {
        let mut parent = Path::new(path).parent()?.to_path_buf();
        for _ in 1..dots {
            parent.pop();
        }
        parent
    };
    if !tail.is_empty() {
        for part in tail.split('.') {
            base.push(part);
        }
    }
    if base.as_os_str().is_empty() {
        return None;
    }
    base.set_extension("py");
    normalize_path(base)
}

#[derive(Debug, Clone)]
enum ImportBinding {
    Module {
        module: String,
    },
    Symbol {
        module: String,
        exported: String,
        target_file: Option<String>,
        relative: bool,
    },
}

fn clean_import_piece(piece: &str) -> &str {
    piece.trim().trim_matches(|ch| ch == '(' || ch == ')')
}

fn parse_imports(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    out: &mut Extraction,
) -> HashMap<String, ImportBinding> {
    let query = crate::queries::query("[(import_statement) (import_from_statement)] @import");
    let mut bindings = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    while let Some(found) = matches.next() {
        let statement = found.captures()[0].node;
        let raw = cx.text(&statement).replace(['\n', '\\'], " ");
        let mut modules = BTreeSet::new();
        if let Some(rest) = raw.trim().strip_prefix("import ") {
            for piece in rest.split(',').map(clean_import_piece) {
                let mut alias = piece.split_whitespace();
                let Some(module) = alias.next() else { continue };
                let local = match (alias.next(), alias.next()) {
                    (Some("as"), Some(local)) => local,
                    _ => module.split('.').next().unwrap_or(module),
                };
                modules.insert(module.to_string());
                bindings.insert(
                    local.to_string(),
                    ImportBinding::Module {
                        module: module.to_string(),
                    },
                );
            }
        } else if let Some(rest) = raw.trim().strip_prefix("from ")
            && let Some((module, names)) = rest.split_once(" import ")
        {
            let module = module.trim();
            let relative = module.starts_with('.');
            modules.insert(module.to_string());
            for piece in names.split(',').map(clean_import_piece) {
                let mut alias = piece.split_whitespace();
                let Some(exported) = alias.next() else {
                    continue;
                };
                if exported == "*" {
                    continue;
                }
                let local = match (alias.next(), alias.next()) {
                    (Some("as"), Some(local)) => local,
                    _ => exported,
                };
                bindings.insert(
                    local.to_string(),
                    ImportBinding::Symbol {
                        module: module.to_string(),
                        exported: exported.to_string(),
                        target_file: module_file(cx.path, module),
                        relative,
                    },
                );
            }
        }
        for module in modules {
            let destination = if module.starts_with('.') {
                module_file(cx.path, &module)
                    .map(|path| file_id(cx.id.repo, &path))
                    .unwrap_or_else(|| format!("mod:{module}"))
            } else {
                format!("mod:{module}")
            };
            out.edges.push(Edge {
                src: file_id(cx.id.repo, cx.path),
                dst: destination,
                label: "IMPORTS".into(),
                props: serde_json::json!({
                    "specifier": module,
                    "prov": cx.prov(&statement, &format!("IMPORTS {}", cx.text(&statement))),
                }),
            });
        }
    }
    bindings
}

fn literal_string(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let text = cx.text(&node).trim();
    let quote_at = text.find(['\'', '"'])?;
    if text[..quote_at]
        .chars()
        .any(|prefix| matches!(prefix, 'f' | 'F'))
    {
        return None;
    }
    Some(text[quote_at..].trim_matches(['\'', '"']).to_string())
}

fn call_name(cx: &FileCx<'_>, call: TsNode<'_>) -> Option<String> {
    call.child_by_field_name("function")
        .map(|function| cx.text(&function).to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framework {
    FastApi,
    Flask,
}

/// An import-proven framework constructor: `(framework, is_router)`.
fn receiver_factory(
    callee: &str,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<(Framework, bool)> {
    if let Some(framework) = framework_factory(callee, bindings) {
        return Some((framework, false));
    }
    let mut parts = callee.split('.');
    let base = parts.next()?;
    let member = parts.next();
    let (module, exported) = match (bindings.get(base), member) {
        (
            Some(ImportBinding::Symbol {
                module, exported, ..
            }),
            None,
        ) => (module.as_str(), exported.as_str()),
        (Some(ImportBinding::Module { module }), Some(member)) => (module.as_str(), member),
        _ => return None,
    };
    match (module, exported) {
        ("flask", "Blueprint") => Some((Framework::Flask, true)),
        ("fastapi", "APIRouter") => Some((Framework::FastApi, true)),
        _ => None,
    }
}

/// The value of a keyword argument (`url_prefix=` / `prefix=`) on a call. A
/// `**options` expansion may carry the keyword, so without an explicit one the
/// prefix is Unknown rather than absent.
fn keyword_prefix(cx: &FileCx<'_>, call: TsNode<'_>, keyword: &str) -> Prefix {
    let mut expanded = false;
    for argument in named_arguments(call) {
        if argument.kind() == "dictionary_splat" {
            expanded = true;
            continue;
        }
        if argument.kind() != "keyword_argument" {
            continue;
        }
        let (Some(name), Some(value)) = (
            argument.child_by_field_name("name"),
            argument.child_by_field_name("value"),
        ) else {
            continue;
        };
        if cx.text(&name) == keyword {
            return literal_string(cx, value).map_or(Prefix::Unknown, Prefix::Literal);
        }
    }
    if expanded {
        Prefix::Unknown
    } else {
        Prefix::Absent
    }
}

/// A router constructor's own prefix. Flask's `Blueprint` also accepts
/// `url_prefix` as its sixth positional argument, and a `*args` expansion may
/// supply it; FastAPI's `APIRouter` takes `prefix` by keyword only.
fn constructor_prefix(cx: &FileCx<'_>, call: TsNode<'_>, framework: Framework) -> Prefix {
    let prefix = keyword_prefix(cx, call, prefix_keyword(framework));
    if framework != Framework::Flask || prefix != Prefix::Absent {
        return prefix;
    }
    let positional: Vec<TsNode<'_>> = named_arguments(call)
        .into_iter()
        .filter(|argument| !matches!(argument.kind(), "keyword_argument" | "comment"))
        .collect();
    if positional
        .iter()
        .any(|argument| argument.kind() == "list_splat")
    {
        return Prefix::Unknown;
    }
    match positional.get(5) {
        Some(argument) => literal_string(cx, *argument).map_or(Prefix::Unknown, Prefix::Literal),
        None => Prefix::Absent,
    }
}

/// A Flask `Blueprint`'s name: its `name=` keyword or first positional
/// argument.
fn blueprint_name(cx: &FileCx<'_>, call: TsNode<'_>) -> Prefix {
    match keyword_prefix(cx, call, "name") {
        Prefix::Absent => {}
        named => return named,
    }
    match named_arguments(call).first() {
        Some(first) if !matches!(first.kind(), "keyword_argument" | "list_splat") => {
            literal_string(cx, *first).map_or(Prefix::Unknown, Prefix::Literal)
        }
        _ => Prefix::Unknown,
    }
}

/// The nearest enclosing function definition of `node`.
pub(crate) fn enclosing_function_node(node: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut parent = node.parent();
    while let Some(current) = parent {
        if current.kind() == "function_definition" {
            return Some(current);
        }
        parent = current.parent();
    }
    None
}

/// Whether `name` is a parameter of `function`: `Some(annotation)` when it is,
/// with the annotation's text if one is written.
fn parameter_annotation<'s>(
    cx: &'s FileCx<'_>,
    function: TsNode<'_>,
    name: &str,
) -> Option<Option<&'s str>> {
    let parameters = function.child_by_field_name("parameters")?;
    let mut walk = parameters.walk();
    for parameter in parameters.named_children(&mut walk) {
        let (ident, annotation) = match parameter.kind() {
            "identifier" => (Some(parameter), None),
            "default_parameter" => (parameter.child_by_field_name("name"), None),
            "typed_default_parameter" => (
                parameter.child_by_field_name("name"),
                parameter.child_by_field_name("type"),
            ),
            "typed_parameter" => {
                let mut inner = parameter.walk();
                let ident = parameter
                    .named_children(&mut inner)
                    .find(|child| child.kind() == "identifier");
                (ident, parameter.child_by_field_name("type"))
            }
            _ => (None, None),
        };
        if ident.is_some_and(|ident| cx.text(&ident) == name) {
            return Some(annotation.map(|annotation| cx.text(&annotation).trim()));
        }
    }
    None
}

/// Whether `target` (an assignment, `for`, `with` or `del` target) rebinds
/// `name`. Attribute and subscript targets write members, not the name.
fn target_binds(cx: &FileCx<'_>, target: TsNode<'_>, name: &str) -> bool {
    match target.kind() {
        "identifier" => cx.text(&target) == name,
        "attribute" | "subscript" => false,
        _ => {
            let mut walk = target.walk();
            target
                .named_children(&mut walk)
                .any(|child| target_binds(cx, child, name))
        }
    }
}

/// Whether a `match` pattern node captures `name` itself: a bare name
/// (`case V:`, `C(key=V)`, `[V, *V]`), not a class (`case V():`), a dotted
/// value (`case V.X:`) or a keyword. Nested patterns are separate nodes, and
/// `as` aliases are matched as `as_pattern`.
fn captures(cx: &FileCx<'_>, node: TsNode<'_>, name: &str) -> bool {
    if name == "_" {
        return false;
    }
    let mut walk = node.walk();
    node.named_children(&mut walk)
        .any(|child| match child.kind() {
            "dotted_name" if matches!(node.kind(), "case_pattern" | "keyword_pattern") => {
                child.named_child_count() == 1 && cx.text(&child) == name
            }
            "identifier" => {
                matches!(
                    node.kind(),
                    "splat_pattern" | "list_pattern" | "tuple_pattern"
                ) && cx.text(&child) == name
            }
            _ => false,
        })
}

/// Whether any identifier in `node` is spelled `name`.
fn mentions(cx: &FileCx<'_>, node: TsNode<'_>, name: &str) -> bool {
    if node.kind() == "identifier" {
        return cx.text(&node) == name;
    }
    let mut walk = node.walk();
    node.named_children(&mut walk)
        .any(|child| mentions(cx, child, name))
}

/// How `function`'s own scope binds `name`.
#[derive(Default)]
pub(crate) struct ScopeBinding<'t> {
    /// Every statement or expression that binds `name` in this scope,
    /// including a nested scope's `nonlocal` rebinding.
    pub(crate) writes: Vec<TsNode<'t>>,
    pub(crate) global: bool,
    pub(crate) nonlocal: bool,
}

/// Collect `function`'s bindings of `name`. Anything that might bind it
/// counts, so an unrecognised form fails closed rather than proving a value.
pub(crate) fn scope_binding<'t>(
    cx: &FileCx<'_>,
    function: TsNode<'t>,
    name: &str,
) -> ScopeBinding<'t> {
    collect_binding(cx, function.child_by_field_name("body"), name, false)
}

/// Collect the module's bindings of `name` (`root` is the `module` node).
/// A nested scope declaring `global name` counts as a write: it may rebind
/// the module name whenever it runs.
pub(crate) fn module_binding<'t>(
    cx: &FileCx<'_>,
    root: TsNode<'t>,
    name: &str,
) -> ScopeBinding<'t> {
    collect_binding(cx, Some(root), name, true)
}

fn collect_binding<'t>(
    cx: &FileCx<'_>,
    start: Option<TsNode<'t>>,
    name: &str,
    module: bool,
) -> ScopeBinding<'t> {
    let mut binding = ScopeBinding::default();
    let mut stack: Vec<TsNode<'t>> = start.into_iter().collect();
    while let Some(node) = stack.pop() {
        let binds = match node.kind() {
            "function_definition" | "class_definition" => {
                // Its own name binds here; its body is another scope, except
                // that a `nonlocal` in it rebinds this one.
                let named = node
                    .child_by_field_name("name")
                    .is_some_and(|ident| cx.text(&ident) == name);
                let mut inner = node.walk();
                let rebinds = node
                    .children_by_field_name("body", &mut inner)
                    .any(|body| nonlocal_in(cx, body, name) || module && global_in(cx, body, name));
                if named || rebinds {
                    binding.writes.push(node);
                }
                // Defaults, annotations and bases run here, at definition.
                stack.extend(definition_time_parts(node));
                continue;
            }
            "lambda" => {
                stack.extend(definition_time_parts(node));
                continue;
            }
            "global_statement" => {
                binding.global |= mentions(cx, node, name);
                false
            }
            "nonlocal_statement" => {
                binding.nonlocal |= mentions(cx, node, name);
                false
            }
            "assignment" => {
                node.child_by_field_name("right").is_some()
                    && node
                        .child_by_field_name("left")
                        .is_some_and(|left| target_binds(cx, left, name))
            }
            "augmented_assignment" | "for_statement" => node
                .child_by_field_name("left")
                .is_some_and(|left| target_binds(cx, left, name)),
            "named_expression" => node
                .child_by_field_name("name")
                .is_some_and(|ident| cx.text(&ident) == name),
            "as_pattern" | "except_clause" => node
                .child_by_field_name("alias")
                .is_some_and(|alias| target_binds(cx, alias, name)),
            "delete_statement" => {
                let mut walk = node.walk();
                node.named_children(&mut walk)
                    .any(|target| target_binds(cx, target, name))
            }
            "import_statement" | "import_from_statement" => import_binds(cx, node, name),
            "case_pattern" | "keyword_pattern" | "splat_pattern" | "list_pattern"
            | "tuple_pattern" => captures(cx, node, name),
            _ => false,
        };
        if binds {
            binding.writes.push(node);
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    binding
}

/// Whether an import statement binds `name`: an `as` alias, the first part
/// of an unaliased `import a.b`, or the name of `from m import a`. A
/// wildcard may bind anything.
fn import_binds(cx: &FileCx<'_>, node: TsNode<'_>, name: &str) -> bool {
    let mut walk = node.walk();
    if node
        .named_children(&mut walk)
        .any(|child| child.kind() == "wildcard_import")
    {
        return true;
    }
    let from = node.kind() == "import_from_statement";
    let mut walk = node.walk();
    node.children_by_field_name("name", &mut walk)
        .any(|imported| match imported.kind() {
            "aliased_import" => imported
                .child_by_field_name("alias")
                .is_some_and(|alias| cx.text(&alias) == name),
            _ if from => cx.text(&imported).trim() == name,
            _ => cx.text(&imported).split('.').next().map(str::trim) == Some(name),
        })
}

/// The parts of a `def`, `class` or `lambda` evaluated in the enclosing
/// scope when it is defined: parameter defaults and annotations, the return
/// annotation, and class bases. Parameter names themselves bind nothing
/// here.
fn definition_time_parts<'t>(node: TsNode<'t>) -> Vec<TsNode<'t>> {
    [
        "parameters",
        "return_type",
        "superclasses",
        "type_parameters",
    ]
    .into_iter()
    .filter_map(|field| node.child_by_field_name(field))
    .collect()
}

/// Whether a `global` statement anywhere under `node` names `name`.
fn global_in(cx: &FileCx<'_>, node: TsNode<'_>, name: &str) -> bool {
    if node.kind() == "global_statement" {
        return mentions(cx, node, name);
    }
    let mut walk = node.walk();
    node.named_children(&mut walk)
        .any(|child| global_in(cx, child, name))
}

/// Whether a `nonlocal` statement anywhere under `node` names `name`.
fn nonlocal_in(cx: &FileCx<'_>, node: TsNode<'_>, name: &str) -> bool {
    if node.kind() == "nonlocal_statement" {
        return mentions(cx, node, name);
    }
    let mut walk = node.walk();
    node.named_children(&mut walk)
        .any(|child| nonlocal_in(cx, child, name))
}

/// What a function-local receiver `name` at `object` is, following Python's
/// scope rules outward from the nearest enclosing function (#747).
enum LocalReceiver<'s> {
    /// The scope's only binding is an import-proven factory assignment that
    /// precedes the call: at the call the name holds that app or is unbound.
    /// The assignment's start byte identifies the app.
    App(Framework, usize),
    /// An unrebound parameter, with its annotation if one is written.
    Parameter(Option<&'s str>),
    /// Bound locally, but not provably to an application.
    Unproven,
    /// Not bound by any enclosing function: a module-level name.
    Free,
}

fn local_receiver<'s>(
    cx: &'s FileCx<'_>,
    object: TsNode<'_>,
    name: &str,
    bindings: &HashMap<String, ImportBinding>,
) -> LocalReceiver<'s> {
    let mut scope = enclosing_function_node(object);
    while let Some(function) = scope {
        let binding = scope_binding(cx, function, name);
        let parameter = parameter_annotation(cx, function, name);
        if binding.global {
            return if binding.writes.is_empty() && parameter.is_none() {
                LocalReceiver::Free
            } else {
                LocalReceiver::Unproven
            };
        }
        if binding.nonlocal {
            if !binding.writes.is_empty() || parameter.is_some() {
                return LocalReceiver::Unproven;
            }
        } else if let Some(annotation) = parameter {
            return if binding.writes.is_empty() {
                LocalReceiver::Parameter(annotation)
            } else {
                LocalReceiver::Unproven
            };
        } else if !binding.writes.is_empty() {
            let [write] = binding.writes[..] else {
                return LocalReceiver::Unproven;
            };
            // A constructor after the call may not have run when it does.
            // In an outer scope that is any write after the definition of
            // the closure making the call, which may be invoked first.
            if write.end_byte() > object.start_byte() {
                return LocalReceiver::Unproven;
            }
            return factory_assignment(cx, write, name, bindings)
                .map_or(LocalReceiver::Unproven, |framework| {
                    LocalReceiver::App(framework, write.start_byte())
                });
        }
        scope = enclosing_function_node(function);
    }
    LocalReceiver::Free
}

/// The framework of `write` when it is exactly `name = <factory>(...)`.
fn factory_assignment(
    cx: &FileCx<'_>,
    write: TsNode<'_>,
    name: &str,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<Framework> {
    if write.kind() != "assignment" {
        return None;
    }
    let left = write.child_by_field_name("left")?;
    let right = write.child_by_field_name("right")?;
    if left.kind() != "identifier" || cx.text(&left) != name || right.kind() != "call" {
        return None;
    }
    call_name(cx, right).and_then(|callee| framework_factory(&callee, bindings))
}

/// What the object of a mount call is, as far as this file can prove it.
fn mount_parent(
    cx: &FileCx<'_>,
    object: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> MountParent {
    if object.kind() != "identifier" {
        return MountParent::Unproven;
    }
    let name = cx.text(&object);
    match local_receiver(cx, object, name, bindings) {
        LocalReceiver::App(framework, _) => return MountParent::LocalApp(framework),
        LocalReceiver::Parameter(annotation) => {
            return annotation
                .and_then(|annotation| framework_factory(annotation, bindings))
                .map_or(MountParent::Unproven, MountParent::AnnotatedApp);
        }
        LocalReceiver::Unproven => return MountParent::Unproven,
        LocalReceiver::Free => {}
    }
    match bindings.get(name) {
        Some(ImportBinding::Symbol {
            target_file: Some(target),
            exported,
            ..
        }) => MountParent::Def((target.clone(), exported.clone())),
        Some(_) => MountParent::Unproven,
        None => MountParent::Def((cx.path.to_string(), name.to_string())),
    }
}

fn prefix_keyword(framework: Framework) -> &'static str {
    match framework {
        Framework::Flask => "url_prefix",
        Framework::FastApi => "prefix",
    }
}

fn framework_factory(callee: &str, bindings: &HashMap<String, ImportBinding>) -> Option<Framework> {
    let mut parts = callee.split('.');
    let base = parts.next()?;
    let member = parts.next();
    match (bindings.get(base), member) {
        (
            Some(ImportBinding::Symbol {
                module, exported, ..
            }),
            None,
        ) if module == "fastapi" && exported == "FastAPI" => Some(Framework::FastApi),
        (
            Some(ImportBinding::Symbol {
                module, exported, ..
            }),
            None,
        ) if module == "flask" && exported == "Flask" => Some(Framework::Flask),
        (Some(ImportBinding::Module { module }), Some("FastAPI")) if module == "fastapi" => {
            Some(Framework::FastApi)
        }
        (Some(ImportBinding::Module { module }), Some("Flask")) if module == "flask" => {
            Some(Framework::Flask)
        }
        _ => None,
    }
}

fn assignment_receivers(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) -> HashMap<String, Framework> {
    let query =
        crate::queries::query(r#"(assignment left: (identifier) @variable right: (call) @call)"#);
    let mut receivers = HashMap::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    while let Some(found) = matches.next() {
        let mut variable = None;
        let mut call = None;
        for capture in found.captures() {
            match query.capture_names()[capture.index as usize] {
                "variable" => variable = Some(cx.text(&capture.node).to_string()),
                "call" => call = Some(capture.node),
                _ => {}
            }
        }
        let (Some(variable), Some(call)) = (variable, call) else {
            continue;
        };
        let Some(callee) = call_name(cx, call) else {
            continue;
        };
        let Some((framework, router)) = receiver_factory(&callee, bindings) else {
            continue;
        };
        if !router {
            receivers.insert(variable.clone(), framework);
        }
        let sole = call.parent().is_some_and(|assignment| {
            let writes = module_binding(cx, root, &variable).writes;
            writes.len() == 1 && writes[0] == assignment
        });
        out.receiver_defs.push(ReceiverDef {
            key: (cx.path.to_string(), variable),
            framework,
            router,
            module_level: enclosing_function_node(call).is_none(),
            sole,
            prefix: if router {
                constructor_prefix(cx, call, framework)
            } else {
                Prefix::Absent
            },
            name: if router && framework == Framework::Flask {
                blueprint_name(cx, call)
            } else {
                Prefix::Absent
            },
            evidence: cx.evidence(&call),
        });
    }
    receivers
}

pub(crate) fn qualified_function_name(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<String> {
    let name = function.child_by_field_name("name")?;
    let mut names = vec![cx.text(&name).to_string()];
    let mut parent = function.parent();
    while let Some(node) = parent {
        if matches!(node.kind(), "function_definition" | "class_definition")
            && let Some(name) = node.child_by_field_name("name")
        {
            names.push(cx.text(&name).to_string());
        }
        parent = node.parent();
    }
    names.reverse();
    Some(names.join("."))
}

fn named_arguments(node: TsNode<'_>) -> Vec<TsNode<'_>> {
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut walk = arguments.walk();
    arguments.named_children(&mut walk).collect()
}

/// A `@receiver.verb("/path", methods=[...])` decorator, before the receiver
/// is known to be a framework object.
struct RouteDecorator<'tree> {
    receiver: TsNode<'tree>,
    verb: String,
    path: String,
    listed_methods: Vec<String>,
    /// `endpoint=`, which replaces the view function's name.
    endpoint: Prefix,
}

fn route_decorator<'tree>(
    cx: &FileCx<'_>,
    decorator: TsNode<'tree>,
) -> Option<RouteDecorator<'tree>> {
    let mut walk = decorator.walk();
    let call = decorator
        .named_children(&mut walk)
        .find(|child| child.kind() == "call")?;
    let function = call.child_by_field_name("function")?;
    if function.kind() != "attribute" {
        return None;
    }
    let receiver = function.child_by_field_name("object")?;
    let verb = cx
        .text(&function.child_by_field_name("attribute")?)
        .to_string();
    let arguments = named_arguments(call);
    let path = arguments
        .first()
        .copied()
        .and_then(|argument| literal_string(cx, argument))?;
    let mut listed_methods = Vec::new();
    for argument in arguments.iter().skip(1) {
        if argument.kind() != "keyword_argument" {
            continue;
        }
        let (Some(name), Some(value)) = (
            argument.child_by_field_name("name"),
            argument.child_by_field_name("value"),
        ) else {
            continue;
        };
        if cx.text(&name) != "methods" || value.kind() != "list" {
            continue;
        }
        let mut walk = value.walk();
        listed_methods.extend(
            value
                .named_children(&mut walk)
                .filter_map(|item| literal_string(cx, item))
                .map(|method| method.to_ascii_uppercase()),
        );
    }
    Some(RouteDecorator {
        receiver,
        verb,
        path,
        listed_methods,
        endpoint: keyword_prefix(cx, call, "endpoint"),
    })
}

/// HTTP methods a decorator declares on a receiver of `framework`.
fn methods_for(framework: Framework, verb: &str, listed: &[String]) -> Vec<String> {
    const DIRECT_METHODS: &[&str] = &["get", "post", "put", "delete", "patch", "options", "head"];
    if DIRECT_METHODS.contains(&verb) {
        return vec![verb.to_ascii_uppercase()];
    }
    if framework != Framework::Flask || verb != "route" {
        return Vec::new();
    }
    if listed.is_empty() {
        vec!["GET".into()]
    } else {
        listed.to_vec()
    }
}

/// Join a router prefix and a route path the way Flask and FastAPI do.
fn join_route(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return path.to_string();
    }
    if path.starts_with('/') {
        format!("{prefix}{path}")
    } else {
        format!("{prefix}/{path}")
    }
}

fn enclosing_function(mut node: TsNode<'_>, functions: &HashMap<usize, String>) -> Option<String> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "function_definition"
            && let Some(id) = functions.get(&parent.start_byte())
        {
            return Some(id.clone());
        }
        node = parent;
    }
    None
}

/// Point an absolute import at the recovered file it names from the
/// repository root, the same root `directory-proven` calls resolve against:
/// `a.b` is `a/b.py` or the package `a/b/__init__.py`. When both exist, or
/// neither, or a parent is a plain module (`a.py` without `a/__init__.py`,
/// which Python loads ahead of the directory, so `a.b` cannot import), the
/// import keeps its `mod:` endpoint for boundary classification.
fn resolve_absolute_imports(out: &mut Extraction, repo: &str) {
    let files = out
        .nodes
        .iter()
        .filter(|node| node.label == "File")
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    for edge in out.edges.iter_mut().filter(|edge| edge.label == "IMPORTS") {
        let Some(module) = edge.dst.strip_prefix("mod:") else {
            continue;
        };
        if module.starts_with('.') {
            continue;
        }
        let Some(module_path) = module_file("", module) else {
            continue;
        };
        let parts = module.split('.').collect::<Vec<_>>();
        let blocked = (1..parts.len()).any(|end| {
            let prefix = parts[..end].join("/");
            files.contains(&file_id(repo, &format!("{prefix}.py")))
                && !files.contains(&file_id(repo, &format!("{prefix}/__init__.py")))
        });
        if blocked {
            continue;
        }
        let package = format!("{}/__init__.py", module_path.trim_end_matches(".py"));
        let found = [module_path, package]
            .into_iter()
            .map(|path| file_id(repo, &path))
            .filter(|file| files.contains(file))
            .collect::<Vec<_>>();
        if let [file] = found.as_slice() {
            edge.dst = file.clone();
            edge.props["resolution"] = "directory-proven".into();
        }
    }
}

/// Every name a repository source could answer an absolute import's top
/// level with: each directory beneath the walk and each module stem
/// (`.py`/`.pyi`/`.pyx`/`.pxd` sources and built `.so`/`.pyd` extensions),
/// across every source root. Over-inclusion only turns an external
/// classification into a Gap — fail closed.
fn local_module_names(root: &Path) -> Result<BTreeSet<String>, ExtractError> {
    let skip = |name: &str| {
        name.starts_with('.')
            || matches!(
                name,
                "__pycache__" | "venv" | "site-packages" | "node_modules" | "dist" | "build"
            )
    };
    let mut out = BTreeSet::new();
    for file in source_walk::files(root, &skip, source_walk::Gitignores::Honor)? {
        let mut components: Vec<&str> = file.rel.split('/').collect();
        components.pop();
        out.extend(components.into_iter().map(str::to_string));
        if let Some((stem, extension)) = file.name.split_once('.')
            && matches!(
                extension.rsplit('.').next(),
                Some("py" | "pyi" | "pyx" | "pxd" | "so" | "pyd")
            )
        {
            out.insert(stem.to_string());
        }
    }
    Ok(out)
}

/// Classify an absolute import no declaration resolved (#237, ADR-0031):
/// external only when no directory or module anywhere in the repository
/// shares its top-level name; otherwise the repository may provide it and
/// the import stays an explicit Gap. Relative imports are always in-system.
fn classify_import(module: &str, local_names: &BTreeSet<String>) -> Boundary {
    let top = module.split('.').next().unwrap_or(module);
    if module.starts_with('.') || local_names.contains(top) {
        return Boundary::Unresolved {
            reason: "unresolved import of a module this repository may provide".into(),
        };
    }
    Boundary::External {
        reason: "module not provided by any source in this repository".into(),
        evidence: vec![],
    }
}

/// `x.register_blueprint(bp, url_prefix=...)` and `x.include_router(r,
/// prefix=...)` wherever they appear (often inside a factory function whose
/// app is a parameter). The mounted router is resolved after the walk.
fn collect_mounts(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) {
    let query =
        crate::queries::query("(call function: (attribute attribute: (identifier) @verb)) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    while let Some(found) = matches.next() {
        let (mut call, mut verb) = (None, None);
        for capture in found.captures() {
            match query.capture_names()[capture.index as usize] {
                "call" => call = Some(capture.node),
                "verb" => verb = Some(cx.text(&capture.node)),
                _ => {}
            }
        }
        let (Some(call), Some(verb)) = (call, verb) else {
            continue;
        };
        let framework = match verb {
            "register_blueprint" => Framework::Flask,
            "include_router" => Framework::FastApi,
            _ => continue,
        };
        let Some(object) = call
            .child_by_field_name("function")
            .and_then(|function| function.child_by_field_name("object"))
        else {
            continue;
        };
        let app = app_scope::app_ref(cx, object, bindings);
        let first = named_arguments(call).first().copied();
        let Some(first) = first.filter(|first| first.kind() == "identifier") else {
            if framework == Framework::Flask {
                out.wild_mounts.push(app);
            }
            continue;
        };
        let name = cx.text(&first).to_string();
        let receiver = match bindings.get(&name) {
            Some(ImportBinding::Symbol {
                target_file: Some(target),
                exported,
                ..
            }) => (target.clone(), exported.clone()),
            Some(_) => {
                if framework == Framework::Flask {
                    out.wild_mounts.push(app);
                }
                continue;
            }
            None => (cx.path.to_string(), name),
        };
        out.mounts.push(Mount {
            receiver,
            parent: mount_parent(cx, object, bindings),
            framework,
            prefix: keyword_prefix(cx, call, prefix_keyword(framework)),
            name: match framework {
                Framework::Flask => keyword_prefix(cx, call, "name"),
                Framework::FastApi => Prefix::Absent,
            },
            app,
            evidence: cx.evidence(&call),
        });
    }
}

/// Longest mount chain followed from a router to its application.
const MAX_MOUNT_DEPTH: usize = 16;

const UNREADABLE_PREFIX: &str = "router prefix is not a string literal";

/// One prefix a router's routes are served under, with the evidence and
/// confidence of the mount chain that proves it.
#[derive(Debug, Clone)]
struct Served {
    prefix: Prefix,
    evidence: Vec<EvidenceRef>,
    confidence: ConfidenceTier,
    mount: Option<&'static str>,
    reason: &'static str,
    /// The dotted Blueprint names before a route's own endpoint name: empty
    /// on an application, None when a name is not a literal.
    names: Option<String>,
}

fn tier_rank(tier: ConfidenceTier) -> u8 {
    match tier {
        ConfidenceTier::Confirmed => 3,
        ConfidenceTier::InferredStrong => 2,
        ConfidenceTier::InferredWeak => 1,
        ConfidenceTier::Gap => 0,
    }
}

/// A prefix or name written as a literal.
fn literal(prefix: &Prefix) -> Option<&str> {
    match prefix {
        Prefix::Literal(value) => Some(value),
        _ => None,
    }
}

/// `outer` then `inner`, as a nested mount composes them.
fn concat_prefix(outer: &Prefix, inner: &Prefix) -> Prefix {
    match (outer, inner) {
        (Prefix::Unknown, _) | (_, Prefix::Unknown) => Prefix::Unknown,
        (Prefix::Absent, Prefix::Absent) => Prefix::Absent,
        (Prefix::Absent, other) | (other, Prefix::Absent) => other.clone(),
        (Prefix::Literal(outer), Prefix::Literal(inner)) => {
            Prefix::Literal(join_route(outer, inner))
        }
    }
}

/// The prefix one mount gives a router (Flask: the mount's `url_prefix`
/// replaces the blueprint's own; FastAPI: the mount's `prefix` is prepended).
fn mount_contribution(framework: Framework, mount: &Prefix, declared: &Prefix) -> Prefix {
    match (framework, mount) {
        (_, Prefix::Unknown) => Prefix::Unknown,
        (Framework::Flask, Prefix::Literal(prefix)) => Prefix::Literal(prefix.clone()),
        (Framework::Flask, Prefix::Absent) => declared.clone(),
        (Framework::FastApi, mount) => concat_prefix(mount, declared),
    }
}

/// Every prefix the routes on `key` are served under, following mounts up to
/// the application. A mount on a receiver that is not a proven application or
/// router of the same framework never confirms a path: it stays InferredStrong,
/// and is dropped whenever another chain confirms the router.
fn served_prefixes(
    key: &(String, String),
    defs: &HashMap<(String, String), ReceiverDef>,
    mounts: &HashMap<(String, String), Vec<Mount>>,
    chain: &mut Vec<(String, String)>,
) -> Vec<Served> {
    let Some(def) = defs.get(key) else {
        return Vec::new();
    };
    if !def.router {
        return vec![Served {
            prefix: Prefix::Absent,
            evidence: vec![def.evidence.clone()],
            confidence: ConfidenceTier::Confirmed,
            mount: None,
            reason: UNREADABLE_PREFIX,
            names: Some(String::new()),
        }];
    }
    if chain.contains(key) || chain.len() >= MAX_MOUNT_DEPTH {
        return vec![Served {
            prefix: Prefix::Unknown,
            evidence: vec![def.evidence.clone()],
            confidence: ConfidenceTier::Gap,
            mount: None,
            reason: "router mount chain is cyclic or deeper than 16 mounts",
            names: None,
        }];
    }
    let observed: Vec<&Mount> = mounts
        .get(key)
        .into_iter()
        .flatten()
        .filter(|mount| mount.framework == def.framework)
        .collect();
    if observed.is_empty() {
        return vec![Served {
            prefix: def.prefix.clone(),
            evidence: vec![def.evidence.clone()],
            confidence: ConfidenceTier::InferredStrong,
            mount: Some("mount_not_observed"),
            reason: UNREADABLE_PREFIX,
            names: literal(&def.name).map(str::to_string),
        }];
    }
    chain.push(key.clone());
    let mut served = Vec::new();
    for mount in observed {
        let own = mount_contribution(def.framework, &mount.prefix, &def.prefix);
        let root = |confidence, label| Served {
            prefix: Prefix::Absent,
            evidence: Vec::new(),
            confidence,
            mount: Some(label),
            reason: UNREADABLE_PREFIX,
            names: Some(String::new()),
        };
        let parents = match &mount.parent {
            MountParent::Def(parent)
                if defs.get(parent).is_some_and(|parent| {
                    parent.module_level && parent.framework == def.framework
                }) =>
            {
                served_prefixes(parent, defs, mounts, chain)
            }
            MountParent::LocalApp(framework) if *framework == def.framework => {
                vec![root(ConfidenceTier::Confirmed, "mounted")]
            }
            MountParent::AnnotatedApp(framework) if *framework == def.framework => {
                vec![root(
                    ConfidenceTier::InferredStrong,
                    "mount_receiver_annotated",
                )]
            }
            _ => vec![root(
                ConfidenceTier::InferredStrong,
                "mount_receiver_unproven",
            )],
        };
        let own_name = match &mount.name {
            Prefix::Absent => literal(&def.name),
            name => literal(name),
        };
        for parent in parents {
            let names = match (parent.names.as_deref(), own_name) {
                (Some(""), Some(own)) => Some(own.to_string()),
                (Some(outer), Some(own)) => Some(format!("{outer}.{own}")),
                _ => None,
            };
            let mut evidence = vec![def.evidence.clone(), mount.evidence.clone()];
            evidence.extend(parent.evidence);
            let prefix = concat_prefix(&parent.prefix, &own);
            let reason = if parent.prefix == Prefix::Unknown {
                parent.reason
            } else {
                UNREADABLE_PREFIX
            };
            served.push(Served {
                prefix,
                evidence,
                confidence: if tier_rank(parent.confidence) < tier_rank(ConfidenceTier::Confirmed) {
                    parent.confidence
                } else {
                    ConfidenceTier::Confirmed
                },
                mount: Some(parent.mount.unwrap_or("mounted")),
                reason,
                names,
            });
        }
    }
    chain.pop();
    if served
        .iter()
        .any(|served| served.confidence == ConfidenceTier::Confirmed)
    {
        served.retain(|served| served.mount != Some("mount_receiver_unproven"));
    }
    served
}

/// Every application the routes on `key` may be served by, over every mount
/// chain `served_prefixes` follows, none dropped (#1149).
fn served_apps(
    key: &(String, String),
    defs: &HashMap<(String, String), ReceiverDef>,
    mounts: &HashMap<(String, String), Vec<Mount>>,
    chain: &mut Vec<(String, String)>,
) -> Vec<app_scope::AppRef> {
    let Some(def) = defs.get(key) else {
        return vec![app_scope::AppRef::Any];
    };
    if !def.router {
        return vec![app_scope::AppRef::Def(key.clone())];
    }
    let observed: Vec<&Mount> = mounts
        .get(key)
        .into_iter()
        .flatten()
        .filter(|mount| mount.framework == def.framework)
        .collect();
    if observed.is_empty() || chain.contains(key) || chain.len() >= MAX_MOUNT_DEPTH {
        return vec![app_scope::AppRef::Any];
    }
    chain.push(key.clone());
    let mut apps = Vec::new();
    for mount in observed {
        match &mount.parent {
            MountParent::Def(parent)
                if defs
                    .get(parent)
                    .is_some_and(|parent| parent.router && parent.framework == def.framework) =>
            {
                apps.extend(served_apps(parent, defs, mounts, chain));
            }
            _ => apps.push(mount.app.clone()),
        }
    }
    chain.pop();
    apps
}

/// Join pending router routes with their definitions and the complete mount
/// chain up to an application. A prefix this tier cannot read, a cyclic or
/// over-deep chain, yields a Gap on the handler instead of an invented path. A
/// router with no observed mount keeps its declared prefix at InferredStrong;
/// so does one mounted only on an annotated parameter or an unproven receiver.
fn resolve_routes(out: &mut Extraction, repo: &str) {
    out.app_defs.extend(app_scope::app_defs(
        &out.receiver_defs,
        &out.module_attr_writes,
    ));
    let defs: HashMap<(String, String), ReceiverDef> = std::mem::take(&mut out.receiver_defs)
        .into_iter()
        .map(|def| (def.key.clone(), def))
        .collect();
    let mut mounts: HashMap<(String, String), Vec<Mount>> = HashMap::new();
    for mount in std::mem::take(&mut out.mounts) {
        mounts
            .entry(mount.receiver.clone())
            .or_default()
            .push(mount);
    }
    // A Blueprint registration this tier cannot name may register any
    // Blueprint on its parent.
    let mut wild = std::mem::take(&mut out.wild_mounts);
    wild.extend(
        mounts
            .iter()
            .filter(|(key, _)| {
                !defs
                    .get(*key)
                    .is_some_and(|def| def.router && def.framework == Framework::Flask)
            })
            .flat_map(|(_, mounts)| mounts)
            .filter(|mount| mount.framework == Framework::Flask)
            .map(|mount| mount.app.clone()),
    );
    wild.sort();
    wild.dedup();
    let mut emitted = BTreeSet::new();
    for route in std::mem::take(&mut out.pending_routes) {
        let Some(def) = defs.get(&route.receiver) else {
            continue;
        };
        let methods = methods_for(def.framework, &route.verb, &route.listed_methods);
        if methods.is_empty() {
            continue;
        }
        if def.framework == Framework::Flask {
            let mut apps = served_apps(&route.receiver, &defs, &mounts, &mut Vec::new());
            if def.router {
                apps.extend(wild.iter().cloned());
            }
            out.endpoint_apps
                .extend(apps.into_iter().map(|app| (route.handler.clone(), app)));
        }
        for served in served_prefixes(&route.receiver, &defs, &mounts, &mut Vec::new()) {
            let mount = served.mount;
            let prefix = match served.prefix {
                Prefix::Absent => String::new(),
                Prefix::Literal(prefix) => prefix,
                Prefix::Unknown => {
                    let gap_id = format!(
                        "gap:route:{repo}@{}@{}",
                        route.evidence.path, route.evidence.byte_start
                    );
                    if emitted.insert(gap_id.clone()) {
                        let mut evidence = vec![route.evidence.clone()];
                        evidence.extend(served.evidence);
                        out.nodes.push(Node {
                            id: gap_id.clone(),
                            label: "Gap".into(),
                            props: serde_json::json!({
                                "reason": served.reason,
                                "route": route.path,
                                "attempted_tiers": ["T0"],
                                "prov": prov_from(evidence.clone(), ConfidenceTier::Gap, &format!("Gap {gap_id}")),
                            }),
                        });
                        out.edges.push(Edge {
                            src: gap_id.clone(),
                            dst: route.handler.clone(),
                            label: "HANDLES".into(),
                            props: serde_json::json!({
                                "prov": prov_from(evidence, ConfidenceTier::Gap, &format!("HANDLES {gap_id} -> {}", route.handler)),
                            }),
                        });
                    }
                    continue;
                }
            };
            let path = join_route(&prefix, &route.path);
            for method in &methods {
                if !emitted.insert(format!("{method} {path} {}", route.handler)) {
                    continue;
                }
                let mut evidence = vec![route.evidence.clone()];
                evidence.extend(served.evidence.iter().cloned());
                push_endpoint(
                    out,
                    repo,
                    method,
                    &path,
                    &route.handler,
                    def.framework,
                    &evidence,
                    served.confidence,
                    mount,
                    route.inputs.as_ref(),
                    // Responses read Flask's `(body, status)` semantics.
                    route
                        .responses
                        .as_ref()
                        .filter(|_| def.framework == Framework::Flask),
                    endpoint_name(
                        def.framework,
                        served.names.as_deref(),
                        route.endpoint.as_deref(),
                    )
                    .as_deref(),
                );
            }
        }
    }
}

/// The name Flask's `url_for` resolves a route by: the dotted Blueprint
/// names, then the route's own endpoint name.
fn endpoint_name(framework: Framework, names: Option<&str>, own: Option<&str>) -> Option<String> {
    if framework != Framework::Flask {
        return None;
    }
    match (names?, own?) {
        ("", own) => Some(own.to_string()),
        (names, own) => Some(format!("{names}.{own}")),
    }
}

#[allow(clippy::too_many_arguments)]
fn push_endpoint(
    out: &mut Extraction,
    repo: &str,
    method: &str,
    route: &str,
    handler: &str,
    framework: Framework,
    evidence: &[EvidenceRef],
    confidence: ConfidenceTier,
    mount: Option<&str>,
    inputs: Option<&serde_json::Value>,
    responses: Option<&serde_json::Value>,
    endpoint_name: Option<&str>,
) {
    let endpoint = format!("ep:{repo}@{method}:{route}");
    let framework_name = match framework {
        Framework::FastApi => "fastapi",
        Framework::Flask => "flask",
    };
    let mut props = serde_json::json!({
        "method": method,
        "path": route,
        "handler_sym": handler,
        "framework": framework_name,
        "language": "python",
        "prov": prov_from(evidence.to_vec(), confidence, &format!("Endpoint {endpoint}")),
    });
    if let Some(mount) = mount {
        props["mount"] = mount.into();
    }
    if let Some(inputs) = inputs {
        props["request_inputs"] = inputs.clone();
    }
    if let Some(responses) = responses {
        props["responses"] = responses.clone();
    }
    if let Some(name) = endpoint_name {
        props["endpoint_name"] = name.into();
    }
    out.nodes.push(Node {
        id: endpoint.clone(),
        label: "Endpoint".into(),
        props,
    });
    out.edges.push(Edge {
        src: endpoint.clone(),
        dst: handler.to_string(),
        label: "HANDLES".into(),
        props: serde_json::json!({
            "prov": prov_from(
                evidence.to_vec(),
                confidence,
                &format!("HANDLES {endpoint} -> {handler}"),
            ),
        }),
    });
}

/// Recover deterministic facts from one Python source file.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
) -> Result<Extraction, ExtractError> {
    let mut out = extract_file(source, path, id)?;
    let functions = responses::resolve(&mut out, id.repo);
    rate_limits::resolve_proven(&mut out);
    auth_guards::resolve(&mut out, id.repo, &functions);
    target_tests::resolve(&mut out, id.repo);
    sqlalchemy::resolve(&mut out, id.repo);
    flask_pages::resolve(&mut out, id.repo, id.commit, None);
    Ok(out)
}

/// Constants recorded per file, and the longest value recorded (#2027).
const MAX_STRING_CONSTANTS: usize = 256;
const MAX_STRING_CONSTANT_BYTES: usize = 160;

/// Top-level `UPPER_NAME = "plain literal"` bound once in the module, by
/// name (#2027). A sensitive name, a secret-shaped or token-shaped value, an
/// f-string, a prefixed or escaped literal and a long value are not
/// recorded; at most [`MAX_STRING_CONSTANTS`] per file.
fn string_constants(cx: &FileCx<'_>, root: tree_sitter::Node<'_>) -> BTreeMap<String, String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut found: Vec<(String, String)> = Vec::new();
    // Every module-scope binding counts toward a name being bound once,
    // including one under module-level `if`/`try`/`for`/`with`; nested
    // functions and classes are their own scopes.
    let mut stack: Vec<tree_sitter::Node<'_>> = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "function_definition" | "class_definition" | "lambda"
        ) {
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
        if node.kind() == "assignment"
            && node.parent().is_some_and(|p| p.parent() != Some(root))
            && let Some(left) = node.child_by_field_name("left")
            && left.kind() == "identifier"
        {
            *seen.entry(cx.text(&left).to_string()).or_default() += 1;
        }
    }
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        let Some(assignment) = (statement.kind() == "expression_statement")
            .then(|| statement.named_child(0))
            .flatten()
            .filter(|n| n.kind() == "assignment")
        else {
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
        let name = cx.text(&left).to_string();
        *seen.entry(name.clone()).or_default() += 1;
        if !name.starts_with(|c: char| c.is_ascii_uppercase())
            || !name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            || right.kind() != "string"
        {
            continue;
        }
        let text = cx.text(&right);
        let Some(q) = text.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            continue;
        };
        if text.len() < 2
            || !text.ends_with(q)
            || text.starts_with("\"\"\"")
            || text.starts_with("'''")
        {
            continue;
        }
        let value = &text[1..text.len() - 1];
        if value.is_empty()
            || value.len() > MAX_STRING_CONSTANT_BYTES
            || value.contains(['\\', '\n', q])
            || core_redact::is_sensitive_name(&name)
            || core_redact::is_token_shaped(value)
            || !core_redact::recognized_secrets(value).is_empty()
        {
            continue;
        }
        found.push((name, value.to_owned()));
    }
    found
        .into_iter()
        .filter(|(name, _)| seen.get(name) == Some(&1))
        .take(MAX_STRING_CONSTANTS)
        .collect()
}

/// Local names a function binds to a module constant (`res_status =
/// status.E502`), with every constant bound to each and the byte where it
/// is first bound, in source order (#2027): what a later `return
/// res_status` may answer. Nested functions and classes are not entered.
fn constant_locals(
    cx: &FileCx<'_>,
    function: tree_sitter::Node<'_>,
) -> BTreeMap<String, Vec<serde_json::Value>> {
    fn constant(text: &str) -> bool {
        let name = text.rsplit('.').next().unwrap_or(text);
        text.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }) && name.len() > 1
            && name.starts_with(|c: char| c.is_ascii_uppercase())
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    }
    let mut found: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    let Some(body) = function.child_by_field_name("body") else {
        return found;
    };
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "function_definition" | "class_definition" | "lambda"
        ) {
            continue;
        }
        if node.kind() == "assignment"
            && let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            )
            && left.kind() == "identifier"
            && matches!(right.kind(), "attribute" | "identifier")
        {
            let name = cx.text(&left);
            let value = cx.text(&right);
            if name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') && constant(value) {
                let values = found.entry(name.to_string()).or_default();
                if !values.iter().any(|v| v["value"] == value) && values.len() < 8 {
                    values.push(serde_json::json!({ "value": value, "at": node.start_byte() }));
                }
            }
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    found
}

fn extract_file(source: &[u8], path: &str, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| ExtractError::NoTree(path.into()))?;
    let root = tree.root_node();
    let cx = FileCx { source, path, id };
    let mut out = Extraction::default();
    out.nodes.push(Node {
        id: file_id(id.repo, path),
        label: "File".into(),
        props: serde_json::json!({
            "path": path,
            "language": "python",
            "prov": cx.prov(&root, &format!("File {path}")),
        }),
    });
    // Module string constants a rule can name (#2027): `E502 = "550 …"`.
    let constants = string_constants(&cx, root);
    if !constants.is_empty() {
        out.nodes[0].props["string_constants"] = serde_json::json!(constants);
    }

    let bindings = parse_imports(&cx, root, &mut out);
    let receivers = assignment_receivers(&cx, root, &bindings, &mut out);
    collect_mounts(&cx, root, &bindings, &mut out);
    app_scope::collect(&cx, root, &bindings, &mut out);
    sqlalchemy::extract(&cx, root, &bindings, &mut out);
    target_tests::extract(&cx, root, &bindings, &mut out);
    flask_admin::collect(&cx, root, &bindings, &mut out);
    let function_query =
        crate::queries::query("(function_definition name: (identifier) @name) @function");
    let mut functions_by_start = HashMap::new();
    let mut locals = HashMap::new();
    let mut functions = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(function_query, root, source);
    while let Some(found) = matches.next() {
        let mut function = None;
        let mut simple_name = None;
        for capture in found.captures() {
            match function_query.capture_names()[capture.index as usize] {
                "function" => function = Some(capture.node),
                "name" => simple_name = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(function), Some(simple_name), Some(qualified_name)) = (
            function,
            simple_name,
            function.and_then(|function| qualified_function_name(&cx, function)),
        ) else {
            continue;
        };
        let symbol = symbol_id(id.repo, path, &qualified_name);
        functions_by_start.insert(function.start_byte(), symbol.clone());
        if qualified_name == simple_name {
            locals.insert(simple_name.clone(), symbol.clone());
        }
        let mut props = serde_json::json!({
            "name": qualified_name,
            "kind": "function",
            "language": "python",
            "prov": cx.prov(&function, &format!("Symbol {symbol}")),
        });
        let bound = constant_locals(&cx, function);
        if !bound.is_empty() {
            props["constant_locals"] = serde_json::json!(bound);
        }
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props,
        });
        out.edges.push(Edge {
            src: symbol.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&function, &format!("DEFINED_IN {symbol}")),
            }),
        });
        functions.push((function, symbol));
    }

    let request = request_inputs::request_names(&bindings);
    let helpers = responses::flask_helpers(&bindings);
    let resolver = responses::Resolver {
        bindings: &bindings,
        locals: &locals,
    };
    for (function, symbol) in &functions {
        let top_level = function
            .child_by_field_name("name")
            .is_some_and(|name| locals.get(cx.text(&name)) == Some(symbol));
        if top_level
            && let Some(facts) =
                responses::function_facts(&cx, *function, helpers.as_ref(), &resolver)
        {
            out.function_facts.push(facts);
        }
    }
    auth_guards::collect(
        &cx, root, &bindings, &resolver, &locals, &functions, &mut out,
    );
    rate_limits::collect(&cx, root, &bindings, &functions, &mut out);
    flask_pages::collect(&cx, root, &functions, &mut out);
    for (function, handler) in &functions {
        let Some(parent) = function
            .parent()
            .filter(|parent| parent.kind() == "decorated_definition")
        else {
            continue;
        };
        let mut inputs = None;
        let mut returned = None;
        let mut walk = parent.walk();
        for decorator in parent
            .named_children(&mut walk)
            .filter(|child| child.kind() == "decorator")
        {
            let exposed = flask_admin::expose(&cx, root, decorator, *function, &bindings);
            let route = match exposed {
                Some(_) => None,
                None => route_decorator(&cx, decorator),
            };
            if exposed.is_none() && route.is_none() {
                continue;
            }
            let inputs = inputs
                .get_or_insert_with(|| {
                    request_inputs::handler_inputs(&cx, *function, &request).to_props(handler)
                })
                .clone();
            let responses = match &returned {
                Some(responses) => responses,
                None => {
                    let facts = helpers.as_ref().map(|helpers| {
                        let mut facts =
                            responses::handler_facts(&cx, *function, helpers, &resolver);
                        facts
                            .follows
                            .extend(responses::decorator_follows(&cx, parent, &resolver));
                        facts
                    });
                    let props = facts
                        .as_ref()
                        .and_then(|facts| facts.responses.to_props(handler));
                    if let Some(facts) = facts.filter(|facts| !facts.follows.is_empty()) {
                        out.handler_facts.push(responses::HandlerFacts {
                            handler: handler.clone(),
                            facts,
                        });
                    }
                    returned.insert(props)
                }
            }
            .clone();
            if let Some(exposed) = exposed {
                exposed.record(
                    &mut out,
                    handler,
                    cx.evidence(&decorator),
                    inputs,
                    responses,
                );
                continue;
            }
            let Some(route) = route else {
                continue;
            };
            let own_endpoint = match &route.endpoint {
                Prefix::Absent => function
                    .child_by_field_name("name")
                    .map(|name| cx.text(&name).to_string()),
                endpoint => literal(endpoint).map(str::to_string),
            };
            let receiver = cx.text(&route.receiver).to_string();
            if let Some(framework) = receivers.get(&receiver).copied() {
                out.endpoint_apps.push((
                    handler.clone(),
                    app_scope::app_ref(&cx, route.receiver, &bindings),
                ));
                for method in methods_for(framework, &route.verb, &route.listed_methods) {
                    push_endpoint(
                        &mut out,
                        id.repo,
                        &method,
                        &route.path,
                        handler,
                        framework,
                        &[cx.evidence(&decorator)],
                        ConfidenceTier::Confirmed,
                        None,
                        inputs.as_ref(),
                        responses.as_ref().filter(|_| framework == Framework::Flask),
                        endpoint_name(framework, Some(""), own_endpoint.as_deref()).as_deref(),
                    );
                }
                continue;
            }
            // A router defined here, or a receiver imported from a local
            // module: joined with its definition and mounts after the walk.
            let key = match bindings.get(&receiver) {
                Some(ImportBinding::Symbol {
                    target_file: Some(target),
                    exported,
                    ..
                }) => Some((target.clone(), exported.clone())),
                Some(_) => None,
                None if route.receiver.kind() == "identifier" => {
                    Some((path.to_string(), receiver.clone()))
                }
                None => None,
            };
            if let Some(key) = key {
                // The join below trusts the definition; a receiver its own
                // file may have rebound is not proven to be it (#1149).
                if app_scope::app_ref(&cx, route.receiver, &bindings) == app_scope::AppRef::Any {
                    out.endpoint_apps
                        .push((handler.clone(), app_scope::AppRef::Any));
                }
                out.pending_routes.push(PendingRoute {
                    receiver: key,
                    verb: route.verb,
                    path: route.path,
                    listed_methods: route.listed_methods,
                    handler: handler.clone(),
                    endpoint: own_endpoint,
                    evidence: cx.evidence(&decorator),
                    inputs,
                    responses,
                });
            }
        }
    }

    // Guards in test code are not business rules (#2009).
    if !target_tests::in_test_tree(cx.path) {
        rule_evidence::extract(&cx, root, &locals, &mut out);
    }

    let call_query =
        crate::queries::query("(call function: [(identifier) (attribute)] @callee) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(call_query, root, source);
    while let Some(found) = matches.next() {
        let mut call = None;
        let mut callee = None;
        for capture in found.captures() {
            match call_query.capture_names()[capture.index as usize] {
                "call" => call = Some(capture.node),
                "callee" => callee = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(call), Some(callee), Some(src)) = (
            call,
            callee,
            call.and_then(|call| enclosing_function(call, &functions_by_start)),
        ) else {
            continue;
        };
        let direct = callee
            .split_once('.')
            .is_none()
            .then(|| locals.get(&callee).cloned())
            .flatten();
        if let Some(dst) = direct {
            if src != dst {
                out.edges.push(Edge {
                    src,
                    dst,
                    label: "CALLS".into(),
                    props: serde_json::json!({
                        "prov": cx.prov(&call, &format!("CALLS {callee}")),
                    }),
                });
            }
            continue;
        }

        // `self.m()` in a method `C.f` is taken as a call of `C.m` when this
        // file defines that method. A subclass may
        // override `m`, so the edge is InferredStrong (#1998).
        if let Some(member) = callee.strip_prefix("self.")
            && !member.contains('.')
            && let Some((_, qualified)) = src.rsplit_once('#')
            && let Some((class, _)) = qualified.split_once('.')
            && !qualified[class.len() + 1..].contains('.')
        {
            let dst = symbol_id(id.repo, path, &format!("{class}.{member}"));
            if dst != src && functions_by_start.values().any(|symbol| *symbol == dst) {
                out.edges.push(Edge {
                    src: src.clone(),
                    dst: dst.clone(),
                    label: "CALLS".into(),
                    props: serde_json::json!({
                        "resolution": "self-method",
                        "prov": cx.prov_with_confidence(
                            &call,
                            ConfidenceTier::InferredStrong,
                            &format!("CALLS {src} -> {dst}"),
                        ),
                    }),
                });
            }
            continue;
        }
        // `from pkg import sub; sub.f()` is taken as a call of `f` in the
        // submodule `pkg/sub.py` when that file defines `f` (the edge is
        // kept only for a recovered Symbol). Whether the package binds a
        // `sub` of its own is not checked, so the edge is InferredStrong
        // (#1998).
        let mut submodule = false;
        let imported = if let Some((base, member)) = callee.split_once('.') {
            match bindings.get(base) {
                Some(ImportBinding::Module { module }) => module_file(path, module)
                    .map(|target| (target, member.to_string(), module.starts_with('.'))),
                Some(ImportBinding::Symbol {
                    module, exported, ..
                }) if !member.contains('.') && !module.is_empty() => {
                    let joined = if module.ends_with('.') {
                        format!("{module}{exported}")
                    } else {
                        format!("{module}.{exported}")
                    };
                    submodule = true;
                    module_file(path, &joined)
                        .map(|target| (target, member.to_string(), module.starts_with('.')))
                }
                _ => None,
            }
        } else {
            match bindings.get(&callee) {
                Some(ImportBinding::Symbol {
                    target_file,
                    exported,
                    relative,
                    ..
                }) => target_file
                    .clone()
                    .map(|target| (target, exported.clone(), *relative)),
                _ => None,
            }
        };
        let Some((target_file, exported, relative)) = imported else {
            continue;
        };
        let dst = symbol_id(id.repo, &target_file, &exported);
        let resolved = Edge {
            src: src.clone(),
            dst: dst.clone(),
            label: "CALLS".into(),
            props: if submodule {
                serde_json::json!({
                    "resolution": "submodule-attribute",
                    "prov": cx.prov_with_confidence(
                        &call,
                        ConfidenceTier::InferredStrong,
                        &format!("CALLS {src} -> {dst}"),
                    ),
                })
            } else {
                serde_json::json!({
                    "resolution": "directory-proven",
                    "prov": cx.prov(&call, &format!("CALLS {src} -> {dst}")),
                })
            },
        };
        let gap = (relative && !submodule).then(|| {
            let gap_id = format!("gap:call:{}@{}@{}", id.repo, path, call.start_byte());
            let node = Node {
                id: gap_id.clone(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "callee": callee,
                    "reason": "unresolved Python import target",
                    "attempted_tiers": ["T0"],
                    "prov": cx.prov_with_confidence(
                        &call,
                        ConfidenceTier::Gap,
                        &format!("Gap {gap_id}"),
                    ),
                }),
            };
            let edge = Edge {
                src,
                dst: gap_id.clone(),
                label: "CALLS".into(),
                props: serde_json::json!({
                    "reason": "unresolved Python import target",
                    "attempted_resolution": "directory-import",
                    "prov": cx.prov_with_confidence(
                        &call,
                        ConfidenceTier::Gap,
                        &format!("CALLS -> {gap_id}"),
                    ),
                }),
            };
            (node, edge)
        });
        out.pending_calls.push(PendingCall { resolved, gap });
    }

    Ok(out)
}

fn collect_python_files(root: &Path, out: &mut Vec<String>) -> Result<(), ExtractError> {
    let skip = |name: &str| {
        name.starts_with('.')
            || matches!(
                name,
                "__pycache__" | "venv" | "site-packages" | "node_modules" | "dist" | "build"
            )
    };
    for file in source_walk::files(root, &skip, source_walk::Gitignores::Honor)? {
        if file
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("py")
        {
            out.push(file.rel);
        }
    }
    Ok(())
}

/// Recover a Python directory with content-addressed per-file parse reuse.
pub fn extract_dir_incremental(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    extract_dir_incremental_with_progress(root, id, cache, &mut |_| {})
}

/// Same as [`extract_dir_incremental`], calling `on_file` with each file's
/// repo-relative path as it's read (#209 live progress hook).
pub fn extract_dir_incremental_with_progress(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
    on_file: &mut dyn FnMut(&str),
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    extract_dir_incremental_within(root, id, cache, on_file, PythonLimits::default())
}

/// [`extract_dir_incremental_with_progress`] under a project's `limits`
/// (#1653, AC-1805).
pub fn extract_dir_incremental_within(
    root: &Path,
    id: &SourceId<'_>,
    cache: &mut IncrementalCache,
    on_file: &mut dyn FnMut(&str),
    limits: PythonLimits,
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    let files = python_paths(root)?;
    extract_dir_with(
        root,
        id,
        &files,
        &|path| std::fs::read(root.join(path)),
        cache,
        on_file,
        &mut |_, _| {},
        limits,
    )
}

/// The `.py` files the ordinary walk selects, sorted, without reading them.
pub fn python_paths(root: &Path) -> Result<Vec<String>, ExtractError> {
    let mut files = Vec::new();
    collect_python_files(root, &mut files)?;
    files.sort();
    Ok(files)
}

/// Recover a Python directory whose `.py` sources are the retained bytes
/// `read` returns for each of `files` (AC-1024), instead of the checkout.
/// Each file's own extraction is handed to `on_parsed` before directory
/// resolution, so a caller can bind receipts to it; directory resolution
/// still reads the auxiliary inputs under `root` (templates, requirement
/// pins, Alembic migrations, module names), which stay outside any receipt.
pub fn extract_dir_from_sources(
    root: &Path,
    id: &SourceId<'_>,
    files: &[String],
    read: &(dyn Fn(&str) -> std::io::Result<Vec<u8>> + Sync),
    on_file: &mut dyn FnMut(&str),
    on_parsed: &mut dyn FnMut(&str, &Extraction),
) -> Result<Extraction, ExtractError> {
    extract_dir_from_sources_within(
        root,
        id,
        files,
        read,
        on_file,
        on_parsed,
        PythonLimits::default(),
    )
}

/// [`extract_dir_from_sources`] under a project's `limits` (#1653, AC-1805).
pub fn extract_dir_from_sources_within(
    root: &Path,
    id: &SourceId<'_>,
    files: &[String],
    read: &(dyn Fn(&str) -> std::io::Result<Vec<u8>> + Sync),
    on_file: &mut dyn FnMut(&str),
    on_parsed: &mut dyn FnMut(&str, &Extraction),
    limits: PythonLimits,
) -> Result<Extraction, ExtractError> {
    extract_dir_with(
        root,
        id,
        files,
        read,
        &mut IncrementalCache::default(),
        on_file,
        on_parsed,
        limits,
    )
    .map(|(extraction, _)| extraction)
}

#[allow(clippy::too_many_arguments)]
fn extract_dir_with(
    root: &Path,
    id: &SourceId<'_>,
    files: &[String],
    read: &(dyn Fn(&str) -> std::io::Result<Vec<u8>> + Sync),
    cache: &mut IncrementalCache,
    on_file: &mut dyn FnMut(&str),
    on_parsed: &mut dyn FnMut(&str, &Extraction),
    limits: PythonLimits,
) -> Result<(Extraction, IncrementalStats), ExtractError> {
    let active = files.iter().cloned().collect::<BTreeSet<_>>();
    let mut stats = IncrementalStats {
        deleted_files: cache
            .files
            .keys()
            .filter(|path| !active.contains(*path))
            .count() as u64,
        ..IncrementalStats::default()
    };
    cache.files.retain(|path, _| active.contains(path));
    let mut out = Extraction::default();
    // Files parse on parallel workers (#236); results merge here in sorted
    // order, so the output is byte-identical to a serial run.
    // Workers see only each cached file's hash; the merge owns the cache and
    // replaces entries one at a time, exactly as the serial loop did, so a
    // re-ingest never holds a second copy of the cache.
    let previous: std::collections::BTreeMap<String, String> = cache
        .files
        .iter()
        .map(|(path, cached)| (path.clone(), cached.source_hash.clone()))
        .collect();
    let merged = source_walk::parallel::map_ordered(
        files,
        |path| {
            let source = read(path)?;
            let source_hash = core_prov::content_hash(&source);
            let reusable = previous.get(path).is_some_and(|hash| *hash == source_hash);
            let fresh = if reusable {
                None
            } else {
                Some(extract_file(&source, path, id)?)
            };
            Ok::<_, ExtractError>((source_hash, fresh))
        },
        |path, (source_hash, fresh)| {
            on_file(path);
            let extraction = match fresh {
                Some(extraction) => {
                    stats.recomputed_files += 1;
                    extraction
                }
                None => {
                    stats.reused_files += 1;
                    let mut extraction = cache.files[path].extraction.clone();
                    retarget_commit(&mut extraction, id.commit);
                    extraction
                }
            };
            on_parsed(path, &extraction);
            cache.files.insert(
                path.to_string(),
                CachedFile {
                    source_hash,
                    extraction: extraction.clone(),
                },
            );
            out.nodes.extend(extraction.nodes);
            out.edges.extend(extraction.edges);
            out.pending_calls.extend(extraction.pending_calls);
            out.receiver_defs.extend(extraction.receiver_defs);
            out.pending_routes.extend(extraction.pending_routes);
            out.mounts.extend(extraction.mounts);
            out.orm_files.extend(extraction.orm_files);
            out.function_facts.extend(extraction.function_facts);
            out.handler_facts.extend(extraction.handler_facts);
            out.auth_handlers.extend(extraction.auth_handlers);
            out.fn_effects.extend(extraction.fn_effects);
            out.hooks.extend(extraction.hooks);
            out.wsgi_facts.extend(extraction.wsgi_facts);
            out.patched_guards.extend(extraction.patched_guards);
            out.client_fixtures.extend(extraction.client_fixtures);
            out.client_calls.extend(extraction.client_calls);
            out.app_calls.extend(extraction.app_calls);
            out.name_uses.extend(extraction.name_uses);
            out.endpoint_apps.extend(extraction.endpoint_apps);
            out.wild_mounts.extend(extraction.wild_mounts);
            out.module_attr_writes.extend(extraction.module_attr_writes);
            out.limiter_defs.extend(extraction.limiter_defs);
            out.limiter_breach.extend(extraction.limiter_breach);
            out.pending_limits.extend(extraction.pending_limits);
            out.limit_constants.extend(extraction.limit_constants);
            out.attribute_write_objects
                .extend(extraction.attribute_write_objects);
            // A pytest module runs under pytest unless the product imports
            // it, so its writes and references wait for such an import.
            if target_tests::is_test_file(path) || target_tests::is_conftest(path) {
                let file = path.rsplit('/').next().unwrap_or(path);
                out.pytest_modules.push((
                    file.strip_suffix(".py").unwrap_or(file).to_string(),
                    extraction.written_attributes,
                    extraction.member_references,
                ));
            } else {
                out.written_attributes.extend(extraction.written_attributes);
                out.member_references.extend(extraction.member_references);
            }
            out.package_names.extend(extraction.package_names);
            out.flask_admin.extend(extraction.flask_admin);
            out.page_facts.extend(extraction.page_facts);
            Ok(())
        },
    );
    merged?;
    // Count each pytest module a counted file imports, transitively.
    let mut pytest_modules = std::mem::take(&mut out.pytest_modules);
    while let Some(index) = pytest_modules
        .iter()
        .position(|(stem, _, _)| out.member_references.contains(stem))
    {
        let (_, written, references) = pytest_modules.swap_remove(index);
        out.written_attributes.extend(written);
        out.member_references.extend(references);
    }
    let known = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    for pending in std::mem::take(&mut out.pending_calls) {
        if known.contains(&pending.resolved.dst) {
            out.edges.push(pending.resolved);
        } else if let Some((node, edge)) = pending.gap {
            out.nodes.push(node);
            out.edges.push(edge);
        }
    }
    resolve_absolute_imports(&mut out, id.repo);
    flask_admin::resolve(
        &mut out,
        id.repo,
        root,
        id.commit,
        limits.max_lockfile_bytes,
    );
    resolve_routes(&mut out, id.repo);
    let functions = responses::resolve(&mut out, id.repo);
    rate_limits::resolve_proven(&mut out);
    auth_guards::resolve(&mut out, id.repo, &functions);
    target_tests::resolve(&mut out, id.repo);
    sqlalchemy::resolve(&mut out, id.repo);
    flask_pages::resolve(&mut out, id.repo, id.commit, Some(root));
    alembic::extract(
        root,
        id,
        &mut out,
        limits.max_alembic_revisions,
        limits.max_alembic_file_bytes,
    )?;
    let local_names = local_module_names(root)?;
    let Extraction { nodes, edges, .. } = &mut out;
    core_graph::placeholder::close_over_endpoints(
        nodes,
        edges,
        EXTRACTOR_ID,
        core_graph::placeholder::label_for_id,
        |endpoint, _| {
            if endpoint.starts_with(flask_admin_library::LIBRARY_SYMBOLS) {
                return Some(Boundary::External {
                    reason: "Flask-Admin view method of a release the repository pins".into(),
                    evidence: vec![],
                });
            }
            let module = endpoint.strip_prefix("mod:")?;
            Some(classify_import(module, &local_names))
        },
    );
    Ok((out, stats))
}

/// Recover a Python directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

#[cfg(test)]
mod tests;
