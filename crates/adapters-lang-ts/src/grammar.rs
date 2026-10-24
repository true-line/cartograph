//! The two TypeScript grammars this crate parses with, and a process-wide
//! cache of the static queries compiled against each.
//!
//! Compiling a tree-sitter query runs its pattern analysis
//! (`ts_query_new`); doing that for every query on every file dominated a
//! large TypeScript ingest's profile. Every query this crate runs is a
//! `'static` pattern string, so each one is compiled once per grammar and
//! the compiled [`Query`] is shared read-only across files and threads
//! (`Query` is `Send + Sync`). The cache holds at most one entry per
//! distinct pattern per grammar, so its size is fixed by the source code,
//! not by the input.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tree_sitter::{Language, Query};

type QueryCache = Mutex<HashMap<&'static str, &'static Query>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grammar {
    TypeScript,
    Tsx,
}

impl Grammar {
    pub(crate) fn new(is_tsx: bool) -> Self {
        if is_tsx { Self::Tsx } else { Self::TypeScript }
    }

    pub(crate) fn language(self) -> Language {
        match self {
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    /// The compiled form of a static `pattern` for this grammar, compiled
    /// on first use. Panics like the `expect("static query")` it replaces
    /// when the pattern is invalid — a source bug every test run hits.
    pub(crate) fn query(self, pattern: &'static str) -> &'static Query {
        static TYPESCRIPT: OnceLock<QueryCache> = OnceLock::new();
        static TSX: OnceLock<QueryCache> = OnceLock::new();
        let cache = match self {
            Self::TypeScript => &TYPESCRIPT,
            Self::Tsx => &TSX,
        }
        .get_or_init(Default::default);
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.entry(pattern).or_insert_with(|| {
            Box::leak(Box::new(
                Query::new(&self.language(), pattern).expect("static query"),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Grammar;

    #[test]
    fn query_compiles_once_per_grammar_and_pattern() {
        const PATTERN: &str = "(identifier) @id";
        let ts = Grammar::TypeScript.query(PATTERN);
        assert!(std::ptr::eq(ts, Grammar::TypeScript.query(PATTERN)));
        let tsx = Grammar::Tsx.query(PATTERN);
        assert!(std::ptr::eq(tsx, Grammar::Tsx.query(PATTERN)));
        assert!(!std::ptr::eq(ts, tsx));
        assert_eq!(ts.capture_names(), ["id"]);
    }
}
