---
"cartograph": patch
---

Section drafting now sends the model the exact reply format. Before, a drafting run gave the provider no response schema, so models (Opus included) often wrapped their action in prose or left the finish unwrapped, and every section was refused with "The completed provider response was not an admissible action." A section-writer request now carries a schema covering only its own actions, which providers enforce with structured output.
