# Spec Workbench

The generated official artifacts (user stories, flow dossiers, topology, gap
register, ADRs, endpoint permissions, endpoint inventory, pipelines, configuration keys, external services, environments, curated items, product specification, coverage, fact index) with T0-locked semantics:
deterministic facts cannot be edited away, only annotated. Curation decisions (accept / reject / annotate)
persist and re-apply on re-ingest while the underlying evidence is unchanged.

The exported bundle file records what it was built from under `graph_basis`:
the content hash of the whole graph and each source with its commit.
Curation decisions do not change that hash. It covers every source in
the system, so with more than one source it differs from the per-source hash
shown in **Workspace**.

Beside it, `curated_basis` records the curated revision and the counts of
accepted proposals by state. See **Curated items** below.

## Endpoint inventory

Every export includes **endpoint-inventory.md**. It lists every recovered
endpoint once, including endpoints that no test references. Each row shows the
method, path, framework and language as they were recovered, and `unknown`
where nothing was recovered, with the endpoint's tier, confidence and source
span. It does not state parameters, responses or authentication. An endpoint
recorded as a Gap is counted and listed in the gap register instead. A
`verified-only` export leaves out InferredWeak endpoints and says how many; a
`best-effort` export marks them.

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

## Build and deployment pipelines

Every export includes **pipelines.md**. It lists what the repository's
GitHub Actions workflows and Dockerfiles declare about building and
deploying. No model is involved, and nothing is run or evaluated: the
document states what the files declare, not what a run did.

A pipeline is one workflow file or one Dockerfile. A job is a workflow job
or a Dockerfile stage. A step is a workflow step or a Dockerfile
instruction. For each pipeline the document lists:

- its triggers, in the order the file declares them
- its jobs in dependency order: a job comes after every job it needs, and
  where jobs do not depend on each other the one declared first comes first
- each job's steps, in the order the file declares them

A job has no place in the order when one of its needs could not be resolved,
when it needs such a job, or when its needs form a cycle. These jobs are
listed after the ordered ones, each with the reason.

Every row shows the tier, confidence and source span of its fact, and the
citation id of each fact it states: the fact itself, the relation that places
a trigger, job or step, and each need of a job. Each relation shows its
confidence beside its id. Every id is a row of the fact index.

A `verified-only` export leaves out InferredWeak rows and states how many
pipeline facts it left out. A `best-effort` export includes them and marks
them. The same graph gives the same document every time.

**Files present and not read** are listed with the reason recorded when the
repository was ingested: GitLab CI, Jenkins, CircleCI and Azure Pipelines
files, and workflows or Dockerfiles that could not be read exactly. For each
kind and repository the list gives the number of files, the first file and
the reason recorded for that first file. The other files are counted and not
named. This part needs the stored coverage inventory. A reader without it,
such as a specialist reading the document over MCP, is told that nothing is
stated about unread files. For the same reason the document cannot be
selected in a work packet.

Not covered today:

- A Dockerfile that uses a heredoc (`<<`) or changes its escape character is
  not read.
- A workflow that uses a YAML alias, anchor or tag is not read.
- A file with another name is not found at all, so it is not listed as
  unread either. Workflows are YAML files directly in `.github/workflows`.
  Dockerfiles are named `Dockerfile` or `Containerfile`, either with a dotted
  suffix such as `Dockerfile.dev`, or end in `.dockerfile`.
- A matrix is one job as written. Expressions and variables stay as written.
- A reusable workflow and a third-party action are references. Their own
  jobs and steps are not resolved.
- The values of `with:` inputs, `ENV` and `ARG` are not stored. A secret is
  recorded by name only.

## Configuration keys

Every export includes **configuration.md**. It lists each configuration key
once, with the environment files that declare it and the source files and
symbols that read it. No model is involved. Only key names are listed: no
value, default or fallback is stored or shown.

A key is the name of an environment variable. Names are compared exactly,
including case. A key is listed once for each repository, because an
environment file in one repository says nothing about a read in another.

- **Declared** means an environment file among the ingested sources assigns
  the key: `.env`, `.env.*`, `*.env`, `env.example`, `env.sample` or
  `env.template`.
- **Read** means source code reads the key by name through the language's
  own environment read, such as `process.env.X` or `os.getenv("X")`.
