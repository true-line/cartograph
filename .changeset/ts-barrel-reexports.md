---
"cartograph": patch
---

TypeScript calls through barrel modules (`export * from './x'`, `export { a as b } from './x'`) now link to the function or method in the file that declares it, citing each barrel crossed, instead of staying unresolved call Gaps. On Vendure's core package this cuts unresolved call targets from 234 to 63.
