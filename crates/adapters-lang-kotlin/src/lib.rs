//! Kotlin deterministic (T0) language adapter (SPEC-00 §3.3, #212).
//!
//! Classes, interfaces, objects, data classes, enums, and functions
//! (top-level, member, and extension) become Symbols; imports and
//! import-proven cross-file calls become the server graph, with unresolved
//! project-local targets failing closed to explicit Gaps. Annotation-proven
//! Spring Web mappings become Endpoint/HANDLES facts with class+method path
//! composition (Ktor's routing DSL is a follow-on, not v1). Adapters are per
//! language, not per compiler version: the grammar parses current syntax and
//! anything it cannot prove is simply not asserted. This tier never calls an
//! LLM and every emitted fact carries exact source-span provenance.

use adapters_lang_java::jvm::{classify_import, external_module_id, foreign_packages, in_system};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

const EXTRACTOR_ID: &str = "t0.adapter-kotlin";

/// Kotlin extraction errors.
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

/// Kotlin graph facts from one file or directory.
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
    /// Android Navigation 3 entries and back-stack navigations, resolved repo-wide.
    navigation3: navigation3::Pending,
    /// Hilt `@Binds` declarations, resolved repo-wide at the directory join.
    hilt_binds: Vec<hilt_binds::PendingBind>,
    pending_calls: Vec<PendingCall>,
    /// Calls on declared-type receivers (#1048), resolved at the join.
    typed_calls: Vec<TypedCall>,
    /// Functions with a declared return type, as the FQNs that type can
    /// denote (W11c-2: a view model factory's `create(…): VM`).
    function_returns: Vec<(String, Vec<String>)>,
    declared_types: Vec<Declared>,
    declared_functions: Vec<Declared>,
    /// Further import targets (#721): extension functions, top-level
    /// properties, type aliases, annotation classes and enum entries. They
    /// prove only which file an import names, never a call target.
    declared_importables: Vec<Declared>,
    /// Package headers, one per file that has one.
    packages: Vec<String>,
}

/// A call proven by an import, resolvable only with the whole directory in
/// view (the import names a FQN; only the repo-wide index knows which file
/// declares it). `member` is `Some` for a call on an imported type/object
/// and `None` for a call to an imported top-level function.
#[derive(Debug, Clone)]
struct PendingCall {
    src: String,
    fqn: String,
    member: Option<String>,
    resolved_props: serde_json::Value,
    gap: (Node, Edge),
}

/// `x.m(…)` where `x`'s declaration states its type (#1048). `candidates`
/// are the FQNs that type name can denote, in Kotlin's precedence order
/// (explicit import, same package, star imports); the first one this repo
/// declares decides. An unproven type or member asserts nothing: it may be
/// inherited, an extension, or a library type.
#[derive(Debug, Clone)]
struct TypedCall {
    src: String,
    candidates: Vec<String>,
    /// Bound by a `T(…)` initializer: `T` must be a constructible class and
    /// not also a top-level function.
    constructed: bool,
    /// `Some(method)`: the receiver's type is `method`'s declared return
    /// type on the candidate type (a `hiltViewModel` factory callback).
    via: Option<String>,
    member: String,
    resolved_props: serde_json::Value,
    /// An unqualified call in a `with(x) { … }` lambda: what it resolves to
    /// when `x`'s type does not declare the member.
    fallback: Option<Fallback>,
    /// Top-level FQNs that, if this repository declares one (a function, a
    /// callable property or a type), shadow `kotlin.with`: the call then
    /// takes its fallback.
    shadowed_by: Vec<String>,
}

#[derive(Debug, Clone)]
enum Fallback {
    Edge(Edge),
    Pending(PendingCall),
}

/// A declaration (type/object or top-level function) and the
/// fully-qualified name it answers to.
#[derive(Debug, Clone)]
struct Declared {
    fqn: String,
    path: String,
    qualified: String,
    /// A class or data class, whose `Name(...)` can be a constructor call.
    /// An object's or an interface's is a call to an `invoke` operator.
    constructible: bool,
    /// An `interface` or `abstract class` declaration.
    interface_or_abstract: bool,
    /// An `object` or `data object` declaration.
    object: bool,
    /// Candidate-FQN lists for each direct `delegation_specifier` user type.
    supers: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
struct CachedFile {
    source_hash: String,
    extraction: Extraction,
}

/// Reusable per-file Kotlin parse cache.
#[derive(Debug, Default)]
pub struct IncrementalCache {
    files: BTreeMap<String, CachedFile>,
}

/// Physical Kotlin source work performed by an incremental extraction.
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

fn file_id(repo: &str, path: &str) -> String {
    format!("file:{repo}@{path}")
}

fn symbol_id(repo: &str, path: &str, name: &str) -> String {
    format!("sym:{repo}@{path}#{name}")
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
    }
    for edge in &mut extraction.edges {
        retarget_props_commit(&mut edge.props, commit);
    }
    for pending in &mut extraction.pending_calls {
        retarget_props_commit(&mut pending.resolved_props, commit);
        retarget_props_commit(&mut pending.gap.0.props, commit);
        retarget_props_commit(&mut pending.gap.1.props, commit);
    }
    for typed in &mut extraction.typed_calls {
        retarget_props_commit(&mut typed.resolved_props, commit);
        match &mut typed.fallback {
            Some(Fallback::Edge(edge)) => retarget_props_commit(&mut edge.props, commit),
            Some(Fallback::Pending(pending)) => {
                retarget_props_commit(&mut pending.resolved_props, commit);
                retarget_props_commit(&mut pending.gap.0.props, commit);
                retarget_props_commit(&mut pending.gap.1.props, commit);
            }
            None => {}
        }
    }
    extraction.navigation3.retarget(commit);
    hilt_binds::retarget(&mut extraction.hilt_binds, commit);
}

/// Type-declaration kinds that contribute to a member's qualified name.
/// `companion_object` is deliberately transparent: `companion object { fun
/// make() }` inside `Outer` is called as `Outer.make()`, so the companion
/// contributes nothing to the chain.
const TYPE_KINDS: &[&str] = &["class_declaration", "object_declaration"];

/// The dot-joined chain of enclosing type names (outermost first) for a
/// node, e.g. `Outer.Inner` for a function inside a nested class.
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

/// One file's import surface: named bindings (simple name or alias → FQN)
/// plus wildcard-imported packages.
#[derive(Debug, Default)]
struct Imports {
    bindings: HashMap<String, String>,
    wildcard_packages: Vec<String>,
}