- **Read, never declared** means that no environment file in the ingested
  sources declares the key. It does not mean that the key is unset when the
  system runs. A deployment platform, a pipeline setting, a container
  definition, a secret store or a shell can set it, and none of those is read.
- **Declared, never read** means that no recovered read names the key. A
  read that is not covered can still use it.

**Files with reads that cannot be named** are listed separately. Each reads
at least one key whose name is not written as a literal in the source, for
example `process.env[name]`. Those keys exist and the document cannot name
them.

Every row shows the tier, confidence and source span of its fact. An
environment file has no span: these files hold secrets, so no part of one is
stored.

A `verified-only` export leaves out InferredWeak rows and states how many
configuration facts it left out. A `best-effort` export includes them and
marks them. The same graph gives the same document every time. The document
reads graph facts only, so it can be selected in a work packet.

The document states which languages are read: TypeScript, JavaScript, Python,
Go, Rust, Java and Kotlin, each with what is read and what is not.

Not covered today:

- `import.meta.env` and a destructured `process.env`
  (`const { X } = process.env`) in TypeScript and JavaScript.
- A membership test in Python, `"X" in os.environ`.
- The compile-time `env!` and `option_env!` macros in Rust.
- Configuration frameworks: Spring `@Value` and `Environment`, NestJS
  `ConfigService`, Viper, and settings libraries in general.
- Declarations outside environment files. A key set in a container or
  compose file, a deployment manifest, an infrastructure definition, a
  workflow `env:` map or a shell script is not listed as declared.
- Languages other than the seven above.

## Calls and publishes that leave the system

Every export includes **external.md**. It lists what the ingested sources
call or publish to outside themselves, which flows reach each outside
service, and which data entities those flows read or write. No model is
involved.

One rule decides every target:

- **Outside** is a web address whose scheme and host are written in the
  source and that no declared binding covers. Each one is listed once, with
  every place that calls it, the method and path where they are known, and
  the source span.
- **Unclassified** is a target that leaves your code and is not proven
  inside or outside. Each is listed with its reason: the host is computed
  when the system runs or read from configuration; the client belongs to an
  SDK or an identity provider, and nothing records who operates it; or a
  channel is published to and no subscriber or backing resource was
  recovered.
