//! Bean Validation constraints on Spring endpoints (#2162): `props.input_constraints`.
//!
//! A Spring handler parameter annotated `@Valid` (`jakarta.validation` or
//! `javax.validation`) or `@Validated`
//! (`org.springframework.validation.annotation`) is validated before the
//! handler runs. Each constraint annotation
//! (`jakarta.validation.constraints` or `javax.validation.constraints`) on a
//! field or record component of that parameter's type, or of a superclass
//! the repository declares, is one input constraint of every endpoint the
//! handler serves, citing the annotation. A constraint written on a handler
//! parameter itself counts only when the controller class is `@Validated`,
//! which is what turns on Spring's method validation for it.
//!
//! A constraint on a type no handler validates is not a constraint of any
//! endpoint. What this pass cannot read is a Gap on the endpoint
//! (`Endpoint -DEPENDS_ON-> Gap`), never a guessed rule:
//!
//! - `custom_constraint`: an annotation type the repository declares with
//!   `@Constraint`; its validator's logic is not read.
//! - `validation_group`: a constraint with `groups = ...`, or a
//!   `@Validated(...)` naming groups; which groups a request runs is not read.
//! - `cascaded_field`: a field marked `@Valid` inside a validated type; the
//!   nested type's constraints are not followed.
//! - `constraint_value`: an attribute that is neither a literal nor a string
//!   constant the repository declares once.
//!
//! The reading records annotation syntax, not runtime validation behaviour.

use super::method_security::{self, Part, Settled};
use super::{ConfidenceTier, FileCx, Imports, TsNode, retrofit};
use core_graph::{Edge, Node};
use std::collections::{BTreeMap, BTreeSet};

const CONSTRAINTS: &[&str] = &[
    "jakarta.validation.constraints",
    "javax.validation.constraints",
];
const VALIDATION: &[&str] = &["jakarta.validation", "javax.validation"];
const SPRING_VALIDATION: &[&str] = &["org.springframework.validation.annotation"];

/// The built-in constraints read, and the attributes each one carries.
fn attributes(constraint: &str) -> Option<&'static [&'static str]> {
    Some(match constraint {
        "NotNull" | "NotBlank" | "NotEmpty" | "Null" | "AssertTrue" | "AssertFalse" | "Past"
        | "PastOrPresent" | "Future" | "FutureOrPresent" | "Positive" | "PositiveOrZero"
        | "Negative" | "NegativeOrZero" => &[],
        "Size" => &["min", "max"],
        "Min" | "Max" => &["value"],
        "DecimalMin" | "DecimalMax" => &["value", "inclusive"],
        "Digits" => &["integer", "fraction"],
        "Pattern" => &["regexp"],
        "Email" => &["regexp"],
        _ => return None,
    })
}

/// Recursion bound on the superclass walk.
const MAX_SUPERCLASSES: usize = 16;

/// How an annotation is proven to come from one of `packages`: `None` when
/// it is not; otherwise the same-package type a wildcard-only proof would
/// yield to (JLS §6.4.1), settled at the directory join.
fn proof(
    cx: &FileCx<'_>,
    annotation: TsNode<'_>,
    name: &str,
    imports: &Imports,
    package: Option<&str>,
    packages: &[&str],
) -> Option<Option<String>> {
    let spelling = annotation
        .child_by_field_name("name")
        .map(|spelled| cx.text(&spelled))
        .unwrap_or_default();
    if spelling.contains('.') {
        return packages
            .iter()
            .any(|p| spelling == format!("{p}.{name}"))
            .then_some(None);
    }
    match imports.bindings.get(name) {
        Some(fqn) => packages
            .iter()
            .any(|p| *fqn == format!("{p}.{name}"))
            .then_some(None),
        None => imports
            .wildcard_packages
            .iter()
            .any(|w| packages.contains(&w.as_str()))
            .then(|| {
                Some(match package {
                    Some(package) => format!("{package}.{name}"),
                    None => name.to_string(),
                })
            }),
    }
}

