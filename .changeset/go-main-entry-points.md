---
"cartograph": patch
---

A Go `func main()` in `package main` is now an entry point (#2019): an HTTP server when it serves HTTP, a command under `cmd/` or an operator directory, a process otherwise. The architecture view names a `main.go` program for its directory.
