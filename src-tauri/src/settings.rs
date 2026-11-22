//! Persistent tier/provider settings and standing, revocable cloud consent
//! on the state spine (#118).
//!
//! Fail-closed layering (ADR-0004): a **standing tier consent** recorded
//! here only *permits* cloud for that tier — every actual call still passes
//! the egress firewall's per-payload grant. Revocation is immediate; the
//! default is local-only; and T0 is not a row in this store at all — the
//! deterministic tier is always on and never invokes an LLM, so there is
//! nothing to configure or disable.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::path::Path;

/// Configurable recovery tiers (T0 is intentionally absent: always-on,
/// never an LLM).
pub const TIERS: &[&str] = &["T1", "T2", "T3"];
/// Tiers that use a model provider and may opt into cloud.
const LLM_TIERS: &[&str] = &["T2", "T3"];

/// One tier's persisted configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TierSettings {
    /// `T1` | `T2` | `T3`.
    pub tier: String,
    /// Whether the tier participates in recovery.
    pub enabled: bool,
    /// `local` | `cloud` — meaningful for T2/T3 only (`local` for T1).
    pub provider: String,
    /// Standing cloud consent (only possible with `provider == cloud`).
    pub consented: bool,
    /// The disclosure shown when consent was granted (audit trail), JSON.
    pub consent_disclosure: Option<String>,
    /// When consent was granted (UTC, ISO-8601).
    pub consented_at: Option<String>,
}

/// Live egress summary for the status bar (handoff §App Shell).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EgressSummary {
    /// Tiers currently consented to cloud (empty ⇒ local-only).
    pub cloud_tiers: Vec<String>,
    /// Total observed egress bytes across all cloud calls.
    pub bytes_sent: u64,
    /// Rendered line, e.g. `Local-only · 0 bytes egress`.
    pub label: String,
}

/// The "Ingest parallelism" setting (#236) with what it resolves to here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestParallelism {
    /// `0` = Auto, otherwise a fixed worker count (`1` = serial).
    pub setting: u32,
    /// Workers Auto resolves to on this machine (P-cores − 1, memory-capped).
    pub auto_workers: u32,
    /// Workers the current setting actually runs.
    pub workers: u32,
    /// Largest fixed choice offered: [`max_workers`] (available parallelism, at most 64).
    pub max_workers: u32,
}

impl IngestParallelism {
    /// The worker model for a stored `setting`.
    pub fn parallelism(setting: u32) -> source_walk::parallel::Parallelism {
        match setting {
            0 => source_walk::parallel::Parallelism::Auto,
            workers => source_walk::parallel::Parallelism::Fixed(workers as usize),
        }
    }

    /// Describe a stored `setting` against this machine. A fixed count above
    /// today's [`max_workers`] (the state DB was written on a larger machine,
    /// or a CPU quota shrank) is normalized down to it, so startup never
    /// oversubscribes and Settings shows a choice it actually offers (#478).
    fn describe(setting: u32) -> Self {
        let clamp = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let setting = normalize_setting(setting, clamp(max_workers()));
        Self {
            setting,
            auto_workers: clamp(source_walk::parallel::auto_workers()),
            workers: clamp(Self::parallelism(setting).workers()),
            max_workers: clamp(max_workers()),
        }
    }
}

/// A stored setting as it applies on a machine offering `max` fixed workers:
/// Auto (`0`) is kept, a fixed count is capped at `max`.
fn normalize_setting(setting: u32, max: u32) -> u32 {
    if setting == 0 {
        0
    } else {
        setting.min(max.max(1))
    }
}

/// Largest fixed worker count offered in Settings: this machine's available
/// parallelism, capped at [`source_walk::parallel::MAX_WORKERS`] (64).
pub fn max_workers() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(source_walk::parallel::MAX_WORKERS)
}

/// A settings transition that violates an invariant, or a storage failure.
#[derive(Debug)]
pub enum SettingsError {
    UnknownTier(String),
    NoProvider(String),
    NotCloudProvider,
    InvalidProvider(String),
    InvalidParallelism(u32),
    InvalidInvestigationChoice,
    InvalidLocalModel,
    InvalidContextWindow(u32),
    InvalidLocalPort(i64),
    InvalidReadDenyRules(llm::read_deny::ReadDenyRuleError),
    InvalidRunLimit {
        field: &'static str,
        value: u64,
        min: u64,
        max: u64,
    },
    Store(rusqlite::Error),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTier(tier) => write!(
                f,
                "unknown tier '{tier}' — configurable tiers are T1, T2, T3 (T0 is always on)"
            ),
            Self::NoProvider(tier) => write!(f, "tier {tier} does not use a model provider"),
            Self::NotCloudProvider => write!(
                f,
                "cloud consent requires the tier's provider to be 'cloud' (fail closed)"
            ),
            Self::InvalidProvider(provider) => {
                write!(f, "provider must be 'local' or 'cloud', got '{provider}'")
            }
            Self::InvalidParallelism(workers) => write!(
                f,
                "ingest parallelism must be Auto (0) or 1..={} workers, got {workers}",
                max_workers()
            ),
            Self::InvalidInvestigationChoice => write!(
                f,
                "investigation choice must name a specialist and a budget profile (1..=128 of a-z, 0-9, @ . _ -)"
            ),
            Self::InvalidLocalModel => write!(
                f,
                "local model must be an Ollama model name (1..=128 of A-Z, a-z, 0-9, : . _ / + -)"
            ),
            Self::InvalidContextWindow(tokens) => write!(
                f,
                "context window must be one of {:?} tokens, got {tokens}",
                llm::OllamaProvider::CONTEXT_WINDOWS
            ),
            Self::InvalidLocalPort(port) => {
                write!(f, "Ollama port must be 1..=65535, got {port}")
            }
            Self::InvalidReadDenyRules(error) => write!(f, "{error}"),
            Self::InvalidRunLimit {
                field,
                value,
                min,
                max,
            } => write!(f, "{field} must be {min}..={max}, got {value}"),
            Self::Store(error) => write!(f, "settings store: {error}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl From<rusqlite::Error> for SettingsError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error)
    }
}

/// The user's last explicit Investigations budget and specialist (#1636).
/// The host stores identifiers only; the UI applies them only when the
/// current catalog still offers that pair, otherwise the first-run default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvestigationChoice {
    pub specialist_id: String,
    pub limit_profile: String,
}

fn choice_identifier(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"@._-".contains(&b))
}

/// Largest task active-time budget the Configure window accepts (owner
/// decision on #1633: 60 minutes).
pub const MAX_ACTIVE_SECONDS: u64 = 3_600;
/// Largest per-model-call timeout the Configure window accepts (owner
/// decision on #1633: 10 minutes).
pub const MAX_REQUEST_TIMEOUT_MS: u64 = 600_000;

/// Shipped task active-time budget: the registry's investigation default.
pub fn default_active_seconds() -> u64 {
    agents::investigation::INVESTIGATION_V2_POLICY
        .limits()
        .active_seconds
}

