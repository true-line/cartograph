import { createElement, Fragment } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { SpecWorkbench } from './components/SpecWorkbench';
import { renderArtifactMarkdown } from './markdownView';
import type { SpecArtifact, SpecBundle } from './store';

function toHtml(markdown: string): string {
  return renderToStaticMarkup(createElement(Fragment, null, ...renderArtifactMarkdown(markdown)));
}

describe('renderArtifactMarkdown (#993)', () => {
  it('returns an empty list for an empty string and is deterministic across renders', () => {
    expect(renderArtifactMarkdown('')).toEqual([]);
    expect(toHtml('')).toBe('');

    const sample = '# Title\n\n| A | B |\n|---|---|\n| `1` | **two** |\n';
    expect(toHtml(sample)).toBe(toHtml(sample));
  });

  it('maps ATX heading levels h1..h6 onto h3..h6', () => {
    const html = toHtml(
      ['# H1', '## H2', '### H3', '#### H4', '##### H5', '###### H6'].join('\n\n'),
    );
    expect(html).toBe('<h3>H1</h3><h4>H2</h4><h5>H3</h5><h6>H4</h6><h6>H5</h6><h6>H6</h6>');
  });

  it('renders GFM pipe tables with alignment rows, escaped pipes, pipes inside code spans, and ragged rows', () => {
    const tableHtml = toHtml(
      [
        '|Expr | Details | Notes|',
        '|:---|:--:|---:|',
        '| a \\| b | `c | d` | ok |',
      ].join('\n'),
    );
    expect(tableHtml).toContain('<div class="md-table-wrap"><table>');
    expect(tableHtml).toContain('<thead><tr><th>Expr</th><th>Details</th><th>Notes</th></tr></thead>');
    expect(tableHtml).toContain(
      '<tbody><tr><td>a | b</td><td><code>c | d</code></td><td>ok</td></tr></tbody>',
    );

    const raggedHtml = toHtml(
      [
        '| Col1 | Col2 | Col3 |',
        '|---|---|---|',
        '| only-one |',
        '| one | two | three | extra-dropped |',
      ].join('\n'),
    );
    expect(raggedHtml).toContain('<tr><td>only-one</td><td></td><td></td></tr>');
    expect(raggedHtml).toContain('<tr><td>one</td><td>two</td><td>three</td></tr>');
    expect(raggedHtml).not.toContain('extra-dropped');
  });

  it('renders fenced code blocks verbatim including unclosed fences', () => {
    const closedHtml = toHtml('```ts\n| a |\n# x\n```\n');
    expect(closedHtml).toBe('<pre class="md-code"><code>| a |\n# x</code></pre>');

    const unclosedHtml = toHtml('~~~\nunclosed line 1\n# unclosed line 2');
    expect(unclosedHtml).toBe(
      '<pre class="md-code"><code>unclosed line 1\n# unclosed line 2</code></pre>',
    );
  });

  it('renders unordered and ordered lists with 2+ space continuation lines, blockquotes, and horizontal rules', () => {
    const html = toHtml(
      [
        '- first item',
        '  continued on next line',
        '* second item',
        '',
        '1. step one',
        '   step one detail',
        '2. step two',
        '',
        '---',
        '',
        '> quoted line one',
        '> quoted line two',
      ].join('\n'),
    );
    expect(html).toContain('<ul><li>first item continued on next line</li><li>second item</li></ul>');
    expect(html).toContain('<ol><li>step one step one detail</li><li>step two</li></ol>');
    expect(html).toContain('<hr/>');
    expect(html).toContain('<blockquote><p>quoted line one quoted line two</p></blockquote>');
  });

  it('renders inline bold, code (winning over bold), escapes, and links without anchor or href', () => {
    const html = toHtml(
      'Use **bold**, `code **not bold**`, \\*literal\\*, and [runbook](https://example.com/runbook).',
    );
    expect(html).toContain('<strong>bold</strong>');
    expect(html).toContain('<code>code **not bold**</code>');
    expect(html).toContain('*literal*');
    expect(html).toContain('runbook <code class="md-link-target">https://example.com/runbook</code>');
    expect(html).not.toContain('<a');
    expect(html).not.toContain('href');
  });

  it('escapes raw HTML such as <script> and <img onerror> across text, table cells, and code', () => {
    const untrusted = [
      'Paragraph <script>alert(1)</script> and <img src=x onerror=alert(1)>',
      '',
      '| Header <script>alert(1)</script> | Pic |',
      '|---|---|',
      '| <script>alert(1)</script> | <img src=x onerror=alert(1)> |',
      '',
      ' Inline `<script>alert(1)</script>` and `<img onerror=alert(1)>` ',
      '',
      '```html',
      '<script>alert(1)</script>',
      '<img src=x onerror=alert(1)>',
      '```',
    ].join('\n');

    const html = toHtml(untrusted);
    expect(html).not.toContain('<script');
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;script&gt;alert(1)&lt;/script&gt;');
    expect(html).toContain('&lt;img src=x onerror=alert(1)&gt;');
  });

  it('renders non-diagram markdown artifacts in SpecWorkbench by default and keeps json artifacts as source', () => {
    const mdArtifact: SpecArtifact = {
      id: 'security-view',
      file_name: 'security.md',
      title: 'Security findings',
      format: 'markdown',
      schema: 'cartograph.spec/security.md@1',
      content_id: 'spec-artifact-v1:sec',
      content: '# Security findings\n\n| Finding | Tier |\n|---|---|\n| GET /admin | Confirmed |\n',
      content_truncated: false,
      content_byte_len: 80,
      assertions: [],
      assertions_truncated: false,
      assertions_total: 0,
    };
    const mdBundle: SpecBundle = {
      mode: 'best-effort',
      artifacts: [mdArtifact],
      assertion_count: 0,
      gap_count: 0,
      drift_count: 0,
      security_count: 0,
    };
    const mdHtml = renderToStaticMarkup(
      createElement(SpecWorkbench, {
        bundle: mdBundle,
        mode: 'best-effort',
        decisions: [],
        busy: false,
        error: null,
        canCurate: false,
        onModeChange: () => {},
        onCurate: () => {},
        onCopyArtifact: () => {},
        onExportBundle: () => {},
      }),
    );
    expect(mdHtml).toContain('aria-label="Artifact view"');
    expect(mdHtml).toContain('data-testid="spec-artifact-rendered"');
    expect(mdHtml).not.toContain('data-testid="spec-artifact-source"');
    expect(mdHtml).toContain('<table>');

    const jsonBundle: SpecBundle = {
      ...mdBundle,
      artifacts: [{ ...mdArtifact, id: 'index-json', file_name: 'index.json', format: 'json', content: '{"ok":true}' }],
    };
    const jsonHtml = renderToStaticMarkup(
      createElement(SpecWorkbench, {
        bundle: jsonBundle,
        mode: 'best-effort',
        decisions: [],
        busy: false,
        error: null,
        canCurate: false,
        onModeChange: () => {},
        onCurate: () => {},
        onCopyArtifact: () => {},
        onExportBundle: () => {},
      }),
    );
    expect(jsonHtml).not.toContain('aria-label="Artifact view"');
    expect(jsonHtml).toContain('data-testid="spec-artifact-source"');
    expect(jsonHtml).not.toContain('data-testid="spec-artifact-rendered"');
  });

  it('stays linear on unclosed link targets from untrusted text', () => {
    // Rescanning for `)` after every opener would be quadratic in 200k chars.
    const html = toHtml(`**lone ${'[a]( '.repeat(40000)}`);
    expect(html.startsWith('<p>**lone [a]( [a](')).toBe(true);
    expect(html).not.toContain('md-link-target');
  });
});
