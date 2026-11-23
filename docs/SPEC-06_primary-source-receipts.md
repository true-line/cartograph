# SPEC-06 — Producer-bound primary source inspection

Status: implementation in progress, #395 under #385. Decision: [ADR-0024](adr/ADR-0024-primary-source-receipts.md).
SPEC-00 integrity, SPEC-03 immutable staging, SPEC-04 capture and SPEC-05 registration remain binding.

## Meaning and participation

The app retains the exact selected TypeScript/JavaScript bytes consumed by the
production parser and supplies captured inspection for direct guarded-exit rule
observations, their lexical owner nodes and lexical GOVERNS edges, and every other
directly emitted callable declaration (#691), plus top-level variables, first-level
config-object properties, method decorators and oversized-callable headers (#693,
ADR-0039). A declaration that owns no rule gets the
same provenance-only inventory as an owner, so its exact span is citable. A receipt means
`primary_source_only` with `input_closure_not_established`. It never establishes a
complete business rule, execution predicate, caller interpretation, configuration
closure, current revision freshness or curated-context eligibility.

Only the adapter invocation consuming an immutable `CapturedFile` can produce a
participating receipt. The factory calls the actual parser on that buffer; there
is no public operation to attest an unrelated earlier extraction. Direct lexical
eligibility belongs to that producer. Eval/synthetic buffers, placeholders,
directory resolution, config/event stitching, plugins, other adapters and later
enrichments are not admitted by matching their apparent evidence spans. Preserve
all existing rule interpretation gaps and producing tier/confidence.

## Receipt and current association

A version-one immutable metadata-only receipt binds:

- the registered source ID and exact repository key;
- the complete CaptureFileRef (capture, path, raw digest and length);
- the producer contract version and selected TS/TSX grammar/package version;
- a typed node identity or directed `(source, label, destination)` edge identity;
- a canonical digest of the entire emitted node/edge, including all properties
  and provenance; use a versioned domain prefix and recursively sorted JSON object
  keys, preserving array order and fact kind;
- every original-file citation, including nested rule sources, as a fixed ordered
  range inventory with role/index and the original EvidenceRef;
- the primary-source and incomplete-input-closure scope markers above.

The receipt ID hashes this complete canonical content, excluding only its own ID.
It is not the semantic `prov.content_hash`. Receipt metadata stays outside fact
properties to avoid circular identity. Receipts contain no original source text.
Bounds cover serialized receipt bytes, range count, identities and stored totals;
oversized/invalid facts remain explicitly outside participating inspection, never
receive truncated or adjusted source ranges. Capture text reads remain strict UTF-8.

The graph maintains private node/edge current-association tables, separate from
canonical recovered facts and exports. Each association includes the exact receipt
ID, full fact digest and repository key. Publish repository reconciliation and its
current associations in one SQLite IMMEDIATE transaction against an owned expected
graph snapshot. Clear previous associations for that repository even when the new
facts are unchanged or no longer participate. Validate the final emitted digest
before accepting an association. Any ordinary graph mutation invalidates affected
associations; deletion/clear removes current associations without deleting history.

An equal-length trailing-comment edit can leave complete facts and the graph
snapshot hash unchanged while changing their captures. Therefore neither graph
snapshot identity, fact digest nor EvidenceRef selects a current receipt. A source
request must preserve the exact expected receipt ID. Current binding reads copy
the fact and association from one database revision and reject mismatch. A later
mutation cannot turn an old returned receipt into a different historical source;
an expected-current request after publication must detect the changed receipt.

Persist source captures and immutable receipts before graph publication. A failed
publication may leave unused retained metadata/bytes, but may not publish a
binding as readable if persistence failed. Separate stores do not claim a global
transaction; corruption/deletion always returns unavailable, never a substitute.

## Production parsing

The common TypeScript branch used by direct ingest, supported retry, add-repo and
each manifest member acquires selected working-tree bytes using its registration.
Clone HEAD is only a separate revision label; parsed checkout bytes are not claimed
to be Git blobs. Git-object production ingestion remains outside this slice.

Enumerate supported extensions with exact UTF-8 paths, bounded visited entries,
file count and depth, without following symlinks. Preserve intentional vendor,
hidden-directory, declaration-file and generated-output exclusions. Skip symlinks
unread and name them in coverage (#1229); reject unsupported paths explicitly; do
not silently widen or narrow the selection. The
rooted capture reader independently confines acquisition. A changing external
filesystem can change the acquired byte set; the parser consumes exactly the set
retained, not a claimed atomic filesystem snapshot.

The participating branch bypasses the ordinary TS parse cache. *Amended by
ADR-0087 (AC-1517):* it reuses only its own attested parse of a file whose
retained bytes, path, source, repository, registry set and producer contract
are unchanged, and mints that file's receipts afresh over the current capture.
Existing ordinary adapter
entry points remain available for preflight/test callers without claiming retained
evidence. Directory enrichment still runs after captured per-file parsing and
remains outside input closure; before graph publication keep only receipts whose
complete final fact still matches. Other language caches are unchanged. Capturing
or persisting fails the requested recovery explicitly, with no live-read fallback.

## Host storage and retention

Use private local application storage for captures, receipts and persistent
per-source retention lock files. Unix directories/files are owner-only; reject
symlink substitutions between operations through rooted handles and identity checks.
SQLite opens its sidecars by pathname, so the canonical private application
namespace must remain trusted during each SQLite operation. These checks do not
promise isolation from arbitrary concurrent renames or writes by another process
with the same user's filesystem authority. On other platforms retain application-directory ownership
and fail unsupported access operations explicitly. No raw source enters proposal
SQLite, graph properties, default logs, context responses or spec exports. Retention
does not permit model disclosure or egress.

Short SQLite/WAL transactions bound receipt persistence (128 MiB logical metadata,
200,000 receipts; raised from 64 MiB / 100,000 for REA, ADR-0046 decision 7); capture storage retains SPEC-04's 2 GiB/10,000-capture
logical limits. Within one extraction, rule, owner and GOVERNS receipts are admitted
first under those ceilings exactly as before; declaration receipts only use the
remaining budget, admitted greedily in manifest order, so they never turn an
extraction that fit into an extraction limit failure (the host's persistent store
capacity remains separately authoritative). Each file also stops minting declarations at its
own bound (1,024 receipts / 2 MiB), so a file of many tiny callables cannot retain unbounded
receipts before admission. Every omitted declaration is counted under its file path in the
adapter's result (surfacing it in coverage is #693). SQLite/WAL overhead is additional. Capacity errors explain that
the user can forget retained source before recovering again; no automatic eviction.
Captured inspection validates manifest membership and the selected raw object
without loading unrelated source objects. Output still obeys the retained span cap.

Recovery holds a shared per-source retention guard from acquisition through
publication. Inspection holds a shared guard while copying verified bytes.
Forgetting requires the exclusive guard. Acquire guards before graph/cache/store
mutexes and use fresh persistent try-only OS handles; busy/error fails explicitly,
never retries under another lock or falls back unlocked. Guards coordinate app
processes, not external editors. Managed-source guards remain separate and precede
retention guards; forget never acquires a managed guard.

The source retention UI previews the exact source and retained capture/receipt
counts and current versus historical reference counts. A metadata fingerprint
binds the affected capture/receipt inventory and the current associations shown;
confirmation revalidates it under
the exclusive guard. A changed inventory requires a refreshed preview. Forgetting
atomically removes that source's capture manifests and only raw objects no retained
manifest references. Immutable receipt metadata and proposal/review history remain;
their source reads become unavailable. Current graph associations may remain as
explicit unavailable references. Clearing graph/jobs never deletes retained source.
An explicit later recovery can restore identical content under its identical capture
identity, but different bytes never satisfy the old reference. Deletion ends the
stored span-cap lifetime; explicit recapture applies its supplied limits, while
immutable historical receipt ranges stay unchanged. Receipt metadata is
not automatically removed; reaching its capacity remains an explicit limit.

## Captured inspection and compatibility

The host exposes a fact-qualified description containing its exact receipt ID,
emitted digest and stored range inventory. Inspection accepts typed fact identity,
expected receipt ID and a stored range index; callers cannot supply another root,
path or arbitrary offsets. The host checks current association, fact digest,
registration, immutable receipt and capture membership before returning strict text
from the validated object. Unknown, changed, corrupt, forgotten or invalid-range
references return a fixed unavailable/stale status without echoing source content.
The description request also supplies the selected complete node or edge; the host
hashes it as a consistency check and refuses a changed selected fact. Descriptions
and receipts do not grant arbitrary filesystem access.

The evidence panel offers captured primary source only when the selected fact has
a participating association; it shows the incomplete-input-coverage label. Current
working-tree inspection remains a separately labeled operation with unverified
cited revision. Async results bind their selection and receipt, so switching facts
cannot display an earlier fact's returned source. Retention controls name the exact
source and require a concrete preview before deletion.

EvidenceRef and staging v1 bytes/IDs/reviews are unchanged. Agent task assembly
continues its unverified working-tree basis, and existing accepted proposals stay
awaiting reconciliation. H2 business semantics, full #385, H3 projection and MCP/ACP
execution remain incomplete. Runtime activation requires this whole parser,
publication, inspection and retention contract, not a standalone receipt API.

Reserved T-0148–0155 trace parser invocation, receipt integrity, atomic association,
production ownership, captured inspection, retention, UI and compatibility.

## Local-definition extension

[SPEC-08](SPEC-08_local-rule-definitions.md), tracked in #398, specifies v2
producer receipts for new local initializer citations while retaining the exact
v1 receipt contract and immutable history. Its expanded citations do not change
this specification's primary-source or incomplete-input-closure boundaries.

## Agent task evidence extension

[SPEC-09](SPEC-09_captured-task-basis.md), #401, consumes selected receipt ranges
through a coherent graph/association basis and records immutable per-item source
origins in staging v2. It preserves this specification's primary-source-only and
incomplete-input-closure boundaries; legacy receipts and proposals are unchanged.

## Gap task supporting facts (AC-1019, #385)

A gap task (SPEC-09) also reads the unresolved-call `Gap` it resolves, the
file's own `File` node and its `TestCase` facts. The same captured invocation
records each at emission: the `File` node and `TestCase` facts as the parser
writes them, and the `Gap` from the parser's own pending call, whose exact call
span it read. Directory completion decides whether the call stays unresolved
and may enrich the others, so these receipts are minted per file but kept only
by the directory lane, and only when the complete emitted fact survives
completion unchanged; the single-file lane mints none. They are admitted after
every rule and declaration receipt, and one that does not fit is left out
unnamed, so its fact stays uncaptured. A `Gap` receipt attests the call span
only: that no target was found is a directory-resolution outcome, outside the
receipt's `primary_source_only` / `input_closure_not_established` scope.

Task assembly omits a captured span longer than one span or past the combined
evidence cap (`captured_span_over_limit`) instead of failing the task, as it
omits an uncarriable working-tree span (#1635).

## Python lane (AC-1024, #385)

A source's `.py` files (the Python adapter's own selection) join its capture.
If they cannot be listed or retained together with the TS/JS selection, Python
keeps its uncaptured lane and the TS/JS capture stands alone; TS/JS never loses
its capture to Python. With a capture, the Python pass parses each file's
retained bytes instead of the checkout, and each fact a captured file emits
whose primary provenance lies wholly in that file gets a receipt under its own
contract: prefix `py-primary-v1:`, producer `t0.adapter-python/direct-lexical-v1`,
grammar `python` (`tree-sitter-python@0.25.0`, pinned exactly), in the v3 shape
with provenance ranges only. A rule-bearing fact gets none; Python rule
inventories are a later contract. Receipts are minted per file and kept only
when the complete fact survives directory resolution unchanged, then admitted
after every TS/JS receipt within the same extraction budget; one that does not
fit is left out and its fact stays uncaptured. Directory resolution still reads
templates, requirement pins, Alembic migrations and module names from the
checkout, outside closure, so facts it derives (routes, migrations, screens)
carry no receipt. Inspection, task evidence and MCP reads accept the prefix
like any v3 receipt.

The Python observed-infrastructure pass (AC-1055) reads the same captured
bytes, for its files and for every module it follows an identity into, and
follows only into captured paths. Its facts are not yet receipt-bound.

## Go lane (AC-1026, #385)

A source's `.go` files (the Go adapter's own selection) join the same capture
as the Python lane's. If the combined set cannot be retained, both lanes keep
their uncaptured reads and the TS/JS capture stands alone. With a capture, the
Go pass parses each file's retained bytes, and each fact a captured file emits
whose primary provenance lies wholly in that file gets a receipt under its own
contract: prefix `go-primary-v1:`, producer `t0.adapter-go/direct-lexical-v1`,
grammar `go` (`tree-sitter-go@0.25.0`, pinned exactly), in the v3 shape with
provenance ranges only, under the same per-file minting, survivor check and
budget admission as Python. Directory resolution still reads `go.mod`, module
layout and Vercel configuration from the checkout, outside closure, so facts
it derives or rewrites carry no receipt. Inspection, task evidence and MCP
reads accept the prefix like any v3 receipt.

## Java lane (AC-1031, #385)

A source's `.java` files (the Java adapter's own selection) join the same
capture as the Python and Go lanes'. With a capture, the Java pass parses each
file's retained bytes, and each fact a captured file emits whose primary
provenance lies wholly in that file gets a receipt under its own contract:
prefix `java-primary-v1:`, producer `t0.adapter-java/direct-lexical-v1`,
grammar `java` (`tree-sitter-java@0.23.5`, pinned exactly), in the v3 shape
with provenance ranges only, under the same per-file minting, survivor check
and budget admission as Python and Go. Directory resolution (Spring endpoint
joins, modules, configuration and build files) still reads the checkout,
outside closure, so facts it derives or rewrites carry no receipt.

