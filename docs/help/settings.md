# Settings

Recovery tiers, providers, consent, and adapters.

- **T0 is always on** — it is the product's floor, not configuration.
- Cloud fails closed: enabling a cloud provider requires an explicit
  disclosure and revocable consent, and every call still shows its exact
  payload for a per-action grant. The status bar reports live egress.
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

**Disable** revokes the service. Already-running local work may finish before
its reserved capacity is released; refresh while the status is draining.
If cleanup fails, Settings reports it and blocks new access; a disabled status is
not reported as though endpoint removal succeeded. Re-enabling creates a new endpoint, so regenerate the client configuration after
re-enabling or restarting Cartograph. Removing an entry from a client's
configuration does not disable the service in Cartograph.
