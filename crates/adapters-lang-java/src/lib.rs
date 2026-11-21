//! Java deterministic (T0) language adapter (SPEC-00 §3.3, #168).
//!
//! Classes, interfaces, enums, records, and methods become Symbols; imports
//! and import-proven cross-file calls become the server graph, with
//! unresolved project-local targets failing closed to explicit Gaps.
//! Annotation-proven Spring Web mappings become Endpoint/HANDLES facts with
//! class+method path composition. Adapters are per language, not per JDK
//! version: the grammar parses current syntax and anything it cannot prove
//! is simply not asserted. Import-proven JPA annotations become DataEntity
//! nodes with REFERENCES relations, and Spring Data repositories bind to
//! their entity with READS/WRITES from repository call sites (#239). This
//! tier never calls an LLM and every emitted fact carries exact source-span
//! provenance.

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

mod apache_http;
mod jaxrs;
mod jpa;
pub mod jvm;
mod overload;
mod queries;

use jvm::{classify_import, external_module_id, foreign_packages, in_system};

const EXTRACTOR_ID: &str = "t0.adapter-java";

/// Java extraction errors.
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

/// Identity of the source being recovered.
pub struct SourceId<'a> {
    /// Repository identity.
    pub repo: &'a str,
    /// Commit SHA, or `workdir` for an unversioned tree.
    pub commit: &'a str,
}

/// Java graph facts from one file or directory.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    /// Recovered nodes.
    pub nodes: Vec<Node>,
    /// Recovered edges.
    pub edges: Vec<Edge>,
    /// Retrofit client calls (#737), stitched by the `events` crate.
    pub fetch_sites: Vec<adapters_fw::client::FetchSite>,
    /// Retrofit builder base URLs, joined to `fetch_sites` repo-wide.
    retrofit_bases: Vec<adapters_fw::client::FetchBase>,
    /// RestTemplate (AC-0435) and WebClient (AC-0441) calls: kept out of the
    /// Retrofit base join and appended to `fetch_sites` after it.
    unjoined_sites: Vec<adapters_fw::client::FetchSite>,
    /// Spring Kafka producer and listener sites (AC-0459), stitched by the
    /// `events` crate.
    pub event_sites: Vec<adapters_fw::events::EventSite>,
    pending_calls: Vec<PendingCall>,
    declared_types: Vec<DeclaredType>,
    /// Package declarations, one per file that has one.
    packages: Vec<String>,
    /// Every declared method/constructor's owner type, name and arity,
    /// keyed for repo-wide overload resolution (#435).
    method_index: Vec<MethodSig>,
    /// JPA entities declared in the repository, keyed by class FQN (#239).
    entity_index: Vec<jpa::EntityDecl>,
    /// Spring Data repository interfaces, keyed by FQN (#239).
    repository_index: Vec<jpa::RepositoryDecl>,
    /// Relation and repository-binding edges awaiting their target entity.
    pending_links: Vec<jpa::PendingLink>,
    /// Repository call sites awaiting the receiver type's resolution.
    pending_repo_calls: Vec<jpa::PendingRepoCall>,
    /// Wildcard-proven JAX-RS endpoints awaiting same-package shadowing.
    pending_endpoints: Vec<jaxrs::Pending>,
    /// OpenFeign sites awaiting same-package shadowing of wildcard proofs.
    pending_feign: Vec<feign::Pending>,
    /// Fully qualified names of every top-level type a file declares,
    /// annotation types included: the package members that can shadow a
    /// wildcard import.
    package_types: Vec<String>,
    /// `System.getenv` sites awaiting the repo-wide `System` check (#1789).
    pending_config: Vec<adapters_fw::config_read::ConfigReadSite>,
    /// Some file declares or imports something named `System`.
    system_rebound: bool,
}

/// One declared method or constructor, indexed for overload resolution by
/// its owning type's fully qualified name (package-prefixed, matches
/// [`DeclaredType::fqn`] — a bare local qualified name would collide when
/// two packages declare a same-named type, #435), simple method name, and
/// erasure-level arity (#435).
#[derive(Debug, Clone)]
struct MethodSig {
    owner_fqn: String,
    name: String,
    /// Declared parameter count, excluding a trailing varargs parameter.
    min_arity: usize,
    /// The last declared parameter is varargs (`T...`): the method accepts
    /// `min_arity` or more call-site arguments, not exactly `min_arity`.
    variadic: bool,
    /// Shapes of the fixed parameters, for literal-kind narrowing (AC-0433).
    params: Vec<overload::ParamShape>,
    symbol: String,
}

/// Whether a call site's argument count could plausibly invoke a method
/// declared with `min_arity` fixed parameters (`variadic` false) or
/// `min_arity` fixed parameters plus a trailing varargs slot (`variadic`
/// true, which accepts `min_arity` or more) (#435).
fn arity_matches(min_arity: usize, variadic: bool, call_arity: usize) -> bool {
    if variadic {
        call_arity >= min_arity
    } else {
        call_arity == min_arity
    }
}

/// (owner fully qualified name, method name) -> every declared overload's
/// (min_arity, variadic, symbol id, fixed parameter shapes), repo-wide
/// (#435). Keyed by the owning type's package-prefixed FQN so two packages'
/// same-named types never share an overload set (#435 review).
type TypeMethodIndex<'a> =
    HashMap<(&'a str, &'a str), Vec<(usize, bool, &'a str, &'a [overload::ParamShape])>>;

/// Simple qualified name (no signature) -> every same-file overload's
/// (min_arity, variadic, full signature-qualified symbol id, fixed parameter
/// shapes) (#435, AC-0433).
type LocalMethodIndex = HashMap<String, Vec<(usize, bool, String, Vec<overload::ParamShape>)>>;

/// A call to an imported type, resolvable only with the whole directory in
/// view (the import names a FQN; only the repo-wide type index knows which
/// file declares it).
#[derive(Debug, Clone)]
struct PendingCall {
    src: String,
    fqn: String,
    method: String,
    /// Argument count at the call site, matched against the resolved
    /// type's overloads (#435). T0 does not type argument expressions.
    arity: usize,
    /// What each argument proves, for narrowing same-arity overloads.
    args: Vec<overload::ArgKind>,
    resolved_props: serde_json::Value,
    /// The FQN does not resolve to a type this repo declares exactly once.
    unresolved_gap: (Node, Edge),
    /// The FQN resolves, but no single declared overload matches the call
    /// by arity, or by argument kinds where arity leaves several (AC-0433).
    overload_gap: (Node, Edge),
}

/// A type declaration and the fully-qualified name it answers to.
#[derive(Debug, Clone)]
struct DeclaredType {
    fqn: String,
    path: String,
    qualified: String,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file Java parse cache.
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Physical Java source work performed by an incremental extraction.
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

    fn prov(&self, node: &TsNode<'_>, fact: &str) -> serde_json::Value {
        self.prov_with_confidence(node, ConfidenceTier::Confirmed, fact)
    }

    fn prov_with_confidence(
        &self,
        node: &TsNode<'_>,
        confidence: ConfidenceTier,
        fact: &str,
    ) -> serde_json::Value {
        self.prov_spanning(&[*node], confidence, fact)
    }

