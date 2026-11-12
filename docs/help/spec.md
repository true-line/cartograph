# Spec Workbench

The generated official artifacts (user stories, flow dossiers, topology, gap
register, ADRs) with T0-locked semantics: deterministic facts cannot be
edited away, only annotated. Curation decisions (accept / reject / annotate)
persist and re-apply on re-ingest while the underlying evidence is unchanged.

The exported bundle file records what it was built from under `graph_basis`:
the content hash of the whole graph and each source with its commit.
Curation decisions do not change that hash. It covers every source in
the system, so with more than one source it differs from the per-source hash
shown in **Workspace**.
