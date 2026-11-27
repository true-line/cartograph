# First run: install, set up, first ingest

This page takes you from a downloaded build to a first recovered system. It
describes version 0.81 on macOS and Windows.

You need no model provider to recover a system. The deterministic tier (T0)
reads the code on this machine and calls no model. The local lane and a cloud
provider are optional and are set up afterwards.

## 1. Install and open

Download the file for your machine from the release page of the repository.

**macOS**

1. Open the `.dmg`. The build is universal: one file for Intel and Apple
   Silicon Macs.
2. Drag **Cartograph** to **Applications**, then open it.

A signed release opens like any other app. If the file name contains
`unsigned-dev`, it is a development build: macOS says it cannot check it and
does not open it. To open it anyway, choose **System Settings → Privacy &
Security**, scroll to the message about Cartograph, and choose **Open Anyway**.
You do this once. Only do it for a build you got from the project's own
release page.

**Windows**

1. Choose `x64_signed-setup.exe` for most PCs, or `arm64_signed-setup.exe`
   for a Windows on Arm PC.
2. Run it and follow the installer, then open **Cartograph** from the Start
   menu.

The installer is signed. Windows SmartScreen may still show **Windows
protected your PC** while the signing certificate is new to it. Choose
**More info**, check that the publisher is shown, then **Run anyway**.

Keyboard shortcuts in this help use `Cmd` as on macOS. On Windows use `Ctrl`
instead; quick dispatch is `Ctrl+J`.

## 2. What works with no provider

Everything in the recovery path:

- **Connect → Preflight → Recover** on a GitHub repository, a local folder or a
  system manifest.
- The **Atlas**, **Flows**, **Spec**, **Gaps** and **Provenance** views of what
  was recovered, and the exports.
- Capabilities, domain entities, actors and business flows derived by rule.

The status bar reads `Local-only · 0 bytes egress`. That figure counts what is
sent to a model provider: none of your source content goes to one until you
turn on a cloud provider and consent to a specific step.

Fetching a repository is separate. If you connect a GitHub repository,
Cartograph clones it from GitHub when recovery starts, using your GitHub
access for a private one. That request is not part of the egress figure. A
local folder is read in place and makes no network request.

What a provider adds: similarity search and embeddings (T2), and proposals and
specialist investigations that reason over the recovered facts (T3). Those
results are always marked as inferred.

## 3. First ingest

1. Open **Connect** and point at a repository or folder, or drop a folder on
   the window.
2. **Preflight** depends on what you connected. For a local folder it scans
   the files and shows which languages and frameworks it can read, and which
   constructs it cannot. For a GitHub repository or a system manifest it
   shows **Detection deferred**: the files are not on this machine yet, so
   detection happens when recovery starts. That screen is expected. Nothing
   is recovered at this step either way.
3. **Recover** runs the extraction as a job you can cancel. The summary lists
   files, nodes and edges per language, including zeros.

Where to look afterwards:

- **Workspace** lists the coverage inventory: which producers ran for each
  repository and why any did not.
- **Atlas** shows the system; **Flows** follows a request through it.
- **Gaps** lists what could not be resolved. A gap is shown as a gap, never
  filled in.
- If a notice headed **Knowledge phase** appears at the top of the window, the
  step that derives capabilities and similar nodes skipped something. It names
  each reason. The **Settings** help topic explains it.

## 4. Set up the local lane (optional)

The local lane runs models on this machine through Ollama (ollama.com).
Nothing leaves the machine.

1. Install Ollama and start it: open the Ollama app, or from a terminal run
   `ollama serve`.
2. In Cartograph open **Settings → Local lane**. It shows whether the server
   answers and which models are installed.
3. For each missing model it shows the command to fetch it. Run each command
   in a terminal.
4. Choose **Check local lane** again. Cartograph never downloads a model
   itself.
5. In **Settings**, turn on **Semantic (T2)** and, if you want proposals,
   **Agentic (T3)**, and leave their provider on local.

The three models and their commands:

- Embeddings: `ollama pull nomic-embed-text`
- Triage: `ollama pull qwen3:1.7b`
- Proposals: `ollama pull qwen3:8b`

## 5. Add a cloud provider (optional)

1. In **Settings → Provider keys**, paste your API key and choose **Save key**.
   It is stored in the macOS Keychain, or the Windows Credential Manager, and
   nowhere else, and the app never shows it again. macOS may ask you to allow
   Cartograph to use the Keychain.
2. Choose **Check connection** to confirm the provider accepts the key. That
   request carries the key and nothing from your sources.
3. In **Settings**, set a tier's provider to cloud. The app shows a disclosure
   of what that tier can send and asks for your consent. You can revoke it at
   any time.

Consent has two levels, and both are needed:

- **Tier consent** allows a tier to use the cloud at all.
- **Per-step consent**: each cloud step shows the exact payload that would
  leave and waits for you to approve it. Secrets are redacted from that
  payload first.

Saving a key grants neither. Without both, cloud calls are refused. The status
bar shows the total bytes sent.

## If something does not work

- **The app does not open**: see step 1 for an `unsigned-dev` build on
  macOS, or the SmartScreen prompt on Windows.
- **A language shows zero files**: Preflight and the coverage inventory say
  whether an adapter exists for it. The **Ingest** help topic has the detail.
- **"Provider not configured"**: for the local lane check **Settings → Local
  lane**; for cloud check **Settings → Provider keys**.
- **Cloud is on but nothing is sent**: the tier needs consent, and each step
  still waits for your approval.
