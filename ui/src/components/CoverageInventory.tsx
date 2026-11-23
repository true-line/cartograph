import { useRef, useState } from "react";
import {
  coverageVerdict,
  producerRows,
  REFUSAL_REASON_TEXT,
  unsupportedRows,
  type RepoCoverage,
  type RestoredCoverage,
} from "../coverage";
import { relabelRepoText } from "../repositoryLabels";
import { useAppStore, type DraftAdapterPacket } from "../store";

/** A plugin that was refused on a file ran, but not over every file. */
const REFUSED_LABEL = "Ran, files refused";

const STATE_LABEL: Record<string, string> = {
  ran: "Ran",
  not_selected: "Excluded",
  missing_input: "Missing input",
  not_applicable: "Nothing to read",
};

const PACKET_ERROR: Record<string, string> = {
  unavailable_evidence:
    "No recovered local checkout of this repo is available to sample.",
  unpinned_revision:
    "This repo was recovered from a working directory, not a clean git checkout, so samples cannot be pinned to a commit. Commit or stash changes, recover it again, then draft the packet.",
  selection_conflict:
    "The checkout has changed since it was recovered. Recover it again, then draft the packet.",
};

function formatPacketError(failure: unknown): string {
  const code =
    failure instanceof Error
      ? failure.message
      : typeof failure === "object" && failure !== null
        ? ((failure as { code?: unknown }).code ?? String(failure))
        : String(failure);
  return PACKET_ERROR[String(code)] ?? String(code);
}

/** Names a source by its readable label; the exact key stays the default. */
type RepoLabel = (repo: string) => string;
const exactKey: RepoLabel = (repo) => repo;

