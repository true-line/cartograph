# ADR-0039 — Citable TS/JS declaration facts

- **Status:** Proposed
- **Date:** 2026-10-21
- **Deciders:** Cartograph maintainers (owner review pending)

## Context

#691 made every directly emitted callable citable through a provenance-only
primary receipt. On the frozen Vendure closure, important code still had no
citable range:

- configuration object literals and other top-level non-callable `const`
  declarations had no fact at all;
- method decorators carrying permission rules sat outside the method span;
- some callables exceed the host's 8 KiB per-range inspection cap, so their
  single span could not be inspected.

Widening an existing Symbol span (to include decorators, say) would change
that fact's digest and identity across every graph, so it is avoided.

## Decision

The TypeScript adapter emits additional direct facts. Each gets its own exact
span and a provenance-only v2 receipt:

- **`Declaration` (`kind: Variable`)** covers each `const`/`let`/`var`
  declarator directly in the program, exported or not, whose value is not
  itself a callable (callables are already Symbols). The id is
  `decl:<repo>@<path>#<name>`, and props include `binding` and `exported`.
- **`Declaration` (`kind: Property`)** covers each first-level `pair` of such a
  declarator's object-literal initializer, seen through `as`, `satisfies` and
  parentheses, with id `decl:<repo>@<path>#<name>.<key>`.
  - Key naming reuses the callable rules. Quoted and number keys are named by
    position (`key@N`) and never persisted.
  - Computed keys and callable values (seen through `as`, `satisfies` and
    parentheses) emit nothing.
  - A section whose span exceeds 8 KiB is flagged `span_exceeds_range_cap`,
    and its object value is described one level deeper the same way, so each
    section reaches an inspectable range or remains a named gap.
  - A repeated name (a legal duplicate `var` or object key) is
    position-qualified as `<name>@N`, so every id stays unique.
- **`Decorator`** covers each decorator applied to a method, with id
  `deco:<repo>@<path>#<method name>/<index>`.
  - The `name` prop is the callee path rebuilt from identifier tokens, never
    arguments or comments; any other callee is position-named.
  - Comments between decorators and the method do not hide them.
  - A `DECORATES` edge links it to the method's Symbol.
- **`Signature`** is emitted for a callable whose evidence span exceeds 8 KiB.
  - It covers the header, from the evidence start to the body start, with
    `body_exceeds_range_cap: true`, plus `header_exceeds_range_cap` when even
    the header does not fit. That flag names the gap instead of hiding it.
  - A `SIGNATURE_OF` edge links it to the Symbol.
  - Nested callables keep their own Symbols.

Declaration and Property facts also get a `DEFINED_IN` edge to their File.

These facts never become rule owners: the producer records them separately
from callable owners. They are minted under #691's per-file and directory
declaration bounds, and every omission is named per file.

Existing Symbol and Component spans, identities and props do not change.
Label-matching consumers send the new labels to their existing generic
handling for unknown labels.

## Consequences

- Configuration sections become individually citable within one inspection
  range, so a reviewer can follow a config choice to the exact source.
- Graphs grow by roughly one node and one edge per top-level declarator,
  first-level config property, method decorator and oversized callable.
- Class, field and parameter decorators remain uncovered. Parameter decorators
  already sit inside their method's span.
- JSDoc comments remain a named gap. They are documentation, not behaviour.
