# ADR-0080 — Manifest observation inputs are captured under their own identity

- **Status:** Accepted (coordinator as the owner's delegate, 2026-11-21)
- **Date:** 2026-11-21
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #385 acceptance 6 and "manifest state/trace evidence"; US-0028 / AC-1041; ADR-0024; ADR-0046; ADR-0072; ADR-0077

## Context

A system manifest may declare, per repository, observation inputs that the T1
producers read: Terraform `state_json`, Pulumi `pulumi_json` and OTLP
`otel_jsonl` trace files, each relative to the manifest. Until now they were
read straight from disk, and their observed evidence (`observed_prov`, and the
`prov` of a placeholder that state resolved, of a state-root Gap, or of a
channel a trace filled) cited the joined absolute path with an empty `repo`
and `commit_sha`.

That evidence was never upgraded: task evidence could not resolve an empty
repo, so it was dropped as a read that was unavailable. But it was not
explicitly unverified either. The absolute path also named the author's local
folder layout. #385 asks that manifest-supplied state and traces get their own
source and capture identities rather than borrowing a repository commit, or
stay explicitly unverified.

## Decision

1. **An input capture per repository entry.** The declared observation inputs
   of one manifest entry that lie inside the manifest folder are captured
   together: a working-tree capture rooted at the manifest folder, selecting
   only those declared relative paths, under the source identity
   `input/<registered source id>`. It never shares the repository's capture or
   commit.
2. **Parsers read the captured bytes.** The state, Pulumi and trace parsers
   read the captured bytes, which must be UTF-8, rather than reopening the
   file. A capture that does not hold a declared file is a failure, not a
   fallback. How state is matched to a root module (ADR-0077) is unchanged.
3. **Evidence cites the input identity.** Observed evidence from a captured
   input cites `repo = input/<source id>`, the declared relative path (a
   leading `./` removed) and `commit_sha = workdir`. Graph-wide consumers that
   enumerate source registrations from evidence (graph basis, system
   contents) skip the `input/` namespace.
4. **What cannot be captured stays explicitly unverified.** A declared path
   that escapes the manifest folder (`..`, absolute), or a file the capture
   limits refuse (over 16 MiB, a symlink), is still read from disk. Its
   evidence keeps the uncaptured form, and the producer's coverage reason
   names it: "not captured, so unverified (outside the manifest folder or past
   the capture limits)". When the inputs cannot all be captured together, the
   subset that fits the capture limits is kept, path by path in order.
5. **Input captures are retained with their source (AC-1043).** A published
   recovery retains each repository's input capture in the capture store
   under `input/<source id>`, beside that source's own captures. The
   retained-source preview counts it and binds it in its fingerprint, and
   forgetting the source removes it. A capture that cannot be retained fails
   the recovery, as the source's own does.
6. **Receipts come next.** Minting receipts for observed facts, and binding
   them, is a following decision (ADR-0079). Until then no observed fact is
   verified, so curated context cannot activate on it.

## Consequences

- Observed evidence no longer names the author's absolute folder layout when
  the input lies inside the manifest folder.
- Parsing and citation see the same bytes, the property #385 requires of
  repository source.
- Coverage shows which declared inputs are unverified, and why.
- Manifests that keep state beside, but outside, the manifest folder keep
  working; their evidence is labelled unverified.
