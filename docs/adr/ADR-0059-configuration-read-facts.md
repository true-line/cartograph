# ADR-0059 — Configuration-read facts

- **Status:** Proposed; owner review
- **Date:** 2026-11-12
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1789; #1745; US-0002 / AC-0597; `docs/DOCUMENT-CATALOGUE.md` D7 §7.3, D6 §6.4

## Context

The catalogue's D7 §7.3 lists every configuration key the system declares or
reads. Declared keys exist as `Tool` facts from environment files. Read keys
do not exist as facts at all.

What the Python and TypeScript adapters emit today is something else: when an
environment variable is the identity argument of a recognised client call
(`smtplib.SMTP(os.environ["SMTP_HOST"])`), they emit an observed `Resource`
with an `env_key` property. That fact says a client is configured by a key.
A bare `x = os.getenv("X")` emits nothing, in any language, and Go, Java,
Kotlin and Rust recognise no environment read at all. #1789 was written on
the assumption that a per-read fact existed in Python; it does not, so its
shape has to be decided.

## Decision

1. **A new node label, `ConfigRead`.** One node per source file and key:
   id `cfgread:{repo}@{path}:env:{KEY}`. Props: `source: "env"`, `key`,
   `language`, and provenance whose evidence lists the span of every read of
   that key in that file, in byte order. The node is T0 Deterministic and
   Confirmed.
2. **A dynamic read is recorded without a name.** A read whose key is not a
   string literal yields one node per file, id
   `cfgread:{repo}@{path}:env:?`, with `dynamic: true`, no `key`, and the
   span of every such read. The documents can then say that keys exist which
   they cannot name.
3. **A new edge label, `READS_CONFIG`,** from the enclosing symbol of each
   read, or from the file when there is none, to the node.
4. **Only a proven standard-library read is a fact.** The adapter must
   establish that the call is the language's own environment read
   (`process.env.X`, `os.getenv`, `os.Getenv`, `System.getenv`,
   `std::env::var`), by the same means it already uses to prove an imported or
   global symbol. A shadowed look-alike yields nothing. Where an adapter
   cannot prove it, it emits nothing and the limit is stated.
5. **Names only.** No default value, fallback or resolved value is stored.
6. **Every compiled-in adapter emits the same shape,** assembled by one
   function in `adapters-fw`, including Python and TypeScript. Their existing
   `env_key` resources and `CONFIGURED_IN` edges are unchanged; a read that is
   also a client identity yields both facts, so §7.3 is built from one fact
   kind.
7. **A literal that is not a key name is not stored.** An empty, blank,
   over-long or secret-shaped literal is recorded as a dynamic read.
8. **A configuration read is never a candidate target for a gap.** The
   agentic tier's candidate lists skip `ConfigRead` nodes; they stay in the
   surrounding context a prompt may show.
9. **Coverage is stated per language.** Each adapter has a `config_reads`
   coverage cell with a conformance fixture, and the cell names what that
   adapter does not read.
10. **Configuration frameworks are not covered** (Spring `@Value`, NestJS
   `ConfigService`, Viper). They follow as separate increments under the same
   label with a different `source`.

## Consequences

- Re-ingesting a repository adds `ConfigRead` nodes and `READS_CONFIG` edges,
  so its graph content hash changes once. Determinism per commit is
  unaffected.
- Keying the node by file and key, not by byte offset, keeps its identity
  stable when unrelated lines in the file change; the spans move, the fact
  does not.
- A key read in ten files is ten nodes. The projection groups them by key.
- Until a projection reads them (a later increment of #1789), the facts appear
  only in the graph and in graph-backed lists.
- Java and Kotlin record `System.getenv` when nothing in the repository
  declares, imports or shadows a `System`. A member of that name inherited
  from a class outside the repository cannot be ruled out; the coverage cells
  say so. Kotlin files with library star imports yield nothing.
- TypeScript does not read `import.meta.env` or destructured `process.env`;
  Python does not read `"X" in os.environ`; Rust does not read `env!`.
- The nodes are not grouped under a layer in the Atlas yet and show their raw
  id there.

*Amended for #1821 (AC-0651):* `configuration.md` projects these facts, so
they no longer appear only in the graph. In the Atlas a `ConfigRead` node is
in the Server code band, in the cluster of the file that reads it, and is
labelled with its key; a dynamic read is labelled "key not named in source".
The reads listed above as not read are still not read.

## Alternatives

1. **Reuse the observed `Resource` with `env_key`.** It needs a provider and
   describes a client, and collapses reads; a bare read fits neither.
2. **One node per read site,** keyed by byte offset. Simple, but every edit
   above a read changes its identity and the node count follows the number of
   reads, not the number of keys.
3. **One node per repository and key.** Smallest, but loses which file reads
   the key unless every span is carried on one node, and makes incremental
   per-file re-ingest rewrite a shared node.
