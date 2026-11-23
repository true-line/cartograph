# ADR-0093 — Fail-closed Windows Authenticode signing through Azure Artifact Signing

- **Status:** Proposed (implemented: the credential-set check in
  `scripts/signing-secrets.mjs`, the signing installer workflow
  `windows-installer.yml`, and `release.yml` attaching only a signed
  installer; Accepted once a signed release verifies)
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
2. **The organization's existing account and service principal.** The owner
   already runs an Artifact Signing account for the organization's other
   signed apps. Cartograph signs with the same account, certificate profile
   and service principal, so it needs no new identity validation. The
   non-secret coordinates are committed in `windows-installer.yml` (endpoint
   `https://eus.codesigning.azure.net/`, account `zts1`, profile
   `overlook`). The credential set is three repository Actions secrets with
   the same names as the other apps: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID` and
   `AZURE_CLIENT_SECRET`. The signing dlib authenticates only through
   `EnvironmentCredential`; every other Azure credential is excluded. The
   secrets reach only the two steps that sign.
3. **Fail closed, as ADR-0016 does.** `node scripts/signing-secrets.mjs
   windows` classifies the set before any build:
   - no credential → `unsigned-dev`;
   - all three → `signed`;
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
   mode is `signed`. Until the three secrets exist, releases stay macOS-only,
   which is today's behaviour.

## Consequences

- A configured release ships a Windows installer that SmartScreen and
  enterprise policy can attribute to a verified publisher. A timestamped
  signature stays valid after the short-lived certificate expires.
- No private key is stored. The client secret is rotated in Azure and in the
  repository secret together; revoking it stops signing at once.
- The account, its identity validation and the service principal already
  exist. Only the three repository secrets have to be added; until then the
  path stays `unsigned-dev`, visibly.
- Microsoft Store distribution, MSIX packaging and WebView2 offline installers
  are out of scope.

## Alternatives (≤3)

- **OV or EV certificate on a hardware token, signed on the self-hosted
  runner.** Rejected because it ties every Windows release to one physical
  machine and token, and EV no longer grants immediate SmartScreen reputation.
- **Third-party cloud HSM (DigiCert KeyLocker, SSL.com eSigner).** Workable,
  but each needs a long-lived API credential as a secret. Artifact Signing
  already has a provisioned, validated account and service principal.
- **GitHub OIDC federation instead of a client secret.** It stores no
  credential, but needs a new federated credential on the service principal
  in Azure, which only the owner can add. Matching the existing client-secret
  setup ships signing now; OIDC can replace it later without changing the
  fail-closed check.
- **Ship unsigned.** Rejected: SmartScreen blocks by default, and an unsigned
  binary cannot be attributed to its publisher.