    /// Provenance citing every node that establishes the fact — e.g. a route
    /// Gap cites both the unprovable class base and the method mapping.
    fn prov_spanning(
        &self,
        nodes: &[TsNode<'_>],
        confidence: ConfidenceTier,
        fact: &str,
    ) -> serde_json::Value {
        let evidence = nodes
            .iter()
            .map(|node| EvidenceRef {
                repo: self.id.repo.into(),
                path: self.path.into(),
                byte_start: node.start_byte() as u64,
                byte_end: node.end_byte() as u64,
                commit_sha: self.id.commit.into(),
            })
            .collect();
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
}

fn file_id(repo: &str, path: &str) -> String {
    format!("file:{repo}@{path}")
}

fn symbol_id(repo: &str, path: &str, name: &str) -> String {
    format!("sym:{repo}@{path}#{name}")
}

/// The erasure-level declared type of each formal parameter of a method or
/// constructor, in source order (#435). This is not true bytecode erasure —
/// generics arguments and unresolved imports are kept exactly as spelled —
/// but it is enough to give overloaded methods distinct identity without
/// full type resolution, which T0 does not attempt.
fn param_type_list(cx: &FileCx<'_>, declaration: TsNode<'_>) -> Vec<String> {
    let Some(params) = declaration.child_by_field_name("parameters") else {
        return Vec::new();
    };
    let mut walk = params.walk();
    params
        .named_children(&mut walk)
        .filter_map(|param| match param.kind() {
            "formal_parameter" => param
                .child_by_field_name("type")
                .map(|ty| cx.text(&ty).to_string()),
            "spread_parameter" => {
                let mut inner = param.walk();
                param
                    .named_children(&mut inner)
                    .find(|child| {
                        !matches!(
                            child.kind(),
                            "modifiers"
                                | "annotation"
                                | "marker_annotation"
                                | "variable_declarator"
                        )
                    })
                    .map(|ty| format!("{}...", cx.text(&ty)))
            }
            _ => None,
        })
        .collect()
}

/// Overload signature suffix appended to a method's qualified name, e.g.
/// `(String,int)` — parenthesized, comma-joined, empty for a no-arg method
/// or constructor (#435).
fn signature_suffix(param_types: &[String]) -> String {
    format!("({})", param_types.join(","))
}

/// Argument count of a call site (`method_invocation`'s `arguments` field),
/// the first overload-selection signal (#435); literal argument kinds narrow
/// further when arity leaves several overloads (AC-0433).
fn call_arity(call: TsNode<'_>) -> usize {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return 0;
    };
    let mut walk = arguments.walk();
    arguments
        .named_children(&mut walk)
        .filter(|argument| !is_comment(*argument))
        .count()
}

fn retarget_props_commit(props: &mut serde_json::Value, commit: &str) {
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

fn retarget_commit(extraction: &mut Extraction, commit: &str) {
    for node in &mut extraction.nodes {
        retarget_props_commit(&mut node.props, commit);
        if node.label == "BusinessRule" {
            rule_evidence::retarget(&mut node.props, commit);
        }
    }
    for edge in &mut extraction.edges {
        retarget_props_commit(&mut edge.props, commit);
    }
    for pending in &mut extraction.pending_calls {
        retarget_props_commit(&mut pending.resolved_props, commit);
        retarget_props_commit(&mut pending.unresolved_gap.0.props, commit);
        retarget_props_commit(&mut pending.unresolved_gap.1.props, commit);
        retarget_props_commit(&mut pending.overload_gap.0.props, commit);
        retarget_props_commit(&mut pending.overload_gap.1.props, commit);
    }
    for pending in &mut extraction.pending_endpoints {
        pending.retarget(|props| retarget_props_commit(props, commit));
    }
    jpa::retarget(extraction, commit);
}

const TYPE_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
];

/// The dot-joined chain of enclosing type names (outermost first) for a
/// node, e.g. `Outer.Inner` for a method inside a nested class.
fn enclosing_type_chain(cx: &FileCx<'_>, mut node: TsNode<'_>) -> Vec<String> {
    let mut chain = Vec::new();
    while let Some(parent) = node.parent() {
        if TYPE_KINDS.contains(&parent.kind())
            && let Some(name) = parent.child_by_field_name("name")
        {
            chain.push(cx.text(&name).to_string());
        }
        node = parent;
    }
    chain.reverse();
    chain
}

/// One file's import surface: named bindings (simple name → FQN) plus
/// wildcard-imported packages. Static imports are ignored — a statically
/// imported method call is not provable to a type in v1.
#[derive(Debug, Default)]
struct Imports {
    bindings: HashMap<String, String>,
    wildcard_packages: Vec<String>,
}

fn parse_imports(cx: &FileCx<'_>, root: TsNode<'_>, out: &mut Extraction) -> Imports {
    let query = crate::queries::query("(import_declaration) @import");
    let mut imports = Imports::default();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    while let Some(found) = matches.next() {
        let statement = found.captures()[0].node;
        let raw = cx.text(&statement).replace(['\n', '\\'], " ");
        let Some(rest) = raw.trim().strip_prefix("import") else {
            continue;
        };
        let rest = rest.trim().trim_end_matches(';').trim();
        let (is_static, rest) = match rest.strip_prefix("static ") {
            Some(tail) => (true, tail.trim()),
            None => (false, rest),
        };
        let (module, wildcard) = match rest.strip_suffix(".*") {
            Some(package) => (package.to_string(), true),
            None => (rest.to_string(), false),
        };
        if module.is_empty() {
            continue;
        }
        if !is_static {
            if wildcard {
                imports.wildcard_packages.push(module.clone());
            } else if let Some((_, simple)) = module.rsplit_once('.') {
                imports.bindings.insert(simple.to_string(), module.clone());
            }
        }
        out.edges.push(Edge {
            src: file_id(cx.id.repo, cx.path),
            dst: format!("mod:{module}"),
            label: "IMPORTS".into(),
            props: serde_json::json!({
                "specifier": module,
                "prov": cx.prov(&statement, &format!("IMPORTS {}", cx.text(&statement))),
            }),
        });
    }
    imports
}

/// Annotations attached to a declaration via its `modifiers` child:
/// `(simple name, annotation node)` pairs.
fn declaration_annotations<'t>(
    cx: &FileCx<'_>,
    declaration: TsNode<'t>,
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
            let simple = cx.text(&name);
            let simple = simple.rsplit('.').next().unwrap_or(simple).to_string();
            found.push((simple, annotation));
        }
    }
    found
}

/// A mapping annotation's path argument, three-way — the Kotlin adapter's
/// model (#234). "No argument" and "argument present but unprovable" are
/// different facts: an absent path is Spring's documented default (`""`),
/// while a present-but-dynamic one (a constant, a concatenation, any
/// non-literal expression) is a runtime identity T0 cannot confirm — it must
/// fail closed, never collapse to the default.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathArg {
    /// No path-designating argument: Spring defaults the path to `""`.
    Absent,
    /// One or more provable string literals; Spring maps each of them.
    Literal(Vec<String>),
    /// A path argument exists but is not a provable literal.
    Dynamic,
}

impl PathArg {
    /// The path segments this argument designates, or `None` when unprovable.
    fn segments(self) -> Option<Vec<String>> {
        match self {
            PathArg::Absent => Some(vec![String::new()]),
            PathArg::Literal(paths) => Some(paths),
            PathArg::Dynamic => None,
        }
    }
}

