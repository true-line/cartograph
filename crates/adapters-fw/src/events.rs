//! Event SDK signature registry (SPEC-00 §3.4, US-0004): versioned
//! deterministic knowledge mapping producer/consumer SDK call shapes to
//! channel semantics. Like the HTTP registry, this is data, not inference —
//! a language adapter consults it while walking the AST and emits
//! [`EventSite`] facts; the `events` crate resolves identities and stitches
//! the channel graph.
//!
//! The entries live in versioned data files (#594, AC-0255), never code:
//! `registries/event-sdks.json` and `registries/channel-backings.json` ship
//! embedded as the built-in layer, and [`Registries::layered`] adds
//! extension files on top. Every file is validated strictly and as a whole;
//! a file that fails is rejected entirely (fail closed), so the facts
//! recovered with it are exactly the facts recovered without it. Each
//! entry carries the identity of the file that declared it, and that
//! identity lands in the provenance of every fact the entry produces.

pub mod logical;

use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Whether a matched call site produces to or consumes from a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRole {
    /// Sends/publishes/emits onto the channel.
    Produces,
    /// Subscribes to / handles messages from the channel.
    Consumes,
}

/// Where the channel identity lives in the matched call's arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum IdentityArg {
    /// The call's first argument is the identity (`emit('order.placed')`).
    First,
    /// The identity is the value of this key anywhere inside the first
    /// object argument (`{ QueueUrl: … }`, `{ Entries: [{ DetailType: … }] }`).
    Key(String),
}

/// How a call site is recognized as belonging to the SDK — the receiver
/// proof, mirroring the HTTP registry's factory tracking (never guessed
/// from variable names).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SdkPattern {
    /// `new <Ctor>({ … })` where `<Ctor>` is imported from `module`
    /// (AWS SDK v3 command style: the command object carries the identity).
    Constructor {
        /// npm module the constructor must be imported from.
        module: String,
        /// Constructor name (e.g. `SendMessageCommand`).
        ctor: String,
    },
    /// `recv.<method>(…)` where `recv` is a variable bound to
    /// `new <Ctor>(…)` and `<Ctor>` is imported from `module`
    /// (AWS SDK v2 / EventEmitter style).
    Method {
        /// npm module the receiver's constructor must be imported from.
        module: String,
        /// Constructor that creates the receiver (e.g. `AWS.SQS`,
        /// `EventEmitter`).
        ctor: String,
        /// Method name on the receiver (e.g. `sendMessage`, `emit`).
        method: String,
    },
    /// `recv.<method>(…)` where `recv` is bound to a factory *member call*
    /// on an SDK object (`kafka.producer()`), gated on the file importing
    /// `module` (kafkajs style).
    FactoryMethod {
        /// npm module whose import gates the match.
        module: String,
        /// Factory member name that creates the receiver (`producer`).
        factory: String,
        /// Method name on the receiver (`send`).
        method: String,
    },
}

impl SdkPattern {
    fn names(&self) -> Vec<&str> {
        match self {
            SdkPattern::Constructor { module, ctor } => vec![module, ctor],
            SdkPattern::Method {
                module,
                ctor,
                method,
            } => vec![module, ctor, method],
            SdkPattern::FactoryMethod {
                module,
                factory,
                method,
            } => vec![module, factory, method],
        }
    }
}

/// The registry data file an entry came from, as recorded in provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryIdentity {
    /// `builtin:<file>` or `extension:<file name>`.
    pub name: String,
    /// The file's declared schema.
    pub schema: String,
    /// The file's declared version.
    pub version: String,
    /// Content hash of the file's exact bytes (`core_prov::content_hash`).
    pub content_hash: String,
}

/// One registry entry: an SDK call shape that produces or consumes.
#[derive(Debug, Clone)]
pub struct EventSdk {
    /// Channel kind this entry establishes (`sqs-queue`, `kafka-topic`, …).
    pub kind: String,
    /// Producer or consumer.
    pub role: ChannelRole,
    /// How the call site is recognized (receiver proof included).
    pub pattern: SdkPattern,
    /// Where the channel identity lives. For a `pypi` entry, `First` is the
    /// first positional argument and `Key` a keyword argument.
    pub identity: IdentityArg,
    /// Ecosystem the module belongs to (#1343): npm unless declared, in a
    /// `@2` file. Each adapter reads only its own ecosystem's entries.
    pub ecosystem: Ecosystem,
    /// For a `pypi` method entry, the literal first argument the receiver's
    /// factory call must carry (`boto3.client("sqs")`).
    pub select: Option<ClientSelect>,
    /// The file that declared this entry.
    pub registry: RegistryIdentity,
}

/// A secondary identity component an SDK call can prove from a literal
/// argument, alongside the registry entry's primary identity (#573: an
/// EventBridge channel is a detail type on a bus). Only literal-proven
/// values are recorded; anything else stays unknown, never a wildcard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQualifier {
    /// Channel kind of the registry entries the qualifier applies to.
    pub kind: String,
    /// Component name recorded on the site (`bus`).
    pub component: String,
    /// Key holding the value anywhere inside the first object argument.
    pub key: String,
    /// The file that declared this entry.
    pub registry: RegistryIdentity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QualifierEntry {
    kind: String,
    component: String,
    key: String,
}

