//! Repository decorators proven not to authenticate (#1173, AC-0446).
//!
//! A repository decorator on a Flask handler is non-authenticating when its
//! request-time wrapper provably ends a request only by calling the wrapped
//! view, by returning nothing, or by raising a werkzeug HTTP exception
//! whose status is neither 401 nor 403. Two shapes are read:
//!
//! - a bare decorator `def deco(f): … def wrapper(…): …; return wrapper`,
//!   every return of `deco` naming that one inner wrapper;
//! - a factory call `@factory(…)` with inert arguments, where every return
//!   of `factory` instantiates a class of its own file, `Cls(…)`, whose
//!   `__call__(self, f)` returns such a wrapper. The class is undecorated,
//!   has no base class, and defines no dunder other than `__init__` and
//!   `__call__`, so instantiation, the call and method lookup are its own.
//!   Its body holds only undecorated methods (and a docstring or `pass`);
//!   `__init__` only assigns `self.<attr>` from inert values or its own
//!   parameters, whose defaults are inert; no other method, nor the
//!   wrapper, writes an attribute of `self`; `factory`'s defaults are inert
//!   and it passes only inert values or its own parameters. Every
//!   attribute of the instance is then an inert value or one the decorator
//!   site wrote as an inert value, so calling one runs no repository code.
//!
//! In the wrapper, and in each method it calls on `self` (followed at most
//! four deep; deeper or a cycle is unproven): a `return` of anything but
//! the wrapped call or `None`, a `yield` or `await`, any `abort`, `setattr`,
//! `delattr`, `exec` or `eval` call, a bare `raise`, a raise of anything
//! but a known werkzeug exception (with inert arguments only: a `response`
//! would replace the error), a call through an expression that is not a
//! name or attribute, a call that may enforce authentication (see
//! `denying_call`), and a read of one of the class's methods other than to
//! call it are unproven. Every name that resolves to a repository
//! function is recorded in `calls`, to be proven deny-free at resolve time;
//! any other call is assumed not to abort. The wrapper may be decorated only
//! with `functools.wraps(<wrapped>)`.
use crate::auth_guards::{
    dotted_root, every_descendant, import_dotted, inert, named, rebound, unparenthesize,
};
use crate::request_inputs::{
    binding_sites, handler_bindings, is_write_target, parameter_name, visit_scope,
};
use crate::responses::{FnKey, Resolver};
use crate::{FileCx, module_binding};
use std::collections::{BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

/// Methods of the instance's class followed from the wrapper.
const MAX_METHOD_DEPTH: usize = 4;

/// Werkzeug HTTP exceptions whose status is neither 401 nor 403.
const NON_AUTH_ERRORS: &[&str] = &[
    "BadRequest",
    "NotFound",
    "MethodNotAllowed",
    "NotAcceptable",
    "RequestTimeout",
    "Conflict",
    "Gone",
    "LengthRequired",
    "PreconditionFailed",
    "RequestEntityTooLarge",
    "RequestURITooLarge",
    "UnsupportedMediaType",
    "RequestedRangeNotSatisfiable",
    "ExpectationFailed",
    "ImATeapot",
    "UnprocessableEntity",
    "Locked",
    "FailedDependency",
    "PreconditionRequired",
    "TooManyRequests",
    "RequestHeaderFieldsTooLarge",
    "UnavailableForLegalReasons",
    "InternalServerError",
    "NotImplemented",
    "BadGateway",
    "ServiceUnavailable",
    "GatewayTimeout",
    "HTTPVersionNotSupported",
];

/// Modules whose calls may enforce authentication: never assumed not to abort.
const AUTH_MODULES: &[&str] = &[
    "flask_login",
    "flask_jwt_extended",
    "flask_httpauth",
    "flask_security",
    "flask_principal",
    "flask_praetorian",
    "authlib",
    "jwt",
];

/// Callee names that abort or enforce authentication wherever they come from.
const DENYING_NAMES: &[&str] = &[
    "abort",
    "unauthorized",
    "verify_jwt_in_request",
    "login_required",
    "fresh_login_required",
    "jwt_required",
    "auth_required",
    "roles_required",
    "permission_required",
    "setattr",
    "delattr",
    "exec",
    "eval",
];

/// Whether calling `callee` may abort or enforce authentication: a denying
/// name, Flask's or werkzeug's `abort` under any alias, or anything from an
/// authentication library.
pub(crate) fn denying_call(cx: &FileCx<'_>, callee: TsNode<'_>, resolver: &Resolver<'_>) -> bool {
    let name = match callee.kind() {
        "identifier" => Some(callee),
        "attribute" => callee.child_by_field_name("attribute"),
        _ => None,
    };
    name.is_some_and(|name| DENYING_NAMES.contains(&cx.text(&name)))
        || import_dotted(cx, callee, resolver.bindings).is_some_and(|dotted| {
            dotted.ends_with(".abort")
                || AUTH_MODULES.iter().any(|module| {
                    dotted == *module
                        || dotted
                            .strip_prefix(module)
                            .is_some_and(|rest| rest.starts_with('.'))
                })
        })
}

/// What a top-level function proves as a non-authenticating decorator.
#[derive(Debug, Clone)]
pub(crate) struct WrapperProof {
    /// Used as `@factory(…)`, not as a bare `@decorator`.
    pub factory: bool,
    /// The wrapper and the methods it calls are proven by their own bodies.
    pub clean: bool,
    /// Repository functions they name, each to be proven deny-free.
    pub calls: Vec<FnKey>,
    /// Module names whose replacement anywhere voids the proof: the
    /// decorator or factory, and the class.
    pub names: Vec<String>,
    /// A factory's class, which no other file may import or read as an
    /// attribute: passed to a helper, it could be rewritten there.
    pub class: Option<String>,
}

/// Whether `raise` raises a known werkzeug exception other than 401/403,
/// its local name not rebound.
pub(crate) fn allowed_raise(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    statement: TsNode<'_>,
    resolver: &Resolver<'_>,
) -> bool {
    let Some(raised) = statement
        .named_child(0)
        .filter(|child| child.kind() != "comment")
        .map(unparenthesize)
    else {
        // A bare `raise` re-raises whatever was caught.
        return false;
    };
    let class = if raised.kind() == "call" {
        // A `response` argument would be served in place of the error.
        let inert_arguments = raised
            .child_by_field_name("arguments")
            .is_some_and(|arguments| {
                named(arguments)
                    .into_iter()
                    .all(|argument| match argument.kind() {
                        "comment" => true,
                        "keyword_argument" => argument
                            .child_by_field_name("value")
                            .is_some_and(|value| inert(cx, root, unparenthesize(value))),
                        _ => inert(cx, root, unparenthesize(argument)),
                    })
            });
        match raised.child_by_field_name("function") {
            Some(callee) if inert_arguments => callee,
            _ => return false,
        }
    } else {
        raised
    };
    let known = import_dotted(cx, class, resolver.bindings).is_some_and(|dotted| {
        dotted
            .strip_prefix("werkzeug.exceptions.")
            .is_some_and(|name| NON_AUTH_ERRORS.contains(&name))
    });
    known && dotted_root(cx, class).is_some_and(|name| !rebound(cx, root, &name, Some(statement)))
}

/// The proof `function` (a top-level `function_definition`) carries as a
/// decorator or decorator factory, when it has either shape.
pub(crate) fn proof(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    function: TsNode<'_>,
    resolver: &Resolver<'_>,
) -> Option<WrapperProof> {
    if decorated(function) || is_async(function) {
        return None;
    }
    let name = cx.text(&function.child_by_field_name("name")?).to_string();
    let parameters = plain_parameters(cx, function);
    let body = function.child_by_field_name("body")?;
    let mut prover = Prover {
        cx,
        root,
        resolver,
        methods: HashMap::new(),
        calls: BTreeSet::new(),
    };
    // A decorator name bound twice in its module may be either binding.
    let sole = module_binding(cx, root, &name).writes.len() == 1;
    let mut names = vec![name];
    if let Some([wrapped]) = parameters.as_deref() {
        // `def deco(f)`: the returned inner wrapper runs per request, with
        // `f` the view unless the decorator rebinds it.
        let wrapper = returned_wrapper(cx, body)?;
        let mut rebinds = false;
        binding_sites(cx, body, &mut |_, bound| rebinds |= bound.contains(wrapped));
        let clean = sole && !rebinds && prover.wrapper(wrapper, wrapped, None);
        return Some(WrapperProof {
            factory: false,
            clean,
            calls: prover.calls.into_iter().collect(),
            names,
            class: None,
        });
    }
    let class = factory_class(cx, root, function, body)?;
    let class_name = cx.text(&class.child_by_field_name("name")?).to_string();
    names.push(class_name.clone());
    let clean = sole
        && !class_escapes(cx, root, class, body, &class_name)
        && match class_methods(cx, class) {
            Some(methods) => {
                prover.methods = methods;
                prover
                    .methods
                    .get("__call__")
                    .copied()
                    .is_some_and(|call| prover.instance_is_inert() && prover.dunder_call(call))
            }
            None => false,
        };
    Some(WrapperProof {
        factory: true,
        clean,
        calls: prover.calls.into_iter().collect(),
        names,
        class: Some(class_name),
    })
}

fn decorated(function: TsNode<'_>) -> bool {
    function
        .parent()
        .is_some_and(|parent| parent.kind() == "decorated_definition")
}

fn is_async(function: TsNode<'_>) -> bool {
    (0..function.child_count())
        .filter_map(|index| function.child(index))
        .any(|child| child.kind() == "async")
}

/// The names of `function`'s parameters, when every one is a plain or
/// typed name with no default.
fn plain_parameters(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<Vec<String>> {
    let parameters = function.child_by_field_name("parameters")?;
    named(parameters)
        .into_iter()
        .filter(|parameter| parameter.kind() != "comment")
        .map(|parameter| match parameter.kind() {
            "identifier" | "typed_parameter" => {
                parameter_name(parameter).map(|name| cx.text(&name).to_string())
            }
            _ => None,
        })
        .collect()
}

/// The inner function every return of `body` names, defined directly in it.
fn returned_wrapper<'t>(cx: &FileCx<'_>, body: TsNode<'t>) -> Option<TsNode<'t>> {
    let mut inner = HashMap::new();
    for statement in named(body) {
        let definition = match statement.kind() {
            "decorated_definition" => statement.child_by_field_name("definition"),
            "function_definition" => Some(statement),
            _ => None,
        };
        if let Some(definition) = definition.filter(|d| d.kind() == "function_definition")
            && let Some(name) = definition.child_by_field_name("name")
        {
            inner.insert(cx.text(&name).to_string(), definition);
        }
    }
    let mut returned = Vec::new();
    let mut other = false;
    visit_scope(body, &mut |node| {
        if node.kind() != "return_statement" {
            return;
        }
        match node
            .named_child(0)
            .map(unparenthesize)
            .filter(|value| value.kind() == "identifier")
            .and_then(|value| inner.get(cx.text(&value)))
        {
            Some(definition) => returned.push(*definition),
            None => other = true,
        }
    });
    let first = *returned.first()?;
    // The returned name is bound once, by its definition. `binding_sites`
    // reports a decorated definition at both the decoration and the `def`.
    let name = cx.text(&first.child_by_field_name("name")?).to_string();
    let mut bindings = 0;
    binding_sites(cx, body, &mut |site, bound| {
        let decorated_def = site.kind() == "function_definition" && decorated(site);
        bindings += usize::from(bound.contains(&name) && !decorated_def);
    });
    (!other && bindings == 1 && returned.iter().all(|definition| *definition == first))
        .then_some(first)
}

/// The top-level, sole-bound class every return of factory `function`
/// instantiates, with only inert values or the factory's own unrebound
/// parameters as arguments and inert parameter defaults.
fn factory_class<'t>(
    cx: &FileCx<'_>,
    root: TsNode<'t>,
    function: TsNode<'t>,
    body: TsNode<'t>,
) -> Option<TsNode<'t>> {
    let parameters = defaulted_parameters(cx, root, function)?;
    let mut rebound_names = BTreeSet::new();
    binding_sites(cx, body, &mut |_, bound| {
        rebound_names.extend(bound.iter().cloned())
    });
    let mut classes = Vec::new();
    let mut other = false;
    visit_scope(body, &mut |node| {
        if node.kind() != "return_statement" {
            return;
        }
        let instantiated = node
            .named_child(0)
            .map(unparenthesize)
            .filter(|value| value.kind() == "call")
            .and_then(|call| {
                let callee = call.child_by_field_name("function")?;
                let arguments = call.child_by_field_name("arguments")?;
                let passed = named(arguments).into_iter().all(|argument| {
                    let value = match argument.kind() {
                        "comment" => return true,
                        "keyword_argument" => argument.child_by_field_name("value"),
                        _ => Some(argument),
                    };
                    value.map(unparenthesize).is_some_and(|value| {
                        inert(cx, root, value)
                            || (value.kind() == "identifier"
                                && parameters.contains(cx.text(&value))
                                && !rebound_names.contains(cx.text(&value)))
                    })
                });
                // The class name is the module's, neither a parameter nor
                // rebound in the factory.
                let class = cx.text(&callee);
                (callee.kind() == "identifier"
                    && arguments.kind() == "argument_list"
                    && passed
                    && !parameters.contains(class)
                    && !rebound_names.contains(class))
                .then(|| class.to_string())
            });
        match instantiated {
            Some(class) => classes.push(class),
            None => other = true,
        }
    });
    let first = classes.first()?.clone();
    if other || classes.iter().any(|class| *class != first) {
        return None;
    }
    let writes = module_binding(cx, root, &first).writes;
    let [definition] = writes.as_slice() else {
        return None;
    };
    (definition.kind() == "class_definition"
        && definition
            .parent()
            .is_some_and(|parent| parent.kind() == "module"))
    .then_some(*definition)
}

