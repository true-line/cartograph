import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

/** What the knowledge phase did after an ingest (`KnowledgeReport` in the core). */
export interface KnowledgeReport {
  embedded: number;
  /** Units past the unit cap. */
  omitted: number;
  /** Named reason nothing was embedded, or the base could not be read. */
  omission: string | null;
  derived: number;
  /** Producer candidates dropped by their size cap. */
  oversized: number;
  /** Named reason the derived nodes were not published. */
  derivedOmission: string | null;
  /** Producers that yielded nothing, by named reason. */
  producerOmissions: string[];
  published: boolean;
}

/** The most recent phase outcome: a report, or the failure that prevented one. */
export interface KnowledgeStatus {
  report: KnowledgeReport | null;
  error: string | null;
}

/** Plain wording for each named reason the core can report. */
const REASONS: Record<string, string> = {
  'knowledge-base-bounds':
    'The graph is larger than the knowledge phase can read. No capabilities, domain entities, actors or business flows were produced, and nothing was embedded.',
  'semantic-tier-disabled':
    'Nothing was embedded because the Semantic tier (T2) is off in Settings. Capabilities, domain entities, actors and business flows were still produced.',
  'embedding-provider-unavailable':
    'Nothing was embedded because the local embedding model is not installed.',
  'embedding-provider-failed':
    'Nothing was embedded because the local embedding provider failed.',
  'knowledge-derived-rejected':
    'The capabilities, domain entities, actors and business flows were too large to publish together, so none were published.',
};

function reason(code: string, subject: string): string {
  return REASONS[code] ?? `${subject} was skipped (${code}).`;
}

/**
 * One line per thing the phase skipped or left out, each with its named
 * reason code. Empty when the phase did everything, or when a newer ingest
 * superseded it and another pass follows.
 */
export function knowledgePhaseLines(status: KnowledgeStatus | null): { code: string; text: string }[] {
  if (!status) return [];
  if (status.error) return [{ code: 'knowledge-phase-failed', text: `The knowledge phase failed: ${status.error}` }];
  const report = status.report;
  if (!report) return [];
  const lines: { code: string; text: string }[] = [];
  if (report.omission) lines.push({ code: report.omission, text: reason(report.omission, 'Embedding') });
  if (report.derivedOmission) lines.push({ code: report.derivedOmission, text: reason(report.derivedOmission, 'Publishing the derived nodes') });
  for (const code of report.producerOmissions) lines.push({ code, text: reason(code, 'One knowledge producer') });
  if (report.omitted > 0) {
    lines.push({ code: 'embedding-unit-cap', text: `${report.omitted} unit${report.omitted === 1 ? ' was' : 's were'} past the embedding cap and not embedded.` });
  }
  if (report.oversized > 0) {
    lines.push({ code: 'knowledge-oversized', text: `${report.oversized} candidate node${report.oversized === 1 ? ' was' : 's were'} too large and left out.` });
  }
  return lines;
}

export interface KnowledgePhaseNoticePanelProps {
  status: KnowledgeStatus | null;
  onDismiss: () => void;
}

/**
 * Shell notice for the post-ingest knowledge phase (#1782). Shown only when
 * the phase skipped or left something out, so a skipped phase no longer looks
 * the same as one that ran. Each line carries the core's reason code.
 */
export function KnowledgePhaseNoticePanel({ status, onDismiss }: KnowledgePhaseNoticePanelProps) {
  const lines = knowledgePhaseLines(status);
  if (lines.length === 0) return null;
  return (
    <section className="knowledge-phase-notice" role="status" aria-label="Knowledge phase">
      <strong>Knowledge phase: some work was skipped</strong>
      <ul>
        {lines.map((line) => (
          <li key={line.code} data-reason={line.code}>{line.text} <code>{line.code}</code></li>
        ))}
      </ul>
      <button type="button" className="secondary-button" onClick={onDismiss}>Dismiss</button>
    </section>
  );
}

function signature(status: KnowledgeStatus | null): string {
  return knowledgePhaseLines(status).map((line) => line.text).join('|');
}

/**
 * The notice bound to the core: follows the `knowledge-phase` event and reads
 * the last outcome once the listener is in place, so an outcome that arrives
 * while the window opens is not lost. A dismissal is held in this window only
 * and ends as soon as an outcome with different lines arrives, so the same
 * reason reported again after a clean phase is shown again.
 */
export function KnowledgePhaseNotice({ available }: { available: boolean }) {
  const [status, setStatus] = useState<KnowledgeStatus | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(null);
  useEffect(() => {
    if (!available) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let heardEvent = false;
    const receive = (next: KnowledgeStatus | null) => {
      setStatus(next);
      setDismissed((held) => (held !== null && held !== signature(next) ? null : held));
    };
    void (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        const stop = await listen<KnowledgeStatus>('knowledge-phase', (event) => {
          heardEvent = true;
          receive(event.payload);
        });
        if (disposed) stop();
        else unlisten = stop;
      } catch {
        // No event bridge available; the retained outcome is still read below.
      }
      if (disposed) return;
      try {
        const last = await invoke<KnowledgeStatus | null>('knowledge_phase_status');
        // An event heard since subscribing is newer than this read.
        if (!disposed && !heardEvent) receive(last);
      } catch {
        // An older core has no such command; the event still arrives.
      }
    })();
    return () => { disposed = true; unlisten?.(); };
  }, [available]);
  const current = signature(status);
  if (!available || current === dismissed) return null;
  return <KnowledgePhaseNoticePanel status={status} onDismiss={() => setDismissed(current)} />;
}
