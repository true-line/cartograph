import type { IngestSource, IngestSummary } from './store';

/** The ingest route of an accepted dropped path (`drop_intake::DropSource`). */
export type DropSource = Extract<IngestSource, 'local' | 'manifest'>;

/** One backend intake decision (`classify_drop`, #587). */
export interface DropCandidate {
  path: string;
  /** Set when accepted. */
  source: DropSource | null;
  /** Set when rejected. */
  reason: string | null;
}

/** Where one dropped path stands. `queued` and `ingesting` are unresolved. */
export type DropStatus = 'rejected' | 'queued' | 'ingesting' | 'ingested' | 'failed';

export interface DropItem {
  path: string;
  source: DropSource | null;
  status: DropStatus;
  /** Why it was rejected or failed. */
  reason: string | null;
  /** `identity@commit` of each repo it ingested (AC-0237). */
  repos: string[];
}

/** Rejection reason for a drop that arrives while intake is running. */
export const BUSY_REASON = 'another intake is still running; drop it again when it finishes';

export function toDropItems(candidates: DropCandidate[]): DropItem[] {
  return candidates.map((candidate) => ({
    path: candidate.path,
    source: candidate.source,
    status: candidate.source ? 'queued' : 'rejected',
    reason: candidate.source ? null : (candidate.reason ?? 'rejected'),
    repos: [],
  }));
}

/** Each repo an ingest recorded, with its commit identity. A local tree's
 *  commit is always `workdir`; a clone names its SHA; a system lists each. */
export function summaryRepos(summary: IngestSummary | null): string[] {
  if (!summary) return [];
  if (summary.repos?.length) return summary.repos;
  if (!summary.repo) return [];
  return [`${summary.repo}@${summary.commit_sha ? summary.commit_sha.slice(0, 12) : 'workdir'}`];
}

export interface DropSetStatus {
  /** True only when every dropped path ingested. */
  complete: boolean;
  /** Paths still queued or ingesting. */
  unresolved: number;
  label: string;
}

/** The set's honest status: never "all ingested" while any path is
 *  unresolved, rejected or failed (AC-0237). */
export function dropSetStatus(items: DropItem[]): DropSetStatus {
  const count = (status: DropStatus) => items.filter((item) => item.status === status).length;
  const ingested = count('ingested');
  const unresolved = count('queued') + count('ingesting');
  const rejected = count('rejected');
  const failed = count('failed');
  const complete = items.length > 0 && ingested === items.length;
  if (complete) {
    return {
      complete,
      unresolved,
      label: items.length === 1 ? 'Dropped folder ingested' : `All ${items.length} dropped folders ingested`,
    };
  }
  const parts = [`${ingested} of ${items.length} ingested`];
  if (unresolved) parts.push(`${unresolved} in progress`);
  if (rejected) parts.push(`${rejected} rejected`);
  if (failed) parts.push(`${failed} failed`);
  return { complete, unresolved, label: parts.join(' · ') };
}
