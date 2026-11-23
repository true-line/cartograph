# Authoring a new language adapter

Cartograph turns source code into a graph of Confirmed (T0) facts — files,
symbols, imports, call edges, endpoints — via per-language *adapters*. There
are two ways to add one, and they are not interchangeable:

| | WASM plugin | Compiled-in crate |
|---|---|---|
| Who can ship it | Anyone — no core-repo PR | A maintainer, via PR to this repo |
| Where it runs | Sandboxed (`wasmtime`), inside the app | Native, linked into the `app` binary |
| Toolchain | `rustc --target wasm32-wasip2` (or any language that compiles to a WASI-P2 component) | Full Rust workspace |
| Use it for | A new language nobody's covered yet | Extending a language family a core crate already owns (e.g. this repo added plain JavaScript to the existing TypeScript crate rather than shipping a new one — see `crates/adapters-lang-ts`, #209) |

**Default to the plugin path.** [ADR-0017](../adr/ADR-0017-runtime-wasm-adapter-plugins.md)
frames it as the intended way to extend language coverage without every
contributor needing to land Rust in the trusted core. Reach for a compiled-in
crate only when you're a maintainer extending a language family that already
has one (see Path B).

## Path A: WASM plugin

The whole loop — discover → gate → enable → route → extract → close the
Preflight finding — is implemented and tested end to end in
`crates/adapters-plugin-host`. `app::plugin_lane_end_to_end_gate_accept_reingest_closes_finding`
(`src-tauri/src/lib.rs`) is a real, passing test that runs this exact path
against a compiled fixture. Nothing below is aspirational.

### 1. The contract

`crates/adapters-plugin-host/wit/adapter.wit` is the whole SPI — one
function:

```wit
extract-source: func(
    source: list<u8>,
    path: string,
    id: source-id,
) -> result<extraction, extract-error>;
```

You get read-only source bytes and a repo-relative path; you hand back nodes
and edges. `node`/`edge` records carry `id`/`label`/`props-json` — the same
shape compiled-in adapters embed in `core_graph::Node`/`Edge.props`, so
`props-json` must be a serialized JSON object. Add a `prov` key only to cite
a tighter evidence span than the whole file (optional, and checked — see
step 4).

### 2. Start from the reference implementation

`crates/adapters-plugin-host/tests/fixtures/ok-adapter/src/lib.rs` is a
complete, minimal, working guest — not a real language adapter, but
structurally the template:

```rust
wit_bindgen::generate!({
    path: "../../../wit",
    world: "adapter",
});

struct Guest;

impl exports::cartograph::adapter::extract::Guest for Guest {
    fn extract_source(
        source: Vec<u8>,
        path: String,
        id: exports::cartograph::adapter::extract::SourceId,
    ) -> Result<
        exports::cartograph::adapter::extract::Extraction,
        exports::cartograph::adapter::extract::ExtractError,
    > {
        // parse `source`, emit nodes/edges
    }
}

export!(Guest);
```

