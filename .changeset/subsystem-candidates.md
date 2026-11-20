---
"cartograph": minor
---

The knowledge phase now groups a system's source files into named subsystems: files that belong to one concept are one group even when they sit in different directories (`api/views/alias.py`, `dashboard/views/alias_transfer.py` and `alias_utils.py` are one `Alias` subsystem). Each group is a weak candidate that cites every member file, files used from everywhere are set aside as `Shared code`, and no model or embedding provider is needed. On SimpleLogin it finds 14 subsystems such as Authentication, Custom domain, OAuth and Phone.
