//! Flask-Admin views as Flask endpoints (#1193).
//!
//! Flask-Admin (verified against 1.5.7, `flask_admin/base.py`) turns each
//! method of a view class decorated with an import-proven
//! `flask_admin.expose(url='/', methods=('GET',))` into a rule on a Blueprint
//! named `view.endpoint` and prefixed by the view's URL, so the endpoint is
//! `<endpoint>.<method>`. `AdminViewMeta` collects the exposed methods through
//! `dir(cls)`, so a base class's exposed method counts unless the class (or a
//! base before it in the C3 method resolution order) redefines the name.
//! This tier reads:
//!
//! - repository classes at module level whose bases reach an import-proven
//!   `BaseView`, `AdminIndexView` or `ModelView` (`flask_admin.contrib.*`,
//!   `flask_admin.model.BaseModelView`) in C3 order, each base and view
//!   constructor named by an import or module class bound once and not
//!   shadowed where it is used;
//! - an `Admin(…)` assigned once to a name, with that name's
//!   `init_app(app, index_view=…, url=…)` and `add_view(V(…))` /
//!   `add_views(V(…), …)` calls on an inline view constructor.
//!
//! The view's endpoint is its `endpoint` argument, else `admin` for an index
//! view, the model's name lower-cased for a model view, or the class name
//! lower-cased. Its URL is `_get_view_url`: an `url` starting with `/` as is,
//! a relative one under `admin.url`, none as `admin.url + "/" + endpoint`
//! (`"/" + endpoint` when `admin.url` is `/`). `admin.url` is `init_app`'s
//! `url` or its index view's URL when it is given an index view, else the
//! constructor's `url` or index view's URL, `/admin` by default. Rules join the
//! prefix as Flask's Blueprint does.
//!
//! Endpoints carry `mount: "flask_admin"`; they are Confirmed when the app is
//! a recovered Flask application and InferredStrong when it is only an
//! annotated parameter or another expression. Their `auth` is `required`
//! when [`crate::flask_admin_access`] proves the view's `is_accessible`
//! requires login, else `unknown`: the view's `is_accessible` runs on every
//! request, named by the class defining it (with why it is not proven), or
//! `flask_admin_default_allows_all` when no repository class does. In that
//! last case an exposed method whose own body answers an anonymous request
//! first ([`access::body_check`], #1320) is `required` on that basis, and
//! one that reads the login check in an unproven shape names the function
//! holding it (`in_body_auth_check:<function>`).
//! Routes a library class generates (a model view's list/create/edit views,
//! the default index page) are not in source and are one Gap per view. A
//! non-literal argument, an unrecovered or unmodelled base, a repository
//! override of `__init__`, `_get_endpoint`, `_get_view_url` or
//! `create_blueprint`, an index view passed other than as a constructor call,
//! or an `Admin` never given an application is a named Gap, never a guess.

use crate::auth_guards::{self, bounded, import_dotted, rebound, unparenthesize};
use crate::flask_admin_access::{self as access, Hook};
use crate::flask_admin_library::{self as library, Pins};
use crate::{
    Extraction, FileCx, Framework, ImportBinding, MountParent, ReceiverDef,
    enclosing_function_node, literal_string, module_binding, module_file, mount_parent,
    named_arguments, parameter_annotation, prov_from, push_endpoint, scope_binding, symbol_id,
};
use core_graph::Node;
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use tree_sitter::Node as TsNode;

/// A module-level class: `(path, name)`.
type Key = (String, String);

/// An `Admin` instance: the file and start byte of its only assignment.
type Site = (String, usize);

/// Longest base-class chain followed from a registered view.
const MAX_CHAIN: usize = 16;

/// Repository definitions that may change a view's endpoint or URL.
const OVERRIDES: &[&str] = &[
    "__init__",
    "_get_endpoint",
    "_get_view_url",
    "create_blueprint",
];

/// One file's Flask-Admin facts, joined by [`resolve`] once every file is read.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    classes: Vec<ViewClass>,
    exposed: Vec<Exposed>,
    sites: Vec<SiteCall>,
    views: Vec<Registration>,
    /// Writes of an `anonymous_user` attribute.
    anonymous_user: Vec<EvidenceRef>,
    /// Writes of a view hook attribute outside a class body's definitions.
    hook_writes: Vec<EvidenceRef>,
}

impl Facts {
    pub(crate) fn extend(&mut self, other: Facts) {
        self.classes.extend(other.classes);
        self.exposed.extend(other.exposed);
        self.sites.extend(other.sites);
        self.views.extend(other.views);
        self.anonymous_user.extend(other.anonymous_user);
        self.hook_writes.extend(other.hook_writes);
    }
}

#[derive(Debug, Clone)]
struct ViewClass {
    key: Key,
    bases: Vec<Base>,
    /// Every name the class body binds: methods, nested classes, attributes.
    defines: BTreeSet<String>,
    /// The proofs of the `is_accessible` and `inaccessible_callback` it binds.
    hooks: BTreeMap<String, Hook>,
}

