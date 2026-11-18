//! Configuration reads (#1789, AC-0597, ADR-0059): `System.getenv("KEY")`
//! as the shared `ConfigRead` shape ([`adapters_fw::config_read`]).
//!
//! `System` is `java.lang.System` only through Kotlin's default imports,
//! the lowest-priority scope, so the read is proven by ruling out everything
//! that resolves first:
//!
//! - **In the file:** `System` appears only as the receiver of a member
//!   access (`System.getenv`, `System.out`). Any other use is a declaration
//!   or a type of that name (a local, parameter, property, class, object,
//!   type alias, or an import or import alias ending in `System`) and voids
//!   the file's reads.
//! - **Star imports** come before default imports. A file's reads stand only
//!   when every package it star-imports is declared by this repository,
//!   where the next rule covers it; a star import of anything else may
//!   bring a `System` into scope and voids them.
//! - **In the repository:** no Kotlin source declares or imports anything
//!   named `System` and no product Java source declares a top-level type of
//!   that name. Otherwise it may be a same-package or star-imported
//!   declaration, so every Kotlin read is withheld (fail closed).
//!
//! Not ruled out: a member named `System` reached through an implicit
//! receiver or a supertype declared outside the repository, and a Java
//! field or nested type of that name inherited from a repository class.
//!
//! The key is named only as a pure string literal; any other single
//! argument, a constant or a template included, is a dynamic read.
//! `System.getenv()` (the whole map) and `java.lang.System.getenv(…)` are
//! not read.

use super::rest_template::named_children;
use super::*;
use adapters_fw::config_read::ConfigReadSite;

const SYSTEM: &str = "System";

/// One file's `System.getenv(key)` sites and the packages it star-imports.
#[derive(Debug, Clone)]
pub(super) struct Pending {
    sites: Vec<ConfigReadSite>,
    star_imports: Vec<String>,
}

/// The file's candidate sites, and whether the file gives `System` a
/// meaning of its own (which voids them, here and repo-wide).
pub(super) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    functions: &[(TsNode<'_>, String)],
    imports: &Imports,
) -> (Option<Pending>, bool) {
    let mut sites = Vec::new();
    let mut rebinds = false;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let children = named_children(node);
        if children.is_empty() && node.kind().ends_with("identifier") && cx.text(&node) == SYSTEM {
            let receiver = node.parent().is_some_and(|parent| {
                parent.kind() == "navigation_expression" && parent.named_child(0) == Some(node)
            });
            rebinds |= !receiver;
        }
        if node.kind() == "call_expression"
            && let [callee, arguments] = children[..]
            && callee.kind() == "navigation_expression"
            && arguments.kind() == "value_arguments"
            && let [receiver, name] = named_children(callee)[..]
            && receiver.kind() == "identifier"
            && cx.text(&receiver) == SYSTEM
            && name.kind() == "identifier"
            && cx.text(&name) == "getenv"
            && let [argument] = code_children(arguments)[..]
            && argument.kind() == "value_argument"
        {
            // A named or spread argument is not read as a key name.
            let key = match code_children(argument)[..] {
                [value] => pure_string_literal(cx, value).filter(|key| !key.is_empty()),
                _ => None,
            };
            sites.push(ConfigReadSite {
                key,
                language: "kotlin",
                symbol: rest_template::enclosing(functions, node).cloned(),
                path: cx.path.to_string(),
                byte_start: node.start_byte() as u64,
                byte_end: node.end_byte() as u64,
            });
        }
        stack.extend(children.into_iter().rev());
    }
    let pending = (!rebinds && !sites.is_empty()).then(|| Pending {
        sites,
        star_imports: imports.wildcard_packages.clone(),
    });
    (pending, rebinds)
}

/// Append the `ConfigRead` nodes and `READS_CONFIG` edges of the pending
/// files whose star imports all name a package `declared` knows, unless
/// some source gives `System` another meaning.
pub(super) fn resolve(
    id: &SourceId<'_>,
    pending: Vec<Pending>,
    system_rebound: bool,
    declared: impl Fn(&str) -> bool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    if system_rebound {
        return;
    }
    let sites: Vec<ConfigReadSite> = pending
        .into_iter()
        .filter(|file| file.star_imports.iter().all(|package| declared(package)))
        .flat_map(|file| file.sites)
        .collect();
    let source = adapters_fw::observed::ObservedSource {
        repo: id.repo,
        commit: id.commit,
        extractor: EXTRACTOR_ID,
    };
    let (read_nodes, read_edges) = adapters_fw::config_read::assemble(&source, &sites);
    nodes.extend(read_nodes);
    edges.extend(read_edges);
}
