# W17 sufficiency review: SimpleLogin handoff bundle (#1752)

This directory holds the review of one rebuild handoff bundle against the
sufficiency checklist of [SPEC-16](../../SPEC-16_rebuild-handoff-bundle.md).

## Status

No bundle has been reviewed. The definition of done of #1752 is met only
when a completed `review.md` is committed here, whatever its result.

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

For question 14 the reviewer picks five statements before opening the fact
index, writes them down, then follows each citation.

```markdown
# Sufficiency review

- Reviewer:
- Date:
- Bundle as-of: graph content hash, curated revision, catalogue version, export mode
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

## Question 14: the five statements

| Statement | Citation id | Resolves to a span that shows it? |
|-----------|-------------|-----------------------------------|
```

A bundle is sufficient when no answer is `silent`, all five statements of
question 14 resolve, and questions 15 and 16 are answered no.
