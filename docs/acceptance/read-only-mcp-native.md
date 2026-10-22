# Read-only MCP: native macOS fixture observations

Status: partial acceptance, 2026-10-22 UTC. Issue #718, PR #727;
AC-0288..0290 and MT-W8A-01..03 remain open. These observations do not
complete an entire manual procedure or establish installed agent-client quality.

## Build and isolation

Tested implementation: `ec3f366ef1bd78fcc3536828788903d0a3ca8746`.
Rust 1.96.1, Tauri CLI 2.12.3, locked Tauri runtime 2.12.4; macOS 27.0.1
(26A434), ARM64 execution. Both app and relay contain arm64 and x86_64 slices;
Intel execution was not tested. Windows MCP is unsupported by this slice.

Universal compilation, ad-hoc `.app` signing, strict signature verification and
headless DMG creation succeeded. `hdiutil verify` accepted the DMG. Production
Developer ID signing, notarization and Gatekeeper installation were not tested.
The production-identifier app was not launched against the owner's profile.

For native tests, a separate compile overlay set:

```json
{
  "productName": "Cartograph MCP Fixture",
  "identifier": "ai.cartograph.fixture.mcp.ec3f366",
  "app": {
    "appDirectoriesOverride": "/tmp/cartograph-ui-testing/mcp-fixture-ec3f366/profile"
  }
}
```

The existing relay bundle overlay was also applied at packaging time. The fixture
identifier and explicit profile path were verified in the built executable;
the profile was absent before first launch. Live open-file inspection confirmed
application databases under that profile. WebKit maintains its own separate
store under the unique fixture identifier. No owner app databases were opened.

| Artifact | SHA-256 |
| --- | --- |
| Fixture app executable | `522cbe81b73b30b56cd900f946bf22c7aea68b71fc5e981130936394523a0ef2` |
| Packaged relay | `d4b084d080406a1f7a09c14579a729692762f623c3fbd224eeb7530ceb3bdb28` |
| Production-identifier ad-hoc universal DMG | `8859c82e48d2a88d74dc32d4b6237de773f47a3d1ff0ed6cc4e530b5c221e161` |

All activation occurred through native Settings. The explicit policy transcribed
`mcp_reads::client_smoke_tests` and `worker::tests::limits`: one connection/worker,
65,536-byte frames, 10-second frame deadline, 30-second session lifetime,
128 request IDs, 1,000,000 reserved accounting bytes and 600,000 cumulative
read-accounting bytes. Graph/coverage used their existing test defaults. These
are synthetic test inputs, not calibrated production defaults, investigation
ceilings, process-RSS measurements or permission for provider execution.

## Observed lifecycle and boundaries

An independent Node process used the actual packaged console relay; a separate
socket probe tested the native app's authentication/admission boundary. No
provider, model, SDK agent session, real-target intake or credential logging
was used. Raw socket probes ran as the same OS user; they do not prove rejection
of a different OS user.

| Case | Observation |
| --- | --- |
| First launch and restart | Native Settings showed disabled; configuration unavailable; no policy persisted across restart. |
| Explicit activation | Native Settings showed running; activation directory mode 0700 and record mode 0600. |
| Negotiation | Protocol `2025-11-25`; exactly five advertised read-only tools. |
| Disable with stdin open | Relay exited normally at 3,718 ms; record removed. |
| Re-enable | Old record refused with no stdout. New record connected. |
| Native app quit with stdin open | Relay exited normally at 5,583 ms; app process and record gone. Both quit/revoke exits preceded frame/session deadlines. |
| Client EOF | Final ping response ID 999 arrived after EOF, then normal exit at 7 ms. Client disconnection alone left service running. |
| Malformed JSON | Relay rejected with no stdout and bounded generic diagnostic. |
| Oversized frame | 65,538 framed input bytes against 65,536 cap rejected with no stdout and generic frame failure. First rapid attempt hit connection refusal instead; it was retained as inconclusive for frame enforcement, then separately retried. |
| Duplicate request ID | One response to original ID, then connection closed. |
| Unknown mutation tool / task request | Correlated errors -32602 / -32601. |
| Wrong capability / MCP before authentication | Connection closed with zero response bytes. |
| Concurrent connections | Second client refused at max_connections=1; new client authenticated after first disconnected. |
| Unauthenticated idle deadline | Connection closed at 10,004 ms. |
| Authenticated lifetime | With pings every second, relay exited normally at 30,010 ms with stdin open; no stderr; 45-second driver watchdog did not fire. |
| Cumulative budget across clients | First connection admitted 102 frames (101 responses plus initialized notification); second admitted 44 (43 responses plus notification). Total charge 598,016 bytes. Next 4,096-byte charge would exceed 600,000 and was refused; a third connection could not initialize. Reconnection did not refund charges. |

