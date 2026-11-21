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

## Scheduled watch (#1768)

The **Scheduled checks** workflow runs every lane above on the first of each month, or on demand (**Run workflow**, lane `scale-showcase`), on the self-hosted Mac: `node scripts/showcase-watch.mjs scale` runs `scripts/bench-ingest.mjs` and compares each lane's endpoints, wall time and peak memory with [`bench/thresholds/scale.json`](../../bench/thresholds/scale.json). A missed threshold fails the lane and opens or updates the issue "Scheduled: scale showcase below its thresholds" (AC-1700). The thresholds start from the numbers on this page, with wide ceilings for time and memory; the first scheduled run's `metrics.json` artifact, which states the Cartograph commit and version it measured, is the source for recording the full baseline and for updating this page.

## Results

Measured at Cartograph `151a403b` (a commit that is not on `main`; its version was not recorded) on an M4 laptop (darwin-arm64), with the release test binary already built.

| Lane | Files | Wall | Peak RSS | Peak footprint | Nodes | Edges | `Gap` nodes | Endpoints | Rules | Receipts |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| simplelogin | 766 | 58.0 s | 1.00 GiB | 0.69 GiB | 26,069 | 37,757 | 7,041 | 426 | 1,169 | 12,642 |
| rea | 5,493 | 298.5 s | 3.91 GiB | 5.99 GiB | 310,343 | 437,800 | 161,516 | 34 | 26,262 | 37,025 |
| vendure | 940 | 18.0 s | 1.23 GiB | 0.55 GiB | 29,124 | 46,997 | 7,812 | 374 | 1,414 | 18,076 |
| openmrs-core | 1,488 | 11.5 s | 1.29 GiB | 0.82 GiB | 28,190 | 45,839 | 3,761 | 6 | 542 | 3,188 |
| kafka | 6,453 | 89.7 s | 2.64 GiB | 4.90 GiB | 105,947 | 227,627 | 6,449 | 0 | 0 | 0 |
| kubernetes | 8,758 | 69.7 s | 1.54 GiB | 1.90 GiB | 80,947 | 151,259 | 100 | 53 | 12 | 113 |

**Timing caveat.** A full workspace test suite was running on the same machine during this run, so the wall times above are likely high. Earlier runs at the same pins, with the machine otherwise idle, measured at Cartograph `dcb68037`:

| Lane | Wall | Peak RSS | Peak footprint |
|---|---:|---:|---:|
| openmrs-core | 8.95 s | 1.28 GiB | 0.86 GiB |
| kafka | 70.35 s | 3.18 GiB | 4.93 GiB |
| kubernetes | 27.9 s | 1.82 GiB | 1.92 GiB |

The node, edge, `Gap` node, endpoint, rule and receipt counts for openmrs-core, kafka and kubernetes were identical across the two commits.

Memory is binary: 1 GiB = 2^30 bytes. `Gap` nodes are graph facts. They are not the gap-register finding counts below, which group instances by class.

**Receipt omissions.** On rea, 107,022 nodes and 11,217 edges are in the graph but have no stored source receipt. The TypeScript adapter's receipt budget gives precedence to rules over declarations, and it records each omitted fact in `source_node_omissions` / `source_edge_omissions` (`receipt_omissions` in the results JSON). So rea's receipt count does not cover its whole graph. The other lanes had no omissions.

## What the gap registers say

- **TypeScript rule evidence dominates** simplelogin, rea, vendure and openmrs-core. On rea it is 161,694 findings covering 319,326 instances. This is the main #725 cost driver.
- **Kafka**'s gaps are Java overload, import and module resolution classes (3,165 / 2,818 / 2,009 findings, and so on). Preflight names Scala (255 files) as a language with no adapter.
- **Kubernetes** has 105 findings, including 23 Go route handler expressions that could not be resolved and 12 CronJob `workingDir` values.

## Findings

- **Test sources counted as product endpoints (#1250).** Five of openmrs-core's six Spring endpoints come from `src/test/java`.
- **Languages without an adapter were silently omitted (#1251).** Fixed: preflight now names each one with an uncovered-language finding. Kafka's Scala files are the visible case.
- **JAX-RS endpoints were neither recovered nor reported (#1257).** Kafka has 49 JAX-RS files and recovered 0 endpoints, with no gap saying so. Fixed in #1264: at Cartograph `7b57678d` the same Kafka pin recovers 43 JAX-RS endpoints (29 in Connect runtime, 14 in Trogdor). It also emits 2 route Gaps for `InternalClusterResource` methods, which are served only through subclasses. One of the 43 is a test-source resource in Connect's integration tests, the #1250 class. The results table above predates this fix.
- **The Go adapter silently skips files (#1258).** Preflight counts 17,994 Go files in Kubernetes. Of those, 4,339 are vendored and most of the rest are `_test.go` files, which leaves 10,073 non-vendor, non-test files. About 145 of them are platform-suffixed and about 1,179 carry a `//go:build` constraint, and the adapter skipped both kinds without disclosure. The other 8,758 were recovered. Fixed in #1262: preflight now names both skip classes with their counts. Re-measured at `7b57678d`, the recovered Kubernetes graph counts are unchanged.

## Limits

- No T2 or T3 tiers ran. These are deterministic costs only.
- These are separate lanes on a single machine, not a 50-repo estate in one profile.
- Wall times are from a loaded machine (see the caveat above). Rerun on an idle machine before quoting them as numbers to hold to.
