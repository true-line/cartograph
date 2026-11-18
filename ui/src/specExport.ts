import type { SpecArtifact } from './store';

export type ReadSpecArtifact = (artifactId: string) => Promise<SpecArtifact | null>;
/** Several full artifacts in one host request (`read_spec_artifacts`,
 *  #1662), in the order asked; `null` when the fetch itself failed. */
export type ReadSpecArtifacts = (artifactIds: string[]) => Promise<SpecArtifact[] | null>;
export type ReportSpecError = (message: string) => void;

/**
 * Whether a fetched full artifact is the one the preview stands for
 * (AC-0242): same id, schema and content identity. A mismatch means the
 * graph or curation changed after the preview was compiled, so the fetched
 * content belongs to a different selection.
 */
export function isSameArtifact(preview: SpecArtifact, full: SpecArtifact): boolean {
  return (
    preview.id === full.id && preview.schema === full.schema && preview.content_id === full.content_id
  );
}

export function staleArtifactMessage(artifact: SpecArtifact): string {
  return `${artifact.file_name} changed since this preview was compiled. Refresh the Spec Workbench and try again.`;
}

async function readMatching(
  preview: SpecArtifact,
  readSpecArtifact: ReadSpecArtifact,
  reportError?: ReportSpecError,
): Promise<SpecArtifact | null> {
  const full = await readSpecArtifact(preview.id);
  if (full === null) return null;
  if (!isSameArtifact(preview, full)) {
    reportError?.(staleArtifactMessage(preview));
    return null;
  }
  return full;
}

export function incompleteArtifactsMessage(): string {
  return 'The full spec artifacts did not match the ones this export asked for. Refresh the Spec Workbench and try again.';
}

function isCapped(artifact: SpecArtifact): boolean {
  return artifact.content_truncated || artifact.assertions_truncated;
}

/**
 * Fills in every artifact whose `export_spec` preview was capped (#488) with
 * its full content/assertions before it leaves the app (export).
 *
 * All capped artifacts are fetched in one `read_spec_artifacts` request, so
 * the host prepares one graph snapshot, flow trace and curation read for
 * them together (#1662, AC-0720); untruncated artifacts are not fetched.
 *
 * `read_spec_artifacts` is a `Result`-typed Tauri command, never `Option`,
 * so a `null` return from `readSpecArtifacts` always means the on-demand
 * fetch itself failed — the store's action already records `specError` in
 * that case — never a legitimate empty result. Returning `null` here tells
 * the caller to abort the whole action instead of silently substituting
 * the capped preview, which would hand out incomplete content/assertions
 * under an action presented as complete (PR #533 review thread). So does a
 * result that is missing, repeats, reorders or adds an artifact, is still
 * truncated, or whose identity differs from its preview (AC-0242).
 */
export async function withFullArtifacts(
  artifacts: SpecArtifact[],
  readSpecArtifacts: ReadSpecArtifacts,
  reportError?: ReportSpecError,
): Promise<SpecArtifact[] | null> {
  const capped = artifacts.filter(isCapped);
  if (capped.length === 0) return artifacts;
  const fetched = await readSpecArtifacts(capped.map((artifact) => artifact.id));
  if (fetched === null) return null;
  if (
    fetched.length !== capped.length ||
    fetched.some((full, index) => full.id !== capped[index].id || isCapped(full))
  ) {
    reportError?.(incompleteArtifactsMessage());
    return null;
  }
  const stale = capped.find((preview, index) => !isSameArtifact(preview, fetched[index]));
  if (stale) {
    reportError?.(staleArtifactMessage(stale));
    return null;
  }
  const full = new Map(capped.map((preview, index) => [preview, fetched[index]]));
  return artifacts.map((artifact) => full.get(artifact) ?? artifact);
}

/**
 * Resolves the artifact whose content should be copied for the "Copy
 * artifact" action — the preview as-is when it was never truncated, or the
 * full artifact fetched on demand. Returns `null` to tell the caller to
 * abort instead of copying the capped preview — see `withFullArtifacts`.
 * A fetched artifact whose identity differs from the preview's is also
 * refused (AC-0242), with `reportError` told why.
 */
export async function resolveArtifactForCopy(
  artifact: SpecArtifact,
  readSpecArtifact: ReadSpecArtifact,
  reportError?: ReportSpecError,
): Promise<SpecArtifact | null> {
  if (!artifact.content_truncated) return artifact;
  return readMatching(artifact, readSpecArtifact, reportError);
}
