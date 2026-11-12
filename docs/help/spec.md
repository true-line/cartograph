# Spec Workbench

The generated official artifacts (user stories, flow dossiers, topology, gap
register, ADRs, endpoint permissions, coverage, fact index) with T0-locked semantics:
deterministic facts cannot be edited away, only annotated. Curation decisions (accept / reject / annotate)
persist and re-apply on re-ingest while the underlying evidence is unchanged.

The exported bundle file records what it was built from under `graph_basis`:
the content hash of the whole graph and each source with its commit.
Curation decisions do not change that hash. It covers every source in
the system, so with more than one source it differs from the per-source hash
shown in **Workspace**.

## Endpoints and permission sites

Every export includes **endpoint-permissions.md**. It joins each recovered
endpoint with the permission sites recovered for it. A permission site is a
place in the source where a guard or a permission requirement was recovered,
such as a login decorator. No model is involved.

The document has four parts:

- endpoints with a recovered permission site, each with the site and its source span
- endpoints with none recovered, where the handler path was read to its end
- endpoints whose reading is partial
- a coverage note for endpoints in frameworks where no permission site is recovered

**None recovered** means only that. It does not mean that no check runs. A
check made by middleware, by a gateway in front of the service or by code
inside the handler is not recovered. That is a limit of what the tool reads,
not a finding about the system.

Permission sites are recovered for three frameworks today: Flask guard
decorators, Flask-Admin views that require a logged-in user, and the `Allow`
decorator on NestJS GraphQL operations. The document lists them. An endpoint
in any other framework appears under the coverage note and in neither table.

The handler path is the endpoint's handler and the calls reachable from it.
An endpoint is listed as partial when no site was recovered and the path was
not read to its end: a call could not be resolved, no handler was recovered,
the walk reached its bound, or the guards could not be classified. The walk
follows at most 16 calls below the handler.

Every row shows the tier, confidence and source span of the endpoint, and of
the permission site where there is one.

A `verified-only` export leaves out a row whose permission site is
InferredWeak and states how many it left out. A `best-effort` export includes
the row and marks it. The same graph gives the same document every time.

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

## The fact and evidence index

Every export includes **fact-index.md** and **fact-index.json**. They list
each fact the other documents state, once, with where it comes from:

- the citation id, such as `F-3a9c41d07be2f815`
- the fact itself: a node, or a relation between two nodes
- its tier, confidence, producing method, evidence basis and extractor
- every evidence span: repository, path, byte range and commit
- the documents that state it

A citation id names the fact, not a place in a document. The same fact has
the same id in every document and in every export of the same graph, so a
sentence can keep its citation when a document is rewritten or reordered.
The id changes only when the fact itself becomes a different fact.

The index holds spans, not source text. Use a span to open the source at
that place.

The index follows the export mode. A `verified-only` export has no row for
an InferredWeak fact. A `best-effort` export has those rows and marks each
one. A gap has a row like any other fact and is marked as a gap, so an
unresolved hop is never read as confirmed.

When two documents state the same fact with different evidence, the row
lists every evidence record and the documents that carry each one. Nothing
is chosen for you.

An export is refused when one of its statements cites an id that has no row,
or when two different facts would share one id. The documents do not print
the citation id beside each statement yet. That is added one document at a
time.

The index grows with the number of facts the documents state, not with the
size of the graph.
