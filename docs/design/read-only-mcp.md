# Read-only MCP implementation contract

Status: in progress for #718; no installed-client or Windows acceptance claimed.
Decision: [ADR-0044](../adr/ADR-0044-read-only-mcp.md).
Trace: US-0041 / AC-0288..0290 / T-0288..0290.

## Projection, receipt and platform scope

Initial discovery returns recovered node candidates; the context snapshot also
contains edges, but the initial MCP surface has no edge-query tool. Relationships
and business flows remain
available through the official `flow-dossiers` artifact; capabilities must state
this explicitly rather than implying that absent discovery results mean no flows.

Evidence resolution uses the production receipt resolver and its version-aware
reader. No tool assumes a v1 range layout. When producer evidence explicitly
records a receipt omitted under a named limit, return `receipt_omitted`, its
reason/filling seam, and no citable target. Absence of a binding alone does not
prove omission: retain unavailable/stale/missing outcomes until #725's persisted
`(coverage/basis id, FactKey)` lookup supports this distinction. #726 records
aggregate counts and a first site per capability, not per-fact/per-file persisted
omission evidence; those counts may qualify the inventory but cannot classify
an individual missing binding. A receipt omission is neither a
forgotten capture nor an ordinary failed read.

The current resolver preserves typed failures as `evidence_unavailable`
(receipt/capture absent), `evidence_invalid` (retained evidence cannot be
validated), `stale_basis` (selection/identity changed), and `storage_unavailable`
(operational failure). Invalid does not identify the underlying cause: the
retention layer also uses that category for its own read bounds. Explicit MCP
admission limits still return `response_limit`. These fixed codes contain no
source text or filesystem diagnostics; existing desktop inspection callers keep
their original refresh message.

`evidence_absence_reason_available: false` advertises a remaining acceptance gap:
the store has no durable forget tombstone, so a missing capture cannot establish
that a human forgot it. Distinguishing positively forgotten evidence remains
required by AC-0288; the generic unavailable response does not satisfy it. Neither
clients nor the skill may infer forgotten or receipt-omitted from absence.

Delivery is macOS/Unix first. Acceptance must name the actual tested platform;
a Unix result does not claim Windows support. Windows remains a separate pending
deliverable with native security, lifecycle and packaging verification.

## Protocol and transport

For external stdio, follow the versioned MCP transport specification: UTF-8
newline-delimited JSON-RPC, with diagnostics exclusively on stderr. A framing
module validates JSON and boundaries; method, envelope and lifecycle validation
remain the MCP layer's responsibility. The framing layer rejects blank lines,
malformed JSON/UTF-8 and unterminated EOF fragments rather than dispatching them.
An idle clean EOF closes normally. Byte caps count every byte before LF, including an optional terminal CR.
CR remains JSON whitespace; it is not removed from the admission accounting.

### Recovered projection and source identity

Initial W8a distinguishes the original graph snapshot used by source receipts
from the policy-filtered discovery/read projection. The official compiler's
inclusion policy removes rejected inferred assertions and applies export mode;
`spec::project_recovered_facts` shares that policy, including suppression of
relations touching hidden BusinessRule nodes, with selected artifact compilation.
It returns original borrowed facts and does not add compiler-derived ADR/security
facts to discovery. The host must not implement a second confidence rule.
Accepted annotations do not manufacture H3. Both the raw graph basis and filtered
read identity belong in the session basis, along with the effective rejection
set and coverage qualification. A receipt-bound evidence read validates the raw
graph identity, never a filtered identity substituted for it.

Use `DecisionLog::read_curation_observation` for bounded effective rejection
policy instead of unbounded `list_assertions`; bracket it with graph/coverage and
registration acquisition. Bounded raw graph storage admission alone does not
bound all retained raw/projection/index copies. The session owner must account
for those objects and selected-artifact caches under its explicit aggregate cap.
Discovery's own search-coverage metadata is separate from producer coverage and
both remain visible in the host response.

### Local capability preamble