/// A single-line string literal whose source spelling is its runtime value.
/// Escapes (`-`, `\t`, …) are not decoded: the source bytes would not
/// be the route, so such literals fail closed rather than confirm a spelling
/// Spring never maps. Text blocks are rejected for the same reason — the
/// compiler re-indents their content.
fn string_literal(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string_literal" {
        return None;
    }
    let mut walk = node.walk();
    if node
        .named_children(&mut walk)
        .any(|child| child.kind() != "string_fragment")
    {
        return None;
    }
    let text = cx.text(&node).trim_matches('"');
    // Unicode escapes are resolved before lexing, so the grammar may not
    // surface them as `escape_sequence` nodes; a backslash in the source
    // spelling is never a literal route character.
    (!text.contains('\\')).then(|| text.to_string())
}

fn is_comment(node: TsNode<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

/// A path element value: one literal, or an array initializer whose every
/// element is a literal (`{ "/a", "/b" }`; an empty `{}` is the default path).
fn path_value(cx: &FileCx<'_>, value: TsNode<'_>) -> PathArg {
    if value.kind() != "element_value_array_initializer" {
        return match string_literal(cx, value) {
            Some(path) => PathArg::Literal(vec![path]),
            None => PathArg::Dynamic,
        };
    }
    let mut walk = value.walk();
    let paths: Option<Vec<String>> = value
        .named_children(&mut walk)
        .filter(|element| !is_comment(*element))
        .map(|element| string_literal(cx, element))
        .collect();
    match paths {
        Some(paths) if paths.is_empty() => PathArg::Absent,
        Some(paths) => PathArg::Literal(paths),
        None => PathArg::Dynamic,
    }
}

/// The path argument of a mapping annotation: a bare (positional) value,
/// which Java binds to `value`, or a `value =` / `path =` pair. Other named
/// elements (`produces = …`) do not designate a path.
fn annotation_path(cx: &FileCx<'_>, annotation: TsNode<'_>) -> PathArg {
    let Some(arguments) = annotation.child_by_field_name("arguments") else {
        return PathArg::Absent;
    };
    let mut walk = arguments.walk();
    for argument in arguments.named_children(&mut walk) {
        if is_comment(argument) {
            continue;
        }
        if argument.kind() != "element_value_pair" {
            return path_value(cx, argument);
        }
        let (Some(key), Some(value)) = (
            argument.child_by_field_name("key"),
            argument.child_by_field_name("value"),
        ) else {
            continue;
        };
        if matches!(cx.text(&key), "value" | "path") {
            return path_value(cx, value);
        }
    }
    PathArg::Absent
}

/// The exact Spring package each recognized annotation lives in. Proof is
/// per annotation, not per vendor: a wildcard of one Spring package must
/// never prove an annotation from another (#170 review, AC-0080).
fn spring_annotation_package(name: &str) -> Option<&'static str> {
    match name {
        "RestController" | "RequestMapping" | "GetMapping" | "PostMapping" | "PutMapping"
        | "DeleteMapping" | "PatchMapping" => Some("org.springframework.web.bind.annotation"),
        "Controller" => Some("org.springframework.stereotype"),
        _ => None,
    }
}

/// A Spring annotation is proven only by its import: a named import of
/// exactly `{declaring package}.{name}`, or a wildcard import of exactly
/// its declaring package. Lookalikes — including same-named annotations
/// from other packages, Spring or not — prove nothing.
fn spring_proven(name: &str, imports: &Imports) -> bool {
    let Some(package) = spring_annotation_package(name) else {
        return false;
    };
    if let Some(fqn) = imports.bindings.get(name) {
        return fqn == &format!("{package}.{name}");
    }
    imports
        .wildcard_packages
        .iter()
        .any(|wildcard| wildcard == package)
}

/// How one spelled Spring annotation is proven (#1288): `None` when it is
/// not; `Some(true)` when only a wildcard import of its package proves it,
/// which a same-package type of the same simple name obscures (JLS §6.4.1),
/// so the directory join settles it; `Some(false)` when a named import or
/// the exact qualified spelling proves it outright. A qualified spelling of
/// any other type is a lookalike, whatever the imports say.
fn spring_proof(
    cx: &FileCx<'_>,
    annotation: TsNode<'_>,
    name: &str,
    imports: &Imports,
) -> Option<bool> {
    let package = spring_annotation_package(name)?;
    let spelling = annotation
        .child_by_field_name("name")
        .map(|spelled| cx.text(&spelled))
        .unwrap_or_default();
    if spelling.contains('.') {
        return (spelling == format!("{package}.{name}")).then_some(false);
    }
    spring_proven(name, imports).then(|| !imports.bindings.contains_key(name))
}

/// The `method =` restriction of one `@RequestMapping` (#1598): each proven
/// `RequestMethod.X`, sorted and de-duplicated, and empty when the element
/// is absent or `{}` (no restriction). `None` when any named verb is
/// unprovable (a static import, a constant or a lookalike `RequestMethod`),
/// so the caller emits a Gap. The flag is whether a verb's `RequestMethod`
/// rests on a wildcard import alone, which a same-package type of that name
/// obscures (JLS §6.4.1). Spring combines the class and method levels by
/// union, and an empty union serves every method (`ALL`, the TS adapter's
/// convention); see `RequestMethodsRequestCondition::combine`.
fn spring_request_methods(
    cx: &FileCx<'_>,
    annotation: TsNode<'_>,
    imports: &Imports,
) -> Option<(BTreeSet<String>, bool)> {
    let Some(value) = retrofit::element(cx, annotation, "method") else {
        return Some((BTreeSet::new(), false));
    };
    let constants: Vec<TsNode<'_>> = if value.kind() == "element_value_array_initializer" {
        let mut walk = value.walk();
        value
            .named_children(&mut walk)
            .filter(|element| !matches!(element.kind(), "line_comment" | "block_comment"))
            .collect()
    } else {
        vec![value]
    };
    let qualified = "org.springframework.web.bind.annotation.RequestMethod";
    let mut verbs = BTreeSet::new();
    let mut wildcard = false;
    for constant in &constants {
        if constant.kind() != "field_access" {
            return None;
        }
        let object = constant.child_by_field_name("object")?;
        let field = constant.child_by_field_name("field")?;
        let spelling: String = cx
            .text(&object)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let proven_wildcard = if spelling == qualified {
            false
        } else if spelling == "RequestMethod" {
            match imports.bindings.get("RequestMethod") {
                Some(bound) if bound == qualified => false,
                Some(_) => return None,
                None if imports
                    .wildcard_packages
                    .iter()
                    .any(|package| package == "org.springframework.web.bind.annotation") =>
                {
                    true
                }
                None => return None,
            }
        } else {
            return None;
        };
        let verb = cx.text(&field);
        if !matches!(
            verb,
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "TRACE"
        ) {
            return None;
        }
        wildcard |= proven_wildcard;
        verbs.insert(verb.to_string());
    }
    Some((verbs, wildcard))
}

const SPRING_SHADOWED: &str =
    "Spring annotation proven only by a wildcard import is shadowed by a same-package type";

