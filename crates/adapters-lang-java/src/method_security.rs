//! Spring method security on Spring endpoints (#2020): `props.auth` in the
//! Python adapter's three states, read from the handler's annotations.
//!
//! The annotations are Spring Security's `@PreAuthorize`, `@PostAuthorize`
//! (`org.springframework.security.access.prepost`) and `@Secured`
//! (`org.springframework.security.access.annotation`), and JSR-250's
//! `@RolesAllowed`, `@PermitAll` and `@DenyAll` (`jakarta.annotation.security`
//! or `javax.annotation.security`). Each is proven by a named import, a
//! wildcard import of its package, or its exact qualified spelling. A
//! method's own security annotations replace its class's, as Spring
//! resolves them.
//!
//! - `required`: a guard annotation with a literal value. `roles` and
//!   `authorities` are read from `hasRole`, `hasAnyRole`, `hasAuthority`
//!   and `hasAnyAuthority` (a `ROLE_` prefix dropped). `caller` names who
//!   the guard admits in words. `@DenyAll` and `denyAll()` admit no caller.
//! - `none`: `@PermitAll`, or a `permitAll()` expression with no other
//!   condition. The annotation only states what method security admits.
//! - `unknown`: an annotation whose value is not a literal or a constant the
//!   repository declares once.
//!
//! At the directory join each reading is combined with the `HttpSecurity`
//! URL rule that covers the endpoint ([`super::http_security`]): both must
//! admit a caller, so a handler with no annotation takes the URL rule's
//! reading, and one with neither is `unknown` (servlet filters and
//! interceptors are not read).
//!
//! The reading records annotation syntax, not effective runtime
//! authorization.

use super::http_security::{self, Chain, Url};
use super::{ConfidenceTier, FileCx, Imports, Provenance, Tier, TsNode, retrofit};

const NO_ANNOTATION: &str = "no method security annotation";

const PREPOST: &str = "org.springframework.security.access.prepost";
const SECURED: &str = "org.springframework.security.access.annotation";
const JSR250: &[&str] = &["jakarta.annotation.security", "javax.annotation.security"];

/// The packages a security annotation may come from.
fn packages(name: &str) -> &'static [&'static str] {
    match name {
        "PreAuthorize" | "PostAuthorize" => &[PREPOST],
        "Secured" => &[SECURED],
        "RolesAllowed" | "PermitAll" | "DenyAll" => JSR250,
        _ => &[],
    }
}

fn proven(cx: &FileCx<'_>, annotation: TsNode<'_>, name: &str, imports: &Imports) -> bool {
    let packages = packages(name);
    if packages.is_empty() {
        return false;
    }
    let spelling = annotation
        .child_by_field_name("name")
        .map(|spelled| cx.text(&spelled))
        .unwrap_or_default();
    if spelling.contains('.') {
        return packages.iter().any(|p| spelling == format!("{p}.{name}"));
    }
    imports
        .bindings
        .get(name)
        .map(|fqn| packages.iter().any(|p| *fqn == format!("{p}.{name}")))
        .unwrap_or_else(|| {
            imports
                .wildcard_packages
                .iter()
                .any(|w| packages.contains(&w.as_str()))
        })
}

/// The annotation's `value`: the sole unnamed argument or `value = …`.
fn value<'t>(cx: &FileCx<'_>, annotation: TsNode<'t>) -> Option<TsNode<'t>> {
    if let Some(value) = retrofit::element(cx, annotation, "value") {
        return Some(value);
    }
    let arguments = annotation.child_by_field_name("arguments")?;
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
        [one] => Some(one),
        _ => None,
    }
}

/// What a guard admits, read from its words.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Admits {
    pub(super) roles: Vec<String>,
    pub(super) authorities: Vec<String>,
    pub(super) authenticated: bool,
    pub(super) permit_all: bool,
    pub(super) deny_all: bool,
    pub(super) anonymous: bool,
    /// Whether the expression holds a condition beyond those above.
    pub(super) other: bool,
    /// Whether checks are joined by `and` or negated, so the roles and
    /// authorities above are not alternatives.
    pub(super) compound: bool,
}

fn role_name(role: &str) -> String {
    role.strip_prefix("ROLE_").unwrap_or(role).to_string()
}

