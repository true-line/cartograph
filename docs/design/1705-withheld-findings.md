# Finding withholding after the shared correction

Implementation specification for #1705, approved by Decision #1704 option 1.

## Version boundary

Introduce domain-analyst@25 reusing the prompt14 bytes. Withholding remains
host-only and is not described to the model. The saved request already binds
specialist identity to completion behavior; an unrecorded host-policy switch
would reinterpret historical requests. Preserve @20–@24 serialized contracts
and goldens. Keep the existing numeric limits and shared correction allowance.
The prepared input retains the builder-validated specialist identity privately.
Reserve sizing and sealing use that frozen identity; shared prompt bytes never
select @25 authority. Reject a saved-request/builder role mismatch.

## Admission

Only after the single correction has been consumed, run complete deterministic
validation on the current finish. The eligible residual failures are finding-local
unverified enumeration and configuration claims. Withhold the entire affected
finding, deduplicate indexes, and retain survivor order. Earlier finish indexes
are not exclusion authority.

Replay, invalid citations, placeholders, denial of acquired evidence, global or
narrative locators, omitted corrections, and ambiguous locations remain fatal.
Recheck projected findings through normal admission. Analyst@25 accepts only
schema-1 finishes without narratives; projection explicitly rejects message-role
schemas and narratives rather than dropping or rewriting forbidden output.
Survivors are renumbered finding-1..n consistently in results, audit and history;
the witness maps them to original positions. Survivor prose referencing another
finding by number is not rewritten and remains subject to semantic review.

Re-derive obligations from the unchanged request, ledger, disclosure and attempts.
Any unmet obligation or an empty survivor set remains a failure. Acquisition is
not proof that the answer semantically covers the requested workflow.

## Durable result

Expose a host-authorized partial disposition,
`findings_withheld_after_correction`, with fixed wording:

> Some findings were withheld because their claims could not be verified.

Do not coerce this result to Supported or allow the model to grant itself this
host disposition. Preserve original response provenance and derive the result
from an explicit projection. Sealing must consume that projection rather than
re-admit the original finish action. Settlement, reopen, UI and export must
preserve the partial disposition. Existing JSON results and saved history carry
that disposition; this change adds no export feature and leaves confirmed-fact
exports unchanged.
The in-run audit remains advisory and receives the fixed withheld disposition
without discarded text. A separate saved-parent auditor receives the original
scope, saved execution status and partial result through authenticated history.

Bind a bounded, text-free witness to the route/detector version, original finish
hash, request/obligations hash, ledger/disclosure hash, prior correction witness,
and projected result hash. Validate all bindings against durable authority.
Use existing envelope limits; overflow fails closed. Never persist refused prose,
source text, or extracted identifiers. Hash bindings do not reproduce a discarded
prose detector decision; do not claim that level of independent replay.

Obligations are evidence-supply obligations and are unaffected by withholding.
Whether surviving findings answer the question is judged by independent semantic
review, including the frozen rubric’s useful-supported-finding criterion.

## Required verification

Offline fixtures cover relocated claims, both eligible classes, all findings
withheld, unmet obligations, stale ledger/witness substitution, deterministic
survivor ordering and hash, reopen/export equality, and unchanged @20–@24 goldens.
Fatal-class fixtures must establish that withholding does not relax other guards.
The terminal error mapping must distinguish claims from placeholders.

Run full workspace gates under nice with Rust test threads limited to four.
Claude reviews the local commit and publishes signed. A single DEV r6 needs a
separate approved run packet after merge; this change grants no provider run or
release-cut permission. Partial output alone does not establish #598 acceptance.
