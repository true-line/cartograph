---
"cartograph": minor
---

Scheduled jobs are now flow triggers. A yacron-format schedule file (`jobs:` with `schedule` and `command`) makes each job a `Schedule` trigger, which traces into the file the job runs and from there into its symbols and calls. The entry is Confirmed when the root `Dockerfile` proves where the repository sits in the container; otherwise it is marked as inferred, or kept as a Gap naming the job (#770).
