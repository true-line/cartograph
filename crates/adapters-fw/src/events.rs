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
    /// Where the channel identity lives.
    pub identity: IdentityArg,
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
const SDK_SCHEMA: &str = "cartograph-event-sdk-registry@1";
const BACKING_SCHEMA: &str = "cartograph-channel-backing-registry@1";
const BUILTIN_SDKS: &str = include_str!("../registries/event-sdks.json");
const BUILTIN_BACKINGS: &str = include_str!("../registries/channel-backings.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SdkEntry {
    kind: String,
    role: ChannelRole,
    pattern: SdkPattern,
    identity: IdentityArg,
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
    /// Identities of the accepted files, in layering order.
    pub sources: Vec<RegistryIdentity>,
    /// Files rejected whole, in the order offered.
    pub rejected: Vec<RegistryRejection>,
}

static BUILTIN: LazyLock<Registries> = LazyLock::new(|| {
    let mut registries = Registries::default();
    for (name, bytes) in [
        ("builtin:event-sdks.json", BUILTIN_SDKS),
        ("builtin:channel-backings.json", BUILTIN_BACKINGS),
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
    (1..=64).contains(&value.len())
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
        let entries =
            file.sdks.len() + file.qualifiers.len() + file.batch_keys.len() + file.backings.len();
        if entries == 0 || entries > MAX_ENTRIES {
            return Err(format!("must declare 1..={MAX_ENTRIES} entries"));
        }
        match file.schema.as_str() {
            SDK_SCHEMA if file.backings.is_empty() => {}
            BACKING_SCHEMA
                if file.sdks.is_empty()
                    && file.qualifiers.is_empty()
                    && file.batch_keys.is_empty() => {}
            SDK_SCHEMA | BACKING_SCHEMA => return Err("entries outside its schema".into()),
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
            if !valid_kind(&entry.kind) || !names_valid {
                return Err(format!("invalid sdk entry for kind {:?}", entry.kind));
            }
            if sdks.iter().any(|sdk| sdk.pattern == entry.pattern) {
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
        self.sources.push(registry);
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
            ["builtin:event-sdks.json", "builtin:channel-backings.json"]
        );
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
        assert_eq!(layered.sources.len(), 2 + MAX_EXTENSION_FILES);
    }
}
