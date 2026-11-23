# Releasing Cartograph

Semantic version application and tags are automation-owned; see ADR-0015 and
the release rules in `AGENTS.md`. macOS bundle trust is governed by ADR-0016.

## Cut and publish a release

1. Merge behavior changes with their required Changesets. The **Version cut**
   workflow keeps the `changeset-release/main` **Version packages** PR current.
   That commit is written through the GitHub git data API as `trueline-chores-dumb`
   (`scripts/version-commit.mjs`), so it is GitHub-signed and satisfies the
   default-branch `required_signatures` rule; nothing on the branch is pushed
   from the runner's own git identity.
2. Review that ready PR, require its exact head and merge-queue candidate to
   pass the normal complete suite, and merge it. Version-cut does not dispatch
   a second equivalent suite. This merge is the release decision: automation
   waits for the exact `main` commit's stable CI result, creates the immutable
   annotated `vX.Y.Z` tag, and hands it to the **Release** workflow.
3. The Release workflow validates the exact tag, its reviewed PR head, and
   complete-suite evidence for the exact release commit. It then runs the
   reusable macOS package workflow and publishes only that invocation's named
   artifact. The release notes come from the exact version section generated
   in `CHANGELOG.md`.

With the complete five-secret set below, the result is a normal GitHub Release
containing signed, notarized, stapled, Gatekeeper-verified universal app and DMG
assets. With no signing secrets, it is an explicitly titled and warned
`unsigned-dev` prerelease. A partial secret set fails before packaging.

Publication is idempotent. Dispatching **Release** again with the same existing
tag updates the release's title, notes, flags, and assets in place. It replaces
same-named assets and removes stale assets from the opposite signing mode; it
does not create another release or move the tag.

## Recovery and verification

If the reviewed version merge created its tag but the release handoff failed,
run **Version cut** manually. It requires the existing tag and redispatches the
missing release. You may instead dispatch **Release** with that exact tag. Do
not create, delete, or move a tag by hand.

### Versions that were cut but never tagged (#1762)

Every `## X.Y.Z` heading in `CHANGELOG.md` is a cut. The daily **Scheduled
checks** lane "Changelog versions are tagged" (`node scripts/release-tags.mjs`)
compares them with the `v*` tags. A version cut in the last three hours is
still waiting for its tag; any other version without a tag fails the lane and
opens or updates the issue "Scheduled: changelog versions without a release
tag". The tag job also fails, instead of exiting quietly, when a cut commit
still holds pending changesets.

Seventeen versions (v0.15.4 to v0.69.0) were cut and never tagged. They are
recorded as never released, with each one's cut commit and cause, in
[`docs/release/untagged-versions.json`](release/untagged-versions.json):

- **Changesets still pending at the cut (5):** v0.23.1, v0.27.0, v0.37.0,
  v0.40.0, v0.44.0. A feature PR's changeset landed on main after the Version
  packages branch was cut, the squash merge carried it, and the tag step
  logged "nothing to tag" and passed.
