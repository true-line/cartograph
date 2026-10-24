//! Flask responses a route handler returns (#721): the status codes and the
//! top-level JSON keys its own `return` statements prove.
//!
//! A return is read as `body` or `(body, status[, headers])`:
//!
//! - `body` is `json` when it is a call of Flask's `jsonify` (keyword names,
//!   or the literal keys of one dict argument) or a dict literal, which Flask
//!   serializes; its literal string keys are recorded. A call of Flask's
//!   `render_template` is `html`, and of `redirect` is `redirect`.
//! - `status` is a literal integer. Without one, `json` and `html` are 200,
//!   and `redirect` is its literal `code` (302 when absent).
//!
//! Anything else adds a reason to `partial`: a JSON body built elsewhere
//! (`jsonify(**payload)`, a computed key) is `response_built_elsewhere`, any
//! other body (a variable, another call) is `response_not_proven`, a
//! non-literal status is `status_computed`. `abort(…)` and errors raised are
//! not responses here, and each Flask name must be imported and not rebound
//! in the handler. Every recorded kind, status and key cites its `return`.
//!
//! Repository functions are followed after every file is read (#940), and
//! each one followed is listed in `followed`:
//!
//! - A bare `@decorator` on the handler whose body defines a wrapper and
//!   returns it adds the wrapper's own responses: the call of the wrapped
//!   function passes through, `return None` is not a response, and
//!   `x = check(…); … return x` adds `check`'s responses. A decorator outside
//!   the repository, or a call such as `@limiter.limit("2/minute")`, is not
//!   followed.
//! - A dict is built by a local bound once to a dict literal and extended
//!   only with literal-key subscript assignments (`ret["k"] = …`). Its keys
//!   are read from `jsonify(ret)` / `return ret`, and from
//!   `jsonify(helper(…))` / `jsonify(**helper(…))` when every return of the
//!   repository `helper` is such a dict or a dict literal; an unproven helper
//!   is `response_built_elsewhere`.
use crate::request_inputs::{
    binding_sites, handler_bindings, is_write_target, parameter_name, visit_scope,
};
use crate::{Extraction, FileCx, ImportBinding, literal_string, module_file, prov_from, symbol_id};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

/// Evidence spans kept per handler; more set `evidence_truncated`.
const MAX_EVIDENCE: usize = 64;

/// Follows chained through decorators and their checks before giving up.
const MAX_DEPTH: usize = 4;

/// Responses of one handler.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Responses {
    pub kinds: BTreeSet<&'static str>,
    pub statuses: BTreeSet<u16>,
    pub keys: BTreeSet<String>,
    pub partial: BTreeSet<&'static str>,
    /// Symbols of the repository functions followed.
    pub followed: BTreeSet<String>,
    pub evidence: Vec<EvidenceRef>,
}

impl Responses {
    /// The endpoint props value, or `None` when no return was read.
    pub(crate) fn to_props(&self, handler: &str) -> Option<serde_json::Value> {
        if self.kinds.is_empty() && self.partial.is_empty() {
            return None;
        }
        let mut props = serde_json::Map::new();
        if !self.kinds.is_empty() {
            props.insert("kinds".into(), serde_json::json!(self.kinds));
        }
        if !self.statuses.is_empty() {
            props.insert("statuses".into(), serde_json::json!(self.statuses));
        }
        if !self.keys.is_empty() {
            props.insert("keys".into(), serde_json::json!(self.keys));
        }
        if !self.partial.is_empty() {
            props.insert("partial".into(), serde_json::json!(self.partial));
        }
        if self.followed.is_empty() {
            props.insert("scope".into(), "handler_body".into());
        } else {
            props.insert("followed".into(), serde_json::json!(self.followed));
            props.insert("scope".into(), "handler_body_and_followed".into());
        }
        if !self.evidence.is_empty() {
            let mut evidence = self.evidence.clone();
            evidence.sort_by(|a, b| {
                (&a.path, a.byte_start, a.byte_end).cmp(&(&b.path, b.byte_start, b.byte_end))
            });
            if evidence.len() > MAX_EVIDENCE {
                evidence.truncate(MAX_EVIDENCE);
                props.insert("evidence_truncated".into(), true.into());
            }
            props.insert(
                "prov".into(),
                prov_from(
                    evidence,
                    ConfidenceTier::Confirmed,
                    &format!("Responses of {handler}"),
                ),
            );
        }
        Some(serde_json::Value::Object(props))
    }

