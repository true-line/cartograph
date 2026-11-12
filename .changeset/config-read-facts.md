---
"cartograph": minor
---

Environment variables a system reads in source are now recovered as facts in every built-in language adapter: `process.env.X` (TypeScript and JavaScript), `os.environ` / `os.getenv` (Python), `os.Getenv` / `os.LookupEnv` (Go), `System.getenv` (Java and Kotlin) and `std::env::var` (Rust). Each fact records the key name and where it is read, never a value; a read whose key is computed at runtime is recorded without a name. Re-ingesting a repository adds these facts to its graph. The exported documents do not list them yet. Preflight and Settings show a `config_reads` coverage cell per language, stating what each adapter does not read.