/// The quoted arguments of each `name(...)` call in a SpEL expression.
fn calls<'e>(expression: &'e str, name: &str) -> Vec<Vec<&'e str>> {
    let mut found = Vec::new();
    let mut rest = expression;
    while let Some(at) = rest.find(name) {
        let before = rest[..at].chars().last();
        let after = &rest[at + name.len()..];
        rest = after;
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '#') {
            continue;
        }
        let Some(inner) = after.strip_prefix('(') else {
            continue;
        };
        let Some(close) = inner.find(')') else {
            continue;
        };
        found.push(
            inner[..close]
                .split(',')
                .map(|a| a.trim().trim_matches(['\'', '"']))
                .filter(|a| !a.is_empty())
                .collect(),
        );
    }
    found
}

/// Whether `name` occurs as a whole word, called or not.
fn word(expression: &str, name: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '.' || c == '#';
    expression.match_indices(name).any(|(at, _)| {
        !expression[..at].chars().last().is_some_and(ident)
            && !expression[at + name.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Read a SpEL access expression.
pub(super) fn expression(expression: &str) -> Admits {
    let mut admits = Admits::default();
    for (name, roles) in [
        ("hasRole", true),
        ("hasAnyRole", true),
        ("hasAuthority", false),
        ("hasAnyAuthority", false),
    ] {
        for arguments in calls(expression, name) {
            for argument in arguments {
                if roles {
                    admits.roles.push(role_name(argument));
                } else {
                    admits.authorities.push(argument.to_string());
                }
            }
        }
    }
    admits.authenticated = ["isAuthenticated", "isFullyAuthenticated", "isRememberMe"]
        .iter()
        .any(|name| !calls(expression, name).is_empty());
    // SpEL spells these as properties (`permitAll`) or calls.
    admits.permit_all = word(expression, "permitAll");
    admits.deny_all = word(expression, "denyAll");
    admits.anonymous = !calls(expression, "isAnonymous").is_empty();
    let checks: usize = [
        "hasRole",
        "hasAnyRole",
        "hasAuthority",
        "hasAnyAuthority",
        "isAuthenticated",
        "isFullyAuthenticated",
        "isRememberMe",
        "isAnonymous",
    ]
    .iter()
    .map(|name| calls(expression, name).len())
    .sum();
    let negated = word(expression, "not") || expression.contains('!');
    let joined = word(expression, "and") || expression.contains("&&");
    admits.compound = negated || (joined && checks > 1);
    // Whatever is left after the recognised calls and `and`/`or`/`not`.
    let mut residue = expression.to_string();
    for name in [
        "hasAnyRole",
        "hasRole",
        "hasAnyAuthority",
        "hasAuthority",
        "isFullyAuthenticated",
        "isAuthenticated",
        "isRememberMe",
        "permitAll",
        "denyAll",
        "isAnonymous",
    ] {
        while let Some(at) = residue.find(name) {
            let after = at + name.len();
            let end = if residue[after..].starts_with('(') {
                residue[at..]
                    .find(')')
                    .map_or(residue.len(), |close| at + close + 1)
            } else {
                after
            };
            residue.replace_range(at..end, " ");
        }
    }
    admits.other = residue
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '#' || c == '.'))
        .any(|word| !word.is_empty() && !matches!(word, "and" | "or" | "not"));
    admits
}

/// Who a guard admits, in a few words.
fn caller(admits: &Admits, expression: &str) -> String {
    if admits.deny_all {
        return "no caller (denied to all)".into();
    }
    let mut parts = Vec::new();
    match &admits.roles[..] {
        [] => {}
        [one] => parts.push(format!("caller with role {one}")),
        many => parts.push(format!("caller with any role of {}", many.join(", "))),
    }
    match &admits.authorities[..] {
        [] => {}
        [one] => parts.push(format!("caller with authority {one}")),
        many => parts.push(format!("caller with any authority of {}", many.join(", "))),
    }
    if parts.is_empty() && admits.authenticated {
        parts.push("signed-in user".into());
    }
    if parts.is_empty() && admits.anonymous {
        parts.push("anonymous caller".into());
    }
    if parts.is_empty() || admits.other || admits.compound {
        return format!("caller that `{expression}` admits");
    }
    parts.join(" or ")
}

/// A string constant a class declares (`static final String X = "…"`).
#[derive(Debug, Clone)]
pub(super) struct Constant {
    /// `{class FQN}.{field}`.
    pub(super) name: String,
    /// Literals and other constants, concatenated.
    pub(super) value: Vec<Part>,
}

/// The repository's string constants by name, each settled to its value
/// (constants built from constants included), or `None` when declared with
/// two values.
pub(super) struct Settled(std::collections::BTreeMap<String, Option<String>>);

impl Settled {
    pub(super) fn new(constants: &[Constant]) -> Self {
        let mut declared: std::collections::BTreeMap<&str, Option<&[Part]>> = Default::default();
        for constant in constants {
            declared
                .entry(constant.name.as_str())
                .and_modify(|seen| {
                    if *seen != Some(constant.value.as_slice()) {
                        *seen = None;
                    }
                })
                .or_insert(Some(constant.value.as_slice()));
        }
        let mut settled = Self(Default::default());
        for (name, value) in &declared {
            if value.is_none() {
                settled.0.insert((*name).to_string(), None);
            }
        }
        loop {
            let mut progress = false;
            for (name, value) in &declared {
                let Some(value) = value else { continue };
                if settled.0.contains_key(*name) {
                    continue;
                }
                if let Some(text) = settled.text(value) {
                    settled.0.insert((*name).to_string(), Some(text));
                    progress = true;
                }
            }
            if !progress {
                break settled;
            }
        }
    }

    /// The text of `parts`, naming each constant that does not settle.
    pub(super) fn settle(&self, parts: &[Part], missing: &mut Vec<String>) -> String {
        parts
            .iter()
            .map(|part| match part {
                Part::Text(text) => text.clone(),
                Part::Constant { spelled, names } => names
                    .iter()
                    .find_map(|n| self.0.get(n).cloned().flatten())
                    .unwrap_or_else(|| {
                        missing.push(spelled.clone());
                        String::new()
                    }),
            })
            .collect()
    }

    pub(super) fn text(&self, parts: &[Part]) -> Option<String> {
        let mut missing = Vec::new();
        let text = self.settle(parts, &mut missing);
        missing.is_empty().then_some(text)
    }
}

/// A Java string literal's value, with the simple escapes decoded; `None`
/// for a text block or a unicode or octal escape.
fn decoded(cx: &FileCx<'_>, node: TsNode<'_>) -> Option<String> {
    if node.kind() != "string_literal" {
        return None;
    }
    let text = cx.text(&node);
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    if text.starts_with("\"\"\"") {
        return None;
    }
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\'' => out.push('\''),
            '\\' => out.push('\\'),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            _ => return None,
        }
    }
    Some(out)
}