/// A Spring route Gap anchored at the method mapping, the last evidence node.
fn spring_route_gap(
    cx: &FileCx<'_>,
    evidence: &[TsNode<'_>],
    handler: &str,
    http_method: &str,
    reason: &str,
) -> (Node, Edge) {
    let anchor = evidence.last().expect("a Gap cites at least one node");
    let gap_id = format!(
        "gap:route:{}@{}@{}",
        cx.id.repo,
        cx.path,
        anchor.start_byte()
    );
    let node = Node {
        id: gap_id.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "method": http_method,
            "handler_sym": handler,
            "framework": "spring",
            "language": "java",
            "reason": reason,
            "attempted_tiers": ["T0"],
            "prov": cx.prov_spanning(evidence, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
        }),
    };
    let edge = Edge {
        src: gap_id.clone(),
        dst: handler.to_string(),
        label: "HANDLES".into(),
        props: serde_json::json!({
            "reason": reason,
            "attempted_resolution": "literal-path",
            "prov": cx.prov_spanning(
                evidence,
                ConfidenceTier::Gap,
                &format!("HANDLES {gap_id} -> {handler}"),
            ),
        }),
    };
    (node, edge)
}

/// `<file package>.<Name>`: the type that would obscure a wildcard proof.
fn same_package_type(package: Option<&str>, name: &str) -> String {
    match package {
        Some(package) => format!("{package}.{name}"),
        None => name.to_string(),
    }
}

fn mapping_method(annotation_name: &str) -> Option<&'static str> {
    match annotation_name {
        "GetMapping" => Some("GET"),
        "PostMapping" => Some("POST"),
        "PutMapping" => Some("PUT"),
        "DeleteMapping" => Some("DELETE"),
        "PatchMapping" => Some("PATCH"),
        _ => None,
    }
}

/// Compose a class-level base with a method-level path (AC-0206, #445).
/// Segments meet at exactly one `/`, but a trailing slash spelled in the
/// source is kept (as one `/`): Spring 6 matches `/a` and `/a/` as distinct
/// routes, so `"/api/"` + `""` is `/api/` and `"/api"` + `"/"` is `/api/`.
fn join_route(base: &str, tail: &str) -> String {
    let route = if tail.is_empty() {
        base.to_string()
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            tail.trim_start_matches('/')
        )
    };
    if route.ends_with('/') || route.is_empty() {
        format!("{}/", route.trim_end_matches('/'))
    } else {
        route
    }
}

/// Retarget each `IMPORTS` edge whose complete target this repository
/// declares exactly once to the declaring `File` — the same shape as a
/// resolved TS relative import. The target is proven when it is a declared
/// type (nested types included), or a member whose `Symbol` the declaring
/// type defines. A prefix alone never proves it (`a.Foo.Missing` is not
/// `a.Foo`): unproven targets keep their `mod:` id and are classified by
/// [`classify_import`].
fn resolve_repo_imports(
    edges: &mut [Edge],
    repo: &str,
    types_by_fqn: &BTreeMap<&str, Option<&DeclaredType>>,
    known_symbols: &HashSet<String>,
) {
    for edge in edges {
        if edge.label != "IMPORTS" {
            continue;
        }
        let Some(module) = edge.dst.strip_prefix("mod:") else {
            continue;
        };
        let declared = types_by_fqn.get(module).copied().flatten().or_else(|| {
            let (owner, member) = module.rsplit_once('.')?;
            let owner = types_by_fqn.get(owner).copied().flatten()?;
            // A nested type member matches its symbol id exactly; a method
            // member now carries an erasure-level signature suffix the
            // import statement itself never names (#435), so a static
            // method import proves the member exists via a prefix match —
            // "some overload of this name is declared" — rather than a
            // specific overload, which only a call site's arity can prove.
            let target = symbol_id(repo, &owner.path, &format!("{}.{member}", owner.qualified));
            known_symbols
                .iter()
                .any(|symbol| *symbol == target || symbol.starts_with(&format!("{target}(")))
                .then_some(owner)
        });
        if let Some(declared) = declared {
            edge.dst = file_id(repo, &declared.path);
            edge.props["resolution"] = "import-proven".into();
        }
    }
}

/// Recover deterministic facts from one Java source file. Facts a directory
/// join would otherwise settle are settled within the file alone: its
/// RestTemplate sites are fetch sites like any other, and its wildcard-proven
/// endpoints and Feign sites are settled against the types this file declares.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
) -> Result<Extraction, ExtractError> {
    let mut out = extract_file(source, path, id)?;
    let unjoined_sites = std::mem::take(&mut out.unjoined_sites);
    out.fetch_sites.extend(unjoined_sites);
    let package_types: BTreeSet<String> = out.package_types.iter().cloned().collect();
    jaxrs::resolve(
        std::mem::take(&mut out.pending_endpoints),
        |fqn| package_types.contains(fqn),
        &mut out.nodes,
        &mut out.edges,
    );
    let feign_sites = feign::resolve(std::mem::take(&mut out.pending_feign), |fqn| {
        package_types.contains(fqn)
    });
    out.fetch_sites.extend(feign_sites);
    config_read::resolve(
        id,
        std::mem::take(&mut out.pending_config),
        out.system_rebound,
        &mut out.nodes,
        &mut out.edges,
    );
    Ok(out)
}

/// One file's facts, with the facts the directory join settles held back.
/// The file's `package` clause, if any.
fn java_package(cx: &FileCx<'_>, root: TsNode<'_>) -> Option<String> {
    let query =
        crate::queries::query("(package_declaration [(scoped_identifier) (identifier)] @package)");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    matches
        .next()
        .map(|found| cx.text(&found.captures()[0].node).to_string())
}

