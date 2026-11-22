# ADR-0082 — A locked runner scores independent semantic evaluations

- **Status:** Accepted (coordinator as the owner's delegate, 2026-11-22; owner approved the locked account and chose no human review)
- **Date:** 2026-11-22
- **Deciders:** Cartograph owner (delegated); Claude implements
- **Related:** #258; amends ADR-0038; ADR-0037 (embedding-space binding); US-0008 / AC-1508

## Context

ADR-0038 admits a T2 family only on an independent receipt. That receipt is
scored on reviewed labels the policy's authors never saw. Its own text says
operational blindness needs "an independently controlled store and runner",
and that hidden files and shared agent filesystems do not provide it.

The desktop app scores in-process today. It reads the reviewed dataset from
`<app data>/semantic-evaluation/dataset`, and the evaluation texts land in
its embedding cache. Agents that develop the policy run under the same
operating-system account, so they can read both. The store therefore gives
no blindness against the agents that tune thresholds.

The owner chose a locked store and runner with no human reviewer
(2026-11-22).

## Decision

1. **Evaluation labels live only in a locked account.** The store belongs to
   a standard, non-admin operating-system account on a dedicated host. Its
   home is mode 0700. No agent has a shell, file access or an admin login on
   that host. The account's one SSH key is restricted to a forced command
   (`command="…/eval-runner serve",restrict`): no pty, forwarding or file
   transfer. The host's administrator can still read the store. Blindness is
   required against the agents that write policy, and the administrator is
   not one of them.
2. **The runner scores, and the app only verifies.** `eval-runner serve`
   reads one bounded request on stdin: a frozen family policy. It loads the
   reviewed dataset from its store under the same checks
   `ReviewedDataset::load` applies. It embeds through that
   host's loopback model with its own cache, seals predictions before
   reading labels, and scores with the shared scoring code. It writes one
   signed receipt to stdout.
3. **A receipt carries scores, never cases.** The receipt holds:
   - per family: whether it was adequate and passed, precision, recall and
     the aggregate scoring counts (cases, answerable, no-match, abstained)
   - digests of the envelope, inputs, leakage, labels, corpora, policy and
     predictions, the embedding namespace and dimension, and the runner
     version
   - the submission count
   It is signed with an Ed25519 key that exists only in the locked account.
   No case id, text, span, label or per-case outcome leaves the runner, and
   a failure is a fixed error code.
4. **One score per policy per partition.** The runner appends every
   request's outcome and policy digest to a log in its store. It commits the
   first policy to the dataset's review envelope and refuses any different
   policy for that dataset. A new policy needs a new evaluation partition,
   frozen inside the lock. Resubmitting the committed policy returns the
   same receipt.
5. **A model reviewer inside the lock.** ADR-0038's reviewer must be distinct
   from the label author and the policy implementer. Under this ADR that
   reviewer is a different local model, with deterministic source checks,
   run inside the locked account. Every no-match and hard-negative label cites
   a source span from the retained capture. A case where the reviewer and the
   author disagree is dropped, never adjudicated by an agent. As in ADR-0038,
   reviewer identities are recorded claims, not authentication.
6. **Agents draft calibration, never evaluation.** Calibration labels and the
   authoring rubric may be written and read in the open. The evaluation
   partition is authored, reviewed and frozen only inside the lock.
7. **The app admits only on a verified receipt.** The app pins the runner's
   public key in its host-owned store. A receipt is descriptive data until
   the app has verified the signature and revalidated the receipt's corpus,
   capture, embedding-binding and policy digests against live state. Any
   mismatch abstains, as ADR-0038 already requires on reload.
   The policy digest binds the dataset digests, the embedding space and the
   family parameters, never calibration labels, so the app recomputes it
   from the receipt and its own parameters without a runner change
   (AC-1511).

## Consequences

- Admission evidence no longer needs a dataset on the desktop host. The
  in-process reviewed path stays for the visible mechanism fixtures, which
  ADR-0038 already keeps from admitting.
- Policy work is one-shot per partition. A failed policy is a result, not
  something to iterate against the same labels.
- The locked host is a dependency for admission. Without it, every family
  abstains, as it does today.
- Setup is one administrator step on the eval host (account and key). After
  that, the runner binary and store are maintained through the forced
  command and the host administrator.

## Alternatives

1. **A human reviewer with the store on the desktop host.** The owner chose
   no human review, and the desktop host gives no blindness against agents
   running as the same account.
2. **Encrypting the dataset on the desktop host.** The app must decrypt it to
   score, so any agent that can run the app can read the labels.
3. **A remote scoring service.** That needs network egress from ingest and a
   third party holding the labels. The dedicated host already provides the
   isolation.
