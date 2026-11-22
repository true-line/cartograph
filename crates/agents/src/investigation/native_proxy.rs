//! The per-run loopback model proxy for tool-calling runs (ADR-0085 decision
//! 7, AC-1422).
//!
//! The runtime never holds a provider key and has no network path of its own:
//! its only model endpoint is this proxy on `127.0.0.1`, reached with a
//! per-run token. The host arms the proxy for exactly one model request per
//! step, with the text the bridge just sent, and the proxy forwards a request
//! only when its path, token, model, frozen context window and last message
//! all match. It records the exact request size through the host's dispatch
//! callback before forwarding, so a call that then fails is still counted.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use serde_json::Value;

/// The server behind the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyBackend {
    /// Ollama's native `/api/chat`, which carries `options.num_ctx`.
    Ollama,
    /// An OpenAI-compatible `/v1/chat/completions` (vLLM), whose context
    /// window is fixed when the server starts.
    OpenAiCompatible,
}

impl ProxyBackend {
    fn path(self) -> &'static str {
        match self {
            Self::Ollama => "/api/chat",
            Self::OpenAiCompatible => "/v1/chat/completions",
        }
    }
}

/// Fixed limits for one run's proxy.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Run id; the only path prefix the proxy serves.
    pub run_id: String,
    /// Per-run bearer token the runtime must present.
    pub token: String,
    /// Upstream base URL, e.g. `http://127.0.0.1:11434` or `http://spark:8000`.
    pub upstream: String,
    /// The server type behind `upstream`.
    pub backend: ProxyBackend,
    /// The only model id the runtime may request.
    pub model: String,
    /// The frozen context window; Ollama requests must carry it as `num_ctx`.
    pub context_window: Option<u32>,
    /// Largest request body forwarded.
    pub max_request_bytes: usize,
    /// Largest upstream response body returned.
    pub max_response_bytes: usize,
}

/// Why the proxy refused or failed a model request. The runtime sees only an
/// HTTP status; the host sees the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProxyRefusal {
    /// No connection arrived before the deadline.
    #[error("no model request before the deadline")]
    Deadline,
    /// The path did not name this run's endpoint.
    #[error("wrong path")]
    Path,
    /// The bearer token was missing or wrong.
    #[error("wrong token")]
    Token,
    /// The request was not a JSON object within the size limit.
    #[error("malformed or oversized request")]
    Request,
    /// The request named another model.
    #[error("wrong model")]
    Model,
    /// An Ollama request did not carry the frozen context window.
    #[error("context window differs from the frozen one")]
    ContextWindow,
    /// The last message is not the text the host sent this step.
    #[error("last message is not the authorized text")]
    Unauthorized,
    /// The host declined to record the egress.
    #[error("egress not recorded")]
    NotRecorded,
    /// The upstream call failed or its response was too large.
    #[error("upstream failed")]
    Upstream,
}

/// One forwarded exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyExchange {
    /// Exact request body bytes forwarded.
    pub request_bytes: u64,
    /// Exact response body bytes returned.
    pub response_bytes: u64,
    /// Upstream HTTP status.
    pub status: u16,
}

/// A bound, per-run loopback proxy.
pub struct ModelProxy {
    listener: TcpListener,
    config: ProxyConfig,
    client: reqwest::blocking::Client,
}

