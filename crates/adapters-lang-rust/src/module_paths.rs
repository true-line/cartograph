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
//!
//! A declaration that is not followed is a Gap on the declaring file
//! (#2161): its path attribute is not a plain literal, sits under
//! `cfg_attr`, is inside an inline `mod {}` block or on one, leaves the
//! repository or is absolute, names a file that is not in the source set,
//! names a file another declaration already wires in, or is part of a
//! declaration cycle.

use super::{CrateInfo, language, module_for, query};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, QueryCursor};

/// Whether `source` may hold a path attribute, direct or under `cfg_attr`;
/// files without one are not parsed by this pass.
pub(super) fn mentions_path(source: &[u8]) -> bool {
    let has = |needle: &[u8]| source.windows(needle.len()).any(|w| w == needle);
    has(b"#[path") || (has(b"cfg_attr") && has(b"path"))
}

/// `(module name, path as written)` of each top-level `mod name;` that has
/// a `#[path = "..."]` outer attribute.
#[cfg(test)]
pub(super) fn declarations(source: &[u8]) -> Vec<(String, String)> {
    scan(source)
        .into_iter()
        .filter_map(|decl| decl.path.ok().map(|path| (decl.name, path)))
        .collect()
}

/// One `mod` item carrying a path attribute, followed or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Declaration {
    /// The declared module name.
    pub name: String,
    /// The path as written, or why the declaration is not followed.
    pub path: Result<String, &'static str>,
    /// Byte span of the path attribute.
    pub span: (usize, usize),
    /// The attribute as written, for a Gap.
    pub attribute: String,
}

/// Every `mod` item with a path attribute (`#[path ...]`, or `path` inside
/// `cfg_attr`), in source order. Only a top-level `mod name;` with a plain
/// `#[path = "..."]` literal is followed; any other is `Err(reason)`.
pub(super) fn scan(source: &[u8]) -> Vec<Declaration> {
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
        let Ok(name) = name_node.utf8_text(source) else {
            continue;
        };
        let top_level = item.parent().is_some_and(|p| p.id() == root.id());
        let has_body = item.child_by_field_name("body").is_some();
        let mut sibling = item.prev_sibling();
        while let Some(attr) = sibling {
            match attr.kind() {
                "attribute_item" => {
                    let text = attr.utf8_text(source).unwrap_or("");
                    if let Some(path) = path_attribute(text) {
                        let path = if !top_level {
                            Err("inline_module")
                        } else if has_body {
                            Err("module_body")
                        } else {
                            path
                        };
                        found.push(Declaration {
                            name: name.to_string(),
                            path,
                            span: (attr.start_byte(), attr.end_byte()),
                            attribute: shorten(text),
                        });
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

/// What a path-bearing attribute says: `Ok(literal)` for a plain
/// `#[path = "..."]`, `Err(reason)` for one this pass does not follow, and
/// `None` for an attribute that sets no path.
fn path_attribute(attr: &str) -> Option<Result<String, &'static str>> {
    let inner = attr.trim().strip_prefix("#[")?.strip_suffix(']')?.trim();
    if let Some(rest) = inner.strip_prefix("cfg_attr") {
        let sets_path = rest.split(',').skip(1).any(|part| {
            part.trim()
                .strip_prefix("path")
                .is_some_and(|after| after.trim_start().starts_with('='))
        });
        return sets_path.then_some(Err("cfg_attr"));
    }
    let after = inner.strip_prefix("path")?;
    if !after.trim_start().starts_with('=') {
        return None;
    }
    Some(path_literal(attr).ok_or("non_literal"))
}

/// At most 160 bytes of an attribute, cut at a character boundary.
fn shorten(text: &str) -> String {
    if text.len() <= 160 {
        return text.to_string();
    }
    let mut cut = 160;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
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

/// Followed declarations that still do not wire their file in, as
/// `(declaring file, index into its followed declarations, reason)`: the
/// path leaves the repository or is absolute (`outside_repository`), names
/// the declaring file itself (`declaring_file`) or a file not in `files`
/// (`file_not_found`), names a file an earlier declaration in path order
/// wires in (`already_declared`), or sits on a declaration cycle (`cycle`),
/// which keeps the location's module.
pub(super) fn unresolved(
    declared: &BTreeMap<String, Vec<(String, String)>>,
    files: &BTreeSet<String>,
) -> Vec<(String, usize, &'static str)> {
    let mut out = Vec::new();
    let mut parent: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for (declarer, decls) in declared {
        for (index, (_, literal)) in decls.iter().enumerate() {
            match resolve(declarer, literal) {
                None => out.push((declarer.clone(), index, "outside_repository")),
                Some(target) if target == *declarer => {
                    out.push((declarer.clone(), index, "declaring_file"));
                }
                Some(target) if !files.contains(&target) => {
                    out.push((declarer.clone(), index, "file_not_found"));
                }
                Some(target) => match parent.entry(target) {
                    Entry::Occupied(_) => {
                        out.push((declarer.clone(), index, "already_declared"));
                    }
                    Entry::Vacant(slot) => {
                        slot.insert((declarer.clone(), index));
                    }
                },
            }
        }
    }
    for (target, (declarer, index)) in &parent {
        let mut chain = vec![target.clone()];
        let mut at = target.clone();
        while let Some((up, _)) = parent.get(&at) {
            if chain.contains(up) {
                out.push((declarer.clone(), *index, "cycle"));
                break;
            }
            chain.push(up.clone());
            at = up.clone();
        }
    }
    out.sort();
    out
}
