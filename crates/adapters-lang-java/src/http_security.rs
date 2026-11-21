//! Spring Security URL rules (#2020): the `authorizeHttpRequests` (and
//! legacy `authorizeRequests`) rules of an `HttpSecurity` configuration, in
//! source order, so an endpoint's caller can be read from the first rule
//! whose matcher covers it.
//!
//! A file proves the rules when it imports `HttpSecurity` (named, wildcard or
//! qualified). Each rule is a matcher (`requestMatchers`, `antMatchers`,
//! `mvcMatchers` with an optional `HttpMethod.X` and literal or constant
//! patterns, or `anyRequest()`) and an access call (`permitAll()`,
//! `denyAll()`, `authenticated()`, `fullyAuthenticated()`, `anonymous()`,
//! `rememberMe()`, `hasRole`/`hasAnyRole`/`hasAuthority`/`hasAnyAuthority`,
//! or `access("SpEL")`), read as the SpEL expression Spring would evaluate.
//!
//! Matching follows Spring's path patterns: `**` spans segments, `*` and
//! `{var}` match one segment. An endpoint's `{id}` segment matches a literal
//! pattern segment for only some requests, so such a rule leaves the reading
//! unknown rather than guessing. Rules inside an `if`, `switch` or loop are
//! conditional and decide nothing. Several filter chains, a
//! `securityMatcher` or a matcher the adapter does not read leave the
//! reading unknown.

use super::method_security::{Part, parts};
use super::{ConfidenceTier, FileCx, Imports, TsNode};

const BUILDERS: &str = "org.springframework.security.config.annotation.web.builders";

/// What a rule's matcher covers.
#[derive(Debug, Clone)]
pub(super) enum Matcher {
    Any,
    Patterns(Vec<Vec<Part>>),
    /// A matcher object, a regex or a dispatcher type: not read.
    Unread,
}

#[derive(Debug, Clone)]
pub(super) struct Rule {
    pub(super) verb: Option<String>,
    pub(super) matcher: Matcher,
    /// The access as SpEL; `None` when the access call is not read.
    pub(super) access: Option<Vec<Part>>,
    /// The rule as spelled, for the guard.
    pub(super) display: String,
    pub(super) prov: serde_json::Value,
    /// Whether the rule is stated under an `if`, `switch` or loop inside the
    /// configuring lambda, so it applies only under a condition.
    pub(super) conditional: bool,
}

/// The rules of one `authorizeHttpRequests` call.
#[derive(Debug, Clone)]
pub(super) struct Chain {
    /// The enclosing method (one filter chain bean): `path@byte`.
    pub(super) bean: String,
    pub(super) conditional: bool,
    /// The bean narrows the chain with `securityMatcher`.
    pub(super) scoped: bool,
    pub(super) rules: Vec<Rule>,
}

pub(super) fn retarget(chains: &mut [Chain], mut retarget: impl FnMut(&mut serde_json::Value)) {
    for chain in chains {
        for rule in &mut chain.rules {
            let mut props = serde_json::json!({ "prov": rule.prov.take() });
            retarget(&mut props);
            rule.prov = props["prov"].take();
        }
    }
}

fn proven(imports: &Imports, root: TsNode<'_>, cx: &FileCx<'_>) -> bool {
    imports
        .bindings
        .get("HttpSecurity")
        .is_some_and(|fqn| fqn == &format!("{BUILDERS}.HttpSecurity"))
        || imports.wildcard_packages.iter().any(|p| p == BUILDERS)
        || cx.text(&root).contains(&format!("{BUILDERS}.HttpSecurity"))
}

fn arguments<'t>(call: TsNode<'t>) -> Vec<TsNode<'t>> {
    let Some(list) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|n| !matches!(n.kind(), "line_comment" | "block_comment"))
        .collect()
}

fn name<'s>(cx: &'s FileCx<'_>, call: TsNode<'_>) -> &'s str {
    call.child_by_field_name("name").map_or("", |n| cx.text(&n))
}

