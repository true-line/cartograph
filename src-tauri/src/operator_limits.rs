//! Operator limits (#1653): a closed registry of operational limits a
//! project may change from their defaults. Each key has a default (used
//! until a value is saved), an inclusive range, and an `advanced` flag for
//! limits the UI should keep behind an advanced section. Free-form keys are
//! refused, a value outside its range is never saved, and a stored value
//! that no longer validates fails closed instead of silently reverting.
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// One registered limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitSpec {
    pub key: &'static str,
    pub default: u64,
    pub min: u64,
    pub max: u64,
    pub advanced: bool,
    pub description: &'static str,
}

/// Graph hops around a gap that T3 escalation searches for candidates.
pub const T3_CONTEXT_HOPS: &str = "t3_context_hops";

/// Nodes, edges and flows a diagram view sends for rendering (#488, #591).
pub const SPEC_VIEW_MAX_NODES: &str = "spec_view_max_nodes";
pub const SPEC_VIEW_MAX_EDGES: &str = "spec_view_max_edges";
pub const SPEC_VIEW_MAX_FLOWS: &str = "spec_view_max_flows";

/// Files and MiB one source capture may retain during recovery (AC-1510).
pub const SOURCE_CAPTURE_MAX_FILES: &str = "source_capture_max_files";
pub const SOURCE_CAPTURE_MAX_MIB: &str = "source_capture_max_mib";
const MIB: u64 = 1024 * 1024;

/// MiB and captures the retained-source capture store may hold (AC-1801).
pub const CAPTURE_STORE_MAX_MIB: &str = "capture_store_max_mib";
pub const CAPTURE_STORE_MAX_CAPTURES: &str = "capture_store_max_captures";

/// Directory entries one TS/JS selection walk may examine (AC-1802).
pub const TS_SELECTION_MAX_VISITED_ENTRIES: &str = "ts_selection_max_visited_entries";

/// Sandbox bounds for each plugin extraction call (AC-0070, AC-1803).
pub const PLUGIN_MAX_FUEL_BILLIONS: &str = "plugin_max_fuel_billions";
pub const PLUGIN_MAX_MEMORY_MIB: &str = "plugin_max_memory_mib";
pub const PLUGIN_DEADLINE_SECONDS: &str = "plugin_deadline_seconds";

/// API reference documents a repository may declare, and MiB each may hold
/// (AC-0313, AC-1804).
pub const API_DOCS_MAX_DOCS: &str = "api_docs_max_docs";
pub const API_DOCS_MAX_MIB: &str = "api_docs_max_mib";

