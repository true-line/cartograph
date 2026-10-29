//! `net/http` client calls (#1279, AC-0434).
//!
//! A call is a client call only when its package operand is the file's
//! `net/http` import: `http.Get` / `Head` / `Post` / `PostForm`, the same
//! verbs on `http.DefaultClient`, and `http.NewRequest(method, url, body)` /
//! `http.NewRequestWithContext(ctx, method, url, body)`. The operand is the
//! import only when the top-level declaration around the call never uses
//! the name as anything but a package qualifier; a parameter, local or type
//! of that name shadows it. Go forbids a package-level name that collides
//! with a file's import, so no other scope can rebind it.
//!
//! A method is an `http.MethodX` constant or a plain-word literal as
//! written (`""` is GET, as in `net/http`); anything else is `?`. A URL is a
//! literal only as exact text (a raw literal, or an interpreted one with no
//! escape); anything else is a computed URL the stitcher keeps as a Gap.
//! Requests sent through a constructed `http.Client` value are read through
//! their `NewRequest` site only.

use super::*;
use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;

const VERBS: [(&str, &str); 4] = [
    ("Get", "GET"),
    ("Head", "HEAD"),
    ("Post", "POST"),
    ("PostForm", "POST"),
];

/// The method an `http.MethodX` constant names.
fn method_constant(name: &str) -> Option<&'static str> {
    Some(match name {
        "MethodGet" => "GET",
        "MethodHead" => "HEAD",
        "MethodPost" => "POST",
        "MethodPut" => "PUT",
        "MethodPatch" => "PATCH",
        "MethodDelete" => "DELETE",
        "MethodConnect" => "CONNECT",
        "MethodOptions" => "OPTIONS",
        "MethodTrace" => "TRACE",
        _ => return None,
    })
}

/// The top-level declaration `node` sits in.
pub(super) fn top_level(mut node: TsNode<'_>) -> TsNode<'_> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "source_file" {
            break;
        }
        node = parent;
    }
    node
}

/// Whether `name` is used in `scope` as anything but the operand of a
/// selector, which only a local value or type of that name can be.
pub(super) fn shadowed(cx: &FileCx<'_>, scope: TsNode<'_>, name: &str) -> bool {
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier") && cx.text(&node) == name {
            let qualifier = node.parent().is_some_and(|parent| {
                parent.kind() == "selector_expression"
                    && parent.child_by_field_name("operand") == Some(node)
            });
            if !qualifier || node.kind() == "type_identifier" {
                return true;
            }
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    false
}

/// Per-file proof context.
pub(super) struct Proof<'a> {
    cx: &'a FileCx<'a>,
    /// Local names the file binds to `net/http`.
    names: Vec<&'a str>,
}

impl<'a> Proof<'a> {
    pub(super) fn new(cx: &'a FileCx<'a>, bindings: &'a HashMap<String, ImportBinding>) -> Self {
        let mut names = bindings
            .iter()
            .filter(|(_, binding)| binding.path == "net/http")
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        Self { cx, names }
    }

    /// Whether `node` is an identifier naming the `net/http` import at `at`.
    fn is_http(&self, node: TsNode<'_>, at: TsNode<'_>) -> bool {
        node.kind() == "identifier"
            && self.names.contains(&self.cx.text(&node))
            && !shadowed(self.cx, top_level(at), self.cx.text(&node))
    }

    /// `pkg.member` with `pkg` the `net/http` import: the member name.
    fn http_member(&self, node: TsNode<'_>, at: TsNode<'_>) -> Option<&'a str> {
        if node.kind() != "selector_expression" {
            return None;
        }
        let operand = node.child_by_field_name("operand")?;
        let field = node.child_by_field_name("field")?;
        self.is_http(operand, at).then(|| self.cx.text(&field))
    }

    fn url(&self, node: TsNode<'_>) -> IdentityExpr {
        let text = self.cx.text(&node);
        let exact = match node.kind() {
            "raw_string_literal" if !text.contains('\r') => unquote(text),
            "interpreted_string_literal" if !text.contains('\\') => unquote(text),
            _ => None,
        };
        match exact {
            Some(url) => IdentityExpr::Literal(url),
            None => IdentityExpr::Computed(text.trim().to_string()),
        }
    }

    fn method(&self, node: TsNode<'_>, at: TsNode<'_>) -> String {
        if let Some(method) = self.http_member(node, at).and_then(method_constant) {
            return method.to_string();
        }
        let text = self.cx.text(&node);
        let literal = match node.kind() {
            "raw_string_literal" | "interpreted_string_literal" => unquote(text),
            _ => None,
        };
        match literal {
            Some(method) if method.is_empty() => "GET".to_string(),
            Some(method) if method.bytes().all(|b| b.is_ascii_alphabetic()) => method,
            _ => "?".to_string(),
        }
    }

    /// The method and URL of a proven client call.
    pub(super) fn site(&self, call: TsNode<'_>) -> Option<(String, IdentityExpr)> {
        let callee = call.child_by_field_name("function")?;
        if callee.kind() != "selector_expression" {
            return None;
        }
        let member = self.cx.text(&callee.child_by_field_name("field")?);
        let verb = VERBS.iter().find(|(name, _)| *name == member);
        // The member is checked first: the scope walk is the costly proof.
        if verb.is_none() && !matches!(member, "NewRequest" | "NewRequestWithContext") {
            return None;
        }
        let args = call_arguments(call);
        let operand = callee.child_by_field_name("operand")?;
        if let Some((_, method)) = verb
            && (self.is_http(operand, call)
                || self.http_member(operand, call) == Some("DefaultClient"))
        {
            return Some((method.to_string(), self.url(*args.first()?)));
        }
        if !self.is_http(operand, call) {
            return None;
        }
        match member {
            "NewRequest" => Some((self.method(*args.first()?, call), self.url(*args.get(1)?))),
            "NewRequestWithContext" => {
                Some((self.method(*args.get(1)?, call), self.url(*args.get(2)?)))
            }
            _ => None,
        }
    }

    /// A fetch site for `call` owned by `symbol`, if it is a client call.
    pub(super) fn fetch_site(&self, call: TsNode<'_>, symbol: Option<String>) -> Option<FetchSite> {
        let (method, url) = self.site(call)?;
        Some(FetchSite {
            method,
            url,
            symbol,
            path: self.cx.path.to_string(),
            byte_start: call.start_byte() as u64,
            byte_end: call.end_byte() as u64,
            base: None,
            entry: None,
        })
    }
}