## Rust lane (AC-1033, #385)

A source's `.rs` files (the Rust adapter's own selection) join the same
capture as the other non-TS lanes'. With a capture, the Rust pass reads each
file's retained bytes for `#[path]` wiring, `#[cfg(test)]` module discovery and
extraction, and each fact a captured file emits whose primary provenance lies
wholly in that file gets a receipt under its own contract: prefix
`rs-primary-v1:`, producer `t0.adapter-rust/direct-lexical-v1`, grammar `rust`
(`tree-sitter-rust@0.24.2`, pinned exactly), in the v3 shape with provenance
ranges only, under the same per-file minting, survivor check and budget
admission as the other lanes. Crate discovery still reads `Cargo.toml` files
from the checkout, outside closure.

## Kotlin lane (AC-1035, #385)

A source's `.kt` and `.kts` files (the Kotlin adapter's own selection) join the
same capture as the other non-TS lanes'. With a capture, the Kotlin pass reads
each file's retained bytes, and each fact a captured file emits whose primary
provenance lies wholly in that file gets a receipt under its own contract:
prefix `kt-primary-v1:`, producer `t0.adapter-kotlin/direct-lexical-v1`, grammar
`kotlin` (`tree-sitter-kotlin-ng@1.1.0`, pinned exactly), in the v3 shape with
provenance ranges only, under the same per-file minting, survivor check and
budget admission as the other lanes. Java type discovery for Kotlin/Java
interop still reads `.java` files from the checkout, outside closure; facts
the directory join derives (unresolved-call gaps, modules, navigation
screens) carry no receipt.

