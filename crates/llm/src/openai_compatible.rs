//! Local OpenAI-compatible chat provider (#938): an owner-hosted server such
//! as vLLM, LiteLLM or llama.cpp speaking `/v1/chat/completions` and
//! `/v1/models`.
//!
//! Like [`crate::OllamaProvider`] it is declared [`Locality::Local`], so its
//! base URL must be plain-HTTP loopback and its transport never uses a proxy.
//! A server elsewhere on the owner's network is reached through a loopback
//! tunnel (`ssh -L 8000:127.0.0.1:8000 host`), which keeps the payload's
//! route an explicit operator choice. It has no embeddings and never sends an
//! API key.

use crate::{
    Completion, Embedding, LlmProvider, Locality, ProviderCaps, ProviderCompletionRequest,
    ProviderError, bounded, build_direct_loopback_client, loopback_base,
};
use reqwest::Url;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::Duration;

/// Response bytes read from `/v1/models` or a legacy completion.
const METADATA_LIMIT: u64 = 1024 * 1024;

/// Local OpenAI-compatible chat provider.
pub struct OpenAiCompatibleProvider {
    base_url: Url,
    model: String,
    provider_id: String,
    client: Client,
    disable_thinking: bool,
    pub(crate) bounded_client: Client,
}

impl OpenAiCompatibleProvider {
    /// Default loopback endpoint of vLLM's OpenAI-compatible server.
    pub const DEFAULT_URL: &'static str = "http://127.0.0.1:8000/";

    /// Construct a loopback-only provider for `model`, served under
    /// `{base_url}v1/`.
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let base_url = loopback_base(base_url)?;
        let model = model.into();
        Ok(Self {
            provider_id: format!("openai-compatible:{model}"),
            base_url,
            model,
            disable_thinking: false,
            client: build_direct_loopback_client(Client::builder(), timeout)?,
            bounded_client: bounded::build_client(Client::builder(), true)?,
        })
    }

    /// Opt in to the local server's chat-template switch for bounded calls.
    /// The option is part of provider identity and the bounded request bytes.
    pub fn with_thinking_disabled(mut self) -> Self {
        self.disable_thinking = true;
        self.provider_id = format!("openai-compatible:{}:thinking-disabled", self.model);
        self
    }

    /// Model ids the server lists. An OpenAI-compatible server reports no
    /// weight digest, so the id names a served model, not proven weights.
    pub fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        #[derive(Deserialize)]
        struct Models {
            data: Vec<Model>,
        }
        #[derive(Deserialize)]
        struct Model {
            id: String,
        }
        let url = self.join("v1/models")?;
        let response = self.client.get(url).send()?.error_for_status()?;
        let models: Models = serde_json::from_slice(&read_capped(response)?)?;
        Ok(models.data.into_iter().map(|model| model.id).collect())
    }

    fn join(&self, path: &str) -> Result<Url, ProviderError> {
        self.base_url
            .join(path)
            .map_err(|error| ProviderError::InvalidUrl(error.to_string()))
    }
}

fn read_capped(response: reqwest::blocking::Response) -> Result<Vec<u8>, ProviderError> {
    let mut bytes = Vec::new();
    response
        .take(METADATA_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
    if bytes.len() as u64 > METADATA_LIMIT {
        return Err(ProviderError::InvalidResponse("response limit".into()));
    }
    Ok(bytes)
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 2],
    stream: bool,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

impl LlmProvider for OpenAiCompatibleProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }

    fn locality(&self) -> Locality {
        Locality::Local
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
        self.provider_id.clone()
    }

    fn complete(&self, request: &ProviderCompletionRequest) -> Result<Completion, ProviderError> {
        let user = request.user_message()?;
        let response = self
            .client
            .post(self.join("v1/chat/completions")?)
            .json(&ChatRequest {
                model: &self.model,
                messages: [
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
                response_format: ResponseFormat {
                    kind: "json_object",
                },
            })
            .send()?
            .error_for_status()?;
        // The bounded path's envelope admission, so a truncated, refused or
        // tool-calling response is never a completion here either.
        let (text, ..) =
            bounded::openai_compatible_envelope(&read_capped(response)?).map_err(|error| {
                match error.code {
                    bounded::FailureCode::Refused => ProviderError::Refused {
                        provider: self.provider_id.clone(),
                        category: None,
                    },
                    code => ProviderError::InvalidResponse(format!("{code:?}")),
                }
            })?;
        Ok(Completion { text })
    }

    fn supports_bounded_response_schema(&self) -> bool {
        true
    }

    fn bounded_profile(&self) -> Result<bounded::ProviderProfile, bounded::BoundedCallError> {
        bounded::openai_compatible_profile(self.id(), &self.base_url, &self.model)
    }

    fn complete_bounded(
        &self,
        request: &bounded::AuthorizedBoundedCompletion,
        control: &bounded::CompletionControl<'_>,
    ) -> Result<bounded::BoundedCompletion, bounded::BoundedCallError> {
        bounded::complete_openai_compatible(
            &self.bounded_client,
            &self.bounded_profile()?,
            self.disable_thinking,
            request,
            control,
        )
    }
}