    fn cite(&mut self, evidence: EvidenceRef) {
        if !self.evidence.iter().any(|seen| {
            (&seen.path, seen.byte_start, seen.byte_end)
                == (&evidence.path, evidence.byte_start, evidence.byte_end)
        }) {
            self.evidence.push(evidence);
        }
    }

    fn absorb(&mut self, other: Responses) {
        self.kinds.extend(other.kinds);
        self.statuses.extend(other.statuses);
        self.keys.extend(other.keys);
        self.partial.extend(other.partial);
        self.followed.extend(other.followed);
        for evidence in other.evidence {
            self.cite(evidence);
        }
    }
}

/// A repository function, as `(file, top-level name)`.
pub(crate) type FnKey = (String, String);

/// A repository function a response is followed into.
#[derive(Debug, Clone)]
pub(crate) enum Follow {
    /// `@decorator` on the handler: its wrapper's responses.
    Decorator(FnKey),
    /// `x = check(…); … return x` in a wrapper: `check`'s responses.
    Delegate(FnKey),
    /// `jsonify(helper(…))`: the keys of the dict `helper` builds.
    Built(FnKey),
}

/// Responses read from one function, before its follows are resolved.
#[derive(Debug, Clone, Default)]
pub(crate) struct Facts {
    pub responses: Responses,
    pub follows: Vec<Follow>,
}

/// What a top-level function proves when another one follows it.
#[derive(Debug, Clone)]
pub(crate) struct FunctionFacts {
    pub key: FnKey,
    /// Its wrapper's responses, when it is a decorator.
    pub wrapper: Option<Facts>,
    /// Its responses, `return None` aside, when a wrapper returns its result.
    pub returns: Option<Facts>,
    /// The literal keys of the dict every return builds, with their evidence.
    pub built: Option<(Vec<String>, Vec<EvidenceRef>)>,
}

/// A handler whose responses follow repository functions.
#[derive(Debug, Clone)]
pub(crate) struct HandlerFacts {
    pub handler: String,
    pub facts: Facts,
}

/// Resolves a callee or decorator to the repository function it names.
pub(crate) struct Resolver<'a> {
    pub bindings: &'a HashMap<String, ImportBinding>,
    /// Top-level function names defined in this file.
    pub locals: &'a HashMap<String, String>,
}

impl Resolver<'_> {
    fn target(&self, cx: &FileCx<'_>, node: TsNode<'_>, bound: &BTreeSet<String>) -> Option<FnKey> {
        match node.kind() {
            "identifier" => {
                let name = cx.text(&node);
                if bound.contains(name) {
                    return None;
                }
                match self.bindings.get(name) {
                    Some(ImportBinding::Symbol {
                        target_file: Some(target),
                        exported,
                        ..
                    }) => Some((target.clone(), exported.clone())),
                    Some(_) => None,
                    None => self
                        .locals
                        .contains_key(name)
                        .then(|| (cx.path.to_string(), name.to_string())),
                }
            }
            "attribute" => {
                let object = node.child_by_field_name("object")?;
                let attribute = node.child_by_field_name("attribute")?;
                if object.kind() != "identifier" || bound.contains(cx.text(&object)) {
                    return None;
                }
                match self.bindings.get(cx.text(&object))? {
                    ImportBinding::Module { module } => Some((
                        module_file(cx.path, module)?,
                        cx.text(&attribute).to_string(),
                    )),
                    ImportBinding::Symbol { .. } => None,
                }
            }
            _ => None,
        }
    }
}

/// A Flask response helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Helper {
    Jsonify,
    RenderTemplate,
    Redirect,
}

const HELPERS: &[(&str, Helper)] = &[
    ("jsonify", Helper::Jsonify),
    ("render_template", Helper::RenderTemplate),
    ("redirect", Helper::Redirect),
];

