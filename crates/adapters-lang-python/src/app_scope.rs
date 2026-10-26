//! Which Flask application an object may be, so that a request hook reaches
//! only the endpoints of the application it is registered on (#1149,
//! AC-0383).
//!
//! An object is resolved to a set of application identities, or to *any*:
//!
//! - a function-local `name = Flask(...)` that is its scope's only binding,
//!   by its construction site;
//! - a module-level `name = Flask(...)` (or an import of that exact name
//!   from its defining file), by its definition key, when every receiver
//!   definition under that key is such an application;
//! - a parameter of a top-level, undecorated function, as the union of the
//!   argument passed at every call of that function's name in the
//!   repository, matched by name text, which over-approximates. It is *any*
//!   when the name is used other than as a callee, its own definition or
//!   an unaliased import; when a call omits the argument or spreads
//!   `*args` / `**kwargs`; when no call is found; past four parameters
//!   deep; or on a cycle;
//! - anything else is *any*.
//!
//! A function reached only by string lookup (`getattr`) is not followed, and
//! callers outside the repository are not seen: a `none` fact that relies
//! on scoping records `request_hooks_reach_only_their_application`.
use crate::{
    Extraction, FileCx, ImportBinding, LocalReceiver, ReceiverDef, enclosing_function_node,
    local_receiver, parameter_annotation,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use tree_sitter::Node as TsNode;

/// Parameters followed back to their callers before giving up.
const MAX_PARAMETER_DEPTH: usize = 4;

/// What one object may be, as its own file reads it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum AppRef {
    /// A module-level or imported name, by its definition key.
    Def((String, String)),
    /// A function-local `name = Flask(...)`, by file and assignment start.
    Local(String, usize),
    /// A parameter of top-level function `function` in `path`; `index` is
    /// its position, `None` when it is keyword-only.
    Param {
        path: String,
        function: String,
        index: Option<usize>,
        name: String,
    },
    /// Not provably any one application.
    Any,
}

/// One call anywhere in the repository, with what each identifier argument
/// may be.
#[derive(Debug, Clone)]
pub(crate) struct AppCall {
    /// The called name: an identifier, or an attribute's last segment.
    callee: String,
    positional: Vec<AppRef>,
    keywords: Vec<(String, AppRef)>,
    /// `*args`, `**kwargs` or a generator argument: any parameter may be
    /// anything.
    spread: bool,
}

/// The applications an object may be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Apps {
    Any,
    Known(BTreeSet<AppRef>),
}

impl Apps {
    pub(crate) fn union(self, other: Apps) -> Apps {
        match (self, other) {
            (Apps::Known(mut a), Apps::Known(b)) => {
                a.extend(b);
                Apps::Known(a)
            }
            _ => Apps::Any,
        }
    }

    /// Whether the two may share an application.
    pub(crate) fn meets(&self, other: &Apps) -> bool {
        match (self, other) {
            (Apps::Known(a), Apps::Known(b)) => !a.is_disjoint(b),
            _ => true,
        }
    }
}