function RepoCoverageRow({
  repo,
  repoLabel,
}: {
  repo: RepoCoverage;
  repoLabel: RepoLabel;
}) {
  // #2471: a local source reads as its folder name, not `local/src_<hash>`;
  // the exact key stays one hover away and in every request.
  const label = repoLabel(repo.repo);
  const named = (text: string) => relabelRepoText(text, repo.repo, label);
  const draftAdapterWorkPackets = useAppStore(
    (state) => state.draftAdapterWorkPackets,
  );
  const [busy, setBusy] = useState(false);
  const [packets, setPackets] = useState<DraftAdapterPacket[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const { shown, notApplicable } = producerRows(repo);
  const unsupported = unsupportedRows(repo);

  const handleDraft = async () => {
    const ticket = ++generation.current;
    setBusy(true);
    setPackets(null);
    setError(null);
    try {
      const result = await draftAdapterWorkPackets(repo.repo);
      if (ticket === generation.current) {
        setPackets(result);
      }
    } catch (failure) {
      if (ticket === generation.current) {
        setError(formatPacketError(failure));
      }
    } finally {
      if (ticket === generation.current) {
        setBusy(false);
      }
    }
  };

  return (
    <details className="coverage-repo" open={repo.limited}>
      <summary>
        <strong title={label === repo.repo ? undefined : repo.repo}>{label}</strong>{" "}
        <code>@{repo.commit.slice(0, 12)}</code>
        {repo.commit_note ? ` (${repo.commit_note})` : ""}{" "}
        <span
          className={repo.limited ? "coverage-limited" : "coverage-scoped"}
        >
          {coverageVerdict(repo)}
        </span>
      </summary>
      <ul className="coverage-producers">
        {shown.map((producer) => (
          <li
            key={producer.id}
            className={
              producer.refusals
                ? "coverage-refused-producer"
                : `coverage-${producer.state}`
            }
          >
            <span className="coverage-state">
              {producer.refusals
                ? REFUSED_LABEL
                : STATE_LABEL[producer.state]}
            </span>{" "}
            <code>{producer.id}</code> ({producer.covers}):{" "}
            {named(producer.stopping_reason)}
            {producer.refusals && (
              <ul
                className="coverage-refused"
                aria-label={`Files refused for ${producer.id}`}
              >
                {producer.refusals.files.map((file) => (
                  <li key={file.path}>
                    <code>{file.path}</code>: not covered,{" "}
                    {REFUSAL_REASON_TEXT[file.reason] ?? file.reason}
                  </li>
                ))}
                {producer.refusals.omitted > 0 && (
                  <li>
                    {producer.refusals.omitted} more refused files are counted
                    but not listed
                  </li>
                )}
              </ul>
            )}
          </li>
        ))}
      </ul>
      {notApplicable.length > 0 && (
        <p className="muted">
          Nothing to read for: {notApplicable.join(", ")}.
        </p>
      )}
      {repo.missing_inputs.length > 0 && (
        <>
          <h4>Missing inputs</h4>
          <ul>
            {repo.missing_inputs.map((missing) => (
              <li key={missing.input}>
                <code>{missing.input}</code>: {named(missing.reason)}
              </li>
            ))}
          </ul>
        </>
      )}
      {unsupported.length > 0 && (
        <>
          <h4>Unsupported capabilities</h4>
          <ul>
            {unsupported.map((unsupported) => (
              <li key={unsupported.capability}>
                {unsupported.capability} ×{unsupported.occurrences}, first at{" "}
                <code>{unsupported.first_site}</code>: {named(unsupported.reason)}
              </li>
            ))}
          </ul>
        </>
      )}
      <div className="coverage-adapter-packets">
        <button
          type="button"
          className="btn-ghost"
          disabled={busy}
          onClick={() => void handleDraft()}
        >
          Draft adapter work packet
        </button>
        {error && (
          <p role="alert" className="coverage-packet-error">
            {error}
          </p>
        )}
        {packets !== null &&
          (packets.length === 0 ? (
            <p className="muted">
              Every looked-up cell has a declaring adapter
            </p>
          ) : (
            packets.map((packet) => (
              <div
                key={`${packet.language}:${packet.content_id}`}
                className="coverage-packet-draft"
              >
                <div className="coverage-packet-header">
                  <strong>{packet.language}</strong>{" "}
                  <code>{packet.content_id}</code>{" "}
                  <button
                    type="button"
                    className="btn-ghost"
                    onClick={() =>
                      void navigator.clipboard?.writeText(packet.markdown)
                    }
                  >
                    Copy
                  </button>
                </div>
                <pre aria-label={`${packet.language} adapter work packet`}>
                  {packet.markdown}
                </pre>
              </div>
            ))
          ))}
      </div>
    </details>
  );
}

/** What each repo's producers covered and where they stopped (AC-0238),
 *  shown before any interpretation is offered. Each repo opens to its
 *  producers, missing inputs and unsupported capabilities. */
export function CoverageInventory({
  coverage,
  note,
  repoLabel = exactKey,
}: {
  coverage: RepoCoverage[];
  /** Readable source names (#2471); exact keys when absent. */
  repoLabel?: RepoLabel;
  /** States where the inventories come from when not from this session. */
  note?: string;
}) {
  if (coverage.length === 0) return null;
  return (
    <section
      className="coverage-inventory"
      aria-label="Coverage inventory"
      data-testid="coverage-inventory"
    >
      <h3 className="settings-section-title">Coverage inventory</h3>
      {note && <p className="muted">{note}</p>}
      {coverage.map((repo) => (
        <RepoCoverageRow
          key={`${repo.repo}@${repo.commit}`}
          repo={repo}
          repoLabel={repoLabel}
        />
      ))}
    </section>
  );
}

/** The inventories earlier ingests recorded, restored after a relaunch
 *  (AC-0700). Current ones are shown as recorded, not revalidated; stale ones
 *  are named as stale with nothing read from them; a failed read is stated
 *  and shows no card. */
export function RecordedCoverageInventory({
  restored,
  error,
  repoLabel = exactKey,
}: {
  restored: RestoredCoverage[];
  error?: string | null;
  repoLabel?: RepoLabel;
}) {
  if (error) {
    return (
      <p role="alert" data-testid="recorded-coverage-error">
        Recorded coverage could not be read: {error}. No recorded inventory is
        shown.
      </p>
    );
  }
  const current = restored.flatMap((repo) =>
    repo.status === "recorded" && repo.inventory ? [repo.inventory] : [],
  );
  const stale = restored.filter((repo) => repo.status === "stale");
  return (
    <>
      <CoverageInventory
        coverage={current}
        repoLabel={repoLabel}
        note="Recorded by the last ingest of each repo. Its registration and plugins were compared now; source inputs and everything else are as recorded, not newly validated."
      />
      {stale.length > 0 && (
        <section
          className="coverage-inventory"
          aria-label="Stale coverage inventories"
          data-testid="stale-coverage"
        >
          <h3 className="settings-section-title">Stale coverage inventories</h3>
          <ul>
            {stale.map((repo) => (
              <li key={repo.repo}>
                <strong title={repoLabel(repo.repo) === repo.repo ? undefined : repo.repo}>
                  {repoLabel(repo.repo)}
                </strong>{" "}
                <code>{repo.inventory_hash.slice(0, 19)}</code>: stale, not
                current. Its registration or plugins changed since it was
                recorded (compared: {repo.compared.join(", ")}), so nothing is
                read from it. Re-ingest to record current coverage.
              </li>
            ))}
          </ul>
        </section>
      )}
    </>
  );
}
