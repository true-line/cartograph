# Bounded query-history disclosure — #707

Status: implementation in progress; no provider acceptance claim.

## Contract

The new @9 specialist family uses request generation 6 and prompt generation 8 under the unchanged investigation-v2 numeric limits. Routing must bind the full family tuple, not infer prompt generation from policy alone. Existing request5/@8 and older families remain immutable, including replay and saved results.

The durable acquisition ledger remains complete. The prompt retains complete acquired fact and evidence-option representations, but prior queries are represented by ordered fixed-size identities. Each identity binds the request hash, frozen context/scope, acquisition ordinal and complete query manifest (including response identity). A registry hash binds their order and the current ledger. Only the latest selector and typed result metadata are fully disclosed. Qualification is supplied once. Unknown fields that a projection would otherwise discard are rejected.

The progress contract must identify the last query without reintroducing its selector elsewhere. A reference does not supply source, graph properties or inventory text and cannot discharge a completion obligation. Finishing disclosure and firewall sealing must explicitly distinguish references from supplied representations.

## Progressive access

A versioned replay_query action resolves the original query from this task's durable ledger using its context, ordinal and digest. The host runs the exact selector through the existing frozen query service, charges normal tool and raw-response budgets and appends an ordinary acquisition. The returned page becomes the latest full view. No continuation cursor is manufactured. Redisclosure costs another acquisition; lookup is not an uncharged channel.

## Bounds and remaining work

Test every allowed history depth with both repeated and distinct escaped selectors. Measure after outer payload serialization, using production paths. The fixed references and one latest selector need a bound derived from existing admitted field limits; an arbitrary smaller component limit is not a proof. Preserve the existing @8 overflow witnesses unchanged.

Query-history bounds do not prove bounds for source escaping, inventories, graph representations, attempts, feedback, progress or supplied parent history. The separate full-budget acceptance certificate must cover all of them. An inductive admission guard is a sibling design: it must establish fit before committing a tool action, include bounded refusal/feedback and finishing paths, and avoid claiming early refusal fulfills the original full-budget guarantee.

## Verification before delivery

- Ledger/request/context/index/digest/order mismatch rejects.
- Firewall changes to references, latest selector, pagination or progress reject.
- Historical replay executes the exact selector and consumes ordinary budgets.
- Reference-only data cannot supply evidence or fulfill obligations.
- All historical role/request/prompt/result wires and reopen behavior remain unchanged.
- No provider, fresh capture or Vendure preparation occurs in these tests.

## Current deterministic evidence

The derived history-only bound is 32,780 bytes after nested JSON encoding, from existing selector/metadata field ceilings and at most 56 acquisitions. This is diagnostic accounting, not a new quota. Production prompt8 witnesses for every history depth 0–56 peak at 20,530 bytes for repeated escaped selectors and 20,533 bytes for distinct escaped selectors. These empty-page witnesses contain no source, attempts, feedback or parent history. They do not establish general full-budget fit.

Disclosure schema5 carries an ordered query reference binding separately from the selector/result representations: prior views are explicitly omitted with `reference_only`, and only the latest view is supplied. This reason is legal only for prior query entries in disclosure5; it is rejected for latest queries, facts, sources, parent history and all older disclosure generations. Supplied reference identities never make a fact or source representation supplied and cannot grant citation eligibility or discharge obligations. The sealer requires exact equality of the complete typed history before and after firewall preparation, including selectors, cursors and indexes. The saved task request must independently match the reference binding's request hash. Result schema6 is paired only with disclosure5; old completion results remain schema5/disclosure4.