Rapid serial connection cleanup at the one-connection limit was not characterized
as a latency guarantee. Remaining transport work includes different-user peer
checks and the full reservation/cancellation/cache matrix in the manual procedure.
The fixture service was explicitly disabled after each test group.

## Native retained evidence and official artifact identity

Native Connect/Preflight/Run full recovery ingested only the `RETAINED` synthetic
TypeScript source from `src-tauri/src/mcp_reads/client_smoke_tests.rs`. It produced
12 nodes and 10 edges; the UI reported zero egress. The source folder was renamed
away during the read, then restored. The original source path was unavailable;
retained stores were not modified.

The existing `scripts/mcp-stdio-smoke.mjs` passed against the actual native app
and packaged relay with its explicit 65,536-byte frame cap, 10-second timeout,
524,288-byte output cap and 4,096-byte stderr cap. It verified two discovery
pages, artifact reconstruction, retained source identity/range and seven
stale/substituted/byte-cap/mutation refusals. This remains a scripted client test.

The returned source range 137..236 was 99 bytes. SHA-256 of that range, independently
read from the restored synthetic fixture, matched the returned evidence hash:
`96302f89516cd78c7274affc984dc7bba27b6981ace5ad16d6abe8608182e2f7`.
The capture file digest uses BLAKE3, not SHA-256; no cross-algorithm comparison
is claimed. Instruction-like source strings remained quoted data.

The native verified-only Spec Workbench showed the same `rule-evidence`
content identity as MCP:
`spec-artifact-v1:ed1cb5cbf1d5b42bfcbadd6e4476cfd1e5f545a1f900cb25a098d7ff8fd65371`.
It also showed the first rule's 137..236 source span and locked Confirmed curation
controls. This establishes displayed identity/span parity, not exported-file parity.

## Disclosure measurement

Comparison: same session, same complete selected artifact, UTF-8 wire bytes
including protocol envelopes, LF framing and shared schema overhead. Tokens
were not measured. Reconstructing every small page is intentionally not an
example of selective retrieval savings.

| Measure | Bytes/count |
| --- | ---: |
| Complete artifact content | 10,575 bytes |
| Pages at 256 content bytes | 42 |
| Shared protocol overhead | 12,042 bytes |
| Tool input schemas | 5,837 bytes |
| Full-content read including shared overhead | 24,044 bytes |
| All progressive pages including shared overhead | 70,754 bytes |
| Additional wire cost of complete paging | 46,710 bytes |

All pages reconstructed the complete MCP artifact exactly. Progressive disclosure
can avoid unread content when a client stops early; this measurement does not
prove token savings, answer quality, complete input closure or H3 activation.

## Export failure and remaining acceptance

Clicking native Export bundle caused computer-use `noWindowsAvailable`, followed
by accessibility/screenshot timeouts. The process remained alive. A one-second
native sample showed its main thread in WebKit download destination handling,
then SandboxExtension / `_sandbox_extension_issue` / `__mac_syscall`. No export
file was verified. This is one observation on the ad-hoc fixture; root cause and
production reproducibility remain unknown. The exact fixture process was
terminated and reopened with its 12-node/10-edge graph intact.