Fork it into your own crate (own `Cargo.toml`, `crate-type = ["cdylib"]`,
depends only on `wit-bindgen` plus whatever pure-Rust parsing crate you need
— no C toolchain, no network calls, no filesystem access beyond the `source`
bytes you're given). The four adversarial fixtures next to it
(`busy-loop`, `memory-hog`, `net-probe`, `clock-probe`) are worth reading too
— each proves one sandbox guarantee (fuel, memory, no network, no clock) and
doubles as a checklist of things your adapter must never rely on.

### 3. Build

```sh
rustup target add wasm32-wasip2   # once
cargo build --target wasm32-wasip2 --release
```

No `cargo-component` or `wasm-tools` needed — `cargo build` on this target
emits a component the host loads directly. Copy
`target/wasm32-wasip2/release/{crate_name}.wasm` to where you're testing it
(step 7).

### 4. Determinism and provenance — what's checked, what's free

- **Extractor identity is not yours to set.** The host overwrites
  `prov.extractor_id` to `{your-plugin-id}@{artifact-blake3-hash}` after every
  call (`pin_extraction`) — a swapped artifact can never claim an old
  identity, and you can't impersonate a compiled-in extractor.
- **Missing provenance is backfilled, not rejected.** If a fact you return
  has no `prov` (absent or `null`), the host builds T0
  Deterministic/Confirmed provenance with a whole-file evidence span
  (`enforce_provenance`). Stamp your own only if you can cite a tighter span
  (a specific line/byte range) — otherwise leave it out and let the host do
  it.
- **Provenance you write is checked, and only its evidence spans are kept.**
  The host rebuilds every `prov` itself. A `prov` object you write must
  satisfy all of these, or the call fails with `MalformedFact` and no fact
  from that file is kept:
  - `tier` is `"Deterministic"` and `confidence_tier` is `"Confirmed"`. A
    plugin cannot claim an observed (T1), semantic or agentic fact, and no
    manifest field raises this ceiling.
  - `evidence` holds at least one reference, and each one has the `repo` and
    `commit_sha` from the `id` argument, the `path` argument exactly as
    given, and `byte_start <= byte_end <= source.len()`.
  - No other key is present except `extractor_id` and `content_hash`, which
    the host discards and replaces. `method` and `basis` are rejected.
- **The content hash is not yours to set.** The host computes
  `prov.content_hash` over the fact's id, label and props (without `prov`),
  for facts with and without their own provenance alike.
- **Determinism is enforced, not assumed.** The conformance gate (step 6)
  runs your corpus twice and requires byte-identical canonical output. Don't
  read wall-clock time, random numbers, or ambient state — you don't have
  access to any of it anyway (see below), but design your logic as if you
  didn't even if some future capability grant changed that.

### 5. Sandbox limits

Every call gets a fresh `Store`, an **empty WASI context** (no filesystem,
no network, no environment, no args, clocks fixed at zero, deterministic
RNG), and these bounds (`PluginLimits::default()`):

| Bound | Default |
|---|---|
| Fuel (≈ instructions) | 10,000,000,000 |
| Linear memory | 64 MB |
| Table elements | 10,000 |
| Wall-clock deadline | 5 seconds |

Any violation — fuel exhaustion, memory/table cap, deadline, a trap, a
malformed fact — fails that call closed: zero facts, never partial.

What happens to the rest of the ingest depends on the violation:

- **The file is refused and the ingest continues** when the failure depends
  only on your plugin and the file: a malformed fact, an `Err` from
  `extract-source`, a trap, or the fuel, memory or table bound. No fact from
  that file by your plugin joins the graph. The coverage inventory lists the
  file under your plugin with its reason class (`malformed_fact`,
  `guest_error` or `bound_exceeded`) and does not count it as covered. Your
  error text is not recorded.
- **The whole ingest fails** on the wall-clock deadline, a call-stack
  overflow, a failed instantiation, or any failure that is not a wasm trap.
  These depend on the machine or have no established cause, so skipping the
  file would give different graphs for the same commit.

Design for files in the low hundreds of KB parsed in well under a second; if
your grammar is slow enough to worry about these numbers, that's a signal to
simplify before it's a signal to ask for more fuel.

### 6. Write the golden corpus

Next to your compiled artifact, `{your-plugin-id}.golden.json`:

```json
{
  "extensions": ["rb"],
  "cases": [
    {
      "path": "src/lib.rb",
      "source": "def greet(name)\n  puts name\nend\n",
      "nodes": [
        { "id": "file:golden@src/lib.rb", "label": "File", "props": {} },
        { "id": "sym:golden@src/lib.rb#greet", "label": "Symbol", "props": { "kind": "Function" } }
      ],
      "edges": []
    }
  ],
  "coverage": [
    { "language": "Ruby", "framework": "core", "capability": "types",
      "support": "supported", "cases": ["src/lib.rb"] },
    { "language": "Ruby", "framework": "rails", "capability": "http_endpoints",
      "support": "unsupported", "note": "routes.rb is not read yet" }
  ]
}
```

- `extensions` is your coverage claim (#201 routing) — once gated, the host
  hands your plugin every file under these extensions (sorted walk, vendored
  dirs skipped) and nothing else.
- Each case's `nodes`/`edges` are the **exact** expected facts. Don't
  hand-write the pinned `extractor_id`, the content hash,
  `plugin_artifact_hash`, or a whole-file `prov` — the gate computes those
  the same way a real call would. If your plugin cites its own spans, the
  expected fact carries the same `prov` (`tier`, `confidence_tier`,
  `evidence`), held to the step 4 rules. The gate diffs against
  your `extract-source` output canonically (`node:id:label:props` /
  `edge:src:dst:label:props`, sorted).
- The gate's fixed source id for expected-fact computation is
  `{ repo: "golden", commit: "golden" }` — that's what `id.repo`/`id.commit`
  will be inside `extract_source` when the gate calls you, and it's fine (and
  expected) for your node/edge ids to embed it, as in the example above. Node
  id *shape* is entirely your choice — the host doesn't enforce
  `file:`/`sym:` conventions — but reusing them keeps your facts consistent
  with everything else in the graph.
- An empty corpus fails the gate closed. Cover the constructs you actually
  claim to extract; each golden case's `path` is also the file the routing
  layer would hand you at that extension, so pick realistic paths.
- `coverage` states your language × framework × capability cells (AC-0263),
  shown in Preflight and Settings alongside the compiled-in matrix. A
  `supported` or `partial` cell names the golden `cases` that prove it, at
  least one of which asserts nodes or edges (an empty-output negative case
  may accompany it but cannot prove a claim alone). Since every case must
  pass, a gated plugin's claims are proven. Cells are reported per language,
  so a plugin covering several languages lists each one's cells. For an
  extension the core cannot name, `languages` gives its language
  (`"languages": {"foo": "Foo"}`, keys must be claimed extensions); without
  one it shows as `.foo` with no cells. Language names compare
  case-insensitively, so `Ruby` and `ruby` are one cell. `partial`
  and `unsupported` need a `note` saying what is missed, `supported` must
  have none, and `unsupported` names no cases. `capability` is snake_case and
  may be one the core does not know (`graphql_operations`), which is how a
  plugin makes a new kind of fact visible. A malformed declaration fails the
  `coverage-declarations` check.

### 7. Drop it in and gate it

Copy `{your-plugin-id}.wasm` and `{your-plugin-id}.golden.json` into
`.cartograph/adapters/` at the root of the repo you're testing against (a
user-level directory also works, for adapters you want available across
every project — project copies win on id conflict). Open **Settings →
Adapters**: your plugin appears under "Plugin adapters (discovered)" with a
content hash and a **Run conformance gate** button. Run it — every check
(`spi-compiles`, `corpus-nonempty`, one `contract:{path}` + `golden:{path}`
pair per case, `coverage-declarations`, `determinism-double-run`) needs to
pass; a failure names the specific check and why. The verdict binds to the
artifact *and* the exact corpus bytes: editing `extensions` or `coverage`
after gating leaves the plugin ungated until you run the gate again.

