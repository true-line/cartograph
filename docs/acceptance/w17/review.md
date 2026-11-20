# Sufficiency review 1: SimpleLogin, before drafting

- Reviewer: Claude (agent session for #1752). The reviewer wrote none of the
  code that compiled this bundle and accepted no draft. The reviewer did run
  the export: the unmodified showcase harness, which takes no choice that
  changes the content. See "Limits of this review".
- Date: 2026-11-20
- Cartograph: `main` at `bc4d29b` (0.71.0 plus #1986 to #1993)
- Bundle as-of, copied from `bundle-manifest.md`:
  - Graph content hash: `109bc63568846399c0b3bc38a99675b45cb5db6c1d58b002a5376208f2d28714`
  - Curated revision: `curated-revision-v3:13e434b213ffce61af17f47151f9e15c19ce5cce2eac3f355470b04e103ce32a`
  - Catalogue version: 1.0
  - Export mode: `best-effort`
  - Source revisions: app `4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798`,
    browser-extension `d18b40fc774a3b477468bc608c3589a24f04c3ed`,
    Simple-Login-Android `ab6e7103c450f10a6065b1d27e4ed93b6c01b3cb`
- Result: **not sufficient**
- Questions 1 to 13: answered 10 / stated unknown 0 / silent 3

This bundle holds no drafted claim ("Drafted claims printed: 0"). The pilot
in SPEC-16 asks for an export made after the W13 drafts are reviewed; no
draft has been accepted, so this is the baseline: what the deterministic
tiers alone hand to a rebuilding team. A second review follows once D1
carries accepted sections.

The checklist is the one SPEC-16 held before this review (questions 1 to
16). The answers use its definitions literally: `stated unknown` means a
gap register row or an H blank.

## Checklist

| # | Answer | Where | Note |
|---|--------|-------|------|
| 1 | silent | `product-specification.md` §1.1, §1.7 | §1.1 reads "Not drafted". That is neither an answer nor a gap. Nothing in the bundle says what SimpleLogin is for. §1.7 is an H blank, so "what is outside it" is stated unknown |
| 2 | answered | `product-specification.md` §1.2, `endpoint-permissions.md` | Seven callers, each named by the guard that admits it (`require_api_auth`, `BaseAdminView.is_accessible`). "No `Actor` fact is in this export." Roles in the business sense (free user, paying user, administrator) are not stated |
| 3 | answered | `product-specification.md` §1.3, `user_stories.md` | 96 capabilities, one per HTTP controller. See finding 2: receiving and forwarding mail is not among them and is not a gap either |
| 4 | silent | `product-specification.md` §1.4, §1.5 | Refusals are answered: 2, 8 and 18 rejection exits for random alias, custom alias v2 and alias operations. What success requires and returns is not stated anywhere: §1.4 lists test names and says "the test's assertions are not restated here" |
| 5 | answered | `product-specification.md` §1.5, `rule-evidence.md` | 155 rejection exits, each cited. Each is a condition and an outcome copied from the source, not a rule in domain terms |
| 6 | answered | `product-specification.md` §1.6, `flow_dossiers.md` | Traces of callables, stated as "not an execution order". Of the five sampled operations only update has a flow; delete reads "nothing further traced" and toggle and both creations have no line |
| 7 | answered | `data_model.md` | 80 entities with columns, keys, nullability and relations. Foreign-key and timestamp column types are blank |
| 8 | silent | `endpoint-inventory.md`, `test_arrangements.md` | 426 endpoints with method and path. The inventory says it "states no parameters, response schemas or authentication". Request inputs are recovered (`endpoints.json` of the showcase) and are in no exported document |
| 9 | answered | `external.md`, `topology.md` | Five outside origins, 21 unclassified targets, six resources |
| 10 | answered | `toolchain.md`, `configuration.md`, `pipelines.md`, `environments.md` | Environments: 0, with one environment file listed as not established |
| 11 | answered | `test_arrangements.md` | 1,047 tests, 304 references to 87 of 426 endpoints, with asserted statuses |
| 12 | answered | `gap_register.md`, `coverage.md` | 14,521 findings in 50 classes. The classes describe the analysis ("a value prerequisite lacks a stable scope-proven declaration"), not the system. A reader cannot tell from them which behaviour is missing |
| 13 | answered | every table; `fact-index.md` | Tier and confidence on every row |
| 14 | all five resolve | `fact-index.md` | Table below |
| 15 | **yes** | `product-specification.md` §1.2, §1.3, §1.5 | Rules are Python expressions (`not user.can_create_new_alias()` returns `jsonify(error=…), 400`). Actors are decorator names. One capability is named by a file path, "Manage dashboard (app/dashboard/views/index.py)" |
| 16 | no | whole bundle | No target code, no instruction to write to the source |

Not sufficient on three counts: questions 1, 4 and 8 are silent, and
question 15 is yes.

## Question 14: the five statements

Chosen from D1 before the fact index was opened.

| Statement | Citation id | Resolves to a span that shows it? |
|-----------|-------------|-----------------------------------|
| §1.5: a random alias mode other than word or uuid returns 400 | F-a7b5c24b1ff221df | Yes: `app/api/views/new_random_alias.py` bytes 3695..3759 is the `return … 400`. The span shows the outcome; the condition is in `rule-evidence.md` |
| §1.5: an alias name longer than 128 characters returns 400 | F-97b868db8bc03b1e | Yes: `app/api/views/alias.py` bytes 9073..9142 |
| §1.5: an address that exists or was deleted returns 409 | F-c2a6b8a81329b1d7 | Yes: `app/api/views/new_custom_alias.py` bytes 9534..9597 |
| §1.3: capability "Manage new random alias (API)" serves POST /api/alias/random/new | F-e6766f8201160f9c | Yes: bytes 578..630 is the route declaration |
| §1.4: scenario "out of quota" requests POST /api/alias/random/new | F-dbe8123f31c73370 | Yes: `tests/api/test_new_random_alias.py` bytes 3034..3124 is the request. The statement is only that the test issues the request; what the test establishes is not stated |

## Findings

1. **The checklist passes a hollow answer.** Questions 2, 3, 5 and 6 are
   `answered` here, and none of the answers would let a team that cannot
   read Python rebuild the behaviour. The three-way answer measures whether
   the bundle speaks, not whether what it says is enough. Only question 15
   caught it.
2. **The system's main job is missing, and the bundle does not say so.**
   SimpleLogin receives mail for an alias and forwards it to a mailbox.
   The word "forward" does not occur in D1. Capabilities are derived from
   HTTP controllers only, so the mail handler is no capability, and no gap
   row says a non-HTTP entry point was left out. `flow_dossiers.md` names
   `email_handler` only inside the names of test files. Question 3 is
   still `answered`.
3. **"Not drafted" is a fourth answer the definition does not have.** It is
   not an answer and not a gap row. Read loosely it counts as stated
   unknown, and then a bundle exported before any drafting passes
   questions 1 and 4.
4. **Rules cover refusals only.** A rule is a rejection exit. What a
   successful request changes, limits set by a decorator or by
   configuration, and defaults are not recovered as rules, and no gap row
   says so. The probe below measures it.
5. **Rule 3 binds drafted prose only.** The projected frames of §1.2, §1.3
   and §1.5 print source expressions and symbol names as the statement.
6. **Request and response shapes are recovered and not exported.**

## Rebuild probe

After the checklist was answered, the bundle was compared with the expected
set of the W13 pilot (`../w13/expected.json`): 26 statements about the
alias lifecycle, written by hand from the source before any run. An item is
**stated** when the bundle says it, in any form; **partly** when it gives
some of it; **absent** otherwise.

| Section | Items | Stated | Partly | Absent |
|---|---|---|---|---|
| 1.2 Actors and roles | 4 | 0 | 0 | 4 |
| 1.3 Capabilities | 6 | 1 | 4 | 1 |
| 1.5 Business rules | 16 | 5 | 5 | 6 |
| Total | 26 | 6 | 9 | 11 |

- Stated: C4, R1, R8, R9, R12, R15. All but C4 are stated as a Python
  condition.
- Partly: C1, C2, C3 and C5 (the operation, not its inputs or result); R2
  (refused with 400, the limit is not given); R6 and R7 (the refusals, not
  the 600 seconds or the domain rule); R13 (128 characters, not the removed
  line breaks); R16 (update writes the audit log, toggle has no flow).
- Absent: A1 to A4 (how a caller is identified, disabled and deleted
  accounts, plans); C6; R3 (no limit for paying users); R4 and R5 (rate
  limits); R10 and R11 (what deleting an alias does); R14.

A team rebuilding the alias lifecycle from this bundle would get the error
responses right and would not learn how a caller authenticates, what the
plans are, that creation is rate limited, or that a deleted alias goes to a
trash for 30 days.

The unaccepted composer drafts of 2026-11-20 (`section-composer@1`, §1.3
and §1.5, not part of this bundle) restate the stated and partly stated
rules in domain terms. They add none of the absent items, because the
facts behind those items are not in the graph the composer is handed.

## Limits of this review

- The reviewer ran the export. The harness is deterministic and was not
  changed, but a reviewer who receives only the export directory is the
  stronger reading of "did not produce it".
- The reviewer is an agent that knows what SimpleLogin is from outside the
  bundle. Finding 2 rests on that knowledge. A reviewer without it would
  have missed the mail path, which is why the checklist now asks.
- Question 4 and the probe cover one capability set. The other 91
  capabilities were read in outline only.

## Reproduce

Clone the three repositories at the revisions above into one directory with
the system manifest of `docs/showcases/simplelogin.md`, then from
`src-tauri/`:

```sh
CARTOGRAPH_SHOWCASE_MANIFEST=$SRC/cartograph.system.toml \
CARTOGRAPH_SHOWCASE_OUTPUT=$OUT \
  cargo test showcase_recover_and_export -- --ignored
```

The bundle is `$OUT/best-effort/`. The graph content hash above is for
local sources, whose registry keys are random per installation (ADR-0023);
another run prints another hash for the same content.
