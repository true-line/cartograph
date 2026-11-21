// #1762: changelog versions against release tags. Pure; no git.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';

import { changelogVersions, classify, parseRecord, report, RECORD } from './release-tags.mjs';

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
const sha = 'b'.repeat(40);
const HOUR = 3_600_000;

test('changelog versions are read from level-two version headings only', () => {
  const markdown = '# cartograph\n\n## 0.3.0\n\n### Minor Changes\n\n- x\n\n## 0.2.1\n\n## 0.2.0 extra\n';
  assert.deepEqual(changelogVersions(markdown), ['0.3.0', '0.2.1']);
  assert.throws(() => changelogVersions('## 0.1.0\n## 0.1.0\n'), /twice/u);
});

test('a version is tagged, pending inside the window, recorded, or a gap', () => {
  const recorded = parseRecord(JSON.stringify({ causes: { timeout: 'timed out' }, versions: [{ version: '0.2.0', cut_commit: sha, cause: 'timeout' }] }));
  const now = 100 * HOUR;
  const cut = { '0.5.0': now - HOUR, '0.4.0': now - 5 * HOUR };
  const result = classify({
    versions: ['0.5.0', '0.4.0', '0.3.0', '0.2.0', '0.1.0'],
    tags: new Set(['v0.3.0', 'v0.1.0']),
    recorded,
    now,
    windowHours: 3,
    cutAt: (version) => cut[version] ?? null,
  });
  assert.deepEqual(result, { tagged: ['0.3.0', '0.1.0'], pending: ['0.5.0'], recorded: ['0.2.0'], gaps: ['0.4.0'], stale: [] });
  const text = report(result, { windowHours: 3 });
  assert.match(text, /1 version\(s\) have no tag after the handoff window:\*\* v0\.4\.0/u);
  assert.match(text, /tag pending: v0\.5\.0/u);
});

test('a recorded version that is tagged since, or left the changelog, is stale', () => {
  const recorded = parseRecord(JSON.stringify({ causes: { x: 'x' }, versions: [
    { version: '0.2.0', cut_commit: sha, cause: 'x' },
    { version: '0.9.9', cut_commit: sha, cause: 'x' },
  ] }));
  const result = classify({ versions: ['0.2.0'], tags: new Set(['v0.2.0']), recorded, now: 0, windowHours: 3, cutAt: () => null });
  assert.deepEqual(result.stale, ['0.2.0', '0.9.9']);
  assert.match(report(result, { windowHours: 3 }), /Stale record entries/u);
});

test('the record names a full cut commit and a listed cause for every version', () => {
  assert.throws(() => parseRecord(JSON.stringify({ causes: { x: 'x' }, versions: [{ version: '0.1.0', cut_commit: 'abc', cause: 'x' }] })), /full cut_commit/u);
  assert.throws(() => parseRecord(JSON.stringify({ causes: { x: 'x' }, versions: [{ version: '0.1.0', cut_commit: sha, cause: 'y' }] })), /cause listed under causes/u);
  const shipped = parseRecord(readFileSync(path.join(root, RECORD), 'utf8'));
  assert.equal(shipped.size, 17);
  const changelog = new Set(changelogVersions(readFileSync(path.join(root, 'CHANGELOG.md'), 'utf8')));
  for (const version of shipped.keys()) assert.ok(changelog.has(version), version);
});
