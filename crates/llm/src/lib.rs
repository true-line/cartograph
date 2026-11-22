//! Pluggable model providers behind a local-first egress firewall.
//!
//! All completion calls are prepared by [`EgressFirewall`]. The firewall
//! redacts secrets, hard-fails cloud providers unless the analysis tier is
//! enabled, and binds one explicit consent grant to the exact redacted
//! span-level payload. Provider-facing completion requests cannot be
//! constructed outside this crate, so callers cannot bypass the gate.

pub mod anthropic;
pub mod bounded;
pub mod catalog;
pub mod debug_log;
pub mod openai;
pub mod openai_compatible;
pub mod read_deny;

use reqwest::Url;
use reqwest::blocking::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::Duration;

/// Dense embedding vector returned by a provider.
pub type Embedding = Vec<f32>;

/// Whether a provider executes locally or sends data off-device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locality {
    /// The payload stays on this machine.
    Local,
    /// The payload leaves the machine and requires explicit consent.
    Cloud,
}

/// Model-backed analysis tiers governed independently by egress policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AnalysisTier {
    /// T2 semantic resolution.
    Semantic,
    /// T3 bounded agentic resolution.
    Agentic,
}

/// Capabilities exposed by a model provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCaps {
    /// Dense text embeddings.
    pub embeddings: bool,
    /// Text/chat completion.
    pub chat: bool,
    /// Tool-calling completion.
    pub tool_use: bool,
}

/// One exact source span included in a model payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadSpan {
    /// Stable identifier referenced by the prompt and model citations.
    pub id: String,
    /// Repository identity.
    pub repo: String,
    /// Repository-relative path.
    pub path: String,
    /// Inclusive byte offset in the source file.
    pub byte_start: u64,
    /// Exclusive byte offset in the source file.
    pub byte_end: u64,
    /// Commit from which the span was read.
    pub commit_sha: String,
    /// Exact text sent after secret redaction.
    pub text: String,
}

/// Structured completion payload shown to the user and sent to the provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionPayload {
    /// Inactive by default. JSON object text for bounded Ollama only. Preparation
    /// refuses JSON escapes or redaction-changing text; it does not validate
    /// JSON Schema semantics. Host response validation remains authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<String>,
    /// System instructions.
    pub system: String,
    /// Task instructions; evidence text lives only in `spans`.
    pub prompt: String,
    /// Exact evidence spans leaving the device.
    pub spans: Vec<PayloadSpan>,
}

/// A model-backed action submitted to the egress firewall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionAction {
    /// Stable identifier for this one action.
    pub action_id: String,
    /// Tier requesting model access.
    pub tier: AnalysisTier,
    /// Structured payload.
    pub payload: CompletionPayload,
}

/// Exact redacted payload preview used by the consent UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressPreview {
    /// Provider receiving the payload.
    pub provider_id: String,
    /// Provider locality.
    pub locality: Locality,
    /// Tier requesting egress.
    pub tier: AnalysisTier,
    /// Stable action identifier.
    pub action_id: String,
    /// Exact redacted structured payload.
    pub payload: CompletionPayload,
    /// BLAKE3 hash binding consent to this provider, action, and payload.
    pub payload_hash: String,
    /// Number of secret-shaped values replaced before preview or dispatch.
    pub redaction_count: usize,
}

/// Explicit user grant for exactly one previewed cloud action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentGrant {
    /// Provider approved by the user.
    pub provider_id: String,
    /// Tier approved by the user.
    pub tier: AnalysisTier,
    /// Action approved by the user.
    pub action_id: String,
    /// Hash copied from the preview the user saw.
    pub payload_hash: String,
}

impl ConsentGrant {
    /// Create a grant from the exact preview a user accepted.
    pub fn from_preview(preview: &EgressPreview) -> Self {
        Self {
            provider_id: preview.provider_id.clone(),
            tier: preview.tier,
            action_id: preview.action_id.clone(),
            payload_hash: preview.payload_hash.clone(),
        }
    }

    fn matches(&self, preview: &EgressPreview) -> bool {
        self.provider_id == preview.provider_id
            && self.tier == preview.tier
            && self.action_id == preview.action_id
            && self.payload_hash == preview.payload_hash
    }
}

/// Per-tier cloud egress policy. The default contains no allowed tiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EgressPolicy {
    cloud_tiers: BTreeSet<AnalysisTier>,
}

impl EgressPolicy {
    /// Default-deny local-only policy.
    pub fn local_only() -> Self {
        Self::default()
    }

    /// Permit cloud access for the listed tiers. Every action still requires
    /// its own matching [`ConsentGrant`].
    pub fn allow_cloud_for(tiers: impl IntoIterator<Item = AnalysisTier>) -> Self {
        Self {
            cloud_tiers: tiers.into_iter().collect(),
        }
    }

    /// Whether cloud access is configured for a tier.
    pub fn cloud_allowed(&self, tier: AnalysisTier) -> bool {
        self.cloud_tiers.contains(&tier)
    }
}

/// Completion output returned by a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    /// Provider-returned text.
    pub text: String,
}

/// Provider-facing request. Its fields are private so only the firewall can
/// construct an authorized, redacted request.
pub struct ProviderCompletionRequest {
    payload: CompletionPayload,
}

impl ProviderCompletionRequest {
    /// Redacted system instructions.
    pub fn system(&self) -> &str {
        &self.payload.system
    }

    /// Redacted task instructions.
    pub fn prompt(&self) -> &str {
        &self.payload.prompt
    }

    /// Exact redacted evidence spans.
    pub fn spans(&self) -> &[PayloadSpan] {
        &self.payload.spans
    }

    fn user_message(&self) -> Result<String, ProviderError> {
        Ok(format!(
            "{}\n\nEvidence spans (JSON):\n{}",
            self.payload.prompt,
            serde_json::to_string_pretty(&self.payload.spans)?
        ))
    }
}