/// A key whose array value lists several entries in one call
/// (`PutEvents({ Entries: [...] })`). A site with more than one entry, or
/// a non-literal entry list, is marked as a batch: its first entry's
/// identity cannot stand for the others, and trace observations cannot be
/// correlated per entry (#573).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchKey {
    /// Channel kind of the registry entries the key applies to.
    pub kind: String,
    /// Key holding the entry list.
    pub key: String,
    /// The file that declared this entry.
    pub registry: RegistryIdentity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchEntry {
    kind: String,
    key: String,
}

/// A deployed resource type whose observed attribute names a code-layer
/// channel. Kinds must be established by an event SDK entry — that
/// equality is what makes the `BACKS` join deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelBacking {
    /// Terraform resource type (`aws_sqs_queue`).
    pub resource_type: String,
    /// Channel kind it backs (`sqs-queue`).
    pub kind: String,
    /// Observed attribute carrying the channel identity (`url`).
    pub attribute: String,
    /// The file that declared this entry.
    pub registry: RegistryIdentity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackingEntry {
    resource_type: String,
    kind: String,
    attribute: String,
}

/// The infrastructure family an observed client belongs to (#734).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservedFamily {
    /// Client-side storage (IndexedDB wrappers, Web Storage, Cache API).
    BrowserStorage,
    /// A cloud or service SDK client (AWS SDK v3, Stripe, MongoDB, …).
    SdkClient,
    /// An identity-provider client (Auth0, Clerk, NextAuth, Cognito).
    AuthProvider,
    /// An HTTP API the code calls, one resource per origin (`fetch`,
    /// axios, ky with a literal or const-resolved base URL).
    HttpApi,
}

impl ObservedFamily {
    /// Wire spelling, recorded as the resource's `kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            ObservedFamily::BrowserStorage => "browser-storage",
            ObservedFamily::SdkClient => "sdk-client",
            ObservedFamily::AuthProvider => "auth-provider",
            ObservedFamily::HttpApi => "http-api",
        }
    }
}

/// How an observed client is created from its imported binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientForm {
    /// `new <Export>(…)`.
    New,
    /// `<Export>(…)`, a factory function.
    Call,
}

/// The package ecosystem an observed client's module belongs to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ecosystem {
    /// An npm module, matched by the TypeScript adapter.
    #[default]
    Npm,
    /// A Python module, matched by the Python adapter.
    Pypi,
}

/// A literal first argument a client call must carry to match, for a
/// factory that names its service (`boto3.client("s3")`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSelect {
    /// The exact string literal the call's first argument must be.
    pub first: String,
}

/// A client constructor or factory whose call site is observed
/// infrastructure when the code has no IaC for it (#734): the call proves
/// the code talks to `provider`, and `identity`, when declared, names
/// which instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedClient {
    /// Infrastructure family.
    pub family: ObservedFamily,
    /// Provider recorded as the resource type (`aws-s3`, `stripe`).
    pub provider: String,
    /// Ecosystem the module belongs to (npm unless declared).
    pub ecosystem: Ecosystem,
    /// Module the binding must be imported (or required) from: an npm
    /// module, or a dotted Python module for `pypi`.
    pub module: String,
    /// Exported name, or `default` for the module's default export.
    pub export: String,
    /// Constructor or factory call.
    pub form: ClientForm,
    /// Where the instance identity lives, if the client declares one.
    pub identity: Option<IdentityArg>,
    /// For a `pypi` client whose identity is the first argument: that
    /// parameter's name, so a call passing it by keyword (`uri=…`) names
    /// the instance too.
    pub keyword: Option<String>,
    /// The literal first argument the call must carry, if any.
    pub select: Option<ClientSelect>,
    /// The file that declared this entry.
    pub registry: RegistryIdentity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientEntry {
    family: ObservedFamily,
    provider: String,
    #[serde(default)]
    ecosystem: Ecosystem,
    module: String,
    export: String,
    form: ClientForm,
    #[serde(default)]
    identity: Option<IdentityArg>,
    #[serde(default)]
    keyword: Option<String>,
    #[serde(default)]
    select: Option<ClientSelect>,
}

/// A registry data file that was rejected whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryRejection {
    /// The rejected file's name.
    pub name: String,
    /// Why it was rejected.
    pub reason: String,
}

/// Largest registry data file accepted, checked before parsing.
pub const MAX_REGISTRY_BYTES: usize = 64 * 1024;
/// Most extension files one layering accepts.
pub const MAX_EXTENSION_FILES: usize = 32;
const MAX_ENTRIES: usize = 256;
const MAX_VERSION_BYTES: usize = 64;
const SDK_SCHEMA: &str = "cartograph-event-sdk-registry@1";
/// `@1` plus per-entry `ecosystem` and `select` (#1343). An `@1` file may
/// not carry them, so its entries keep their npm meaning, and a reader that
/// predates `@2` rejects a `@2` file whole rather than misreading it.
const SDK_SCHEMA_V2: &str = "cartograph-event-sdk-registry@2";
const BACKING_SCHEMA: &str = "cartograph-channel-backing-registry@1";
const CLIENT_SCHEMA: &str = "cartograph-observed-client-registry@1";
const BUILTIN_SDKS: &str = include_str!("../registries/event-sdks.json");
const BUILTIN_PYPI_SDKS: &str = include_str!("../registries/event-sdks-pypi.json");
const BUILTIN_BACKINGS: &str = include_str!("../registries/channel-backings.json");
const BUILTIN_CLIENTS: &str = include_str!("../registries/observed-clients.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SdkEntry {
    kind: String,
    role: ChannelRole,
    pattern: SdkPattern,
    identity: IdentityArg,
    #[serde(default)]
    ecosystem: Option<Ecosystem>,
    #[serde(default)]
    select: Option<ClientSelect>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    schema: String,
    version: String,
    #[serde(default)]
    sdks: Vec<SdkEntry>,
    #[serde(default)]
    qualifiers: Vec<QualifierEntry>,
    #[serde(default)]
    batch_keys: Vec<BatchEntry>,
    #[serde(default)]
    backings: Vec<BackingEntry>,
    #[serde(default)]
    clients: Vec<ClientEntry>,
}

