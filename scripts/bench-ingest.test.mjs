// AC-0423: manifest, timing, summary and comparison are pure; no Cargo, git or showcase runs.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, symlink, writeFile, readFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';
import { checkout, compare, contained, parseManifest, parseTime, realPath, selectRoot, summarize } from './bench-ingest.mjs';

const sha = 'a'.repeat(40);

test('the shipped manifest pins every repository to a full commit', async () => {
  const repos = parseManifest(await readFile(new URL('../bench/repos.toml', import.meta.url), 'utf8'));
  assert.deepEqual(repos.map((repo) => repo.name), ['simplelogin', 'rea', 'vendure', 'openmrs-core', 'kafka', 'kubernetes']);
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

test('comparison reports measurement deltas and flags counts that differ for one build and source', () => {
  const repo = (wall, nodes, fields = {}) => ({
    name: 'one', url: 'https://example.com/one', commit: sha, measurements: { wall_seconds: wall, max_rss_bytes: null, peak_footprint_bytes: 100 },
    counts: { nodes }, ...fields });
  const run = (commit, ...repos) => ({ cartograph_commit: commit, build: 'cargo', repos });
  assert.deepEqual(compare(run('c1', repo(100, 5)), run('c1', repo(90, 5))),
    { lines: ['one: wall_seconds 100 -> 90 (-10.0%)', 'one: peak_footprint_bytes 100 -> 100 (0.0%)'], countsDiffer: false, nondeterministic: false, membershipDiffers: false });
  const same = compare(run('c1', repo(100, 5)), run('c1', repo(100, 6)));
  assert.equal(same.nondeterministic, true);
  assert.equal(same.lines.at(-1), 'determinism: counts differ between two runs of the same build and source');
  // A moved pin, url or path selects other source: its counts may differ.
  for (const [fields, line] of [
    [{ commit: 'b'.repeat(40) }, `one: commit moved ${sha} -> ${'b'.repeat(40)}`],
    [{ url: 'https://example.com/two' }, 'one: url moved https://example.com/one -> https://example.com/two'],
    [{ path: 'packages/core' }, 'one: path moved (none) -> packages/core'],
  ]) {
    const moved = compare(run('c1', repo(100, 5)), run('c1', repo(100, 6, fields)));
    assert.equal(moved.countsDiffer, true);
    assert.equal(moved.nondeterministic, false);
    assert.ok(moved.lines.includes(line), line);
    assert.ok(!moved.lines.some((entry) => entry.startsWith('determinism')));
  }
  // Membership changes are reported both ways and fail the comparison.
  for (const [before, after, line] of [[run('c1'), run('c1', repo(1, 1)), 'one: new'], [run('c1', repo(1, 1)), run('c1'), 'one: removed']]) {
    const changed = compare(before, after);
    assert.deepEqual(changed.lines, [line]);
    assert.equal(changed.membershipDiffers, true);
  }
  // An uncommitted tree, another commit or a supplied binary is not the same build.
  for (const after of [
    { ...run('c1', repo(100, 6)), cartograph_dirty: true },
    run('c2', repo(100, 6)),
    { ...run('c1', repo(100, 6)), build: 'supplied' },
  ]) {
    const other = compare(run('c1', repo(100, 5)), after);
    assert.equal(other.countsDiffer, true);
    assert.equal(other.nondeterministic, false);
  }
});

test('a selected root never leaves its checkout, even through a symlink', async (t) => {
  const base = await mkdtemp(path.join(os.tmpdir(), 'cartograph-1228-root-'));
  t.after(() => rm(base, { recursive: true, force: true }));
  const checkout = path.join(base, 'checkout');
  await mkdir(path.join(checkout, 'packages', 'core'), { recursive: true });
  await mkdir(path.join(base, 'outside'));
  await writeFile(path.join(checkout, 'file'), '');
  await symlink(path.join(base, 'outside'), path.join(checkout, 'escape'));
  await symlink(path.join(checkout, 'packages'), path.join(checkout, 'alias'));
  const real = (...parts) => path.join(realpathSync(checkout), ...parts);
  assert.equal(selectRoot(checkout), real());
  assert.equal(selectRoot(checkout, 'packages/core'), real('packages', 'core'));
  assert.equal(selectRoot(checkout, 'alias/core'), real('packages', 'core'));
  for (const bad of ['escape', 'file', 'missing']) assert.throws(() => selectRoot(checkout, bad), undefined, bad);
  // A path that does not exist yet resolves through its existing ancestors.
  assert.equal(realPath(path.join(checkout, 'escape', 'new', 'dir')), path.join(realpathSync(base), 'outside', 'new', 'dir'));
  assert.ok(contained(real(), realPath(path.join(checkout, 'alias', 'new'))));
});

test('containment compares whole path components', () => {
  assert.ok(contained('/a/b', '/a/b'));
  assert.ok(contained('/a/b', '/a/b/c'));
  assert.ok(contained('/a/b', '/a/b/..c'));
  assert.ok(!contained('/a/b', '/a/bc'));
  assert.ok(!contained('/a/b', '/a'));
  assert.ok(!contained('/a/b', '/x'));
});

test('a kept clean checkout moves to a new pin, and a dirty one is refused', async (t) => {
  const base = await mkdtemp(path.join(os.tmpdir(), 'bench-checkout-'));
  t.after(() => rm(base, { recursive: true, force: true }));
  const origin = path.join(base, 'origin');
  const git = (cwd, ...args) => execFileSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@example.invalid', '-C', cwd, ...args], { encoding: 'utf8' }).trim();
  await mkdir(origin);
  git(origin, 'init', '--quiet');
  await writeFile(path.join(origin, 'a'), '1');
  git(origin, 'add', 'a');
  git(origin, 'commit', '--quiet', '-m', 'one');
  const first = git(origin, 'rev-parse', 'HEAD');
  await writeFile(path.join(origin, 'a'), '2');
  git(origin, 'commit', '--quiet', '-am', 'two');
  const second = git(origin, 'rev-parse', 'HEAD');
  const work = path.join(base, 'work');
  const dir = path.join(work, 'sources', 'repo');
  checkout({ name: 'repo', url: origin, commit: first }, work);
  assert.equal(git(dir, 'rev-parse', 'HEAD'), first);
  await writeFile(path.join(origin, 'a'), '3');
  git(origin, 'commit', '--quiet', '-am', 'three');
  const third = git(origin, 'rev-parse', 'HEAD');
  checkout({ name: 'repo', url: origin, commit: third }, work);
  assert.equal(git(dir, 'rev-parse', 'HEAD'), third);
  checkout({ name: 'repo', url: origin, commit: second }, work);
  assert.equal(git(dir, 'rev-parse', 'HEAD'), second);
  await writeFile(path.join(dir, 'a'), 'local');
  assert.throws(() => checkout({ name: 'repo', url: origin, commit: first }, work), /not a clean checkout/u);
  assert.equal(await readFile(path.join(dir, 'a'), 'utf8'), 'local');
});
