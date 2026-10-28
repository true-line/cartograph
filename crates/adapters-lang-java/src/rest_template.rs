//! Spring `RestTemplate` client calls (#1283, AC-0435).
//!
//! A call is a client call only when its receiver is proven to be
//! `org.springframework.web.client.RestTemplate`: a `new RestTemplate(…)`
//! expression, or a name whose visible declaration (local, parameter, or a
//! field of the immediately enclosing named type, as
//! [`jpa::receiver_type`] reads it) spells that type. The spelling is the
//! fully qualified name, or the simple name under its single-type import in
//! a file that declares no type or type parameter of that name. Wildcard
//! imports prove nothing, since a same-package type would win.
//!
//! Verb methods give their method. `exchange`/`execute(url, method, …)`
//! take an import-proven `HttpMethod.X` constant whose first segment no
//! visible variable, declared type or imported type obscures, else `?`. The URL is the
//! first argument, a literal only as an exact string literal (URI
//! templates included); anything else is a computed URL the stitcher keeps
//! as a Gap.

use super::*;
use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;

const REST_TEMPLATE: &str = "org.springframework.web.client.RestTemplate";
const HTTP_METHOD: &str = "org.springframework.http.HttpMethod";

/// The method a verb method sends, or `None` for `exchange`/`execute`.
fn verb(name: &str) -> Option<Option<&'static str>> {
    Some(Some(match name {
        "getForObject" | "getForEntity" => "GET",
        "headForHeaders" => "HEAD",
        "postForObject" | "postForEntity" | "postForLocation" => "POST",
        "put" => "PUT",
        "patchForObject" => "PATCH",
        "delete" => "DELETE",
        "optionsForAllow" => "OPTIONS",
        "exchange" | "execute" => return Some(None),
        _ => return None,
    }))
}

/// Simple names of every type and type parameter the file declares.
pub(crate) fn declared_names(cx: &FileCx<'_>, root: TsNode<'_>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let name = match node.kind() {
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration" => node.child_by_field_name("name"),
            "type_parameter" => {
                let mut walk = node.walk();
                node.named_children(&mut walk)
                    .find(|child| child.kind() == "type_identifier")
            }
            _ => None,
        };
        if let Some(name) = name {
            names.insert(cx.text(&name).to_string());
        }
        let mut walk = node.walk();
        stack.extend(node.named_children(&mut walk));
    }
    names
}

struct Proof<'a> {
    cx: &'a FileCx<'a>,
    imports: &'a Imports,
    declared: BTreeSet<String>,
}

impl Proof<'_> {
    /// Whether a type spelling names `fqn` in this file.
    fn names(&self, spelling: &str, fqn: &str) -> bool {
        let spelling: String = spelling.chars().filter(|c| !c.is_whitespace()).collect();
        if spelling == fqn {
            return true;
        }
        let simple = fqn.rsplit('.').next().unwrap_or(fqn);
        spelling == simple
            && self.imports.bindings.get(simple).map(String::as_str) == Some(fqn)
            && !self.declared.contains(simple)
    }

    /// Whether `object` at `call` in `method` is a proven `RestTemplate`.
    fn is_client(&self, method: TsNode<'_>, call: TsNode<'_>, object: TsNode<'_>) -> bool {
        let cx = self.cx;
        let (receiver, this_only) = match object.kind() {
            "object_creation_expression" => {
                // An anonymous subclass may override the verb methods.
                let mut walk = object.walk();
                let anonymous = object
                    .named_children(&mut walk)
                    .any(|child| child.kind() == "class_body");
                return !anonymous
                    && object.child_by_field_name("type").is_some_and(|ty| {
                        ty.kind() != "generic_type" && self.names(cx.text(&ty), REST_TEMPLATE)
                    });
            }
            "identifier" => (cx.text(&object), false),
            "field_access" => {
                let (Some(base), Some(field)) = (
                    object.child_by_field_name("object"),
                    object.child_by_field_name("field"),
                ) else {
                    return false;
                };
                if base.kind() != "this" {
                    return false;
                }
                (cx.text(&field), true)
            }
            _ => return false,
        };
        jpa::receiver_type(cx, method, call, receiver, this_only)
            .is_some_and(|ty| self.names(&ty, REST_TEMPLATE))
    }

    /// The method an `HttpMethod.X` argument names, else `?`.
    fn method(&self, method: TsNode<'_>, call: TsNode<'_>, argument: TsNode<'_>) -> String {
        let cx = self.cx;
        let constant = (argument.kind() == "field_access")
            .then(|| {
                let object = argument.child_by_field_name("object")?;
                let field = argument.child_by_field_name("field")?;
                let spelling = cx.text(&object);
                // In expression context a variable obscures a type, and a
                // type obscures a package, so the first segment of the
                // spelling must name neither.
                let first = spelling.split('.').next().unwrap_or(spelling).trim();
                let proven = self.names(spelling, HTTP_METHOD)
                    && !jpa::names_a_variable(cx, method, call, first)
                    && (!spelling.contains('.')
                        || (!self.declared.contains(first)
                            && !self.imports.bindings.contains_key(first)));
                proven.then(|| cx.text(&field))
            })
            .flatten();
        match constant {
            Some(
                verb @ ("GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "TRACE"),
            ) => verb.to_string(),
            _ => "?".to_string(),
        }
    }
}