/// Candidate fully qualified names of a type spelled in this file: an
/// explicit import, a nested type of an enclosing class, the file's
/// package, then each wildcard import. The join takes the first the
/// repository declares exactly once.
fn candidates(
    spelled: &str,
    imports: &Imports,
    package: Option<&str>,
    chain: &[String],
) -> Vec<String> {
    // `List<Foo>` and arrays are not followed: their element is not the
    // validated bean.
    let spelled = spelled.trim();
    if spelled.contains(['<', '[']) || spelled.is_empty() {
        return Vec::new();
    }
    let qualify = |name: &str| match package {
        Some(package) => format!("{package}.{name}"),
        None => name.to_string(),
    };
    let (head, tail) = match spelled.split_once('.') {
        Some((head, tail)) => (head, Some(tail)),
        None => (spelled, None),
    };
    let mut out = Vec::new();
    if let Some(fqn) = imports.bindings.get(head) {
        out.push(match tail {
            Some(tail) => format!("{fqn}.{tail}"),
            None => fqn.clone(),
        });
        return out;
    }
    if tail.is_some() {
        out.push(spelled.to_string());
    }
    for depth in (1..=chain.len()).rev() {
        out.push(qualify(&format!("{}.{spelled}", chain[..depth].join("."))));
    }
    out.push(qualify(spelled));
    for wildcard in &imports.wildcard_packages {
        out.push(format!("{wildcard}.{spelled}"));
    }
    out
}

/// One constraint annotation, as written.
#[derive(Debug, Clone)]
pub(super) struct Constraint {
    /// The constraint's simple name, e.g. `Size`.
    name: String,
    /// The attributes read, each as text parts; `None` when not readable.
    attributes: Vec<(String, Option<Vec<Part>>)>,
    message: Option<Vec<Part>>,
    /// `groups = ...` is present.
    grouped: bool,
    /// A same-package type that would take a wildcard-only proof.
    shadowed_by: Option<String>,
    prov: serde_json::Value,
    gap_anchor: (String, usize),
    gap_prov: serde_json::Value,
    text: String,
}

/// What an annotation on a validated member is.
#[derive(Debug, Clone)]
pub(super) enum Mark {
    Constraint(Constraint),
    /// Any other annotation: a custom constraint when the repository
    /// declares one of `candidates` with `@Constraint`.
    Other {
        candidates: Vec<String>,
        text: String,
        gap_anchor: (String, usize),
        gap_prov: serde_json::Value,
    },
    /// `@Valid` on a field: cascaded validation.
    Cascade {
        text: String,
        gap_anchor: (String, usize),
        gap_prov: serde_json::Value,
    },
}

/// A field or record component and its annotations.
#[derive(Debug, Clone)]
pub(super) struct Member {
    name: String,
    marks: Vec<Mark>,
}

/// A class or record this file declares, with its constrained members.
#[derive(Debug, Clone)]
pub(super) struct Bean {
    fqn: String,
    /// Candidates for the superclass, when it has one.
    superclass: Vec<String>,
    members: Vec<Member>,
}

/// A validated handler parameter.
#[derive(Debug, Clone)]
pub(super) struct Param {
    name: String,
    /// Candidates for the parameter's type; empty when it is not followed.
    types: Vec<String>,
    /// `@Valid` / `@Validated` proves the bean is validated.
    validated: bool,
    /// Constraints on the parameter itself.
    marks: Vec<Mark>,
    /// `@Validated(...)` naming groups.
    groups: Option<(String, (String, usize), serde_json::Value)>,
    /// A wildcard-only proof of `@Valid`/`@Validated`.
    shadowed_by: Option<String>,
}

/// A Spring handler's validated parameters.
#[derive(Debug, Clone)]
pub(super) struct Handler {
    handler: String,
    params: Vec<Param>,
}

fn retarget(value: &mut serde_json::Value, commit: &str) {
    let mut props = serde_json::json!({ "prov": value.take() });
    super::retarget_props_commit(&mut props, commit);
    *value = props["prov"].take();
}

