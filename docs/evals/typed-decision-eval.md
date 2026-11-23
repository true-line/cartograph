# Typed-decision eval (#429)

This is the harness for measuring a typed-decision provider, such as TypeSafe
Jev, on candidate selection. The decision behind it is in ADR-0091.

## What it measures

Each labelled case is one unresolved hop: its cited context, its resolved
candidates and its acceptable targets (an empty set means no match). The
provider gets one choice question per case, over the candidates plus `none`.

For each run, `semantic::decision_eval::report` gives:
- **precision@1** over the cases answered;
- **calibration:** a 10-bin table of mean confidence against observed
  accuracy, plus the expected calibration error;
- **latency:** p50 and p95 of each call's wall time;
- **cost:** reported input tokens, and their cost at the price passed in;
- **local baseline:** the local embedding resolver's top-1 precision on the
  same cases.

A case whose consent is refused is counted as refused, and no call is made
for it.

## Status

There is no live run. Jev is in limited early access and the project has no
access or key. `crates/semantic/fixtures/decision-recorded-v1` holds six
hand-authored cases with recorded answers. It shows the harness and the
firewall work; it is not an accuracy measurement.

## Running it live

ADR-0091 lists what a live run needs: access and a key in the OS secure
store, a Jev `TypedDecisionProvider`, the frozen #258 labels mapped into
cases, consent per case, and the vendor's then-current price. The report goes
in this folder next to this file, and #429 closes with the ADR follow-up that
adopts or rejects Jev.