/// Top-level type names (`package.Name`, or `Name` in the default package)
/// declared by the product Java sources under `root`, sorted. In a mixed
/// JVM tree they are same-package declarations that can obscure a Kotlin
/// star import (#1288). Test source sets are not compiled with the product,
/// so they are skipped.
pub fn product_top_level_types(root: &Path) -> Result<Vec<String>, ExtractError> {
    let mut files = Vec::new();
    collect_java_files(root, &mut files)?;
    files.sort();
    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let id = SourceId {
        repo: "",
        commit: "",
    };
    let mut out = Vec::new();
    for path in files
        .iter()
        .filter(|path| !adapters_fw::in_jvm_test_source_set(path))
    {
        let source = std::fs::read(root.join(path))?;
        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| ExtractError::NoTree(path.clone()))?;
        let cx = FileCx {
            source: &source,
            path,
            id: &id,
        };
        let root = tree.root_node();
        out.extend(jaxrs::top_level_types(
            &cx,
            root,
            java_package(&cx, root).as_deref(),
        ));
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn extract_file(source: &[u8], path: &str, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
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
            "language": "java",
            "prov": cx.prov(&root, &format!("File {path}")),
        }),
    });

    let package = java_package(&cx, root);
    out.packages.extend(package.clone());
    let imports = parse_imports(&cx, root, &mut out);

    // Types: classes, interfaces, enums, records — nested chains included.
    // Built from TYPE_KINDS, so compiled once here rather than through the
    // static-pattern cache.
    static TYPE_QUERY: std::sync::LazyLock<Query> = std::sync::LazyLock::new(|| {
        let clauses = TYPE_KINDS
            .iter()
            .map(|kind| format!("({kind} name: (identifier) @name) @decl"))
            .collect::<Vec<_>>()
            .join(" ");
        Query::new(&tree_sitter_java::LANGUAGE.into(), &format!("[{clauses}]"))
            .expect("static query")
    });
    let type_query: &Query = &TYPE_QUERY;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(type_query, root, source);
    while let Some(found) = matches.next() {
        let (mut decl, mut name) = (None, None);
        for capture in found.captures() {
            match type_query.capture_names()[capture.index as usize] {
                "decl" => decl = Some(capture.node),
                "name" => name = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(decl), Some(name)) = (decl, name) else {
            continue;
        };
        let mut chain = enclosing_type_chain(&cx, decl);
        chain.push(name.clone());
        let qualified = chain.join(".");
        let symbol = symbol_id(id.repo, path, &qualified);
        let kind = decl.kind().trim_end_matches("_declaration");
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": qualified,
                "kind": kind,
                "language": "java",
                "prov": cx.prov(&decl, &format!("Symbol {symbol}")),
            }),
        });
        out.edges.push(Edge {
            src: symbol.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&decl, &format!("DEFINED_IN {symbol}")),
            }),
        });
        if let Some(package) = &package {
            out.declared_types.push(DeclaredType {
                fqn: format!("{package}.{qualified}"),
                path: path.to_string(),
                qualified,
            });
        }
    }

    // Methods and constructors, qualified by their enclosing type chain.
    let method_query = crate::queries::query(
        "[(method_declaration name: (identifier) @name) @method
          (constructor_declaration name: (identifier) @name) @method]",
    );
    let mut methods_by_start: HashMap<usize, String> = HashMap::new();
    let mut local_method_index: LocalMethodIndex = HashMap::new();
    let mut methods = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(method_query, root, source);
    while let Some(found) = matches.next() {
        let (mut method, mut name) = (None, None);
        for capture in found.captures() {
            match method_query.capture_names()[capture.index as usize] {
                "method" => method = Some(capture.node),
                "name" => name = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(method), Some(name)) = (method, name) else {
            continue;
        };
        let chain = enclosing_type_chain(&cx, method);
        if chain.is_empty() {
            continue;
        }
        let owner_qualified = chain.join(".");
        let simple_qualified = format!("{owner_qualified}.{name}");
        // Package-prefixed, matching `DeclaredType::fqn` exactly, so the
        // repo-wide overload index never merges two packages' same-named
        // types (#435 review). Local (same-file) resolution below still
        // uses the bare `owner_qualified`/`simple_qualified` — package
        // ambiguity cannot arise within a single file.
        let owner_fqn = match &package {
            Some(package) => format!("{package}.{owner_qualified}"),
            None => owner_qualified.clone(),
        };
        let param_types = param_type_list(&cx, method);
        let variadic = param_types.last().is_some_and(|ty| ty.ends_with("..."));
        let min_arity = if variadic {
            param_types.len() - 1
        } else {
            param_types.len()
        };
        let params = overload::param_shapes(source, method);
        let qualified = format!("{simple_qualified}{}", signature_suffix(&param_types));
        let symbol = symbol_id(id.repo, path, &qualified);
        methods_by_start.insert(method.start_byte(), symbol.clone());
        local_method_index
            .entry(simple_qualified)
            .or_default()
            .push((min_arity, variadic, symbol.clone(), params.clone()));
        out.method_index.push(MethodSig {
            owner_fqn,
            name: name.clone(),
            min_arity,
            variadic,
            params,
            symbol: symbol.clone(),
        });
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": qualified,
                "kind": "method",
                "language": "java",
                "prov": cx.prov(&method, &format!("Symbol {symbol}")),
            }),
        });
        out.edges.push(Edge {
            src: symbol.clone(),
            dst: file_id(id.repo, path),
            label: "DEFINED_IN".into(),
            props: serde_json::json!({
                "prov": cx.prov(&method, &format!("DEFINED_IN {symbol}")),
            }),
        });
        methods.push((method, symbol, chain, name));
    }

    // Guarded returns and throws as business rules (#2020).
    rule_evidence::extract(&cx, root, &methods_by_start, &mut out);

    // Retrofit client calls (#737): interface methods now, base URL at the
    // directory join.
    out.fetch_sites
        .extend(retrofit::sites(&cx, &methods, &imports));
    out.retrofit_bases
        .extend(retrofit::bases(&cx, root, &imports));

    // Configuration reads (#1789, AC-0597) wait for the repo-wide check.
    let (config_sites, system_rebound) = config_read::sites(&cx, root, &methods_by_start);
    out.pending_config.extend(config_sites);
    out.system_rebound |= system_rebound;

    // Endpoints are product facts: a test source set yields none (#1250).
    let product = !adapters_fw::in_jvm_test_source_set(path);
    // RestTemplate (AC-0435) and WebClient (AC-0441) client calls are
    // product facts too, and never take a Retrofit base.
    if product {
        out.unjoined_sites
            .extend(rest_template::sites(&cx, root, &imports, &methods_by_start));
        out.unjoined_sites
            .extend(webclient::sites(&cx, root, &imports, &methods_by_start));
        // JDK `java.net.http` request builders (AC-0475).
        out.unjoined_sites
            .extend(http_request::sites(&cx, root, &imports, &methods_by_start));
        // Apache HttpClient 4 and 5 request constructors (AC-0478).
        out.unjoined_sites
            .extend(apache_http::sites(&cx, root, &imports, &methods_by_start));
        // Square OkHttp request builders (AC-0476).
        out.unjoined_sites
            .extend(okhttp::sites(&cx, root, &imports, &methods_by_start));
        // Spring Kafka producers and listeners (AC-0459).
        out.event_sites.extend(spring_kafka::sites(
            &cx,
            root,
            &imports,
            &methods,
            &methods_by_start,
        ));
        // OpenFeign clients (AC-0437) wait for the same-package check.
        out.pending_feign.extend(feign::sites(
            &cx,
            root,
            &methods,
            &imports,
            package.as_deref(),
        ));
    }

    // Spring Web endpoints: annotation-proven controllers, class-level
    // @RequestMapping base path, method-level @{Get,Post,...}Mapping.
    for (method, handler, chain, _) in methods.iter().filter(|_| product) {
        if method.kind() != "method_declaration" {
            continue;
        }
        let Some(class_decl) = ({
            let mut node = *method;
            let mut found = None;
            while let Some(parent) = node.parent() {
                if parent.kind() == "class_declaration" {
                    found = Some(parent);
                    break;
                }
                node = parent;
            }
            found
        }) else {
            continue;
        };
        let class_annotations = declaration_annotations(&cx, class_decl);
        // Same-package types that would obscure a wildcard-only proof.
        let mut shadowed_by: BTreeSet<Vec<String>> = BTreeSet::new();
        // Any one proven stereotype makes the class a controller: a direct
        // proof dominates, and wildcard-only proofs fail closed only when a
        // same-package type shadows every one of them.
        let controllers: Vec<(&str, bool)> = class_annotations
            .iter()
            .filter(|(name, _)| matches!(name.as_str(), "RestController" | "Controller"))
            .filter_map(|(name, annotation)| {
                spring_proof(&cx, *annotation, name, &imports)
                    .map(|wildcard| (name.as_str(), wildcard))
            })
            .collect();
        if controllers.is_empty() {
            continue;
        }
        if controllers.iter().all(|(_, wildcard)| *wildcard) {
            let group: BTreeSet<String> = controllers
                .iter()
                .map(|(controller, _)| same_package_type(package.as_deref(), controller))
                .collect();
            shadowed_by.insert(group.into_iter().collect());
        }
        // "No @RequestMapping" and "@RequestMapping with no path argument"
        // both mean Spring's default base (""); a present-but-dynamic base
        // poisons every mapping under it (None), failing them closed.
        let base_mapping = class_annotations.iter().find_map(|(name, node)| {
            (name == "RequestMapping")
                .then(|| spring_proof(&cx, *node, name, &imports))
                .flatten()
                .map(|wildcard| (*node, wildcard))
        });
        if let Some((_, true)) = base_mapping {
            shadowed_by.insert(vec![same_package_type(
                package.as_deref(),
                "RequestMapping",
            )]);
        }
        let base_mapping = base_mapping.map(|(node, _)| node);
        // The class-level `method =` restriction every handler inherits
        // (union semantics); `None` when it cannot be proven.
        let class_methods = match base_mapping {
            None => Some((BTreeSet::new(), false)),
            Some(node) => spring_request_methods(&cx, node, &imports),
        };
        if let Some((_, true)) = class_methods {
            shadowed_by.insert(vec![same_package_type(package.as_deref(), "RequestMethod")]);
        }
        let bases = match base_mapping {
            None => Some(vec![String::new()]),
            Some(node) => annotation_path(&cx, node).segments(),
        };
        for (name, annotation) in declaration_annotations(&cx, *method) {
            // A method-level `@RequestMapping` binds a handler as much as the
            // verb shorthands do (#1598); its verbs come from `method =`.
            let request_mapping = name == "RequestMapping";
            if !request_mapping && mapping_method(&name).is_none() {
                continue;
            }
            let Some(wildcard) = spring_proof(&cx, annotation, &name, &imports) else {
                continue;
            };
            let mut shadowed_by = shadowed_by.clone();
            if wildcard {
                shadowed_by.insert(vec![same_package_type(package.as_deref(), &name)]);
            }
            let method_methods = if request_mapping {
                spring_request_methods(&cx, annotation, &imports)
            } else {
                mapping_method(&name).map(|verb| (BTreeSet::from([verb.to_string()]), false))
            };
            let (Some((class_verbs, _)), Some((mut verbs, method_wildcard))) =
                (class_methods.as_ref(), method_methods)
            else {
                // A proven handler whose verb T0 cannot prove, at either
                // level, is a route Gap, never a guessed method (R-INT-4).
                let evidence: Vec<TsNode<'_>> = match (class_methods.is_none(), base_mapping) {
                    (true, Some(base)) => vec![base, annotation],
                    _ => vec![annotation],
                };
                let gap = spring_route_gap(
                    &cx,
                    &evidence,
                    handler,
                    "?",
                    "unresolved Spring request method",
                );
                if shadowed_by.is_empty() {
                    out.nodes.push(gap.0);
                    out.edges.push(gap.1);
                } else {
                    // A shadowed binding, not the verb, is then what blocks.
                    let shadowed = spring_route_gap(&cx, &evidence, handler, "?", SPRING_SHADOWED);
                    out.pending_endpoints.push(jaxrs::Pending::new(
                        shadowed_by.iter().cloned().collect(),
                        gap,
                        shadowed,
                    ));
                }
                continue;
            };
            if method_wildcard {
                shadowed_by.insert(vec![same_package_type(package.as_deref(), "RequestMethod")]);
            }
            verbs.extend(class_verbs.iter().cloned());
            if verbs.is_empty() {
                verbs.insert("ALL".into());
            }
            // Gaps are one per mapping annotation (their id is its span), so
            // a multi-verb mapping names all of its verbs on one Gap.
            let gap_method = verbs
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("|");
            let http_method = gap_method.as_str();
            let tails = annotation_path(&cx, annotation).segments();
            let (Some(bases), Some(tails)) = (bases.as_ref(), tails) else {
                // The mapping is proven but its route is a runtime identity
                // (constant/expression path, on the method or the class
                // base). T0 cannot confirm the route, so the endpoint is an
                // explicit Gap, never a default path presented as Confirmed
                // (R-INT-4).
                // Cite what made the route unprovable: the class base when it
                // is dynamic, and always the method mapping that binds the
                // handler, so the evidence shows the actual expression.
                let evidence: Vec<TsNode<'_>> = match (bases.is_none(), base_mapping) {
                    (true, Some(base)) => vec![base, annotation],
                    _ => vec![annotation],
                };
                let dynamic = spring_route_gap(
                    &cx,
                    &evidence,
                    handler,
                    http_method,
                    "dynamic Spring mapping path",
                );
                if shadowed_by.is_empty() {
                    out.nodes.push(dynamic.0);
                    out.edges.push(dynamic.1);
                } else {
                    // A shadowed binding, not the path, is then what blocks.
                    let shadowed =
                        spring_route_gap(&cx, &evidence, handler, http_method, SPRING_SHADOWED);
                    out.pending_endpoints.push(jaxrs::Pending::new(
                        shadowed_by.iter().cloned().collect(),
                        dynamic,
                        shadowed,
                    ));
                }
                continue;
            };
            // Sorted and de-duplicated so emission order never depends on
            // source order (determinism); `{ "/a", "/a/" }` stays two routes.
            let routes: BTreeSet<String> = bases
                .iter()
                .flat_map(|base| tails.iter().map(move |tail| join_route(base, tail)))
                .collect();
            // A composed route depends on the class base as much as on the
            // method mapping, so both are cited (R-INT-1 traceability).
            let evidence: Vec<TsNode<'_>> = base_mapping
                .into_iter()
                .chain(std::iter::once(annotation))
                .collect();
            // A wildcard-only proof waits for the directory join, where a
            // same-package type of that name turns the route into a Gap.
            let shadow_gap = (!shadowed_by.is_empty())
                .then(|| spring_route_gap(&cx, &evidence, handler, http_method, SPRING_SHADOWED));
            // One endpoint per proven verb and route.
            for (http_method, route) in verbs.iter().flat_map(|verb| {
                routes
                    .iter()
                    .map(move |route| (verb.as_str(), route.clone()))
            }) {
                let endpoint = format!("ep:{}@{http_method}:{route}", id.repo);
                let node = Node {
                    id: endpoint.clone(),
                    label: "Endpoint".into(),
                    props: serde_json::json!({
                        "method": http_method,
                        "path": route,
                        "handler_sym": handler,
                        "framework": "spring",
                        "language": "java",
                        "prov": cx.prov_spanning(
                            &evidence,
                            ConfidenceTier::Confirmed,
                            &format!("Endpoint {endpoint}"),
                        ),
                    }),
                };
                let edge = Edge {
                    src: endpoint.clone(),
                    dst: handler.clone(),
                    label: "HANDLES".into(),
                    props: serde_json::json!({
                        "prov": cx.prov_spanning(
                            &evidence,
                            ConfidenceTier::Confirmed,
                            &format!("HANDLES {endpoint} -> {handler}"),
                        ),
                    }),
                };
                match &shadow_gap {
                    None => {
                        out.nodes.push(node);
                        out.edges.push(edge);
                    }
                    Some(gap) => out.pending_endpoints.push(jaxrs::Pending::new(
                        shadowed_by.iter().cloned().collect(),
                        (node, edge),
                        gap.clone(),
                    )),
                }
            }
        }
        let _ = chain;
    }

    // JAX-RS resource endpoints (#1257): class `@Path` plus method HTTP
    // annotations; what T0 cannot place is a route Gap.
    let (jaxrs_nodes, jaxrs_edges, jaxrs_pending) = jaxrs::endpoints(
        &cx,
        if product { &methods } else { &[] },
        &imports,
        package.as_deref(),
    );
    out.nodes.extend(jaxrs_nodes);
    out.edges.extend(jaxrs_edges);
    out.pending_endpoints.extend(jaxrs_pending);
    // Test-only types are not compiled with the product, so they never shadow
    // a product file's wildcard import.
    if product {
        out.package_types
            .extend(jaxrs::top_level_types(&cx, root, package.as_deref()));
    }

    // Calls: same-class unqualified/this calls resolve locally; calls on an
    // imported type resolve repo-wide at the directory join, failing closed
    // to an explicit Gap when the project-local target cannot be proven.
    let call_query = crate::queries::query("(method_invocation name: (identifier) @name) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(call_query, root, source);
    while let Some(found) = matches.next() {
        let (mut call, mut name) = (None, None);
        for capture in found.captures() {
            match call_query.capture_names()[capture.index as usize] {
                "call" => call = Some(capture.node),
                "name" => name = Some(cx.text(&capture.node).to_string()),
                _ => {}
            }
        }
        let (Some(call), Some(name)) = (call, name) else {
            continue;
        };
        let Some(src) = ({
            let mut node = call;
            let mut found = None;
            while let Some(parent) = node.parent() {
                if let Some(symbol) = methods_by_start.get(&parent.start_byte())
                    && matches!(
                        parent.kind(),
                        "method_declaration" | "constructor_declaration"
                    )
                {
                    found = Some(symbol.clone());
                    break;
                }
                node = parent;
            }
            found
        }) else {
            continue;
        };
        let object = call
            .child_by_field_name("object")
            .map(|object| cx.text(&object).to_string());
        match object.as_deref() {
            None | Some("this") => {
                let chain = enclosing_type_chain(&cx, call);
                if chain.is_empty() {
                    continue;
                }
                let qualified = format!("{}.{name}", chain.join("."));
                let Some(candidates) = local_method_index.get(&qualified) else {
                    continue;
                };
                let call_arity = call_arity(call);
                let matches: Vec<(&str, &[overload::ParamShape])> = candidates
                    .iter()
                    .filter(|(min_arity, variadic, _, _)| {
                        arity_matches(*min_arity, *variadic, call_arity)
                    })
                    .map(|(_, _, symbol, params)| (symbol.as_str(), params.as_slice()))
                    .collect();
                let narrowed = overload::narrow(&matches, &overload::arg_kinds(source, call));
                let target = match matches.as_slice() {
                    [(dst, _)] => Some(*dst),
                    _ => narrowed,
                };
                match target {
                    Some(dst) => {
                        let dst = dst.to_string();
                        if dst != src {
                            let mut props = serde_json::json!({
                                "prov": cx.prov(&call, &format!("CALLS {qualified}")),
                            });
                            if narrowed.is_some() {
                                props["overload_resolution"] = overload::RESOLUTION.into();
                            }
                            out.edges.push(Edge {
                                src,
                                dst,
                                label: "CALLS".into(),
                                props,
                            });
                        }
                    }
                    None => {
                        // Zero local overloads accept this call's argument
                        // count, or several do and its argument kinds leave
                        // zero or several (AC-0433): fail closed to an
                        // explicit Gap rather than guess which overload
                        // runs (#435).
                        let reason = if matches.is_empty() {
                            "no local overload accepts this call's argument count"
                        } else {
                            "ambiguous overload: multiple local methods accept this argument count"
                        };
                        let gap_id =
                            format!("gap:overload:{}@{}@{}", id.repo, path, call.start_byte());
                        out.nodes.push(Node {
                            id: gap_id.clone(),
                            label: "Gap".into(),
                            props: serde_json::json!({
                                "callee": qualified,
                                "reason": reason,
                                "attempted_tiers": ["T0"],
                                "prov": cx.prov_with_confidence(
                                    &call,
                                    ConfidenceTier::Gap,
                                    &format!("Gap {gap_id}"),
                                ),
                            }),
                        });
                        out.edges.push(Edge {
                            src,
                            dst: gap_id.clone(),
                            label: "CALLS".into(),
                            props: serde_json::json!({
                                "reason": reason,
                                "attempted_resolution": "overload-arity",
                                "prov": cx.prov_with_confidence(
                                    &call,
                                    ConfidenceTier::Gap,
                                    &format!("CALLS -> {gap_id}"),
                                ),
                            }),
                        });
                    }
                }
            }
            Some(object_text) => {
                let Some(fqn) = imports.bindings.get(object_text) else {
                    continue;
                };
                let callee = format!("{object_text}.{name}");
                let call_arity = call_arity(call);
                let unresolved_gap_id =
                    format!("gap:call:{}@{}@{}", id.repo, path, call.start_byte());
                let unresolved_gap_node = Node {
                    id: unresolved_gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "callee": callee,
                        "reason": "unresolved Java import target",
                        "attempted_tiers": ["T0"],
                        "prov": cx.prov_with_confidence(
                            &call,
                            ConfidenceTier::Gap,
                            &format!("Gap {unresolved_gap_id}"),
                        ),
                    }),
                };
                let unresolved_gap_edge = Edge {
                    src: src.clone(),
                    dst: unresolved_gap_id.clone(),
                    label: "CALLS".into(),
                    props: serde_json::json!({
                        "reason": "unresolved Java import target",
                        "attempted_resolution": "import-fqn",
                        "prov": cx.prov_with_confidence(
                            &call,
                            ConfidenceTier::Gap,
                            &format!("CALLS -> {unresolved_gap_id}"),
                        ),
                    }),
                };
                // The FQN may resolve to a declared type whose overloads
                // still can't pick a unique arity match — a second gap
                // variant for that case, built here (not at directory join)
                // since only the per-file parse has the call's provenance
                // span (#435).
                let overload_gap_id =
                    format!("gap:overload:{}@{}@{}", id.repo, path, call.start_byte());
                let overload_gap_node = Node {
                    id: overload_gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "callee": callee,
                        "reason": "no unique overload proves this call's argument count",
                        "attempted_tiers": ["T0"],
                        "prov": cx.prov_with_confidence(
                            &call,
                            ConfidenceTier::Gap,
                            &format!("Gap {overload_gap_id}"),
                        ),
                    }),
                };
                let overload_gap_edge = Edge {
                    src: src.clone(),
                    dst: overload_gap_id.clone(),
                    label: "CALLS".into(),
                    props: serde_json::json!({
                        "reason": "no unique overload proves this call's argument count",
                        "attempted_resolution": "overload-arity",
                        "prov": cx.prov_with_confidence(
                            &call,
                            ConfidenceTier::Gap,
                            &format!("CALLS -> {overload_gap_id}"),
                        ),
                    }),
                };
                out.pending_calls.push(PendingCall {
                    src,
                    fqn: fqn.clone(),
                    method: name,
                    arity: call_arity,
                    args: overload::arg_kinds(source, call),
                    resolved_props: serde_json::json!({
                        "resolution": "import-proven",
                        "prov": cx.prov(&call, &format!("CALLS {callee}")),
                    }),
                    unresolved_gap: (unresolved_gap_node, unresolved_gap_edge),
                    overload_gap: (overload_gap_node, overload_gap_edge),
                });
            }
        }
    }

    jpa::extract(
        &cx,
        root,
        &imports,
        package.as_deref(),
        &methods_by_start,
        &mut out,
    );

    Ok(out)
}