#[derive(Debug, Clone)]
enum Base {
    /// A repository class, when one is recovered under this key; the text
    /// as written names it in a Gap.
    Repo(Key, String),
    Library(Library),
    /// Anything this tier does not model.
    Opaque(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Library {
    View,
    Index,
    /// A model view, with the positions of its `endpoint` and `url`
    /// arguments when its constructor is known.
    Model(Option<(usize, usize)>),
}

#[derive(Debug, Clone)]
struct Exposed {
    class: Key,
    method: String,
    /// The rule, when it is a literal.
    url: Option<String>,
    /// The listed methods, when every one is a literal.
    methods: Option<Vec<String>>,
    handler: String,
    evidence: EvidenceRef,
    inputs: Option<serde_json::Value>,
    responses: Option<serde_json::Value>,
    /// The login check the method's own body makes (#1320).
    body: access::BodyCheck,
}

/// An `Admin(…)` constructor or an `init_app(…)` call on its instance.
#[derive(Debug, Clone)]
struct SiteCall {
    site: Site,
    init: bool,
    app: Option<MountParent>,
    url: Value,
    /// The `endpoint` its default `AdminIndexView` takes (constructor only).
    endpoint: Value,
    index: Index,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone)]
enum Index {
    Absent,
    View(Box<ViewCall>),
    /// Passed, but not as a constructor call this tier can read.
    Opaque,
}

/// An inline view constructor `V(…)`.
#[derive(Debug, Clone)]
struct ViewCall {
    class: Base,
    positional: Vec<Value>,
    keywords: Vec<(String, Value)>,
    /// A `*args` or `**kwargs` expansion may supply any argument not written.
    expanded: bool,
    evidence: EvidenceRef,
}

/// An `add_view` / `add_views` argument; `site` is `None` when the receiver
/// is not proven to be an `Admin`.
#[derive(Debug, Clone)]
struct Registration {
    site: Option<Site>,
    view: Option<ViewCall>,
    evidence: EvidenceRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Absent,
    Literal(String),
    None,
    /// A name, as the class it is imported as, else as written.
    Name(String),
    Unknown,
}

impl ViewCall {
    /// The argument a parameter at `position` named `keyword` receives.
    fn argument(&self, keyword: &str, position: Option<usize>) -> Value {
        if let Some((_, value)) = self.keywords.iter().find(|(name, _)| name == keyword) {
            return value.clone();
        }
        match position.and_then(|position| self.positional.get(position)) {
            Some(value) => value.clone(),
            None if self.expanded => Value::Unknown,
            None => Value::Absent,
        }
    }
}

/// An exposed method found while walking a file's decorated functions.
pub(crate) struct Expose {
    class: Key,
    method: String,
    url: Option<String>,
    methods: Option<Vec<String>>,
    body: access::BodyCheck,
}

impl Expose {
    pub(crate) fn record(
        self,
        out: &mut Extraction,
        handler: &str,
        evidence: EvidenceRef,
        inputs: Option<serde_json::Value>,
        responses: Option<serde_json::Value>,
    ) {
        out.flask_admin.exposed.push(Exposed {
            class: self.class,
            method: self.method,
            url: self.url,
            methods: self.methods,
            handler: handler.to_string(),
            evidence,
            inputs,
            responses,
            body: self.body,
        });
    }
}

/// `decorator` on `function` as an import-proven `@expose(…)` on a method of
/// a module-level class.
pub(crate) fn expose(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    decorator: TsNode<'_>,
    function: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<Expose> {
    let call = unparenthesize(decorator.named_child(0)?);
    if call.kind() != "call" {
        return None;
    }
    let callee = call.child_by_field_name("function")?;
    let dotted = import_dotted(cx, callee, bindings)?;
    if !matches!(
        dotted.as_str(),
        "flask_admin.expose" | "flask_admin.base.expose"
    ) || rebound(cx, root, root_name(cx, callee)?, Some(function))
    {
        return None;
    }
    let decorated = function
        .parent()
        .filter(|parent| parent.kind() == "decorated_definition")?;
    let class = decorated
        .parent()
        .filter(|block| block.kind() == "block")?
        .parent()
        .filter(|class| class.kind() == "class_definition" && module_level(*class))?;
    let url = match argument(cx, call, "url", 0) {
        Value::Absent => Some("/".to_string()),
        Value::Literal(url) => Some(url),
        _ => None,
    };
    let methods = match argument_node(cx, call, "methods", 1) {
        Argument::Absent => Some(vec!["GET".to_string()]),
        Argument::Node(list) if matches!(list.kind(), "list" | "tuple") => {
            let mut walk = list.walk();
            list.named_children(&mut walk)
                .map(|item| literal_string(cx, item).map(|method| method.to_ascii_uppercase()))
                .collect()
        }
        _ => None,
    };
    let class_name = cx.text(&class.child_by_field_name("name")?);
    Some(Expose {
        class: (cx.path.to_string(), class_name.to_string()),
        method: cx.text(&function.child_by_field_name("name")?).to_string(),
        url,
        methods,
        body: access::body_check(cx, root, function, decorated, class_name, bindings),
    })
}

/// Collect this file's module-level classes, `Admin` instances and their
/// `init_app` / `add_view` calls.
pub(crate) fn collect(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) {
    let mut walk = root.walk();
    for child in root.named_children(&mut walk) {
        let (class, decorated) = match child.kind() {
            "class_definition" => (child, false),
            "decorated_definition" => match child
                .child_by_field_name("definition")
                .filter(|definition| definition.kind() == "class_definition")
            {
                Some(class) => (class, true),
                None => continue,
            },
            _ => continue,
        };
        let Some(name) = class.child_by_field_name("name") else {
            continue;
        };
        let mut bases = Vec::new();
        if decorated {
            // A class decorator may replace the class.
            bases.push(Base::Opaque("class decorator".into()));
        }
        if let Some(superclasses) = class.child_by_field_name("superclasses") {
            let mut walk = superclasses.walk();
            for base in superclasses.named_children(&mut walk) {
                match base.kind() {
                    "comment" => {}
                    "keyword_argument" | "list_splat" | "dictionary_splat" => {
                        bases.push(Base::Opaque(cx.text(&base).to_string()));
                    }
                    _ if cx.text(&base) == "object" && !bindings.contains_key("object") => {}
                    _ => bases.push(base_of(cx, root, base, None, bindings)),
                }
            }
        }
        let mut defines = BTreeSet::new();
        let mut hooks = BTreeMap::new();
        if let Some(body) = class.child_by_field_name("body") {
            class_names(cx, body, &mut defines);
        }
        // Every binding form counts for a hook, not only those `class_names` reads.
        for hook in access::HOOKS {
            if access::binds(cx, class, hook) {
                defines.insert(hook.to_string());
            }
        }
        for hook in ["is_accessible", "inaccessible_callback"] {
            if defines.contains(hook) {
                let proof = access::hook(cx, root, class, cx.text(&name), hook, bindings);
                hooks.insert(hook.to_string(), proof);
            }
        }
        out.flask_admin.classes.push(ViewClass {
            key: (cx.path.to_string(), cx.text(&name).to_string()),
            bases,
            defines,
            hooks,
        });
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        let writes = |names: &[&str]| {
            access::binding_target(node)
                .is_some_and(|target| access::names_attribute(cx, target, names, true))
                || access::names_attribute(cx, node, names, false)
        };
        if writes(&["anonymous_user"]) {
            out.flask_admin.anonymous_user.push(cx.evidence(&node));
        }
        if writes(access::HOOKS) {
            out.flask_admin.hook_writes.push(cx.evidence(&node));
        }
        match node.kind() {
            "assignment" => {
                if let (Some(left), Some(right)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                ) && left.kind() == "identifier"
                    && admin_call(cx, root, right, bindings)
                {
                    out.flask_admin.sites.push(SiteCall {
                        site: (cx.path.to_string(), node.start_byte()),
                        init: false,
                        app: app_argument(cx, right, 0, bindings),
                        url: argument(cx, right, "url", 2),
                        endpoint: argument(cx, right, "endpoint", 6),
                        index: index_argument(cx, root, right, 4, bindings),
                        evidence: cx.evidence(&right),
                    });
                }
            }
            "call" => {
                let Some(function) = node
                    .child_by_field_name("function")
                    .filter(|function| function.kind() == "attribute")
                else {
                    continue;
                };
                let (Some(object), Some(verb)) = (
                    function.child_by_field_name("object"),
                    function.child_by_field_name("attribute"),
                ) else {
                    continue;
                };
                let site = (object.kind() == "identifier")
                    .then(|| admin_site(cx, root, object, bindings))
                    .flatten();
                match cx.text(&verb) {
                    "init_app" => {
                        if let Some(site) = site {
                            out.flask_admin.sites.push(SiteCall {
                                site,
                                init: true,
                                app: app_argument(cx, node, 0, bindings),
                                url: argument(cx, node, "url", 3),
                                endpoint: argument(cx, node, "endpoint", 2),
                                index: index_argument(cx, root, node, 1, bindings),
                                evidence: cx.evidence(&node),
                            });
                        }
                    }
                    verb @ ("add_view" | "add_views") => {
                        let arguments = named_arguments(node);
                        let arguments = if verb == "add_view" {
                            &arguments[..arguments.len().min(1)]
                        } else {
                            &arguments[..]
                        };
                        for argument in arguments {
                            let view = (argument.kind() == "call")
                                .then(|| view_call(cx, root, *argument, bindings));
                            if site.is_none() && view.is_none() {
                                continue;
                            }
                            out.flask_admin.views.push(Registration {
                                site: site.clone(),
                                view,
                                evidence: cx.evidence(argument),
                            });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Names a class body binds, outside nested function and class bodies.
fn class_names(cx: &FileCx<'_>, node: TsNode<'_>, names: &mut BTreeSet<String>) {
    let mut walk = node.walk();
    for child in node.named_children(&mut walk) {
        match child.kind() {
            "function_definition" | "class_definition" => {
                if let Some(name) = child.child_by_field_name("name") {
                    names.insert(cx.text(&name).to_string());
                }
            }
            "decorated_definition" => {
                if let Some(name) = child
                    .child_by_field_name("definition")
                    .and_then(|definition| definition.child_by_field_name("name"))
                {
                    names.insert(cx.text(&name).to_string());
                }
            }
            "assignment" | "augmented_assignment" => {
                if let Some(left) = child.child_by_field_name("left") {
                    target_names(cx, left, names);
                }
                if let Some(right) = child.child_by_field_name("right") {
                    class_names(cx, right, names);
                }
            }
            "lambda" => {}
            _ => class_names(cx, child, names),
        }
    }
}

fn target_names(cx: &FileCx<'_>, target: TsNode<'_>, names: &mut BTreeSet<String>) {
    match target.kind() {
        "identifier" => {
            names.insert(cx.text(&target).to_string());
        }
        "pattern_list" | "tuple_pattern" | "list_pattern" | "tuple" | "list" => {
            let mut walk = target.walk();
            for item in target.named_children(&mut walk) {
                target_names(cx, item, names);
            }
        }
        _ => {}
    }
}

fn module_level(class: TsNode<'_>) -> bool {
    let parent = class.parent();
    let parent = match parent {
        Some(decorated) if decorated.kind() == "decorated_definition" => decorated.parent(),
        other => other,
    };
    parent.is_some_and(|parent| parent.kind() == "module")
}

/// The identifier an identifier / attribute chain starts with.
fn root_name<'s>(cx: &'s FileCx<'_>, mut node: TsNode<'_>) -> Option<&'s str> {
    while node.kind() == "attribute" {
        node = node.child_by_field_name("object")?;
    }
    (node.kind() == "identifier").then(|| cx.text(&node))
}

/// A base class expression, or a view constructor's callee, evaluated in
/// `scope` (`None` for the module): its name must be bound once, by the
/// import or module-level class it names, and not shadowed around the use.
fn base_of(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    node: TsNode<'_>,
    scope: Option<TsNode<'_>>,
    bindings: &HashMap<String, ImportBinding>,
) -> Base {
    let node = unparenthesize(node);
    let text = cx.text(&node).to_string();
    if root_name(cx, node).is_none_or(|name| rebound(cx, root, name, scope)) {
        return Base::Opaque(text);
    }
    if let Some(dotted) = import_dotted(cx, node, bindings) {
        if dotted.starts_with("flask_admin.") {
            return library(&dotted).map_or(Base::Opaque(dotted), Base::Library);
        }
        return match dotted
            .rsplit_once('.')
            .and_then(|(module, class)| Some((module_file(cx.path, module)?, class)))
        {
            Some((file, class)) => Base::Repo((file, class.to_string()), text),
            None => Base::Opaque(text),
        };
    }
    if node.kind() == "identifier"
        && module_binding(cx, root, &text)
            .writes
            .first()
            .is_some_and(|write| write.kind() == "class_definition")
    {
        return Base::Repo((cx.path.to_string(), text.clone()), text);
    }
    Base::Opaque(text)
}

fn library(dotted: &str) -> Option<Library> {
    match dotted {
        "flask_admin.BaseView" | "flask_admin.base.BaseView" => Some(Library::View),
        "flask_admin.AdminIndexView" | "flask_admin.base.AdminIndexView" => Some(Library::Index),
        "flask_admin.contrib.sqla.ModelView" | "flask_admin.contrib.sqla.view.ModelView" => {
            Some(Library::Model(Some((4, 5))))
        }
        "flask_admin.model.BaseModelView" | "flask_admin.model.base.BaseModelView" => {
            Some(Library::Model(Some((3, 4))))
        }
        dotted if dotted.starts_with("flask_admin.contrib.") && dotted.ends_with(".ModelView") => {
            Some(Library::Model(None))
        }
        _ => None,
    }
}

/// Whether `call` is an import-proven `flask_admin.Admin(…)`.
fn admin_call(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    call: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> bool {
    if call.kind() != "call" {
        return false;
    }
    let Some(callee) = call.child_by_field_name("function") else {
        return false;
    };
    import_dotted(cx, callee, bindings).is_some_and(|dotted| {
        matches!(
            dotted.as_str(),
            "flask_admin.Admin" | "flask_admin.base.Admin"
        )
    }) && root_name(cx, callee).is_some_and(|name| !rebound(cx, root, name, Some(call)))
}

/// The `Admin` the name `object` holds: its scope's only binding of the name
/// is an `Admin(…)` assignment, made before `object` when in a function.
fn admin_site(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    object: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<Site> {
    let name = cx.text(&object);
    let mut scope = enclosing_function_node(object);
    let write = loop {
        let Some(function) = scope else {
            let binding = module_binding(cx, root, name);
            let [write] = binding.writes[..] else {
                return None;
            };
            break write;
        };
        let binding = scope_binding(cx, function, name);
        if binding.global || parameter_annotation(cx, function, name).is_some() {
            return None;
        }
        match binding.writes[..] {
            [] => scope = enclosing_function_node(function),
            [write] if !binding.nonlocal && write.end_byte() <= object.start_byte() => break write,
            _ => return None,
        }
    };
    let right = write
        .child_by_field_name("left")
        .filter(|left| write.kind() == "assignment" && left.kind() == "identifier")
        .and(write.child_by_field_name("right"))?;
    admin_call(cx, root, right, bindings).then(|| (cx.path.to_string(), write.start_byte()))
}

enum Argument<'t> {
    Absent,
    Node(TsNode<'t>),
    /// A `*args` / `**kwargs` expansion may supply it.
    Unknown,
}

/// The argument a call passes to the parameter at `position` named `keyword`.
fn argument_node<'t>(
    cx: &FileCx<'_>,
    call: TsNode<'t>,
    keyword: &str,
    position: usize,
) -> Argument<'t> {
    let (mut index, mut starred, mut expanded, mut found) = (0, false, false, None);
    for argument in named_arguments(call) {
        match argument.kind() {
            "keyword_argument" => {
                if argument
                    .child_by_field_name("name")
                    .is_some_and(|name| cx.text(&name) == keyword)
                    && let Some(value) = argument.child_by_field_name("value")
                {
                    return Argument::Node(value);
                }
            }
            "dictionary_splat" => expanded = true,
            "list_splat" => starred = true,
            "comment" => {}
            _ => {
                if !starred && index == position {
                    found = Some(argument);
                }
                index += 1;
            }
        }
    }
    match found {
        Some(node) => Argument::Node(node),
        None if starred || expanded => Argument::Unknown,
        None => Argument::Absent,
    }
}

fn argument(cx: &FileCx<'_>, call: TsNode<'_>, keyword: &str, position: usize) -> Value {
    match argument_node(cx, call, keyword, position) {
        Argument::Absent => Value::Absent,
        Argument::Unknown => Value::Unknown,
        Argument::Node(node) => value_of(cx, None, node),
    }
}

/// A literal, `None` or — given the module and its imports — a class name:
/// imported (as the name it is exported under) or defined once at module
/// level, and not rebound around `node`.
fn value_of(
    cx: &FileCx<'_>,
    scope: Option<(TsNode<'_>, &HashMap<String, ImportBinding>)>,
    node: TsNode<'_>,
) -> Value {
    let node = unparenthesize(node);
    if let Some(text) = literal_string(cx, node) {
        return Value::Literal(text);
    }
    match node.kind() {
        "none" => Value::None,
        "identifier" => {
            let Some((root, bindings)) = scope else {
                return Value::Unknown;
            };
            let name = cx.text(&node);
            if rebound(cx, root, name, Some(node)) {
                return Value::Unknown;
            }
            match bindings.get(name) {
                Some(ImportBinding::Symbol { exported, .. }) => Value::Name(exported.clone()),
                Some(ImportBinding::Module { .. }) => Value::Unknown,
                None => match module_binding(cx, root, name).writes[..] {
                    [write] if write.kind() == "class_definition" => Value::Name(name.to_string()),
                    _ => Value::Unknown,
                },
            }
        }
        _ => Value::Unknown,
    }
}

fn app_argument(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    position: usize,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<MountParent> {
    match argument_node(cx, call, "app", position) {
        Argument::Absent => None,
        Argument::Node(node) if unparenthesize(node).kind() == "none" => None,
        Argument::Node(node) => Some(mount_parent(cx, unparenthesize(node), bindings)),
        Argument::Unknown => Some(MountParent::Unproven),
    }
}

fn index_argument(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    call: TsNode<'_>,
    position: usize,
    bindings: &HashMap<String, ImportBinding>,
) -> Index {
    match argument_node(cx, call, "index_view", position) {
        Argument::Absent => Index::Absent,
        Argument::Node(node) => match unparenthesize(node) {
            node if node.kind() == "none" => Index::Absent,
            node if node.kind() == "call" => {
                Index::View(Box::new(view_call(cx, root, node, bindings)))
            }
            _ => Index::Opaque,
        },
        Argument::Unknown => Index::Opaque,
    }
}

fn view_call(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    call: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> ViewCall {
    let class = call.child_by_field_name("function").map_or_else(
        || Base::Opaque(String::new()),
        |callee| base_of(cx, root, callee, Some(call), bindings),
    );
    let (mut positional, mut keywords, mut starred, mut expanded) =
        (Vec::new(), Vec::new(), false, false);
    for argument in named_arguments(call) {
        match argument.kind() {
            "keyword_argument" => {
                if let (Some(name), Some(value)) = (
                    argument.child_by_field_name("name"),
                    argument.child_by_field_name("value"),
                ) {
                    keywords.push((
                        cx.text(&name).to_string(),
                        value_of(cx, Some((root, bindings)), value),
                    ));
                }
            }
            "list_splat" => (starred, expanded) = (true, true),
            "dictionary_splat" => expanded = true,
            "comment" => {}
            // After `*args`, positions are unknown.
            _ if starred => {}
            _ => positional.push(value_of(cx, Some((root, bindings)), argument)),
        }
    }
    ViewCall {
        class,
        positional,
        keywords,
        expanded,
        evidence: cx.evidence(&call),
    }
}

/// What a registered view class's chain proves.
struct Chain<'f> {
    /// The registered class's own name.
    name: String,
    kind: Library,
    exposed: Vec<&'f Exposed>,
    /// The first class in the chain defining `is_accessible`.
    access: Option<String>,
    /// The chain's login proof: its spans and `is_accessible`'s symbol, or
    /// why there is none.
    login: Result<(Vec<EvidenceRef>, Key), String>,
    /// A repository class owns `index`: it defines it before any library base
    /// that does.
    defines_index: bool,
    /// Every name a repository class owns, defining it before any library
    /// base that routes it: a library method of that name is no longer
    /// Flask-Admin's.
    defined: BTreeSet<String>,
}

fn find<'f>(classes: &HashMap<Key, &'f ViewClass>, key: &Key) -> Option<&'f ViewClass> {
    classes.get(key).copied().or_else(|| {
        let package = format!("{}/__init__.py", key.0.strip_suffix(".py")?);
        classes.get(&(package, key.1.clone())).copied()
    })
}

/// One entry of a view class's method resolution order: a recovered
/// repository class, or a modelled Flask-Admin base (whose own methods are
/// not modelled, so its place only matters relative to the others).
#[derive(Clone, Copy)]
enum Mro<'f> {
    Class(&'f ViewClass),
    Library(Library),
}

impl PartialEq for Mro<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Mro::Class(a), Mro::Class(b)) => a.key == b.key,
            (Mro::Library(a), Mro::Library(b)) => a == b,
            _ => false,
        }
    }
}

/// Python's C3 linearization of `key`, as `type()` computes `__mro__`, so
/// `dir`/`getattr` in `AdminViewMeta` see the same owner of each name. A
/// hierarchy Python would refuse (repeated or inconsistent bases) or one
/// deeper than `MAX_CHAIN` is a Gap reason, as is any unmodelled base.
fn linearize<'f>(
    key: &Key,
    text: &str,
    depth: usize,
    classes: &HashMap<Key, &'f ViewClass>,
    memo: &mut HashMap<Key, Vec<Mro<'f>>>,
) -> Result<Vec<Mro<'f>>, String> {
    if depth > MAX_CHAIN {
        return Err("flask_admin_view_class_chain_too_deep".into());
    }
    let class =
        find(classes, key).ok_or_else(|| format!("flask_admin_view_class_not_recovered:{text}"))?;
    if let Some(line) = memo.get(&class.key) {
        return Ok(line.clone());
    }
    let mut sequences = Vec::new();
    for base in &class.bases {
        sequences.push(match base {
            Base::Repo(key, text) => linearize(key, text, depth + 1, classes, memo)?,
            Base::Library(library) => vec![Mro::Library(*library)],
            Base::Opaque(text) => {
                return Err(format!("flask_admin_view_base_not_modelled:{text}"));
            }
        });
    }
    // A repeated base fails the merge below, as Python refuses it.
    let heads = sequences.iter().map(|line| line[0]).collect();
    sequences.push(heads);
    let mut line = vec![Mro::Class(class)];
    loop {
        sequences.retain(|sequence| !sequence.is_empty());
        let Some(next) = sequences.iter().map(|sequence| sequence[0]).find(|head| {
            !sequences
                .iter()
                .any(|sequence| sequence[1..].contains(head))
        }) else {
            if sequences.is_empty() {
                break;
            }
            return Err(format!("flask_admin_view_mro_inconsistent:{}", class.key.1));
        };
        line.push(next);
        for sequence in &mut sequences {
            if sequence[0] == next {
                sequence.remove(0);
            }
        }
    }
    memo.insert(class.key.clone(), line.clone());
    Ok(line)
}

/// The registered class's method resolution order: a name the first class
/// in it to define it owns, so its `@expose` (or the lack of one) wins.
fn chain<'f>(
    base: &Base,
    classes: &HashMap<Key, &'f ViewClass>,
    exposed: &HashMap<Key, Vec<&'f Exposed>>,
) -> Result<Chain<'f>, String> {
    let (key, text) = match base {
        Base::Repo(key, text) => (key, text),
        Base::Library(kind) => {
            return Ok(Chain {
                name: String::new(),
                kind: *kind,
                exposed: Vec::new(),
                access: None,
                login: Err("flask_admin_default_allows_all".into()),
                defines_index: false,
                defined: BTreeSet::new(),
            });
        }
        Base::Opaque(text) => return Err(format!("flask_admin_view_class_not_modelled:{text}")),
    };
    let mut memo = HashMap::new();
    let mro = linearize(key, text, 0, classes, &mut memo)?;
    let (mut order, mut libraries) = (Vec::new(), Vec::new());
    for entry in mro.iter().copied() {
        match entry {
            Mro::Class(class) => order.push(class),
            Mro::Library(library) => libraries.push(library),
        }
    }
    let kind = libraries
        .iter()
        .find(|library| matches!(library, Library::Model(_)))
        .or_else(|| libraries.iter().find(|library| **library == Library::Index))
        .or(libraries.first())
        .copied()
        .ok_or_else(|| "flask_admin_view_base_not_modelled".to_string())?;
    for class in &order {
        if let Some(name) = OVERRIDES.iter().find(|name| class.defines.contains(**name)) {
            return Err(format!("flask_admin_view_overrides:{name}:{}", class.key.1));
        }
    }
    // Each name belongs to the first entry in C3 order defining it: a
    // library base owns the routed methods it defines, so a repository
    // class after it (`class V(ModelView, LateMixin)`) does not replace them.
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    let mut defined = BTreeSet::new();
    for entry in &mro {
        match entry {
            Mro::Library(library) => {
                let routes = match library {
                    Library::Model(_) => library::MODEL_ROUTES,
                    Library::Index => library::INDEX_ROUTES,
                    Library::View => &[],
                };
                seen.extend(routes.iter().map(|(name, _, _)| name.to_string()));
            }
            Mro::Class(class) => {
                for name in &class.defines {
                    if !seen.insert(name.clone()) {
                        continue;
                    }
                    defined.insert(name.clone());
                    if let Some(list) = exposed.get(&class.key) {
                        found.extend(list.iter().filter(|exposed| &exposed.method == name));
                    }
                }
            }
        }
    }
    let login = login(&order, &memo);
    Ok(Chain {
        name: order[0].key.1.clone(),
        kind,
        exposed: found,
        login,
        access: order
            .iter()
            .find(|class| class.defines.contains("is_accessible"))
            .map(|class| class.key.1.clone()),
        defines_index: defined.contains("index"),
        defined,
    })
}

