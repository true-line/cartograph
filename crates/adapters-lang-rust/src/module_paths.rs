//! `#[path = "..."] mod name;` (#2041): a file wired in by a path attribute
//! is the module its declaration names, not the module its location
//! suggests. `src/curated.rs` declaring
//! `#[path = "curated_drafts_tests.rs"] mod tests;` makes
//! `src/curated_drafts_tests.rs` the module `crate::curated::tests`.
//!
//! Only declarations at the top level of a file are read: there the path is
//! relative to the directory of the declaring file. A declaration inside an
//! inline `mod {}` block (whose path also depends on the block) and one
//! under `cfg_attr` keep the location's module.

use super::{CrateInfo, language, module_for, query};
use std::collections::{BTreeMap, HashMap};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, QueryCursor};

/// Whether `source` may hold a path attribute; files without one are not
/// parsed by this pass.
pub(super) fn mentions_path(source: &[u8]) -> bool {
    source.windows(6).any(|w| w == b"#[path")
}

/// `(module name, path as written)` of each top-level `mod name;` that has
/// a `#[path = "..."]` outer attribute.
pub(super) fn declarations(source: &[u8]) -> Vec<(String, String)> {
    let mut parser = Parser::new();
    if parser.set_language(&language()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut found = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query("(mod_item name: (identifier) @name)"), root, source);
    while let Some(m) = matches.next() {
        let name_node = m.captures()[0].node;
        let Some(item) = name_node.parent() else {
            continue;
        };
        if item.parent().is_none_or(|p| p.id() != root.id())
            || item.child_by_field_name("body").is_some()
        {
            continue;
        }
        let Ok(name) = name_node.utf8_text(source) else {
            continue;
        };
        let mut sibling = item.prev_sibling();
        while let Some(attr) = sibling {
            match attr.kind() {
                "attribute_item" => {
                    if let Some(path) = attr.utf8_text(source).ok().and_then(path_literal) {
                        found.push((name.to_string(), path));
                        break;
                    }
                }
                "line_comment" | "block_comment" => {}
                _ => break,
            }
            sibling = attr.prev_sibling();
        }
    }
    found
}

/// The literal of `#[path = "..."]`, `None` for any other attribute.
fn path_literal(attr: &str) -> Option<String> {
    let inner = attr.trim().strip_prefix("#[")?.strip_suffix(']')?.trim();
    let value = inner.strip_prefix("path")?.trim_start().strip_prefix('=')?;
    let value = value.trim();
    let literal = value.strip_prefix('"')?.strip_suffix('"')?;
    (!literal.is_empty() && !literal.contains('\\')).then(|| literal.to_string())
}

/// The repo-relative file `literal` names from `declarer`: relative to the
/// declaring file's directory, with `.` and `..` folded. `None` when it
/// leaves the repository or is absolute.
pub(super) fn resolve(declarer: &str, literal: &str) -> Option<String> {
    if literal.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = declarer.split('/').collect();
    parts.pop();
    for segment in literal.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// The module of every file a path attribute wires in, keyed by its
/// repo-relative path: the declaring file's module (itself resolved the
/// same way) plus the declared name, in the declaring file's crate.
/// `declared` maps each declaring file to its declarations. A file two
/// declarations name takes the first declaring file in path order.
pub(super) fn modules(
    declared: &BTreeMap<String, Vec<(String, String)>>,
    crates: &[CrateInfo],
) -> HashMap<String, (Vec<String>, String)> {
    let mut parent: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (declarer, decls) in declared {
        for (name, literal) in decls {
            if let Some(target) = resolve(declarer, literal)
                && target != *declarer
            {
                parent
                    .entry(target)
                    .or_insert_with(|| (declarer.clone(), name.clone()));
            }
        }
    }
    let mut out = HashMap::new();
    for target in parent.keys() {
        // Walk up the declarations to a file wired in by location; a cycle
        // keeps the location's module.
        let mut chain = vec![target.clone()];
        let mut names = Vec::new();
        let mut at = target.clone();
        let mut cyclic = false;
        while let Some((declarer, name)) = parent.get(&at) {
            names.push(name.clone());
            if chain.contains(declarer) {
                cyclic = true;
                break;
            }
            chain.push(declarer.clone());
            at = declarer.clone();
        }
        if cyclic {
            continue;
        }
        let (mut module, crate_name) = module_for(&at, crates);
        module.extend(names.into_iter().rev());
        out.insert(target.clone(), (module, crate_name));
    }
    out
}
