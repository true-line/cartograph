---
"cartograph": patch
---

The traceability gate now checks that mapped tests actually run in CI. A test id whose every bound Rust test is `#[ignore]`d fails unless its row names a manual procedure or a CI lane that runs ignored tests, and test ids with some ignored tests are listed. Story statuses are checked against the Status enum; US-0043 moves from the undefined `Planned` to `In-Progress`. T-0329, whose opt-in relay tests no lane runs yet, is recorded in a baseline that can only shrink.
