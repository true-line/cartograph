# ADR-0081 — A desktop investigation may choose Ollama's context window

- **Status:** Accepted (Cartograph lead as delegate)
- **Date:** 2026-11-21
- **Deciders:** Cartograph lead (delegate); Claude implements
- **Related:** #1653; US-0032 / AC-1406, AC-1407, AC-0564; ADR-0053

## Context

A bounded Ollama chat sends only `num_predict`. Ollama then uses its own
default context window, which is often a few thousand tokens. When a prompt
is longer than that, Ollama keeps the end and drops the rest without an
error, so a local model can answer from a cut prompt and nothing records it.
Investigation inputs routinely run to tens of kilobytes of evidence.

A larger window costs memory on the Mac that runs the model, and the right
size depends on the model and the machine. A fixed larger default would
change every existing run and could exhaust memory on smaller Macs.

## Decision

1. The window is chosen, not computed. **Configure → Run Limits…** gets a
   **Context window** section: **Ollama default** (nothing sent) or one of
   8,192, 16,384, 32,768 and 65,536 tokens (`OllamaProvider::CONTEXT_WINDOWS`).
   No other value is accepted, by the window or the host. It is stored in the
   app profile (`settings_local_context`) and kept across project switches.
2. A provider with a window sends it as `options.num_ctx` on every bounded
   chat. Its provider id gains `:num-ctx-<tokens>`, the same way
   `:thinking-enabled` binds reasoning. The provider id is in the bounded
   provider profile, so the consent fingerprint, the recorded provider
   descriptor and provenance all name the window a run used.
3. Only new desktop investigations on the local provider use it, like the
   saved local model. MCP starts, the cloud provider and running or saved
   investigations keep what they had.
4. With **Ollama default**, the request bytes and provider id are exactly what
   they were before this decision. The protocol version does not change: the
   option is a provider choice recorded in the profile, not a transport change.
5. **Ollama port (AC-1407).** The same window has an **Ollama port** section. Only
   the port is configurable (1 to 65535, default 11434); the host stays
   `127.0.0.1`, so the loopback-only, no-proxy local transport is unchanged.
   New desktop investigations, the installed-model list and the local Ollama
   check use it. The bounded profile's endpoint id, which the descriptor,
   consent and provenance record, names the port. Ingest, composition and
   escalation keep the default endpoint for now.

## Consequences

- A slow-hardware pilot can raise the window without a rebuild, and its
  saved runs say which window they used.
- The default is unchanged, so prompts longer than Ollama's own window are
  still cut for profiles that never choose one. Detecting a cut prompt from
  `prompt_eval_count` is a separate decision.
- Choosing a window larger than the model supports is the user's call; Ollama
  reports the failure and the run stops with it.