/// Every limit a project may change. Adding a key here is the only way to
/// make a limit configurable; its consumer reads it with [`read`].
pub const REGISTRY: &[LimitSpec] = &[
    LimitSpec {
        key: T3_CONTEXT_HOPS,
        default: 2,
        min: 1,
        max: 4,
        advanced: false,
        description: "Graph hops around a gap that escalation searches for candidate facts.",
    },
    LimitSpec {
        key: SPEC_VIEW_MAX_NODES,
        default: spec::SPEC_VIEW_MAX_NODES as u64,
        min: 100,
        max: 20_000,
        advanced: true,
        description: "Nodes a diagram view shows before it is truncated. Large values slow rendering.",
    },
    LimitSpec {
        key: SPEC_VIEW_MAX_EDGES,
        default: spec::SPEC_VIEW_MAX_EDGES as u64,
        min: 100,
        max: 40_000,
        advanced: true,
        description: "Edges a diagram view shows before it is truncated. Large values slow rendering.",
    },
    LimitSpec {
        key: SPEC_VIEW_MAX_FLOWS,
        default: spec::SPEC_VIEW_MAX_FLOWS as u64,
        min: 10,
        max: 2_000,
        advanced: true,
        description: "Flows the flow view shows before it is truncated.",
    },
    LimitSpec {
        key: SOURCE_CAPTURE_MAX_FILES,
        default: source_capture::MAX_FILES as u64,
        min: 1_024,
        max: source_capture::HARD_MAX_FILES as u64,
        advanced: true,
        description: "Source files one recovery may retain for citations. Larger values use more disk and memory.",
    },
    LimitSpec {
        key: SOURCE_CAPTURE_MAX_MIB,
        default: source_capture::MAX_CAPTURE_BYTES / MIB,
        min: 16,
        max: source_capture::HARD_MAX_CAPTURE_BYTES / MIB,
        advanced: true,
        description: "MiB of source one recovery may retain for citations. Larger values use more disk and memory.",
    },
    LimitSpec {
        key: CAPTURE_STORE_MAX_MIB,
        default: source_capture::MAX_STORE_BYTES / MIB,
        min: 256,
        max: source_capture::MAX_STORE_BYTES / MIB,
        advanced: true,
        description: "MiB of retained source kept across all recoveries. Lowering it keeps what is stored but refuses new captures until space is freed.",
    },
    LimitSpec {
        key: CAPTURE_STORE_MAX_CAPTURES,
        default: source_capture::MAX_STORED_CAPTURES,
        min: 100,
        max: source_capture::MAX_STORED_CAPTURES,
        advanced: true,
        description: "Retained source captures kept across all recoveries. Lowering it keeps what is stored but refuses new captures until space is freed.",
    },
    LimitSpec {
        key: TS_SELECTION_MAX_VISITED_ENTRIES,
        default: adapters_lang_ts::captured::MAX_VISITED_ENTRIES as u64,
        min: 10_000,
        max: adapters_lang_ts::captured::HARD_MAX_VISITED_ENTRIES as u64,
        advanced: true,
        description: "Directory entries, including ignored ones, one recovery may examine when selecting TS/JS source. Larger values let big trees finish selection but take longer.",
    },
    LimitSpec {
        key: PLUGIN_MAX_FUEL_BILLIONS,
        default: 10,
        min: 1,
        max: 100,
        advanced: true,
        description: "Billions of fuel units (roughly wasm instructions) one plugin call may use per file. Raising it lets a slow or looping plugin hold the ingest longer; only raise it for plugins you trust.",
    },
    LimitSpec {
        key: PLUGIN_MAX_MEMORY_MIB,
        default: 64,
        min: 16,
        max: 1024,
        advanced: true,
        description: "MiB of memory one plugin call may grow to. Raising it lets an untrusted plugin take more of this machine's memory; only raise it for plugins you trust.",
    },
    LimitSpec {
        key: PLUGIN_DEADLINE_SECONDS,
        default: 5,
        min: 1,
        max: 60,
        advanced: true,
        description: "Seconds one plugin call may run on one file before the whole plugin pass fails. Raising it lets a stuck plugin hold the ingest longer; only raise it for plugins you trust.",
    },
    LimitSpec {
        key: API_DOCS_MAX_DOCS,
        default: ingest::api_docs::MAX_API_DOCS as u64,
        min: 1,
        max: ingest::api_docs::HARD_MAX_API_DOCS as u64,
        advanced: false,
        description: "API reference documents one repository may declare. More declared documents than this reads none of them and records a gap.",
    },
    LimitSpec {
        key: API_DOCS_MAX_MIB,
        default: ingest::api_docs::MAX_API_DOC_BYTES / MIB,
        min: 1,
        max: ingest::api_docs::HARD_MAX_API_DOC_BYTES / MIB,
        advanced: false,
        description: "MiB one declared API reference document may hold. A larger document is not read and is recorded as a gap.",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorLimitError {
    UnknownKey(String),
    OutOfRange { key: String, value: i64 },
    Storage(String),
}

impl std::fmt::Display for OperatorLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey(key) => write!(f, "'{key}' is not a configurable limit"),
            Self::OutOfRange { key, value } => {
                let spec = spec(key);
                match spec {
                    Some(spec) => write!(
                        f,
                        "{key} must be between {} and {}, not {value}",
                        spec.min, spec.max
                    ),
                    None => write!(f, "{key} is out of range: {value}"),
                }
            }
            Self::Storage(error) => write!(f, "{error}"),
        }
    }
}

