import type { CoverageCell } from './store';

export type SupportCounts = Record<CoverageCell['support'], number>;

/** Cells per support level, for a one-line summary. */
export function supportCounts(cells: CoverageCell[]): SupportCounts {
  const counts: SupportCounts = { supported: 0, partial: 0, unsupported: 0 };
  for (const cell of cells) counts[cell.support] += 1;
  return counts;
}

/** `gin · HTTP endpoints`; language-level cells read `language · imports`. */
export function cellLabel(cell: CoverageCell): string {
  const capability = cell.capability.replace(/_/g, ' ').replace(/^http /, 'HTTP ');
  const framework = cell.framework === 'core' ? 'language' : cell.framework;
  return `${framework} · ${capability}`;
}
