# Scale: a deterministic showcase

**Question.** How long does Cartograph take, and how much memory does it use, to ingest six real repositories of very different sizes with no model calls? And what does it report that it could not recover?

Only deterministic tiers ran: no T2 embeddings, no T3 provider calls and no network. Tracking issue: #1256. The #725 scale numbers belong to the owner and are not changed here. This page reports measurements only.

## Inputs

The lanes are pinned in [`bench/repos.toml`](../../bench/repos.toml).

| Lane (repo) | Commit | Main languages |
|---|---|---|
| [simple-login/app](https://github.com/simple-login/app) | `4f212552d4f8ba31e2ef8f4a8e52a642ee0f9798` | Python (Flask), JS |
| [morluto/rea](https://github.com/morluto/rea) | `258a1dd3b7d867264655e5a69e6364921e3d0c79` | TypeScript |
| [vendure-ecommerce/vendure](https://github.com/vendure-ecommerce/vendure) | `2cc309deb5394af176314552ac7320a7b26a0104` | TypeScript |
| [openmrs/openmrs-core](https://github.com/openmrs/openmrs-core) | `0755f785f7d237a7a2e9d9f4e76aff43b7613ad3` | Java (Spring) |
| [apache/kafka](https://github.com/apache/kafka) | `7596c4bc4b252083fa2fec8da798d16dc988f077` | Java, Scala |
| [kubernetes/kubernetes](https://github.com/kubernetes/kubernetes) | `7f2897a0f477bcd9a78a6ec9fea35dffb639510a` | Go |

## Reproduce

```sh
# $WORK is a scratch directory; the harness clones the pinned commits into it.
node scripts/bench-ingest.mjs --work $WORK            # all lanes
node scripts/bench-ingest.mjs --work $WORK --only kafka
```

The harness builds the release showcase test binary once. It then runs each lane through the production ingest path against an isolated profile and writes a JSON result. Each result records the measurements (`wall_seconds`, `max_rss_bytes`, `peak_footprint_bytes`) and the deterministic counts.

## Results

Measured at Cartograph `151a403b` on an M4 laptop (darwin-arm64), with the release test binary already built.

| Lane | Files | Wall | Peak RSS | Peak footprint | Nodes | Edges | Gap findings | Endpoints | Rules | Receipts |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| simplelogin | 766 | 58.0 s | 1.00 GB | 0.69 GB | 26,069 | 37,757 | 7,041 | 426 | 1,169 | 12,642 |
| rea | 5,493 | 298.5 s | 3.91 GB | 5.99 GB | 310,343 | 437,800 | 161,516 | 34 | 26,262 | 37,025 |
| vendure | 940 | 18.0 s | 1.23 GB | 0.55 GB | 29,124 | 46,997 | 7,812 | 374 | 1,414 | 18,076 |
| openmrs-core | 1,488 | 11.5 s | 1.29 GB | 0.82 GB | 28,190 | 45,839 | 3,761 | 6 | 542 | 3,188 |
| kafka | 6,453 | 89.7 s | 2.64 GB | 4.90 GB | 105,947 | 227,627 | 6,449 | 0 | 0 | 0 |
| kubernetes | 8,758 | 69.7 s | 1.54 GB | 1.90 GB | 80,947 | 151,259 | 100 | 53 | 12 | 113 |

**Timing caveat.** A full workspace test suite was running on the same machine during this run, so the wall times above are likely high. Earlier runs at the same pins with the machine otherwise idle:

| Lane | Wall | Peak RSS | Peak footprint |
|---|---:|---:|---:|
| openmrs-core | 8.95 s | — | — |
| kafka | 70.35 s | 3.42 GB | 5.29 GB |
| kubernetes | 27.9 s | 1.96 GB | 2.06 GB |

The node, edge, gap, endpoint, rule and receipt counts for openmrs-core, kafka and kubernetes were identical across runs.

## What the gap registers say

- **TypeScript rule evidence dominates** simplelogin, rea, vendure and openmrs-core. On rea it is 161,694 findings covering 319,326 instances. This is the main #725 cost driver.
- **Kafka**'s gaps are Java overload, import and module resolution classes (3,165 / 2,818 / 2,009 findings, and so on). Preflight names Scala (255 files) as a language with no adapter.
- **Kubernetes** has 105 findings, including 23 Go route handler expressions that could not be resolved and 12 CronJob `workingDir` values.

## Findings

- **Test sources counted as product endpoints (#1250).** Five of openmrs-core's six Spring endpoints come from `src/test/java`.
- **Languages without an adapter were silently omitted (#1251).** Fixed: preflight now names each one with an uncovered-language finding. Kafka's Scala files are the visible case.
- **JAX-RS endpoints are neither recovered nor reported (#1257).** Kafka has 49 JAX-RS files and 0 recovered endpoints, and no gap says so.
- **The Go adapter silently skips files (#1258).** Preflight counts 17,994 Go files in Kubernetes. Of those, 4,339 are vendored and most of the rest are `_test.go` files, which leaves 10,073 non-vendor, non-test files. About 145 of them are platform-suffixed and about 1,179 carry a `//go:build` constraint, and the adapter skips both kinds without disclosure. The other 8,758 were recovered.

## Limits

- No T2 or T3 tiers ran. These are deterministic costs only.
- These are separate lanes on a single machine, not a 50-repo estate in one profile.
- Wall times are from a loaded machine (see the caveat above). Rerun on an idle machine before quoting them as numbers to hold to.
