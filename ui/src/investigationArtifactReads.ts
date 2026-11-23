import { useEffect, useState } from 'react';
import { invokeOr } from './tauri';
import type { InvestigationArtifactReadView } from './investigationTypes';

export interface ArtifactReadsState {
  reads: InvestigationArtifactReadView[] | null;
  error: string | null;
}

/** Saved artifact pages for a specialist that offers `read_artifact`;
 * reloaded on each task revision, since a page is saved with its turn. */
export function useArtifactReads(investigationId: string | null, revision: number, enabled: boolean): ArtifactReadsState {
  const key = enabled && investigationId ? `${investigationId}:${revision}` : null;
  const [loaded, setLoaded] = useState<ArtifactReadsState & { key: string | null }>({ key: null, reads: null, error: null });
  useEffect(() => {
    if (!key || !investigationId) return;
    let live = true;
    invokeOr<InvestigationArtifactReadView[]>('investigation_artifact_reads', [], { investigationId })
      .then((reads) => { if (live) setLoaded({ key, reads, error: null }); })
      .catch((error: unknown) => { if (live) setLoaded({ key, reads: null, error: String(error) }); });
    return () => { live = false; };
  }, [key, investigationId]);
  // A stale load for another task or revision reads as still loading.
  return loaded.key === key && key ? { reads: loaded.reads, error: loaded.error } : { reads: null, error: null };
}

/** Plain-language span of one page in content bytes, such as
 * "bytes 0–4,096, continues"; the last page runs to the end. */
export function artifactSpan(read: InvestigationArtifactReadView): string {
  const from = read.offset.toLocaleString('en-US');
  return read.next_offset === null ? `from byte ${from} to the end`
    : `bytes ${from}–${read.next_offset.toLocaleString('en-US')}, continues`;
}
