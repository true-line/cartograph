//! Provider API keys, held in the operating system's credential store
//! (#1763, ADR-0057).
//!
//! A release build opened from Finder or the Dock inherits no shell
//! variables, so the environment alone cannot open the cloud lane. This
//! module is the one place a provider key is read, saved or removed.
//!
//! What must stay true:
//!
//! - The key value is written only to the OS credential store. It is never
//!   written to the settings database, a log, an export or a consent record,
//!   and no command returns it. Callers outside this module get a
//!   [`KeyStatus`], which says where a key comes from and never what it is.
//! - Having a key grants nothing. Every cloud step still passes the egress
//!   firewall and its consent (`egress.rs`).
//! - The environment variable keeps working and wins over a stored key, so a
//!   developer or a CI run can override the saved key for one process.

use serde::Serialize;
use std::sync::{Mutex, OnceLock};

/// Service name of every entry this app writes to the credential store.
#[cfg(any(target_os = "macos", windows))]
const SERVICE: &str = "ai.true-line.cartograph.provider-key";
/// Longest key accepted. Real keys are far shorter; the bound keeps a pasted
/// document out of the credential store.
const MAX_KEY_BYTES: usize = 512;

/// A provider whose key the app can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    Anthropic,
    /// Hosted OpenAI, the GPT side of the frontier track (#1709).
    #[serde(rename = "openai")]
    OpenAi,
}

impl ProviderId {
    pub const ALL: &'static [Self] = &[Self::Anthropic, Self::OpenAi];

    /// Stable id: the account name in the credential store and the wire id.
    pub fn id(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
        }
    }

    /// The environment variable that overrides the stored key.
    pub fn env_var(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
        }
    }

    pub fn parse(id: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|provider| provider.id() == id)
            .ok_or_else(|| format!("unknown provider '{id}'"))
    }
}

/// Where the key in use comes from. Never the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeySource {
    /// The environment variable is set; it overrides any stored key.
    Environment,
    /// A key saved in the OS credential store.
    Stored,
    /// No key: the cloud lane stays closed.
    None,
}

/// What the Settings surface shows for one provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyStatus {
    pub provider: &'static str,
    pub label: &'static str,
    pub env_var: &'static str,
    /// The source of the key a cloud call would use now.
    pub source: KeySource,
    /// A key is saved in the credential store (it may be overridden by the
    /// environment variable).
    pub stored: bool,
    /// The credential store could not be read; `stored` is then `false` and
    /// this says why. Never contains a key.
    pub store_error: Option<String>,
}

/// The credential store, reduced to what this module needs. Errors are
/// messages for the user and must never contain a key.
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, secret: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// The login Keychain on macOS and the Credential Manager on Windows. Other
/// platforms have no store wired; only the environment variable works there.
pub struct OsCredentialStore;

#[cfg(any(target_os = "macos", windows))]
impl OsCredentialStore {
    fn entry(account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, account).map_err(store_error)
    }
}

/// A store failure as a message. `keyring::Error` never carries the secret
/// except in `BadEncoding`, which is reduced to a fixed message here.
#[cfg(any(target_os = "macos", windows))]
fn store_error(error: keyring::Error) -> String {
    match error {
        keyring::Error::BadEncoding(_) => {
            "the saved key is not valid text; remove it and enter it again".into()
        }
        other => format!("the system credential store refused the request: {other}"),
    }
}

#[cfg(any(target_os = "macos", windows))]
impl SecretStore for OsCredentialStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(store_error(error)),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(store_error)
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(store_error(error)),
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
const NO_STORE: &str = "this platform has no credential store wired; set the environment variable";

#[cfg(not(any(target_os = "macos", windows)))]
impl SecretStore for OsCredentialStore {
    fn get(&self, _account: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn set(&self, _account: &str, _secret: &str) -> Result<(), String> {
        Err(NO_STORE.into())
    }

    fn delete(&self, _account: &str) -> Result<(), String> {
        Ok(())
    }
}

/// An in-memory store for tests. Never used in a shipped path.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct MemoryStore(Mutex<std::collections::BTreeMap<String, String>>);

#[cfg(any(test, feature = "test-support"))]
impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        self.0.lock().unwrap().insert(account.into(), secret.into());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

/// Reads one environment variable. A function, so tests never touch the
/// process environment.
type Env = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub struct ProviderCredentials {
    store: Box<dyn SecretStore>,
    env: Env,
    /// Serializes save and remove so a status read between them is coherent.
    write: Mutex<()>,
}

impl ProviderCredentials {
    pub fn new(store: Box<dyn SecretStore>, env: Env) -> Self {
        Self {
            store,
            env,
            write: Mutex::new(()),
        }
    }