impl From<rusqlite::Error> for OperatorLimitError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

/// A limit as Settings shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperatorLimit {
    pub key: &'static str,
    /// The value in effect: the saved one, or the default.
    pub value: u64,
    /// Whether a value is saved (otherwise the default applies).
    pub saved: bool,
    pub default: u64,
    pub min: u64,
    pub max: u64,
    pub advanced: bool,
    pub description: &'static str,
}

fn spec(key: &str) -> Option<&'static LimitSpec> {
    REGISTRY.iter().find(|spec| spec.key == key)
}

/// Create the project table. Called with the other settings tables.
pub fn initialize(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings_operator_limits (
             key   TEXT PRIMARY KEY NOT NULL,
             value INTEGER NOT NULL CHECK (value >= 0)
         ) STRICT;",
    )
}

fn stored(conn: &Connection, spec: &LimitSpec) -> Result<Option<u64>, OperatorLimitError> {
    let value: Option<i64> = conn
        .query_row(
            "SELECT value FROM settings_operator_limits WHERE key = ?1",
            params![spec.key],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|value| {
            u64::try_from(value)
                .ok()
                .filter(|value| (spec.min..=spec.max).contains(value))
                .ok_or_else(|| OperatorLimitError::OutOfRange {
                    key: spec.key.into(),
                    value,
                })
        })
        .transpose()
}

/// The value in effect for `key`. A stored value outside the key's range
/// (a tampered or future-version row) is an error, never the default.
pub fn read(conn: &Connection, key: &str) -> Result<u64, OperatorLimitError> {
    let spec = spec(key).ok_or_else(|| OperatorLimitError::UnknownKey(key.into()))?;
    Ok(stored(conn, spec)?.unwrap_or(spec.default))
}

/// Save `value` for `key`, or clear it with `None` so the default applies.
/// Nothing is written unless the key is registered and the value in range.
pub fn write(
    conn: &Connection,
    key: &str,
    value: Option<u64>,
) -> Result<OperatorLimit, OperatorLimitError> {
    let spec = spec(key).ok_or_else(|| OperatorLimitError::UnknownKey(key.into()))?;
    match value {
        None => {
            conn.execute(
                "DELETE FROM settings_operator_limits WHERE key = ?1",
                params![spec.key],
            )?;
        }
        Some(value) => {
            if !(spec.min..=spec.max).contains(&value) {
                return Err(OperatorLimitError::OutOfRange {
                    key: spec.key.into(),
                    value: i64::try_from(value).unwrap_or(i64::MAX),
                });
            }
            conn.execute(
                "INSERT INTO settings_operator_limits (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![spec.key, value as i64],
            )?;
        }
    }
    describe(conn, spec)
}

fn describe(
    conn: &Connection,
    spec: &'static LimitSpec,
) -> Result<OperatorLimit, OperatorLimitError> {
    let saved = stored(conn, spec)?;
    Ok(OperatorLimit {
        key: spec.key,
        value: saved.unwrap_or(spec.default),
        saved: saved.is_some(),
        default: spec.default,
        min: spec.min,
        max: spec.max,
        advanced: spec.advanced,
        description: spec.description,
    })
}