Full MT-W8A procedures remain incomplete: exported-file parity, remaining native
boundary matrix, independently installed agent-client/skill workflow, cross-review,
owner approval and exact-head CI are outstanding. Positive forgotten-capture and
per-fact receipt-omitted classifications remain unavailable; absence must not be
reinterpreted as either. Production notarization is not established.

Local raw records and helper probes were retained under
`/tmp/cartograph-ui-testing/mcp-fixture-ec3f366/`: `REPORT.md`,
`bundle-verification.json`, `native-retained-smoke.json`, `native-boundaries.json`,
`native-oversize-retry.json`, `native-auth-capacity.json`,
`native-cumulative-budget.json`, lifecycle JSONL records and the export process
sample. These local files are supporting run evidence, not repository fixtures
or CI certifications. This document preserves the reviewed scope and observations
when that temporary directory is unavailable.


## Later bounded implementation: ebea61a

The observations above retain their original build scope. A later clean source
build at `ebea61aefc1bd462b0862815ffc6483cc1ef5e6a` was exercised separately
with an ARM debug app and its packaged console relay. This disposable fixture
reports v0.20.0; the owner's installed app was v0.19.0 and was not tested or
modified. This later bundle is not universal or production-notarized evidence.
Its uniquely named app used SQLite backups of the same synthetic 12-node,
10-edge fixture, with an explicit isolated profile. An initial duplicate-instance
attempt was excluded; the accepted lifecycle sequence was repeated with one
process holding the disposable graph.

### Observed current-build behavior

- Native Settings starts disabled and has no available client configuration.
  Explicit activation accepts the derived logical reservation; exact minimum
  and one-below worker/aggregate validation were checked separately. Logical
  reservation units do not establish RSS or recommended deployment settings.
- Packaged relay smoke negotiates protocol 2025-11-25, lists five read tools,
  discovers facts, reconstructs an artifact and resolves retained source. It
  rejects stale/substituted identities, insufficient response bounds and mutation
  requests. EOF drains the final response before exit.
- Disabling the service closes a live relay with stdin open. Re-enabling refuses
  the previous activation record. Native quit closes a live relay; restart is
  disabled. Activation records are removed after shutdown.
- Installed official MCP Inspector CLI 2.12.0, using an isolated private daemon,
  completed capabilities, bounded BusinessRule discovery, artifact catalog and
  page, retained-range description and exact span read with one session/basis.
  Retrieved instruction-like text was treated as evidence. This is operator
  workflow evidence, not an evaluation of model resistance or answer quality.
- Fresh native boundary cases passed: malformed JSON, oversized frame, unknown
  tool, task request and duplicate ID. Frame failures were distinguished from
  authentication refusal; every revised-run case passed its first attempt. The
  earlier failed driver log remains preserved rather than counted as a pass.
- Same-user socket probes rejected wrong credentials and MCP-before-auth,
  enforced the single-connection allowance, restored capacity after release and
  enforced the 10-second unauthenticated deadline. No different-OS-user claim.
- Separate unused activation admitted 146 charged dispatches across connections:
  128 responses, then 16, then none. Notifications also consumed allowance.
  The accumulated charge was 598,016 bytes; the next 4,096-byte charge exceeded
  the unchanged 600,000-byte ceiling. The native app remained healthy.
- A separate activation accepted 128 distinct IDs and closed without replying to
  ID 129 while cumulative allowance remained sufficient. Another session, with
  stdin open and pings every 3,333 ms, closed at 30,006.748 ms. Its last send was
  3,217.702 ms before close, below the 10-second frame deadline. The existing
  session lifetime was 30 seconds; the harness disclosed a 2-second scheduling
  tolerance without changing policy. Native Settings remained responsive.

After these checks, the private client daemon was stopped, native access disabled,
and the fixture quit. No activation record remained. No provider execution,
model download, real-target ingestion or owner database access occurred.

### Evidence and remaining scope

