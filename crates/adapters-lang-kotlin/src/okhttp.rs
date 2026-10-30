//! Square OkHttp request builders (#1409, AC-0477), as Java reads them
//! (AC-0476).
//!
//! A chain `Request.Builder()…build()` is a client call only when its root
//! calls a proven `okhttp3.Request.Builder` constructor with no arguments:
//! a named-imported (or aliased) `Request` or `Request.Builder`, or a fully
//! qualified spelling, whose first segment no visible binding obscures.
//! Every link must be a `Request.Builder` member called positionally with
//! that member's arity and no trailing lambda; a function of the link's
//! name declared or imported by the file may be the one called, and a
//! literal argument the member's parameter cannot take selects another
//! function, so either proves nothing. The last method link wins and the
//! default is `GET`; `method("<NAME>", body)` gives its literal only as a
//! token without lowercase letters, else `?`. The last `url(…)` wins; only a
//! pure string literal is a literal, any other argument is computed. A chain
//! without `url` yields no site. Request URLs are absolute, so there is no
//! base.

use adapters_fw::client::FetchSite;
use adapters_fw::events::IdentityExpr;
use tree_sitter::Node as TsNode;

use super::rest_template::{self, Proof, named_children};
use super::webclient::member_call;
use super::{FileCx, Imports, pure_string_literal};

const REQUEST: &str = "okhttp3.Request";
const BUILDER: &str = "okhttp3.Request.Builder";

/// Whether a method name is an RFC 9110 token without lowercase letters, so
/// `PROPFIND` keeps its literal while a mistyped `patch` does not.
fn uppercase_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_uppercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
        })
}

/// Whether `node` is `Request.Builder()` on a proven OkHttp builder at
/// `site`.
fn builder_root(proof: &Proof<'_>, site: TsNode<'_>, node: TsNode<'_>) -> bool {
    let cx = proof.cx;
    if node.kind() != "call_expression" {
        return false;
    }
    let [head, arguments] = named_children(node)[..] else {
        return false;
    };
    if arguments.kind() != "value_arguments"
        || !named_children(arguments).is_empty()
        || !matches!(head.kind(), "identifier" | "navigation_expression")
    {
        return false;
    }
    let spelling: String = cx
        .text(&head)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    proof.names_value_at(site, &spelling, BUILDER)
        || spelling
            .strip_suffix(".Builder")
            .is_some_and(|outer| proof.names_value_at(site, outer, REQUEST))
}

/// Whether `argument` is a literal that a parameter of `String` type
/// (`string`) or of an object type (otherwise) cannot take; `null` fits
/// either.
fn foreign_literal(cx: &FileCx<'_>, argument: TsNode<'_>, string: bool) -> bool {
    let kind = argument.kind();
    let literal = (kind.ends_with("_literal")
        && !matches!(kind, "lambda_literal" | "object_literal"))
        || matches!(cx.text(&argument), "true" | "false");
    literal && cx.text(&argument) != "null" && !(string && kind == "string_literal")
}

/// OkHttp request sites in this file, owned by their innermost enclosing
/// function.
pub(crate) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &[(TsNode<'_>, String)],
    imports: &Imports,
) -> Vec<FetchSite> {
    // A qualified spelling may space its dots (`okhttp3 . Request`), so the
    // unimported fast path looks for the package name alone.
    if !imports
        .bindings
        .values()
        .any(|fqn| fqn == REQUEST || fqn == BUILDER)
        && !cx.text(&root).contains("okhttp3")
    {
        return Vec::new();
    }
    let proof = Proof::new(cx, root, imports);
    let mut out = Vec::new();
    let mut stack = vec![root];
    'calls: while let Some(build) = stack.pop() {
        stack.extend(named_children(build));
        let Some((mut node, "build", build_args)) = member_call(cx, build) else {
            continue;
        };
        if !build_args.is_empty() || proof.shadows("build") {
            continue;
        }
        let Some(symbol) = rest_template::enclosing(functions, build) else {
            continue;
        };
        // Walk back to `Request.Builder()`; links seen first are the last
        // called, so the first method and `url` found are the ones that apply.
        let mut verb = None;
        let mut url = None;
        while !builder_root(&proof, build, node) {
            let Some((receiver, name, args)) = member_call(cx, node) else {
                continue 'calls;
            };
            if proof.shadows(name) {
                continue 'calls;
            }
            // `fits(arg, true)` checks a `String` parameter; bodies, headers
            // and cache controls take objects, and a tag takes anything.
            let fits = |arg: &TsNode<'_>, string: bool| !foreign_literal(cx, *arg, string);
            let link = match (name, args.as_slice()) {
                ("get" | "head", []) => Some(name.to_ascii_uppercase()),
                ("delete" | "post" | "put" | "patch", [body]) if fits(body, false) => {
                    Some(name.to_ascii_uppercase())
                }
                ("delete", []) => Some("DELETE".to_string()),
                ("method", [method, body]) if fits(method, true) && fits(body, false) => Some(
                    pure_string_literal(cx, *method)
                        .filter(|method| uppercase_token(method))
                        .unwrap_or_else(|| "?".to_string()),
                ),
                // `url` takes a `String`, an `HttpUrl` or a `URL`.
                ("url", [target]) if fits(target, true) => {
                    url.get_or_insert(*target);
                    None
                }
                ("header" | "addHeader", [key, value]) if fits(key, true) && fits(value, true) => {
                    None
                }
                ("removeHeader", [key]) if fits(key, true) => None,
                ("headers" | "cacheControl", [value]) if fits(value, false) => None,
                ("tag", [_] | [_, _]) => None,
                _ => continue 'calls,
            };
            if let Some(link) = link {
                verb.get_or_insert(link);
            }
            node = receiver;
        }
        let Some(url) = url else {
            continue;
        };
        // `Type.function`, naming the call in a computed-URL Gap.
        let entry = symbol
            .rsplit_once('#')
            .map(|(_, member)| member.to_string());
        out.push(FetchSite {
            method: verb.unwrap_or_else(|| "GET".to_string()),
            url: match pure_string_literal(cx, url) {
                Some(literal) => IdentityExpr::Literal(literal),
                None => IdentityExpr::Computed(cx.text(&url).to_string()),
            },
            symbol: Some(symbol.clone()),
            path: cx.path.to_string(),
            byte_start: node.start_byte() as u64,
            byte_end: build.end_byte() as u64,
            base: None,
            entry,
        });
    }
    out.sort_by_key(|site| site.byte_start);
    out
}