/// The repository side of [`access`]'s proof over a view's repository
/// classes in C3 order, `memo` holding each one's own linearization.
fn login(
    order: &[&ViewClass],
    memo: &HashMap<Key, Vec<Mro<'_>>>,
) -> Result<(Vec<EvidenceRef>, Key), String> {
    for class in order {
        if let Some(name) = ["_handle_view", "_run_view"]
            .iter()
            .find(|name| class.defines.contains(**name))
        {
            return Err(format!("flask_admin_view_overrides:{name}:{}", class.key.1));
        }
        // A class that is not a view may follow `BaseView`, whose hooks win.
        let view = memo
            .get(&class.key)
            .is_some_and(|line| line.iter().any(|entry| matches!(entry, Mro::Library(_))));
        if !view
            && access::HOOKS
                .iter()
                .any(|name| class.defines.contains(*name))
        {
            return Err(format!(
                "flask_admin_hook_outside_view_class:{}",
                class.key.1
            ));
        }
    }
    let mut evidence = Vec::new();
    let mut guard = None;
    for hook in ["is_accessible", "inaccessible_callback"] {
        let Some(class) = order.iter().find(|class| class.defines.contains(hook)) else {
            if hook == "is_accessible" {
                return Err("flask_admin_default_allows_all".into());
            }
            continue;
        };
        match class.hooks.get(hook) {
            Some(Hook::Proven(spans)) => evidence.extend(spans.iter().cloned()),
            Some(Hook::Unproven(reason)) => return Err(reason.clone()),
            None => {
                return Err(format!(
                    "flask_admin_hook_not_a_plain_method:{hook}:{}",
                    class.key.1
                ));
            }
        }
        guard.get_or_insert_with(|| (class.key.0.clone(), format!("{}.{hook}", class.key.1)));
    }
    let guard = guard.ok_or_else(|| "flask_admin_default_allows_all".to_string())?;
    Ok((evidence, guard))
}