fn retarget_marks(marks: &mut [Mark], commit: &str) {
    for mark in marks {
        match mark {
            Mark::Constraint(constraint) => {
                retarget(&mut constraint.prov, commit);
                retarget(&mut constraint.gap_prov, commit);
            }
            Mark::Other { gap_prov, .. } | Mark::Cascade { gap_prov, .. } => {
                retarget(gap_prov, commit);
            }
        }
    }
}

/// Point a reused file's held constraints at `commit` (AC-0054).
pub(super) fn retarget_all(beans: &mut [Bean], handlers: &mut [Handler], commit: &str) {
    for bean in beans {
        for member in &mut bean.members {
            retarget_marks(&mut member.marks, commit);
        }
    }
    for handler in handlers {
        for param in &mut handler.params {
            retarget_marks(&mut param.marks, commit);
            if let Some((_, _, prov)) = &mut param.groups {
                retarget(prov, commit);
            }
        }
    }
}

/// Annotations in a declaration's `modifiers`, or directly on a parameter.
fn annotations<'t>(cx: &FileCx<'_>, declaration: TsNode<'t>) -> Vec<(String, TsNode<'t>)> {
    let mut found = super::declaration_annotations(cx, declaration);
    if found.is_empty() {
        let mut walk = declaration.walk();
        for child in declaration.named_children(&mut walk) {
            if !matches!(child.kind(), "annotation" | "marker_annotation") {
                continue;
            }
            if let Some(name) = child.child_by_field_name("name") {
                let simple = cx.text(&name);
                found.push((
                    simple.rsplit('.').next().unwrap_or(simple).to_string(),
                    child,
                ));
            }
        }
    }
    found
}

/// An attribute value as text parts: an integer, boolean or string literal,
/// or string constants; `None` otherwise.
fn value_parts(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    value: TsNode<'_>,
) -> Option<Vec<Part>> {
    let text = cx.text(&value).trim();
    let numeric = value.kind() == "decimal_integer_literal"
        || (value.kind() == "unary_expression"
            && text.starts_with('-')
            && text[1..]
                .trim()
                .chars()
                .all(|c| c.is_ascii_digit() || c == '_'));
    if numeric || matches!(value.kind(), "true" | "false") {
        return Some(vec![Part::Text(text.replace([' ', '_'], ""))]);
    }
    let mut parts = Vec::new();
    method_security::parts(cx, imports, package, value, &mut parts)?;
    Some(parts)
}

fn anchor(cx: &FileCx<'_>, node: TsNode<'_>) -> (String, usize) {
    (cx.path.to_string(), node.start_byte())
}

fn gap_prov(cx: &FileCx<'_>, node: TsNode<'_>) -> serde_json::Value {
    cx.prov_with_confidence(
        &node,
        ConfidenceTier::Gap,
        &format!("Gap bean validation {}@{}", cx.path, node.start_byte()),
    )
}