/// Local names (or `flask.x` prefixes) of Flask's response helpers, or `None`
/// when the file imports nothing from Flask: a FastAPI handler's tuple is not
/// a `(body, status)` response.
pub(crate) fn flask_helpers(
    bindings: &HashMap<String, ImportBinding>,
) -> Option<BTreeMap<String, Helper>> {
    let flask = bindings.values().any(|binding| match binding {
        ImportBinding::Module { module } | ImportBinding::Symbol { module, .. } => {
            module == "flask" || module.starts_with("flask.")
        }
    });
    if !flask {
        return None;
    }
    let mut helpers = BTreeMap::new();
    for (name, binding) in bindings {
        match binding {
            ImportBinding::Symbol {
                module, exported, ..
            } if module == "flask" => {
                if let Some((_, helper)) = HELPERS.iter().find(|(n, _)| n == exported) {
                    helpers.insert(name.clone(), *helper);
                }
            }
            ImportBinding::Module { module } if module == "flask" => {
                for (helper_name, helper) in HELPERS {
                    helpers.insert(format!("{name}.{helper_name}"), *helper);
                }
            }
            _ => {}
        }
    }
    Some(helpers)
}

/// The bare repository decorators of a `decorated_definition`.
pub(crate) fn decorator_follows(
    cx: &FileCx<'_>,
    definition: TsNode<'_>,
    resolver: &Resolver<'_>,
) -> Vec<Follow> {
    named(definition)
        .into_iter()
        .filter(|child| child.kind() == "decorator")
        .filter_map(|decorator| decorator.named_child(0))
        .filter_map(|expression| resolver.target(cx, expression, &BTreeSet::new()))
        .map(Follow::Decorator)
        .collect()
}

/// How a function's returns are read.
enum Mode<'m> {
    /// A route handler: every return is its response.
    Handler,
    /// A decorator's wrapper: calling the wrapped function passes through.
    Wrapper { wrapped: &'m str },
    /// A check a wrapper returns the result of: `None` is no response.
    Check,
}

struct Reader<'r, 'c, 't> {
    cx: &'r FileCx<'c>,
    helpers: BTreeMap<String, Helper>,
    resolver: &'r Resolver<'r>,
    bound: BTreeSet<String>,
    body: TsNode<'t>,
    mode: Mode<'r>,
}

/// Responses `function` (a `function_definition`) returns as a handler.
pub(crate) fn handler_facts(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    helpers: &BTreeMap<String, Helper>,
    resolver: &Resolver<'_>,
) -> Facts {
    read_function(cx, function, helpers, resolver, Mode::Handler)
}

/// What the top-level `function` proves when followed; `helpers` is `None`
/// outside a Flask file, where only a built dict is read.
pub(crate) fn function_facts(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    helpers: Option<&BTreeMap<String, Helper>>,
    resolver: &Resolver<'_>,
) -> Option<FunctionFacts> {
    let name = cx.text(&function.child_by_field_name("name")?).to_string();
    let built = built_keys(cx, function);
    let (wrapper, returns) = match helpers {
        Some(helpers) => (
            wrapper_facts(cx, function, helpers, resolver),
            Some(read_function(cx, function, helpers, resolver, Mode::Check))
                .filter(|facts| !facts.responses.kinds.is_empty() || !facts.follows.is_empty()),
        ),
        None => (None, None),
    };
    (wrapper.is_some() || returns.is_some() || built.is_some()).then(|| FunctionFacts {
        key: (cx.path.to_string(), name),
        wrapper,
        returns,
        built,
    })
}

/// `def decorator(f): def wrapper(…): …; return wrapper`: the wrapper's
/// responses.
fn wrapper_facts(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    helpers: &BTreeMap<String, Helper>,
    resolver: &Resolver<'_>,
) -> Option<Facts> {
    let parameters = function.child_by_field_name("parameters")?;
    let parameters = named(parameters);
    let [wrapped] = parameters.as_slice() else {
        return None;
    };
    let wrapped = parameter_name(*wrapped)?;
    let body = function.child_by_field_name("body")?;
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
    let mut wrapper = None;
    visit_scope(body, &mut |node| {
        if node.kind() == "return_statement"
            && let Some(value) = node.named_child(0)
            && value.kind() == "identifier"
            && let Some(definition) = inner.get(cx.text(&value))
        {
            wrapper = Some(*definition);
        }
    });
    let mode = Mode::Wrapper {
        wrapped: cx.text(&wrapped),
    };
    Some(read_function(cx, wrapper?, helpers, resolver, mode))
}

