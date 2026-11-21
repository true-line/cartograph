//! Channel-identity resolution — deterministic (T0) stitching of
//! producer/consumer event sites into the channel graph (SPEC-00 §3.4,
//! US-0004).
//!
//! Language adapters emit [`adapters_fw::events::EventSite`] facts; this
//! crate resolves each site's channel identity (literal → as-is, env ref →
//! via the [`ConfigIndex`] built from env files present in the repo) and
//! emits `Channel{kind, identity}` nodes with Confirmed `PUBLISHES` /
//! `SUBSCRIBES` edges (AC-0010, AC-0011). A runtime-computed identity
//! cannot be resolved at T0: the site emits an explicit `Gap` node with a
//! reason and the attempted tiers — never silently dropped (AC-0012,
//! R-INT-4). The T1/T2 rungs of the ladder join at M6/M7; cross-repo
//! matching at M5.

use adapters_fw::events::{ChannelRole, EventSite, IdentityExpr, RegistryIdentity};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub mod http_binding;
pub use http_binding::{
    BINDING_STATE, BINDING_STATE_AMBIGUOUS, BINDING_STATE_NAMES_REPOSITORY, BINDING_STATE_NONE,
    BINDING_STATE_ORIGIN_DECLARED, BindingKind, CrossRepoStitch, DEFERRED_REASON_PREFIX,
    HttpBinding, JoinedFetch, bind_observed_clients, binding_origins, stitch_cross_repo,
    stitch_cross_repo_with,
};

/// Stitching errors.
#[derive(Debug, thiserror::Error)]
pub enum StitchError {
    /// Filesystem failure while scanning for config files.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Env files hold values that look like secrets, so the ingest is
    /// refused until they are removed (#933, AC-0990). Each entry names the
    /// file, line and key; no value is ever carried.
    #[error("{}", refusal_message(.0))]
    SecretsInEnvFiles(Vec<EnvSecret>),
}

/// One secret-shaped env-file value that refused an ingest (AC-0990).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSecret {
    /// Repo-relative env file.
    pub file: String,
    /// 1-based line.
    pub line: usize,
    /// The key; never the value.
    pub key: String,
    /// Why the value counts as a secret.
    pub why: &'static str,
}

/// Most refused values a refusal lists by name; the rest are counted.
const MAX_LISTED_SECRETS: usize = 20;

fn refusal_message(found: &[EnvSecret]) -> String {
    let mut message = format!(
        "ingest refused: {} env-file value{} look{} like secrets. Cartograph does not ingest a \
         checkout whose env files hold secrets (#933).",
        found.len(),
        if found.len() == 1 { "" } else { "s" },
        if found.len() == 1 { "s" } else { "" },
    );
    for secret in found.iter().take(MAX_LISTED_SECRETS) {
        message.push_str(&format!(
            "\n- {}:{} {} ({})",
            secret.file, secret.line, secret.key, secret.why
        ));
    }
    if found.len() > MAX_LISTED_SECRETS {
        message.push_str(&format!(
            "\n- and {} more",
            found.len() - MAX_LISTED_SECRETS
        ));
    }
    message.push_str(
        "\nTo proceed: remove these values (or the files) from the checkout and ingest again. \
         A value the resolver needs that is not a secret, such as a queue name or a base URL \
         without credentials, can be declared in cartograph.system.toml under [env].",
    );
    message
}

/// Facts stitched from event sites (same shape as the extractor outputs).
#[derive(Debug, Default)]
pub struct Extraction {
    /// `Channel` and `Gap` nodes.
    pub nodes: Vec<Node>,
    /// `PUBLISHES` / `SUBSCRIBES` edges.
    pub edges: Vec<Edge>,
}

/// Identity of the source being stitched; lands in every `EvidenceRef`.
pub struct SourceId<'a> {
    /// Repository identifier (e.g. `owner/name`, or `local` for a bare dir).
    pub repo: &'a str,
    /// Commit SHA, or `workdir` when extracting an unversioned tree.
    pub commit: &'a str,
}