/// A view's `auth`: proven `required`, or `unknown` with its reasons.
enum Auth {
    Required {
        guard: String,
        basis: &'static str,
        assumptions: Vec<&'static str>,
        evidence: Vec<EvidenceRef>,
    },
    Unknown(Vec<String>),
}

/// `is_accessible` is proven to require login.
const BASIS_ACCESS: &str = "flask_admin_is_accessible_requires_login";
/// The view's own body answers an anonymous request first (#1320).
const BASIS_BODY: &str = "flask_view_body_denies_anonymous";
const ASSUME_ANONYMOUS: &str = "anonymous_user_not_named_by_computed_or_escaped_string";
/// A helper's `redirect(…)` is returned by `x or …` only while the response
/// object is truthy, as Werkzeug's is.
const ASSUME_TRUTHY: &str = "redirect_response_is_truthy";

impl Auth {
    fn props(&self, handler: &str) -> serde_json::Value {
        match self {
            Auth::Required {
                guard,
                basis,
                assumptions,
                evidence,
            } => serde_json::json!({
                "state": "required",
                "authenticated": true,
                "guards": [guard],
                "basis": [basis],
                "assumptions": assumptions,
                "prov": prov_from(
                    bounded(evidence.clone()),
                    ConfidenceTier::InferredStrong,
                    &format!("Auth of {handler}"),
                ),
            }),
            Auth::Unknown(reasons) => serde_json::json!({ "state": "unknown", "reasons": reasons }),
        }
    }
}