/// `RestTemplate` fetch sites in this file, owned by their enclosing method
/// or constructor.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    imports: &Imports,
    methods_by_start: &HashMap<usize, String>,
) -> Vec<FetchSite> {
    let named = |fqn: &str| {
        let simple = fqn.rsplit('.').next().unwrap_or(fqn);
        imports.bindings.get(simple).map(String::as_str) == Some(fqn)
            || cx.text(&root).contains(fqn)
    };
    if !named(REST_TEMPLATE) {
        return Vec::new();
    }
    let proof = Proof {
        cx,
        imports,
        declared: declared_names(cx, root),
    };
    let query = crate::queries::query("(method_invocation) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, cx.source);
    let mut out = Vec::new();
    while let Some(found) = matches.next() {
        let call = found.captures()[0].node;
        let (Some(object), Some(name), Some(arguments)) = (
            call.child_by_field_name("object"),
            call.child_by_field_name("name"),
            call.child_by_field_name("arguments"),
        ) else {
            continue;
        };
        let Some(fixed) = verb(cx.text(&name)) else {
            continue;
        };
        let mut walk = arguments.walk();
        let args = arguments
            .named_children(&mut walk)
            .filter(|argument| !is_comment(*argument))
            .collect::<Vec<_>>();
        let Some(url) = args.first().copied() else {
            continue;
        };
        if fixed.is_none() && args.len() < 4 {
            continue;
        }
        // The innermost method or constructor: owner and binding scope.
        let mut node = call;
        let mut enclosing = None;
        while let Some(parent) = node.parent() {
            if let Some(symbol) = methods_by_start.get(&parent.start_byte())
                && matches!(
                    parent.kind(),
                    "method_declaration" | "constructor_declaration"
                )
            {
                enclosing = Some((parent, symbol.clone()));
                break;
            }
            node = parent;
        }
        let Some((method, symbol)) = enclosing else {
            continue;
        };
        if !proof.is_client(method, call, object) {
            continue;
        }
        let http_method = match fixed {
            Some(verb) => verb.to_string(),
            None => proof.method(method, call, args[1]),
        };
        out.push(FetchSite {
            method: http_method,
            url: match string_literal(cx, url) {
                Some(literal) => IdentityExpr::Literal(literal),
                None => IdentityExpr::Computed(cx.text(&url).to_string()),
            },
            symbol: Some(symbol),
            path: cx.path.to_string(),
            byte_start: call.start_byte() as u64,
            byte_end: call.end_byte() as u64,
            base: None,
            entry: None,
        });
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
