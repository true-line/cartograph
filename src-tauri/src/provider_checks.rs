//! Connection checks for the provider lanes (#1795).
//!
//! - The cloud check asks the provider whether it accepts the configured key
//!   with one bodiless metadata request. Nothing from the user's sources is
//!   sent, so it needs no tier consent, but the request does leave the
//!   machine, so it is written to the egress record before it is sent.
//! - The local check reports whether the local model server answers and which
//!   catalog models are installed, with the command that fetches each missing
//!   one. Cartograph never pulls a model itself.
//! - Both are bounded by a timeout and neither result carries a key or any
//!   part of a response body.

use crate::provider_credentials::{ProviderCredentials, ProviderId};
use llm::anthropic::ConnectionOutcome;
use llm::catalog::{LocalModel, ModelAction, model_for};
use serde::Serialize;
use std::time::Duration;

/// How long either check may take.
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The egress record's provider label for a connection check, so the record
/// shows these requests apart from completions.
pub(crate) fn check_label(provider: ProviderId) -> String {
    format!("{}:connection-check", provider.id())
}

/// What a cloud connection check found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ConnectionCheck {
    pub provider: &'static str,
    /// `accepted`, `refused`, `unreachable`, or `no-key` when there is no key
    /// to check and nothing was sent.
    pub outcome: &'static str,
    /// Bytes written to the egress record for this check; zero when nothing
    /// was sent.
    pub bytes_sent: u64,
}

/// The pinned model-list URL a provider's connection check asks. Each key
/// goes only to its own provider (#1709).
pub(crate) fn check_url(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Anthropic => llm::anthropic::CONNECTION_CHECK_URL,
        ProviderId::OpenAi => llm::openai::CONNECTION_CHECK_URL,
    }
}

/// Check the configured key against `endpoint`. `record` writes the egress
/// record and must succeed before anything is sent (fail closed).
pub(crate) fn check_connection_with(
    credentials: &ProviderCredentials,
    provider: ProviderId,
    endpoint: &str,
    timeout: Duration,
    record: impl FnOnce(&str, u64) -> bool,
) -> Result<ConnectionCheck, String> {
    let Some(key) = credentials.resolve(provider)? else {
        return Ok(ConnectionCheck {
            provider: provider.id(),
            outcome: "no-key",
            bytes_sent: 0,
        });
    };
    let bytes = match provider {
        ProviderId::Anthropic => llm::anthropic::connection_check_bytes(endpoint, &key),
        ProviderId::OpenAi => llm::openai::connection_check_bytes(endpoint, &key),
    };
    if !record(&check_label(provider), bytes) {
        return Err("the egress record could not be written, so nothing was sent".into());
    }
    let outcome = match provider {
        ProviderId::Anthropic => llm::anthropic::check_connection(endpoint, &key, timeout),
        ProviderId::OpenAi => llm::openai::check_connection(endpoint, &key, timeout),
    };
    let outcome = match outcome {
        ConnectionOutcome::Accepted => "accepted",
        ConnectionOutcome::Refused => "refused",
        ConnectionOutcome::Unreachable => "unreachable",
    };
    Ok(ConnectionCheck {
        provider: provider.id(),
        outcome,
        bytes_sent: bytes,
    })
}

/// One catalog model on the local lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LocalModelStatus {
    pub model: &'static str,
    pub purpose: &'static str,
    pub installed: bool,
    /// The command that fetches the model; present only when it is missing.
    pub pull_command: Option<String>,
}

/// Whether the local model server answers and which catalog models it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LocalLaneStatus {
    pub reachable: bool,
    /// The command that starts the server; present only when it did not answer.
    pub start_command: Option<&'static str>,
    /// Every catalog model, in catalog order. Empty when the server did not
    /// answer, because nothing is known about what is installed.
    pub models: Vec<LocalModelStatus>,
}

const CATALOG_ACTIONS: [ModelAction; 3] = [
    ModelAction::Embedding,
    ModelAction::Triage,
    ModelAction::Proposal,
];