/// Failures from provider construction, authorization, or invocation.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Provider URL is malformed.
    #[error("invalid provider URL: {0}")]
    InvalidUrl(String),
    /// A provider declared local attempted to use a non-loopback endpoint.
    #[error("local provider endpoint must be loopback: {0}")]
    NonLoopback(String),
    /// HTTP transport or response decoding failed.
    #[error("provider request: {0}")]
    Request(#[from] reqwest::Error),
    /// JSON serialization or decoding failed.
    #[error("provider payload: {0}")]
    Payload(#[from] serde_json::Error),
    /// Provider returned a structurally invalid embedding batch.
    #[error("invalid embedding response: {0}")]
    InvalidEmbeddings(String),
    /// The provider does not implement the requested operation.
    #[error("provider does not support {0}")]
    Unsupported(&'static str),
    /// Default-deny policy blocked a cloud provider before invocation.
    #[error("cloud provider {provider} is disabled for {tier:?}")]
    EgressDenied {
        /// Blocked provider.
        provider: String,
        /// Blocked analysis tier.
        tier: AnalysisTier,
    },
    /// Cloud access is enabled but this exact action has not been accepted.
    #[error("explicit consent required for cloud action {0:?}")]
    ConsentRequired(Box<EgressPreview>),
    /// A grant was presented for a different provider, action, or payload.
    #[error("consent does not match the exact redacted payload")]
    ConsentMismatch,
    /// Provider returned an empty completion.
    #[error("provider returned an empty completion")]
    EmptyCompletion,
    /// Provider returned an oversized or structurally invalid response.
    #[error("invalid provider response: {0}")]
    InvalidResponse(String),
    /// A cloud provider's safety classifiers declined the request. The
    /// escalation surfaces this as an explicit outcome, never a retry storm.
    #[error("cloud provider {provider} declined the request (category: {category:?})")]
    Refused {
        /// Declining provider.
        provider: String,
        /// Policy category when the provider reported one.
        category: Option<String>,
    },
    /// The host could not record a cloud egress, so the call was not made.
    #[error("cloud egress to {provider} could not be recorded; nothing was sent")]
    EgressUnrecorded {
        /// Provider that was not invoked.
        provider: String,
    },
}

/// Observed local embedding artifact identity; not execution attestation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingIdentity {
    /// Endpoint the embeddings were requested from.
    pub endpoint: String,
    /// Model name as configured.
    pub model: String,
    /// Model digest reported by the endpoint.
    pub digest: String,
    /// Versioned request recipe used to produce the embeddings.
    pub recipe: String,
}

/// Model-provider SPI from SPEC-00 §8.2.
pub trait LlmProvider: Send + Sync {
    /// Stable provider identifier.
    fn id(&self) -> &str;
    /// Where the provider executes.
    fn locality(&self) -> Locality;
    /// Provider features.
    fn capabilities(&self) -> ProviderCaps;
    /// Embed a batch of texts in input order.
    fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError>;
    /// Read-only verified identity for persistent embedding reuse. Unknown is unpinned.
    fn embedding_cache_identity(&self) -> Result<Option<EmbeddingIdentity>, ProviderError> {
        Ok(None)
    }
    /// Identity of the model `embed` uses, recorded as a fact's producing
    /// method (AC-0244). Defaults to the provider identity; a provider that
    /// pairs separate embedding and completion models names only this one.
    fn embedding_model_id(&self) -> String {
        self.id().to_string()
    }
    /// Identity of the model completions use, recorded as a proposal's
    /// producing method (AC-0244). Defaults to the provider identity.
    fn completion_model_id(&self) -> String {
        self.id().to_string()
    }
    /// Execute a completion already authorized and redacted by the firewall.
    fn complete(&self, _request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        Err(ProviderError::Unsupported("chat completion"))
    }

    /// Explicit bounded schema support; legacy and other transports default deny.
    fn supports_bounded_response_schema(&self) -> bool {
        false
    }

    /// Pure identity of the separately bounded completion protocol. Legacy
    /// providers must opt in explicitly; no unbounded fallback is permitted.
    fn bounded_profile(&self) -> Result<bounded::ProviderProfile, bounded::BoundedCallError> {
        Err(bounded::BoundedCallError::unsupported())
    }

    /// Execute one already-authorized bounded invocation. The host owns durable
    /// consent consumption, invocation reservation and execution fencing.
    fn complete_bounded(
        &self,
        _request: &bounded::AuthorizedBoundedCompletion,
        _control: &bounded::CompletionControl<'_>,
    ) -> Result<bounded::BoundedCompletion, bounded::BoundedCallError> {
        Err(bounded::BoundedCallError::unsupported())
    }

    /// The credential the host's per-run model proxy adds to a tool-calling
    /// runtime's request for this provider's bounded endpoint (ADR-0085
    /// decision 7). The runtime itself never holds it. `None` for providers
    /// that need none or have no tool-calling lane.
    fn proxy_credential(&self) -> Option<UpstreamCredential> {
        None
    }
}

/// One request header holding a provider key. Its value never appears in
/// `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct UpstreamCredential {
    header: &'static str,
    value: String,
}

impl UpstreamCredential {
    /// A credential sent as `header: value`.
    pub fn new(header: &'static str, value: impl Into<String>) -> Self {
        Self {
            header,
            value: value.into(),
        }
    }

    /// The header name.
    pub fn header(&self) -> &'static str {
        self.header
    }

    /// The secret header value.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Debug for UpstreamCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamCredential")
            .field("header", &self.header)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Records one cloud egress before it happens: the provider id and the byte
/// length of the exact redacted payload. `false` means the record could not
/// be written.
pub type EgressRecorder = dyn Fn(&str, u64) -> bool + Send + Sync;

/// Default-deny completion firewall shared by semantic and agentic callers.
pub struct EgressFirewall {
    policy: EgressPolicy,
    recorder: Option<Box<EgressRecorder>>,
    read_deny: read_deny::ReadDenyRules,
}

impl EgressFirewall {
    /// Construct a firewall from persisted/configured policy.
    pub fn new(policy: EgressPolicy) -> Self {
        Self {
            policy,
            recorder: None,
            read_deny: read_deny::ReadDenyRules::default(),
        }
    }

