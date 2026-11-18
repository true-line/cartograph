# ADR-0041 — Bounded query history with explicit runtime families

- **Status:** Accepted with amendment, decided by the owner on 2026-11-18 (#1761; see the 2026-11-18 amendment)
- **Date:** 2026-10-21
- **Deciders:** Cartograph maintainers; Codex implementation, Claude cross-review

## Context

The immutable V2 investigation limits admit 56 tool actions, but the @8 prompt
retains every full query selector. Production witnesses with valid escaped
selectors exceed the 128 KiB transport input ceiling before those actions are
spent. The existing runtime rejects the oversized input safely. Deduplicating
identical selectors cannot bound distinct selections. A larger numeric policy
alone does not resolve this representational growth.

The durable ledger already retains each exact selector and response identity.
That acquisition history must remain complete even when model-visible history
uses progressive disclosure. Existing saved tasks and uncertain retries must
retain their original request, prompt and result meaning.

## Decision

Introduce an explicit @9/request6/prompt8 family under the unchanged V2 numeric
policy. Select a runtime by its complete admitted role/request/policy tuple;
never infer a prompt generation from the numeric profile alone. Preserve @8
and earlier wires, hashes, behavior and saved-task interpretation. The current
catalog advertises the newest families for new expanded selections, and older
families stay readable (amended 2026-11-18; this sentence first named @9 as
current). Old catalog snapshots remain
valid, and a previously selected unavailable role requires explicit replacement;
an uncertain retry retains its frozen original request.

Retain complete acquired fact and evidence-option representations. Represent
older queries with fixed-size ordered references, bound to request intent,
frozen context, acquisition ordinal and the complete query manifest. Supply
only the latest full selector and result metadata. A registry hash binds the
ordered references to the current ledger. Supply coverage qualification once,
and replace repeated selectors in turn progress with their query identity.
Unknown metadata that would otherwise be discarded fails closed.

An explicit replay_query action resolves only against this task's saved ledger.
After validating its identity, the host executes the exact selector through the
ordinary frozen query path, consuming the normal tool and raw-response budget
and appending an ordinary acquisition. No separate uncharged lookup channel,
new persisted tool kind or invented continuation cursor is introduced.

Disclosure schema5 records supplied references separately from selector/result
representations. Prior views are explicitly omitted; only the latest view is
supplied. The sealer compares the full typed history before and after firewall
preparation, including cursor and indexes. The binding's request hash must
match the actual saved intent. Only schema6 results may carry this disclosure;
legacy completion result5 remains paired with disclosure4. References confer
no source or graph-property citation eligibility and discharge no obligations.

## Consequences

History growth is bounded independently of repeated versus distinct selectors.
Redisclosing an older selection costs another query. Complete original selectors
and response identities remain durable; the usual retention rules still apply.

The derived history-only transport bound is 32,780 bytes. Empty-page production
witnesses for depths 0–56 peak at 20,533 bytes. These measurements do not certify
full payloads containing sources, inventories, attempts, feedback and parent
history. Those components require separate bounds before #598 acceptance.
The inductive admission guard is tracked separately in #708. Neither this
change nor a passing deterministic test authorizes provider execution.

## Alternatives

1. Intern exact selectors: helps repetition but cannot bound distinct history.
2. Raise V2 numeric or transport limits: changes an immutable contract without
   addressing cumulative growth or proving the next limit sufficient.
3. Silently shorten history or evidence: loses usable provenance and can imply
   unsupported completeness; explicit references and counted replay avoid that.

## Amendment — 2026-11-18: accepted; the catalog sentence states the rule

The owner accepted this ADR on 2026-11-18 (#1761) with this amendment. The
Decision no longer names @9 as the current advertised family, because later
families have shipped since. The rule is unchanged: newer families are
advertised, and older ones stay readable and keep their meaning.
