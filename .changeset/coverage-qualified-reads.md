---
"cartograph": minor
---

Context reads can now be qualified by recorded coverage. Each recovery stores its repo's coverage inventory together with the facts it describes. A qualified context query reports, for every repo in scope, whether that inventory is recorded, stale or missing. It also reports what the inventory could not see and what was checked at read time, and never claims exhaustive coverage. The existing context query is unchanged.
