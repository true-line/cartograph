# W10a content acceptance: SimpleLogin test links (#946)

This is the pre-run answer key for #946's content acceptance. The labels in
[`labels.json`](labels.json) were written by hand from the SimpleLogin source
at `4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798`, before any scored run, without
reading Cartograph output. Keep this directory outside all model inputs.
Recovery never reads it.

## Sample

[`select.py`](select.py) parses `tests/**` with Python's `ast` and never
imports or runs SimpleLogin code (`python3 -I select.py <app checkout>`). A
pytest test is a `test*` function at module level or a `test*` method of a
`Test*` class, in `test_*.py` or `*_test.py`. Of the 330 tests whose source
contains `flask_client.`, it takes the 20 with the lowest sha256 of
`<path>::<qualified name>`, with the path in POSIX form. All 26
`flask_client.<verb>(…)` calls in those tests are labelled;
`session_transaction()` is not a request.

Each call records:

- **truth**: the view function that actually serves the request, as
  `<file>::<function>`. It is `flask-admin:<view>` for a Flask-Admin view and
  `none` when no route serves the request.
- **expect**: the outcome the W10a contract (AC-0343) requires.
  - `link`: a `REQUESTS` edge to the truth Endpoint.
  - `link:follows_redirects`: the same, marked `follows_redirects`, with no
    expectations.
  - `gap:<reason>`: a Gap naming the call. `gap:multiple_urls` is one view
    registered under three rules: AC-0343 makes an endpoint name that builds
    more than one URL a Gap, even though the handler is unambiguous.
- **statuses / keys**: what the test asserts about this call's response.
  Statuses are `status_code == N` assertions. Keys are `json[…]` and
  `get_json()[…]` reads and `"k" in r.json`, up to the next binding of the
  response name. An `if` condition, `>=`, `in [..]` or whole-body equality
  asserts nothing here; the notes mark each such case.

The sample contains 19 `link` calls (3 of them `follows_redirects`) and 7
`gap` calls. It also contains SimpleLogin's own test bug: line 751 of
`tests/api/test_alias.py` posts to `/api/aliases/random/new`, which no route
serves.

## Scoring

Score one exported run of the showcase manifest at the commit above.

| Measure | Definition | Pass |
|---|---|---|
| Link recall | `link` calls cited by a `REQUESTS` edge from their test to the Endpoint of their truth view and method | ≥ 18 / 19 |
| Link precision | edges citing a sampled call whose Endpoint is that call's truth | 100 % |
| Gap accuracy | `gap` calls that are a Gap naming the call whose reason means the labelled one, and are not linked | 7 / 7 |
| Redirects | `follows_redirects` set on exactly the three labelled edges, with no statuses or keys | 3 / 3 |
| Status precision / recall | pairs of (edge, status) against the union of the edge's labelled calls | 100 % / ≥ 90 % |
| Key precision / recall | the same for (edge, key) | 100 % / ≥ 90 % |

Every link must also cite the call's line. A miss is reported with its call
line and the reason, never silently dropped.

The pass thresholds were frozen by the owner's one-line go on 2026-11-22.
Any later change to a threshold or a label needs a new go and is recorded
here with the reason.

## Result: first scored run (2026-11-22)

Run: the showcase manifest (`docs/showcases/simplelogin.md`, gap 1) at
Cartograph `40ff457`, with the app at the commit above, the extension at
`d18b40fc` and Android at `ab6e7103`. Scored by
[`score.py`](score.py) (`python3 -I score.py <app checkout> <graph.db>`).
The report is in [`scores.json`](scores.json).

| Measure | Result | Pass |
|---|---|---|
| Link recall | 19 / 19 | yes |
| Link precision | 19 / 22 | no |
| Gap accuracy | 4 / 7 | no |
| Redirects | 3 / 3, plus 1 extra | no |
| Status precision / recall | 11 / 13 and 11 / 11 | no / yes |
| Key precision / recall | 7 / 7 and 7 / 7 | yes / yes |

**The run fails against the frozen labels.** All eight misses come from the
three Flask-Admin calls (`url_for(admin.email_search.index)` at
`tests/admin/test_email_search.py:254` and `:557`, and
`url_for(admin.custom_domain_search.index)` at
`tests/admin/test_custom_domain_search.py:204`). The labels expect
`gap:unknown_endpoint_name` for them, because Flask-Admin views were out of
the route model when the labels were written. This run recovers them as
Endpoints: `GET /admin/admin.email_search/` and
`GET /admin/admin.custom_domain_search/`. Those URLs match
`app/admin/index.py:44-47`, where the view's `endpoint=` is also its URL.
The three calls therefore link instead of becoming Gaps. Their statuses
(200 at lines 254 and 204) appear on the edges but are not labelled, and
`follow_redirects=True` at line 557 marks one more edge `follows_redirects`.

On the other 23 calls the run matches every label: 19 links with no wrong
endpoint, 4 Gaps with the labelled reasons, 3 redirect edges, and every
labelled status and key.

The labels are not changed here. Relabelling the three Flask-Admin calls
needs a new go.