/// The package and enclosing class chain of a node, as a FQN.
fn enclosing_class(cx: &FileCx<'_>, mut node: TsNode<'_>, package: Option<&str>) -> Option<String> {
    let mut names = Vec::new();
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "class_declaration" | "interface_declaration" | "enum_declaration"
        ) && let Some(name) = parent.child_by_field_name("name")
        {
            names.push(cx.text(&name).to_string());
        }
        node = parent;
    }
    if names.is_empty() {
        return None;
    }
    names.reverse();
    Some(match package {
        Some(package) => format!("{package}.{}", names.join(".")),
        None => names.join("."),
    })
}

/// The file's `static final String` constants with a literal value.
pub(super) fn constants(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    package: Option<&str>,
) -> Vec<Constant> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
        if !matches!(node.kind(), "field_declaration" | "constant_declaration") {
            continue;
        }
        let in_interface = node
            .parent()
            .and_then(|body| body.parent())
            .is_some_and(|decl| decl.kind() == "interface_declaration");
        let mut walk = node.walk();
        let modifiers = node
            .named_children(&mut walk)
            .find(|c| c.kind() == "modifiers")
            .map(|m| cx.text(&m).to_string())
            .unwrap_or_default();
        let constant = in_interface
            || (modifiers.split_whitespace().any(|w| w == "static")
                && modifiers.split_whitespace().any(|w| w == "final"));
        let string = node
            .child_by_field_name("type")
            .is_some_and(|t| matches!(cx.text(&t), "String" | "java.lang.String"));
        if !constant || !string {
            continue;
        }
        let Some(class) = enclosing_class(cx, node, package) else {
            continue;
        };
        let mut walk = node.walk();
        for declarator in node.children_by_field_name("declarator", &mut walk) {
            let (Some(name), Some(value)) = (
                declarator.child_by_field_name("name"),
                declarator.child_by_field_name("value"),
            ) else {
                continue;
            };
            let mut parts_out = Vec::new();
            if parts(cx, imports, package, value, &mut parts_out).is_some() {
                out.push(Constant {
                    name: format!("{class}.{}", cx.text(&name)),
                    value: parts_out,
                });
            }
        }
    }
    out
}

