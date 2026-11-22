//! Anthropic Claude API provider (#123): the opt-in cloud reasoning lane.
//!
//! First cloud implementation of [`LlmProvider`] (ADR-0004). Three pinned
//! lanes: Haiku for cheap pre-ranking/triage, Opus as the default T3
//! reasoning lane, and Fable as the opt-in hard-reasoning lane for the most
//! demanding escalations. Discipline unchanged from the local path:
//!
//! - Requests are only ever built from a firewall-authorized, redacted
//!   [`ProviderCompletionRequest`] — there is no other code path from graph
//!   data to the network.
//! - `embed` is [`ProviderError::Unsupported`]: embeddings stay local
//!   (Ollama + usearch); Claude is the reasoning provider.
//! - A safety refusal is a typed outcome ([`ProviderError::Refused`]), and
//!   the Fable lane opts into a server-side fallback to Opus so a benign
//!   false positive degrades gracefully instead of failing the escalation.
//! - [`disclosure`] renders exactly what the Settings consent panel must
//!   show before consent (provider/model, endpoint, pricing, retention).
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]

use crate::{
    Completion, LlmProvider, Locality, ProviderCaps, ProviderCompletionRequest, ProviderError,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The production Messages API endpoint. Not configurable in release code:
/// consented payloads can only ever go to Anthropic.
pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
/// Server-side refusal-fallback beta (Fable lane).
const FALLBACK_BETA: &str = "server-side-fallback-2026-06-01";

/// The three cloud lanes. Model ids are pinned here the way the local
/// catalog pins Ollama models — changing one is a reviewed decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ClaudeLane {
    /// Cheap/fast: candidate pre-ranking, triage summaries.
    Haiku,
    /// Default reasoning lane for T3 proposals.
    Opus,
    /// Opt-in hard-reasoning lane for the most demanding escalations
    /// (long-horizon, multi-repo). Thinking is always on; requires 30-day
    /// data retention on the Anthropic org.
    Fable,
}

impl ClaudeLane {
    /// Exact API model id.
    pub fn model_id(self) -> &'static str {
        match self {
            // Dated snapshot, not the alias: pre-4.6 aliases are mutable
            // convenience pointers, and a pinned lane must not change model
            // behind a stable identity (review fix on #131).
            Self::Haiku => "claude-haiku-4-5-20251001",
            // #1709: re-pinned for the v1 frontier track (owner decision 2026-11-07).
            Self::Opus => "claude-opus-5-5",
            Self::Fable => "claude-fable-5",
        }
    }

    /// (input, output) USD per million tokens — consent-panel estimates.
    pub fn usd_per_mtok(self) -> (f64, f64) {
        match self {
            Self::Haiku => (1.0, 5.0),
            Self::Opus => (4.0, 20.0),
            Self::Fable => (10.0, 50.0),
        }
    }
}

/// Everything the fail-closed consent panel shows *before* consent
/// (design handoff §Settings; consumed by #112/#113/#118).
#[derive(Debug, Clone, Serialize)]
pub struct CloudDisclosure {
    pub provider: &'static str,
    pub model: &'static str,
    pub endpoint: &'static str,
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    /// Lane-specific caveats the user must see (e.g. Fable retention).
    pub notes: Vec<&'static str>,
}

/// The consent disclosure for one lane.
pub fn disclosure(lane: ClaudeLane) -> CloudDisclosure {
    let (input, output) = lane.usd_per_mtok();
    let mut notes = vec!["payload is the exact redacted span set shown by the egress firewall"];
    if lane == ClaudeLane::Fable {
        notes.push("requires 30-day data retention on the Anthropic organization");
        notes.push("safety refusals fall back server-side to claude-opus-5-5");
    }
    CloudDisclosure {
        provider: "Anthropic",
        model: lane.model_id(),
        endpoint: API_URL,
        input_usd_per_mtok: input,
        output_usd_per_mtok: output,
        notes,
    }
}

/// Bounds on one cloud completion outside the bounded lane (#1653,
/// AC-1806): how long the request may take and the output tokens it may ask
/// for. Both may only be lowered from the shipped values, because a longer
/// non-streaming request risks being cut off upstream. Any value other than
/// the default is named in the provider id, so consent and provenance record
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudCallLimits {
    timeout_secs: u64,
    max_tokens: u32,
}