fn parse_imports(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    language: &tree_sitter::Language,
    out: &mut Extraction,
) -> Imports {
    let query = Query::new(language, "(import) @import").expect("static query");
    let mut imports = Imports::default();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    while let Some(found) = matches.next() {
        let statement = found.captures()[0].node;
        let raw = cx.text(&statement).replace(['\n', '\\'], " ");
        let Some(rest) = raw.trim().strip_prefix("import") else {
            continue;
        };
        let rest = rest.trim().trim_end_matches(';').trim();
        let (rest, alias) = match rest.rsplit_once(" as ") {
            Some((module, alias)) => (module.trim(), Some(alias.trim().to_string())),
            None => (rest, None),
        };
        let (module, wildcard) = match rest.strip_suffix(".*") {
            Some(package) => (package.to_string(), true),
            None => (rest.to_string(), false),
        };
        if module.is_empty() {
            continue;
        }
        if wildcard {
            imports.wildcard_packages.push(module.clone());
        } else {
            let simple = alias.unwrap_or_else(|| {
                module
                    .rsplit_once('.')
                    .map(|(_, simple)| simple.to_string())
                    .unwrap_or_else(|| module.clone())
            });
            imports.bindings.insert(simple, module.clone());
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

/// A mapping annotation's path argument, three-way. "No argument" and
/// "argument present but unprovable" are different facts: an absent path
/// is Spring's own documented default (`""`), while a present-but-dynamic
/// one (a `$` template, a constant reference, any non-literal expression)
/// is a runtime identity T0 cannot confirm — it must fail closed, never
/// collapse to the default.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathArg {
    /// No path-designating argument: Spring defaults the path to `""`.
    Absent,
    /// A provable string literal.
    Literal(String),
    /// A path argument exists but is not a provable literal.
    Dynamic,
}

/// One recognized annotation use: simple name, the node carrying its
/// evidence span, and its path argument.
struct AnnotationUse<'t> {
    simple: String,
    node: TsNode<'t>,
    path: PathArg,
}

/// A string literal with no interpolation — `"/api/users"` yes,
/// `"/api/$version"` no. Templates are runtime identities, not asserted.
/// The grammar only materializes an `interpolation` node for `${expr}`;
/// a bare `$name` template is lexed as plain `string_content` pieces, so
/// any `$` in the text also fails closed.
fn pure_string_literal(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string_literal" {
        return None;
    }
    let mut walk = node.walk();
    if node
        .named_children(&mut walk)
        .any(|child| child.kind() != "string_content")
    {
        return None;
    }
    let text = cx.text(&node).trim_matches('"').to_string();
    if text.contains('$') {
        return None;
    }
    Some(text)
}

/// The path argument of a mapping annotation's `value_arguments`: a bare
/// (positional) argument, or a `value =` / `path =` pair. A found argument
/// that is not a pure string literal is [`PathArg::Dynamic`] — present but
/// unprovable; other named arguments (`produces = …`, `method = …`) do not
/// designate a path and leave it [`PathArg::Absent`].
fn arguments_path(cx: &FileCx<'_>, arguments: TsNode<'_>) -> PathArg {
    let mut walk = arguments.walk();
    for argument in arguments.named_children(&mut walk) {
        if argument.kind() != "value_argument" {
            continue;
        }
        let children: Vec<TsNode<'_>> = {
            let mut inner = argument.walk();
            argument.named_children(&mut inner).collect()
        };
        match children.as_slice() {
            [value] => {
                return match pure_string_literal(cx, *value) {
                    Some(literal) => PathArg::Literal(literal),
                    None => PathArg::Dynamic,
                };
            }
            [key, value] if key.kind() == "identifier" => {
                if matches!(cx.text(key), "value" | "path") {
                    return match pure_string_literal(cx, *value) {
                        Some(literal) => PathArg::Literal(literal),
                        None => PathArg::Dynamic,
                    };
                }
            }
            _ => {}
        }
    }
    PathArg::Absent
}

/// Interpret one grammar `annotation` node: the simple name comes from the
/// last segment of its `user_type`, the path argument from its
/// `constructor_invocation` arguments when present.
fn read_annotation<'t>(cx: &FileCx<'_>, annotation: TsNode<'t>) -> Option<AnnotationUse<'t>> {
    let mut walk = annotation.walk();
    for child in annotation.named_children(&mut walk) {
        match child.kind() {
            "user_type" => {
                let simple = cx.text(&child);
                let simple = simple.rsplit('.').next().unwrap_or(simple).trim();
                return Some(AnnotationUse {
                    simple: simple.to_string(),
                    node: annotation,
                    path: PathArg::Absent,
                });
            }
            "constructor_invocation" => {
                let mut inner = child.walk();
                let mut simple = None;
                let mut path = PathArg::Absent;
                for part in child.named_children(&mut inner) {
                    match part.kind() {
                        "user_type" => {
                            let name = cx.text(&part);
                            simple =
                                Some(name.rsplit('.').next().unwrap_or(name).trim().to_string());
                        }
                        "value_arguments" => path = arguments_path(cx, part),
                        _ => {}
                    }
                }
                return simple.map(|simple| AnnotationUse {
                    simple,
                    node: annotation,
                    path,
                });
            }
            _ => {}
        }
    }
    None
}

/// Annotations attached to a declaration via its `modifiers` child.
fn declaration_annotations<'t>(cx: &FileCx<'_>, declaration: TsNode<'t>) -> Vec<AnnotationUse<'t>> {
    let mut found = Vec::new();
    let mut walk = declaration.walk();
    for child in declaration.named_children(&mut walk) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut inner = child.walk();
        for annotation in child.named_children(&mut inner) {
            if annotation.kind() != "annotation" {
                continue;
            }
            if let Some(read) = read_annotation(cx, annotation) {
                found.push(read);
            }
        }
    }
    found
}

