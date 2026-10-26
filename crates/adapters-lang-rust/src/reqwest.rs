//! reqwest HTTP client calls (#999).
//!
//! A call is a client call only when it provably reaches `reqwest`:
//! `reqwest::get(url)` / `reqwest::blocking::get(url)` through `use` or
//! qualified paths, or a verb method (`get`, `post`, `put`, `delete`,
//! `patch`, `head`, `request(Method::X, url)`) on a receiver proven to be a
//! reqwest `Client`. A receiver is proven when it is a `Client::new()` /
//! `Client::builder()…build()` expression, a name the function binds exactly
//! once to such an expression, a parameter typed `Client` (behind `&`,
//! `Arc` or `Rc`), or `self.<field>` where every field of that name in the
//! file is typed `Client`. A literal URL is reported as written; anything
//! else is a computed URL the stitcher keeps as a Gap. Lookalikes yield
//! nothing.

use super::*;
use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;
use http::{call_arguments, resolve_external_paths, unquote_string_literal};

const VERBS: [(&str, &str); 6] = [
    ("get", "GET"),
    ("post", "POST"),
    ("put", "PUT"),
    ("delete", "DELETE"),
    ("patch", "PATCH"),
    ("head", "HEAD"),
];

/// Per-function proof context.
struct Proof<'a, 't> {
    cx: &'a FileCx<'a>,
    bindings: &'a Bindings,
    function: TsNode<'t>,
    shadows: BTreeSet<String>,
    /// File-level field names whose every declaration is a reqwest Client.
    client_fields: &'a BTreeSet<String>,
}

impl<'t> Proof<'_, 't> {
    /// The single external path `text` resolves to at `node`, if exactly one.
    fn path(&self, node: TsNode<'_>, text: &str) -> Option<Vec<String>> {
        let scopes = visible_scopes(self.cx, node);
        let mut paths =
            resolve_external_paths(self.cx, self.bindings, &scopes, text, Some(&self.shadows));
        (paths.len() == 1).then(|| paths.remove(0))
    }

    fn is_client_type(&self, node: TsNode<'_>) -> bool {
        let mut text = self.cx.text(&node).trim();
        loop {
            let stripped = text
                .strip_prefix('&')
                .map(str::trim_start)
                .map(|t| t.strip_prefix("mut ").unwrap_or(t))
                .or_else(|| {
                    ["Arc<", "Rc<", "std::sync::Arc<", "std::rc::Rc<"]
                        .iter()
                        .find_map(|wrapper| text.strip_prefix(wrapper))
                        .and_then(|t| t.strip_suffix('>'))
                });
            match stripped {
                Some(inner) => text = inner.trim(),
                None => break,
            }
        }
        self.path(node, text)
            .is_some_and(|path| is_client_path(&path))
    }

    /// Whether `expr` evaluates to a reqwest `Client`.
    fn is_client_expr(&self, expr: TsNode<'_>) -> bool {
        match expr.kind() {
            "parenthesized_expression" | "try_expression" => expr
                .named_child(0)
                .is_some_and(|inner| self.is_client_expr(inner)),
            "call_expression" => {
                let Some(callee) = expr.child_by_field_name("function") else {
                    return false;
                };
                match callee.kind() {
                    "field_expression" => {
                        let (Some(receiver), Some(method)) = (
                            callee.child_by_field_name("value"),
                            callee.child_by_field_name("field"),
                        ) else {
                            return false;
                        };
                        match self.cx.text(&method) {
                            "unwrap" | "expect" => self.is_client_expr(receiver),
                            "build" => self.is_builder_expr(receiver),
                            _ => false,
                        }
                    }
                    "scoped_identifier" | "identifier" => self
                        .path(callee, self.cx.text(&callee))
                        .is_some_and(|path| {
                            path.split_last().is_some_and(|(last, ty)| {
                                matches!(last.as_str(), "new" | "default") && is_client_path(ty)
                            })
                        }),
                    _ => false,
                }
            }
            "identifier" => self.is_client_binding(expr),
            "field_expression" => self.is_client_field(expr),
            _ => false,
        }
    }

    /// `Client::builder()` / `ClientBuilder::new()` followed by any methods.
    fn is_builder_expr(&self, expr: TsNode<'_>) -> bool {
        if expr.kind() != "call_expression" {
            return false;
        }
        let Some(callee) = expr.child_by_field_name("function") else {
            return false;
        };
        match callee.kind() {
            "field_expression" => callee
                .child_by_field_name("value")
                .is_some_and(|receiver| self.is_builder_expr(receiver)),
            "scoped_identifier" => self
                .path(callee, self.cx.text(&callee))
                .is_some_and(|path| {
                    path.split_last().is_some_and(|(last, ty)| {
                        (last == "builder" && is_client_path(ty))
                            || (last == "new" && is_builder_path(ty))
                    })
                }),
            _ => false,
        }
    }

    /// A name the function binds exactly once, to a Client.
    fn is_client_binding(&self, name: TsNode<'_>) -> bool {
        let wanted = self.cx.text(&name);
        let mut sites = Vec::new();
        let mut stack = vec![self.function];
        while let Some(node) = stack.pop() {
            let binds =
                |pattern: TsNode<'_>| pattern_names(self.cx, pattern).iter().any(|n| n == wanted);
            match node.kind() {
                "let_declaration" | "parameter"
                    if node.child_by_field_name("pattern").is_some_and(binds) =>
                {
                    sites.push(node);
                }
                // Any other binding of the name fails closed.
                "for_expression" | "let_condition" | "match_pattern" | "closure_parameters"
                    if binds(node) =>
                {
                    return false;
                }
                _ => {}
            }
            let mut walk = node.walk();
            for child in node.named_children(&mut walk) {
                if child.kind() != "function_item" {
                    stack.push(child);
                }
            }
        }
        let [site] = sites.as_slice() else {
            return false;
        };
        let simple = site
            .child_by_field_name("pattern")
            .is_some_and(|pattern| pattern.kind() == "identifier");
        if !simple {
            return false;
        }
        match site.kind() {
            "parameter" => site
                .child_by_field_name("type")
                .is_some_and(|ty| self.is_client_type(ty)),
            _ => {
                site.child_by_field_name("type")
                    .is_some_and(|ty| self.is_client_type(ty))
                    || site
                        .child_by_field_name("value")
                        .is_some_and(|value| self.is_client_expr(value))
            }
        }
    }

    fn is_client_field(&self, expr: TsNode<'_>) -> bool {
        let (Some(value), Some(field)) = (
            expr.child_by_field_name("value"),
            expr.child_by_field_name("field"),
        ) else {
            return false;
        };
        value.kind() == "self" && self.client_fields.contains(self.cx.text(&field))
    }

    fn url(&self, node: TsNode<'_>) -> IdentityExpr {
        let node = if node.kind() == "reference_expression" {
            node.child_by_field_name("value").unwrap_or(node)
        } else {
            node
        };
        match unquote_string_literal(self.cx, node) {
            Some(literal) => IdentityExpr::Literal(literal),
            None => IdentityExpr::Computed(self.cx.text(&node).trim().to_string()),
        }
    }

    /// The method and URL argument of a proven client call.
    fn site(&self, call: TsNode<'t>) -> Option<(String, TsNode<'t>)> {
        let callee = call.child_by_field_name("function")?;
        let args = call_arguments(call);
        match callee.kind() {
            "scoped_identifier" | "identifier" => {
                let path = self.path(callee, self.cx.text(&callee))?;
                let free = matches!(
                    path.iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .as_slice(),
                    ["reqwest", "get"] | ["reqwest", "blocking", "get"]
                );
                let url = *args.first()?;
                free.then(|| ("GET".to_string(), url))
            }
            "field_expression" => {
                let receiver = callee.child_by_field_name("value")?;
                let method = self.cx.text(&callee.child_by_field_name("field")?);
                let (verb, url) = if method == "request" {
                    let verb = args.first()?;
                    let path = self.path(*verb, self.cx.text(verb))?;
                    let [krate, ty, name] = path.as_slice() else {
                        return None;
                    };
                    let known = matches!(krate.as_str(), "reqwest" | "http")
                        && ty == "Method"
                        && VERBS.iter().any(|(_, upper)| upper == name);
                    (known.then(|| name.clone())?, *args.get(1)?)
                } else {
                    let (_, upper) = VERBS.iter().find(|(lower, _)| *lower == method)?;
                    (upper.to_string(), *args.first()?)
                };
                self.is_client_expr(receiver).then_some((verb, url))
            }
            _ => None,
        }
    }
}

