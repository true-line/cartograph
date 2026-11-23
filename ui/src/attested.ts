import { useCallback, useEffect, useRef, useState } from 'react';
import { invokeOr } from './tauri';

// ADR-0090 (AC-1081): attested records as the Gaps register and the Spec
// Workbench show them. An attested record is not recovered from the system:
// it is what an engineer or an agent states, and it never counts as
// coverage. A Gap it answers stays open.

export type AttestedStanding = 'withdrawn' | 'disputed' | 'engineer_backed' | 'agent_attested_unconfirmed';
export type AttestedTarget =
  | { kind: 'gap'; gap_id: string; slot: string }
  | { kind: 'section'; address: { catalogue_version: string; document: string; section: string; scope: string } };
export interface AttestedContributor {
  display_name: string;
  kind: 'engineer' | 'agent';
  verb: string;
  run: string | null;
}
export interface AttestedView {
  record_id: string;
  citation_id: string;
  target: AttestedTarget;
  statement: string;
  standing: AttestedStanding;
  /** `attested`, `agent-attested, unconfirmed`, `disputed` or `withdrawn`. */
  label: string;
  attestation_count: number;
  contributors: AttestedContributor[];
}
export interface AttestedViewPage { items: AttestedView[]; next_before: number | null }
export type ReviewVerb = 'confirmed' | 'disputed' | 'withdrew';
export type AttestationSource = 'gaps_register' | 'workbench';

const PAGE = 100;

export function loadAttested(before: number | null = null): Promise<AttestedViewPage> {
  return invokeOr<AttestedViewPage>('list_attested_records', { items: [], next_before: null }, { limit: PAGE, before });
}

/** The engineer's review, made at the count the surface read. */
export function reviewAttested(view: AttestedView, verb: ReviewVerb, source: AttestationSource): Promise<unknown> {
  return invokeOr('review_attested_record', null, {
    recordId: view.record_id,
    expectedCount: view.attestation_count,
    verb,
    source,
    reference: null,
  });
}

/** Record the project's engineer (v1: one). The name stays in the
 *  project's contributor table. */
export function setEngineer(displayName: string): Promise<unknown> {
  return invokeOr('set_attested_engineer', null, { displayName, handle: null });
}

/** What the engineer may do with a record. Confirming is offered until the
 *  engineer's latest word states or confirms it; withdrawing only once it
 *  has; disputing until the engineer's latest word disputes it. A withdrawn
 *  record may be confirmed again. */
export function reviewActions(view: AttestedView): ReviewVerb[] {
  const engineer = [...view.contributors].reverse().find((c) => c.kind === 'engineer')?.verb ?? null;
  const backs = engineer === 'stated' || engineer === 'confirmed';
  const actions: ReviewVerb[] = [];
  if (!backs) actions.push('confirmed');
  if (engineer !== 'disputed') actions.push('disputed');
  if (backs) actions.push('withdrew');
  return actions;
}

export const REVIEW_LABEL: Record<ReviewVerb, string> = { confirmed: 'Confirm', disputed: 'Dispute', withdrew: 'Withdraw' };

/** One line naming each contributor and what they did, such as
 *  `domain-analyst@29 (local/qwen3) stated · Engineer confirmed`. */
export function contributorLine(view: AttestedView): string {
  return view.contributors.map((c) => `${c.display_name} ${c.verb}`).join(' · ');
}

/** The records answering one Gap. */
export function answersOf(records: AttestedView[], gapId: string): AttestedView[] {
  return records.filter((r) => r.target.kind === 'gap' && r.target.gap_id === gapId);
}

function words(caught: unknown): string {
  return caught instanceof Error ? caught.message : String(caught);
}

export interface AttestedState {
  records: AttestedView[];
  busy: boolean;
  error: string | null;
  /** A review was refused because the project has no engineer yet. */
  needsEngineer: boolean;
  reload: () => void;
  review: (view: AttestedView, verb: ReviewVerb) => void;
  setEngineer: (displayName: string) => void;
}

/** The records of the project, read when `active`, and the engineer's
 *  review from `source`. A review reads the records again; a refused one,
 *  such as a stale count, is shown as an error. */
export function useAttested(active: boolean, source: AttestationSource): AttestedState {
  const [records, setRecords] = useState<AttestedView[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);

  // A newer read wins over one still in flight.
  const load = useCallback(() => {
    const mine = ++generation.current;
    loadAttested()
      .then((page) => {
        if (mine !== generation.current) return;
        setRecords(page.items);
        setError(null);
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

  const review = useCallback((view: AttestedView, verb: ReviewVerb) => {
    setBusy(true);
    reviewAttested(view, verb, source)
      .then(() => load())
      // A refused review, such as a stale count, changes nothing.
      .catch((caught: unknown) => {
        setError(words(caught));
        setBusy(false);
      });
  }, [load, source]);

  const recordEngineer = useCallback((displayName: string) => {
    setBusy(true);
    setEngineer(displayName)
      .then(() => setError(null))
      .catch((caught: unknown) => setError(words(caught)))
      .finally(() => setBusy(false));
  }, []);

  return { records, busy, error, needsEngineer: error === 'no_engineer', reload, review, setEngineer: recordEngineer };
}
