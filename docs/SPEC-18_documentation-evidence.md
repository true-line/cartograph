# SPEC-18 — Target documentation as evidence

- **Status:** Proposed for [#1750](https://github.com/true-line/cartograph/issues/1750). Slice 1 is implemented; the rest is not.
- **Date:** 2026-11-17
- **Scope:** US-0049 / AC-0676..0681; ROADMAP W16; v1.0.0 criterion 8.
- **Decision:** [ADR-0065](adr/ADR-0065-target-documentation-as-evidence.md).
- **Extends:** ADR-0036 (coverage inventory), ADR-0058 (citation ids), the drift register.

## Outcome and boundary

After ingest, the Markdown and plain-text files of each registered source
are in the graph as documents and sections. A section that names a file, a
route or a fact exactly is linked to it. A route or file reference that no
longer matches is in the drift register with what it was compared with. The
coverage document counts the files found, read and skipped.

Nothing a document says becomes a fact about the code. No model is used. No
target file is written (NG1).

## Files read

- Extensions: `.md`, `.markdown` (Markdown) and `.txt` (plain text),
  matched without regard to case.
- Walk: the source walk already used for decision records, honouring
  ignore files and skipping dot directories, `node_modules`, `target`,
  `dist`, `build` and `vendor`.
- Bounds: 1 MiB per file; 2,000 files per repository, taken in path order;
  512 sections per file.
- Skips, each counted with one reason: `too_large`, `not_utf8`,
  `over_file_limit`. A file past the section bound is read to the bound and
  counted as `sections_truncated`.

## Facts

| Fact | Identity | Properties |
|---|---|---|
| `Document` node | `doc:{repo}@{path}` | `path`, `format` (`markdown` or `text`), `title` (the first level-1 heading, else the file name), `sections`, `bytes` |
| `DocSection` node | `docsec:{repo}@{path}#{ordinal}` | `ordinal` (from 0, in file order), `level` (0 for text before the first heading and for a plain-text file), `heading`, `path` |
| `HAS_SECTION` edge | document to section | none |
| `MENTIONS` edge | section to the fact a reference resolves to | `reference` (the text as written), `rule` |

All four are Deterministic and Confirmed. A document cites the whole file;
a section cites its byte span, from its heading line to the next heading of
the same or a higher level; a `MENTIONS` edge cites the span of the
reference. The content hash of a section covers its bytes, so an edit
changes it. A `title` and a `heading` pass `core_redact::redact_text`
before they are stored. Section text is not stored.

Markdown structure is ATX headings (`#` to `######`) outside fenced code
blocks. Setext headings and HTML are not structure.

## Exact references

A reference is the text of an inline code span, or the target of an inline
link, outside fenced code blocks. One reference yields at most one link.
The rules, in order:

1. `fact_id`: the text equals the id of a fact.
2. `file_path`: the text, or a link target resolved against the document's
   directory, is a repository-relative path with a `File` fact in the same
   repository. A fragment and a line suffix (`#L10`, `:10`) are dropped
   before the comparison. Absolute URLs are not references.
3. `route`: the text is a method and a path (`GET /users/{id}`). It links
   when exactly one `Endpoint` of the system has that method and that path
   after parameter names are normalized (`{id}`, `:id` and `<id>` compare
   equal).
4. `name`: the text equals the `name` of exactly one fact among
   `Resource`, `Channel`, `DataEntity`, `Service` and `Workload`.

No link is made to a `Gap`, a `Drift`, a `Document` or a `DocSection`.

## Drift

| Kind | When | Cites |
|---|---|---|
| `doc-route-method-differs` | a `route` reference whose path is served, with no endpoint of its method | the reference span and the endpoints of that path |
| `doc-route-not-served` | a `route` reference whose path no endpoint of the system serves | the reference span; the entry states how many endpoints were compared |
| `doc-file-missing` | a relative link target or a code span with a path separator and a file extension that names no file of the repository at the commit | the reference span; the entry states the path looked for |

Each is a `Drift` node, Deterministic and InferredStrong, with
`kind: "documentation-drift"`, in `drift_register.md` beside contract drift.
One reference yields at most one entry. A route is checked only when the
system has at least one recovered endpoint: with none, the comparison says
nothing and no entry is made.

## Coverage

The coverage inventory of a repository gains `documents`: files found,
read, skipped by reason, sections, references linked by rule, and drift
entries by kind. The coverage document prints them. A count is of the
declared repository and commit, never a statement that the documentation
is complete.

## Surfaces

- **Graph and fact index.** Documents and sections are listed like any
  fact; a citation of one resolves.
- **`documentation-evidence.md`** (new compiled artifact). Per repository:
  each document with its sections, and under a section the facts it
  mentions with the rule and citation ids. Both export modes; it holds
  headings and ids, never section text.
- **`drift_register.md`.** The three kinds above.
- **Coverage document.** The counts.
- **Drafting.** The citation set of a section draft includes the
  `DocSection` facts that mention a fact in the set, so a
  `documented_intent` claim can cite the section it reports.

## Slices

Each slice is one PR with its acceptance criterion.

1. **Documents and sections** (AC-0676). The reader, the two node kinds
   and `HAS_SECTION`, the bounds, and determinism on a second ingest.
2. **Coverage** (AC-0677). Counts and reasons in the inventory and the
   coverage document.
3. **Exact references** (AC-0678). The four rules and `MENTIONS`.
4. **Drift** (AC-0679). The three kinds in the register, on the fixture
   with one route that exists and one that does not.
5. **Stated intent boundary and redaction** (AC-0680). No other fact
   changes; no section text in the graph or an export; headings redacted.
6. **Compiled artifact and drafting** (AC-0681). `documentation-evidence.md`
   and `DocSection` facts in a draft's citation set.

No slice uses a model.

## Open items

- reStructuredText and AsciiDoc are read as plain text.
- The two existing readers are not yet cases of this one (ADR-0065
  decision 6).
- External documentation systems are out of scope.