/// Grammar-quirk recovery (verify-at-build, #212): depending on what
/// follows the class in the same file, `tree-sitter-kotlin-ng` sometimes
/// resolves class-level annotations into a *preceding sibling*
/// `annotated_expression` statement — `@RequestMapping("/x")` becomes an
/// annotation wrapping a parenthesized string expression — instead of the
/// class's own `modifiers`. The chain shape is unambiguous, so recover it:
/// each nesting level contributes one annotation, and a terminal
/// `(parenthesized_expression (string_literal))` is the innermost
/// annotation's path argument. Proof standards are unchanged — the
/// recovered names still need the Spring import to assert anything.
fn preceding_expression_annotations<'t>(
    cx: &FileCx<'_>,
    declaration: TsNode<'t>,
) -> Vec<AnnotationUse<'t>> {
    let mut found = Vec::new();
    let mut sibling = declaration.prev_named_sibling();
    while let Some(candidate) = sibling {
        if candidate.kind() != "annotated_expression" {
            break;
        }
        let mut level = candidate;
        loop {
            let mut next_level = None;
            let mut walk = level.walk();
            for child in level.named_children(&mut walk) {
                match child.kind() {
                    "annotation" => {
                        if let Some(read) = read_annotation(cx, child) {
                            found.push(read);
                        }
                    }
                    "annotated_expression" => next_level = Some(child),
                    "parenthesized_expression" => {
                        // The wrapped expression is the innermost
                        // annotation's argument: a pure literal is its
                        // path, anything else is a present-but-dynamic
                        // argument that must not collapse to the default.
                        let literal = {
                            let mut inner = child.walk();
                            child
                                .named_children(&mut inner)
                                .find_map(|expr| pure_string_literal(cx, expr))
                        };
                        if let Some(last) = found.last_mut()
                            && last.path == PathArg::Absent
                        {
                            last.path = match literal {
                                Some(literal) => PathArg::Literal(literal),
                                None => PathArg::Dynamic,
                            };
                        }
                    }
                    _ => {}
                }
            }
            match next_level {
                Some(next) => level = next,
                None => break,
            }
        }
        sibling = candidate.prev_named_sibling();
    }
    found
}

/// The exact Spring package each recognized annotation lives in. Proof is
/// per annotation, not per vendor: a wildcard of one Spring package must
/// never prove an annotation from another (#170 review, AC-0080 standard).
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

/// The declaration's kind, in Kotlin's own words: `class`, `interface`,
/// `object`, `data class`, `enum class`, `annotation class`.
fn declaration_kind(cx: &FileCx<'_>, declaration: TsNode<'_>) -> &'static str {
    if declaration.kind() == "object_declaration" {
        return "object";
    }
    let mut walk = declaration.walk();
    for child in declaration.children(&mut walk) {
        if child.kind() == "interface" {
            return "interface";
        }
        if child.kind() == "modifiers" {
            let mut inner = child.walk();
            for modifier in child.named_children(&mut inner) {
                if modifier.kind() == "class_modifier" {
                    match cx.text(&modifier) {
                        "data" => return "data class",
                        "enum" => return "enum class",
                        "annotation" => return "annotation class",
                        _ => {}
                    }
                }
            }
        }
    }
    "class"
}

/// Whether a `class_declaration` carries the `abstract` inheritance modifier.
pub(crate) fn is_abstract(cx: &FileCx<'_>, declaration: TsNode<'_>) -> bool {
    if declaration.kind() != "class_declaration" {
        return false;
    }
    let mut walk = declaration.walk();
    for child in declaration.named_children(&mut walk) {
        if child.kind() != "modifiers" {
            continue;
        }
        let mut inner = child.walk();
        for modifier in child.named_children(&mut inner) {
            if modifier.kind() == "inheritance_modifier" && cx.text(&modifier) == "abstract" {
                return true;
            }
        }
    }
    false
}

/// Candidate-FQN lists for each direct `delegation_specifier` user type of a
/// class or interface declaration.
fn declaration_supers(
    cx: &FileCx<'_>,
    declaration: TsNode<'_>,
    imports: &Imports,
    package: Option<&str>,
    local_types: &HashSet<String>,
) -> Vec<Vec<String>> {
    let mut supers = Vec::new();
    let mut walk = declaration.walk();
    for child in declaration.named_children(&mut walk) {
        if child.kind() != "delegation_specifiers" {
            continue;
        }
        let mut inner = child.walk();
        for spec in child.named_children(&mut inner) {
            if spec.kind() != "delegation_specifier" {
                continue;
            }
            let Some(first) = spec.named_child(0) else {
                continue;
            };
            let user_type = match first.kind() {
                "user_type" => Some(first),
                "constructor_invocation" | "explicit_delegation" => {
                    let mut sw = first.walk();
                    first
                        .named_children(&mut sw)
                        .find(|n| n.kind() == "user_type")
                }
                _ => None,
            };
            let Some(ty) = user_type.and_then(|u| receivers::declared_type(cx, u)) else {
                continue;
            };
            if receivers::is_type_parameter(cx, declaration, ty.split('.').next().unwrap_or(&ty)) {
                continue;
            }
            supers.push(type_candidates(
                cx,
                imports,
                package,
                local_types,
                declaration,
                &ty,
            ));
        }
    }
    supers
}

/// An extension function's receiver type name (`String` for
/// `fun String.shout()`): the base identifier of the `user_type` (or the
/// type inside a `nullable_type`) appearing before the function's name.
fn extension_receiver(cx: &FileCx<'_>, function: TsNode<'_>, name: TsNode<'_>) -> Option<String> {
    let mut walk = function.walk();
    for child in function.named_children(&mut walk) {
        if child.start_byte() >= name.start_byte() {
            break;
        }
        let receiver = match child.kind() {
            "user_type" => Some(child),
            "nullable_type" => {
                let mut inner = child.walk();
                child
                    .named_children(&mut inner)
                    .find(|inner_child| inner_child.kind() == "user_type")
            }
            _ => None,
        };
        if let Some(receiver) = receiver {
            let mut inner = receiver.walk();
            let base = receiver
                .named_children(&mut inner)
                .find(|part| part.kind() == "identifier")
                .map(|part| cx.text(&part).to_string());
            return base.or_else(|| Some(cx.text(&receiver).to_string()));
        }
    }
    None
}

/// Import targets that are not call-resolvable Symbols (#721), each keyed
/// `package.Name`, or `package.Type.ENTRY` for an enum entry.
fn collect_importables(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    language: &tree_sitter::Language,
    package: &str,
    path: &str,
    out: &mut Vec<Declared>,
) {
    let mut push = |qualified: String| {
        out.push(Declared {
            fqn: format!("{package}.{qualified}"),
            path: path.to_string(),
            qualified,
            constructible: false,
            interface_or_abstract: false,
            object: false,
            supers: Vec::new(),
        });
    };
    let mut walk = root.walk();
    for declaration in root.named_children(&mut walk) {
        let name = match declaration.kind() {
            // Extension functions: top-level plain ones are already types'
            // and functions' business.
            "function_declaration" => declaration
                .child_by_field_name("name")
                .filter(|name| extension_receiver(cx, declaration, *name).is_some()),
            "type_alias" => declaration.child_by_field_name("type"),
            "property_declaration" => declaration
                .named_children(&mut declaration.walk())
                .find(|child| child.kind() == "variable_declaration")
                .and_then(|variable| variable.named_child(0))
                .filter(|name| name.kind() == "identifier"),
            // tree-sitter-kotlin-ng parses `@A(...) annotation class Name` as
            // an infix expression. `class` is a hard keyword that can never
            // name an infix function, so this exact shape is that declaration.
            "annotated_expression" => annotation_class_name(cx, declaration),
            _ => None,
        };
        if let Some(name) = name {
            push(cx.text(&name).to_string());
        }
    }
    let query = Query::new(language, "(enum_entry (identifier) @entry)").expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, cx.source);
    while let Some(found) = matches.next() {
        let entry = found.captures()[0].node;
        let mut chain = enclosing_type_chain(cx, entry);
        chain.push(cx.text(&entry).to_string());
        push(chain.join("."));
    }
}

