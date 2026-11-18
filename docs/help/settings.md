# Settings

Recovery tiers, providers, consent, and adapters.

- **T0 is always on** — it is the product's floor, not configuration.
- Cloud fails closed: enabling a cloud provider requires an explicit
  disclosure and revocable consent, and every call still shows its exact
  payload for a per-action grant. The status bar reports live egress.
- **Semantic (T2) off** means nothing is embedded. Capabilities, domain
  entities, actors and business flows are still derived, because those rules
  are deterministic and call no model.
- After each ingest a background **knowledge phase** derives those nodes. If
  it skips or leaves out work, a notice at the top of the window names each
  reason. It stays until you dismiss it or the next ingest changes it.
- **Adapters** lists every installed extractor (and what it covers) from the
  same registry Preflight consults, plus known adapter types you can request.

## Provider keys

A cloud provider needs an API key. In **Provider keys**, type the key and choose
**Save key**. Cartograph stores it in the system credential store (the login
Keychain on macOS, the Credential Manager on Windows) and nowhere else: it is
not written to the settings file, a log or an export, and the app never shows
it again. macOS may ask you to allow Cartograph to use the Keychain.

The section shows whether a key is saved. **Replace key** overwrites it and
**Remove key** deletes it. With no key the cloud lane stays closed.

If the `ANTHROPIC_API_KEY` environment variable is set when Cartograph starts,
it is used instead of the saved key, and the section says so. An app opened from
Finder or the Dock does not see variables set in a terminal, so use the saved
key there.

Saving a key sends nothing and grants nothing. Each cloud step still shows the
exact payload and asks for your consent.

**Check connection** tells you whether the provider accepts the key. It sends
one request for the provider's model list, with your key and nothing else: no
source text, no prompt and no graph content. Because nothing of yours is sent it
asks for no consent, and its size is added to the egress record like any other
request. The answer is one of: key accepted, key refused, or provider
unreachable. It stops after 10 seconds.

## Local lane

The local lane runs models through Ollama on this machine. **Local lane** shows
whether the Ollama server is running and which of the models Cartograph uses
are installed. If the server is not running, it shows the command that starts
it (`ollama serve`). For each missing model it shows the exact command to fetch
it, for example `ollama pull qwen3:8b`. Run the command in a terminal, then
choose **Check local lane** again. Cartograph never downloads a model itself,
and this check sends nothing off this machine.

## Read-only MCP

The local MCP service starts disabled. In **Read-only MCP**, expand the advanced
resource policy and supply all required positive limits before enabling it.
The example shows the required fields, not recommended capacity settings.
Byte limits and worker reservations bound admitted work; reservations are not
measurements of process memory. Use limits agreed for your machine and data.

Once running, choose **Show client configuration** and copy the generated
configuration into your MCP client. The macOS package includes the
`cartograph-mcp` relay; an installation without that executable cannot generate
usable client configuration. Windows transport is not available in this release.

Clients can inspect capabilities, discover recovered nodes, list and read
compiled artifacts, and describe or read retained evidence. These tools cannot
start ingestion, change confirmed facts, or execute Cartograph providers.
Your connected client controls its own model and provider, so consider its data
handling before connecting it to repository evidence.

A complete-session client can also read **curated** and **curated-json** with
the artifact tool. `curated` is the same **curated.md** an export holds for the
same reviews, graph and export mode. `curated-json` gives the same content as
data: the curated revision, the graph it was computed over, each accepted
proposal with its proposed relation, tier, confidence, state and reason, and
the counts. This shows the client which proposals you accepted and why each is
or is not a curated item; review notes are not shown. Both also list the
investigation findings you accepted as curated: the eligible ones with their
statement and the facts they cite, and the others with the reason only. A
`verified-only` service shows no curated item and no finding, only how many it
left out. The session's basis
carries the curated revision, and `h3_curated_available` is true only when the
session computed it; otherwise the basis and both documents give the reason
and every other read still works. One such reason is a Workbench decision
that matches no recovered fact: checking it needs every document compiled,
which a client session does not do, so read **curated.md** in an export
instead. A review made while a session starts makes
that read fail as stale; read again. A scoped session offers no curated read.

**Disable** revokes the service. Already-running local work may finish before
its reserved capacity is released; refresh while the status is draining.
If cleanup fails, Settings reports it and blocks new access; a disabled status is
not reported as though endpoint removal succeeded. Re-enabling creates a new endpoint, so regenerate the client configuration after
re-enabling or restarting Cartograph. Removing an entry from a client's
configuration does not disable the service in Cartograph.
