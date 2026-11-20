---
"cartograph": minor
---

Draw the recovered system in the C4 model (#2049). Every export now includes `architecture.md`, with Mermaid diagrams and cited tables for system context, containers (web application, processes by entry-point kind, operator commands, scheduled jobs, client applications, data stores and outside systems) and per-container components from the subsystem areas, and `architecture.dsl`, the same model as a Structurizr DSL workspace. No model call; each box and arrow carries the confidence of the facts it rests on.
