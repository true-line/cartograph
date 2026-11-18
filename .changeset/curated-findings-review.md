---
"cartograph": patch
---

Adds the curation review for investigation findings behind the scenes (ADR-0074). A saved finding can now be accepted as curated or rejected through a new command, separate from the follow-up review, and the curated projection reads those reviews. An accepted finding is eligible only while the facts it cites are as the investigation read them. No screen offers the review yet, and no export or agent read lists a finding yet.