fn read_function(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    helpers: &BTreeMap<String, Helper>,
    resolver: &Resolver<'_>,
    mode: Mode<'_>,
) -> Facts {
    let mut facts = Facts::default();
    let Some(body) = function.child_by_field_name("body") else {
        return facts;
    };
    let bound = handler_bindings(cx, function, body);
    let helpers = helpers
        .iter()
        .filter(|(name, _)| !bound.contains(name.split('.').next().unwrap_or(name)))
        .map(|(name, helper)| (name.clone(), *helper))
        .collect::<BTreeMap<_, _>>();
    let reader = Reader {
        cx,
        helpers,
        resolver,
        bound,
        body,
        mode,
    };
    visit_scope(body, &mut |node| {
        if node.kind() == "return_statement" {
            reader.read_return(node, &mut facts);
        }
    });
    facts
}

impl Reader<'_, '_, '_> {
    fn read_return(&self, statement: TsNode<'_>, facts: &mut Facts) {
        let cx = self.cx;
        let Some(value) = statement.named_child(0) else {
            return; // a bare `return` is not a Flask response
        };
        let value = unparenthesize(value);
        if !matches!(self.mode, Mode::Handler) {
            if value.kind() == "none" {
                return;
            }
            if let Mode::Wrapper { wrapped } = self.mode
                && value.kind() == "call"
                && value
                    .child_by_field_name("function")
                    .is_some_and(|function| cx.text(&function) == wrapped)
            {
                return;
            }
            if let Some(check) = self.delegate(value) {
                facts.follows.push(Follow::Delegate(check));
                facts.responses.cite(cx.evidence(&statement));
                return;
            }
        }
        let responses = &mut facts.responses;
        let (body, status) = match value.kind() {
            "expression_list" | "tuple" if (2..=3).contains(&value.named_child_count()) => {
                (value.named_child(0), value.named_child(1))
            }
            _ => (Some(value), None),
        };
        let Some(body) = body.map(unparenthesize) else {
            return;
        };
        let mut recorded = false;
        let default = match self.classify(body) {
            Body::Json(keys, evidence) => {
                responses.keys.extend(keys);
                for evidence in evidence {
                    responses.cite(evidence);
                }
                Some(("json", Some(200)))
            }
            Body::JsonElsewhere => {
                responses.partial.insert("response_built_elsewhere");
                Some(("json", Some(200)))
            }
            Body::Built(helper) => {
                facts.follows.push(Follow::Built(helper));
                Some(("json", Some(200)))
            }
            Body::Html => Some(("html", Some(200))),
            Body::Redirect(code) => {
                if code.is_none() {
                    responses.partial.insert("status_computed");
                }
                Some(("redirect", code))
            }
            Body::Unknown => {
                responses.partial.insert("response_not_proven");
                None
            }
        };
        if let Some((kind, _)) = default {
            responses.kinds.insert(kind);
            recorded = true;
        }
        match status {
            Some(status) => match literal_status(cx, status) {
                Some(code) => {
                    responses.statuses.insert(code);
                    recorded = true;
                }
                None => {
                    responses.partial.insert("status_computed");
                }
            },
            None => {
                if let Some((_, Some(code))) = default {
                    responses.statuses.insert(code);
                }
            }
        }
        if recorded {
            responses.cite(cx.evidence(&statement));
        }
    }

    /// `x` bound once in the function, to a call of a repository function.
    fn delegate(&self, value: TsNode<'_>) -> Option<FnKey> {
        if value.kind() != "identifier" {
            return None;
        }
        let name = self.cx.text(&value);
        let mut writes = Vec::new();
        binding_sites(self.cx, self.body, &mut |node, bound| {
            if bound.contains(name) {
                writes.push(node);
            }
        });
        let [write] = writes.as_slice() else {
            return None;
        };
        let call = (write.kind() == "assignment")
            .then(|| write.child_by_field_name("left"))
            .flatten()
            .filter(|left| left.kind() == "identifier")
            .and_then(|_| write.child_by_field_name("right"))
            .map(unparenthesize)
            .filter(|right| right.kind() == "call")?;
        let mut bound = self.bound.clone();
        bound.remove(name);
        self.resolver
            .target(self.cx, call.child_by_field_name("function")?, &bound)
    }

    fn classify(&self, body: TsNode<'_>) -> Body {
        let cx = self.cx;
        match body.kind() {
            "dictionary" => return dict_keys(cx, body),
            "identifier" => {
                if let Some((keys, evidence)) = built_dict(cx, self.body, body) {
                    return Body::Json(keys, evidence);
                }
            }
            _ => {}
        }
        let helper = (body.kind() == "call")
            .then(|| body.child_by_field_name("function"))
            .flatten()
            .and_then(|function| self.helpers.get(cx.text(&function)));
        match helper {
            Some(Helper::Jsonify) => self.jsonify_keys(body),
            Some(Helper::RenderTemplate) => Body::Html,
            Some(Helper::Redirect) => Body::Redirect(redirect_code(cx, body)),
            None => Body::Unknown,
        }
    }

    /// `jsonify(k=…)` keyword names, or the keys of one dict argument: a
    /// literal, a built local, or what a repository helper builds.
    fn jsonify_keys(&self, body: TsNode<'_>) -> Body {
        let cx = self.cx;
        let Some(arguments) = body.child_by_field_name("arguments") else {
            return Body::JsonElsewhere;
        };
        let arguments = named(arguments);
        if let [only] = arguments.as_slice()
            && only.kind() != "keyword_argument"
        {
            let dict = match only.kind() {
                "dictionary_splat" => only.named_child(0),
                _ => Some(*only),
            };
            let Some(dict) = dict.map(unparenthesize) else {
                return Body::JsonElsewhere;
            };
            return match dict.kind() {
                "dictionary" => dict_keys(cx, dict),
                "identifier" => match built_dict(cx, self.body, dict) {
                    Some((keys, evidence)) => Body::Json(keys, evidence),
                    None => Body::JsonElsewhere,
                },
                "call" => dict
                    .child_by_field_name("function")
                    .and_then(|function| self.resolver.target(cx, function, &self.bound))
                    .map_or(Body::JsonElsewhere, Body::Built),
                _ => Body::JsonElsewhere,
            };
        }
        let mut keys = Vec::new();
        for argument in arguments {
            match argument.kind() {
                "keyword_argument" => {
                    if let Some(name) = argument.child_by_field_name("name") {
                        keys.push(cx.text(&name).to_string());
                    }
                }
                _ => return Body::JsonElsewhere,
            }
        }
        Body::Json(keys, Vec::new())
    }
}

