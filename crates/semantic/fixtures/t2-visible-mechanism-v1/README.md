# t2-visible-mechanism-v1

Author: Claude (trueline-claude-agent[bot]). Reviewer: none recorded. This is a visible mechanism fixture for #661 under `258-fixture-manifest-schema-v1` (schema 6d047790…, README 9c193da0…). It is not a holdout, not a quality corpus and not threshold evidence. The corpus `fixture/storefront` is synthetic.

Files: `inputs.json`, `leakage.json`, `labels.json`, the retained span bytes in `sources/`, and the stand-in canonical graph/capture reference documents in `corpora/` (JCS bytes, hashed by `graph_snapshot_hash`/`capture_manifest_hash`). The partition salt `cartograph-661-visible-mechanism-v1` and `calibration_buckets` 5000 were fixed before any split was computed.

| Case | Family | Exercises |
|---|---|---|
| c-call-unique / c-call-unique-r2 | Call/null | same call site across two revisions; one leakage group |
| c-call-no-match | Call/null | `no_match` with a source-proof exclusivity basis over a shared candidate set |
| c-call-same-name | Call/null | hard same-name negative (`sms.send`) proven by the import; non-proven non-target left unlabeled |
| c-call-multi-acceptable | Call/null | two acceptable targets (definition + re-export) |
| c-call-unknown | Call/null | `unknown`, null exclusivity basis, no hard negatives |
| c-call-empty-universe | Call/null | empty candidates → no-candidate abstention |
| c-channel-sqs | Channel/sqs-queue | PUBLISHES; hard negative proven by the env binding |
| c-channel-sns | Channel/sns-topic | SUBSCRIBES; hard negative proven by the env binding |
| c-channel-null-subtype | Channel/null | distinct null-subtype family; must never pool with sqs-queue/sns-topic |

Splits: calibration = g-catalog-plugins, g-legacy-bus, g-notify, g-orders-queue, g-payments-topic. Evaluation = g-billing, g-i18n, g-webhooks. Per-family coverage is deliberately thin, so a gate must report insufficiency rather than pass.

## Negative mutations (each must be rejected; apply one at a time)

1. Move `c-call-unique-r2` into a new group of its own whose split differs from `g-billing`. Expected: rejected as related lineage straddling splits.
2. Change any group's `split` to the other value. Expected: rejected on split recomputation.
3. Set one candidate of `c-channel-sqs` to family `Channel/null`. Expected: rejected on family mismatch.
4. Add `channel:sqs-queue:orders-created` to `c-channel-sqs` hard negatives. Expected: rejected on overlap with the acceptable set.
5. Give `c-call-unknown` a hard negative or a non-null `exclusivity_basis`. Expected: rejected.
6. Flip one byte in `sources/s-email-send.txt`. Expected: rejected on content hash, and dependent manifests invalid.
7. Drop `fn:billing/refund.ts#refundCard` from `c-call-unique` candidates without updating `candidate_set_hash`. Expected: rejected.
8. Duplicate a JSON key anywhere in `labels.json`. Expected: rejected before semantic checks.
9. Replace `bytes_path` with `../sources/x.txt`, an absolute path, or a symlink. Expected: rejected by the confined reader.
10. Remove the label for `c-call-empty-universe`. Expected: rejected on coverage.