### 8. Enable it

Once gated, flip the enable toggle in the same Settings panel. Enablement is
bound to the exact content hash that passed — rebuild your adapter and it
goes back to ungated until you re-run the gate, by design (a swapped
artifact never silently runs).

### 9. Confirm it closes the finding

Re-run Preflight (or re-ingest) on a repo containing files your `extensions`
cover. The `uncovered-language` finding for that language should disappear —
your plugin is now part of the active extraction set for that root.

### 10. Suggested AI-assisted workflow

This is the part [ADR-0017](../adr/ADR-0017-runtime-wasm-adapter-plugins.md)
explicitly hasn't built yet (in-app generation — see its Decision section,
point 6, "v1 scope") — but nothing stops you from doing it yourself, assisted,
outside the app:

1. Give an AI assistant `wit/adapter.wit`, the `ok-adapter` fixture, and a
   handful of representative source files in your target language.
2. Ask it to scaffold a guest crate that parses those constructs into
   nodes/edges following the same id/label conventions this repo's
   compiled-in adapters use (`file:{repo}@{path}`, `sym:{repo}@{path}#{name}`,
   `IMPORTS`/`CALLS` edges) — cite `crates/adapters-lang-go/src/lib.rs` as a
   second, non-WASM reference for "what facts matter" if the language is
   Go-shaped (imports, calls, endpoints), or whichever existing adapter is
   closest in shape to your target language.
