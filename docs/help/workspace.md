# Workspace

The landing surface after a recovery: the honest outcome (Full, Partial, or
Inferred recovery), what the current system contains (each repo with its
commit identity, derived from graph facts), provenance health, and the
generated artifact set with generation and authority badges.

- **System contents** lists every repo contributing facts; further ingests
  merge into the same system. Where an ingest of that repo at that commit is
  on record, its graph content hash follows in brackets: re-ingesting the
  same commit under the same registration shows the same hash. A local
  folder's hash is specific to this installation. **Clear system** removes
  all recovered facts; job history and settings survive.
- **New project…** starts over with a different repository without
  reinstalling. It removes everything tied to the loaded system: its
  recovered facts, registered sources, retained source, findings,
  investigations and chats, section drafts, reviews, decisions and job
  history. Settings, provider keys, run limits, the local model and
  installed adapters are kept. Cartograph restarts with an empty project.
  It is unavailable while a job is queued or running.
- **Triage gaps** and **Provenance & eval** jump to the register surfaces.
