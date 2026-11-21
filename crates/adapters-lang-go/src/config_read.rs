//! Configuration reads (#1789, AC-0597, ADR-0059): `os.Getenv(key)` and
//! `os.LookupEnv(key)` as the shared `ConfigRead` shape
//! ([`adapters_fw::config_read`]).
//!
//! A call is a read only when its package operand is the file's import of
//! the standard `os` package (import path exactly `os`, under any alias),
//! proven as the `net/http` client calls are: the top-level declaration
//! around the call never uses the name as anything but a package qualifier,
//! so a parameter, local or type of that name shadows it. A function merely
//! named `Getenv`, another package aliased `os`, and a dot-import's bare
//! `Getenv(…)` yield nothing.
//!
//! The key is named only as exact text: a raw literal, or an interpreted
//! one with no escape. Any other single argument, a constant included, is a
//! dynamic read. Only the name and the span of the call are recorded.

use super::*;
use adapters_fw::config_read::ConfigReadSite;

/// The `os` functions that read one environment variable by name.
const READS: [&str; 2] = ["Getenv", "LookupEnv"];

/// The environment read `call` is: `Some(Some(key))` for a literal key,
/// `Some(None)` for a dynamic one, `None` when `call` is no proven read.
fn read(
    cx: &FileCx<'_>,
    bindings: &HashMap<String, ImportBinding>,
    call: TsNode<'_>,
) -> Option<Option<String>> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "selector_expression" {
        return None;
    }
    let operand = function.child_by_field_name("operand")?;
    let field = function.child_by_field_name("field")?;
    let name = cx.text(&operand);
    let standard = operand.kind() == "identifier"
        && bindings
            .get(name)
            .is_some_and(|binding| binding.path == "os" && binding.package_dir.is_none())
        && READS.contains(&cx.text(&field))
        && !client::shadowed(cx, client::top_level(call), name);
    if !standard {
        return None;
    }
    let [key] = call_arguments(call)[..] else {
        return None;
    };
    let text = cx.text(&key);
    Some(match key.kind() {
        "raw_string_literal" if !text.contains('\r') => unquote(text),
        "interpreted_string_literal" if !text.contains('\\') => unquote(text),
        _ => None,
    })
}

/// The configuration-read site `call` is, read from `symbol` (the file
/// when `None`).
pub(super) fn site(
    cx: &FileCx<'_>,
    bindings: &HashMap<String, ImportBinding>,
    call: TsNode<'_>,
    symbol: Option<String>,
) -> Option<ConfigReadSite> {
    Some(ConfigReadSite {
        key: read(cx, bindings, call)?,
        language: "go",
        symbol,
        path: cx.path.to_string(),
        byte_start: call.start_byte() as u64,
        byte_end: call.end_byte() as u64,
    })
}

/// Append the `ConfigRead` nodes and `READS_CONFIG` edges of one file.
pub(super) fn emit(cx: &FileCx<'_>, sites: &[ConfigReadSite], out: &mut Extraction) {
    let source = adapters_fw::observed::ObservedSource {
        repo: cx.id.repo,
        commit: cx.id.commit,
        extractor: EXTRACTOR_ID,
    };
    let (nodes, edges) = adapters_fw::config_read::assemble(&source, sites);
    out.nodes.extend(nodes);
    out.edges.extend(edges);
    // A store reached by hand through its conventional URL key (#2065).
    let stores = adapters_fw::config_read::store_sites(sites);
    let (nodes, edges) = adapters_fw::observed::assemble(&source, &stores);
    out.nodes.extend(nodes);
    out.edges.extend(edges);
}
