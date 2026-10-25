import { createElement, type ReactNode } from 'react';

function findClosingBold(text: string, start: number): number {
  let j = start;
  while (j < text.length) {
    const ch = text[j];
    if (ch === '\\' && j + 1 < text.length && '|*`\\'.includes(text[j + 1])) {
      j += 2;
      continue;
    }
    if (ch === '`') {
      const closeTick = text.indexOf('`', j + 1);
      if (closeTick > j + 1) {
        j = closeTick + 1;
        continue;
      }
    }
    if (text.startsWith('**', j)) {
      return j;
    }
    j += 1;
  }
  return -1;
}

type LinkMatch =
  | { kind: 'link'; label: string; url: string; end: number }
  | { kind: 'fail'; until: number }
  | { kind: 'none' }
  | { kind: 'exhausted' };

/** `[label](target)` at `start`. The target is scanned once, to the first
 *  `)` or the first character after its whitespace-free run. A target that
 *  fails at `until` proves every later target starting before `until` fails
 *  the same way, so callers pass it back as `failUntil` and untrusted text
 *  stays linear (#993 review). */
function matchLink(text: string, start: number, failUntil: number): LinkMatch {
  let j = start + 1;
  while (j < text.length) {
    const ch = text[j];
    if (ch === '\\' && j + 1 < text.length) {
      j += 2;
      continue;
    }
    if (ch === '`') {
      const closeTick = text.indexOf('`', j + 1);
      if (closeTick > j + 1) {
        j = closeTick + 1;
        continue;
      }
    }
    if (ch === '[') {
      return { kind: 'none' };
    }
    if (ch === ']') {
      break;
    }
    j += 1;
  }
  if (j >= text.length || text[j] !== ']' || text[j + 1] !== '(') {
    return { kind: 'none' };
  }
  const targetStart = j + 2;
  if (targetStart < failUntil) {
    return { kind: 'fail', until: failUntil };
  }
  const isSpace = (ch: string) => /\s/u.test(ch);
  let k = targetStart;
  while (k < text.length && isSpace(text[k])) k += 1;
  const urlStart = k;
  while (k < text.length && text[k] !== ')' && !isSpace(text[k])) k += 1;
  const urlEnd = k;
  while (k < text.length && isSpace(text[k])) k += 1;
  if (k >= text.length) {
    return { kind: 'exhausted' };
  }
  const label = text.slice(start + 1, j);
  if (text[k] !== ')' || urlEnd === urlStart || !label) {
    return { kind: 'fail', until: k };
  }
  return { kind: 'link', label, url: text.slice(urlStart, urlEnd), end: k + 1 };
}

function renderInline(text: string, keyPrefix: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let buffer = '';
  let tokenIndex = 0;

  const flushBuffer = () => {
    if (buffer.length > 0) {
      nodes.push(buffer);
      buffer = '';
    }
  };

  // A failed closing search proves no later opener can close either, so stop
  // rescanning: untrusted artifacts must stay linear, not quadratic.
  let boldOpen = true;
  let linkOpen = true;
  let linkFailUntil = -1;
  let i = 0;
  while (i < text.length) {
    const ch = text[i];

    if (ch === '\\' && i + 1 < text.length && '|*`\\'.includes(text[i + 1])) {
      buffer += text[i + 1];
      i += 2;
      continue;
    }

    if (ch === '`') {
      const closeTick = text.indexOf('`', i + 1);
      if (closeTick > i + 1) {
        flushBuffer();
        const key = `${keyPrefix}:c:${tokenIndex++}`;
        nodes.push(<code key={key}>{text.slice(i + 1, closeTick)}</code>);
        i = closeTick + 1;
        continue;
      }
    }

    if (boldOpen && text.startsWith('**', i)) {
      const closeBold = findClosingBold(text, i + 2);
      if (closeBold === -1) boldOpen = false;
      if (closeBold > i + 2) {
        flushBuffer();
        const key = `${keyPrefix}:b:${tokenIndex++}`;
        nodes.push(<strong key={key}>{renderInline(text.slice(i + 2, closeBold), key)}</strong>);
        i = closeBold + 2;
        continue;
      }
      buffer += '**';
      i += 2;
      continue;
    }

    if (linkOpen && ch === '[') {
      const link = matchLink(text, i, linkFailUntil);
      if (link.kind === 'exhausted') linkOpen = false;
      if (link.kind === 'fail') linkFailUntil = link.until;
      if (link.kind === 'link') {
        flushBuffer();
        const key = `${keyPrefix}:l:${tokenIndex++}`;
        nodes.push(
          ...renderInline(link.label, `${key}:t`),
          ' ',
          <code key={`${key}:u`} className="md-link-target">
            {link.url}
          </code>,
        );
        i = link.end;
        continue;
      }
    }

    buffer += ch;
    i += 1;
  }

  flushBuffer();
  return nodes;
}

