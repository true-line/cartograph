# Vendure cart readiness: D1 rule score (#590)

**All 8 frozen rules are now stated in D1 with their predicate, scope, configuration gate and consequence: 8/8 rules stated, up from 0/8.** The 5 entry guards are confirmed local observations. The 3 supporting stock rules are confirmed in their own callables, but D1 links them to the stock guard by inference: by method name, and through the configured strategy. All 41 decision cases remain unexecuted. This is a rule-coverage result, not a decision-fidelity pass.

## Run

| Item | Value |
|---|---|
| Target | `vendurehq/vendure` at `2cc309deb5394af176314552ac7320a7b26a0104` (the pinned commit in the [oracle](vendure-cart-readiness.md)) |
| Oracle | [expected set](vendure-cart-readiness.expected.json), SHA-256 `dcd9833e8f9be2269a6711f8bd04119c83931f6de45e04d17d1b99da9d51295d`, unchanged |
| Cartograph | #2370 head `a891129575ac4b08b195641d56e65ed9c9baa0c4` (merged as `5d604d0413ecf798f42411a930e40ac5122d0d44`) |
| Run | `showcase_tests::showcase_recover_and_export` over the pinned checkout, default configuration |
| Business rules | 225 (211 before #590 R05), with no capture stops |
| D1 | `best-effort/product-specification.md`, 176,945 bytes |
| Determinism | Two fresh runs agree byte for byte once the run-local source id (`local/src_…`) and the ids and hashes derived from it are normalized. |

The score below reads D1 itself, not the extractor or the oracle's code.

## Rules

| ID | D1 statement (abridged) | Result |
|---|---|---|
| VEN-READY-01 | "When the options's check all variants exist is not false and the from state is “AddingItems” and the to state is not “Cancelled” and the lines's length is more than 0, and the available variants's length is not the variant ids's length, the move is refused … Here the available variants are the product variants that are not deleted and whose product is not deleted, among the variant ids." | Stated, confirmed. D1 does not say the variant ids are distinct. |
| VEN-READY-02 | "When the to state is “ArrangingPayment”, and the options's arranging payment requires contents is not false and the lines's length is 0, the move is refused with the message “cannot transition to payment when order is empty”." | Stated, confirmed. |
| VEN-READY-03 | "… arranging payment requires customer is not false and the order's customer is not set, the move is refused with the message “cannot transition to payment without customer”." | Stated, confirmed. |
| VEN-READY-04 | "… arranging payment requires shipping is not false and either the order's shipping lines is not set or the shipping lines's length is 0, the move is refused …" | Stated, confirmed. Undefined and empty are both covered. |
| VEN-READY-05 | "… arranging payment requires stock is not false, and the variants with insufficient saleable stock are not empty, the move is refused. Here the variants with insufficient saleable stock start empty, and when … the line's quantity is more than the available stock, the line's product variant is added to the variants with insufficient saleable stock." | Stated, confirmed. Strict "more than" keeps equality passing, and the comparison is per line. The rejection message (a `ctx.translate` call naming the variants) is not rendered. |
| VEN-READY-06 | "↳ When the variant's track inventory is “FALSE” or both the variant's track inventory is “INHERIT” and the track inventory is false, the product variant's saleable stock level is the largest safe integer (9007199254740991)." | Stated. Confirmed in its own callable, but linked to the guard by method name (inferred). |
| VEN-READY-07 | "↳ Otherwise, the product variant's saleable stock level is the stock on hand minus the stock allocated minus the effective out of stock threshold. Here the effective out of stock threshold is the out of stock threshold when the variant's use global out of stock threshold is set, and otherwise the variant's out of stock threshold." | Stated. Same link as 06. |
| VEN-READY-08 | "↳ MultiChannelStockLocationStrategy.getAvailableStock: When the stock level applies to active channel, the stock level's stock on hand is added to the stock on hand." The same statement is made for stock allocated. | Stated, inferred. The strategy is linked from `defaultConfig.catalogOptions.stockLocationStrategy` (InferredStrong). "Applies to active channel" is read from the method name `stockLevelAppliesToActiveChannel`, and its body (channel membership) is not followed. |

Under the oracle's gate ("inferred proposals do not count as confirmed matches"), confirmed coverage is 5/5 entry guards. The supporting rules are 2/3 confirmed in their own callables and 1/3 inferred; none of the three is confirmed end-to-end from the guard.

## Still open

- The 41 decision cases have not been executed. Rejection precedence (first rejection in source order) is visible from row order, but D1 does not state it.
- The VEN-READY-05 rejection text and its variant names are not rendered.
- The body of `stockLevelAppliesToActiveChannel` and the active channel's cached membership are not followed.
- Configuration overrides beyond the `!== false` gates (custom processes, another stock strategy) are out of this slice, as the oracle states.

## History

- [Baseline](vendure-cart-readiness.baseline.md): 0/8 complete.
- [Local definitions](vendure-cart-readiness.local-definitions.score.md): 0/8, with more local evidence.
- This score: 8/8 stated after #2340, #2345, #2348 and #2370 (AC-1044 to AC-1054).