impl CloudCallLimits {
    pub const MIN_TIMEOUT_SECS: u64 = 60;
    pub const MAX_TIMEOUT_SECS: u64 = 600;
    pub const MIN_MAX_TOKENS: u32 = 1_024;
    pub const MAX_MAX_TOKENS: u32 = 16_000;

    /// Limits inside the shipped bounds, or `None`.
    pub fn within(timeout_secs: u64, max_tokens: u32) -> Option<Self> {
        ((Self::MIN_TIMEOUT_SECS..=Self::MAX_TIMEOUT_SECS).contains(&timeout_secs)
            && (Self::MIN_MAX_TOKENS..=Self::MAX_MAX_TOKENS).contains(&max_tokens))
        .then_some(Self {
            timeout_secs,
            max_tokens,
        })
    }

    pub fn timeout_secs(self) -> u64 {
        self.timeout_secs
    }

    pub fn max_tokens(self) -> u32 {
        self.max_tokens
    }
}

impl Default for CloudCallLimits {
    fn default() -> Self {
        Self {
            timeout_secs: Self::MAX_TIMEOUT_SECS,
            max_tokens: Self::MAX_MAX_TOKENS,
        }
    }
}

/// Cloud provider for one Claude lane. Declared [`Locality::Cloud`], so the
/// egress firewall demands explicit consent bound to the exact payload
/// before [`LlmProvider::complete`] can ever run.
pub struct AnthropicProvider {
    lane: ClaudeLane,
    provider_id: String,
    api_key: String,
    endpoint: String,
    max_tokens: u32,
    client: Client,
    bounded_client: Client,
}

impl AnthropicProvider {
    /// Construct a provider for `lane`. The API key comes from the caller
    /// (OS keychain at the app layer) — it is never persisted here.
    pub fn new(lane: ClaudeLane, api_key: impl Into<String>) -> Result<Self, ProviderError> {
        Self::with_endpoint(lane, api_key, API_URL.to_string())
    }

    /// [`Self::new`] with the project's [`CloudCallLimits`].
    pub fn with_limits(
        lane: ClaudeLane,
        api_key: impl Into<String>,
        limits: CloudCallLimits,
    ) -> Result<Self, ProviderError> {
        Self::build(lane, api_key, API_URL.to_string(), limits)
    }

    /// Test seam: same provider against a local mock transport. Not
    /// reachable from release call sites, which pin [`API_URL`].
    #[doc(hidden)]
    pub fn with_endpoint(
        lane: ClaudeLane,
        api_key: impl Into<String>,
        endpoint: String,
    ) -> Result<Self, ProviderError> {
        Self::build(lane, api_key, endpoint, CloudCallLimits::default())
    }

