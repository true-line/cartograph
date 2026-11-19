---
"cartograph": patch
---

User stories are no longer empty after ingesting a NestJS GraphQL codebase such as Vendure. The Spec Workbench, the export bundle and single-artifact reads now include the Capabilities and caller sets the knowledge layer derived, as the read-only MCP already did, and each derived Capability lists one readable story per operation, for example "As an admin API caller holding CreateCatalog or CreateProduct, I can create product (MUTATION admin.createProduct)." These stories are InferredWeak, so a verified-only export still omits them and says how many it left out.
