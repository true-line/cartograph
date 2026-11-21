---
"cartograph": patch
---

The Rust adapter now gives files wired in by `#[path = "…"] mod name;` the module they are declared as (so `super::` in a `*_tests.rs` file resolves), and Rust entry points are recovered: `fn main()` binaries (desktop app, gRPC server, HTTP server, command or process), `#[tauri::command]` IPC commands and tonic gRPC service methods.