    fn build(
        lane: ClaudeLane,
        api_key: impl Into<String>,
        endpoint: String,
        limits: CloudCallLimits,
    ) -> Result<Self, ProviderError> {
        let mut provider_id = format!("anthropic:{}", lane.model_id());
        if limits != CloudCallLimits::default() {
            provider_id.push_str(&format!(
                ":timeout-{}s:max-tokens-{}",
                limits.timeout_secs, limits.max_tokens
            ));
        }
        Ok(Self {
            provider_id,
            lane,
            api_key: api_key.into(),
            endpoint,
            max_tokens: limits.max_tokens,
            client: Client::builder()
                .timeout(Duration::from_secs(limits.timeout_secs))
                .build()?,
            bounded_client: crate::bounded::build_client(Client::builder(), false)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_test_bounded_client(mut self, client: Client) -> Self {
        self.bounded_client = client;
        self
    }

    fn request_body(&self, request: &ProviderCompletionRequest) -> serde_json::Value {
        // Evidence spans travel verbatim — the firewall already showed the
        // user this exact payload; reshaping it here would break the
        // consent hash contract.
        let spans = request
            .spans()
            .iter()
            .map(|span| format!("[{}] {}", span.id, span.text))
            .collect::<Vec<_>>()
            .join("\n");
        let content = format!("{}\n\n{spans}", request.prompt());
        let mut body = serde_json::json!({
            "model": self.lane.model_id(),
            "max_tokens": self.max_tokens,
            "system": request.system(),
            "messages": [{ "role": "user", "content": content }],
        });
        if self.lane == ClaudeLane::Fable {
            // Thinking is always on for Fable (no `thinking` param), and a
            // policy refusal re-runs server-side on Opus. The beta opt-in
            // travels as the `anthropic-beta` header (see `complete`).
            body["fallbacks"] = serde_json::json!([{ "model": ClaudeLane::Opus.model_id() }]);
        }
        body
    }
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    stop_details: Option<StopDetails>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct StopDetails {
    category: Option<String>,
}

impl LlmProvider for AnthropicProvider {
    fn bounded_profile(
        &self,
    ) -> Result<crate::bounded::ProviderProfile, crate::bounded::BoundedCallError> {
        // The investigation protocol has no server-side fallback or alternate
        // thinking-model lane. Legacy Fable completion remains unchanged.
        if self.lane == ClaudeLane::Fable {
            return Err(crate::bounded::BoundedCallError::unsupported());
        }
        Ok(crate::bounded::ProviderProfile {
            protocol_version: crate::bounded::PROTOCOL_VERSION.into(),
            provider_id: self.provider_id.clone(),
            locality: Locality::Cloud,
            endpoint_id: self.endpoint.clone(),
            requested_model: self.lane.model_id().into(),
        })
    }

    fn complete_bounded(
        &self,
        request: &crate::bounded::AuthorizedBoundedCompletion,
        control: &crate::bounded::CompletionControl<'_>,
    ) -> Result<crate::bounded::BoundedCompletion, crate::bounded::BoundedCallError> {
        crate::bounded::complete_anthropic(
            &self.bounded_client,
            &self.api_key,
            &self.bounded_profile()?,
            request,
            control,
        )
    }

    /// Bounded schemas travel as `output_config.format` (#1709). Fable has no
    /// bounded profile at all, so this never widens its surface.
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }

    /// The key, for the host's loopback proxy in front of the Claude lane's
    /// runtime (ADR-0085 decision 7). Only lanes with a bounded profile.
    fn proxy_credential(&self) -> Option<crate::UpstreamCredential> {
        (self.lane != ClaudeLane::Fable)
            .then(|| crate::UpstreamCredential::new("x-api-key", self.api_key.clone()))
    }

    fn id(&self) -> &str {
        &self.provider_id
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
        // Embeddings stay local (Ollama + usearch): the cloud lane exists
        // for reasoning, and bulk evidence never leaves for similarity.
        Err(ProviderError::Unsupported(
            "embeddings are local-only; Claude is the reasoning provider",
        ))
    }

    fn complete(&self, request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        let mut builder = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION);
        if self.lane == ClaudeLane::Fable {
            // The server-side fallback opt-in is a header, not a body field;
            // `fallbacks` without it is rejected (review fix on #131).
            builder = builder.header("anthropic-beta", FALLBACK_BETA);
        }
        let response = builder
            .json(&self.request_body(request))
            .send()?
            .error_for_status()?;
        let message: MessagesResponse = response.json()?;
        if message.stop_reason.as_deref() == Some("refusal") {
            return Err(ProviderError::Refused {
                provider: self.provider_id.clone(),
                category: message.stop_details.and_then(|details| details.category),
            });
        }
        if message.stop_reason.as_deref() == Some("max_tokens") {
            // A cut-off answer is never handed on, even when what arrived
            // still parses (AC-1806).
            return Err(ProviderError::InvalidResponse(
                "the answer reached the output token limit".into(),
            ));
        }
        let text: String = message
            .content
            .iter()
            .filter(|block| block.kind == "text")
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("");
        // The firewall rejects empty completions after this returns; the
        // provider's own contract is just faithful transport.
        Ok(Completion { text })
    }
}

/// The metadata request a connection check makes: the first entry of the
/// provider's model list. It has no body.
pub const CONNECTION_CHECK_URL: &str = "https://api.anthropic.com/v1/models?limit=1";

/// What a connection check learned (#1795). Never carries the key or any
/// part of the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionOutcome {
    /// The provider answered and accepted the key.
    Accepted,
    /// The provider answered and refused the key (HTTP 401 or 403).
    Refused,
    /// No usable answer: no connection, a timeout, or any other status.
    Unreachable,
}

/// Bytes of the connection-check request that Cartograph itself composes:
/// the request line and the two headers it sets, the key included. The check
/// has no body, so this is everything of the user's that leaves.
pub fn connection_check_bytes(endpoint: &str, api_key: &str) -> u64 {
    let target = request_target(endpoint);
    let request = format!(
        "GET {target} HTTP/1.1\r\nx-api-key: {api_key}\r\nanthropic-version: {API_VERSION}\r\n\r\n"
    );
    request.len() as u64
}

/// The request-line target of a connection check: path and query.
pub(crate) fn request_target(endpoint: &str) -> String {
    reqwest::Url::parse(endpoint)
        .map(|url| match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        })
        .unwrap_or_else(|_| endpoint.to_owned())
}

