// #227 (AC-1711): the same pinned ingest on two platforms is compared by its
// recorded hashes, and a difference names the first differing fact. Pure.
import test from 'node:test';
import assert from 'node:assert/strict';

import { compare, firstDifferingFact, parseFacts, parseRecord, readRecord } from './graph-parity.mjs';

const summary = (overrides = {}) => JSON.stringify({ pinned_commit: 'abc', platform: 'macos-aarch64', graph_hash: 'h1', recorded_hash: 'r1', nodes: 2, edges: 1, ...overrides });
const files = (entries) => (file) => entries[file.replaceAll('\\', '/')] ?? null;
const FACTS = 'edge a -[CALLS]-> b\td1\nnode a\td2\nnode b\td3\n';

test('a record is label and directory; a Windows path keeps its colon and equals signs', () => {
  assert.deepEqual(parseRecord('windows=C:\\temp\\a=b'), { label: 'windows', directory: 'C:\\temp\\a=b' });
  assert.throws(() => parseRecord('windows'), /label/u);
  assert.throws(() => parseRecord('windows='), /label/u);
});

test('fact lines split on the last tab', () => {
  assert.deepEqual(parseFacts('node a\tb\td1\nnode c\td2\n'), [['node a\tb', 'd1'], ['node c', 'd2']]);
});

test('the first differing fact is named, in byte order of the keys', () => {
  const base = parseFacts(FACTS);
  assert.equal(firstDifferingFact(base, parseFacts(FACTS)), null);
  assert.equal(firstDifferingFact(base, parseFacts(FACTS.replace('d2', 'dx')), ['macOS', 'Windows']), 'node a differs between macOS and Windows');
  assert.equal(firstDifferingFact(base, parseFacts('edge a -[CALLS]-> b\td1\nnode b\td3\n'), ['macOS', 'Windows']), 'node a only on macOS');
  assert.equal(firstDifferingFact(parseFacts('node b\td3\n'), base, ['macOS', 'Windows']), 'edge a -[CALLS]-> b only on Windows');
  // "Z" sorts before "a" as bytes, as Rust's sort has it.
  assert.equal(firstDifferingFact(parseFacts('node Z\td\nnode a\td\n'), parseFacts('node a\td\n'), ['macOS', 'Windows']), 'node Z only on macOS');
});

test('platforms that agree pass and state the shared hash', () => {
  const read = files({ 'mac/graph-hash.json': summary(), 'mac/facts.tsv': FACTS, 'win/graph-hash.json': summary({ platform: 'windows-x86_64' }), 'win/facts.tsv': FACTS });
  const result = compare([readRecord({ label: 'macOS', directory: 'mac' }, read), readRecord({ label: 'Windows', directory: 'win' }, read)]);
  assert.equal(result.passed, true);
  assert.match(result.text, /Every platform yields graph hash `h1`\./u);
  assert.match(result.text, /\| Windows \| windows-x86_64 \| `abc` \| 2 \| 1 \| `h1` \|/u);
});

test('a differing hash fails and names the first differing fact', () => {
  const read = files({ 'mac/graph-hash.json': summary(), 'mac/facts.tsv': FACTS, 'win/graph-hash.json': summary({ graph_hash: 'h2' }), 'win/facts.tsv': FACTS.replace('d3', 'dx') });
  const result = compare([readRecord({ label: 'macOS', directory: 'mac' }, read), readRecord({ label: 'Windows', directory: 'win' }, read)]);
  assert.equal(result.passed, false);
  assert.match(result.text, /macOS and Windows differ in graph_hash\. First differing fact: node b differs between macOS and Windows\./u);
});

test('a platform that recorded nothing, or another commit, cannot pass', () => {
  const read = files({ 'mac/graph-hash.json': summary(), 'mac/facts.tsv': FACTS, 'other/graph-hash.json': summary({ pinned_commit: 'def' }), 'bad/graph-hash.json': '{', 'thin/graph-hash.json': '{"graph_hash":"h1"}' });
  const mac = readRecord({ label: 'macOS', directory: 'mac' }, read);
  const missing = compare([mac, readRecord({ label: 'Windows', directory: 'win' }, read)]);
  assert.equal(missing.passed, false);
  assert.match(missing.text, /Windows: no graph-hash\.json: the platform recorded nothing\./u);
  assert.equal(compare([readRecord({ label: 'Windows', directory: 'win' }, read), mac]).passed, false);
  const moved = compare([mac, readRecord({ label: 'Windows', directory: 'other' }, read)]);
  assert.equal(moved.passed, false);
  assert.match(moved.text, /ingested different commits/u);
  assert.match(readRecord({ label: 'x', directory: 'bad' }, read).error, /not JSON/u);
  assert.match(readRecord({ label: 'x', directory: 'thin' }, read).error, /lacks pinned_commit, recorded_hash, nodes, edges/u);
  assert.throws(() => compare([mac]), /at least two/u);
});