/// Extractor id of channel and client-call stitching over extracted sites.
pub const STITCH_EXTRACTOR_ID: &str = "t0.events";
const EXTRACTOR_ID: &str = STITCH_EXTRACTOR_ID;

/// Env keys resolved from env files present in the repo (AC-0011). Only
/// files literally named `.env` or `.env.*` participate — this is the
/// deterministic config resolver, not environment guessing.
#[derive(Debug, Default)]
pub struct ConfigIndex {
    /// key → (value, repo-relative source file)
    env: BTreeMap<String, (String, String)>,
    /// Origins the system manifest binds; absolute calls to them wait for
    /// the system pass (#589).
    deferred_origins: std::collections::BTreeSet<String>,
    /// Keys the system manifest declared, the only source for a base URL a
    /// client reads from runtime state (#737).
    manifest_keys: std::collections::BTreeSet<String>,
}

impl ConfigIndex {
    /// Scan `root` for env files (skipping `node_modules`, `dist`, and
    /// dot-directories) and index their `KEY=VALUE` lines. Files are
    /// visited in sorted order and the first definition of a key wins, so
    /// the index is deterministic regardless of walk order (US-0014).
    ///
    /// Fails closed with [`StitchError::SecretsInEnvFiles`] when any line
    /// holds a secret-shaped value (AC-0990), listing every one in file and
    /// line order, so one refusal names everything to remove.
    pub fn from_dir(root: &Path) -> Result<Self, StitchError> {
        let mut index = ConfigIndex::default();
        let mut secrets = Vec::new();
        for rel in env_files(root)? {
            let content = std::fs::read_to_string(root.join(&rel))?;
            for (number, line) in content.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let key = key.trim().trim_start_matches("export ").trim();
                let value = value.trim().trim_matches(['"', '\'']);
                if key.is_empty() || key.contains(char::is_whitespace) {
                    continue;
                }
                if let Some(why) = secret_reason(key, value) {
                    secrets.push(EnvSecret {
                        file: rel.clone(),
                        line: number + 1,
                        key: key.to_string(),
                        why,
                    });
                    continue;
                }
                index
                    .env
                    .entry(key.to_string())
                    .or_insert_with(|| (value.to_string(), rel.clone()));
            }
        }
        if !secrets.is_empty() {
            return Err(StitchError::SecretsInEnvFiles(secrets));
        }
        Ok(index)
    }

    /// Resolve an env key to `(value, source file)`.
    pub fn resolve(&self, key: &str) -> Option<(&str, &str)> {
        self.env.get(key).map(|(v, f)| (v.as_str(), f.as_str()))
    }

    /// Defer absolute-URL calls to these origins (`scheme://host`) to the
    /// system-wide binding pass instead of matching them by path in this
    /// repo (#589): the manifest's declaration outranks a path coincidence.
    pub fn defer_origins(&mut self, origins: &std::collections::BTreeSet<String>) {
        self.deferred_origins.extend(origins.iter().cloned());
    }

    /// Apply author-declared identities from the topology manifest
    /// (US-0001 AC-0002). Manifest entries *override* env files — the
    /// author's declaration is the most confirmed knowledge we have.
    pub fn apply_manifest(&mut self, env: &BTreeMap<String, String>, source: &str) {
        for (key, value) in env {
            self.env
                .insert(key.clone(), (value.clone(), source.to_string()));
            self.manifest_keys.insert(key.clone());
        }
    }

    /// Resolve a runtime expression (`SETTINGS.apiUrl`) only when the system
    /// manifest declares it by its source text; env files never do (#737).
    pub fn resolve_declared(&self, expr: &str) -> Option<(&str, &str)> {
        self.manifest_keys
            .contains(expr)
            .then(|| self.resolve(expr))
            .flatten()
    }
}