/// Whether the module names `class` anywhere but its definition and the
/// calls returned by the factory `body`.
fn class_escapes(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    class: TsNode<'_>,
    body: TsNode<'_>,
    name: &str,
) -> bool {
    let mut allowed = Vec::new();
    allowed.extend(class.child_by_field_name("name"));
    visit_scope(body, &mut |node| {
        if node.kind() == "return_statement" {
            allowed.extend(
                node.named_child(0)
                    .map(unparenthesize)
                    .filter(|value| value.kind() == "call")
                    .and_then(|call| call.child_by_field_name("function")),
            );
        }
    });
    let mut escapes = false;
    every_descendant(root, &mut |node| {
        escapes |=
            node.kind() == "identifier" && cx.text(&node) == name && !allowed.contains(&node);
    });
    escapes
}

/// The parameter names of `function`, when each default is inert and none
/// is a `*` / `**` collector.
fn defaulted_parameters(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    function: TsNode<'_>,
) -> Option<BTreeSet<String>> {
    let parameters = function.child_by_field_name("parameters")?;
    let mut names = BTreeSet::new();
    for parameter in named(parameters) {
        match parameter.kind() {
            "comment" | "keyword_separator" | "positional_separator" => {}
            "identifier" | "typed_parameter" => {
                names.insert(cx.text(&parameter_name(parameter)?).to_string());
            }
            "default_parameter" | "typed_default_parameter" => {
                let value = parameter.child_by_field_name("value")?;
                if !inert(cx, root, unparenthesize(value)) {
                    return None;
                }
                names.insert(cx.text(&parameter.child_by_field_name("name")?).to_string());
            }
            _ => return None,
        }
    }
    Some(names)
}

