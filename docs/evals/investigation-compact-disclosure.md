# Compact specialist input: matched acquisition measurement

Issue #596 · US-0032 · AC-0236 · 2026-10-19

The deterministic fixture in
`crates/agents/src/investigation/sealing_tests.rs`,
`compact_disclosure_matched_payload_measurement_keeps_unobserved_metrics_unknown`,
prepares @2 cumulative and @3 compact payloads from the same question, ledger,
three identical query pages, empty history, empty source list and usage counters.
The repeated fact includes original properties and producing-method metadata.
Both preparations retain the identical acquisition fingerprint:
`2157502ffba2f22455131139648dfa1a7db26d03d007f7282df2a25bd54255bd`.

| Metric | Cumulative @2 | Compact @3 |
| --- | ---: | ---: |
| Serialized full CompletionPayload bytes | 19045 | 10868 |
| Provider-reported input tokens | unknown | unknown |
| Provider-reported output tokens | unknown | unknown |
| Provider call count | unknown | unknown |
| Provider latency | unknown | unknown |

No provider was executed for this comparison. The byte counts include the
versioned system instructions and JSON escaping; they are not token estimates or
request-body counts. This fixture establishes savings when complete query
representations repeat, not a universal percentage or a provider efficacy claim.
The test prints the measurements as JSON and asserts exact acquisition equality
and a smaller compact payload. Run it with:

```sh
cargo test -p agents compact_disclosure_matched_payload_measurement -- --nocapture
```

The default keeps every unique graph representation and acquired source span.
Differing representations of the same typed fact are preserved. A separately
tested host-selected source working set records an explicit reason and original
citation/fact/role/index handle for each omitted source. That handle cannot support
a source citation. No source acquisition is inferred from compaction.

Controlled real-provider acceptance belongs to #598 / AC-0191. Neither this
measurement nor scripted worker tests satisfy that acceptance or unexecuted H4/H5
obligations.