fn collect_java_files(root: &Path, out: &mut Vec<String>) -> Result<(), ExtractError> {
    let skip = |name: &str| {
        name.starts_with('.')
            || matches!(
                name,
                "target" | "build" | "out" | "node_modules" | "dist" | "generated"
            )
    };
    for file in source_walk::files(root, &skip, source_walk::Gitignores::Honor)? {
        if file
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("java")
        {
            out.push(file.rel);
        }
    }
    Ok(())
}

/// Recover a Java directory with content-addressed per-file parse reuse.
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
    let mut files = Vec::new();
    collect_java_files(root, &mut files)?;
    files.sort();
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
        &files,
        |path| {
            let source = std::fs::read(root.join(path))?;
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
            out.declared_types.extend(extraction.declared_types);
            out.packages.extend(extraction.packages);
            out.fetch_sites.extend(extraction.fetch_sites);
            out.retrofit_bases.extend(extraction.retrofit_bases);
            out.unjoined_sites.extend(extraction.unjoined_sites);
            out.event_sites.extend(extraction.event_sites);
            out.method_index.extend(extraction.method_index);
            out.entity_index.extend(extraction.entity_index);
            out.repository_index.extend(extraction.repository_index);
            out.pending_links.extend(extraction.pending_links);
            out.pending_repo_calls.extend(extraction.pending_repo_calls);
            out.pending_endpoints.extend(extraction.pending_endpoints);
            out.pending_feign.extend(extraction.pending_feign);
            out.package_types.extend(extraction.package_types);
            out.pending_config.extend(extraction.pending_config);
            out.system_rebound |= extraction.system_rebound;
            Ok(())
        },
    );
    merged?;
    adapters_fw::client::join_repo_base(
        &mut out.fetch_sites,
        &out.retrofit_bases,
        retrofit::LIBRARY,
    );
    let unjoined_sites = std::mem::take(&mut out.unjoined_sites);
    out.fetch_sites.extend(unjoined_sites);

    // Directory join: an imported FQN resolves only to a type this repo
    // declares exactly once — a duplicate FQN (the same class in two source
    // roots or modules) is ambiguous and fails closed to a Gap instead of
    // silently picking whichever file sorts last (#170 review). A call on a
    // declared-package import that cannot be proven is an explicit Gap; a
    // call on a foreign package asserts nothing, while the import itself
    // closes over a proven external boundary (#237, `jvm::classify_import`).
    let mut types_by_fqn: BTreeMap<&str, Option<&DeclaredType>> = BTreeMap::new();
    for declared in &out.declared_types {
        types_by_fqn
            .entry(declared.fqn.as_str())
            .and_modify(|unique| *unique = None)
            .or_insert(Some(declared));
    }
    // Every package this repository declares — Kotlin sources included, so
    // a mixed JVM tree never mistakes its own Kotlin package for external.
    let mut repo_packages: BTreeSet<String> = out.packages.iter().cloned().collect();
    let foreign = foreign_packages(root, &["kt", "kts"])?;
    repo_packages.extend(foreign.packages);
    let known = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    let mut type_method_index: TypeMethodIndex = HashMap::new();
    for method in &out.method_index {
        type_method_index
            .entry((method.owner_fqn.as_str(), method.name.as_str()))
            .or_default()
            .push((
                method.min_arity,
                method.variadic,
                method.symbol.as_str(),
                method.params.as_slice(),
            ));
    }
    let package_types: BTreeSet<&str> = out.package_types.iter().map(String::as_str).collect();
    jaxrs::resolve(
        std::mem::take(&mut out.pending_endpoints),
        |fqn| package_types.contains(fqn),
        &mut out.nodes,
        &mut out.edges,
    );
    // Feign sites carry their own base, so they follow the Retrofit join.
    let feign_sites = feign::resolve(std::mem::take(&mut out.pending_feign), |fqn| {
        package_types.contains(fqn)
    });
    out.fetch_sites.extend(feign_sites);
    config_read::resolve(
        id,
        std::mem::take(&mut out.pending_config),
        out.system_rebound,
        &mut out.nodes,
        &mut out.edges,
    );
    for pending in std::mem::take(&mut out.pending_calls) {
        let declared = types_by_fqn.get(pending.fqn.as_str()).copied().flatten();
        match declared {
            Some(declared) => {
                let candidates = type_method_index
                    .get(&(declared.fqn.as_str(), pending.method.as_str()))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let matches: Vec<(&str, &[overload::ParamShape])> = candidates
                    .iter()
                    .filter(|(min_arity, variadic, _, _)| {
                        arity_matches(*min_arity, *variadic, pending.arity)
                    })
                    .map(|(_, _, symbol, params)| (*symbol, *params))
                    .collect();
                let narrowed = overload::narrow(&matches, &pending.args);
                let target = match matches.as_slice() {
                    [(dst, _)] => Some(*dst),
                    _ => narrowed,
                };
                match target {
                    // The declared type's own Symbol nodes were just
                    // indexed above, so a match is always `known` — no
                    // need to re-check membership like the old code did.
                    Some(dst) => {
                        let mut props = pending.resolved_props;
                        if narrowed.is_some() {
                            props["overload_resolution"] = overload::RESOLUTION.into();
                        }
                        out.edges.push(Edge {
                            src: pending.src,
                            dst: dst.to_string(),
                            label: "CALLS".into(),
                            props,
                        });
                    }
                    None => {
                        let (node, edge) = pending.overload_gap;
                        out.nodes.push(node);
                        out.edges.push(edge);
                    }
                }
            }
            None => {
                if in_system(&pending.fqn, &repo_packages) {
                    let (node, edge) = pending.unresolved_gap;
                    out.nodes.push(node);
                    out.edges.push(edge);
                }
            }
        }
    }
    resolve_repo_imports(&mut out.edges, id.repo, &types_by_fqn, &known);
    jpa::resolve(&mut out);
    // #464 (ADR-0033): an import proven external is re-keyed on its package
    // id rather than the imported type/member — `mod:jakarta.persistence`,
    // not `mod:jakarta.persistence.Entity` — with the original target kept
    // verbatim on the edge's `specifier`. Classification must use the full
    // path first: the capitalization split alone cannot tell an in-system
    // prefix from a proven-external one.
    for edge in &mut out.edges {
        if edge.label != "IMPORTS" {
            continue;
        }
        let Some(module) = edge.dst.strip_prefix("mod:") else {
            continue;
        };
        if matches!(
            classify_import(module, &repo_packages, foreign.complete),
            core_graph::placeholder::Boundary::External { .. }
        ) {
            edge.dst = external_module_id(module);
        }
    }
    let Extraction { nodes, edges, .. } = &mut out;
    core_graph::placeholder::close_over_endpoints(
        nodes,
        edges,
        EXTRACTOR_ID,
        core_graph::placeholder::label_for_id,
        |endpoint, _| {
            let module = endpoint.strip_prefix("mod:")?;
            Some(classify_import(module, &repo_packages, foreign.complete))
        },
    );
    Ok((out, stats))
}

/// Recover a Java directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

mod config_read;
mod feign;
mod http_request;
mod okhttp;
mod rest_template;
mod retrofit;
mod rule_evidence;
mod spring_kafka;
mod webclient;

#[cfg(test)]
mod tests;