/// The `auth` an exposed method's own body gives it when no class in its
/// chain defines `is_accessible`: `required` when the body's check is
/// proven and every condition outside the view holds, else `unknown` naming
/// the function that holds the check. `None` when the body reads no check.
fn body_auth(
    repo: &str,
    exposed: &Exposed,
    login: &Result<(Vec<EvidenceRef>, Key), String>,
    global: &Result<Vec<EvidenceRef>, String>,
) -> Option<Auth> {
    let open = "flask_admin_default_allows_all";
    let (function, proof) = match &exposed.body {
        access::BodyCheck::Absent => return None,
        access::BodyCheck::Seen(function) => (function, None),
        access::BodyCheck::Proven {
            function,
            helper,
            evidence,
        } => (function, Some((*helper, evidence))),
    };
    let mut reasons = vec![open.to_string(), format!("in_body_auth_check:{function}")];
    let Some((helper, spans)) = proof else {
        return Some(Auth::Unknown(reasons));
    };
    // A `_handle_view` or `_run_view` override, or a hook outside a view
    // class, may run something other than the method.
    let chain = match login {
        Err(reason) if reason == open => Ok(()),
        Err(reason) => Err(reason.clone()),
        Ok(_) => Ok(()),
    };
    match chain.and(global.as_ref().map_err(Clone::clone)) {
        Ok(pinned) => {
            let mut evidence = spans.clone();
            evidence.extend(pinned.iter().cloned());
            let mut assumptions = vec![ASSUME_ANONYMOUS];
            if helper {
                assumptions.push(ASSUME_TRUTHY);
            }
            Some(Auth::Required {
                guard: symbol_id(repo, &exposed.class.0, function),
                basis: BASIS_BODY,
                assumptions,
                evidence,
            })
        }
        Err(reason) => {
            reasons.push(reason);
            Some(Auth::Unknown(reasons))
        }
    }
}

