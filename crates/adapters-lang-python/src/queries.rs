//! Process-wide cache of this adapter's static tree-sitter queries (#897).
//!
//! Compiling a query runs its pattern analysis (`ts_query_new`), so
//! building every query per file repeats that work for each file. Each
//! `'static` pattern is compiled once and the immutable [`Query`] is
//! shared across files and threads (`Query` is `Send + Sync`). The cache
//! holds one entry per distinct pattern in the source, never per input.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tree_sitter::Query;

/// The compiled form of a static `pattern`, compiled on first use. Panics
/// like the `expect("static query")` it replaces when the pattern is
/// invalid — a source bug every test run hits.
pub(crate) fn query(pattern: &'static str) -> &'static Query {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, &'static Query>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.entry(pattern).or_insert_with(|| {
        Box::leak(Box::new(
            Query::new(&tree_sitter_python::LANGUAGE.into(), pattern).expect("static query"),
        ))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn query_compiles_once_per_pattern() {
        const PATTERN: &str = "(identifier) @id";
        let first = super::query(PATTERN);
        assert!(std::ptr::eq(first, super::query(PATTERN)));
        assert_eq!(first.capture_names(), ["id"]);
    }
}