/// What the receiver `object` of a route, mount or hook may be.
pub(crate) fn app_ref(
    cx: &FileCx<'_>,
    object: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> AppRef {
    if object.kind() != "identifier" {
        return AppRef::Any;
    }
    let name = cx.text(&object);
    match local_receiver(cx, object, name, bindings) {
        LocalReceiver::App(crate::Framework::Flask, start) => {
            AppRef::Local(cx.path.to_string(), start)
        }
        LocalReceiver::App(..) | LocalReceiver::Unproven => AppRef::Any,
        LocalReceiver::Parameter(_) => parameter_ref(cx, object, name),
        LocalReceiver::Free => match bindings.get(name) {
            Some(ImportBinding::Symbol {
                target_file: Some(target),
                exported,
                ..
            }) => AppRef::Def((target.clone(), exported.clone())),
            Some(_) => AppRef::Any,
            None => AppRef::Def((cx.path.to_string(), name.to_string())),
        },
    }
}

/// The parameter `name` that `object` reads, when its function is a
/// top-level, undecorated definition.
fn parameter_ref(cx: &FileCx<'_>, object: TsNode<'_>, name: &str) -> AppRef {
    let mut scope = enclosing_function_node(object);
    while let Some(function) = scope {
        if parameter_annotation(cx, function, name).is_some() {
            let top_level = function
                .parent()
                .is_some_and(|parent| parent.kind() == "module");
            let Some(function_name) = function.child_by_field_name("name").filter(|_| top_level)
            else {
                return AppRef::Any;
            };
            return AppRef::Param {
                path: cx.path.to_string(),
                function: cx.text(&function_name).to_string(),
                index: parameter_position(cx, function, name),
                name: name.to_string(),
            };
        }
        scope = enclosing_function_node(function);
    }
    AppRef::Any
}

/// The position of parameter `name`, `None` once a `*` or `*args` makes it
/// keyword-only.
fn parameter_position(cx: &FileCx<'_>, function: TsNode<'_>, name: &str) -> Option<usize> {
    let parameters = function.child_by_field_name("parameters")?;
    let mut walk = parameters.walk();
    let mut index = 0;
    for parameter in parameters.named_children(&mut walk) {
        let ident = match parameter.kind() {
            "identifier" => Some(parameter),
            "default_parameter" | "typed_default_parameter" => {
                parameter.child_by_field_name("name")
            }
            "typed_parameter" => {
                let mut inner = parameter.walk();
                // `*args: T` and `**kwargs: T` hold no identifier.
                let found = parameter
                    .named_children(&mut inner)
                    .find(|child| child.kind() == "identifier")?;
                Some(found)
            }
            "list_splat_pattern" | "keyword_separator" | "dictionary_splat_pattern" => {
                return None;
            }
            _ => continue,
        };
        if ident.is_some_and(|ident| cx.text(&ident) == name) {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// Record every call and every name used other than as a callee.
pub(crate) fn collect(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
    out: &mut Extraction,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "call" => {
                if let Some(call) = app_call(cx, node, bindings) {
                    out.app_calls.push(call);
                }
            }
            "identifier" if is_use(node) => {
                out.name_uses.insert(cx.text(&node).to_string());
            }
            _ => {}
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
}

fn app_call(
    cx: &FileCx<'_>,
    call: TsNode<'_>,
    bindings: &HashMap<String, ImportBinding>,
) -> Option<AppCall> {
    let function = call.child_by_field_name("function")?;
    let callee = match function.kind() {
        "identifier" => function,
        "attribute" => function.child_by_field_name("attribute")?,
        _ => return None,
    };
    let mut found = AppCall {
        callee: cx.text(&callee).to_string(),
        positional: Vec::new(),
        keywords: Vec::new(),
        spread: false,
    };
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return Some(found);
    };
    if arguments.kind() != "argument_list" {
        found.spread = true;
        return Some(found);
    }
    let value =
        |argument: TsNode<'_>| app_ref(cx, crate::auth_guards::unparenthesize(argument), bindings);
    let mut walk = arguments.walk();
    for argument in arguments.named_children(&mut walk) {
        match argument.kind() {
            "comment" => {}
            "list_splat" | "dictionary_splat" => found.spread = true,
            "keyword_argument" => {
                if let (Some(name), Some(argument)) = (
                    argument.child_by_field_name("name"),
                    argument.child_by_field_name("value"),
                ) {
                    found
                        .keywords
                        .push((cx.text(&name).to_string(), value(argument)));
                }
            }
            _ => found.positional.push(value(argument)),
        }
    }
    Some(found)
}

/// Whether identifier `node` may pass its value on: anything but a callee,
/// a definition's own name, or a name an import binds without renaming.
fn is_use(node: TsNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };
    let is_callee = |call: TsNode<'_>, function: TsNode<'_>| {
        call.kind() == "call" && call.child_by_field_name("function") == Some(function)
    };
    match parent.kind() {
        _ if is_callee(parent, node) => return false,
        "attribute" if parent.child_by_field_name("attribute") == Some(node) => {
            return !parent.parent().is_some_and(|call| is_callee(call, parent));
        }
        "function_definition" if parent.child_by_field_name("name") == Some(node) => {
            return false;
        }
        _ => {}
    }
    let mut ancestor = Some(parent);
    while let Some(current) = ancestor {
        match current.kind() {
            // `from m import f as g` calls `f` by another name.
            "aliased_import" => {
                return current.child_by_field_name("name").is_some_and(|name| {
                    name.start_byte() <= node.start_byte() && node.end_byte() <= name.end_byte()
                });
            }
            "import_statement" | "import_from_statement" | "future_import_statement" => {
                return false;
            }
            _ => {}
        }
        ancestor = current.parent();
    }
    true
}

/// Keys whose every receiver definition is a module-level Flask
/// application.
pub(crate) fn app_defs(defs: &[ReceiverDef]) -> HashSet<(String, String)> {
    let mut apps = HashSet::new();
    let mut other = HashSet::new();
    for def in defs {
        if !def.router && def.module_level && def.framework == crate::Framework::Flask {
            apps.insert(def.key.clone());
        } else {
            other.insert(def.key.clone());
        }
    }
    apps.retain(|key| !other.contains(key));
    apps
}

/// Every call and name use in the repository, for following parameters.
pub(crate) struct Scope<'a> {
    calls: HashMap<&'a str, Vec<&'a AppCall>>,
    used: &'a BTreeSet<String>,
    app_defs: &'a HashSet<(String, String)>,
}

impl<'a> Scope<'a> {
    pub(crate) fn new(
        calls: &'a [AppCall],
        used: &'a BTreeSet<String>,
        app_defs: &'a HashSet<(String, String)>,
    ) -> Self {
        let mut by_callee: HashMap<&str, Vec<&AppCall>> = HashMap::new();
        for call in calls {
            by_callee
                .entry(call.callee.as_str())
                .or_default()
                .push(call);
        }
        Scope {
            calls: by_callee,
            used,
            app_defs,
        }
    }

    pub(crate) fn apps(&self, app: &AppRef) -> Apps {
        self.resolve(app, &mut Vec::new())
    }

    fn resolve(&self, app: &AppRef, chain: &mut Vec<AppRef>) -> Apps {
        match app {
            AppRef::Any => Apps::Any,
            AppRef::Local(..) => Apps::Known(BTreeSet::from([app.clone()])),
            AppRef::Def(key) if self.app_defs.contains(key) => {
                Apps::Known(BTreeSet::from([app.clone()]))
            }
            AppRef::Def(_) => Apps::Any,
            AppRef::Param {
                function,
                index,
                name,
                ..
            } => {
                if chain.len() >= MAX_PARAMETER_DEPTH
                    || chain.contains(app)
                    || self.used.contains(function)
                {
                    return Apps::Any;
                }
                let Some(calls) = self.calls.get(function.as_str()) else {
                    return Apps::Any;
                };
                chain.push(app.clone());
                let mut apps = Apps::Known(BTreeSet::new());
                for call in calls {
                    let argument =
                        index
                            .and_then(|index| call.positional.get(index))
                            .or_else(|| {
                                call.keywords
                                    .iter()
                                    .find(|(keyword, _)| keyword == name)
                                    .map(|(_, value)| value)
                            });
                    let Some(argument) = argument.filter(|_| !call.spread) else {
                        apps = Apps::Any;
                        break;
                    };
                    apps = apps.union(self.resolve(argument, chain));
                    if apps == Apps::Any {
                        break;
                    }
                }
                chain.pop();
                apps
            }
        }
    }
}
