# Spec Workbench

The generated official artifacts (user stories, flow dossiers, topology, gap
register, ADRs, coverage) with T0-locked semantics: deterministic facts cannot
be edited away, only annotated. Curation decisions (accept / reject / annotate)
persist and re-apply on re-ingest while the underlying evidence is unchanged.

The exported bundle file records what it was built from under `graph_basis`:
the content hash of the whole graph and each source with its commit.
Curation decisions do not change that hash. It covers every source in
the system, so with more than one source it differs from the per-source hash
shown in **Workspace**.

## Coverage in an export

Every export includes **coverage.md** and **coverage.json**. They show, for
each repository, what the trace read and what it did not, with the reason
recorded when the repository was ingested:

- the producers that ran, by language and file kind, and how many files each read
- the producers the layer hints excluded
- the inputs that were not declared, such as a Terraform state file
- the languages and constructs no installed producer covers, with the first place each was found

Use them to tell a finding that is absent from a file that was never read. An
entry that was not covered is a limit of the tool, not a gap in the system
under study.

Coverage describes the run, so it is the same in a `verified-only` and a
`best-effort` export. Each repository is listed with its inventory hash, which
identifies the stored inventory the documents were made from. The same
inventory gives the same documents every time.

On a large estate **coverage.md** stays short. Up to 12 repositories it shows
the detail for each one. Past 12 it keeps one row per repository and the
totals by producer and by reason, and the detail for every repository is in
**coverage.json**.

The inventory counts files per producer. It does not list every file, so the
documents name the first place a limit was found, not each file.
