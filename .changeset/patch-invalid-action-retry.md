---
"cartograph": patch
---

An @29 finish patch that fails projection no longer ends the run. It is journaled as an invalid_action refusal with its rejection code, and the patch step is offered again, within the run's allowance of 2 (#2431, AC-1432).
