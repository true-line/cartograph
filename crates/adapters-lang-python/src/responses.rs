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
//! (`jsonify(user_to_dict(u))`, `jsonify(**payload)`, a computed key) is
//! `response_built_elsewhere`, any other body (a variable, another call) is
//! `response_not_proven`, a non-literal status is `status_computed`. Helpers
//! and decorators (an auth wrapper's 401) are not followed, `abort(…)` and
//! errors raised are not responses here, and each Flask name must be
//! imported and not rebound in the handler. Every recorded kind, status and
//! key cites its `return`.

use crate::request_inputs::{handler_bindings, visit_scope};
use crate::{FileCx, ImportBinding, literal_string, prov_from};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

/// Evidence spans kept per handler; more set `evidence_truncated`.
const MAX_EVIDENCE: usize = 64;

/// Responses of one handler.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Responses {
    pub kinds: BTreeSet<&'static str>,
    pub statuses: BTreeSet<u16>,
    pub keys: BTreeSet<String>,
    pub partial: BTreeSet<&'static str>,
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
        props.insert("scope".into(), "handler_body".into());
        if !self.evidence.is_empty() {
            let mut evidence = self.evidence.clone();
            evidence.sort_by_key(|e| (e.byte_start, e.byte_end));
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

/// Responses `function` (a `function_definition`) returns.
pub(crate) fn handler_responses(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    helpers: &BTreeMap<String, Helper>,
) -> Responses {
    let mut responses = Responses::default();
    let Some(body) = function.child_by_field_name("body") else {
        return responses;
    };
    let bound = handler_bindings(cx, function, body);
    let helpers = helpers
        .iter()
        .filter(|(name, _)| !bound.contains(name.split('.').next().unwrap_or(name)))
        .map(|(name, helper)| (name.clone(), *helper))
        .collect::<BTreeMap<_, _>>();
    visit_scope(body, &mut |node| {
        if node.kind() == "return_statement" {
            read_return(cx, node, &helpers, &mut responses);
        }
    });
    responses
}

fn read_return(
    cx: &FileCx<'_>,
    statement: TsNode<'_>,
    helpers: &BTreeMap<String, Helper>,
    responses: &mut Responses,
) {
    let Some(value) = statement.named_child(0) else {
        return; // a bare `return` is not a Flask response
    };
    let value = unparenthesize(value);
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
    let default = match classify(cx, body, helpers) {
        Body::Json(keys) => {
            responses.keys.extend(keys);
            Some(("json", Some(200)))
        }
        Body::JsonElsewhere => {
            responses.partial.insert("response_built_elsewhere");
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
        let evidence = cx.evidence(&statement);
        if !responses.evidence.iter().any(|seen| {
            (seen.byte_start, seen.byte_end) == (evidence.byte_start, evidence.byte_end)
        }) {
            responses.evidence.push(evidence);
        }
    }
}

enum Body {
    /// Proven JSON with these literal top-level keys.
    Json(Vec<String>),
    /// JSON whose keys are built outside the expression.
    JsonElsewhere,
    Html,
    /// A redirect with its literal status, `None` when computed.
    Redirect(Option<u16>),
    Unknown,
}

fn classify(cx: &FileCx<'_>, body: TsNode<'_>, helpers: &BTreeMap<String, Helper>) -> Body {
    if body.kind() == "dictionary" {
        return dict_keys(cx, body);
    }
    let helper = (body.kind() == "call")
        .then(|| body.child_by_field_name("function"))
        .flatten()
        .and_then(|function| helpers.get(cx.text(&function)));
    match helper {
        Some(Helper::Jsonify) => jsonify_keys(cx, body),
        Some(Helper::RenderTemplate) => Body::Html,
        Some(Helper::Redirect) => Body::Redirect(redirect_code(cx, body)),
        None => Body::Unknown,
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

/// `jsonify(k=…)` keyword names, or the literal keys of one dict argument.
fn jsonify_keys(cx: &FileCx<'_>, body: TsNode<'_>) -> Body {
    let Some(arguments) = body.child_by_field_name("arguments") else {
        return Body::JsonElsewhere;
    };
    let mut cursor = arguments.walk();
    let arguments = arguments.named_children(&mut cursor).collect::<Vec<_>>();
    if let [only] = arguments.as_slice()
        && only.kind() != "keyword_argument"
    {
        return match unparenthesize(*only).kind() {
            "dictionary" => dict_keys(cx, unparenthesize(*only)),
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
    Body::Json(keys)
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
    Body::Json(keys)
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
