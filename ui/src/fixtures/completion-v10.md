# Next-prompt investigation fixtures

The three `completion-v10-*.json` files contain actual `detail`, `result`, and
`events` returned by the production job-store readers after scripted workers
run against isolated retained-source fixtures. No live provider was called.

- `supported`: a captured source is acquired and the worker saves result 7.
- `insufficient`: retained capture objects are removed before execution; the
  worker reports unavailable evidence and saves an insufficient-evidence result.
- `terminal`: the worker fails without a result; history still reopens.

The Rust exporter is `next_prompt_export_scripted_wire_fixtures`. Run it explicitly
with `cargo test --locked -p app next_prompt_export_scripted_wire_fixtures -- --ignored`.
It writes to `/tmp/cartograph-708-wire/`. Inspect the outputs before copying them
here; fixture IDs come from real isolated stores and can vary between runs.

The UI admission tests and `ActualNextPromptWorkerReopened` story consume these
records without rewriting their identity hashes. These fixtures establish wire
compatibility and presentation, not real-provider acceptance.