/// A literal endpoint, or the default when Flask-Admin's `if endpoint:` is false.
fn endpoint_of(
    value: Value,
    default: impl FnOnce() -> Option<String>,
) -> Result<String, &'static str> {
    match value {
        Value::Literal(endpoint) if !endpoint.is_empty() => Ok(endpoint),
        Value::Absent | Value::None | Value::Literal(_) => {
            default().ok_or("flask_admin_model_not_named")
        }
        _ => Err("flask_admin_view_endpoint_not_literal"),
    }
}

/// A view's own `url`: `None` when not given.
fn url_of(value: Value) -> Result<Option<String>, &'static str> {
    match value {
        Value::Absent | Value::None => Ok(None),
        Value::Literal(url) => Ok(Some(url)),
        _ => Err("flask_admin_view_url_not_literal"),
    }
}

/// Flask-Admin's `_get_view_url`.
fn view_url(admin_url: &str, own: Option<String>, endpoint: &str, is_index: bool) -> String {
    match own {
        None if admin_url != "/" => format!("{admin_url}/{endpoint}"),
        None if is_index => "/".into(),
        None => format!("/{endpoint}"),
        Some(url) if url.starts_with('/') => url,
        Some(url) => format!("{admin_url}/{url}"),
    }
}

/// A Blueprint rule under its prefix, as Flask joins them (a `/` prefix is
/// no prefix at all in `create_blueprint`).
fn rule_path(prefix: &str, rule: &str) -> String {
    if prefix == "/" {
        rule.to_string()
    } else if rule.is_empty() {
        prefix.to_string()
    } else {
        format!(
            "{}/{}",
            prefix.trim_end_matches('/'),
            rule.trim_start_matches('/')
        )
    }
}

struct Emit<'o> {
    out: &'o mut Extraction,
    repo: &'o str,
    emitted: BTreeSet<String>,
}

impl Emit<'_> {
    fn gap(&mut self, reason: &str, class: &str, evidence: Vec<EvidenceRef>) {
        let Some(first) = evidence.first() else {
            return;
        };
        let gap_id = format!(
            "gap:flask_admin:{}@{}@{}",
            self.repo, first.path, first.byte_start
        );
        if !self.emitted.insert(gap_id.clone()) {
            return;
        }
        self.out.nodes.push(Node {
            id: gap_id.clone(),
            label: "Gap".into(),
            props: serde_json::json!({
                "reason": reason,
                "class": class,
                "framework": "flask_admin",
                "language": "python",
                "attempted_tiers": ["T0"],
                "prov": prov_from(evidence, ConfidenceTier::Gap, &format!("Gap {gap_id}")),
            }),
        });
    }
}

