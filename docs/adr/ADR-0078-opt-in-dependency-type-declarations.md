# ADR-0078 — Dependency type declarations are read only for allowlisted packages

- **Status:** Accepted (coordinator as the owner's delegate, 2026-11-21)
- **Date:** 2026-11-21
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #995; US-0002 / AC-1504, AC-1506; ADR-0075 (allowlisted env value reads); ADR-0003

## Context

The TypeScript adapter never reads `node_modules`. A call into a dependency
(`client.send(...)`, `S3.putObject(...)`, `useQuery(...)`) produces no edge:
the bare import becomes an external `mod:` boundary and the call through it is
dropped. On a dependency-heavy service (an SDK-client-centric backend) those
calls carry most of the business behavior, so flows and rule prerequisites go
quiet exactly where the system talks to the outside world.

The #995 measurement showed two things. On this repository's own UI a reader
closes no Gaps, because package calls are never Gaps. And a prototype that
emitted a Gap for every typed dependency call it could not prove added about
480 new Gaps, which made the register noisier without adding evidence. So the
value is in proving the calls that *can* be proven, as external boundaries,
and staying silent about the rest, exactly as today.

`node_modules` is not part of a commit. It is installed state, often large,
and its contents are third-party code the author did not choose to share.
Reading it is a disclosure and reproducibility decision, so it belongs to the
author, package by package, as ADR-0075 decided for env values.

## Decision

1. **Allowlist, exact package names.** Declarations are read only for the
   packages listed in `.cartograph/types.toml` at a repository's root:
   `read = ["@aws-sdk/client-s3", "stripe"]`. Entries are exact npm package
   names (`name` or `@scope/name`). A glob, a subpath, a duplicate or any
   other shape refuses the configuration, and so does a symlinked,
   non-regular or oversized file. No file means an empty list. With an empty
   list the adapter opens nothing under `node_modules`, and the facts are
   byte-identical to an ingest without this feature.
2. **Type declarations only.** For a listed package the reader opens the
   package's `package.json` and declaration files (`.d.ts`, `.d.mts`,
   `.d.cts`), never implementation code. It finds the package the way Node
   does, from the importing manifest's directory up to the repository root,
   and falls back to `@types/<name>` when the package ships no types. The
   entry is `types`, `typings`, or the `types` condition of the `.` export,
   else `index.d.ts`. Relative `export * from` and `export { … } from` inside
   the package are followed. Every opened path must resolve, after symlinks
   (pnpm links are fine), inside the repository root. The reader is bounded
   at 64 declaration files and 8 MiB per package. Past a bound, or on any
   failure, the package proves nothing.
3. **What is proven.** A call `name()` on a named import, or `ns.name()` on a
   namespace import, from a listed package's root specifier is proven when
   the package's declarations export `name` as a function, class, variable
   or namespace. The proven call becomes a `CALLS` edge from the calling
   symbol to the package's existing `mod:` boundary, with
   `resolution: "dependency-types"`, the `member`, the `declaration` kind,
   the package-relative declaration file and the installed `package_version`.
   Its evidence is the call site plus the dependency's declaring line in the
   importer's `package.json`. A package the importer's manifest does not
   declare proves nothing.
4. **No new Gaps.** A call the declarations do not prove (a default import, a
   subpath specifier, `export =`, a missing or ambiguous member, a package
   past its bounds or not installed) stays omitted, as it is today. The
   reader only adds proof; it never adds a Gap.
5. **Reported, not silent.** The extraction reports how many listed packages
   were read, which were unavailable and why, and how many calls were
   proven. A commit snapshot counts `.cartograph/types.toml` as an input,
   like `.cartograph/env.toml`, so an untracked allowlist marks the
   snapshot dirty.

## Consequences

- Repositories without `.cartograph/types.toml` are unaffected.
- With an allowlist, dependency calls appear in flows and call
  neighborhoods as external boundaries that name the member called.
- The proof depends on installed state. The edge records the package
  version it was proven against, and its repository evidence is the call
  site and the manifest line, both in the commit.
- Default imports, subpath exports, `export =` packages and typed receivers
  (`const c = new Client(); c.send()`) are not proven in this slice. Each is
  a later, separate extension of decision 3.

## Alternatives

1. **Read every direct dependency's types by default.** It reads third-party
   state the author never chose, makes default ingest depend on installed
   state, and was measured to add little on repositories without SDK-heavy
   code.
2. **Emit a Gap for each unproven dependency call.** Measured in #995: about
   480 new Gaps on the UI alone, without new evidence.
3. **Fetch declarations from the registry.** Network access during ingest,
   and the version fetched may not be the one installed.