/// Shipped per-model-call timeout: the bounded-completion default.
pub fn default_request_timeout_ms() -> u64 {
    llm::bounded::CompletionLimitValues::default().request_timeout_ms
}

/// The time limits an investigation runs under (#1633). Only these two are
/// tunable; every other cap stays fixed in the policy registry. A stored
/// `NULL` means "use the shipped default", so a later default change still
/// reaches profiles that never touched the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RunLimits {
    /// Effective active-time budget per task, in seconds.
    pub active_seconds: u64,
    /// Effective timeout for one model call, in milliseconds.
    pub request_timeout_ms: u64,
    pub default_active_seconds: u64,
    pub default_request_timeout_ms: u64,
    pub max_active_seconds: u64,
    pub max_request_timeout_ms: u64,
    /// Whether either value differs from the shipped default.
    pub customized: bool,
}

impl RunLimits {
    fn describe(active: Option<u64>, timeout: Option<u64>) -> Self {
        let default_active_seconds = default_active_seconds();
        let default_request_timeout_ms = default_request_timeout_ms();
        let active_seconds = active.unwrap_or(default_active_seconds);
        let request_timeout_ms = timeout.unwrap_or(default_request_timeout_ms);
        Self {
            active_seconds,
            request_timeout_ms,
            default_active_seconds,
            default_request_timeout_ms,
            max_active_seconds: MAX_ACTIVE_SECONDS,
            max_request_timeout_ms: MAX_REQUEST_TIMEOUT_MS,
            customized: active_seconds != default_active_seconds
                || request_timeout_ms != default_request_timeout_ms,
        }
    }
}

fn check_run_limit(
    field: &'static str,
    value: u64,
    min: u64,
    max: u64,
) -> Result<(), SettingsError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(SettingsError::InvalidRunLimit {
            field,
            value,
            min,
            max,
        })
    }
}

/// The local completion model desktop investigations use (#1643). A stored
/// `NULL` means the shipped default, so a later default change still reaches
/// profiles that never chose a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalModelChoice {
    /// The Ollama model new desktop investigations request.
    pub model: String,
    /// The shipped default model.
    pub default_model: String,
    /// Whether the model differs from the shipped default.
    pub customized: bool,
}

impl LocalModelChoice {
    fn describe(stored: Option<String>) -> Self {
        let default_model = llm::OllamaProvider::DEFAULT_COMPLETION_MODEL.to_owned();
        let model = stored.unwrap_or_else(|| default_model.clone());
        Self {
            customized: model != default_model,
            model,
            default_model,
        }
    }
}

/// An Ollama model name: `name[:tag]`, optionally namespaced. Syntax only;
/// whether it is installed is checked against Ollama when it is saved.
pub fn local_model_name(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":._/+-".contains(&b))
}

/// The saved loopback Ollama port, or the default when none is saved (#1653).
/// Every local model path (investigations, composition, area experts,
/// escalation, the knowledge phase and the lane probe) reaches Ollama here.
pub fn saved_ollama_port(settings: &std::sync::Mutex<SettingsStore>) -> Result<u16, String> {
    Ok(settings
        .lock()
        .map_err(|e| e.to_string())?
        .local_ollama_port()
        .map_err(|e| e.to_string())?
        .unwrap_or(llm::OllamaProvider::DEFAULT_PORT))
}

/// Store for tier settings and the egress log, on the state spine.
pub struct SettingsStore {
    conn: Connection,
}