/// The calls of a chain from its receiver outward: `a.b().c()` from `a`.
fn chain_calls<'t>(mut outer: TsNode<'t>) -> (Vec<TsNode<'t>>, TsNode<'t>) {
    let mut calls = Vec::new();
    while outer.kind() == "method_invocation" {
        calls.push(outer);
        match outer.child_by_field_name("object") {
            Some(object) => outer = object,
            None => break,
        }
    }
    calls.reverse();
    (calls, outer)
}

/// The SpEL an access call states.
fn access(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    call: TsNode<'_>,
) -> Option<Vec<Part>> {
    let text = |s: &str| vec![Part::Text(s.to_string())];
    let args = arguments(call);
    match name(cx, call) {
        "permitAll" => Some(text("permitAll")),
        "denyAll" => Some(text("denyAll")),
        "authenticated" => Some(text("isAuthenticated()")),
        "fullyAuthenticated" => Some(text("isFullyAuthenticated()")),
        "rememberMe" => Some(text("isRememberMe()")),
        "anonymous" => Some(text("isAnonymous()")),
        "access" => {
            let mut out = Vec::new();
            parts(cx, imports, package, *args.first()?, &mut out)?;
            Some(out)
        }
        name @ ("hasRole" | "hasAnyRole" | "hasAuthority" | "hasAnyAuthority") => {
            let mut out = vec![Part::Text(format!("{name}("))];
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    out.push(Part::Text(",".into()));
                }
                out.push(Part::Text("\"".into()));
                parts(cx, imports, package, *arg, &mut out)?;
                out.push(Part::Text("\"".into()));
            }
            out.push(Part::Text(")".into()));
            Some(out)
        }
        _ => None,
    }
}

fn is_access(name: &str) -> bool {
    matches!(
        name,
        "permitAll"
            | "denyAll"
            | "authenticated"
            | "fullyAuthenticated"
            | "rememberMe"
            | "anonymous"
            | "access"
            | "hasRole"
            | "hasAnyRole"
            | "hasAuthority"
            | "hasAnyAuthority"
            | "hasIpAddress"
            | "not"
    )
}

fn matcher(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    call: TsNode<'_>,
) -> Option<(Option<String>, Matcher)> {
    match name(cx, call) {
        "anyRequest" => Some((None, Matcher::Any)),
        "requestMatchers" | "antMatchers" | "mvcMatchers" => {
            let mut args = arguments(call).into_iter().peekable();
            let mut verb = None;
            if let Some(first) = args.peek()
                && first.kind() == "field_access"
                && first
                    .child_by_field_name("object")
                    .is_some_and(|o| cx.text(&o).ends_with("HttpMethod"))
            {
                verb = first
                    .child_by_field_name("field")
                    .map(|f| cx.text(&f).to_string());
                args.next();
            }
            let mut patterns = Vec::new();
            for arg in args {
                let mut out = Vec::new();
                if parts(cx, imports, package, arg, &mut out).is_none() {
                    return Some((verb, Matcher::Unread));
                }
                patterns.push(out);
            }
            if patterns.is_empty() {
                // `requestMatchers(HttpMethod.GET)` covers every path.
                return Some((verb, Matcher::Any));
            }
            Some((verb, Matcher::Patterns(patterns)))
        }
        "regexMatchers" | "dispatcherTypeMatchers" => Some((None, Matcher::Unread)),
        _ => None,
    }
}