    /// Apply these read deny rules: the `egress-read-deny/v1` defaults plus
    /// the project's own (#679). The defaults always apply.
    #[must_use]
    pub fn with_read_deny_rules(mut self, rules: read_deny::ReadDenyRules) -> Self {
        self.read_deny = rules;
        self
    }

    /// Decide whether one acquired read representation may reach `provider`
    /// (#679, `egress-read-deny/v1`). The provider's own locality decides
    /// whether the rules apply, so a local provider is always allowed. A
    /// refusal carries the only constructor of its witness.
    pub fn evaluate_read(
        &self,
        provider: &dyn LlmProvider,
        acquired_attempt_id: &str,
        read: &read_deny::ReadIdentity,
        representation: &[u8],
    ) -> read_deny::ReadDecision {
        self.read_deny.evaluate(
            provider.locality(),
            acquired_attempt_id,
            read,
            representation,
        )
    }

    /// Record every cloud payload `complete` hands to a provider (#1767).
    /// The recorder runs after consent has matched and before the provider
    /// is invoked, so a call that then fails or times out is still counted.
    /// A recorder that returns `false` stops the call unsent.
    #[must_use]
    pub fn with_egress_recorder(
        mut self,
        recorder: impl Fn(&str, u64) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.recorder = Some(Box::new(recorder));
        self
    }

    /// Build the exact redacted preview without invoking the provider.
    pub fn preview(
        &self,
        provider: &dyn LlmProvider,
        action: &CompletionAction,
    ) -> Result<EgressPreview, ProviderError> {
        if action.payload.response_schema.is_some() {
            return Err(ProviderError::Unsupported("legacy response schema"));
        }
        if !provider.capabilities().chat {
            return Err(ProviderError::Unsupported("chat completion"));
        }
        if provider.locality() == Locality::Cloud && !self.policy.cloud_allowed(action.tier) {
            return Err(ProviderError::EgressDenied {
                provider: provider.id().to_string(),
                tier: action.tier,
            });
        }
        let (payload, redaction_count) = redact_payload(&action.payload);
        let hash_bytes = serde_json::to_vec(&(
            provider.id(),
            provider.locality(),
            action.tier,
            &action.action_id,
            &payload,
        ))?;
        Ok(EgressPreview {
            provider_id: provider.id().to_string(),
            locality: provider.locality(),
            tier: action.tier,
            action_id: action.action_id.clone(),
            payload,
            payload_hash: blake3::hash(&hash_bytes).to_hex().to_string(),
            redaction_count,
        })
    }

    /// Invoke a local provider immediately or a cloud provider only after a
    /// matching one-action grant. The provider sees only the redacted payload
    /// from the preview.
    pub fn complete(
        &self,
        provider: &dyn LlmProvider,
        action: &CompletionAction,
        consent: Option<&ConsentGrant>,
    ) -> Result<Completion, ProviderError> {
        let preview = self.preview(provider, action)?;
        if preview.locality == Locality::Cloud {
            let Some(grant) = consent else {
                return Err(ProviderError::ConsentRequired(Box::new(preview)));
            };
            if !grant.matches(&preview) {
                return Err(ProviderError::ConsentMismatch);
            }
            if let Some(recorder) = &self.recorder {
                let bytes = serde_json::to_vec(&preview.payload)?.len() as u64;
                if !recorder(&preview.provider_id, bytes) {
                    return Err(ProviderError::EgressUnrecorded {
                        provider: preview.provider_id,
                    });
                }
            }
        }
        let request = ProviderCompletionRequest {
            payload: preview.payload,
        };
        let call = debug_log::enabled().then(|| {
            // Borrowed parts: the redacted texts are not copied before the
            // debug log reserves room for them.
            let heads: Vec<String> = request
                .spans()
                .iter()
                .map(|span| {
                    format!(
                        "\n\n--- span {} {}/{}@{} [{}..{}] ---\n",
                        span.id,
                        span.repo,
                        span.path,
                        span.commit_sha,
                        span.byte_start,
                        span.byte_end
                    )
                })
                .collect();
            let mut parts: Vec<&[u8]> = vec![
                b"SYSTEM:\n",
                request.system().as_bytes(),
                b"\n\nPROMPT:\n",
                request.prompt().as_bytes(),
            ];
            for (head, span) in heads.iter().zip(request.spans()) {
                parts.push(head.as_bytes());
                parts.push(span.text.as_bytes());
            }
            debug_log::begin("firewall", &preview.provider_id, &parts)
        });
        let result = provider.complete(&request).and_then(|completion| {
            if completion.text.trim().is_empty() {
                Err(ProviderError::EmptyCompletion)
            } else {
                Ok(completion)
            }
        });
        if let Some(call) = call {
            call.finish(
                result
                    .as_ref()
                    .ok()
                    .map(|completion| completion.text.as_bytes()),
                result.as_ref().err().map(ToString::to_string),
            );
        }
        let completion = result?;
        Ok(completion)
    }
}

/// Local Ollama provider using `/api/embed` and `/api/chat`.
pub struct OllamaProvider {
    bounded_thinking: bool,
    bounded_context_window: Option<u32>,
    base_url: Url,
    embedding_model: String,
    completion_model: String,
    provider_id: String,
    client: Client,
    bounded_client: Client,
}