/// Env files are local configuration that repositories conventionally
/// `.gitignore` — yet they are exactly what identity resolution reads, so this
/// walk deliberately disregards `.gitignore` (#248); only the fixed skips apply.
///
/// Returns their repo-relative paths, sorted. Public so a clean-checkout
/// check can prove every env file recovery reads is tracked (#740).
pub fn env_files(root: &Path) -> std::io::Result<Vec<String>> {
    let skip = |name: &str| name == "node_modules" || name == "dist" || name.starts_with('.');
    let mut files: Vec<String> =
        source_walk::files(root, &skip, source_walk::Gitignores::Disregard)?
            .into_iter()
            .filter(|file| file.name == ".env" || file.name.starts_with(".env."))
            .map(|file| file.rel)
            .collect();
    files.sort();
    Ok(files)
}

/// Why an env-file value counts as a secret, or none (#933, AC-0990):
/// a recognized secret pattern (provider tokens, private keys, cloud access
/// keys), a token-shaped value, a URL carrying a password, or a value under
/// a credential key name that is not an obvious placeholder.
fn secret_reason(key: &str, value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return None;
    }
    if !core_redact::recognized_secrets(value).is_empty() {
        return Some("recognized secret pattern");
    }
    if core_redact::is_token_shaped(value) {
        return Some("token-shaped value");
    }
    if url_password(value) {
        return Some("URL with a password");
    }
    let credential_key = core_redact::is_sensitive_name(key) || credential_suffix(key);
    if credential_key && !placeholder(value) {
        return Some("value under a credential key");
    }
    None
}

/// Whether `value` is a `scheme://user:password@host` URL.
fn url_password(value: &str) -> bool {
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    authority.rsplit_once('@').is_some_and(|(userinfo, _)| {
        userinfo
            .split_once(':')
            .is_some_and(|(_, pass)| !pass.is_empty())
    })
}

/// Whether `key` ends in a credential word (`DB_PASSWORD`, `APP_SECRET`,
/// `GITHUB_TOKEN`, `STRIPE_API_KEY`), whatever the value's length.
fn credential_suffix(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "pwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "private_key",
        "secret_key",
        "access_key",
        "credentials",
    ]
    .iter()
    .any(|word| lower.ends_with(word))
}

/// Whether a credential value is an explicit template placeholder rather
/// than a secret: a reference (`${VAR}`, `<your-key>`), one repeated
/// character (`xxxx`), a whole placeholder word (`changeme`), or a
/// `your-...` / `your_...` fill-in. A value merely containing such a word
/// (`mydummyPass!42`) is not one.
fn placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    (value.starts_with("${") && value.ends_with('}'))
        || (value.starts_with('<') && value.ends_with('>'))
        || value.chars().all(|c| value.starts_with(c))
        || lower.starts_with("your-")
        || lower.starts_with("your_")
        || [
            "changeme",
            "change-me",
            "change_me",
            "example",
            "placeholder",
            "replaceme",
            "replace-me",
            "replace_me",
            "todo",
            "dummy",
            "fixme",
        ]
        .contains(&lower.as_str())
}

