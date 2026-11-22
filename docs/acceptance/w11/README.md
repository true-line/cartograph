# W11 content acceptance: create an alias from the extension (#2169)

This is the pre-run expected set for the W11 journey gate (ADR-0050
decision 6). [`expected.json`](expected.json) was written by hand from
SimpleLogin's public help only, before any scored run and without reading
Cartograph output:

- **H1**: `docs/api.md` in [simple-login/app](https://github.com/simple-login/app),
  the public API documentation. It was read at `master`, because the pinned
  commit page could not be fetched.
- **H2**: `DEVELOPMENT.md` in
  [simple-login/browser-extension](https://github.com/simple-login/browser-extension),
  which names the extension's screens.

The scored run ingests the revisions the SimpleLogin showcase pins
([`docs/showcases/simplelogin.md`](../../showcases/simplelogin.md)): the app at
`4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798` and the browser extension at
`d18b40fc774a3b477468bc608c3589a24f04c3ed`. A run at any other revision is
not scored against this set.

Keep this directory outside every model input. Recovery never reads it.

## Status

The expected set and the pass column below were frozen by the owner's
one-line go on 2026-11-22. Any later change to a step, a label or a pass
value needs a new go and is recorded here with the reason. The first
scored run is recorded under Result below.

## Expected journey

| Step | Kind | Expectation | Label | Required |
|---|---|---|---|---|
| J1 | screen | API key setup: entered by the user, or the web onboarding pages | documented intent | yes |
| J1a | client call | `GET /api/user_info` validates the key | inferred | no |
| J2 | screen | Main screen: recommendation, alias creation, existing alias | documented intent | yes |
| J3 | client call | `GET /api/v5/alias/options?hostname=` | documented intent | yes |
| J4 | client call | `GET /api/v2/mailboxes` for `mailbox_ids` | inferred | no |
| J5 | client call | `POST /api/v3/alias/custom/new` or `/api/v2/alias/custom/new` with prefix, signed suffix, mailboxes | inferred | yes |
| J5a | client call | `POST /api/alias/random/new`, the alternate of J5 | inferred | no |
| J6 | screen | New alias screen, where the user copies the alias | documented intent | yes |

Labels follow ADR-0036 decision 2. Nothing is labelled `implemented`,
because public help cannot show what the code does. The run decides that.
An `inferred` step says what public help implies but does not state. The
`unknown` list in `expected.json` names what public help leaves open.

The required order is J1 → J2 → J3 → J5 → J6.

## Scoring

Score the `journeys` artifact of one exported showcase run at the pinned
commit. Each candidate step is matched to an expected step by screen or by
method and path, and the candidate step's own tier is recorded beside it.
J5 matches either custom-alias version in `endpoint_any`, because public
help does not say which one the pinned extension calls. The score records
the version found.

| Measure | Definition | Pass |
|---|---|---|
| Required steps | required steps present in one candidate journey | 5 / 5 |
| Order | required steps in the order above | yes |
| Optional steps | J1a, J4, J5a found, reported by name | reported, not scored |
| Extra steps | candidate steps that match no expected step, reported by name | reported, not scored |
| Alternate path | J5a never shown as the implemented path when J5 is | yes |
| Endpoints | every matched client call links to the server endpoint of the same method and path | 100 % |

A missing required step makes the result partial. It is reported with the
step it misses, never silently dropped. A step public help states but the
run cannot find stays a named miss, not proof the code lacks it.

## Result: first scored run (2026-11-22)

Run: the showcase manifest (`docs/showcases/simplelogin.md`, gap 1) at
Cartograph `40ff457`, with the app at `4f212552` and the extension at
`d18b40fc`. Scored by [`score.py`](score.py)
(`python3 -I score.py <run output dir>`). The report is in
[`scores.json`](scores.json). The scorer read all 25 candidates in
`journeys.md`. It matches a screen step by its route (J1 `api-key` or
`onboarding`, J2 `/main`, J6 `new-alias`) and a client call by method and
path. It scores the candidate with the most required steps, which is
`Main → POST /api/v2/alias/custom/new`
(`/` → `/main`, then the goal).

| Step | Expected | Found | |
|---|---|---|---|
| J1 | API key setup screen | not on the path | missing |
| J1a | `GET /api/user_info` | on `/main` | optional, found |
| J2 | Main screen | `/main` | found |
| J3 | `GET /api/v5/alias/options` | `GET /api/v4/alias/options` on `/main` | missing |
| J4 | `GET /api/v2/mailboxes` | `GET /api/mailboxes` on `/main` | optional, missing |
| J5 | custom alias, v3 or v2 | `POST /api/v2/alias/custom/new`, the goal | found |
| J5a | `POST /api/alias/random/new` | on `/main`; the goal of a separate candidate | optional, found |
| J6 | New alias screen | not on the path | missing |

| Measure | Result | Pass |
|---|---|---|
| Required steps | 2 / 5 | no |
| Order | J2 before J5 | no (steps missing) |
| Alternate path | J5a is not this journey's goal | yes |
| Endpoints | 3 / 3 matched calls link to the server endpoint | yes |

**The result is partial: J1, J3 and J6 are missing.**

- **J1 and J6 are recovery misses.** The extension declares both screens,
  `/api-key-setting` and `/new-alias-result/` (`src/popup/Navigation.js`),
  but the candidate goes straight from `/` to `/main` and ends at the goal
  endpoint, with no result screen.
- **J3 is a version miss.** Public help documents `v5`, and the pinned
  extension calls `v4` (`src/popup/APIService.js:13`). J4 has the same
  problem (`/api/mailboxes`, not `/api/v2/mailboxes`).

Extra calls on the path, reported by name: `GET /api/mailboxes`,
`GET /api/v4/alias/options`, `POST /api/aliases/<int:alias_id>/toggle`,
`POST /api/api_key` (on `/`) and `POST /api/v2/aliases`.

The expected set is not changed here. Accepting `v4` for J3 needs a new go.
