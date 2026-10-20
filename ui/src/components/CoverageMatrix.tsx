import { cellLabel, supportCounts } from '../coverageMatrix';
import type { CoverageCell } from '../store';

const SUPPORT_LABEL: Record<CoverageCell['support'], string> = {
  supported: 'Supported',
  partial: 'Partial',
  unsupported: 'Unsupported',
};

/** One adapter's language × framework × capability cells (AC-0253), folded
 *  to a count line; partial and unsupported cells say what they miss. */
export function CoverageMatrix({ language, cells }: { language: string; cells: CoverageCell[] }) {
  if (cells.length === 0) return null;
  const counts = supportCounts(cells);
  return (
    <details className="coverage-matrix">
      <summary>
        {counts.supported} supported · {counts.partial} partial · {counts.unsupported} unsupported
      </summary>
      <ul aria-label={`${language} coverage cells`}>
        {cells.map((cell) => (
          <li key={`${cell.framework}.${cell.capability}`}>
            <span className={`gate-chip support-${cell.support}`}>{SUPPORT_LABEL[cell.support]}</span>{' '}
            {cellLabel(cell)}
            {cell.note && <span className="muted"> — {cell.note}</span>}
          </li>
        ))}
      </ul>
    </details>
  );
}