/// A piece of an annotation value: literal text, or a constant by the
/// names it may resolve to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Part {
    Text(String),
    Constant { spelled: String, names: Vec<String> },
}

type Annotations = Vec<(String, Option<Vec<Vec<Part>>>)>;

fn to_json(annotations: &Annotations) -> serde_json::Value {
    annotations
        .iter()
        .map(|(name, values)| {
            let values = values.as_ref().map(|values| {
                values
                    .iter()
                    .map(|parts| {
                        parts
                            .iter()
                            .map(|part| match part {
                                Part::Text(text) => serde_json::json!({ "text": text }),
                                Part::Constant { spelled, names } => {
                                    serde_json::json!({ "constant": spelled, "names": names })
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            });
            serde_json::json!({ "name": name, "values": values })
        })
        .collect()
}

fn from_json(value: &serde_json::Value) -> Annotations {
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect()
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|annotation| {
            let name = annotation["name"].as_str().unwrap_or_default().to_string();
            let values = annotation["values"].as_array().map(|values| {
                values
                    .iter()
                    .map(|parts| {
                        parts
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|part| match part["text"].as_str() {
                                Some(text) => Part::Text(text.to_string()),
                                None => Part::Constant {
                                    spelled: part["constant"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .to_string(),
                                    names: strings(&part["names"]),
                                },
                            })
                            .collect()
                    })
                    .collect()
            });
            (name, values)
        })
        .collect()
}

pub(super) fn parts(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    node: TsNode<'_>,
    out: &mut Vec<Part>,
) -> Option<()> {
    if let Some(text) = decoded(cx, node) {
        out.push(Part::Text(text));
        return Some(());
    }
    match node.kind() {
        "parenthesized_expression" => parts(cx, imports, package, node.named_child(0)?, out),
        "binary_expression" => {
            let operator = node.child_by_field_name("operator")?;
            if cx.text(&operator) != "+" {
                return None;
            }
            parts(cx, imports, package, node.child_by_field_name("left")?, out)?;
            parts(
                cx,
                imports,
                package,
                node.child_by_field_name("right")?,
                out,
            )
        }
        "identifier" => {
            let class = enclosing_class(cx, node, package)?;
            let spelled = cx.text(&node).to_string();
            out.push(Part::Constant {
                names: vec![format!("{class}.{spelled}")],
                spelled,
            });
            Some(())
        }
        "field_access" => {
            let spelled = cx.text(&node).to_string();
            let object = node.child_by_field_name("object")?;
            let field = cx.text(&node.child_by_field_name("field")?);
            let object_text = cx.text(&object);
            let mut names = vec![spelled.clone()];
            if object.kind() == "identifier" {
                if let Some(fqn) = imports.bindings.get(object_text) {
                    names.insert(0, format!("{fqn}.{field}"));
                } else {
                    if let Some(package) = package {
                        names.push(format!("{package}.{object_text}.{field}"));
                    }
                    for wildcard in &imports.wildcard_packages {
                        names.push(format!("{wildcard}.{object_text}.{field}"));
                    }
                }
            }
            out.push(Part::Constant { spelled, names });
            Some(())
        }
        _ => None,
    }
}

/// The `auth` reading of a Spring handler, from its method's and class's
/// annotations. A value that names constants is held (`state: "pending"`)
/// until [`resolve`] knows the repository's constants.
pub(super) fn auth<'t>(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    method: &[(String, TsNode<'t>)],
    class: &[(String, TsNode<'t>)],
) -> serde_json::Value {
    fn security<'t>(
        cx: &FileCx<'_>,
        imports: &Imports,
        annotations: &[(String, TsNode<'t>)],
    ) -> Vec<(String, TsNode<'t>)> {
        annotations
            .iter()
            .filter(|(name, node)| proven(cx, *node, name, imports))
            .map(|(name, node)| (name.clone(), *node))
            .collect()
    }
    let mut found = security(cx, imports, method);
    if found.is_empty() {
        found = security(cx, imports, class);
    }
    if found.is_empty() {
        return serde_json::json!({
            "state": "unknown",
            "annotated": false,
            "reasons": [NO_ANNOTATION],
        });
    }
    let nodes: Vec<TsNode<'_>> = found.iter().map(|(_, n)| *n).collect();
    let prov = cx.prov_spanning(&nodes, ConfidenceTier::Confirmed, "auth");
    let mut annotations: Annotations = Vec::new();
    for (name, annotation) in &found {
        if matches!(name.as_str(), "PermitAll" | "DenyAll") {
            annotations.push((name.clone(), Some(Vec::new())));
            continue;
        }
        let values = value(cx, *annotation).and_then(|value| {
            let elements: Vec<TsNode<'_>> = if value.kind() == "element_value_array_initializer" {
                let mut walk = value.walk();
                value
                    .named_children(&mut walk)
                    .filter(|e| !matches!(e.kind(), "line_comment" | "block_comment"))
                    .collect()
            } else {
                vec![value]
            };
            elements
                .into_iter()
                .map(|element| {
                    let mut out = Vec::new();
                    parts(cx, imports, package, element, &mut out).map(|()| out)
                })
                .collect::<Option<Vec<_>>>()
        });
        annotations.push((name.clone(), values));
    }
    let pending = annotations.iter().any(|(_, values)| {
        values
            .iter()
            .flatten()
            .flatten()
            .any(|part| matches!(part, Part::Constant { .. }))
    });
    if pending {
        return serde_json::json!({
            "state": "pending",
            "annotations": to_json(&annotations),
            "prov": prov,
        });
    }
    let texts = annotations
        .into_iter()
        .map(|(name, values)| {
            let values = values.map(|values| {
                values
                    .into_iter()
                    .map(|parts| {
                        parts
                            .into_iter()
                            .map(|part| match part {
                                Part::Text(text) => text,
                                Part::Constant { .. } => unreachable!("pending was handled"),
                            })
                            .collect::<String>()
                    })
                    .collect()
            });
            (name, values)
        })
        .collect();
    evaluate(texts, prov)
}

/// The guard name prefix of a URL rule reading.
const URL: &str = "HttpSecurity ";

/// Settle every held reading against the repository's constants, then
/// combine each Spring endpoint's method reading with the URL rule that
/// covers it: both must admit a caller.
pub(super) fn resolve(nodes: &mut [super::Node], settled: &Settled, chains: &[Chain]) {
    let settle = |parts: &[Part], missing: &mut Vec<String>| settled.settle(parts, missing);
    let text = |parts: &[Part]| settled.text(parts);
    for node in nodes.iter_mut() {
        if node.label != "Endpoint" || node.props["framework"] != "spring" {
            continue;
        }
        if node.props["auth"]["state"] == "pending" {
            let auth = node.props["auth"].take();
            let prov = auth["prov"].clone();
            let mut missing = Vec::new();
            let texts = from_json(&auth["annotations"])
                .into_iter()
                .map(|(name, values)| {
                    let values = values.map(|values| {
                        values
                            .iter()
                            .map(|parts| settle(parts, &mut missing))
                            .collect()
                    });
                    (name, values)
                })
                .collect();
            node.props["auth"] = if missing.is_empty() {
                evaluate(texts, prov)
            } else {
                missing.sort();
                missing.dedup();
                serde_json::json!({
                    "state": "unknown",
                    "reasons": missing
                        .iter()
                        .map(|m| format!("{m} is not a string constant the repository declares once"))
                        .collect::<Vec<_>>(),
                })
            };
        }
        if node.props["auth"].is_null() {
            continue;
        }
        let method = node.props["method"].as_str().unwrap_or("?").to_string();
        let path = node.props["path"].as_str().unwrap_or("/").to_string();
        let url = match http_security::reading(chains, &method, &path, &text) {
            Url::Absent => None,
            Url::Unknown(reason) => Some(Err(reason)),
            Url::Decided {
                access,
                display,
                prov,
            } => Some(Ok(evaluate(
                vec![(format!("{URL}{display}"), Some(vec![access]))],
                prov,
            ))),
        };
        let auth = node.props["auth"].take();
        node.props["auth"] = combine(auth, url);
    }
}

/// Both readings must admit a caller: the URL rule's and the method's.
fn combine(
    method: serde_json::Value,
    url: Option<Result<serde_json::Value, String>>,
) -> serde_json::Value {
    let annotated = method["annotated"] != false;
    let url = match url {
        None if annotated => return method,
        None => {
            return serde_json::json!({
                "state": "unknown",
                "reasons": [format!("{NO_ANNOTATION}, and no HttpSecurity URL rule is recovered; servlet filters and interceptors are not read")],
            });
        }
        Some(Err(reason)) if !annotated => {
            return serde_json::json!({
                "state": "unknown",
                "reasons": [format!("{NO_ANNOTATION}"), reason],
            });
        }
        // An unread URL rule may be stricter than the method's own reading,
        // so the endpoint is partial: the annotation is kept as a site, the
        // caller it names is not stated as the endpoint's.
        Some(Err(reason)) => {
            if method["state"] == "unknown" {
                return method;
            }
            let read = match method["state"].as_str() {
                Some("none") => "the handler's method security admits every caller".to_string(),
                _ => format!(
                    "the handler's method security admits {}",
                    method["caller"].as_str().unwrap_or("some callers")
                ),
            };
            let mut partial = method;
            if let Some(props) = partial.as_object_mut() {
                props.remove("caller");
                props.remove("basis");
            }
            partial["state"] = "unknown".into();
            partial["reasons"] = serde_json::json!([read, reason]);
            return partial;
        }
        Some(Ok(url)) => url,
    };
    if !annotated {
        return url;
    }
    match (method["state"].as_str(), url["state"].as_str()) {
        (Some("unknown"), _) => method,
        // `@PermitAll` lifts no URL rule.
        (Some("none"), _) => url,
        (Some("required"), Some("none")) => method,
        (Some("required"), Some("required")) => {
            let list = |v: &serde_json::Value, key: &str| -> Vec<String> {
                v[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            };
            let union = |key: &str| {
                let mut all = list(&url, key);
                all.extend(list(&method, key));
                all.sort();
                all.dedup();
                all
            };
            let mut guards = list(&url, "guards");
            guards.extend(list(&method, "guards"));
            let mut basis = list(&url, "basis");
            basis.extend(list(&method, "basis"));
            let signed_in = "signed-in user";
            let (outer, inner) = (
                url["caller"].as_str().unwrap_or_default(),
                method["caller"].as_str().unwrap_or_default(),
            );
            let caller = if outer == signed_in || outer == inner {
                inner.to_string()
            } else if inner == signed_in {
                outer.to_string()
            } else {
                format!("{inner} who is also {outer}")
            };
            let mut out = method.clone();
            out["guards"] = guards.into();
            out["basis"] = basis.into();
            out["roles"] = union("roles").into();
            let authorities = union("authorities");
            if !authorities.is_empty() {
                out["authorities"] = authorities.into();
            }
            out["caller"] = caller.into();
            out["prov"] = joined(&[&url["prov"], &method["prov"]]);
            out
        }
        _ => method,
    }
}

/// One provenance citing the evidence of several, at the weaker tier.
fn joined(provs: &[&serde_json::Value]) -> serde_json::Value {
    let read: Vec<Provenance> = provs
        .iter()
        .filter_map(|p| serde_json::from_value::<Provenance>((*p).clone()).ok())
        .collect();
    let evidence = read.iter().flat_map(|p| p.evidence.clone()).collect();
    let confidence = read
        .iter()
        .map(|p| p.confidence_tier)
        .min_by_key(|tier| match tier {
            ConfidenceTier::Confirmed => 3,
            ConfidenceTier::InferredStrong => 2,
            ConfidenceTier::InferredWeak => 1,
            ConfidenceTier::Gap => 0,
        })
        .unwrap_or(ConfidenceTier::InferredStrong);
    Provenance::new(
        Tier::Deterministic,
        confidence,
        evidence,
        super::EXTRACTOR_ID,
        b"auth",
    )
    .ok()
    .and_then(|p| serde_json::to_value(p).ok())
    .unwrap_or_default()
}

/// The reading of proven security annotations with their values: each
/// annotation by name, with its values, or `None` when a value is not a
/// literal.
fn evaluate(
    found: Vec<(String, Option<Vec<String>>)>,
    prov: serde_json::Value,
) -> serde_json::Value {
    let mut guards = Vec::new();
    let mut basis = Vec::new();
    let mut admits = Admits::default();
    let mut expressions = Vec::new();
    let mut permit = false;
    for (name, values) in &found {
        match name.as_str() {
            "PermitAll" => {
                permit = true;
                basis.push("@PermitAll".to_string());
            }
            "DenyAll" => {
                admits.deny_all = true;
                guards.push("@DenyAll".to_string());
                basis.push("@DenyAll".to_string());
            }
            _ => {
                let Some(values) = values else {
                    return serde_json::json!({
                        "state": "unknown",
                        "reasons": [format!("@{name} value is not a literal or constant")],
                    });
                };
                let url = name.starts_with(URL);
                basis.push(if url {
                    name.clone()
                } else {
                    format!("@{name}")
                });
                for value in values {
                    guards.push(if url {
                        name.clone()
                    } else {
                        format!("@{name}({value})")
                    });
                    match name.as_str() {
                        _ if url => {
                            let read = expression(value);
                            admits.roles.extend(read.roles);
                            admits.authorities.extend(read.authorities);
                            admits.authenticated |= read.authenticated;
                            admits.permit_all |= read.permit_all;
                            admits.deny_all |= read.deny_all;
                            admits.anonymous |= read.anonymous;
                            admits.other |= read.other;
                            admits.compound |= read.compound;
                        }
                        "PreAuthorize" | "PostAuthorize" => {
                            let read = expression(value);
                            admits.roles.extend(read.roles);
                            admits.authorities.extend(read.authorities);
                            admits.authenticated |= read.authenticated;
                            admits.permit_all |= read.permit_all;
                            admits.deny_all |= read.deny_all;
                            admits.anonymous |= read.anonymous;
                            admits.other |= read.other;
                            admits.compound |= read.compound;
                            expressions.push(value.clone());
                        }
                        // `@Secured` takes config attributes; `ROLE_` ones
                        // are roles, `IS_AUTHENTICATED_*` sign-in states.
                        "Secured" => match value.as_str() {
                            "IS_AUTHENTICATED_FULLY" | "IS_AUTHENTICATED_REMEMBERED" => {
                                admits.authenticated = true;
                            }
                            "IS_AUTHENTICATED_ANONYMOUSLY" => admits.permit_all = true,
                            _ => admits.roles.push(role_name(value)),
                        },
                        _ => admits.roles.push(role_name(value)),
                    }
                }
            }
        }
    }
    admits.roles.sort();
    admits.roles.dedup();
    admits.authorities.sort();
    admits.authorities.dedup();
    let only_permit = !admits.deny_all
        && admits.roles.is_empty()
        && admits.authorities.is_empty()
        && !admits.authenticated
        && !admits.other
        && !admits.anonymous;
    if (permit || admits.permit_all) && only_permit {
        return serde_json::json!({
            "state": "none",
            "basis": format!("{} admits every caller", basis.join(", ")),
            "prov": prov,
        });
    }
    let mut expression = expressions.join(" and ");
    if expression.is_empty()
        && let [(name, Some(values))] = &found[..]
        && name.starts_with(URL)
    {
        expression = values.join(" and ");
    }
    let mut out = serde_json::json!({
        "state": "required",
        "guards": guards,
        "basis": basis,
        "roles": admits.roles,
        "caller": caller(&admits, &expression),
        "prov": prov,
    });
    if !admits.authorities.is_empty() {
        out["authorities"] = admits.authorities.clone().into();
    }
    if !expressions.is_empty() {
        out["expression"] = expression.into();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spel_expressions_name_roles_and_callers() {
        let read = expression("hasRole('ROLE_ADMIN') or hasAnyRole('EDITOR', 'OWNER')");
        assert_eq!(read.roles, ["ADMIN", "EDITOR", "OWNER"]);
        assert!(!read.other);
        assert_eq!(
            caller(&read, ""),
            "caller with any role of ADMIN, EDITOR, OWNER"
        );
        let read = expression("isAuthenticated()");
        assert_eq!(caller(&read, ""), "signed-in user");
        let read = expression("hasRole('USER') and #id == principal.id");
        assert!(read.other);
        assert_eq!(
            caller(&read, "hasRole('USER') and #id == principal.id"),
            "caller that `hasRole('USER') and #id == principal.id` admits"
        );
        assert!(expression("permitAll()").permit_all);
        assert!(!expression("permitAll()").other);
        // Both roles are required, and a negated role is no role to hold.
        for both in [
            "hasRole('ADMIN') and hasRole('AUDITOR')",
            "hasRole('ADMIN') && hasAuthority('audit')",
            "not hasRole('GUEST')",
            "!hasRole('GUEST')",
        ] {
            let read = expression(both);
            assert!(read.compound, "{both}");
            assert_eq!(caller(&read, both), format!("caller that `{both}` admits"));
        }
        assert!(!expression("hasRole('A') or hasAuthority('b')").compound);
    }
}