impl OllamaProvider {
    /// The `/api/embed` exchange itself; `embed` adds the debug log entries.
    fn embed_unlogged(
        &self,
        endpoint: reqwest::Url,
        batch: &[String],
    ) -> Result<Vec<Embedding>, ProviderError> {
        let mut response = self
            .client
            .post(endpoint)
            .json(&EmbedRequest {
                model: &self.embedding_model,
                input: batch,
                truncate: false,
            })
            .send()?
            .error_for_status()?;
        use std::io::Read;
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| ProviderError::InvalidEmbeddings(e.to_string()))?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(ProviderError::InvalidEmbeddings(
                "embedding response bounds".into(),
            ));
        }
        let response: EmbedResponse = serde_json::from_slice(&bytes)?;
        validate_embeddings(batch.len(), &response.embeddings)?;
        Ok(response.embeddings)
    }

    /// Default loopback endpoint used by Ollama.
    pub const DEFAULT_URL: &'static str = "http://127.0.0.1:11434/";
    /// Ollama's default port, the one in [`Self::DEFAULT_URL`].
    pub const DEFAULT_PORT: u16 = 11434;

    /// The loopback endpoint for an Ollama listening on `port` (#1653). Only
    /// the port is configurable: the host stays `127.0.0.1`.
    pub fn loopback_url(port: u16) -> String {
        format!("http://127.0.0.1:{port}/")
    }
    /// Default local embedding model.
    pub const DEFAULT_EMBEDDING_MODEL: &'static str = "nomic-embed-text";
    /// Default local completion model. Cartograph never downloads it.
    pub const DEFAULT_COMPLETION_MODEL: &'static str = "qwen3:8b";
    /// Compatibility alias for the M7 embedding default.
    pub const DEFAULT_MODEL: &'static str = Self::DEFAULT_EMBEDDING_MODEL;

    /// Construct a loopback-only provider using one model for both operations.
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let model = model.into();
        Self::with_models(base_url, model.clone(), model, timeout)
    }

    /// Construct a loopback-only provider with separate embedding and chat
    /// models.
    pub fn with_models(
        base_url: &str,
        embedding_model: impl Into<String>,
        completion_model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let base_url = loopback_base(base_url)?;
        let client = build_direct_loopback_client(Client::builder(), timeout)?;
        let bounded_client = bounded::build_client(Client::builder(), true)?;
        let embedding_model = embedding_model.into();
        let completion_model = completion_model.into();
        let provider_id = if embedding_model == completion_model {
            format!("ollama:{embedding_model}")
        } else {
            format!("ollama:{embedding_model}+{completion_model}")
        };
        Ok(Self {
            bounded_thinking: false,
            bounded_context_window: None,
            base_url,
            embedding_model,
            completion_model,
            provider_id,
            client,
            bounded_client,
        })
    }

    /// Select reasoning for bounded auditor calls and bind it to the provider profile.
    pub fn with_thinking_enabled(mut self) -> Self {
        if !self.bounded_thinking {
            self.provider_id.push_str(":thinking-enabled");
            self.bounded_thinking = true;
        }
        self
    }

    /// Context windows a bounded chat may request from Ollama (#1653). Without
    /// one, the server's own default applies, which can silently cut a long
    /// prompt short on a local model.
    pub const CONTEXT_WINDOWS: [u32; 4] = [8192, 16384, 32768, 65536];

    /// Request `num_ctx` on bounded chats and bind it to the provider profile,
    /// so the consent fingerprint and provenance record it. Only one of
    /// [`Self::CONTEXT_WINDOWS`] is accepted.
    pub fn with_context_window(mut self, tokens: u32) -> Result<Self, ProviderError> {
        if self.bounded_context_window.is_some() || !Self::CONTEXT_WINDOWS.contains(&tokens) {
            return Err(ProviderError::Unsupported(
                "a context window other than one of OllamaProvider::CONTEXT_WINDOWS",
            ));
        }
        self.provider_id.push_str(&format!(":num-ctx-{tokens}"));
        self.bounded_context_window = Some(tokens);
        Ok(self)
    }

    /// Construct the default local provider.
    pub fn local_default() -> Result<Self, ProviderError> {
        Self::local_on_port(Self::DEFAULT_PORT)
    }

    /// The default local provider on the loopback Ollama listening on `port`
    /// (#1653). Its provider id does not depend on the port.
    pub fn local_on_port(port: u16) -> Result<Self, ProviderError> {
        Self::with_models(
            &Self::loopback_url(port),
            Self::DEFAULT_EMBEDDING_MODEL,
            Self::DEFAULT_COMPLETION_MODEL,
            Duration::from_secs(120),
        )
    }

    /// Probe the local endpoint for installed models (name **and** digest —
    /// the digest is what makes two artifacts behind the same mutable tag
    /// distinguishable). Loopback-only like every other call on this
    /// provider; the result feeds [`catalog::classify_health`] — Cartograph
    /// reports a missing model, it never pulls one itself.
    pub fn list_local_models(&self) -> Result<Vec<crate::catalog::LocalModel>, ProviderError> {
        #[derive(Deserialize)]
        struct Tags {
            models: Vec<TagEntry>,
        }
        #[derive(Deserialize)]
        struct TagEntry {
            name: String,
            #[serde(default)]
            digest: String,
        }
        let url = self
            .base_url
            .join("api/tags")
            .map_err(|error| ProviderError::InvalidUrl(error.to_string()))?;
        let mut response = self.client.get(url).send()?.error_for_status()?;
        // Redirects are not followed; even valid JSON in a 3xx response does
        // not establish a successful local metadata API observation.
        if !response.status().is_success() {
            return Err(ProviderError::InvalidEmbeddings(
                "local metadata status".into(),
            ));
        }
        let mut bytes = Vec::new();
        use std::io::Read;
        response
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| ProviderError::InvalidEmbeddings(error.to_string()))?;
        if bytes.len() > 1024 * 1024 {
            return Err(ProviderError::InvalidEmbeddings(
                "model metadata limit".into(),
            ));
        }
        let tags: Tags = serde_json::from_slice(&bytes)?;
        Ok(tags
            .models
            .into_iter()
            .map(|entry| crate::catalog::LocalModel {
                name: entry.name,
                digest: entry.digest,
            })
            .collect())
    }
}

/// A plain-HTTP loopback base URL ending in `/`; any other host is not Local.
fn loopback_base(base_url: &str) -> Result<Url, ProviderError> {
    let mut base_url =
        Url::parse(base_url).map_err(|error| ProviderError::InvalidUrl(error.to_string()))?;
    let loopback = base_url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if base_url.scheme() != "http" || !loopback {
        return Err(ProviderError::NonLoopback(base_url.to_string()));
    }
    if !base_url.path().ends_with('/') {
        base_url.set_path(&format!("{}/", base_url.path()));
    }
    Ok(base_url)
}