Before MCP admission, a verified native peer sends exactly the internal bridge
protocol `cartograph-local@1` and a 256-bit capability encoded as lowercase hex.
The server generates that capability from OS entropy (`getrandom`), with no
predictable fallback. Equal-length secret comparisons use `subtle`; errors and
acknowledgements contain no credential. Failed authentication permanently rejects
that connection. Successful authentication returns only the exact internal
acknowledgement `{ "protocol": "cartograph-local@1", "authenticated": true }`;
the relay verifies this before forwarding MCP input and never forwards this
preamble/acknowledgement to the external MCP client. Explicit revocation permanently rejects an authenticated
connection too; the host must revoke every active connection and drop sessions
on disable/restart before closing native transports. This preamble supplements
native permission/peer checks; it is not proof those checks are implemented.

### Protocol admission foundation

The initial protocol version is `2025-11-25`. Initialization validates the client
identity/capability envelope and responds with the supported version even when
the requested version differs; the client decides whether it can continue. Tool
reads become eligible only after the initialized notification. The boundary
supports ping and rejects unsupported methods without forwarding their names.
Notifications never cause a response or a read dispatch.

A closed read-tool enum and the host's explicit implemented-tool set determine
what can be dispatched. An empty set advertises no tools capability. Tool list
schemas and argument validation belong to the real host adapters; the foundation
does not return fabricated tool results. Task metadata or extension fields cannot
be treated as permission to execute work.

Request IDs are string/integer values, never null, and are unique within the
connection. A caller-selected limit bounds retained ID history. Exhaustion ends
the connection rather than evicting history and accepting a replay. The network
host must also enforce connection/frame byte limits; ID-count admission is not a
standalone memory bound. JSON-RPC batch requests and malformed envelopes are
rejected. Protocol admission does not authenticate a client: the local transport
must complete authentication first.

## Validation and delivery

US-0041 and AC-0288..0290 are reserved, with shared traceability updates to be
coordinated before ready review. The intended groups cover projection/evidence
parity, protocol/read-only enforcement, and skill/external-client measurement.

Test framing independently with short reads, exact/over-limit payloads, escaped
serialization, invalid UTF-8, malformed/deep JSON and EOF. Then test actual
allowlisted host reads against production fixture stores, stale cursors,
registration/decision races and forgotten captures. Test authentication, disable,
exit and resource bounds through real local IPC on each advertised platform.

Finally run an independent local MCP client without model calls, recording build,
protocol/client versions and transcripts. Progressive-disclosure comparisons use
the same fixed artifact and include schema overhead. Report bytes and named
actual tokenizer/version, or explicitly estimate/unknown. Token savings do not
prove answer quality. Verify installation/removal using the packaged relay, not
only a source-tree executable. Add new agent/build entrypoints to CODEOWNERS.

This work neither closes #598 nor establishes complete H5/ACP acceptance. Parked
#716/#720 remains separate. Live provider execution and target intake retain their
separate authorization requirements.

### Relay executable foundation

The separate `cartograph-mcp` console executable takes an explicit endpoint record,
nonzero maximum frame bytes and nonzero timeout milliseconds. It authenticates
before forwarding stdin, preserves buffered post-handshake bytes, and copies
bounded JSON frames in both directions. Failure or server EOF ends both pumps.
Clean stdin EOF shuts down the upstream write half and drains the remaining
server responses until server EOF or the configured deadline. Pending
stdio reads must not prevent process exit when the native peer disappears;
the Tokio runtime shuts down background blocking stdin work without waiting for
another input byte. Diagnostics are fixed/source-free on stderr; neither local
authentication messages nor logs enter MCP stdout. Unsupported native platforms
exit explicitly rather than launching a desktop fallback. A source-built relay
is not evidence of installed/bundled availability.

### Unix endpoint construction

The initial native adapter verifies an owner-controlled, non-group/world-writable
parent and creates a fresh private child directory for each activation. It binds
inside that directory and sets the socket to owner-only access before publishing
anything. Directory/socket identities are retained and checked before accepting
or publishing. Credential records use exclusive creation and owner-only mode;
no overwrite or stale-listener deletion is permitted. Cleanup removes only the
owned named objects with matching identities, then the empty directory; it does
not recursively delete substituted paths. Peer credentials are checked against
the current effective UID before any handshake. Same-user arbitrary filesystem
mutation remains outside the account-isolation boundary, as for retained storage.