/// The rules one chain states, from its calls in order.
fn rules(
    cx: &FileCx<'_>,
    imports: &Imports,
    package: Option<&str>,
    calls: &[(TsNode<'_>, bool)],
) -> Vec<Rule> {
    let mut out = Vec::new();
    let mut pending: Option<(TsNode<'_>, Option<String>, Matcher)> = None;
    for (call, conditional) in calls {
        let called = name(cx, *call);
        if let Some((verb, matcher)) = matcher(cx, imports, package, *call) {
            pending = Some((*call, verb, matcher));
            continue;
        }
        if !is_access(called) {
            continue;
        }
        let Some((matcher_call, verb, matcher)) = pending.take() else {
            continue;
        };
        let spelled = |c: TsNode<'_>| {
            let start = c
                .child_by_field_name("name")
                .map_or(c.start_byte(), |n| n.start_byte());
            std::str::from_utf8(&cx.source[start..c.end_byte()]).unwrap_or("")
        };
        let display = format!("{}.{}", spelled(matcher_call), spelled(*call))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        out.push(Rule {
            verb,
            matcher,
            access: access(cx, imports, package, *call),
            display,
            prov: cx.prov_spanning(
                &[matcher_call, *call],
                ConfidenceTier::InferredStrong,
                "HttpSecurity rule",
            ),
            conditional: *conditional,
        });
    }
    out
}

/// The file's URL rule chains.
pub(super) fn chains(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    package: Option<&str>,
) -> Vec<Chain> {
    if !proven(imports, root, cx) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut found = Vec::new();
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
        if node.kind() == "method_invocation"
            && matches!(
                name(cx, node),
                "authorizeHttpRequests" | "authorizeRequests"
            )
        {
            found.push(node);
        }
    }
    found.sort_by_key(|n| n.start_byte());
    for call in found {
        let mut bean = None;
        let mut conditional = false;
        let mut up = call;
        while let Some(parent) = up.parent() {
            // A branch, loop or switch may not run the configuring call.
            if matches!(
                parent.kind(),
                "if_statement"
                    | "switch_expression"
                    | "switch_statement"
                    | "conditional_expression"
                    | "for_statement"
                    | "enhanced_for_statement"
                    | "while_statement"
                    | "do_statement"
            ) {
                conditional = true;
            }
            if parent.kind() == "method_declaration" {
                bean = Some(parent);
                break;
            }
            up = parent;
        }
        let Some(bean) = bean else {
            continue;
        };
        // Only a call on the bean's own `HttpSecurity` configures a chain.
        if !on_http_security(cx, call, bean) {
            continue;
        }
        let scoped = cx.text(&bean).contains("securityMatcher");
        let calls: Vec<(TsNode<'_>, bool)> = match arguments(call).first() {
            // `authorizeHttpRequests(authz -> authz.requestMatchers(…)…)`
            Some(lambda) if lambda.kind() == "lambda_expression" => {
                let Some(body) = lambda.child_by_field_name("body") else {
                    continue;
                };
                // A statement nested under an `if`, `switch`, loop or `try`
                // of the block states its rules under a condition.
                let statements: Vec<(TsNode<'_>, bool)> = if body.kind() == "block" {
                    let mut statements = Vec::new();
                    let mut cursor = body.walk();
                    for statement in body.named_children(&mut cursor) {
                        if statement.kind() == "expression_statement" {
                            statements.extend(statement.named_child(0).map(|s| (s, false)));
                            continue;
                        }
                        // Each outermost call chain in it, a nested lambda's too.
                        let mut stack = vec![statement];
                        let mut nested = Vec::new();
                        while let Some(node) = stack.pop() {
                            let outermost = node.kind() == "method_invocation"
                                && !node.parent().is_some_and(|parent| {
                                    parent.kind() == "method_invocation"
                                        && parent.child_by_field_name("object") == Some(node)
                                });
                            if outermost {
                                nested.push(node);
                            }
                            let mut walk = node.walk();
                            stack.extend(node.named_children(&mut walk));
                        }
                        nested.sort_by_key(|n| n.start_byte());
                        statements.extend(nested.into_iter().map(|s| (s, true)));
                    }
                    statements
                } else {
                    vec![(body, false)]
                };
                statements
                    .into_iter()
                    .flat_map(|(s, conditional)| {
                        chain_calls(s).0.into_iter().map(move |c| (c, conditional))
                    })
                    .collect()
            }
            Some(_) => continue,
            // `http.authorizeRequests().antMatchers(…).permitAll().and()…`
            None => {
                let mut calls = Vec::new();
                let mut node = call;
                while let Some(parent) = node.parent() {
                    if parent.kind() != "method_invocation"
                        || parent.child_by_field_name("object") != Some(node)
                    {
                        break;
                    }
                    if name(cx, parent) == "and" {
                        break;
                    }
                    calls.push((parent, false));
                    node = parent;
                }
                calls
            }
        };
        out.push(Chain {
            bean: format!("{}@{}", cx.path, bean.start_byte()),
            conditional,
            scoped,
            rules: rules(cx, imports, package, &calls),
        });
    }
    out
}

/// Whether `call`'s receiver chain starts at a parameter or local of the
/// bean method declared as `HttpSecurity`.
fn on_http_security(cx: &FileCx<'_>, call: TsNode<'_>, bean: TsNode<'_>) -> bool {
    let Some(object) = call.child_by_field_name("object") else {
        return false;
    };
    let (_, root) = chain_calls(object);
    if root.kind() != "identifier" {
        return false;
    }
    let name = cx.text(&root);
    let typed = |node: TsNode<'_>| {
        node.child_by_field_name("type").is_some_and(|t| {
            let t = cx.text(&t);
            t == "HttpSecurity" || t == format!("{BUILDERS}.HttpSecurity")
        })
    };
    let mut stack = vec![bean];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "formal_parameter"
                if typed(node)
                    && node
                        .child_by_field_name("name")
                        .is_some_and(|n| cx.text(&n) == name) =>
            {
                return true;
            }
            "local_variable_declaration" if typed(node) => {
                let mut walk = node.walk();
                if node
                    .children_by_field_name("declarator", &mut walk)
                    .any(|d| {
                        d.child_by_field_name("name")
                            .is_some_and(|n| cx.text(&n) == name)
                    })
                {
                    return true;
                }
            }
            _ => {}
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    false
}

