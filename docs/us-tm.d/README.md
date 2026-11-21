# docs/us-tm.d — US-TM fragments

Rows here are part of the traceability matrix in [`../US-TM.md`](../US-TM.md).
`scripts/check-traceability.mjs` reads `US-TM.md` and every fragment in this
directory as one matrix.

Shared rows in `US-TM.md` (US-0002 most of all) change in almost every PR, and
parallel PRs that edit neighbouring rows conflict. To add new ACs, **add a new
file here instead of editing a row**:

- `AC-NNNN.md`, named after the first AC it maps, holds one or more US rows in
  the `US-TM.md` column layout:

  ```
  | US-0003 | AC-1500..1501 | iac, dynamic | M6 | — | ADR-0077 | T-1500..1501 |
  ```

  The story must already have its row in `US-TM.md`. Its ACs, ADRs and test ids
  are added to that story. Each AC is mapped by exactly one row, and each row
  pairs its AC and T ranges 1:1 in order.
- `ADR-NNNN.md` holds the ADR index row of a new ADR, in the `US-TM.md` ADR
  index layout (`| ADR-NNNN | Summary | Status | US-… |`).

Fragments can be folded into `US-TM.md` in a quiet moment. The check does not
care where a row lives.
