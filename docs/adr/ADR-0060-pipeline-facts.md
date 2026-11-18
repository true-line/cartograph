# ADR-0060 — Build and deployment pipelines as ordered facts

- **Status:** Proposed; owner review
- **Date:** 2026-11-12
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1790; #1745; US-0019 / AC-0598; `docs/DOCUMENT-CATALOGUE.md` D7 §7.5

## Context

D7 §7.5, "How to deploy", is an agent-authored section. An authored sentence
must cite a fact. The toolchain reader records that a workflow or a Dockerfile
exists and which tools it names, but not the order of anything: which jobs
run, what each needs, which steps run in which order, which image stages build
on which. There was no ordered-step schema to extend; `STEP_OF` is a
domain-layer trace relation.

## Decision

1. **Four node labels for both formats:** `Pipeline` (one workflow file or one
   Dockerfile), `PipelineTrigger`, `PipelineJob` (a workflow job or a
   Dockerfile stage) and `PipelineStep` (a workflow step or a Dockerfile
   instruction). Ids are derived from repository, path and the declared job
   id or position.
2. **Two edge labels:** `PART_OF` (trigger or job to pipeline, step to job),
   carrying the declared position as `index`; and `NEEDS` (job to job),
   carrying how the dependency was declared (`needs`, `FROM <stage>`,
   `COPY --from`, and since #1818 `RUN --mount=…,from=<stage>`). Order is always a stored property, never store order. A
   job's `depth` is its longest resolved dependency chain.
3. **No existing edge label is reused.** `DEFINED_IN` is walked by the flow
   tracer, `TRIGGERS` and `DEPENDS_ON` are drawn on the topology map, and
   `STEP_OF` belongs to the domain layer; reusing any of them would add hops
   or nodes to existing flows and documents.
4. **Declared, not evaluated.** A matrix is one job with its matrix as
   written. A reusable workflow is a reference. Expressions and variables stay
   as written. Nothing is executed or simulated, and third-party actions are
   not resolved.
5. **Exact or nothing.** A file the reader cannot read exactly (a YAML alias,
   anchor or tag, a parse error, a dependency cycle, a Dockerfile heredoc or
   `escape` directive) yields no facts and is listed in coverage as present
   and not read. A `needs` naming a job that does not exist is an explicit Gap
   (R-INT-4), and no depth is invented above it.
6. **Names of secrets only.** A `secrets.NAME` reference is stored as the
   name. `with:` values and `env:` maps are not stored; `ENV` and `ARG` keep
   names only. Command and instruction text passes the existing fail-closed
   redaction before it is stored.
7. **Other CI systems are named, not read.** GitLab CI, Jenkins, CircleCI and
   Azure Pipelines files are listed in coverage as present and not read.
8. **Every fact is T0 Deterministic and Confirmed** with the exact byte span
   of the job, step, trigger or instruction it states.

## Consequences

- An authored deployment section can cite jobs, steps and stages in order.
  The projection that lists them per pipeline is `pipelines.md` (#1818).
- Re-ingesting a repository with workflows or Dockerfiles adds facts, so its
  graph content hash changes once.
- Redaction fails closed: a path or identifier of forty or more token
  characters inside a command is withheld like a token would be.
- Dockerfiles using heredocs, and workflows using YAML anchors, are reported
  as not read until the reader covers them. `Containerfile` and
  `*.dockerfile` names are read since #1818.
- A repository with an unread CI system or an unreadable pipeline file has a
  limited coverage inventory.

## Alternatives

1. **Extend `Tool` facts with an ordered list property.** No new labels, but
   a step would have no identity of its own to cite and no span.
2. **Reuse `STEP_OF`, `TRIGGERS` and `DEPENDS_ON`.** Fewer labels, but
   pipeline steps would appear in domain traces, flows and the topology map.
3. **Evaluate matrices and expressions.** Closer to what runs, but it is a
   simulation, not a reading of what the file declares.
