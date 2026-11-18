---
"cartograph": patch
---

Export bundle in the Spec Workbench now writes the file from the desktop host, streamed straight to disk, instead of building the whole bundle as one string in the app window and sending it back. On a large repository that string was close to a gigabyte, longer than the window's maximum string, so the export could fail or hold about a gigabyte of memory. The saved file is unchanged, byte for byte. If the spec changed since the Workbench showed it, the export is refused with the same "Refresh the Spec Workbench" message as before.