enum Body {
    /// Proven JSON with these literal top-level keys, and the evidence of
    /// keys proven outside the `return`.
    Json(Vec<String>, Vec<EvidenceRef>),
    /// JSON whose keys are built outside the expression.
    JsonElsewhere,
    /// JSON a repository helper builds.
    Built(FnKey),
    Html,
    /// A redirect with its literal status, `None` when computed.
    Redirect(Option<u16>),
    Unknown,
}

fn named(node: TsNode<'_>) -> Vec<TsNode<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// The keys of the dict every return of `function` builds.
fn built_keys(cx: &FileCx<'_>, function: TsNode<'_>) -> Option<(Vec<String>, Vec<EvidenceRef>)> {
    let body = function.child_by_field_name("body")?;
    let mut keys = Vec::new();
    let mut evidence = Vec::new();
    let mut proven = true;
    visit_scope(body, &mut |node| {
        if node.kind() != "return_statement" || !proven {
            return;
        }
        let value = node.named_child(0).map(unparenthesize);
        let built = match value.map(|value| (value.kind(), value)) {
            Some(("dictionary", dict)) => match dict_keys(cx, dict) {
                Body::Json(keys, _) => Some((keys, Vec::new())),
                _ => None,
            },
            Some(("identifier", name)) => built_dict(cx, body, name),
            _ => None,
        };
        match built {
            Some((built, cited)) => {
                keys.extend(built);
                evidence.extend(cited);
                evidence.push(cx.evidence(&node));
            }
            None => proven = false,
        }
    });
    (proven && !evidence.is_empty()).then_some((keys, evidence))
}

/// The dict `used` (an identifier read in `scope`) names, when it is bound
/// once in `scope` to a dict literal and otherwise only read by subscript,
/// returned, or extended by `name["k"] = …`: its literal keys and the
/// statements proving them. Any other use (an alias, an argument, a method
/// call, `del`, a closure) may change it.
fn built_dict(
    cx: &FileCx<'_>,
    scope: TsNode<'_>,
    used: TsNode<'_>,
) -> Option<(Vec<String>, Vec<EvidenceRef>)> {
    let name = cx.text(&used);
    let mut sites = Vec::new();
    binding_sites(cx, scope, &mut |node, bound| {
        if bound.contains(name) {
            sites.push(node);
        }
    });
    let [init] = sites.as_slice() else {
        return None;
    };
    let left = init.child_by_field_name("left")?;
    let right = init.child_by_field_name("right").map(unparenthesize)?;
    if init.kind() != "assignment" || left.kind() != "identifier" || right.kind() != "dictionary" {
        return None;
    }
    let Body::Json(mut keys, _) = dict_keys(cx, right) else {
        return None;
    };
    let mut evidence = vec![cx.evidence(init)];
    let mut proven = true;
    every_descendant(scope, &mut |node| {
        if !proven || node.kind() != "identifier" || cx.text(&node) != name || node == left {
            return;
        }
        let Some(parent) = node.parent() else {
            return;
        };
        let field = |field: &str| parent.child_by_field_name(field) == Some(node);
        match parent.kind() {
            // `x.name`, `f(name=…)`: another name.
            "attribute" if field("attribute") => {}
            "keyword_argument" if field("name") => {}
            "return_statement" => {}
            _ if node == used => {}
            "subscript" if field("value") => {
                let assigned = parent.parent().filter(|statement| {
                    statement.kind() == "assignment"
                        && statement.child_by_field_name("left") == Some(parent)
                });
                match assigned {
                    Some(statement) => match parent
                        .child_by_field_name("subscript")
                        .and_then(|key| literal_string(cx, key))
                    {
                        Some(key) => {
                            keys.push(key);
                            evidence.push(cx.evidence(&statement));
                        }
                        None => proven = false,
                    },
                    None => proven = !is_write_target(parent),
                }
            }
            _ => proven = false,
        }
    });
    proven.then_some((keys, evidence))
}

/// Every node under `node`, nested scopes included.
fn every_descendant<'t>(node: TsNode<'t>, f: &mut dyn FnMut(TsNode<'t>)) {
    f(node);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        every_descendant(child, f);
    }
}