impl SettingsStore {
    /// Open (creating if absent) the settings tables at `path`. Missing
    /// tier rows are seeded with the local-only defaults.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tier_settings (
                 tier               TEXT PRIMARY KEY CHECK (tier IN ('T1', 'T2', 'T3')),
                 enabled            INTEGER NOT NULL CHECK (enabled IN (0, 1)),
                 provider           TEXT NOT NULL CHECK (provider IN ('local', 'cloud')),
                 consented          INTEGER NOT NULL CHECK (consented IN (0, 1)),
                 consent_disclosure TEXT,
                 consented_at       TEXT
             ) STRICT;
             CREATE TABLE IF NOT EXISTS plugin_settings (
                 project_root TEXT NOT NULL,
                 plugin_id    TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 enabled      INTEGER NOT NULL CHECK (enabled IN (0, 1)),
                 updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                 PRIMARY KEY (project_root, plugin_id)
             ) STRICT;
             CREATE TABLE IF NOT EXISTS plugin_gates (
                 plugin_id    TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 passed       INTEGER NOT NULL CHECK (passed IN (0, 1)),
                 report_json  TEXT NOT NULL,
                 updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                 PRIMARY KEY (plugin_id, content_hash)
             ) STRICT;
             CREATE TABLE IF NOT EXISTS ingest_settings (
                 id          INTEGER PRIMARY KEY CHECK (id = 1),
                 parallelism INTEGER NOT NULL CHECK (parallelism >= 0)
             ) STRICT;
             INSERT OR IGNORE INTO ingest_settings (id, parallelism) VALUES (1, 0);
             -- Outside `investigation_*`, which the investigations store owns
             -- and validates as a closed set.
             CREATE TABLE IF NOT EXISTS settings_investigation_choice (
                 id            INTEGER PRIMARY KEY CHECK (id = 1),
                 specialist_id TEXT,
                 limit_profile TEXT
             ) STRICT;
             INSERT OR IGNORE INTO settings_investigation_choice (id) VALUES (1);
             CREATE TABLE IF NOT EXISTS run_limit_settings (
                 id                 INTEGER PRIMARY KEY CHECK (id = 1),
                 active_seconds     INTEGER CHECK (active_seconds > 0),
                 request_timeout_ms INTEGER CHECK (request_timeout_ms > 0)
             ) STRICT;
             INSERT OR IGNORE INTO run_limit_settings (id) VALUES (1);
             CREATE TABLE IF NOT EXISTS settings_local_model (
                 id               INTEGER PRIMARY KEY CHECK (id = 1),
                 completion_model TEXT CHECK (length(completion_model) BETWEEN 1 AND 128)
             ) STRICT;
             INSERT OR IGNORE INTO settings_local_model (id) VALUES (1);
             -- NULL leaves the context window to Ollama's own default (#1653).
             CREATE TABLE IF NOT EXISTS settings_local_context (
                 id             INTEGER PRIMARY KEY CHECK (id = 1),
                 context_tokens INTEGER CHECK (context_tokens > 0)
             ) STRICT;
             INSERT OR IGNORE INTO settings_local_context (id) VALUES (1);
             -- NULL is Ollama's default port 11434; the host is always loopback (#1653).
             CREATE TABLE IF NOT EXISTS settings_local_endpoint (
                 id   INTEGER PRIMARY KEY CHECK (id = 1),
                 port INTEGER CHECK (port BETWEEN 1 AND 65535)
             ) STRICT;
             INSERT OR IGNORE INTO settings_local_endpoint (id) VALUES (1);
             -- The project's own egress-read-deny/v1 rules, added to the
             -- defaults, in order (ADR-0083, #679). A project table.
             CREATE TABLE IF NOT EXISTS settings_read_deny_rules (
                 position INTEGER PRIMARY KEY CHECK (position >= 0),
                 pattern  TEXT NOT NULL UNIQUE
             ) STRICT;
             CREATE TABLE IF NOT EXISTS egress_log (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 provider   TEXT NOT NULL,
                 bytes      INTEGER NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ) STRICT;",
        )?;
        crate::agent_pool_store::initialize(&conn)?;
        crate::operator_limits::initialize(&conn)?;
        for tier in TIERS {
            conn.execute(
                "INSERT OR IGNORE INTO tier_settings (tier, enabled, provider, consented)
                 VALUES (?1, 1, 'local', 0)",
                params![tier],
            )?;
        }
        Ok(Self { conn })
    }

    /// All tier settings, T1 → T3.
    pub fn all(&self) -> rusqlite::Result<Vec<TierSettings>> {
        let mut stmt = self.conn.prepare(
            "SELECT tier, enabled, provider, consented, consent_disclosure, consented_at
             FROM tier_settings ORDER BY tier",
        )?;
        stmt.query_map([], |row| {
            Ok(TierSettings {
                tier: row.get(0)?,
                enabled: row.get(1)?,
                provider: row.get(2)?,
                consented: row.get(3)?,
                consent_disclosure: row.get(4)?,
                consented_at: row.get(5)?,
            })
        })?
        .collect()
    }

    fn require_tier(&self, tier: &str) -> Result<(), SettingsError> {
        if TIERS.contains(&tier) {
            Ok(())
        } else {
            Err(SettingsError::UnknownTier(tier.to_string()))
        }
    }

    /// Enable/disable a configurable tier. T0 is rejected by the tier
    /// vocabulary itself — it is always on.
    pub fn set_enabled(&mut self, tier: &str, enabled: bool) -> Result<(), SettingsError> {
        self.require_tier(tier)?;
        self.conn.execute(
            "UPDATE tier_settings SET enabled = ?2 WHERE tier = ?1",
            params![tier, enabled],
        )?;
        Ok(())
    }

    /// Enable/disable one discovered plugin for one project (#198).
    /// Discovery never activates anything by itself: a plugin is off until
    /// this records an explicit opt-in, per project — and the opt-in binds
    /// to the exact artifact hash, so replaced bytes are disabled again
    /// (#203 review).
    pub fn set_plugin_enabled(
        &mut self,
        project_root: &str,
        plugin_id: &str,
        content_hash: &str,
        enabled: bool,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO plugin_settings (project_root, plugin_id, content_hash, enabled)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (project_root, plugin_id)
             DO UPDATE SET content_hash = ?3,
                           enabled = ?4,
                           updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
            params![project_root, plugin_id, content_hash, enabled as i64],
        )?;
        Ok(())
    }

    /// `(plugin_id, content_hash)` pairs explicitly enabled for
    /// `project_root`. Absent rows — and rows recorded for different
    /// artifact bytes — are disabled: the fail-closed default.
    pub fn enabled_plugins(&self, project_root: &str) -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT plugin_id, content_hash FROM plugin_settings
             WHERE project_root = ?1 AND enabled = 1 ORDER BY plugin_id",
        )?;
        let rows = stmt.query_map(params![project_root], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// Scalar selection for bounded readers; never materialize the enabled inventory.
    pub fn plugin_enabled_exact(
        &self,
        project_root: &str,
        plugin_id: &str,
        content_hash: &str,
    ) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM plugin_settings
             WHERE project_root=?1 AND plugin_id=?2 AND content_hash=?3 AND enabled=1)",
            params![project_root, plugin_id, content_hash],
            |row| row.get(0),
        )
    }

    /// Read only the outcome; an arbitrarily large stored report is not copied.
    pub fn plugin_gate_passed_exact(
        &self,
        plugin_id: &str,
        content_hash: &str,
    ) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM plugin_gates
             WHERE plugin_id=?1 AND content_hash=?2 AND passed=1)",
            params![plugin_id, content_hash],
            |row| row.get(0),
        )
    }

    /// Persist one conformance-gate outcome (#200), keyed by the exact
    /// artifact hash the gate actually ran: replaced bytes are ungated
    /// again, no matter what an earlier copy proved.
    pub fn record_plugin_gate(
        &mut self,
        plugin_id: &str,
        content_hash: &str,
        passed: bool,
        report_json: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO plugin_gates (plugin_id, content_hash, passed, report_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (plugin_id, content_hash)
             DO UPDATE SET passed = ?3,
                           report_json = ?4,
                           updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')",
            params![plugin_id, content_hash, passed as i64, report_json],
        )?;
        Ok(())
    }

    /// The recorded gate outcome for one exact artifact:
    /// `Some((passed, report_json))`, or `None` while ungated — the
    /// proposed state every artifact starts in.
    pub fn plugin_gate(
        &self,
        plugin_id: &str,
        content_hash: &str,
    ) -> rusqlite::Result<Option<(bool, String)>> {
        self.conn
            .query_row(
                "SELECT passed, report_json FROM plugin_gates
                 WHERE plugin_id = ?1 AND content_hash = ?2",
                params![plugin_id, content_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// Choose `local` or `cloud` for an LLM tier (T2/T3). Switching away
    /// from cloud always revokes any standing consent (fail closed).
    pub fn set_provider(&mut self, tier: &str, provider: &str) -> Result<(), SettingsError> {
        self.require_tier(tier)?;
        if !LLM_TIERS.contains(&tier) {
            return Err(SettingsError::NoProvider(tier.to_string()));
        }
        if provider != "local" && provider != "cloud" {
            return Err(SettingsError::InvalidProvider(provider.to_string()));
        }
        self.conn.execute(
            "UPDATE tier_settings SET provider = ?2,
                 consented = CASE WHEN ?2 = 'cloud' THEN consented ELSE 0 END,
                 consent_disclosure = CASE WHEN ?2 = 'cloud' THEN consent_disclosure ELSE NULL END,
                 consented_at = CASE WHEN ?2 = 'cloud' THEN consented_at ELSE NULL END
             WHERE tier = ?1",
            params![tier, provider],
        )?;
        Ok(())
    }

    /// Record standing cloud consent for a tier, keeping the disclosure the
    /// user saw as the audit trail. Only possible when the tier's provider
    /// is already `cloud`.
    pub fn grant_consent(&mut self, tier: &str, disclosure: &str) -> Result<(), SettingsError> {
        self.require_tier(tier)?;
        let provider: String = self.conn.query_row(
            "SELECT provider FROM tier_settings WHERE tier = ?1",
            params![tier],
            |row| row.get(0),
        )?;
        if provider != "cloud" {
            return Err(SettingsError::NotCloudProvider);
        }
        self.conn.execute(
            "UPDATE tier_settings SET consented = 1, consent_disclosure = ?2,
                 consented_at = strftime('%Y-%m-%dT%H:%M:%SZ','now')
             WHERE tier = ?1",
            params![tier, disclosure],
        )?;
        Ok(())
    }

    /// Revoke a tier's standing consent — effective immediately.
    pub fn revoke_consent(&mut self, tier: &str) -> Result<(), SettingsError> {
        self.require_tier(tier)?;
        self.conn.execute(
            "UPDATE tier_settings SET consented = 0, consent_disclosure = NULL,
                 consented_at = NULL
             WHERE tier = ?1",
            params![tier],
        )?;
        Ok(())
    }

    /// Record observed cloud egress (called where a consented cloud call
    /// actually completes; the escalation orchestration wires this — #120).
    pub fn record_egress(&mut self, provider: &str, bytes: u64) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO egress_log (provider, bytes) VALUES (?1, ?2)",
            params![provider, bytes as i64],
        )?;
        Ok(())
    }

    /// The status bar's live egress line (handoff: `Local-only · 0 bytes
    /// egress` unless a tier is consented to cloud).
    pub fn egress_summary(&self) -> rusqlite::Result<EgressSummary> {
        let cloud_tiers: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT tier FROM tier_settings
                 WHERE enabled = 1 AND provider = 'cloud' AND consented = 1
                 ORDER BY tier",
            )?;
            stmt.query_map([], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let bytes_sent: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(bytes), 0) FROM egress_log",
            [],
            |row| row.get(0),
        )?;
        let bytes_sent = bytes_sent.max(0) as u64;
        let label = if cloud_tiers.is_empty() {
            format!("Local-only · {bytes_sent} bytes egress")
        } else {
            format!(
                "Cloud enabled ({}) · {bytes_sent} bytes egress",
                cloud_tiers.join(", ")
            )
        };
        Ok(EgressSummary {
            cloud_tiers,
            bytes_sent,
            label,
        })
    }

    /// The agent pool controls (#252, AC-1704); nothing gated by default.
    pub fn agent_pool_controls(
        &self,
    ) -> rusqlite::Result<crate::agent_pool_store::AgentPoolControls> {
        crate::agent_pool_store::read(&self.conn)
    }

    /// Persist the agent pool controls; an out-of-range limit changes nothing.
    pub fn set_agent_pool_controls(
        &mut self,
        controls: &crate::agent_pool_store::AgentPoolControls,
    ) -> Result<(), String> {
        crate::agent_pool_store::write(&self.conn, controls)
    }

    /// Every configurable operator limit with the value in effect (#1653).
    pub fn operator_limits(
        &self,
    ) -> Result<
        Vec<crate::operator_limits::OperatorLimit>,
        crate::operator_limits::OperatorLimitError,
    > {
        crate::operator_limits::list(&self.conn)
    }

    /// The value in effect for one operator limit (#1653).
    pub fn operator_limit(
        &self,
        key: &str,
    ) -> Result<u64, crate::operator_limits::OperatorLimitError> {
        crate::operator_limits::read(&self.conn, key)
    }

    /// Save one operator limit, or clear it with `None` (#1653).
    pub fn set_operator_limit(
        &mut self,
        key: &str,
        value: Option<u64>,
    ) -> Result<crate::operator_limits::OperatorLimit, crate::operator_limits::OperatorLimitError>
    {
        crate::operator_limits::write(&self.conn, key, value)
    }

    /// The persisted "Ingest parallelism" setting (#236); Auto by default.
    pub fn ingest_parallelism(&self) -> rusqlite::Result<IngestParallelism> {
        let setting: u32 = self.conn.query_row(
            "SELECT parallelism FROM ingest_settings WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(IngestParallelism::describe(setting))
    }

    /// Persist the "Ingest parallelism" setting: `0` = Auto, otherwise a
    /// fixed worker count up to [`max_workers`]. The
    /// caller applies it to extraction; output never depends on it.
    pub fn set_ingest_parallelism(
        &mut self,
        setting: u32,
    ) -> Result<IngestParallelism, SettingsError> {
        if setting as usize > max_workers() {
            return Err(SettingsError::InvalidParallelism(setting));
        }
        self.conn.execute(
            "UPDATE ingest_settings SET parallelism = ?1 WHERE id = 1",
            params![setting],
        )?;
        Ok(self.ingest_parallelism()?)
    }

    /// The persisted Investigations choice, if the user has made one (#1636).
    /// A row that is incomplete or malformed reads as no choice.
    pub fn investigation_choice(&self) -> rusqlite::Result<Option<InvestigationChoice>> {
        let (specialist_id, limit_profile): (Option<String>, Option<String>) = self.conn.query_row(
            "SELECT specialist_id, limit_profile FROM settings_investigation_choice WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(match (specialist_id, limit_profile) {
            (Some(specialist_id), Some(limit_profile))
                if choice_identifier(&specialist_id) && choice_identifier(&limit_profile) =>
            {
                Some(InvestigationChoice {
                    specialist_id,
                    limit_profile,
                })
            }
            _ => None,
        })
    }

    /// Persist the user's explicit Investigations choice.
    pub fn set_investigation_choice(
        &mut self,
        specialist_id: &str,
        limit_profile: &str,
    ) -> Result<Option<InvestigationChoice>, SettingsError> {
        if !choice_identifier(specialist_id) || !choice_identifier(limit_profile) {
            return Err(SettingsError::InvalidInvestigationChoice);
        }
        self.conn.execute(
            "UPDATE settings_investigation_choice SET specialist_id = ?1, limit_profile = ?2 WHERE id = 1",
            params![specialist_id, limit_profile],
        )?;
        Ok(self.investigation_choice()?)
    }

    /// The persisted investigation time limits (#1633); shipped defaults
    /// until the owner changes them in Configure → Run Limits.
    /// A stored value outside [default, maximum] (a hand-edited or
    /// future-version profile) is reported as an error, never returned as
    /// effective (#1637 review).
    pub fn run_limits(&self) -> Result<RunLimits, SettingsError> {
        let (active, timeout): (Option<i64>, Option<i64>) = self.conn.query_row(
            "SELECT active_seconds, request_timeout_ms FROM run_limit_settings WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let stored = |field: &'static str, value: Option<i64>, min: u64, max: u64| {
            value
                .map(|v| {
                    let checked = u64::try_from(v).unwrap_or(0);
                    check_run_limit(field, checked, min, max).map(|()| checked)
                })
                .transpose()
        };
        let active = stored(
            "active_seconds",
            active,
            default_active_seconds(),
            MAX_ACTIVE_SECONDS,
        )?;
        let timeout = stored(
            "request_timeout_ms",
            timeout,
            default_request_timeout_ms(),
            MAX_REQUEST_TIMEOUT_MS,
        )?;
        Ok(RunLimits::describe(active, timeout))
    }

    /// Persist both time limits. Each must lie between its shipped default
    /// and its maximum; a value equal to the default is stored as `NULL`.
    /// Nothing is written unless both values are valid.
    pub fn set_run_limits(
        &mut self,
        active_seconds: u64,
        request_timeout_ms: u64,
    ) -> Result<RunLimits, SettingsError> {
        let (active_default, timeout_default) =
            (default_active_seconds(), default_request_timeout_ms());
        check_run_limit(
            "active_seconds",
            active_seconds,
            active_default,
            MAX_ACTIVE_SECONDS,
        )?;
        check_run_limit(
            "request_timeout_ms",
            request_timeout_ms,
            timeout_default,
            MAX_REQUEST_TIMEOUT_MS,
        )?;
        let stored = |value: u64, default: u64| {
            (value != default).then(|| i64::try_from(value).unwrap_or(i64::MAX))
        };
        self.conn.execute(
            "UPDATE run_limit_settings SET active_seconds = ?1, request_timeout_ms = ?2 WHERE id = 1",
            params![
                stored(active_seconds, active_default),
                stored(request_timeout_ms, timeout_default)
            ],
        )?;
        self.run_limits()
    }

    /// Return both time limits to the shipped defaults. This also clears a
    /// stored value that [`Self::run_limits`] refuses.
    pub fn reset_run_limits(&mut self) -> Result<RunLimits, SettingsError> {
        self.conn.execute(
            "UPDATE run_limit_settings SET active_seconds = NULL, request_timeout_ms = NULL WHERE id = 1",
            [],
        )?;
        self.run_limits()
    }

    /// The local completion model for new desktop investigations (#1643).
    /// A stored value that is not a model name is reported as an error.
    pub fn local_model(&self) -> Result<LocalModelChoice, SettingsError> {
        let stored: Option<String> = self.conn.query_row(
            "SELECT completion_model FROM settings_local_model WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        match stored {
            Some(model) if !local_model_name(&model) => Err(SettingsError::InvalidLocalModel),
            stored => Ok(LocalModelChoice::describe(stored)),
        }
    }

    /// Persist the local completion model; the shipped default is stored as
    /// `NULL`. The caller confirms the model is installed before saving.
    pub fn set_local_model(&mut self, model: &str) -> Result<LocalModelChoice, SettingsError> {
        if !local_model_name(model) {
            return Err(SettingsError::InvalidLocalModel);
        }
        let stored = (model != llm::OllamaProvider::DEFAULT_COMPLETION_MODEL).then_some(model);
        self.conn.execute(
            "UPDATE settings_local_model SET completion_model = ?1 WHERE id = 1",
            params![stored],
        )?;
        self.local_model()
    }

    /// Return to the shipped default model; also clears an invalid stored value.
    pub fn reset_local_model(&mut self) -> Result<LocalModelChoice, SettingsError> {
        self.conn.execute(
            "UPDATE settings_local_model SET completion_model = NULL WHERE id = 1",
            [],
        )?;
        self.local_model()
    }

    /// The Ollama context window new desktop investigations request (#1653):
    /// `None` leaves it to Ollama. A stored value outside the closed set is
    /// reported as an error.
    pub fn local_context_window(&self) -> Result<Option<u32>, SettingsError> {
        let stored: Option<i64> = self.conn.query_row(
            "SELECT context_tokens FROM settings_local_context WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        match stored {
            None => Ok(None),
            Some(tokens) => u32::try_from(tokens)
                .ok()
                .filter(|tokens| llm::OllamaProvider::CONTEXT_WINDOWS.contains(tokens))
                .map(Some)
                .ok_or(SettingsError::InvalidContextWindow(
                    u32::try_from(tokens).unwrap_or(u32::MAX),
                )),
        }
    }

    /// Persist one of the closed context windows, or `None` for Ollama's own.
    pub fn set_local_context_window(
        &mut self,
        tokens: Option<u32>,
    ) -> Result<Option<u32>, SettingsError> {
        if let Some(tokens) = tokens
            && !llm::OllamaProvider::CONTEXT_WINDOWS.contains(&tokens)
        {
            return Err(SettingsError::InvalidContextWindow(tokens));
        }
        self.conn.execute(
            "UPDATE settings_local_context SET context_tokens = ?1 WHERE id = 1",
            params![tokens],
        )?;
        self.local_context_window()
    }

    /// The read-deny rules every investigation firewall applies: the
    /// defaults, which cannot be removed, plus this project's saved rules in
    /// order (ADR-0083, #679). A stored rule that no longer validates fails
    /// closed: the caller refuses to build a firewall.
    pub fn read_deny_rules(&self) -> Result<llm::read_deny::ReadDenyRules, SettingsError> {
        let mut statement = self
            .conn
            .prepare("SELECT pattern FROM settings_read_deny_rules ORDER BY position")?;
        let patterns = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        llm::read_deny::ReadDenyRules::with_project_rules(patterns)
            .map_err(SettingsError::InvalidReadDenyRules)
    }

    /// Replace this project's read-deny rules. Every rule is validated first,
    /// and nothing is written unless all are valid; duplicates collapse.
    pub fn set_read_deny_rules(
        &mut self,
        patterns: Vec<String>,
    ) -> Result<llm::read_deny::ReadDenyRules, SettingsError> {
        let rules = llm::read_deny::ReadDenyRules::with_project_rules(patterns)
            .map_err(SettingsError::InvalidReadDenyRules)?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM settings_read_deny_rules", [])?;
        for (position, pattern) in rules.project_rules().iter().enumerate() {
            tx.execute(
                "INSERT INTO settings_read_deny_rules (position, pattern) VALUES (?1, ?2)",
                params![position as i64, pattern],
            )?;
        }
        tx.commit()?;
        self.read_deny_rules()
    }

    /// The loopback port new desktop investigations reach Ollama on (#1653):
    /// `None` is Ollama's default, [`llm::OllamaProvider::DEFAULT_PORT`].
    pub fn local_ollama_port(&self) -> Result<Option<u16>, SettingsError> {
        let stored: Option<i64> = self.conn.query_row(
            "SELECT port FROM settings_local_endpoint WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        stored
            .map(|port| {
                u16::try_from(port)
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or(SettingsError::InvalidLocalPort(port))
            })
            .transpose()
    }

    /// Persist a loopback port, or `None` for the default. The default port
    /// itself is stored as `None`, so it reads back as the default.
    pub fn set_local_ollama_port(
        &mut self,
        port: Option<u16>,
    ) -> Result<Option<u16>, SettingsError> {
        if port == Some(0) {
            return Err(SettingsError::InvalidLocalPort(0));
        }
        let port = port.filter(|port| *port != llm::OllamaProvider::DEFAULT_PORT);
        self.conn.execute(
            "UPDATE settings_local_endpoint SET port = ?1 WHERE id = 1",
            params![port],
        )?;
        self.local_ollama_port()
    }

    /// Derive the egress firewall policy from persisted settings: cloud is
    /// allowed only for enabled LLM tiers whose provider is `cloud` **and**
    /// which hold standing consent. Everything else stays local-only.
    pub fn egress_policy(&self) -> rusqlite::Result<llm::EgressPolicy> {
        let mut stmt = self.conn.prepare(
            "SELECT tier FROM tier_settings
             WHERE enabled = 1 AND provider = 'cloud' AND consented = 1",
        )?;
        let tiers = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|tier| match tier.as_str() {
                "T2" => Some(llm::AnalysisTier::Semantic),
                "T3" => Some(llm::AnalysisTier::Agentic),
                _ => None,
            });
        Ok(llm::EgressPolicy::allow_cloud_for(tiers))
    }

    /// Test-only: raw SQL on the settings connection, for fault injection.
    #[cfg(any(test, feature = "test-support"))]
    pub fn execute_batch_for_test(&self, sql: &str) -> rusqlite::Result<()> {
        self.conn.execute_batch(sql)
    }
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    #[test]
    fn investigation_choice_persists_across_reopen_and_rejects_malformed_ids() {
        // AC-0563 (#1636): no choice on a fresh profile; an explicit choice
        // survives reopen; malformed identifiers are refused without writing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(store.investigation_choice().unwrap(), None);
        let chosen = store
            .set_investigation_choice("domain-analyst@3", "investigation-v1")
            .unwrap();
        drop(store);
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(store.investigation_choice().unwrap(), chosen);
        assert_eq!(
            chosen,
            Some(InvestigationChoice {
                specialist_id: "domain-analyst@3".into(),
                limit_profile: "investigation-v1".into(),
            })
        );
        for (specialist, profile) in [
            ("", "investigation-v1"),
            ("Domain Analyst", "investigation-v1"),
            ("domain-analyst@3", &*"x".repeat(129)),
        ] {
            assert!(matches!(
                store.set_investigation_choice(specialist, profile),
                Err(SettingsError::InvalidInvestigationChoice)
            ));
        }
        assert_eq!(store.investigation_choice().unwrap(), chosen);
    }

    // AC-0564: the local model defaults to the shipped one, persists across
    // reopen, refuses non-model names without writing, and resets.
    #[test]
    fn local_model_default_persist_validate_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = SettingsStore::open(&path).unwrap();
        let shipped = store.local_model().unwrap();
        assert_eq!(shipped.model, llm::OllamaProvider::DEFAULT_COMPLETION_MODEL);
        assert!(!shipped.customized);
        let chosen = store.set_local_model("qwen3:30b-a3b").unwrap();
        assert_eq!(chosen.model, "qwen3:30b-a3b");
        assert!(chosen.customized);
        for bad in ["", "qwen3 30b", "a;b", &"x".repeat(129)] {
            assert!(matches!(
                store.set_local_model(bad),
                Err(SettingsError::InvalidLocalModel)
            ));
        }
        drop(store);
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(store.local_model().unwrap(), chosen);
        let default = store
            .set_local_model(llm::OllamaProvider::DEFAULT_COMPLETION_MODEL)
            .unwrap();
        assert!(!default.customized);
        store.set_local_model("library/qwen3:30b-a3b").unwrap();
        store
            .conn
            .execute(
                "UPDATE settings_local_model SET completion_model = 'bad model' WHERE id = 1",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.local_model(),
            Err(SettingsError::InvalidLocalModel)
        ));
        assert_eq!(store.reset_local_model().unwrap(), shipped);
    }

    // #1653: the Ollama port defaults to 11434, persists across reopen,
    // refuses port 0 and stores the default port as the default.
    #[test]
    fn local_ollama_port_default_persist_validate_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(store.local_ollama_port().unwrap(), None);
        assert!(matches!(
            store.set_local_ollama_port(Some(0)),
            Err(SettingsError::InvalidLocalPort(0))
        ));
        assert_eq!(
            store.set_local_ollama_port(Some(11500)).unwrap(),
            Some(11500)
        );
        assert_eq!(
            store
                .set_local_ollama_port(Some(llm::OllamaProvider::DEFAULT_PORT))
                .unwrap(),
            None
        );
        store.set_local_ollama_port(Some(11500)).unwrap();
        drop(store);
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(store.local_ollama_port().unwrap(), Some(11500));
        assert_eq!(store.set_local_ollama_port(None).unwrap(), None);
    }

    // AC-1417 / #679: project read-deny rules start empty, persist in order
    // across reopen, collapse duplicates, are replaced whole, write nothing
    // when any rule is invalid, and fail closed on a tampered stored rule.
    #[test]
    fn read_deny_rules_persist_validate_and_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = SettingsStore::open(&path).unwrap();
        assert!(store.read_deny_rules().unwrap().project_rules().is_empty());
        let saved = store
            .set_read_deny_rules(vec!["secrets/*".into(), "*.sql".into(), "secrets/*".into()])
            .unwrap();
        assert_eq!(saved.project_rules(), ["secrets/*", "*.sql"]);
        assert!(matches!(
            store.set_read_deny_rules(vec!["ok/*".into(), "/abs".into()]),
            Err(SettingsError::InvalidReadDenyRules(_))
        ));
        drop(store);
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(
            store.read_deny_rules().unwrap().project_rules(),
            ["secrets/*", "*.sql"]
        );
        // The defaults still apply on top of the project rules.
        assert!(
            store
                .read_deny_rules()
                .unwrap()
                .matching_rule(".env")
                .is_some()
        );
        assert_eq!(
            store
                .read_deny_rules()
                .unwrap()
                .matching_rule("secrets/a")
                .as_deref(),
            Some("project:0")
        );
        store
            .conn
            .execute(
                "UPDATE settings_read_deny_rules SET pattern = '../x' WHERE position = 0",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.read_deny_rules(),
            Err(SettingsError::InvalidReadDenyRules(_))
        ));
        assert!(
            store
                .set_read_deny_rules(vec![])
                .unwrap()
                .project_rules()
                .is_empty()
        );
    }

    // #1653: the context window defaults to Ollama's own, persists across
    // reopen, refuses values outside the closed set without writing, and
    // reports a tampered stored value.
    #[test]
    fn local_context_window_default_persist_validate_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(store.local_context_window().unwrap(), None);
        assert_eq!(
            store.set_local_context_window(Some(32768)).unwrap(),
            Some(32768)
        );
        for bad in [0, 4096, 32767, 131072] {
            assert!(matches!(
                store.set_local_context_window(Some(bad)),
                Err(SettingsError::InvalidContextWindow(value)) if value == bad
            ));
        }
        drop(store);
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(store.local_context_window().unwrap(), Some(32768));
        store
            .conn
            .execute(
                "UPDATE settings_local_context SET context_tokens = 5000 WHERE id = 1",
                [],
            )
            .unwrap();
        assert!(store.local_context_window().is_err());
        assert_eq!(store.set_local_context_window(None).unwrap(), None);
    }

    #[test]
    fn run_limits_default_persist_bound_and_reset() {
        // AC-0562 (#1633): shipped defaults until changed; a valid change
        // persists across reopen; values outside [default, max] are refused
        // without writing either field; reset restores the defaults.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = SettingsStore::open(&path).unwrap();
        let shipped = store.run_limits().unwrap();
        assert_eq!(shipped.active_seconds, 600);
        assert_eq!(shipped.request_timeout_ms, 180_000);
        assert_eq!(
            (shipped.max_active_seconds, shipped.max_request_timeout_ms),
            (3_600, 600_000)
        );
        assert!(!shipped.customized);

        let raised = store.set_run_limits(3_600, 600_000).unwrap();
        assert_eq!(
            (raised.active_seconds, raised.request_timeout_ms),
            (3_600, 600_000)
        );
        assert!(raised.customized);
        drop(store);
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(store.run_limits().unwrap(), raised);

        for (active, timeout, field) in [
            (3_601, 600_000, "active_seconds"),
            (599, 600_000, "active_seconds"),
            (1_200, 600_001, "request_timeout_ms"),
            (1_200, 179_999, "request_timeout_ms"),
        ] {
            assert!(matches!(
                store.set_run_limits(active, timeout),
                Err(SettingsError::InvalidRunLimit { field: f, .. }) if f == field
            ));
            assert_eq!(store.run_limits().unwrap(), raised);
        }

        let partly = store.set_run_limits(1_200, 180_000).unwrap();
        assert_eq!(
            (partly.active_seconds, partly.request_timeout_ms),
            (1_200, 180_000)
        );
        let reset = store.reset_run_limits().unwrap();
        assert_eq!(reset, shipped);
    }

    #[test]
    fn out_of_range_stored_run_limits_are_refused_on_read() {
        // AC-0562 (#1637 review): a persisted value outside [default, max]
        // is never reported as effective, after reopen too; reset clears it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        for (column, value, field) in [
            ("active_seconds", 3_601_i64, "active_seconds"),
            ("active_seconds", 599, "active_seconds"),
            ("request_timeout_ms", 600_001, "request_timeout_ms"),
            ("request_timeout_ms", 1, "request_timeout_ms"),
        ] {
            let store = SettingsStore::open(&path).unwrap();
            store
                .conn
                .execute(
                    &format!("UPDATE run_limit_settings SET {column} = ?1 WHERE id = 1"),
                    params![value],
                )
                .unwrap();
            drop(store);
            let mut store = SettingsStore::open(&path).unwrap();
            assert!(matches!(
                store.run_limits(),
                Err(SettingsError::InvalidRunLimit { field: f, .. }) if f == field
            ));
            let reset = store.reset_run_limits().unwrap();
            assert!(!reset.customized);
        }
    }

    #[test]
    fn ingest_parallelism_defaults_to_auto_persists_and_rejects_out_of_range() {
        // AC-0208 (#236): Auto by default; a fixed choice persists across
        // reopen; a value beyond this machine's parallelism is refused.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = SettingsStore::open(&path).unwrap();
        let auto = store.ingest_parallelism().unwrap();
        assert_eq!(auto.setting, 0);
        assert_eq!(auto.workers, auto.auto_workers);
        assert!(auto.auto_workers >= 1 && auto.max_workers >= 1);
        let serial = store.set_ingest_parallelism(1).unwrap();
        assert_eq!((serial.setting, serial.workers), (1, 1));
        drop(store);
        let mut store = SettingsStore::open(&path).unwrap();
        assert_eq!(store.ingest_parallelism().unwrap().setting, 1);
        let too_many = auto.max_workers + 1;
        assert!(matches!(
            store.set_ingest_parallelism(too_many),
            Err(SettingsError::InvalidParallelism(n)) if n == too_many
        ));
        assert_eq!(store.ingest_parallelism().unwrap().setting, 1);
    }

    #[test]
    fn a_stored_worker_count_above_this_machine_is_normalized_on_read() {
        // AC-0208 (#478): a fixed count persisted on a larger machine (or
        // before a CPU quota shrank) resolves to this machine's maximum, both
        // for what Settings shows and for what startup applies.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let store = SettingsStore::open(&path).unwrap();
        store
            .conn
            .execute(
                "UPDATE ingest_settings SET parallelism = 10000 WHERE id = 1",
                [],
            )
            .unwrap();
        let read = store.ingest_parallelism().unwrap();
        assert_eq!(read.setting, read.max_workers);
        assert_eq!(read.workers, read.max_workers);
        assert_eq!(normalize_setting(0, 8), 0, "Auto is kept");
        assert_eq!(normalize_setting(5, 8), 5);
        assert_eq!(normalize_setting(65, 64), 64);
        assert_eq!(normalize_setting(3, 0), 1, "never below one worker");
    }

    #[test]
    fn plugin_enablement_is_per_project_per_artifact_and_fails_closed() {
        // AC-0068 (#198): enable/disable persists per project AND binds to
        // the exact artifact hash — unknown rows and changed bytes are
        // disabled by default (#203 review).
        let dir = tempfile::tempdir().unwrap();
        let mut store = super::SettingsStore::open(dir.path().join("state.db")).unwrap();
        assert!(store.enabled_plugins("/proj/a").unwrap().is_empty());

        store
            .set_plugin_enabled("/proj/a", "t0.adapter-ruby", "hash-1", true)
            .unwrap();
        store
            .set_plugin_enabled("/proj/a", "t0.adapter-swift", "hash-2", true)
            .unwrap();
        store
            .set_plugin_enabled("/proj/b", "t0.adapter-ruby", "hash-3", true)
            .unwrap();
        assert_eq!(
            store.enabled_plugins("/proj/a").unwrap(),
            [
                ("t0.adapter-ruby".to_string(), "hash-1".to_string()),
                ("t0.adapter-swift".to_string(), "hash-2".to_string()),
            ]
        );

        // Disable persists too, and only for that project.
        store
            .set_plugin_enabled("/proj/a", "t0.adapter-ruby", "hash-1", false)
            .unwrap();
        assert_eq!(
            store.enabled_plugins("/proj/a").unwrap(),
            [("t0.adapter-swift".to_string(), "hash-2".to_string())]
        );
        assert_eq!(
            store.enabled_plugins("/proj/b").unwrap(),
            [("t0.adapter-ruby".to_string(), "hash-3".to_string())]
        );

        // Re-enabling under new bytes replaces the bound hash — the old
        // opt-in never silently covers a different artifact.
        store
            .set_plugin_enabled("/proj/b", "t0.adapter-ruby", "hash-4", true)
            .unwrap();
        assert_eq!(
            store.enabled_plugins("/proj/b").unwrap(),
            [("t0.adapter-ruby".to_string(), "hash-4".to_string())]
        );
    }

    #[test]
    fn plugin_gate_outcome_binds_to_the_exact_artifact() {
        // AC-0068 (#200): a gate verdict is stored per (plugin, artifact
        // hash). Unknown pairs are ungated — the proposed state — and
        // replaced bytes never inherit an earlier pass.
        let dir = tempfile::tempdir().unwrap();
        let mut store = super::SettingsStore::open(dir.path().join("state.db")).unwrap();
        assert_eq!(
            store.plugin_gate("t0.adapter-ruby", "hash-1").unwrap(),
            None
        );

        store
            .record_plugin_gate("t0.adapter-ruby", "hash-1", true, r#"{"passed":true}"#)
            .unwrap();
        assert_eq!(
            store.plugin_gate("t0.adapter-ruby", "hash-1").unwrap(),
            Some((true, r#"{"passed":true}"#.to_string()))
        );
        // Different bytes of the same plugin id: still ungated.
        assert_eq!(
            store.plugin_gate("t0.adapter-ruby", "hash-2").unwrap(),
            None
        );

        // A rerun overwrites the verdict for that exact artifact.
        store
            .record_plugin_gate("t0.adapter-ruby", "hash-1", false, r#"{"passed":false}"#)
            .unwrap();
        assert_eq!(
            store.plugin_gate("t0.adapter-ruby", "hash-1").unwrap(),
            Some((false, r#"{"passed":false}"#.to_string()))
        );
    }

    #[test]
    fn bounded_plugin_selection_uses_exact_identity_without_report_decode() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = super::SettingsStore::open(dir.path().join("state.db")).unwrap();
        assert!(
            !store
                .plugin_enabled_exact("root", "plugin", "hash")
                .unwrap()
        );
        assert!(!store.plugin_gate_passed_exact("plugin", "gate").unwrap());
        store
            .set_plugin_enabled("root", "plugin", "hash", true)
            .unwrap();
        store
            .record_plugin_gate("plugin", "gate", true, &"x".repeat(1_000_000))
            .unwrap();
        assert!(
            store
                .plugin_enabled_exact("root", "plugin", "hash")
                .unwrap()
        );
        assert!(
            !store
                .plugin_enabled_exact("other", "plugin", "hash")
                .unwrap()
        );
        assert!(
            !store
                .plugin_enabled_exact("root", "plugin", "changed")
                .unwrap()
        );
        assert!(store.plugin_gate_passed_exact("plugin", "gate").unwrap());
        assert!(!store.plugin_gate_passed_exact("plugin", "changed").unwrap());
        // Invalid UTF-8 cannot be decoded as a String by the legacy report API.
        // Scalar eligibility deliberately never retrieves the report column.
        store
            .conn
            .execute(
                "UPDATE plugin_gates SET report_json=CAST(x'FF' AS TEXT)",
                [],
            )
            .unwrap();
        assert!(store.plugin_gate("plugin", "gate").is_err());
        assert!(store.plugin_gate_passed_exact("plugin", "gate").unwrap());
        store
            .set_plugin_enabled("root", "plugin", "hash", false)
            .unwrap();
        store
            .record_plugin_gate("plugin", "gate", false, "{}")
            .unwrap();
        assert!(
            !store
                .plugin_enabled_exact("root", "plugin", "hash")
                .unwrap()
        );
        assert!(!store.plugin_gate_passed_exact("plugin", "gate").unwrap());
    }

    #[test]
    fn defaults_are_local_only_and_durable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let store = SettingsStore::open(&path).unwrap();
            let all = store.all().unwrap();
            assert_eq!(all.len(), 3);
            assert!(all.iter().all(|tier| tier.enabled));
            assert!(all.iter().all(|tier| tier.provider == "local"));
            assert!(all.iter().all(|tier| !tier.consented));
            assert_eq!(
                store.egress_summary().unwrap().label,
                "Local-only · 0 bytes egress"
            );
        }
        // Reopen: settings survive and are not re-seeded over.
        let mut store = SettingsStore::open(&path).unwrap();
        store.set_enabled("T3", false).unwrap();
        drop(store);
        let store = SettingsStore::open(&path).unwrap();
        let t3 = &store.all().unwrap()[2];
        assert!(!t3.enabled);
    }

    #[test]
    fn t0_is_not_configurable() {
        // The deterministic tier is always on: it is not even part of the
        // settings vocabulary, so no code path can disable it.
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();
        assert!(matches!(
            store.set_enabled("T0", false),
            Err(SettingsError::UnknownTier(_))
        ));
        assert!(matches!(
            store.set_provider("T0", "cloud"),
            Err(SettingsError::UnknownTier(_))
        ));
    }

    #[test]
    fn consent_is_fail_closed_and_revocation_immediate() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();

        // Consent without a cloud provider is impossible.
        assert!(matches!(
            store.grant_consent("T2", "{}"),
            Err(SettingsError::NotCloudProvider)
        ));
        // T1 never gets a provider choice (no LLM in the dynamic tier).
        assert!(matches!(
            store.set_provider("T1", "cloud"),
            Err(SettingsError::NoProvider(_))
        ));

        // The legal path: cloud provider, then explicit consent.
        store.set_provider("T2", "cloud").unwrap();
        store
            .grant_consent(
                "T2",
                r#"{"provider":"Anthropic","model":"claude-opus-4-8"}"#,
            )
            .unwrap();
        let summary = store.egress_summary().unwrap();
        assert_eq!(summary.cloud_tiers, vec!["T2"]);
        assert!(summary.label.starts_with("Cloud enabled (T2)"));
        // The derived firewall policy opens exactly that tier.
        assert!(
            store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Semantic)
        );
        assert!(
            !store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Agentic)
        );

        // Revocation is immediate and total.
        store.revoke_consent("T2").unwrap();
        assert!(
            !store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Semantic)
        );
        assert_eq!(
            store.egress_summary().unwrap().label,
            "Local-only · 0 bytes egress"
        );
        let t2 = &store.all().unwrap()[1];
        assert_eq!(t2.consent_disclosure, None);
    }

    #[test]
    fn disabling_a_tier_drops_it_from_the_cloud_summary() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();
        store.set_provider("T2", "cloud").unwrap();
        store.grant_consent("T2", "{}").unwrap();
        assert_eq!(store.egress_summary().unwrap().cloud_tiers, vec!["T2"]);

        // A disabled tier can never egress, so the status bar must not
        // advertise it as cloud-enabled — summary and policy stay in lockstep.
        store.set_enabled("T2", false).unwrap();
        let summary = store.egress_summary().unwrap();
        assert!(summary.cloud_tiers.is_empty());
        assert!(summary.label.starts_with("Local-only"));
        assert!(
            !store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Semantic)
        );
    }

    #[test]
    fn switching_away_from_cloud_revokes_consent() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();
        store.set_provider("T3", "cloud").unwrap();
        store.grant_consent("T3", "{}").unwrap();
        // Back to local: standing consent cannot survive the switch.
        store.set_provider("T3", "local").unwrap();
        let t3 = &store.all().unwrap()[2];
        assert!(!t3.consented);
        assert!(
            !store
                .egress_policy()
                .unwrap()
                .cloud_allowed(llm::AnalysisTier::Agentic)
        );
    }

    #[test]
    fn egress_summary_accumulates_observed_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::open(dir.path().join("state.db")).unwrap();
        store
            .record_egress("anthropic:claude-opus-4-8", 1200)
            .unwrap();
        store
            .record_egress("anthropic:claude-opus-4-8", 300)
            .unwrap();
        assert_eq!(store.egress_summary().unwrap().bytes_sent, 1500);
    }
}