Current-build reports are under the local review directory
`/tmp/cartograph-ui-testing/mcp-fixture-ebea61a/`: `REPORT.md`,
`bundle-verification.json`, `single-*` lifecycle records,
`inspector-workflow-transcript.json`, `native-auth-boundary.json`,
`native-boundary-isolated.json`, `native-budget-derived.json`,
`native-request-id-cap.json` and `native-session-lifetime.json`.
The earlier disclosure byte comparison remains attached to its original source;
these new checks do not relabel it as a measurement of ebea61a.

Full procedures remain incomplete. Controlled registration/rejection/coverage
changes and retention-cleanup races still require their complete observed
matrix, along with same-build disclosure accounting and exported-file parity.
The actual no-cache implementation recompiles each artifact page; no cache-hit,
eviction or bounded resident-cache behavior is claimed. Positive forgotten and
per-fact omission capabilities remain false until their producer-backed resolver
integration is verified. Production installation/signing and other-platform
native evidence are not established by this ARM fixture. Required cross-review,
owner review and current-head CI are separate from these observations.


## Integrated export and independent client — 94c7cf3

Tested signed source `94c7cf3f4207a55e41dd1e442f295a5fe73f2741`, tree
`47e4fbebaf32f5f8e38331bdaf8c72aca1d9b9cc`. This was a separately identified
ARM macOS debug v0.20 fixture, with its packaged relay and an isolated copy of
the existing 12-node/10-edge synthetic profile made through SQLite backups.
The owner's installed v0.19 app and stores were untouched. No ingestion,
provider execution or model download occurred.

The current production reservation functions accepted the derived exact worker
and aggregate minimum and rejected each one-below value. These are logical
payload accounting units, not resident-memory guarantees. The initial derivation
driver selected zero tests because its exact filter was incomplete; the corrected
full-name invocation executed one passing test. The temporary test-only include
was removed before building the app from the clean published source.

### Native export parity

The service started disabled. In Spec Workbench, verified-only export of the
selected source-rule evidence opened the native Save dialog and produced a
91,899-byte JSON file; the app remained responsive. No graph, registration or
curation change occurred between export and MCP retrieval.

The packaged-relay retained smoke and offline parity checks passed. The exported
artifact and complete MCP reconstruction have the same schema
`cartograph.spec/rule-evidence@1`, all 11 assertions, 10,575 UTF-8 content bytes,
and official identity
`spec-artifact-v1:ed1cb5cbf1d5b42bfcbadd6e4476cfd1e5f545a1f900cb25a098d7ff8fd65371`.
Content SHA256 is
`b654657ba0d2b65b02533c1959e06b750fa77fd4f595307a74558ba7561ba2a9`.
All nine parity checks passed, including complete content/assertions and the
export's duplicate `files` member. Whole exported JSON size is a separate measure
from artifact content or protocol wire size.

### Disclosure and independent client

The same-session, same-artifact complete-content comparison measured 24,409 wire
bytes for one complete response versus 71,119 for all 42 progressive pages.
Both include 12,407 shared protocol bytes; tool schemas account for 5,837 bytes
within that overhead. Retrieving every page costs more. Token counts remain
unknown; these measurements do not establish savings for a selective task or
model answer quality.

On a separate fresh activation, installed official MCP Inspector `mcpdo` 2.12.0
followed the repository skill: list tools once, capabilities, one BusinessRule
candidate, artifact catalog, a 512-byte selected artifact page, retained evidence
description and the exact returned span. All six tool calls passed on one
session and basis. The retained fixture's instruction-like text was treated as
quoted data; it did not authorize a mutation or provider call. This was an
operator-driven client test, not a model-resistance evaluation. Raw Inspector
wire bytes were not measured and its JSON transcript size is not a substitute.

After each run, native read access was disabled and the fixture quit normally.
The private Inspector daemon was stopped and activation-record absence verified.