/// Join every file's Flask-Admin facts into endpoints and Gaps. Runs before
/// the router join consumes the receiver definitions.
pub(crate) fn resolve(
    out: &mut Extraction,
    repo: &str,
    root: &Path,
    commit: &str,
    max_lock_bytes: u64,
) {
    let facts = std::mem::take(&mut out.flask_admin);
    if facts.sites.is_empty() && facts.views.is_empty() {
        return;
    }
    let pins = library::pins(root, repo, commit, max_lock_bytes);
    // Conditions of the login proof that do not depend on the view.
    let patched = |name: &str| out.patched_guards.iter().any(|patched| patched == name);
    let global = match (
        &pins,
        &library::login_pins(root, repo, commit, max_lock_bytes),
    ) {
        _ if patched(auth_guards::CURRENT_USER) => Err("flask_login_current_user_replaced".into()),
        _ if patched(auth_guards::ABORT) || patched(auth_guards::RESPONSE) => {
            Err("flask_response_helper_replaced".into())
        }
        _ if !facts.anonymous_user.is_empty() => Err("flask_login_anonymous_user_replaced".into()),
        _ if !facts.hook_writes.is_empty() => Err("flask_admin_view_hook_replaced".into()),
        (Pins::Checked(admin), Pins::Checked(login)) => {
            Ok(admin.iter().chain(login).cloned().collect::<Vec<_>>())
        }
        (Pins::Absent, _) => Err("flask_admin_version_not_pinned".to_string()),
        (Pins::Unchecked(reason), _) | (_, Pins::Unchecked(reason)) => Err(reason.clone()),
        (_, Pins::Absent) => Err("flask_login_version_not_pinned".to_string()),
    };
    let classes: HashMap<Key, &ViewClass> = facts
        .classes
        .iter()
        .map(|class| (class.key.clone(), class))
        .collect();
    let mut exposed: HashMap<Key, Vec<&Exposed>> = HashMap::new();
    for item in &facts.exposed {
        exposed.entry(item.class.clone()).or_default().push(item);
    }
    let flask_apps: HashSet<Key> = out
        .receiver_defs
        .iter()
        .filter(|def| !def.router && def.module_level && def.framework == Framework::Flask)
        .map(|def: &ReceiverDef| def.key.clone())
        .collect();
    let mut sites: BTreeMap<&Site, Vec<&SiteCall>> = BTreeMap::new();
    for call in &facts.sites {
        sites.entry(&call.site).or_default().push(call);
    }
    let mut registrations: BTreeMap<&Site, Vec<&Registration>> = BTreeMap::new();
    let mut emit = Emit {
        out,
        repo,
        emitted: BTreeSet::new(),
    };
    for registration in &facts.views {
        match &registration.site {
            Some(site) => registrations.entry(site).or_default().push(registration),
            None => {
                // `add_view` on a receiver not proven to be an `Admin`.
                if let Some(view) = &registration.view
                    && let Ok(chain) = chain(&view.class, &classes, &exposed)
                {
                    emit.gap(
                        "flask_admin_admin_instance_not_proven",
                        &chain.name,
                        vec![registration.evidence.clone()],
                    );
                }
            }
        }
    }
    for (site, calls) in sites {
        let views = registrations.remove(site).unwrap_or_default();
        let ctor = calls.iter().copied().find(|call| !call.init);
        let inits: Vec<&SiteCall> = calls.iter().copied().filter(|call| call.init).collect();
        let fail = |emit: &mut Emit<'_>, reason: &str| {
            for registration in &views {
                emit.gap(reason, "", vec![registration.evidence.clone()]);
            }
            if let Some(ctor) = ctor {
                emit.gap(reason, "", vec![ctor.evidence.clone()]);
            }
        };
        if inits.len() > 1 {
            fail(&mut emit, "flask_admin_init_app_called_more_than_once");
            continue;
        }
        let init = inits.first().copied();
        let binder = init
            .filter(|init| init.app.is_some())
            .or(ctor.filter(|ctor| ctor.app.is_some()));
        let Some(binder) = binder else {
            fail(&mut emit, "flask_admin_admin_not_bound_to_app");
            continue;
        };
        let app = binder.app.clone().unwrap_or(MountParent::Unproven);
        let confidence = match &app {
            MountParent::LocalApp(Framework::Flask) => ConfidenceTier::Confirmed,
            MountParent::Def(key) if flask_apps.contains(key) => ConfidenceTier::Confirmed,
            _ => ConfidenceTier::InferredStrong,
        };
        let from_init = init.filter(|init| !matches!(init.index, Index::Absent));
        let index = from_init
            .or(ctor)
            .map_or(&Index::Absent, |call| &call.index);
        let index_view = match index {
            Index::Opaque => {
                fail(&mut emit, "flask_admin_index_view_not_constructed_inline");
                continue;
            }
            Index::View(call) => match chain(&call.class, &classes, &exposed) {
                Ok(chain) if chain.kind == Library::Index => Some((call, chain)),
                Ok(_) => {
                    fail(&mut emit, "flask_admin_index_view_not_admin_index_view");
                    continue;
                }
                Err(reason) => {
                    fail(&mut emit, &reason);
                    continue;
                }
            },
            Index::Absent => None,
        };
        let index_url = match &index_view {
            Some((call, _)) => match call.argument("url", Some(3)) {
                Value::Absent | Value::None => Ok("/admin".to_string()),
                Value::Literal(url) if !url.is_empty() => Ok(url),
                _ => Err("flask_admin_view_url_not_literal"),
            },
            None => Ok("/admin".to_string()),
        };
        let own = match from_init {
            Some(init) => &init.url,
            None => ctor.map_or(&Value::Absent, |ctor| &ctor.url),
        };
        let admin_url = match own {
            Value::Literal(url) if !url.is_empty() => Ok(url.clone()),
            Value::Absent | Value::None | Value::Literal(_) => index_url,
            _ => Err("flask_admin_admin_url_not_literal"),
        };
        let admin_url = match admin_url {
            Ok(url) => url,
            Err(reason) => {
                fail(&mut emit, reason);
                continue;
            }
        };
        let mut site_evidence = vec![binder.evidence.clone()];
        if let Some(ctor) = ctor.filter(|ctor| ctor.evidence != binder.evidence) {
            site_evidence.push(ctor.evidence.clone());
        }
        let mut entries: Vec<(&ViewCall, bool, Result<Chain<'_>, String>)> = Vec::new();
        match index_view {
            Some((call, chain)) => entries.push((call, true, Ok(chain))),
            None => {
                if let Some(ctor) = ctor {
                    match (
                        &pins,
                        endpoint_of(ctor.endpoint.clone(), || Some("admin".into())),
                    ) {
                        (Pins::Checked(pinned), Ok(endpoint)) => {
                            let prefix =
                                view_url(&admin_url, Some(admin_url.clone()), &endpoint, true);
                            let mut evidence = site_evidence.clone();
                            evidence.extend(pinned.iter().cloned());
                            emit.library(
                                false,
                                library::INDEX_ROUTES,
                                &BTreeSet::new(),
                                &prefix,
                                &endpoint,
                                &evidence,
                                confidence,
                                &Auth::Unknown(vec!["flask_admin_default_allows_all".into()]),
                            );
                        }
                        (Pins::Checked(_), Err(reason)) => {
                            emit.gap(reason, "AdminIndexView", vec![ctor.evidence.clone()]);
                        }
                        (Pins::Absent, _) => emit.gap(
                            "flask_admin_library_index_route",
                            "AdminIndexView",
                            vec![ctor.evidence.clone()],
                        ),
                        (Pins::Unchecked(reason), _) => {
                            emit.gap(reason, "AdminIndexView", vec![ctor.evidence.clone()]);
                        }
                    }
                }
            }
        }
        for registration in &views {
            match &registration.view {
                Some(view) => entries.push((view, false, chain(&view.class, &classes, &exposed))),
                None => emit.gap(
                    "flask_admin_view_not_constructed_inline",
                    "",
                    vec![registration.evidence.clone()],
                ),
            }
        }
        for (view, is_index, chain) in entries {
            let chain = match chain {
                Ok(chain) => chain,
                Err(reason) => {
                    emit.gap(&reason, "", vec![view.evidence.clone()]);
                    continue;
                }
            };
            let library = match chain.kind {
                Library::Model(_) => Some((
                    true,
                    library::MODEL_ROUTES,
                    "flask_admin_model_view_routes_in_library",
                )),
                Library::Index if !chain.defines_index => Some((
                    false,
                    library::INDEX_ROUTES,
                    "flask_admin_library_index_route",
                )),
                _ => None,
            };
            let library = library.and_then(|(model, routes, unpinned)| match &pins {
                Pins::Checked(pinned) => Some((model, routes, pinned)),
                Pins::Absent | Pins::Unchecked(_) => {
                    let reason = match &pins {
                        Pins::Unchecked(reason) => reason.as_str(),
                        _ => unpinned,
                    };
                    emit.gap(reason, &chain.name, vec![view.evidence.clone()]);
                    None
                }
            });
            if chain.exposed.is_empty() && library.is_none() {
                continue;
            }
            let (endpoint, url) = match chain.kind {
                Library::Index | Library::View => (
                    view.argument("endpoint", Some(2)),
                    view.argument("url", Some(3)),
                ),
                Library::Model(Some((endpoint, url))) => (
                    view.argument("endpoint", Some(endpoint)),
                    view.argument("url", Some(url)),
                ),
                // Every known model view takes `endpoint` after its model,
                // session or collection, and name and category.
                Library::Model(None) if view.positional.len() > 2 => {
                    (Value::Unknown, Value::Unknown)
                }
                Library::Model(None) => {
                    (view.argument("endpoint", None), view.argument("url", None))
                }
            };
            let endpoint = endpoint_of(endpoint, || match chain.kind {
                Library::Index => Some("admin".into()),
                Library::View => Some(chain.name.to_lowercase()),
                Library::Model(_) => match view.argument("model", Some(0)) {
                    Value::Name(model) => Some(model.to_lowercase()),
                    _ => None,
                },
            });
            let url = url_of(url).map(|url| match chain.kind {
                Library::Index => url.or_else(|| Some("/admin".into())),
                _ => url,
            });
            let (endpoint, url) = match (endpoint, url) {
                (Ok(endpoint), Ok(url)) => (endpoint, url),
                (Err(reason), _) | (_, Err(reason)) => {
                    emit.gap(reason, &chain.name, vec![view.evidence.clone()]);
                    continue;
                }
            };
            let prefix = view_url(&admin_url, url, &endpoint, is_index);
            let auth = match (&chain.access, &chain.login, &global) {
                (Some(_), Ok((spans, guard)), Ok(pinned)) => {
                    let mut evidence = spans.clone();
                    evidence.extend(pinned.iter().cloned());
                    Auth::Required {
                        guard: symbol_id(repo, &guard.0, &guard.1),
                        basis: BASIS_ACCESS,
                        assumptions: vec![ASSUME_ANONYMOUS],
                        evidence,
                    }
                }
                (None, _, _) => Auth::Unknown(vec!["flask_admin_default_allows_all".into()]),
                (Some(class), login, global) => {
                    let mut reasons = vec![format!("flask_admin_access_check:{class}")];
                    if let Err(reason) = login.as_ref().map(|_| ()).and(global.as_ref().map(|_| ()))
                    {
                        reasons.push(reason.clone());
                    }
                    Auth::Unknown(reasons)
                }
            };
            for exposed in &chain.exposed {
                let (Some(rule), Some(methods)) = (&exposed.url, &exposed.methods) else {
                    emit.gap(
                        if exposed.url.is_none() {
                            "flask_admin_expose_url_not_literal"
                        } else {
                            "flask_admin_expose_methods_not_literal"
                        },
                        &chain.name,
                        vec![exposed.evidence.clone()],
                    );
                    continue;
                };
                let path = rule_path(&prefix, rule);
                let name = format!("{endpoint}.{}", exposed.method);
                // #1320: no `is_accessible` guards the view, and its own
                // body may still answer an anonymous request first.
                let own = chain
                    .access
                    .is_none()
                    .then(|| body_auth(repo, exposed, &chain.login, &global))
                    .flatten();
                let mut evidence = vec![exposed.evidence.clone(), view.evidence.clone()];
                evidence.extend(site_evidence.iter().cloned());
                for method in methods {
                    if !emit
                        .emitted
                        .insert(format!("{method} {path} {}", exposed.handler))
                    {
                        continue;
                    }
                    push_endpoint(
                        emit.out,
                        repo,
                        method,
                        &path,
                        &exposed.handler,
                        Framework::Flask,
                        &evidence,
                        confidence,
                        Some("flask_admin"),
                        exposed.inputs.as_ref(),
                        exposed.responses.as_ref(),
                        Some(&name),
                    );
                    if let Some(node) = emit.out.nodes.last_mut() {
                        node.props["auth"] = own.as_ref().unwrap_or(&auth).props(&exposed.handler);
                    }
                }
            }
            if let Some((model, routes, pinned)) = library {
                let mut evidence = vec![view.evidence.clone()];
                evidence.extend(site_evidence.iter().cloned());
                evidence.extend(pinned.iter().cloned());
                emit.library(
                    model,
                    routes,
                    &chain.defined,
                    &prefix,
                    &endpoint,
                    &evidence,
                    confidence,
                    &auth,
                );
            }
        }
    }
}

