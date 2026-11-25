# W17 sufficiency review: SimpleLogin handoff bundle (#1752)

This directory holds the review of one rebuild handoff bundle against the
sufficiency checklist of [SPEC-16](../../SPEC-16_rebuild-handoff-bundle.md).

## Status

[`review.md`](review.md) is review 1 (2026-11-20): a `best-effort` export
with no drafted section, found **not sufficient**. It skipped step 2 below
because no draft has been accepted.

[`review-2.md`](review-2.md) is review 2 (2026-11-25), of the same kind of
export from `main` at `d8058a1`: still no drafted section, so again a
baseline of the deterministic tiers, and **not sufficient**. Only question
1 is silent (§1.1 is not drafted), and one of five sampled citations does
not show its statement. The entry points (8 of 8) and the rebuild probe
(26 of 26 stated) pass.

The review of a bundle made after drafting is still owed, and is committed
here as `review-3.md`, whatever its result.

## Producing the bundle

1. Ingest SimpleLogin at `4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798`, as the
   showcase does (`docs/showcases/simplelogin.md`).
2. Complete the W13 pilot run (`docs/acceptance/w13/README.md`) so that D1
   carries reviewed drafts.
3. Export in best-effort mode. Optionally generate one work packet for a
   change in the alias lifecycle and export it beside the bundle.
4. Give the reviewer the export directory and nothing else: no source
   checkout, no access to the app.

## Reviewing

The reviewer is someone who did not produce the bundle or accept its
drafts. They copy the table below into `review.md` and fill it in.

- **Answer**: `answered`, `stated unknown` or `silent`.
- **Where**: the file and section that answers, or that states the unknown.
- **Note**: for `silent`, what was looked for and not found.

An answer the reviewer could not act on without reading the source (a list
of code names, a condition printed as a source expression) is recorded as
`silent`, with what was there. A section that reads "Not drafted" is
`silent`.

For question 14 the reviewer picks five statements before opening the fact
index, writes them down, then follows each citation.

For question 17 the reviewer first lists every entry point the bundle
shows (requests, scheduled jobs, messages and mail received, queue and job
workers, commands, webhooks). Only after the rest is answered, the reviewer
opens the held-out list [`entry-points.md`](entry-points.md), written from
the source, and marks for each of its rows whether a bundle document or a
gap row covers it. The list is given to the reviewer with the expected set
of question 18, not with the bundle.

Question 18 is the rebuild probe. Only after the rest is answered, the
reviewer opens `../w13/expected.json` and marks each item `stated`,
`partly` or `absent` from the bundle alone, and for each absent item
whether a gap row accounts for it.

```markdown
# Sufficiency review

- Reviewer:
- Date:
- Bundle as-of, copied from `bundle-manifest.md`: graph content hash, curated revision, catalogue version, export mode, source revisions
- Result: sufficient | not sufficient
- Answered / stated unknown / silent: N / N / N

| # | Answer | Where | Note |
|---|--------|-------|------|
| 1 | | | |
| 2 | | | |
| 3 | | | |
| 4 | | | |
| 5 | | | |
| 6 | | | |
| 7 | | | |
| 8 | | | |
| 9 | | | |
| 10 | | | |
| 11 | | | |
| 12 | | | |
| 13 | | | |
| 14 | | | |
| 15 | | | |
| 16 | | | |
| 17 | | | |
| 18 | | | |

## Question 14: the five statements

| Statement | Citation id | Resolves to a span that shows it? |
|-----------|-------------|-----------------------------------|

## Question 17: entry points

| Entry point | In a bundle document? | In the gap register? |
|-------------|-----------------------|----------------------|

## Question 18: rebuild probe

| Section | Items | Stated | Partly | Absent | Absent with no gap row |
|---------|-------|--------|--------|--------|------------------------|
```

`stated` means in domain terms with a citation. A rule given only as a
source expression is `partly`.

A bundle is sufficient when no answer is `silent`; questions 3, 4 and 5 are
`answered` for the sample; all five statements of question 14 resolve;
questions 15 and 16 are answered no; every entry point of the held-out list
is in a bundle document or in the gap register; and the probe meets the
pass values of SPEC-16.