/// Merge every handler's follows into its Flask endpoints, once all files
/// are read.
pub(crate) fn resolve(out: &mut Extraction, repo: &str) {
    let handlers = std::mem::take(&mut out.handler_facts);
    let functions = std::mem::take(&mut out.function_facts)
        .into_iter()
        .map(|facts| (facts.key.clone(), facts))
        .collect::<HashMap<_, _>>();
    if handlers.is_empty() {
        return;
    }
    let resolved = handlers
        .into_iter()
        .map(|handler| {
            let responses = merge(&handler.facts, &functions, repo, 0);
            (handler.handler, responses)
        })
        .collect::<HashMap<_, _>>();
    for node in &mut out.nodes {
        if node.label != "Endpoint" || node.props["framework"] != "flask" {
            continue;
        }
        let Some(handler) = node.props["handler_sym"].as_str().map(str::to_string) else {
            continue;
        };
        let Some(responses) = resolved.get(&handler) else {
            continue;
        };
        match responses.to_props(&handler) {
            Some(props) => node.props["responses"] = props,
            None => {
                if let Some(props) = node.props.as_object_mut() {
                    props.remove("responses");
                }
            }
        }
    }
}

fn lookup<'f>(
    functions: &'f HashMap<FnKey, FunctionFacts>,
    (file, name): &FnKey,
) -> Option<&'f FunctionFacts> {
    functions.get(&(file.clone(), name.clone())).or_else(|| {
        let package = format!("{}/__init__.py", file.strip_suffix(".py")?);
        functions.get(&(package, name.clone()))
    })
}