Reports are under `/tmp/cartograph-ui-testing/mcp-fixture-94c7cf3/`:
`bundle-verification.json`, `accounting-receipt.json`, `native-smoke.json`,
`export-parity.json`, `inspector-workflow-transcript.json`,
`inspector-result.json` and `REPORT.md`.

### Remaining acceptance

This supersedes only the earlier pending export-parity and current-head
independent-client observations. The full controlled registration, rejection,
coverage-change and retention-cleanup concurrency matrix remains incomplete.
Per-fact omission classification still lacks its producer contract; the new
forgotten-only capability is explicitly partial and broad absence remains false.
No cache exists to exercise eviction. Other-platform MCP, production signing,
owner review and current-head CI are not established by these fixture runs.


## Controlled qualification follow-up — current unbound selections

The controlled driver was extended after the `94c7cf3` native run. It exposed
a classification defect: a current visible fact lacking a source binding was
reported stale. The production resolver now checks the expected bounded raw
snapshot identity before classifying that absence as `evidence_unavailable`.
A changed graph, removed fact, or previously supplied receipt target whose
association disappeared remains stale. Invalid binding metadata remains invalid.
The desktop Inspector retains its existing stale-selection behavior.

The post-fix driver executed the working-tree patch on signed docs-only base
`e1f3efc0b4331660d286e3447edd1ac817a58b43`. Its production patch and before/after
SHA256 inventories are retained under `mcp-fixture-94c7cf3`; temporary test
adapters were removed and restored bytes verified. This is not an unchanged
`94c7cf3` binary rerun. Focused MCP tests passed 60 tests with one existing
ignored scaling test; the external driver passed two tests with none ignored.

Observed cases include both-mode confirmed, inferred-strong and inferred-weak
controls; rejected adjacency; official selected artifact parity; discovery
query/context refusal; 71 UTF-8 artifact pages per mode; and content identity
and response-cap refusals. Qualification controls include explicitly synthetic
cited BusinessRules and do not claim adapter-produced inferred facts. Actual
registration and rejection changes inside acquisition brackets refused stale
results. Coverage-only publication invalidated replay into a fresh session;
the original immutable session remained coherent.

Retention evidence composes the production receipt resolver's held guard with
the bounded span reader: supported forget refused while the guard was held,
retained bytes remained readable after the isolated checkout was deleted, and
forget succeeded after release with subsequent evidence classified forgotten.
This is not a paused single native MCP request and does not independently prove
every literal concurrency step in MT-W8A-01. The report is
`qualified-matrix-postfix-report.md`, with its events and exact source patch.

A separate closed-app packaged-relay probe returned exit 1, zero stdout and the
fixed unavailable diagnostic without launching the fixture. It used the revoked
or missing activation record; it does not establish a valid-record/dead-endpoint
case. See `closed-app-no-launch.json`. Per-fact omission remains unimplemented
and broad absence capability remains false.

## Private Inspector connection lifecycle — supplemental 94c7cf3 run

A follow-up on the same isolated `94c7cf3` ARM fixture used installed Inspector
2.12.0 with a new private daemon and disposable catalog. A transparent stdio
wrapper launched the packaged relay and retained actual JSON-RPC envelopes for
initialize, initialized notification, tools/list and capabilities. Observed
request IDs 0–3 each had exactly one matching response; the notification had no
response. This supplements the earlier six-call artifact/source transcript; it
does not retroactively add raw envelopes to that earlier run.

While connected, `lsof` observed the relay executable, dyld, its working
directory, standard pipes, kqueues and the local Unix socket. No application
store file or network socket was open at that observation. The separate
`otool -L` record shows libiconv and libSystem. These observations support the
source/dependency audit; they are not a proof of every possible future process
state or static dependency.

The client disconnected and its disposable catalog entry was removed through
the documented catalog-edit route. Inspector reported no configured servers
and no live connections. Refreshing the app's Settings still showed the read
service running: client removal does not revoke app access. Access was then
explicitly disabled, the fixture quit, and relay termination and activation
record absence were verified. The private daemon was stopped. The original
configuration remains as evidence, separate from the now-empty live catalog.