/// Ollama reports `name:tag`; an untagged catalog pin matches any tag.
fn is_installed(pinned: &str, installed: &[LocalModel]) -> bool {
    installed.iter().any(|model| {
        model.name == pinned
            || model
                .name
                .split_once(':')
                .is_some_and(|(name, _)| name == pinned)
    })
}

/// Classify a probe of the local server. `None` means it did not answer.
pub(crate) fn local_lane_status_from(installed: Option<&[LocalModel]>) -> LocalLaneStatus {
    let Some(installed) = installed else {
        return LocalLaneStatus {
            reachable: false,
            start_command: Some("ollama serve"),
            models: Vec::new(),
        };
    };
    let models = CATALOG_ACTIONS
        .into_iter()
        .map(|action| {
            let spec = model_for(action);
            let present = is_installed(spec.model, installed);
            LocalModelStatus {
                model: spec.model,
                purpose: spec.purpose,
                installed: present,
                pull_command: (!present).then(|| format!("ollama pull {}", spec.model)),
            }
        })
        .collect();
    LocalLaneStatus {
        reachable: true,
        start_command: None,
        models,
    }
}

/// Probe the local server on its loopback address at the saved `port` (#1653).
pub(crate) fn probe_local_lane(port: u16) -> LocalLaneStatus {
    let installed = llm::OllamaProvider::with_models(
        &llm::OllamaProvider::loopback_url(port),
        llm::OllamaProvider::DEFAULT_EMBEDDING_MODEL,
        llm::OllamaProvider::DEFAULT_COMPLETION_MODEL,
        CHECK_TIMEOUT,
    )
    .and_then(|provider| provider.list_local_models())
    .ok();
    local_lane_status_from(installed.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_credentials::for_test;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    const KEY: &str = "sk-ant-test-0123456789abcdef";

    fn mock(status: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}/v1/models?limit=1",
            listener.local_addr().unwrap()
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 65536];
            let read = stream.read(&mut buffer).unwrap();
            let reply =
                format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            stream.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&buffer[..read]).into_owned()
        });
        (endpoint, handle)
    }

    // AC-0590: the check is recorded before it is sent, with its byte count
    // and its own label, and the result never carries the key.
    #[test]
    fn a_connection_check_is_recorded_with_its_bytes_and_never_returns_the_key() {
        let credentials = for_test(Some(KEY));
        let (endpoint, server) = mock("200 OK");
        let mut recorded = Vec::new();
        let check = check_connection_with(
            &credentials,
            ProviderId::Anthropic,
            &endpoint,
            Duration::from_secs(5),
            |label, bytes| {
                recorded.push((label.to_owned(), bytes));
                true
            },
        )
        .unwrap();
        let request = server.join().unwrap();
        assert_eq!(check.outcome, "accepted");
        assert!(check.bytes_sent > KEY.len() as u64);
        assert_eq!(
            recorded,
            [("anthropic:connection-check".to_owned(), check.bytes_sent)]
        );
        assert!(request.ends_with("\r\n\r\n"), "no body is sent");
        assert!(!serde_json::to_string(&check).unwrap().contains(KEY));

        let (endpoint, server) = mock("401 Unauthorized");
        let refused = check_connection_with(
            &credentials,
            ProviderId::Anthropic,
            &endpoint,
            Duration::from_secs(5),
            |_, _| true,
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(refused.outcome, "refused");
    }

    // AC-0752 (#1709): an OpenAI check sends the OpenAI key as a bearer
    // header, only to the OpenAI endpoint, under its own egress label.
    #[test]
    fn an_openai_check_sends_its_own_key_to_its_own_endpoint() {
        assert_eq!(
            check_url(ProviderId::OpenAi),
            llm::openai::CONNECTION_CHECK_URL
        );
        assert_eq!(
            check_url(ProviderId::Anthropic),
            llm::anthropic::CONNECTION_CHECK_URL
        );
        let credentials = crate::provider_credentials::ProviderCredentials::new(
            Box::new(crate::provider_credentials::MemoryStore::default()),
            Box::new(|name| match name {
                "OPENAI_API_KEY" => Some("sk-proj-openai".to_owned()),
                "ANTHROPIC_API_KEY" => Some(KEY.to_owned()),
                _ => None,
            }),
        );
        let (endpoint, server) = mock("200 OK");
        let mut recorded = Vec::new();
        let check = check_connection_with(
            &credentials,
            ProviderId::OpenAi,
            &endpoint,
            Duration::from_secs(5),
            |label, bytes| {
                recorded.push((label.to_owned(), bytes));
                true
            },
        )
        .unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();
        assert_eq!((check.provider, check.outcome), ("openai", "accepted"));
        assert_eq!(
            recorded,
            [("openai:connection-check".to_owned(), check.bytes_sent)]
        );
        assert!(request.contains("authorization: bearer sk-proj-openai"));
        assert!(
            !request.contains(&KEY.to_ascii_lowercase()),
            "no Anthropic key"
        );
        assert!(!request.contains("x-api-key"));
        assert!(!serde_json::to_string(&check).unwrap().contains("sk-proj"));
    }

    // AC-0590: with no key nothing is sent or recorded; when the record
    // cannot be written nothing is sent either.
    #[test]
    fn nothing_is_sent_without_a_key_or_without_an_egress_record() {
        // A listener that would see any connection attempt; none may be made.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1/models?limit=1", closed.local_addr().unwrap());
        let no_key = check_connection_with(
            &for_test(None),
            ProviderId::Anthropic,
            &endpoint,
            Duration::from_secs(5),
            |_, _| panic!("nothing is recorded when nothing is sent"),
        )
        .unwrap();
        assert_eq!((no_key.outcome, no_key.bytes_sent), ("no-key", 0));

        closed.set_nonblocking(true).unwrap();
        let unrecorded = check_connection_with(
            &for_test(Some(KEY)),
            ProviderId::Anthropic,
            &endpoint,
            Duration::from_secs(5),
            |_, _| false,
        );
        let error = unrecorded.unwrap_err();
        assert!(error.contains("nothing was sent"));
        assert!(!error.contains(KEY));
        assert!(closed.accept().is_err(), "no connection was attempted");
    }

    // AC-1416 / #1653: the lane probe asks the Ollama on the saved port, not
    // the default one; once nothing listens there it reports it stopped.
    #[test]
    fn the_local_lane_probe_uses_the_saved_port() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_ne!(port, llm::OllamaProvider::DEFAULT_PORT);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let n = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..n]).starts_with("GET /api/tags HTTP/1.1\r\n")
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{{\"models\":[]}}"
            )
            .unwrap();
        });
        let running = probe_local_lane(port);
        server.join().unwrap();
        assert!(running.reachable);
        assert_eq!(running.start_command, None);
        assert!(!probe_local_lane(port).reachable);
    }

    // AC-0590: the local lane names the server command when it does not
    // answer, and each missing model with its pull command when it does.
    #[test]
    fn the_local_lane_names_missing_models_and_their_pull_commands() {
        let stopped = local_lane_status_from(None);
        assert!(!stopped.reachable);
        assert_eq!(stopped.start_command, Some("ollama serve"));
        assert!(stopped.models.is_empty());

        let installed = [LocalModel {
            name: "nomic-embed-text:latest".into(),
            digest: "sha256:abc".into(),
        }];
        let running = local_lane_status_from(Some(&installed));
        assert!(running.reachable);
        assert_eq!(running.start_command, None);
        let summary: Vec<_> = running
            .models
            .iter()
            .map(|model| (model.model, model.installed, model.pull_command.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("nomic-embed-text", true, None),
                ("qwen3:1.7b", false, Some("ollama pull qwen3:1.7b")),
                ("qwen3:8b", false, Some("ollama pull qwen3:8b")),
            ]
        );
    }
}
