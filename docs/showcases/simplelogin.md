# SimpleLogin: a deterministic showcase

**Question.** If three SimpleLogin repositories go into Cartograph with no model calls, how much of the system's documented HTTP contract comes back, and what is missing?

The answer key is the project's own API reference, [`docs/api.md`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/docs/api.md). Cartograph never reads it during recovery. It is used only for scoring.

Only deterministic tiers ran: no T2 embeddings, no T3 provider calls and no network. Tracking issue: #721.

## Inputs

| Repo | Commit | Language |
|---|---|---|
| [simple-login/app](https://github.com/simple-login/app) | `4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798` | Python (Flask), JS |
| [simple-login/browser-extension](https://github.com/simple-login/browser-extension) | `d18b40fc774a3b477468bc608c3589a24f04c3ed` | JS (Vue), WebExtension |
| [simple-login/Simple-Login-Android](https://github.com/simple-login/Simple-Login-Android) | `ab6e7103c450f10a6065b1d27e4ed93b6c01b3cb` | Kotlin |

## Reproduce

```sh
# Clone the three repos into one directory and check out the commits above
# in each. $SRC is that directory and $OUT a path that does not exist yet;
# the harness creates it and refuses an existing one.
cd src-tauri
CARTOGRAPH_SHOWCASE_SOURCES=$SRC/app:$SRC/browser-extension:$SRC/Simple-Login-Android \
CARTOGRAPH_SHOWCASE_OUTPUT=$OUT \
  cargo test --release showcase_recover_and_export -- --ignored --nocapture
cd .. && node scripts/score-api-contract.mjs $SRC/app/docs/api.md $OUT/endpoints.json /api
```

The harness is in `src-tauri/src/showcase_tests.rs`. It uses the production path (register source, start job, ingest) against an isolated profile under `$OUT/profile`, then writes these outputs:

- `ingest.json`: per-source summary and coverage;
- `verified-only/` and `best-effort/`: the spec bundles;
- `endpoints.json`: the recovered `Endpoint` nodes;
- `labels.json`: node counts by label.

The scorer, `scripts/score-api-contract.mjs`, reduces `:x`, `<x>` and `{x}` to one parameter shape, then compares each `METHOD /path` pair.

On an M4 laptop, ingesting and exporting all three repos through the system manifest (gap 1) took 18.9 s wall time, with an already built release test binary. The first build takes about 20 minutes. Peak resident memory was 1.03 GiB (`/usr/bin/time -l` on the test binary). It was 1.09 GiB at `d326a47e`, and 1.39 GiB before the publish snapshot check stopped re-reading the whole graph (#768). Measured at Cartograph `2508c5a4` (0.23.1).

## Scheduled watch (#1768)

The **Scheduled checks** workflow runs this showcase every Sunday on the self-hosted Mac, with a release build and the system manifest above (`node scripts/showcase-watch.mjs simplelogin`). It compares the run with the thresholds in [`bench/thresholds/simplelogin.json`](../../bench/thresholds/simplelogin.json): endpoints recovered, the `/api` contract scores below, client calls linked, traced flows, business rules, data entities, tests, gap nodes and gap findings by extractor, wall time and peak memory. A missed threshold fails the lane and opens or updates the issue "Scheduled: SimpleLogin showcase below its thresholds" (AC-1700). Each run's report states the Cartograph commit and version it measured, and its `metrics.json` artifact is the source for updating the numbers on this page.

The thresholds were recorded at Cartograph `30f76fb5` (v0.75.0) with the #1774 gap-cause fix, release build, linux-x64:

| Metric | Value |
|---|---:|
| Endpoints recovered | 426 |
| Documented `/api` endpoints matched | 44 / 45 |
| Documented input names matched | 55 |
| Documented statuses matched | 30 |
| Documented top-level keys matched | 14 |
| Client calls linked to an endpoint | 59 (24 Gaps) |
| Traced flows | 484 (38 Verified, 23 Partial) |
| Business rules | 6,071 |
| Data entities | 80 |
| Tests | 1,049 |
| Gap nodes | 22,435 |
| Gap findings | 22,478 in 61 classes (Python 14,404, TypeScript/JS 7,964, Kotlin 52) |
| Whole harness wall time (ingest, knowledge phase, both exports, briefings) | 142.3 s on 4 vCPU |
| Peak resident memory | 1.67 GiB |

Against v0.72.0 (`ee9df763`), endpoints, the `/api` contract scores, client links, Verified flows and data entities hold exactly; 136 more business rules, one more traced flow and two more tests were recovered. The 2,297 more gap findings are mostly deliberate: rules that are now recovered but not yet fully interpreted. They come from Python condition locals, bindings and `except` handling (AC-1034, AC-1036, AC-1040), from Kotlin guarded exits (AC-1106, AC-1108) and from TypeScript bindings. Their classes now name each reason (AC-1064). Before the fix, 1,092 Python gaps shared one generic "Source-rule interpretation remains incomplete." class.

The sections below were measured earlier and say at which commit.

## Score

| Metric | Value |
|---|---|
| Documented endpoints | 45 |
| Recovered under `/api` | 56 |
| Matched | 44 |
| **Recall** | **44 / 45 (97.8%)** |
| Documented share of recovered | 44 / 56 (78.6%) |

Before Flask Blueprint support landed with this showcase, recall was **0 / 45**. Every API route is declared as `@api_bp.route(...)` on a Blueprint mounted with `url_prefix="/api"`, and the Python adapter recognised only `@app.route`. Blueprint and FastAPI `APIRouter` prefixes are now composed with their mounts, including mounts in other files. A prefix that can't be read becomes a `Gap`, and a router with no observed mount stays `InferredStrong`, marked `mount_not_observed`. Nested mounts compose their whole chain (up to 16 levels). A cyclic chain, or a prefix built by `**kwargs` or `*args` expansion, becomes a `Gap` rather than a guessed path (`crates/adapters-lang-python/src/lib.rs`, `served_prefixes`).

**The one miss is a documentation error.** `docs/api.md` documents `POST /api/notifications/:notification_id`, but the code registers `POST /api/notifications/<int:notification_id>/read` ([`app/api/views/notification.py:63`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/notification.py#L63)). Cartograph recovered the route the code actually serves.

**The 12 undocumented recoveries are real routes that are missing from the docs.** Examples:

- `GET /api/aliases` and `POST /api/aliases` ([`alias.py:39`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/alias.py#L39));
- `GET /api/mailboxes` ([`mailbox.py:165`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/mailbox.py#L165));
- `POST /api/v2/alias/custom/new` and `DELETE /api/setting/unlink_proton_account`.

The scorer lists all 12.

All 426 recovered endpoints are Flask. This includes the web UI, OAuth and Flask-Admin routes outside `/api`. Their confidence follows how much of the mount chain is proven:

- **9 `Confirmed`**: declared directly on the app object created in `create_app()`.
- **239 `InferredStrong`, marked `mount_receiver_annotated`**: SimpleLogin registers its Blueprints inside `register_blueprints(app: Flask)`. The receiver is a parameter annotated as `Flask`, not an app constructed in that function, so the type annotation is the only evidence that it is the served app. Cartograph composes the prefix, but it doesn't claim the mount is proven.
- **24 `InferredStrong`, marked `flask_admin`**: the `@expose` methods of the Flask-Admin views registered in `init_admin(app: Flask)` (#1193), on the same annotated receiver. Each is named `<endpoint>.<method>` (such as `admin.email_search.disable_2fa`) at the path Flask-Admin composes (`/admin/admin.email_search/disable_2fa`). Since #1216 the 23 views inheriting `BaseAdminView` are `auth: required` (basis `flask_admin_is_accessible_requires_login`, InferredStrong). Their `is_accessible` returns `current_user.is_authenticated and current_user.is_admin and …`, their `inaccessible_callback` returns `redirect_destination or redirect(…)`, and every lockfile pins Flask-Admin 1.5.7/1.5.8 and flask-login 0.5.0. The index view does not override `is_accessible`; its `index` method returns `_redirect_if_user_is_not_allowed() or …`, a helper whose first statement redirects a user who is not authenticated to the login page, so since #1320 it is `auth: required` on the body's own basis (`flask_view_body_denies_anonymous`, the helper as its guard). The 14 model views' list, create and edit pages are generated by Flask-Admin itself (see the next item).
- **154 `InferredStrong`, marked `flask_admin` with `handler_origin: library`**: the routes Flask-Admin's own `BaseModelView` exposes for each of the 14 model views (#1203): its list, create, edit, details, delete, action, export and ajax pages, 11 method-specific routes per view. Their rules come from the Flask-Admin release source, so they are projected only because every root lockfile (`poetry.lock`, `requirements.lock`, `requirements-dev.lock`, `uv.lock`) pins Flask-Admin exactly to 1.5.7 or 1.5.8. Those two releases' source was checked and exposes the same routes, and each route cites the pins. An unpinned, inexact or unchecked version keeps one Gap per view instead. Each handler is the library method (`sym:pypi/flask-admin@flask_admin/model/base.py#BaseModelView.edit_view`), classified as an external boundary. Its `auth` is `required` through the view's `BaseAdminView.is_accessible` (#1216; `unknown` before). A method a view redefines stays the repository's own route.

No route was promoted to `Confirmed` from a receiver it couldn't prove.

## Request inputs (#721)

Each Flask endpoint records, as `request_inputs`, the literal names its handler body reads: `query` from `request.args`, `form` from `request.form`, and `body` from `request.get_json()` or `request.json`, read directly or through a local name such as `data = request.get_json()`. Each recorded name cites the expression that reads it, under the inputs' own `prov`. The same scorer compares those names with each documented endpoint's `Input:` bullets. Headers and path parameters are excluded, because the route path already carries the parameters.

| Metric | Value |
|---|---|
| Matched endpoints with documented inputs | 24 |
| Documented input names | 55 |
| **Recall** | **55 / 55 (100%)** |
| **Precision** | **55 / 55 (100%)** |

The first scored run measured 52 / 60. Five of the misses were path parameters (`:mailbox_id` and similar) that `api.md` lists under `Input:`, so the scorer now skips them. The other three were membership tests: `GET /api/v2/aliases` reads its filters as `"pinned" in request.args` ([`alias.py:123`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/alias.py#L123)), and the adapter now records them as query reads.

Of the 426 endpoints, 175 record inputs; the 154 library routes record none, since their handlers are not repository code. 44 of those are marked `partial`: their handler reads something that isn't a literal key, so the recorded names are what the body proves, not the complete set. The reasons:

- `body_read_dynamically`: 23;
- `form_read_dynamically`: 16;
- `query_read_dynamically`: 7;
- `request_passed`: 4.

Helpers the handler calls are not followed.

## Responses (#721)

Each Flask endpoint also records `responses`: what its own `return` statements prove. That is the response `kinds` (`json`, `html`, `redirect`, or `text` for a returned string), the `statuses`, and the top-level JSON `keys`, each `return` cited. Since #940 the record also follows repository auth decorators and the dicts a handler or its helper builds, and lists each function followed. Of the 426 endpoints, 267 record responses (none of the 154 library routes): 135 return `html` (`render_template`), 166 return `redirect`, 68 return `json`, and 13 return `text` (a string such as `"redirect_uri must be set", 400`, or `str(resp)`). Since #1182 a `make_response(…)` call, returned directly or through a local that only gets cookies or headers, is read as its body and status. Since #1187 a handler's `return helper(…)` adds that repository function's responses when it cannot return `None`, as `return after_login(user, next_url)` does in the login, password-reset and five OAuth callback handlers. One handler can return several kinds. 87 follow at least one repository function, most often `@require_api_auth` and its `authorize_request` check (44 endpoints) or the dashboard's `@sudo_required` (25). 24 are marked `partial` (before #1182: 48; before #1187: 26): 12 `response_built_elsewhere`, where a JSON body is built by something not proven, and 12 `response_not_proven`, where another call or an unproven variable is returned, such as `alias_export_csv(…)`, which can return a plain `si.getvalue()`. 8 of those 12 are Flask-Admin views returning `self.render(…)` (#1193). Under `/api`, 7 of 56 are partial (before #940: 22).

Scored against each documented endpoint's `Output:` section:

| Metric | Value | Before #940 |
|---|---|---|
| Documented statuses recovered | **30 / 30 (100%)** | 23 / 30 (76.7%) |
| Documented top-level keys recovered | **14 / 29 (48.3%)** | 2 / 29 (6.9%) |
| Recovered keys that are documented | 14 / 20 (70.0%) | 2 / 7 (28.6%) |

The decorators supply the 401, 403 and 440 statuses ([`app/api/base.py`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/base.py)). That includes the 403 documented for `GET /api/user/cookie_token`: the handler itself returns 401 there ([`user.py:54`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/user.py#L54)), and only `authorize_request`'s disabled-account branch returns 403. `GET /api/logout`'s 200 is read through `make_response(…)` since #1182 (29 / 30 before).

The keys now include the success bodies built by `auth_payload` (login, MFA) and the `ret` dict of `GET /api/v5/alias/options`. The remaining misses are the per-alias fields of `GET /api/v2/aliases`, which `api.md` lists as top-level bullets although they are nested under `aliases`. The undocumented keys are the `error` keys of the documented endpoints' failure returns.


## Tests (#946)

SimpleLogin's own pytest suite is read as test arrangements: what each test requests and what it expects back. All 1,047 tests become `TestCase` facts citing their definitions. The suite's tests reach the app through the `flask_client` fixture in `tests/conftest.py`, which yields `app.test_client()`. A call in a test is read only when its receiver is proven to be that client.

| Measure | Value |
|---|---|
| Client calls in test bodies read | 418 |
| Test → endpoint references (`REQUESTS`) | 304, from 352 calls |
| Recovered endpoints referenced by a test | 87 / 426 |
| Under `/api` | 43 / 56 |
| Unresolved test requests (Gaps) | 66 |

`test_arrangements.md` lists each referenced endpoint with the tests that reference it, the statuses they assert and the response keys they read, then the 339 endpoints that no test references (154 of them library routes). These are static references, not coverage. They show that a test issues a request, not that the test runs, passes or reaches a given branch. Each reference is InferredStrong and marked `client_app_assumed`, because the fixture's app is assumed to be the one serving the recovered routes. A hand-checked sample of 12 references was correct.

The 66 Gaps, by reason (before #1193, another 63 named a Flask-Admin endpoint, `url_for("admin.…")`; they now reference the recovered admin views):

- **34** request a computed URL.
- **30** name an `oauth.*` endpoint. The OAuth Blueprint is registered twice, so its names build more than one URL.
- **1** requests a path that no recovered endpoint serves (below).
- **1** matches more than one endpoint equally (`GET /`).

**The one unserved path is a test bug.** `test_cannot_create_alias_with_admin_disabled_mailbox_via_api` posts to `/api/aliases/random/new` ([`tests/api/test_alias.py:752`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/tests/api/test_alias.py#L752)), but the route is `/api/alias/random/new` ([`new_random_alias.py:21`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/new_random_alias.py#L21)). Read from the source (the suite was not run for this showcase), Flask has no route at that path and would answer 404, and the test checks the mailbox only `if r.status_code == 201`; otherwise it asserts `r.status_code >= 400`, which a 404 satisfies. So it can pass without reaching the mailbox check its name describes.

Seven client calls are in helper functions such as `login()` and are not followed.

### Test and contract drift (#953)

`drift_register.md` compares what the tests expect of each referenced endpoint with what its recovered responses return. The comparison stays within the responses' recorded scope: the handler body, plus any repository functions it follows.

- Of the 87 referenced endpoints, 32 are reported as **incomplete evidence**, not drift:
  - 6 have partial responses;
  - 2 (`GET /admin/admin.email_search/`, `GET /admin/admin.custom_domain_search/`) have responses that carry no usable provenance, because `self.render(…)` proves nothing, so the returned side cannot be cited;
  - 24 are referenced by a call that follows redirects, so the test reads another endpoint's response.
- On the other 55, **no test asserts a status, or reads a key, that the handler does not return** (0 `asserted-not-returned`).
- **46 endpoints return a status that no resolved test asserts** (`returned-not-asserted`, InferredWeak). That is 83 statuses in total: 403 ×28, 401 ×28, 400 ×11, 200 ×9, 302 ×6 and 422 ×1, with 34 of the 46 endpoints under `/api`. Most are the 401 and 403 branches of `@require_api_auth`, which the API tests exercise for only a few endpoints. These findings are weak because unresolved test requests and calls in helper functions are not read, so the claim is only that no recovered test reference asserts the status.

### Test references in flow dossiers (#965)

Each test is also linked to its own function, so a function a test calls directly is referenced by it. Every flow in `flow_dossiers.md` now ends with a table naming, for each step, the tests that request its endpoint or call its function, and the steps no recovered test references. Like the test references above (#946), these are static references: they don't show that a test executed the step, and calls a step makes further down are not counted.

| Flow trigger | Flows | With a referenced step | Steps referenced |
|---|---|---|---|
| Endpoint | 396 | 137 | 481 / 1,619 |
| Screen | 21 | 14 | 75 / 596 |
| Schedule | 18 | 14 | 70 / 154 |
| Command | 2 | 0 | 0 / 5 |
| Extension context | 2 | 0 | 0 / 28 |
| **All** | **439** | **165** | **626 / 2,402** |

Of the 626 referenced steps, 92 are endpoint steps (the 87 endpoints of `test_arrangements.md`, some in more than one flow) and 534 are functions a test calls directly. Four scheduled jobs have no referenced step: deleting old monitoring records, clearing the alias and user audit logs, and deleting logs. The best-covered is the custom-domain check, with 14 of its 21 steps referenced. Source: this run's `flow_dossiers.md`; best-effort and verified-only give the same counts.

## What else was recovered

| Source | Producers that ran | Nodes | Edges | Coverage |
|---|---|---|---|---|
| app | TypeScript/JS (49 files), Python (717 files), toolchain, schedules (2 files) | 24,670 | 34,271 | limited: no `otel_jsonl` declared, so 250 endpoint and channel facts stay T0 |
| browser-extension | TypeScript/JS (28 files), WebExtension manifest, toolchain | 609 | 698 | limited |
| Simple-Login-Android | Kotlin (195 files), toolchain, cross-repo HTTP stitching | 1,297 | 3,202 | complete for the producers that ran |

Source: `ingest.json`, coverage producers, from the manifest run. Android's totals include 41 nodes and 41 `FETCHES` edges from cross-repo HTTP stitching. At `2508c5a4` the coverage record charged these to `t0.adapter-ts`, which ran on no Android files; since #874 they are the `t0.events` row's. Every coverage record cites its repository's commit (gap 6).

The bundle has 26,662 assertions, 7,171 open gap findings in 26 cause classes and 3 security findings (`bundle.json`, `gap_register.md`, `security.md`), regenerated at `2508c5a4`. At `d326a47e` it had 29,256 assertions and 7,495 findings in 27 classes; the gap 8 fixes account for most of the difference. Class numbers below are this run's: the register numbers classes by size, so they shift between runs. The security findings are three over-broad host permissions in the extension manifest: `http://*/*`, `https://*/*` and `https://*.simplelogin.io/*`.

## Gaps, ranked by impact

1. **Client-to-server links: resolved by #737.** Before #737, neither client produced a call fact that could be matched to a recovered endpoint. The Android app declares its API as Retrofit annotations (`core/network/src/main/java/io/simplelogin/core/network/ApiService.kt`), which the Kotlin adapter did not recognise. The extension keeps a `{method, path}` table (`src/popup/APIService.js:7` onward) and joins it to a base URL read from storage at runtime, so its calls ended as `runtime-computed fetch URL` gaps. Now both become client calls carrying the base URL they join, and a system manifest binds them to the server. Ingesting the three repos through the manifest below links:
   - **all 41** Retrofit methods in the Android app, to 37 distinct server endpoints. They join the app's one `Retrofit.Builder().baseUrl("https://app.simplelogin.io")`;
   - **18 of 20** extension calls, to 14 distinct endpoints, through `callAPI(API_ROUTE.X)` and direct `fetch(apiUrl + API_ROUTE.X.path)`. The other two stay Gaps: the `fetch` inside the `callAPI` wrapper itself, and an unrelated computed URL in `src/content_script/input_tools.js`.

   Together the clients reach 43 distinct server endpoints, 8 of them from both. Every link cites the `declared-service` binding (`fetches.json`). The extension's runtime base is author-declared in the manifest `env` by its source text, with the value of the extension's own default (`SLStorage.DEFAULT_SETTINGS`). Without that declaration its calls stay Gaps naming the route-table entry. The same run fixed route matching for Flask `<int:id>` parameters, which until then matched no client path.

   ```toml
   [[repos]]
   url = "app"
   [[repos]]
   url = "browser-extension"
   [[repos]]
   url = "Simple-Login-Android"

   [env]
   "SETTINGS.apiUrl" = "https://app.simplelogin.io"
   "apiUrl" = "https://app.simplelogin.io"
   "this.apiUrl" = "https://app.simplelogin.io"

   [[services]]
   name = "simplelogin"
   repo = "app"
   base_urls = ["https://app.simplelogin.io"]
   ```

   Save it as `$SRC/cartograph.system.toml` and run the showcase with `CARTOGRAPH_SHOWCASE_MANIFEST=$SRC/cartograph.system.toml` in place of `CARTOGRAPH_SHOWCASE_SOURCES`. The run also writes `fetches.json`, listing every client call with its target endpoint or Gap reason.
2. **Data model: resolved by #738.** `app/models.py` declares 80 SQLAlchemy models. Before #738, `data_model.md` reported *No DataEntity facts have been recovered yet*. With the SQLAlchemy producer, all 80 become Confirmed `DataEntity` facts with cited columns, linked by 99 `REFERENCES` edges, one per entity pair. That includes the 64 `ForeignKey` targets written as attribute expressions (`sa.ForeignKey(User.id, …)`), which were `Gap`s until each was resolved to the model and declared column it names. No SQLAlchemy `Gap` remains. The 275 Alembic revisions in `migrations/versions/` are now replayed to head `b7e2d4a9c1f3`, merge revisions included (#833). The head schema matches all 80 modeled tables and their 661 columns, so `drift_register.md`'s Schema drift section reports no drift and no Gap. Getting there meant reading `deferred(sa.Column(...))` model columns, which had been dropped, and the revisions' exact raw-SQL `ADD COLUMN`s and autocommit blocks. A perturbed copy of the repository with drift injected reports it.
3. **Infrastructure and scheduled work: largely resolved by #734 and #770.** `topology.md` now lists five observed `Resource`s, each citing the code that proves it:
   - S3 through boto3 ([`app/s3.py`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/s3.py)), Confirmed;
   - Redis, `redis:env:MEM_STORE_URI` (see below);
   - the SQLAlchemy engine, `sql-database:env:DB_URI`. `app/db.py` builds it from `config.DB_URI`, and Cartograph follows that constant into `app/config.py`, where it is read from the environment. It is InferredStrong and marked `identity_assumes_method_receivers`, not Confirmed, because the repository also writes computed attributes through a method's `self` (`setattr(self, k, v)` in `app/dashboard/views/alias_log.py`), and the follow rests on that receiver not being the config module;
   - `localStorage`, used by the server's `static/local-storage-polyfill.js`;
   - the extension's WebExtension `sync` storage area (`webextension-storage:sync`), reached through `webextension-polyfill` in [`src/popup/SLStorage.js`](https://github.com/simple-login/browser-extension/blob/d18b40fc774a3b477468bc608c3589a24f04c3ed/src/popup/SLStorage.js), Confirmed.

   Redis is now named. The server reaches it only through the `limits` rate-limit storages (`RedisStorage` and `RedisSentinelStorage` in [`app/redis_services.py`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/redis_services.py)), with the URL passed in as a parameter. The parameter is now followed to the function's one caller, `initialize_redis_services(app, config.MEM_STORE_URI)` in `simplelogin_app.py`, and on into `app/config.py`, so both sites name `redis:env:MEM_STORE_URI` (that class is closed). The result is InferredStrong and marked `identity_assumes_static_callers`, because a dynamic lookup of the function would not be seen, and also `identity_assumes_method_receivers`, for the same receiver writes as the SQL engine.

   SMTP is not among them. Each of the three `smtplib.SMTP` sites takes a host computed at runtime (`random.choice(config.POSTFIX_SERVERS)` in `app/email_utils.py`, a host split inside a method in [`app/mail_sender.py`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/mail_sender.py)), so each is a Gap naming the call (class C-19). Earlier runs showed an unnamed, Confirmed `smtp` for `mail_sender.py`; the `host=` keyword is now read, so its runtime host is reported as a Gap instead.

   The 18 yacron jobs in `crontab.yml` and `crontab-all-hosts.yml` are now `Schedule` triggers carrying their cron expressions, and each appears in `flow_dossiers.md` as a flow from the schedule into the handler its `cron.py -j <job>` argument selects (#779): in each of the 18 the first participant after the schedule is its own handler function. The handlers of 15 jobs are defined in `cron.py`, one of them (`check_hibp`) run through `asyncio.run`; `check_custom_domain` and both `send_undelivered_mails` jobs call functions imported from `tasks/check_custom_domains.py` and `app/mail_sender.py`. Every entry is InferredStrong and marked `entry_may_be_replaced_in_image`, not Confirmed. The final Dockerfile stage proves `/code` is the repository, but a later `COPY --from=npm /code /code` may overwrite the script, and the ENTRY cites that instruction. `alembic.ini`'s migrations are replayed under gap 2.

   `example.env` and `tests/test.env` are now read as env files, key names only and with no evidence span (AC-0097). Each env-referenced resource links to the files that declare its key with a `CONFIGURED_IN` edge in the graph: `DB_URI` to both files, `MEM_STORE_URI` to `tests/test.env` only, because `example.env` never declares it. `topology.md` draws each link as a dotted arrow to the file, named by its path, and the file's assertion cites no span. Which file a deployment actually loads is not proven, so every declaring file gets its own edge. Template values are never read, so a placeholder such as `DB_URI`'s example URL never becomes a resource identity.
4. **Documentation drift: resolved by #739.** Cartograph now takes a declared API reference in as a contract and reports drift in its own `drift_register.md`, cited on both sides. Declared as below, it reports the notification mismatch and the 12 undocumented routes that the external scorer found, with no scorer involved. The declaration is opt-in, so the default run above reports 0 drift. For enterprises with outdated docs, this is the direct value.
5. **No user stories.** `user_stories.md` reports *No Capability facts have been recovered yet*. Capabilities need the interpretation tier, which this deterministic run excludes by design.
6. **Commit pinning: resolved by #740.** A local source that is a clean Git checkout is now recorded at its `HEAD` commit, so each coverage record in `ingest.json` and every citation names the full commit above. Recovery still reads the working tree; it is not a Git-object capture. Instead, Cartograph checks that the tree is clean at `HEAD` before extraction and again before publishing, and fails the run if it changed. A dirty checkout still records `workdir`, with the reason in the coverage note. Declaring the API reference below dirties `app`, so that run records `workdir` for `app` only.
7. **TypeScript rule gaps dominate the count but not the impact.** Classes C-01 to C-09 account for 6,835 of the 7,171 gap findings. They are unresolved interpretation (return/throw consumers, predicates, mutation, loops), routed to T1, and they don't affect the HTTP contract.
8. **Unresolved module edges: Python mostly resolved.** Python had 177, but they were not third-party packages: 160 were SimpleLogin's own modules (`app.db`, `tasks.check_custom_domains`, …), because an absolute import never resolved to the file it names. It now resolves from the repository root (`app/db.py` or the package `app/__init__.py`), the same root calls through those imports already used, when exactly one of the two exists. 17 remain (C-13). 16 are standard-library or installed names that a nested directory shares (`email` and `mailbox` with `app/email`-style packages, `google`, `newrelic`, `facebook`), which the rule keeps as Gaps because the repository could shadow them. The other is `app.events.generated`, a directory with no `__init__.py`. Kotlin's 69 had the same cause in another form: imports of enum entries (`Theme.DARK`), extension functions, top-level properties, a type alias and annotation classes, none of which the import resolver indexed. They now resolve to their declaring file, and 4 remain (C-18), all build-generated (`R`, `BuildConfig`). The 38 unresolved Kotlin import call Gaps were constructor calls of imported repository classes (`ApiKey(...)` ×11, `LoginBody(...)`, …), which the adapter does not model as calls, as for a class in the same file. They no longer gap, and 2 remain (C-24): `UserSessionPreferences.serializer` and `DevicePreferences.serializer`, members the kotlinx-serialization compiler plugin generates and the source never declares.

## Measuring contract drift (#739)

To have Cartograph recover and report API contract drift directly in `drift_register.md`, declare `docs/api.md` in `$SRC/app/.cartograph/api-docs.toml` (writing this file makes the local checkout dirty, so recovery records `workdir` as the source commit) and re-run the showcase command:

```sh
mkdir -p "$SRC/app/.cartograph"
cat > "$SRC/app/.cartograph/api-docs.toml" <<'EOF'
[[doc]]
path = "docs/api.md"
prefix = "/api"
EOF

cd src-tauri
CARTOGRAPH_SHOWCASE_SOURCES=$SRC/app:$SRC/browser-extension:$SRC/Simple-Login-Android \
CARTOGRAPH_SHOWCASE_OUTPUT=$OUT \
  cargo test --release showcase_recover_and_export -- --ignored --nocapture
```

Verified at Cartograph `d326a47e`, in `$OUT/verified-only/drift_register.md` under `## API contract drift`: **1 `documented-not-served`** (`POST /api/notifications/{}`) and **12 `served-undocumented`** routes under `/api`, each citing the documented route or the recovered endpoint, its mount chain and `docs/api.md`. Every entry is InferredStrong. Remove `.cartograph/` afterwards to restore the clean checkout and commit pinning.

## Known limits

- Swift is unsupported; no iOS client was ingested.
- There is no Terraform or other IaC in these repos. The topology shows only what the source proves: five observed resources (gap 3).
- Tests are read only for pytest and the Flask test client. Calls in helper functions are not followed, and a test reference is static: it does not prove that the test ran.
- Request inputs are recorded only from the handler body. Responses also follow repository wrapper decorators and dict-building helpers (#940), but not `make_response(…)`, decorator factories or helpers outside the repository, and nested response fields are not recovered.