/// Local providers' transport must never inherit either environment/system
/// proxies or a caller-added proxy, nor follow a redirect off loopback.
/// Otherwise loopback evidence could leave the device without passing cloud
/// consent.
fn build_direct_loopback_client(
    builder: ClientBuilder,
    timeout: Duration,
) -> Result<Client, ProviderError> {
    Ok(builder
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()?)
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
    truncate: bool,
}

fn deserialize_bounded_embeddings<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Embedding>, D::Error> {
    struct Vector(Embedding);
    impl<'de> Deserialize<'de> for Vector {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Vector;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("at most 4096 embedding coordinates")
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> Result<Vector, A::Error> {
                    let mut out = Vec::new();
                    while let Some(value) = seq.next_element::<f32>()? {
                        if out.len() == 4096 {
                            return Err(serde::de::Error::custom(
                                "embedding dimensions exceed 4096",
                            ));
                        }
                        out.push(value);
                    }
                    Ok(Vector(out))
                }
            }
            d.deserialize_seq(Visitor)
        }
    }
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = Vec<Embedding>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("at most 128 embedding vectors")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(Vector(value)) = seq.next_element::<Vector>()? {
                if out.len() == 128 {
                    return Err(serde::de::Error::custom("embedding count exceeds 128"));
                }
                out.push(value);
            }
            Ok(out)
        }
    }
    deserializer.deserialize_seq(Visitor)
}

#[derive(Deserialize)]
struct EmbedResponse {
    #[serde(deserialize_with = "deserialize_bounded_embeddings")]
    embeddings: Vec<Embedding>,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    stream: bool,
    format: &'static str,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

impl LlmProvider for OllamaProvider {
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<bounded::ProviderProfile, bounded::BoundedCallError> {
        bounded::ollama_profile(self.id(), &self.base_url, &self.completion_model)
    }

    fn complete_bounded(
        &self,
        request: &bounded::AuthorizedBoundedCompletion,
        control: &bounded::CompletionControl<'_>,
    ) -> Result<bounded::BoundedCompletion, bounded::BoundedCallError> {
        bounded::complete_ollama(
            &self.bounded_client,
            &self.bounded_profile()?,
            self.bounded_thinking,
            self.bounded_context_window,
            request,
            control,
        )
    }

    fn id(&self) -> &str {
        &self.provider_id
    }

    fn embedding_model_id(&self) -> String {
        format!("ollama:{}", self.embedding_model)
    }

    fn embedding_cache_identity(&self) -> Result<Option<EmbeddingIdentity>, ProviderError> {
        let name = if self.embedding_model.contains(':') {
            self.embedding_model.clone()
        } else {
            format!("{}:latest", self.embedding_model)
        };
        let models = self.list_local_models()?;
        let matching: Vec<_> = models.iter().filter(|m| m.name == name).collect();
        if matching.len() != 1 {
            return Ok(None);
        }
        let digest = matching[0]
            .digest
            .strip_prefix("sha256:")
            .unwrap_or(&matching[0].digest);
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(None);
        }
        Ok(Some(EmbeddingIdentity {
            endpoint: self.base_url.to_string(),
            model: name,
            digest: format!("sha256:{}", digest.to_ascii_lowercase()),
            recipe: "ollama-embed-truncate-false@1".into(),
        }))
    }

    fn completion_model_id(&self) -> String {
        format!("ollama:{}", self.completion_model)
    }

    fn locality(&self) -> Locality {
        Locality::Local
    }

    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: true,
            chat: true,
            tool_use: false,
        }
    }

    fn embed(&self, batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let endpoint = self
            .base_url
            .join("api/embed")
            .map_err(|error| ProviderError::InvalidUrl(error.to_string()))?;
        if batch.len() > 128
            || batch.iter().any(|t| t.len() > 65536)
            || batch.iter().map(String::len).sum::<usize>() > 1024 * 1024
        {
            return Err(ProviderError::InvalidEmbeddings(
                "embedding request bounds".into(),
            ));
        }
        // #1624: input texts as SENT; the vectors themselves are summarized.
        let call = debug_log::enabled().then(|| {
            let head = format!("EMBED model={} texts={}", self.embedding_model, batch.len());
            let mut parts: Vec<&[u8]> = vec![head.as_bytes()];
            for text in batch {
                parts.push(b"\n--- text ---\n");
                parts.push(text.as_bytes());
            }
            debug_log::begin("embed", endpoint.as_str(), &parts)
        });
        let result = self.embed_unlogged(endpoint, batch);
        if let Some(call) = call {
            let summary = result.as_ref().ok().map(|vectors| {
                format!(
                    "{} vectors of {} dimensions (values not shown)",
                    vectors.len(),
                    vectors.first().map_or(0, Vec::len)
                )
            });
            call.finish(
                summary.as_deref().map(str::as_bytes),
                result.as_ref().err().map(ToString::to_string),
            );
        }
        result
    }

    fn complete(&self, request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        let endpoint = self
            .base_url
            .join("api/chat")
            .map_err(|error| ProviderError::InvalidUrl(error.to_string()))?;
        let user = request.user_message()?;
        let response: ChatResponse = self
            .client
            .post(endpoint)
            .json(&ChatRequest {
                model: &self.completion_model,
                messages: vec![
                    ChatMessage {
                        role: "system",
                        content: request.system(),
                    },
                    ChatMessage {
                        role: "user",
                        content: &user,
                    },
                ],
                stream: false,
                format: "json",
            })
            .send()?
            .error_for_status()?
            .json()?;
        Ok(Completion {
            text: response.message.content,
        })
    }
}