fn shorten(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.len() <= 160 {
        return text;
    }
    let mut cut = 160;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// What each annotation on a member says.
fn marks(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    chain: &[String],
    found: &[(String, TsNode<'_>)],
    cascade: bool,
) -> Vec<Mark> {
    let mut out = Vec::new();
    for (name, annotation) in found {
        let text = shorten(cx.text(annotation));
        if let Some(names) = attributes(name)
            && let Some(shadowed_by) = proof(cx, *annotation, name, imports, package, CONSTRAINTS)
        {
            let read = |key: &str| {
                retrofit::element(cx, *annotation, key)
                    .map(|value| value_parts(cx, imports, package, value))
            };
            let mut attributes = Vec::new();
            for key in names {
                // `@Min(5)` and `@Size(max = 5)` alike: `value` may be unnamed.
                let value = match read(key) {
                    Some(value) => Some(value),
                    None if *key == "value" => annotation
                        .child_by_field_name("arguments")
                        .and_then(|arguments| {
                            let mut walk = arguments.walk();
                            let values: Vec<_> = arguments
                                .named_children(&mut walk)
                                .filter(|a| {
                                    !matches!(
                                        a.kind(),
                                        "line_comment" | "block_comment" | "element_value_pair"
                                    )
                                })
                                .collect();
                            match values[..] {
                                [one] => Some(value_parts(cx, imports, package, one)),
                                _ => None,
                            }
                        }),
                    None => None,
                };
                if let Some(value) = value {
                    attributes.push(((*key).to_string(), value));
                }
            }
            out.push(Mark::Constraint(Constraint {
                name: name.clone(),
                attributes,
                message: read("message").flatten(),
                grouped: retrofit::element(cx, *annotation, "groups").is_some(),
                shadowed_by,
                prov: cx.prov(annotation, &format!("Bean Validation {text}")),
                gap_anchor: anchor(cx, *annotation),
                gap_prov: gap_prov(cx, *annotation),
                text,
            }));
            continue;
        }
        if cascade
            && name == "Valid"
            && proof(cx, *annotation, name, imports, package, VALIDATION).is_some()
        {
            out.push(Mark::Cascade {
                text,
                gap_anchor: anchor(cx, *annotation),
                gap_prov: gap_prov(cx, *annotation),
            });
            continue;
        }
        let spelled = annotation
            .child_by_field_name("name")
            .map(|n| cx.text(&n).to_string())
            .unwrap_or_default();
        out.push(Mark::Other {
            candidates: candidates(&spelled, imports, package, chain),
            text,
            gap_anchor: anchor(cx, *annotation),
            gap_prov: gap_prov(cx, *annotation),
        });
    }
    out
}

/// The classes and records this file declares, with their annotated
/// fields and components, and the annotation types it declares with
/// `@Constraint`.
pub(super) fn beans(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    package: Option<&str>,
) -> (Vec<Bean>, Vec<String>) {
    let qualify = |chain: &[String]| match package {
        Some(package) => format!("{package}.{}", chain.join(".")),
        None => chain.join("."),
    };
    let mut beans = Vec::new();
    let mut constraint_types = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        let kind = node.kind();
        if !matches!(
            kind,
            "class_declaration" | "record_declaration" | "annotation_type_declaration"
        ) {
            continue;
        }
        let Some(name) = node.child_by_field_name("name") else {
            continue;
        };
        let mut chain = super::enclosing_type_chain(cx, node);
        chain.push(cx.text(&name).to_string());
        if kind == "annotation_type_declaration" {
            let found = annotations(cx, node);
            if found.iter().any(|(name, annotation)| {
                name == "Constraint"
                    && proof(cx, *annotation, name, imports, package, VALIDATION).is_some()
            }) {
                constraint_types.push(qualify(&chain));
            }
            continue;
        }
        let superclass = node
            .child_by_field_name("superclass")
            .and_then(|s| s.named_child(0))
            .map(|t| candidates(cx.text(&t), imports, package, &chain[..chain.len() - 1]))
            .unwrap_or_default();
        let mut members = Vec::new();
        if kind == "record_declaration"
            && let Some(parameters) = node.child_by_field_name("parameters")
        {
            let mut walk = parameters.walk();
            for component in parameters.named_children(&mut walk) {
                let Some(name) = component.child_by_field_name("name") else {
                    continue;
                };
                let found = annotations(cx, component);
                let marks = marks(cx, imports, package, &chain, &found, true);
                if !marks.is_empty() {
                    members.push(Member {
                        name: cx.text(&name).to_string(),
                        marks,
                    });
                }
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            let mut walk = body.walk();
            for field in body.named_children(&mut walk) {
                if field.kind() != "field_declaration" {
                    continue;
                }
                let found = super::declaration_annotations(cx, field);
                let is_static = {
                    let mut walk = field.walk();
                    field
                        .named_children(&mut walk)
                        .find(|c| c.kind() == "modifiers")
                        .is_some_and(|m| cx.text(&m).split_whitespace().any(|w| w == "static"))
                };
                if found.is_empty() || is_static {
                    continue;
                }
                let marks = marks(cx, imports, package, &chain, &found, true);
                let mut walk = field.walk();
                for declarator in field.children_by_field_name("declarator", &mut walk) {
                    if let Some(name) = declarator.child_by_field_name("name") {
                        members.push(Member {
                            name: cx.text(&name).to_string(),
                            marks: marks.clone(),
                        });
                    }
                }
            }
        }
        beans.push(Bean {
            fqn: qualify(&chain),
            superclass,
            members,
        });
    }
    (beans, constraint_types)
}

/// The file's `static final` integer constants with a literal value, for
/// constraint attributes that name them (`@Size(min = MIN_LENGTH)`).
pub(super) fn numeric_constants(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    package: Option<&str>,
) -> Vec<method_security::Constant> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if !matches!(node.kind(), "field_declaration" | "constant_declaration") {
            continue;
        }
        let Some(owner) = node.parent().and_then(|body| body.parent()) else {
            continue;
        };
        let in_interface = matches!(
            owner.kind(),
            "interface_declaration" | "annotation_type_declaration"
        );
        let mut walk = node.walk();
        let modifiers = node
            .named_children(&mut walk)
            .find(|c| c.kind() == "modifiers")
            .map(|m| cx.text(&m).to_string())
            .unwrap_or_default();
        let words: Vec<&str> = modifiers.split_whitespace().collect();
        let constant = in_interface || (words.contains(&"static") && words.contains(&"final"));
        let integral = node.child_by_field_name("type").is_some_and(|t| {
            matches!(
                cx.text(&t),
                "int" | "long" | "short" | "byte" | "Integer" | "Long"
            )
        });
        if !constant || !integral {
            continue;
        }
        let Some(name) = owner.child_by_field_name("name") else {
            continue;
        };
        let mut chain = super::enclosing_type_chain(cx, owner);
        chain.push(cx.text(&name).to_string());
        let class = match package {
            Some(package) => format!("{package}.{}", chain.join(".")),
            None => chain.join("."),
        };
        let mut walk = node.walk();
        for declarator in node.children_by_field_name("declarator", &mut walk) {
            let (Some(name), Some(value)) = (
                declarator.child_by_field_name("name"),
                declarator.child_by_field_name("value"),
            ) else {
                continue;
            };
            let text = cx.text(&value).replace([' ', '_'], "");
            let digits = text.strip_prefix('-').unwrap_or(&text);
            let digits = digits.strip_suffix(['L', 'l']).unwrap_or(digits);
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                out.push(method_security::Constant {
                    name: format!("{class}.{}", cx.text(&name)),
                    value: vec![Part::Text(text.trim_end_matches(['L', 'l']).to_string())],
                });
            }
        }
    }
    out
}

