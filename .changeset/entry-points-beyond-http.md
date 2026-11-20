---
"cartograph": minor
---

Capabilities now cover work that no HTTP request starts. Each process start becomes an entry point with a kind read from the source: a Python `__main__` block or operator script (SMTP mail listener, database `LISTEN` listener, queue consumer, polling loop, operator shell, command), the container command, a Procfile process, or a declared console script or `bin`. Each entry point, and each schedule file's jobs, sits under a capability in the product specification's §1.3, and its traced flow is in §1.6. An entry point that cannot be resolved is recorded as a gap and named, not dropped. On SimpleLogin, mail receiving and forwarding, the job runner, the event listener, the monitoring loop, 17 operator commands, the operator shell and 18 scheduled jobs now appear as 12 capabilities. Before this change none of them appeared.
