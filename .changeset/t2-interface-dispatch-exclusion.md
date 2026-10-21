---
"cartograph": minor
---

Exclude interface-dispatch hops from T2 semantic matching: an unresolved call shaped `IFace.method` (for example `IChannel.listen`) is dynamic dispatch that embeddings cannot resolve, so it is now classified out of ANN candidacy — never embedded, never proposed — and stays an open gap tagged `dynamic dispatch - needs type hierarchy` so the register can group it. Conservative look-alikes such as `Item.get`, `IO.read` or `ID.parse` still enter candidacy.
