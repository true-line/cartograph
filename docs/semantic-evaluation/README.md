# Host-owned independent semantic evaluation

This store is read from `<app_data>/semantic-evaluation/`. No investigation or
semantic-preview IPC parameter can supply its review authority. Missing or
incompatible evidence preserves staged proposals and admits no semantic edges.
The normal application still performs local embedding work only when a preview
is explicitly invoked.

The independent review workflow must install these files before a run:

- `review-anchor.json`: a closed object with `schema_version: 1`,
  `envelope_sha256`, `policy_implementer` and `corpora`. Each corpus selector has
  exactly `corpus_id`, `repo_id`, `revision` and `capture_id`. Selectors are
  unique by corpus and repo, with at most 32 entries. The SHA256 anchor comes
  from a trusted independent review workflow; hashing a local envelope and
  echoing its reviewer name does not authenticate that reviewer.
- `policy.json`: a closed object with `schema_version: 1` and `families`.
  Each entry contains `family` (`kind` and nullable `subtype`), `threshold`,
  `ambiguity_margin`, `neighbors`, `min_evaluation_cases`,
  `min_answerable_cases`, `min_no_match_cases` and `precision_floor`.
  Operating points and minima are frozen before prediction; no holdout result
  selects a threshold. Neighbors are 2..256; all minima are 1..1,000,000.
- `dataset/`: the reviewed independent dataset, retained span files and
  `review-envelope.json`, loaded under the independent evaluation contract.
  The envelope must bind this exact policy, embedding namespace, corpus,
  labels and partition. Independent datasets use `catalogue-leakage@1`:
  shared *unlabeled* catalogue occurrences are not supervision; repeated
  supervised sources/targets remain grouped. Visible mechanism fixtures retain
  their original purpose and cannot authorize independent quality admission.

Both host config files are bounded to 128 KiB, opened without following links,
and rejected if nonregular or hardlinked. Directory identity and exact config
bytes are rechecked after execution and before persistence. Dataset descendants
are independently read through the bounded, no-follow loader. This is private
same-user application storage, not a sandbox against arbitrary concurrent
filesystem mutation by the same user.

## Observed corpus recipes

The host never copies the dataset's declared hashes into its observations.
Every actual semantic source/candidate repo must have a selector. The source
registry resolves each repo to its host-owned source ID; a retained-source guard
pins the selected capture through provider work and recording. Capture metadata
admits at most 4096 files and 32 MiB of selected raw bytes in aggregate before
copying; loading validates the complete canonical manifest and all raw objects.

Graph input is bounded by the existing snapshot profile before cloning. For each
repo, select the current preview nodes and edges whose provenance evidence names
that repo, requiring the selector's exact revision. Construct the ordinary
`ContextSnapshot` identity from these actual properties. SHA256 of integer-only
JCS for this object is `graph_snapshot_hash`:

```json
{"recipe":"cartograph-t2-host-graph@1","corpus_id":"...","repo_id":"...","revision":"...","snapshot_id":"context-v1:..."}
```

SHA256 of integer-only JCS for the following object is
`capture_manifest_hash`. `capture_id` is the validated capture's existing
versioned BLAKE3 identity of its complete canonical manifest, not a digest supplied
by the dataset:

```json
{"recipe":"cartograph-t2-host-capture@1","corpus_id":"...","repo_id":"...","revision":"...","capture_id":"..."}
```

The selected graph/capture association is explicit independent review input.
It does not manufacture producer receipts for File, Gap or CALLS facts, assert
that every fact was emitted from that capture, or increase a fact's confidence.
Each dataset span must match the actual selected capture bytes at its exact
repo/path/revision/range. Every dataset query and its evidence must match an
actual `graph_inputs` hop; its complete candidate catalogue must match the same
production family grouping, including IDs, texts and evidence. Missing or
incompatible provenance is refused rather than partially accepted.

Current independent dataset schemas require 40-hex committed revisions.
Working-tree `workdir` contexts remain staged with an incompatibility reason;
they are not relabeled as Git commits. The host tests create a disposable real
Git commit, assert a pristine working tree and compare captured bytes to HEAD.
Their scripted embeddings and simulated trusted review establish mechanism
correctness only, not real-model quality or independently conducted review.

After prediction, recording holds the graph mutex, rereads a fresh bounded graph,
rechecks captures, dataset span/catalogue bindings, review envelope, policy and
anchor, then persists the descriptive evaluation summary. Source guards survive
this check. Changed state returns an error; no stale admission is recorded.
