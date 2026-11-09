import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { analyst27Correction } from './completionFixtures';
import { InvestigationFindings } from './components/InvestigationFindings';

describe('analyst27 corrected answer rendering (AC-0574)', () => {
  it.each([false, true])('renders admitted survivors with withholding=%s', (partial) => {
    const { result } = analyst27Correction(partial);
    const html = renderToStaticMarkup(createElement(InvestigationFindings, {
      result, citations: [], citationId: null, read: null, loading: false, error: null,
      readOnly: true, onRead: () => {}, onClose: () => {},
    }));
    expect(html.includes('Some findings were withheld')).toBe(partial);
    expect(html).toContain('InferredWeak');
    for (const finding of result.findings) expect(html).toContain(finding.title);
    expect(html).not.toContain('Use for follow-up');
  });
});