/// A Spring handler's validated parameters, `None` when it has none.
pub(super) fn handler(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    chain: &[String],
    method: TsNode<'_>,
    handler: &str,
    class_annotations: &[(String, TsNode<'_>)],
) -> Option<Handler> {
    let class_validated = class_annotations.iter().any(|(name, annotation)| {
        name == "Validated"
            && proof(cx, *annotation, name, imports, package, SPRING_VALIDATION).is_some()
    });
    let parameters = method.child_by_field_name("parameters")?;
    let mut params = Vec::new();
    let mut walk = parameters.walk();
    for parameter in parameters.named_children(&mut walk) {
        if parameter.kind() != "formal_parameter" {
            continue;
        }
        let Some(name) = parameter.child_by_field_name("name") else {
            continue;
        };
        let found = annotations(cx, parameter);
        let mut validated = false;
        let mut shadowed_by = None;
        let mut groups = None;
        let mut rest = Vec::new();
        for (simple, annotation) in &found {
            let proven = match simple.as_str() {
                "Valid" => proof(cx, *annotation, simple, imports, package, VALIDATION),
                "Validated" => proof(cx, *annotation, simple, imports, package, SPRING_VALIDATION),
                _ => None,
            };
            match proven {
                Some(shadow) => {
                    validated = true;
                    shadowed_by = shadowed_by.or(shadow);
                    if simple == "Validated"
                        && annotation
                            .child_by_field_name("arguments")
                            .is_some_and(|a| a.named_child_count() > 0)
                    {
                        groups = Some((
                            shorten(cx.text(annotation)),
                            anchor(cx, *annotation),
                            gap_prov(cx, *annotation),
                        ));
                    }
                }
                None => rest.push((simple.clone(), *annotation)),
            }
        }
        // Only constraints matter on the parameter itself; other annotations
        // (`@RequestBody`, `@PathVariable`) are Spring's binding.
        let marks: Vec<Mark> = marks(cx, imports, package, chain, &rest, false)
            .into_iter()
            .filter(|mark| matches!(mark, Mark::Constraint(_)))
            .collect();
        if !validated && (marks.is_empty() || !class_validated) {
            continue;
        }
        let types = if validated {
            parameter
                .child_by_field_name("type")
                .map(|t| candidates(cx.text(&t), imports, package, chain))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        params.push(Param {
            name: cx.text(&name).to_string(),
            types,
            validated,
            marks: if class_validated { marks } else { Vec::new() },
            groups,
            shadowed_by,
        });
    }
    (!params.is_empty()).then(|| Handler {
        handler: handler.to_string(),
        params,
    })
}

/// A Gap this pass records on an endpoint.
struct GapSite<'a> {
    reason: &'static str,
    text: &'a str,
    anchor: &'a (String, usize),
    prov: &'a serde_json::Value,
}

/// The reading of one validated parameter: rows and Gaps.
#[derive(Default)]
struct Reading<'a> {
    rows: Vec<serde_json::Value>,
    gaps: Vec<GapSite<'a>>,
}

/// Settle `marks` declared on `field` (of `declared_in`) into `reading`.
#[allow(clippy::too_many_arguments)]
fn read_marks<'a>(
    marks: &'a [Mark],
    parameter: &str,
    field: Option<&str>,
    declared_in: Option<&str>,
    settled: &Settled,
    shadowed: &dyn Fn(&str) -> bool,
    constraint_types: &BTreeSet<&str>,
    reading: &mut Reading<'a>,
) {
    for mark in marks {
        match mark {
            Mark::Constraint(constraint) => {
                if constraint.shadowed_by.as_deref().is_some_and(shadowed) {
                    continue;
                }
                if constraint.grouped {
                    reading.gaps.push(GapSite {
                        reason: "validation_group",
                        text: &constraint.text,
                        anchor: &constraint.gap_anchor,
                        prov: &constraint.gap_prov,
                    });
                    continue;
                }
                let mut attributes = serde_json::Map::new();
                let mut unread = false;
                for (key, value) in &constraint.attributes {
                    match value.as_deref().and_then(|parts| settled.text(parts)) {
                        Some(text) => {
                            attributes.insert(key.clone(), text.into());
                        }
                        None => unread = true,
                    }
                }
                if unread {
                    reading.gaps.push(GapSite {
                        reason: "constraint_value",
                        text: &constraint.text,
                        anchor: &constraint.gap_anchor,
                        prov: &constraint.gap_prov,
                    });
                    continue;
                }
                let message = constraint
                    .message
                    .as_deref()
                    .and_then(|parts| settled.text(parts));
                reading.rows.push(serde_json::json!({
                    "parameter": parameter,
                    "type": declared_in,
                    "field": field,
                    "constraint": constraint.name,
                    "attributes": attributes,
                    "message": message,
                    "annotation": constraint.text,
                    "prov": constraint.prov,
                }));
            }
            Mark::Other {
                candidates,
                text,
                gap_anchor,
                gap_prov,
            } => {
                if candidates
                    .iter()
                    .any(|fqn| constraint_types.contains(fqn.as_str()))
                {
                    reading.gaps.push(GapSite {
                        reason: "custom_constraint",
                        text,
                        anchor: gap_anchor,
                        prov: gap_prov,
                    });
                }
            }
            Mark::Cascade {
                text,
                gap_anchor,
                gap_prov,
            } => reading.gaps.push(GapSite {
                reason: "cascaded_field",
                text,
                anchor: gap_anchor,
                prov: gap_prov,
            }),
        }
    }
}

