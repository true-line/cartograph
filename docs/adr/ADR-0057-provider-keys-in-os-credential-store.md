# ADR-0057 — Provider keys in the operating system's credential store

- **Status:** Proposed; owner review
- **Date:** 2026-11-10
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1763; US-0009 / AC-0586; ADR-0004

## Context

The cloud lane read its key from the process environment only
(`ANTHROPIC_API_KEY`). A macOS app opened from Finder or the Dock inherits no
shell variables, so a normally launched release build could not reach the cloud
lane at all, and no screen accepted a key.

A key the app accepts has to be kept somewhere. The app owns a settings
database, logs, a model debug log, exports and consent records. None of them
may hold a secret: they are read by tools, copied into bug reports and, for
exports, handed to other people.

## Decision

1. **Storage.** A provider key entered in the app is written to the operating
   system's credential store and nowhere else: the login Keychain on macOS and
   the Credential Manager on Windows, under the service name
   `ai.true-line.cartograph.provider-key` with the provider id as the account.
   The `keyring` crate (3.x, platform features only, no default features) is
   the one new dependency. Other platforms have no store wired; there only the
   environment variable works, and saving a key is refused with that reason.
2. **Write-only.** No command returns a key. The app shows, per provider,
   whether a key is present and where the key in use comes from
   (`environment`, `stored` or `none`).
3. **Precedence.** The environment variable wins over the stored key. A
   developer or a CI run can override the saved key for one process without
   changing what is saved. The Settings section says when this is happening.
4. **One reader.** `src-tauri/src/provider_credentials.rs` is the only module
   that reads, saves or removes a key, and `egress.rs` is the only caller that
   resolves one, when it constructs a provider.
5. **A key grants nothing.** Consent is unchanged. Every cloud step still
   passes the egress firewall with consent bound to its exact payload
   (ADR-0004). No key and no variable means the cloud lane stays closed with a
   message that says where to add one.
6. **Validation.** A key is trimmed and must be one line of printable ASCII
   with no spaces, at most 512 bytes. A refusal never repeats what was typed.

### Amendment — 2026-11-10: a connection check (#1795)

Settings can ask a provider whether it accepts the configured key. This adds
one exception to decisions 4 and 5, and nothing else changes.

- **What is sent.** One `GET` for the provider's model list
  (`/v1/models?limit=1`) with the key and the API version header. There is no
  body. No source text, prompt, span or graph content is part of the request,
  and nothing in the code path can add one: the check takes the key and an
  endpoint and no other input.
- **No tier consent.** Consent (ADR-0004) exists to control what of the user's
  sources leaves the machine. The check sends none of it, so it does not pass
  the egress firewall and asks for no consent. The Settings section states
  exactly what is sent before the user chooses to run it; the check never runs
  on its own.
- **Recorded.** The request does leave the machine, so its size (the request
  line and the headers Cartograph sets, the key included) is written to the
  egress record under `<provider>:connection-check` before it is sent. If the
  record cannot be written, nothing is sent.
- **Second resolver.** `provider_checks.rs` resolves the key through
  `provider_credentials.rs` for this request. It is the only caller besides
  `egress.rs`.
- **Result.** `accepted`, `refused` (HTTP 401 or 403), `unreachable` (anything
  else, including a timeout after 10 seconds) or `no-key`. The response body is
  never read and the result never carries the key. Redirects are not followed.

## Consequences

- A release build launched from Finder can use the cloud lane after the user
  saves a key once.
- macOS may ask the user to allow Keychain access when the signing identity of
  the app changes (for example between an unsigned development build and a
  signed release).
- Saving or removing a key drops the cached cloud client, so the next
  investigation start uses the current key. A running task keeps the client it
  started with.
- Adding a provider is one enum case in `provider_credentials.rs`.
- Linux has no stored-key path until a store is wired for it.

## Alternatives

1. **Encrypt the key in the settings database.** The encryption key would
   itself need a home; this moves the problem and adds a format to maintain.
2. **Keep the environment variable only and document `launchctl setenv`.**
   Leaves a first-run step that most users will not get past, and puts the key
   in every process the user launches.
3. **Tauri's Stronghold plugin.** A password-protected vault adds a prompt on
   every launch and a second secret for the user to keep.