/// The effective event registries: every accepted file's entries, in
/// layering order, and the files that were rejected.
#[derive(Debug, Clone, Default)]
pub struct Registries {
    /// Event SDK call shapes.
    pub sdks: Vec<EventSdk>,
    /// Qualifier components, per channel kind.
    pub qualifiers: Vec<EventQualifier>,
    /// Batch keys, per channel kind.
    pub batch_keys: Vec<BatchKey>,
    /// Resource types backing channels.
    pub backings: Vec<ChannelBacking>,
    /// Client constructors observed as infrastructure (#734).
    pub clients: Vec<ObservedClient>,
    /// Identities of the accepted files, in layering order.
    pub sources: Vec<RegistryIdentity>,
    /// Files rejected whole, in the order offered.
    pub rejected: Vec<RegistryRejection>,
}

static BUILTIN: LazyLock<Registries> = LazyLock::new(|| {
    let mut registries = Registries::default();
    for (name, bytes) in [
        ("builtin:event-sdks.json", BUILTIN_SDKS),
        ("builtin:event-sdks-pypi.json", BUILTIN_PYPI_SDKS),
        ("builtin:channel-backings.json", BUILTIN_BACKINGS),
        ("builtin:observed-clients.json", BUILTIN_CLIENTS),
    ] {
        // The embedded files are validated by this crate's tests; a broken
        // built-in can never ship, so failing here is a build defect.
        registries
            .admit(name, bytes.as_bytes())
            .unwrap_or_else(|reason| panic!("{name}: {reason}"));
    }
    registries
});

fn valid_kind(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_name(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_graphic())
}

