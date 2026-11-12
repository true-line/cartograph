import type { CuratedRecordState, CuratedSectionRead } from './store';

/** The state the host gave one accepted record, with its reason sentence. */
export interface CuratedRecordStatus {
  state: CuratedRecordState;
  reason_sentence: string;
}

/**
 * The state of each record a curated read returned, by record identity
 * (SPEC-12 slice 3, AC-0655). Only what the host returned is here: a
 * `verified-only` read returns no curated item, so an eligible record has no
 * entry in that mode, and a read that was not computed has none at all.
 */
export function curatedRecordStatuses(
  read: CuratedSectionRead | null,
): ReadonlyMap<string, CuratedRecordStatus> {
  const statuses = new Map<string, CuratedRecordStatus>();
  if (read?.status !== 'computed') return statuses;
  for (const record of [...read.items, ...read.listed]) {
    statuses.set(record.record_id, { state: record.state, reason_sentence: record.reason_sentence });
  }
  return statuses;
}
