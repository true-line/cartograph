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
pub(crate) fn initialize(conn: &Connection) -> rusqlite::Result<()> {
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

#[cfg(test)]
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
