# Sufficiency review 2: SimpleLogin, deterministic baseline before drafting

- Reviewer: Claude (agent session for #1752, 2026-11-25). The reviewer wrote
  none of the code that compiled this bundle and accepted no draft. The
  reviewer did run the export: the unmodified showcase harness, which takes
  no choice that changes the content. **This review was not blind**: see
  "Limits of this review".
- Date: 2026-11-25
- Cartograph: `main` at `d8058a1` (0.80.0)
- Bundle as-of, copied from `bundle-manifest.md`:
  - Graph content hash: `b8df0a694b0c982254e6f929335b51eee4656efe2cb2e524f4c84e87a11c31e3`
  - Curated revision: `curated-revision-v4:f78c4a337d867068f65ffe96a1ab47e98ffe413b64b91571505f537f6f3ee0d4`
  - Catalogue version: 1.0
  - Export mode: `best-effort`
  - Source revisions: app `4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798`,
    browser-extension `d18b40fc774a3b477468bc608c3589a24f04c3ed`,
    Simple-Login-Android `ab6e7103c450f10a6065b1d27e4ed93b6c01b3cb`
- Result: **not sufficient**, on two counts: question 1 is silent, and one
  of the five statements of question 14 does not resolve to a span that
  shows it.
- Questions 1 to 13: answered 12 / stated unknown 0 / silent 1

This bundle still holds no drafted claim ("Drafted claims printed: 0"): the
W13 pilot's drafts are not accepted (#1748). It is therefore the second
baseline of what the deterministic tiers alone hand to a rebuilding team,
not the review after drafting that SPEC-16 asks for. That review is still
owed and is committed as `review-3.md`.

Against review 1 the deterministic bundle now passes everything it owns
except one citation: questions 4, 8 and 15, the held-out entry points and
the rebuild probe all pass.

## Checklist

| # | Answer | Where | Note |
|---|--------|-------|------|
| 1 | silent | `product-specification.md` §1.1, §1.7 | §1.1 reads "Not drafted", with a pointer to the C4 model in `architecture.md`, which shows who uses the system and what it depends on but not what it is for. §1.7 is an H blank, so "what is outside it" is stated unknown |
| 2 | answered | `product-specification.md` §1.2, `endpoint-permissions.md`, `security.md` | Six callers named by role (administrator, signed-in user, API client, API client in sudo mode, Twilio webhook, any caller), each with the guard that admits it and, under "What each guard checks", the rule in domain terms: an API client acts as the user of the API key in the `Authentication` header; a disabled account is refused with 403; an account with deletion requested with 401. "No `Actor` fact is in this export" |
| 3 | answered | `product-specification.md` §1.3, `user_stories.md` | 110 capabilities in 17 areas. Receiving mail over SMTP, scheduled jobs, the job runner, database notifications, the monitoring loop and operator commands are capabilities beside the HTTP ones |
| 4 | answered | `product-specification.md` §1.3, §1.5 | For each sampled operation §1.3 states what it takes, the success status with its JSON keys and the entities it inserts, updates or deletes (random alias: query `hostname`, `mode`, body `note`; 201 with the alias fields; inserts Alias and AliasUsedOn). §1.5 states the refusals with status and message and the changes with their conditions. §1.4 still lists test names only |
| 5 | answered | `product-specification.md` §1.5, `rule-evidence.md` | 2,250 rule rows, each cited; the Rule column is in domain terms and the Source column keeps the source condition as evidence. 252 rows (11%) read "Not stated in domain terms (Gap)" |
| 6 | answered | `product-specification.md` §1.6, `flow_dossiers.md` | All five sampled operations have a flow with the entities written. The steps are callable names in trace order, stated as "not an execution order" |
| 7 | answered | `data_model.md` | 80 entities with columns, keys, nullability and relations. Foreign-key column types are still blank |
| 8 | answered | `endpoint-inventory.md` | 426 endpoints; per endpoint the input names by location, the statuses with their JSON keys, and the rate limit. Authentication is in `endpoint-permissions.md` |
| 9 | answered | `external.md`, `topology.md`, `architecture.md` | Outside origins with call sites, and the C4 levels 1 and 2 |
| 10 | answered | `toolchain.md`, `configuration.md`, `pipelines.md`, `environments.md` | Configuration constants are stated with their defaults and environment variables beside the rules that use them (alias trash days 30, free plan limit 5) |
| 11 | answered | `test_arrangements.md`, `product-specification.md` §1.4 | 296 acceptance scenarios named per capability |
| 12 | answered | `gap_register.md`, `coverage.md`, `product-specification.md` §1.5 | 23,150 findings in 126 classes. The classes still describe the analysis, not the system. §1.5 is more useful to a reader: it names the 21 files where rule capture stopped and marks each rule it could not state |
| 13 | answered | every table; `fact-index.md` | Tier and confidence on every row; inferred rows say "matched by name, inferred" |
| 14 | four of five resolve | `fact-index.md` | Table below |
| 15 | no | `product-specification.md` §1.2, §1.3, §1.5 | Rules read without knowing Python: "When the full alias contains “..”, the request is refused with status 400." The wording is rough where it follows an identifier ("a user can create num aliases", "the “convert to ID” result"), and flows name callables |
| 16 | no | whole bundle | No target code, no instruction to write to the source |
| 17 | 8 of 8 | Table below | |
| 18 | 26 of 26 stated | Table below | Passes the frozen bar |

## Question 14: the five statements

Chosen from D1 before the fact index was opened.

| Statement | Citation id | Resolves to a span that shows it? |
|-----------|-------------|-----------------------------------|
| §1.5: a random alias mode other than word or uuid is refused with 400 | F-7a221039af130a46 | Yes: `app/api/views/new_random_alias.py` bytes 3695..3759 is `return jsonify(error=f"{mode} must be either word or uuid"), 400` |
| §1.5: an address that is an alias, a deleted alias or a domain-deleted alias is refused with 409 | F-594556cc65a3b6a4 | Yes: `app/api/views/new_custom_alias.py` bytes 3626..3689 is `return jsonify(error=f"alias {full_alias} already exists"), 409` |
| §1.5: an alias name longer than 128 characters is refused with 400 | F-f46dc656dd67b3d4 | Yes: `app/api/views/alias.py` bytes 9073..9142 is `return jsonify(error="Name can't be longer than 128 characters"), 400` |
| §1.2: a disabled account is refused with 403 | F-a29d7dc45e7b3aee | Yes: `app/api/base.py` bytes 1056..1101 is `return jsonify(error="Disabled account"), 403` |
| §1.5: toggling an alias accepts at most 100 requests an hour | F-16e81d56bfb60ea9 | **No.** The row cites the Endpoint fact, whose spans are the route declaration (`app/api/views/alias.py` bytes 4533..4598), the blueprint and its registration. The limit is declared two lines below the route, outside every cited span. The bundle does hold the right span, under the derived `RateLimits` fact that `endpoint-inventory.md` cites for the same limit; D1 does not cite it |

## Question 17: entry points

The reviewer's list from the bundle (made after `entry-points.md` had
already been read, so not blind): HTTP requests (426 endpoints and "Serve HTTP requests
(container image)"), mail received over SMTP, scheduled jobs (two crontab
files), the job runner loop, database notifications, the monitoring loop,
operator commands (two sets) and the operator shell.