fn prov(
    id: &SourceId,
    path: &str,
    span: (u64, u64),
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let p = Provenance::new(
        Tier::Deterministic,
        confidence,
        vec![EvidenceRef {
            repo: id.repo.into(),
            path: path.into(),
            byte_start: span.0,
            byte_end: span.1,
            commit_sha: id.commit.into(),
        }],
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic tier admits every confidence tier at or below Confirmed");
    serde_json::to_value(p).expect("provenance serializes")
}

/// Graph endpoint a site's edge starts from: its enclosing symbol, or the
/// file itself for top-level sites (repo-namespaced like every file id).
fn site_source(repo: &str, symbol: &Option<String>, path: &str) -> String {
    symbol
        .clone()
        .unwrap_or_else(|| format!("file:{repo}@{path}"))
}

/// Record a site's qualifier components and batch shape (#573) on a node or
/// edge's props, only when present so other kinds' facts are unchanged.
fn add_site_qualifiers(props: &mut serde_json::Value, site: &EventSite) {
    let Some(map) = props.as_object_mut() else {
        return;
    };
    if !site.components.is_empty() {
        map.insert("components".into(), serde_json::json!(site.components));
    }
    if let Some(batch) = &site.batch {
        map.insert("batch".into(), batch.as_str().into());
    }
}

/// Record the registry files that shaped the site (#594): the recognizing
/// SDK file's version as `registry`, as before, and every contributing
/// file's full identity — name, schema, version, content hash — as
/// `registry_sources`, recognizing file first. Sites no registry entry
/// produced carry neither.
fn add_site_registry(props: &mut serde_json::Value, site: &EventSite) {
    let (Some(map), Some(first)) = (props.as_object_mut(), site.registries.first()) else {
        return;
    };
    map.insert("registry".into(), first.version.as_str().into());
    map.insert(
        "registry_sources".into(),
        serde_json::json!(site.registries),
    );
}

/// Stitch event sites into channel facts. Deterministic: channels are
/// emitted in sorted id order, edges in site order (sites arrive sorted by
/// file from the extractor).
pub fn stitch(sites: &[EventSite], cfg: &ConfigIndex, id: &SourceId) -> Extraction {
    let mut out = Extraction::default();
    let mut channels: BTreeMap<String, Node> = BTreeMap::new();
    // Every registry file that shaped any site of a channel, by name: a
    // channel several sites resolve to names all of them, in name order.
    let mut channel_registries: BTreeMap<String, BTreeMap<String, &RegistryIdentity>> =
        BTreeMap::new();

    for site in sites {
        let edge_label = match site.role {
            ChannelRole::Produces => "PUBLISHES",
            ChannelRole::Consumes => "SUBSCRIBES",
        };
        let src = site_source(id.repo, &site.symbol, &site.path);

        // Resolve the identity at T0: literal as-is (AC-0010), env ref via
        // the config index (AC-0011).
        let resolved: Option<(String, String)> = match &site.identity {
            IdentityExpr::Literal(s) => Some((s.clone(), "literal".into())),
            IdentityExpr::EnvRef(key) => cfg
                .resolve(key)
                .map(|(value, file)| (value.to_string(), format!("config:{file}"))),
            IdentityExpr::Computed(_) => None,
        };

        match (resolved, &site.identity) {
            (Some((identity, resolver)), _) => {
                let chan_id = format!("chan:{}:{}", site.kind, identity);
                let contributors = channel_registries.entry(chan_id.clone()).or_default();
                for registry in &site.registries {
                    contributors.insert(registry.name.clone(), registry);
                }
                channels.entry(chan_id.clone()).or_insert_with(|| Node {
                    id: chan_id.clone(),
                    label: "Channel".into(),
                    props: serde_json::json!({
                        "kind": site.kind,
                        "identity": identity,
                        "prov": prov(
                            id, &site.path, (site.byte_start, site.byte_end),
                            ConfidenceTier::Confirmed, &format!("Channel {chan_id}"),
                        ),
                    }),
                });
                let mut props = serde_json::json!({
                    "resolver": resolver,
                    "prov": prov(
                        id, &site.path, (site.byte_start, site.byte_end),
                        ConfidenceTier::Confirmed, &format!("{edge_label} -> {chan_id}"),
                    ),
                });
                add_site_qualifiers(&mut props, site);
                add_site_registry(&mut props, site);
                out.edges.push(Edge {
                    src,
                    dst: chan_id.clone(),
                    label: edge_label.into(),
                    props,
                });
            }
            (None, identity) => {
                // Unresolved at T0. No higher rung exists yet (T1 at M6,
                // T2 at M7), so the ladder bottoms out here: an explicit
                // Gap node truncates the hop with a reason (AC-0012,
                // R-INT-4) — the site is never silently dropped.
                let (reason, raw) = match identity {
                    IdentityExpr::EnvRef(key) => (
                        format!("env key {key} not found in any env file in the repo"),
                        format!("process.env.{key}"),
                    ),
                    IdentityExpr::Computed(raw) => {
                        ("runtime-computed channel identity".to_string(), raw.clone())
                    }
                    IdentityExpr::Literal(_) => unreachable!("literals always resolve"),
                };
                let gap_id = format!("gap:chan:{}@{}@{}", id.repo, site.path, site.byte_start);
                let mut props = serde_json::json!({
                    "reason": &reason,
                    "raw": raw,
                    "kind": site.kind,
                    "attempted_tiers": ["T0"],
                    "prov": prov(
                        id, &site.path, (site.byte_start, site.byte_end),
                        ConfidenceTier::Gap, &format!("Gap {gap_id}"),
                    ),
                });
                add_site_qualifiers(&mut props, site);
                if let (Some(map), false) = (props.as_object_mut(), site.registries.is_empty()) {
                    map.insert(
                        "registry_sources".into(),
                        serde_json::json!(site.registries),
                    );
                }
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props,
                });
                let mut props = serde_json::json!({
                    "reason": reason,
                    "prov": prov(
                        id, &site.path, (site.byte_start, site.byte_end),
                        ConfidenceTier::Gap, &format!("{edge_label} -> {gap_id}"),
                    ),
                });
                add_site_registry(&mut props, site);
                out.edges.push(Edge {
                    src,
                    dst: gap_id.clone(),
                    label: edge_label.into(),
                    props,
                });
            }
        }
    }

    for (chan_id, contributors) in channel_registries {
        if contributors.is_empty() {
            continue;
        }
        if let Some(map) = channels
            .get_mut(&chan_id)
            .and_then(|node| node.props.as_object_mut())
        {
            let sources: Vec<_> = contributors.into_values().collect();
            map.insert("registry_sources".into(), serde_json::json!(sources));
        }
    }
    out.nodes.extend(channels.into_values());
    out
}

