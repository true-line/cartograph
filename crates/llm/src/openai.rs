//! Hosted OpenAI provider (#1709): the GPT side of the frontier acceptance
//! track. It is bounded-only and declared [`Locality::Cloud`], so the egress
//! firewall requires consent bound to the exact payload before
//! [`LlmProvider::complete_bounded`] can run.
//!
//! - The endpoint is pinned to [`API_URL`]. Consented payloads can only go to
//!   OpenAI.
//! - Every request sends `store: false`. The account's own API data-retention
//!   terms still apply, and the consent disclosure must state them.
//! - Response schemas travel as the strict projection from
//!   `request_wire::openai_cloud_schema`. The host validates every response
//!   against the complete original schema.
//! - The legacy unbounded completion path is unsupported. Investigations use
//!   only the bounded path.

use crate::{
    Completion, Embedding, LlmProvider, Locality, ProviderCaps, ProviderCompletionRequest,
    ProviderError, bounded,
};
use reqwest::blocking::Client;

/// The production Chat Completions endpoint. Not configurable in release code.
pub const API_URL: &str = "https://api.openai.com/v1/chat/completions";

/// The hosted GPT model that takes the investigation output allowance
/// ([`crate::bounded::HOSTED_OUTPUT_TOKENS`]). Its reasoning tokens count toward
/// `max_completion_tokens`, so the static 2,048 cap starves the finish turn.
pub const HOSTED_ALLOWANCE_MODEL: &str = "gpt-6.1-sol";

/// Cloud provider for one hosted OpenAI model. The model id is the caller's
/// reviewed choice and is bound into the provider id and bounded profile.
pub struct OpenAiProvider {
    model: String,
    provider_id: String,
    api_key: String,
    endpoint: String,
    bounded_client: Client,
}

impl OpenAiProvider {
    /// Construct a provider for `model` against the pinned [`API_URL`]. The API
    /// key comes from the caller and is never persisted or logged here.
    pub fn new(
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        Self::build(model.into(), api_key.into(), API_URL.to_string())
    }

    /// Test-only seam: the same provider against a local fixture transport.
    /// Release builds cannot select another destination for the bearer key.
    #[cfg(test)]
    pub(crate) fn with_endpoint(
        model: impl Into<String>,
        api_key: impl Into<String>,
        endpoint: String,
    ) -> Result<Self, ProviderError> {
        Self::build(model.into(), api_key.into(), endpoint)
    }

    fn build(model: String, api_key: String, endpoint: String) -> Result<Self, ProviderError> {
        Ok(Self {
            provider_id: format!("openai:{model}"),
            model,
            api_key,
            endpoint,
            bounded_client: bounded::build_client(Client::builder(), false)?,
        })
    }
}

impl LlmProvider for OpenAiProvider {
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
        Err(ProviderError::Unsupported("embeddings"))
    }

    fn completion_model_id(&self) -> String {
        self.model.clone()
    }

    fn complete(&self, _request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        Err(ProviderError::Unsupported("unbounded completion"))
    }

    fn supports_bounded_response_schema(&self) -> bool {
        true
    }

    fn bounded_profile(&self) -> Result<bounded::ProviderProfile, bounded::BoundedCallError> {
        let profile = bounded::ProviderProfile {
            protocol_version: bounded::PROTOCOL_VERSION.into(),
            provider_id: self.provider_id.clone(),
            locality: Locality::Cloud,
            endpoint_id: self.endpoint.clone(),
            requested_model: self.model.clone(),
        };
        Ok(profile)
    }

    fn complete_bounded(
        &self,
        request: &bounded::AuthorizedBoundedCompletion,
        control: &bounded::CompletionControl<'_>,
    ) -> Result<bounded::BoundedCompletion, bounded::BoundedCallError> {
        bounded::complete_openai_cloud(
            &self.bounded_client,
            &self.api_key,
            &self.bounded_profile()?,
            request,
            control,
        )
    }
}

pub use crate::anthropic::ConnectionOutcome;

/// The metadata request an OpenAI connection check makes: the model list.
/// It has no body. Pinned like [`API_URL`].
pub const CONNECTION_CHECK_URL: &str = "https://api.openai.com/v1/models";

/// Bytes of the connection-check request that Cartograph itself composes:
/// the request line and the bearer header, the key included. The check has
/// no body, so this is everything of the user's that leaves.
pub fn connection_check_bytes(endpoint: &str, api_key: &str) -> u64 {
    let target = crate::anthropic::request_target(endpoint);
    let request = format!("GET {target} HTTP/1.1\r\nauthorization: Bearer {api_key}\r\n\r\n");
    request.len() as u64
}

/// Ask OpenAI whether it accepts `api_key`, with one bodiless request for
/// the model list (#1709). No prompt, source text or graph content is sent,
/// redirects are not followed and the response body is never read. Fails
/// closed: anything but a success status is not an acceptance.
pub fn check_connection(
    endpoint: &str,
    api_key: &str,
    timeout: std::time::Duration,
) -> ConnectionOutcome {
    crate::anthropic::classify_check(timeout, |client| client.get(endpoint).bearer_auth(api_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    /// One-shot loopback server answering `status` with a body the check
    /// must never read back.
    fn mock_status(status: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1/models", listener.local_addr().unwrap());
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

    // AC-0750 (#1709): the OpenAI check is one bodiless GET of the model list
    // carrying the bearer key and nothing else, with three outcomes.
    #[test]
    fn openai_connection_check_sends_a_bodiless_bearer_request_and_classifies_the_answer() {
        assert_eq!(CONNECTION_CHECK_URL, "https://api.openai.com/v1/models");
        let timeout = Duration::from_secs(5);
        let (endpoint, server) = mock_status("200 OK");
        assert_eq!(
            check_connection(&endpoint, "sk-proj-test", timeout),
            ConnectionOutcome::Accepted
        );
        let request = server.join().unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("GET /v1/models HTTP/1.1"));
        let head = head.to_ascii_lowercase();
        assert!(head.contains("authorization: bearer sk-proj-test"));
        assert!(!head.contains("x-api-key"), "no Anthropic header");
        assert!(body.is_empty(), "the check has no body");
        for field in ["messages", "prompt", "span", "content-length: "] {
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
                check_connection(&endpoint, "sk-proj-test", timeout),
                expected
            );
            server.join().unwrap();
        }

        let (_reserved, closed) = crate::bounded::tests::refused_endpoint();
        let endpoint = format!("http://{closed}/v1/models");
        assert_eq!(
            check_connection(&endpoint, "sk-proj-test", timeout),
            ConnectionOutcome::Unreachable
        );
    }

    #[test]
    fn openai_connection_check_bytes_count_the_request_line_and_the_key() {
        let short = connection_check_bytes(CONNECTION_CHECK_URL, "k");
        let long = connection_check_bytes(CONNECTION_CHECK_URL, "k".repeat(101).as_str());
        assert_eq!(long - short, 100);
        assert!(short > "GET /v1/models HTTP/1.1".len() as u64);
    }
}
