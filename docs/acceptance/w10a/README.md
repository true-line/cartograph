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
  `<file>::<function>`, or `<file>::<View>.index` for a Flask-Admin view
  (amendment 1). It is `none` when no route serves the request.
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

The sample contains 22 `link` calls (4 of them `follows_redirects`) and 4
`gap` calls. It also contains SimpleLogin's own test bug: line 751 of
`tests/api/test_alias.py` posts to `/api/aliases/random/new`, which no route
serves.

## Scoring

Score one exported run of the showcase manifest at the commit above.

| Measure | Definition | Pass |
|---|---|---|
| Link recall | `link` calls cited by a `REQUESTS` edge from their test to the Endpoint of their truth view and method | ≥ 21 / 22 |
| Link precision | edges citing a sampled call whose Endpoint is that call's truth | 100 % |
| Gap accuracy | `gap` calls that are a Gap naming the call whose reason means the labelled one, and are not linked | 4 / 4 |
| Redirects | `follows_redirects` set on exactly the four labelled edges, with no statuses or keys | 4 / 4 |
| Status precision / recall | pairs of (edge, status) against the union of the edge's labelled calls | 100 % / ≥ 90 % |
| Key precision / recall | the same for (edge, key) | 100 % / ≥ 90 % |

Every link must also cite the call's line. A miss is reported with its call
line and the reason, never silently dropped.

The pass thresholds were frozen by the owner's one-line go on 2026-11-22.
Any later change to a threshold or a label needs a new go and is recorded
here with the reason.

### Amendment 1 (2026-11-22, owner's go)

The three Flask-Admin calls (`tests/admin/test_email_search.py:254` and
`:557`, `tests/admin/test_custom_domain_search.py:204`) are relabelled from
`gap:unknown_endpoint_name` to `link`, the one at line 557 as
`link:follows_redirects`. Their truth views are `EmailSearchAdmin.index`
and `CustomDomainSearchAdmin.index`.

Reason: a Flask-Admin view is a Flask route. `@expose("/")` on a view
registered by `add_view(..., endpoint=...)` (`app/admin/index.py:44-47`)
serves `/admin/<endpoint>/`, and `url_for(admin.<endpoint>.index)` resolves
to it. The old labels predate Flask-Admin support in the route model.

The counts move with the labels: 22 links, 4 of them redirects, and 4 Gaps.
The tolerance is unchanged: at most one link miss, no Gap or redirect miss.

## Result: rescore after amendment 1 (2026-11-22)

Run: the showcase manifest (`docs/showcases/simplelogin.md`, gap 1) at
Cartograph `40ff457`, with the app at the commit above, the extension at
`d18b40fc` and Android at `ab6e7103`. This is the same run as the first
score; only the labels changed. Scored by [`score.py`](score.py)
(`python3 -I score.py <app checkout> <graph.db>`). The report is in
[`scores.json`](scores.json).

| Measure | Result | Pass |
|---|---|---|
| Link recall | 22 / 22 | yes |
| Link precision | 22 / 22 | yes |
| Gap accuracy | 4 / 4 | yes |
| Redirects | 4 / 4, no extra | yes |
| Status precision / recall | 13 / 13 and 13 / 13 | yes / yes |
| Key precision / recall | 7 / 7 and 7 / 7 | yes / yes |

**The run passes W10a.** Every call links to its truth view with its line,
the four Gaps carry the labelled reasons, and every labelled status and key
is on its edge with nothing extra.

### First score, before amendment 1

Against the original labels the run failed: link precision 19 / 22, gap
accuracy 4 / 7, one extra redirect edge and status precision 11 / 13. All
eight misses were the three Flask-Admin calls, which the run recovered as
`GET /admin/admin.email_search/` and `GET /admin/admin.custom_domain_search/`
and linked instead of reporting as Gaps. On the other 23 calls it matched
every label.