/// Path portion of a fetch URL: query/fragment stripped, scheme+host
/// stripped when present (`https://api.example/users?x=1` → `/users`).
fn url_path(url: &str) -> String {
    let u = url.split(['?', '#']).next().unwrap_or(url);
    if let Some(i) = u.find("://") {
        let rest = &u[i + 3..];
        match rest.find('/') {
            Some(j) => rest[j..].to_string(),
            None => "/".to_string(),
        }
    } else {
        u.to_string()
    }
}

/// How a prefix endpoint's route matches a request path (#1329), as its
/// producer declares it in `prefix_kind` beside `match: "prefix"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefixKind {
    /// node-http `startsWith`: the raw string before `*` prefixes the path.
    Raw,
    /// Next `[...x]`: the segments before `/*`, then one or more segments.
    SegmentsPlus,
    /// Next `[[...x]]`: the segments before `/*`, then zero or more.
    SegmentsStar,
    /// No known kind, or producers disagree on one id: matches nothing, so
    /// its semantics are never guessed.
    Unsupported,
}

/// The [`PrefixKind`] of every prefix Endpoint among `nodes`. One without a
/// known kind, or whose id carries differing kinds, is
/// [`PrefixKind::Unsupported`].
pub fn prefix_kinds(nodes: &[Node]) -> BTreeMap<String, PrefixKind> {
    let mut kinds = BTreeMap::new();
    for n in nodes
        .iter()
        .filter(|n| n.label == "Endpoint" && n.props["match"] == "prefix")
    {
        let kind = match n.props["prefix_kind"].as_str() {
            Some("raw") => PrefixKind::Raw,
            Some("segments+") => PrefixKind::SegmentsPlus,
            Some("segments*") => PrefixKind::SegmentsStar,
            _ => PrefixKind::Unsupported,
        };
        kinds
            .entry(n.id.clone())
            .and_modify(|seen: &mut PrefixKind| {
                if *seen != kind {
                    *seen = PrefixKind::Unsupported;
                }
            })
            .or_insert(kind);
    }
    kinds
}