    /// The OS credential store and the process environment.
    pub fn system() -> Self {
        Self::new(
            Box::new(OsCredentialStore),
            Box::new(|name| std::env::var(name).ok()),
        )
    }

    fn env_key(&self, provider: ProviderId) -> Option<String> {
        (self.env)(provider.env_var())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    /// The key a cloud call uses now: the environment variable if set, else
    /// the stored key, else none. Only provider construction may call this.
    pub fn resolve(&self, provider: ProviderId) -> Result<Option<String>, String> {
        if let Some(key) = self.env_key(provider) {
            return Ok(Some(key));
        }
        self.store.get(provider.id())
    }

    pub fn status(&self, provider: ProviderId) -> KeyStatus {
        let (stored, store_error) = match self.store.get(provider.id()) {
            Ok(secret) => (secret.is_some(), None),
            Err(error) => (false, Some(error)),
        };
        let source = if self.env_key(provider).is_some() {
            KeySource::Environment
        } else if stored {
            KeySource::Stored
        } else {
            KeySource::None
        };
        KeyStatus {
            provider: provider.id(),
            label: provider.label(),
            env_var: provider.env_var(),
            source,
            stored,
            store_error,
        }
    }

    pub fn statuses(&self) -> Vec<KeyStatus> {
        ProviderId::ALL
            .iter()
            .map(|provider| self.status(*provider))
            .collect()
    }

    /// Save or replace the stored key. The key is trimmed; it must be one
    /// line of printable ASCII with no spaces, which every supported
    /// provider's keys are.
    pub fn save(&self, provider: ProviderId, key: &str) -> Result<KeyStatus, String> {
        let key = key.trim();
        if key.is_empty() {
            return Err("Enter a key.".into());
        }
        if key.len() > MAX_KEY_BYTES {
            return Err(format!("A key is at most {MAX_KEY_BYTES} characters."));
        }
        if !key.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("A key is one line with no spaces.".into());
        }
        let _guard = self.write.lock().map_err(|_| "credential store busy")?;
        self.store.set(provider.id(), key)?;
        Ok(self.status(provider))
    }

    /// Remove the stored key. Removing an absent key succeeds.
    pub fn remove(&self, provider: ProviderId) -> Result<KeyStatus, String> {
        let _guard = self.write.lock().map_err(|_| "credential store busy")?;
        self.store.delete(provider.id())?;
        Ok(self.status(provider))
    }
}

/// The process-wide credentials: OS store plus process environment.
pub fn system() -> &'static ProviderCredentials {
    static SYSTEM: OnceLock<ProviderCredentials> = OnceLock::new();
    SYSTEM.get_or_init(ProviderCredentials::system)
}

