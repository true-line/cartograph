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
