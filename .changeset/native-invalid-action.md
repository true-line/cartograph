---
"cartograph": patch
---

A tool-calling investigation no longer ends on the first tool call that does not decode. The call is not run; it goes back to the model as an invalid_action tool error, recorded in the run's journal, up to two times a run, and a third ends the run as before (AC-1420).

On an OpenAI-compatible server such as vLLM, a step that offers tools now requires the model to call one. Before this, GLM could write the finish as plain text, and the run ended with empty_completion.
