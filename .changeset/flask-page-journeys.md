---
"cartograph": minor
---

`journeys.md` now covers server-rendered Flask applications. Cartograph reads each page's route, its redirects, the templates it renders and their links and forms, and walks them from the home page to the form submissions that change data. On SimpleLogin, `journeys.md` goes from empty to 21 journeys, each ending in an observed database write. For example: home → dashboard → directories → create a directory, which writes `directory_mailbox` and is covered by `test_create_directory`.