/// How much of an endpoint a matcher covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Cover {
    None,
    Some,
    All,
}

fn template(segment: &str) -> bool {
    segment.starts_with('{') && segment.ends_with('}')
}

/// A `*`/`?` glob within one segment.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob(&pattern[1..], text) || (!text.is_empty() && glob(pattern, &text[1..]))
        }
        (Some(b'?'), Some(_)) => glob(&pattern[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => glob(&pattern[1..], &text[1..]),
        _ => false,
    }
}

fn segment(pattern: &str, endpoint: &str) -> Cover {
    if pattern == "*" || template(pattern) {
        return Cover::All;
    }
    if template(endpoint) || endpoint.contains('{') {
        return Cover::Some;
    }
    if glob(pattern.as_bytes(), endpoint.as_bytes()) {
        Cover::All
    } else {
        Cover::None
    }
}

fn segments(pattern: &[&str], endpoint: &[&str]) -> Cover {
    match pattern.split_first() {
        None => {
            if endpoint.is_empty() {
                Cover::All
            } else {
                Cover::None
            }
        }
        Some((&"**", rest)) => (0..=endpoint.len())
            .map(|k| segments(rest, &endpoint[k..]))
            .max()
            .unwrap_or(Cover::None),
        Some((first, rest)) => match endpoint.split_first() {
            None => Cover::None,
            Some((head, tail)) => segment(first, head).min(segments(rest, tail)),
        },
    }
}

