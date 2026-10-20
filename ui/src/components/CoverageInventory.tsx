import { coverageVerdict, producerRows, type RepoCoverage } from "../coverage";

const STATE_LABEL: Record<string, string> = {
  ran: "Ran",
  not_selected: "Excluded",
  missing_input: "Missing input",
  not_applicable: "Nothing to read",
};

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
      {coverage.map((repo) => {
        const { shown, notApplicable } = producerRows(repo);
        return (
          <details
            key={`${repo.repo}@${repo.commit}`}
            className="coverage-repo"
            open={repo.limited}
          >
            <summary>
              <strong>{repo.repo}</strong>{" "}
              <code>@{repo.commit.slice(0, 12)}</code>{" "}
              <span
                className={
                  repo.limited ? "coverage-limited" : "coverage-scoped"
                }
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
                      {unsupported.capability} ×{unsupported.occurrences}, first
                      at <code>{unsupported.first_site}</code>:{" "}
                      {unsupported.reason}
                    </li>
                  ))}
                </ul>
              </>
            )}
          </details>
        );
      })}
    </section>
  );
}
