# ADR-0083 — The firewall may refuse one acquired read toward a cloud provider

- **Status:** Accepted (Cartograph lead as delegate, 2026-11-22)
- **Date:** 2026-11-22
- **Deciders:** Cartograph lead (delegate); Claude implements
- **Related:** #679; #633; US-0032 / AC-1409

## Context

`AttemptOutcome::FirewallRefused` is reserved and cannot be persisted,
because no production firewall condition can deny one acquired
representation while allowing the rest of the bounded input. A whole-action
cloud `EgressDenied`, a cancelled consent and redaction are not that
condition. #679 needs a reviewed, target-specific predicate before
persistence is enabled.

## Decision

1. **Recipe `egress-read-deny/v1`.** A read representation is refused
   exactly when the action's provider locality is Cloud **and** the
   representation's repo-relative path matches a deny rule. A local
   provider never matches: nothing leaves the machine.
2. **Default rules (cannot be removed).** A pattern without `/` matches the
   file name; with `/` it matches trailing path components; `*` stays within
   one component; ASCII case is ignored.
   - `env-file`: `.env`, `.env.*`, `*.env`
   - `key-material`: `*.pem`, `*.key`, `*.p12`, `*.pfx`, `id_rsa*`,
     `id_ed25519*`, `*.keystore`, `*.jks`
   - `credential-file`: `.npmrc`, `.pypirc`, `.netrc`, `credentials.json`,
     `*.tfvars`, `.aws/credentials`, `.docker/config.json`
3. **Project rules only add.** Up to 64 printable-ASCII relative patterns of
   at most 256 bytes, without empty or `..` components, identified as
   `project:<index>`.
4. **Host-owned witness.** Only `EgressFirewall::evaluate_read` builds a
   `ReadDenialWitness`: recipe, rule id, exact read identity (repo, path,
   commit, byte range), BLAKE3 of the refused bytes and the acquired attempt
   id. It has no public constructor and does not deserialize, so no model
   output, IPC request or stored row can build one. It never holds the
   withheld text.
5. **Redaction stays separate.** A matching path is refused, not redacted;
   a non-matching path keeps today's redaction. Whole-action denial,
   consent failure and preparation failure never produce a per-read refusal.

## Consequences

- Egress is only tightened; nothing that is refused today becomes allowed.
- The rules hold no state of their own, so existing hash domains and the
  `EgressPolicy` wire shape are unchanged.
- Worker withholding, durable persistence and readback of the refusal
  (#679 sections "Required behavior" and tests 1–6) build on this witness in
  later slices.

## Alternatives

1. **Content-based secret detection as the predicate.** Already the
   redaction path; turning redaction into denial is explicitly out of scope.
2. **A field on `EgressPolicy`.** Equivalent decision, but changes a
   serialized policy shape; the rules are held by the firewall instead.