fn split(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

fn path_cover(pattern: &str, path: &str) -> Cover {
    segments(&split(pattern), &split(path))
}

fn verb_cover(rule: Option<&str>, endpoint: &str) -> Cover {
    match rule {
        None => Cover::All,
        Some(verb) if verb.eq_ignore_ascii_case(endpoint) => Cover::All,
        Some(_) if endpoint == "ALL" || endpoint == "?" => Cover::Some,
        Some(_) => Cover::None,
    }
}

/// The URL reading of one endpoint.
pub(super) enum Url {
    /// No chain is recovered in the repository.
    Absent,
    Decided {
        access: String,
        display: String,
        prov: serde_json::Value,
    },
    Unknown(String),
}

/// The first rule that covers `method path`, with constants settled by
/// `text` (`None` for a constant the repository does not declare once).
pub(super) fn reading(
    chains: &[Chain],
    method: &str,
    path: &str,
    text: &dyn Fn(&[Part]) -> Option<String>,
) -> Url {
    if chains.is_empty() {
        return Url::Absent;
    }
    let beans: std::collections::BTreeSet<&str> = chains.iter().map(|c| c.bean.as_str()).collect();
    if beans.len() > 1 {
        return Url::Unknown(format!(
            "{} HttpSecurity filter chains; which one serves the endpoint is not read",
            beans.len()
        ));
    }
    if chains.iter().any(|c| c.scoped) {
        return Url::Unknown(
            "the HttpSecurity chain has a securityMatcher, which is not read".into(),
        );
    }
    // Rules apply in source order; a conditional rule met first leaves the
    // endpoint to the condition.
    for chain in chains {
        for rule in &chain.rules {
            // A rule for another verb never covers the endpoint, whatever
            // its matcher.
            let verb = verb_cover(rule.verb.as_deref(), method);
            if verb == Cover::None {
                continue;
            }
            let path_cover = match &rule.matcher {
                Matcher::Any => Cover::All,
                Matcher::Unread => {
                    return Url::Unknown(format!(
                        "URL rule {} has a matcher that is not read",
                        rule.display
                    ));
                }
                Matcher::Patterns(patterns) => {
                    let mut best = Cover::None;
                    for pattern in patterns {
                        let Some(pattern) = text(pattern) else {
                            return Url::Unknown(format!(
                                "URL rule {} names a constant the repository does not declare once",
                                rule.display
                            ));
                        };
                        best = best.max(path_cover(&pattern, path));
                    }
                    best
                }
            };
            match path_cover.min(verb) {
                Cover::None => continue,
                Cover::Some => {
                    return Url::Unknown(format!(
                        "URL rule {} covers only some requests of the endpoint",
                        rule.display
                    ));
                }
                Cover::All if chain.conditional || rule.conditional => {
                    return Url::Unknown(format!(
                        "URL rule {} applies only under a condition",
                        rule.display
                    ));
                }
                Cover::All => {
                    let Some(access) = rule.access.as_deref().and_then(text) else {
                        return Url::Unknown(format!(
                            "URL rule {} has an access check that is not read",
                            rule.display
                        ));
                    };
                    return Url::Decided {
                        access,
                        display: rule.display.clone(),
                        prov: rule.prov.clone(),
                    };
                }
            }
        }
    }
    Url::Unknown("no HttpSecurity URL rule covers the endpoint".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_path_patterns_cover_endpoints() {
        assert_eq!(path_cover("/api/**", "/api/users/{login}"), Cover::All);
        assert_eq!(path_cover("/api/admin/**", "/api/users"), Cover::None);
        assert_eq!(path_cover("/api/admin/**", "/api/admin"), Cover::All);
        assert_eq!(path_cover("/api/*/x", "/api/{id}/x"), Cover::All);
        assert_eq!(
            path_cover("/api/users/me", "/api/users/{login}"),
            Cover::Some
        );
        assert_eq!(path_cover("/*.js", "/main.js"), Cover::All);
        assert_eq!(path_cover("/api/register", "/api/register/"), Cover::All);
        assert_eq!(verb_cover(Some("POST"), "GET"), Cover::None);
        assert_eq!(verb_cover(Some("POST"), "ALL"), Cover::Some);
    }
}