- **Before the tag job moved to the CI completion trigger (#1436) (9):**
  v0.15.4, v0.16.0, v0.22.0, v0.33.0, v0.42.0, v0.43.0, v0.45.0, v0.49.0,
  v0.50.0. The job polled CI for at most 70 minutes and was never retried
  (#1378).
- **After #1436, cause not recoverable (3):** v0.52.0, v0.53.1, v0.69.0. No
  changesets were pending, and the run history for that period is no longer
  kept; a non-success CI conclusion on the cut commit is the remaining refusal.

They are not tagged retroactively: a tag would name a build that never passed
release packaging, and tags are never moved. The next tagged version contains
each one's changes. To record a new gap, find why its tag run refused, then
either recover it with **Version cut** or add it to the record with its cause.
The lane fails on a recorded version that has since been tagged, so the record
only shrinks.

After publication, verify the workflow and release:

```sh
gh run list --workflow release.yml --limit 5
gh release view vX.Y.Z --json tagName,isDraft,isPrerelease,name,assets
gh release download vX.Y.Z --pattern 'Cartograph_*_universal_*' --dir dist
```

For a production release, require `isDraft: false`, `isPrerelease: false`, and
exactly three versioned `signed` assets: the app zip, the DMG and the dSYM zip
(`Cartograph_X.Y.Z_universal_signed.dSYM.zip`, the debug symbols for crash
symbolication). Then perform the manual
Gatekeeper installation smoke test below and record the evidence on the release
task.

## macOS packaging

Run the **Package macOS** workflow manually for a branch, tag, or commit SHA.
The optional `ref` input is resolved once to an immutable commit; when omitted,
the triggering SHA is used. Packaging first requires successful complete-suite
evidence for that exact commit. It does not repeat generic lint, format,
typecheck, unit, Storybook, security, compliance, or docs gates. It retains the
release-specific dependency install, notices generation, universal Intel +
Apple Silicon build, signing/notarization boundary, bundle inspection, and
artifact upload.

With no signing secrets, the workflow uses an ad-hoc signature required by
Apple Silicon and uploads artifacts whose names contain `unsigned-dev`. These
are development artifacts, do not pass Gatekeeper as trusted downloads, and
must never be promoted as production releases.

## Windows packaging (#227)

Windows is not released yet. `.github/workflows/windows.yml` builds an
unsigned NSIS installer on `windows-latest` with the MSVC toolchain on every
push to main, and uploads it as the `Cartograph_<version>_x64_unsigned-dev`
artifact for manual testing. To build it locally on Windows, with Node, Rust
and the Visual Studio C++ build tools installed, run:

```powershell
npm ci; npm --prefix ui ci
npm run package:windows
```

`package:windows` builds the MCP relay (`scripts/build-mcp-relay.mjs`) and the
agent runtime from the pinned, checksum-verified Node `win-x64` release
(`scripts/build-agent-runtime.mjs x86_64-pc-windows-msvc`). It then bundles
both sidecars with `src-tauri/tauri.windows-bundle.conf.json`. The installer
downloads the WebView2 bootstrapper on systems that lack WebView2; Windows 10
1803 and later already have it. The Windows app does not carry Claude Code:
only the macOS app stages it (AC-1424).

The installer is not Authenticode signed yet, so SmartScreen warns on first
run. ADR-0093 sets the signing posture: Azure Artifact Signing, reached
through GitHub OIDC so that no key or password is stored, timestamped and
fail-closed like ADR-0016. `node scripts/signing-secrets.mjs windows` already
classifies the credential set:

| Secret | Value |
|---|---|
| `AZURE_TENANT_ID` | Tenant of the Artifact Signing account |
| `AZURE_CLIENT_ID` | App registration with a federated credential for this repository's release workflow and the Artifact Signing Certificate Profile Signer role |
| `ARTIFACT_SIGNING_ENDPOINT` | The account's regional endpoint, such as `https://eus.codesigning.azure.net` |
| `ARTIFACT_SIGNING_ACCOUNT` | Artifact Signing account name |
| `ARTIFACT_SIGNING_PROFILE` | Public-trust certificate profile name |

With none of them the build is `unsigned-dev`, with all five it is `signed`,
and a partial set fails closed. `.github/workflows/windows-installer.yml`
holds the installer job. It compiles with no signing input in the
environment (#480), then on the signed path fetches the pinned
`Microsoft.ArtifactSigning.Client` package (sha256-checked), signs in to Azure
with the GitHub OIDC token, signs both sidecars with `scripts/windows-sign.ps1`,
and lets `tauri bundle` sign the app and the installer through the same
script. Every shipped binary must then report a valid, timestamped
Authenticode signature or nothing is uploaded. `windows.yml` calls it without
secrets, so pull requests and main stay `unsigned-dev` (#631). Until a signing
caller lands in `release.yml` (#227), releases publish macOS only.

## Required GitHub secrets

Add all five repository Actions secrets together. A partial set fails closed:

| Secret | Value |
|---|---|
| `CSC_LINK` | Base64-encoded Developer ID Application `.p12` export |
| `CSC_KEY_PASSWORD` | Password chosen when exporting the `.p12` |
| `APPLE_API_KEY` | Base64-encoded App Store Connect API `.p8` private key |
| `APPLE_API_KEY_ID` | App Store Connect API Key ID |
| `APPLE_API_ISSUER` | App Store Connect Issuer ID |

The names match `qwts/photos`. The workflow maps them to Tauri's documented
Apple inputs and decodes the API key into a permission-restricted temporary
file. It never prints secret material. Tauri manages the temporary certificate
keychain, so no sixth keychain-password secret is required.

The signed path must pass all of these before upload:

- strict deep signature validation of `Cartograph.app` and signature validation
  of the DMG;
- Gatekeeper assessment of the app and disk image;
- notarization ticket/staple validation of the app and disk image.

Tauri submits and staples the application during its build. After the signed
DMG is assembled, the workflow submits that outer disk image to Apple's notary
service, waits for acceptance, and staples its ticket before running the final
checks above.

## Manual Gatekeeper installation smoke test

Perform this procedure on the versioned `signed` DMG downloaded from GitHub
Actions. Record the workflow URL, artifact filename, macOS version, hardware
architecture, and result in issue #92 (or the release task that supersedes it).

1. Confirm the filename contains the expected version, `universal`, and
   `signed`, and does not contain `unsigned-dev`.
2. Run `spctl --assess --type open --context context:primary-signature -vv
   Cartograph_*.dmg`; require an accepted/notarized result.
3. Open the DMG, drag Cartograph to Applications, eject the DMG, and launch
   Cartograph from Applications without using Control-click or a security
   override.
4. Confirm no unidentified-developer, damaged-app, or notarization warning is
   shown and the Cartograph window opens.
5. Run `codesign --verify --deep --strict -vv /Applications/Cartograph.app` and
   `xcrun stapler validate /Applications/Cartograph.app`; require both to pass.
6. On an Apple Silicon Mac, run `lipo -archs
   /Applications/Cartograph.app/Contents/MacOS/app` and require both `x86_64`
   and `arm64`.

The credential-free workflow exercise can validate configuration and artifact
shape, but it cannot satisfy this production smoke test.