/// Every registered limit in registry order.
pub fn list(conn: &Connection) -> Result<Vec<OperatorLimit>, OperatorLimitError> {
    REGISTRY.iter().map(|spec| describe(conn, spec)).collect()
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        conn
    }

    #[test]
    fn registry_keys_are_unique_and_defaults_in_range() {
        for (i, spec) in REGISTRY.iter().enumerate() {
            assert!(
                spec.min <= spec.default && spec.default <= spec.max,
                "{}",
                spec.key
            );
            assert!(REGISTRY[..i].iter().all(|other| other.key != spec.key));
        }
    }

    // AC-1800 / #1653: the source capture keys default to the capture
    // crate's historical bounds and stop at its hard ceilings, so every
    // saved value converts to limits the crate accepts.
    #[test]
    fn capture_limit_keys_stop_at_the_crate_hard_ceilings() {
        let conn = conn();
        let files = spec(SOURCE_CAPTURE_MAX_FILES).unwrap();
        let mib = spec(SOURCE_CAPTURE_MAX_MIB).unwrap();
        assert_eq!(files.default, source_capture::MAX_FILES as u64);
        assert_eq!(mib.default * MIB, source_capture::MAX_CAPTURE_BYTES);
        assert_eq!(files.max, source_capture::HARD_MAX_FILES as u64);
        assert_eq!(mib.max * MIB, source_capture::HARD_MAX_CAPTURE_BYTES);
        assert!(files.advanced && mib.advanced);
        write(&conn, SOURCE_CAPTURE_MAX_FILES, Some(files.max)).unwrap();
        write(&conn, SOURCE_CAPTURE_MAX_MIB, Some(mib.max)).unwrap();
        let limits = source_capture::CaptureLimits {
            max_files: read(&conn, SOURCE_CAPTURE_MAX_FILES).unwrap() as usize,
            max_total_bytes: read(&conn, SOURCE_CAPTURE_MAX_MIB).unwrap() * MIB,
            ..source_capture::CaptureLimits::default()
        };
        assert!(limits.validate().is_ok());
        assert!(matches!(
            write(&conn, SOURCE_CAPTURE_MAX_FILES, Some(files.max + 1)),
            Err(OperatorLimitError::OutOfRange { .. })
        ));
    }

    // AC-1801 / #1653: the capture store quota keys default to, and stop
    // at, the store's hard maxima, so the quota can only be lowered.
    #[test]
    fn capture_store_keys_lower_only_within_the_store_maxima() {
        let mib = spec(CAPTURE_STORE_MAX_MIB).unwrap();
        let captures = spec(CAPTURE_STORE_MAX_CAPTURES).unwrap();
        assert_eq!(mib.default * MIB, source_capture::MAX_STORE_BYTES);
        assert_eq!(mib.max, mib.default);
        assert_eq!(captures.default, source_capture::MAX_STORED_CAPTURES);
        assert_eq!(captures.max, captures.default);
        assert!(mib.advanced && captures.advanced);
        let conn = conn();
        assert!(matches!(
            write(&conn, CAPTURE_STORE_MAX_MIB, Some(mib.max + 1)),
            Err(OperatorLimitError::OutOfRange { .. })
        ));
        write(&conn, CAPTURE_STORE_MAX_CAPTURES, Some(captures.min)).unwrap();
        assert_eq!(read(&conn, CAPTURE_STORE_MAX_CAPTURES).unwrap(), 100);
    }

    // AC-1802 / #1653: the TS/JS selection walk key defaults to the
    // adapter's historical bound and stops at its hard ceiling.
    #[test]
    fn selection_walk_key_stops_at_the_adapter_hard_ceiling() {
        let entries = spec(TS_SELECTION_MAX_VISITED_ENTRIES).unwrap();
        assert_eq!(
            entries.default,
            adapters_lang_ts::captured::MAX_VISITED_ENTRIES as u64
        );
        assert_eq!(
            entries.max,
            adapters_lang_ts::captured::HARD_MAX_VISITED_ENTRIES as u64
        );
        assert!(entries.advanced);
        let conn = conn();
        write(&conn, TS_SELECTION_MAX_VISITED_ENTRIES, Some(entries.max)).unwrap();
        assert!(matches!(
            write(
                &conn,
                TS_SELECTION_MAX_VISITED_ENTRIES,
                Some(entries.max + 1)
            ),
            Err(OperatorLimitError::OutOfRange { .. })
        ));
    }

    // AC-1803 / #1653: the plugin sandbox keys default to the host's
    // AC-0070 bounds, so an untouched project runs plugins exactly as before.
    #[test]
    fn plugin_sandbox_keys_default_to_the_host_bounds() {
        let defaults = adapters_plugin_host::PluginLimits::default();
        let fuel = spec(PLUGIN_MAX_FUEL_BILLIONS).unwrap();
        let memory = spec(PLUGIN_MAX_MEMORY_MIB).unwrap();
        let deadline = spec(PLUGIN_DEADLINE_SECONDS).unwrap();
        assert_eq!(fuel.default * 1_000_000_000, defaults.max_fuel);
        assert_eq!(memory.default * MIB, defaults.max_memory_bytes as u64);
        assert_eq!(deadline.default, defaults.deadline.as_secs());
        for key in [fuel, memory, deadline] {
            assert!(key.advanced);
            assert!(
                key.description
                    .contains("only raise it for plugins you trust")
            );
        }
        let conn = conn();
        assert!(matches!(
            write(&conn, PLUGIN_MAX_MEMORY_MIB, Some(memory.max + 1)),
            Err(OperatorLimitError::OutOfRange { .. })
        ));
    }

    // AC-1804 / #1653: the API docs keys default to the loader's bounds and
    // stop at its hard ceilings, so every saved pair is one it accepts.
    #[test]
    fn api_docs_keys_default_to_the_loader_bounds_and_stop_at_its_ceilings() {
        use ingest::api_docs::ApiDocLimits;
        let docs = spec(API_DOCS_MAX_DOCS).unwrap();
        let mib = spec(API_DOCS_MAX_MIB).unwrap();
        assert_eq!(
            ApiDocLimits::within(docs.default as usize, mib.default * MIB),
            Some(ApiDocLimits::default())
        );
        assert!(ApiDocLimits::within(docs.max as usize, mib.max * MIB).is_some());
        assert!(ApiDocLimits::within(docs.min as usize, mib.min * MIB).is_some());
        assert!(ApiDocLimits::within(docs.max as usize + 1, mib.max * MIB).is_none());
        assert!(ApiDocLimits::within(docs.max as usize, (mib.max + 1) * MIB).is_none());
    }

    // AC-1418 / #1653: defaults apply until a value is saved; a saved value
    // persists, is clamped to its range on write, clears back to the
    // default, and a tampered stored value fails closed.
    #[test]
    fn operator_limits_default_validate_persist_and_fail_closed() {
        let conn = conn();
        assert_eq!(read(&conn, T3_CONTEXT_HOPS).unwrap(), 2);
        assert!(!list(&conn).unwrap()[0].saved);
        assert_eq!(
            read(&conn, "max_anything"),
            Err(OperatorLimitError::UnknownKey("max_anything".into()))
        );
        assert!(matches!(
            write(&conn, "max_anything", Some(1)),
            Err(OperatorLimitError::UnknownKey(_))
        ));
        let saved = write(&conn, T3_CONTEXT_HOPS, Some(3)).unwrap();
        assert_eq!((saved.value, saved.saved), (3, true));
        assert_eq!(read(&conn, T3_CONTEXT_HOPS).unwrap(), 3);
        for bad in [0, 5] {
            assert!(matches!(
                write(&conn, T3_CONTEXT_HOPS, Some(bad)),
                Err(OperatorLimitError::OutOfRange { .. })
            ));
        }
        assert_eq!(read(&conn, T3_CONTEXT_HOPS).unwrap(), 3);
        conn.execute(
            "UPDATE settings_operator_limits SET value = 99 WHERE key = ?1",
            params![T3_CONTEXT_HOPS],
        )
        .unwrap();
        assert!(matches!(
            read(&conn, T3_CONTEXT_HOPS),
            Err(OperatorLimitError::OutOfRange { value: 99, .. })
        ));
        let cleared = write(&conn, T3_CONTEXT_HOPS, None).unwrap();
        assert_eq!((cleared.value, cleared.saved), (2, false));
    }
}
