# ADR-0071 — Specialist roles, retired versions and one capability table

- **Status:** Accepted (2026-11-21, by the coordinator as the owner's review delegate, with the amendments in decision 1)
- **Date:** 2026-11-18
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1751; #1745; #1874 stack (section-writer@1); #598; #1710; #1757; #256; [SPEC-10](../SPEC-10_specialist-investigations.md); AC-0574, AC-0584, AC-0589

## Context

`SpecialistId` in `crates/agents/src/investigation/specialists.rs` (main at
`a3d04310`) has 44 variants: domain-analyst @1–@15 and @17–@27 (26; there is
no @16), evidence-auditor @1–@14 (14), spec-writer @1–@2 and gap-triager
@1–@2. (#1751 counted 41 at `d36e7e08`.) The #1874 stack adds
`section-writer@1` (`SectionWriterV1`) as a 45th.

Every variant was added during an acceptance attempt (#598, later #1710).
What differs between versions is spread over 17 `uses_*` predicates on
`SpecialistId` (`uses_compact_disclosure` through
`uses_typed_citation_correction`), each a `match` over version arms, with
about 250 non-test call sites in `crates/` and `src-tauri/`. The UI mirrors
parts of the same lists by hand: `ui/src/investigationFamilies.ts`
(`isCompletionSpecialist`, `isExpandedSpecialist`, the request-family table),
`ui/src/investigationObligations.ts` and the quick-dispatch list in
`ui/src/components/InvestigationQuickDispatch.tsx`. Those lists have already
drifted: the family-7 list in `investigationFamilies.ts` omits analyst
@21–@23, which the backend offers (the run-limits parity bug fixed by #1757
moved that one list to the catalog).

What a user can start today is decided in one place. `available_specialists()`
in `src-tauri/src/investigations/commands.rs` is the desktop catalog, and
`completion_start.rs` refuses any start whose id is not in it ("This
specialist version is not available."). It offers:

| Profile | Offered for a new start |
|---|---|
| investigation-v1 (Standard) | domain-analyst @3–@7, evidence-auditor @3–@7, spec-writer @1, @2, gap-triager @1, @2 (and section-writer@1 with the #1874 stack) |
| investigation-v2 (Expanded) | domain-analyst @10, @11, @12, @18–@23, @27; evidence-auditor @10–@14 |

investigation-v2 is registered in `policy.rs`, so both rows are live. The
per-profile `specialist_ids` in `policy_catalog()` narrow the Standard row to
the completion versions (analyst/auditor @7, spec-writer, gap-triager,
section-writer); @3–@6 start through the older non-completion paths.

Everything else deserializes, so a stored run made with it still opens, but
the catalog does not offer it (the legacy schema-v1 start path can still start
some of these; see decision 2): domain-analyst @1, @2, @8, @9, @13, @14, @15, @17,
@24, @25, @26; evidence-auditor @1, @2, @8, @9. AC-0574 pins that @24–@26
stay out of the catalog while @27 is in it.

The user sees all of this as version numbers. #1745 adds document authoring
to the agent roles; adding it on top of 45 ids and 17 predicates multiplies
the cost.

Quick dispatch (`InvestigationQuickDispatch.tsx`) defaults to
domain-analyst@3, the oldest offered analyst.

## Decision

### 1. One current version per role

The current version of a role is the newest version the catalog offers for a
new start today. Every other version of that role is **retired**: it is kept
only so stored runs reopen.

| Role | Current | Retired (kept so stored runs reopen) | Runs under |
|---|---|---|---|
| domain-analyst | **@28** | @1–@15, @17–@26; @27 stays offered (see below) | investigation-v2 |
| evidence-auditor | **@14** | @1–@13 | investigation-v2 |
| spec-writer | **@2** | @1 | investigation-v1 |
| gap-triager | **@2** | @1 | investigation-v1 |
| section-writer | **@1** (#1874 stack) | none | investigation-v1 |

**Accepted amendments (2026-11-21).** domain-analyst@28 (#1335, the selected
response schema shown in the prompt) joined the catalog after this ADR was
written; as the newest offered analyst it is the current one. domain-analyst@27
is not retired while an acceptance freeze names it (AC-0574, AC-0589): it stays
offered as an explicit, advanced version choice. The profile question below is
settled as recommended: one current version per role, and the composer picks
the profile from the role.

Reasons, from what the code offers:

- **domain-analyst@27** is the newest analyst in the catalog and the one
  AC-0574 advertises; #598's frozen acceptance cases bind it (AC-0589). @24,
  @25 and @26 are newer than @23 but are not offered; they stay retired.
- **evidence-auditor@14** is the newest auditor offered; it holds the
  correction reserve and capacity exclusion of AC-0596.
- **spec-writer@2** and **gap-triager@2** add the authored cited narrative
  (`uses_cited_narrative`, AC-0404, AC-0406) on the same Standard contract as
  @1.
- **section-writer@1** is the only section-writer version.

Retiring a version changes which ids a new start accepts. It does not change
any saved record, prompt, prompt fingerprint, fact, graph hash or tier.

**Owner question (profile coupling).** analyst@27 and auditor@14 run only
under investigation-v2; the writer roles only under investigation-v1. If
retiring every Standard analyst and auditor is not wanted, the alternative is
to keep **@7** as the Standard-profile current for those two roles. This ADR
recommends one current per role; the composer then chooses the profile from
the role, not the user.

### 2. Retired versions reopen read-only, showing their original version

- Every saved run reopens: its detail, findings, final message, transcript,
  provider attribution and saved limits, decoded with its own version's
  contract as today.
- The run shows its **original** specialist id (for example
  `domain-analyst@9`) and is labelled retired. Nothing rewrites a saved
  `specialist_id` or upgrades it to the current version.
- A retired run starts no new provider work under its own version: retry,
  resume, continue, re-draft and finish correction are not offered. A
  follow-up keeps the saved specialist selection, as SPEC-10 and AC-0260
  require. When that version is retired, the user picks an available
  replacement explicitly before the follow-up starts, with the role's current
  version listed first. Nothing upgrades the selection silently. The follow-up
  is a child of the saved run.
- The start refusal stays the existing `invalid_input` from
  `completion_start.rs`, and the read-only state is driven by the catalog, not
  by a client list. Today only `completion_start::start_access` checks
  `available_specialists()`. The legacy schema-v1 path
  (`commands::start_access` through `StartInvestigationRequest::validate`)
  rejects only completion-obligation roles, so a retired id such as
  `domain-analyst@1` can still start there. The first code slice applies the
  same availability gate to every start path, legacy included, before any id
  is retired from the catalog.

### 3. One capability table replaces the `uses_*` predicates

Per-version behaviour becomes data: one table in `crates/agents`, one row per
`SpecialistId`, giving its role, version, status (current or retired), limit
profile and request family, and one field per capability that a `uses_*`
predicate answers today (compact disclosure, turn progress, inspection
requirements, coverage qualification, completion obligations, policy
selection, query-history disclosure, next-prompt admission, cited narrative,
combined finish feedback, exact source reservation, query-budget refusals,
acquisition choices, exact-node queries, capacity exclusions, finish-feedback
witness, typed citation correction). Every call site reads the table; a new
behaviour is a new column, not a new arm in many matches.

The prompt text, version-specific prompt suffixes and compiled definition
data stay where they are: they are immutable history, and the table refers to
them, it does not hold them.

**Timing.** The table lands **after the #1874 stack merges** (the stack adds
`SectionWriterV1` arms to the same predicates, so earlier work would
conflict and miss a row) and **between #598 and #1710 acceptance runs**, never
while one is in flight. Slice (a) changes no id, prompt byte, fingerprint or
limit, so a freeze receipt that names domain-analyst@27 stays valid; landing
between runs removes any doubt. No slice retires a version that an open
acceptance freeze names.

### 4. Slices

- **(a) Capability table, no behaviour change.** Add the table; make each
  `uses_*` a one-line read of it (then inline them at call sites). A test
  asserts, for every `SpecialistId`, that each capability equals the old
  predicate's answer, so the change is provably neutral.
- **(b) Read-only reopen.** First (AC-1400): an `availability` column
  (current, offered, retired) marks exactly today's offered set, the desktop
  catalog and every policy's `specialist_ids` are derived from it, and every
  start path, legacy included, refuses a version it does not offer. Then
  mark retired versions in the table; the catalog
  stops offering them; the reopened run shows its original id and offers no
  new work under it (decision 2).
  As built (AC-1062, #1751): every version that is not current is retired
  except domain-analyst@27 (AC-0589 freeze) and domain-analyst@29
  (ADR-0085 runtime). The history and run detail label a retired run; its
  only host actions were cancel and follow-up, and a follow-up already
  required an explicit offered version.
- **(c) Backend catalog serves roles and capabilities.** The
  `investigation_specialists` command returns roles, each with its current
  version and the capability rows the UI needs. The UI deletes its hand
  lists (`investigationFamilies.ts`, the version tests in
  `investigationObligations.ts`, the quick-dispatch id list) and reads the
  catalog.
- **(d) Role-based composer.** The composer and quick dispatch offer roles
  by name; the version is the role's current one. A version choice, if the
  owner keeps one, is an advanced option listing only offered versions.

## Consequences

- A user picks "Domain analyst", not "domain-analyst@19".
- A new capability is one column and its readers; drift between the backend
  and the UI's copies ends because the UI has none.
- The enum keeps every variant forever; retired ids still decode.
- Retiring the offered-but-not-current versions (analyst @3–@7, @10–@12,
  @18–@23; auditor @3–@13; spec-writer@1; gap-triager@1) removes them from new
  starts. Their stored runs are unaffected.
- No fact identity, graph hash, tier or provenance semantics change.

## Alternatives

1. **Keep versions, add display names.** Cheaper, but the 17 predicates and
   the hand-mirrored UI lists remain, and #1745 adds more.
2. **Migrate stored runs to the current version.** Would rewrite saved
   identity and break exact historical decoding; rejected.
3. **Delete retired variants.** Stored runs would no longer open; rejected.