/// True when a concrete request path matches an endpoint route of `kind`
/// (`None` for a whole-path pattern, [`route_matches`]).
fn route_matches_kind(kind: Option<PrefixKind>, pattern: &str, concrete: &str) -> bool {
    let min = match kind {
        None => return route_matches(pattern, concrete),
        Some(PrefixKind::Raw) => {
            return pattern
                .strip_suffix('*')
                .is_some_and(|raw| concrete.starts_with(raw));
        }
        Some(PrefixKind::SegmentsPlus) => 1,
        Some(PrefixKind::SegmentsStar) => 0,
        Some(PrefixKind::Unsupported) => return false,
    };
    let Some(base) = pattern.strip_suffix("/*") else {
        return false;
    };
    let head = base.split('/').count();
    let cs: Vec<&str> = concrete.split('/').collect();
    if cs.len() < head {
        return false;
    }
    let rest = &cs[head..];
    // A trailing slash adds no segment.
    let rest = rest.strip_suffix(&[""]).unwrap_or(rest);
    route_matches(base, &cs[..head].join("/"))
        && rest.len() >= min
        && rest.iter().all(|segment| !segment.is_empty())
}

/// True when a concrete request path matches an endpoint's route pattern:
/// exact segments, or parameter segments matching any single segment. A
/// parameter is written `:id` (Express), `<id>`/`<int:id>` (Flask) or
/// `{id}` (FastAPI, Spring).
fn route_matches(pattern: &str, concrete: &str) -> bool {
    let param = |p: &str| {
        p.starts_with(':')
            || (p.len() > 2 && p.starts_with('<') && p.ends_with('>'))
            || (p.len() > 2 && p.starts_with('{') && p.ends_with('}'))
    };
    let ps: Vec<&str> = pattern.split('/').collect();
    let cs: Vec<&str> = concrete.split('/').collect();
    ps.len() == cs.len()
        && ps
            .iter()
            .zip(&cs)
            .all(|(p, c)| p == c || (param(p) && !c.is_empty()))
}

/// The request URL of a fetch site and how it resolved, or why it did not.
/// A joined site (#737) resolves its base like any identity, a runtime
/// expression only through a system-manifest declaration, and joins its
/// literal path to it.
fn resolve_site_url(
    site: &adapters_fw::client::FetchSite,
    cfg: &ConfigIndex,
) -> (Option<String>, Option<String>, Option<String>) {
    let entry = site.entry.as_deref().unwrap_or("client call");
    let classify = |expr: &IdentityExpr, joined: bool| -> Result<(String, String), String> {
        match expr {
            IdentityExpr::Literal(url) => Ok((url.clone(), "literal".into())),
            IdentityExpr::EnvRef(key) => cfg
                .resolve(key)
                .map(|(value, file)| (value.to_string(), format!("config:{file}")))
                .ok_or_else(|| match joined {
                    true => format!("env key {key} (base URL of {entry}) not found in any env file in the repo"),
                    false => format!("env key {key} not found in any env file in the repo"),
                }),
            IdentityExpr::Computed(text) if joined => cfg
                .resolve_declared(text)
                .map(|(value, file)| (value.to_string(), format!("config:{file}")))
                .ok_or_else(|| {
                    format!("runtime base URL `{text}` of {entry} is not declared in the system manifest")
                }),
            IdentityExpr::Computed(_) => Err("runtime-computed fetch URL".into()),
        }
    };
    let Some(base) = &site.base else {
        return match classify(&site.url, false) {
            Ok((url, source)) => (Some(url), Some(source), None),
            Err(reason) => (None, None, Some(reason)),
        };
    };
    let IdentityExpr::Literal(path) = &site.url else {
        return (
            None,
            None,
            Some(format!("runtime-computed path of {entry}")),
        );
    };
    match classify(&base.url, true) {
        Ok((url, source)) => (
            Some(adapters_fw::client::join_base(&url, path, base.join)),
            Some(format!("base:{source}")),
            None,
        ),
        Err(reason) => (None, None, Some(reason)),
    }
}

