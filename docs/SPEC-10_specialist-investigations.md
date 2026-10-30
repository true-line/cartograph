# SPEC-10 — Durable specialist investigations

Status: implementation contract for #404, child of #256/#255. No implementation
or delivery gate is claimed by this document. Extends SPEC-01 and reuses the
source and execution boundaries in SPEC-03 through SPEC-09.
Decision: [ADR-0028](adr/ADR-0028-specialist-investigation-coordinator.md).
Trace: US-0032 / AC-0180–0191 / T-0180–0191.

## User outcome and scope

A developer asks a scoped question, watches a named specialist query recovered
context and read evidence, and retrieves cited findings and the task history.
The same host coordinator owns application actions and future transport adapters.
This is an actual bounded tool loop, not an edge-resolution task disguised as chat.

Ship domain-analyst@2 (Domain analyst) and evidence-auditor@2 (Evidence auditor).
Both are T3, have an InferredWeak ceiling, and share the same enforced tool set.
Their immutable definitions include role/prompt version, prompt fingerprint,
purpose, supported operations and limits. Persist the exact definition and
configured provider/model identity used by each task, plus separately observed
response identity when available. A model tag is not a weight-content attestation.
Do not advertise T0/T1/T2 agent reasoning or implemented architecture metrics.

Version 2 clarifies the host-mediated discovery loop: the first invocation has no
query pages or copied source because acquisition has not started. The specialist
should return a query action, inspect its results and request relevant original
evidence before concluding that support is insufficient. Empty initial input is
not an empty search result. The host still admits a valid insufficient-evidence
finish without inventing tool activity or retrying it; such a run does not satisfy
the controlled query/read/finish acceptance case. The original @1 definitions stay
byte-for-byte reproducible and supported for saved history and explicit old-version
requests. The catalog and new app requests select @2; changing a default never
reinterprets an existing task's definition or prompt fingerprint.

The initial scope is the app's recovered system, optionally restricted to a known
fact neighborhood. It is not a named multi-project/domain isolation implementation.
A task records its host context owner and exact normalized scope; source identities
remain the registered host identities. No caller root or filesystem path grants
access. Domain names must come from evidence or remain labeled interpretations.

Keep implemented-behavior claims, documented-intent claims, inferred
interpretations and proposed future designs distinguishable. Every model-generated
finding remains T3/InferredWeak irrespective of its claim kind or human response.
Missing design/domain/feature evidence means unknown within the searched scope,
not proof that no such design/domain/feature exists. Findings do not mutate the
recovered graph, edge-proposal history, accepted overlays, exports or curated state.

AC-0110 and MT-H4-01 remain unchanged, including their real MCP ingress requirement.
This application/coordinator increment does not pass H4 by itself. Full H2/H3,
MCP/ACP execution and delegation, H6 measurements and H7 pilot remain open.

## Contracts and execution boundaries

Add distinct versioned investigation request, action, input-ledger and result
contracts in agents. Preserve AgentTask, AgentProposal, PreparedAgentTask and
literal staging-v1/v2 identities. Never fabricate a Gap, source/target relation or
confirmed assertion to carry a question or answer.

A request includes a bounded client request nonce, conversation identity (or new
conversation), specialist definition ID, question, normalized scope, provider mode,
limit profile, optional expected graph revision, origin and optional parent/follow-up
identity. The context owner is the existing private state-store execution namespace,
recorded in host metadata and exposed only through an opaque host-derived context
identity; it is not a caller-supplied domain name. The webview cannot select another
private store or supply execution ownership. The first transport origin is app;
reserve typed transport extension points rather than pretending MCP is connected.

Start validates cheap client syntax, then checks the durable origin/nonce and
client-intent hash before mutable availability/capacity checks or allocating a new
conversation. An identical retry returns its original task even if a provider has
become unavailable or capacity is full. Only a new task resolves and freezes host
definitions/provider configuration and commits identity before expensive
graph/source work or model completion. Keep the client-intent hash separate from
the immutable host-resolved request hash. A context revision is
explicitly pending until preparation commits the frozen input. If the caller
supplied an expected revision, preparation must match it or fail. Otherwise the
request explicitly means the current recovered context at preparation time.
The UI shows the actual prepared revision and scope before findings are presented.

The coordinator exposes start, task/status, conversation/history, ordered events,
result, pending-consent, approve-step, cancel and historical-citation reads. These
are transport-independent host operations wrapped by Tauri. Jobs are the execution
lifecycle surface; investigations own questions, definitions, input/result identity,
ordered events, consent state and findings. A live map holds ephemeral execution
payloads and wakeups only; SQLite is the identity/lifecycle authority.

The same request nonce returns the same task for concurrent or repeated identical
starts, including a lost start response. Reusing it for different request material
fails. Client retry of a transport observation never creates a new task. A follow-up
is a separate explicitly requested task and basis; previous findings supplied as
history remain T3 and retain their original revision/coverage labels.

## Frozen scope and coherent acquisition

Copy one complete graph snapshot on a worker. Build ContextSnapshot once from that
owned graph. A restricted neighborhood derives a frozen scope projection and its
own query identity, while the ledger also retains the original full graph identity.
Tool queries use this frozen projection, not repeated calls to the live
query_context Tauri command. Anchors outside the authorized scope fail. Preserve
paging, explicit selected/returned counts and exact typed fact identities.

Maintain a cumulative ledger of at most 64 distinct selected facts. Before a new
query result or evidence selection can enter a model payload, obtain one coherent
read_source_selection_snapshot for the union of prior and newly selected keys.
Compare its complete graph to the original owned graph, and every prior fact digest
and association to the bound ledger. Acquire sorted source/retention guards outside
application/database mutexes, then authoritatively recheck the same bounded union
before supplying newly copied source. Prior associations cannot be silently rebound.
Unvisited unrelated associations do not invalidate the ledger. Global graph changes,
visited invalid selections and changed prior associations stop acquisition explicitly.

A query page is admitted as a whole or fails its bound; do not silently skip a large
fact in favor of a later fact. Record exact fact digests, present/absent source
associations, query identity and the returned scope/count/continuation information. Query payloads also expose
bounded evidence options with each original role/index and source reference so a
specialist can discover nested occurrences rather than guess them. The complete
context plus inventory envelope must fit the query page byte limit. Source receipt
metadata does not claim that retained bytes are currently available.
Whole-graph copy/comparison is not constant-cost and is separate from the query
response budget. A bounded coherent reader enforces the graph row/byte ceilings,
SQL type and raw-body byte preflight before loading bodies, bounded JSON depth/value
counts and exact bounded canonical serialization in one read transaction. Every
union recheck uses that same bounded reader, so a later oversized live graph cannot
allocate without bounds before its revision is compared. These ceilings limit
admitted structure, not an exact resident-memory promise. Work runs off the UI
thread; even narrowed task scopes pay the full bounded snapshot cost.

The read_evidence action names an already admitted typed fact, receipt range role
and occurrence index. It supplies no path, replacement offsets or arbitrary receipt.
For participating facts, validate the exact selected receipt and original inventory
occurrence and read the strict retained span. Reuse source ownership and capture
validation from SPEC-09, including nested v2 local-definition occurrences. Check
8-KiB original-span limits before reading; no synthetic excerpts. Known missing,
forgotten, corrupt or unsupported participating evidence fails without checkout
fallback. Valid association absence can use the existing bounded legacy reader,
with its explicit working_tree_unverified classification and original cited range.
Legacy reads cannot manufacture retained historical source.

