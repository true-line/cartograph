//! ADR-0085: the provider a tool-calling specialist version runs on.
//!
//! A version with `NativeToolRuntime` gets, for each run, a fresh
//! `cartograph-agent` process behind a fresh loopback model proxy, wrapped
//! around the provider the start already resolved. The wrapped provider keeps
//! its profile, so authorization, the frozen descriptor and the run record
//! are unchanged; only the transport differs. Every other version gets the
//! resolved provider back as it is.
//!
//! The runtime is the `cartograph-agent` executable bundled next to the app
//! (AC-1427). `CARTOGRAPH_AGENT_RUNTIME` overrides it for development: an
//! executable, or a `.ts`/`.js` entry script run by `CARTOGRAPH_AGENT_NODE`
//! (`node` by default). A tool-calling start with neither fails before any
//! model request.
//!
//! The Claude lane (AC-1424) runs the Claude Agent SDK, which drives Claude
//! Code. That executable ships as an app resource under Anthropic's own
//! signature (`CARTOGRAPH_CLAUDE_CODE` overrides it for development) and is
//! checked against its pinned sha256, and on macOS its Anthropic Developer ID
//! signature, before every runtime start that would use it.

use agents::investigation::SpecialistId;
use agents::investigation::native_proxy::{ModelProxy, ProxyBackend, ProxyConfig};
use agents::investigation::native_session::{
    NativeSession, NativeToolProvider, ProcessLink, RuntimeLane, SessionConfig,
};
use llm::LlmProvider;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// Fixed byte caps for one run's proxy; each step's own limits are tighter.
const MAX_PROXY_BYTES: usize = 8 << 20;
/// Turns the runtime may take on its own; the host's step limits bind first.
/// The runtime accepts at most 256 (`agent-runtime/src/protocol.ts`).
const MAX_RUNTIME_TURNS: u32 = 256;
/// Conversation budget before the bridge restarts the runtime; each step's
/// request limit lowers it.
const CONVERSATION_BYTES: usize = 1 << 20;

/// The bundled runtime's file name; Tauri drops the target suffix when it
/// places an external binary next to the app executable.
const BUNDLED_RUNTIME: &str = "cartograph-agent";

/// Claude Code's place in the macOS app bundle, from `Contents/MacOS`.
const BUNDLED_CLAUDE_CODE: &str = "../Resources/claude-code/claude";

/// The Claude Code executables the Agent SDK pin allows, by platform, and
/// the Apple team that signs them (`scripts/agent-runtime-claude.json`).
const CLAUDE_CODE_PIN: &str = include_str!("../../../scripts/agent-runtime-claude.json");

/// How to start the tool-calling runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeCommand {
    /// The executable: the runtime itself, or Node.js for an entry script.
    pub program: OsString,
    /// The entry script when `program` is Node.js; empty otherwise.
    pub args: Vec<OsString>,
    /// Claude Code, for the Claude lane; checked before each use.
    pub claude_code: Option<PathBuf>,
}

impl RuntimeCommand {
    /// The runtime named by the process environment, else the one bundled
    /// with the app, if any.
    pub(crate) fn from_env() -> Option<Self> {
        let beside = |name: &str| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| Some(exe.parent()?.join(name)))
                .filter(|path| path.is_file())
        };
        // The Windows bundle installs it as cartograph-agent.exe (#227).
        let bundled = format!("{BUNDLED_RUNTIME}{}", std::env::consts::EXE_SUFFIX);
        let mut command = Self::from_vars(&|name| std::env::var_os(name), beside(&bundled))?;
        command.claude_code = std::env::var_os("CARTOGRAPH_CLAUDE_CODE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| beside(BUNDLED_CLAUDE_CODE));
        Some(command)
    }

    /// Whether a Claude start can run here: Claude Code is present and this
    /// platform has a pinned build (only Apple silicon on macOS for now).
    pub(crate) fn has_claude_lane(&self) -> bool {
        self.claude_code.as_deref().is_some_and(Path::is_file)
            && platform()
                .and_then(|platform| claude_code_pin(&platform))
                .is_some()
    }

    fn from_vars(
        var: &dyn Fn(&str) -> Option<OsString>,
        bundled: Option<std::path::PathBuf>,
    ) -> Option<Self> {
        let Some(entry) = var("CARTOGRAPH_AGENT_RUNTIME").filter(|entry| !entry.is_empty()) else {
            return bundled.map(|path| Self {
                program: path.into(),
                args: Vec::new(),
                claude_code: None,
            });
        };
        let script = std::path::Path::new(&entry)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "ts" | "js" | "mjs" | "cjs"));
        if !script {
            return Some(Self {
                program: entry,
                args: Vec::new(),
                claude_code: None,
            });
        }
        let node = var("CARTOGRAPH_AGENT_NODE")
            .filter(|node| !node.is_empty())
            .unwrap_or_else(|| "node".into());
        Some(Self {
            program: node,
            args: vec![entry],
            claude_code: None,
        })
    }

    /// The child command. Its environment holds only `PATH`, so no key or
    /// proxy setting of the app reaches the runtime (ADR-0085 decision 7).
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).env_clear();
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
    /// Claude Code is missing, or not the pinned, Anthropic-signed build for
    /// this platform.
    #[error("Claude Code for the Claude lane is missing or not the pinned build.")]
    ClaudeCode,
}

/// This platform's key in the Claude Code pin, such as `darwin-arm64`.
fn platform() -> Option<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => return None,
    };
    Some(format!("{os}-{arch}"))
}

