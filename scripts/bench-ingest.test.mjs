// AC-0423: manifest, timing, summary and comparison are pure; no Cargo, git or showcase runs.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile, readFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { compare, parseManifest, parseTime, summarize } from './bench-ingest.mjs';

const sha = 'a'.repeat(40);

test('the shipped manifest pins every repository to a full commit', async () => {
  const repos = parseManifest(await readFile(new URL('../bench/repos.toml', import.meta.url), 'utf8'));
  assert.deepEqual(repos.map((repo) => repo.name), ['simplelogin', 'rea', 'vendure']);
});

test('manifest entries need a name, credential-free https url and exact commit', () => {
  assert.deepEqual(parseManifest(`# pins\n[[repo]]\nname = "one"\nurl = "https://example.com/one"\ncommit = "${sha}" # exact\n`),
    [{ name: 'one', url: 'https://example.com/one', commit: sha }]);
  assert.deepEqual(parseManifest(`[[repo]]\nname = "one"\nurl = "https://example.com/one"\ncommit = "${sha}"\npath = "packages/core"\n`)[0].path,
    'packages/core');
  for (const bad of [
    `[[repo]]\nname = "one"\nurl = "https://example.com/one"\ncommit = "main"\n`,
    `[[repo]]\nname = "one"\nurl = "https://user:token@example.com/one"\ncommit = "${sha}"\n`,
    `[[repo]]\nname = "one"\nurl = "http://example.com/one"\ncommit = "${sha}"\n`,
    `[[repo]]\nname = "One"\nurl = "https://example.com/one"\ncommit = "${sha}"\n`,
    `[[repo]]\nname = "one"\nurl = "https://example.com/one"\ncommit = "${sha}"\nbranch = "main"\n`,
    `name = "one"\n`,
    ...['/abs', '../up', 'a/../b', 'a/./b', 'a//b', 'a/', ''].map((bad) =>
      `[[repo]]\nname = "one"\nurl = "https://example.com/one"\ncommit = "${sha}"\npath = "${bad}"\n`),
    `[[repo]]\nname = "one"\nurl = "https://example.com/one"\ncommit = "${sha}"\n[[repo]]\nname = "one"\nurl = "https://example.com/two"\ncommit = "${sha}"\n`,
  ]) assert.throws(() => parseManifest(bad), undefined, bad);
});

test('timing is read from macOS and Linux /usr/bin/time output', () => {
  assert.deepEqual(parseTime('      147.51 real       161.28 user        22.63 sys\n          5099094016  maximum resident set size\n          9818546840  peak memory footprint\n', 'darwin'),
    { wall_seconds: 147.51, max_rss_bytes: 5099094016, peak_footprint_bytes: 9818546840 });
  assert.deepEqual(parseTime('\tElapsed (wall clock) time (h:mm:ss or m:ss): 1:02:03.5\n\tMaximum resident set size (kbytes): 2048\n', 'linux'),
    { wall_seconds: 3723.5, max_rss_bytes: 2097152, peak_footprint_bytes: null });
  assert.deepEqual(parseTime('\tElapsed (wall clock) time (h:mm:ss or m:ss): 2:27.51\n', 'linux'),
    { wall_seconds: 147.51, max_rss_bytes: null, peak_footprint_bytes: null });
  assert.deepEqual(parseTime('', 'darwin'), { wall_seconds: null, max_rss_bytes: null, peak_footprint_bytes: null });
});

test('a showcase output is summarized into sorted deterministic counts', async (t) => {
  const output = await mkdtemp(path.join(os.tmpdir(), 'cartograph-1228-'));
  t.after(() => rm(output, { recursive: true, force: true }));
  await writeFile(path.join(output, 'ingest.json'), JSON.stringify([
    { summary: { files: 2, nodes: 10, edges: 7 } }, { summary: { files: 1, nodes: 3, edges: 1 } }]));
  await writeFile(path.join(output, 'labels.json'), JSON.stringify({ Symbol: 4, File: 3 }));
  await writeFile(path.join(output, 'endpoints.json'), JSON.stringify([
    { props: { auth: { state: 'required' } } }, { props: { auth: { state: 'unknown' } } },
    { props: { auth: { state: 'required' } } }, { props: {} }]));
  const queries = [];
  const query = (file, sql) => {
    queries.push(path.relative(output, file));
    if (sql.includes('receipt_headers')) return [{ headers: 1, header_bytes: 40 }];
    if (sql.includes('from receipts')) return [{ receipts: 5, payload_bytes: 900 }];
    return [{ nodes: 2, edges: 0 }];
  };
  const counts = summarize(output, query);
  assert.deepEqual(counts, {
    files: 3, nodes: 13, edges: 8,
    labels: { File: 3, Symbol: 4 },
    endpoint_auth: { absent: 1, required: 2, unknown: 1 },
    receipts: { receipts: 5, payload_bytes: 900, headers: 1, header_bytes: 40 },
    receipt_omissions: { nodes: 2, edges: 0 },
  });
  assert.deepEqual(Object.keys(counts.labels), ['File', 'Symbol']);
  assert.deepEqual(queries, [
    'profile/data/retained-source/receipts.sqlite', 'profile/data/retained-source/receipts.sqlite', 'profile/data/graph.db']);
  // An unreadable store is reported as unknown, never as zero.
  assert.equal(summarize(output, () => null).receipts, null);
});

test('comparison reports measurement deltas and flags counts that differ on one commit', () => {
  const repo = (wall, nodes, commit = sha) => ({
    name: 'one', commit, measurements: { wall_seconds: wall, max_rss_bytes: null, peak_footprint_bytes: 100 },
    counts: { nodes } });
  const run = (commit, ...repos) => ({ cartograph_commit: commit, repos });
  assert.deepEqual(compare(run('c1', repo(100, 5)), run('c1', repo(90, 5))),
    { lines: ['one: wall_seconds 100 -> 90 (-10.0%)', 'one: peak_footprint_bytes 100 -> 100 (0.0%)'], countsDiffer: false });
  const same = compare(run('c1', repo(100, 5)), run('c1', repo(100, 6)));
  assert.equal(same.countsDiffer, true);
  assert.equal(same.lines.at(-1), 'determinism: counts differ between two runs of the same Cartograph commit');
  const changed = compare(run('c1', repo(100, 5)), run('c2', repo(100, 6, 'b'.repeat(40))));
  assert.equal(changed.countsDiffer, true);
  assert.ok(changed.lines.includes(`one: pin moved ${sha} -> ${'b'.repeat(40)}`));
  assert.ok(!changed.lines.some((line) => line.startsWith('determinism')));
  assert.deepEqual(compare(run('c1'), run('c1', repo(1, 1))).lines, ['one: new']);
  // An uncommitted tree is not the same build, even at one commit.
  const dirty = compare(run('c1', repo(100, 5)), { ...run('c1', repo(100, 6)), cartograph_dirty: true });
  assert.equal(dirty.countsDiffer, true);
  assert.ok(!dirty.lines.some((line) => line.startsWith('determinism')));
});