The relay receives an explicit absolute `connection.json` path; it performs no
implicit profile search. The reader requires an owner-only parent, a regular
single-link owner-only record of at most 4096 bytes, and an owner-only native
socket at the fixed sibling name. It opens the record with no-follow/nonblocking
flags, brackets opened/named identities and bounds the read. Exact typed fields
reject duplicate or extra keys, unknown versions and redirected socket paths.
It revalidates identities around connection and then verifies the fixed internal
authentication acknowledgement. No MCP traffic is forwarded before that succeeds.

Frame timeouts cover the whole frame rather than restarting with every byte.
I/O or framing failure ends the connection; shutdown drops both stream halves.
Native fixtures use isolated temporary directories and never open app stores.
Windows remains a separate required implementation, not an implicit Unix fallback.

### Platform integration still required

The IPC audit found that Interprocess 2.4.6 offers native sockets/pipes and safe
Windows security-descriptor installation, but Windows peer credentials expose
only a PID, not an account SID. Its synchronous Windows stream timeouts are
unsupported. Selecting this library alone would not satisfy private account
identity or bounded shutdown. A reviewed Windows SID/token and file-ACL interface
plus cancellable I/O and native tests remain necessary. Unix can reuse Tokio peer
credentials and safe effective-UID lookup. No platform is advertised by the
protocol/capability library alone; no listener is implemented in that library.


### Windows implementation gap

The current transport is Unix-only. A source audit found an existing repository
precedent for narrowly scoped Windows FFI in `job_execution/windows.rs`; the
workspace denies unsafe code by default but does not forbid a reviewed wrapper.
Tokio's default named-pipe ACL is insufficient for this private endpoint. A future
wrapper must create protected current-user DACLs atomically, reject remote peers,
validate record ownership/ACLs/reparse points and pin file identities. Server-side
client SID verification requires immediate impersonation/token inspection/revert
with no asynchronous suspension; a PID lookup alone is not equivalent.

