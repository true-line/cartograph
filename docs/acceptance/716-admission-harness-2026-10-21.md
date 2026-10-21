# Issue 716 — AC-0191 / T-0191 admission-family supplement

This dated supplement targets revision3 of `598-admission-family-harness-transition-20261021.md`, SHA256 `6c514eaaa4bcb3609422613b605de63254a7c167c45b9657022cb13e179d67b1`. The reviewed scope and complete test-map revision are recorded in [issue #716](https://github.com/true-line/cartograph/issues/716#issuecomment-6327538570). AC-0191 text and the historical @8 map remain unchanged. The expanded target is now @10 only; the frozen V7 harness and prior receipts remain immutable.

PR #714 landed as `18de5a5a7919301a3939d48618063e7ea16910a9`; this supplement governs the separate #716 implementation. Repository test-map updates will retain T-0191 as UNEXECUTED and link this supplement plus real test names only after they exist. No planned test below is passing evidence. Scripted checks establish harness mechanics, never live-provider or native-operator acceptance.

| Obligation | Production boundary and proposed regression |
| --- | --- |
| Exact family | Derive request7/@10, prompt9, disclosure6, result7, task3/summary7, intent4 and investigation-v2 from registered definitions. Reject every mixed-family combination and unknown identity before provider construction. Preserve V7 golden bytes and original entrypoints. Cross-verifier tests reject @10 receipts under the V7 verifier and V7 receipts under the @10 verifier, even when individual fields or payloads otherwise look compatible. |
| Fixed evidence denominator | Preparation freezes the complete run-definition obligation ledger with count and digest: request source/inventory obligations plus separately identified procedure-level H4/H5 cross-ingress obligations. Every stage receipt preserves that full ledger and reports each entry as discharged, insufficient, refused, cancelled or unexecuted, with evidence for any discharge. A worker source-coverage result cannot discharge a native or cross-ingress procedure obligation. Capacity refusal, cancellation, FinishOnly and early finish cannot remove entries. Compare complete and capacity-refused fixtures: identical denominator/digest, lower discharged count for the refused fixture. |
| Frozen preparation | Separate provider-free entrypoint binds build commit/tree, source registration, capture, graph, selection, neutral questions, role and model/runtime/endpoint/context/sampling identity. Unknown observations remain explicit. Changing any bound dimension invalidates execution; no fresh capture or current-checkout substitution. |
| Three distinct capacity claims | Serialize exact initial request; emit a versioned conservative full-budget certificate; record runtime admission invariant separately. Fixtures assert runtime admission cannot turn unproven/oversized full-budget evidence into fit. |
| Non-byte capacity | Record model-invocation and generated-token reservations separately from byte certificates. Provider context-window/tokenizer capacity stays unknown unless independently observed and bound to the runtime identity. A byte-fit certificate never implies token or context fit. A bytes-fit fixture with exhausted token reservation yields the explicit token-capacity outcome, not execution eligibility. |
| Complete upper bound | Cover unrestricted and FinishOnly instruction variants, maximal latest-only capacity feedback, completion refusal feedback, widest allowed progress/usage counters, query/source/inventory payloads, completed-attempt IDs and AttemptRecords at the maximum permitted count, unavailable-read attempt metadata, query-history fixed-size identities plus the latest full selector, escaping, fixed overhead, and actual parent history. Test each independently and in reachable combinations using production serialization. Any unsound/unknown component gives bound_unproven; checked arithmetic overflow cannot yield fit. |
| Boundary and diagnostic honesty | At a known certified cap, equality fits and one byte above does not. A conservative overestimate does not claim observed impossibility. Certified prefixes are diagnostic only, with maximality claimed only after proof of monotonicity or exhaustive evaluation. No implicit budget or obligation narrowing. |
| Provider exclusion | Construction/dispatch spy remains untouched for preparation, missing certificate, unknown/oversized bound, mixed family, stale preparation, and duplicate attempts. Preparation API has no provider capability. |
| Exclusive execution | Immutable matching preparation plus exclusive one-attempt marker precedes actual worker dispatch. Duplicate stage, lost acknowledgment, cancellation and unknown outcome retain the marker. No implicit preparation, fallback or replay. |
| Runtime receipt parity | Scripted production worker generates durable query/read/finish steps, citations, usage and identities. Receipt observations are re-derived from saved records after reopen and compared with actual task/result. No synthetic events or invented unknown values. |
| Admission outcomes | Separate capacity-refusal counts, FinishOnly transitions and calls in FinishOnly; retain final finish disposition and requested coverage. Exercise multiple refusals, partial/insufficient finish, cancellation and successful complete coverage. Honest terminal status is never equated with full-obligation acceptance. |
| Independent review | Verify authenticated review binds exact protocol/run/result digest and role. Wrong digest/protocol/role, stale review, and unauthenticated local file fail closed. No oracle is supplied to the analyst. |
| Auditor | Separate preparation binds the actual saved reviewed parent, including attributed T3 history. Reject substituted parent, current checkout and unknown future parent treated as empty. Advisory schema-max auditor estimate stays distinct from authoritative actual-parent certificate. |
| Retention and native limits | Reopen validates source/result/receipt identities without provider access. Native process restart and disposable forgetting remain separate operator procedures, not inferred from database reopen. Preserve the unexecuted H4/H5 cross-ingress obligations. |

## Delivery order and acceptance boundary

1. Issue716 and this supplement define implementation scope. Announce before changing shared repository documentation; retain AC-0191 unchanged and update existing map/matrix only with accurate implementation evidence.
2. Add isolated @10 preparation/proof tests before execution control, then immutable receipts and scripted production-worker integration. Keep new functions/modules separate from the frozen V7 acceptance surface.
3. Cross-review proof completeness, identity binding and evidence denominator with Claude; run repository-required gates and request owner review of the exact published head.
4. Provider execution remains a separate owner decision accepting revision3 and specifying merged build, exact command, frozen run definition and exclusive output path. No fresh Vendure intake, preparation/measurement, model download, policy-ceiling change or destructive forgetting is authorized by this supplement.

Issue598 remains open until real-provider, independent semantic review, auditor, negative-case and native/retention evidence satisfies its full requirements.

## Implementation evidence — foundation checkpoint

The separate `worker/acceptance/admission` module has nine scripted tests for
immutable artifact publication/readback, serialization bounds, preserved
request/procedure denominators, mixed-family rejection and discharge-kind
separation. The frozen ledger explicitly retains MT-H4-01 and MT-H5-01 through
MT-H5-04 as well as the MT-H4-02 analyst/auditor/operator obligations. Receipt
validation is structural: an evidence digest alone does not establish authentic
or sufficient evidence. Durable worker derivation and authenticated operator
review remain controller requirements.

This checkpoint does not implement the full-budget certificate, preparation
controller or provider execution. It is not a passing AC-0191 or H4/H5 result.

## Preserved draft status

This checkpoint is parked pending an owner-scheduled real-provider acceptance
run and renewed implementation coordination. Issue #716 and the original
procedure/test-map agreements remain open; #598 is not closed. Read-only MCP
access is tracked separately in #718. This draft is not ready to merge.

A production-derived coverage-detail component additionally bounds repeated
inventory details and their nested JSON encoding using the retained archive and
registered policy. Four focused tests cover repeat/escaping parity, unavailable
inventory rejection, empty inventory framing and checked overflow. Its explicit
partial-component result grants no whole-workflow fit or execution authority.
The query/source/history/redaction/transport aggregation, execution controller,
actual-worker receipt derivation and full V7 cross-verifier tests remain absent.
