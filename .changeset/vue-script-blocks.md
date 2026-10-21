---
"cartograph": minor
---

Vue single-file components are now ingested: the `<script>` and `<script setup>` blocks of `.vue` files are parsed like JavaScript/TypeScript, so their imports, calls and HTTP client calls appear in the graph with evidence pointing into the component. Preflight and Settings list Vue as covered for script content and state that templates are not parsed.