/// The pinned sha256 for `platform` and the signing team, from the pin.
fn claude_code_pin(platform: &str) -> Option<(String, String)> {
    let pin: serde_json::Value = serde_json::from_str(CLAUDE_CODE_PIN).ok()?;
    let sha = pin.pointer(&format!("/sha256/{platform}"))?.as_str()?;
    let team = pin.get("team_id")?.as_str()?;
    Some((sha.to_owned(), team.to_owned()))
}

/// The code requirement Claude Code must satisfy on macOS: a Developer ID
/// Application certificate issued by Apple to the pinned team.
fn claude_code_requirement(team: &str) -> String {
    format!(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team}\""
    )
}

/// Checks `path` is the pinned Claude Code for this platform: its sha256,
/// and on macOS its signature against [`claude_code_requirement`].
fn verify_claude_code(path: &Path) -> Result<(), ToolRuntimeError> {
    let (sha, team) = platform()
        .and_then(|platform| claude_code_pin(&platform))
        .ok_or(ToolRuntimeError::ClaudeCode)?;
    let mut file = std::fs::File::open(path).map_err(|_| ToolRuntimeError::ClaudeCode)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)
            .map_err(|_| ToolRuntimeError::ClaudeCode)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != sha {
        return Err(ToolRuntimeError::ClaudeCode);
    }
    if cfg!(target_os = "macos") {
        let status = Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "-R"])
            .arg(format!("={}", claude_code_requirement(&team)))
            .arg(path)
            .env_clear()
            .status()
            .map_err(|_| ToolRuntimeError::ClaudeCode)?;
        if !status.success() {
            return Err(ToolRuntimeError::ClaudeCode);
        }
    }
    Ok(())
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
    if provider_id.starts_with("anthropic:")
        && let Some(base) = endpoint.strip_suffix("/v1/messages")
    {
        return Ok((base.to_owned(), ProxyBackend::Anthropic, None));
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

/// The provider one run uses, and its runtime when it has one.
pub(crate) struct RunProvider {
    /// What the worker calls.
    pub provider: Arc<dyn LlmProvider>,
    native: Option<Arc<NativeToolProvider<ProcessLink>>>,
    lane: RuntimeLane,
}

impl RunProvider {
    /// The runtime this run executed on, once it has reported its identity
    /// (AC-1423). A bounded-loop run has none.
    pub(crate) fn runtime(&self) -> Option<crate::jobs::investigations::RunRuntime> {
        let identity = self.native.as_ref()?.runtime_identity()?;
        Some(crate::jobs::investigations::RunRuntime {
            lane: match self.lane {
                RuntimeLane::Pi => "pi",
                RuntimeLane::Claude => "claude",
            }
            .into(),
            runtime: identity.runtime,
            version: identity.version,
            runner: identity.runner,
        })
    }
}

/// The provider `specialist` runs on for run `run_id`: `provider` itself for
/// a bounded-loop version, otherwise `provider` driven by a fresh runtime.
pub(crate) fn provider_for_run(
    specialist: SpecialistId,
    run_id: &str,
    provider: &Arc<dyn LlmProvider>,
    runtime: Option<&RuntimeCommand>,
) -> Result<RunProvider, ToolRuntimeError> {
    if !specialist.uses_native_tool_runtime() {
        return Ok(RunProvider {
            provider: provider.clone(),
            native: None,
            lane: RuntimeLane::Pi,
        });
    }
    let runtime = runtime.ok_or(ToolRuntimeError::NotInstalled)?;
    let profile = provider
        .bounded_profile()
        .map_err(|_| ToolRuntimeError::Preparation)?;
    let (upstream, backend, context_window) = upstream(&profile.provider_id, &profile.endpoint_id)?;
    // Claude runs on the Agent SDK lane behind a keyed proxy (AC-1424);
    // local models on Pi with no key at all.
    let (lane, credential, claude_code) = match (profile.locality, backend) {
        (llm::Locality::Cloud, ProxyBackend::Anthropic) => {
            let credential = provider
                .proxy_credential()
                .ok_or(ToolRuntimeError::UnsupportedProvider)?;
            let claude_code = runtime
                .claude_code
                .clone()
                .ok_or(ToolRuntimeError::ClaudeCode)?;
            verify_claude_code(&claude_code)?;
            (RuntimeLane::Claude, Some(credential), Some(claude_code))
        }
        (llm::Locality::Local, ProxyBackend::Ollama | ProxyBackend::OpenAiCompatible) => {
            (RuntimeLane::Pi, None, None)
        }
        _ => return Err(ToolRuntimeError::UnsupportedProvider),
    };
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
        credential,
    })
    .map_err(|_| ToolRuntimeError::Preparation)?;
    let runtime = runtime.clone();
    let session = NativeSession::new(
        proxy,
        SessionConfig {
            lane,
            max_turns: MAX_RUNTIME_TURNS,
            budget_bytes: CONVERSATION_BYTES,
            claude_executable: claude_code
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        },
        // Claude Code is checked again before every (re)start that uses it.
        Box::new(move || {
            if let Some(path) = &claude_code {
                verify_claude_code(path).map_err(std::io::Error::other)?;
            }
            ProcessLink::spawn(runtime.command())
        }),
    );
    let wrapped = Arc::new(
        NativeToolProvider::new(provider.clone(), session)
            .map_err(|_| ToolRuntimeError::Preparation)?,
    );
    Ok(RunProvider {
        provider: wrapped.clone(),
        native: Some(wrapped),
        lane,
    })
}

#[cfg(test)]
#[path = "tool_runtime_tests.rs"]
mod tests;
