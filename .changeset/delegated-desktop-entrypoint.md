---
"cartograph": patch
---

A Rust desktop app whose executable only calls its library's `run()` (for example `fn main() { app::run(); }`) is now recovered as a desktop application rather than a command. The library function that builds the Tauri app is cited as the evidence (#1227).