Evidence is in `mcp-fixture-94c7cf3/client-lifecycle/`: `protocol.jsonl`,
`relay-open-files.txt`, `catalog-before.json`, `catalog-after-list.json`,
`connections-after.json`, `ui-after-client-removal.txt` and `result.json`.
This closes the previously unobserved process-inspection and client-entry
removal cases for this fixture. Different-OS-user peer rejection, non-ARM
execution and production signing remain unestablished. No provider execution,
owner-store access or real-target ingestion occurred.

## Native receipt-omission follow-up — 2026-10-22, a7915641

A separately identified ARM macOS debug fixture was built from clean signed
`a79156416ce7fd2508c4bc37c54bc46d118354ee`, tree
`2bde9044ce81f0201c62d1c4fe9ffa7e14b687c5`, version 0.20.0. Its identifier was
`ai.cartograph.fixture.omissiona791`; the embedded disposable profile was verified.
Preparation-only ignored test helpers were restored before the native build.
They used real capture and `run_ingest` with fixture-only receipt capacity one,
plus the existing per-file declaration bound, to preserve retained,
`store_capacity`-omitted and unrecorded controls. No omission rows were inserted
manually. The generated graph had 1,034 nodes and 1,031 edges; no owner database,
real target or provider was used.

Native Settings enabled the explicit policy derived from current production
reservation functions. Derivation accepted the exact worker/aggregate floors
and refused each one-below value; offered policy ceilings were unchanged.
These are logical accounting units, not RSS measurements. On one authenticated
session, the external Node client used the fixture's packaged relay and recorded
exact JSON-RPC requests and results:

- The current unbound omitted fact returned `isError: true`,
  `error: "evidence_omitted"` and `omission_kind: "store_capacity"`, with no
  target or span. The separate omission capability was available and partial;
  broad `evidence_absence_reason_available` remained false.
- A current retained binding yielded a descriptor and exact 13-byte span,
  `return false;`. Its SHA-256 was
  `dabbdf16b8204c2b7217958e893ae2d600d2f01a226e3e0d8e7d1ef950819832`,
  independently calculated from the prepared source bytes at the manifest's
  original range. The native descriptor/read targets matched; the fixture's
  separate BLAKE3 manifest hash was preserved and labeled.
- The genuine unrecorded control returned `evidence_unavailable`, never an
  inferred omission. All calls retained the same acquired session and basis.
- Relay EOF drained cleanly with exit zero, no signal and no stderr. Native
  access was disabled and the fixture quit; cleanup found no fixture processes
  or connection records.

Two failed harness attempts remain recorded. Run 1 supplied the wrong connection
record path and failed initialization; the real record used a generated
`mcp-runtime/mcp-*/connection.json` subdirectory. Run 2 reached retained read but
the client incorrectly compared SHA-256 with the manifest's BLAKE3 digest.
The client was corrected to calculate expected SHA-256 independently from the
prepared source file, checking containment and byte bounds. Run 3 passed every
assertion; returned text was never used as its own expected-value oracle.

Artifacts are under
`/tmp/cartograph-ui-testing/mcp-fixture-a791-omission`: `fixture-manifest.json`,
`accounting-receipt.json`, `bundle-verification.json`,
`native-omission-result.json`, `native-omission-result-run2.json`,
`native-omission-result-run3.json`, the preserved pre-correction client and
`cleanup.json`. Reports contain no bearer credential or raw relay stderr.

This supersedes the earlier omission-pending labels for this consumer branch;
it does not relabel historical binaries or qualification/guard observations.
The retained guard-composition evidence remains separately attributed above.
This run does not establish native results for every omission kind, Windows or
non-ARM execution, production signing, different-OS-user peer rejection, or a
complete MT-W8A-02 pass. Those are not implied by the positive omission result.
No requirement to run all three omission kinds natively is introduced; the
closed kind inventory and wire behavior retain their automated evidence.