function splitTableRow(line: string): { cells: string[]; hasPipe: boolean } {
  const trimmed = line.trim();
  const rawCells: string[] = [];
  let current = '';
  let i = 0;
  let pipeCount = 0;
  let startedWithPipe = false;
  let endedWithPipe = false;

  while (i < trimmed.length) {
    const ch = trimmed[i];
    if (ch === '\\' && i + 1 < trimmed.length) {
      current += ch + trimmed[i + 1];
      i += 2;
      endedWithPipe = false;
      continue;
    }
    if (ch === '`') {
      const closeTick = trimmed.indexOf('`', i + 1);
      if (closeTick > i + 1) {
        current += trimmed.slice(i, closeTick + 1);
        i = closeTick + 1;
        endedWithPipe = false;
        continue;
      }
    }
    if (ch === '|') {
      if (i === 0) startedWithPipe = true;
      endedWithPipe = i === trimmed.length - 1;
      pipeCount += 1;
      rawCells.push(current);
      current = '';
      i += 1;
      continue;
    }
    current += ch;
    endedWithPipe = false;
    i += 1;
  }
  rawCells.push(current);

  if (pipeCount === 0) {
    return { cells: [], hasPipe: false };
  }
  if (startedWithPipe) {
    rawCells.shift();
  }
  if (endedWithPipe) {
    rawCells.pop();
  }
  return {
    cells: rawCells.map((cell) => cell.trim()),
    hasPipe: true,
  };
}

function isDelimiterRow(line: string, expectedCols: number): boolean {
  const parsed = splitTableRow(line);
  if (!parsed.hasPipe || parsed.cells.length !== expectedCols || expectedCols === 0) {
    return false;
  }
  return parsed.cells.every((cell) => /^:?-+:?$/u.test(cell));
}

function isFenceStart(line: string): { char: string; len: number } | null {
  const match = line.match(/^ {0,3}(`{3,}|~{3,})(.*)$/u);
  if (!match) return null;
  const fence = match[1];
  const info = match[2];
  if (fence[0] === '`' && info.includes('`')) return null;
  return { char: fence[0], len: fence.length };
}

function isFenceEnd(line: string, fenceChar: string, fenceLen: number): boolean {
  const match = line.match(/^ {0,3}(`{3,}|~{3,})\s*$/u);
  if (!match) return false;
  return match[1][0] === fenceChar && match[1].length >= fenceLen;
}

/** Deterministic, pure Markdown renderer for untrusted spec artifacts (#993).
 *  Everything renders as React elements/text — no HTML injection and no links. */