| Entry point | In a bundle document? | In the gap register? |
|-------------|-----------------------|----------------------|
| E1 Web and API server | Yes: §1.3 "Serve HTTP requests (container image)", "Run server"; `endpoint-inventory.md` | n/a |
| E2 Mail handler | Yes: §1.3 and §1.5 "Receive mail over SMTP", with the SMTP replies as rules | n/a |
| E3 Scheduled jobs | Yes: "Run scheduled jobs (crontab)" and "(crontab all hosts)" | n/a |
| E4 Job runner | Yes: "Run the job runner loop" | n/a |
| E5 Event listener | Yes: "Handle database notifications" | n/a |
| E6 Monitoring loop | Yes: "Run the monitoring loop" | n/a |
| E7 Operator commands | Yes: "Run operator commands (commands)" | n/a |
| E8 Operator shell | Yes: "Open the operator shell" | n/a |

## Question 18: rebuild probe

| Section | Items | Stated | Partly | Absent | Absent with no gap row |
|---------|-------|--------|--------|--------|------------------------|
| 1.2 Actors and roles | 4 | 4 | 0 | 0 | 0 |
| 1.3 Capabilities | 6 | 6 | 0 | 0 | 0 |
| 1.5 Business rules | 16 | 16 | 0 | 0 | 0 |
| Total | 26 | 26 | 0 | 0 | 0 |

Against the frozen pass values: capabilities 6 of 6 (pass: all), actor and
rule items 20 of 20 (pass: 14), absent with no gap row 0 (pass: 0).

Three items are stated across several rows rather than in one sentence. A4
(only the free plan has a quota) is the pair "a user can create num aliases
when the check ‘lifetime or active subscription’ holds" and the free-limit
row. R6 states the 412 and, in the row below it, that the signed suffix
must be at most 600 seconds old. R10 states the trash period (the
configured alias trash days, 30 by default) and the immediate deletion in
separate rows.

## Findings

1. **§1.1 is the only section the deterministic pipeline cannot fill.**
   Nothing in the bundle says what SimpleLogin is for. It needs one accepted
   W13 draft (#1748).
2. **A rate-limit rule in D1 cites a fact whose spans do not show the
   limit.** D1 cites the Endpoint so that a draft can cite the row
   (AC-1608); the Endpoint's evidence is its route. The limit declaration's
   span is in the bundle under the derived limits fact, which only
   `endpoint-inventory.md` cites. Either the Endpoint's evidence should
   carry the limit declaration, or the D1 row should cite both.
3. **The gap register still speaks about the analysis.** Its 126 classes
   name stop reasons of the extractors. A rebuilding team learns more about
   what is missing from §1.5's "Not stated in domain terms (Gap)" rows and
   its list of files where rule capture stopped.
4. **§1.4 names tests and restates no assertion.** Question 4 is answered
   from §1.3 and §1.5; the acceptance criteria section itself adds only
   which scenarios exist.

## Limits of this review

- **Not blind.** The README asks that the reviewer receive the export and
  nothing else, and open the held-out lists only after the rest is
  answered. Here the reviewer ran the export, so had the source checkout
  and this repository, and read `entry-points.md` and
  `../w13/expected.json` before finishing questions 1 to 16. The source was
  opened only to check the five spans of question 14. Questions 17 and 18
  were therefore scored knowing what to look for: each expected statement
  was searched for in the bundle and its row read. The counts are what the
  bundle states; they are not evidence that a reader who did not know the
  expected set would have found each one.
- A blind review by someone given only the export is still owed for the
  bundle made after drafting (`review-3.md`).
- The reviewer is an agent, not an engineer about to rebuild the system.
- The sample for questions 4 and 18 is the alias lifecycle; areas outside
  it were read for the checklist but not probed against an expected set.