/// Send one bodiless check request and classify the status. Redirects are
/// not followed and the response body is never read. Fails closed.
pub(crate) fn classify_check(
    timeout: Duration,
    request: impl FnOnce(&Client) -> reqwest::blocking::RequestBuilder,
) -> ConnectionOutcome {
    let Ok(client) = Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return ConnectionOutcome::Unreachable;
    };
    match request(&client).send().map(|response| response.status()) {
        Ok(status) if status.is_success() => ConnectionOutcome::Accepted,
        Ok(status) if matches!(status.as_u16(), 401 | 403) => ConnectionOutcome::Refused,
        _ => ConnectionOutcome::Unreachable,
    }
}

/// Ask the provider whether it accepts `api_key`, with one bodiless metadata
/// request. No prompt, source text or graph content is sent, redirects are
/// not followed and the response body is never read. Fails closed: anything
/// but a success status is not an acceptance.
pub fn check_connection(endpoint: &str, api_key: &str, timeout: Duration) -> ConnectionOutcome {
    classify_check(timeout, |client| {
        client
            .get(endpoint)
            .header("x-api-key", api_key)
            .header("anthropic-version", API_VERSION)
    })
}

use crate::Embedding;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AnalysisTier, CompletionAction, CompletionPayload, EgressFirewall, EgressPolicy,
        PayloadSpan,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// One-shot local mock of the Messages API: accepts a single request,
    /// captures its body, answers with `response_json`.
    fn mock_api(response_json: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 65536];
            let read = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response_json.len(),
                response_json
            );
            stream.write_all(reply.as_bytes()).unwrap();
            request
        });
        (endpoint, handle)
    }

    /// The one legal path to a cloud completion: the real firewall, with a
    /// consent grant bound to the exact payload.
    fn action() -> CompletionAction {
        CompletionAction {
            action_id: "propose-gap-target".into(),
            tier: AnalysisTier::Agentic,
            payload: CompletionPayload {
                response_schema: None,
                system: "You are a bounded resolver.".into(),
                prompt: "Cite only listed candidates.".into(),
                spans: vec![PayloadSpan {
                    id: "span-1".into(),
                    repo: "local/app".into(),
                    path: "src/queue.ts".into(),
                    byte_start: 0,
                    byte_end: 24,
                    commit_sha: "workdir".into(),
                    text: "queueUrl = env.ORDERS".into(),
                }],
            },
        }
    }

    fn complete_with_consent(
        provider: &AnthropicProvider,
    ) -> Result<crate::Completion, ProviderError> {
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
        let action = action();
        let preview = firewall.preview(provider, &action).expect("previewable");
        let grant = crate::ConsentGrant::from_preview(&preview);
        firewall.complete(provider, &action, Some(&grant))
    }

    #[test]
    fn completes_through_firewall_with_pinned_model_and_verbatim_spans() {
        let (endpoint, server) = mock_api(
            r#"{"content":[{"type":"text","text":"{\"target\":\"ch:orders\"}"}],"stop_reason":"end_turn"}"#,
        );
        let provider =
            AnthropicProvider::with_endpoint(ClaudeLane::Opus, "sk-test", endpoint).unwrap();
        let completion = complete_with_consent(&provider).unwrap();
        assert!(completion.text.contains("ch:orders"));

        let raw = server.join().unwrap();
        // Pinned model, auth header, and the exact consented span text.
        assert!(raw.contains("claude-opus-5-5"));
        assert!(raw.contains("x-api-key: sk-test"));
        assert!(raw.contains("queueUrl = env.ORDERS"));
        // Opus lane sends no fallback beta.
        assert!(!raw.contains("server-side-fallback"));
    }

    // AC-1806 / #1653: lowered limits reach the request and the provider id,
    // the defaults leave the id as it was, and nothing above the shipped
    // bounds is accepted.
    #[test]
    fn cloud_call_limits_reach_the_request_and_the_provider_id() {
        assert_eq!(
            CloudCallLimits::within(600, 16_000),
            Some(CloudCallLimits::default())
        );
        for (timeout, tokens) in [(59, 16_000), (601, 16_000), (600, 1_023), (600, 16_001)] {
            assert_eq!(CloudCallLimits::within(timeout, tokens), None);
        }
        let default = AnthropicProvider::new(ClaudeLane::Opus, "sk-test").unwrap();
        assert_eq!(default.id(), "anthropic:claude-opus-5-5");

        let (endpoint, server) = mock_api(
            r#"{"content":[{"type":"text","text":"{\"target\":\"ch:orders\"}"}],"stop_reason":"end_turn"}"#,
        );
        let limits = CloudCallLimits::within(120, 4_096).unwrap();
        let provider =
            AnthropicProvider::build(ClaudeLane::Opus, "sk-test", endpoint, limits).unwrap();
        assert_eq!(
            provider.id(),
            "anthropic:claude-opus-5-5:timeout-120s:max-tokens-4096"
        );
        complete_with_consent(&provider).unwrap();
        let raw = server.join().unwrap();
        assert!(raw.contains("\"max_tokens\":4096"), "{raw}");
    }

    // AC-1806: an answer stopped at the output token limit is refused even
    // when its text still parses.
    #[test]
    fn a_completion_cut_off_at_the_token_limit_is_refused() {
        let (endpoint, server) = mock_api(
            r#"{"content":[{"type":"text","text":"{\"target\":\"ch:orders\"}"}],"stop_reason":"max_tokens"}"#,
        );
        let provider =
            AnthropicProvider::with_endpoint(ClaudeLane::Opus, "sk-test", endpoint).unwrap();
        assert!(matches!(
            complete_with_consent(&provider),
            Err(ProviderError::InvalidResponse(reason)) if reason.contains("output token limit")
        ));
        server.join().unwrap();
    }

    #[test]
    fn cloud_completion_without_consent_fails_closed() {
        // No mock server: the request must be refused before any transport.
        let provider = AnthropicProvider::new(ClaudeLane::Opus, "sk-test").unwrap();
        let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
        let error = firewall
            .complete(&provider, &action(), None)
            .expect_err("cloud without consent is impossible");
        assert!(matches!(error, ProviderError::ConsentRequired(_)));
    }

    #[test]
    fn fable_lane_opts_into_server_side_fallback_and_refusals_are_typed() {
        let (endpoint, server) = mock_api(
            r#"{"content":[],"stop_reason":"refusal","stop_details":{"category":"cyber"}}"#,
        );
        let provider =
            AnthropicProvider::with_endpoint(ClaudeLane::Fable, "sk-test", endpoint).unwrap();
        let error = complete_with_consent(&provider).expect_err("refusal is typed");
        match error {
            ProviderError::Refused { provider, category } => {
                assert_eq!(provider, "anthropic:claude-fable-5");
                assert_eq!(category.as_deref(), Some("cyber"));
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        let raw = server.join().unwrap();
        assert!(raw.contains("claude-fable-5"));
        // The beta opt-in travels as a header — `fallbacks` in the body is
        // rejected without it.
        assert!(raw.contains(&format!("anthropic-beta: {FALLBACK_BETA}")));
        assert!(raw.contains("claude-opus-5-5")); // fallback target in body
        assert!(!raw.contains("\"betas\"")); // never a body field
        assert!(!raw.contains("\"thinking\"")); // always-on: param omitted
    }

    // AC-1424 (ADR-0085 decision 7): the proxy, not the runtime, holds the
    // key; Fable has no bounded lane and so no proxy credential.
    #[test]
    fn only_bounded_lanes_give_the_proxy_their_key_and_debug_hides_it() {
        let opus = AnthropicProvider::new(ClaudeLane::Opus, "sk-proxy").unwrap();
        let credential = opus.proxy_credential().unwrap();
        assert_eq!(credential.header(), "x-api-key");
        assert_eq!(credential.value(), "sk-proxy");
        assert!(!format!("{credential:?}").contains("sk-proxy"));
        let fable = AnthropicProvider::new(ClaudeLane::Fable, "sk-proxy").unwrap();
        assert!(fable.proxy_credential().is_none());
    }

    #[test]
    fn embeddings_are_refused_and_disclosure_is_complete() {
        let provider = AnthropicProvider::new(ClaudeLane::Haiku, "sk-test").unwrap();
        assert!(matches!(
            provider.embed(&["text".into()]),
            Err(ProviderError::Unsupported(_))
        ));
        assert_eq!(provider.locality(), Locality::Cloud);

        for lane in [ClaudeLane::Haiku, ClaudeLane::Opus, ClaudeLane::Fable] {
            let disclosure = disclosure(lane);
            assert_eq!(disclosure.provider, "Anthropic");
            assert_eq!(disclosure.endpoint, API_URL);
            assert!(disclosure.input_usd_per_mtok > 0.0);
        }
        // The Fable lane discloses its retention requirement before consent.
        assert!(
            disclosure(ClaudeLane::Fable)
                .notes
                .iter()
                .any(|note| note.contains("30-day data retention"))
        );
    }

    /// One-shot mock that answers any request with `status` and returns the
    /// raw request it received.
    fn mock_status(status: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}/v1/models?limit=1",
            listener.local_addr().unwrap()
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 65536];
            let read = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let body = r#"{"data":[{"id":"secret-model-name"}]}"#;
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            request
        });
        (endpoint, handle)
    }

    // AC-0590 (#1795): the check is one bodiless GET carrying the key and
    // nothing of the user's sources, and it tells three outcomes apart.
    #[test]
    fn connection_check_sends_a_bodiless_metadata_request_and_classifies_the_answer() {
        let timeout = Duration::from_secs(5);
        let (endpoint, server) = mock_status("200 OK");
        assert_eq!(
            check_connection(&endpoint, "sk-test-key", timeout),
            ConnectionOutcome::Accepted
        );
        let request = server.join().unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("GET /v1/models?limit=1 HTTP/1.1"));
        assert!(head.to_ascii_lowercase().contains("x-api-key: sk-test-key"));
        assert!(body.is_empty(), "the check has no body");
        for field in ["messages", "prompt", "system", "span", "content-length: "] {
            assert!(!request.to_ascii_lowercase().contains(field), "{field}");
        }

        for (status, expected) in [
            ("401 Unauthorized", ConnectionOutcome::Refused),
            ("403 Forbidden", ConnectionOutcome::Refused),
            ("500 Internal Server Error", ConnectionOutcome::Unreachable),
            ("429 Too Many Requests", ConnectionOutcome::Unreachable),
            ("302 Found", ConnectionOutcome::Unreachable),
        ] {
            let (endpoint, server) = mock_status(status);
            assert_eq!(
                check_connection(&endpoint, "sk-test-key", timeout),
                expected
            );
            server.join().unwrap();
        }

        // Nothing listening: unreachable, never a pass. The port stays
        // reserved, so no parallel test's server receives this probe.
        let (_reserved, closed) = crate::bounded::tests::refused_endpoint();
        let endpoint = format!("http://{closed}/v1/models?limit=1");
        assert_eq!(
            check_connection(&endpoint, "sk-test-key", timeout),
            ConnectionOutcome::Unreachable
        );
    }

    #[test]
    fn connection_check_bytes_count_the_request_line_and_the_key() {
        let short = connection_check_bytes(CONNECTION_CHECK_URL, "k");
        let long = connection_check_bytes(CONNECTION_CHECK_URL, "k".repeat(101).as_str());
        assert_eq!(long - short, 100);
        assert!(short > "GET /v1/models?limit=1 HTTP/1.1".len() as u64);
    }
}
