---
"cartograph": minor
---

TypeScript calls into dependencies you list in `.cartograph/types.toml` now appear as calls to the package boundary, naming the member and the installed version. Cartograph reads only those packages' type declarations, never their code, and never reads `node_modules` without the list.
