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

The pass thresholds are a proposal until the owner freezes them with a
one-line go. Agreement from a reviewing agent does not freeze them.
