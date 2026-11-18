//! Apache HttpClient request constructors (#1411, AC-0478).
//!
//! `new HttpGet(<url>)` and its siblings are client calls only when the
//! constructed type is proven to be one of the request classes of
//! HttpClient 4 (`org.apache.http.client.methods`) or 5
//! (`org.apache.hc.client5.http.classic.methods`), through a single-type
//! import in a file declaring no type of that name or a fully qualified
//! spelling that no declared or imported `org` type captures, with exactly
//! one argument and no class body. The class gives the method. The URL is
//! a literal only as a string literal or a proven `URI.create("<lit>")` or
//! `new URI("<lit>")` (as [`http_request::url`] reads it); anything else is
//! computed. A no-argument constructor sets its URI later and yields no
//! site. Request URLs are absolute, so there is no base.

use super::rest_template::Proof;
use super::webclient::arguments;
use super::*;
use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;

/// The request-class packages of HttpClient 4 and 5.
const PACKAGES: &[&str] = &[
    "org.apache.http.client.methods",
    "org.apache.hc.client5.http.classic.methods",
];

/// Request classes and the method each sends.
const CLASSES: &[(&str, &str)] = &[
    ("HttpGet", "GET"),
    ("HttpHead", "HEAD"),
    ("HttpPost", "POST"),
    ("HttpPut", "PUT"),
    ("HttpPatch", "PATCH"),
    ("HttpDelete", "DELETE"),
    ("HttpOptions", "OPTIONS"),
    ("HttpTrace", "TRACE"),
];

/// The method a proven request-class spelling sends.
fn method(proof: &Proof<'_>, spelling: &str) -> Option<&'static str> {
    let spelling: String = spelling.chars().filter(|c| !c.is_whitespace()).collect();
    // A file-declared or imported `org` type captures a qualified spelling,
    // as in `webclient::static_call`.
    if spelling.starts_with("org.")
        && (proof.declared.contains("org") || proof.imports.bindings.contains_key("org"))
    {
        return None;
    }
    CLASSES.iter().find_map(|&(class, verb)| {
        PACKAGES
            .iter()
            .any(|package| proof.names(&spelling, &format!("{package}.{class}")))
            .then_some(verb)
    })
}

/// Apache HttpClient request sites in this file, owned by their enclosing
/// method or constructor.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    methods_by_start: &HashMap<usize, String>,
) -> Vec<FetchSite> {
    if !cx.text(&root).contains("apache") {
        return Vec::new();
    }
    let proof = Proof::new(cx, root, imports);
    let query = crate::queries::query("(object_creation_expression) @new");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    let mut out = Vec::new();
    while let Some(found) = matches.next() {
        let new = found.captures()[0].node;
        // An anonymous subclass may override the request method.
        let mut walk = new.walk();
        if new
            .named_children(&mut walk)
            .any(|child| child.kind() == "class_body")
        {
            continue;
        }
        let Some(verb) = new
            .child_by_field_name("type")
            .and_then(|ty| method(&proof, cx.text(&ty)))
        else {
            continue;
        };
        let [target] = arguments(new)[..] else {
            continue;
        };
        let Some((method, symbol)) = rest_template::enclosing(new, methods_by_start) else {
            continue;
        };
        let url = match string_literal(cx, target) {
            Some(literal) => IdentityExpr::Literal(literal),
            None => http_request::url(&proof, method, target),
        };
        // `Type.method`, naming the call in a computed-URL Gap.
        let entry = symbol
            .rsplit_once('#')
            .map(|(_, member)| member.split('(').next().unwrap_or(member).to_string());
        out.push(FetchSite {
            straight_line: None,
            method: verb.to_string(),
            url,
            symbol: Some(symbol),
            path: cx.path.to_string(),
            byte_start: new.start_byte() as u64,
            byte_end: new.end_byte() as u64,
            base: None,
            entry,
        });
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
