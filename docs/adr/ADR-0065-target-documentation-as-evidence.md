# ADR-0065 — Target documentation as evidence

- **Status:** Proposed; owner review
- **Date:** 2026-11-17
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1750; #1745; US-0049 / AC-0676..0681; ADR-0056; ADR-0058; ADR-0036; ADR-0064; ADR-0045; [SPEC-18](../SPEC-18_documentation-evidence.md)

## Context

The engine is meant to trace four things: infrastructure, code, UI and
documentation. Documentation is read today by two narrow readers:

- declared API reference files (`crates/ingest/src/api_docs.rs`), compared
  with served endpoints in `crates/spec/src/contract.rs`;
- decision records found under decision directories
  (`spec::extract_found_adrs`), stored as `ADR` nodes with `DECIDES` edges
  to the fact ids they name.

A README, a `docs/` tree, a runbook or a design note is not read. The
product cannot say what the existing documentation refers to, cannot show
where a reference no longer matches the code, and gives the Section Writer
no cited record of stated intent, although a claim of kind
`documented_intent` already exists (ADR-0064).

## Decision

1. **A document is a fact about a file, read at T0.** Each Markdown or
   plain-text file of a registered source that the reader accepts becomes
   one `Document` node, and each of its sections one `DocSection` node,
   with the byte span of the section, its heading and its level. They are
   Deterministic and Confirmed: they state that the file and the section
   exist at that commit, nothing more.
2. **What a document says is stated intent.** No statement in a document
   becomes a fact about the code, and no document changes the tier,
   confidence or content of another fact (R-INT-1). A document's text is
   not stored in the graph: a section node holds its heading, its span and
   a content hash, and the text is read from the source through the same
   redacting read as any source excerpt.
3. **Only an exact reference is linked deterministically.** A reference
   is a Markdown inline code span or a link target inside a section. It is
   linked when it resolves to exactly one fact by one of four rules, tried
   in this order: a fact id; a repository-relative file path; a route
   written as a method and a path; a name that exactly one fact of a named
   kind carries. The link is a `MENTIONS` edge from the section to the
   fact, citing the span of the reference. A reference that matches no
   rule, or more than one fact, is not linked and is not a finding: looser
   matching belongs to the semantic or agentic tier and stays staged.
4. **A broken exact reference is drift.** Two reference forms assert
   something checkable: a route, and a relative link or path to a file of
   the same repository. When a route names a path that is served only with
   other methods, or that no recovered endpoint of the system serves, or
   when a relative file target does not exist at the commit, the reader
   emits a `Drift` node. It cites the reference span, and the recovered
   facts it was compared with where there are any; where there are none it
   states what was searched.
5. **The reader is bounded and reports what it skipped.** It reads files
   by extension, honours ignore files, and stops at a per-file size, a
   per-repository file count and a per-file section count. Every file it
   finds is counted as read or as skipped with a closed reason, in the
   coverage inventory.
6. **The two existing readers are unchanged in this decision.** A declared
   API reference file and a found decision record are also documents and
   get `Document` and `DocSection` nodes. Their `ADR`, contract and drift
   facts stay as they are. Folding those readers into the document reader
   is later work and changes no fact identity when it happens.
7. **Same commit, same facts.** Node ids are derived from the repository,
   the path and the section's ordinal. Files, sections and references are
   processed in sorted order. A second ingest of the same commit yields the
   same nodes, edges and graph content hash.

## Consequences

- Section drafts can cite a `DocSection` for a `documented_intent` claim,
  and staleness (ADR-0045) applies to it like any cited fact: an edited
  section has another content hash.
- The fact index lists documents and sections, so a citation resolves.
- The drift register gains entries whose only evidence may be a document
  span. They are InferredStrong, as contract drift is.
- A large documentation tree is partly read. The coverage document says so
  in counts and reasons, never as completeness.

## Not decided here

- **External systems.** Confluence, Jira, Notion and wiki exports that are
  not files of a registered source.
- **Other formats.** PDF, Office and HTML.
- **Meaning.** Whether a paragraph agrees with the code is a T2 or T3
  question. Nothing here answers it.
- **reStructuredText and AsciiDoc structure.** They are plain text to this
  reader: one section per file.

## Alternatives rejected

1. Store document text in the graph, as `ADR` nodes store their body.
   Rejected: it would put unredacted prose into exports and embeddings by
   default and grow the graph with content no rule needs.
2. Link a reference that matches several facts to all of them. Rejected:
   an ambiguous mention is not evidence about any one of them.
3. Treat every unresolved code span as drift. Rejected: most code spans in
   prose are not references to the system, and the register would fill
   with findings no one can act on.
4. Let a model find references. Rejected for this tier: structure and
   exact references are established at the lowest tier that can.
