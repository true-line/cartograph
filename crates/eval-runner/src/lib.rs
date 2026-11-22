//! The locked evaluation runner (ADR-0082). It runs as a standard account
//! whose only SSH key is restricted to `eval-runner serve`. It reads one
//! policy request on stdin, scores it against the store in that account's
//! home with [`semantic::evaluation::locked::score`], and prints the receipt
//! with an Ed25519 signature over its exact bytes. The signing key never
//! leaves the store.
use ring::{
    rand::SystemRandom,
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

/// Signing key file inside the store, PKCS#8 v2.
pub const KEY_FILE: &str = "signing-key.pk8";
/// Model configuration file inside the store.
pub const MODEL_FILE: &str = "model.json";

/// What `serve` prints on success.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signed {
    /// The receipt's exact bytes, as UTF-8 JSON text.
    pub receipt: String,
    /// Ed25519 signature over `receipt`'s bytes, lowercase hex.
    pub signature: String,
    /// The signer's public key, lowercase hex.
    pub public_key: String,
}

/// The embedding model the runner calls on its own loopback endpoint.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// Always 1.
    pub schema_version: u32,
    /// Ollama model name, for example `nomic-embed-text`.
    pub model: String,
}

/// The store: `store/` in the runner account's home.
pub fn store_root() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("store"))
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn key(store: &Path) -> Result<Ed25519KeyPair, &'static str> {
    let bytes = std::fs::read(store.join(KEY_FILE)).map_err(|_| "invalid_store")?;
    Ed25519KeyPair::from_pkcs8(&bytes).map_err(|_| "invalid_store")
}

/// Create the signing key once, readable only by its owner, and return its
/// public key. An existing key is never replaced.
pub fn keygen(store: &Path) -> Result<String, &'static str> {
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| "store_io")?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(store.join(KEY_FILE))
        .map_err(|_| "key_exists_or_store_io")?;
    file.write_all(document.as_ref()).map_err(|_| "store_io")?;
    file.sync_all().map_err(|_| "store_io")?;
    public_key(store)
}

/// The signing key's public half, lowercase hex.
pub fn public_key(store: &Path) -> Result<String, &'static str> {
    Ok(hex(key(store)?.public_key().as_ref()))
}

/// Sign receipt bytes with the store's key.
pub fn sign(store: &Path, receipt: Vec<u8>) -> Result<Signed, &'static str> {
    let key = key(store)?;
    let signature = hex(key.sign(&receipt).as_ref());
    Ok(Signed {
        receipt: String::from_utf8(receipt).map_err(|_| "store_io")?,
        signature,
        public_key: hex(key.public_key().as_ref()),
    })
}

/// Whether `signed` carries a valid signature by `pinned_public_key` (hex).
/// The key embedded in `signed` is never trusted on its own.
pub fn verify(signed: &Signed, pinned_public_key: &str) -> bool {
    let (Some(key), Some(signature)) = (unhex(pinned_public_key), unhex(&signed.signature)) else {
        return false;
    };
    signed.public_key == pinned_public_key
        && UnparsedPublicKey::new(&ED25519, key)
            .verify(signed.receipt.as_bytes(), &signature)
            .is_ok()
}

/// Score one request and sign the receipt, or return the fixed error code.
pub fn serve(
    store: &Path,
    request: &[u8],
    provider: &dyn llm::LlmProvider,
) -> Result<Signed, &'static str> {
    // Refuse before scoring when the key is missing, so no policy is committed
    // without a receipt that can be signed.
    key(store)?;
    let receipt = semantic::evaluation::locked::score(store, request, provider)
        .map_err(semantic::evaluation::locked::Code::as_str)?;
    sign(store, receipt)
}

/// The model the store is configured for.
pub fn model(store: &Path) -> Result<String, &'static str> {
    let bytes = std::fs::read(store.join(MODEL_FILE)).map_err(|_| "invalid_store")?;
    let config: ModelConfig = serde_json::from_slice(&bytes).map_err(|_| "invalid_store")?;
    if config.schema_version != 1 || config.model.is_empty() || config.model.len() > 128 {
        return Err("invalid_store");
    }
    Ok(config.model)
}

#[cfg(test)]
mod tests;
