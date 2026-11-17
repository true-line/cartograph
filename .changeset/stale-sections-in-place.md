---
"cartograph": minor
---

Keeps a stale section in the product specification and marks it. When a fact cited by an accepted section draft changes, a `best-effort` export of `product-specification.md` still prints the draft in its section, under "Stale: cited facts changed since this text was accepted" with the changed citation ids, and each claim that cites one ends with "cites a changed fact". A draft that cites a fact no longer in the graph is not printed; the section says how many are missing and `curated.md` names them. The document header now lists the stale sections, or "none". A `verified-only` export prints no drafted claim, as before, and carries the same list.
