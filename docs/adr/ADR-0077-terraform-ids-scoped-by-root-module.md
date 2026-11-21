# ADR-0077 — Terraform resource ids scoped by root module

- **Status:** Accepted
- **Date:** 2026-11-21
- **Deciders:** Cartograph owner's delegate (project coordinator); Claude implements
- **Related:** #1844; #1791; ADR-0070 (addendum, which this decides); AC-0009, AC-0048, AC-0689, AC-1500, AC-1501

## Context

A Terraform resource id was `res:{repo}@{address}`. The directory walk read
every `.tf` file with no root-module scope, so `envs/prod/main.tf` and
`envs/staging/main.tf` that both declare `aws_sqs_queue.orders` produced one
id. The store upserts on id and kept one declaration. AC-0689 made the loss
visible with a `gap:tf-address-collision:` Gap but did not stop it, and D7
§7.2 cannot list a Terraform environment's resources while it happens.

The ADR-0070 addendum set out three options: keep the Gap, scope every id,
or scope ids by root module with the repository root unprefixed. This ADR
picks one, chosen to be the safest for projects that already hold a graph.

## Decision

1. **Ids carry the root module, and the repository root has none.** A
   Terraform fact's id is `res:{repo}@{rootdir}/{address}`, where
   `{rootdir}` is the repo-relative directory of the root module with `/`
   separators. Terraform at the repository root keeps
   `res:{repo}@{address}`, so a repository whose Terraform lives only at its
   root keeps identical ids, nodes and graph hash.
2. **Local modules inherit the caller's scope.** A local module's resources
   are instantiated under the scope of the root module that calls it, after
   the `module.<name>.` prefix (AC-0048). Two root modules calling the same
   module are two instances.
3. **Direct reads of child modules are scoped by their own directory.** The
   walk also reads a module source directory directly. Those nodes are
   scoped by that directory and carry `child_module: "<dir>"` instead of
   `root_module`, so they can collide with nothing and are never treated as
   a root module.
4. **Edges stay inside their root module.** Terraform references cannot
   cross root modules, so every `DEPENDS_ON`, `REFERENCES`, `GRANTS`
   (including the policy-document chase) and Capability Registry edge target,
   and every placeholder, takes the scope of the block that names it. The
   per-file cache key includes the scope.
5. **Scope is a prop, not something to parse.** Nodes outside the repository
   root carry `root_module` or `child_module`; the repository root carries
   neither and the prop never holds `"."` or `""`. Only the `iac` crate
   splits a scope out of an id. Observed (`res:{repo}@observed:…`) and Pulumi
   ids keep their shapes and may contain `/`.
6. **`state_json` keeps its meaning, with a safe fallback.** The one
   `state_json` per repository is the repository-root module's state and
   matches nodes with neither prop, exactly as before. When it matches none
   of those and the repository has exactly one root module elsewhere (a lone
   `infra/` or `terraform/` directory, the common layout), it is that
   module's state, so existing projects keep their observed values. With two
   or more such root modules nothing says which one it belongs to: nothing
   is applied and a T1 Gap `gap:tf-state-root:{repo}@` names the root
   modules and cites the state file. Declaring one state per root module in
   the manifest is a later increment.
7. **The collision Gap is retired.** Scoped ids cannot collide, so
   `gap:tf-address-collision:` is no longer emitted.
8. **Labels.** `logical_id` stays the Terraform address. The topology label
   reads `{rootdir}: {address}` when a scope prop is present.

## Consequences

- Repositories with Terraform only at the root (and no local modules read
  from subdirectories) are unchanged.
- Repositories with Terraform in subdirectories get new ids for those
  resources and their edges once. Accepted drafts and citations that name an
  old id go stale through the normal stale-citation path; nothing is
  migrated in place. Directly read child modules, which used to share ids
  with root-module resources of the same address, become their own nodes.
- A repository with one root module in a subdirectory and a `state_json`
  keeps its observed values (decision 6).
- A repository with several root modules and one `state_json` loses the
  observed values it used to get by address. Those values were already
  unreliable, because the address matched whichever declaration survived the
  upsert; the Gap says why.
- This is a fact-identity and graph-hash change: its PR carries the
  `owner-review` label.

## Alternatives

1. **Keep the Gap only.** No identity change, but one declaration per
   collision stays lost and D7 §7.2 cannot describe Terraform environments.
2. **Scope every id, including the root (`./{address}`).** One uniform rule,
   but every Terraform project's ids and hash change for no gain where
   nothing collides.
3. **Scope only addresses that collide.** Fewer id changes today, but adding
   a second environment would silently rename the first one's resources, so
   identity would not be stable.