## Terraform lane (AC-1053, #385)

When infrastructure is recovered, a source's `.tf` files (the Terraform
adapter's own confined, `.gitignore`-honouring walk) join the same capture as
the other lanes', under the same fallback. With a capture, the Terraform pass
reads each captured file's retained bytes. Each fact whose primary provenance
lies wholly in one captured `.tf` file gets a receipt under its own contract,
taken from the final facts after local module expansion:

- prefix `tf-primary-v1:`;
- producer `t0.adapter-iac/direct-lexical-v1`;
- grammar `terraform`, with `hcl-edit@0.9.8` (pinned exactly) as both grammar
  and parser package;
- the v3 shape with provenance ranges only.

Per-file minting, the survivor check and budget admission are the same as for
the other lanes.

Some reads stay outside closure. Local module directories are listed in the
checkout. A module file outside the capture is read from the checkout and
carries no receipt. Observed state that later rewrites a resource drops that
resource's receipt at the publication check. The captured lane keeps no parse
cache between runs.

## Captured pipeline reads (AC-1057, #385)

The pipeline reader's workflows and Dockerfiles are retained with the TS/JS
selection itself for every layer selection, even when the lanes' files fall
back; if they cannot be listed or retained, recovery is not published, as for
TS/JS. A capture therefore always holds the reader's whole selection, possibly
none, and with a capture the reader parses only those retained bytes: a
pipeline file created after capture is not read. Preflight's unreadable-pipeline
finding still reads the checkout, and pipeline facts carry no receipt.