Relay verification of the server and cancellation/handle lifetime remain native
Windows review and test requirements. No Unix test, compile-only check or source
audit establishes them. No synchronous peer-drain/flush may enter shutdown. See
[Tokio named-pipe security attributes](https://docs.rs/tokio/1.52.3/tokio/net/windows/named_pipe/struct.ServerOptions.html)
and [Windows named-pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights).

### Blocking worker ownership

The app adapter now owns its frozen dispatcher and borrowed AppState inside a
single blocking worker, with a one-request channel. Authentication must complete
before the host invokes its factory. A shared activation ledger admits workers
without an unbounded waiting queue and reserves host-supplied conservative bytes.
The permit moves into the real worker; cancelling a socket or async waiter cannot
release it while synchronous graph copying, compilation or retained-file reading
continues. Each frame conservatively consumes the maximum allowed retained-file
validation bytes before dispatch; failure does not refund this cumulative charge.
This ledger does not measure total disk I/O, CPU work or RSS.

Revocation denies further work. The connection owner also closes transports and
drops worker senders; an already-running synchronous operation finishes with its
reservation held and its result discarded. Session lifetime bounds the client
connection, not preemptive termination of a blocking thread. Settings now supplies
human-facing activation controls with an explicit policy and no numeric defaults.
Conservative memory-cost calibration (including inspection resolver copies) and
packaged-client validation remain outstanding.


### Activation ownership

The app registers a dormant service; startup never enables it. Its internal
activation API takes explicit transport, worker and aggregate policy, returns a
fresh private connection-record path, and supplies the actual AppHandle-backed
worker only after authentication. Connections, including unauthenticated ones,
occupy bounded task slots. Completed tasks are reaped; excess sockets close
without constructing app context. Listener failure is observable as stopped.

Disable revokes the shared work ledger before awaiting native cleanup. Cleanup
continues if its command waiter disappears. A new activation is refused while
cleanup is pending or an old synchronous worker still retains its reservation;
re-enable cannot reset those budgets. Normal app exit sets a permanent shutdown
latch under the activation lock, then joins native cleanup, including a disable
already in progress. Queued enable work cannot publish afterward. Cleanup failure
is retained as `cleanup_failed` and blocks reactivation.
The lower-level activation Drop requests cancellation immediately, but filesystem
cleanup requires runtime polling; abrupt process termination is not a verified
cleanup path. Fresh activation never reuses the old capability or directory.

Native app fixtures exercise authentication through a real socket into the actual
worker/catalog and reject a mutation tool. This is separate from an independent
installed-client test and does not establish packaged relay availability.


### Human activation and packaging

Settings exposes status, enable, disable, refresh and client configuration. Every
numeric transport, graph, coverage, scope, dispatcher, evidence and aggregate limit is
required; unknown fields, zero values and inconsistent cross-field limits fail
closed. No policy is persisted or automatically activated. The example policy
contains placeholders rather than an implied capacity recommendation.

The required `scope` object supplies `max_sources`,
`max_source_registry_bytes`, `max_event_registry_bytes`,
`max_directory_entries`, `max_artifact_bytes`, `max_metadata_bytes`, and
`max_corpus_bytes`. Source row/raw SQL text limits and aggregate event-extension
file bytes apply independently to each scope observation. Directory entries and
native encoded path/name metadata are bounded per scan; a plugin scan includes
both the user and one project directory, and the event registry scan is separate.
Artifact bytes and enabled-plugin corpus bytes are aggregate per repository;
`max_sources` bounds how many repository scans run. Corpus path metadata has an
independent per-repository allowance using the same metadata ceiling. The scope
is observed before and after the graph acquisition. A host admission failure
refuses the entire session; it does not qualify a partially collected scope.

The MCP worker uses scalar plugin enablement and exact gate checks, bounded
streaming artifact hashes, bounded corpus reads before parsing, and bounded
source-registry and event-directory materialization. Official accepted event
registry identities and bounded malformed-file rejections match normal reads.
These input ceilings feed the derived logical worker minimum described below.
They do not establish an allocator or RSS bound.


Client configuration points to the packaged sibling `cartograph-mcp` executable
and the current private connection record. Arguments are separate JSON array
entries, preserving spaces without shell evaluation. Configuration contains no
capability secret and must be regenerated after reactivation. The client controls
its own provider and data handling; enabling local reads does not configure it.

The macOS compile step builds both relay architectures and stages a universal
binary. The external-binary overlay is applied during bundling only, because the
Tauri build helper otherwise requests individual architecture suffixes. Packaging
checks the bundled executable, both architectures and its signature before upload.
A local locked release build of the ARM relay succeeded on 2026-10-22 UTC
(Rust 1.96.1, app version 0.20.0; Mach-O arm64). The locked Tauri CLI 2.12.3
was also installed. The Intel standard library download initially failed locally.
The official archive was retrieved through the Mac Studio and verified against
Rust 1.96.1's installed channel-manifest SHA-256 before rustup installed it from
its cache. The actual package script now builds and combines both relay slices;
native `lipo` verifies arm64 and x86_64 separately. A native regression test also
compiles tiny Mach-O objects and exercises the actual combine/verify commands,
rather than relying only on mocked argument checks. The universal app bundle,
signing/notarization and installed-client lifecycle still need separate evidence;
a universal relay alone does not establish those results.

### Separate-process smoke evidence

The isolated app fixture explicitly runs `scripts/mcp-stdio-smoke.mjs` against
an ARM source-built release relay. This Node client uses no model. Three
independent temporary AppState instances exercise retained evidence, missing
captures and a curation-rejected fact. Production capture/extraction/receipt
publication prepares the retained fixture; its checkout is deleted before
activation. Returned spans match the original bytes and provenance, while the
graph remains unchanged. Instruction-like text is actually returned within a
span as data; no claim about model behavior follows from this transport test.

The retained scenario exercises discovery and artifact pagination plus seven
refusals: stale discovery snapshot, stale context, substituted artifact content,
stale evidence basis, substituted receipt, evidence byte cap and mutation tool.
The missing-capture scenario leaves a live replacement checkout canary in place
and verifies refusal without reading or changing that substitute. The rejected
fact has real synthetic citation evidence and a DecisionLog rejection; identifier
discovery returns no candidate and its evidence request fails closed.

The three-scenario release-relay run passed on 2026-10-22 UTC. Retained evidence
returned a 99-byte exact span. Measured stdout bytes were 69,062 (retained), 65,715
(missing capture) and 10,780 (rejected fact), respectively. No stderr was emitted.
The same-fixture artifact comparison reconstructs all 10,457 content bytes in
41 pages of at most 256 bytes and checks the identical content identity and hash
against a full-content response. Including actual request/response LF framing and
shared initialization, tool schemas, capabilities and catalog overhead, the
full-content strategy used 23,926 wire bytes; complete pagination used 69,491,
which is **45,565 bytes more**. Shared overhead was 12,042 bytes, including 5,837
bytes of tool-input schemas. This is not a savings result. A comparison for a task that needs only selected
pages requires its own measurement. Tokens and tokenizer identity remain
explicitly unknown.

The reproducible opt-in test is
`mcp_scripted_stdio_client_smoke_against_isolated_app_service`, with an absolute
`CARTOGRAPH_TEST_MCP_RELAY` pointing to the explicitly source-built relay. Its
policy limits are synthetic fixture allowances, not application defaults or
capacity calibration. Controlled scenario/stage diagnostics identify failures
without echoing arbitrary client output or source text.

These are scripted external-process results, not installed-client, signed-package,
model-quality or H5 acceptance. The client skill and packaged-client lifecycle
procedure remain required before issue closeout.

## Native acceptance and retrieval cost

[Native macOS acceptance](../acceptance/read-only-mcp-native.md) records the
tested commit, build identity, isolated app lifecycle and retained-source results.
It does not establish provider or full installed-agent acceptance.

Historical source-relay head `bf5b9b2` reconstructed 10,457 content bytes in
41 pages of at most 256 bytes. With framing and shared overhead, full content
used 23,884 wire bytes; all pages used 69,449 (+45,565). Shared overhead was
12,000 bytes, including 5,837 input-schema bytes. Tokens were unknown. These
figures are specific to that head; the native report records later measurements.

Artifact pages currently repeat the full trace and compilation. The wire-byte
measurements above exclude that CPU/allocation cost and do not establish that
pagination reduces total retrieval cost. Smaller pages do not reduce compilation
work. Session graph admission also has a 50,000-row ceiling: larger graphs cannot
open a read session, regardless of how narrow the later query would be.
Tracked by [#731](https://github.com/true-line/cartograph/issues/731).
Packaged skill discovery is tracked separately in
[#732](https://github.com/true-line/cartograph/issues/732).

Evidence visibility is frozen once from the shared compiler projection. Freshness
validation still takes two graph snapshots per describe or read; those reads use
the session's admitted graph limits. Selected object type/length and the host's
file-byte ceiling are checked before copying the blob, within the same read
transaction. Capture presence validates only the selected canonical manifest;
unrelated corrupt capture manifests no longer block that selected lookup.

Source registry, plugin discovery, corpus and event-registry reads now require
explicit bounded acquisition before session construction. All activation entry
points share policy validation before endpoint creation. The versioned
`mcp-transport-payload-v1` logical accounting model reserves every admitted
connection's read-ahead, input bytes, parsed message/argument roles, response DOM
and serialization buffers before admitting workers. This includes unauthenticated
connections and terminal responses that can outlive a worker. The worker ledger
receives only the aggregate remainder; overflow or insufficient space rejects
activation. Logical JSON header/index charges are deliberately uniform accounting
units, not claims about allocator or B-tree layout.

The `mcp-worker-payload-v1` model derives a minimum from module-owned acquisition,
retained session, tracing, projection/compiler, discovery, evidence and protocol
construction roles. Admission reserves the greater of acquisition and retained
session plus request IDs, decoded arguments and the largest sequential request
phase. Checked arithmetic rejects overflow. Activation and direct worker startup
reject a caller reservation below this minimum; the worker also counts request
serialization before queueing and enforces the activation's input-frame bound.
Fixed tool schemas and capability metadata are counted even when the configured
response ceiling is smaller than their construction workspace. Security-derived
rows and bytes are admitted before supporting-node materialization.

Receipt lookup checks SQL cell type and byte length before decoding identity or
payload fields, then applies the existing canonical receipt validation. Missing
and malformed rows remain distinct; no omission or forgotten classification is
inferred from either result.

Both logical accounting models exclude allocator slack, SQLite/executor/kernel
caches and native stacks; neither is a heap/RSS ceiling. The tests establish
checked admission and logical role coverage, not total process memory containment.
