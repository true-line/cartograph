import type { CoverageCell } from './store';

export type SupportCounts = Record<CoverageCell['support'], number>;

/** Cells per support level, for a one-line summary. */
export function supportCounts(cells: CoverageCell[]): SupportCounts {
  const counts: SupportCounts = { supported: 0, partial: 0, unsupported: 0 };
  for (const cell of cells) counts[cell.support] += 1;
  return counts;
}

/** `gin · HTTP endpoints`; language-level cells read `language · imports`.
 *  `withLanguage` prefixes the language, for cell sets spanning several. */
export function cellLabel(cell: CoverageCell, withLanguage = false): string {
  const capability = cell.capability.replace(/_/g, ' ').replace(/^http /, 'HTTP ');
  const framework = cell.framework === 'core' ? 'language' : cell.framework;
  const label = `${framework} · ${capability}`;
  return withLanguage ? `${cell.language} · ${label}` : label;
}

/** True when the cells name more than one language (e.g. one plugin's). */
export function spansLanguages(cells: CoverageCell[]): boolean {
  return new Set(cells.map((cell) => cell.language.toLowerCase())).size > 1;
}