/// `Name` of a top-level `annotation class Name` the grammar read as an
/// annotated infix expression.
fn annotation_class_name<'t>(cx: &FileCx<'_>, mut node: TsNode<'t>) -> Option<TsNode<'t>> {
    while node.kind() == "annotated_expression" {
        node = node.named_children(&mut node.walk()).last()?;
    }
    let parts = (node.kind() == "infix_expression")
        .then(|| node.named_children(&mut node.walk()).collect::<Vec<_>>())?;
    match parts.as_slice() {
        [annotation, class, name]
            if parts.iter().all(|part| part.kind() == "identifier")
                && cx.text(annotation) == "annotation"
                && cx.text(class) == "class" =>
        {
            Some(*name)
        }
        _ => None,
    }
}

/// Retarget each `IMPORTS` edge whose complete target this repository
/// declares exactly once — a type, a top-level function, or a member whose
/// `Symbol` the declaring type defines — or declares in exactly one file
/// (overloads, extension functions, top-level properties, type aliases,
/// annotation classes, enum entries) to the declaring `File`. A prefix
/// alone never proves it (`a.Foo.missing` is not `a.Foo`): unproven targets
/// keep their `mod:` id and are classified by [`classify_import`].
fn resolve_repo_imports(
    edges: &mut [Edge],
    repo: &str,
    types: &BTreeMap<String, Option<Declared>>,
    functions: &BTreeMap<String, Option<Declared>>,
    known_symbols: &HashSet<String>,
    importable_files: &BTreeMap<String, Option<String>>,
) {
    for edge in edges {
        if edge.label != "IMPORTS" {
            continue;
        }
        let Some(module) = edge.dst.strip_prefix("mod:") else {
            continue;
        };
        let exact = types
            .get(module)
            .or_else(|| functions.get(module))
            .and_then(Option::as_ref);
        let unique = exact.or_else(|| {
            let (owner, member) = module.rsplit_once('.')?;
            let owner = types.get(owner)?.as_ref()?;
            known_symbols
                .contains(&symbol_id(
                    repo,
                    &owner.path,
                    &format!("{}.{member}", owner.qualified),
                ))
                .then_some(owner)
        });
        // The combined index is authoritative where it names the target: a
        // plain function in one file and an extension in another is a
        // recorded ambiguity, not the plain function's file.
        let path = match importable_files.get(module) {
            Some(unique) => unique.as_deref(),
            None => unique.map(|unique| unique.path.as_str()),
        };
        if let Some(path) = path {
            edge.dst = file_id(repo, path);
            edge.props["resolution"] = "import-proven".into();
        }
    }
}

