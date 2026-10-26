import { useRef, useState } from "react";
import { coverageVerdict, producerRows, type RepoCoverage } from "../coverage";
import { useAppStore, type DraftAdapterPacket } from "../store";

const STATE_LABEL: Record<string, string> = {
  ran: "Ran",
  not_selected: "Excluded",
  missing_input: "Missing input",
  not_applicable: "Nothing to read",
};

const PACKET_ERROR: Record<string, string> = {
  unavailable_evidence:
    "No recovered local checkout of this repo is available to sample.",
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

function RepoCoverageRow({ repo }: { repo: RepoCoverage }) {
  const draftAdapterWorkPackets = useAppStore(
    (state) => state.draftAdapterWorkPackets,
  );
  const [busy, setBusy] = useState(false);
  const [packets, setPackets] = useState<DraftAdapterPacket[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const { shown, notApplicable } = producerRows(repo);

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
        <strong>{repo.repo}</strong> <code>@{repo.commit.slice(0, 12)}</code>
        {repo.commit_note ? ` (${repo.commit_note})` : ""}{" "}
        <span
          className={repo.limited ? "coverage-limited" : "coverage-scoped"}
        >
          {coverageVerdict(repo)}
        </span>
      </summary>
      <ul className="coverage-producers">
        {shown.map((producer) => (
          <li key={producer.id} className={`coverage-${producer.state}`}>
            <span className="coverage-state">
              {STATE_LABEL[producer.state]}
            </span>{" "}
            <code>{producer.id}</code> ({producer.covers}):{" "}
            {producer.stopping_reason}
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
                <code>{missing.input}</code>: {missing.reason}
              </li>
            ))}
          </ul>
        </>
      )}
      {repo.unsupported.length > 0 && (
        <>
          <h4>Unsupported capabilities</h4>
          <ul>
            {repo.unsupported.map((unsupported) => (
              <li key={unsupported.capability}>
                {unsupported.capability} ×{unsupported.occurrences}, first at{" "}
                <code>{unsupported.first_site}</code>: {unsupported.reason}
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
export function CoverageInventory({ coverage }: { coverage: RepoCoverage[] }) {
  if (coverage.length === 0) return null;
  return (
    <section
      className="coverage-inventory"
      aria-label="Coverage inventory"
      data-testid="coverage-inventory"
    >
      <h3 className="settings-section-title">Coverage inventory</h3>
      {coverage.map((repo) => (
        <RepoCoverageRow
          key={`${repo.repo}@${repo.commit}`}
          repo={repo}
        />
      ))}
    </section>
  );
}
