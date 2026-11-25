# SPEC-16 — Rebuild handoff bundle

- **Status:** Proposed for [#1752](https://github.com/true-line/cartograph/issues/1752). This is a definition. It changes no shipped behaviour, adds no export file and does not change the work packet execution state `Unoffered`. Review 1 of the pilot (a bundle with no drafted section) is recorded and found the bundle not sufficient; the review after drafting is not done.
- **Date:** 2026-11-17, revised 2026-11-20 after review 1 (sufficiency floor, entry points, probe)
- **Scope:** process, no user story; ROADMAP W17; v1.0.0 criterion 9; VISION V4 and V6.
- **Decision:** [ADR-0036](adr/ADR-0036-lifecycle-reconstruction-direction.md) decision 6 (the work packet is the handoff unit), [ADR-0056](adr/ADR-0056-documentation-reconstruction-pipeline.md) (NG2).
- **Builds on:** `docs/DOCUMENT-CATALOGUE.md` version 1.0 (the document set), [SPEC-15](SPEC-15_authored-sections.md) (authored sections), [SPEC-12](SPEC-12_curated-projection.md) (curated revision), [SPEC-11](SPEC-11_local-work-packets.md) (work packets), ADR-0058 (citation ids).

## Outcome and boundary

An engineering team that is told "rebuild this system", in another language
or to changed requirements, receives one bundle. From the bundle alone they
can say what the system does, for whom, under which rules, with which data
and integrations, and what is not known. This specification says what the
bundle holds and how its sufficiency is reviewed.

Cartograph enables that work and does not perform it:

- **NG1: no writes to the ingested target.** The bundle is read. Nothing in
  it is applied to a repository by Cartograph. Implementation happens in a
  checkout the team authorizes separately and comes back through re-ingest.
- **NG2: no target code generation.** The bundle holds no generated code, no
  code skeleton, no migration script and no patch for the new system. A work
  packet states intent, evidence and acceptance; it is not an implementation.

A bundle is not a claim of completeness. A bundle with a long gap register
is a correct bundle. A bundle that hides a gap is not.

Correct is not sufficient. A bundle can cite every statement, hide nothing
and still carry too little to rebuild from: every question answered with a
list, or every question answered "not known". The checklist below therefore
has a floor that honesty alone does not meet.

## Terms

- **Bundle.** One export of one project at one as-of, in one export mode,
  with the parts below.
- **As-of.** The graph content hash, the curated revision, the catalogue
  version, the export mode and the source revisions (the export's
  `graph_basis`). The export is deterministic for a given as-of.
- **Statement.** One sentence, row or cell of a bundle document that asserts
  something about the system.

## What the bundle holds

| Part | Content | Today |
|---|---|---|
| 1. Documents | The catalogue's D1 to D10, arranged by section, each section marked P, A, P+A or H | D1 is compiled as `product-specification.md`; its §1.1 links the system at a glance, drawn in `architecture.md` (C4 levels 1 to 3) and `architecture.dsl` (#2049). D2 to D10 are not compiled yet; their content is exported under the current artifact names, mapped in catalogue §3 |
| 2. Requirements | D1 §1.3 capabilities and §1.4 acceptance criteria: each criterion as Given/When/Then, each clause cited | The frames exist. The prose is drafted by `section-writer@1` and printed once accepted (SPEC-15) |
| 3. Rules | D1 §1.5, with `rule-evidence.md` as the evidence | Exported |
| 4. Traceability matrix | D11 §11.1, `US-TM.md` and `US-TM.json` | Exported |
| 5. Gap register | D11 §11.2, `gap_register.md` and `gap_register.json`, plus each document's known gaps and `coverage.md` for what was read and skipped | Exported |
| 6. Fact index | D11 §11.7, `fact-index.md` and `fact-index.json`: one row per cited fact with its tier, confidence, method and source span | Exported; export fails if a cited id has no row |
| 7. Work packets | Zero or more SPEC-11 packets, each exported as its own file beside the bundle | Generated locally on request. Execution state is `Unoffered`: a packet in a bundle is a proposal and grants no authority |
| 8. As-of | The values above, and the list of files by part | `bundle-manifest.md` states all five values and lists every exported file under its part (AC-0682). The D1 header and `curated.md` repeat the hash and the curated revision |

The bundle is what the existing export writes, read under this definition.
`bundle-manifest.md` lists the parts and the as-of in one place. It is a
listing: it reads no fact, asserts nothing about the system and cites
nothing, and a value the host did not supply is stated as not supplied.
Work packets are not in it, because the export does not write them.

## Rules for every statement

1. **Tier and confidence are shown.** Every statement carries its tier
   (T0 to T3) and confidence, in words (R-INT-2). A drafted claim is T3 /
   InferredWeak before and after review and names the role that wrote it
   (R-INT-3).
2. **Every statement cites.** A statement names at least one citation id
   that has a row in the fact index. A statement with no admitted fact
   behind it is not written; the absence is a gap.
3. **Language-neutral statement, source as evidence.** A requirement or a
   rule states behaviour in domain terms: who can do what, under which
   condition, with which result. It does not say how the source implements
   it. This holds for a projected frame as much as for drafted prose: a
   condition or an outcome copied from the source as an expression is
   evidence, and the requirement is the sentence that says what it means.
   Until a section has that sentence, its frame is evidence without a
   requirement. Identifiers of the source implementation (file paths, function and
   class names, framework terms, decorator or annotation names) appear only
   in the citation and the fact index, never as the subject of the
   requirement. HTTP methods, paths, status codes, field names and message
   names are part of the observable contract and may be stated.
4. **Unknown is stated.** What could not be recovered is a row in the gap
   register with its class. H sections are printed as labelled blanks and
   are never filled by a model.
5. **Mode is honoured.** A `verified-only` bundle prints no InferredWeak
   statement and says how many it left out of each section. A `best-effort`
   bundle prints them, annotated (R-INT-5).
6. **Rationale is not invented.** Why a decision was made is written only
   where a target document states it (catalogue default 2).

Rule 3 has one deterministic check (AC-0683). When a drafting run ends, a
draft is not staged if any statement spells out, as a whole token, an
identifier of a fact that statement cites: a path of the fact's evidence,
the file name of that path, or the name of a cited code declaration when
the name is shaped like code (it holds `_`, `::`, `.`, `#` or `(`, or a
lower-case letter directly followed by an upper-case one). The run reports
`statement_names_source` and nothing is stored. A documentation file a
claim reports on is not an implementation identifier.

The check decides only what a rule can decide. A single plain word is not
told apart from a domain term, framework terms are not listed, and an
identifier of a fact the statement does not cite is not compared. Those
remain for the reviewer, under question 15.

## Sufficiency checklist

A reviewer answers each question from the bundle alone, without the source
repository and without asking anyone. Each answer is one of:

- **answered**: the bundle states it, with citations that resolve;
- **stated unknown**: the bundle says it is not known and why: a gap
  register row, an H blank, or a document's own sentence naming what it
  does not cover;
- **silent**: the bundle neither answers nor says it is unknown. A section
  that reads "Not drafted" is silent: nothing was tried, so nothing is
  known to be unrecoverable.

An answer counts as **answered** only if the reviewer could act on it
without reading the source. A list of names copied from the code, or a
condition printed as a source expression, is not an answer to a question
about behaviour; the reviewer records it as silent and says what was there.

| # | Question | Where the answer should be |
|---|---|---|
| 1 | What is the system for, and what is outside it? | D1 §1.1, §1.7 |
| 2 | Who uses it, in which roles, and what may each role do? | D1 §1.2, D6 §6.3 |
| 3 | What can each actor do? List the capabilities. | D1 §1.3 |
| 4 | For each capability in the sample: what must be true for it to succeed, what does success return and change, and what happens when it is refused? | D1 §1.4, §1.5 |
| 5 | Which business rules apply, and what evidence supports each? | D1 §1.5, D11 §11.4 |
| 6 | Which steps make up each main flow, and across which components? | D1 §1.6, D9, D11 §11.5, `architecture.md` level 3 |
| 7 | Which data does it hold, with which fields and relations? | D5 |
| 8 | Which interfaces does it expose, with their requests and responses? | D4 |
| 9 | Which external systems, queues and stores does it depend on? | D7 §7.6, D2, `architecture.md` levels 1 and 2 |
| 10 | How is it configured, built and deployed, in which environments? | D7 |
| 11 | Which tests exist, and what do they cover? | D8 |
| 12 | What is not known, and what would close each gap? | D11 §11.2, §11.6 |
| 13 | Which statements are verified facts and which are an agent's reading? | Tier annotations, D11 §11.7 |
| 14 | For five statements chosen by the reviewer: does each citation resolve to a source span that shows it? | D11 §11.7 |
| 15 | Does any requirement or rule depend on knowing the source language or framework to be understood? | Rule 3 |
| 16 | Does the bundle contain code for the new system, or an instruction to write to the source? | NG1, NG2: it must not |
| 17 | Through which entry points does work reach the system (requests, scheduled jobs, messages and mail received, queue and job workers, commands, webhooks), and is each entry point of the held-out list either in a bundle document or in the gap register? | D1 §1.3, D2, D7 §7.4, D11 §11.2, §11.6, scored against the held-out list |
| 18 | Of the expected statements written from the source for the sample, how many does the bundle state? | The rebuild probe below |

A bundle is **sufficient** when all of these hold:

1. No question is silent.
2. Questions 3, 4 and 5 are answered, not stated unknown, for the sample
   capability set. A bundle that says "not known" about what the sample
   does and under which rules is honest and is not a handoff.
3. Question 14 resolves for all five statements.
4. Questions 15 and 16 are answered no.
5. Every entry point of the held-out list is in a bundle document or in
   the gap register.
6. The rebuild probe meets its pass values.

Elsewhere "stated unknown" does not fail a bundle; it is the bundle doing
its job. The review records how many answers were stated unknown, so a
reader sees how much of the system the bundle actually carries.

Question 17 cannot be answered from the bundle alone: an entry point the
bundle leaves out of every document and of the gap register is not there
to be listed. It is scored against a held-out list of entry points,
written by hand from the source before the run and kept from the reviewer
until the rest of the checklist is answered, as the expected set of the
probe is. The reviewer first lists the entry points the bundle shows,
then opens the list and marks each of its rows. The pilot's list is
[`docs/acceptance/w17/entry-points.md`](acceptance/w17/entry-points.md).

### Rebuild probe

The checklist asks whether the bundle speaks. The probe asks whether what
it says is enough. It uses an expected set: statements about the sample
capability set, written by hand from the source before the run and kept out
of every model input. The W13 set (`docs/acceptance/w13/expected.json`, 26
statements on the alias lifecycle) is the pilot's.

The reviewer answers the checklist first, then opens the expected set and
marks each item, from the bundle alone:

- **stated**: the bundle says it in domain terms, with a citation;
- **partly**: the bundle gives some of it, or gives it only as a source
  expression;
- **absent**: the bundle does not say it. The reviewer notes whether a gap
  row accounts for it.

| Measure | Pass |
|---|---|
| Expected capabilities (1.3) stated | all |
| Expected actor and rule items (1.2, 1.5) stated | at least 14 of 20 |
| Absent items with no gap row | 0 |

The pass values are those of the W13 pilot. The owner froze them for this
probe on 2026-11-20; changing one is the owner's decision. The counts are
recorded whatever they are. An absent
item with no gap row is the failure this specification exists to prevent:
the bundle is silent and looks complete.

## Pilot

One bundle is produced for SimpleLogin at the commit pinned in
`bench/repos.toml` and reviewed against the checklist by someone who did not
produce it. The record form is in
[`docs/acceptance/w17/README.md`](acceptance/w17/README.md). The result is
committed whatever it is.

The pilot uses a `best-effort` export after the W13 pilot's drafts are
reviewed (SPEC-15, `docs/acceptance/w13/`), so that D1 carries authored
sections. Questions 4 and 18 are asked of the alias lifecycle capability
set.

**Review 1** ([`review.md`](acceptance/w17/review.md), 2026-11-20) took a
`best-effort` export with no drafted section, the state of `main` before
any draft is accepted. Result: not sufficient. Questions 1, 4 and 8 were
silent and question 15 was yes. The probe found 1 of 26 expected statements
stated, 14 partly (5 of them complete but printed as a source expression)
and 11 absent, none of the absent in a gap row. Two of eight entry points
are in the bundle. Mail forwarding, the system's main job, is in no
capability and no gap row. Questions 17 and 18, the floor on
questions 3 to 5 and the reading of "Not drafted" as silent were added
because of that review.

**Review 2** ([`review-2.md`](acceptance/w17/review-2.md), 2026-11-25) took
the same kind of export from `main` at `d8058a1`, still with no drafted
section. Result: not sufficient. Question 1 is silent, because §1.1 is not
drafted, and one of the five sampled citations (a rate limit in D1) resolves
to a span that does not show the limit. Questions 4, 8 and 15 now pass,
all eight held-out entry points are in the bundle, and the probe finds 26
of 26 expected statements stated. The review after drafting is still owed.

## Open items

- Capabilities are derived from HTTP controllers. Work that enters another
  way (mail received, a job or queue worker, an event listener, a command)
  is under no capability, and nothing records the omission as a gap
  (review 1, finding 2). Scheduled jobs are recovered as flows and are
  under no capability either. Tracked in #1997.
- A recovered rule is a rejection exit. What a successful operation
  returns and changes, limits set by a decorator or by configuration, and
  defaults are not recovered as rules and have no gap row (review 1,
  finding 4). Tracked in #1998.
- The frames of D1 §1.2, §1.3 and §1.5 print guard names, controller names
  and source expressions. Rule 3 is met only where accepted prose restates
  them. Tracked in #1999.
- Request inputs and response shapes are recovered and are in no exported
  document, so question 8 is silent on them. Tracked in #1998.

- D2 to D10 are not compiled as catalogue documents. Until they are, a
  reviewer finds their content under the current artifact names, and
  questions 7 to 11 are answered from those files.
- Work packets are exported one at a time; nothing collects the packets
  that belong to one bundle.
- The rule 3 check covers paths and code-shaped symbol names of cited
  facts only. Plain-word symbol names and framework terms are left to
  review.
- The D1 header does not yet carry the system name or source revisions
  (SPEC-15 open items); the complete as-of is in `bundle-manifest.md`.
