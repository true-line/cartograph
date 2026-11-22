# ADR-0088 — The desktop app is a library with a one-line executable

- **Status:** Accepted (coordinator as the owner's delegate, 2026-11-22)
- **Date:** 2026-11-22
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #1227; Discussion #1221; #1924 (CI time and crate splits)

## Context

The `app` package was one binary crate whose root, `src-tauri/src/main.rs`,
had grown to about 16,500 lines: every module declaration, the Tauri
commands, the application state and the `main` that builds and runs the app,
with the unit tests of the whole package compiled into the binary's test
target. Discussion #1221 asked for a main/library separation so the
executable consumes the application rather than being it.

## Decision

1. **The application is the `app` library.** `src-tauri/src/main.rs` becomes
   the library root (`[lib] path = "src/main.rs"`), unchanged apart from the
   entry function. Module declarations, relative paths, private items, test
   gates and test module paths are kept, so test names stay the same. Moving
   the file to the conventional `src/lib.rs` is a later, rename-only change.
2. **`run()` is the entry point.** The former `fn main()` becomes the
   documented `pub fn run()`.
3. **A one-line executable.** `src-tauri/src/bin/app.rs` calls `app::run()`
   and nothing else. It carries the Windows `windows_subsystem` attribute,
   which only applies to an executable. The binary keeps the name `app`, so
   the Cargo package, its dependencies and the Tauri configuration are
   unchanged.
4. **Tests run on the library target.** The package's unit tests are the
   library's: CI and the scheduled workflow run `cargo test -p app --lib`
   where they ran `--bin app`. A test that starts `current_exe()` starts the
   library test binary, as it started the binary's test harness before.

## Consequences

- The executable no longer owns application code, and later work can move
  pieces of the library into their own crates (#1924) without touching the
  entry point.
- This does not extract an engine crate, add a headless mode or change any
  runtime behavior.
- The library root keeps the name `main.rs` until the rename lands, so open
  branches that edit it are unaffected now.
- Historical references to `main.rs` in accepted ADRs are left as written.
