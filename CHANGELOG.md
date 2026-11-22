# cartograph

## 0.75.0

### Minor Changes

- a6a5249: TypeScript calls through an interface, such as an RPC proxy typed `ExtHostUrlsShape` or an injected `ILogService`, now link to the method on each class that implements the interface instead of staying an open gap. The links are marked inferred, name the interface and implementation, and never point product code at test mocks (#258, AC-1515).
- 461c2be: A project can change how many Alembic revision files a migration history may have, how large each may be, and how large a lockfile may be before Python recovery records a gap (advanced operator limits) (#1653).

### Patch Changes

- bf2d191: The signed macOS build checks Claude Code's Anthropic signature correctly again, so the release that carries the bundled tool-calling runtime can be published (AC-1424).

## 0.74.0

### Minor Changes

- 2e4572f: The agent pool shows the model calls and provider-reported tokens each topic expert question and section draft used, with a total over the listed work.
- cb88c60: The agent pool now lists saved chat sessions, one per conversation, with their turns, newest status and whether one is open, and opens a session from there.
- 6569ec5: The agent pool can pause new agent work and limit how many local or cloud agent jobs run at once.
- fa6b692: Agent work that the pool controls do not admit yet waits as queued and starts in order when a place frees or the pool is resumed, instead of being refused.
- 49b2439: Specialist investigations now follow the agent pool controls: a paused pool or a full mode holds a new investigation as queued, and it starts when a place frees.
- b38d0a3: The agent pool panel can check whether the local model server is running and which of its models are missing.
- 557e025: Agent Chats now opens with an agent pool: the agent work that is queued or running and what finished last, the configured providers, and how many staged proposals wait for a decision, with links to Jobs and curation.
- 505062a: The host now keeps a named agent roster: Domain Analyst, Evidence Auditor, Spec Writer, Gap Triager and Section Writer, each at its current version with a tier ceiling, and reports which agents have work in flight.
- d2fa04c: Android: activities, services, broadcast receivers and content providers an `AndroidManifest.xml` declares are entry points entering their class's lifecycle function, and a Kotlin WorkManager worker's `doWork` is a background job; a library or unresolvable component is a Gap naming it.
- c980471: A project can change how many API reference documents a repository may declare (up to 64) and how large each may be (up to 8 MiB) (#1653).
- 04f40e6: Area briefings: an area's briefing now carries the rules printed for the strategy implementations and configured instances it dispatches to, wherever the call site is, marked inferred, so rules reached only through configuration are stated where the area's expert looks.
- f0efaea: A project can lower the retained-source store's quota (MiB and capture count, advanced operator limits). What is already retained stays readable; new captures past the quota are refused until space is freed (#1653).
- 2366c3f: Raise the source-capture maxima to 20,480 files and 320 MiB per capture, so a full VSCode checkout (16,180 TS/JS files, about 234 MiB) captures and ingests. The new maxima are sized from a measured whole-VSCode ingest that peaked at 12.3 GB RSS.
- 4a96e5e: Tool-calling specialist versions can now run on Claude: a Cloud start runs the Claude Agent SDK with Anthropic's signed Claude Code, which the app bundles and checks before each run, and the API key stays with the app. Cloud tool-calling runs need an Apple silicon Mac for now.
- e9dbb87: On a cloud provider, a captured source read whose path matches a read-deny rule (environment files, key material, credential files) is now withheld from the model instead of being sent. The read and its firewall refusal are recorded together, and the run continues (#679).
- 5d604d0: A guard that tests a list its function starts empty and fills in a loop now says in D1 what is added to the list, for example "Here the variants with insufficient stock start empty, and when the line's quantity is more than the available stock, the line's product variant is added to the variants with insufficient stock."
- 5d604d0: D1 now follows a strategy chosen in the default configuration (such as the stock location strategy) and states the totals it adds up and the test that decides each addition.
- 7fd9fda: Coverage: the coverage inventory, `coverage.md` and `coverage.json` now name, per language present, each extractor family (target tests, persistence, rule evidence, endpoint auth) that no installed producer recovers for it, so an absent test, rule or guard of that language is never read as checked.
- c3f97d3: TypeScript calls into dependencies you list in `.cartograph/types.toml` now appear as calls to the package boundary, naming the member and the installed version. Cartograph reads only those packages' type declarations, never their code, and never reads `node_modules` without the list.
- 1caaf4e: D1 now says when a guard compares a count with the distinct values of a list, for example "Here the variant ids are the distinct product variant IDs of the order's lines."
- 5c8bea0: The egress firewall can now refuse a single read of an environment file, key or credential file toward a cloud provider, and record a host-only witness of that refusal.
- 5d604d0: When a guard tests a value a method returns (such as the saleable stock level), D1 now follows that method and states what it returns in each case, including the value it computes otherwise.
- 353786a: TypeScript calls on a class that inherits the method it calls, such as `map.get()` on a `DisposableResourceMap extends DisposableMap`, now link to the base class method instead of staying an open gap. Methods of abstract classes are now named after their class (`Disposable.dispose`) rather than anonymously (#258, AC-1514).
- 9758472: Journey candidates now continue past a step that writes data, so screens reached only after signing in (such as the browser extension's API key setup) appear in a journey. Each goal keeps its shortest journey and also lists alternative journeys through screens that one skips. Journey ids change once with the new rule version.
- 4eede6c: The knowledge phase now embeds documentation sections. A README or docs heading becomes searchable together with its text, which is read from your source at embedding time and only used while it still matches what was ingested. Document text is still never stored in the graph.
- 25fb3bf: Kotlin: Kotest spec classes (FunSpec, StringSpec, ShouldSpec, DescribeSpec, BehaviorSpec, WordSpec, FreeSpec, FeatureSpec, ExpectSpec) are now recovered as test cases named by their class and container chain, with the requests their test lambdas perform linked to endpoints.
- f3efe9d: Semantic admission can now run on a receipt from the locked evaluation runner. With `locked-receipt.json` and `runner-key.txt` in the app's evaluation store, the app holds no dataset or labels: it verifies the receipt against the pinned key and rechecks it against its own policy, embedding space and corpora before any family admits (ADR-0082).
- 9f49848: Terraform state, Pulumi and trace files a system manifest declares are now captured under their own identity when they sit inside the manifest folder, so observed evidence cites captured bytes and a relative path instead of your local folder layout; inputs outside the folder are flagged as unverified in coverage.
- e13aa94: Rule evidence keeps dotted lowercase message keys such as `message.cannot-transition-to-payment-when-order-is-empty` instead of withholding them as possible secrets, and D1 states a refused move's message in words. Real tokens stay withheld.
- 579e93e: Semantic (T2) previews no longer propose a call whose name matches two or more symbols of the same name, such as `join` or `isEqual` helpers copied across modules. Similarity cannot tell such homonyms apart, so the hop stays an open gap tagged `name collision - needs import or type resolution` (#259, AC-1513).
- 0960fe4: Configure → Run Limits has a Context window choice, so local models can read long prompts instead of Ollama silently cutting them short. The window used is recorded with each run.
- 7a5dc18: Configure → Run Limits has an Ollama port setting, for an Ollama that listens on a port other than 11434. The address stays on this Mac (127.0.0.1), and each run records the endpoint it used.
- 6a7f281: Projects can now change selected operational limits from their defaults through a closed registry of operator limits. The first is the escalation search radius around a gap (1 to 4 hops, default 2). Values outside a limit's range are refused (#1653).
- 2edc8d6: A project can change the plugin sandbox's fuel, memory and per-file deadline bounds (advanced operator limits, to raise only for trusted plugins). The conformance gate still runs under the defaults (#1653).
- e9dbb87: A project can now save its own read-deny rules. They add to the built-in rules for environment, key and credential files, and every cloud investigation applies them: a captured source whose path matches is withheld from the model (#679).
- faa590e: A TypeScript rule that tests what a TypeORM query builder selected now states the query in D1, for example "the product variants that are not deleted and whose product is not deleted, among the variant ids". Other calls keep their gap.
- c289138: A "Dispatch to an agent" pill at the bottom of the workspace shows the quick dispatch shortcut until the first confirmed dispatch. It can be hidden, and Settings turns it back on.
- c2adbde: D1 now states a Python rule's local that is assigned more than once, using the assignment that reaches the condition, and states each local that value reads in its own sentence, such as the alias suggested from a website's domain name.
- 5a51f55: An investigation can now durably record that the egress firewall withheld an acquired read from a cloud provider, and reopening it verifies that record against the read and its citation.
- cad2792: When rule capture stops early in a file (for example at the per-file rule limit), D1 now says so in the rules lead and under the file's area, with how many exits and changes were not recorded.
- a8c9e40: Rust: axum and actix-web endpoints now record their authentication from auth extractors on the handler and axum `layer` / `route_layer` guards, and `endpoint-permissions.md` lists those guards.
- f859f0d: Semantic previews now handle VSCode-scale repositories in one pass: up to about 262,000 candidate and query texts are embedded, cached and reused, where the cache previously stopped at 100,000 and re-embedded the rest on every run.
- 61e4f25: A project can raise or lower how many source files and MiB one recovery retains for citations (advanced operator limits, up to 65,536 files and 1 GiB). Defaults are unchanged (#1653).
- 841d36b: Subsystem candidates are derived for a repository of up to 65,536 unit files, such as VSCode, instead of being omitted past 8,192; smaller repositories get the same candidates as before.
- 10c88a0: Adds `domain-analyst@29`, a domain analyst that works by calling tools natively on a local model, beside the current @28. Each run gets its own tool-calling runtime and loopback model proxy. Every request still passes the egress firewall and the step's own limits. A start on a cloud provider, or on a machine without the runtime, is refused before anything runs. Until the runtime ships inside the app, `CARTOGRAPH_AGENT_RUNTIME` names its entry script. Every other analyst version still uses the bounded loop.
- 9dcd0a0: TypeScript: a call through a configured instance (`new ShippingCalculator({ calculate })` reached through the class's `this.calculateFn(…)`) and a call on an instance looked up from a typed field (`this.allCalculators[code].calculate(…)`) are now inferred `CALLS`, so the rules in a configured default object are reached from the code that runs it.
- f4b9672: A raised source capture file limit now reaches the TS/JS selection walk, and a project can raise how many directory entries that walk examines (advanced operator limit), so very large trees can finish selection (#1653).
- 182dcae: Raise the embedding plan, working set and vector cache bound to 1,048,576 rows (3 GiB of vectors, 512 MiB of plan text) so a whole-VSCode semantic preview (476,907 candidates plus 81,654 hops) embeds in one pass. Sealing no longer copies the vector set and the ANN index frees each returned vector as it is added, so peak vector memory stays near twice the set (#259, AC-1507).
- 5cee04d: The investigation input can now withhold an acquired source read that the firewall refused: the model sees only a closed `firewall_refused` handle with no text, and the sealed disclosure records the source as `firewall_refused`, separate from omitted and redacted (#679).

### Patch Changes

- 11fcc53: An investigation that fails on a host error after the model proposed an action now keeps that action, such as the exact query it asked for, so the failure can be explained from the saved profile.
- e9407d0: A topic expert question, section draft or gap escalation still waiting in the agent pool when the app stops is now ended at the next start with its reason instead of staying queued, and can be cleared from Jobs. Jobs no longer offers Retry for agent jobs the app cannot retry.
- 8f0c830: A specialist investigation still waiting in the agent pool when the app stops is now ended as interrupted at the next start instead of staying queued.
- 2408999: TypeScript ingest no longer crashes on a comment inside an arrow function's parameter list, such as `(x, /* note */) => x` (found on VSCode).
- 4b3ade7: An area briefing now carries the state machines other areas declare (states, moves and transition guards), the area's neighbours first, so an Order expert sees the payment, fulfillment and refund processes beside the order's. D1 states an `onTransitionStart` guard as a refused move instead of a Gap row, and introduces `onTransitionEnd` rows as effects after a move.
- c6acec9: Saving an exported bundle now moves the finished file into place instead of copying it, so a very large export needs its size in free disk once rather than twice. It is still copied when the destination is on another drive.
- 87ca10d: Export bundle no longer holds the whole bundle in memory before writing it. Each artifact is written to a temporary file as soon as it is compiled, so very large repositories can be exported. On a 16,180-file TypeScript repository the export's memory peak fell from 13.4 GB, which ran out of memory, to about 10 GB. The exported file is unchanged.
- 682431c: The macOS app now ships its tool-calling runtime. `domain-analyst@29` runs on the bundled, signed `cartograph-agent` executable, so nothing has to be installed by hand. It is built from a committed lockfile with Pi pinned, on a pinned and checksum-verified Node release. The Claude lane is not in this build yet. `CARTOGRAPH_AGENT_RUNTIME` still overrides the bundled runtime for development.
- 1996119: The capabilities overview and the purpose-and-scope summary are drafted from the rules each capability enforces and the endpoints it groups, which their claims can now cite.
- bd069e6: A drafted claim that asserts far more than the facts it cites is left out, and a claim that names HTTP methods must cite an endpoint.
- 896ee6a: A drafted claim that states a number, count or version must cite a fact that shows it, and a claim about endpoints must cite an endpoint. Each composition now records why each left-out claim was left out.
- 2bd1959: A drafted figure counts as cited only when a cited fact shows it as a whole number, not as digits inside an id, and the capabilities overview is drafted without routes.
- 2b940e8: "Compose from the knowledge graph" now drafts D1 1.1 Purpose and scope from the actors and capability overview it summarises, instead of refusing it with no citable fact.
- ac3f27e: A composed claim that names a route is staged only when a fact it cites shows that route; the composer is told to cite the endpoint.
- 737b8f7: Drafting Purpose and scope reads a trimmed summary of the sections below it, so large projects fit the request limit, and a drafted claim that names part of a route must cite the endpoint that shows it.
- 702efde: D1 §1.5 now says what a Python condition's local name was assigned ("Here the API key is the API key whose code is the “Authentication” header.") and follows a local assigned a function's result into that function. Returns inside `except T:` and directly in a `try` body read as "a bad signature error is raised" / "no bad signature error is raised".
- 0cc3c0f: The product specification's caller-role table cites the derived Actors behind each role, so a drafted section can cite who may act.
- 14be2ad: Spec exports and ingests of very large repositories use less memory to compute the graph content hash, and the gap register no longer holds two copies of every row's provenance. On a 16,180-file TypeScript repository the hash step's extra memory fell from 1.5 GB to 1.0 GB, and every exported file is unchanged.
- adddb64: The Spec Workbench export uses less memory on large repositories. Each artifact is cut down to its preview as soon as it is compiled, instead of after the whole bundle has been built, and what the Workbench shows is unchanged. On a 5,360-file TypeScript repository the export peak fell from 5.6 GB to 4.9 GB.
- aeb901c: The spec export uses less memory when it builds the fact index for a large repository. On a 5,360-file TypeScript repository the export peak fell by about 190 MB, and the exported bundle is unchanged.
- 893ea74: Spec exports of very large repositories hold the fact and evidence index in less memory: the names, paths and commits that repeat across its rows are kept once. On a 16,180-file TypeScript repository the export's peak fell by about 0.5 GB, and every exported file is unchanged.
- 458cbaf: Captured Terraform state, Pulumi and trace inputs are now kept with their repository: the retained-source view counts them, and forgetting the source removes them too.
- 07681c3: The knowledge phase that follows each ingest reads the recovered graph in less memory. Each fact's provenance is held as compact text as the graph is read, so the graph is never decoded whole. On a 5,360-file TypeScript repository the phase's working memory fell by about 1.1 GB, and the derived facts are unchanged.
- 0ca14e2: Independent semantic admission now ranks graphs with more than 4,096 unresolved hops, up to the 262,144-row embedding plan bound, instead of abstaining on every family.
- e150cb9: Add `eval-runner`, the locked evaluation runner (ADR-0082). It scores one frozen semantic policy against held-out labels in an account agents cannot read, and prints an Ed25519-signed receipt of aggregate scores and digests only.
- e0ac18a: Facts observed in captured Terraform state, Pulumi exports and traces are now bound to their observed receipts beside their declaration bindings, and receipts a later recovery no longer uses are reclaimed.
- f58fcf0: Facts enriched from captured Terraform state, Pulumi exports and traces now get a receipt for the exact bytes they were observed in, kept and forgotten with their repository.
- 031606f: Scoped MCP evidence can cite where a fact was observed: kind `retained_observed_input` reads the exact span of the captured Terraform state, Pulumi export or trace.
- ee62dfa: The captured-source panel shows where a fact was observed: the exact span of the captured Terraform state, Pulumi export or trace, under "Observed in", beside its declaration.
- f5c9799: A semantic preview whose embedding plan is too large now says which bound it exceeded (text count, longest text or total bytes) and by how much.
- 4b3ade7: D1 §1.5 Rule cells now state a check a method runs on its receiver, a sum compared with a method's value, a flag bit, and membership in a method's value, instead of a Gap row. The last answer of a Python method that returns `True` or `False` elsewhere is now a rule even when the method's name has no `is_`, `has_` or `can_` prefix ("Otherwise, the user's lifetime or active subscription is true when the user's active subscription is set").
- 900a20f: Workspace Projects card: a long project name now wraps beside its Rename, Switch and Delete buttons instead of running under them, and the full name shows as a tooltip (#2252).
- 0ee08a9: The spec export reads the recovered graph in less memory. Each fact's provenance and each business rule's payload is held as compact text as the graph is read, so the graph is never decoded whole. On a 5,360-file TypeScript repository the export's working memory fell by about 1.3 GB, and the exported bundle is unchanged.
- 11fcc53: A failed investigation whose finish was refused now keeps the same bounded, redacted record of the rejected response as a decode failure.
- 23dd7f4: An analyst without closed action feedback (such as domain-analyst@17) that repeats an already answered metadata query now stops at once with `limit=repeated_query`, instead of re-reading the same page until the query byte limit (#1352).
- 76a1de2: The spec export uses less memory on repositories with many recovered business rules. Each rule's payload is held as compact text while the export compiles. On a 5,360-file TypeScript repository the export peak fell by about 0.5 GB, and the exported bundle is unchanged.
- 03fdc17: A run of a tool-calling analyst now records which runtime drove it, with the runtime's name and version and the installed harness version (for example `pi-agent-core@0.73.1`), beside the provider and model it already saved. Reopening the run shows the same record. Bounded-loop runs are unchanged.
- 4b42925: A saved local Ollama port is now used everywhere the app talks to the local model: section composition, area-expert questions, local escalation, the semantic preview, knowledge embeddings and the local lane status. Before, only investigations used it (#1653).
- 89d0d6b: Listing and checking completion choices for an investigation scoped to a neighborhood or an area now reads only that scope from the graph, so it no longer stops at the graph size limit on a large system.
- a99ce60: D1 states a signed value read back with an age limit in plain words: "the signed suffix, once its signature is checked and it is at most 600 seconds old".
- a0664e3: The diagram views' node, edge and flow caps are now operator limits, so a large repository can show more of its topology, flows and data model before truncation. Defaults are unchanged and artifact identity does not depend on them (#1653).
- 622596d: A stale accepted proposal whose cited files are all unchanged now says so (`cited_files_unchanged`), so it can be re-confirmed as it is instead of reading the same as one whose evidence changed.
- 5395201: Recovered user stories and actors are listed by name, so re-adding a source keeps every story number (US-R-NNNN) instead of renumbering them.
- c53ea05: TypeScript calls through barrel modules (`export * from './x'`, `export { a as b } from './x'`) now link to the function or method in the file that declares it, citing each barrel crossed, instead of staying unresolved call Gaps. On Vendure's core package this cuts unresolved call targets from 234 to 63.
- e123002: Workspace: the last ingest's totals, merged occurrences, layer breakdown and delta are shown again, folded under one line, instead of living in an unused card.

## 0.73.0

### Minor Changes

- af3e113: D1 §1.5: a record created into a local (`alias = Alias.create(…)`) is now a rule row, and an `elif` chain's earlier "no alias is given or …" tests are not repeated beside "no alias is given". SimpleLogin's random alias suggestion now reads "When a hostname is given and the user's include website in one click alias is set, and no alias is given, a new “alias” record is created."
- 85fad05: Cloned repositories are now parsed from the exact Git objects of the cloned commit instead of the checked-out files, so evidence citations match the commit byte for byte. A selected file missing from that commit stops the recovery instead of falling back to the checkout.
- 6250aa7: **Behavior change:** the config resolver no longer reads env-file values implicitly. It reads a value only for a key listed in `env_read` in `cartograph.system.toml` or in `read` in a repository's `.cartograph/env.toml` (exact names, no globs). A channel or client call whose key an env file sets but no list names is now a Gap that names the key, the file and how to list it. URL values lose any `user@` part; `[env]` values and the secret refusal are unchanged (#933, ADR-0075).
- 780678e: Allowlisted env values now appear in toolchain facts: an env file's tool lists the values of the keys named in `env_read` / `.cartograph/env.toml`, each citing its own line, and opening that evidence shows the line alone. Template files (`.env.example`, `env.sample`, `.env.dist`) are marked as templates, and the config resolver now prefers any real env file over a template (#933, ADR-0075).
- 090476e: Resolving an unresolved call now cites captured source for every piece of evidence: the call's Gap, its file and its tests get primary-source receipts, so a task no longer mixes in unverified working-tree reads. A captured span too large for the task is left out and named, instead of failing the task.
- 14e0073: Draft the business rules section (D1 1.5) on your local model with "Compose locally, checked": Cartograph drafts ten rule rows at a time, checks every claim against the rows it cites (coverage, quotes, numbers, code names, ordering), retries what fails with the reasons, has an auditor send unsupported claims back once, and stages the drafts for review, one per area plus one for the rules no area owns, so a large system's rules are drafted whole (#2083).
- 650c570: Go files are now parsed from the retained capture, and their files, symbols, routes, gaps and tests carry primary-source receipts, so evidence for Go code is cited from captured source instead of unverified working-tree reads.
- 5c078b1: Go: `net/http`, chi and gin endpoints now record their authentication from route middleware (handler wrappers, chi `With`, `Use`), and `endpoint-permissions.md` lists those guards.
- 0b46e2b: Helm charts and helmfiles are now read for the environments document. Each chart is listed with its version and the key paths of its default values, and each helmfile release is an environment that names the chart it installs and the values files it uses. No value is read and templates are not rendered; templated helmfiles, inline values, `set` and `secrets` are listed as not established.
- cef6a48: Java files are now parsed from the retained capture, and their files, symbols, entities, gaps and tests carry primary-source receipts, so evidence for Java code is cited from captured source instead of unverified working-tree reads.
- 1e444e7: Java JUnit tests are now recovered and linked to the Spring endpoints they call through MockMvc (`mvc.perform(post(ENTITY_API_URL))`), with URL constants resolved across files and the statuses each test expects, so the product specification's acceptance section lists Java test scenarios per capability. On the JHipster sample app: 215 tests, 97 links reaching 37 of 38 endpoints.
- 5035cf0: Java methods now yield business rules (#2020): a `throw` or a valued `return` under `if`/`else` branches is a guarded exit with its sanitized conditions, owned by its method, like the Python, TypeScript and Go producers, and D1 states a thrown exception by its message or its kind ("it fails with the error “Invalid id”"). On the JHipster sample app this recovers 89 rules across 20 files, such as `userDTO.getId() != null` → `BadRequestAlertException("A new user cannot already have an ID")`.
- 1e444e7: Java Spring endpoints now carry who may call them, read from their method-security annotations (`@PreAuthorize`/`@PostAuthorize` SpEL role, authority and sign-in checks, `@Secured`, `@RolesAllowed`, `@PermitAll`, `@DenyAll`, on the handler or its class) and from the `HttpSecurity` URL rules that cover them, with string constants resolved across files. `endpoint-permissions.md` now covers Spring, and the product specification's §1.2 lists each caller. On the JHipster sample app all 38 endpoints are read: 6 public (register, activate, sign-in, password reset), 22 for signed-in users and 10 for `ROLE_ADMIN` (via `AuthoritiesConstants.ADMIN`); before, none were.
- 3940c52: Kotlin files are now parsed from the retained capture, and their files and symbols carry primary-source receipts, so evidence for Kotlin code is cited from captured source instead of unverified working-tree reads.
- 009a1cd: Kotlin: business-rule evidence now also covers elvis exits (`x ?: throw …`), exits in subject-less `when` entries and `require`/`check`/`requireNotNull`/`error` preconditions; plain-name string templates stay readable in rule messages, and a scope-function or precondition name another file rebinds no longer counts as the standard-library call.
- 04e8bb9: Kotlin: what starts a Kotlin system is now recovered as entry points. `fun main` (top level, or `@JvmStatic` in an object) is a process start, and an HTTP server when it starts Ktor's `embeddedServer` or Spring Boot with web endpoints; Spring message listeners are queue consumers with their topics, `@Scheduled` functions are scheduled jobs, and `CommandLineRunner`/`ApplicationRunner` runners are startup tasks.
- 00f1be2: Kotlin: JetBrains Exposed tables are now recovered. `object`s extending an Exposed `Table` or id table become SQL tables with their columns, and DSL statements called on them (`selectAll`, `insert`, `update`, `upsert`, `deleteWhere` and the like) read and write those tables. Computed table names, and statements on receivers that are not recovered tables, are reported as Gaps. Room entity properties whose accessor only reads another value's `field` member are no longer taken for columns.
- 24c9f47: Kotlin: guarded returns and throws in Kotlin functions are now recovered as business-rule evidence, like Java's, with sanitized conditions and the same unestablished-interpretation Gaps; the specification no longer lists Kotlin files as source no rules were read from, and a thrown Kotlin exception reads as the error it raises.
- 0894169: Kotlin: JPA entities are now recovered. Classes annotated with a Jakarta or javax `@Entity` become JPA data entities with their table and fields (`@Id`, `@Column`, `@Transient`), in the same shape as Java's, and relation properties (`@ManyToOne`, `@OneToMany` and the rest) link them to their target entities. Computed names and relation targets that are not recovered entities are reported as Gaps.
- c97129c: Kotlin: JUnit and `kotlin.test` test functions are now recovered as test cases, and the requests they make through Spring's MockMvc (the Kotlin DSL or `MockMvcRequestBuilders`) or Ktor's test client are linked to the Spring and Ktor endpoints they exercise, with the statuses they expect. Requests with computed URLs or paths no endpoint serves are reported as Gaps.
- 83ce1d9: Kotlin: Ktor server routes are now recovered. Import-proven `routing { }` blocks and `Route` extension functions yield Endpoints with `route("…")` prefixes composed, including groups mounted from another file, and each handler lambda becomes a Symbol that owns its calls. Computed paths and unmounted or ambiguous groups are reported as Gaps.
- 092b0c7: Kotlin: Android Room persistence is now recovered. `@Entity` classes become SQL tables with their columns, and `@Dao` functions read and write those tables, through literal `@Query` SQL or through the entity that an `@Insert`/`@Update`/`@Delete`/`@Upsert` parameter names. Computed queries, and tables or types that no entity declares, are reported as Gaps.
- 6eeba14: Kotlin: Spring Data repositories are now read. An interface extending `JpaRepository`, `CrudRepository` or another Spring Data base is linked to the JPA entity it manages, and calls such as `findById`, `save` or `deleteAll` on a repository-typed parameter, property or local become reads and writes of that entity from the calling function. A repository whose entity is not a recovered JPA entity is reported as a Gap.
- dd7808c: Endpoints, modules and other facts that Python, Go, Java, Rust and Kotlin recovery builds across a directory now carry primary-source receipts when everything they cite lies in one captured file, so Spring and Go endpoints are cited from captured source.
- 5291df0: D1 states when a local is discarded or cleaned under a branch: `alias = None` as "the alias is discarded" (an alias that belongs to another user is never returned) and `new_name = new_name.replace("\n", "")` as "line breaks are removed from the new name" (#1998).
- 9f272b8: Facts whose evidence spans several captured files, such as Flask routes and the Blueprint mounted in another module, now carry one primary-source receipt citing every file, so most SimpleLogin endpoints are cited from captured source.
- f921891: Projects: the Workspace's **New project…** sets the loaded system aside (its recovered facts, sources, findings, investigations, drafts, reviews, decisions and job history) and restarts Cartograph empty, so you can load a different repository without reinstalling. **Projects** lists every project and switches between them with no re-ingest, renames them and deletes parked ones. Settings, provider keys, run limits, the local model and installed adapters are shared by every project.
- 09d56b5: Documentation, found decision records and declared API references are now read in one walk, each file once. `documentation-evidence.md` names each document's role (`decision_record`, `api_reference`), a decision record's document links to its ADR, and a declared API reference in a hidden folder or an OpenAPI `.json` file is listed as a document.
- 8e22ce2: Drafted sections can now cite the derived capabilities and actors the product specification prints. A draft that cites one is no longer listed as `unresolvable`: it is printed in `best-effort` exports, and the cited capability or actor has its row in `fact-index.md`.
- 9fe01f4: Python files are now parsed from the retained capture, and their files, symbols, gaps and tests carry primary-source receipts, so evidence for Python code is cited from captured source instead of unverified working-tree reads.
- 55ac02d: D1 §1.5 Changes: a record created or deleted on a model class under conditions is now a rule row, e.g. SimpleLogin's "When a hostname is given and no alias used on with this alias ID and hostname exists, a new “alias used on” record is created." (66 new rows on SimpleLogin).
- f2bd8d2: Investigations now ask for a role (domain analyst, evidence auditor, spec writer, gap triager, section writer) instead of a version number. Each role runs its current version, and its budget follows from that version, so the separate budget picker is gone. New investigations default to the current domain analyst. Older versions remain an explicit choice in the composer's version list, and follow-ups keep their saved version.
- de258de: Rust files are now parsed from the retained capture, and their files, symbols, gaps and tests carry primary-source receipts, so evidence for Rust code is cited from captured source instead of unverified working-tree reads.
- b986f41: Guarded `return`s in Rust functions are now recovered as rule evidence, like Python, TypeScript, Go and Java. The specification states them in words: a returned `StatusCode` reads as a refusal with that status, and an `Err(…)` as the error it fails with, by its message or variant name.
- dc78d7d: Rust code that runs literal SQL through `rusqlite`, `postgres`, `tokio-postgres`, `sqlx` or `diesel` now shows the tables each function reads and writes, with their operations and columns. Statements built with `format!` around a computed table are reported as gaps.
- efe33ca: Rust `#[test]` and `#[tokio::test]` functions are now recovered as tests, linked to the functions they run and, for axum and actix-web test requests with literal paths, to the endpoints they call and the statuses they assert. SQL in test code no longer shows up as tables the system reads or writes.
- f6e3a55: D1 §1.5 Changes: a record added with `Session.add(Model(…))` and a query delete (`Model.filter_by(…).delete()`) are now rule rows, e.g. SimpleLogin's "When no deleted alias with this email exists, a new “deleted alias” record is created."
- 270e6b2: Spring Boot profiles are now environments in the environments document: a configuration document activated for a profile, and a profile-specific `application-<profile>` file whose profile a base file declares, each listed with the key paths it sets. No value is read. Undeclared profile files and settings files named like an environment (`settings/production.py`, `config/staging.yml`) are listed as not established.
- 6a8ff42: Terraform resources are now told apart by the root module that declares them, so two environments (for example `envs/prod` and `envs/staging`) that declare the same resource keep both declarations instead of one replacing the other. Terraform at the repository root keeps its existing ids. A repository's `state_json` still applies to a single root module in a subdirectory; with several root modules it is not applied and a Gap explains why.
- ea29d30: D1 §1.5 Changes for TypeScript: repository writes, published events, service calls and field changes under conditions are now rule rows, e.g. Vendure's "When the corrected quantity is 0, the “order line” record is deleted." and "When the user is not verified or a pending native activation is given, the “account registration” event is published."
- 9b8c5fb: An accepted proposal can now be reconciled from its review dialog: resolve its gap again on the current source to get a new proposal, and withdraw the old acceptance so it leaves the curated content.

### Patch Changes

- 0f5d9d6: Area briefings are listed by area name, so the same source gives the same order and file numbering every time it is added, rather than an order that changed with each registration.
- 885fa7d: Curated reads no longer fail at 257 accepted section drafts or findings. Accepted drafts and findings are now bounded by their total size (64 MiB of drafts, 16 MiB of findings, the same worst case as before), so many small ones are all read; past that, `curated.md` states `accepted_too_large`.
- 14618a9: D1 §1.5 Rule cells no longer say an `elif` branch's value differs from each earlier option before saying the value it takes ("the form name is not “cancel”, and the form name is “change-yearly”" now reads "the form name is “change-yearly”").
- ff054b9: An allowlisted env key whose first assignment is dropped (multiline or secret-shaped) no longer takes a later duplicate's value in toolchain facts: the first assignment wins, as documented (#933).
- 489c2d8: Areas: a widely used errors or exceptions module is shared code, not the core of the area that imports it most.
- 4b95f59: D1: a Flask flash message built from an f-string now reads "the user is shown “Deleted … aliases”" instead of "it runs “flash”".
- 214d665: "Compose locally, checked": a local model can no longer get more claims past a chunk than the workflow allows, claims are never staged when the auditor's reply is unreadable twice, and two claims that differ only in case are both kept.
- 6c219d9: Go repositories now have their GORM models recovered (#2019): each model is a data entity with its table and columns, and the functions that create, read, update and delete it through GORM read and write it, across packages. GORM models also become domain candidates. On the Gin RealWorld example this recovers 7 models and 28 reads and writes, with 2 gaps where a model arrives as an untyped `interface{}`.
- 985ca04: Large repositories use much less memory during ingest. Source binding and ingest metrics now decode one fact at a time, and the knowledge phase reads its base without business-rule text it never uses. On a 5,360-file TypeScript repository, the ingest peak fell from 4.3 GB to 2.1 GB and the knowledge-phase peak from 3.8 GB to 2.8 GB.
- 2b90ac4: The Java adapter records Bean Validation constraints (`@NotNull`, `@Size`, `@Pattern` and the other built-ins) on the Spring endpoints whose handlers validate them, with Gaps for custom constraints, validation groups and cascaded fields (#2162).
- c4b60b2: Kotlin Room: an explicit `tableName = ""` now names the table after its class, as Room does, and a body property with a custom getter stays a column when it is initialized or its accessors use `field`.
- 0d3ed17: D1 states `x.name = x.name.replace("\n", "")` as "line breaks are removed from the alias's name" and `"\n" in alias.name` as "the alias's name contains a line break", instead of a Gap (#1998).
- 502bb20: D1 §1.5 now states an area's state machines first, before its capabilities' rules, so an area briefing that has to be cut to its size bound keeps the area's lifecycle and the guards of its hooks.
- d78d3a0: Areas: a large area whose only files without its subject are named for one other subject is split in two, so Vendure's refund entity, resolvers and default refund process get their own Refund area instead of sitting in Event bus.
- c6e5ff0: A failed investigation now keeps a short, redacted excerpt of the model response it rejected, along with the response's size and hash, so the failure can be explained from the saved profile without reproducing it. Responses rejected as sensitive or as copied source keep no text.
- 582f405: Rust: a `#[path]` module declaration that Cartograph does not follow is now a Gap on the declaring file that says why, instead of being skipped silently.
- 1327dd6: The Rust adapter now gives files wired in by `#[path = "…"] mod name;` the module they are declared as (so `super::` in a `*_tests.rs` file resolves), and Rust entry points are recovered: `fn main()` binaries (desktop app, gRPC server, HTTP server, command or process), `#[tauri::command]` IPC commands and tonic gRPC service methods.
- bdf9a6a: Investigations scoped to a neighborhood or an area now retain only the scope's facts while keeping the complete-graph identity, so they prepare on graphs larger than the task's limits (#1602).
- 6e27a7f: Every investigation start, including the older start path and MCP, now accepts only a specialist version the catalog offers; an unoffered version is refused with "This specialist version is not available." Saved runs of any version still reopen.
- 0a343f5: D4 success responses: a Flask `jsonify(alias=…, **serialize(…))` now states the keyword's keys and the keys the repository helper builds, instead of "body built outside the handler" (SimpleLogin's alias creation now reads "201 with JSON keys alias, email, id, …").
- 561da1d: Primary-source receipts for TypeScript and JavaScript now record the tree-sitter version the parser actually links; receipts saved before this change keep validating.
- 7b635c0: The desktop client now reads which contract each specialist version uses from a table generated from the host, so domain-analyst @21–@23 and @28 start and reopen with the right request and result contract instead of being missed by hand-kept lists.

## 0.72.0

### Minor Changes

- 5f5907a: The product specification's §1.4 "Acceptance criteria per capability" now lists, under each capability, the recovered tests that exercise its operations: the scenario as the test author named it (snake_case test names read as words), the test file, the operations it requests and a citation for each reference. On Vendure `packages/core` the section goes from a pointer to other files to 1,924 scenarios; on SimpleLogin, 296.
- 4d5594e: An area has a compiled briefing: its part of the product specification (capabilities, acceptance criteria, business rules and flows, as the export prints them) with the names of the areas it calls and is called from. A topic expert agent will start from it; nothing in the app shows it yet. The showcase run writes one briefing per area under `briefings/`.
- 9b125b0: D1 §1.5 states, after each area's capabilities, the rules in the area's own code that none of its capabilities reach, so an area's briefing carries them (#2066).
- c754c5d: Every export now includes `bundle-manifest.md`. It states what the export was made from (the document catalogue version, the export mode, the graph content hash, the curated revision and each source with its revision) and lists every exported file once under the part of the rebuild handoff bundle it belongs to. It says nothing about your system and cites no fact.
- cfb451a: D1 §1.6 Business flows now says what each flow does. With derived Capabilities, each operation gets one line under its capability: the handler, the functions it reaches in trace order, and the entities those functions write and read with their operations, citing the handler and data edges (SimpleLogin: `DELETE /api/user` is handled by `delete_user`, calls the audit-log helpers and writes `Job` (insert) and `UserAuditLog` (insert)). Operations handled with nothing further are listed together, and flows outside every capability keep the trigger table.
- 772d7c2: The product specification now states rules, callers and capabilities in business terms (#1999). Each rule row has a Rule sentence in domain terms, such as "When the user cannot create new alias, the request is refused with status 400", with the source condition kept beside it as evidence, or a Gap when the condition is outside the rendering rules. Callers are named by role: API client, administrator, Twilio webhook. Capability names no longer carry file paths.
- b6e46d5: Add the Architecture view to the Spec Workbench (#2049). Selecting `architecture.md` now draws the recovered system as C4 diagrams you can move through: system context, containers, and each container's components. Click any box or arrow to see the facts it rests on, with their confidence and source location; Gap boxes and arrows are drawn apart.
- 6c50e04: Draw the recovered system in the C4 model (#2049). Every export now includes `architecture.md`, with Mermaid diagrams and cited tables for system context, containers (web application, processes by entry-point kind, operator commands, scheduled jobs, client applications, data stores and outside systems) and per-container components from the subsystem areas, and `architecture.dsl`, the same model as a Structurizr DSL workspace. No model call; each box and arrow carries the confidence of the facts it rests on.
- a0a5c54: The product specification now lists a system's capabilities by area. Each capability sits under the subsystem that holds most of its code, so the capability list, the acceptance criteria, the business rules and the traced flows read area by area (for example Alias, Authentication, Custom domain) instead of as one flat list per controller. Capabilities whose code belongs to no subsystem are listed last under "Other capabilities".
- 359a5b5: TypeScript recovery states how a value is chosen: a predicate's answer, a pick with a fallback (`rate || this.defaultTaxRate` with its `.find(r => r.test(zone, taxCategory))`), and a configured strategy's answer are business rules, so D1 and the area briefings say how Vendure picks a tax rate.
- 3947c42: Calls through configuration now reach plain objects as well as classes. An object declared with an interface type (`const defaultPaymentProcess: PaymentProcess = {…}`) or returned by a function declared to return one implements that interface. A loop over configured hooks (`for (const process of processes)`), or a call on a typed class field (`this.config.onTransitionStart`), links to every implementation. On Vendure, a state-machine transition now reaches the default order, payment, fulfillment and refund process rules (#2069).
- 2471d6b: `data_model.md` now reads per entity. Its diagram draws only the entities and the relations between them (SimpleLogin goes from 80 entities plus 300-odd `Symbol:` writer boxes to the 80 entities), and a new "Entities" section gives each entity its table, columns, the entities it references and is referenced by, and the functions that write or read it, grouped by operation. The id tables with every citation follow unchanged.
- a535c93: Counts the documentation files the trace read. Each repository's coverage inventory now records how many Markdown and plain-text files were found, read and skipped, with the reason for each skip, and how many sections were read. `coverage.md` prints the counts under the repository. A skipped or partly read file marks the inventory as limited.
- b0c2435: Lists where existing documentation no longer matches the code. A route named in a Markdown file that no recovered endpoint serves, or that is served only with other methods, and a link or path to a file that is not in the repository, now appear under "Documentation drift" in `drift_register.md`, each with the document, the reference and what it was compared with. The entry goes away once the documentation matches again.
- 3c22508: Every export now includes `documentation-evidence.md`: the documentation files of your sources, their sections by heading, and the files, routes and other recovered facts each section names, with citation ids that are in the fact index. It counts the links by rule and the documentation drift entries by kind, and never holds the text of a section. A section drafted for one subject can now cite the documentation sections that name that subject.
- 2d30143: Links existing documentation to the facts it names. A code span or link in a Markdown section that names exactly one recovered fact, by its id, a file path, a route written as a method and a path, or a unique resource, channel, entity, service or workload name, is now a `MENTIONS` link from that section to the fact, citing the reference. A reference that matches nothing or more than one fact is not linked.
- aba16de: Reads a source's existing documentation as evidence. Ingest now stores each Markdown and plain-text file of a registered source as a `Document` fact with one `DocSection` fact per section, carrying its heading, level and byte span. Section text is not stored and headings are redacted. Files over 1 MiB or not UTF-8 are counted and skipped, and a re-ingest drops the facts of a removed file or section.
- c88e66d: Documentation is held as stated intent: the documentation pass changes no other fact, stores no section text, and redacts the route or path of a documentation drift entry, including in the entry's id.
- dbe6cc1: Names the facts behind a stale section draft. For each accepted draft of a document section, `curated.md` now has a row under "Accepted section drafts" with its state and two lists: the cited facts whose content changed since the draft was written, and the cited ids that no longer resolve to a fact. The Spec Workbench read carries the same lists. They come from comparing recorded and current content hashes, with no model involved. The section appears only once a draft has been accepted.
- 4809d0f: A drafting run no longer stores a draft when one of its statements spells out a file path or a code name of a fact that statement cites. Requirements and rules state what the system does; where the code is stays in the citation and the fact index. The run reports `statement_names_source`, and you can run the draft again. Routes, field names and message names may still be stated.
- 54133e7: The knowledge phase now also embeds the flows traced from a repository, not only its endpoints, symbols and other base facts. Each flow is rendered by its trigger, its status and its hops, with secrets redacted first, and at most 2,048 flows are embedded per run. Re-ingest a repository with the local embedding model installed to get them.
- 3f28d7b: Capabilities now cover work that no HTTP request starts. Each process start becomes an entry point with a kind read from the source: a Python `__main__` block or operator script (SMTP mail listener, database `LISTEN` listener, queue consumer, polling loop, operator shell, command), the container command, a Procfile process, or a declared console script or `bin`. Each entry point, and each schedule file's jobs, sits under a capability in the product specification's §1.3, and its traced flow is in §1.6. An entry point that cannot be resolved is recorded as a gap and named, not dropped. On SimpleLogin, mail receiving and forwarding, the job runner, the event listener, the monitoring loop, 17 operator commands, the operator shell and 18 scheduled jobs now appear as 12 capabilities. Before this change none of them appeared.
- 28936f0: Capabilities are now also recovered from Flask Blueprints: the routes a Blueprint serves from one file become one capability candidate that cites each endpoint and its handler function. Routes registered on the application itself belong to no group. Re-ingest a repository to get them.
- c5d0eb6: `journeys.md` now covers server-rendered Flask applications. Cartograph reads each page's route, its redirects, the templates it renders and their links and forms, and walks them from the home page to the form submissions that change data. On SimpleLogin, `journeys.md` goes from empty to 21 journeys, each ending in an observed database write. For example: home → dashboard → directories → create a directory, which writes `directory_mailbox` and is covered by `test_create_directory`.
- 1c88ae2: Go code now yields business rules: guarded returns and error responses written before a bare `return` (`http.Error(w, "forbidden", http.StatusForbidden)`) are recovered, and D1 §1.5 states them in domain terms ("When the request's method is not “POST”, the request is refused with status 405 (“method not allowed”)"). On true-line/agent-bot, §1.5 goes from no Go rows to 186 stated rules (#2009).
- 8c19664: Capabilities are now also recovered from Go and Rust HTTP routes: handler functions in one Go package (`net/http`, chi, gin) or one Rust module (axum, actix-web) become one capability candidate per file that cites each endpoint and its handler. These frameworks give a group of routes a path prefix and no name, so the package or module is the group. Re-ingest a repository to get them.
- 8b0a1e7: Recover what a system changes, not only what it refuses (#1998). Python assignments through an attribute and calls to repository functions are now rule facts with the branch conditions around them. D1 §1.5 lists them under each capability as "Changes", and §1.2 shows whom a guard makes the request act as. On SimpleLogin this states how an API key or a signed-in session becomes the acting user, and that deleting an alias moves it to the trash for the configured number of days or deletes it.
- 9267a34: An investigation can be scoped to one area. A start request may name a subsystem area (`{"type":"area","id":...}`), and the task then reads only that area's files and what they define. An area that no longer matches the ingested code refuses the start instead of answering from the whole system. This is the groundwork for topic expert agents; nothing in the app offers the choice yet.
- d17bd54: Java systems now show what starts their work: a `public static void main` (an HTTP server when it runs Spring Boot and the repository has Spring web endpoints), `@KafkaListener`/`@RabbitListener`/`@JmsListener`/`@SqsListener` methods as queue consumers with the topics or queues they name, `@Scheduled` methods as scheduled jobs with their schedule, and `CommandLineRunner`/`ApplicationRunner` beans as startup tasks. Each is an entry point flows start from and a capability in D1 §1.3. On the JHipster sample app: the Spring Boot server and the `removeNotActivatedUsers` scheduled job.
- db7a891: Capabilities are now also recovered from JAX-RS resource classes in Java: each resource class becomes one capability candidate that cites the class, its HTTP endpoints and their handlers. Sub-resource locators and resource methods declared on an interface stay route gaps and give no candidate. Re-ingest a repository to get them.
- 3c8bfd2: Files left in no subsystem area that share a word of their own names now form that subject's area, so configured strategies such as Vendure's tax zone strategies reach the documents and topic experts (AC-0970).
- 5b00c2b: Lets an outside agent such as Codex or Claude Code start a drafting run of the in-app section writer over MCP. A session with the task grant starts `section-writer@1` for one drafted catalogue section with `cartograph_task_start`, and `cartograph_task_result` says whether the run staged a draft and gives its id. The draft is written by the role in the app, passes the same admission as a run started in the app, and waits for human review there.
- e8d0ed2: Node scripts behind a `node` shebang are entry points, and a command-line script's subcommands (`run`, `status`, …) are each a command, grouped under one capability (#2008 follow-up).
- f1d0b80: Tests written for Node's built-in runner (`node:test`) are recovered as test cases (#2008).
- 9195f65: Python code now yields business-rule observations: every guarded `return` and `raise` in a function is recovered with its branch conditions and outcome, sanitized like TypeScript rules. The product specification's §1.5 lists a Flask handler's `raise` and error-status responses (`return jsonify(error="Forbidden"), 403`) under each capability. On SimpleLogin, §1.5 goes from empty to 155 rejections and `rule-evidence.md` from 0 to 2,540 observations.
- 2e58ad5: Python callers are now named by the guard that admits them. The product specification's §1.2 adds a "Callers by recovered guard" table (signed-in user via `login_required`, a caller passing an API guard such as `require_api_auth`, Flask-Admin views, and any caller where no guard is recovered); user stories say "As a signed-in user, I can view coupon" instead of "As an API caller, I can coupon"; and `security.md` and `endpoint-permissions` print guards by name instead of symbol id.
- de77085: Derived capabilities now read as what a reader does with them: "Manage draft orders (admin API)" instead of "admin GraphQL operations in DraftOrderResolver", "Manage custom domain (dashboard)" instead of "HTTP operations in blueprint dashboard (app/dashboard/views/custom_domain.py)", "Reset password (authentication)" for a password-reset view. The verb comes from whether any operation changes state, the subject from the class or handler-file name without its framework words.
- 78b7098: `journeys.md` now reads as a path through the application instead of a list of graph ids. Each journey opens with a **Path:** line, for example "`/` (index) → `/dashboard/` (via `set_index_page.index`, conditional) → `/dashboard/mailbox` (via a link in `templates/menu.html`) → …, then POST /dashboard/mailbox/<int:mailbox_id>/, which writes `AuthorizedAddress`, `UserAuditLog`", and the steps, goal, writes, flows and tests print page routes, handler names, operations and table names. Ids remain on the `Id` line, in the fact index and in `bundle.json`.
- 2cd878e: Lets you re-draft a stale section. "Re-draft this section" on an accepted draft that is stale or unresolvable now asks Cartograph for the request, which is refused with a reason when the draft is current or a re-draft of it is already waiting. The draft the run stores names the accepted draft it replaces. The accepted draft stays printed until you accept the re-draft, and is then kept as replaced. Rejecting a re-draft changes nothing else and allows another. An existing review store is upgraded in place the first time it is opened, with every stored draft kept.
- f587314: Ingest now refuses a checkout whose `.env` or `.env.*` files hold secret-shaped values (recognized secret patterns, token-shaped values, URLs with a password, or real values under credential key names). The refusal names each file, line and key, never the value, and says how to proceed: remove the values or files, and declare non-secret values the resolver needs in `cartograph.system.toml` under `[env]`. Previously such values could reach the graph and exports.
- ca8bb5c: State more rules in plain language (#2028). D1 now restates conditions that test request keys and text membership, look records up (`Alias.get_by(email=…)`), check types and lengths, compare times, read headers through a constant, and call validation or check functions. On SimpleLogin the share of §1.5 rejection rows left as "Not stated in domain terms" drops from 25% to 2%.
- f5ad621: Test code is no longer a source of business rules, and `rule-evidence.md` and D1 §1.5 say which source no rule evidence was read from (e.g. "It is not extracted from the 153 Go files in this export"), so the rules are not read as covering the whole system (#2009).
- bcef4a9: Recover more business rules (#1998). D1 §1.2 now has a "What each guard checks" table that lists the refusals a repository guard and its wrapper reach. D4 and D1 tie response keys to their statuses. Rule rows state a configuration constant's default and its environment variable, read from a new numeric or boolean `defaults` field on Python `ConfigRead` nodes. Deletes and updates made through a model classmethod that returns a query (`Alias.filter(...).delete()`) are now recovered as writes.
- 192a989: Recovery now goes past refusals. Flask endpoints record their flask-limiter rate limits, with the value read from the decorator or from the configuration constant it names (an environment default is named as such). `endpoint-inventory.md` gains a "Requests and responses" section that states, per endpoint, the inputs the handler reads, the statuses, body kinds and JSON keys it returns, and its rate limit, each cited. The product specification's §1.3 now states, per operation, what it takes, its success status and the entities it inserts, updates or deletes, and names what was not recovered; §1.5 lists the rate limits per capability. Calls through an imported submodule (`from app import alias_delete; alias_delete.delete_alias(...)`) now resolve. Writes made by assigning a column of an annotated model instance (`alias.enabled = False`) are now recovered as updates, calls on `self` reach the class's own method, and a function called in a rule's condition (`user.can_create_new_alias()`) is followed into its guarded exits, so the free-plan alias quota and the paid-plan exemption appear under the rule. On SimpleLogin: 69 endpoints with rate limits, including alias creation at 100 a day, 50 an hour and 5 a minute.
- 33b60c0: Product specification sections can now be composed in one model call from what Cartograph already knows. The section composer is handed a D1 section as the export compiles it, with its capabilities, operations, rules and flows, plus the facts that section cites. It writes dense prose claims that cite only those facts. Claims that name a source file or code symbol are left out. The rest are staged as a draft for review. A cloud composition sends only the payload you approved. On SimpleLogin, a trial of this approach turned §1.3 to §1.6 into 48 cited claims for about $1.
- 8f686b5: Shows what a re-draft changes. In the Spec Workbench, a draft that replaces another now has "Change from the draft it replaces": the claims added, removed and kept, and the cited facts that are new, no longer cited, or cited by both with different recorded content. It is there before you review the draft, and it comes from comparing the two stored drafts, with no model involved.
- c383c22: `security.md` now has an "Access control per capability" section: under each capability, every operation with who may call it as the recovered permission site states (a NestJS `@Allow` permission set, a Flask `login_required` guard, none recovered, or not established and why), each with a citation. On Vendure `packages/core` the document goes from "No explicit security findings" to access control for 310 operations under 42 capabilities; on SimpleLogin, 263 operations under 96, with the unguarded ones being sign-in, registration, OAuth and webhook routes.
- 12fb842: Capabilities are now also recovered from Spring MVC controllers in Java and Kotlin: each controller class becomes one capability candidate that cites the class, its HTTP endpoints and their handlers. NestJS controllers keep theirs under the same rule, now named `knowledge.capability.http-controller`. Re-ingest a repository to get them. JAX-RS, Flask, Go and Rust route groups are not covered yet.
- d1ffdb6: Keeps a stale section in the product specification and marks it. When a fact cited by an accepted section draft changes, a `best-effort` export of `product-specification.md` still prints the draft in its section, under "Stale: cited facts changed since this text was accepted" with the changed citation ids, and each claim that cites one ends with "cites a changed fact". A draft that cites a fact no longer in the graph is not printed; the section says how many are missing and `curated.md` names them. The document header now lists the stale sections, or "none". A `verified-only` export prints no drafted claim, as before, and carries the same list.
- f6ebdb9: Declarative state machines (a `transitions` table, as in Vendure's order, payment, fulfillment and refund processes) are recovered from TypeScript, and D1 §1.5 states each one's states, allowed moves and hook guards under its area (#2067).
- fdd82bd: Calls through an injected strategy (`taxZoneStrategy.determineTaxZone(...)`) now reach the repository's classes that implement the strategy interface, so the business rules in default implementations such as `DefaultTaxZoneStrategy` are stated under the capabilities that call them (#2069).
- bcdf2a9: The knowledge phase now groups a system's source files into named subsystems: files that belong to one concept are one group even when they sit in different directories (`api/views/alias.py`, `dashboard/views/alias_transfer.py` and `alias_utils.py` are one `Alias` subsystem). Each group is a weak candidate that cites every member file, files used from everywhere are set aside as `Shared code`, and no model or embedding provider is needed. On SimpleLogin it finds 14 subsystems such as Authentication, Custom domain, OAuth and Phone.
- fee23c1: Subsystem areas read more like the system's real subjects. An area is now named for the term most of its files share instead of a rarer word a few of them carry (for example "Metric" instead of "Backend and metric", "Event bus" instead of "Bus and event"). Files that only resemble each other weakly are no longer published as an area with an invented name, an area that held two subjects is split in two, and two areas with the same name are merged. A widely used file that is named for a subject (for example `alias_utils`) now stays in that subject's area instead of moving to "Shared code", which kept the Alias area from disappearing. In the product specification, capabilities that no HTTP request starts (scheduled jobs, operator commands, mail listeners) are listed under the area of the code they start, instead of under "Other capabilities".
- 78c7688: Ask an area's expert. The Spec Workbench lists the areas the knowledge phase grouped the code into; choose one and ask a question, and `topic-expert@1` answers from that area's part of the product specification in one model call. You review the exact payload before it is sent. An answer is labelled as the agent's reading, cites the specification rows it rests on, says what the briefing does not hold, and names a neighbouring area to ask instead when the question belongs there.
- 3640d5c: Recovers the data model of TypeORM codebases. An `@Entity` class imported from `typeorm` becomes a data entity with its own columns (type, primary key, nullable, stored column name) and its base class, and each `@ManyToOne`/`@OneToMany`/`@OneToOne`/`@ManyToMany` relation becomes a relation to the target entity the import names. `data_model.md` now also lists every entity's columns for all languages (SQLAlchemy, JPA, TypeORM), each citing its entity. On Vendure `packages/core` the data model goes from empty to 89 entities and about 470 relation fields.
- 07eec23: A Flask template link or form that names a route Cartograph did not recover is now a Gap naming the endpoint, the method and why, instead of being dropped. D1 §1.3 lists these after the capabilities under "User actions with no recovered target" (#2005).
- ca87c82: D1 §1.3 now lists, under each capability, the user actions that reach it: the forms, links and client requests on each page, with the words the page shows ("Log in", "Sign in with GitHub") and the operation or page they reach. Flask template links and forms record their visible text (#2005).
- 8757c4e: Vercel Go functions are now routes: a Go file under `api/` exporting an `http.HandlerFunc` is an endpoint at its served path, and `vercel.json` rewrites onto it are endpoints at their public paths (`/docs/{path...}` → `/api/docs`). Unreadable rewrites are Gaps. On true-line/agent-bot this adds the sign-in, webhook, docs and hooks routes and takes D1 from 17 to 31 capabilities (#2010).
- 1e67c45: Vercel JavaScript and TypeScript functions under `api/` are recovered as endpoints, with the `vercel.json` rewrites onto them.
- 70bfd27: Shows what made a section draft stale in the Spec Workbench. An accepted draft that is stale or unresolvable now lists the cited facts that changed since it was accepted, each of which opens as evidence, and the cited ids that are no longer in the graph. Each claim that cites a changed fact says so. "Re-draft this section" on such a draft opens the investigation composer for the same section and scope; the accepted draft stays in place while the new one is written and reviewed.

### Patch Changes

- 39c363a: An area briefing over its bound keeps the rows of a large subsection that fit instead of dropping it whole, and fits the business rules last so they take the remaining room (#2068).
- 592a02a: The architecture diagrams now show more of a Go + Cloudflare system (#2065): a Cloudflare Worker config (`wrangler.toml`/`.json`/`.jsonc`) becomes a container named for the worker; a Go client whose base URL sits in a struct field or package constant (`c.API + path`) names its outside host; and a store reached through a REST client by its conventional URL variable (`KV_REST_API_URL`, `UPSTASH_REDIS_REST_URL`, `REDIS_URL`, ...) is drawn as a data store.
  
  A worker is also drawn with who calls it and what it calls: a webhook signature header its main module reads (GitHub, Stripe, Slack, ...) adds the sender as an outside system, and a URL in its `vars` whose path matches another container's endpoint draws an inferred call to that container.
- 6797402: `product-specification.md` §1.1 now links the architecture diagrams (`architecture.md` and `architecture.dsl`), so a reader of the handoff bundle finds the system at a glance from the first section (#2049).
- b2c4af6: When two capabilities would get the same title, they are now told apart by the URL route they serve, such as "View orders (/admin/orders)", instead of by a source file path. The file path is still used when the routes don't tell them apart.
- 6b83182: D1 §1.3 states how many HTTP endpoints no Capability groups, and why: no resolved handler, or a handler no capability rule covers (by framework).
- 9ed6d81: Recovering a large TypeScript or JavaScript source now needs far less memory while it reads the code. The recovered facts are kept in their compact stored form file by file instead of as one in-memory tree for the whole repository. On the REA benchmark (5,360 files, 26,278 rules, 161,569 gaps) the peak of this step drops from 4.58 GB to 1.31 GB. The recovered graph and its citations are unchanged.
- 8abf7dc: When the capabilities section lists capability areas, the section composer now describes those areas instead of inventing its own grouping. It may still merge or rename an area whose name is weak.
- 9105679: The section composer now reads D1 1.3 as an overview: it gets the capability and area tables, and leaves out the per-capability operation tables. On a large system those tables pushed the request past its size cap.
- df8d4f8: Composed sections now read like documentation rather than inventories. The capabilities section describes each area by its purpose, its users and its outcome instead of listing routes. Business rules are translated into domain terms, with their exact user-facing messages and statuses. A composition can no longer stage an uncited or one-word placeholder. In the business rules section, a negated compound condition now reads correctly: `not (a or b)` instead of `not a or b`.
- b2ac92a: The knowledge overlay recipe is now a fixed-length digest of its producer rules, so new producers never overflow its 256-byte name bound.
- ecf198b: Prepares the curated projection for investigation findings (ADR-0074). The projection can now hold a finding accepted as curated, with a state of `eligible`, `stale` or `unresolvable` decided from the facts the finding cites. No screen accepts or shows one yet. The curated revision moves to `curated-revision-v3`, so every project reports a new curated revision once, with unchanged content.
- 5c3d932: Findings accepted as curated now appear in the curated reads (ADR-0074). A best-effort export prints each eligible finding in `curated.md` as a cited statement marked InferredWeak, with its role, limitations and source support, and an agent reads the same through `curated` and `curated-json`. A verified-only export prints none and says how many it left out. Findings whose cited facts changed or are gone are listed with the reason, without their text. The app does not yet offer the review or draw the list.
- 5c3d932: Adds the curation review for investigation findings behind the scenes (ADR-0074). A saved finding can now be accepted as curated or rejected through a new command, separate from the follow-up review, and the curated projection reads those reviews. An accepted finding is eligible only while the facts it cites are as the investigation read them. No screen offers the review yet, and no export or agent read lists a finding yet.
- 23a2406: You can now accept an investigation finding as curated in the app (ADR-0074). Each saved finding has **Accept as curated** and **Reject as curated** beside its follow-up review, with the saved decision, revision and time. The Spec Workbench lists the accepted findings in **Curated items**: the eligible ones with their statement and the facts they cite, and the others with the reason only. The Workbench reads the bundle again when a curation review is recorded.
- f88879e: D1 §1.6 names only a process start's named callables as its handlers, not the constants and anonymous functions its file defines, so no raw `decl:` ids or `anon@` offsets appear in the flow line.
- 5bb8493: Go: routes registered on a router passed in as a parameter are recovered, and handlers written as func literals, local closures, receiver method values or wrapper calls resolve to a callable, so Go services get capabilities (#2006, #2007). On agent-bot, endpoints go from 75 to 159 and capabilities from 0 to 16.
- 056868a: A Go `func main()` in `package main` is now an entry point (#2019): an HTTP server when it serves HTTP, a command under `cmd/` or an operator directory, a process otherwise. The architecture view names a `main.go` program for its directory.
- 056868a: Go repositories now have their SQL persistence recovered (#2019): `database/sql` and `sqlx` calls whose statement is a literal, a constant or a local the function builds (through `+`, `+=` and `fmt.Sprintf`) read and write the tables they name, each table a data entity with the columns its inserts and updates set. A statement that names no table and is computed is a gap. On miniflux this recovers 15 tables, 91 reads and 65 writes, with 3 gaps where a query arrives as a parameter.
- fb95c97: Go repositories now have their tests recovered (#2019): each `func TestXxx(t *testing.T)` is a test case linked to the code it calls and, through `httptest.NewRequest`/`http.NewRequest` with a literal method and path, to the endpoint it exercises with the statuses it checks. `test_arrangements.md`, D1 §1.4 and the flow dossiers list them. Calls between files of one Go package are now followed too, so capabilities reach the rules in those files.
- 13c02dd: Rules name a request header by its real name when a module constant holds it ("the “X-Sl-Allowcookies” header is set"), say a repeated condition once, and read membership in a literal list (#1998).
- c9515cb: Agent drafting no longer fails on large codebases. A whole-system query that matched more than 50,000 facts (SimpleLogin matches 84,625) was rejected by the investigation ledger, so every section-writer run stopped at its first query with "Investigation input preparation failed". Only the returned page and the cursor are bounded now.
- a424922: User stories are no longer empty after ingesting a NestJS GraphQL codebase such as Vendure. The Spec Workbench, the export bundle and single-artifact reads now include the Capabilities and caller sets the knowledge layer derived, as the read-only MCP already did, and each derived Capability lists one readable story per operation, for example "As an admin API caller holding CreateCatalog or CreateProduct, I can create product (MUTATION admin.createProduct)." These stories are InferredWeak, so a verified-only export still omits them and says how many it left out.
- 27ec8cf: Package macOS releases on the self-hosted Mac again; the hosted runner could not finish the universal compile in time.
- 8382336: Give the hosted macOS release packaging job 240 minutes so the universal compile can finish.
- 0ce2ce7: The product specification now states a mail handler's decisions: each guarded SMTP reply (accepted, deferred or refused, with its text) is a business rule under the capability whose listener, process start or scheduled job enters the handler's file (#2027).
- 68f61d9: `user_stories.md` now reads as a document. Each derived Capability lists its stories and the source files and handlers that implement them, and the caller permission sets are one table (permissions, operation, where the guard is) instead of one JSON block per operation. The exact story identities and cited facts are kept, as JSON, in a "Story identities and support" appendix at the end. On Vendure `packages/core` the readable part is now about 88 KB instead of being spread through 2.1 MB of JSON.
- 4e44bb5: Follow a check that several functions share a name with to the one on the caller's receiver class (#2029): `g.user.is_active()` now shows what `User.is_active` checks, instead of "5 repository functions have this name".
- aa399fa: Documentation references link to code again. After a recent change, a README or doc that named a file, a route or a resource was stored without its link to that fact, and documentation drift was not compared, because the reference index saw only repository nodes. The step that links found ADRs and reads documentation after a recovery also no longer loads the whole graph into memory: on the REA benchmark the peak memory of a recovery drops from 6.73 GB to 4.89 GB.
- ca8bb5c: Guarded effects no longer count as use-case guard candidates; builtins, constructor calls, directly imported logging verbs and module names shadowed by a local are not effects, and a module imported inside a function is. Rule statements keep nested `and`/`or` grouping and read only the request's own inputs as "given".
- 4f07aa4: D1 rule rows from Go code name the call whose error they test ("When the “store save” call fails, the request is refused …") instead of "When an error occurred".
- c4fafc6: A rule in an inline Go route handler names the route it handles (`handler of POST /sign-in`) instead of the literal's offset.
- 67301b3: Section drafting now sends the model the exact reply format. Before, a drafting run gave the provider no response schema, so models (Opus included) often wrapped their action in prose or left the finish unwrapped, and every section was refused with "The completed provider response was not an admissible action." A section-writer request now carries a schema covering only its own actions, which providers enforce with structured output.
- 8bd78ad: The product specification (`product-specification.md`) now fills 1.2 Actors and roles and 1.3 Capabilities from the knowledge layer instead of saying "No `Capability` fact is in this export". 1.3 lists each derived Capability with its operation count and examples; 1.2 groups the derived caller sets into roles by the permissions their guards list (on Vendure `packages/core`: 93 permission sets over 289 guarded operations). Both are marked Semantic / InferredWeak, and a `verified-only` export still withholds them.
- fdeb888: Subsystem areas stay inside one repository. When a system is ingested as several repositories (a server with a browser extension and a mobile app), each repository is now grouped on its own, so areas no longer mix server and client files and the server's areas are the ones it has when ingested alone. A subject two repositories both have is one area per repository, where the larger keeps the name and the other adds where its files are (for example "Alias" and "Alias (core/model)"), and shared code is listed per repository.
- 5f38f79: TypeScript and JavaScript test code no longer shows up as the system's own API. Controllers and routes in end-to-end suites, `__tests__`/`__mocks__`/`__fixtures__`/`test`/`tests` directories and `*.spec.ts`/`*.test.ts`/`*.e2e-spec.ts` files yield no product Endpoint, as JVM test source sets already don't. On Vendure `packages/core` this removes 63 of 374 endpoints (e2e fixture plugins such as `TestController` at `GET /run-job`) from the endpoint inventory, flows, capabilities and user stories.
- dbd1f8e: A Vercel function that only passes requests on to the mux routes under its paths is no longer a Capability of its own ("Manage webs"); D1 §1.3 states it as a front door with the routes behind it.
- 1509852: Update wasmtime to 50.0.3 for RUSTSEC-2026-0587.

## 0.71.0

### Minor Changes

- ea2bf86: Offers `domain-analyst@28` in the desktop specialist catalog next to `domain-analyst@27`, under the same expanded profile. The host now starts, corrects, withholds and prepares captured acceptance for @28 by the same rules as @27, through shared lineage predicates. @27's behaviour and bytes are unchanged.
- b9e3256: Atlas has a Find entity field. It searches every entity in the loaded graph by display name, kind or stable ID, across all layers, focus levels and collapsed groups, and says so. Choosing a result opens its evidence and brings it into view: the layer widens to All layers only if needed, focus backs out only as far as needed, and the groups and cluster that hide it open. Clearing the search restores the usual indexes and keeps the current selection. Before, only the first 24 visible entities were listed, so an entity in a collapsed group or another layer could not be found.
- 89e15df: Environment variables a system reads in source are now recovered as facts in every built-in language adapter: `process.env.X` (TypeScript and JavaScript), `os.environ` / `os.getenv` (Python), `os.Getenv` / `os.LookupEnv` (Go), `System.getenv` (Java and Kotlin) and `std::env::var` (Rust). Each fact records the key name and where it is read, never a value; a read whose key is computed at runtime is recorded without a name. Re-ingesting a repository adds these facts to its graph. The exported documents do not list them yet. Preflight and Settings show a `config_reads` coverage cell per language, stating what each adapter does not read.
- 89e15df: Every export now includes `configuration.md`. It lists each configuration key once for each repository, with the environment files that declare it and the source files and symbols that read it, each with tier, confidence and source span. A key that is read and never declared, and a key that is declared and never read, are marked. "Never declared" means that no environment file in the ingested sources declares the key, not that it is unset when the system runs. Files with a read whose key is not written as a literal are listed, so the document says that keys exist which it cannot name. Only key names are shown, never a value. The document states which languages are read (TypeScript, JavaScript, Python, Go, Rust, Java and Kotlin) and what is not covered: `import.meta.env`, destructured `process.env`, `"X" in os.environ`, Rust `env!`, and configuration frameworks such as Spring `@Value`, NestJS `ConfigService` and Viper. A `verified-only` export leaves out InferredWeak rows and states how many; a `best-effort` export marks them. In the Atlas, a configuration read now sits in the Server code band beside the file that reads it and shows its key instead of its id.
- 89e15df: Every export now includes `coverage.md` and `coverage.json`. They show, for each repository, which producers ran and how many files each read, which were excluded, which inputs were missing and which languages or constructs no producer covers, each with its recorded reason and the repository's inventory hash. Before, a reader of the exported documents could not tell an absent finding from a file that was never read. The two files are the same in `verified-only` and `best-effort` exports.
- 89e15df: Every export now includes `curated.md`, and the bundle records a `curated_basis` (the curated revision and counts) beside `graph_basis`. The document lists every proposal you accepted. An accepted proposal whose evidence is all captured source text that is still readable, and whose graph and source associations are unchanged, is a curated item: a `best-effort` export shows it marked InferredWeak and cites its Gap, source and target; a `verified-only` export shows no item and states how many it left out. Every other accepted proposal is listed with one state (unverified, already-covered, unresolvable, stale or conflicted) and the reason in a sentence. Most accepted proposals are listed as unverified today, because their evidence was read from the working tree and nothing verified it. Accepting a proposal still writes nothing to the graph, resolves no Gap and leaves the graph hash and every other document unchanged. The document also summarises Workbench decisions, including those that no longer match any statement. An export reads at most 4,096 proposals and 256 accepted ones; beyond that, or when the proposal history cannot be read, the export still succeeds, `curated.md` says that the curated projection was not computed and why, and no accepted proposal is shown. The Workbench does not show curated items yet, and MCP readers and work packets cannot select the document.
- 89e15df: A read-only MCP client in a complete session can now read the curated projection. `curated` is readable with the artifact tool and is byte for byte the `curated.md` an export holds for the same reviews, graph and export mode. `curated-json` gives the same projection as data: the view, the curated revision, the graph basis, each accepted proposal with its record id, proposed relation, Gap, tier and confidence as produced, review status, source support, state and reason, the Workbench decision summary and the counts. A `verified-only` service shows no curated item, only how many it left out. The session basis now carries the curated revision, so a session over other reviews has another basis id, and `h3_curated_available` is true only where the session computed the projection. When it could not be computed, the basis and both documents give the reason and every other read still works. A Workbench decision that matches no recovered fact is one such reason: checking it needs every document compiled, which a client session does not do, and the export still shows the curated items. A review made while a session is being acquired makes that read fail as stale. Scoped sessions offer no curated read and say why, and work packets still cannot select the document. Review notes are never shown to the client.
- 8062c00: The product specification header now shows the graph content hash whenever the export has one, including when the curated items could not be computed; it then gives the reason in place of the curated revision. The curated revision now also covers reviewed section drafts, so it changes once for every project after this update, with no change to the curated items themselves.
- 89e15df: The Spec Workbench now shows the curated items. A **Curated items** section above the document list gives the curated revision (the one the bundle on screen records as `curated_basis`), the graph basis, the counts of accepted proposals by state, each curated item with its record id, proposed relation, Gap, tier and confidence as produced, review, source support and state, the accepted proposals that are not items grouped by state with the reason in a sentence, and the Workbench decisions that no longer match any statement. Every curated item is marked InferredWeak; in `verified-only` mode no item is shown, only how many were left out. When no proposal is accepted the section says so, and when the projection could not be computed it gives the reason and shows no proposal. The section is read again when you record a review, change the export mode, record a Workbench decision or choose **Refresh curated items**; recording a review now also recompiles the bundle, so `curated.md` in the Workbench no longer lags behind a review. Proposal history and the review dialog no longer say that an accepted proposal is "awaiting context reconciliation": they show the proposal's state and its reason when the read returned it, and otherwise point to the section.
- 9e075c8: `data_model.md` now prints a fact-index citation id beside each statement it makes. A new table lists each entity and each node related to one, with its tier, confidence and citation id. The access and mapping relations table gains Confidence and Citation columns, and each relation field shows the confidence and citation id of the relation it belongs to. Every id printed resolves to a row of `fact-index.md` and `fact-index.json`. A `verified-only` export still prints no InferredWeak fact and so no id for one; a `best-effort` export prints them with their confidence. The diagram is unchanged and carries no ids. The document's bytes and content identity change; no other artifact changes.
- 3c67a59: Every export now includes `endpoint-inventory.md`. It lists every recovered endpoint once, including endpoints no test references, with its method, path, framework and language as the adapter recorded them, shown as `unknown` where nothing was recorded, and with the endpoint's tier, confidence and source span. It states no parameters, response schemas or authentication. An endpoint recorded as a Gap is counted and not listed. A `verified-only` export leaves out InferredWeak endpoints and states how many; a `best-effort` export marks them.
- 89e15df: Every export now includes `endpoint-permissions.md`. It lists each recovered endpoint with the permission sites recovered for it, the endpoints with none recovered, and the endpoints whose handler path could not be read to its end, each with tier, confidence and source span. "None recovered" means only that: a check made by middleware, a gateway or code inside a handler is not recovered, and the document never says an endpoint has no check. Permission sites are recovered for Flask, Flask-Admin and NestJS GraphQL today; an endpoint in any other framework is listed under a coverage note and in neither table. A `verified-only` export leaves out a row whose permission site is InferredWeak and states how many; a `best-effort` export marks it. The document can be read over MCP and selected in a work packet like the other graph documents.
- 89e15df: Every export now includes `environments.md`. Each Compose file is listed as one environment, shown by the path of its file and named only when the file states a `name`: a file or folder name such as `prod` is never turned into an environment name. Under each environment are the workloads it declares, one for each service, with its image, build context, ports, the names of its environment keys, its environment files, its dependencies and its profiles, each with tier, confidence and source span. A service name that two or more Compose files of one repository declare is listed once for each declaration, with the properties that differ; the document does not say which one a deployment uses. A Compose file that cannot be read, `extends`, `include`, a YAML anchor, alias or tag, a dependency on a service the file does not declare, and an environment file such as `staging.env` whose name is the only thing that names an environment are listed as not established. Only the names of environment keys are stored, never a value. Kubernetes manifests, kustomize files, Helm charts and Helm values files are not read yet; Preflight and the coverage document now name each kind they find as present and not read. Re-ingesting a repository that has a Compose file adds these facts to its graph.
- 89e15df: `environments.md` now covers kustomize and Kubernetes. Every kustomization file is listed as one environment, shown by the path of its file and named only when the file states a `namespace`; a base that other kustomizations include is one too, and its row says which include it. Each `Deployment`, `StatefulSet`, `DaemonSet` and `Job` manifest is a workload, with one row for each container: its image, ports, the names of its environment keys and the names of the ConfigMaps and Secrets it references, never a value. A kustomization lists the workloads its `resources` reach inside the repository. A workload that several kustomizations reach is listed once for each of them, with what that kustomization's patch files and `images` entries state for it, or "no patch stated"; nothing is applied, and the document does not say which one a deployment uses. Kubernetes workloads that no kustomization reaches have their own section. Remote or missing resources, patches that match nothing, inline and JSON patches, `target` selectors, cycles and transformers or generators such as `namePrefix` and `secretGenerator` are listed as not established. Helm charts and values files are still not read; Preflight and the coverage document now name unread Kubernetes manifests by kind (`Service`, `ConfigMap` and so on) instead of by file, and no longer list kustomization files or files that hold only the kinds that are read. Re-ingesting a repository that has kustomization files or these manifests adds the facts to its graph.
- 5d58df2: In a system with declared bindings, `external.md` now lists a call to a host no binding names as outside, with its call sites, spans and flows, where it was listed as unclassified before. Adding a system records, on each outbound call it could not join to an endpoint, whether a declared service or gateway route names the call's host, and records the bindings on each observed HTTP client of a declared host. A call that a binding places in an ingested repository and that matched no endpoint is still counted and not listed, including a call whose method is computed, and an observed HTTP client of a declared host is no longer listed as outside. A system added before this release keeps the earlier result until it is added again. The recorded outcome changes those facts, so the graph hash of a system with declared bindings changes on its next ingest.
- 89e15df: Every export now includes `external.md`. It lists what the ingested sources call or publish to outside themselves. Each web address whose scheme and host are written in the source, and that no declared binding covers, is listed once as outside, with every call site, its method and path where known, its source span, the flows that reach it and the data entities those flows read or write. The entities column does not say that an entity is sent. A target that is not proven inside or outside is listed as unclassified with its reason: a host computed at run time or read from configuration, an SDK or identity-provider client, and a published channel with no recovered subscriber or backing resource. A call that a declared binding places in another ingested repository is counted and not listed. The outside list is a lower bound. Without a declared binding, a call to another ingested repository by its full address is listed as outside; when bindings exist, a call to a host they do not cover is listed as unclassified with the sentence recorded for it. Every row carries the tier and confidence of its fact and never a higher one. A `verified-only` export leaves out InferredWeak rows and states how many; a `best-effort` export marks them. The document cannot be selected in a work packet.
- 89e15df: Every export now includes `fact-index.md` and `fact-index.json`: one row for each fact the exported documents state, with its tier, confidence, producing method, evidence basis, extractor, every evidence span and the documents that state it. Each fact has a stable citation id (`F-` and 16 hex characters) derived from the fact alone, so it is the same in every document and every export of the same graph. A `verified-only` export has no InferredWeak row, a `best-effort` export marks them, and gaps are marked as gaps. An export is refused if a cited id has no row or two facts share an id. The existing documents do not print the id beside each statement yet.
- 84d2ede: A Flask-Admin view that no `is_accessible` guards is now recovered as requiring login when its own body turns anonymous users away before doing anything else: an `if not current_user.is_authenticated …` that returns a redirect or aborts with 401 or 403, written in the view or in a helper the view returns first. Where the body checks login in a shape that is not proven, the endpoint stays `unknown` and names the function holding the check, so a reviewer knows where to look.
- 89e15df: Show each source's recorded graph content hash in Workspace system contents, and record in the exported spec bundle the content hash of the graph it was built from together with its source revisions.
- 266872b: Capabilities are now also recovered from NestJS REST controllers: each controller class becomes one capability candidate that cites its HTTP endpoints and their handlers. Until now only NestJS GraphQL resolvers produced capabilities. Re-ingest a repository to get them. Other frameworks' controllers are not covered yet.
- d6aa777: Settings → Provider keys now lists OpenAI beside Anthropic. An OpenAI API key is saved in the system credential store (Keychain on macOS, Credential Manager on Windows) as its own entry, and `OPENAI_API_KEY` overrides it. **Check connection** on the OpenAI row sends one bodiless request for OpenAI's model list with the OpenAI key only, and adds its size to the egress record under `openai:connection-check`. Saving a key sends nothing and opens no cloud step without consent.
- 89e15df: Every export now includes `pipelines.md`. For each GitHub Actions workflow and Dockerfile it lists the triggers, the jobs in dependency order and each job's steps in declared order, each with tier, confidence and source span. A job whose needs could not be resolved or form a cycle is listed with the reason and is never given a place in the order. Files that are present and were not read (GitLab CI, Jenkins, CircleCI, Azure Pipelines, and workflows or Dockerfiles that could not be read exactly) are listed with the recorded reason, and a closing note says what is not covered. A `verified-only` export leaves out InferredWeak rows and states how many; a `best-effort` export marks them. The pipeline reader now also reads `Containerfile` and `*.dockerfile` names and treats a `RUN --mount=…,from=<stage>` as a dependency on that stage, so re-ingesting a repository with such files adds facts. Dockerfiles with heredocs and workflows with YAML anchors are still listed as not read.
- 89e15df: Recover the order of build and deployment steps as cited facts: GitHub Actions workflows yield their triggers, jobs, `needs` relations and steps; Dockerfiles yield their stages, base images, instructions and final stage. Secret references are stored by name only and command text is redacted. GitLab CI, Jenkins, CircleCI and Azure Pipelines files are listed in the coverage inventory as present and not read.
- 4ea5a90: `pipelines.md` now prints a fact-index citation id beside each statement it makes. Every pipeline, trigger, job and step row gains a Citation column with the id of its fact; a trigger, job or step also cites the relation that places it, and a job cites each of its needs in the order its Needs cell lists them, each relation with its confidence. Every id printed resolves to a row of `fact-index.md` and `fact-index.json`. A `verified-only` export still prints no InferredWeak fact and so no id for one; a `best-effort` export prints them with their confidence. The document's bytes and content identity change; no other artifact changes.
- 89e15df: A gated plugin that fails on one file (a malformed fact, a guest error or trap, or its fuel, memory or table bound) no longer fails the whole ingest: that file is skipped for that plugin and no fact from it by that plugin joins the graph. The coverage inventory, `coverage.md`, `coverage.json` and the Workspace name the plugin, the refused files and the reason class, and do not count those files as covered; a plugin that runs past its time limit still fails the ingest.
- f59d0c5: `product-specification.md` now prints an accepted section draft in its section. Each claim is shown with its citation ids, marked T3 / InferredWeak, with the role that wrote it and "human-reviewed: accepted". A draft is printed only while every fact it cites is unchanged; one that is stale, replaced, in conflict or citing a missing fact is named with its reason instead. A `verified-only` export prints no drafted claim and says how many it left out of each section. The document is now readable over MCP with `cartograph_read_artifact`, as the same bytes the export writes. No agent drafts a section yet, so existing exports show "Not drafted" as before.
- 33be974: Every export now includes `product-specification.md`, the first document arranged as the document catalogue describes it. It holds every section of the product specification in order, each marked as compiled from facts, to be drafted, or left for your team. Actors and capabilities are listed with their tier, confidence and a citation id that resolves in `fact-index.md`; flows are summarized and Gap findings counted; business rules and acceptance criteria point to `rule-evidence.md` and `test_arrangements.md`. Sections an agent will draft say "Not drafted", since no agent writes a section yet. The header carries the catalogue version, export mode, graph content hash and curated revision. No model is involved. Every other exported file is unchanged, except that `fact-index.md` and `fact-index.json` now list this document beside each fact it cites.
- c02ac30: Adds the document view to the Spec Workbench. Under "Sections and drafts", each section of the product specification is listed with its kind and every draft stored for it, with its claims, their T3 / InferredWeak annotation and their citation ids. "Draft this section" opens the investigation composer with the Section Writer role and the section's question filled in. Accept, Reject and Return record a review with an optional note at the revision on screen; a draft that changed since is refused. An accepted draft shows its state and reason from the curated read, and an accepted, current draft is printed in the next export.
- abb3cbe: Adds the Section Writer role (`section-writer@1`). It drafts one section of the product specification in an ordinary investigation run, under the same limits, consent and cancellation as the other roles. A run is started with a question that names the section and its scope (the whole system, or one actor, capability or rule); any other question is refused. When the run finishes with findings, each becomes one claim citing its facts by citation id, and the draft is stored for review. A run that finishes with insufficient evidence, proposes a design, or cites a fact outside the section's scope stores no draft. Review is not in the app yet, so exports still show "Not drafted".

### Patch Changes

- 0550d70: Adds the internal analyst identity `domain-analyst@28`, which shows the model the exact response schema chosen for each call, as prompt text, alongside the provider's structured-output field. It is not offered yet. Earlier analyst versions keep their prompts and request bytes unchanged.
- e7b30bd: The selected compiler's logical workspace bound is lower for the same limits, so the agent read service can admit official artifacts for larger graphs. Evidence and attempted-tier slots copied from flow output are charged by the smallest serialized form of one element rather than one slot per output byte, and omission classification builds its plain words in one buffer. At 4 MiB / 5,000 rows / 100k values the logical peak falls from about 2.36 GB to about 1.44 GB, and at 1 MiB / 2,000 rows / 25k values from about 1.31 GB to about 0.62 GB. Artifacts, classification results and the configured limits are unchanged.
- 4095239: Workspace keeps its coverage inventory after you relaunch the app. The inventories earlier ingests recorded are read back from the store without re-running an ingest: a current one is shown as recorded, a stale one (its registration or plugins changed since) is named as stale, and a failed read says so instead of showing old cards. Clearing the graph removes them for good.
- 89e15df: Exporting no longer freezes the window while it checks accepted proposals and compiles the documents: the export runs in the background, checks all accepted proposals against one copy of the graph instead of one copy each, and no longer compiles the documents a second time on every export when a Workbench decision matches no current statement.
- 10d641f: Saving a follow-up decision while another window or the MCP server is writing to the same store no longer reports a storage failure when the wait for that writer runs out. The decision is reported as not saved, and you are asked to refresh the reviews and choose again. Nothing is retried automatically.
- d27509f: Found ADR and RFC files are now redacted before their text is stored. A secret pasted into a decision record no longer reaches the graph or the exported `adrs.md`; the fact records how many values were replaced. An ADR file over 1 MiB or not valid UTF-8 is skipped instead of failing the whole ADR read, and the coverage inventory names each skipped file by reason. The ADR reader and the API contract reader now have their own coverage rows. The ADR row counts a decision's links into other repositories of the same system.
- 22f70cd: A failed investigation now keeps a machine-readable cause in its saved detail when the provider's response failed action decoding (the decode stage or rule and the error class) or when input preparation was refused at a named step (the phase). The record holds no response text, query or error string, and the failure message is unchanged.
- 6711c34: An investigation's wall-clock cap and consent-wait budget now come from the limits saved on the task, as the active-time limit already did, so one stored record decides when a run ends. The defaults are unchanged (10 minutes active, 1 hour wall clock, 15 minutes per consent wait).
- 138c363: A journey candidate no longer shows a result screen as following its goal call when the call is made only on some paths, such as inside an `if`, a loop, a `try` or a callback, and the navigation after it is not. Such a journey is now listed with an unordered result. The check uses a new record on each TypeScript or JavaScript client call, so a system ingested before this release lists every result as unordered until it is ingested again.
- 89e15df: An MCP read of `coverage` or `coverage-index` now returns the stored coverage inventories, the same content as the exported `coverage.md` and `coverage.json`, instead of saying that no inventory was supplied; a repository whose inventory was recorded under a registration or plugin basis that is no longer current is listed as stale with its inventory hash, and nothing is read from that inventory.
- 2fb46ee: The MCP task cap for an external agent now counts only its live tasks. Before, every task the agent had ever started counted, including finished and cancelled ones, so an agent with a cap of one could start one task and then never another. Finished tasks stay stored and readable, and retrying a request with the same nonce still returns its original task, finished or not. The cap's default and upper bound are unchanged.
- 8f98f22: Preflight now previews a system manifest instead of skipping it. Each local repository the manifest declares is scanned relative to the manifest's folder and shows its detected languages, frameworks and adapter coverage. Remote and managed entries are listed as deferred and are not fetched. The preview registers no source and records no finding; recovery still records the findings. A preview that fails or is cancelled shows no partial results.
- 8499c51: Raises the receipt ceilings so that a large TypeScript repository can cite every eligible fact. One extraction and the receipt store now keep up to 200,000 receipts and 128 MiB, up from 100,000 and 64 MiB, and a source can bind up to 200,000 facts. On the REA benchmark this keeps 155,264 receipts with no omitted declarations or rules, where it kept 54,725 before.
- 6265931: When the retained receipt store is full, the coverage reason for the receipts it could not keep now says how to free space: forget a retained source, or recover a source again so its superseded receipts are reclaimed. It used to say the store could not reclaim space, which stopped being true once forget and supersession reclaimed receipts nothing pins.
- 3502dab: Reclaiming superseded receipts after a recovery now reads only that source's receipts instead of rebuilding and decoding every receipt of every source in the store, so recovering a small repository no longer pays for the others. A damaged receipt of another source no longer blocks this cleanup. It is still refused when it is read or when its own source is cleaned up. Stored receipts and their ids are unchanged.
- 7ca096b: Semantic retrieval now takes its vector candidates from a usearch index that is rebuilt from the cached unit vectors. The index only proposes candidates: scores and order still come from the stored vectors, so results on small graphs are unchanged.
- 89e15df: `security.md` no longer calls an endpoint unauthenticated when the tool only failed to recover a check. The finding is now titled **No authentication check recovered: {endpoint}**, the wording `endpoint-permissions.md` uses for the same fact, and the document states once that a check outside the source that was read, such as a gateway or middleware the adapters do not read, is not visible to the finding. The finding's type, identity, tier, confidence and evidence are unchanged.
- dc9fd6f: Export bundle now fetches every artifact whose preview was capped in one request, so the graph snapshot, flow trace and curation are prepared once for all of them instead of once per artifact. The exported file is unchanged, and an incomplete or changed result still refuses the whole export.
- e98ce08: The source-capture store and the primary-source receipt store now wait up to 5 seconds for another connection's write lock, like every other local store, instead of 1 second. On a loaded machine a recovery or evidence read is less likely to fail with "database is locked" while another window or the MCP server is writing.
- 2a77737: Export bundle in the Spec Workbench now writes the file from the desktop host, streamed straight to disk, instead of building the whole bundle as one string in the app window and sending it back. On a large repository that string was close to a gigabyte, longer than the window's maximum string, so the export could fail or hold about a gigabyte of memory. The saved file is unchanged, byte for byte. If the spec changed since the Workbench showed it, the export is refused with the same "Refresh the Spec Workbench" message as before.
- 02801be: When two Terraform root modules in one repository (for example `envs/prod` and `envs/staging`) declare the same resource address, the graph now records a Gap that names the address and every declaring file and span. Before, one of the declarations was dropped without any notice because both share one resource id. Resource ids are unchanged, and repositories without such a collision get the same graph as before.
- 1d7fb9c: The traceability gate now checks that mapped tests actually run in CI. A test id whose every bound Rust test is `#[ignore]`d fails unless its row names a manual procedure or a CI lane that runs ignored tests, and test ids with some ignored tests are listed. Story statuses are checked against the Status enum; US-0043 moves from the undefined `Planned` to `In-Progress`. T-0329, whose opt-in relay tests no lane runs yet, is recorded in a baseline that can only shrink.

## 0.70.0

### Minor Changes

- 05ffd91: Add evidence-auditor@14 with tighter proven correction sizing and authenticated capacity exclusions, preserving existing limits and historical auditor definitions.

### Patch Changes

- b8eec06: Make language-plugin provenance host-authoritative. A plugin fact may only be T0 Deterministic and Confirmed, its evidence must cite the file the host handed the plugin with byte ranges inside it, and the host computes the content hash. A fact that breaks a rule fails that file's extraction with a named error instead of being stored.

## 0.69.0

### Minor Changes

- 6f6a6bb: Add evidence-auditor@13 with one bounded replay/citation correction turn and parent-finding reference guidance. Preserve text-free finish rejection reasons and final-response usage instead of reporting content rejections as generic store failures.
- 9515b35: Settings can now check a provider key and the local lane. **Check connection** asks the provider whether it accepts your key with one request that carries no source text, prompt or graph content; it needs no consent and its size is added to the egress record. A new **Local lane** section shows whether the Ollama server is running, which models are missing, and the exact `ollama pull` command for each.

### Patch Changes

- 73d1d45: Help has a new **First run** topic: installing and opening the build (including an `unsigned-dev` build), what works with no provider, the first ingest and where to look afterwards, setting up the local lane, and adding a cloud provider key with what consent means.
- 0d240dd: When the knowledge phase that runs after an ingest skips or leaves out work, the app now shows a notice naming each reason; before, a skipped phase looked the same as one that ran. Capabilities, domain entities, actors and business flows are now derived when the Semantic tier (T2) is off; only embedding is skipped. A desktop context query now answers on a graph over 50,000 rows instead of returning an error.

## 0.68.1

### Patch Changes

- e6fee0d: evidence-auditor@12 names a tool, model-call or active-time cap in its terminal diagnostic only when that cap actually stopped the run. An unrelated host or store failure that happens while a counter is exhausted is no longer labelled as a cap.
- 4b92c25: The topology map in `topology.md` now prints the tier and confidence of every node and edge, so a static, an observed and an inferred connection no longer look the same. The standalone topology and flow exports take the export mode and withhold weak inferences in verified-only.

## 0.68.0

### Minor Changes

- 69424a7: Add an immutable evidence-auditor@12 with obligation-bound acquisition and bounded, text-free query-budget feedback while preserving auditor@11 replay and existing limits.
- eefcc3e: Settings has a new Provider keys section. Enter, replace or remove a provider API key in the app; it is stored in the system credential store (Keychain on macOS, Credential Manager on Windows) and never shown again. The `ANTHROPIC_API_KEY` environment variable still works and takes precedence. A release build opened from Finder can now use the cloud lane. Consent is unchanged: each cloud step still asks.

### Patch Changes

- eb2e7dc: The knowledge phase no longer skips repositories whose graph is larger than 50,000 rows. It now reads graphs up to 10,000,000 rows, so capabilities, domain entities, actors and business flows are produced for systems of real size.
- bd01a90: Help → Guide, Help → Report an issue, the Help links and the adapter-request links now open the repository at its current location, `true-line/cartograph`.
- 4607683: The investigation composer's limit line now shows the saved run limit for every specialist that uses it, including domain-analyst@21 to @27. The app decides this in one place instead of keeping a second list in the UI.

## 0.67.4

### Patch Changes

- edb064b: Preserve versioned parent-history policy through source reservation, response schemas and acquisition validation, and retain optional closed, text-free host/store failure diagnostics. New source reservation records require exact effective-policy bindings while historical hosted records remain readable.
- b514d82: The egress total now counts every cloud payload that was sent, including calls that then failed or timed out. Previously only calls that returned a usable answer were counted. A cloud call is not made if its egress cannot be recorded.
- 86a7032: Source text returned to an MCP client is now redacted before it leaves the app, the same way model prompts are. The evidence response states the redaction policy and how many values were replaced.
- 3340661: Redact credentials assigned to compound names (`DB_PASSWORD`, `clientSecret`, `AWS_SECRET_ACCESS_KEY`, `SECRET_KEY`) and to quoted keys (`"password": "…"`) before text is sent to a model, embedded or recorded. Previously only a bare `password`, `secret`, `api_key`, `access_token` or `auth_token` name was recognised. The redaction policy is now `redact-text@2`; cached embeddings and stored source-size measurements made under the previous policy are recomputed.

## 0.67.3

### Patch Changes

- f562ec2: Allow newly created desktop investigation-v2 child tasks to admit up to 32,768 bytes of authenticated saved-parent history, preserving historical bounds and all generation limits.

## 0.67.2

### Patch Changes

- d36e7e0: Allow the captured auditor harness to review bound withheld-parent results through the existing owner-authorized hosted consent gate, preserving the parent disposition and unchanged limits.

## 0.67.1

### Patch Changes

- cecaf40: Fix hosted OpenAI investigations, which failed on their first call with an HTTP error. The strict response schema now gives every literal an explicit type and spells out object and array constants, as the OpenAI API requires.

## 0.67.0

### Minor Changes

- 900d539: Give the pinned hosted GPT model (gpt-6.1-sol) the same 16,384-token per-call output allowance as the Opus lane, because its reasoning tokens count toward the output cap. Every other provider and limit is unchanged.

## 0.66.0

### Minor Changes

- 47406f4: Make Domain analyst @27 the desktop default, with the versioned local finish-only allowance and complete correction-result display. Keep @24–@26 out of new-task choices while existing @26 tasks reopen with their saved identity and policy.

## 0.65.0

### Minor Changes

- 66e41dd: Add an opt-in analyst @26 role that requests correction of bare finish field names in authored prose while preserving historical analyst behavior.
- 7552e1a: Add an opt-in analyst @27 with bounded terminal-empty and exclusive-subject transition predicates. Add one-turn memory-only finding patch corrections with full merged-answer admission and explicit local finish allowance support. Historical analyst grammar, diagnostics and replay remain unchanged.
- e1543ac: Add opt-in analyst24 support for measured captured-source reservations and combined correction feedback, preserving historical analyst behavior and the existing capacity limits.
- 59d0fd8: Filter loaded flow triggers by name, kind or ID while retaining the current selection.
- 0539506: Create a new system manifest from Connect with explicit repository, configuration, service and gateway declarations, a validated TOML preview, and a save action that preserves existing files.
- f454d7d: Hosted Opus investigations reserve 16,384 output tokens per call, because Opus 5.5's always-on thinking counts toward the limit. A provider stop at the output limit now reports `output_limit_reached` instead of `incomplete_response`.
- 9858136: Add an explicit versioned local finish-only allowance while preserving historical investigation budgets.
- e5d79dd: Retain bounded, text-free diagnostics for newly refused or withheld findings from analyst roles 22–25. Development receipts expose authenticated reason codes and positions, with explicit omission markers when details do not fit. Diagnostics preserve model-facing feedback and existing workspace capacity accounting.
- 68f6a74: Add opt-in analyst25 support for withholding unverified findings after the shared correction, with an explicit partial result preserved through audit and saved history. Keep historical analyst behavior and existing capacity limits unchanged.

### Patch Changes

- cc863cb: The response schema sent to Anthropic for hosted Claude investigation calls is now in a form the Messages API accepts. References are inlined instead of carried as `$defs`, and exact structured choices are sent as closed object schemas instead of structured `const`.
- 495b871: Captured analyst validation can freeze an explicit desktop run-limit pair and verify it on reopen, with the saved settings included in new configured receipts. Product defaults are unchanged.
- 5b143e0: Keep rendered artifact table labels readable and long provenance accessible through horizontal scrolling.
- 407b70b: Keep the system breadcrumb correct after relaunch by reading current graph state, and show No system after clearing it.
- c31c833: Show recorded screen routes in Atlas and evidence titles while preserving stable identities and provenance.
- 17bc07a: Honor the saved hosted output allowance when reserving and settling source reads, so valid investigations can continue beyond eight calls without a spurious storage-capacity failure.

## 0.64.0

### Minor Changes

- 331eaa6: Add analyst @22 finish feedback with text-free replay locations and a distinct status for obligations excluded by prompt capacity. Saved diagnostics are bound to independent response metadata and fail closed when that witness is missing or altered. Existing analyst identities and correction limits are preserved.
- 47fb6a5: Add analyst @23 checks for unsupported claims about cited literal enumerations and configuration defaults. Typed feedback identifies the finding and sentence without retaining refused prose, using the existing single correction turn. Historical analyst definitions and limits remain unchanged.

### Patch Changes

- ae54a96: Prevent analyst investigations from repeatedly offering acquisitions already refused for prompt capacity. Analyst @21 excludes refused actions until acquisition progress advances the input, and offers finish-only when no acquisition remains. Historical analyst tasks retain their saved behavior.
- 7d12b6d: Render selected flow diagrams beyond the overview cap while preserving artifact identity and existing limits.
- 79b263f: Distinguish unresolved findings from supporting assertion rows in the Spec Workbench.

## 0.63.0

### Minor Changes

- dbcd52e: Add analyst@20 with authenticated target-to-action correction guidance after an eligible premature finish. Preserve historical analyst versions, source citation checks, and the existing correction allowance. Use analyst@20 for new desktop investigations when the host advertises it, while retaining saved specialist choices.
  
  Guide analyst@20 to paraphrase cited source and retain a content-free source-replay refusal with measured model usage when its finish copies a complete source excerpt.
  
  Require analyst@20 limitations to state evidence boundaries and workflow answers to explain supported steps without inferring absent behavior from missing evidence.
  
  Make the in-run model semantic audit advisory (candidate-evidence-auditor@3, ADR-0054): its verdicts are saved and shown as advisory feedback but no longer withhold a result, and an audit that cannot complete is recorded as not completed. Deterministic checks stay blocking, including a new host-side check that refuses limitations that deny evidence already present in the acquisition ledger. The separately configured auditor model may reason before answering, has its own 16,384-token output allowance reserved from the task budget, and OpenAI-compatible servers receive a bounded strict JSON schema.

## 0.62.0

### Minor Changes

- 04f451f: Choose which installed Ollama model local investigations use in Configure → Run Limits… (default qwen3:8b). The window lists installed models with their digests, refuses a model that is not installed, and shows a saved model that has since been removed. The Investigations footer now shows the saved task time limit (#1643).

### Patch Changes

- 601f142: Correct premature investigation failure when an analyst cites a known source passage before reading it. The new analyst version offers one bounded correction through the existing refusal history, requires explicit source acquisition, and preserves historical analyst receipts and resource limits.
- 6aefdf2: Explain when an investigation answer cites evidence it did not acquire, preserving a durable refusal and measured model usage without saving rejected findings.

## 0.61.0

### Minor Changes

- cdf2caa: Add Configure → Run Limits… to the menubar: a separate window where the investigation time limit (up to 60 minutes) and the per-model-call timeout (up to 10 minutes) can be raised without a rebuild (#1633).
- 412523a: Investigations show live progress after you ask a question: the current step in plain words, how long it has taken (ticking), model calls and actions so far, and a pointer to Help → Debug Viewer. Each activity row now says whether the model's action was proposed, running, executed or not executed (#1631).

### Patch Changes

- ab10b6f: Help analyst investigations recover from invalid query identifiers and finish after repeated metadata reads without repeating the acquisition or inventing evidence.
- 223539d: Opening a gap's Resolution Strategy no longer fails with "Prepared task exceeds its bounded evidence or metadata contract". Oversized working-tree spans are omitted like unreadable ones, and a gap whose relation the agents cannot propose shows its evidence with escalation marked unavailable (#1635).
- c78b382: The Investigations composer now remembers your last explicit budget and specialist across app restarts (stored in the app profile); a fresh profile still starts with domain-analyst@18 under Expanded (#1636).

## 0.60.0

### Minor Changes

- ed4627a: Help → Debug Viewer opens a local, always-on log of every model request and response in the plain-text editor. Entries stream as each call is sent and received, and the log rotates at 25 MB (#1624).

### Patch Changes

- 1907558: Offer the analyst only exact recorded query replay references, and omit replay when no queries have been recorded. Coverage inventory reads no longer expose an impossible replay action after initial acquisition.

## 0.59.1

### Patch Changes

- 052ae7f: The desktop Investigations composer now defaults to the schema-backed domain-analyst@18 under the Expanded budget when the app offers it, so local models get a structured action schema. Older analysts stay selectable (#1618).
- 046286b: Recover from rejected investigation queries with bounded feedback while preserving charged attempts and durable history. Invalid host state remains terminal.

## 0.59.0

### Minor Changes

- 27fb17e: Desktop investigations read the frozen graph under a new investigation ceiling (4 GiB / 10M rows) so real estates no longer stop at "Investigation limit reached"; MCP tasks keep their default limits and reservations (ADR-0052).

### Patch Changes

- 391a6e0: Evidence drawer: Inferred facts from deterministic analysis no longer claim they were proposed by a higher tier, the cited span scrolls into view, and a missing producing method reads "producer not recorded" instead of looking like a missing HTTP method.
- 922e859: The evidence drawer treats an unknown producing tier named like a built-in object property (for example `constructor`) as not recorded.
- 251e2b8: Avoid decoding the same saved invocation twice during transactional source and inventory history validation while retaining all ledger and binding checks.
- c5cdc98: Kotlin Spring controllers: function-level `@RequestMapping` handlers become endpoints, with verbs that union the class-level and function-level `method =` restrictions, and literal path arrays become routes. Unprovable verbs become route Gaps instead of being dropped.
- cde1594: Avoid repeating full immutable-ledger validation for every query when constructing disclosed investigation history. Preserve existing reference identities, registry validation, and empty-history behavior (#1596).
- bd226a1: Java Spring controllers that declare handlers with method-level `@RequestMapping` (classic Spring MVC) now produce endpoints, one per proven verb, or an honest route gap. Before, they were silently dropped: a 61-repo OpenMRS estate recovered 2 endpoints from 592 such handlers.

## 0.58.0

### Minor Changes

- 6630f29: Enable explicitly selected domain-analyst@18 investigations through the existing App and owner-granted MCP coordinator, with finite acquisition choices, durable continuation and existing per-step consent. Preserve historical versions, default selection and resource limits (#1385).

### Patch Changes

- 87a8d6c: Prepare exact @18 UI family recognition and conditional version selection for a future compatible catalog. Runtime dispatch and current default/catalog exposure remain unchanged (#1586).
- 2f34767: Job-store connections now wait up to 5 seconds (was 1 second) for a competing writer before reporting a storage error. This reduces spurious storage failures when app and MCP requests contend on a busy machine, for example concurrent identical MCP retries. Contention beyond 5 seconds can still fail.

## 0.57.1

### Patch Changes

- b018b89: Repository ingest keeps recovered facts packed after the TypeScript walk: later passes decode only the labels they read, so recovery holds far less memory on large repositories (#1574).
- 07b676e: Declared API doc contracts decode only recovered Endpoint nodes, matching every other fact by id (#1574).
- 0234e23: Repository ingest publishes recovered facts packed, so the app no longer holds a decoded copy of every fact between recovery and the graph write (#1574).
- 7bdda70: Placeholder reconciliation can run over packed facts, decoding only duplicated ids, and the TypeScript walk can return its facts packed, so the app pipeline can stay packed until publication (#1574).
- 5499e97: Reuse one authenticated same-transaction replay for saved @18 call core and capacity validation, preserving standalone checks, historical behavior and existing limits (#1573).
- 7e02cd1: Reserve the selected user-stories renderer workspace on the combined knowledge path while retaining common compiler owners and legacy generic bounds.
- 29cb1ed: Compose sequential Capability and Actor renderer scratch by peak while retaining both persistent reference roles and existing output admission (#1557).
- e2ae719: Reuse each settlement materialization's validated attempt prefix for count and observation, preserving exact reservation bounds, corruption checks and existing resource limits (#1581).
- b268421: Fetching one full spec artifact compiles only that artifact instead of the whole bundle (#1582).
- c6e852f: Drop story validation indexes before rendering and compose the logical worker reservation over the actual validation/rendering stages without changing story output or admission limits (#1555).
- b895259: TypeScript pending-call and Pulumi relationship completion now run through the shared fact interface, so they work unchanged over packed facts (#1551).
- e60107e: TypeScript guessed-target reconciliation, config-driven bare-import resolution and the directory endpoint closure now run through the shared fact interface, so they work unchanged over packed facts (#1566).
- 4a152d2: Next.js pages-router Screen completion now runs through the shared fact interface, so it works unchanged over packed facts (#1562).
- a8d6b1b: The TypeScript directory walk now packs each file's facts once and keeps the incremental cache packed, cutting peak extraction memory on large repositories (#1571).

## 0.57.0

### Minor Changes

- 6fd3aa5: MCP read sessions now capture permission Actors with Capabilities under one bounded read and compile them into the official user stories, keeping each Actor's ordered assumption alternatives and Endpoint citation (#1539).

### Patch Changes

- 99bc779: Expose the combined Actor and Capability user-stories compiler with explicit caller-admitted flows and security limits. This spec-only entrypoint preserves existing admission behavior; host integration remains separate.
- 583d3cc: The knowledge overlay offers one bounded read of Capability and Actor facts under a combined charge (#1534).
- 9922f58: Count derived user stories omitted by their own provenance as well as supporting facts, and explain both cases accurately. Omission-bearing artifact text and content identities change; stored facts and provenance remain unchanged.
- ff4861e: TypeScript Next.js app-page and route completion now run through the shared fact interface, so they work unchanged over packed facts (#1548).
- 0319f4a: TypeScript Vue Router route and navigation completion now run through the shared fact interface, so they work unchanged over packed facts (#1543).

## 0.56.0

### Minor Changes

- 90b16f4: Show weak Actor caller-set assumptions with ordered alternatives and exact source citations in the official user-stories compiler. Preserve Capability-only output; host adoption remains separate.
- ce01ddb: The knowledge phase publishes permission Actor candidates beside the other deterministic producers (#1530).

### Patch Changes

- c99b07a: Give graph recovery passes one interface over plain and packed facts, and close over missing edge endpoints while decoding only the edges that reference them, in preparation for bounding ingest memory on large repositories.
- f13ce31: Publish recovered facts packed: ingest holds each fact as compact JSON from merge through the conditional graph write, decoding only the facts it needs to compare, so publishing a large repository uses less memory with identical rows.

## 0.55.2

### Patch Changes

- 1c92daf: Add a library-only weak Actor caller-set producer with an explicit versioned Allow convention and exact import-bound permission-site evidence. Automatic publication and compiler-visible assumption rendering remain separate.
- 62893d8: Let graph recovery stages select packed facts by identity, such as specific ids or edge endpoints, and decode only those, in preparation for bounding ingest memory on large repositories.
- e4af230: Read every stored or packed number back with its exact value: JSON floats are now parsed correctly rounded, so a fact never changes by a last digit between writing and reading.
- 36101cb: Add lossless packed fact types and an order-preserving packed fact store to the graph core, the building block for bounding ingest memory on large repositories.

## 0.55.1

### Patch Changes

- e6db0b2: Recovered user stories render capability names, ids and inline provenance as literal Markdown text, so stored HTML, fences, backticks and headings stay inert (#1453). The artifact viewer decodes these escapes, so it still shows the exact text.
- b05b327: Ordinary spec export builds each Markdown register and its JSON index (US-TM, gap register, rule evidence) in one pass instead of two, with unchanged artifacts (#1478).

## 0.55.0

### Minor Changes

- 7ff6356: Publish ORM DomainEntity candidates and their `MAPS_TO` mappings with the knowledge phase after ingest, beside the Capability and BusinessFlow producers.

### Patch Changes

- 1d9179a: Derive weak, provenance-cited DomainEntity candidates and `MAPS_TO` relations from structurally validated JPA and SQLAlchemy persistence facts in the knowledge overlay.
- b55eb11: Selected-compiler logical accounting now counts security findings with a proven per-row JSON value count, instead of one value per serialized output byte. This lowers the reservation floor the MCP read service derives for official artifacts. At 4 MiB / 5,000 rows / 100k values it falls from about 19 GB to about 9.5 GB.
- f3d68ee: The registers workspace now charges the listed-subject token scratch at its proven five-part maximum, not one fragment per input byte. At 4 MiB / 5,000 rows / 100k values the selected-compiler logical floor falls from about 4.0 GB to about 2.4 GB.
- beddd10: Selected-compiler logical accounting now charges typed provenance, omission and source-rule decodes by an explicit ownership recurrence, instead of one value role per input byte. Fixed fallback text, omission classification and the omission `Arc` counters each get an explicit charge. At 4 MiB / 5,000 rows / 100k values the derived floor falls from about 9.5 GB to about 4.0 GB.

## 0.54.0

### Minor Changes

- ffa850d: Include captured derived Capability stories and their exact cited assertions in complete-session user-stories output, with immutable paging and existing curation/export limits.
- af8fc01: Bind complete MCP read sessions to the captured store revision, rejecting observed changes during acquisition while preserving existing artifact content identities (#1471).

### Patch Changes

- bae1e88: Add a deterministic library producer for weak BusinessFlow candidates from bounded traces with exact base citations, preserving partial and Gap state. Host publication and rendering remain separate.
- 715448d: Prepare bounded immutable Capability selections for complete read sessions while preserving existing artifact output.
- 26b81f7: The GraphQL Capability rule now refuses support whose provenance names an artifact or observation evidence basis, so derived capabilities never relabel non-source evidence as source. The rule version moves to 2, so existing Capability ids and the knowledge recipe change on the next ingest.
- c48a8cf: Cite the exact Allow decorator and named import bindings for GraphQL permission syntax. Legacy `Endpoint.permissions` narrows: same-file enums, unproved import bindings and multiple Allow sites now omit permission lists. Runtime authorization semantics remain unestablished.
- 82ca30a: The background knowledge phase also publishes traced BusinessFlow candidates with the knowledge overlay; a producer that cannot finish is reported by name while the others still publish.

## 0.53.1

### Patch Changes

- 1be3e04: Derive provenance-cited Capability candidates from structurally validated GraphQL resolver facts, including adapter-supported API directory names.
  
  Accept adapter-proven TypeScript resolver class and member names containing `$` or Unicode while retaining GraphQL field-name rules and exact owner/identity checks.
- ad2cb96: The background knowledge phase now publishes deterministic knowledge facts, starting with GraphQL Capabilities, in the knowledge overlay after each ingest, with or without a local model.

## 0.53.0

### Minor Changes

- 3f802f2: After an ingest finishes, Cartograph now runs the knowledge embedding phase in the background. When the local Ollama embedding model is installed it embeds the new graph and records the run in the knowledge overlay; without it, the run is reported as skipped and the ingest is unaffected (#1441).

### Patch Changes

- b724a8f: Directory recovery now holds TypeScript receipts in a compact form: each file's identity is stored once and shared by its receipts, and the full receipt is rebuilt when needed. The receipt budgets measure this retained memory, so a large repository keeps more of its receipts citable within the same limits. Persistence also writes receipts one at a time instead of holding the whole batch's JSON (#725).
- 3bd282b: Adds graph-first hybrid retrieval from ADR-0051 as a library step. A query that names a symbol or endpoint returns that node first, then its graph neighbours, then text matches ranked by SQLite full-text search and, when a local embedding model is available, fused with vector similarity. Every hit cites the base graph and nothing is written back (#1442).
- 85c2a00: Provenance can now record its evidence basis, separately from its tier: source, artifact or observation (ADR-0051). Artifact evidence cites the artifact's content digest, its kind and an offset or member path. Existing graphs and exports are unchanged, and read as `source` (or `observation` for T1 facts). No artifact reader ships yet (#1444).

## 0.52.1

### Patch Changes

- a4a7e25: Adds the knowledge-layer embedding phase from ADR-0051 as a library step. It picks graph nodes by a versioned recipe, redacts their text before it reaches the local embedding model, and reuses cached vectors for unchanged nodes. With no embedding model it reports a named omission and leaves the base graph untouched (#1441).
- a357733: Adds the knowledge overlay store from ADR-0051. Derived knowledge-layer facts live in a separate, disposable file, and each fact cites the base facts it rests on. Facts are read back only at the base revision they were derived from. The overlay is discarded together with cached vectors whenever the graph is cleared or a retained source is forgotten (#1440).

## 0.52.0

### Minor Changes

- 4651f1a: Python ingest reads `aiohttp` client requests as fetch sites: `aiohttp.request(method, url)` and the verb methods of a once-bound `aiohttp.ClientSession`, whose `base_url` is resolved against as aiohttp does (#1432).
- 26d50c4: The receipt store shares its capacity across retained sources: one source's recovery keeps receipts up to a ceiling proportional to its captured bytes (never below a sixteenth of the store), and anything past it becomes a named omission rather than crowding other sources out (#725).

## 0.51.0

### Minor Changes

- 09e2fc4: Kotlin RestTemplate, WebClient, OkHttp, Ktor and Apache HttpClient requests in property or top-level initializers are now recovered as file-owned client calls (#1426).

## 0.50.0

### Minor Changes

- 02979d5: Java sources now recover Apache HttpClient 4 and 5 request constructors (`new HttpGet(…)` and siblings) as client calls (#1411).
- 53a10b4: Java sources now recover OkHttp `Request.Builder` chains as client calls (#1405).
- f754460: Kotlin sources now recover Apache HttpClient request constructors (`HttpGet(url)` and siblings) as client calls (#1420).
- 84f8765: Kotlin sources now recover Ktor client requests (`client.get("…")` and the other `io.ktor.client.request` verbs) as client calls (#1416).
- cbe6696: Kotlin sources now recover OkHttp `Request.Builder` chains as client calls (#1409).

### Patch Changes

- 88f85fc: Java and Kotlin adapters now treat Kotlin Multiplatform and Android test source sets (`jvmTest`, `commonTest`, `androidUnitTest`, variant `testDebug`, …) as test code (#1424).
- 4cc7e65: Kotlin: a `suspend` declaration directly after a wildcard import is no longer dropped by the soft-keyword pass (#1417).

## 0.49.0

### Minor Changes

- 51ffaab: Java sources now recover JDK `java.net.http` request builder chains as client calls (#1395).
- f40418c: Kotlin Spring Kafka producers (`KafkaTemplate.send`) and `@KafkaListener` functions now become `kafka-topic` event sites, like their Java counterparts.
- 9efa9cb: Kotlin sources now recover Spring `WebClient` request chains on proven clients (#1342).
- ae81eb5: Next.js App Router pages (`app/**/page.tsx`) now become screens that render their default export, under a package that declares `next`.
- 0cf43c8: The coverage report now claims Python SQLAlchemy data models (Partial), backed by a conformance fixture, so recovered `DataEntity` schemas from Flask-SQLAlchemy and declarative models are no longer reported as uncovered (#1389).

### Patch Changes

- 93a61f4: A Java Spring `WebClient` base URL that ends in `/` now keeps the slash before a relative request path, so `create("https://host/api/").get().uri("orders")` binds to `/api/orders` (#1380).

## 0.48.0

### Minor Changes

- 94bc02b: Rust sources now recover rdkafka producer records and consumer subscriptions as Kafka event sites (#1371).

### Patch Changes

- fd72cf6: Build on Rust 1.97.1 and wasmtime/wasmtime-wasi 50.0.1 (#1376).

## 0.47.0

### Minor Changes

- 164b151: Go sources now recover AWS SDK SQS/SNS request structs and kafka-go writer and reader configs as event sites (#1349).

## 0.46.0

### Minor Changes

- 885e020: Add an explicitly selected analyst family with exact-node retrieval while preserving historical task behavior and existing resource limits.
- 885e020: Allow direct context queries to retrieve one exact graph node without neighboring facts or edges. Existing query limits and specialist protocols remain unchanged.

### Patch Changes

- 48e527e: Bump libz-sys to 1.1.31 for RUSTSEC-2026-0418 (#1368).

## 0.45.0

### Minor Changes

- d9ff4bc: Java sources now recover Spring Kafka `KafkaTemplate` sends and `@KafkaListener` topics as event sites (#1347).
- d8962f8: Kotlin sources now recover Spring `RestTemplate` client calls on proven receivers (#1340).
- f539379: Python sources now recover boto3 SQS/SNS and kafka-python producer and consumer calls as event sites, and event SDK registry files can declare `pypi` entries in schema `@2` (#1343).

### Patch Changes

- 3220b98: Prefix routes now declare how they match (`prefix_kind`: node-http raw `startsWith`, Next.js one-or-more or zero-or-more catch-alls). Same-repo and cross-repo fetch stitching match by that kind and keep overlapping matches as ambiguity Gaps. Cross-repo joins also reach method-less `ALL`/`ANY` routes.

## 0.44.0

### Minor Changes

- 65fd067: Add opt-in captured analyst @15 phase-aware discovery guidance while preserving historical definitions, source admission and investigation limits.
- 4aaccb3: Recover Next.js API routes: `pages/api` default exports and app-router `route.ts` method exports under a package that declares `next` become cited endpoints, so client fetches can stitch to them.

### Patch Changes

- 6bba3f1: Client fetches now stitch to method-less `ALL` routes (Express `all`, node-http, Next.js pages/api), as they already did to `ANY`.
- 3223a43: Persist the first next-prompt host-limit preparation phase and identify the cumulative query-and-inventory byte budget only when that typed check refuses.

## 0.43.0

### Minor Changes

- 0a25aa2: Add opt-in captured analyst @14 source-action schemas bound to admitted facts, with semantic rejection before source preparation and saved-task reopen support.
- a79cfab: Prove Flask repository decorators and class-based decorator factories (such as SimpleLogin's concurrency lock) non-authenticating when their wrapper can only call the view or raise non-401/403 errors.
- 199d477: Flask auth: a request hook that only ends requests with a literal 429 or 5xx status (except 511) (such as a maintenance-mode hook) no longer makes routes unknown.

## 0.42.0

### Minor Changes

- b78ccdd: Show exact saved provider-reported token observations on active specialist task rows without expanding projection limits.
- b4f4347: Show exact provider-reported cache-read tokens in specialist task activity with independent unknown and unavailable states.
- d8ee6d7: Persist provider-reported cache-read usage through bounded investigation replay and expose optional exact activity metadata without changing resource limits.
- 55ce195: Give saved chat panes independent keyboard-scrollable message regions and distinct author styling.
- 5f2d2e0: Graph load moves recovered facts into the publish instead of copying every edge and changed node, and receipt matching reads a borrowed view of the merged edges, lowering peak memory on large ingests (#1235).
- f24f5dd: Flask-Admin model views and default index views now show the routes Flask-Admin itself provides (list, create, edit, details, delete, action, export and ajax), when the repository pins a checked Flask-Admin release (1.5.7 or 1.5.8). Unpinned or unchecked versions stay Gaps.
- 2bf0141: Mark Flask-Admin views as requiring login when their `is_accessible` provably checks `flask_login.current_user.is_authenticated` on pinned, checked releases.
- b3b8e06: Go `net/http` client calls (`http.Get`/`Head`/`Post`/`PostForm`, `http.DefaultClient` verbs and `http.NewRequest`) now become fetch sites that stitch to same-repo endpoints or cited Gaps, also in client-only manifests (#1279). Method-less routes such as Go `HandleFunc("/x", …)` now match a fetch of any method.
- 7b57678: The Java adapter now recovers JAX-RS resource endpoints (`jakarta.ws.rs` and `javax.ws.rs`). Methods it cannot place, such as inherited resource methods, interface methods, sub-resource locators and dynamic paths, become explicit route Gaps.
- 28108e6: OpenFeign `@FeignClient` interface methods now become fetch sites joined to their client's url, or Gaps naming an undeclared service (#1287).
- bf45266: Resolve same-arity Java overloads when literal, null or lambda arguments rule out every overload but one, instead of reporting an ambiguous-overload gap.
- a2869d5: Spring `RestTemplate` calls on proven receivers now become fetch sites that stitch to same-repo endpoints or cited Gaps (#1283).
- 031d602: Java Spring `WebClient` request chains on proven clients become fetch sites, with inline literal bases joined and field-held client bases reported as undeclared runtime bases.
- 3623dac: Spring Web and JAX-RS endpoints declared in Java or Kotlin test source sets (`src/test`, `src/testFixtures`, `src/integrationTest`, `src/androidTest`) are no longer reported as product endpoints. The test code itself is still recovered.
- 585a1bd: Add an explicit bounded local Ollama metadata API check in Agent Chats without task dispatch or model-readiness claims.
- 7595f7c: Preflight now counts and names the Go files that recovery skips because of a platform file-name suffix or a build constraint, instead of leaving them out silently.
- 9220406: Preflight now names Scala, Groovy, Dart, Elixir, Erlang, Clojure, F#, Visual Basic, Haskell and Lua sources that no adapter covers, instead of omitting them silently.
- a064463: Python `requests` and `httpx` module calls are now recovered as fetch sites, so they stitch to same-repo endpoints or show as cited Gaps (#1238).
- d1b0eee: Python `requests.Session()` and `httpx.Client()`/`AsyncClient()` receivers are now recovered as fetch sites, and httpx `base_url` joins resolve under the base path (#1240).
- cb044d5: Show exact provider-reported usage in task-bound saved chat snapshots without changing durable records or resource limits.
- 6ffa0cb: Add a local scale benchmark (`scripts/bench-ingest.mjs`) that records ingest time, peak memory and deterministic coverage counts for pinned repositories, and compares two runs.
- b46f90c: Add opt-in analyst and auditor @11 progress guidance while preserving historical @10 definitions and existing investigation limits.
- 442d008: Recover TS/JS repositories that contain symlinks (such as `CLAUDE.md -> AGENTS.md`): symlinks are skipped unread and named in coverage instead of failing the whole recovery.

### Patch Changes

- e6c3170: Retain fixed action-decoder stage and error-class diagnostics without rejected content or changes to accepted actions.
- 3f9d7bb: Compile the Python and Kotlin adapters' tree-sitter queries once per process instead of once per file.
- bb426ed: Compile the Go and Java adapters' tree-sitter queries once per process instead of once per file.
- 1ca5046: Add an explicitly selected analyst definition with complete finish examples and existing shape bounds.
- ddb2f86: Add inactive bounded Ollama response-schema transport support with exact firewall binding and existing resource limits.
- 6ac7a6f: Distinguish fixed action families at existing typed rejection boundaries without retaining rejected content or changing accepted actions.
- 287fee1: Add explicit captured analyst @13 with bound role/mode response schemas, preserving historical definitions and desktop availability.
- 2c5d9af: Kotlin functions, properties and parameters named `dynamic` or `suspend` no longer make the parser drop the declaration, or under stacked class annotations the whole class with its endpoints. A block comment before a declaration, such as after an expression-bodied function, no longer drops that declaration either.
- 90f21a8: Kotlin Spring endpoints proven only by a star import now become route Gaps when the repository declares a same-package name that obscures the import, and qualified lookalike annotations no longer prove a Spring mapping.
- 224ce73: Retain fixed content-free phases for existing next-prompt input-preparation failures.
- 325fd8a: Preserve fixed bounded-provider failure codes and distinguish host-limit and store-capacity diagnostics without changing terminal status, retries or resource limits.
- 40b790c: Distinguish content-free investigation response rejection stages in saved failure diagnostics without retaining rejected provider content.
- 3cbcf6d: Attribute action semantic refusals to fixed content-free rule categories without changing admission.
- 490641d: Java Spring endpoints proven only by a wildcard import now become route Gaps when the repository declares a same-package type of that name, qualified lookalike annotations no longer prove a Spring mapping, and single-file extraction keeps wildcard-proven Spring and JAX-RS endpoints.
- 7c2f60e: Preserve nested finish decoder diagnostics and distinguish bounded finish shape groups without changing accepted actions.
- f909e19: Retain the existing content-free action-decode refusal class in saved investigation failures without retaining rejected provider output.

## 0.41.0

### Minor Changes

- dc3d031: Show known unread saved results and incomplete-check uncertainty on the Agent Chats rail.
- 1d47e05: Show truthful tier and source/graph citation metadata on saved specialist messages.
- 0bcac5c: Add a keyboard search palette over open and saved task chats, including saved specialist names and admitted answers.
- e75a18c: Add accessible Agent Chats pane-layout controls with three-up wide-row placement and honest empty chat slots.
- 0f88f7a: Show each saved chat pane’s specialist name, version and requested provider/model attribution.
- d0162c6: Flask-Admin views now appear as Flask endpoints: each `@expose` method of a registered view gets its composed path, its `<endpoint>.<method>` name and an access-check note, and library-generated admin routes are recorded as Gaps.
- 4770c95: Search saved specialist display names in Agent Chats and Investigations with bounded name-match metadata.

## 0.40.0

### Minor Changes

- b7a98c2: Flask endpoints now follow a returned repository helper's responses when it cannot return `None`, and read `str(…)` as a text response.
- 9c6188d: Flask endpoints now record string responses as `text` and read `make_response(…)` bodies and statuses.
- 9e1e696: Gap Triager @2 saves and displays an admitted cited final next-step message while preserving historical roles and existing limits.
- 41d5360: Find saved specialist answers by their admitted final narrative text and open the exact cited chat.
- 6f7caf2: Add an immutable Spec Writer version with an admitted, persisted cited final message.

## 0.39.0

### Minor Changes

- 12ace4f: Treat a repository Flask decorator that aborts with 401 or 403 as an authentication guard.
- a46d391: Add an Agent Chats destination for browsing saved specialist conversations and opening their exact findings.
- 8e5cd90: An endpoint whose authentication is unknown because a request hook may block now names each such hook and cites where it is registered. A function a hook calls no longer counts as blocking just because it returns a value.
- bea0a23: Allow one fixed-feedback, finish-only correction after an ordinary source-replay refusal, within existing investigation limits.
- 1dc080c: Add parent-bound follow-up task dispatch from validated Agent Chats panes.
- 19dc304: Route confirmed Quick Dispatch tasks to their exact validated chat in Agent Chats.
- 03a26f4: Recover Flask authentication guards onto endpoints: each endpoint is required, none or unknown with reasons, and security findings cite the guard evidence.
- 981a7ff: A Flask request hook that may block no longer makes the endpoints of a separate Flask application `unknown`. It applies only where the hook and the endpoint may share an application, and anything that can't be proven apart still keeps the hook.
- 5f25aa2: Reconcile focused saved chats with durable task progress and committed findings.
- 0c7a118: Send supported parent-bound reply tasks directly from saved Agent Chats panes.
- 262e24e: Browse persisted conversation turns from saved Agent Chats panes.
- 45b3709: Group loaded saved chat exchanges by conversation with explicit parent and fork links.
- 801c52d: Flask routes that carry only flask-limiter rate limits or flask-cors `cross_origin` decorators now report no authentication instead of unknown, when those decorators are proven to come from those libraries. The result lists the decorators and records the assumption.
- ff51214: Show bounded active-task status counts with explicit partial, last-observed, and unknown states.
- ff51214: Open a saved investigation directly from its exact-version active specialist task row, with one shared empty or uncertainty state for the activity list.
- 08f0b95: Let saved-result notices open the matching validated conversation in Agent Chats or its findings in Investigations.
- 094a3d5: Let Agent Chats place saved-result notices in any screen corner with a safe local default.
- f62306a: Find saved investigations by accepted finding titles and statements in both history views.
- d5b83d5: Dock up to four saved task chats with close/reopen, oldest-pane replacement, and explicit last-observed snapshots.
- 951267e: Show validated saved findings and exact-task historical citations in Agent Chats panes.
- 49f57c9: Show bounded app-shell notices only for results observed after a valid pending transcript baseline, and keep dismissed notices in the session unread list until the matching saved transcript is visibly opened.

## 0.38.0

### Minor Changes

- 483e9d3: Show persisted active investigations in a dedicated history view (#1107).
- a4c8ffd: Navigate persisted follow-up investigation tasks by conversation, with task-specific saved findings.
- 2214466: Draft adapter work packets from a recovered repo's coverage inventory (#1101).
- 3fadf7d: Persist bounded dispatch chat identities, redacted questions, and immutable result references for specialist investigations (#1115).
- 7d791ca: Add a cited Gap Triager specialist for proposing bounded next investigation steps from recorded gaps.
- 2623282: Observed infrastructure now shows a client whose instance is computed at runtime, such as an SMTP server chosen from configuration, as its provider in the topology. The instance stays a recorded Gap (#1116).
- 6887d69: Show persisted investigation questions and saved specialist result references in the selected investigation view (#1121).
- 0687c4c: Expose validated, bounded persisted investigation chat and saved producer identity to the app host (#1119).
- 675d35b: Preflight shows the adapter lookup for each unsupported or partial cell: which adapter covers it, which plugin declares it without a passed gate, or that none does.
- 91d51e7: Allow quick dispatch to start Standard completion specialists with explicitly selected recorded inventories, enforce the advertised selection count, and refresh choices only after stale-context rejection.
- 2722627: Add keyboard quick dispatch for supported saved investigations.
- 9bca7e0: Show the saved provider and model for each active specialist task, and reject new tasks whose saved attribution cannot fit the existing bounded activity projection before committing task rows.
- 711d82b: Show bounded active task status beside each exact versioned specialist choice (#1123).
- 733ca58: Add the versioned, cited Spec Writer investigation specialist.

### Patch Changes

- f549f61: Coalesce investigation conversation refreshes and refresh explicitly loaded older pages when their rows change, without discarding page order or cursor state.

## 0.37.0

### Minor Changes

- 63403ad: Generate a deterministic W6 work packet for each coverage-matrix cell no installed adapter covers, citing sample sources and the conformance fixtures to write first.
- b43f6f8: The Atlas stays readable at estate scale: a band with more than 60 clusters opens as repository and range groups that wrap into a grid, with edges combined between groups. The code bands are now labelled "Server code" and "Client code".
- ea78eb9: Add an owner-only command to activate MCP task access for a registered client with explicit resource and identity limits. Owner status reports the live grant; credential rotation drops task authority.
- 457680a: Add explicit investigation access controls to MCP Settings, including registered client selection, separate task grants, and observed grant status after credential rotation.
- 2b0c8f7: Add deterministic (T0) HTTP endpoint recovery for Rust axum and actix-web routes.
- 33bce82: Recover reqwest HTTP client calls in the Rust adapter, so Rust services join the client-to-endpoint stitching.
- fc425b1: Search persisted investigation summaries by saved question or specialist ID, including older retained history.

### Patch Changes

- 3e3060e: Keep content-free validation classes in ordinary investigation finish and unadmitted-read diagnostics without retaining rejected output or retrying the task (#937).
- c7918f2: Service-worker code and browser-storage Resources now appear in the Atlas Client layer, and anonymous callbacks get readable display names (#1078).

## 0.36.0

### Minor Changes

- 440afa7: Recover raw Node `http.createServer` route predicates (pathname comparisons, prefixes, literal regex alternations and method-path lists) as `node-http` endpoints, with computed routes cited as gaps.
- 95b6d50: Adds a deterministic Rust language adapter extracting files, symbols, imports and call edges.

## 0.35.0

### Minor Changes

- 596d192: Preflight reports, for each unsupported or partial coverage cell, which installed built-in adapter or WASM plugin declares coverage of it and whether that claim passes its conformance evidence.
- c1c5dd7: Separate established interpretation-seam omissions into an Interpretation backlog lane and summary count while keeping all gaps listed and counted (#994).

## 0.34.0

### Minor Changes

- c623608: Spec Workbench renders Markdown artifacts (headings, tables, code, lists) by default with a Rendered/Source toggle. Rendering never interprets HTML or creates links from artifact content. Diagram and JSON artifacts are unchanged.

## 0.33.0

### Minor Changes

- 048e510: Kotlin calls on an interface bound with a Hilt `@Binds` in a `SingletonComponent` module now continue to the bound implementation, so Android journeys can reach the endpoint the implementation calls. The journey records that hop as a `hilt_dispatch` assumption.
- a4af007: Python code that writes SQLAlchemy models, directly through a proven session or through a model helper classmethod such as `Alias.create(…)`, now records `WRITES` edges to the model's data entity. Journeys whose goal endpoint is fetched from a screen now count those writes, so they can reach an observed-write goal.

### Patch Changes

- bb883f3: Refine MCP evidence-read admission to include temporary capture-manifest decoding and selected-file buffers before reading retained source text. Existing file/span policy ceilings are unchanged.

## 0.32.0

### Minor Changes

- d53d147: Kotlin screens that get their view model from Hilt's `hiltViewModel` (by type argument, or through an assisted factory callback) now resolve calls on that view model, including unqualified calls inside `with(hiltViewModel<VM>()) { … }`.

## 0.31.0

### Minor Changes

- 4d50730: Kotlin calls on a parameter, property or local whose type is written in source (for example an injected `private val api: ApiService`) now resolve to that type's method, so call chains reach Retrofit interfaces and view models. Receivers with no stated type still resolve nothing.

## 0.30.0

### Minor Changes

- 4b60083: Recover bounded T2 journey candidates (`journey-candidate@1`) from path-router screens, navigations, client calls, and server flows, and emit `journeys.md` in compiled spec bundles.
- 089d186: Journey candidates now start from Android Navigation 3 initial destinations. Startup navigations and callbacks passed into screen entries are followed only as recorded assumptions, each with its limitation and the source span where the callback is registered; callbacks a screen never uses are listed on the step. `journeys.md` adds a per-entry search summary.
- fe3b69b: Record initial destinations, startup navigation phases, and entry-callback registrations for Android Navigation 3 screens in Kotlin repositories.
- f4c3667: Recover Android Navigation 3 `entryProvider` screens and back-stack `NAVIGATES_TO` edges in the Kotlin adapter.
- b62479c: Vue Router navigations now link the code that performs them to the screen they open, including navigations through a project's own navigation helper. A navigation inside a condition is marked as conditional, and targets that cannot be read from source are reported as gaps.
- d02c0db: Vue Router route tables now become screens, each rendering its component file, including tables built by an imported helper from a constant path table and a components object. Routes that cannot be read from source are reported as gaps.

### Patch Changes

- 508a8e9: The Atlas opens on large graphs instead of freezing the app: banding Gap nodes no longer scans every edge once per Gap.
- 11cf97b: The desktop app opened from Finder or the Dock now finds a Homebrew-installed GitHub CLI login, so private repositories clone without setting GH_TOKEN. When no credential is found at all, the error now says so and lists where it looked.

## 0.29.0

### Minor Changes

- 51cfad0: Vitest and Jest tests are recovered, and a test that sends a GraphQL document through a proven test client (such as Vendure's `adminClient` and `shopClient`) is linked to each resolver operation it requests, so `test_arrangements.md` shows which GraphQL operations have a static test reference.

### Patch Changes

- 1ddf3a5: Spec diagrams stay inside their card: the canvas no longer covers the navigation rail when the page scrolls, and a diagram with one or two nodes is no longer zoomed to fill the canvas.

## 0.28.0

### Minor Changes

- 40313f1: Flow dossiers now name, for each step, the tests that reference it directly — by requesting its endpoint or calling its function — and the steps no recovered test references. References are static, not execution coverage.
- 640dfe4: NestJS GraphQL resolvers are recovered: each `@Query()` and `@Mutation()` method becomes a GraphQL endpoint with its field, the API it is assumed to be mounted on, and any `@Allow` permissions, so its flows can be traced like HTTP routes.

## 0.27.0

### Minor Changes

- 9f85d16: Python recovery now reads pytest tests and links each Flask test-client call to the endpoint it requests, by `url_for` endpoint name or by path, with the statuses the test asserts and the response keys it reads. Calls it cannot resolve become named gaps. The spec bundle gains `test_arrangements.md`, which lists the tests referencing each endpoint with what they assert, and the endpoints with no recovered test reference.
- dc9cbb2: The drift register now compares what each endpoint's tests assert with what its recovered responses return, in both directions, and names endpoints whose responses or test calls are incomplete instead of reporting drift for them.

## 0.26.0

### Minor Changes

- 6dc29d8: Investigations can use an owner-hosted OpenAI-compatible server (vLLM, LiteLLM, llama.cpp) as a local, loopback-only provider with the same bounded transport limits as Ollama.

## 0.25.0

### Minor Changes

- 8b528c1: Flask endpoints now record the literal query, form and JSON body names their handler reads, marked partial when a read is not literal, and the API contract scorer measures them against a documented contract.
- 8a99694: Flask endpoint responses now follow repository auth decorators (their wrapper's 401/403 returns) and the dicts a handler or its helper builds, citing each followed statement.
- 3038c41: Flask endpoints now record the response kinds (JSON, HTML, redirect), status codes and top-level JSON keys their handler's own returns prove, each cited, and the API contract scorer measures them against a documented contract's Output sections.

## 0.24.0

### Minor Changes

- e4dde62: Read env templates such as `example.env`, `test.env` and `env.example` as environment files, keeping key names only, and link each env-referenced observed resource to the files that declare its key.
- d82ecf2: Draw the env files that declare an observed resource's key on the topology map, as dotted `CONFIGURED_IN` links to the file named by its path.

## 0.23.4

### Patch Changes

- d8cd613: Index gap-node edge labels and rule relationships once per bundle instead of scanning every edge per node, cutting spec bundle time on large graphs by more than half.
- d10266c: Reuse prepared SQL statements and keep statement subjournals in memory when publishing graph patches, speeding up the database phase of ingest.
- add3554: Prepare investigation neighborhoods from fact references without building temporary response pages, preserving graph-admitted facts that exceed the public query response limit.
- 97dfd93: Reject malformed investigation namespace metadata before copying it into Rust during task loading.
- bd19308: Compile each TypeScript tree-sitter query once per grammar instead of once per file, roughly halving TypeScript ingest time on large repositories.

## 0.23.3

### Patch Changes

- 04e2f12: Coverage: channel and client-call stitching now reports its own `t0.events` row in the ingest summary and the coverage inventory, instead of adding its facts to the TypeScript adapter's counts (#874). A repo without TypeScript, such as SimpleLogin Android, no longer shows a not-applicable TypeScript producer carrying 41 nodes and 41 edges.

## 0.23.2

### Patch Changes

- 1245503: Kotlin: constructing an imported repository class no longer raises an unresolved-call Gap (#721). On SimpleLogin Android, those Gaps drop from 38 to 2, both members generated by kotlinx-serialization.

## 0.23.1

### Patch Changes

- e8020f8: Python absolute imports and Kotlin imports of extension functions, top-level properties, type aliases, annotation classes and enum entries now resolve to the repository file that declares them, instead of leaving an unresolved module Gap. On SimpleLogin, unresolved module edges drop from 177 to 17 in Python and from 69 to 4 in Kotlin.

## 0.23.0

### Minor Changes

- 265eb45: GitHub Actions workflows with `on.schedule` now contribute one `Schedule` flow trigger per cron, entering the workflow file itself (#779).
- 5cfe827: Python repositories with an `alembic.ini` now replay their Alembic migration history and report schema drift against the SQLAlchemy models, both ways, in the drift register's new Schema drift section; anything the replay cannot follow is a Gap (#833).
- cfbafda: SQLAlchemy `ForeignKey(Model.column)` targets now resolve to the recovered model when the declaring file names it and the column is declared, instead of always becoming a Gap.
- 855e4d1: Prepare investigation storage for separate client request namespaces while preserving existing App task history.
- 498b6f9: Bind durable investigation origin and intent metadata to its bounded indexed request namespace, preserving historical App records.
- 0d4792a: Scheduled jobs declared in crontab(5) files (`crontab`, `*.cron`, `cron.d/*`) now become `Schedule` flow triggers, entering the script each line runs like yacron jobs do (#779).
- 7da4f61: Recover client HTTP calls declared as data: Retrofit interface annotations in Kotlin and Java, joined to the repository's `Retrofit.Builder().baseUrl(...)`, and `{ method, path }` route tables in JavaScript and Vue, joined to their base URL by a wrapper or a direct `fetch`. A base read from runtime state resolves only when `cartograph.system.toml` declares it under `env`, so joined calls can bind to another repo's endpoints; anything unresolved stays a Gap naming the entry. Flask `<id>` and FastAPI `{id}` route parameters now match client paths like Express `:id` (#737).
- 6bc97eb: Add durable MCP client registration and bounded owner-side listing without enabling investigation access.
- efcb1be: A scheduled job's dispatched branch can now call a function imported from another module in the repository, or run a coroutine through `asyncio.run`, and its flow still starts at that function (#779).
- 8c84c15: Kubernetes `CronJob` manifests now become `Schedule` flow triggers, one per container, entering the script its command runs; entries mapped through the root `Dockerfile` are marked as an image assumption (#779).
- 85a954a: Python code that reaches Redis through the `limits` rate-limit storages (`RedisStorage`, `RedisSentinelStorage`, `RedisClusterStorage`) now shows an observed `redis` resource, or a Gap naming the call when the URL is computed at run time (#734).
- 737e088: Add owner-controlled local MCP credential rotation with revocation and shutdown fencing.
- 5bfc0c8: Add credential rotation to MCP Settings, clearing old client configuration while credentials are replaced.
- afb2024: Observed infrastructure now includes HTTP APIs: calls through the platform `fetch`, axios or ky whose URL starts with a proven absolute origin use one `http-api` resource per origin, whichever client made them. Relative URLs name no resource, a host the code leaves open is never truncated to a guessed origin, an env-led base records its variable name, and a computed axios or ky URL is a Gap.
- 961de17: Recover infrastructure from code that has no IaC. Proven browser storage (IndexedDB, Cache Storage, Web Storage), SDK clients (AWS SDK v3, Firebase, Supabase, Stripe, MongoDB, Postgres, Redis, Prisma) and auth clients (Auth0, Clerk, NextAuth, Cognito) become observed `Resource` nodes under Atlas → Infrastructure, each linked to the code that uses it. The client table is registry data that extension files can add to.
- 46b3843: Observed infrastructure from Python: import-proven boto3, redis, smtplib and SQLAlchemy clients become observed resources (#734).
- be17b60: A Python client whose identity is a function parameter is now followed to the function's one static caller (and on through `config.X`), so SimpleLogin's Redis rate-limit storage resolves to `MEM_STORE_URI` instead of a Gap. The result is InferredStrong and marked `identity_assumes_static_callers`.
- d326a47: Observed infrastructure from Python now follows a client identity into another repository module's constant (`create_engine(config.DB_URI)` becomes the `DB_URI` env variable), citing the assignment it rests on and failing closed on anything that may rebind it (#734).
- b40b0a0: A scheduled Python job such as `python cron.py -j stats` now enters the function its argument selects when the script's `__main__` argparse dispatch proves it, so each job's flow starts at its own code instead of the whole file (#779).
- 3627635: Scheduled jobs are now flow triggers. A yacron-format schedule file (`jobs:` with `schedule` and `command`) makes each job a `Schedule` trigger, which traces into the file the job runs and from there into its symbols and calls. The entry is Confirmed when the root `Dockerfile` proves where the repository sits in the container and nothing later in the image may replace it; otherwise it is marked as inferred, or kept as a Gap naming the job (#770).
- 3fee806: Add explicit scoped MCP sessions for bounded discovery, selected fact details, and retained evidence on graphs that exceed complete-snapshot admission. Scoped reads expose partial coverage and revision-bound continuation, preserve existing complete sessions, and compile bounded stored-fact data-model and rule-evidence partitions with distinct identities. Complete flow artifacts remain unavailable in scoped mode.
- 5a9804d: Recovering a source again now reclaims the receipts its previous recovery left behind. After a recovery publishes its graph, every receipt of that source that nothing pins and the current graph no longer binds is reclaimed under a `superseded` tombstone, so the receipt store stops growing with each re-import. Pinned and capture-forgotten receipts, capture bytes, and the receipts of a task prepared during the reclaim are kept (#725, ADR-0046 decision 5).
- 3baac0d: Observed infrastructure from TypeScript and JavaScript now recovers WebExtension storage: `chrome.storage.sync`, `browser.storage.local` and the other areas each become an observed resource, including through `webextension-polyfill` (#734).

### Patch Changes

- 93f221c: Keep App investigation retries in their own nonce namespace so a stored MCP task cannot be returned for an App request with the same nonce.
- 05d6bbd: Enforce the existing coverage archive byte limit before allocating canonical comparison JSON during saved archive validation.
- 52c710d: Keep the MCP protocol available when graph acquisition fails; acquire read context on the first read and retain one request-ID ledger across retries.
- afb2024: Observed infrastructure: a write through a cast, `!` or parentheses (`Object.assign(A as any, …)`, `(A!).X = …`) now voids the namespace it overwrites.
- 5e43471: The TypeScript and Python observed-infrastructure producers now assemble resources, `USES` edges and Gaps through one shared implementation, so both languages keep one output shape (#734).
- f1f9231: Publishing an extraction into the graph no longer holds a second whole-graph copy: the snapshot check streams the stored graph and compares it row by row, and the patch is built from borrowed snapshot facts with incident edges indexed once. On the SimpleLogin showcase, the publish heap peak drops from 735 / 509 / 576 MB to 535 / 303 / 357 MB per repo, and process RSS from about 1.9 GB to 1.41 GB (#725).
- ffd0227: FastAPI and Flask mounts on a function-local app now confirm a route only when the app's sole binding in its scope is the framework constructor, ahead of the call. A rebound, late-constructed, branch- or loop-assigned, `nonlocal`- or `global`-rebound receiver leaves the route unproven instead of fabricating a Confirmed path.
- ae0c9db: Bound durable investigation record serialization before allocating round-trip comparison values.
- bcf015e: Registering sources concurrently no longer fails as "storage unavailable" when another registration's commit is slow; the registry now waits up to 5 s for it (#839).
- a9a495e: Spec export no longer copies the graph to compile it: the projection borrows every selected fact and owns only the ADR, drift and security facts it derives, and typed provenance is parsed only for the facts ADR recovery and drift actually inspect. On the SimpleLogin showcase the export heap peak drops from 749 MB to 358 MB with the same artifacts (#725).
- 7d0bdf5: Bump tokio to 1.54.1 for RUSTSEC-2026-0386 (unsound `AsyncFd` constructors; Cartograph never calls them).

## 0.22.0

### Minor Changes

- 818dce6: Package the read-only context skill and its references with the desktop app. Settings shows build-matched guidance without enabling MCP or changing client settings, and reports missing or modified skill files as unavailable.
- 892219b: Add an explicitly enabled, bounded read-only MCP service and a packaged macOS stdio relay. Connected clients can discover recovered nodes, read compiled artifacts, and follow retained evidence without changing confirmed facts. Settings exposes service status, an explicit resource policy, revocation, and client configuration; Windows transport remains unavailable. Typed evidence failures distinguish positively recorded receipt omissions and exact-binding forgotten receipts from unproven absence.

## 0.21.0

### Minor Changes

- 7c19f16: Recover documented API contract drift from author-declared Markdown and OpenAPI 3 JSON references (`.cartograph/api-docs.toml` and `cartograph.system.toml` `api_docs`), comparing canonicalized `DocumentedRoute` facts against recovered `Endpoint` nodes and reporting `documented-not-served`, `served-undocumented`, and `method-mismatch` findings with both sides cited in `drift_register.md` (#739).
- 9c52068: Forgetting a retained source now also frees its receipts. Receipts that a saved investigation or staged proposal still cites keep their identity, marked as forgotten, and all others are reclaimed, so repeated recoveries no longer fill the receipt store permanently. Existing stores upgrade automatically.
- 135d565: Record the full `HEAD` commit SHA when recovering a clean local git checkout instead of `"workdir"`, fail closed before publishing if a clean checkout changes mid-recovery, and attach a bounded `commit_note` to `RepoCoverage` explaining why non-clean or non-repository local trees remain `"workdir"`.
- 9730ff9: Python recovery now finds routes declared on Flask Blueprints and FastAPI `APIRouter`s, and gives each route its full path. A router's own prefix is joined with the prefix it is mounted under, even when the mount is in another file. Nested routers compose their whole mount chain. A route is confirmed only when its chain ends at an app the code is seen creating. A mount on an app passed in as a parameter, or a router whose mount was never observed, is marked as inferred rather than confirmed. A prefix that can't be read, or a cyclic chain, is reported as a gap.
- dbd173d: Record positive per-fact receipt omission records and per-repository completeness in the graph store so callers can distinguish omitted primary receipts (`declarations`, `rules`, `store_capacity`) from unrecorded or non-omitted facts (#725).
- 203db4c: TypeScript primary-source receipts are now minted in a compact v3 format that stores each cited range once instead of repeating the file identity, cutting receipt size about 4–5×. More facts in large repositories fit within the receipt store's limits. Existing v1 and v2 receipts keep working unchanged.
- a5a522d: Record a human follow-up decision on an exact saved investigation finding and reopen it with its historical evidence. Decisions remain separate from graph acceptance and permission to execute work.
  
  Upgrades the investigation store; earlier app versions cannot open it afterwards.
- 9edf363: Python recovery now reads SQLAlchemy declarative models. Each model with a declared table becomes a data entity with its columns, including inherited ones, and its foreign keys and relationships link entities in the data model. Anything that can't be proven, such as a missing or dynamic table name, a table claimed by two models, or a reference to an unknown table, is reported as a gap rather than guessed. On SimpleLogin, all 80 models are recovered.

### Patch Changes

- dabeb65: Atlas layer filters show only facts that belong to the layer and their direct neighbors. Rule exits and rule gaps no longer appear under Infrastructure, browser-extension relationships (permissions, command handlers) stay visible under Client, and an empty Infrastructure layer now says what would fill it.
- f79bbda: Saving an export over an existing file is now atomic: the content is written to a temporary file beside it and swapped in only when complete, so a full disk or failed write leaves the previous file intact.
- 13241c8: Track committed graph revisions atomically across facts, source bindings and coverage, preserving legacy data during the additive storage migration. Bounded revision reads reject stale acquisitions and malformed generation metadata.
- 1a3a7e6: Exports from the desktop app (spec bundle and work packet) now open a native Save dialog and are written by the app, instead of using an in-page download that could freeze the app on macOS.
- 9a942f0: Large TypeScript repositories no longer fail to recover when their primary receipts exceed the extraction budget or the retained receipt store is full. Receipts that fit are kept in a stable order, facts keep their place in the graph, and coverage marks the recovery limited and reports how many receipts in how many files could not be cited, with a separate entry when the receipt store is full.
- a165fe7: Pin primary-source receipts cited by saved work packets so forgetting a retained source preserves those receipts as capture-forgotten instead of reclaiming them, and include work-packet receipt references in the retention preview and confirmation fingerprint.

## 0.20.0

### Minor Changes

- 86b9684: Use bounded query history in newly selected expanded investigations. Earlier queries remain available through explicit, budgeted replay, while the latest query stays visible. Saved investigations and pending retries retain their original specialist and request versions.
- 520dafb: Recover a data model from Java JPA code: import-proven `@Entity` classes become cited data entities with their table, fields and `@Id`, `@OneToMany`/`@ManyToOne`/`@OneToOne`/`@ManyToMany` fields become relations with cardinality and `@JoinColumn`, and Spring Data repositories bind to their entity so repository calls (`save`, `findById`, `deleteById`, …) show as reads and writes. Computed table names and relations to non-entities stay explicit gaps. The recovered data model document now lists entity relations alongside access edges.
- 804abc5: Check whether an expanded investigation's next prompt can fit before acquiring more evidence. Show capacity refusals separately from evidence results, preserve charged work through interruptions, and retain the original versions of saved investigations and pending retries.
- 3c85d3e: Render topology, flow, deployment, and data-model views as interactive local diagrams from structured graph facts and provenance, bound to the exported projection's schema and content identity (`AC-0246`, `AC-0247`). Selecting a node or edge opens its evidence, tier, and confidence, Gap and partial hops are visibly and textually distinct from confirmed ones, large views page or expand with explicit hidden counts, and copyable Mermaid text remains available as a secondary action.
- 55abc5b: Exclude interface-dispatch hops from T2 semantic matching: an unresolved call shaped `IFace.method` (for example `IChannel.listen`) is dynamic dispatch that embeddings cannot resolve, so it is now classified out of ANN candidacy — never embedded, never proposed — and stays an open gap tagged `dynamic dispatch - needs type hierarchy` so the register can group it. Conservative look-alikes such as `Item.get`, `IO.read` or `ID.parse` still enter candidacy.

## 0.19.0

### Minor Changes

- 0508c8c: Add an explicit expanded investigation budget for larger evidence sets. The selected policy stays bound to requests, consent, execution and saved history; existing investigations retain their original limits. Expanded prompts avoid repeating source-target descriptions, and storage and input limits still fail closed before excess work.
- bc0e913: Add a use-case projection with content acceptance: the guards recovered for a declared feature, such as Vendure cart readiness, become a decision table where every row cites its source. A check against an independently written expected set labels each expectation implemented, documented intent, inferred or unknown. An incomplete projection is marked partial and lists what is missing. Reading a row as a business guard stays a labeled, rule-derived T2 inference and is never promoted to a confirmed rule.

## 0.18.0

### Minor Changes

- fbcbae6: Make every directly emitted TypeScript/JavaScript callable citable as retained primary-source evidence, not only code that contains a detected rule, so investigations can cite plain functions, methods and strategy classes. Rule receipts keep their existing limits; declarations that do not fit the remaining extraction budget are counted per file instead of failing the capture.
- a023daa: Make TypeScript/JavaScript configuration citable: top-level variables, each first-level section of a configuration object, method decorators such as permission guards, and the header of a function too large to inspect whole each become their own citable primary-source evidence. Declarations that could not be given evidence are now listed as a limitation in the coverage inventory.
- 1ec3b1c: Make local settings inside very large functions citable: when a function is too large to inspect whole, the `const`/`let`/`var` declarations directly in its body, and the sections of any configuration object they hold, each become their own citable evidence. Top-level declarations keep priority within each file's evidence budget.

## 0.17.0

### Minor Changes

- 23aa519: Add explicit source and coverage-inventory obligations for V7 specialist investigations. Supported findings require the exact selected evidence in the finishing disclosure; insufficient-evidence results and unmet-obligation termination remain visibly distinct. Preserve the selected contract and evidence assessment in saved investigation history.

### Patch Changes

- 64479d6: Require an explicit replacement when a saved or selected investigation specialist is unavailable, preserving historical versions and selections across catalog refreshes.
- 1c03e4e: Reuse one immutable scoped graph projection for qualified specialist investigations, reducing duplicate preparation work and retained graph data while preserving evidence identities and query limits.

## 0.16.0

### Minor Changes

- 1ae8ebd: Every compiled spec artifact now carries a versioned schema and a content identity. The Spec Workbench shows both, a capped preview keeps the identity of the complete artifact, and Copy artifact or Export bundle refuse an artifact that changed after its preview was compiled, explaining why instead of handing out mismatched content.
- 0e3bbda: Specialists use compact recovered context with exact per-invocation disclosure. Identical graph representations are shared, differing evidence is preserved, and findings can cite only support supplied to their invocation. Saved results retain the finishing disclosure identity without storing source text; older investigations keep their original meaning.
- d45df37: Every recovery now reports a coverage inventory per repo, shown in the Workspace before any investigation. The inventory lists which deterministic and observational producers ran, which the layer hints excluded, which declared inputs were missing and which capabilities no installed producer covers. Each entry has a stopping reason scoped to that repo, revision and layer hints.
- 017edef: Settings and Preflight now show what each adapter covers as language × framework × capability cells, marked supported, partial or unsupported. Partial and unsupported cells say what they miss, and every supported or partial claim is checked by a conformance fixture the build runs.
- b678f75: Context reads can now be qualified by recorded coverage. Each recovery stores its repo's coverage inventory together with the facts it describes. A qualified context query reports, for every repo in scope, whether that inventory is recorded, stale or missing. It also reports what the inventory could not see and what was checked at read time, and never claims exhaustive coverage. The existing context query is unchanged.
- 4920b45: Client calls now cross repos when the system manifest says how. A system manifest can declare services, with the repo that serves each and its base URLs, and gateway routes that forward a path prefix to a service. After every repo is ingested, an absolute-URL call that one of these covers is joined to the matching endpoint in the serving repo, and the edge cites the declaration. A call no declaration covers, or one with several equally supported targets, stays a gap that says why.
- fadbd01: The context hub can now find nodes deterministically, by exact identifier, exact name or case-insensitive text, under a declared and versioned matching policy (`discovery-v1`). Each candidate is a bounded handle that carries its label, its source repo, path and commit, why it matched, its edge counts and its confidence. Same-name nodes in different repos stay separate candidates and are never merged or bound. Every response states what was examined and what the policy does not search, so an empty page never reads as "nothing exists". Continuation cursors are bound to the snapshot, normalized query and policy version, and a candidate too large for the byte budget fails explicitly instead of being skipped.
- f2c72ee: Folders can be dropped onto the Connect step. Each dropped folder ingests the same way as a typed local path, or as a system when it holds a system manifest. Every path then shows its own outcome: rejected with a reason, ingested with each repo and its commit identity, or failed with the error. The set is never reported as fully ingested while any folder is still running, rejected or failed, and a local ingest's summary now names its repo.
- d8f35ae: Event SDK call shapes and the deployed resources that back channels are now versioned data files instead of code. Cartograph also reads extra registry files from its `registries` folder in the app data directory. Each file is checked whole: an invalid one is rejected with its reason in the ingest summary and changes nothing, and every channel fact names the registry file that recognized it.
- 2962cfb: Add explicit captured-source inspection requirements for versioned specialist investigations. Required ranges retain their original source identity, and saved results distinguish evidence acquired, supplied, and cited without treating execution completion as task fulfillment. Structured source locations refer to retained captured bytes; older investigations remain unassessed.
- ca157d1: Keep semantic proposals staged unless compatible independently reviewed evidence passes their exact family policy. Distinguish calibration, visible mechanism tests and independent evaluation in persisted provenance summaries.
- 62589a1: Prepare local agent work packets from explicitly selected recovered facts, compiled artifacts, and historical decisions. Record requested intent, missing information, and proposed verification alongside immutable evidence. Save, reopen, review with revision-conflict detection, assess source freshness separately, and export an inert Markdown handoff. Packet requests do not authorize or execute agent work.
- 6e4c1f4: Unresolved TypeScript and JavaScript calls whose callee name matches exactly one function or method in scope now gain an inferred link to it, from a named deterministic rule rather than embeddings. The original gap stays visible and counted, the link is marked inferred rather than confirmed, and it is recomputed on every load, so a second symbol with the same name withdraws it. Semantic matching no longer spends effort on hops these links already cover.
- 13ddf2b: Every gap now says why it is missing and what would fill it. Each Gap is classified as an unsupported extractor, missing input, unresolved relationship, contradictory or stale observation, or genuine product or requirement gap. It also names the filling seam: an adapter, a registry entry, a manifest input, interpretation, or a requirement. The gap register adds a by-class rollup and per-row class columns, its JSON index records the same, and the Gaps lane shows the class on each gap and cause. Reasons no rule recognizes are labeled unclassified rather than guessed.
- aa74894: Reuse bounded local embedding vectors across semantic previews and app restarts. Model identity and redacted input changes invalidate reuse, while graph membership and evaluation decisions are recomputed for every preview. Clearing the graph or forgetting retained sources deletes cached vectors and invalidates in-flight cache work.
- a429078: Plugin adapters can declare their own coverage cells in their golden corpus. Each supported or partial cell names the golden cases that prove it, and Preflight and Settings show a plugin's cells once its conformance gate passes. A gate verdict now covers the plugin's corpus as well as its artifact, so editing the declared extensions or coverage after gating leaves the plugin ungated. The corpus also enters the recorded coverage basis, so repos ingested with a plugin show their coverage as stale until re-ingested after a corpus change. A corpus can name the language of an extension Cartograph does not know (`languages`). Run the gate once more for plugins gated before this change.
- 5761f31: Facts now record how they were produced, separately from their tier and confidence: a named versioned rule, a model, or a human. Semantic matches and agent proposals name the model behind them, and an accepted agent proposal stays a model's output. The Spec Workbench, tier badges and exported tables show the method, and older facts without one read "not recorded".
- 2439676: Add explicit version 6 specialists that carry recorded analysis coverage through every investigation step. They can read bounded original inventory details, retain the preparation-time coverage for history, and report source-inspection obligations separately. Existing specialist defaults remain unchanged.
  
  Opening an existing investigation store performs a one-way migration to retain coverage archives. Older builds refuse that store after this build has opened it; downgrading does not restore compatibility.
- 27a57ad: Add explicitly selectable V4 specialists with durable turn progress, query continuation, repeated-result indicators, and remaining context-query budget. Existing specialist versions and the default selection remain unchanged.
- 000a721: Vue single-file components are now ingested: the `<script>` and `<script setup>` blocks of `.vue` files are parsed like JavaScript/TypeScript, so their imports, calls and HTTP client calls appear in the graph with evidence pointing into the component. Preflight and Settings list Vue as covered for script content and state that templates are not parsed.

### Patch Changes

- 9bf8d48: Update certificate verification and bundled trust roots to reject distrusted emSign intermediate certificates.
- 5c9b0d3: EventBridge channel gaps can now be filled from OpenTelemetry traces. A span fills the gap only when it comes from the same source file and proves both the bus and the detail type (`aws.eventbridge.detail_type`) as one identity. Otherwise the gap stays, with a reason such as "detail type unobserved", "source attribution missing" or "2 distinct identities". Observed buses are kept as their own channel facts.
- 6b52dd7: Update the Wasmtime plugin runtime to 49.0.4 to fix component stream payload validation and an AArch64 Cranelift branch-table vulnerability.

## 0.15.5

### Patch Changes

- 8a89f16: A Java or Kotlin import proven external now keys its `Module` node and `IMPORTS` edge on the imported package (`mod:jakarta.persistence`) instead of the imported type or static member (`mod:jakarta.persistence.Entity`), so multiple types imported from the same external package share one node instead of one each. The edge still records exactly what was imported. A Kotlin top-level function import with no capitalized package/type boundary (for example `kotlinx.coroutines.launch`) is unaffected and keeps its full path, since there is no boundary to split on. This is a graph-contract change; existing graphs are cleared and rebuilt on the next recovery.
- 3ea5fb6: Java overloaded methods and constructors now get distinct symbol identity (a parameter-signature suffix) instead of collapsing onto one fact, and calls resolve to the matching overload by argument count — scoped to the owning type's fully qualified (package-prefixed) name so two packages' same-named types never share an overload set — failing closed to an explicit Gap when arity alone can't decide. Existing graphs re-key these facts on their next re-ingest (`GRAPH_SCHEMA_VERSION` 5 → 6, sequenced after #527's 4 → 5 bump).

## 0.15.4

### Patch Changes

- 6aef53c: Fix Atlas band assignment: config/build evidence lands in a dedicated Tools/Build band instead of splitting between Server and Unclassified under duplicated cluster names, boundary placeholders are excluded from bands entirely, endpoint clusters are labeled by method and route instead of a trailing-colon id fragment, and collapsed-cluster tiles render their name and count as the tile's own content.
- ccb2a60: Gap register offers escalation only for relations the bounded broker can currently propose, and the Resolution Strategy modal reports an unavailable relation with a user-level reason instead of the broker's raw internal validation error.
- fcc4c4d: Preflight and recovery now report the correct Terraform file count when a module is explicitly referenced from an otherwise-gitignored directory — it's counted, matching what extraction actually reads. A `go.mod` the repository's own `.gitignore` excludes is now treated as absent rather than read anyway; internal Go imports that depend on it fall back to an explicit Gap instead of resolving through an ignored file.
- 66ecb25: Captured TS/JS extraction (the normal recovery path) now honors the "Ingest parallelism" setting instead of always running serially.
- 199a03d: Cap the Spec Workbench's export_spec IPC payload per artifact (content and assertions), truncate the Workbench's artifact preview with a counted note instead of rendering the whole text, and fetch the complete artifact on demand for Copy artifact, Export bundle, and the Gaps & Drift surface.
- 6291bab: Fixed: in a multi-repo workspace, a repo's recorded ingest-history content hash no longer changes when a different repo in the same workspace changes. The recorded hash now scopes to the ingested registration's own facts (a whole-system ingest still hashes every repo it loaded), matching the "same commit under the same source registration" determinism guarantee.

## 0.15.3

### Patch Changes

- 3c87e3a: The exported traceability matrix and source-rule evidence stay readable on large systems. Past 200 links, `US-TM.md` groups links by relation and target, and past 50 observations `rule-evidence.md` groups observations by source file. Each shows a few representatives for at most 50 groups and states every omitted link, observation or group as a counted "N more" line. New `US-TM.json` and `rule-evidence.json` sidecars list every item by group, and each keeps its full provenance in the bundle. Smaller outputs render exactly as before.
- e472481: Bare JS/TS imports that match a literal Vite or webpack `resolve.alias` key are no longer classified as external packages. Cartograph reads the bundler config as data, without running it. When the alias's literal path points at a file in the repository, the import resolves to that file and cites the config. Otherwise the import stays an explicit gap that names the config. Aliases whose key or target isn't a literal (and aliases that map one package name to another) are classified as before.
- dd09b4d: Connect flow: a kept target opens selected so typing replaces it, Enter submits the preflight, and Run full recovery is demoted to a secondary action when the preflight ended without a report.
- ef7c080: The `DEPENDS_ON` edge to an `eval()` code's source-rule Gap now has its own content hash. Before, it reused the Gap node's hash, so two distinct facts shared one. The edge's hash changes once on the next ingest, then stays stable.
- 20b4662: A traced flow is now Partial, not Verified, when a symbol on it runs code T0 could not recover, such as an `eval()` whose argument could not be proven to a literal. The Gap appears as a hop in the flow with its reason. Rule-evidence gaps, which only mark where business-rule evidence stops, still leave a flow Verified.
- 973e15c: Terraform extraction now canonicalizes its root, module ancestors and module sources the same way the app does, without the Windows `\\?\` verbatim prefix, so the two sides compare one path form.
- c934d73: A fixed "Ingest parallelism" worker count saved on a larger machine (or before a CPU quota shrank) is now read back as this machine's maximum, so startup no longer oversubscribes and Settings shows a choice it actually offers. Settings also states that fixed counts stop at 64 workers.
- 709ddef: An `eval()` whose code holds a dynamic nested `eval` (for example `eval("eval(x + 1)")`) now keeps its Unsupported finding in Preflight after recovery. Before, the line was marked covered even though the recovered code still held an unsupported dynamic eval. The facts recovered from the outer code are unchanged.
- 9be4669: A Gap found inside `eval()` code now cites the eval's string argument once, not once per inner location. It also records how many inner locations could not be mapped back into that string (`unmapped_inner_spans`). Before, its evidence listed the same outer span several times.
- fdb5cf4: Cancelling a preflight while it is saving its findings now discards that save. Before, those findings could stay in the register and reappear on the next refresh.
- 8ed8f42: The Preflight surface now always shows the classification the register holds. Before, a preflight that overlapped a recovery of the same repo could leave the surface pending or empty, and a recovery re-run from Jobs → Retry never showed its reconciled report.
- 164defa: A preflight started right after a recovery shows as done is no longer cancelled or overwritten by that recovery's late write of its reconciled findings. Local, GitHub and system-manifest recoveries now take their place in the preflight order before they report completion.
- d4230ba: On Windows, where Cartograph cannot read physical memory, Auto ingest parallelism is now capped at 4 workers instead of one worker per core. Other platforms keep the one-worker-per-2-GiB memory cap.
- 8d91f7a: On Windows, Help links now open with every query parameter intact (the URL goes to the system URL handler instead of `explorer`), and neither the link launcher nor the `gh auth token` step flashes a console window. An OS with no known browser launcher logs the refusal instead of failing to build.

## 0.15.2

### Patch Changes

- 3705692: Facts recovered from `eval()` / `new Function()` code now get a unique content hash per eval site. Before, the same code evaluated at two sites produced distinct CALLS edges that shared one hash. Each recovered fact's hash now comes from its final site-scoped identity. Existing eval-recovered facts get new hashes once on the next ingest, then stay stable.
- 88163db: Go imports into a module that a `go.mod` locally replaces (`replace example.com/shared => ./shared`) now resolve to the repository's own package directory and are shown as Confirmed internal instead of Gaps. An import stays a Gap when its package directory or Go file leaves the repository (including through a symlink), or when the module has a version-qualified `replace` (`replace m v1.0.0 => …`), because the version in use is unknown.
- 170e796: A local recovery cancelled while it reconciles its preflight findings no longer writes them to the register: the reconciled findings are written only once the recovery has completed, as GitHub and system-manifest recoveries already do. A cancelled recovery leaves the pending findings in place.
- e1857d1: Changing the local path while a preflight is still scanning (for example after going Back mid-scan) now stops that scan, as switching the source away from a local tree already did. The abandoned scan no longer keeps running in the background or records findings for the path you left, and its late progress or result can't reach the Preflight screen.
- 3b01e0b: Preflight's Cancel now works while the scan is still listing a large repository, not just once it starts checking files. The listing stops at the next directory and records no findings. While it lists, the status line shows "Finding files… N so far" with the folder being read, rather than "Detecting…" with no progress.

## 0.15.1

### Patch Changes

- 3916aef: The exported Gap register stays readable on large systems. Past 12 gaps, `gap_register.md` groups gaps by cause (stop reason × extractor, largest first) and shows up to 5 representative instances for each of at most 50 classes. Every omitted instance or class is stated as a counted "N more" line. A new `gap_register.json` sidecar lists every instance by class, and each instance keeps its full provenance in the bundle. On a VSCode checkout the Markdown drops from 105 MB to under 100 KB.
- 3d9f084: GitHub and system-manifest recoveries now write their preflight findings to the register, as local recoveries already did. Each recovered repo's dynamic `eval()` / `new Function()` sites, uncovered languages and other unsupported constructs show up as Unsupported findings, reconciled with that recovery's own proof. A failed or cancelled recovery writes nothing.

## 0.15.0

### Minor Changes

- 2624e4c: Recovery now parses source files in parallel, and Settings adds an **Ingest parallelism** control (Auto, or a fixed number of workers). Auto uses one worker per performance core, minus one, and fewer on machines with little memory. Large repositories recover several times faster without using more memory, and the recovered graph is identical whatever the setting.
- 350f986: Imports of external packages no longer appear as system gaps. A clean spring-petclinic ingest previously reported 206 open findings, all of them imports such as `jakarta.persistence` or `org.springframework`. It now reports none. Every placeholder node now records the import or reference that created it and the reason. A package the repository provably cannot provide is recorded as a confirmed external dependency. A package the repository declares is recorded as internal. An in-repo target that fails to resolve stays an explicit gap and names its cause. Java and Kotlin imports of in-repo types now link to the file that declares the type.

### Patch Changes

- 318f9e1: Clear finished now removes the Jobs row of an investigation ended by owner death (interrupted or outcome unknown). The investigation keeps that outcome in its history; the row is never relabelled done or failed, and resumable interrupted jobs are still kept.
- d753b82: A relation cited from several call or import sites now keeps every site's evidence, not just the last one's. The stored edge lists the spans in source order, without duplicates and up to a bound. It states how many spans it omitted beyond that bound. Its tier and confidence are unchanged.
- 09be196: Preflight and recovery now skip everything a repository's own `.gitignore` files exclude. Build output, emitted bundles and vendored code (`build/`, `out/`, `vendor/` and similar) are no longer ingested as source. Every language, Terraform, plugins and Preflight now read the same file set.
- fa07423: Investigations no longer record a completed task without its findings when the result and detail reads straddle the result commit; the inconsistent observation is rejected and the next refresh reads a consistent pair.
- 40bd0a8: Java and Kotlin Spring endpoints now keep a trailing slash written in the source. For example, `@RequestMapping("/api/")` with a bare `@GetMapping` is recorded as `/api/` instead of `/api`, and `/a` and `/a/` are separate endpoints, as they are in Spring 6. Endpoint ids for such routes change on the next ingest.
- 5ac5aa1: A TypeScript/JavaScript `eval()` or `new Function()` whose code argument is a const-shaped binding the adapter cannot prove to a literal now becomes an explicit Gap in the graph, bound to its enclosing symbol (or file) and citing the call and the unproven arguments, instead of appearing only in the preflight report.

## 0.14.4

### Patch Changes

- 3403d43: Preflight no longer runs the full TypeScript parse, which roughly halves the time from connecting a large repository to its first graph. Inline `eval()` and `new Function()` sites are listed as pending until recovery proves them, and recovery then closes, downgrades or keeps each one exactly as before. After a local recovery the Preflight view shows that final classification. Running preflight again lists the sites as pending until the next recovery.

## 0.14.3

### Patch Changes

- 4670421: Preflight no longer freezes the window on large repositories. The scan runs in the background, shows which file it is reading and how far through the repository it is, and can be cancelled. A cancelled scan records no findings.

## 0.14.2

### Patch Changes

- cb6f111: Java Spring mappings written with array syntax (`@GetMapping({ "/vets" })`) now recover their real routes instead of collapsing to `/`. Mappings whose path is a constant or expression are reported as explicit route gaps instead of fabricated Confirmed endpoints.

## 0.14.1

### Patch Changes

- 73efc16: Ingest summaries now quote the distinct facts stored in the graph rather than raw extraction occurrences, so the summary agrees with the Workspace graph counts. Occurrences merged into one fact (one relation cited from several call or import sites, or declarations sharing one id) are stated beside the totals instead of disappearing silently.

## 0.14.0

### Minor Changes

- d1e9c20: Add bounded Domain analyst and Evidence auditor investigations over recovered context, with scoped questions, original evidence citations, per-step cloud consent and durable findings and activity. Generated findings remain inferred; interrupted or uncertain model calls are never automatically replayed.
- 36e2cdf: Bind new agent tasks and staged proposals to their retained parser evidence where available, distinguish unverified working-tree inputs, and expose a read-only assessment of changes since preparation. Existing proposal history remains unchanged and all accepted results still await context reconciliation. Bounded selection and captured-file validation limits are explicit.

## 0.13.0

### Minor Changes

- d2e6949: Preserve live job ownership across app startup and cancellation, fence worker updates to their own attempt, and prevent retries while prior work is still stopping. Earlier jobs without execution ownership records remain historical and require a fresh operation.
  
  Windows job execution requires NTFS for private application storage and verifies file and directory flushes before claiming an attempt. This storage requirement does not restrict the repositories Cartograph can analyze.
- ec089cc: Show cited local constant initializers behind supported TypeScript rule conditions, with structured expressions and explicit unresolved runtime meaning. Preserve historical rule evidence and captured-source receipts without rewriting them.

## 0.12.0

### Minor Changes

- c9bf7b1: Keep similarly named repositories distinct with persistent source identities and host-resolved evidence reads. Isolate managed clones during analysis, preserve historical proposal identities, and require fresh ingestion after the graph identity migration. Legacy path-only jobs remain history and require a new ingest.
- c9bf7b1: Retain primary source for direct TypeScript rule observations, with exact captured inspection and source-specific retention controls. Captured evidence preserves incomplete input coverage; existing proposals remain unverified.

## 0.11.0

### Minor Changes

- ff6a79c: Persist agent proposals before reporting success and resume their review after restart. Reviews bind to an immutable staged result and retain Agentic/InferredWeak confidence. Accepted results await shared-context reconciliation, and current working-tree evidence remains explicitly unverified against its cited revision.
- cc2ac87: Add bounded, revision-bound recovered-context queries for the desktop app and future agent integrations, preserving provenance and explicit pagination with nodes and edges read from one database snapshot.
  
  Reject proposal annotations that replay supplied source spans, summaries or substantial whitespace-normalized excerpts before writing durable records.
- cc2ac87: Recover cited TypeScript guarded returns and throws with correct lexical owners, sanitized source expressions, and explicit dependency and interpretation gaps. Add a source rule evidence inventory to the Spec Workbench and exports. Computed callable names are explicitly omitted and their runtime keys marked unresolved. Nested Nest controller endpoints retain their actual lexical handler identities, and malformed guarded exits produce explicit omission gaps. These observations preserve local syntax without claiming complete business-rule recovery.
  
  Repositories require re-ingestion after upgrade: the disposable graph cache is invalidated so old ambiguous ownership links cannot remain visible.

### Patch Changes

- e95854e: Keep Atlas, specification, flow, topology, findings, metrics and semantic-preview graph reads consistent when another application process writes to the graph. Each projection now reads one database revision while preserving its existing label selection; graph counts also come from one coherent query.

## 0.10.3

### Patch Changes

- ee6fa29: The WASM plugin host now runs on Wasmtime 48.0.1 (`wasmtime` and
  `wasmtime-wasi` bumped together from 47.0.3), which carries the upstream
  fixes for RUSTSEC-2026-0268 (guest-controlled host heap allocation via
  WASIp3 streams) and RUSTSEC-2026-0269 (filesystem sandbox escape with
  trailing slashes in paths/symlinks). Third-party notices were regenerated
  for the moved crates.

## 0.10.2

### Patch Changes

- 79ea14d: Third-party notices are now generated from `ui/package-lock.json` instead of
  `npm ls`, so the npm inventory is stable across npm majors and environment
  noise. A required CI gate regenerates the notices in memory and fails when
  the committed copies drift, and the shipped notices file was refreshed for
  packages whose pinned versions had moved (cytoscape, material-symbols,
  zustand).

## 0.10.1

### Patch Changes

- 55043dc: Unresolved calls are no longer double-counted in the findings register: a gap edge that touches its gap node supports that node's finding (one finding per unresolved call, mirroring how drift counts), while an edge-only gap with no gap node on either end stays a finding of its own. The headline count, the Spec Workbench gap chip, and the gap lane all reconcile through one shared definition. Gap edges also carry the same `reason` as their gap node, so no register row renders a bare edge identity in the reason column.
- d4cc91f: The Workspace outcome badge now states the confidence tier the recovered facts actually carry: a fully deterministic recovery with open gaps reads "Confirmed overall" instead of "InferredStrong overall" (gaps are explicit unresolved markers, not inferred assertions), and a recovery containing InferredWeak facts is no longer overstated as InferredStrong.
- ced6452: Windows path canonicalization no longer leaks `\\?\` extended-length prefixes into stored graph facts: every ingest/evidence canonicalize call now goes through one normalization helper (dunce), so the same commit hashes identically across platforms and evidence lookups compare consistent path forms.

## 0.10.0

### Minor Changes

- b260d5b: Kotlin joins the deterministic tier (#212): a new compiled-in
  `adapters-lang-kotlin` crate recovers Confirmed T0 facts from `.kt`/`.kts`
  sources — classes, interfaces, objects, data classes, enums, and functions
  (top-level, member, and extension) with exact evidence spans; import-proven
  cross-file calls (type/object receivers and imported top-level functions)
  joined repo-wide with declared-package misses failing closed to explicit
  Gaps; and Spring Web endpoints proven per annotation package with
  class+method path composition. Preflight and Settings now report Kotlin as
  installed (`t0.adapter-kotlin`) instead of a requestable planned adapter,
  the ingest summary reports a Kotlin layer row, and live recovery progress
  narrates Kotlin file reads. Ktor routing is a follow-on.
- aba65e1: Literal eval() and new Function() sites give up their facts instead of
  dead-ending: when the code argument is compile-time-known (a string
  literal, a substitution-free template, or a same-file const proven by
  binding), the TS adapter parses the string with the same extractor and
  emits its Symbols, CALLS, and sites as Confirmed T0 — marked `via:
"eval"`, cited at the argument's span at the eval site — and the
  enclosing symbol calls into the extracted code so flows cross the eval
  boundary. Preflight's inline-eval finding now reconciles with that
  proof: fully proven sites are covered and close on the next scan,
  const-shaped-but-unproven arguments downgrade to explicit potential
  Gaps, and everything dynamic stays an Unsupported finding — never a
  guess.
- 4e023c4: Imports now resolve the way Node and TypeScript actually resolve them: directory index files (`./utils` → `utils/index.ts`), the NodeNext `.js`-extension idiom (`import './foo.js'` → the `foo.ts` source), tsconfig/jsconfig `paths` and `baseUrl` aliases, and workspace-package names via their `exports` maps — each resolution proven against real files and citing the deciding config file as evidence. Unresolvable specifiers stay explicitly unresolved, and external packages are never guessed into.
- d17697e: Config files are now first-class evidence: package.json, tsconfig/jsconfig, lockfiles, Dockerfiles/compose files, GitHub workflows, and language manifests become cited Tool facts in the graph — with resolved settings, click-to-source proof spans, a toolchain section in the spec export, and fail-closed redaction (.env files contribute key names only; secret-shaped values are never stored).

## 0.9.0

### Minor Changes

- b48ae60: Conformance gate for WASM adapter plugins (#200): a durable `plugin-gate` job
  proves the SPI contract under the standard bounds, the plugin's own golden
  corpus (expected facts pinned with the host identity), and double-run
  determinism. The verdict persists per (plugin id, content hash) — replaced
  bytes are ungated again — and Settings shows a per-artifact gate chip with the
  failing check named, plus a run-gate action. A failed or ungated plugin stays
  proposed and never joins extraction.
- b29c16c: The unsupported lane resolves (#201): Preflight's uncovered-language findings
  carry a request-adapter action; a plugin that lands in a discovery directory,
  passes the conformance gate, and is enabled for the project counts as
  coverage on the next scan — the originating finding closes — and extraction
  routes the files its golden corpus claims through the plugin with host-pinned
  facts. Compiled-in adapters always win a contested extension, routing is
  deterministic and all-or-nothing per plugin, and bytes swapped after gating
  fail the ingest closed. Manifest-ingested repos now register as discovery
  roots too.

## 0.8.0

### Minor Changes

- ced9d60: Adapter plugins are now discoverable and switchable per project: Cartograph scans the project's .cartograph/adapters/ and a user-level adapters directory (the project copy wins on id conflict, shadowing stated), keys every artifact by its content hash, and lists them in Settings with a per-project enable/disable that fails closed — a plugin is off until you turn it on, and it only ever extracts facts behind the conformance gate.
- 3966468: Plugin facts are pinned to the exact artifact that produced them: the host stamps every fact's provenance with `{plugin-id}@{hash}` and the full BLAKE3 artifact hash, overwriting anything the guest wrote — a plugin can never impersonate a built-in extractor, a rebuilt artifact is a different extractor identity, and repeat runs of the same artifact are provably identical.

## 0.7.0

### Minor Changes

- 68190e2: Settings now answers "what can Cartograph read?": an Adapters section lists every installed adapter with what it extracts and which files it claims — the same registry Preflight consults, so the two can never disagree — explains in plain language what an adapter is, and lists known adapter types not yet installed (JavaScript, C, C++, Kotlin, Swift, Objective-C) with a request link. Preflight findings for those languages now name the missing adapter instead of dead-ending.
- 619c113: Atlas gains a keyboard-first focus mode: select a node and press Enter (or use the breadcrumb control) to re-root the view on that node and its direct connections, with the layout reflowing deterministically. Each focus stacks a navigable level; Esc or the breadcrumb backs out one level at a time to the full graph, and every transition is announced for screen readers.
- bbc41fc: Gap triage now works at the class level end to end: a whole cause class escalates locally in one durable, cancellable job — one staged proposal per instance, failures recorded without aborting — with per-instance accept/reject through the standard decision records (nothing joins the graph unaccepted). Classes that truncate traced flows rank first, in the cause lane and the by-tier tab alike, so the top of the register is the highest-value hour of triage. Cloud escalation stays per-instance: a consent grant binds to one exact payload.
- b59f5e3: Help now lives in the app: a native Help menu (Cartograph Help, the wiki user guide and issue reporting in the system browser, About with the version), an in-app Help view with a topic for every surface plus a concepts page — rendered offline from bundled markdown, reachable from the menu, the command palette, the header's Help action on every surface, or ? / F1 — and single-sourced content: docs/help/ mirrors to the wiki with a CI gate that fails when they drift.
- 13eba95: The app now answers its own jargon in place: small keyboard-accessible "?" affordances explain System gaps, unsupported patterns, no-evidence questions, confidence tiers, verified-only vs best-effort, and recovery authority right where those terms appear — one interaction to open, Esc to dismiss, with a Learn-more link. All notes come from one shared source, so no surface can drift from another.

## 0.6.0

### Minor Changes

- a748749: Multi-ingest stacking is now stated, not silent: the Workspace lists every repo currently merged into the system (from the graph's own facts) and says that new ingests merge in. The destructive action speaks your language — "Clear system" — and its confirmation names exactly what is removed (every recovered fact, listed by repo) and what survives (job history and settings).

## 0.5.0

### Minor Changes

- de05df5: Flow Inspector now traces real targets beyond web apps: extension contexts (popup, background, content scripts) and manifest keyboard commands anchor their own flows, walking from the manifest entry into the entry file's symbols and across message channels. When zero flows trace, the Inspector names every anchor kind recovery looked for and what it found instead of a generic hint.
- 3c09551: The gap register triages by cause at scale: past a screenful, the System-gaps lane groups gaps into cause classes (stop reason × extractor) ranked by instance count — thousands of gaps read as a handful of causes. Each class expands on demand to paged instance rows that stay responsive, and every instance keeps its Resolution Strategy path.

## 0.4.0

### Minor Changes

- 7b127a2: Atlas is readable at real-repo scale: nodes now lay out deterministically in labeled architecture bands (Infrastructure → Cloud → Server → Events → Client) instead of an undifferentiated force grid, clustered within each band by repo/module. Past 200 nodes the initial view renders collapsed clusters with aggregated relation counts — tap a cluster (or its index chip) to expand it in place, with Expand-all/Collapse controls — so a 14k-node graph opens as a handful of labeled boxes, and identical snapshots always produce the identical layout.

## 0.3.0

### Minor Changes

- 49aebb6: Java language adapter (T0): classes, interfaces, enums, records, and methods become Confirmed Symbols with exact evidence spans; same-class and import-proven cross-file calls join the graph deterministically (declared-package misses fail closed to explicit Gaps); annotation-proven Spring Web mappings become Endpoints with class+method path composition. Preflight now detects Java as covered (and Kotlin as its own uncovered language), and the ingest summary reports a Java layer row.
- 638126d: Jobs surface hygiene: the dev-only "Enqueue test job" control is gone from the production UI (with its `enqueue_job` command), and a confirm-gated **Clear finished** action removes done/failed/cancelled jobs from the durable spine while queued, running, and interrupted (resumable) work is always kept.

### Patch Changes

- 5d45f56: Recovery no longer freezes the app: ingest, add-repo, add-system, and ingest retry/resume commands now run their extraction on a blocking worker thread instead of the webview/main thread, so a large repository recovers with the UI fully interactive (no more macOS beachball).

## 0.2.0

### Minor Changes

- ca35dc5: Anthropic Claude API provider: the first cloud reasoning lane behind
  the fail-closed egress firewall, with three pinned model lanes (Haiku
  for triage, Opus as the default T3 reasoning lane, Fable opt-in for the
  hardest escalations with server-side refusal fallback to Opus).
  Embeddings remain local-only; consent disclosures carry provider,
  model, endpoint, pricing, and the Fable retention requirement; safety
  refusals are a typed outcome.
- 327b4de: Atlas canvas v2: node shape now encodes kind independently of color —
  octagon with a dashed red border for Gaps (no longer colliding with
  the gateway diamond), diamonds for gateways/channels, rounded
  rectangles for the rest. Every edge carries a clickable mono
  tier+relation chip (T0 HANDLES, GAP …) on the canvas and in a
  visible-relations index, opening the evidence drawer for the edge
  exactly like a node; the layer filter drives the header scope chip
  (Atlas · <layer>); and a single click keeps selecting nodes and edges
  while the drawer is open.
- 23da0fb: Chrome runtime messaging channels (US-0016, #99): `chrome.runtime.sendMessage`
  / `chrome.tabs.sendMessage` producer sites and explicit handler registrations
  (`[MessageType.X]: handler` dispatch tables behind a real
  `onMessage.addListener`) stitch into deterministic `chrome-message` channels
  with PUBLISHES/SUBSCRIBES edges connecting extension contexts. Message
  identities resolve through string literals, repo-wide const-string maps, and
  one-hop creator functions; anything runtime-computed stays an explicit Gap,
  and shadowed `chrome` bindings or conflicting const maps fail closed.
- cb0d4a6: Gap-escalation orchestration: gap_strategies derives strategy cards
  from real provenance (attempted tiers, stop reason, required evidence
  citations, a closed candidate set from the k-hop context) with exact
  redacted-payload egress estimates; escalation_preview returns the
  exact payload a cloud run would send for the one-action consent
  dialog; run_escalation executes as a durable job — local SLM
  immediately, cloud only with standing consent plus a per-payload
  grant whose hash matches the preview — records egress bytes, and
  returns a staged proposal that never joins the graph.
- 35d1968: Evidence drawer v2: resizable (320-560px) with a click-through overlay
  wrapper, Esc to close, true file line numbers (windowed reads now
  report their real starting line), the full span range including end
  line:col, why-this-is-a-Gap/Inferred strips with a collapsible
  why-this-tier explanation, a provenance table with the complete
  copyable BLAKE3 content hash, navigable supporting-evidence spans, a
  Resolution Strategy CTA on Gaps, and the R-INT-1 integrity footer.
- ca0c09c: Flow Inspector surface (#107): header with flow id, named gap count, trigger
  summary and score; verified-only/best-effort projection with an explicit
  hidden-hop count, annotated excluded cards, and the Gap node always retained
  (R-INT-4); wrapping hop-card layout with Fit/zoom that never scrolls
  horizontally; Gap hops open the Resolution Strategy modal and every other hop
  opens the evidence drawer from its own provenance.
- 1a5db62: Gaps & Drift register surface: the product's core honesty claim gets
  its own surface. Three lanes that never conflate — System Gaps (with
  next-escalation-tier tails and a resolution seam), unsupported
  patterns (tool limitations, explicitly not gaps), and no-evidence
  findings — plus a by-escalation-tier grouping and a Drift tab, all
  wired from the spec compiler's own registers and reconciled with the
  Workspace tally by construction.
- d3dd51c: Knowledge-graph context assembly for the escalation tiers: deterministic
  k-hop subgraph extraction around any focus, citation-bound serialization
  (every statement carries the graph identity it came from and unknown
  provenance is never presented as confirmed), hybrid structural+similarity
  candidate merging, and budgeted packing that records what was dropped —
  never silent truncation.
- 9365e7a: Adopt the production UI design tokens: dark IDE palette from the design
  handoff (app/rail/surface tiers, confidence-tier and register-finding
  colors, radius/shadow/motion scales) and locally bundled Inter, JetBrains
  Mono, and Material Symbols fonts — no runtime font fetch.
- 5f55359: IndexedDB data model (US-0016, #99): explicit `createObjectStore`
  declarations and repository operations (`tx.objectStore(X).put(…)`, bound
  store handles, `.index(…)` chains) become cited `DataEntity` nodes with
  symbol-attributed READS/WRITES relations. Store identities resolve through
  string literals or repo-wide const-string maps; runtime-computed identities
  stay explicit Gaps.
- 1fdbe1d: Connect → Preflight → Recover ingest flow: the bare path-input card is
  replaced by the handoff's three-step flow. Connect picks a GitHub repo,
  local folder, or system manifest behind a local-only reassurance strip;
  Preflight runs on-device detection (0 bytes egress) and renders the
  three-way classification — potential system gaps and unsupported
  patterns as separate cards that never conflate; Recover streams real
  core job progress with a Run-in-background hand-off to the Jobs
  surface.
- 7c6e910: Job spine v2: jobs carry stage, percent progress, failure detail, and
  artifact links; new cancel/retry commands cover the full lifecycle
  (cancel is cooperative at stage boundaries, retry also resumes jobs
  interrupted by an app restart); ingest runs as a staged job emitting
  live `job://changed` events. Existing state-spine databases migrate in
  place.
- 2b10773: Jobs surface: durable job management in the shell — every lifecycle
  state with live progress and stage, cancel/retry/resume actions,
  failure detail, artifact links, and job://changed event subscription;
  degrades gracefully against a pre-v2 core.
- 9cbf4ed: Managed local SLM tier: a versioned model catalog pins every
  LLM-touching action (embedding, triage, proposal) to a local model so
  provenance attributes work to an exact mapping; Ollama health probing
  reports missing models and unreachable endpoints as explicit,
  remediable states — never a silent failure and never a cloud fallback;
  and local completions validate against the caller's schema with a
  bounded retry.
- 454938c: Preflight detection and the register finding model: a local-only
  `preflight` command detects languages, frameworks, and adapter coverage
  and classifies constructs into the three-way split — potential system
  gaps vs unsupported patterns — before recovery runs; unsupported and
  no-evidence findings persist on the state spine distinctly from Gaps,
  and a `findings_summary` command serves every surface the same
  gap/unsupported/no-evidence/drift tallies from one definition.
- 0e13866: Provenance & Eval surface: tier distribution with a fully described
  stacked bar (register findings kept distinct from graph facts),
  per-extractor coverage bars (not-in-scope reads n/a, never 0%),
  paired-eval quality-gate cards with pass/below-floor states, and
  evidence health over re-ingests — the determinism footer claims
  verification only when the history really contains same-commit
  records with equal content hashes. Every chart is screen-reader
  complete; nothing encodes by color alone.
- 21952be: Recovery metrics: every completed ingest now persists a history record
  — tier tallies counted with the register's own provenance definition,
  unsupported/no-evidence counts, per-extractor coverage, and an
  order-independent whole-graph content hash — so re-ingesting the same
  commit shows identical hashes in queryable history and the determinism
  invariant becomes observable data. New ingest_history and
  extractor_coverage commands feed the Provenance & Eval surface.
- 493acf6: Resolution Strategy modal: one component for every gap — escalation
  ladder with the R-INT-1 statement, why deterministic recovery stopped,
  required-evidence citations, and runnable strategy cards with egress,
  cost, latency, privacy, and export impact. Local runs immediately;
  cloud opens the exact-payload one-action consent dialog first and only
  runs with the approved hash. Results are staged proposals with
  Accept/Reject through the durable decision records — they never
  auto-join the spec. Gap rows in the register and the evidence drawer's
  CTA now open the modal for real.
- ba1a5cc: Persistent tier settings and standing revocable cloud consent: per-tier
  enable/provider configuration on the state spine (T0 is not
  configurable — always on, never an LLM), consent that only permits
  cloud on top of the firewall's per-payload grant, immediate
  revocation (including when switching a tier back to local), an
  observed-egress log, and a live egress summary for the status bar.
- e32650e: Settings surface: recovery-tier cards with T0 locked always-on, per-tier
  enable toggles persisted in the core, Local/Cloud provider selection for
  T2/T3, and the fail-closed cloud consent flow — the full disclosure
  (provider, model, endpoint, pricing, caveats) renders before consent is
  recordable, consent is revocable with immediate effect, and the status
  bar's egress line now derives live from settings state.
- bc3d2b9: IDE app shell: left nav rail with eight surfaces, breadcrumb header with
  scope chip and Legend popover, status bar with live state and egress
  summary, thin global progress bar, ⌘K command palette with ⌘1–⌘8
  shortcuts, and a route-level error boundary. Existing dashboards moved
  under their surfaces (Workspace, Atlas, Flows, Spec Workbench, Jobs);
  Gaps & Drift, Provenance & Eval, and Settings show honest interim states.
- fa58d19: Spec Workbench polish (#108): Confirmed (T0/T1) assertion blocks now show
  their Accept/Reject/Annotate controls visibly locked — disabled plus
  aria-disabled, with an inline "Confirmed T0 — locked, read-only" explanation
  and a document-level R-INT-1 note — instead of hiding them; doc-list entries
  carry per-document count chips (US/AC/flows/ADR, alert tone for non-empty
  registers). T2/T3 curation and evidence preservation are unchanged.
- 703f531: WebExtension manifest adapter (US-0016, #99): Manifest V2/V3 `manifest.json`
  files become deterministic T0 topology — Extension, ExtensionContext
  (service worker / background scripts, content scripts, pages, toolbar
  action), and Command nodes with DECLARES/ENTRY edges — plus exact-scope
  GRANTS security facts that surface wildcard host permissions in the security
  projection. Declared entry files bind to their `.ts` sources when the built
  `.js` is absent; a missing entry is an explicit Gap. The ingest summary and
  extractor coverage report the WebExtension layer separately.
- 282cee4: Workspace landing: the post-recovery report replaces the bare card
  grid. An outcome card states the overall tier and the honest findings
  tally (gaps, unsupported patterns, and no-evidence listed explicitly,
  never guessed) with CTAs into Gaps & Drift and Provenance & Eval; five
  provenance-health cards summarize the tier distribution from the same
  register summary every surface shares; and the artifacts grid carries
  independent generation and recovery-authority badges, with the gap
  register showing a single open-findings badge.

## 0.1.0

### Minor Changes

- 01082bc: Ship signed, notarized universal macOS app and DMG assets through versioned GitHub Releases.
