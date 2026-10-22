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
# in each. $SRC is that directory and $OUT a new, empty one.
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

On an M4 laptop, ingesting all three repos took 17.3 s, measured with an already built release test binary. The first build takes about 20 minutes. Peak memory was not measured on this run.

## Score

| Metric | Value |
|---|---|
| Documented endpoints | 45 |
| Recovered under `/api` | 56 |
| Matched | 44 |
| **Recall** | **44 / 45 (97.8%)** |
| Documented share of recovered | 44 / 56 (78.6%) |

Before Flask Blueprint support landed with this showcase, recall was **0 / 45**. Every API route is declared as `@api_bp.route(...)` on a Blueprint mounted with `url_prefix="/api"`, and the Python adapter recognised only `@app.route`. Blueprint and FastAPI `APIRouter` prefixes are now composed with their mounts, including mounts in other files. A prefix that can't be read becomes a `Gap`, and a router with no observed mount stays `InferredStrong`, marked `mount_not_observed` (`crates/adapters-lang-python/src/lib.rs`, `resolve_routes`).

**The one miss is a documentation error.** `docs/api.md` documents `POST /api/notifications/:notification_id`, but the code registers `POST /api/notifications/<int:notification_id>/read` ([`app/api/views/notification.py:63`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/notification.py#L63)). Cartograph recovered the route the code actually serves.

**The 12 undocumented recoveries are real routes that are missing from the docs.** Examples:

- `GET /api/aliases` and `POST /api/aliases` ([`alias.py:39`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/alias.py#L39));
- `GET /api/mailboxes` ([`mailbox.py:165`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/mailbox.py#L165));
- `POST /api/v2/alias/custom/new` and `DELETE /api/setting/unlink_proton_account`.

The scorer lists all 12.

All 248 recovered endpoints are Flask, and all are `Confirmed`: 239 mounted through a Blueprint and 9 declared directly on the app. This includes the web UI, OAuth and admin routes outside `/api`.

## What else was recovered

| Source | Producers that ran | Nodes | Edges | Coverage |
|---|---|---|---|---|
| app | TypeScript/JS (49 files), Python (717 files), toolchain | 24,818 | 34,240 | limited: no `otel_jsonl` declared, so 250 endpoint and channel facts stay T0 |
| browser-extension | TypeScript/JS (28 files), WebExtension manifest, toolchain | 593 | 679 | limited |
| Simple-Login-Android | Kotlin (195 files), toolchain | 1,357 | 3,262 | complete for the producers that ran |

Source: `ingest.json`, coverage producers.

The bundle has 26,910 assertions, 7,512 open gap findings in 25 cause classes and 3 security findings (`bundle.json`, `gap_register.md`, `security.md`). The security findings are three over-broad host permissions in the extension manifest: `http://*/*`, `https://*/*` and `https://*.simplelogin.io/*`.

## Gaps, ranked by impact

1. **No client-to-server links across the three repos.** Both clients call the server, but neither call site becomes a fact that can be matched to a recovered endpoint:
   - The Android app declares its API as Retrofit annotations (`@GET("api/…")`, `@POST("api/…")` in `core/network/src/main/java/io/simplelogin/core/network/ApiService.kt`), and the Kotlin adapter does not recognise them.
   - The extension keeps a `{method, path}` table (`src/popup/APIService.js:7` onward) and joins it to a configured base URL at runtime, so its calls end as `runtime-computed fetch URL` gaps (class C-17, 11 instances).

   Cross-repo stitching (`t0.http-cross-repo`) exists, but it needs a client fact plus a system manifest binding. *Fill:* adapter work (Retrofit; table-driven client maps, #737) and a system manifest for the showcase. This gap blocks the "one app, several repos" view.
2. **No data model.** `app/models.py` declares 80 SQLAlchemy models (`class …(Base, …)`), and `migrations/versions/` holds 275 Alembic files. Yet `data_model.md` reports *No DataEntity facts have been recovered yet*. *Fill:* a Python ORM producer for SQLAlchemy declarative models and Alembic revisions (#738).
3. **No infrastructure.** `topology.md` is empty. The repo has a `Dockerfile`, `crontab.yml`, `example.env` and `alembic.ini`. None of these is infrastructure-as-code that Cartograph reads today, so the Infrastructure layer correctly shows its empty state. *Fill:* the observed-infrastructure producer (#734) plus container and cron config readers.
4. **Documentation drift is not reported.** The notification mismatch above, and the 12 undocumented routes, were found by the external scorer. Cartograph's own `drift_register.md` reports 0, because it doesn't take documentation in as a contract. *Fill:* a documented-contract input (Markdown API reference or OpenAPI) compared against recovered routes, with drift cited on both sides (#739). For enterprises with outdated docs, this is the direct value.
5. **No user stories.** `user_stories.md` reports *No Capability facts have been recovered yet*. Capabilities need the interpretation tier, which this deterministic run excludes by design.
6. **Sources are recorded as `workdir`, not by commit.** All three are git checkouts at the commits above, but each coverage record shows `commit: "workdir"` (`ingest.json`). Citations therefore can't pin the commit they came from. *Fill:* record the HEAD commit when a local source is a clean git checkout (#740).
7. **TypeScript rule gaps dominate the count but not the impact.** Classes C-01 to C-09 account for 6,910 of the 7,512 gap findings. They are unresolved interpretation (return/throw consumers, predicates, mutation, loops), routed to T1, and they don't affect the HTTP contract.
8. **Unresolved module edges.** There are 177 in Python (C-10) and 69 in Kotlin (C-12), plus 38 unresolved Kotlin imports (C-15). These are mostly third-party packages that aren't vendored in the checkout.

## Known limits

- Swift is unsupported; no iOS client was ingested.
- There is no Terraform or other IaC in these repos, so the empty topology is expected (gap 3).
- Request parameters and response shapes were not scored. The scorer compares methods and paths only.
