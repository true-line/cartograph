//! Flask request inputs a route handler reads (#721): the literal names it
//! takes from the JSON body, the query string and the form, within the
//! handler's own body.
//!
//! Only reads the handler body proves are recorded:
//!
//! - `request.args.get("k")`, `request.args["k"]`, `request.args.getlist("k")`,
//!   `"k" in request.args` (query), the same on `request.form` (form) and `request.values` (both);
//! - `request.get_json(...)` or `request.json`, read directly with `.get("k")`
//!   or `["k"]`, or bound to a local name (`data = request.get_json() or {}`)
//!   that is only ever read that way (body).
//!
//! Any other use of a body name or of one of those collections (a computed key,
//! passing it to a call, iterating it, rebinding the name) adds a reason to
//! `partial`. So does passing `request` itself. Helpers the handler calls are
//! not followed, so the inputs never claim to be complete. `request` must be
//! Flask's: imported from `flask`, or `flask.request` on an imported module,
//! and not rebound in the handler (a parameter or local of that name).
//! Nested function and class bodies are outside the handler and are skipped.
//! Writing a key (`data["k"] = v`, `del data["k"]`) is not a read. Every
//! recorded name cites the expression that reads it, under `prov`.

use crate::{FileCx, ImportBinding, literal_string, prov_from};
use core_prov::{ConfidenceTier, EvidenceRef};
use std::collections::{BTreeSet, HashMap};
use tree_sitter::Node as TsNode;

/// Request inputs of one handler.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RequestInputs {
    pub query: BTreeSet<String>,
    pub body: BTreeSet<String>,
    pub form: BTreeSet<String>,
    pub partial: BTreeSet<&'static str>,
    /// The reads behind the recorded names, deduplicated by span.
    pub evidence: Vec<EvidenceRef>,
}

/// Evidence spans kept per handler; more set `evidence_truncated`.
const MAX_EVIDENCE: usize = 64;

impl RequestInputs {
    fn is_empty(&self) -> bool {
        self.query.is_empty()
            && self.body.is_empty()
            && self.form.is_empty()
            && self.partial.is_empty()
    }

    fn cite(&mut self, evidence: EvidenceRef) {
        if !self.evidence.iter().any(|seen| {
            (seen.byte_start, seen.byte_end) == (evidence.byte_start, evidence.byte_end)
        }) {
            self.evidence.push(evidence);
        }
    }

    /// The endpoint props value, or `None` when the handler reads nothing.
    pub(crate) fn to_props(&self, handler: &str) -> Option<serde_json::Value> {
        if self.is_empty() {
            return None;
        }
        let mut props = serde_json::Map::new();
        for (key, names) in [
            ("query", &self.query),
            ("body", &self.body),
            ("form", &self.form),
        ] {
            if !names.is_empty() {
                props.insert(key.into(), serde_json::json!(names));
            }
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
                    &format!("Request inputs of {handler}"),
                ),
            );
        }
        Some(serde_json::Value::Object(props))
    }
}

/// Local names that refer to Flask's `request` (or `flask.request` prefixes).
pub(crate) fn request_names(bindings: &HashMap<String, ImportBinding>) -> BTreeSet<String> {
    bindings
        .iter()
        .filter_map(|(name, binding)| match binding {
            ImportBinding::Symbol {
                module, exported, ..
            } if module == "flask" && exported == "request" => Some(name.clone()),
            ImportBinding::Module { module } if module == "flask" => {
                Some(format!("{name}.request"))
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Collection {
    Query,
    Form,
    Values,
}

const COLLECTIONS: &[(&str, Collection)] = &[
    ("args", Collection::Query),
    ("form", Collection::Form),
    ("values", Collection::Values),
];

/// Inputs `function` (a `function_definition`) reads from `request`.
pub(crate) fn handler_inputs(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    request: &BTreeSet<String>,
) -> RequestInputs {
    let mut inputs = RequestInputs::default();
    let Some(body) = function.child_by_field_name("body") else {
        return inputs;
    };
    // A handler parameter or local named like the import's root rebinds it.
    let bound = handler_bindings(cx, function, body);
    let request = request
        .iter()
        .filter(|name| !bound.contains(name.split('.').next().unwrap_or(name)))
        .cloned()
        .collect::<BTreeSet<_>>();
    if request.is_empty() {
        return inputs;
    }
    let walker = Walker {
        cx,
        request: &request,
    };
    // Pass 1: local names bound to the JSON body.
    let mut json_names = BTreeSet::new();
    walker.visit(body, &mut |node| {
        if node.kind() == "assignment"
            && let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            )
            && left.kind() == "identifier"
            && walker.is_json(unwrap_or(right))
        {
            json_names.insert(cx.text(&left).to_string());
        }
    });
    // Pass 2: every use of request and of those names.
    walker.visit(body, &mut |node| match node.kind() {
        "identifier" if json_names.contains(cx.text(&node)) => {
            walker.body_name_use(node, &mut inputs);
        }
        "attribute" | "identifier" if walker.is_request(node) => {
            walker.request_use(node, &json_names, &mut inputs);
        }
        _ => {}
    });
    inputs
}

/// Names the handler binds itself: parameters, and assignment, `for`, `with`,
/// `except`, `import` and `global`-free walrus targets in its own body.
fn handler_bindings(cx: &FileCx<'_>, function: TsNode<'_>, body: TsNode<'_>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(parameters) = function.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            let name = if parameter.kind() == "identifier" {
                Some(parameter)
            } else {
                parameter
                    .child_by_field_name("name")
                    .or_else(|| parameter.named_child(0))
                    .filter(|name| name.kind() == "identifier")
            };
            if let Some(name) = name {
                names.insert(cx.text(&name).to_string());
            }
        }
    }
    let walker = Walker {
        cx,
        request: &BTreeSet::new(),
    };
    walker.visit(body, &mut |node| {
        let target = match node.kind() {
            "assignment" | "augmented_assignment" | "for_statement" => {
                node.child_by_field_name("left")
            }
            "named_expression" => node.child_by_field_name("name"),
            "as_pattern" => node.child_by_field_name("alias"),
            "aliased_import" => node.child_by_field_name("alias"),
            "dotted_name"
                if node
                    .parent()
                    .is_some_and(|p| p.kind() == "import_statement") =>
            {
                node.named_child(0)
            }
            _ => None,
        };
        if let Some(target) = target {
            bound_names(cx, target, &mut names);
        }
    });
    names
}