impl Emit<'_> {
    /// Flask-Admin's own routes for one view, except a method a repository
    /// class in its chain redefines (its `@expose`, or lack of one, wins).
    /// Release knowledge, not source, so never above InferredStrong.
    #[allow(clippy::too_many_arguments)]
    fn library(
        &mut self,
        model: bool,
        routes: &[(&str, &str, &[&str])],
        defined: &BTreeSet<String>,
        prefix: &str,
        endpoint: &str,
        evidence: &[EvidenceRef],
        confidence: ConfidenceTier,
        auth: &Auth,
    ) {
        let confidence = match confidence {
            ConfidenceTier::Confirmed => ConfidenceTier::InferredStrong,
            other => other,
        };
        for (method_name, rule, methods) in routes {
            if defined.contains(*method_name) {
                continue;
            }
            let path = rule_path(prefix, rule);
            let name = format!("{endpoint}.{method_name}");
            let handler = library::handler(model, method_name);
            for method in *methods {
                if !self.emitted.insert(format!("{method} {path} {handler}")) {
                    continue;
                }
                push_endpoint(
                    self.out,
                    self.repo,
                    method,
                    &path,
                    &handler,
                    Framework::Flask,
                    evidence,
                    confidence,
                    Some("flask_admin"),
                    None,
                    None,
                    Some(&name),
                );
                if let Some(node) = self.out.nodes.last_mut() {
                    node.props["auth"] = auth.props(&handler);
                    node.props["handler_origin"] = "library".into();
                }
            }
        }
    }
}

/// Retarget the evidence a cached file's Flask-Admin facts carry to `commit`.
pub(crate) fn retarget(extraction: &mut Extraction, commit: &str) {
    let facts = &mut extraction.flask_admin;
    for exposed in &mut facts.exposed {
        exposed.evidence.commit_sha = commit.into();
        for nested in [&mut exposed.inputs, &mut exposed.responses]
            .into_iter()
            .flatten()
        {
            crate::retarget_props_commit(nested, commit);
        }
    }
    for call in &mut facts.sites {
        call.evidence.commit_sha = commit.into();
        if let Index::View(view) = &mut call.index {
            view.evidence.commit_sha = commit.into();
        }
    }
    for class in &mut facts.classes {
        for hook in class.hooks.values_mut() {
            if let Hook::Proven(spans) = hook {
                for span in spans {
                    span.commit_sha = commit.into();
                }
            }
        }
    }
    for span in facts
        .anonymous_user
        .iter_mut()
        .chain(&mut facts.hook_writes)
    {
        span.commit_sha = commit.into();
    }
    for registration in &mut facts.views {
        registration.evidence.commit_sha = commit.into();
        if let Some(view) = &mut registration.view {
            view.evidence.commit_sha = commit.into();
        }
    }
}
