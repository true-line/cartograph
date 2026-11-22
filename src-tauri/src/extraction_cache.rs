//! The per-repository incremental extraction caches the app state holds,
//! split out of `main.rs` so that state compiles in app-store (#1924).

#[derive(Default)]
pub struct RepoExtractionCache {
    pub ts: adapters_lang_ts::IncrementalCache,
    /// Attested parses of the captured TS lane (ADR-0087).
    pub ts_captured: adapters_lang_ts::captured::CapturedCache,
    pub python: adapters_lang_python::IncrementalCache,
    pub go: adapters_lang_go::IncrementalCache,
    pub rust: adapters_lang_rust::IncrementalCache,
    pub java: adapters_lang_java::IncrementalCache,
    pub kotlin: adapters_lang_kotlin::IncrementalCache,
    pub tf: iac::IncrementalCache,
}

#[derive(Default)]
pub struct ExtractionCaches {
    pub repos: std::collections::BTreeMap<String, RepoExtractionCache>,
}
