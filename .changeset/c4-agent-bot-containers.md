---
"cartograph": patch
---

The architecture diagrams now show more of a Go + Cloudflare system (#2065): a Cloudflare Worker config (`wrangler.toml`/`.json`/`.jsonc`) becomes a container named for the worker; a Go client whose base URL sits in a struct field or package constant (`c.API + path`) names its outside host; and a store reached through a REST client by its conventional URL variable (`KV_REST_API_URL`, `UPSTASH_REDIS_REST_URL`, `REDIS_URL`, ...) is drawn as a data store.
