import { invoke } from '@tauri-apps/api/core';
import { saveExport } from './saveExport';
import type { SpecArtifact, SpecAssertion, SpecBundle } from './store';
import { inTauri } from './tauri';

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

/** The `schema` the bundle file opens with (ADR-0086, v3). */
export const BUNDLE_FILE_SCHEMA_V3 = 'cartograph.spec-bundle-file/v3';

/** Artifacts the bundle file carries as a rendering of another artifact in
 *  the bundle (ADR-0086 v3): the spec crate's reader renders them back.
 *  Mirrors `RENDERED` in `crates/spec/src/bundle_file.rs`. */
const RENDERED: ReadonlyArray<{ id: string; from: string }> = [{ id: 'fact-index', from: 'fact-index-json' }];

/** An assertion as the v2 file lists it: `ref` for an id that is `node:` or
 *  `edge:` and its subject id, and evidence naming its source by index. The
 *  keys are in the spec crate's order, so the line is byte for byte its. */
function assertionLine(assertion: SpecAssertion, sourceIndex: (repo: string, commit: string) => number): string {
  const derived = (['node', 'edge'] as const).find((ref) => assertion.id === `${ref}:${assertion.subject_id}`);
  const provenance = assertion.provenance as SpecAssertion['provenance'] & { basis?: unknown };
  const line: Record<string, unknown> = derived ? { ref: derived } : { id: assertion.id };
  line.subject_id = assertion.subject_id;
  line.subject_kind = assertion.subject_kind;
  line.summary = assertion.summary;
  const prov: Record<string, unknown> = {
    tier: provenance.tier,
    confidence_tier: provenance.confidence_tier,
    evidence: provenance.evidence.map((row) => ({
      source: sourceIndex(row.repo, row.commit_sha),
      path: row.path,
      byte_start: row.byte_start,
      byte_end: row.byte_end,
    })),
    extractor_id: provenance.extractor_id,
    content_hash: provenance.content_hash,
  };
  if (provenance.method != null && provenance.method.kind !== 'unrecorded') prov.method = provenance.method;
  if (provenance.basis != null) prov.basis = provenance.basis;
  line.provenance = prov;
  if (assertion.edge_label != null) line.edge_label = assertion.edge_label;
  if (assertion.omission != null) line.omission = assertion.omission;
  return JSON.stringify(line);
}

/** A list of entries, one per line (ADR-0086 decision 6). */
function lines(entries: string[]): string {
  return entries.length === 0 ? '[]' : `[\n${entries.join(',\n')}\n]`;
}

/** The bundle file's text (ADR-0086, v3). The host's `save_spec_bundle_export`
 *  writes the same bytes (AC-0740); this copy only serves the browser
 *  preview, whose bundles are small. Content is only in `files`, each
 *  evidence source once in `sources`, each assertion once in `assertions`;
 *  two artifacts with one file name are refused. */
export function specBundleFileContent(bundle: SpecBundle, artifacts: SpecArtifact[]): string {
  const sources: string[] = [];
  const sourceIds = new Map<string, number>();
  const sourceIndex = (repo: string, commit: string): number => {
    const key = JSON.stringify([repo, commit]);
    let index = sourceIds.get(key);
    if (index === undefined) {
      index = sources.length;
      sources.push(JSON.stringify({ repo, commit_sha: commit }));
      sourceIds.set(key, index);
    }
    return index;
  };
  const assertions: string[] = [];
  const listed = new Map<string, number>();
  const names = new Set<string>();
  const artifactLines = artifacts.map((artifact) => {
    if (names.has(artifact.file_name)) {
      throw new Error(`two artifacts share the file name ${JSON.stringify(artifact.file_name)}`);
    }
    names.add(artifact.file_name);
    const indexes = artifact.assertions.map((assertion) => {
      const line = assertionLine(assertion, sourceIndex);
      let index = listed.get(line);
      if (index === undefined) {
        index = assertions.length;
        assertions.push(line);
        listed.set(line, index);
      }
      return index;
    });
    const { id, file_name, title, format, schema, content_id } = artifact;
    return JSON.stringify({ id, file_name, title, format, schema, content_id, assertions: indexes });
  });
  const head = [
    `{"schema":${JSON.stringify(BUNDLE_FILE_SCHEMA_V3)}`,
    `"mode":${JSON.stringify(bundle.mode)}`,
    `"assertion_count":${bundle.assertion_count}`,
    `"gap_count":${bundle.gap_count}`,
    `"drift_count":${bundle.drift_count}`,
    `"security_count":${bundle.security_count}`,
  ];
  if (bundle.graph_basis != null) head.push(`"graph_basis":${JSON.stringify(bundle.graph_basis)}`);
  if (bundle.curated_basis != null) head.push(`"curated_basis":${JSON.stringify(bundle.curated_basis)}`);
  // `files` is written entry by entry: a JS object would move array-index
  // names ahead of the rest, and the file keeps the artifacts' order.
  const files = artifacts.map((artifact) => {
    const rule = RENDERED.find((r) => r.id === artifact.id);
    const from = rule && artifacts.find((other) => other.id === rule.from);
    const value = from ? `{"rendered_from":${JSON.stringify(from.file_name)}}` : JSON.stringify(artifact.content);
    return `${JSON.stringify(artifact.file_name)}:${value}`;
  });
  return [
    ...head,
    `"sources":${lines(sources)}`,
    `"artifacts":${lines(artifactLines)}`,
    `"assertions":${lines(assertions)}`,
    `"files":{${files.length === 0 ? '' : `\n${files.join(',\n')}\n`}}}\n`,
  ].join(',\n');
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
  readSpecArtifacts: ReadSpecArtifacts,
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
  const artifacts = await withFullArtifacts(bundle.artifacts, readSpecArtifacts, reportError);
  if (artifacts === null) return;
  try {
    await saveExport(suggestedName, specBundleFileContent(bundle, artifacts));
  } catch (error) {
    reportError(errorMessage(error));
  }
}