/// Recover deterministic facts from one Kotlin source file.
pub fn extract_source(
    source: &[u8],
    path: &str,
    id: &SourceId<'_>,
) -> Result<Extraction, ExtractError> {
    let language: tree_sitter::Language = tree_sitter_kotlin_ng::LANGUAGE.into();
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
            "language": "kotlin",
            "prov": cx.prov(&root, &format!("File {path}")),
        }),
    });

    let package = {
        let query = Query::new(&language, "(package_header) @package").expect("static query");
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, root, source);
        let mut package = None;
        if let Some(found) = matches.next() {
            let raw = cx.text(&found.captures()[0].node).replace('\n', " ");
            package = raw
                .trim()
                .strip_prefix("package")
                .map(|rest| rest.trim().trim_end_matches(';').trim().to_string())
                .filter(|name| !name.is_empty());
        }
        package
    };
    out.packages.extend(package.clone());
    let imports = parse_imports(&cx, root, &language, &mut out);
    let hilt: HashSet<String> = imports
        .bindings
        .iter()
        .filter(|(_, fqn)| receivers::HILT_VIEW_MODEL.contains(&fqn.as_str()))
        .filter(|(name, _)| !receivers::declares_name(&cx, root, name))
        .map(|(name, _)| name.clone())
        .collect();
    // `with` is Kotlin's receiver-scoping `kotlin.with` unless this file
    // declares or imports another. A same-package or star-imported
    // repository `with` is checked at the join (`TypedCall::shadowed_by`).
    let std_with = !receivers::declares_name(&cx, root, "with")
        && imports
            .bindings
            .get("with")
            .is_none_or(|fqn| fqn == "kotlin.with");
    let with_shadows: Vec<String> = package
        .iter()
        .chain(&imports.wildcard_packages)
        .map(|pkg| format!("{pkg}.with"))
        .chain(package.is_none().then(|| "with".to_string()))
        .collect();

    // Types: classes, interfaces, enums, data classes, objects — nested
    // chains included.
    let type_query = Query::new(
        &language,
        "[(class_declaration name: (identifier) @name) @decl
          (object_declaration name: (identifier) @name) @decl]",
    )
    .expect("static query");
    let mut local_types: HashSet<String> = HashSet::new();
    let mut type_decls: Vec<(TsNode<'_>, String)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&type_query, root, source);
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
        local_types.insert(qualified.clone());
        let symbol = symbol_id(id.repo, path, &qualified);
        out.nodes.push(Node {
            id: symbol.clone(),
            label: "Symbol".into(),
            props: serde_json::json!({
                "name": qualified,
                "kind": declaration_kind(&cx, decl),
                "language": "kotlin",
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
        type_decls.push((decl, qualified));
    }
    if let Some(package) = &package {
        for (decl, qualified) in type_decls {
            let kind = declaration_kind(&cx, decl);
            let abstract_class = kind == "class" && is_abstract(&cx, decl);
            out.declared_types.push(Declared {
                fqn: format!("{package}.{qualified}"),
                path: path.to_string(),
                qualified,
                constructible: matches!(kind, "class" | "data class"),
                interface_or_abstract: kind == "interface" || abstract_class,
                object: decl.kind() == "object_declaration",
                supers: declaration_supers(&cx, decl, &imports, Some(package), &local_types),
            });
        }
    }

    // Functions: top-level, member, and extension, qualified by their
    // enclosing type chain and (for extensions) their receiver type.
    let function_query = Query::new(&language, "(function_declaration name: (identifier) @name)")
        .expect("static query");
    let mut functions_by_start: HashMap<usize, String> = HashMap::new();
    let mut returns: Vec<(String, TsNode<'_>, Option<String>)> = Vec::new();
    let mut local_functions: HashSet<String> = HashSet::new();
    let mut functions = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&function_query, root, source);
    while let Some(found) = matches.next() {
        let name_node = found.captures()[0].node;
        let Some(function) = name_node.parent() else {
            continue;
        };
        let mut name = cx.text(&name_node).to_string();
        if name.is_empty() {
            // Grammar quirk (verify-at-build, #212): a function named by a
            // soft keyword (`fun dynamic()`, legal Kotlin) lexes as that
            // anonymous keyword token followed by an *empty* identifier.
            // Recover the name from the directly adjacent token; a
            // function whose name still cannot be proven is skipped
            // entirely — never an empty-named Symbol.
            name = name_node
                .prev_sibling()
                .filter(|prev| !prev.is_named() && prev.end_byte() == name_node.start_byte())
                .map(|prev| cx.text(&prev).to_string())
                .filter(|text| {
                    !text.is_empty()
                        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && !text.starts_with(|c: char| c.is_ascii_digit())
                })
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
        }
        let chain = enclosing_type_chain(&cx, function);
        let receiver = extension_receiver(&cx, function, name_node);
        let local_name = match &receiver {
            Some(receiver) => format!("{receiver}.{name}"),
            None => name.clone(),
        };
        let qualified = if chain.is_empty() {
            local_name.clone()
        } else {
            format!("{}.{local_name}", chain.join("."))
        };
        let symbol = symbol_id(id.repo, path, &qualified);
        functions_by_start.insert(function.start_byte(), symbol.clone());
        local_functions.insert(qualified.clone());
        let return_type = {
            let mut walk = function.walk();
            let parts: Vec<TsNode<'_>> = function.named_children(&mut walk).collect();
            parts
                .iter()
                .position(|part| part.kind() == "function_value_parameters")
                .and_then(|at| parts.get(at + 1).copied())
                .and_then(|ty| receivers::declared_type(&cx, ty))
        };
        returns.push((symbol.clone(), function, return_type));
        let mut props = serde_json::json!({
            "name": qualified,
            "kind": "function",
            "language": "kotlin",
            "prov": cx.prov(&function, &format!("Symbol {symbol}")),
        });
        if let Some(receiver) = &receiver {
            props["receiver"] = serde_json::json!(receiver);
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
        if chain.is_empty()
            && receiver.is_none()
            && let Some(package) = &package
        {
            out.declared_functions.push(Declared {
                fqn: format!("{package}.{name}"),
                path: path.to_string(),
                qualified: name.clone(),
                constructible: false,
                interface_or_abstract: false,
                object: false,
                supers: Vec::new(),
            });
        }
        functions.push((function, symbol));
    }

    for (symbol, function, return_type) in returns {
        if let Some(ty) = return_type {
            let candidates = type_candidates(
                &cx,
                &imports,
                package.as_deref(),
                &local_types,
                function,
                &ty,
            );
            out.function_returns.push((symbol, candidates));
        }
    }

    if let Some(package) = &package {
        collect_importables(
            &cx,
            root,
            &language,
            package,
            path,
            &mut out.declared_importables,
        );
    }

    // Spring Web endpoints: annotation-proven controllers, class-level
    // @RequestMapping base path, function-level @{Get,Post,...}Mapping.
    for (function, handler) in &functions {
        let Some(class_decl) = ({
            let mut node = *function;
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
        let mut class_annotations = declaration_annotations(&cx, class_decl);
        class_annotations.extend(preceding_expression_annotations(&cx, class_decl));
        let is_controller = class_annotations.iter().any(|annotation| {
            matches!(annotation.simple.as_str(), "RestController" | "Controller")
                && spring_proven(&annotation.simple, &imports)
        });
        if !is_controller {
            continue;
        }
        // "No @RequestMapping" and "@RequestMapping with no path argument"
        // both mean Spring's default base (""); a present-but-dynamic base
        // poisons every mapping under it (None), failing them closed.
        let base = match class_annotations.iter().find(|annotation| {
            annotation.simple == "RequestMapping" && spring_proven(&annotation.simple, &imports)
        }) {
            None => Some(String::new()),
            Some(annotation) => match &annotation.path {
                PathArg::Absent => Some(String::new()),
                PathArg::Literal(path) => Some(path.clone()),
                PathArg::Dynamic => None,
            },
        };
        for annotation in declaration_annotations(&cx, *function) {
            let Some(http_method) = mapping_method(&annotation.simple) else {
                continue;
            };
            if !spring_proven(&annotation.simple, &imports) {
                continue;
            }
            let tail = match &annotation.path {
                PathArg::Absent => Some(String::new()),
                PathArg::Literal(path) => Some(path.clone()),
                PathArg::Dynamic => None,
            };
            let (Some(base), Some(tail)) = (base.clone(), tail) else {
                // The mapping is proven but its route is a runtime
                // identity (template/constant/expression path, on the
                // method or the class base). T0 cannot confirm the route,
                // so the endpoint is an explicit Gap, never a guessed or
                // default path presented as Confirmed (R-INT-4).
                let gap_id = format!(
                    "gap:route:{}@{}@{}",
                    id.repo,
                    path,
                    annotation.node.start_byte()
                );
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "method": http_method,
                        "handler_sym": handler,
                        "framework": "spring",
                        "language": "kotlin",
                        "reason": "dynamic Spring mapping path",
                        "attempted_tiers": ["T0"],
                        "prov": cx.prov_with_confidence(
                            &annotation.node,
                            ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    }),
                });
                out.edges.push(Edge {
                    src: gap_id.clone(),
                    dst: handler.clone(),
                    label: "HANDLES".into(),
                    props: serde_json::json!({
                        "reason": "dynamic Spring mapping path",
                        "attempted_resolution": "literal-path",
                        "prov": cx.prov_with_confidence(
                            &annotation.node,
                            ConfidenceTier::Gap,
                            &format!("HANDLES {gap_id} -> {handler}"),
                        ),
                    }),
                });
                continue;
            };
            let route = join_route(&base, &tail);
            let endpoint = format!("ep:{}@{http_method}:{route}", id.repo);
            out.nodes.push(Node {
                id: endpoint.clone(),
                label: "Endpoint".into(),
                props: serde_json::json!({
                    "method": http_method,
                    "path": route,
                    "handler_sym": handler,
                    "framework": "spring",
                    "language": "kotlin",
                    "prov": cx.prov(&annotation.node, &format!("Endpoint {endpoint}")),
                }),
            });
            out.edges.push(Edge {
                src: endpoint.clone(),
                dst: handler.clone(),
                label: "HANDLES".into(),
                props: serde_json::json!({
                    "prov": cx.prov(
                        &annotation.node,
                        &format!("HANDLES {endpoint} -> {handler}"),
                    ),
                }),
            });
        }
    }

    // Retrofit client calls (#737): interface methods now, base URL at the
    // directory join.
    out.fetch_sites
        .extend(retrofit::sites(&cx, &functions, &imports));
    out.retrofit_bases
        .extend(retrofit::bases(&cx, root, &imports));

    // Android Navigation 3 entries and back-stack navigations (AC-0351).
    out.navigation3 = navigation3::extract(
        &cx,
        root,
        package.as_deref(),
        &imports,
        &functions_by_start,
        &local_functions,
        &local_types,
    );
    out.hilt_binds = hilt_binds::extract(
        &cx,
        root,
        package.as_deref(),
        &imports,
        &functions_by_start,
        &local_types,
        &local_functions,
        &out.declared_importables,
    );

    // Calls: same-scope unqualified/this calls resolve locally; calls
    // proven by an import (a type/object receiver, or an imported top-level
    // function) resolve repo-wide at the directory join, failing closed to
    // an explicit Gap when the project-local target cannot be proven. A
    // receiver whose lexical binding states its type resolves through that
    // type (#1048); any other receiver (an inferred local, a call result)
    // is unproven and asserts nothing at T0.
    let call_query = Query::new(&language, "(call_expression) @call").expect("static query");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&call_query, root, source);
    while let Some(found) = matches.next() {
        let call = found.captures()[0].node;
        let Some(callee) = call.named_child(0) else {
            continue;
        };
        let Some(src) = ({
            let mut node = call;
            let mut found = None;
            while let Some(parent) = node.parent() {
                if parent.kind() == "function_declaration"
                    && let Some(symbol) = functions_by_start.get(&parent.start_byte())
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
        let (receiver, name) = match callee.kind() {
            "identifier" => (None, cx.text(&callee).to_string()),
            "navigation_expression" => {
                let parts: Vec<TsNode<'_>> = {
                    let mut walk = callee.walk();
                    callee.named_children(&mut walk).collect()
                };
                let [receiver, name] = parts.as_slice() else {
                    continue;
                };
                if name.kind() != "identifier" {
                    continue;
                }
                (Some(*receiver), cx.text(name).to_string())
            }
            _ => continue,
        };
        let receiver_is_this = receiver.is_none_or(|receiver| receiver.kind() == "this_expression");
        // `with(x) { m() }`: x's members come before the enclosing class and
        // top level, so a typed `x` is tried first (W11c-2).
        let with_binding = if receiver.is_none() && std_with {
            receivers::with_receiver_binding(&cx, call, &hilt)
                .filter(|binding| *binding != receivers::Binding::Untyped)
        } else {
            None
        };
        if receiver_is_this {
            // Unqualified or `this.` call: resolve within the enclosing
            // type chain, then (for top-level code) the file's top level.
            let chain = enclosing_type_chain(&cx, call);
            let qualified = if chain.is_empty() {
                name.clone()
            } else {
                format!("{}.{name}", chain.join("."))
            };
            let resolved = if local_functions.contains(&qualified) {
                Some(qualified)
            } else if !chain.is_empty() && local_functions.contains(&name) {
                Some(name.clone())
            } else {
                None
            };
            let fallback = if let Some(qualified) = resolved {
                let dst = symbol_id(id.repo, path, &qualified);
                (dst != src).then(|| {
                    Fallback::Edge(Edge {
                        src: src.clone(),
                        dst,
                        label: "CALLS".into(),
                        props: serde_json::json!({
                            "prov": cx.prov(&call, &format!("CALLS {qualified}")),
                        }),
                    })
                })
            } else if receiver.is_some() {
                None
            } else {
                // An unqualified call to an imported top-level function is
                // import-proven the same way a type receiver is.
                imports.bindings.get(&name).map(|fqn| {
                    Fallback::Pending(pending_call(
                        &cx,
                        src.clone(),
                        call,
                        fqn.clone(),
                        None,
                        name.clone(),
                    ))
                })
            };
            // A receiver type that proves nothing (a type parameter) keeps
            // the ordinary resolution.
            let typed = with_binding.and_then(|binding| {
                typed_call(
                    &cx,
                    &imports,
                    package.as_deref(),
                    &local_types,
                    src.clone(),
                    call,
                    binding,
                    name,
                )
            });
            match (typed, fallback) {
                (Some(mut typed), fallback) => {
                    typed.fallback = fallback;
                    typed.shadowed_by = with_shadows.clone();
                    out.typed_calls.push(typed);
                }
                (None, Some(Fallback::Edge(edge))) => out.edges.push(edge),
                (None, Some(Fallback::Pending(pending))) => out.pending_calls.push(pending),
                (None, None) => {}
            }
            continue;
        }
        let receiver = receiver.expect("non-this receiver");
        let binding = match receiver.kind() {
            "identifier" => receivers::lexical_binding(&cx, call, cx.text(&receiver), &hilt),
            "navigation_expression" => {
                let parts: Vec<TsNode<'_>> = {
                    let mut walk = receiver.walk();
                    receiver.named_children(&mut walk).collect()
                };
                match parts.as_slice() {
                    [this, property]
                        if this.kind() == "this_expression" && property.kind() == "identifier" =>
                    {
                        receivers::this_property_binding(&cx, call, cx.text(property), &hilt)
                    }
                    _ => continue,
                }
            }
            _ => continue,
        };
        match binding {
            Some(receivers::Binding::Untyped) => continue,
            Some(binding) => {
                let typed = typed_call(
                    &cx,
                    &imports,
                    package.as_deref(),
                    &local_types,
                    src,
                    call,
                    binding,
                    name,
                );
                out.typed_calls.extend(typed);
                continue;
            }
            None if receiver.kind() != "identifier" => continue,
            None => {}
        }
        let receiver_text = cx.text(&receiver).to_string();
        let Some(fqn) = imports.bindings.get(&receiver_text) else {
            continue;
        };
        let callee_name = format!("{receiver_text}.{name}");
        push_pending(
            &cx,
            &mut out,
            src,
            call,
            fqn.clone(),
            Some(name),
            callee_name,
        );
    }

    Ok(out)
}

/// The FQNs a type name written at `site` can denote, in Kotlin's
/// precedence: explicit import, a same-file type through the enclosing type
/// chain (as nested names resolve), the same package, then star imports.
fn type_candidates(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    local_types: &HashSet<String>,
    site: TsNode<'_>,
    ty: &str,
) -> Vec<String> {
    let (first, rest) = match ty.split_once('.') {
        Some((first, rest)) => (first, Some(rest)),
        None => (ty, None),
    };
    if let Some(fqn) = imports.bindings.get(first) {
        return vec![match rest {
            Some(rest) => format!("{fqn}.{rest}"),
            None => fqn.clone(),
        }];
    }
    let chain = enclosing_type_chain(cx, site);
    let same_file = (0..=chain.len()).rev().find_map(|depth| {
        let qualified = chain[..depth]
            .iter()
            .map(String::as_str)
            .chain([ty])
            .collect::<Vec<_>>()
            .join(".");
        local_types.contains(&qualified).then_some(qualified)
    });
    let prefix = |name: &str| match package {
        Some(package) => format!("{package}.{name}"),
        None => name.to_string(),
    };
    match same_file {
        Some(qualified) => vec![prefix(&qualified)],
        None => std::iter::once(prefix(ty))
            // A dotted name may already be absolute (`com.acme.Api`).
            .chain((rest.is_some() && package.is_some()).then(|| ty.to_string()))
            .chain(
                imports
                    .wildcard_packages
                    .iter()
                    .map(|wildcard| format!("{wildcard}.{ty}")),
            )
            .collect(),
    }
}

/// A declared-type receiver call (#1048) for the directory join. `None` for
/// an untyped binding.
#[allow(clippy::too_many_arguments)]
fn typed_call(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    local_types: &HashSet<String>,
    src: String,
    call: TsNode<'_>,
    binding: receivers::Binding,
    member: String,
) -> Option<TypedCall> {
    let (ty, constructed, via) = match binding {
        receivers::Binding::Typed(ty) => (ty, false, None),
        receivers::Binding::Constructed(ty) => (ty, true, None),
        receivers::Binding::Factory { factory, method } => (factory, false, Some(method)),
        receivers::Binding::Untyped => return None,
    };
    if receivers::is_type_parameter(cx, call, ty.split('.').next().unwrap_or(&ty)) {
        return None;
    }
    let candidates = type_candidates(cx, imports, package, local_types, call, &ty);
    let callee = match &via {
        Some(method) => format!("{ty}.{method}().{member}"),
        None => format!("{ty}.{member}"),
    };
    Some(TypedCall {
        src,
        candidates,
        constructed,
        via,
        member,
        resolved_props: serde_json::json!({
            "resolution": "declared-type",
            "prov": cx.prov(&call, &format!("CALLS {callee}")),
        }),
        fallback: None,
        shadowed_by: Vec::new(),
    })
}

/// Queue an import-proven call for the directory join, with its
/// fail-closed Gap alternative already built from this call site's span.
fn push_pending(
    cx: &FileCx<'_>,
    out: &mut Extraction,
    src: String,
    call: TsNode<'_>,
    fqn: String,
    member: Option<String>,
    callee: String,
) {
    out.pending_calls
        .push(pending_call(cx, src, call, fqn, member, callee));
}

fn pending_call(
    cx: &FileCx<'_>,
    src: String,
    call: TsNode<'_>,
    fqn: String,
    member: Option<String>,
    callee: String,
) -> PendingCall {
    let gap_id = format!("gap:call:{}@{}@{}", cx.id.repo, cx.path, call.start_byte());
    let gap_node = Node {
        id: gap_id.clone(),
        label: "Gap".into(),
        props: serde_json::json!({
            "callee": callee,
            "reason": "unresolved Kotlin import target",
            "attempted_tiers": ["T0"],
            "prov": cx.prov_with_confidence(
                &call,
                ConfidenceTier::Gap,
                &format!("Gap {gap_id}"),
            ),
        }),
    };
    let gap_edge = Edge {
        src: src.clone(),
        dst: gap_id.clone(),
        label: "CALLS".into(),
        props: serde_json::json!({
            "reason": "unresolved Kotlin import target",
            "attempted_resolution": "import-fqn",
            "prov": cx.prov_with_confidence(
                &call,
                ConfidenceTier::Gap,
                &format!("CALLS -> {gap_id}"),
            ),
        }),
    };
    PendingCall {
        src,
        fqn,
        member,
        resolved_props: serde_json::json!({
            "resolution": "import-proven",
            "prov": cx.prov(&call, &format!("CALLS {callee}")),
        }),
        gap: (gap_node, gap_edge),
    }
}

fn collect_kotlin_files(root: &Path, out: &mut Vec<String>) -> Result<(), ExtractError> {
    let skip = |name: &str| {
        name.starts_with('.')
            || matches!(
                name,
                "target" | "build" | "out" | "node_modules" | "dist" | "generated"
            )
    };
    for file in source_walk::files(root, &skip, source_walk::Gitignores::Honor)? {
        if matches!(
            file.path
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("kt" | "kts")
        ) {
            out.push(file.rel);
        }
    }
    Ok(())
}

/// Recover a Kotlin directory with content-addressed per-file parse reuse.
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
    collect_kotlin_files(root, &mut files)?;
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
                Some(extract_source(&source, path, id)?)
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
            out.typed_calls.extend(extraction.typed_calls);
            out.function_returns.extend(extraction.function_returns);
            out.declared_types.extend(extraction.declared_types);
            out.declared_functions.extend(extraction.declared_functions);
            out.declared_importables
                .extend(extraction.declared_importables);
            out.packages.extend(extraction.packages);
            out.fetch_sites.extend(extraction.fetch_sites);
            out.retrofit_bases.extend(extraction.retrofit_bases);
            out.navigation3.extend(extraction.navigation3);
            out.hilt_binds.extend(extraction.hilt_binds);
            Ok(())
        },
    );
    merged?;
    adapters_fw::client::join_repo_base(
        &mut out.fetch_sites,
        &out.retrofit_bases,
        retrofit::LIBRARY,
    );

    // Directory join: an imported FQN resolves only to a declaration this
    // repo makes exactly once — a duplicate FQN (the same class in two
    // source roots or modules) is ambiguous and fails closed to a Gap
    // instead of silently picking whichever file sorts last (the #170
    // standard). A call on a declared-package import that cannot be proven
    // is an explicit Gap; a call on a foreign package asserts nothing, while
    // the import itself closes over a proven external boundary (#237).
    let unique_by_fqn = |declared: &[Declared]| {
        let mut by_fqn: BTreeMap<String, Option<Declared>> = BTreeMap::new();
        for item in declared {
            by_fqn
                .entry(item.fqn.clone())
                .and_modify(|unique| *unique = None)
                .or_insert(Some(item.clone()));
        }
        by_fqn
    };
    let types_by_fqn = unique_by_fqn(&out.declared_types);
    // An import names a file, so overloads declared in one file still prove
    // it; a name declared in two files proves neither.
    let mut importable_files: BTreeMap<String, Option<String>> = BTreeMap::new();
    for item in out
        .declared_types
        .iter()
        .chain(&out.declared_functions)
        .chain(&out.declared_importables)
    {
        importable_files
            .entry(item.fqn.clone())
            .and_modify(|unique| {
                if unique.as_deref() != Some(item.path.as_str()) {
                    *unique = None;
                }
            })
            .or_insert_with(|| Some(item.path.clone()));
    }
    let functions_by_fqn = unique_by_fqn(&out.declared_functions);
    // Every package this repository declares — Java sources included, so a
    // mixed JVM tree never mistakes its own Java package for external.
    let mut repo_packages: BTreeSet<String> = out.packages.iter().cloned().collect();
    let foreign = foreign_packages(root, &["java"])?;
    repo_packages.extend(foreign.packages);
    let known = out
        .nodes
        .iter()
        .filter(|node| node.label == "Symbol")
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    // Declared-type receivers (#1048). The first candidate FQN this repo
    // declares decides; a duplicate fails closed. A `with` call whose
    // receiver type lacks the member falls back to its ordinary resolution.
    let returns: HashMap<String, Vec<String>> = std::mem::take(&mut out.function_returns)
        .into_iter()
        .collect();
    let resolve_type = |candidates: &[String]| {
        candidates.iter().find_map(|fqn| {
            types_by_fqn
                .get(fqn)
                .map(|unique| (fqn.clone(), unique.clone()))
        })
    };
    for typed in std::mem::take(&mut out.typed_calls) {
        let resolved = (|| {
            if typed
                .shadowed_by
                .iter()
                .any(|fqn| importable_files.contains_key(fqn))
            {
                return None;
            }
            let (fqn, declared) = resolve_type(&typed.candidates)?;
            let declared = declared?; // ambiguous FQN: fail closed
            if typed.constructed && (!declared.constructible || functions_by_fqn.contains_key(&fqn))
            {
                return None;
            }
            let declared = match &typed.via {
                None => declared,
                Some(method) => {
                    let factory = symbol_id(
                        id.repo,
                        &declared.path,
                        &format!("{}.{method}", declared.qualified),
                    );
                    resolve_type(returns.get(&factory)?)?.1?
                }
            };
            let dst = symbol_id(
                id.repo,
                &declared.path,
                &format!("{}.{}", declared.qualified, typed.member),
            );
            (known.contains(&dst) && dst != typed.src).then_some(dst)
        })();
        match (resolved, typed.fallback) {
            (Some(dst), _) => out.edges.push(Edge {
                src: typed.src,
                dst,
                label: "CALLS".into(),
                props: typed.resolved_props,
            }),
            (None, Some(Fallback::Edge(edge))) => out.edges.push(edge),
            (None, Some(Fallback::Pending(pending))) => out.pending_calls.push(pending),
            (None, None) => {}
        }
    }
    for pending in std::mem::take(&mut out.pending_calls) {
        // `Foo(...)` on an imported repository class is a constructor call,
        // which this adapter does not model as a call (as for a same-file
        // class), not an unresolved function. A factory function of the same
        // name still resolves below. An object's or interface's `invoke`
        // still gaps, and so does a class declaring any `invoke` (a companion
        // one is transparent, so it is not told apart from a member one).
        if pending.member.is_none()
            && !functions_by_fqn.contains_key(&pending.fqn)
            && matches!(
                types_by_fqn.get(&pending.fqn),
                Some(Some(unique)) if unique.constructible
                    && !known.contains(&symbol_id(
                        id.repo,
                        &unique.path,
                        &format!("{}.invoke", unique.qualified),
                    ))
            )
        {
            continue;
        }
        let resolved = match &pending.member {
            Some(member) => types_by_fqn.get(&pending.fqn).and_then(|unique| {
                unique.as_ref().map(|declared| {
                    symbol_id(
                        id.repo,
                        &declared.path,
                        &format!("{}.{member}", declared.qualified),
                    )
                })
            }),
            None => functions_by_fqn.get(&pending.fqn).and_then(|unique| {
                unique
                    .as_ref()
                    .map(|declared| symbol_id(id.repo, &declared.path, &declared.qualified))
            }),
        };
        match resolved {
            Some(dst) if known.contains(&dst) => out.edges.push(Edge {
                src: pending.src,
                dst,
                label: "CALLS".into(),
                props: pending.resolved_props,
            }),
            _ => {
                if in_system(&pending.fqn, &repo_packages) {
                    let (node, edge) = pending.gap;
                    out.nodes.push(node);
                    out.edges.push(edge);
                }
            }
        }
    }
    hilt_binds::complete(
        std::mem::take(&mut out.hilt_binds),
        id.repo,
        &types_by_fqn,
        &known,
        &mut out.edges,
    );
    navigation3::complete(
        std::mem::take(&mut out.navigation3),
        id.repo,
        &types_by_fqn,
        &functions_by_fqn,
        &known,
        &mut out.nodes,
        &mut out.edges,
    );
    resolve_repo_imports(
        &mut out.edges,
        id.repo,
        &types_by_fqn,
        &functions_by_fqn,
        &known,
        &importable_files,
    );
    // #464 (ADR-0033): an import proven external is re-keyed on its package
    // id rather than the imported type/member — `mod:kotlinx.coroutines`,
    // not `mod:kotlinx.coroutines.launch` — with the original target kept
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

/// Recover a Kotlin directory without retaining an incremental cache.
pub fn extract_dir(root: &Path, id: &SourceId<'_>) -> Result<Extraction, ExtractError> {
    extract_dir_incremental(root, id, &mut IncrementalCache::default())
        .map(|(extraction, _)| extraction)
}

mod hilt_binds;
pub mod navigation3;
mod receivers;
mod retrofit;

#[cfg(test)]
mod tests;