Charge each attempted read and the full captured file length, including repeated
reads of one file. Commit each admitted input-ledger revision and its actual copied
receipt-reference index while acquisition/retention guards remain held; pending
inputs, including uncited source, must affect a concurrent retention preview.
Because selected receipt identities enter the model ledger even before a source
read, the ledger explicitly records registered source/repo/receipt references for
every selected present binding; the retention index includes these too.
Source and retention leases end before every model call or consent wait. Retained bytes may subsequently be forgotten. A finished model
response is admitted against the last owned input ledger, never against a fresh
source read; completed history survives later reingest/forgetting/cancellation.

## Actual bounded tool loop

A model returns exactly one strict JSON action per invocation:

- query_context: a validated bounded query over the frozen authorized scope;
- read_evidence: an admitted fact and original role/index;
- finish: typed findings, citations and limitations.

Reject unknown fields/actions, excess depth/count/string sizes, trailing data,
markdown wrappers and invented references. No execution of paths, commands, URLs,
repository instructions, arbitrary tool names or graph mutations. No delegation
in this contract. This host-interpreted protocol does not claim native provider
function calling or ACP support.

Persist the admitted action identity and safe event metadata before executing its
tool. Tool results are input data, not instructions or permission grants. Every
model step sees the question, role, explicit coverage/budget ledger, admitted
context and evidence, and any bounded prior conversation findings marked as T3.
Keep the full raw tool transcript and copied source transient. There is no hidden
repair/retry model call when JSON is invalid or a provider fails.

Every finding has a stable result-local identity, claim kind, bounded title and
statement, one or more admitted citation IDs, explicit limitations and host-stamped
Agentic/InferredWeak provenance. Citation IDs resolve to original ledger entries;
equal paths, overlapping spans or later facts cannot substitute for them. Reject
invented or unavailable-to-the-model citations. A finish with no supported findings
may explicitly report insufficient evidence; it cannot fabricate a complete answer.
Execution completion and knowledge completeness are independent fields.

Apply source-redaction/replay admission to model-authored prose against every
supplied source/context string, including uncited items and supplied prior findings.
Visit individual graph string values/keys and copied source items, not only the
outer serialized JSON. Raw source excerpts and supplied prior-finding text retain
the existing 48 non-whitespace scalar window plus complete-short-item rejection.
For the explicit parent-history envelope, inspect individual saved finding titles,
statements and limitations inside its JSON; a serialized wrapper must not weaken
short-item protection. Preserve the parent status and revision as metadata.
Graph metadata uses the 48-scalar window without complete-short-scalar rejection:
short identifiers/literals such as a symbol named a may appear in ordinary prose.
Otherwise one character could prohibit almost every English answer. Secret
redaction still applies to all output; this permits short graph metadata, not raw
source-excerpt copying, and is not semantic or encoded-text declassification.
Required typed
citation/reference fields instead undergo exact membership/shape validation: those
fields must reproduce admitted IDs and are not model prose. No prose field becomes
exempt merely by containing a reference. Validate before durable writes;
never archive unvalidated model responses or private thinking text. Persist accepted
findings, typed references, immutable hashes and bounded safe metadata. User question
and follow-up text has an explicit bounded/redacted history representation; an
original input fingerprint may bind identity without storing the original secret.

## Compact disclosure increment (#596)

