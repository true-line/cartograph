import { describe, expect, it } from 'vitest';
import { cellLabel, spansLanguages, supportCounts } from './coverageMatrix';
import type { CoverageCell } from './store';

const cell = (overrides: Partial<CoverageCell> = {}): CoverageCell => ({
  language: 'Go',
  framework: 'gin',
  capability: 'http_endpoints',
  support: 'supported',
  note: '',
  ...overrides,
});

describe('coverage matrix cells (AC-0253)', () => {
  it('counts every support level, including empty ones', () => {
    expect(
      supportCounts([
        cell(),
        cell({ framework: 'chi' }),
        cell({ framework: 'core', capability: 'event_sites', support: 'unsupported' }),
      ]),
    ).toEqual({ supported: 2, partial: 0, unsupported: 1 });
  });

  it('labels framework and language-level cells', () => {
    expect(cellLabel(cell())).toBe('gin · HTTP endpoints');
    expect(cellLabel(cell({ framework: 'core', capability: 'http_client_calls' }))).toBe(
      'language · HTTP client calls',
    );
  });

  it('names the language when one cell set spans several (AC-0263)', () => {
    const ruby = cell({ language: 'Ruby', framework: 'core' });
    const php = cell({ language: 'PHP', framework: 'core', support: 'partial' });
    expect(spansLanguages([ruby, cell({ language: 'ruby' })])).toBe(false);
    expect(spansLanguages([ruby, php])).toBe(true);
    expect(cellLabel(php, true)).toBe('PHP · language · HTTP endpoints');
  });
});