/// The base URL of a joined site whose path is computed (#2065): its
/// origin is known from source although no endpoint can be matched, so
/// the Gap keeps it as the request's URL for the boundary to place.
fn computed_path_base(
    site: &adapters_fw::client::FetchSite,
    cfg: &ConfigIndex,
) -> Option<(String, String)> {
    let base = site.base.as_ref()?;
    if !matches!(site.url, IdentityExpr::Computed(_)) {
        return None;
    }
    match &base.url {
        IdentityExpr::Literal(url) => Some((url.clone(), "base:literal".into())),
        IdentityExpr::EnvRef(key) => cfg
            .resolve(key)
            .map(|(value, file)| (value.to_string(), format!("base:config:{file}"))),
        IdentityExpr::Computed(_) => None,
    }
}

/// Stitch data-fetch sites against recovered endpoints (US-0005, AC-0014):
/// a resolvable URL whose method+path matches exactly one endpoint route is
/// a Confirmed `FETCHES` edge; anything else — computed URL, unresolved env
/// key, no match, ambiguous match — emits an explicit `Gap` with a reason
/// (the ladder above T0 is empty until M6/M7; never silently dropped).
pub fn stitch_fetches(
    sites: &[adapters_fw::client::FetchSite],
    endpoint_ids: &[String],
    cfg: &ConfigIndex,
    id: &SourceId,
) -> Extraction {
    stitch_fetches_with(
        sites,
        endpoint_ids,
        &BTreeSet::new(),
        &BTreeMap::new(),
        cfg,
        id,
    )
}

/// Property of a `FETCHES` relation, and of a fetch Gap awaiting a system
/// join: whether the call is straight-line in its source Symbol's body
/// (`adapters_fw::client::FetchSite::straight_line`, #1012). Absent when
/// the adapter does not determine it.
pub const STRAIGHT_LINE: &str = "straight_line";