fn validate_embeddings(expected: usize, embeddings: &[Embedding]) -> Result<(), ProviderError> {
    if embeddings.len() != expected {
        return Err(ProviderError::InvalidEmbeddings(format!(
            "expected {expected} vectors, got {}",
            embeddings.len()
        )));
    }
    let Some(dimensions) = embeddings.first().map(Vec::len) else {
        return Ok(());
    };
    if dimensions == 0
        || embeddings.iter().any(|embedding| {
            embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
        })
    {
        return Err(ProviderError::InvalidEmbeddings(
            "vectors must be non-empty, finite, and uniformly sized".into(),
        ));
    }
    Ok(())
}

fn redact_payload(payload: &CompletionPayload) -> (CompletionPayload, usize) {
    let (system, mut count) = redact_text(&payload.system);
    let (prompt, prompt_count) = redact_text(&payload.prompt);
    count += prompt_count;
    let spans = payload
        .spans
        .iter()
        .map(|span| {
            let (text, span_count) = redact_text(&span.text);
            count += span_count;
            PayloadSpan {
                text,
                ..span.clone()
            }
        })
        .collect();
    (
        CompletionPayload {
            response_schema: payload.response_schema.clone(),
            system,
            prompt,
            spans,
        },
        count,
    )
}

fn redact_text(input: &str) -> (String, usize) {
    core_redact::redact_text(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn action(text: &str) -> CompletionAction {
        CompletionAction {
            action_id: "resolve-gap-1".into(),
            tier: AnalysisTier::Agentic,
            payload: CompletionPayload {
                response_schema: None,
                system: "Return JSON only".into(),
                prompt: "Resolve the explicit Gap".into(),
                spans: vec![PayloadSpan {
                    id: "source".into(),
                    repo: "acme/shop".into(),
                    path: "src/orders.ts".into(),
                    byte_start: 10,
                    byte_end: 42,
                    commit_sha: "abc123".into(),
                    text: text.into(),
                }],
            },
        }
    }

    struct FakeCloud {
        calls: AtomicUsize,
    }

    impl LlmProvider for FakeCloud {
        fn id(&self) -> &str {
            "cloud:test"
        }

        fn locality(&self) -> Locality {
            Locality::Cloud
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }

        fn embed(&self, _batch: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Err(ProviderError::Unsupported("embeddings"))
        }

        fn complete(
            &self,
            request: &ProviderCompletionRequest,
        ) -> Result<Completion, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let user = request.user_message()?;
            assert!(!user.contains("super-secret"));
            Ok(Completion {
                text: if user.contains("debug-1624-empty") {
                    "  ".into()
                } else {
                    r#"{"target_id":"chan:orders"}"#.into()
                },
            })
        }
    }

    #[test]
    fn firewall_completion_records_redacted_request_and_text_in_debug_log() {
        // AC-0560 (#1624): a single-shot completion records the redacted
        // messages as SENT and the completion text as RECEIVED; the secret
        // never reaches the debug log.
        debug_log::test_support::capture();
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
        let provider = FakeCloud {
            calls: AtomicUsize::new(0),
        };
        let request = action("debug-1624-firewall password=super-secret");
        let preview = firewall.preview(&provider, &request).unwrap();
        let grant = ConsentGrant::from_preview(&preview);
        firewall
            .complete(&provider, &request, Some(&grant))
            .unwrap();
        let calls = debug_log::test_support::calls_with("debug-1624-firewall");
        assert_eq!(calls.len(), 1);
        let (sent, received) = &calls[0];
        assert_eq!(sent.path, "firewall");
        assert_eq!(sent.target, "cloud:test");
        let sent_text = sent.request.as_deref().unwrap();
        assert!(sent_text.starts_with("SYSTEM:\n"));
        assert!(!sent_text.contains("super-secret"));
        let received = received.as_ref().expect("received entry");
        assert_eq!(
            received.response.as_deref(),
            Some(r#"{"target_id":"chan:orders"}"#)
        );
    }

    #[test]
    fn firewall_debug_log_records_an_empty_completion_as_the_error_returned() {
        // AC-0560 (#1624 review): the RECEIVED entry matches the caller's
        // outcome, so whitespace-only text is logged as EmptyCompletion.
        debug_log::test_support::capture();
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
        let provider = FakeCloud {
            calls: AtomicUsize::new(0),
        };
        let request = action("debug-1624-empty");
        let preview = firewall.preview(&provider, &request).unwrap();
        let grant = ConsentGrant::from_preview(&preview);
        let error = firewall
            .complete(&provider, &request, Some(&grant))
            .unwrap_err();
        assert!(matches!(error, ProviderError::EmptyCompletion));
        let calls = debug_log::test_support::calls_with("debug-1624-empty");
        let received = calls[0].1.as_ref().expect("received entry");
        assert_eq!(received.error, Some(error.to_string()));
        assert_eq!(received.response, None);
    }

    #[test]
    fn local_provider_rejects_non_loopback_endpoints() {
        let error = OllamaProvider::new(
            "https://models.example.com",
            "embed",
            Duration::from_secs(1),
        )
        .err()
        .expect("cloud URL rejected");
        assert!(matches!(error, ProviderError::NonLoopback(_)));
    }

    #[test]
    fn ollama_embed_uses_batch_api_and_validates_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0_u8; 8192];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("POST /api/embed HTTP/1.1"));
            assert!(request.contains("\"model\":\"test-model\""));
            assert!(request.contains("\"input\":[\"orders\",\"users\"]"));
            let body = r#"{"embeddings":[[1.0,0.0],[0.0,1.0]]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let provider = OllamaProvider::new(
            &format!("http://{address}"),
            "test-model",
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(provider.id(), "ollama:test-model");
        let embeddings = provider.embed(&["orders".into(), "users".into()]).unwrap();
        assert_eq!(embeddings, [[1.0, 0.0], [0.0, 1.0]]);
        assert_eq!(provider.locality(), Locality::Local);
        assert!(provider.capabilities().embeddings);
        assert!(provider.capabilities().chat);
        server.join().unwrap();
    }

    // #1653: only the port of the local endpoint varies, and the default port
    // is exactly the default endpoint.
    #[test]
    fn ollama_loopback_url_varies_only_the_port() {
        assert_eq!(
            OllamaProvider::loopback_url(OllamaProvider::DEFAULT_PORT),
            OllamaProvider::DEFAULT_URL
        );
        let provider = OllamaProvider::new(
            &OllamaProvider::loopback_url(11500),
            "m",
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            provider.bounded_profile().unwrap().endpoint_id,
            "http://127.0.0.1:11500/api/chat"
        );
    }

    // #1653: a chosen context window is closed, applied once and bound to the
    // provider id, so consent and provenance name it.
    #[test]
    fn ollama_context_window_is_closed_and_named_in_the_provider_id() {
        let provider =
            OllamaProvider::new(OllamaProvider::DEFAULT_URL, "m", Duration::from_secs(1)).unwrap();
        assert_eq!(provider.id(), "ollama:m");
        let chosen = provider.with_context_window(32768).unwrap();
        assert_eq!(chosen.id(), "ollama:m:num-ctx-32768");
        assert_eq!(
            chosen.bounded_profile().unwrap().provider_id,
            "ollama:m:num-ctx-32768"
        );
        assert!(chosen.with_context_window(8192).is_err());
        for refused in [0, 2048, 4096, 32767, 131072] {
            assert!(
                OllamaProvider::new(OllamaProvider::DEFAULT_URL, "m", Duration::from_secs(1))
                    .unwrap()
                    .with_context_window(refused)
                    .is_err()
            );
        }
    }

    #[test]
    fn ollama_client_disables_all_proxies() {
        // AC-0023: a Local provider stays physically local even if a system
        // or explicit reqwest proxy would otherwise capture loopback traffic.
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_address = target.local_addr().unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = target.accept().unwrap();
            let mut request = vec![0_u8; 8192];
            let size = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("POST /api/embed"));
            let body = r#"{"embeddings":[[1.0,0.0]]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let client = build_direct_loopback_client(
            Client::builder().proxy(
                reqwest::Proxy::all(format!("http://{proxy_address}")).expect("valid fake proxy"),
            ),
            Duration::from_secs(2),
        )
        .unwrap();
        let provider = OllamaProvider {
            base_url: Url::parse(&format!("http://{target_address}/")).unwrap(),
            embedding_model: "test-model".into(),
            completion_model: "test-model".into(),
            provider_id: "ollama:test-model".into(),
            bounded_thinking: false,
            bounded_context_window: None,
            client,
            bounded_client: bounded::build_client(Client::builder(), true).unwrap(),
        };
        assert_eq!(provider.embed(&["orders".into()]).unwrap(), [[1.0, 0.0]]);
        server.join().unwrap();

        proxy.set_nonblocking(true).unwrap();
        assert!(
            proxy
                .accept()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock),
            "the fake proxy must receive no loopback connection"
        );
    }

    #[test]
    fn ollama_chat_uses_local_api_after_firewall_preparation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = vec![0_u8; 16384];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("POST /api/chat HTTP/1.1"));
            assert!(request.contains("\"stream\":false"));
            assert!(request.contains("Resolve the explicit Gap"));
            assert!(request.contains("[REDACTED]"));
            assert!(!request.contains("super-secret"));
            let body = r#"{"message":{"role":"assistant","content":"{\"target_id\":\"chan:orders\"}"},"done":true}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let provider = OllamaProvider::new(
            &format!("http://{address}"),
            "test-model",
            Duration::from_secs(2),
        )
        .unwrap();
        let completion = EgressFirewall::new(EgressPolicy::local_only())
            .complete(&provider, &action("password=super-secret"), None)
            .unwrap();
        assert!(completion.text.contains("chan:orders"));
        server.join().unwrap();
    }

    #[test]
    fn local_only_policy_blocks_cloud_before_provider_invocation() {
        // AC-0023: local-only T3 cannot silently fall back to cloud.
        let provider = FakeCloud {
            calls: AtomicUsize::new(0),
        };
        let error = EgressFirewall::new(EgressPolicy::local_only())
            .complete(&provider, &action("safe"), None)
            .unwrap_err();
        assert!(matches!(error, ProviderError::EgressDenied { .. }));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cloud_consent_binds_exact_redacted_span_payload() {
        // AC-0024: the accepted preview is the payload sent, after redaction.
        let provider = FakeCloud {
            calls: AtomicUsize::new(0),
        };
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
        let request = action("password=super-secret");
        let error = firewall.complete(&provider, &request, None).unwrap_err();
        let ProviderError::ConsentRequired(preview) = error else {
            panic!("expected consent preview");
        };
        assert_eq!(preview.payload.spans.len(), 1);
        assert_eq!(preview.payload.spans[0].text, "password=[REDACTED]");
        assert_eq!(preview.redaction_count, 1);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

        let already_redacted = firewall
            .preview(&provider, &action("password=[REDACTED]"))
            .unwrap();
        assert_eq!(
            already_redacted.payload.spans[0].text,
            "password=[REDACTED]"
        );
        assert_eq!(already_redacted.redaction_count, 0);

        let grant = ConsentGrant::from_preview(&preview);
        firewall
            .complete(&provider, &request, Some(&grant))
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        // Changing only a secret value intentionally keeps the same outbound
        // redacted bytes. A real outbound content change invalidates consent.
        let changed = action("password=other-secret\nqueue=users");
        let error = firewall
            .complete(&provider, &changed, Some(&grant))
            .unwrap_err();
        assert!(matches!(error, ProviderError::ConsentMismatch));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cloud_egress_is_recorded_before_a_call_that_fails() {
        // AC-0580 / #1767: the record is written when the payload is handed
        // to the provider, so a failed call is counted like a successful one.
        let provider = FakeCloud {
            calls: AtomicUsize::new(0),
        };
        let recorded = Arc::new(Mutex::new(Vec::<(String, u64)>::new()));
        let sink = Arc::clone(&recorded);
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]))
            .with_egress_recorder(move |provider, bytes| {
                sink.lock().unwrap().push((provider.to_owned(), bytes));
                true
            });

        // No consent: nothing is sent and nothing is recorded.
        let failing = action("debug-1624-empty");
        let preview = firewall.preview(&provider, &failing).unwrap();
        assert!(firewall.complete(&provider, &failing, None).is_err());
        assert!(recorded.lock().unwrap().is_empty());

        // The provider is invoked and its answer is refused as empty.
        let grant = ConsentGrant::from_preview(&preview);
        let error = firewall
            .complete(&provider, &failing, Some(&grant))
            .unwrap_err();
        assert!(matches!(error, ProviderError::EmptyCompletion));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let expected = serde_json::to_vec(&preview.payload).unwrap().len() as u64;
        assert_eq!(
            *recorded.lock().unwrap(),
            [("cloud:test".to_owned(), expected)]
        );
    }

    #[test]
    fn cloud_call_is_not_made_when_its_egress_cannot_be_recorded() {
        // AC-0580 / #1767: fail closed, as for consent.
        let provider = FakeCloud {
            calls: AtomicUsize::new(0),
        };
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]))
            .with_egress_recorder(|_, _| false);
        let request = action("safe");
        let grant = ConsentGrant::from_preview(&firewall.preview(&provider, &request).unwrap());
        let error = firewall
            .complete(&provider, &request, Some(&grant))
            .unwrap_err();
        assert!(matches!(error, ProviderError::EgressUnrecorded { .. }));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ollama_embedding_identity_normalizes_bare_and_prefixed_digests() {
        // AC-0269: actual /api/tags uses bare64hex; both forms name one artifact.
        for prefix in ["", "sha256:"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let body=serde_json::json!({"models":[{"name":"test-model:latest","digest":format!("{prefix}{}","A".repeat(64))}]}).to_string();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let n = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..n]).starts_with("GET /api/tags "));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            });
            let provider = OllamaProvider::new(
                &format!("http://{address}"),
                "test-model",
                Duration::from_secs(2),
            )
            .unwrap();
            let identity = provider.embedding_cache_identity().unwrap().unwrap();
            assert_eq!(identity.digest, format!("sha256:{}", "a".repeat(64)));
            server.join().unwrap();
        }
    }
    #[test]
    fn ollama_embedding_decode_is_dimension_and_count_bounded() {
        // AC-0269: bounds apply while decoding, before unbounded vector allocation.
        let oversized = serde_json::json!({"embeddings":[vec![1.;4097]]}).to_string();
        assert!(serde_json::from_str::<EmbedResponse>(&oversized).is_err());
        let oversized = serde_json::json!({"embeddings":vec![vec![1.];129]}).to_string();
        assert!(serde_json::from_str::<EmbedResponse>(&oversized).is_err());
    }
    // AC-0422: tags require bounded 2xx JSON; redirects never authorize another request.
    #[test]
    fn local_metadata_probe_requires_successful_bounded_response() {
        for (status, body, valid) in [
            ("200 OK", r#"{"models":[]}"#.to_owned(), true),
            (
                "200 OK",
                r#"{"models":[{"name":"synthetic","digest":"abc"}]}"#.to_owned(),
                true,
            ),
            ("302 Found", r#"{"models":[]}"#.to_owned(), false),
            ("503 Unavailable", r#"{"models":[]}"#.to_owned(), false),
            ("200 OK", "PRIVATE malformed body".to_owned(), false),
            ("200 OK", serde_json::json!({"models":[{"name":"x".repeat(1024 * 1024),"digest":"synthetic"}]}).to_string(), false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let redirect = TcpListener::bind("127.0.0.1:0").unwrap();
            let redirect_address = redirect.local_addr().unwrap();
            redirect.set_nonblocking(true).unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let n = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..n]);
                assert!(request.starts_with("GET /api/tags HTTP/1.1\r\n"));
                assert!(!request.to_ascii_lowercase().contains("content-length:"));
                assert!(!request.contains("question") && !request.contains("task"));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nLocation: http://{redirect_address}/never-follow\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
            let provider = OllamaProvider::new(
                &format!("http://{address}"),
                "synthetic",
                Duration::from_secs(2),
            )
            .unwrap();
            assert_eq!(provider.list_local_models().is_ok(), valid);
            server.join().unwrap();
            assert!(
                matches!(redirect.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            thread::sleep(Duration::from_millis(200));
        });
        assert!(
            OllamaProvider::new(&endpoint, "synthetic", Duration::from_millis(30))
                .unwrap()
                .list_local_models()
                .is_err()
        );
        server.join().unwrap();
        assert!(
            OllamaProvider::new(&endpoint, "synthetic", Duration::from_millis(30))
                .unwrap()
                .list_local_models()
                .is_err()
        );
    }

    #[test]
    fn unavailable_local_provider_fails_without_fallback() {
        // AC-0021: an owned EOF endpoint fails closed without another fixture reusing its port.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            crate::bounded::tests::consume_request(&mut stream).unwrap();
            drop(stream); // Close after the full request, without any HTTP response.
            listener // Keep the endpoint reserved until the caller has observed the error.
        });
        let provider = OllamaProvider::new(
            &format!("http://{address}"),
            "test-model",
            Duration::from_millis(200),
        )
        .unwrap();
        let error = provider.embed(&["orders".into()]).unwrap_err();
        let _listener = server.join().unwrap();
        assert!(matches!(error, ProviderError::Request(_)), "{error:?}");
    }

    #[test]
    fn ollama_records_each_operation_model_apart_from_provider_identity() {
        // AC-0244 (#601 review): an embedding fact must not name the
        // completion model, and vice versa.
        let split = OllamaProvider::with_models(
            OllamaProvider::DEFAULT_URL,
            "nomic-embed-text",
            "qwen3:8b",
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(split.id(), "ollama:nomic-embed-text+qwen3:8b");
        assert_eq!(split.embedding_model_id(), "ollama:nomic-embed-text");
        assert_eq!(split.completion_model_id(), "ollama:qwen3:8b");
    }
}