#[cfg(any(test, feature = "test-support"))]
pub fn for_test(env_key: Option<&str>) -> ProviderCredentials {
    let env_key = env_key.map(str::to_owned);
    ProviderCredentials::new(
        Box::new(MemoryStore::default()),
        Box::new(move |_| env_key.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-ant-test-0123456789abcdef";

    // AC-0586: enter, replace and remove; the status never carries the key.
    #[test]
    fn a_key_is_saved_replaced_and_removed_and_status_never_shows_it() {
        let credentials = for_test(None);
        let provider = ProviderId::Anthropic;
        assert_eq!(credentials.status(provider).source, KeySource::None);
        assert_eq!(credentials.resolve(provider).unwrap(), None);

        let saved = credentials.save(provider, &format!("  {KEY}\n")).unwrap();
        assert_eq!(saved.source, KeySource::Stored);
        assert!(saved.stored);
        assert_eq!(credentials.resolve(provider).unwrap().as_deref(), Some(KEY));

        let replaced = credentials.save(provider, "sk-ant-second").unwrap();
        assert_eq!(replaced.source, KeySource::Stored);
        assert_eq!(
            credentials.resolve(provider).unwrap().as_deref(),
            Some("sk-ant-second")
        );

        // What crosses IPC names the provider and the source, never the key.
        let wire = serde_json::to_string(&credentials.statuses()).unwrap();
        assert!(!wire.contains("sk-ant"), "{wire}");
        assert!(wire.contains(r#""provider":"anthropic""#));
        assert!(wire.contains(r#""source":"stored""#));

        let removed = credentials.remove(provider).unwrap();
        assert_eq!(removed.source, KeySource::None);
        assert!(!removed.stored);
        assert_eq!(credentials.resolve(provider).unwrap(), None);
        // Removing again is not an error.
        credentials.remove(provider).unwrap();
    }

    // AC-0586: the environment variable wins and is reported as the source.
    #[test]
    fn the_environment_variable_overrides_a_stored_key() {
        let credentials = for_test(Some("sk-ant-from-env"));
        let provider = ProviderId::Anthropic;
        assert_eq!(credentials.status(provider).source, KeySource::Environment);
        credentials.save(provider, KEY).unwrap();
        let status = credentials.status(provider);
        assert_eq!(status.source, KeySource::Environment);
        assert!(status.stored);
        assert_eq!(
            credentials.resolve(provider).unwrap().as_deref(),
            Some("sk-ant-from-env")
        );
        // A blank variable is no key.
        let blank = for_test(Some("   "));
        assert_eq!(blank.status(provider).source, KeySource::None);
    }

    // AC-0586: nothing that is not a key reaches the credential store.
    #[test]
    fn a_malformed_key_is_refused_and_nothing_is_stored() {
        let credentials = for_test(None);
        let provider = ProviderId::Anthropic;
        for bad in ["", "   ", "two words", "line\nbreak", "caf\u{e9}"] {
            assert!(credentials.save(provider, bad).is_err(), "{bad:?}");
        }
        let long = "k".repeat(MAX_KEY_BYTES + 1);
        let error = credentials.save(provider, &long).unwrap_err();
        // The refusal does not echo what was typed.
        assert!(!error.contains("kkkk"));
        assert!(!credentials.status(provider).stored);
        assert!(ProviderId::parse("open-ai").is_err());
        assert_eq!(ProviderId::parse("anthropic").unwrap(), provider);
    }

    // AC-0586: a store that cannot be read is reported, and closes the lane.
    #[test]
    fn an_unreadable_store_is_reported_without_a_key() {
        struct Broken;
        impl SecretStore for Broken {
            fn get(&self, _: &str) -> Result<Option<String>, String> {
                Err("locked".into())
            }
            fn set(&self, _: &str, _: &str) -> Result<(), String> {
                Err("locked".into())
            }
            fn delete(&self, _: &str) -> Result<(), String> {
                Err("locked".into())
            }
        }
        let credentials = ProviderCredentials::new(Box::new(Broken), Box::new(|_| None));
        let status = credentials.status(ProviderId::Anthropic);
        assert_eq!(status.source, KeySource::None);
        assert_eq!(status.store_error.as_deref(), Some("locked"));
        assert!(credentials.resolve(ProviderId::Anthropic).is_err());
        assert!(credentials.save(ProviderId::Anthropic, KEY).is_err());
    }

    // AC-0751 (#1709): the OpenAI key is its own entry under its own
    // variable; saving or removing it leaves the Anthropic key untouched.
    #[test]
    fn the_openai_key_is_held_apart_from_the_anthropic_key() {
        assert_eq!(ProviderId::parse("openai").unwrap(), ProviderId::OpenAi);
        assert_eq!(ProviderId::OpenAi.env_var(), "OPENAI_API_KEY");
        let credentials = ProviderCredentials::new(
            Box::new(MemoryStore::default()),
            Box::new(|name| (name == "OPENAI_API_KEY").then(|| "sk-proj-env".to_owned())),
        );
        assert_eq!(
            credentials.status(ProviderId::OpenAi).source,
            KeySource::Environment
        );
        assert_eq!(
            credentials.status(ProviderId::Anthropic).source,
            KeySource::None
        );
        credentials.save(ProviderId::Anthropic, KEY).unwrap();
        credentials
            .save(ProviderId::OpenAi, "sk-proj-stored")
            .unwrap();
        assert_eq!(
            credentials.resolve(ProviderId::OpenAi).unwrap().as_deref(),
            Some("sk-proj-env")
        );
        credentials.remove(ProviderId::OpenAi).unwrap();
        assert!(!credentials.status(ProviderId::OpenAi).stored);
        assert_eq!(
            credentials
                .resolve(ProviderId::Anthropic)
                .unwrap()
                .as_deref(),
            Some(KEY)
        );

        let wire = serde_json::to_string(&credentials.statuses()).unwrap();
        assert!(wire.contains(r#""provider":"anthropic""#), "{wire}");
        assert!(wire.contains(r#""provider":"openai""#), "{wire}");
        assert!(wire.contains(r#""label":"OpenAI""#), "{wire}");
        assert!(!wire.contains("sk-"), "{wire}");
    }
}