fn is_client_path(path: &[String]) -> bool {
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["reqwest", "Client"] | ["reqwest", "blocking", "Client"]
    )
}

fn is_builder_path(path: &[String]) -> bool {
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["reqwest", "ClientBuilder"] | ["reqwest", "blocking", "ClientBuilder"]
    )
}

/// Identifiers a pattern binds.
fn pattern_names(cx: &FileCx<'_>, node: TsNode<'_>) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            out.push(cx.text(&node).to_string());
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    out
}

/// Field names whose every declaration in the file is typed reqwest Client.
pub(super) fn client_fields(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &Bindings,
) -> BTreeSet<String> {
    let mut verdicts: BTreeMap<String, bool> = BTreeMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "field_declaration"
            && let (Some(name), Some(ty)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("type"),
            )
        {
            let proof = Proof {
                cx,
                bindings,
                function: node,
                shadows: BTreeSet::new(),
                client_fields: &BTreeSet::new(),
            };
            let client = proof.is_client_type(ty);
            let entry = verdicts.entry(cx.text(&name).to_string()).or_insert(true);
            *entry &= client;
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    verdicts
        .into_iter()
        .filter_map(|(name, client)| client.then_some(name))
        .collect()
}

/// Client calls made in `function` (nested functions excluded).
pub(super) fn extract_function_calls(
    cx: &FileCx<'_>,
    function: TsNode<'_>,
    symbol: &str,
    bindings: &Bindings,
    client_fields: &BTreeSet<String>,
    out: &mut Extraction,
) {
    let proof = Proof {
        cx,
        bindings,
        function,
        shadows: value_bindings(cx, function),
        client_fields,
    };
    let mut calls = Vec::new();
    let mut stack = vec![function];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            calls.push(node);
        }
        let mut walk = node.walk();
        for child in node.named_children(&mut walk) {
            if child.kind() != "function_item" {
                stack.push(child);
            }
        }
    }
    calls.sort_by_key(|call| call.start_byte());
    for call in calls {
        let Some((method, url)) = proof.site(call) else {
            continue;
        };
        out.fetch_sites.push(FetchSite {
            method,
            url: proof.url(url),
            symbol: Some(symbol.to_string()),
            path: cx.path.to_string(),
            byte_start: call.start_byte() as u64,
            byte_end: call.end_byte() as u64,
            base: None,
            entry: None,
        });
    }
}