- **Not listed**: a call that reached an endpoint of an ingested repository,
  a call with no host (it goes to the caller's own origin), an in-process
  event, browser storage and an imported package. A call that a declared
  binding places in an ingested repository and that matched no endpoint
  there is counted, not listed.

The outside list is a lower bound. Read the unclassified count beside it.

A binding is a service base URL or a gateway route in the system manifest.
Without one, a call to another ingested repository by its full web address
is listed as outside. When bindings exist, each call and each observed HTTP
client records what the bindings say about its host. A call to a host no
binding names is listed as outside. A call or client of a host a binding
names is not listed as outside, even where no endpoint matched.

A system ingested before this was recorded keeps the older result: a call
to a host its bindings do not cover is listed as unclassified, with the
sentence recorded for it. Add the system again to record the outcome.

**External data flows** has one row for each flow and outside service it
reaches. The entities are those a symbol on the flow reads or writes. The
row does not say that an entity is sent to the service. A flow that stops at
an unresolved step before the call is not listed as reaching the service; it
stays partial in the gap register.

Every row shows the tier, confidence and source span of its fact, and never
a higher confidence than the fact has. A call that matched no endpoint is a
gap, and its row says so.

A `verified-only` export leaves out InferredWeak rows and states how many
boundary facts it left out. A `best-effort` export includes them and marks
them. The same graph gives the same document every time. The document
cannot be selected in a work packet.

Not covered today: how sensitive the data is, what a request body contains,
traffic that arrives from outside, package dependencies, and who operates an
SDK provider.

## Environments

Every export includes **environments.md**. It lists the environments your
sources declare and what each one declares to run. No model is involved.

- **An environment** is one file that declares a set of things to run
  together: a Compose file with a `services` section, or a kustomization
  file (`kustomization.yaml`, `kustomization.yml` or `Kustomization`). Each
  such file is one environment. A kustomize base is one too; its row says
  which kustomizations include it.
- **An environment is shown by the path of its file.** It has a name only
  when the file states one (a top-level `name` in a Compose file, a
  `namespace` in a kustomization). Otherwise the name reads "none stated".
  Cartograph never turns a file or folder name such as `prod` into an
  environment name.
- **A workload** is one Compose service, with its image, build context,
  ports, the names of its environment keys, its environment files, its
  dependencies and its profiles, as the file writes them. For Kubernetes it
  is one `Deployment`, `StatefulSet`, `DaemonSet` or `Job` document, with
  one row for each container: image, ports, the names of its environment
  keys and the names of the ConfigMaps and Secrets it references.
- **A kustomization declares** the Kubernetes workloads that its
  `resources` and `bases` entries reach inside the repository, through
  other kustomizations where they name one.
- **Declared in more than one environment** lists a workload name that two
  or more declarations of one repository share, such as `web` in
  `docker-compose.yml` and `docker-compose.prod.yml`. Each declaration has
  one row, with its kind, the file that writes it, the environments that
  reach it, the properties that differ and its source span. A workload that
  several kustomizations reach is still one declaration there. Where a name
  is declared both as a Compose service and as a Kubernetes workload, only
  the images, ports and environment key names are compared. A
  Kubernetes workload that two or more kustomizations reach has one row for
  each of them, with what that kustomization's patch files and `images`
  entries state for it, or "no patch stated". Nothing is applied: the
  document shows what each file writes and does not say which one is used
  where.
- **Workloads no environment declares** lists a Kubernetes workload that no
  kustomization reaches.
- **Not established** lists what was found and could not be stated: a file
  that could not be read, `extends`, `include`, a YAML anchor, alias or tag,
  a dependency on a service the file does not declare, a kustomization
  entry that is remote, missing or outside the repository, a patch that
  matches no workload, an inline patch, a `target` selector,
  `patchesJson6902`, a transformer or generator such as `namePrefix` or
  `secretGenerator`, and an environment file such as `staging.env` or
  `.env.production`, whose name is the only thing that names an
  environment. Where a kustomization sets `namePrefix` or `nameSuffix`, its
  rows say that the deployed name differs from the recorded one.

Helm charts and values files are not read, and neither is a Kubernetes
manifest of any other kind, such as a `Service` or a `ConfigMap`. The
coverage document lists each kind found as present and not read. A
`CronJob` is listed as a scheduled job, not as a workload.

Only the names of environment keys are listed, never a value. A value
written as `${...}` is shown as written and marked as not resolved. A long
unbroken run of letters and digits in an image or a port is withheld,
because it has the shape of a token.

Every row shows the tier, confidence and source span of its fact. An
environment file has no span, because these files hold secrets.

A `verified-only` export leaves out InferredWeak rows and states how many
environment facts it left out. A `best-effort` export includes them and
marks them. The same graph gives the same document every time. The document
cannot be selected in a work packet.

Not covered today: Kubernetes manifests, kustomize files, Helm charts and
Helm values files (the coverage document lists each kind it found as present
and not read); Terraform resources per environment; the combined result of
several Compose files; which environment is production; and live state.

## Bundle manifest

Every export includes **bundle-manifest.md**. Open it first when you hand an
export to someone else. No model is involved.

- **As-of** says what the export was made from: the document catalogue
  version, the export mode, the graph content hash, the curated revision,
  and each source with the revision it was read at. Two exports with the
  same as-of hold the same files.
- **Parts** lists every file of the export once, under the part of the
  handoff bundle it belongs to: documents, rules, the traceability matrix,
  the gap register, the fact index and the as-of.

The manifest says nothing about your system and cites no fact. Work packets
are not listed, because you generate and export each one on its own. When
the app reads one file on its own without the review store, a value it did
not read is shown as "not supplied".

## Documentation evidence

Every export includes **documentation-evidence.md**. It lists the
documentation your sources already hold (README files, Markdown and text
files) and what each part of it names. No model is involved.

- **A document** is one documentation file. **A section** is the part under
  one heading. The page shows each section's heading, never its text: to
  read what a section says, open the file.
- **A fact mentioned** is something a section names exactly: a file path, a
  route such as `GET /orders/{id}`, a fact id, or a name only one recovered
  thing has. Each row gives the rule that made the link and two citation
  ids, one for the fact and one for the link. Both are in the fact and
  evidence index.
- **A link is not agreement.** It says the section names that thing. It does
  not say the section describes it correctly, and it never changes what was
  recovered from the code.
- **Counts.** The page counts the links by rule and the documentation drift
  entries by kind. The entries themselves are under "Documentation drift"
  in **drift_register.md**: a route the documentation names that is not
  served, one served with other methods, and a link or path to a file that
  is not there.
- A heading or a reference that holds a secret is shown redacted.
- The page cannot be selected in a work packet.

When a section of a document is drafted for one subject, the draft may cite
the documentation sections that name that subject or something directly
related to it. That is how a statement of documented intent points at where
the documentation says it.

Not covered today: reStructuredText and AsciiDoc structure (those files are
read as plain text), and anything a section says in prose without naming a
file, route, id or unique name.

## Architecture diagrams

Every export includes **architecture.md** and **architecture.dsl**. They
draw your system in the [C4 model](https://c4model.com) at three levels, from
the recovered facts alone. No model is involved.

- **Level 1, system context**: who uses the system and which outside systems
  it depends on.
- **Level 2, containers**: the parts that run on their own (the web
  application, each process an entry point starts, operator commands,
  scheduled jobs, each client application), the data stores they use and
  what they call. A repository with code and no recovered way in is drawn as
  a Gap, never left out.
- **Level 3, components**: per container, the subsystem areas holding the
  files its flows pass through, and the calls between them. The areas are
  inferred, so a `verified-only` export draws no component.

Each box and arrow carries the lowest confidence of the facts it rests on,
and the tables under each diagram cite those facts. A dashed arrow is
InferredWeak; a dashed red box is a Gap. The diagrams are Mermaid, which
GitHub, GitLab and most Markdown viewers render. `architecture.dsl` holds
the same model as a Structurizr DSL workspace, for teams that keep their
architecture in Structurizr.

In the Spec Workbench, select **architecture.md** to open the
**Architecture view**. It opens at level 1; use the level tabs, or
double-click the system or a container with a yellow outline, to go down a
level. Select any box or arrow to see its kind, technology, confidence and
the facts it rests on, each with its tier and source location. The Markdown
source stays one click away under the diagram.

## Product specification

Every export includes **product-specification.md**. It is the first document
arranged as the document catalogue describes it: one file with every section
of the product specification, in order, each marked with its kind.

- **P** sections are compiled from recovered facts. Actors and capabilities
  are listed with their tier, confidence and citation id; each id has a row
  in **fact-index.md**. Flows are summarized and the Gap findings are
  counted, with the detail in **flow_dossiers.md** and **gap_register.md**.
- **P+A** sections show the compiled part, then the accepted draft for the
  section, or **Not drafted**. Business rules and acceptance criteria point
  to **rule-evidence.md** and **test_arrangements.md** and are not repeated.
- **A** sections show the accepted draft, or **Not drafted**. The Section
  Writer role drafts one section per investigation run and its draft is
  stored for review. A draft is printed only after you accept it.
- **H** sections are left blank for your team. Cartograph never fills them.

The header shows the catalogue version, the export mode, the graph content
hash, the curated revision and the **Stale sections**: each section whose
accepted draft cites a fact that changed or is gone, or **none**. No model is involved in writing this document,
and the other exported files are unchanged, except that the fact index now
lists this document beside each fact it cites. An MCP client reads the same
document with `cartograph_read_artifact`.

A draft is shown only after a person accepts it. Each claim is printed with its citation ids and marked
**T3 / InferredWeak**, with the role that wrote it and **human-reviewed:
accepted**. Accepting a draft never raises its tier. A **verified-only**
export prints no drafted claim and says how many it left out of each
section.

When a fact an accepted draft cites changes, the draft is **stale**. A
**best-effort** export keeps it in its section under **Stale: cited facts
changed since this text was accepted**, with the changed citation ids, and
each claim that cites one ends with **cites a changed fact**. Read a stale
section as what was true when it was accepted. A draft citing a fact that
is no longer in the graph is not printed; the section says how many are
missing and **curated.md** names them. A draft that is replaced or in
conflict with another accepted draft is named with that reason and not
printed.

### Drafting and reviewing sections

Open **Spec** and expand **Sections and drafts**. Each section of the
product specification is listed with its kind and every draft stored for it.

- **Draft this section** opens the investigation composer with the Section
  Writer role and the question for that section already filled in. Leave the
  subject empty to draft for the whole system, or enter the citation id of
  one actor, capability or rule. The run starts only when you start it, with
  the same consent and limits as any investigation.
- A draft shows its claims, each marked **T3 / InferredWeak** with its
  citation ids. Selecting a citation id opens the fact index.
- A run stores no draft when one of its statements spells out a file path
  or a code name (such as `create_order` or `OrderService`) of a fact that
  statement cites. A requirement says what the system does, not where the
  code is; the code stays in the citation. Run the draft again. Routes,
  field names and message names may be stated.
- **Accept**, **Reject** and **Return** record your decision with an
  optional note. A decision changes no claim. If the draft changed since the
  view was read, the decision is refused and the view is read again.
- An accepted draft shows its state from the latest curated read: current,
  stale, replaced, in conflict or unresolvable, with the reason. A
  best-effort export prints a current draft, and a stale one marked stale.
- A stale or unresolvable draft lists the facts behind that state.
  **Changed since this draft was accepted** names each cited fact whose
  content changed; selecting one opens it. **No longer in the graph** names
  each cited id that has no fact now, so there is nothing to open. Each
  claim that cites a changed fact says so.
- **Re-draft this section** on a stale or unresolvable draft opens the
  composer for the same section and scope. The accepted draft stays as it
  is while the new one is written and reviewed. The new draft is listed as
  a **re-draft of** the accepted one. Accept it and it takes that draft's
  place; the replaced draft is kept and listed as replaced. Reject it and
  nothing else changes, and you can re-draft again. A re-draft cannot be
  returned. Only one re-draft of a draft is open at a time.
- A draft that replaces another shows **Change from the draft it
  replaces**: how many claims were added, removed and kept, and how many
  cited facts differ. Open it to read the claims and the facts newly cited,
  no longer cited, or cited by both but recorded with another content hash.
  The comparison is of the two stored drafts only, so it is the same before
  and after review.

## Curated items

Every export includes **curated.md**. It lists every proposal you accepted
in **Proposal history** and says, for each, whether it is shown as a curated
item or why it is not. No model is involved in writing it.

A **curated item** is an accepted proposal whose evidence still holds. It is
shown beside the recovered facts and is never one of them. It keeps the tier
and confidence it was produced with, Agentic and InferredWeak. It is not
written to the graph, it does not resolve its Gap, and the graph hash does
not change.

Most accepted proposals are listed as **unverified** today. A proposal
becomes a curated item only when every piece of evidence it was given is
source text that Cartograph captured and retained when it ingested the
source. Most proposals are still made from text read from the working tree,
which nothing verified, and accepting one does not verify it. This changes
as more evidence is captured (#385), not when you accept again.

Each accepted proposal has exactly one state:

- **eligible**: every condition holds. This is a curated item.
- **unverified**: the proposal has no evidence, some of its evidence was not
  captured, or it was staged before captured evidence was recorded.
- **already-covered**: the graph now holds the proposed relation as a
  recovered fact from a lower tier, so the proposal is no longer needed.
- **unresolvable**: the Gap, the source or the target of the proposal is no
  longer in the graph, or the node it names as its Gap is no longer a Gap.
- **stale**: the graph or the source associations of the facts the proposal
  was made from have changed since it was made, its captured text can no
  longer be read, or the comparison could not be made. Any change to the
  graph makes a proposal stale, not only a change near its Gap.
- **conflicted**: another accepted proposal proposes a different relation for
  the same Gap. Neither is shown as an item. Reject one of them.

The document gives the reason for each state in a sentence, with its code.

A `best-effort` export shows the curated items, each marked InferredWeak,
and cites the Gap, source and target of each through the fact index. A
`verified-only` export shows no curated item and states how many it left
out. The accepted proposals that are not items, the counts and the summary
of Workbench decisions are the same in both. When a rejection or the export
mode withholds a node that an item names, the item is still shown and the
document says that the node is not cited.

The document also summarises your Workbench decisions on inferred
statements: how many rejections apply, and which decisions no longer match
any statement the export makes. A decision stops matching when re-ingest
changes the statement it was made on.

The **curated revision** at the top identifies this exact set of reviews,
states and reasons over this graph. The same reviews over the same graph
give the same revision. Any review, including a rejection, gives a new one.
A review note does not.

Withdrawing an accepted proposal is rejecting it. A pending or rejected
proposal is not listed.

Limits today:

- An export reads at most 4,096 proposals and at most 256 accepted ones. With
  more, or when the proposal history cannot be read, the export still
  succeeds: **curated.md** says that the curated projection was not computed
  and why, it shows no accepted proposal, the bundle has no `curated_basis`,
  and every other document is unchanged. It never lists a part of them.
- Accepted section drafts and accepted findings are limited by their size,
  not their number: up to 64 MiB of drafts and 16 MiB of findings, which is
  256 of the largest each store admits and many more small ones. Past that
  the curated projection is not computed, the same way.
- If the graph changes between the check of a proposal's evidence and the
  export's own read of the graph, the proposal is listed as stale for that
  export. Export again.
- Investigation findings accepted for follow-up are not listed. A finding
  is listed only after you accept it as curated in its investigation, which
  is a separate review. A best-effort export then prints it as a cited
  statement marked InferredWeak while every fact it cites is unchanged, and
  the **Curated items** section of the Workbench lists it. A verified-only
  export prints none and says how many it left out. A finding whose cited
  fact changed or is gone is listed with the reason, without its text.
- A work packet cannot select this document. A complete-session MCP client
  can read it (see **Read-only MCP** in Settings).

### Accepted section drafts

When you have accepted a draft of a document section, **curated.md** also
lists each accepted draft with its section, its state and two lists:
**Changed**, the facts it cites whose content differs from what the draft
recorded, and **Missing**, the cited ids that no longer resolve to a fact.
A draft with a changed fact is **stale**; one with a missing fact is
**unresolvable**. The lists are computed by comparing recorded and current
content hashes. No model is involved, and no drafted claim is repeated in
this file.

### Curated items in the Workbench

The Workbench shows the same projection above the document list, under
**Curated items**, without an export. It shows:

- The view, the curated revision, the graph basis and the export mode. The
  revision and the graph basis are shortened; hover over one, or open
  **Full identifiers**, for the whole value. The revision is the one the
  bundle on screen carries as `curated_basis`.
- The counts of accepted proposals by state.
- **Records and decisions**, closed until you open it, with the three lists
  below.
- The curated items, each with its record id, proposed relation, Gap, tier
  and confidence as produced, review, source support, basis and state. Each
  is marked InferredWeak. In `verified-only` mode no item is shown, only how
  many were left out.
- The accepted proposals that are not items, grouped by state, each with the
  reason in a sentence and its code.
- Your Workbench decisions that no longer match any statement.

When no proposal is accepted, the section says so. When the projection could
not be computed, it gives the reason and shows no proposal; that does not
mean there are none.

The section is read again when you record a review, change the export mode,
record a Workbench decision, or choose **Refresh curated items**. It does
not update by itself while you look at it. It is computed together with
the documents on screen, so it always describes the same reviews and graph
as they do.

**Proposal history** and the review dialog show the state and reason of an
accepted proposal when this read returned it. An eligible proposal is not
returned in `verified-only` mode, so its state is not shown there in that
mode. Accepting a proposal does not make it a curated item: its state is
worked out from the graph and the evidence each time it is read.

## Coverage in an export

Every export includes **coverage.md** and **coverage.json**. They show, for
each repository, what the trace read and what it did not, with the reason
recorded when the repository was ingested:

- the producers that ran, by language and file kind, and how many files each read
- the producers the layer hints excluded
- the inputs that were not declared, such as a Terraform state file
- the languages and constructs no installed producer covers, with the first place each was found
- the files a gated plugin was refused on, with the reason: a fact the host does not accept, an error or trap in the plugin, or a fuel, memory or table bound

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
documents name the first place a limit was found, not each file. Files a
plugin was refused on are the exception: up to 256 are listed per plugin and
the rest are counted. A refused file is not in that plugin's file count, and
no fact from it by that plugin is in the graph.

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
or when two different facts would share one id. Most documents do not print
the citation id beside each statement yet. That is added one document at a
time. `data_model.md` prints one beside each entity, related node, relation
and relation field it lists, `pipelines.md` beside each pipeline, trigger,
job, step and need, and `curated.md` beside each curated item.

The index grows with the number of facts the documents state, not with the
size of the graph.
