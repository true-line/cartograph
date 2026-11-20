import { useState } from 'react';
import type { AreaChoice, AreaExchange } from '../areaExpert';
import { refusalText, usageText } from '../areaExpert';

export interface AreaExpertPanelProps {
  areas: AreaChoice[];
  selected: string | null;
  exchanges: AreaExchange[];
  busy?: boolean;
  error?: string | null;
  /** False when the core is not reachable: nothing can be asked. */
  canAsk: boolean;
  onReload?: () => void;
  onSelect: (area: string) => void;
  onAsk: (question: string) => void;
  onOpenCitation?: (citationId: string) => void;
}

const BASIS_LABEL = { stated: 'Stated', inferred: 'Inferred' } as const;
const BASIS_MEANING = {
  stated: 'The briefing states this.',
  inferred: "The expert's reading across briefing rows.",
} as const;

function Exchange({ exchange, onOpenCitation }: {
  exchange: AreaExchange;
  onOpenCitation?: (citationId: string) => void;
}) {
  const { outcome } = exchange;
  return (
    <article className="area-exchange" aria-label={`Question: ${exchange.question}`}>
      <p className="area-question">{exchange.question}</p>
      {outcome.outcome === 'no_answer' ? (
        <p className="curated-notice" role="status">{refusalText(outcome.reason)}</p>
      ) : (
        <div className="area-answer">
          <p className="muted">
            Answered by <code>{outcome.role}</code> for {outcome.area_name}, from the area's briefing.
            An agent's reading (T3 / InferredWeak), not a recovered fact.
          </p>
          {outcome.statements.length > 0 && (
            <ol className="document-claims">
              {outcome.statements.map((item, index) => (
                <li key={index}>
                  <span className="document-claim-statement">{item.statement}</span>{' '}
                  <span className="document-kind" title={BASIS_MEANING[item.basis]}>
                    {BASIS_LABEL[item.basis]}
                  </span>{' '}
                  <span className="document-claim-citations">
                    {item.citations.map((id) => (onOpenCitation ? (
                      <button
                        key={id}
                        type="button"
                        className="citation-link"
                        aria-label={`Open evidence for ${id}`}
                        onClick={() => onOpenCitation(id)}
                      >
                        {id}
                      </button>
                    ) : <code key={id}>{id}</code>))}
                  </span>
                </li>
              ))}
            </ol>
          )}
          {outcome.not_known.length > 0 && (
            <div data-testid="area-not-known">
              <p><strong>Not in the briefing</strong></p>
              <ul>
                {outcome.not_known.map((item, index) => <li key={index}>{item}</li>)}
              </ul>
            </div>
          )}
          {outcome.ask_instead.length > 0 && (
            <p data-testid="area-ask-instead">
              Ask the expert of: {outcome.ask_instead.join(', ')}.
            </p>
          )}
          {outcome.left_out > 0 && (
            <p className="muted">
              {outcome.left_out} {outcome.left_out === 1 ? 'statement was' : 'statements were'} left
              out: no citation the briefing prints.
            </p>
          )}
        </div>
      )}
      <p className="muted" data-testid="area-usage">{usageText(outcome.usage)}</p>
    </article>
  );
}

/**
 * Ask the topic expert of one subsystem area (#2057, AC-0932).
 * Presentational: the areas, the briefing and every check of an answer are
 * the host's. An answer is always shown as agent-authored, with the
 * citations the briefing prints.
 */
export function AreaExpertPanel({
  areas, selected, exchanges, busy = false, error = null, canAsk, onReload, onSelect, onAsk,
  onOpenCitation,
}: AreaExpertPanelProps) {
  const [question, setQuestion] = useState('');
  const chosen = areas.find((choice) => choice.area === selected) ?? null;
  const text = question.trim();
  return (
    <section className="curated-section area-expert" aria-labelledby="area-expert-title">
      <header className="curated-section-header">
        <div>
          <h3 id="area-expert-title">Ask an area's expert</h3>
          <p className="muted">
            Each area has an expert that answers from the area's part of the product specification.
            An area is a candidate grouping inferred by rule, not a confirmed boundary. You review
            the exact payload before a question is sent.
          </p>
        </div>
        {onReload && (
          <button type="button" className="secondary-button" disabled={busy} onClick={onReload}>
            Reload areas
          </button>
        )}
      </header>

      {error && <p className="error" role="alert">{error}</p>}

      {areas.length === 0 ? (
        <p className="muted" data-testid="area-expert-empty">
          No areas yet. Areas appear once the knowledge phase has grouped the system's code.
        </p>
      ) : (
        <div className="document-draft-request">
          <label htmlFor="area-expert-area">Area</label>
          <select
            id="area-expert-area"
            value={selected ?? ''}
            disabled={busy}
            onChange={(event) => {
              if (event.target.value) onSelect(event.target.value);
            }}
          >
            <option value="">Choose an area…</option>
            {areas.map((choice) => (
              <option key={choice.area} value={choice.area}>
                {choice.name} ({choice.files} {choice.files === 1 ? 'file' : 'files'})
              </option>
            ))}
          </select>
          {chosen && (
            <p className="muted" data-testid="area-expert-briefing">
              {chosen.sections.length === 0
                ? 'The product specification lists nothing under this area, so its expert has little to answer from.'
                : `Briefing: sections ${chosen.sections.join(', ')}, ${Math.ceil(chosen.briefing_bytes / 1024)} KB.`}
            </p>
          )}
        </div>
      )}

      {exchanges.map((exchange, index) => (
        <Exchange key={index} exchange={exchange} onOpenCitation={onOpenCitation} />
      ))}

      {chosen && (
        <form
          className="document-draft-request"
          onSubmit={(event) => {
            event.preventDefault();
            if (text === '') return;
            onAsk(text);
            setQuestion('');
          }}
        >
          <label htmlFor="area-expert-question">Question for the {chosen.name} expert</label>
          <textarea
            id="area-expert-question"
            rows={3}
            maxLength={2000}
            value={question}
            disabled={busy || !canAsk}
            onChange={(event) => setQuestion(event.target.value)}
          />
          <button type="submit" disabled={busy || !canAsk || text === ''}>
            {busy ? 'Working…' : 'Review and ask'}
          </button>
        </form>
      )}
    </section>
  );
}