impl ModelProxy {
    /// Binds `127.0.0.1` on a free port.
    pub fn bind(config: ProxyConfig) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(std::io::Error::other)?;
        Ok(Self {
            listener,
            config,
            client,
        })
    }

    /// The base URL the runtime is given.
    pub fn base_url(&self) -> std::io::Result<String> {
        let SocketAddr::V4(addr) = self.listener.local_addr()? else {
            return Err(std::io::Error::other("loopback v4 expected"));
        };
        Ok(format!("http://{addr}/{}", self.config.run_id))
    }

    /// Serves exactly one model request for this step. `authorized` is the
    /// text the bridge sent the runtime this step; `dispatching` records the
    /// request size and may refuse the egress.
    pub fn serve_one(
        &self,
        authorized: &str,
        deadline: Instant,
        dispatching: &dyn Fn(u64) -> bool,
    ) -> Result<ProxyExchange, ProxyRefusal> {
        let mut stream = self.accept(deadline)?;
        let outcome = self.handle(&mut stream, authorized, deadline, dispatching);
        if let Err(refusal) = &outcome {
            let status = match refusal {
                ProxyRefusal::Token => "401 Unauthorized",
                ProxyRefusal::Path => "404 Not Found",
                ProxyRefusal::Upstream => "502 Bad Gateway",
                _ => "403 Forbidden",
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
        }
        outcome
    }

    fn accept(&self, deadline: Instant) -> Result<TcpStream, ProxyRefusal> {
        loop {
            match self.listener.accept() {
                Ok((stream, peer)) if peer.ip().is_loopback() => {
                    stream
                        .set_nonblocking(false)
                        .map_err(|_| ProxyRefusal::Request)?;
                    let left = deadline.saturating_duration_since(Instant::now());
                    stream
                        .set_read_timeout(Some(left.max(Duration::from_millis(1))))
                        .map_err(|_| ProxyRefusal::Request)?;
                    return Ok(stream);
                }
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(ProxyRefusal::Deadline);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return Err(ProxyRefusal::Request),
            }
        }
    }

    fn handle(
        &self,
        stream: &mut TcpStream,
        authorized: &str,
        deadline: Instant,
        dispatching: &dyn Fn(u64) -> bool,
    ) -> Result<ProxyExchange, ProxyRefusal> {
        let (path, headers, body) = read_request(stream, self.config.max_request_bytes)?;
        let expected = format!("/{}{}", self.config.run_id, self.config.backend.path());
        if path != expected {
            return Err(ProxyRefusal::Path);
        }
        let bearer = format!("bearer {}", self.config.token);
        if !headers
            .iter()
            .any(|(name, value)| name == "authorization" && value.eq_ignore_ascii_case(&bearer))
        {
            return Err(ProxyRefusal::Token);
        }
        let request: Value = serde_json::from_slice(&body).map_err(|_| ProxyRefusal::Request)?;
        if request.get("model").and_then(Value::as_str) != Some(self.config.model.as_str()) {
            return Err(ProxyRefusal::Model);
        }
        if self.config.backend == ProxyBackend::Ollama
            && let Some(window) = self.config.context_window
            && request.pointer("/options/num_ctx").and_then(Value::as_u64) != Some(window.into())
        {
            return Err(ProxyRefusal::ContextWindow);
        }
        let last = request
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| messages.last())
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str);
        if last != Some(authorized) {
            return Err(ProxyRefusal::Unauthorized);
        }
        if !dispatching(body.len() as u64) {
            return Err(ProxyRefusal::NotRecorded);
        }
        let url = format!(
            "{}{}",
            self.config.upstream.trim_end_matches('/'),
            self.config.backend.path()
        );
        let left = deadline.saturating_duration_since(Instant::now());
        let response = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .timeout(left.max(Duration::from_millis(1)))
            .body(body.clone())
            .send()
            .map_err(|_| ProxyRefusal::Upstream)?;
        let status = response.status().as_u16();
        let reason = response.status().canonical_reason().unwrap_or("Status");
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let mut upstream = Vec::new();
        response
            .take(self.config.max_response_bytes as u64 + 1)
            .read_to_end(&mut upstream)
            .map_err(|_| ProxyRefusal::Upstream)?;
        if upstream.len() > self.config.max_response_bytes {
            return Err(ProxyRefusal::Upstream);
        }
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            upstream.len()
        )
        .and_then(|()| stream.write_all(&upstream))
        .map_err(|_| ProxyRefusal::Upstream)?;
        Ok(ProxyExchange {
            request_bytes: body.len() as u64,
            response_bytes: upstream.len() as u64,
            status,
        })
    }
}

type Request = (String, Vec<(String, String)>, Vec<u8>);

/// A minimal HTTP/1.1 request reader: `POST`, headers, a `content-length`
/// body. Chunked or oversized bodies are refused.
fn read_request(stream: &mut TcpStream, max_body: usize) -> Result<Request, ProxyRefusal> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|_| ProxyRefusal::Request)?;
    let mut parts = line.split_whitespace();
    if parts.next() != Some("POST") {
        return Err(ProxyRefusal::Request);
    }
    let path = parts.next().ok_or(ProxyRefusal::Request)?.to_owned();
    let mut headers = Vec::new();
    let mut header_bytes = 0usize;
    loop {
        let mut header = String::new();
        let read = reader
            .read_line(&mut header)
            .map_err(|_| ProxyRefusal::Request)?;
        header_bytes += read;
        if read == 0 || header_bytes > 16 * 1024 {
            return Err(ProxyRefusal::Request);
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').ok_or(ProxyRefusal::Request)?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    if headers.iter().any(|(name, _)| name == "transfer-encoding") {
        return Err(ProxyRefusal::Request);
    }
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .ok_or(ProxyRefusal::Request)?;
    if length > max_body {
        return Err(ProxyRefusal::Request);
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| ProxyRefusal::Request)?;
    Ok((path, headers, body))
}

#[cfg(test)]
#[path = "native_proxy_tests.rs"]
mod tests;
