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
value needs a new go and is recorded here with the reason. No run has
been scored yet. #2169 is done only when a scored run and its table are committed
here, whatever the result.

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