## Captured environment reads (AC-1059, #385)

The environment reader's selection (`environments::is_environment_file`:
Compose, kustomize, Helm, Kubernetes-candidate YAML and settings files)
joins the lanes' capture for every layer selection when the whole set fits;
otherwise the lanes are retained without it, so it never costs a lane its
capture. The capture records whether that selection was listed and retained
(`environments_captured`); when it was, the reader reads only the retained
bytes, and the checkout is listed only for its directories and other files: a
selected file absent from the capture is not among the files read, and a
captured file the checkout no longer lists still is. Otherwise, and with no
capture, the reader reads the checkout. Env files are never retained,
because they hold secrets (AC-0097); the reader checks in the checkout only
that one is readable. Preflight's unread-manifest findings still read the
checkout, and environment facts carry no receipt.

## Captured schedule and entry-point reads (AC-1061, #385)

The schedule reader's selection (`schedules::is_schedule_file`) and the
entry-point reader's (`entry_points::is_entry_point_file`), with every file
their facts over the checkout cite, join the lanes'
capture for every layer selection with the environment selection when the
whole set fits (otherwise the lanes are retained without them), and the capture
records whether both were listed and retained (`starts_captured`). When they
were, both readers read only the retained bytes, including the modules a job
or start follows, and list only the capture's own paths, so whether a file a
declaration names exists is decided by the snapshot; otherwise, and with no capture, they read the checkout. Their
facts carry no receipt.