fn bound_names(cx: &FileCx<'_>, target: TsNode<'_>, names: &mut BTreeSet<String>) {
    match target.kind() {
        "identifier" => {
            names.insert(cx.text(&target).to_string());
        }
        "pattern_list"
        | "tuple_pattern"
        | "list_pattern"
        | "as_pattern_target"
        | "tuple"
        | "list"
        | "list_splat_pattern"
        | "parenthesized_expression" => {
            let mut cursor = target.walk();
            for child in target.named_children(&mut cursor) {
                bound_names(cx, child, names);
            }
        }
        _ => {}
    }
}

/// Whether `node` (a subscript) is only written: an assignment or `for`
/// target, possibly inside a destructuring pattern, or deleted.
fn is_write_target(node: TsNode<'_>) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        match parent.kind() {
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "tuple"
            | "list"
            | "parenthesized_expression"
            | "expression_list" => child = parent,
            "assignment" | "for_statement" => {
                return parent.child_by_field_name("left") == Some(child);
            }
            "delete_statement" => return true,
            _ => return false,
        }
    }
    false
}

/// `x or {}` / `x or dict()`: the left operand is what is read.
fn unwrap_or(node: TsNode<'_>) -> TsNode<'_> {
    if node.kind() == "boolean_operator"
        && let Some(left) = node.child_by_field_name("left")
    {
        return left;
    }
    node
}

struct Walker<'a, 'b> {
    cx: &'a FileCx<'b>,
    request: &'a BTreeSet<String>,
}

impl Walker<'_, '_> {
    /// Pre-order walk of `node`, not descending into nested scopes.
    fn visit<'t>(&self, node: TsNode<'t>, f: &mut dyn FnMut(TsNode<'t>)) {
        f(node);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(
                child.kind(),
                "function_definition" | "class_definition" | "lambda"
            ) {
                continue;
            }
            self.visit(child, f);
        }
    }

    fn is_request(&self, node: TsNode<'_>) -> bool {
        matches!(node.kind(), "identifier" | "attribute")
            && self.request.contains(self.cx.text(&node))
    }

    /// `request.get_json(...)` or `request.json`.
    fn is_json(&self, node: TsNode<'_>) -> bool {
        match node.kind() {
            "call" => node
                .child_by_field_name("function")
                .is_some_and(|function| self.request_attribute(function) == Some("get_json")),
            "attribute" => self.request_attribute(node) == Some("json"),
            _ => false,
        }
    }

    /// `attr` of a `request.attr` attribute node.
    fn request_attribute(&self, node: TsNode<'_>) -> Option<&str> {
        if node.kind() != "attribute" {
            return None;
        }
        let object = node.child_by_field_name("object")?;
        if !self.is_request(object) {
            return None;
        }
        node.child_by_field_name("attribute")
            .map(|attribute| self.cx.text(&attribute))
    }

    /// The literal key `value` is read with: `value.get("k", …)`,
    /// `value.getlist("k")` (collections only), `value["k"]` or `"k" in value`,
    /// and the expression that reads it. `Ok(None)` when `value["k"]` is only
    /// written; `Err` when `value` is used in any other way.
    fn literal_read<'t>(
        &self,
        value: TsNode<'t>,
        getlist: bool,
    ) -> Result<Option<(String, TsNode<'t>)>, ()> {
        let parent = value.parent().ok_or(())?;
        match parent.kind() {
            "subscript" if parent.child_by_field_name("value") == Some(value) => {
                let key = parent.child_by_field_name("subscript").ok_or(())?;
                if parent.named_child_count() != 2 {
                    return Err(());
                }
                let key = literal_string(self.cx, key).ok_or(())?;
                Ok((!is_write_target(parent)).then_some((key, parent)))
            }
            "attribute" if parent.child_by_field_name("object") == Some(value) => {
                let method = parent
                    .child_by_field_name("attribute")
                    .map(|attribute| self.cx.text(&attribute))
                    .ok_or(())?;
                if method != "get" && !(getlist && method == "getlist") {
                    return Err(());
                }
                let call = parent
                    .parent()
                    .filter(|call| call.kind() == "call")
                    .ok_or(())?;
                let arguments = call.child_by_field_name("arguments").ok_or(())?;
                let first = arguments.named_child(0).ok_or(())?;
                Ok(Some((literal_string(self.cx, first).ok_or(())?, call)))
            }
            // `"k" in value` / `"k" not in value`.
            "comparison_operator"
                if parent.named_child_count() == 2
                    && parent.named_child(1) == Some(value)
                    && parent
                        .child(1)
                        .is_some_and(|op| matches!(op.kind(), "in" | "not in")) =>
            {
                let key = parent.named_child(0).ok_or(())?;
                Ok(Some((literal_string(self.cx, key).ok_or(())?, parent)))
            }
            _ => Err(()),
        }
    }

    fn body_name_use(&self, name: TsNode<'_>, inputs: &mut RequestInputs) {
        let parent = name.parent();
        // The binding itself: `data = request.get_json()`.
        if let Some(parent) = parent
            && parent.kind() == "assignment"
            && parent.child_by_field_name("left") == Some(name)
        {
            let right = parent.child_by_field_name("right");
            if !right.is_some_and(|right| self.is_json(unwrap_or(right))) {
                inputs.partial.insert("body_name_rebound");
            }
            return;
        }
        // An attribute name (`x.data`) or keyword (`data=`) is not a use.
        if parent.is_some_and(|parent| {
            (parent.kind() == "attribute" && parent.child_by_field_name("attribute") == Some(name))
                || (parent.kind() == "keyword_argument"
                    && parent.child_by_field_name("name") == Some(name))
        }) {
            return;
        }
        match self.literal_read(name, false) {
            Ok(Some((key, read))) => {
                inputs.body.insert(key);
                inputs.cite(self.cx.evidence(&read));
            }
            Ok(None) => {}
            Err(()) => {
                inputs.partial.insert("body_read_dynamically");
            }
        }
    }

    fn request_use(
        &self,
        node: TsNode<'_>,
        json_names: &BTreeSet<String>,
        inputs: &mut RequestInputs,
    ) {
        let Some(parent) = node.parent() else {
            return;
        };
        // The name half of another attribute (`flask.request` seen as its
        // `request` field) is not a use of a request name.
        if parent.kind() == "attribute" && parent.child_by_field_name("attribute") == Some(node) {
            return;
        }
        // `request` must be the object of an attribute; anything else passes
        // or rebinds the request object itself.
        if parent.kind() != "attribute" || parent.child_by_field_name("object") != Some(node) {
            inputs.partial.insert("request_passed");
            return;
        }
        let attribute = parent
            .child_by_field_name("attribute")
            .map(|attribute| self.cx.text(&attribute))
            .unwrap_or("");
        if let Some((_, collection)) = COLLECTIONS.iter().find(|(name, _)| *name == attribute) {
            match (self.literal_read(parent, true), collection) {
                (Ok(None), _) => {}
                (Ok(Some((key, read))), collection) => {
                    inputs.cite(self.cx.evidence(&read));
                    match collection {
                        Collection::Query => {
                            inputs.query.insert(key);
                        }
                        Collection::Form => {
                            inputs.form.insert(key);
                        }
                        Collection::Values => {
                            inputs.query.insert(key.clone());
                            inputs.form.insert(key);
                        }
                    }
                }
                (Err(()), Collection::Query) => {
                    inputs.partial.insert("query_read_dynamically");
                }
                (Err(()), Collection::Form) => {
                    inputs.partial.insert("form_read_dynamically");
                }
                (Err(()), Collection::Values) => {
                    inputs.partial.insert("query_read_dynamically");
                    inputs.partial.insert("form_read_dynamically");
                }
            }
            return;
        }
        let json = match attribute {
            "json" => Some(parent),
            "get_json" => parent.parent().filter(|call| call.kind() == "call"),
            _ => None,
        };
        let Some(json) = json else {
            return; // headers, method, files, …: not named inputs
        };
        // A body expression is either bound (pass 1 saw it) or read directly.
        let bound = json
            .parent()
            .and_then(|parent| {
                if parent.kind() == "boolean_operator" {
                    parent.parent()
                } else {
                    Some(parent)
                }
            })
            .filter(|assignment| assignment.kind() == "assignment")
            .and_then(|assignment| assignment.child_by_field_name("left"))
            .is_some_and(|left| {
                left.kind() == "identifier" && json_names.contains(self.cx.text(&left))
            });
        if bound {
            return;
        }
        match self.literal_read(json, false) {
            Ok(Some((key, read))) => {
                inputs.body.insert(key);
                inputs.cite(self.cx.evidence(&read));
            }
            Ok(None) => {}
            Err(()) => {
                inputs.partial.insert("body_read_dynamically");
            }
        }
    }
}
