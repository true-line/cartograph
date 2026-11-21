# ADR-0075 — Env-file values are read only for allowlisted keys

- **Status:** Proposed; owner review
- **Date:** 2026-11-21
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #933; US-0004 / AC-0011, AC-0990, AC-1018; US-0014 / AC-0097; ADR-0002; ADR-0070 decision 5

## Context

Toolchain facts promise key names only for env files (AC-0097). The events
config resolver did not keep that promise: it indexed **every** `KEY=VALUE`
line of every `.env` / `.env.*` file and wrote the values it used (a queue
name, a client base URL) into the graph. AC-0990 closed the worst of it: an
env file holding a secret-shaped value refuses the ingest. A non-secret
value was still read and stored whether or not anyone chose to share it.

Secrets should not be in `.env`, but sometimes they are, and the secret
patterns cannot recognize every credential. Reading a value is a decision
about disclosure, so it belongs to the author, key by key.

## Decision

1. **Allowlist, exact keys.** An env-file value is read only for a key the
   author lists:
   - `env_read = ["QUEUE_URL", "API_BASE"]` at the top of
     `cartograph.system.toml` (system ingests), and
   - `read = [...]` in `.cartograph/env.toml` at a repository's root (any
     ingest, including a single repository).
   The two lists are unioned. Entries are exact key names
   (`[A-Za-z_][A-Za-z0-9_.]*`); a glob or any other shape refuses the
   configuration. Both lists are empty by default.
2. **Fail closed.** The resolver no longer reads any other value. A site
   whose identity names a key that an env file sets but the allowlist does
   not list stays unresolved, and its Gap says so: the key, the file, and
   that `env_read` / `.cartograph/env.toml` is how to allow it. A key no
   env file sets keeps its existing "not found" Gap. Nothing is silent.
3. **The secret refusal stands.** AC-0990 still scans every line, listed
   or not, and refuses an ingest whose env files hold a secret-shaped value.
   Detection reads a value only to refuse; it never stores one.
4. **Userinfo is never kept.** An allowlisted value that is a URL keeps its
   scheme, host, port and path; any `user@` part is removed. (A password in
   a URL already refuses the ingest under AC-0990.)
5. **The manifest's `[env]` table is unchanged.** Author-declared values
   outrank env files and need no allowlist entry: declaring a value is the
   author's choice to share it.

## Consequences

- **Behavior change.** A repository that relied on the resolver reading
  `.env` implicitly now gets Gaps naming the keys to allow, until the author
  lists them. Channels and client calls resolved from `[env]` are not
  affected.
- Toolchain facts still carry key names only (AC-0097). Reading allowlisted
  values into toolchain facts, with a line citation per value and template
  files ranked below real ones, is a later increment of #933 under this
  decision.
- The graph content hash changes for repositories whose resolved channels
  or client calls came from implicit `.env` reads.

## Alternatives

1. **Keep implicit reads, refuse secrets (the AC-0990 state).** No behavior
   change, but every non-secret value is disclosed without a choice, and the
   secret patterns are a heuristic.
2. **Globs in the allowlist (`QUEUE_*`).** Convenient, but a later key that
   matches is shared without anyone choosing it.
3. **Allowlist only in `cartograph.system.toml`.** A single-repository
   ingest has no system manifest, so it could never allow a value;
   `.cartograph/env.toml` follows `.cartograph/api-docs.toml`.
