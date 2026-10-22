# Disclosure measurements

Measure exact serialized UTF-8 request and response bytes, including framing,
initialization, tool-schema and capability/catalog overhead. State which shared
overhead is charged to each strategy and whether schemas are reused. Compare
identical fixture/basis/content identities and the same requested result. Bytes
are not tokens: identify tokenizer and version for measured token counts;
otherwise report tokens as unknown. Byte savings do not prove answer quality.

Fetching a subset of pages for a real question needs its own measurement. Do not
load the complete graph or artifact just to claim that progressive disclosure
was used. The fixture validates deterministic transport behavior, not a model's
answers, an installed client, H3 curation or H5 provider acceptance.
