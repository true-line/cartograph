//! The topology manifest — `cartograph.system.toml` (SPEC-00 §10,
//! US-0001 AC-0002): an author-editable declaration of which repos form
//! one system, per-repo layer hints, and known channel identities.
//! Everything in it is Confirmed-by-author knowledge, applied
//! deterministically at ingest; T2 may *suggest* additions later (M7),
//! always user-confirmed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The canonical manifest file name.
pub const MANIFEST_NAME: &str = "cartograph.system.toml";

/// A repository's own env-read allowlist (ADR-0075), relative to its root.
pub const REPO_ENV_REL_PATH: &str = ".cartograph/env.toml";

/// Manifest errors.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// TOML syntax/shape failure — the message names the field.
    #[error("manifest parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// A repo entry names a layer outside the five-layer model.
    #[error("unknown layer \"{0}\" (expected infra, cloud, server, events, client)")]
    UnknownLayer(String),
    /// A service names a repo that no `[[repos]]` entry declares.
    #[error("service \"{service}\" names repo \"{repo}\", which no [[repos]] entry declares")]
    UnknownServiceRepo {
        /// The service name.
        service: String,
        /// The undeclared repo url.
        repo: String,
    },
    /// A gateway route names a service no `[[services]]` entry declares.
    #[error("gateway route {prefix} names undeclared service \"{service}\"")]
    UnknownService {
        /// The route prefix.
        prefix: String,
        /// The undeclared service name.
        service: String,
    },
    /// Two `[[services]]` entries share a name, so a gateway route naming
    /// it could not say which repo it reaches.
    #[error("service \"{0}\" is declared more than once")]
    DuplicateService(String),
    /// A base URL is not an absolute `http(s)://host[/path]` URL.
    #[error("base URL \"{0}\" must be absolute http(s)://host[/path], without query or fragment")]
    InvalidBaseUrl(String),
    /// An env-read allowlist entry is not an exact key name (ADR-0075).
    #[error(
        "env_read entry \"{0}\" must be an exact env key name ([A-Za-z_][A-Za-z0-9_.]*); globs are not allowed"
    )]
    InvalidEnvReadKey(String),
    /// `.cartograph/env.toml` is not a readable regular file in the repository.
    #[error("{0}")]
    RepoEnvUnreadable(String),
}

/// The five layers of SPEC-00 §3; hints gate which extractors run.
pub const LAYERS: &[&str] = &["infra", "cloud", "server", "events", "client"];

/// One repo in the system set.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestRepo {
    /// GitHub URL / `owner/name` / `file://` / local path.
    pub url: String,
    /// Layer hints; empty means "extract everything".
    #[serde(default)]
    pub layers: Vec<String>,
    /// Path to `terraform show -json` output (state or plan) for this
    /// repo's infra, relative to the manifest. Observed values feed T1
    /// enrichment (SPEC-00 §3.1, AC-0009); absent means T0 only.
    #[serde(default)]
    pub state_json: Option<String>,
    /// Path to `pulumi stack export` or `pulumi preview --json` output,
    /// relative to the manifest. It enriches import-proven Pulumi T0 facts
    /// without creating resources from observation alone (AC-0052).
    #[serde(default)]
    pub pulumi_json: Option<String>,
    /// OTLP/JSON trace files in collector file-exporter JSON Lines format,
    /// relative to the manifest. Observed messaging identities may fill
    /// unresolved T0 channel slots at T1 (AC-0012, issue #54).
    #[serde(default)]
    pub otel_jsonl: Vec<String>,
    /// Author-declared API reference files for documented-contract drift
    /// comparison (#739, AC-0313). Unioned with `.cartograph/api-docs.toml`
    /// in the repo root, with the manifest entry winning on duplicate `path`.
    #[serde(default)]
    pub api_docs: Vec<crate::api_docs::ApiDocDeclaration>,
}

/// A service another repo's clients call over HTTP (#589, AC-0240): the
/// author declares which repo serves which base URLs. A client call binds
/// to that repo's endpoints only through such a declaration.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ManifestService {
    /// Service name, cited on bound edges.
    pub name: String,
    /// The `[[repos]]` url of the repo that serves it.
    pub repo: String,
    /// Absolute base URLs the service is reachable at, e.g.
    /// `https://app.example.com` or `http://localhost:7777`.
    #[serde(default)]
    pub base_urls: Vec<String>,
}

/// A gateway route forwarding a path prefix under a base URL to a declared
/// service (#589, AC-0240).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ManifestGatewayRoute {
    /// The gateway's absolute base URL.
    pub base_url: String,
    /// Path prefix routed, e.g. `/api`.
    pub prefix: String,
    /// The `[[services]]` name it forwards to.
    pub service: String,
    /// Whether the gateway removes `prefix` before forwarding.
    #[serde(default)]
    pub strip_prefix: bool,
}

/// True for an absolute `http(s)://host[/path]` URL with no query or
/// fragment.
pub fn is_base_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    !host.is_empty() && !url.contains(['?', '#', ' '])
}

