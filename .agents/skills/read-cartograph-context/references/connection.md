# Connection lifecycle

Requires a Cartograph build that includes read-only MCP (Unix only).
Use an owner-approved build with the packaged relay.

In Cartograph Settings, enable Read-only MCP only within the user's authorized
scope and with explicitly supplied resource policy. This skill supplies no
numeric activation defaults. Copy Show client configuration into the chosen
client's normal MCP configuration surface. Preserve the executable path and
separate argument array; do not turn it into a shell command. The current app
supplies a per-activation private connection-record path, not a bearer secret in
the copied configuration. An unavailable relay is a packaging/configuration
failure; do not substitute the desktop executable as a stdio server.

The local app must remain running. Its service starts disabled and is not
persistently enabled. Disable revokes access; old workers may drain. Failed
cleanup blocks reactivation rather than reporting success. Re-enabling or
restarting requires fresh client configuration. Removing a client entry does
not disable the app service; disable in Settings when access should end.

The connected client controls its own model/provider and data handling. Local IPC
does not imply that the external client keeps retrieved source local. This skill
neither configures a provider nor grants egress consent.

The implementation is Unix-only, with macOS packaging integration. Windows MCP
transport is unavailable. Check the build-specific acceptance report for packaging and installed-client
validation; a source-built relay fixture alone does not establish either.

For skill installation, retain this complete skill folder and its references in
the selected client's supported skill location using that client's normal
installation workflow. Installation/removal is a separate requested action; do
not copy into global client configuration merely because the skill was read.