/// Directory join: each Spring endpoint whose handler validates a
/// parameter gains `input_constraints` (sorted, possibly empty) and a Gap
/// for each constraint it cannot read.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve(
    repo: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    beans: &[Bean],
    handlers: &[Handler],
    constraint_types: &[String],
    settled: &Settled,
    shadowed: &dyn Fn(&str) -> bool,
) {
    if handlers.is_empty() {
        return;
    }
    let mut by_fqn: BTreeMap<&str, Option<&Bean>> = BTreeMap::new();
    for bean in beans {
        by_fqn
            .entry(bean.fqn.as_str())
            .and_modify(|unique| *unique = None)
            .or_insert(Some(bean));
    }
    let lookup = |candidates: &[String]| {
        candidates
            .iter()
            .find_map(|fqn| by_fqn.get(fqn.as_str()).copied().flatten())
    };
    let constraint_types: BTreeSet<&str> = constraint_types.iter().map(String::as_str).collect();
    let by_handler: BTreeMap<&str, &Handler> =
        handlers.iter().map(|h| (h.handler.as_str(), h)).collect();
    let mut readings: BTreeMap<&str, Reading<'_>> = BTreeMap::new();
    for handler in handlers {
        let mut reading = Reading::default();
        for param in &handler.params {
            if param.shadowed_by.as_deref().is_some_and(shadowed) {
                continue;
            }
            read_marks(
                &param.marks,
                &param.name,
                None,
                None,
                settled,
                shadowed,
                &constraint_types,
                &mut reading,
            );
            if !param.validated {
                continue;
            }
            if let Some((text, anchor, prov)) = &param.groups {
                reading.gaps.push(GapSite {
                    reason: "validation_group",
                    text,
                    anchor,
                    prov,
                });
                continue;
            }
            // The bean, then each superclass the repository declares.
            let mut seen = BTreeSet::new();
            let mut next = lookup(&param.types);
            while let Some(bean) = next {
                if !seen.insert(bean.fqn.as_str()) || seen.len() > MAX_SUPERCLASSES {
                    break;
                }
                for member in &bean.members {
                    read_marks(
                        &member.marks,
                        &param.name,
                        Some(&member.name),
                        Some(&bean.fqn),
                        settled,
                        shadowed,
                        &constraint_types,
                        &mut reading,
                    );
                }
                next = lookup(&bean.superclass);
            }
        }
        readings.insert(handler.handler.as_str(), reading);
    }
    let mut emitted = BTreeSet::new();
    let mut new_nodes = Vec::new();
    for node in nodes.iter_mut() {
        if node.label != "Endpoint" || node.props["framework"] != "spring" {
            continue;
        }
        let Some(handler) = node.props["handler_sym"].as_str() else {
            continue;
        };
        if !by_handler.contains_key(handler) {
            continue;
        }
        let Some(reading) = readings.get(handler) else {
            continue;
        };
        let mut rows = reading.rows.clone();
        rows.sort_by_key(|row| {
            (
                row["parameter"].to_string(),
                row["type"].to_string(),
                row["field"].to_string(),
                row["prov"]["evidence"][0]["path"].to_string(),
                row["prov"]["evidence"][0]["byte_start"].as_u64(),
            )
        });
        node.props["input_constraints"] = rows.into();
        let mut gaps: Vec<&GapSite<'_>> = reading.gaps.iter().collect();
        gaps.sort_by_key(|gap| (gap.anchor, gap.reason));
        gaps.dedup_by_key(|gap| (gap.anchor, gap.reason));
        for gap in gaps {
            let gap_id = format!(
                "gap:bean-validation:{repo}@{}@{}:{}",
                gap.anchor.0, gap.anchor.1, gap.reason
            );
            if emitted.insert(gap_id.clone()) {
                new_nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "reason": gap.reason,
                        "annotation": gap.text,
                        "framework": "spring",
                        "language": "java",
                        "attempted_tiers": ["T0"],
                        "prov": gap.prov,
                    }),
                });
            }
            edges.push(Edge {
                src: node.id.clone(),
                dst: gap_id,
                label: "DEPENDS_ON".into(),
                props: serde_json::json!({
                    "reason": gap.reason,
                    "prov": gap.prov,
                }),
            });
        }
    }
    nodes.extend(new_nodes);
}
