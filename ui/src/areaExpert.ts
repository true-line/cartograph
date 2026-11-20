// The topic expert (#2057, AC-0932): one question about one subsystem area,
// answered in one bounded call from that area's compiled briefing. The host
// compiles the briefing, shows the exact payload for consent and checks the
// reply; nothing here writes the graph.
import { useCallback, useEffect, useRef, useState } from 'react';
import type { EgressPreview } from './components/EgressConsentDialog';
import { invokeOr } from './tauri';

/** An area a question can be asked of. */
export interface AreaChoice {
  area: string;
  name: string;
  files: number;
  sections: string[];
  briefing_bytes: number;
  /** A digest of the briefing's text; a conversation holds while it is unchanged. */
  briefing_digest: string;
}

export interface ExpertStatement {
  statement: string;
  basis: 'stated' | 'inferred';
  citations: string[];
}

/** What one call used, as the provider reported it. Not verified billing. */
export interface AskUsage {
  model: string;
  input_tokens: number | null;
  output_tokens: number | null;
  request_bytes: number;
}

/** One line on what a call used. */
export function usageText(usage: AskUsage): string {
  const tokens = usage.input_tokens === null || usage.output_tokens === null
    ? `${Math.ceil(usage.request_bytes / 1024)} KB sent; the provider reported no token count`
    : `${usage.input_tokens.toLocaleString('en-US')} input and ${usage.output_tokens.toLocaleString('en-US')} output tokens, as the provider reported`;
  return `${usage.model}: ${tokens}.`;
}

/** What asking produced: an answer, or the closed reason there is none. */
export type AskOutcome =
  | {
      outcome: 'answered';
      area: string;
      area_name: string;
      role: string;
      job_id: string;
      usage: AskUsage;
      statements: ExpertStatement[];
      not_known: string[];
      ask_instead: string[];
      left_out: number;
    }
  | { outcome: 'no_answer'; reason: string; usage: AskUsage };

/** One exchange shown in the panel. */
export interface AreaExchange {
  question: string;
  outcome: AskOutcome;
}

/** An earlier exchange as the host sends it back to the expert. */
export interface ExpertTurn {
  question: string;
  answer: string;
}

/** Why there is no answer, by the host's closed code. */
export const ASK_REFUSAL: Record<string, string> = {
  reply_not_the_schema: 'The model did not reply with the answer schema. Nothing is shown.',
  no_admissible_statement: 'No statement in the reply cited the briefing, so none is shown.',
  question_not_admissible: 'A question is plain text of at most 2,048 bytes.',
  area_not_found: 'This area is no longer in the knowledge overlay. Reload the areas.',
};

/** The reason in words; an unknown code is kept as sent. */
export function refusalText(reason: string): string {
  return ASK_REFUSAL[reason] ?? reason;
}

/** An answer as the plain text sent back with the next question. */
export function answerText(outcome: AskOutcome): string {
  if (outcome.outcome !== 'answered') return '';
  return [
    ...outcome.statements.map((item) => item.statement),
    ...outcome.not_known.map((item) => `Not known: ${item}`),
    ...(outcome.ask_instead.length > 0 ? [`Ask instead: ${outcome.ask_instead.join(', ')}`] : []),
  ].join('\n');
}

/** The answered exchanges as turns, oldest first. */
export function turnsOf(exchanges: AreaExchange[]): ExpertTurn[] {
  return exchanges
    .filter((exchange) => exchange.outcome.outcome === 'answered')
    .map((exchange) => ({ question: exchange.question, answer: answerText(exchange.outcome) }));
}

export async function loadAreaChoices(): Promise<AreaChoice[]> {
  return invokeOr<AreaChoice[]>('area_choices', []);
}

/** The exact payload a cloud answer to the question would send. */
export async function areaAskPreview(
  area: string, question: string, turns: ExpertTurn[],
): Promise<EgressPreview> {
  const preview = await invokeOr<EgressPreview | null>('area_ask_preview', null, { area, question, turns });
  if (!preview) throw new Error('The core did not return a preview of the question.');
  return preview;
}