## Captured toolchain reads (AC-1063, #385)

The toolchain reader's selection (`toolchain::is_toolchain_file`: every file a
detector names, other than env files) joins the reader selections tried
beside the lanes, and the capture records whether it was listed and retained
(`toolchain_captured`). When it was, every detector reads the retained bytes,
and a config file's cited length is its captured length; the reader lists
only the capture's own paths and the checkout's env files. Otherwise, and with
no capture, the reader reads the checkout. Env files are never retained,
because they hold secrets (AC-0097): their key names and allowlisted values
(AC-1021) are read from the checkout. Preflight's detection still reads the
checkout, and toolchain facts carry no receipt.

## Captured documentation reads (AC-1065, #385)

The documentation walk's selection (`spec::is_documentation_file`: Markdown
and plain-text files outside the directories the walk skips) is tried with
the reader selections beside the lanes and dropped first when the whole set
does not fit, and the capture records whether it was retained
(`documents_captured`). When it was, recovery's documentation read and the
found-ADR relinking after publication read the retained bytes, listing only
the capture's own paths; the bytes are copied out of the capture before its
retention guard is released, so the relinking reads what recovery captured.
Otherwise, and with no capture, both read the checkout. Repositories another
source published are relinked from their checkout, declared API references
are read from their loader's validated contents, and documentation drift
compares paths against the checkout's tree. Documentation facts carry no
receipt.