3. Build it (step 3), hand-write or AI-draft the golden corpus (step 6) from
   the same source snippets you scaffolded against.
4. Iterate against the gate (step 7) until every check passes — the gate's
   named failures are exact enough to drive another round of AI iteration
   without you reverse-engineering what went wrong.

## Path B: extending or adding a compiled-in crate

This is the maintainer path — touches the trusted core, needs a PR into this
repo. Use it when you're extending a language family an existing crate
already owns (the JavaScript-in-TypeScript-crate precedent, #209) rather than
introducing a brand-new language (use Path A for that).

There is no shared Rust trait to implement — `docs/adr/ADR-0003-treesitter-adapter-spi.md`
describes the conceptual shape, but each compiled-in crate
(`adapters-lang-ts`, `-python`, `-go`, `-java`) is its own free-function
convention you match by example:

- `pub struct SourceId<'a> { repo: &'a str, commit: &'a str }` — identity
  that lands in every fact's evidence.
- `pub struct IncrementalCache { ... }` — content-hash-keyed per-file parse
  cache for delta re-ingest.
- `pub struct Extraction { nodes: Vec<Node>, edges: Vec<Edge>, .. }` — the
  facts, plus whatever directory-wide "pending" resolution your language
  needs (see `adapters-lang-ts`'s `pending_calls` for the pattern: emit a
  best-guess edge or a Gap, deferred until every file in the directory is
  known).
- `pub fn extract_dir_incremental(root, id, cache) -> Result<(Extraction, IncrementalStats), Error>`
  — walk the directory (skip `node_modules`/`.git`/hidden dirs/etc.), parse
  each file with tree-sitter, merge.
- `pub fn extract_dir_incremental_with_progress(root, id, cache, on_file: &mut dyn FnMut(&str)) -> ...`
  — same, calling `on_file` once per file for the shell's live progress
  indicator (#209); `extract_dir_incremental` is a thin wrapper passing a
  no-op closure, so existing callers never need to change.
- Every fact's `props.prov` is a `core_prov::Provenance` (tier
  `Deterministic`, confidence `Confirmed`, evidence span, content hash,
  `extractor_id`) — built via `Provenance::new(..)`, never invented by hand.

Wiring it in (`src-tauri/src/lib.rs`):

1. Add a call inside `extract_tree_incremental` (gated by whichever
   `wants_*` layer hint applies), merging `Extraction`/`LayerBreakdown`/
   `DeltaSummary` the same way the existing five calls do.
2. Add an `AdapterInfo` entry to `INSTALLED_ADAPTERS`
   (`crates/ingest/src/preflight.rs`) — this is the single registry that
   drives both Preflight coverage and the Settings inventory; they can never
   disagree because there's only one list. If the language was previously in
   `PLANNED_ADAPTERS`, remove it from there (see #209's JavaScript move) —
   and if it shares an `id` with another entry (as JavaScript does with
   TypeScript, both `t0.adapter-ts`), make sure any UI keying off that field
   uses something unique instead (`SettingsSurface.tsx` keys its adapter list
   by `language`, not `id`, for exactly this reason).

3. State coverage as cells in `COVERAGE_MATRIX` (`crates/ingest/src/matrix.rs`,
   AC-0253): one language × framework × capability entry per thing the
   adapter does or deliberately does not do. A `Supported` or `Partial` cell
   needs a conformance fixture at `crates/ingest/conformance/<cell-id>/`, where
   the cell id is `<language>.<framework>.<capability>` (`core` for the
   framework of language-level cells). The fixture is `src/` plus an
   `expect.json` listing the facts it must yield: `nodes` (label, id and
   optionally a props subset), `edges` (label, src, dst), `event_sites`
   (kind, role, identity) and `fetch_sites` (method, url), with ids under
   repo `local/p`. `app::every_claimed_cell_passes_its_conformance_fixture`
   runs every fixture through the full T0 pipeline. Partial and unsupported
   cells carry a `note` naming what is missed; that note is what Preflight and
   Settings show. Ingest tests fail on a claim with no fixture, a fixture
   with no claim, or an installed language with no cells.

### Configuration reads: one shape for every adapter

A standard-library read of an environment variable is a fact
([ADR-0059](../adr/ADR-0059-configuration-read-facts.md), #1789). Every
compiled-in adapter emits it through one function,
`adapters_fw::config_read::assemble`, so the shape never differs by
language. The adapter collects `ConfigReadSite`s (key or `None`, language,
enclosing symbol or `None`, path, span of the read expression) and the
function returns:

| Fact | Shape |
|---|---|
| Named read | Node `ConfigRead`, id `cfgread:{repo}@{path}:env:{KEY}`, props `source: "env"`, `key`, `language`, `prov` |
| Dynamic read (key is not a string literal) | Node `ConfigRead`, id `cfgread:{repo}@{path}:env:?`, props `source: "env"`, `dynamic: true`, `language`, `prov`; no `key` |
| Who reads it | Edge `READS_CONFIG` from the enclosing symbol, or from `file:{repo}@{path}` when there is none, to the node; one edge per source and node |

There is one node per file and key, not per read: `prov.evidence` lists the
span of every read of that key in that file in byte order, and an edge cites
the reads its source makes. Provenance is T0 Deterministic, Confirmed. Ids
follow the other adapter ids (`{scheme}:{repo}@{path}…`, nothing escaped).
The output is independent of the order sites were collected in.

Rules an adapter must keep:

- **Prove the call, or emit nothing.** A site is a read only when the callee
  is provably the language's own environment read, by the means the adapter
  already uses for an imported or global symbol. A look-alike (a local
  function of the same name, a rebound receiver, another package under the
  same alias) is not a site. Say in the module documentation what the proof
  cannot rule out.
- **Names only.** Never record a value, a default or a fallback. A literal
  that is not a key name (empty, containing whitespace, longer than 128
  bytes or matching a secret recognizer) is recorded as a dynamic read.
- **A key is a literal as written.** A constant, a concatenation or a
  template is a dynamic read; do not follow it.
- **Reads, not writes.** Setting or deleting a variable is not a fact.
- **Configuration frameworks are a different `source`** and are not emitted
  yet (Spring `@Value`, NestJS `ConfigService`, Viper).

| Adapter | Recognised | Proof | Module |
|---|---|---|---|
| TypeScript / JavaScript | `process.env.X`, `process.env["X"]` | `process` is the unbound global or an import of `process` / `node:process`; the file does not write `process.env` | `adapters-lang-ts/src/observed_infra.rs` |
| Python | `os.environ["X"]`, `os.environ.get("X")`, `os.getenv("X")` | `os` (or `environ` / `getenv`) is a module-level import nothing rebinds; no repository module is named `os` | `adapters-lang-python/src/observed_infra.rs` |
| Go | `os.Getenv("X")`, `os.LookupEnv("X")` | the operand is the file's import of `os`, unshadowed in the enclosing declaration | `adapters-lang-go/src/config_read.rs` |
| Java | `System.getenv("X")` | nothing in the file or the repository's Java sources is named `System`; no static wildcard import | `adapters-lang-java/src/config_read.rs` |
| Kotlin | `System.getenv("X")` | as Java, and every star import names a repository package | `adapters-lang-kotlin/src/config_read.rs` |
| Rust | `std::env::var("X")`, `std::env::var_os("X")` | the callee path resolves to `std::env` through `use` or the qualified path | `adapters-lang-rust/src/config_read.rs` |

The observed `Resource` with `env_key` (a client configured by a variable)
is a separate fact and is unchanged; a read that is also a client identity
yields both. A fixture test per adapter holds one literal read, one dynamic
read and one shadowed look-alike, and expects exactly one named fact, one
dynamic fact and nothing for the look-alike (AC-0597).

Each adapter states the capability as a `config_reads` cell in
`COVERAGE_MATRIX` (`<language>.core.config_reads`, with its conformance
fixture), and the cell's note names what that adapter does not read, so
Preflight and Settings show which languages cover it.

## Path C: an event registry data file

When an existing adapter already parses the language and only an SDK call
shape is missing (a message bus client, say), no code is needed. The event
SDK and channel backing registries are data: the built-in files are
`crates/adapters-fw/registries/event-sdks.json` and
`crates/adapters-fw/registries/channel-backings.json`, and each ingest
also reads `<app_data>/registries/*.json` in name order.

```json
{
  "schema": "cartograph-event-sdk-registry@1",
  "version": "nats-1",
  "sdks": [
    {
      "kind": "nats-subject",
      "role": "produces",
      "pattern": { "method": { "module": "nats", "ctor": "Client", "method": "publish" } },
      "identity": "first"
    }
  ]
}
```

- `pattern` is one of `constructor {module, ctor}`, `method {module, ctor,
  method}` or `factory_method {module, factory, method}`; `identity` is
  `"first"` or `{"key": "<property>"}`. An SDK file may also declare
  `qualifiers` and `batch_keys`; a
  `cartograph-channel-backing-registry@1` file declares `backings`
  (`resource_type`, `kind`, `attribute`) for kinds some SDK entry
  establishes.
- Each file is validated whole: unknown fields or schemas, duplicate keys,
  invalid names, more than 64 KiB, or a call shape, qualifier, batch key or
  backing that is already registered rejects the entire file, and the
  ingest summary lists it with the reason. A rejected file changes no
  recovered fact.
- Every fact an entry produces names its file in provenance
  (`registry_sources`: name, schema, version and content hash of every
  file whose SDK entry, qualifier or batch key shaped the fact, on the
  channel and Gap nodes as well as the edges), and the set of
  accepted files is part of the coverage basis and the parse cache key, so
  adding or editing a file re-extracts instead of reusing stale facts.

A `cartograph-event-sdk-registry@2` file may give an entry `"ecosystem":
"pypi"` (npm otherwise) for the Python adapter: a `constructor` is a
`<module>.<ctor>(…)` call, a `method` is called on a receiver bound once to
`<module>.<ctor>(…)` or on that call inline, and `"select": {"first":
"<literal>"}` requires the factory's first argument (`boto3.client("sqs")`,
in `crates/adapters-fw/registries/event-sdks-pypi.json`); `identity`
`"first"` is the first positional argument and `{"key": …}` a keyword.
`factory_method` is npm-only, and qualifiers and batch keys shape TypeScript
sites only. The TypeScript and Python adapters consult the event registry,
each reading only its own ecosystem's entries.

## Traceability — required either way

Per `AGENTS.md`'s SDLC, any new user-visible capability gets an AC in
`docs/user_stories.md` (fixed schema, Given/When/Then), a matching row in
`docs/US-TM.md`, and a `docs/test-map.md` entry naming the real test(s) that
realize it. Run `node scripts/check-traceability.mjs` before opening a PR —
it fails closed on any of the three going out of sync.
