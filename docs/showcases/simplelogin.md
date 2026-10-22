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

Before Flask Blueprint support landed with this showcase, recall was **0 / 45**. Every API route is declared as `@api_bp.route(...)` on a Blueprint mounted with `url_prefix="/api"`, and the Python adapter recognised only `@app.route`. Blueprint and FastAPI `APIRouter` prefixes are now composed with their mounts, including mounts in other files. A prefix that can't be read becomes a `Gap`, and a router with no observed mount stays `InferredStrong`, marked `mount_not_observed`. Nested mounts compose their whole chain (up to 16 levels). A cyclic chain, or a prefix built by `**kwargs` or `*args` expansion, becomes a `Gap` rather than a guessed path (`crates/adapters-lang-python/src/lib.rs`, `served_prefixes`).

**The one miss is a documentation error.** `docs/api.md` documents `POST /api/notifications/:notification_id`, but the code registers `POST /api/notifications/<int:notification_id>/read` ([`app/api/views/notification.py:63`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/notification.py#L63)). Cartograph recovered the route the code actually serves.

**The 12 undocumented recoveries are real routes that are missing from the docs.** Examples:

- `GET /api/aliases` and `POST /api/aliases` ([`alias.py:39`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/alias.py#L39));
- `GET /api/mailboxes` ([`mailbox.py:165`](https://github.com/simple-login/app/blob/4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798/app/api/views/mailbox.py#L165));
- `POST /api/v2/alias/custom/new` and `DELETE /api/setting/unlink_proton_account`.

The scorer lists all 12.

All 248 recovered endpoints are Flask. This includes the web UI, OAuth and admin routes outside `/api`. Their confidence follows how much of the mount chain is proven:

- **9 `Confirmed`**: declared directly on the app object created in `create_app()`.
- **239 `InferredStrong`, marked `mount_receiver_annotated`**: SimpleLogin registers its Blueprints inside `register_blueprints(app: Flask)`. The receiver is a parameter annotated as `Flask`, not an app constructed in that function, so the type annotation is the only evidence that it is the served app. Cartograph composes the prefix, but it doesn't claim the mount is proven.

No route was promoted to `Confirmed` from a receiver it couldn't prove.

## What else was recovered

| Source | Producers that ran | Nodes | Edges | Coverage |
|---|---|---|---|---|
| app | TypeScript/JS (49 files), Python (717 files), toolchain | 24,818 | 34,240 | limited: no `otel_jsonl` declared, so 250 endpoint and channel facts stay T0 |
| browser-extension | TypeScript/JS (28 files), WebExtension manifest, toolchain | 593 | 679 | limited |
| Simple-Login-Android | Kotlin (195 files), toolchain | 1,357 | 3,262 | complete for the producers that ran |

Source: `ingest.json`, coverage producers.

The bundle has 26,910 assertions, 7,512 open gap findings in 25 cause classes and 3 security findings (`bundle.json`, `gap_register.md`, `security.md`). The security findings are three over-broad host permissions in the extension manifest: `http://*/*`, `https://*/*` and `https://*.simplelogin.io/*`.

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
2. **Data model: resolved by #738.** `app/models.py` declares 80 SQLAlchemy models. Before #738, `data_model.md` reported *No DataEntity facts have been recovered yet*. With the SQLAlchemy producer, all 80 become Confirmed `DataEntity` facts with cited columns, linked by 82 `REFERENCES` edges, one per entity pair. Five `ForeignKey` targets that are not string literals stay `Gap`s. The 275 Alembic revisions in `migrations/versions/` are not read yet. Migration history is a separate producer. (The node and edge totals in the table above were measured before #738.)
3. **No infrastructure.** `topology.md` is empty. The repo has a `Dockerfile`, `crontab.yml`, `example.env` and `alembic.ini`. None of these is infrastructure-as-code that Cartograph reads today, so the Infrastructure layer correctly shows its empty state. *Fill:* the observed-infrastructure producer (#734) plus container and cron config readers.
4. **Documentation drift is not reported.** The notification mismatch above, and the 12 undocumented routes, were found by the external scorer. Cartograph's own `drift_register.md` reports 0, because it doesn't take documentation in as a contract. *Fill:* a documented-contract input (Markdown API reference or OpenAPI) compared against recovered routes, with drift cited on both sides (#739). For enterprises with outdated docs, this is the direct value.
5. **No user stories.** `user_stories.md` reports *No Capability facts have been recovered yet*. Capabilities need the interpretation tier, which this deterministic run excludes by design.
6. **Sources are recorded as `workdir`, not by commit.** All three are git checkouts at the commits above, but each coverage record shows `commit: "workdir"` (`ingest.json`). Citations therefore can't pin the commit they came from. Stamping HEAD on a clean checkout would overclaim: working-tree capture is not a commit snapshot (SPEC-04). *Fill:* recover a Git source at a commit through the existing Git-object capture, so producers read that commit's blobs (#740).
7. **TypeScript rule gaps dominate the count but not the impact.** Classes C-01 to C-09 account for 6,910 of the 7,512 gap findings. They are unresolved interpretation (return/throw consumers, predicates, mutation, loops), routed to T1, and they don't affect the HTTP contract.
8. **Unresolved module edges.** There are 177 in Python (C-10) and 69 in Kotlin (C-12), plus 38 unresolved Kotlin imports (C-15). These are mostly third-party packages that aren't vendored in the checkout.

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

Expected result to verify in `$OUT/verified-only/drift_register.md` under `## API contract drift`: **1 `documented-not-served`** (`POST /api/notifications/{}`) and **12 `served-undocumented`** routes under `/api`, each citing the documented route or recovered endpoint.

## Known limits

- Swift is unsupported; no iOS client was ingested.
- There is no Terraform or other IaC in these repos, so the empty topology is expected (gap 3).
- Request parameters and response shapes were not scored. The scorer compares methods and paths only.