export function renderArtifactMarkdown(markdown: string): ReactNode[] {
  if (!markdown) return [];

  const lines = markdown.replace(/\r\n/gu, '\n').split('\n');
  const blocks: ReactNode[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  let blockquote: string[][] | null = null;
  let paragraph: string[] = [];

  const flushList = () => {
    if (!list) return;
    const key = `list:${blocks.length}`;
    const items = list.items.map((item, index) => {
      const itemKey = `${key}:li:${index}`;
      return <li key={itemKey}>{renderInline(item, itemKey)}</li>;
    });
    blocks.push(list.ordered ? <ol key={key}>{items}</ol> : <ul key={key}>{items}</ul>);
    list = null;
  };

  const flushBlockquote = () => {
    if (!blockquote) return;
    const nonEmpty = blockquote.filter((part) => part.length > 0);
    if (nonEmpty.length > 0) {
      const key = `bq:${blocks.length}`;
      blocks.push(
        <blockquote key={key}>
          {nonEmpty.map((part, index) => {
            const pKey = `${key}:p:${index}`;
            return <p key={pKey}>{renderInline(part.join(' '), pKey)}</p>;
          })}
        </blockquote>,
      );
    }
    blockquote = null;
  };

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    const key = `p:${blocks.length}`;
    blocks.push(<p key={key}>{renderInline(paragraph.join(' '), key)}</p>);
    paragraph = [];
  };

  let i = 0;
  while (i < lines.length) {
    const line = lines[i];

    if (line.trim() === '') {
      flushList();
      flushBlockquote();
      flushParagraph();
      i += 1;
      continue;
    }

    const fence = isFenceStart(line);
    if (fence) {
      flushList();
      flushBlockquote();
      flushParagraph();
      const codeLines: string[] = [];
      i += 1;
      while (i < lines.length && !isFenceEnd(lines[i], fence.char, fence.len)) {
        codeLines.push(lines[i]);
        i += 1;
      }
      if (i < lines.length) {
        i += 1;
      }
      const key = `code:${blocks.length}`;
      blocks.push(
        <pre key={key} className="md-code">
          <code>{codeLines.join('\n')}</code>
        </pre>,
      );
      continue;
    }

    const headerCandidate = splitTableRow(line);
    if (
      headerCandidate.hasPipe &&
      headerCandidate.cells.length > 0 &&
      i + 1 < lines.length &&
      isDelimiterRow(lines[i + 1], headerCandidate.cells.length)
    ) {
      flushList();
      flushBlockquote();
      flushParagraph();
      const headerCells = headerCandidate.cells;
      const width = headerCells.length;
      const bodyRows: string[][] = [];
      i += 2;
      while (i < lines.length) {
        const rowLine = lines[i];
        if (
          rowLine.trim() === '' ||
          isFenceStart(rowLine) !== null ||
          /^ {0,3}#{1,6}\s+/u.test(rowLine) ||
          /^>\s?/u.test(rowLine) ||
          /^[-*] /u.test(rowLine) ||
          /^\d+\. /u.test(rowLine) ||
          /^ {0,3}(?:-{3,}|\*{3,})\s*$/u.test(rowLine)
        ) {
          break;
        }
        const parsedRow = splitTableRow(rowLine);
        if (!parsedRow.hasPipe) {
          break;
        }
        const normalized = parsedRow.cells.slice(0, width);
        while (normalized.length < width) {
          normalized.push('');
        }
        bodyRows.push(normalized);
        i += 1;
      }
      const key = `table:${blocks.length}`;
      blocks.push(
        <div key={key} className="md-table-wrap">
          <table>
            <thead>
              <tr>
                {headerCells.map((cell, colIndex) => {
                  const thKey = `${key}:th:${colIndex}`;
                  return <th key={thKey}>{renderInline(cell, thKey)}</th>;
                })}
              </tr>
            </thead>
            <tbody>
              {bodyRows.map((row, rowIndex) => (
                <tr key={`${key}:tr:${rowIndex}`}>
                  {row.map((cell, colIndex) => {
                    const tdKey = `${key}:td:${rowIndex}:${colIndex}`;
                    return <td key={tdKey}>{renderInline(cell, tdKey)}</td>;
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>,
      );
      continue;
    }

    const heading = line.match(/^ {0,3}(#{1,6})\s+(.+?)(?:\s+#+\s*)?$/u);
    if (heading) {
      flushList();
      flushBlockquote();
      flushParagraph();
      const mappedLevel = Math.min(6, heading[1].length + 2);
      const tag = `h${mappedLevel}`;
      const key = `h:${blocks.length}`;
      blocks.push(createElement(tag, { key }, renderInline(heading[2].trim(), key)));
      i += 1;
      continue;
    }

    if (paragraph.length === 0 && /^ {0,3}(?:-{3,}|\*{3,})\s*$/u.test(line)) {
      flushList();
      flushBlockquote();
      blocks.push(<hr key={`hr:${blocks.length}`} />);
      i += 1;
      continue;
    }

    const quote = line.match(/^>\s?(.*)$/u);
    if (quote) {
      flushList();
      flushParagraph();
      if (!blockquote) {
        blockquote = [[]];
      }
      const text = quote[1].trim();
      if (text === '') {
        if (blockquote[blockquote.length - 1].length > 0) {
          blockquote.push([]);
        }
      } else {
        blockquote[blockquote.length - 1].push(text);
      }
      i += 1;
      continue;
    }

    const bullet = line.match(/^[-*] (.+)$/u);
    const numbered = line.match(/^\d+\. (.+)$/u);
    const continuation = line.match(/^ {2,}(\S.*)$/u);
    if (bullet || numbered) {
      flushBlockquote();
      flushParagraph();
      const ordered = Boolean(numbered);
      const item = (bullet ?? numbered)![1].trim();
      if (!list || list.ordered !== ordered) {
        flushList();
        list = { ordered, items: [] };
      }
      list.items.push(item);
      i += 1;
      continue;
    }
    if (continuation && list) {
      list.items[list.items.length - 1] += ` ${continuation[1].trim()}`;
      i += 1;
      continue;
    }

    flushList();
    flushBlockquote();
    paragraph.push(line.trim());
    i += 1;
  }

  flushList();
  flushBlockquote();
  flushParagraph();
  return blocks;
}
