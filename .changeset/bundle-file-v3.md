---
"cartograph": minor
---

Export bundle now writes bundle file v3. The Markdown fact index is no longer stored a second time next to its JSON data. The spec crate's reader rebuilds it byte for byte and refuses a file where it does not match. On a large repository this cuts the file from 883 MB to 729 MB. Files written as v1 and v2 still read (#1582).
