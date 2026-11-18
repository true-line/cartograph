---
"cartograph": patch
---

The MCP task cap for an external agent now counts only its live tasks. Before, every task the agent had ever started counted, including finished and cancelled ones, so an agent with a cap of one could start one task and then never another. Finished tasks stay stored and readable, and retrying a request with the same nonce still returns its original task, finished or not. The cap's default and upper bound are unchanged.