fn merge(
    facts: &Facts,
    functions: &HashMap<FnKey, FunctionFacts>,
    repo: &str,
    depth: usize,
) -> Responses {
    let mut responses = facts.responses.clone();
    for follow in &facts.follows {
        let (key, found, unproven) = match follow {
            Follow::Decorator(key) => {
                // A decorator not read as a wrapper is not followed.
                let Some(wrapper) = lookup(functions, key).and_then(|f| f.wrapper.as_ref()) else {
                    continue;
                };
                (key, Some(wrapper), "response_not_proven")
            }
            Follow::Delegate(key) => (
                key,
                lookup(functions, key).and_then(|f| f.returns.as_ref()),
                "response_not_proven",
            ),
            Follow::Built(key) => {
                match lookup(functions, key).and_then(|f| f.built.as_ref()) {
                    Some((keys, evidence)) => {
                        responses.keys.extend(keys.iter().cloned());
                        for evidence in evidence {
                            responses.cite(evidence.clone());
                        }
                        responses.followed.insert(symbol_id(repo, &key.0, &key.1));
                    }
                    None => {
                        responses.partial.insert("response_built_elsewhere");
                    }
                }
                continue;
            }
        };
        match found.filter(|_| depth < MAX_DEPTH) {
            Some(found) => {
                responses.absorb(merge(found, functions, repo, depth + 1));
                responses.followed.insert(symbol_id(repo, &key.0, &key.1));
            }
            None => {
                responses.partial.insert(unproven);
            }
        }
    }
    responses
}

/// Retarget the evidence a cached file's follows carry to `commit`.
pub(crate) fn retarget(extraction: &mut Extraction, commit: &str) {
    let mut facts = extraction
        .handler_facts
        .iter_mut()
        .map(|handler| &mut handler.facts)
        .collect::<Vec<_>>();
    for function in &mut extraction.function_facts {
        facts.extend(function.wrapper.as_mut());
        facts.extend(function.returns.as_mut());
        if let Some((_, evidence)) = &mut function.built {
            for evidence in evidence {
                evidence.commit_sha = commit.into();
            }
        }
    }
    for facts in facts {
        for evidence in &mut facts.responses.evidence {
            evidence.commit_sha = commit.into();
        }
    }
}

/// `redirect(location, code=302)`: the literal code, 302 when absent.
fn redirect_code(cx: &FileCx<'_>, call: TsNode<'_>) -> Option<u16> {
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let mut code = None;
    for (index, argument) in arguments.named_children(&mut cursor).enumerate() {
        match argument.kind() {
            "keyword_argument"
                if argument
                    .child_by_field_name("name")
                    .is_some_and(|name| cx.text(&name) == "code") =>
            {
                code = Some(argument.child_by_field_name("value")?);
            }
            "keyword_argument" => {}
            "list_splat" | "dictionary_splat" => return None,
            _ if index == 1 => code = Some(argument),
            _ => {}
        }
    }
    match code {
        Some(code) => literal_status(cx, code),
        None => Some(302),
    }
}

fn dict_keys(cx: &FileCx<'_>, dict: TsNode<'_>) -> Body {
    let mut keys = Vec::new();
    let mut cursor = dict.walk();
    for entry in dict.named_children(&mut cursor) {
        let key = (entry.kind() == "pair")
            .then(|| entry.child_by_field_name("key"))
            .flatten()
            .and_then(|key| literal_string(cx, key));
        match key {
            Some(key) => keys.push(key),
            None if entry.kind() == "comment" => {}
            None => return Body::JsonElsewhere,
        }
    }
    Body::Json(keys, Vec::new())
}

fn literal_status(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<u16> {
    let node = unparenthesize(node);
    (node.kind() == "integer")
        .then(|| cx.text(&node).parse::<u16>().ok())
        .flatten()
        .filter(|code| (100..=599).contains(code))
}

fn unparenthesize(mut node: TsNode<'_>) -> TsNode<'_> {
    while node.kind() == "parenthesized_expression"
        && let Some(inner) = node.named_child(0)
        && node.named_child_count() == 1
    {
        node = inner;
    }
    node
}