/// The methods of an undecorated, base-less class whose body holds only
/// undecorated methods (no dunder but `__init__` / `__call__`), a docstring
/// or `pass`.
fn class_methods<'t>(cx: &FileCx<'_>, class: TsNode<'t>) -> Option<HashMap<String, TsNode<'t>>> {
    let based = class
        .child_by_field_name("superclasses")
        .is_some_and(|bases| named(bases).iter().any(|base| base.kind() != "comment"));
    if decorated(class) || based || class.child_by_field_name("type_parameters").is_some() {
        return None;
    }
    let mut methods = HashMap::new();
    for statement in named(class.child_by_field_name("body")?) {
        match statement.kind() {
            "comment" | "pass_statement" => {}
            "expression_statement"
                if statement
                    .named_child(0)
                    .is_some_and(|value| value.kind() == "string") => {}
            "function_definition" => {
                let name = cx.text(&statement.child_by_field_name("name")?).to_string();
                let dunder = name.starts_with("__") && name.ends_with("__");
                if (dunder && !matches!(name.as_str(), "__init__" | "__call__"))
                    || is_async(statement)
                    || methods.insert(name, statement).is_some()
                {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(methods)
}

/// The first parameter of a method, a plain name.
fn receiver_of(cx: &FileCx<'_>, method: TsNode<'_>) -> Option<String> {
    let parameters = method.child_by_field_name("parameters")?;
    let first = named(parameters)
        .into_iter()
        .find(|parameter| parameter.kind() != "comment")?;
    (first.kind() == "identifier").then(|| cx.text(&first).to_string())
}

/// Whether `node` is an attribute of identifier `receiver`, by attribute name.
fn receiver_attribute<'s>(cx: &'s FileCx<'_>, node: TsNode<'_>, receiver: &str) -> Option<&'s str> {
    if node.kind() != "attribute" {
        return None;
    }
    let object = node.child_by_field_name("object")?;
    let attribute = node.child_by_field_name("attribute")?;
    (object.kind() == "identifier" && cx.text(&object) == receiver).then(|| cx.text(&attribute))
}

/// Whether `receiver` appears in `body` other than as the object of an
/// attribute, so an alias or callee may reach methods the proof never follows.
fn receiver_escapes(cx: &FileCx<'_>, body: TsNode<'_>, receiver: &str) -> bool {
    let mut escapes = false;
    every_descendant(body, &mut |node| {
        if node.kind() == "identifier" && cx.text(&node) == receiver {
            escapes |= !node.parent().is_some_and(|parent| {
                parent.kind() == "attribute" && parent.child_by_field_name("object") == Some(node)
            });
        }
    });
    escapes
}

/// Whether `node` is the target of an assignment, augmented assignment,
/// loop, `with … as`, or `del`.
fn written(node: TsNode<'_>) -> bool {
    is_write_target(node)
        || node.parent().is_some_and(|parent| {
            (parent.kind() == "augmented_assignment"
                && parent.child_by_field_name("left") == Some(node))
                || matches!(parent.kind(), "as_pattern_target" | "as_pattern")
        })
}

struct Prover<'p, 'c, 't> {
    cx: &'p FileCx<'c>,
    root: TsNode<'t>,
    resolver: &'p Resolver<'p>,
    /// The instance class's methods, by name; empty for a bare decorator.
    methods: HashMap<String, TsNode<'t>>,
    calls: BTreeSet<FnKey>,
}

impl<'t> Prover<'_, '_, 't> {
    /// `__init__` only assigns `self.<attr>` (never a method's name) from an
    /// inert value or one of its parameters with an inert default or none;
    /// no other method writes an attribute of its receiver.
    fn instance_is_inert(&self) -> bool {
        let cx = self.cx;
        let mut ok = true;
        for (name, method) in &self.methods {
            let Some(receiver) = receiver_of(cx, *method) else {
                return false;
            };
            let Some(body) = method.child_by_field_name("body") else {
                return false;
            };
            if name == "__init__" {
                let Some(parameters) = defaulted_parameters(cx, self.root, *method) else {
                    return false;
                };
                for statement in named(body) {
                    let assigned = match statement.kind() {
                        "comment" | "pass_statement" => continue,
                        "expression_statement" => statement.named_child(0),
                        _ => None,
                    };
                    let Some(assigned) = assigned else {
                        return false;
                    };
                    if assigned.kind() == "string" {
                        continue;
                    }
                    let (Some(left), Some(right)) = (
                        assigned.child_by_field_name("left"),
                        assigned.child_by_field_name("right"),
                    ) else {
                        return false;
                    };
                    let right = unparenthesize(right);
                    let attribute = receiver_attribute(cx, left, &receiver);
                    ok &= assigned.kind() == "assignment"
                        && attribute.is_some_and(|attribute| !self.methods.contains_key(attribute))
                        && (inert(cx, self.root, right)
                            || (right.kind() == "identifier"
                                && parameters.contains(cx.text(&right))
                                && cx.text(&right) != receiver));
                }
            } else {
                every_descendant(body, &mut |node| {
                    if receiver_attribute(cx, node, &receiver).is_some() && written(node) {
                        ok = false;
                    }
                });
            }
        }
        ok
    }

    /// `__call__(self, f)` returns an inner wrapper that is proven clean,
    /// with neither name rebound in it.
    fn dunder_call(&mut self, call: TsNode<'t>) -> bool {
        let cx = self.cx;
        let parameters = plain_parameters(cx, call);
        let Some([receiver, wrapped]) = parameters.as_deref() else {
            return false;
        };
        let Some(body) = call.child_by_field_name("body") else {
            return false;
        };
        let mut rebinds = false;
        binding_sites(cx, body, &mut |_, bound| {
            rebinds |= bound.contains(receiver) || bound.contains(wrapped);
        });
        let receiver = receiver.clone();
        let wrapped = wrapped.clone();
        !rebinds
            && !receiver_escapes(cx, body, &receiver)
            && returned_wrapper(cx, body)
                .is_some_and(|wrapper| self.wrapper(wrapper, &wrapped, Some(&receiver)))
    }

    /// Whether `wrapper` ends a request only through `wrapped`, `None` or a
    /// non-auth werkzeug error. `receiver` is the instance it closes over.
    fn wrapper(&mut self, wrapper: TsNode<'t>, wrapped: &str, receiver: Option<&str>) -> bool {
        let cx = self.cx;
        if is_async(wrapper) {
            return false;
        }
        let Some(body) = wrapper.child_by_field_name("body") else {
            return false;
        };
        let bound = handler_bindings(cx, wrapper, body);
        if bound.contains(wrapped) || receiver.is_some_and(|receiver| bound.contains(receiver)) {
            return false;
        }
        // Only `@functools.wraps(<wrapped>)` may wrap the wrapper.
        let wrapped_only = wrapper
            .parent()
            .filter(|parent| parent.kind() == "decorated_definition")
            .is_none_or(|parent| {
                named(parent)
                    .into_iter()
                    .filter(|child| child.kind() == "decorator")
                    .all(|decorator| self.wraps(decorator, wrapped))
            });
        wrapped_only && self.body(body, &bound, Some(wrapped), receiver, &mut Vec::new())
    }

    /// Whether `decorator` is `@functools.wraps(<wrapped>)`, its name not rebound.
    fn wraps(&self, decorator: TsNode<'_>, wrapped: &str) -> bool {
        let cx = self.cx;
        let Some(call) = decorator
            .named_child(0)
            .map(unparenthesize)
            .filter(|expression| expression.kind() == "call")
        else {
            return false;
        };
        let Some(callee) = call.child_by_field_name("function") else {
            return false;
        };
        let arguments = call
            .child_by_field_name("arguments")
            .map(|arguments| {
                named(arguments)
                    .into_iter()
                    .filter(|argument| argument.kind() != "comment")
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        import_dotted(cx, callee, self.resolver.bindings).as_deref() == Some("functools.wraps")
            && dotted_root(cx, callee)
                .is_some_and(|name| !rebound(cx, self.root, &name, Some(decorator)))
            && matches!(arguments.as_slice(), [argument] if argument.kind() == "identifier"
                && cx.text(argument) == wrapped)
    }

    /// Whether `body` is proven. `wrapped` is `Some` in the wrapper, whose
    /// returns are responses; a method's returns are values.
    fn body(
        &mut self,
        body: TsNode<'t>,
        bound: &BTreeSet<String>,
        wrapped: Option<&str>,
        receiver: Option<&str>,
        visiting: &mut Vec<String>,
    ) -> bool {
        let cx = self.cx;
        let mut ok = true;
        let mut methods = Vec::new();
        every_descendant(body, &mut |node| match node.kind() {
            "return_statement" => {
                if let Some(wrapped) = wrapped {
                    ok &= node
                        .named_child(0)
                        .filter(|value| value.kind() != "comment")
                        .map(unparenthesize)
                        .is_none_or(|value| {
                            value.kind() == "none"
                                || (value.kind() == "call"
                                    && value.child_by_field_name("function").is_some_and(
                                        |callee| {
                                            callee.kind() == "identifier"
                                                && cx.text(&callee) == wrapped
                                        },
                                    ))
                        });
                }
            }
            "yield" | "await" => ok = false,
            "raise_statement" => ok &= allowed_raise(cx, self.root, node, self.resolver),
            "call" => {
                let Some(callee) = node.child_by_field_name("function") else {
                    ok = false;
                    return;
                };
                ok &= matches!(callee.kind(), "identifier" | "attribute")
                    && !denying_call(cx, callee, self.resolver);
            }
            "attribute" => {
                if let Some(receiver) = receiver
                    && let Some(name) = receiver_attribute(cx, node, receiver)
                {
                    if written(node) {
                        ok = false;
                    } else if self.methods.contains_key(name) {
                        // A method is only called here, never passed on.
                        let called = node.parent().is_some_and(|parent| {
                            parent.kind() == "call"
                                && parent.child_by_field_name("function") == Some(node)
                        });
                        if called {
                            methods.push(name.to_string());
                        } else {
                            ok = false;
                        }
                    }
                } else if let Some(key) = self.resolver.target(cx, node, bound) {
                    self.calls.insert(key);
                }
            }
            "identifier" => {
                let field = node.parent().is_some_and(|parent| {
                    parent.kind() == "attribute"
                        && parent.child_by_field_name("attribute") == Some(node)
                });
                if !field
                    && Some(cx.text(&node)) != wrapped
                    && let Some(key) = self.resolver.target(cx, node, bound)
                {
                    self.calls.insert(key);
                }
            }
            _ => {}
        });
        for name in methods {
            if !ok {
                break;
            }
            ok &= self.method(&name, visiting);
        }
        ok
    }

    /// Whether the class's method `name` is proven as a callee.
    fn method(&mut self, name: &str, visiting: &mut Vec<String>) -> bool {
        if visiting.len() >= MAX_METHOD_DEPTH || visiting.iter().any(|seen| seen == name) {
            return false;
        }
        let Some(method) = self.methods.get(name).copied() else {
            return false;
        };
        let (Some(receiver), Some(body)) = (
            receiver_of(self.cx, method),
            method.child_by_field_name("body"),
        ) else {
            return false;
        };
        let bound = handler_bindings(self.cx, method, body);
        let mut rebinds = false;
        binding_sites(self.cx, body, &mut |_, names| {
            rebinds |= names.contains(&receiver)
        });
        if rebinds || receiver_escapes(self.cx, body, &receiver) {
            return false;
        }
        visiting.push(name.to_string());
        let ok = self.body(body, &bound, None, Some(&receiver), visiting);
        visiting.pop();
        ok
    }
}
