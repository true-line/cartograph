# ADR-0058 — Citation ids and the fact and evidence index

- **Status:** Accepted, decided by the owner on 2026-11-18 (#1761); proposed in #1785
- **Date:** 2026-11-11
- **Deciders:** Cartograph owner; Claude implements
- **Related:** #1785; #1745; US-0012 / AC-0595; ADR-0056; `docs/DOCUMENT-CATALOGUE.md` D11 §11.7

## Context

The document catalogue's first rule is that every statement in D1 to D10
resolves to evidence, and D11 §11.7 is where it resolves to. Nothing produces
that index. The traceability matrix indexes only the seven trace relations, so
an endpoint's method or a field's type has no row to point at.

Exported artifacts already carry their evidence inline: every
`SpecAssertion` names its subject (a node id, or a `src label dst` edge) and
its provenance. What is missing is one stable name for a fact that a sentence
in any document can cite, and one place that name resolves. Agent-authored
sections (#1748) need the same thing: an authored sentence must cite something
that does not change when a document is re-rendered or re-ordered.

## Decision

1. **A citation id names a fact, not an occurrence.** It is
   `F-` followed by the first 16 lowercase hex characters of
   BLAKE3 over the domain tag `cartograph:citation:v1`, a zero byte, and the
   canonical JSON of the fact's `FactKey` (`{"kind":"node","id":…}` or
   `{"kind":"edge","source":…,"label":…,"destination":…}`). Provenance,
   properties, the artifact and the position in it are not inputs.
2. **The same fact has the same id everywhere.** Two documents citing one
   endpoint cite one id. Re-exporting the same graph yields the same ids. A
   re-ingest keeps an id exactly as long as the fact keeps its identity; the
   row's tier, confidence and evidence may change under an unchanged id.
3. **The length is fixed.** Sixty-four bits is never extended to break a tie,
   because an id that depends on which other facts are present is not stable.
   Two different facts with one id in an export fail the export.
4. **The index lists cited facts only.** It is the union of the subjects of
   every assertion in the export's artifacts, once each, ordered by citation
   id. Each row carries the fact key, the kinds it is cited as, and one
   record per distinct provenance: tier, confidence, producing method,
   evidence basis, extractor, every evidence span (repository, path, byte
   range, commit) and the ids of the artifacts that carry it. Two artifacts
   can show one fact with different provenance (an edge without valid
   provenance is a flow hop under one fallback and an edge under another);
   the row then lists every record and is marked as differing, and none is
   chosen over another. It is not a dump of the graph, so its size follows
   the documents.
5. **The index follows the export mode.** It is built from the assertions the
   mode already admitted, so a `verified-only` index holds no InferredWeak
   row and no statement citing one survives; in `best-effort` such rows are
   annotated (R-INT-5). A `verified-only` export that nevertheless cites an
   InferredWeak fact is refused, not given a row and not silently trimmed. A
   Gap is a row like any other and is labelled as a Gap (R-INT-2, R-INT-4).
6. **Resolution is checked, not assumed.** Compiling an export fails if any
   cited id has no index row. The check takes a list of cited ids, so authored
   sections are held to it by the same function when they arrive.
7. **Two forms, one content.** The export gains `fact-index.json` (artifact
   id `fact-index-json`; every row, every span) and `fact-index.md` (artifact
   id `fact-index`; the same rows in a table, without source text). The index stores spans, not the bytes they cover; any text it does
   carry passes the export's existing redaction. The same graph yields the
   same bytes.
8. **Citations enter the documents one at a time.** This decision adds the id,
   the index and the check. Printing the id beside each statement in an
   existing projected document is done per document in later changes, so no
   document's content identity changes here.

9. **Readers that reserve memory in advance do not serve the index yet.**
   Building it for one selected read compiles every citing artifact in turn.
   MCP reads reserve a bound that does not model that work, so they neither
   advertise nor serve the two index artifacts until the bound does; the
   desktop export and the desktop single-artifact read produce them.

## Consequences

- A sentence in any document, projected or authored, can be checked against a
  row with a span. D4 §4.2, D5 §5.2 and D7 become checkable statement by
  statement once they print their citations.
- A synthesized assertion that names no stored fact (a derived flow hop, a
  placeholder for invalid provenance) is cited by its own subject key and
  appears with the confidence it already carries; it is never given the
  provenance of a neighbouring fact.
- An id survives re-ordering, re-rendering and schema changes of a document.
  It does not survive a change of fact identity; that is drift and is
  reported as such.
- The export grows by two files whose size follows the number of cited facts.
- A collision fails the export instead of silently merging two facts. At
  sixty-four bits this is not expected in practice.

## Alternatives

1. **Sequential numbers per export.** Short and readable, but the same fact
   gets a different number in each export and each document, so an authored
   sentence cannot keep its citation.
2. **Cite the raw fact key.** Always unique, but keys are long, contain paths
   and spaces, and do not survive being printed in a table or a sentence.
3. **Index the whole graph.** Removes the need to collect cited facts, but the
   export bundle is already 870 MB on a large repository (#1582) and most rows
   would be cited by nothing.
