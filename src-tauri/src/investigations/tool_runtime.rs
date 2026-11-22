//! ADR-0085: the provider a tool-calling specialist version runs on.
//!
//! A version with `NativeToolRuntime` gets, for each run, a fresh
//! `cartograph-agent` process behind a fresh loopback model proxy, wrapped
//! around the provider the start already resolved. The wrapped provider keeps
//! its profile, so authorization, the frozen descriptor and the run record
//! are unchanged; only the transport differs. Every other version gets the
//! resolved provider back as it is.
//!
//! Until the runtime ships inside the app (AC-1427) it is found through
//! `CARTOGRAPH_AGENT_RUNTIME` (the runtime's `src/main.ts`) and, optionally,
//! `CARTOGRAPH_AGENT_NODE` (the Node.js executable, `node` by default). A
//! tool-calling start without them fails before any model request.

use agents::investigation::SpecialistId;
use agents::investigation::native_proxy::{ModelProxy, ProxyBackend, ProxyConfig};
use agents::investigation::native_session::{
    NativeSession, NativeToolProvider, ProcessLink, RuntimeLane, SessionConfig,
};
use llm::LlmProvider;
use std::ffi::OsString;
use std::process::Command;
use std::sync::Arc;

/// Fixed byte caps for one run's proxy; each step's own limits are tighter.
const MAX_PROXY_BYTES: usize = 8 << 20;
/// Turns the runtime may take on its own; the host's step limits bind first.
const MAX_RUNTIME_TURNS: u32 = 1024;
/// Conversation budget before the bridge restarts the runtime; each step's
/// request limit lowers it.
const CONVERSATION_BYTES: usize = 1 << 20;

/// How to start the tool-calling runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeCommand {
    /// The Node.js executable.
    pub node: OsString,
    /// The runtime's entry script.
    pub entry: OsString,
}

impl RuntimeCommand {
    /// The runtime named by the process environment, if any.
    pub(crate) fn from_env() -> Option<Self> {
        Self::from_vars(&|name| std::env::var_os(name))
    }

    fn from_vars(var: &dyn Fn(&str) -> Option<OsString>) -> Option<Self> {
        let entry = var("CARTOGRAPH_AGENT_RUNTIME").filter(|entry| !entry.is_empty())?;
        let node = var("CARTOGRAPH_AGENT_NODE")
            .filter(|node| !node.is_empty())
            .unwrap_or_else(|| "node".into());
        Some(Self { node, entry })
    }

    /// The child command. Its environment holds only `PATH`, so no key or
    /// proxy setting of the app reaches the runtime (ADR-0085 decision 7).
    fn command(&self) -> Command {
        let mut command = Command::new(&self.node);
        command.arg(&self.entry).env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        command
    }
}

/// Why a tool-calling run could not get its provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ToolRuntimeError {
    /// No runtime is installed or configured.
    #[error("The tool-calling runtime is not installed.")]
    NotInstalled,
    /// The resolved provider has no tool-calling lane yet.
    #[error("This provider has no tool-calling runtime yet.")]
    UnsupportedProvider,
    /// The proxy, token or provider wrapper could not be prepared.
    #[error("The tool-calling runtime could not be prepared.")]
    Preparation,
}

/// The upstream a local provider's profile names: its base URL, server type
/// and frozen Ollama context window. The runtime runs with thinking off, so a
/// provider frozen with thinking on has no tool-calling lane.
fn upstream(
    provider_id: &str,
    endpoint: &str,
) -> Result<(String, ProxyBackend, Option<u32>), ToolRuntimeError> {
    if provider_id.contains(":thinking-enabled") {
        return Err(ToolRuntimeError::UnsupportedProvider);
    }
    if let Some(base) = endpoint.strip_suffix("/api/chat") {
        return Ok((
            base.to_owned(),
            ProxyBackend::Ollama,
            frozen_window(provider_id)?,
        ));
    }
    if let Some(base) = endpoint.strip_suffix("/v1/chat/completions") {
        return Ok((base.to_owned(), ProxyBackend::OpenAiCompatible, None));
    }
    Err(ToolRuntimeError::UnsupportedProvider)
}

/// The `:num-ctx-N` window an Ollama identity carries, wherever it sits: the
/// id may name separate embedding and completion models
/// (`ollama:<embedding>+<completion>`) and other suffixes. A window that is
/// present but unreadable is refused, never dropped.
fn frozen_window(provider_id: &str) -> Result<Option<u32>, ToolRuntimeError> {
    const MARK: &str = ":num-ctx-";
    let mut found = provider_id.match_indices(MARK);
    let Some((at, _)) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err(ToolRuntimeError::UnsupportedProvider);
    }
    let digits = provider_id[at + MARK.len()..]
        .split(':')
        .next()
        .unwrap_or_default();
    digits
        .parse()
        .ok()
        .filter(|tokens| llm::OllamaProvider::CONTEXT_WINDOWS.contains(tokens))
        .map(Some)
        .ok_or(ToolRuntimeError::UnsupportedProvider)
}

/// The per-run bearer token: 32 bytes from SQLite's OS-seeded randomness,
/// the same source the app uses for its ids.
fn token() -> Result<String, ToolRuntimeError> {
    rusqlite::Connection::open_in_memory()
        .and_then(|conn| {
            conn.query_row("SELECT lower(hex(randomblob(32)))", [], |row| {
                row.get::<_, String>(0)
            })
        })
        .ok()
        .filter(|token| token.len() == 64)
        .ok_or(ToolRuntimeError::Preparation)
}

/// The provider `specialist` runs on for run `run_id`: `provider` itself for
/// a bounded-loop version, otherwise `provider` driven by a fresh runtime.
pub(crate) fn provider_for_run(
    specialist: SpecialistId,
    run_id: &str,
    provider: &Arc<dyn LlmProvider>,
    runtime: Option<&RuntimeCommand>,
) -> Result<Arc<dyn LlmProvider>, ToolRuntimeError> {
    if !specialist.uses_native_tool_runtime() {
        return Ok(provider.clone());
    }
    let runtime = runtime.ok_or(ToolRuntimeError::NotInstalled)?;
    let profile = provider
        .bounded_profile()
        .map_err(|_| ToolRuntimeError::Preparation)?;
    // Claude runs on the Agent SDK lane, whose keyed proxy follows (AC-1424).
    if profile.locality != llm::Locality::Local {
        return Err(ToolRuntimeError::UnsupportedProvider);
    }
    let (upstream, backend, context_window) = upstream(&profile.provider_id, &profile.endpoint_id)?;
    let run_path: String = run_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if run_path.is_empty() {
        return Err(ToolRuntimeError::Preparation);
    }
    let proxy = ModelProxy::bind(ProxyConfig {
        run_id: run_path,
        token: token()?,
        upstream,
        backend,
        model: profile.requested_model.clone(),
        context_window,
        max_request_bytes: MAX_PROXY_BYTES,
        max_response_bytes: MAX_PROXY_BYTES,
    })
    .map_err(|_| ToolRuntimeError::Preparation)?;
    let runtime = runtime.clone();
    let session = NativeSession::new(
        proxy,
        SessionConfig {
            lane: RuntimeLane::Pi,
            max_turns: MAX_RUNTIME_TURNS,
            budget_bytes: CONVERSATION_BYTES,
        },
        Box::new(move || ProcessLink::spawn(runtime.command())),
    );
    let wrapped = NativeToolProvider::new(provider.clone(), session)
        .map_err(|_| ToolRuntimeError::Preparation)?;
    Ok(Arc::new(wrapped))
}

#[cfg(test)]
#[path = "tool_runtime_tests.rs"]
mod tests;