fn valid_version(value: &str) -> bool {
    (1..=MAX_VERSION_BYTES).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl Registries {
    /// The built-in registries shipped with this build.
    pub fn builtin() -> &'static Registries {
        &BUILTIN
    }

    /// The built-in registries plus each extension file, in name order.
    /// A file that is malformed, oversized, of an unknown schema, or that
    /// conflicts with an entry already admitted is rejected whole and adds
    /// nothing; the rest still apply.
    pub fn layered(extensions: &[(String, Vec<u8>)]) -> Registries {
        let mut registries = Registries::builtin().clone();
        let mut files: Vec<&(String, Vec<u8>)> = extensions.iter().collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        for (index, (name, bytes)) in files.into_iter().enumerate() {
            let name = format!("extension:{name}");
            let outcome = if index >= MAX_EXTENSION_FILES {
                Err(format!("more than {MAX_EXTENSION_FILES} extension files"))
            } else {
                registries.admit(&name, bytes)
            };
            if let Err(reason) = outcome {
                registries.rejected.push(RegistryRejection { name, reason });
            }
        }
        registries
    }

    /// Validate one file against everything admitted so far and, only if
    /// every check passes, add its entries.
    fn admit(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > MAX_REGISTRY_BYTES {
            return Err(format!("larger than {MAX_REGISTRY_BYTES} bytes"));
        }
        let file: RegistryFile =
            serde_json::from_slice(bytes).map_err(|error| format!("invalid: {error}"))?;
        if !valid_version(&file.version) {
            return Err("invalid version".into());
        }
        let entries = file.sdks.len()
            + file.qualifiers.len()
            + file.batch_keys.len()
            + file.backings.len()
            + file.clients.len();
        if entries == 0 || entries > MAX_ENTRIES {
            return Err(format!("must declare 1..={MAX_ENTRIES} entries"));
        }
        let event_entries = file.sdks.len() + file.qualifiers.len() + file.batch_keys.len();
        match file.schema.as_str() {
            SDK_SCHEMA | SDK_SCHEMA_V2 if file.backings.is_empty() && file.clients.is_empty() => {}
            BACKING_SCHEMA if event_entries == 0 && file.clients.is_empty() => {}
            CLIENT_SCHEMA if event_entries == 0 && file.backings.is_empty() => {}
            SDK_SCHEMA | SDK_SCHEMA_V2 | BACKING_SCHEMA | CLIENT_SCHEMA => {
                return Err("entries outside its schema".into());
            }
            _ => return Err("unknown schema".into()),
        }
        let registry = RegistryIdentity {
            name: name.into(),
            schema: file.schema.clone(),
            version: file.version.clone(),
            content_hash: core_prov::content_hash(bytes),
        };
        let mut sdks = self.sdks.clone();
        for entry in file.sdks {
            let names_valid = entry.pattern.names().into_iter().all(valid_name)
                && match &entry.identity {
                    IdentityArg::Key(key) => valid_name(key),
                    IdentityArg::First => true,
                };
            // `ecosystem`/`select` exist only from `@2`; a `pypi` entry is a
            // module call or a method on a receiver bound to one, and only a
            // method's receiver factory has a literal selector.
            let v2 = file.schema == SDK_SCHEMA_V2;
            let ecosystem = entry.ecosystem.unwrap_or_default();
            let shape_valid = match (ecosystem, &entry.pattern) {
                (Ecosystem::Npm, _) => entry.select.is_none(),
                (Ecosystem::Pypi, SdkPattern::Constructor { .. }) => entry.select.is_none(),
                (Ecosystem::Pypi, SdkPattern::Method { .. }) => true,
                (Ecosystem::Pypi, SdkPattern::FactoryMethod { .. }) => false,
            } && (v2 || (entry.ecosystem.is_none() && entry.select.is_none()))
                && entry
                    .select
                    .as_ref()
                    .is_none_or(|select| valid_name(&select.first));
            if !valid_kind(&entry.kind) || !names_valid || !shape_valid {
                return Err(format!("invalid sdk entry for kind {:?}", entry.kind));
            }
            // An unselected method overlaps every selected one.
            if sdks.iter().any(|sdk| {
                sdk.pattern == entry.pattern
                    && sdk.ecosystem == ecosystem
                    && (sdk.select.is_none()
                        || entry.select.is_none()
                        || sdk.select == entry.select)
            }) {
                return Err(format!(
                    "call shape {:?} is already registered",
                    entry.pattern
                ));
            }
            sdks.push(EventSdk {
                kind: entry.kind,
                role: entry.role,
                pattern: entry.pattern,
                identity: entry.identity,
                ecosystem,
                select: entry.select,
                registry: registry.clone(),
            });
        }
        let established = |kind: &str| sdks.iter().any(|sdk| sdk.kind == kind);
        let mut qualifiers = self.qualifiers.clone();
        for qualifier in file.qualifiers {
            if !established(&qualifier.kind)
                || !valid_kind(&qualifier.component)
                || !valid_name(&qualifier.key)
                || qualifiers
                    .iter()
                    .any(|q| q.kind == qualifier.kind && q.component == qualifier.component)
            {
                return Err(format!(
                    "invalid or duplicate qualifier for {:?}",
                    qualifier.kind
                ));
            }
            qualifiers.push(EventQualifier {
                kind: qualifier.kind,
                component: qualifier.component,
                key: qualifier.key,
                registry: registry.clone(),
            });
        }
        let mut batch_keys = self.batch_keys.clone();
        for batch in file.batch_keys {
            if !established(&batch.kind)
                || !valid_name(&batch.key)
                || batch_keys.iter().any(|b| b.kind == batch.kind)
            {
                return Err(format!(
                    "invalid or duplicate batch key for {:?}",
                    batch.kind
                ));
            }
            batch_keys.push(BatchKey {
                kind: batch.kind,
                key: batch.key,
                registry: registry.clone(),
            });
        }
        let mut backings = self.backings.clone();
        for backing in file.backings {
            if !established(&backing.kind)
                || !valid_name(&backing.resource_type)
                || !valid_name(&backing.attribute)
                || backings
                    .iter()
                    .any(|b| b.resource_type == backing.resource_type)
            {
                return Err(format!(
                    "invalid or duplicate backing for {:?}",
                    backing.resource_type
                ));
            }
            backings.push(ChannelBacking {
                resource_type: backing.resource_type,
                kind: backing.kind,
                attribute: backing.attribute,
                registry: registry.clone(),
            });
        }
        let mut clients = self.clients.clone();
        for client in file.clients {
            let identity_valid = match &client.identity {
                Some(IdentityArg::Key(key)) => valid_name(key),
                Some(IdentityArg::First) | None => true,
            };
            // A keyword spelling only exists for a Python first argument.
            let keyword_valid = client.keyword.as_ref().is_none_or(|keyword| {
                client.ecosystem == Ecosystem::Pypi
                    && client.identity == Some(IdentityArg::First)
                    && valid_name(keyword)
            });
            let select_valid = client
                .select
                .as_ref()
                .is_none_or(|select| valid_name(&select.first));
            if !valid_kind(&client.provider)
                || !valid_name(&client.module)
                || !valid_name(&client.export)
                || !identity_valid
                || !keyword_valid
                || !select_valid
            {
                return Err(format!("invalid client entry for {:?}", client.provider));
            }
            if clients.iter().any(|c: &ObservedClient| {
                c.ecosystem == client.ecosystem
                    && c.module == client.module
                    && c.export == client.export
                    && c.form == client.form
                    && c.select == client.select
            }) {
                return Err(format!(
                    "client {:?} of {:?} is already registered",
                    client.export, client.module
                ));
            }
            clients.push(ObservedClient {
                family: client.family,
                provider: client.provider,
                ecosystem: client.ecosystem,
                module: client.module,
                export: client.export,
                form: client.form,
                identity: client.identity,
                keyword: client.keyword,
                select: client.select,
                registry: registry.clone(),
            });
        }
        self.sources.push(registry);
        self.clients = clients;
        self.sdks = sdks;
        self.qualifiers = qualifiers;
        self.batch_keys = batch_keys;
        self.backings = backings;
        Ok(())
    }

    /// Identity of the effective registry set: every accepted file, in
    /// layering order. Bind caches and coverage to this, never to a
    /// version string alone.
    pub fn identity(&self) -> String {
        // Structured, not concatenated: a file name may contain any
        // separator, so only an unambiguous encoding names one set.
        let parts: Vec<(&str, &str)> = self
            .sources
            .iter()
            .map(|source| (source.name.as_str(), source.content_hash.as_str()))
            .collect();
        let canonical = serde_json::to_vec(&parts).expect("pairs of strings serialize");
        core_prov::content_hash(&canonical)
    }
}