/** Ask in the cloud for the payload the user approved. */
export async function runAreaAsk(
  area: string, question: string, turns: ExpertTurn[], preview: EgressPreview,
): Promise<AskOutcome> {
  const outcome = await invokeOr<AskOutcome | null>('run_area_ask', null, {
    area, question, turns, mode: 'cloud', approvedPayloadHash: preview.payload_hash,
  });
  if (!outcome) throw new Error('The core did not return an answer.');
  return outcome;
}

function words(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  return refusalText(text);
}

/** Whether a conversation with `selected` still holds after the areas were
 *  read again: the area is still there and its briefing is the same text.
 *  Earlier answers rest on the briefing they were given. */
export function conversationHolds(
  before: AreaChoice[], after: AreaChoice[], selected: string | null,
): boolean {
  const was = before.find((choice) => choice.area === selected);
  const now = after.find((choice) => choice.area === selected);
  return Boolean(now) && (!was || was.briefing_digest === now?.briefing_digest);
}

export interface AreaExpertState {
  areas: AreaChoice[];
  selected: string | null;
  exchanges: AreaExchange[];
  busy: boolean;
  error: string | null;
  /** The payload awaiting consent, with the question it carries. */
  pending: { question: string; preview: EgressPreview } | null;
  reload: () => void;
  select: (area: string) => void;
  ask: (question: string) => void;
  consent: (preview: EgressPreview) => void;
  cancel: () => void;
}

/** The panel's state. A conversation belongs to one area: choosing another
 *  area starts a new one. */
export function useAreaExpert(active: boolean): AreaExpertState {
  const [areas, setAreas] = useState<AreaChoice[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [exchanges, setExchanges] = useState<AreaExchange[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<AreaExpertState['pending']>(null);
  const generation = useRef(0);
  const known = useRef<AreaChoice[]>([]);

  // A newer read wins over one still in flight.
  const load = useCallback(() => {
    const mine = ++generation.current;
    loadAreaChoices()
      .then((loaded) => {
        if (mine !== generation.current) return;
        const before = known.current;
        known.current = loaded;
        setAreas(loaded);
        setError(null);
        setSelected((current) => {
          const there = loaded.some((choice) => choice.area === current);
          if (!conversationHolds(before, loaded, current)) setExchanges([]);
          return there ? current : null;
        });
      })
      .catch((caught: unknown) => {
        if (mine === generation.current) setError(words(caught));
      })
      .finally(() => {
        if (mine === generation.current) setBusy(false);
      });
  }, []);
  const reload = useCallback(() => {
    setBusy(true);
    load();
  }, [load]);
  useEffect(() => {
    if (active) load();
  }, [active, load]);

  const select = useCallback((area: string) => {
    setSelected(area);
    setExchanges([]);
    setError(null);
  }, []);

  const ask = useCallback((question: string) => {
    if (!selected) return;
    setError(null);
    setBusy(true);
    areaAskPreview(selected, question, turnsOf(exchanges))
      .then((preview) => setPending({ question, preview }))
      .catch((caught: unknown) => setError(words(caught)))
      .finally(() => setBusy(false));
  }, [selected, exchanges]);

  const consent = useCallback((preview: EgressPreview) => {
    if (!selected || !pending) return;
    const { question } = pending;
    const area = selected;
    setPending(null);
    setBusy(true);
    runAreaAsk(area, question, turnsOf(exchanges), preview)
      .then((outcome) => setExchanges((current) => [...current, { question, outcome }]))
      .catch((caught: unknown) => setError(words(caught)))
      .finally(() => setBusy(false));
  }, [selected, pending, exchanges]);

  const cancel = useCallback(() => setPending(null), []);

  return { areas, selected, exchanges, busy, error, pending, reload, select, ask, consent, cancel };
}
