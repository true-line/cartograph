---
"cartograph": minor
---

Lets an outside agent such as Codex or Claude Code start a drafting run of the in-app section writer over MCP. A session with the task grant starts `section-writer@1` for one drafted catalogue section with `cartograph_task_start`, and `cartograph_task_result` says whether the run staged a draft and gives its id. The draft is written by the role in the app, passes the same admission as a run started in the app, and waits for human review there.
