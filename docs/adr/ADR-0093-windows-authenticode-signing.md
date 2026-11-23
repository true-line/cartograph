# ADR-0093 — Fail-closed Windows Authenticode signing through Azure Artifact Signing

- **Status:** Proposed (first slice implemented: the credential-set check in
  `scripts/signing-secrets.mjs`)
- **Date:** 2026-11-23
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #227; ADR-0016 (fail-closed macOS Developer ID), ADR-0018
  (Windows now), #480 (signing inputs never reach compilation)

## Context

`windows.yml` builds an unsigned NSIS installer with both sidecars on
`windows-latest` (#2440, #2454). SmartScreen warns on every first run of an
unsigned installer, and `release.yml` publishes macOS only. #227 cannot close
until a release can carry a signed Windows installer, timestamped so the
signature outlives the certificate, under the same fail-closed stance ADR-0016
gives macOS.

Authenticode needs a code-signing certificate. Since 2023, OV and EV
certificates must have their private keys on a hardware token or in a cloud
HSM, so a `.pfx` secret is no longer an option. GitHub-hosted runners cannot
reach a USB token. A cloud HSM signer that the runner reaches over the network
is left as the only practical choice.

## Decision

1. **Signer: Azure Artifact Signing (formerly Trusted Signing).** Microsoft
   holds the key and issues short-lived certificates to a verified identity.
   Its certificates build SmartScreen reputation like an OV certificate. It
   signs through `signtool` with Microsoft's signing dlib and timestamps
   through Microsoft's RFC 3161 service, using SHA-256 for both the file
   digest and the timestamp digest.
2. **No long-lived secret.** The runner signs in to Azure through GitHub OIDC
   federation (`id-token: write` on the signing job only). The complete
   credential set is five repository Actions secrets: `AZURE_TENANT_ID`,
   `AZURE_CLIENT_ID`, `ARTIFACT_SIGNING_ENDPOINT`,
   `ARTIFACT_SIGNING_ACCOUNT` and `ARTIFACT_SIGNING_PROFILE`. None of them is a
   private key or password. They are secrets so that forks and logs do not
   learn the account.
3. **Fail closed, as ADR-0016 does.** `node scripts/signing-secrets.mjs
   windows` classifies the set before any build:
   - no credential → `unsigned-dev`;
   - all five → `signed`;
   - any partial set → the job fails, naming only the missing secret names.
   An `unsigned-dev` installer keeps that word in its artifact name and is
   never attached to a release.
4. **Signing inputs never reach compilation (#480).** The app and sidecars
   compile with `tauri build --no-bundle` in a step with no Azure session and
   no signing variables. Only `tauri bundle` runs with them. Its
   `bundle.windows.signCommand` signs the app executable, both sidecars and
   the NSIS installer as each is produced.
5. **Verify before upload.** On the signed path, `Get-AuthenticodeSignature`
   must report `Valid` with a timestamp for the installer, the installed app
   executable and both sidecars. Otherwise the job fails.
6. **Release.** `release.yml` attaches the Windows installer only when its
   mode is `signed`. Until the five secrets exist, releases stay macOS-only,
   which is today's behaviour.

## Consequences

- A configured release ships a Windows installer that SmartScreen and
  enterprise policy can attribute to a verified publisher. A timestamped
  signature stays valid after the short-lived certificate expires.
- Nobody stores or rotates a key. Revoking the federated credential in Azure
  stops signing at once.
- The account needs Microsoft's identity validation and an Azure
  subscription, which only the owner can provision. Until then the path stays
  `unsigned-dev`, visibly.
- Microsoft Store distribution, MSIX packaging and WebView2 offline installers
  are out of scope.

## Alternatives (≤3)

- **OV or EV certificate on a hardware token, signed on the self-hosted
  runner.** Rejected because it ties every Windows release to one physical
  machine and token, and EV no longer grants immediate SmartScreen reputation.
- **Third-party cloud HSM (DigiCert KeyLocker, SSL.com eSigner).** Workable,
  but each needs a long-lived API credential as a secret. Artifact Signing
  uses OIDC with no stored credential.
- **Ship unsigned.** Rejected: SmartScreen blocks by default, and an unsigned
  binary cannot be attributed to its publisher.