/// How a site's channel-identity expression was classified at T0
/// (US-0004: literal, config-resolvable, or runtime-computed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityExpr {
    /// A string literal (or a local `const` bound to one) — AC-0010.
    Literal(String),
    /// `process.env.<KEY>` — resolvable from a present env file (AC-0011).
    EnvRef(String),
    /// Anything else: runtime-computed; escalates, Gap at T0 (AC-0012).
    /// Carries the raw source text as the Gap reason's evidence.
    Computed(String),
}

/// A producer/consumer call site emitted by a language adapter — the
/// language-independent fact the `events` crate stitches into the graph.
#[derive(Debug, Clone)]
pub struct EventSite {
    /// Channel kind from the registry entry (`sqs-queue`, …).
    pub kind: String,
    /// Producer or consumer.
    pub role: ChannelRole,
    /// The channel-identity expression, classified.
    pub identity: IdentityExpr,
    /// Symbol id of the enclosing function, if any (edge endpoint).
    pub symbol: Option<String>,
    /// File the site was found in (repo-relative).
    pub path: String,
    /// Byte span of the matched call.
    pub byte_start: u64,
    /// End of the matched call's byte span.
    pub byte_end: u64,
    /// Literal-proven qualifier components ([`Registries::qualifiers`]),
    /// e.g. `bus`. Absent components are unknown.
    pub components: std::collections::BTreeMap<String, String>,
    /// Set when the call carries several entries ([`Registries::batch_keys`]):
    /// `"N entries"` or `"computed entries"`.
    pub batch: Option<String>,
    /// The registry files whose entries shaped this site: the matching SDK
    /// entry's file first, then the file of each qualifier or batch key
    /// that set a component or the batch marker, in name order, each once.
    /// Empty for sites no registry entry produced (chrome runtime
    /// messaging).
    pub registries: Vec<RegistryIdentity>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, json: &str) -> (String, Vec<u8>) {
        (name.into(), json.as_bytes().to_vec())
    }

    const EXTRA_SDK: &str = r#"{"schema":"cartograph-event-sdk-registry@1","version":"x-1",
        "sdks":[{"kind":"nats-subject","role":"produces",
          "pattern":{"method":{"module":"nats","ctor":"Client","method":"publish"}},
          "identity":"first"}]}"#;

    #[test]
    fn registry_covers_the_spec_sdk_list() {
        // SPEC-00 §3.4 names the M3 shapes: sqs.sendMessage, sns.publish,
        // eventBridge.putEvents, Kafka producer.send, in-proc emit/on.
        let registries = Registries::builtin();
        let kinds: Vec<&str> = registries.sdks.iter().map(|e| e.kind.as_str()).collect();
        for expected in [
            "sqs-queue",
            "sns-topic",
            "eventbridge-detail-type",
            "kafka-topic",
            "inproc-event",
        ] {
            assert!(kinds.contains(&expected), "missing kind {expected}");
        }
        // Both roles are represented — a producer-only registry cannot stitch.
        assert!(
            registries
                .sdks
                .iter()
                .any(|e| e.role == ChannelRole::Produces)
        );
        assert!(
            registries
                .sdks
                .iter()
                .any(|e| e.role == ChannelRole::Consumes)
        );
        assert!(registries.rejected.is_empty());
        // The backings the dynamic layer joins on are all established kinds.
        for backing in &registries.backings {
            assert!(kinds.contains(&backing.kind.as_str()), "{backing:?}");
        }
        let names: Vec<&str> = registries.sources.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "builtin:event-sdks.json",
                "builtin:event-sdks-pypi.json",
                "builtin:channel-backings.json",
                "builtin:observed-clients.json"
            ]
        );
    }

    // AC-0458: event SDK entries carry an ecosystem from schema `@2` (npm
    // unless declared, the only meaning `@1` has). A `pypi` entry is a
    // module call or a selected receiver method, and its call shape is
    // distinct from an npm one of the same names. An `@1` file declaring
    // either field, an unsupported combination or a duplicate shape rejects
    // the file whole. Qualifiers and batch keys shape TypeScript
    // sites only. (T-0458)
    #[test]
    fn pypi_event_sdks_need_schema_two_and_a_supported_shape() {
        let builtin = Registries::builtin();
        let pypi: Vec<&EventSdk> = builtin
            .sdks
            .iter()
            .filter(|sdk| sdk.ecosystem == Ecosystem::Pypi)
            .collect();
        assert!(!pypi.is_empty());
        assert!(
            pypi.iter()
                .all(|sdk| sdk.registry.name == "builtin:event-sdks-pypi.json")
        );
        let sqs = pypi
            .iter()
            .find(|sdk| sdk.kind == "sqs-queue" && sdk.role == ChannelRole::Produces)
            .unwrap();
        assert_eq!(sqs.select.as_ref().unwrap().first, "sqs");
        assert!(
            builtin
                .sdks
                .iter()
                .filter(|sdk| sdk.ecosystem == Ecosystem::Npm)
                .all(|sdk| sdk.select.is_none())
        );

        let sdk = |schema: &str, entry: &str| {
            format!(
                r#"{{"schema":"cartograph-event-sdk-registry@{schema}","version":"x","sdks":[{entry}]}}"#
            )
        };
        let nats = r#"{"kind":"nats-subject","role":"produces","ecosystem":"pypi",
            "pattern":{"method":{"module":"nats","ctor":"connect","method":"publish"}},
            "identity":"first"}"#;
        let accepted = Registries::layered(&[file("a.json", &sdk("2", nats))]);
        assert!(accepted.rejected.is_empty(), "{:?}", accepted.rejected);
        let added = accepted.sdks.last().unwrap();
        assert_eq!(added.ecosystem, Ecosystem::Pypi);
        // The same names under npm are another call shape.
        let npm = nats.replace(r#""ecosystem":"pypi","#, "");
        let both = Registries::layered(&[
            file("a.json", &sdk("2", nats)),
            file("b.json", &sdk("1", &npm)),
        ]);
        assert!(both.rejected.is_empty(), "{:?}", both.rejected);

        let rejected = [
            // `@1` carries neither field.
            sdk("1", nats),
            sdk(
                "1",
                &npm.replace(r#""identity""#, r#""select":{"first":"x"},"identity""#),
            ),
            // A selector only for a pypi method; no pypi factory method.
            sdk(
                "2",
                &npm.replace(r#""identity""#, r#""select":{"first":"x"},"identity""#),
            ),
            sdk(
                "2",
                r#"{"kind":"nats-subject","role":"consumes","ecosystem":"pypi",
                "pattern":{"constructor":{"module":"nats","ctor":"Sub"}},
                "select":{"first":"x"},"identity":"first"}"#,
            ),
            sdk(
                "2",
                r#"{"kind":"nats-subject","role":"consumes","ecosystem":"pypi",
                "pattern":{"factory_method":{"module":"nats","factory":"f","method":"m"}},
                "identity":"first"}"#,
            ),
            sdk("2", &nats.replace("pypi", "cargo")),
            // A duplicate pypi shape, or one overlapping a selected shape.
            sdk(
                "2",
                r#"{"kind":"sqs-queue","role":"produces","ecosystem":"pypi",
                "pattern":{"method":{"module":"boto3","ctor":"client","method":"send_message"}},
                "identity":{"key":"QueueUrl"}}"#,
            ),
            sdk(
                "2",
                r#"{"kind":"sqs-queue","role":"produces","ecosystem":"pypi",
                "pattern":{"method":{"module":"boto3","ctor":"client","method":"send_message"}},
                "select":{"first":"sqs"},"identity":{"key":"QueueUrl"}}"#,
            ),
        ];
        for bytes in &rejected {
            let layered = Registries::layered(&[file("x.json", bytes)]);
            assert_eq!(layered.rejected.len(), 1, "{bytes}");
            assert_eq!(layered.identity(), builtin.identity(), "{bytes}");
        }
        // Qualifiers and batch keys stay keyed by kind: a pypi entry of a
        // qualified kind is accepted, and the Python reader applies neither.
        let qualifier = r#"{"schema":"cartograph-event-sdk-registry@1","version":"x",
            "qualifiers":[{"kind":"nats-subject","component":"server","key":"servers"}]}"#;
        let layered =
            Registries::layered(&[file("a.json", &sdk("2", nats)), file("b.json", qualifier)]);
        assert!(layered.rejected.is_empty(), "{:?}", layered.rejected);
    }

    // AC-0316: observed clients are registry data like event SDKs: the
    // built-in file names each family, an extension adds entries carrying
    // its identity, and a duplicate shape, an invalid provider or a client
    // entry under another schema rejects the file whole. (T-0316)
    #[test]
    fn observed_clients_layer_like_every_registry() {
        let builtin = Registries::builtin();
        for family in [
            ObservedFamily::BrowserStorage,
            ObservedFamily::SdkClient,
            ObservedFamily::AuthProvider,
            ObservedFamily::HttpApi,
        ] {
            assert!(
                builtin.clients.iter().any(|c| c.family == family),
                "{family:?}"
            );
        }
        let extra = r#"{"schema":"cartograph-observed-client-registry@1","version":"x-1",
            "clients":[{"family":"sdk-client","provider":"algolia","module":"algoliasearch",
              "export":"algoliasearch","form":"call"}]}"#;
        let layered = Registries::layered(&[file("algolia.json", extra)]);
        assert!(layered.rejected.is_empty(), "{:?}", layered.rejected);
        let added = layered.clients.last().unwrap();
        assert_eq!(added.provider, "algolia");
        assert_eq!(added.registry.name, "extension:algolia.json");
        assert_eq!(layered.clients.len(), builtin.clients.len() + 1);
        let cases = [
            (
                "duplicate.json",
                r#"{"schema":"cartograph-observed-client-registry@1","version":"x",
                "clients":[{"family":"sdk-client","provider":"payments","module":"stripe",
                  "export":"default","form":"new"}]}"#
                    .to_string(),
            ),
            (
                "bad-provider.json",
                extra.replace("algolia\"", "Algolia Search\""),
            ),
            ("bad-family.json", extra.replace("sdk-client", "queue")),
            (
                "wrong-schema.json",
                extra.replace("observed-client", "event-sdk"),
            ),
        ];
        for (name, json) in cases {
            let layered = Registries::layered(&[file(name, &json)]);
            assert_eq!(layered.rejected.len(), 1, "{name} was accepted");
            assert_eq!(layered.clients, builtin.clients, "{name}");
        }
    }

    // AC-0318: a Python client is registry data too: `pypi` entries name a
    // dotted module, and a factory that names its service (`boto3.client`)
    // is told apart by a literal first argument, so one export may carry
    // one entry per service but never two for the same one; only a Python
    // first-argument identity has a keyword spelling. (T-0318)
    #[test]
    fn python_clients_carry_their_ecosystem_and_service_selector() {
        let builtin = Registries::builtin();
        let npm = builtin
            .clients
            .iter()
            .find(|c| c.module == "stripe")
            .unwrap();
        assert_eq!(npm.ecosystem, Ecosystem::Npm);
        let s3 = builtin
            .clients
            .iter()
            .find(|c| {
                c.ecosystem == Ecosystem::Pypi
                    && c.module == "boto3"
                    && c.export == "client"
                    && c.select.as_ref().is_some_and(|s| s.first == "s3")
            })
            .unwrap();
        assert_eq!(s3.provider, "aws-s3");
        let entry = |select: &str| {
            format!(
                r#"{{"schema":"cartograph-observed-client-registry@1","version":"x",
                "clients":[{{"family":"sdk-client","provider":"aws-ec2","ecosystem":"pypi",
                  "module":"boto3","export":"client","form":"call"{select}}}]}}"#
            )
        };
        let ec2 = Registries::layered(&[file("ec2.json", &entry(r#","select":{"first":"ec2"}"#))]);
        assert!(ec2.rejected.is_empty(), "{:?}", ec2.rejected);
        assert_eq!(ec2.clients.len(), builtin.clients.len() + 1);
        // A Python first-argument identity may name its keyword spelling.
        let keyed = Registries::layered(&[file(
            "keyed.json",
            &entry(r#","identity":"first","keyword":"endpoint_url""#),
        )]);
        assert!(keyed.rejected.is_empty(), "{:?}", keyed.rejected);
        assert_eq!(
            keyed.clients.last().unwrap().keyword.as_deref(),
            Some("endpoint_url")
        );
        for (name, json) in [
            ("keyword-alone.json", entry(r#","keyword":"uri""#)),
            (
                "keyword-key.json",
                entry(r#","identity":{"key":"host"},"keyword":"uri""#),
            ),
            (
                "keyword-empty.json",
                entry(r#","identity":"first","keyword":"""#),
            ),
            (
                "keyword-npm.json",
                entry(r#","identity":"first","keyword":"uri""#).replace("pypi", "npm"),
            ),
            ("same-service.json", entry(r#","select":{"first":"s3"}"#)),
            ("bad-select.json", entry(r#","select":{"first":""}"#)),
            ("select-key.json", entry(r#","select":{"second":"ec2"}"#)),
            ("bad-ecosystem.json", entry("").replace("pypi", "cargo")),
        ] {
            let layered = Registries::layered(&[file(name, &json)]);
            assert_eq!(layered.rejected.len(), 1, "{name} was accepted");
            assert_eq!(layered.clients, builtin.clients, "{name}");
        }
    }

    #[test]
    fn a_valid_extension_adds_entries_that_carry_its_identity() {
        let layered = Registries::layered(&[file("nats.json", EXTRA_SDK)]);
        assert!(layered.rejected.is_empty(), "{:?}", layered.rejected);
        let added = layered.sdks.last().unwrap();
        assert_eq!(added.kind, "nats-subject");
        assert_eq!(added.registry.name, "extension:nats.json");
        assert_eq!(added.registry.version, "x-1");
        assert_eq!(
            added.registry.content_hash,
            core_prov::content_hash(EXTRA_SDK.as_bytes())
        );
        assert_eq!(layered.sdks.len(), Registries::builtin().sdks.len() + 1);
        assert_ne!(layered.identity(), Registries::builtin().identity());
        // Layering is by file name, not offer order, so the identity is too.
        let other = EXTRA_SDK
            .replace("nats-subject", "nats-other")
            .replace("publish", "request");
        let a = Registries::layered(&[file("a.json", &other), file("b.json", EXTRA_SDK)]);
        let b = Registries::layered(&[file("b.json", EXTRA_SDK), file("a.json", &other)]);
        assert_eq!(a.identity(), b.identity());
        assert_eq!(a.sdks.last().unwrap().kind, "nats-subject");
    }

    #[test]
    fn an_invalid_extension_is_rejected_whole_and_changes_nothing() {
        let oversized = format!("{{\"pad\":\"{}\"}}", "x".repeat(MAX_REGISTRY_BYTES));
        // A builtin-backed qualifier plus a duplicate shape: the qualifier
        // is valid alone, but the file as a whole is not, so neither lands.
        let partial = r#"{"schema":"cartograph-event-sdk-registry@1","version":"x-2",
            "qualifiers":[{"kind":"sqs-queue","component":"region","key":"Region"}],
            "sdks":[{"kind":"sqs-queue","role":"produces",
              "pattern":{"constructor":{"module":"@aws-sdk/client-sqs","ctor":"SendMessageCommand"}},
              "identity":{"key":"QueueUrl"}}]}"#;
        let cases = [
            ("bad-json.json", "{".to_string()),
            ("unknown-field.json", EXTRA_SDK.replace("\"version\"", "\"extra\":1,\"version\"")),
            ("unknown-schema.json", EXTRA_SDK.replace("registry@1", "registry@9")),
            ("unknown-role.json", EXTRA_SDK.replace("produces", "observes")),
            ("bad-kind.json", EXTRA_SDK.replace("nats-subject", "NATS subject")),
            ("duplicate-key.json", EXTRA_SDK.replace("\"identity\":\"first\"", "\"identity\":\"first\",\"identity\":\"first\"")),
            ("escaped-duplicate.json", EXTRA_SDK.replace("\"identity\":\"first\"", "\"identity\":\"first\",\"identit\\u0079\":\"first\"")),
            ("nested-duplicate.json", EXTRA_SDK.replace("\"method\":\"publish\"", "\"method\":\"publish\",\"method\":\"request\"")),
            ("two-patterns.json", EXTRA_SDK.replace("}},", "},\"constructor\":{\"module\":\"nats\",\"ctor\":\"Client\"}},")),
            ("empty.json", r#"{"schema":"cartograph-event-sdk-registry@1","version":"x"}"#.into()),
            ("oversized.json", oversized),
            ("partial.json", partial.into()),
            ("unestablished-backing.json", r#"{"schema":"cartograph-channel-backing-registry@1","version":"x",
                "backings":[{"resource_type":"aws_mq_broker","kind":"mq-queue","attribute":"arn"}]}"#.into()),
            ("mixed.json", r#"{"schema":"cartograph-channel-backing-registry@1","version":"x",
                "qualifiers":[{"kind":"sqs-queue","component":"region","key":"Region"}]}"#.into()),
        ];
        let builtin = Registries::builtin();
        for (name, json) in cases {
            let layered = Registries::layered(&[file(name, &json)]);
            assert_eq!(layered.rejected.len(), 1, "{name} was accepted");
            assert_eq!(layered.rejected[0].name, format!("extension:{name}"));
            assert_eq!(layered.identity(), builtin.identity(), "{name}");
            assert_eq!(layered.sdks.len(), builtin.sdks.len(), "{name}");
            assert_eq!(layered.qualifiers, builtin.qualifiers, "{name}");
        }
    }

    #[test]
    fn identity_is_unambiguous_whatever_the_file_names_contain() {
        // Two files `a.json` and `b.json` against one file whose name
        // embeds the first record: a separator-joined encoding collides.
        let other = EXTRA_SDK
            .replace("nats-subject", "nats-other")
            .replace("publish", "request");
        let two = Registries::layered(&[file("a.json", EXTRA_SDK), file("b.json", &other)]);
        let forged_name = format!(
            "a.json={}\nextension:b.json",
            core_prov::content_hash(EXTRA_SDK.as_bytes())
        );
        let one = Registries::layered(&[file(&forged_name, &other)]);
        assert!(two.rejected.is_empty() && one.rejected.is_empty());
        assert_ne!(two.sdks.len(), one.sdks.len());
        assert_ne!(two.identity(), one.identity());
    }

    #[test]
    fn a_conflict_with_an_earlier_extension_rejects_only_the_later_file() {
        let duplicate = EXTRA_SDK.replace("x-1", "x-9");
        let layered = Registries::layered(&[file("b.json", &duplicate), file("a.json", EXTRA_SDK)]);
        assert_eq!(layered.rejected.len(), 1);
        assert_eq!(layered.rejected[0].name, "extension:b.json");
        let accepted = Registries::layered(&[file("a.json", EXTRA_SDK)]);
        assert_eq!(layered.identity(), accepted.identity());
        // An extension can back a kind another extension established.
        let backing = r#"{"schema":"cartograph-channel-backing-registry@1","version":"x",
            "backings":[{"resource_type":"nats_stream","kind":"nats-subject","attribute":"name"}]}"#;
        let layered = Registries::layered(&[file("a.json", EXTRA_SDK), file("c.json", backing)]);
        assert!(layered.rejected.is_empty(), "{:?}", layered.rejected);
        assert_eq!(layered.backings.last().unwrap().kind, "nats-subject");
        let files: Vec<_> = (0..=MAX_EXTENSION_FILES)
            .map(|i| {
                file(
                    &format!("{i:03}.json"),
                    &EXTRA_SDK.replace("publish", &format!("p{i}")),
                )
            })
            .collect();
        let layered = Registries::layered(&files);
        assert_eq!(layered.rejected.len(), 1);
        assert_eq!(layered.sources.len(), 4 + MAX_EXTENSION_FILES);
    }
}
