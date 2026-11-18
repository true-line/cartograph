import { invoke } from '@tauri-apps/api/core';
import { saveExport } from './saveExport';
import type { SpecArtifact, SpecBundle } from './store';
import { inTauri } from './tauri';

export type ReadSpecArtifact = (artifactId: string) => Promise<SpecArtifact | null>;
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

/**
 * Fills in any artifact whose `export_spec` preview was capped (#488) with
 * its full content/assertions before it leaves the app (export or copy).
 *
 * `read_spec_artifact` is a `Result`-typed Tauri command, never `Option`, so
 * a `null` return from `readSpecArtifact` always means the on-demand fetch
 * itself failed — the store's `readSpecArtifact` action already records
 * `specError` in that case — never a legitimate empty result. Returning
 * `null` here tells the caller to abort the whole action instead of
 * silently substituting the capped preview, which would hand out incomplete
 * content/assertions under an action presented as complete (PR #533 review
 * thread).
 */
export async function withFullArtifacts(
  artifacts: SpecArtifact[],
  readSpecArtifact: ReadSpecArtifact,
  reportError?: ReportSpecError,
): Promise<SpecArtifact[] | null> {
  const resolved = await Promise.all(
    artifacts.map(async (artifact) => {
      if (!artifact.content_truncated && !artifact.assertions_truncated) return artifact;
      return readMatching(artifact, readSpecArtifact, reportError);
    }),
  );
  if (resolved.some((artifact) => artifact === null)) return null;
  return resolved as SpecArtifact[];
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

/** One artifact of the preview, by identity only (AC-0242). */
export type ExpectedSpecArtifact = Pick<SpecArtifact, 'id' | 'file_name' | 'schema' | 'content_id'>;

/** The preview the user exports from, without any artifact content: what
 *  `save_spec_bundle_export` checks its full compile against (AC-0740). */
export type ExpectedSpecBundle = Omit<SpecBundle, 'artifacts'> & {
  artifacts: ExpectedSpecArtifact[];
};

export function expectedSpecBundle(bundle: SpecBundle): ExpectedSpecBundle {
  const { artifacts, ...rest } = bundle;
  return {
    ...rest,
    artifacts: artifacts.map(({ id, file_name, schema, content_id }) => ({
      id,
      file_name,
      schema,
      content_id,
    })),
  };
}

/** The bundle file's text. The host's `save_spec_bundle_export` writes the
 *  same bytes (AC-0740); this copy only serves the browser preview, whose
 *  bundles are small. */
export function specBundleFileContent(bundle: SpecBundle, artifacts: SpecArtifact[]): string {
  const files = Object.fromEntries(artifacts.map((artifact) => [artifact.file_name, artifact.content]));
  return JSON.stringify({ ...bundle, artifacts, files }, null, 2);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : typeof error === 'string' ? error : String(error);
}

/**
 * Export bundle. In the desktop app the host compiles the full bundle,
 * refuses it if it is no longer the previewed selection, and streams it to
 * the file the user picks (#1582, AC-0740): only artifact identities cross
 * IPC, and no bundle string is built here — a large repository's bundle is
 * longer than the WebView's maximum string. A browser preview builds the
 * same file itself and aborts, as before, if any truncated artifact's full
 * content couldn't be fetched (`withFullArtifacts`); it never downloads a
 * file built from capped previews.
 */
export async function exportSpecBundle(
  bundle: SpecBundle,
  readSpecArtifact: ReadSpecArtifact,
  reportError: ReportSpecError,
): Promise<void> {
  const suggestedName = `cartograph-spec-${bundle.mode}.json`;
  if (inTauri()) {
    try {
      await invoke<boolean>('save_spec_bundle_export', {
        suggestedName,
        expected: expectedSpecBundle(bundle),
      });
    } catch (error) {
      reportError(errorMessage(error));
    }
    return;
  }
  const artifacts = await withFullArtifacts(bundle.artifacts, readSpecArtifact, reportError);
  if (artifacts === null) return;
  try {
    await saveExport(suggestedName, specBundleFileContent(bundle, artifacts));
  } catch (error) {
    reportError(errorMessage(error));
  }
}
