//! Configuration reads (#1789, AC-0597, ADR-0059): `std::env::var(key)` and
//! `std::env::var_os(key)` as the shared `ConfigRead` shape
//! ([`adapters_fw::config_read`]).
//!
//! A call is a read only when its callee path provably resolves to the
//! standard library, by the resolution the reqwest and rdkafka sites use:
//! the qualified `std::env::var`, or `env::var` / `var` through a `use` of
//! `std::env`. A nearer item or `use` of the same name, a workspace crate or
//! declared module named `std`, a glob that could bring the name in, a
//! generic parameter of that name, and a value the function binds as `var`
//! all leave it unproven: such a look-alike yields nothing.
//!
//! The key is named only as a string literal argument; any other single
//! argument, a constant included, is a dynamic read. The compile-time
//! `env!` / `option_env!` macros and `std::env::vars()` are not read.

use super::*;
use adapters_fw::config_read::ConfigReadSite;
use http::{call_arguments, resolve_external_paths, unquote_string_literal};

/// The `std::env` functions that read one environment variable by name.
const READS: [&str; 2] = ["var", "var_os"];

/// Append the `ConfigRead` nodes and `READS_CONFIG` edges of one file.
pub(super) fn extract(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    bindings: &Bindings,
    functions: &HashMap<usize, String>,
    out: &mut Extraction,
) {
    let mut sites = Vec::new();
    // Values each function binds, computed once per function.
    let mut shadows: HashMap<usize, BTreeSet<String>> = HashMap::new();
    let call_query = query("(call_expression) @call");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(call_query, root, cx.source);
    while let Some(found) = matches.next() {
        let call = found.captures()[0].node;
        let Some(callee) = call.child_by_field_name("function") else {
            continue;
        };
        if !matches!(callee.kind(), "identifier" | "scoped_identifier") {
            continue;
        }
        let text = cx.text(&callee);
        let first = text.split("::").find(|part| !part.trim().is_empty());
        if first.is_some_and(|first| reqwest::generic_names(cx, call).contains(first.trim())) {
            continue;
        }
        let owner = enclosing_function(call, functions);
        let bound = owner.as_ref().map(|(_, function)| {
            &*shadows
                .entry(function.start_byte())
                .or_insert_with(|| value_bindings(cx, *function))
        });
        let scopes = visible_scopes(cx, call);
        let paths = resolve_external_paths(cx, bindings, &scopes, text, bound);
        let [path] = paths.as_slice() else {
            continue;
        };
        let standard = matches!(
            path.iter().map(String::as_str).collect::<Vec<_>>()[..],
            ["std", "env", read] if READS.contains(&read)
        );
        let [key] = call_arguments(call)[..] else {
            continue;
        };
        if !standard {
            continue;
        }
        sites.push(ConfigReadSite {
            key: unquote_string_literal(cx, key).filter(|key| !key.is_empty()),
            language: "rust",
            symbol: owner.map(|(symbol, _)| symbol),
            path: cx.path.to_string(),
            byte_start: call.start_byte() as u64,
            byte_end: call.end_byte() as u64,
        });
    }
    let source = adapters_fw::observed::ObservedSource {
        repo: cx.id.repo,
        commit: cx.id.commit,
        extractor: EXTRACTOR_ID,
    };
    let (nodes, edges) = adapters_fw::config_read::assemble(&source, &sites);
    out.nodes.extend(nodes);
    out.edges.extend(edges);
}