This section is the implementation contract for AP-2a, US-0032 AC-0234–0236.
It amends the every-step full-ledger presentation and cumulative citation admission
above for new compact specialist definitions. It does not alter the acquisition
ledger, receipt retention accounting, existing specialist definitions or historical
result identities. Discovery (#592) and real-provider acceptance (#598) are separate.

### Acquisition, presentation and disclosure identities

The complete `InvestigationInputLedger` remains the host's acquisition authority,
with its existing v1 serialization and fingerprint. A new, independently versioned
projection removes repeated acquisition bookkeeping from model input. The initial
policy deduplicates identical graph fact and evidence-inventory representations by
typed fact identity, retaining their original provenance and all distinct conflicting
claims. Query selection/count/continuation metadata remains explicit. Exact acquired
source spans and bounded T3 parent findings remain distinct from graph metadata.
No same-name matching, semantic summarization or new source acquisition occurs here.

A host-selected working set records every omitted acquired representation and a
stable handle for explicit reacquisition. A handle supplies neither source content
nor citation eligibility. The initial default preserves acquired exact source and
unique graph facts; removing duplicate envelopes does not imply that a tool ran
again. A required individual representation that cannot fit fails explicitly; it is
never silently truncated. Whole model input and acquisition limits still apply.

The host validates raw selected representations against acquired identities, then
seals a metadata-only per-invocation disclosure against the firewall's actual
redacted payload. Bind both the original acquisition/source hashes and the actual
supplied representation hashes; a redacted representation is not the original
unredacted text. Include projection/policy version, ledger hash, graph/scope/view,
query and fact representation identities, supplied source citation/span identities,
parent-history representation identity, omissions and exact payload identity.
Do not put the manifest's own fingerprint inside the payload it fingerprints.
Manifest construction and checking happen before invocation or consent publication.
Unknown, replaced, conflicting or unaccounted representations fail closed.

Initially the view is recovered-only. Future curated inputs must carry all DR-4
fields through this same contract: view identity, curated revision, accepted-proposal
IDs, unchanged tier/confidence, review status, source support and freshness/conflict.
No private curated overlay or inferred method field is introduced by this increment.

### Admission, durability and compatibility

New result admission uses the invocation's disclosed citation set, not cumulative
host membership. Source support requires the actual admitted source representation
in that invocation; a graph representation, prior disclosure or opaque handle cannot
substitute. A live WorkingTreeUnverified span remains explicitly unverified even
when disclosed; history retains metadata and no source text. Source forgetting does
not change a saved result's historical meaning or permit reading current source.

Persist the bounded validated disclosure with the exact pending/invoked step before
dispatch, under the existing transaction and logical storage reservation. Consent,
invocation start and response admission compare the same immutable identity. A new
result version binds its finishing step and disclosure; historical validation loads
that saved invocation and acquisition ledger. Missing/mismatched manifests fail
rather than silently falling back to cumulative admission. Receipt-reference indexes
continue to cover all acquired evidence, including omitted and uncited support.

Old step and result bytes retain their original serialization and hash domains.
Old specialist definitions continue to select the legacy input path. New compact
definitions require the new disclosure/result shapes; absence cannot downgrade a
new invocation into legacy behavior. No backfilled manifests or reconstructed old
prompts are allowed. Raw payloads/source stay transient. Manifest storage failure
prevents dispatch; interruption, late finish and unknown outcomes retain the existing
no-replay and no-late-tool guarantees. The disclosure body is capped at 64 KiB; an invocation row may contain that body
plus 4 KiB of existing step metadata. The enclosing 256 KiB record, 2 MiB task and
256 MiB global caps remain unchanged. Capacity is checked transactionally before
dispatch, including a pending disclosure inside the task record.

### Explicit turn progress (@4; #618)

The next specialist version adds a separate model-input schema. Existing @1–@3
prompts, payload bytes and saved task identities remain immutable. Selection is
explicit; no saved task is migrated to a newer definition.

Derive progress from the durable ordered action/input basis and verify it against
the admitted ledger and exact query pages. Describe initial state, the last
completed query or read, newly discovered typed facts, exact continuation and
unchanged repeated results. A repeated representation can differ even when the
fact identity is unchanged; do not call that identical evidence. Reopening must
preserve the same basis. A follow-up retains its saved specialist version. If that version or the current composer selection is absent from the loaded catalog, show the unavailable version and require the caller to select an available replacement explicitly. Catalog refresh or recovery never selects a replacement; saved historical identity remains unchanged. Starting waits while the catalog loads. A follow-up starts its own progress; parent history does
not become its own tool execution. Invalid or ambiguous bindings fail before a
provider call rather than inventing an action order.

Expose cumulative acquired query-response bytes separately from compact model
payload bytes. Both the visible limit and host admission use the same 96 KiB
constant; every admitted duplicate page still consumes that acquisition budget.
This information helps the model choose its next action. It does not authorize
automatic pagination, forced evidence reads, retries, larger caps or synthetic
completion. All existing disclosure, source-citation and outcome gates remain.
T-0260 covers durable progress derivation and shared budget admission; a separate
real-provider run must still establish useful query/read/finish behavior.

### Evidence required for acceptance

Map T-0234–0236 to production-path tests for exact representation binding, host-only
and handle-only citation rejection, deterministic compaction and measured envelope
comparison. Include redaction, duplicate queries, changed source/receipt, omitted
support, conflicting claims, oversized objects, source injection, storage failure,
consent/start races, cancellation/restart and literal legacy identity fixtures.
Inspect saved disclosure identity through history without retaining raw source.

Compare old and compact envelopes on the same pinned acquisition/history inputs.
Record serialized bytes separately from provider-reported tokens, calls and latency;
unmeasured values remain unknown. Establish numeric efficiency claims only from
those observations. These checks do not pass AC-0191 or any unexecuted native,
retention or H4/H5 procedure; their receipts remain separate.

## Budgets and provider acquisition

Query page byte limits cover the complete response, including evidence inventories.
Return the largest fitting ordered prefix and its original-selection continuation
cursor; a fact whose complete inventory cannot fit alone fails explicitly and is
never skipped. Only returned facts join the cumulative ledger and receipt index.
Page sizing within one frozen query is deterministic host work, not a model retry.

Bounded transport version `bounded-json-completion@2` requests Ollama's JSON
response mode with optional thinking disabled (`think: false`), so short action
turns need not inherit a model's default extended reasoning trace. This is a
request setting, not proof that every model honors it; models that still emit
thinking remain subject to the complete response-body, generated-token and time
ceilings. No private trace is stored or used to fabricate an action. Anthropic
and legacy unbounded completion wire behavior remain unchanged. The new bounded
protocol identity participates in exact consent hashing and is recorded in each
new investigation's provider descriptor. Older records without that field retain
unknown transport identity; reading them never assigns the current version.

Initial hard ceiling profile (the host may narrow it, never silently enlarge it):

| Work | Ceiling |
|---|---:|
| Model invocations | 8 per task |
| Host tool actions | 8 per task |
| Distinct selected fact keys | 64 per task |
| Query page | 32 facts / 32 KiB serialized |
| Cumulative returned query data, including repeats | 96 KiB |
| Evidence requests | 64 per task |
| Copied evidence items | 12 per task |
| Original span / combined copied source | 8 KiB / 48 KiB |
| Captured full-file validation, including repeats | 128 MiB |
| Question | 2 KiB UTF-8 |
| Supplied prior finding history | 16 KiB |
| Logical completion input before redaction | 128 KiB |
| Serialized request JSON body / complete response body | 512 KiB / 256 KiB |
| Returned model action text | 32 KiB |
| Requested generated tokens | 2,048 per invocation / 16,384 reservations per task |
| Connection / entire request-response timeout | 10 s / 180 s |
| Cumulative active execution time | 600 s |
| One consent wait / total task wall time | 900 s / 3,600 s |
| Finished findings | 12, each with at most 8 citations |
| Serialized admitted result / input manifest | 64 KiB / 128 KiB |
| One durable event / task event count | 16 KiB / 96 |
| One stored row/body (not aggregate task) | 256 KiB |
| History/event page | 50 items / 256 KiB |
| One history summary | 4 KiB |
| Nonterminal tasks per state.db | 2 |
| Retained tasks / conversations per state.db | 128 / 128 |
| Logical reservation per admitted task / total coordinator capacity | 2 MiB / 256 MiB |
| Reserved terminal publication headroom within each task | 256 KiB |
| Frozen canonical graph / combined node and edge rows | 32 MiB / 50,000 |
| Graph JSON depth / cumulative JSON values | 32 / 1,000,000 |

Saved-history search (AC-0368) is a read-only filter over the complete retained
investigation summary set, not just rows already loaded by the UI. It matches
literal substrings in the saved question and specialist ID with ASCII-only case
folding; leading and trailing ASCII whitespace is trimmed, and the raw query is
bounded by the existing question byte limit before trimming.
Search results retain descending ordinal order and the existing 50-item,
4-KiB-summary and 256-KiB-page limits. A search cursor binds the normalized term
and the initial ordinal high-water mark using a fixed-size query digest;
nonmatching rows are scanned without skipping the first unreturned matching
row. Unfiltered v1 cursors retain their existing representation. Only summary
metadata is searchable; activity events and findings are separate records, not
chat messages. No legacy summary is rewritten or upgraded by searching.

**Explicit opt-in policy (ADR-0040, AC-0281).** The table above remains the
immutable default `investigation-v1`. The separately selected `investigation-v2`
contract uses request generation 5, envelope generation 2 and @8 specialists.
Its ceilings are 56 tool actions, 64 invocations, 56 obligations/evidence items,
64 KiB request/envelope/acquired source bytes, 512 journal events and 131,072
generated-token reservations. Other table limits remain unchanged. Registration
is gated on complete admission, execution, replay and capacity validation.
The UI freezes this explicit choice with the request across uncertain retries.

V2 prompt schema 7 carries each exact source target once, in the completion
contract. Its inspection hash and ordered progress bind to the host's private
admitted targets during firewall sealing. V1 prompt bytes remain unchanged.
The bounded transport resolves `max_spans` from the saved policy's evidence-item
ceiling; its default 12 is omitted from serialized limits to preserve historical
hashes, while a nondefault allowance participates in the consent hash. No profile
enlarges the 128 KiB serialized input ceiling. Independent maxima need not fit
together; overflowing combinations fail before provider dispatch, without
silently dropping source text. Policy approval does not authorize provider runs.

Bounds are independent. Enforce task/conversation counts and the aggregate 2-MiB
per-task reservation in the same IMMEDIATE start transaction. Queued, consent-waiting,
cancellation-pending and failed ownership observations continue to consume one of
the two nonterminal slots; an in-process semaphore alone is insufficient. A duplicate
start returns before capacity rejection. Do not evict history automatically. The
256-MiB cap applies to coordinator logical data/reservations, not all shared SQLite
pages, WAL bytes, captured source, graph storage or filesystem overhead. Ordinary
journal growth cannot spend the reserved terminal 256 KiB; reserve the actual input
manifest/result/event/metadata sizes before publication. A budget failure must
remain recordable. The 256-KiB row/body bound is not an aggregate task budget. A tool result that cannot fit
the next bounded input fails explicitly; no silent source omission or context
replacement. Every invocation begun consumes its invocation and generated-token
reservation even on timeout, cancellation, malformed response or provider failure.
Record provider-reported usage separately, with unavailable values remaining absent.
Input byte limits and requested output-token caps are not a verified monetary cap
or a claim of exact token accounting. Never relabel estimates as measured usage.

Active time includes acquisition, validation and calls; explicit human consent
waiting is separate and bounded. Waiting cannot reset any spent budget. Check
ownership, cancellation and remaining budgets before each new operation and after
bounded work. Apply the minimum of call cap and remaining active/wall deadline to
the actual HTTP request. Time limits bound awaited local work; they do not claim
remote inference or operating-system resolver work has been killed. Long existing
graph/OS work is cooperative; report timeout without claiming preemption.

Add a separate bounded completion/firewall API. Legacy completion methods, payloads
and preview hashes remain compatible; the new method must not default to the old
unbounded complete implementation. Bound input before redaction, request
serialization before growing its buffer, and actual HTTP bytes before JSON decode,
including chunked/no-Content-Length and oversized ignored provider fields. Body
counters exclude HTTP headers, framing and TLS overhead. Disable automatic content
decoding/compression and reject unsupported Content-Encoding. Disable redirects
and transport retries for bounded calls. Use explicit generation limits
and accept only supported complete stop conditions; truncation/refusal/unknown
states are not usable protocol actions. Provider errors are fixed and source-free.
Keep configured model, observed response model and request identity distinct.

Verify HTTPS support explicitly: the current reqwest configuration lacks a TLS
backend. Enable a supported TLS feature and retain certificate/hostname validation.
Use controlled HTTPS fixtures as well as HTTP fixtures. Reuse application-lifetime
clients; do not create/drop a blocking client runtime for each limited invocation.
Keep Ollama local/no-proxy restrictions. Do not silently change provider or model.

## Per-step consent and cancellation

Each bounded preview has a separate versioned hash domain binding provider/model
profile, exact redacted payload, action/step identity, protocol and declared limits.
Existing ConsentGrant can carry that hash; a legacy grant cannot authorize it.
Each tool result changes the transcript and therefore the next cloud action.
A consent wait stores only safe identity/hash/limit metadata durably; the exact
preview/input stays with the live worker. Approval must match task revision, step,
provider and payload, and is consumed atomically with invocation start and budget
reservation. Old, reused, changed-provider or changed-payload grants perform no call.
Declining a step does not switch to a local provider. Hiding the dialog is not consent.

Cancellation is a durable request separate from execution/result state. Acknowledge
it promptly and wake a consent wait. Jobs-originated cancellation uses this same
coordinator transition, rather than changing only the Job row. Check cancellation
before every new tool or invocation.
The bounded synchronous provider may finish an already issued request or time out;
the UI must state that its outcome is pending, not claim remote termination. Retain
the real execution lease while that work is outstanding. A complete valid finish
response is preserved against its original input before finalizing the job, even
when cancellation or job cleanup happened during the call. After cancellation,
never execute a returned tool action or begin another model call.

## Durable ownership, storage and restart

Use coordinator-owned versioned tables in the existing state.db with explicit WAL
and synchronous=FULL durability. JobStore provides one transaction boundary for
request deduplication, task lifecycle, ordered journal, result publication and
indexed receipt-reference rows. Insert the initial unclaimed Job and investigation
in the same IMMEDIATE transaction. Claiming execution likewise records the private
namespace/job/generation/owner and CASes the investigation into preparing in the
same transaction as the Job claim; there is no crash window between claiming a
Job and attaching its execution to the investigation. No cascading foreign key to
Jobs may erase history. Do not introduce a second database or rely on ATTACH for
cross-WAL crash atomicity.

Every transition checks task revision, expected phase and exact private execution
identity. Before network dispatch, atomically journal invocation start, consume the
matching approval and reserve budgets. Persist admitted response/action/result and
its receipt references together before job finalization. A failure to stage is not
a successful answer. Events have monotonic sequence IDs and closed kinds; progress
comes from actual events, never invented elapsed-time percentages or model reasoning.

Retain a private immutable job namespace/id/generation/owner reference independently
of deletable Jobs rows. Startup scans the coordinator's own pending/nonterminal
records, including cancellation-requested tasks. Generic Job recovery alone is
insufficient: it ignores cancelled rows and cleanup can remove their attempt rows.
Probe the exact existing execution lock before declaring owner death. A live owner
is not interrupted merely because its UI disconnected. A missing/substituted lock
or failed ownership observation is operational failure, not abandonment.

The frozen graph, raw tool payloads and pending exact preview are RAM-only. After
proved owner death, pre-dispatch/lost-input phases become interrupted; a
started invocation with no durable outcome becomes outcome_unknown. Do not rebuild
an old run or preview from current sources, replay a possibly executed call, or
turn a timestamp/connection loss into proof that execution stopped. Known completed
results and conversations remain queryable. Unknown outcome stays explicit; the
application offers no generic Retry/Resume for these jobs. Any later recovery or
fresh follow-up must be an explicit coordinator action with the prior uncertainty
preserved; it is never an automatic replay or a declaration of remote nonexecution.

History pages return bounded summaries, with detail and result fetched separately
by ID. Reserve envelope headroom before adding an item and use a stable cursor;
a valid maximum-sized detail must not become unreachable through a page budget.

Strict version-dispatched SQL decoding preflights type/length, depth/container
counts and references before allocation. Bound history and total storage admission;
capacity errors are explicit, never silent eviction. A staged-reference index must
be validated against immutable investigation content and published transactionally.
Retention previews include actual investigation receipt references in their counts
and fingerprint. Forgetting alters historical availability, never historical meaning.
No raw source is stored to make restart/resume convenient.

## Application and historical inspection

Add an Investigations workspace subview, a Workspace entry, and a palette route
without changing the eight existing rail shortcut assignments. Jobs link to the
investigation and use coordinator-supported actions instead of generic retry.
The saved-investigation list supports bounded search over all retained questions and specialist IDs, including older versions, without loading every page into the UI first. Search is ASCII-case-insensitive literal substring matching; clearing it restores the ordinary list. It does not search activity text or findings.

Show specialist/provider identity, question, scope/revision, real ordered events,
consumed limits, consent state and durable findings/history. Keep this orchestration
in a dedicated UI store with task ID/request-generation/revision guards. Fetch
missing event sequences after reconnect; duplicate/conflicting/late responses
cannot replace a newer selection or silently reorder history.

Reuse the exact egress payload renderer with investigation-specific decline and
waiting copy. Do not display Keep local as the consequence of denying a cloud
step. Reading or accepting a finding must not claim curated activation.

Historical citation actions take investigation/citation IDs only. Captured source
inspection verifies the saved receipt occurrence, original fact digest, text hash
and range under a tagged retained-source lease; it must not call the current-fact
source viewer or silently read a later checkout. Graph-only historical citations
show their retained metadata/digest and the limitation that raw old graph properties
were not archived. Unverified working-tree input is not retrospectively retained.
Unavailable/invalid/operational failure remain separate observations. Every new
component has a story, including complete App navigation and stale-response cases.

## Verification before delivery claims

Tests must exercise real captured TypeScript production receipts and source access,
not solely fabricate a valid DTO. A scripted provider queries a non-Gap scope, reads
a v2 initializer occurrence, then finishes with cited findings; delete the checkout
before acquisition and prove original bytes reached the actual authorized payload.
Interleave graph and equal-fact receipt changes, unavailable/forgotten source,
cancellation, storage failure, duplicate starts, reconnect and owner death at each
critical phase. A completed result survives cleanup/restart with its original basis.

Provider HTTP/HTTPS fixtures inspect full framed requests and enforce actual body
caps, chunked overflow, slow headers/body under one deadline, no redirects/retries,
TLS validation, complete stop conditions, exact generated-token caps and source-free
errors. Use barriers for races. Preserve existing legacy completion/consent tests.

Provisioning a reachable loopback runtime/model is an acceptance prerequisite;
do not describe the workflow as verified while it is unavailable. A lightweight
harness must call the real transport-independent coordinator, source reader and
bounded provider rather than copy their orchestration. Record that evidence as
coordinator/local-provider acceptance with separately CI-tested UI integration;
it is not a native-app or MCP end-to-end claim.

Run an actual controlled local-provider investigation with pinned input, question,
role/prompt/model identity, tools, output, runtime and usage/coverage record. Review
answer citations independently and keep the expected answers out of its inputs.
No source data is sent to a cloud provider without its exact app consent flow.
A fake provider or green unit suite does not prove this user workflow. Retain real
limits/failures; do not replace the Vendure oracle or claim T3 findings are complete
confirmed rules. Full H4/H5 manual procedures stay unexecuted until actually run.

Run focused local gates under the shared execution policy and complete exact-SHA
CI before ready status. Record the solution as built, validation and remaining
parent gates on #404 before closeout. Do not mark #256 or the market goal complete
from this increment alone.


### Explicit source-inspection requirements (#622; AC-0261–0262)

This next slice accepts caller-selected, known captured source targets only.
Each requirement binds an exact fact, original role/index, graph snapshot, fact
digest and retained source/receipt identity. The captured-source viewer's receipt
inventory ordinal is resolved by the host; it is not the protocol role index.
Start admission revalidates bindings and rejects stale or rebound selections.
Selecting or viewing a range does not count as an agent inspection.

Use a new request/input version and explicitly selected V5 specialists. Preserve
V1–V4 request, prompt, input and saved-result behavior, including default V3.
Requirements are immutable parts of caller intent, disclosure and result identity;
nonce replay and follow-ups preserve the existing no-replay boundaries. Follow-up
requirements must be explicitly supplied, never inferred from parent history.
Keep existing request and evidence budgets; do not expand them to fit a selection.
The request permits at most 12 targets and at most 8,192 serialized bytes total,
including the question and all target descriptors. These are simultaneous limits;
long paths or questions can make fewer than 12 targets fit. The UI reports the
request byte limit before submission rather than silently dropping requirements.

Host assessment distinguishes acquired, supplied to the finishing invocation, and
cited source. A requirement is met only by the exact matching source representation
actually supplied to that invocation. Acquired but omitted/redacted source, another
range, or parent history cannot satisfy it. Supplied but uncited evidence is not
claimed as support for a finding. Repeated reads do not inflate unique coverage.
Save valid partial results with unmet requirements; execution status remains
separate. A terminal V5 task without a finishing result reports not_assessed,
while a live task awaits assessment. Legacy untyped tasks also report not_assessed,
without acquiring typed requirements. No assessment certifies
entailment, relevance, complete knowledge or whole-task fulfillment. The host never
forces a tool, retries, invents requirements or repairs a failed model response.

Locations retain explicit byte units. Structured line/column locations, where
shown, are computed from the exact captured source version and bound to that
source identity; never derive a line from an opaque fact-ID suffix or the current
working tree. Free-text claims remain subject to independent quality review;
regex matching over arbitrary finding prose is not a semantic correctness gate.

The app offers an explicit require-source action from a known captured range and
removable human-readable requirements in the composer. Composer intent, including
an explicitly selected follow-up parent, survives source-selection navigation;
requirements are not inherited from that parent. Saved detail/history shows
per-target assessment independently of execution and knowledge completeness.
Discovery-selected promotion, generic open-ended task certification and automatic
migration of old controls are outside this first slice.


## Coverage-qualified investigations (@6; #588, AC-0239)

Implementation contract; delivery remains unproven until the host, persistence,
model input, disclosure and application tests below pass together. Explicit
`domain-analyst@6` and `evidence-auditor@6` use the frozen coverage API from
SPEC-01. V1–V5 definitions, serialized records and fingerprints remain unchanged;
the application's current default does not change.

Before the first provider call, preparation reads a bounded graph and coverage
inventories in one transaction and captures registration/plugin scope before and
after that read. A changed scope is qualified stale, never recorded. Requested
layer hints, when supplied, participate in qualification. Registered or recorded
repos with no graph facts remain represented. The scoped graph identity, complete
graph identity and qualified context identity have separate meanings.

Coverage remains per repository: a neighborhood-scoped investigation retains the
qualification for every repository in the prepared scope, including repositories
with no facts in that neighborhood. It is not per-neighborhood coverage.

The entire public qualification is mandatory in every model invocation, including
the initial invocation and the invocation that produces a finish. It states the
per-repo status, recorded counts, layer hints, compared checks and
`exhaustive: false`. It is not a count-only summary or a pointer to current metrics.
Budgets may reduce graph facts, but never remove qualification. If qualification
cannot fit or egress preparation changes it, fail before provider dispatch.

The host saves a separate durable proof containing the recorded identity basis
used by the shared coverage hash algorithm. This permits verification after
restart without adding host-only basis fields to the public qualification wire.
The proof establishes internal consistency of the preparation-time record; it
does not establish current registration truth or exhaustive extractor support.
The original inventory bodies are retained immutably under exact hashes before
provider activity, within an explicit bounded investigation archive. Later graph
publication, removal or clearing cannot replace the saved qualification or detail. Preparation derives its
raw inventory-read ceiling from remaining task and global storage capacity,
reserving proof/envelope space before the graph store copies inventory bodies.
A count-only serialization check then rejects escaping or metadata expansion
that would exceed archive capacity. Publication rechecks capacity transactionally;
concurrent storage growth can still refuse the write. Capacity refusal is an
explicit limit failure before model dispatch, never an unrecorded fallback.

Inventory details are progressively disclosed by an explicit
`read_coverage_inventory` operation naming the frozen context ID, repo, exact
inventory hash and response byte budget. Only recorded entries are readable;
stale/unrecorded entries, wrong hashes, wrong contexts and over-budget bodies
fail explicitly. There is no latest lookup or path-based read. Detail responses
consume acquisition, prompt and durable storage budgets and appear in the actual
finishing disclosure. Paths and reason strings are untrusted source-derived data
subject to existing redaction and egress policy. Inventory details never become
source citations or discharge source inspection requirements. The aggregate
qualification alone does not supply named stopping reasons.

An @6 request may omit inspection targets for a coverage-only investigation. If
present, targets must be the existing normalized nonempty set of exact targets;
null and an explicitly empty list are invalid. No target is silently selected by
the host. @5 continues to require targets. Optional source obligations retain
their separate acquisition/supply/citation assessment and do not attest coverage
or semantic correctness.

The saved ledger, finishing disclosure and result bind the same qualification,
context identity and representation. History renders that saved preparation-time
basis, never a newly queried inventory or a current freshness claim. A restart
must verify those bindings and retained inventory hashes before exposing detail. The public
`InvestigationDetail.coverage_context` contains only `{ context_id, coverage }`,
where `coverage` is the frozen public qualification. It is absent before
preparation and on legacy tasks, required on prepared @6 tasks, and validated
against the retained proof when reading history. Host-only basis and inventory
bodies are not included in this history DTO.

Required verification includes: unchanged legacy serialization/fingerprints;
mandatory qualification on initial and every later mocked provider call;
qualification-only oversize and egress mutation refusal; wrong-context query and
inventory refusal; inventory A retained after publication of B with identical
graph facts; restart after current inventory removal; independent hash/basis
mutation rejection; coverage-only and explicit-target runs; and UI rendering of
saved stale/unrecorded/limited states without an exhaustive or live-freshness
claim. These extend T-0239; static shared-read tests alone do not prove the
investigation path.


### Staged completion-obligation primitives (#645, AC-0264)

The generation-4 request supplies the immutable intent for the three-slice #633
contract. The first slice introduced these primitives with V7 unavailable;
AC-0266 below connects their production start, prompt and result paths.
A single `investigation-completion-obligations@1` envelope contains source or
inventory entries, each with a kind-specific content ID. Source identity retains
all captured-target fields and the existing fact/role/index conflict rule;
inventory identity includes exact context, repo and inventory hash. Exact duplicate
entries normalize in kind/ID order; conflicting identities fail closed.

Raw lists admit 1–12 entries and the entire request admits at most 8 KiB before
partition clones; twelve targets are a ceiling, never a capacity guarantee.
Legacy inspection requirements and the new envelope cannot both be supplied.
Absent legacy requirements stay absent and present-null is rejected. Envelope,
source, inventory and generation-4 request identities use respectively
`cartograph:completion-obligations:v1`, `cartograph:source-obligation:v1`,
`cartograph:inventory-obligation:v1`, and
`cartograph:investigation-client-intent:v4`, each with a trailing NUL domain
separator. Existing client-intent v1/v2/v3 and inspection-requirements-v1 remain
unchanged. These pure identities attest caller intent; authoritative retained
inventory/source resolution is required before admitting a new task.


### Staged finish admission and refusal journal (#648, AC-0265)

The private completion contract distinguishes `supported` from
`insufficient_evidence`. Existing action, citation, replay, metadata and disclosure
security checks run before obligation classification. A supported candidate needs
all source and inventory obligations in the actual finishing disclosure, using
exact source assessment and validated retained-inventory read manifests. Acquiring
bytes, graph metadata, parent history and omitted, redacted or changed bodies do
not establish exact supply. An insufficient candidate has no findings, retains
required limitations, and needs a completed durable read attempt for every
obligation. Neither candidate publishes a legacy-shaped V7 result.

Attempts bind immutable task/request intent, obligation, actual read action and
invocation, and saved ledger basis. Unavailable outcomes are a closed set: pinned
target absent, range/index out of bounds, and retained inventory exceeding caller
`max_bytes`. `FirewallRefused` remains a reserved decoding variant; the host
rejects its persistence until an actual per-read denial operation exists.
Redaction of acquired evidence remains an acquired attempt. Operational,
ownership, storage and provider
failures retain terminal semantics; model claims and started reads are not
completed attempts.

Recoverable refusal feedback contains only obligation IDs, kind and fixed reason
codes in normalized order, bounded to 2 KiB for the complete serialization.
Supported refusal precedence is no attempt, unavailable acquisition, absent
finishing representation, redaction, then unequal body hash. Insufficient finish
refuses only unattempted obligations. Invalid security evidence remains an ordinary
error. Envelope, attempts, finish, feedback and refusals have separate identities;
staged sealing protects completion metadata without changing V1–V6 inputs.

The optional private JobStore journal binds staged intent to an existing task's
context/source projection and real invocation records. Its append helper joins a
caller-owned transaction: refusal metadata, response usage and bounded feedback
commit together or all roll back. Ordered durable records reconstruct the journal
on a fresh connection, with bounded decoding and identity/basis validation. The
journal retains hashes and derived assessment, not rejected finding prose; this
read-back does not claim raw response replay or complete V7 task recovery.

The first refusal returns `Continue`; the second returns `TerminalRequired`, and
no third continuation is admissible. This slice does not commit a second-refusal
Ready task. AC-0266 connects this decision to atomic second-refusal publication
and host `Failed/obligations_unmet` with no result, preserves charged usage across
intervening reads, and validates cancellation, ownership loss, reopen, history
and UI projections. The pure primitives themselves dispatch no provider calls.

### V7 production completion integration (#650, AC-0266)

The final slice connects the single generation-4 request authority to input6,
disclosure4 and result5. The IPC reads the generation before decoding exactly one
closed request arm. A source obligation comes from the retained receipt resolver;
an inventory obligation is selectable only for the exact Recorded context/repo/
inventory identity. Start revalidates the selection before durable admission,
while a committed nonce still recovers its original task before mutable state
checks. No selection or envelope is inferred from the question.

StoredTask2 retains the normalized completion intent and its original request
hash. Actual completed reads and refusals supply ordered, hash-bound observations;
a planned read never counts as acquired. Redaction of an acquired representation
does not turn that read into a firewall refusal: it remains acquired, but cannot
support the finishing claim. Whole-input firewall failure terminates execution
without inventing a per-read observation. Legacy StoredTask1 rows are read without
rewriting their bytes or interpreting them as completion tasks.

A wrapped supported finish requires every obligation's exact disclosed
representation. An empty insufficient-evidence finish requires a real completed
attempt for every obligation. Security-invalid output fails before recoverable
classification. The first unmet finish records the actual response, usage,
refusal and bounded feedback atomically. The second records a host terminal
obligations-unmet outcome in the same transaction, without a result or third
continuation. Saved history distinguishes finishing-disclosure assessments from
terminal observations and not-assessed state; cancellation and unknown invocation
outcomes retain their independent execution status.

The UI renders generation-4 summary/detail and generation-5 results as distinct
contracts, rejects mixed-family responses, and clears explicit obligation
selection for follow-ups and scope changes. Same-nonce retries retain the exact
original request. This integration does not establish real-provider acceptance;
the frozen V6 control remains immutable. Explicit V7 catalog entries and direct starts share the same availability
gate, activated with the complete production path in this slice. Defaults stay
unchanged.

## Saved finding follow-up review (#730; US-0042)

**Specified, pending implementation and verification.** AC-0293–0296 / T-0293–0296
cover this first W8b dependency. It extends SPEC-03 review principles without
completing W8b, H3/H4/H5 or #598. Existing investigation request/result/prompt/input,
staging, history and citation encodings remain unchanged. Every finding retains
its original Agentic/InferredWeak tier and historical limitations.

### Exact subject and bounded current state

Define a host-only typed FindingReviewBasisV1 with fixed serialization order:
`schema_version:1`, execution namespace, investigation ID, saved intent hash,
exact saved specialist, configured provider, scope and limits, result ID, exact
immutable result-body hash and result-local finding ID. The subject is
`investigation-finding-review-v1:` followed by lowercase BLAKE3 of the domain
`cartograph:investigation-finding-review:v1` plus an actual zero byte and bounded
JSON bytes of that typed basis. Fix serialization with an independent literal
fixture. Bind all result fields, including citations, provenance, coverage and
observed finishing identity through the whole-result hash. A finding ordinal alone
is never durable identity; changing any finding in the result changes the subject.

Resolve the saved execution task and family-aware validated result in one SQLite
transaction. Retain hash verification, ledger/invocation/completion/disclosure and
citation-binding qualification; require exactly one matching finding. Use saved
specialist/provider/limits rather than today's catalog or runtime configuration.
The configured provider binds requested model/endpoint/protocol, not executed
weights. The result's reported response model is separately bound through its
bytes; absent means unknown and a reported alias is not weight attestation. Display
“Requested model (saved configuration)” and “Reported response model” or “Unknown”.
Do not include mutable task revision, usage or live source availability in identity.

The public subject contains only `investigation_id`, `result_id`, `finding_id`,
`subject_id`. Caller supplies no basis, prose, citations, provider, namespace or
source paths. Persist one row per `(task_id,result_id,finding_id)` containing unique
subject ID, immutable result-body hash, positive signed-SQLite-compatible review
revision, closed decision `accepted_for_follow_up|rejected_for_follow_up` and UTC
review time. SQL guards prohibit delete, identity/hash replacement and updates
other than revision+1. IDs retain existing 128-byte shape bounds, body hashes are
64 lowercase hex characters, timestamps at most 32 bytes; preflight SQL types and
UTF-8 lengths before allocation. No freeform notes, history stream or auto-pruning.

### Explicit versioned storage migration

Use the existing state.db/JobStore connection, without Jobs cascading foreign keys.
At store open, migrate investigation_meta version 1→2 in one IMMEDIATE transaction.
Validate the complete supported version-1 inventory and execution namespace before
changes, including the established original-to-coverage upgrade path and any
supported independently versioned optional extensions. Unknown versions, malformed
metadata or unexpected review objects fail closed; do not adopt/repair partial
layouts with IF NOT EXISTS.

The existing metadata DDL checks version=1, so updating its value alone is invalid.
Rebuild only investigation_meta with the exact corresponding version-2 DDL/check,
retaining exactly singleton `(1,2,original_namespace)`, then create the review table
and both guards. Validate the exact complete version-2 inventory and metadata before
commit. Do not rewrite old archive bodies/hashes or create pending review rows.
Fresh stores use that exact version-2 layout. Post-open helpers admit only version
2; retain version-1 validation solely for migration, not a second readable layout.
Existing read-reservation/admission extensions, where present on the implementation
base, keep their own validators and bytes; this slice adds no optional namespace.
Reopen is idempotent. Faults after metadata rebuild, after review DDL or before
commit restore the entire old schema/version/bytes. Older binaries fail closed;
no downgrade compatibility is promised. No writable_schema manipulation.

### Metadata-only commands and transaction boundary

Add off-thread `get_investigation_finding_reviews(investigationId,expectedResultId)`
and `review_investigation_finding(requestJson)`; keep investigation_result and its
response unchanged. Getter returns schema version, investigation/result IDs and
one review observation per finding in original order: subject, revision, decision,
reviewed_at. Missing persisted rows derive revision 0 and null decision/time;
no-findings results return empty reviews. Internally validate original saved result
and every task review row; orphan/malformed rows must not disappear through filters.
Missing task/result/finding is not_found; a different expected result or valid but
different subject is subject_changed; corrupt stored bindings are invalid_record.
A review failure cannot make the separate valid historical result unreadable.

Mutation accepts only schema_version 1, public subject, expected_revision and the
closed decision. Reuse bounded raw-action JSON admission before typed allocation:
reject duplicate/trailing/unknown fields, excess depth, malformed IDs and signed
integer overflow. Begin IMMEDIATE, validate namespace/exact schema and saved basis,
recompute subject, resolve current review and compare expected review revision.
Mismatch returns review_conflict with current winner metadata. Insert revision 1
or identity-matched UPDATE WHERE revision=expected, incrementing once; never upsert
or compare task lifecycle revision. No worker execution lease is required for this
review of immutable persisted content.

Bound only metadata responses with existing MAX_RECORD_BYTES; use existing
MAX_ACTION_BYTES for raw mutation ingress. Do not wrap/truncate the result or impose
a smaller result ceiling. Count derives from the actual admitted result, not a new
policy number. The existing MAX_RESULT_BYTES result contract remains authoritative.
Charge review rows, scalar UTF-8 bytes and index allowances with checked arithmetic
into existing per-task/global accounting (existing 256-byte row allowance); exceeding
capacity rolls back. Terminal tasks receive no guarantee of unused reserved space.
Bound the coherent response before commit. After commit emit
`investigation-review://changed` with investigation_id, result_id, subject_id and
review_revision; failed event delivery does not undo committed state. Lost response
requires a durable read, never automatic write replay. Even a repeated identical
decision with a stale revision conflicts. Getter and mutation return metadata only.

### UI, historical evidence and side effects

Join the unchanged result read with a separate exact-result-ID review read. Publish
valid result independently of review success. Verify review membership/count/
uniqueness and monotone revisions; separate selection and review-request generations
prevent late responses replacing another task/result or newer review. Mutation
responses update review metadata only, never the result. Failed reads retain the
last known decision with explicit unavailable/stale observation, not invented pending.

Use “Use for follow-up” / “Do not use for follow-up” controls and “Pending review”,
“Accepted for follow-up”, “Rejected for follow-up” statuses. Keep the T3 badge and
visible explanation that review changes neither graph/context nor evidence validity.
Show revision/time, disable that finding's actions while pending and never optimistic
update. On conflict show the winner and require a fresh deliberate action. Refresh
through existing historical-task navigation, explicit Refresh, app focus and review
invalidation with race guards. No new rail/global review list or no-findings controls.

Corrupt saved result/ledger/citation bindings disable review. Structurally valid
historical citations whose retained bytes are forgotten, unavailable, corrupt or
operationally unreadable preserve their historical decision and existing distinct
inspection observations. No current graph/checkout replacement or source lock/read
belongs in review. The existing historical citation inspector remains the sole
retained-byte access path. Live graph/catalog/provider/source changes do not rewrite
historical review or imply current evidence freshness.

Getter/mutation make zero provider and current-source acquisition calls; do not
alter results, ledger/refs, recovered facts/Gap, proposals, context/export, job status,
task lifecycle revision, model/tool counters or worker events. Opening/citing/finishing
a result does not itself record review. Later evidence/execution consumers must
revalidate independently. Tests must use real file-backed production worker/store
fixtures, separate competing connections, migration fault points and deferred UI
responses; literal DTO fixtures alone are insufficient. Existing golden bytes and
prompt fingerprints remain unchanged. T-0293–0296 currently map only to UNEXECUTED
procedures; replace/supplement mappings with real tests as implementation lands.

## Durable dispatch chat records (#1115; AC-0375)

A newly admitted task has one durable, separately identified dispatch chat. Its
first immutable message contains only the question already redacted and saved
in the task record. Identical nonce replay resolves the existing chat; it never
creates another chat or user message. Conflicting or rolled-back admission
creates no chat rows. This is persisted archival identity, not assistant prose,
a general reply surface, or model memory.

A validated result adds one typed assistant result-card reference, atomically
with the result in both ordinary `ActionAdmitted` and next-prompt finish
transactions. The reference binds the exact task and immutable result ID/hash;
it does not duplicate the result body. A test-only bounded validator checks it
against the saved task and result, including existing result/citation
validation. A future app read surface must use those same saved identities,
never current catalog defaults; this slice adds no public chat reader. No success card is recorded for failed,
interrupted, refused, or outcome-unknown work. Historical tasks predating the
chat schema remain readable and are not backfilled from questions, events or
results.

The schema migration is transactional and preserves existing task/result
bytes. Chat/message rows are immutable, retained when a chat is closed or
finished jobs are cleared, and charged within the unchanged task/global
capacity ceilings and terminal reserve. Persisted chats do not become model
input automatically. Reply streaming, generated assistant text, UI/IPC
presentation, chat search, deletion/forget policy and memory admission remain
separate work.

### Spec Writer final message (#1179, AC-0404)
`spec-writer@2` retains Standard request/disclosure4 and investigation-v1 limits.
Its closed finish envelope2 and result8 add an optional, omitted-when-absent
`narrative` containing authored `text` and local `finding_ids`. Supported finishes
require it; insufficient evidence forbids it. References resolve only to admitted
proposed-design findings and their citations; membership does not prove entailment.
The body reuses the existing finding-statement bound (2048 UTF-8 bytes) and shares
existing joined prose/privacy and encoded capacity limits. Result identity
covers it, and the existing atomic chat result reference remains its only publication
owner. Historical roles keep their exact wire/prompt/definition contracts. This adds
no streaming, new provider call, source authority, or full-conversation model memory.

### Obligation acquisition-choice foundation (#1383, AC-0472; related #1382)

A pure, bounded helper derives deterministic acquisition candidates from a validated
completion contract, current admitted ledger and exact ordered completed-attempt
evidence. Unadmitted Node targets offer exact-node discovery; matching frozen
bindings offer the pinned fact/role/index read, and inventory obligations offer
their exact qualified inventory read. An already recorded exact discovery with no
admissible binding offers no repeated discovery and remains unattempted. Completed
unavailable/refused reads retain their actual outcome; a refused finish alone
never creates a read attempt or satisfies an obligation. Candidate absence is not
satisfaction. Choices are a stable byte-bounded prefix under the existing action/envelope
ceilings, with deferred and blocked obligations explicit. This foundation performs
no dispatch, source access, family registration or runtime exposure. Authentication
of host-journal completion remains the caller's responsibility. Model projection,
schema/host membership, saved-call sealing and seven-target worker integration
remain required follow-up work before claiming the runtime feature or #598 success.

### Reserved acquisition admission stamp (#1387, AC-0473)

Admission schema1 reserves an optional `acquisition` stamp containing a choices
hash and an optional semantic-feedback hash. Absent stamps preserve exact
historical serialization; present stamps validate hash shape but are rejected
by the shared admission validator because no currently registered family supports
them. This is a compatibility prerequisite for #1385/#1382, not runtime choices,
a new specialist, a dispatch permission or a change to numerical limits.

A future unknown specialist (including planned @18) remains fail-closed in older
binaries: its per-ID task read fails, as can a history page containing it and the
full receipt-reconciliation audit. SQLite schema validation alone does not detect
this incompatibility. Future opt-in validation must use disposable profiles; no
task is silently skipped and no old saved bytes are rewritten here.

### Gated analyst input/schema (#1390, AC-0480)

Immutable `domain-analyst@18`, based on @17, defines the final caller-obligation
choice contract, including reserved null semantic feedback. Typed candidate
authority supplies compact input choices, finite acquisition schema and exact
acquisition membership; blocked/deferred state never opens broad exploration.
FinishOnly remains finish-only. No acquisition stamp is admitted in this slice.
Dispatch is explicitly disabled before source/provider preparation and at the
store boundary before a transaction. The reason is shared, and historical
nonce-recovery ordering, @15/@17 identities, catalogs and limits stay unchanged.
Host authentication, semantic journal, sizing/sealing, worker and UI activation
remain separate prerequisites; pure input tests establish no model efficacy.

### Saved-call acquisition stamp shape (#1394, AC-0481)

The journal-independent saved-call core obtains the specialist from validated
saved task context. Only @18 requires a present, well-shaped acquisition stamp;
historical families require its absence. Generic validation and sealing remain
closed to every present stamp, and @18 dispatch remains disabled. This checks
role/shape and existing saved witnesses, not candidate identity or authenticated
feedback. Other durable readers and full @18 reopen are not enabled here.

### Saved acquisition authority (#1398, AC-0483)

Existing saved-call readers rederive @18 choices from the exact invocation's
prior authenticated attempts, metadata and retained ledger revisions. Revision
keys, scope/graph and coverage stay bound; repeated revisions share one transient
ledger owner. The saved canonical choices hash must match. Non-null semantic
feedback hashes remain unsupported here; activation must authenticate them before
support is enabled. Unqualified invocation/metadata readers remain closed to @18;
only the private replay bootstrap admits shape pending exact choices validation.
Generic validation, dispatch and sealing stay closed; no runtime workflow is enabled.

### Gated action-refusal journal (#1397, AC-0482)

The existing authenticated admission writer can atomically record an @18 Choice
acquisition absent from its exact saved candidates, with response and paired
ActionRefused event. All entries consume existing sequence/call bounds; semantic
entries consume no capacity-refusal, tool, query, evidence or attempt budget.
Capacity/semantic feedback project independently and clear only after accepted
publication. Canonical feedback hashes bind the writer's current projection;
feedbackSome stamps remain rejected by every reader until activation implements
prefix authentication. Dispatch/sealing/UI stay gated; full runtime acquisition
and seven-source/inventory/finish/reopen acceptance remain mandatory follow-up.

### Gated real @18 sealing and feedback prefix (#1403, AC-0484)

Actual current input/sealing and pre-call/consent readers bind typed choices and
canonical semantic feedback to the exact authenticated journal prefix. The raw
history/timeline bootstrap remains choices-only; public saved readers validate
feedback after that replay, without calling the journal from choices derivation.
FeedbackSome is supported only together with this prefix check. Old families
remain None-only with identical bytes. Public/store starts and saved @18 worker
execution remain disabled before provider dispatch. Prospective acquisitions,
worker membership, UI and seven-target execution are the activation follow-up.

### Gated prospective metadata choices (#1408, AC-0485)

The @18 query/inventory continuation derives candidate choices from authenticated
saved attempts plus the exact projected attempt suffix and candidate ledger.
The actual candidate Input attaches this authority before certification/sealing;
the publication transaction independently rederives and checks its choices stamp.
Accepted metadata publication clears semantic feedback. Stale choices refuse
before retention callbacks or durable effects, and reopened current choices agree.
Existing logical owner bounds include transient history/ledgers and retained
choices; no RSS guarantee is claimed. Source continuation is separately guarded
by AC-0487; membership, repair, UI and all dispatch/worker activation remain
follow-ups.

### Gated source-choice certificate (#1415, AC-0486)

The unread-source serializer certificate covers post-source @18 choices in both
call modes, including exposed deferred candidates and the final-attempt fallback
schema. The same private candidate builder/selectors serve actual choices and
sizing-only serialization. A bounded, unencoded pending mask preserves knowledge
lost by prefix truncation; existing choices JSON and hashes stay identical.
Sizing hashes/Values never become Input, attempt evidence or publication authority.
Acquired/unavailable projections retain existing protected-redaction and wire
checks. This certificate grants no publication or activation authority; source
snapshot/settlement attachment is separately guarded by AC-0487, and every public
dispatch/worker gate remains closed.

### Gated source prospective attachment (#1423, AC-0487)

The source continuation snapshot derives @18 choices from the exact candidate
ledger and materialized outcome records, and attaches them before preparation
and sealing. Source settlement independently derives that authority again in its
owner transaction, comparing the sealed choices and absent action feedback before
metadata-hash acceptance. Acquired and unavailable outcomes retain their existing
read, citation, ledger and accounting guards; every matching obligation gets its
actual outcome record. Reservation sizing borrows the actual same-transaction
authenticated invocation core; it cannot replay future, unpublished reservation
events as saved authority. Persisted-state readers remain strict. The unread-source
certificate remains mandatory. Public
start, store-start and worker/resume gates remain closed; this private admission
slice does not activate @18 or establish model-driven investigation acceptance.

### Gated acquisition membership (#1427, AC-0488)

Before metadata/source preparation, the shared response coordinator checks typed
Choice-mode reads against the exact current Input choices. A valid unoffered
@18 action is published once through the authenticated action-refusal writer;
only its existing invocation is charged, with no acquisition or capacity debit.
Cancellation, stale revision, invalid decode and FinishOnly retain their existing
boundaries. Reopened feedback is bound to that call; no new model attempt is
started by this slice. All initial/repair-loop, UI and activation work remains
separate, and public/store/worker gates remain closed.

### Pure acquisition finish consistency (#1431 prerequisite, AC-0491)

A borrowed agents witness binds exact task, invocation, ledger, full disclosure
and policy to typed choices and feedback. It proves consistency only: callers
can supply their own derivation inputs, and host authentication of saved witnesses
remains separate. The existing assessment, citation, result and fingerprint
implementations share that witness without changing generic APIs or byte domains.
Unbound validation still rejects acquisition stamps. This agents prerequisite
neither authenticates SQLite nor completes transactional finish/reopen (AC-0490),
initial-authority acceptance (AC-0489), or any activation gate.

### Gated authenticated accepted finish (#1431, AC-0490)

A private host basis selects the last named invocation in one strict journal
replay and rederives exact choices and prior feedback. It binds the saved intent,
task, call, ledger and complete disclosure before the pure consistency API is
used. Accepted result creation, finishing hashes and durable reread share this
authority within the existing transaction. Generic/history behavior and ceilings
remain unchanged. Early-Finish refusal persistence is not adopted here, nor are
initial worker authority, public/store/resume or UI activation enabled.

### Gated historical Finish refusals (#1446, AC-0492)

A distinct historical refusal binding reads the actual saved refusal sequence
and authenticates its immutable invocation, ledger, choices and strict prior
feedback cutoff. It does not require the task's later current call or ledger.
Accepted-result binding stays latest-only, with no skip-current switch. Existing
finish decisions, refusal record fingerprints/assessment, two-refusal cap, usage,
rollback and reread share their strict validators. Generic stamps remain rejected
and all public/store/worker/UI activation gates and limits stay unchanged.