/// The parsed manifest.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SystemManifest {
    /// The repo set forming one analyzable system.
    #[serde(default)]
    pub repos: Vec<ManifestRepo>,
    /// Known identities: key → value, applied to the config resolver ahead
    /// of `.env` files (author-declared, e.g. a queue URL the code reads
    /// from an env var no checked-in file defines). A key may also be the
    /// source text of a runtime base URL a client joins its paths to
    /// (`SETTINGS.apiUrl`, #737): only this declaration resolves it.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Env keys whose values the config resolver may read from env files
    /// (ADR-0075, AC-1018): exact names, empty by default. No other
    /// env-file value is read.
    #[serde(default)]
    pub env_read: Vec<String>,
    /// Declared HTTP services (#589).
    #[serde(default)]
    pub services: Vec<ManifestService>,
    /// Declared gateway routes (#589).
    #[serde(default)]
    pub gateway_routes: Vec<ManifestGatewayRoute>,
}

impl SystemManifest {
    /// Load and validate a manifest from a file path (or a directory
    /// containing `cartograph.system.toml`).
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let file = if path.is_dir() {
            path.join(MANIFEST_NAME)
        } else {
            path.to_path_buf()
        };
        let text = std::fs::read_to_string(&file)?;
        let manifest: SystemManifest = toml::from_str(&text)?;
        for repo in &manifest.repos {
            for layer in &repo.layers {
                if !LAYERS.contains(&layer.as_str()) {
                    return Err(ManifestError::UnknownLayer(layer.clone()));
                }
            }
        }
        manifest.validate_bindings()?;
        validate_env_read(&manifest.env_read)?;
        Ok(manifest)
    }

    /// The `env_read` allowlist as a set.
    pub fn env_read_keys(&self) -> BTreeSet<String> {
        self.env_read.iter().cloned().collect()
    }

    /// Check that service names are unique, services name declared repos,
    /// gateway routes name declared services, and every base URL is absolute.
    pub fn validate_bindings(&self) -> Result<(), ManifestError> {
        let mut names = std::collections::BTreeSet::new();
        for service in &self.services {
            if !names.insert(service.name.as_str()) {
                return Err(ManifestError::DuplicateService(service.name.clone()));
            }
            if !self.repos.iter().any(|repo| repo.url == service.repo) {
                return Err(ManifestError::UnknownServiceRepo {
                    service: service.name.clone(),
                    repo: service.repo.clone(),
                });
            }
            if let Some(bad) = service.base_urls.iter().find(|url| !is_base_url(url)) {
                return Err(ManifestError::InvalidBaseUrl(bad.clone()));
            }
        }
        for route in &self.gateway_routes {
            if !self
                .services
                .iter()
                .any(|service| service.name == route.service)
            {
                return Err(ManifestError::UnknownService {
                    prefix: route.prefix.clone(),
                    service: route.service.clone(),
                });
            }
            if !is_base_url(&route.base_url) {
                return Err(ManifestError::InvalidBaseUrl(route.base_url.clone()));
            }
        }
        Ok(())
    }
}

/// Refuse an allowlist entry that is not an exact env key name.
fn validate_env_read(keys: &[String]) -> Result<(), ManifestError> {
    let exact = |key: &str| {
        let mut chars = key.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    };
    match keys.iter().find(|key| !exact(key)) {
        Some(bad) => Err(ManifestError::InvalidEnvReadKey(bad.clone())),
        None => Ok(()),
    }
}

/// Top-level schema of a repository's `.cartograph/env.toml`.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoEnv {
    #[serde(default)]
    read: Vec<String>,
}

/// The env keys a repository lets the config resolver read, from
/// `.cartograph/env.toml` at its root (ADR-0075, AC-1018): `read = [...]`,
/// exact names. No file is an empty allowlist; a malformed file or entry,
/// a symlink, or a non-regular or oversized file is an error, never an
/// empty list.
pub fn repo_env_read(root: &Path) -> Result<BTreeSet<String>, ManifestError> {
    use crate::api_docs::{AnchoredRead, MAX_API_DOC_BYTES, anchored_reason, read_anchored};
    // No symlink anywhere in the path, a regular file, bounded size: the
    // same anchored read as `.cartograph/api-docs.toml`.
    let bytes = match read_anchored(root, REPO_ENV_REL_PATH, MAX_API_DOC_BYTES) {
        Ok(bytes) => bytes,
        Err(AnchoredRead::NotFound) => return Ok(BTreeSet::new()),
        Err(failure) => {
            return Err(ManifestError::RepoEnvUnreadable(anchored_reason(
                REPO_ENV_REL_PATH,
                failure,
            )));
        }
    };
    let text = String::from_utf8(bytes).map_err(|error| {
        ManifestError::RepoEnvUnreadable(format!(
            "{REPO_ENV_REL_PATH} is not valid UTF-8 ({error})"
        ))
    })?;
    let parsed: RepoEnv = toml::from_str(&text)?;
    validate_env_read(&parsed.read)?;
    Ok(parsed.read.into_iter().collect())
}