## Directory-built lane facts (AC-1037, #385)

The Python, Go, Java, Rust, Kotlin and Terraform lanes mint their receipts from each
lane's final facts, after directory resolution, rather than from each file's
own extraction. A fact whose every primary evidence entry cites one captured
lane file is minted against that file under the lane's contract, whether the
file's own pass or the directory join built it, so a Spring endpoint composed
from a class and a method mapping, or a module built from a file's package, is
covered; files are taken in capture order and candidates are staged within the
extraction budget as before. The publication check still drops a receipt whose
fact publication does not materialize as minted. A fact citing more than one
file (a Flask route and the Blueprint mount in another module) or a file
outside the capture still carries no receipt, nor does a rule-bearing fact. As
before, the receipt binds the cited captured spans only; input closure is not
established, and earlier lane sections' "facts the join derives carry no
receipt" applies only to multi-file facts from here on.

## Facts citing several captured files (AC-1039, #385)

A lane fact whose primary evidence cites more than one captured file of the
same lane, such as a Flask route and the Blueprint mount in another module,
now gets one receipt under that lane's contract. The receipt's `file` is the
file its first evidence entry cites; the others are listed once each in
`other_files`, sorted by path, and each range names its file by index into that
list (`file` absent for the primary file). Every listed file must belong to the
same capture and grammar, be cited by at least one range, and differ from the
primary file; at most 8 are listed, an empty list or an explicit null index is
rejected, and only provenance-only v3 receipts may carry the list. Each range is
validated and read back against its own file. A fact citing a file outside the
capture or of another lane, or a rule, still carries no receipt; single-file
receipts are unchanged on the wire. As before, the receipt binds the cited
captured spans only and input closure is not established.

## Manifest observation inputs (AC-1041, ADR-0080, #385)

Terraform `state_json`, Pulumi `pulumi_json` and OTLP `otel_jsonl` inputs a
system manifest declares for a repository are input artifacts, not repository
source. Those inside the manifest folder are captured together, as a
working-tree capture of that folder selecting only the declared paths, under
the identity `input/<registered source id>`. The T1 parsers read the captured
bytes, and observed evidence cites that identity, the declared relative path
and `workdir`. A declared path outside the folder, or one the capture limits
refuse, is still read from disk, but its evidence keeps the uncaptured form
(empty repo and commit) and the producer's coverage reason names it as not
captured, so unverified. A published recovery retains each input capture
beside its source's captures; the retained-source preview counts it and a
forget removes it (AC-1043). Each fact whose observed evidence cites one
captured input file gets a provenance-only `obs-input-v1` receipt for those
exact spans, owned by the input identity and stored within its source's
receipt share; the preview counts them and a forget reclaims them (AC-1045,
ADR-0079). Each retained one is published as the fact's observed binding,
beside its primary binding and in the same transaction, in
`source_node_observations` or `source_edge_observations` (association
schema v3); a later recovery that no longer binds it reclaims it
(AC-1047). The app describes and reads an observed binding on its own,
admitting only the source's input identity, and shows it as "Observed in"
beside the declaration, never in its place (AC-1049). Scoped MCP evidence
of kind `retained_observed_input` resolves the observed role at the session's
revision: its target names the input identity, its scope is
`observed_input_only`, and the kind and target must agree (AC-1051).
