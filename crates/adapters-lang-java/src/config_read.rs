//! Configuration reads (#1789, AC-0597, ADR-0059): `System.getenv("KEY")`
//! as the shared `ConfigRead` shape ([`adapters_fw::config_read`]).
//!
//! `System` needs no import, so the read is proven by ruling out everything
//! that could give the name another meaning:
//!
//! - **In the file:** `System` appears only as the qualifier of a member
//!   access or call (`System.getenv`, `System.out`). Any other use is a
//!   declaration or a type of that name (a local, parameter, field, pattern,
//!   class, type parameter or an import ending in `System`) and voids the
//!   file's reads, as does a static wildcard import, which may bring a
//!   member named `System` into scope.
//! - **In the repository:** no Java source declares or imports anything
//!   named `System`. One that does may be a same-package type, or a field
//!   or member type another file inherits, so every Java read is withheld
//!   (fail closed).
//!
//! Not ruled out: a member named `System` inherited from a type declared
//! outside the repository, and a same-package Kotlin declaration.
//!
//! The key is named only as an exact string literal; any other single
//! argument, a constant included, is a dynamic read. `System.getenv()` (the
//! whole map) and the qualified `java.lang.System.getenv(…)` are not read.

use super::*;
use adapters_fw::config_read::ConfigReadSite;

const SYSTEM: &str = "System";

/// The file's proven `System.getenv(key)` sites, and whether the file gives
/// `System` a meaning of its own (which voids them, here and repo-wide).
pub(super) fn sites(
    cx: &FileCx<'_>,
    root: TsNode<'_>,
    methods_by_start: &HashMap<usize, String>,
) -> (Vec<ConfigReadSite>, bool) {
    let mut sites = Vec::new();
    let mut rebinds = false;
    let mut static_wildcard = false;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut walk = node.walk();
        let children: Vec<_> = node.named_children(&mut walk).collect();
        stack.extend(children.into_iter().rev());
        match node.kind() {
            "identifier" | "type_identifier" if cx.text(&node) == SYSTEM => {
                let qualifier = node.kind() == "identifier"
                    && node.parent().is_some_and(|parent| {
                        matches!(parent.kind(), "method_invocation" | "field_access")
                            && parent.child_by_field_name("object") == Some(node)
                    });
                rebinds |= !qualifier;
            }
            "import_declaration" => {
                let text: String = cx
                    .text(&node)
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                static_wildcard |= text.starts_with("importstatic") && text.ends_with(".*;");
            }
            "method_invocation" => {
                let reads = node.child_by_field_name("object").is_some_and(|object| {
                    object.kind() == "identifier" && cx.text(&object) == SYSTEM
                }) && node
                    .child_by_field_name("name")
                    .is_some_and(|name| cx.text(&name) == "getenv");
                let args = webclient::arguments(node);
                if let (true, [key]) = (reads, args.as_slice()) {
                    sites.push(ConfigReadSite {
                        key: string_literal(cx, *key).filter(|key| !key.is_empty()),
                        language: "java",
                        symbol: rest_template::enclosing(node, methods_by_start)
                            .map(|(_, symbol)| symbol),
                        path: cx.path.to_string(),
                        byte_start: node.start_byte() as u64,
                        byte_end: node.end_byte() as u64,
                    });
                }
            }
            _ => {}
        }
    }
    if rebinds || static_wildcard {
        sites.clear();
    }
    (sites, rebinds)
}

/// Append the `ConfigRead` nodes and `READS_CONFIG` edges of the pending
/// sites, unless some source gives `System` another meaning.
pub(super) fn resolve(
    id: &SourceId<'_>,
    sites: Vec<ConfigReadSite>,
    system_rebound: bool,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    if system_rebound {
        return;
    }
    let source = adapters_fw::observed::ObservedSource {
        repo: id.repo,
        commit: id.commit,
        extractor: EXTRACTOR_ID,
    };
    let (read_nodes, read_edges) = adapters_fw::config_read::assemble(&source, &sites);
    nodes.extend(read_nodes);
    edges.extend(read_edges);
}
