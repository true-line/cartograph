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