/// [`stitch_fetches`], where each id in `get_serves_head` is a GET route
/// its framework also serves for HEAD (Go's `ServeMux`, AC-0434), and
/// `prefix_kinds` ([`prefix_kinds`]) says how each prefix route matches.
pub fn stitch_fetches_with(
    sites: &[adapters_fw::client::FetchSite],
    endpoint_ids: &[String],
    get_serves_head: &BTreeSet<String>,
    prefix_kinds: &BTreeMap<String, PrefixKind>,
    cfg: &ConfigIndex,
    id: &SourceId,
) -> Extraction {
    // ep:{repo}@VERB:route (repo-namespaced), parsed once. Fetch sites only
    // match endpoints of their own repo at T0; cross-repo is M5 manifest work.
    let ns = format!("{}@", id.repo);
    let endpoints: Vec<(&str, &str, &str)> = endpoint_ids
        .iter()
        .filter_map(|eid| {
            let rest = eid.strip_prefix("ep:")?.strip_prefix(&ns)?;
            let (verb, route) = rest.split_once(':')?;
            Some((eid.as_str(), verb, route))
        })
        .collect();

    let mut out = Extraction::default();
    for site in sites {
        let src = site_source(id.repo, &site.symbol, &site.path);
        let fetch_prov = |confidence: ConfidenceTier, fact: &str| {
            prov(
                id,
                &site.path,
                (site.byte_start, site.byte_end),
                confidence,
                fact,
            )
        };

        let (resolved_url, url_source, unresolved) = resolve_site_url(site, cfg);
        // A joined call cites its client entry and the join that built it.
        let join = site.base.as_ref().map(|base| {
            serde_json::json!({
                "entry": site.entry,
                "path": base.path,
                "byte_start": base.byte_start,
            })
        });
        let resolved: Option<(String, String)> = resolved_url
            .as_deref()
            .zip(url_source.clone())
            .map(|(url, source)| (url_path(url), source));

        let deferred = resolved_url
            .as_deref()
            .and_then(http_binding::url_origin)
            .filter(|origin| cfg.deferred_origins.contains(origin));
        let outcome: Result<(&str, String), String> = match (&resolved, site.method.as_str()) {
            (Some(_), method) if method != "?" && deferred.is_some() => Err(format!(
                "{} {} is bound in the system manifest",
                http_binding::DEFERRED_REASON_PREFIX,
                deferred.as_deref().unwrap_or_default()
            )),
            (None, _) => Err(unresolved.unwrap_or_else(|| "runtime-computed fetch URL".into())),
            (Some(_), "?") => Err("runtime-computed HTTP method".to_string()),
            (Some((path, resolver)), method) => {
                let hits: Vec<&str> = endpoints
                    .iter()
                    // A method-less route (`ANY`, or Express-style `ALL`,
                    // #1327) serves every method.
                    .filter(|(eid, verb, route)| {
                        (*verb == method
                            || *verb == "ANY"
                            || *verb == "ALL"
                            || (method == "HEAD" && get_serves_head.contains(*eid)))
                            && route_matches_kind(prefix_kinds.get(*eid).copied(), route, path)
                    })
                    .map(|(eid, _, _)| *eid)
                    .collect();
                match hits.as_slice() {
                    [one] => Ok((one, resolver.clone())),
                    [] => Err(format!("no recovered endpoint matches {method} {path}")),
                    many => Err(format!(
                        "ambiguous endpoint match for {method} {path} ({} candidates)",
                        many.len()
                    )),
                }
            }
        };

        match outcome {
            Ok((ep_id, resolver)) => {
                let mut props = serde_json::json!({
                    "method": site.method,
                    "resolver": resolver,
                    "prov": fetch_prov(
                        ConfidenceTier::Confirmed,
                        &format!("FETCHES -> {ep_id}"),
                    ),
                });
                if let Some(join) = &join {
                    props["join"] = join.clone();
                }
                if let Some(straight_line) = site.straight_line {
                    props[STRAIGHT_LINE] = straight_line.into();
                }
                out.edges.push(Edge {
                    src,
                    dst: ep_id.to_string(),
                    label: "FETCHES".into(),
                    props,
                });
            }
            Err(reason) => {
                let gap_id = format!("gap:fetch:{}@{}@{}", id.repo, site.path, site.byte_start);
                let mut props = serde_json::json!({
                    "reason": &reason,
                    "attempted_tiers": ["T0"],
                    "prov": fetch_prov(ConfidenceTier::Gap, &format!("Gap {gap_id}")),
                });
                if let Some(join) = &join {
                    props["join"] = join.clone();
                }
                // Kept on the Gap so a system-wide join carries it (#1012).
                if let Some(straight_line) = site.straight_line {
                    props[STRAIGHT_LINE] = straight_line.into();
                }
                // The resolved request, for system-wide binding (#589).
                if let (Some(url), Some(source)) = (resolved_url.as_ref(), url_source.as_ref()) {
                    props["request"] = serde_json::json!({
                        "method": site.method,
                        "url": url,
                        "url_source": source,
                    });
                } else if let Some((base, source)) = computed_path_base(site, cfg) {
                    // Only the base is known: the path is computed (#2065).
                    props["request"] = serde_json::json!({
                        "method": site.method,
                        "url": base,
                        "url_source": source,
                        "path_computed": true,
                    });
                }
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props,
                });
                out.edges.push(Edge {
                    src,
                    dst: gap_id.clone(),
                    label: "FETCHES".into(),
                    props: serde_json::json!({
                        "reason": reason,
                        "method": site.method,
                        "prov": fetch_prov(ConfidenceTier::Gap, &format!("FETCHES -> {gap_id}")),
                    }),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
